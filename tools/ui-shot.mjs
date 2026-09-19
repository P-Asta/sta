#!/usr/bin/env node
// Screenshot a sta UI page in mock mode with headless Edge (or whichever Chromium is installed —
// `defaultHeadlessBrowser`). Normally run through
// tools/ui-shot.ps1 (same options, PowerShell-style names); see ui/README.md.
//
//   node tools/ui-shot.mjs --path '/sidebar/?mock' --out sidebar.png [--width 248] [--height 900]
//        [--dark] [--console] [--budget 3000] [--scale 1] [--edge <msedge.exe>]
//
// 1. Serves ui/ in-process (tools/ui-serve.mjs) on a free 127.0.0.1 port.
// 2. Starts headless Edge with a throwaway profile and `--remote-debugging-port=0` (the port is
//    read from the profile's DevToolsActivePort file, so no port can collide).
// 3. Pins the viewport with Emulation.setDeviceMetricsOverride (exact CSS size, also below Edge's
//    ~500 px minimum window width), loads the page, waits for the load event, `--budget` ms and,
//    on mock pages, `ui.ready`, then writes the PNG.
// 4. Always stops Edge (whole process tree), the server and removes the profile.
//
// Exit codes: 0 captured and clean; 1 capture failed (no PNG); 2 captured, but the page reported
// problems: console errors, uncaught exceptions, failed loads of its own resources (4xx/5xx for
// ui/ files), or a mock page that never sent ui.ready. Problems and warnings (a capture without
// any visible text, icon or control; an unexpected viewport size) are always printed; `--console`
// also prints every other console message and external load failures (remote favicons etc.,
// which don't count as problems).
//
// Everything is printed to stdout (the PowerShell wrapper must not see native stderr).

import { spawn, spawnSync } from 'node:child_process';
import { existsSync } from 'node:fs';
import { mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { defaultHeadlessBrowser, startStaticServer } from './ui-serve.mjs';

const DEFAULT_EDGE = defaultHeadlessBrowser();
const toolsDir = path.dirname(fileURLToPath(import.meta.url));
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
/** A timeout that doesn't keep the process alive once everything else is done. */
const timeout = (ms) => new Promise((r) => setTimeout(r, ms).unref());
const log = (...a) => console.log(...a);

// ------------------------------------------------------------------------------------ options

function parseArgs(argv) {
  const o = { width: 1280, height: 900, budget: 3000, scale: 1, dark: false, console: false, edge: DEFAULT_EDGE };
  for (let i = 0; i < argv.length; i++) {
    const key = argv[i].replace(/^--?/, '').toLowerCase();
    const value = () => {
      if (i + 1 >= argv.length) throw new Error(`--${key} needs a value`);
      return argv[++i];
    };
    switch (key) {
      case 'path': o.path = value(); break;
      case 'out': o.out = value(); break;
      case 'width': o.width = Number(value()); break;
      case 'height': o.height = Number(value()); break;
      case 'budget': o.budget = Number(value()); break;
      case 'scale': o.scale = Number(value()); break;
      case 'edge': o.edge = value(); break;
      case 'dark': o.dark = true; break;
      case 'console': o.console = true; break;
      default: throw new Error(`unknown option ${argv[i]}`);
    }
  }
  if (!o.path || !o.out) throw new Error('--path and --out are required');
  for (const k of ['width', 'height']) if (!Number.isInteger(o[k]) || o[k] < 1) throw new Error(`invalid --${k} ${o[k]}`);
  if (!Number.isFinite(o.budget) || o.budget < 0) throw new Error(`invalid --budget ${o.budget}`);
  if (!Number.isFinite(o.scale) || o.scale <= 0) throw new Error(`invalid --scale ${o.scale}`);
  if (!existsSync(o.edge)) throw new Error(`no headless browser at ${o.edge} (pass --edge / -Edge)`);
  return o;
}

// ------------------------------------------------------------------------------------ processes

/** Kill a process and its children; never throws. */
function killTree(child) {
  if (!child || child.exitCode !== null || child.signalCode !== null) return;
  if (process.platform === 'win32') {
    spawnSync('taskkill', ['/PID', String(child.pid), '/T', '/F'], { stdio: 'ignore', windowsHide: true });
  } else {
    try {
      child.kill('SIGKILL');
    } catch {}
  }
}

const exited = (child, ms) =>
  child.exitCode !== null || child.signalCode !== null
    ? Promise.resolve()
    : Promise.race([new Promise((r) => child.once('exit', r)), timeout(ms)]);

/** Port from `<profile>/DevToolsActivePort` (written by Edge once DevTools listens). */
async function devToolsPort(profileDir, edge, timeoutMs) {
  const file = path.join(profileDir, 'DevToolsActivePort');
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    if (edge.exitCode !== null) throw new Error(`Edge exited early (code ${edge.exitCode})`);
    const text = await readFile(file, 'utf8').catch(() => '');
    const port = Number(text.split(/\r?\n/)[0]);
    if (Number.isInteger(port) && port > 0) return port;
    await sleep(100);
  }
  throw new Error('Edge did not open its DevTools port');
}

// ------------------------------------------------------------------------------------ capture

async function capture(o, origin, profileDir, edge, { problems, warnings, notes }) {
  const port = await devToolsPort(profileDir, edge, 20000);
  let page;
  for (let i = 0; i < 100 && !page; i++) {
    page = await fetch(`http://127.0.0.1:${port}/json/list`)
      .then((r) => r.json())
      .then((targets) => targets.find((t) => t.type === 'page'))
      .catch(() => null);
    if (!page) await sleep(100);
  }
  if (!page) throw new Error('no DevTools page target');

  const ws = new WebSocket(page.webSocketDebuggerUrl);
  await new Promise((ok, err) => {
    ws.onopen = ok;
    ws.onerror = () => err(new Error('DevTools WebSocket failed'));
  });
  let nextId = 0;
  const pending = new Map();
  let onLoad;
  const loaded = new Promise((r) => (onLoad = r));
  const isLocal = (u) => typeof u === 'string' && u.startsWith(origin);
  const ignoredUrl = (u) => /^(chrome|edge|extension)[-:]/.test(u || '') || /\/favicon\.ico(\?|$)/.test(u || '');
  ws.onmessage = (event) => {
    const msg = JSON.parse(event.data);
    if (msg.id && pending.has(msg.id)) {
      const p = pending.get(msg.id);
      pending.delete(msg.id);
      if (msg.error) p.reject(new Error(JSON.stringify(msg.error)));
      else p.resolve(msg.result);
      return;
    }
    const params = msg.params || {};
    if (msg.method === 'Page.loadEventFired') onLoad();
    else if (msg.method === 'Runtime.consoleAPICalled') {
      const text = params.args.map((a) => (a.value !== undefined ? String(a.value) : a.description || a.type)).join(' ');
      const frame = params.stackTrace?.callFrames?.[0];
      const entry = { level: params.type, text, url: frame ? `${frame.url}:${frame.lineNumber + 1}` : '' };
      (params.type === 'error' || params.type === 'assert' ? problems : notes).push(entry);
    } else if (msg.method === 'Runtime.exceptionThrown') {
      const d = params.exceptionDetails;
      problems.push({ level: 'exception', text: d.exception?.description || d.text, url: d.url ? `${d.url}:${d.lineNumber + 1}` : '' });
    } else if (msg.method === 'Log.entryAdded') {
      const e = params.entry;
      if (ignoredUrl(e.url)) return;
      const entry = { level: `${e.source}:${e.level}`, text: e.text, url: e.url || '' };
      // Failed loads of ui/ files break the page; remote resources (favicons) may be offline.
      const counts = e.level === 'error' && (e.source !== 'network' || isLocal(e.url) || !e.url);
      (counts ? problems : notes).push(entry);
    }
  };
  const send = (method, params = {}) =>
    new Promise((resolve, reject) => {
      const id = ++nextId;
      pending.set(id, { resolve, reject });
      ws.send(JSON.stringify({ id, method, params }));
    });
  const evaluate = async (expression) => (await send('Runtime.evaluate', { expression, returnByValue: true })).result.value;

  try {
    await send('Page.enable');
    await send('Runtime.enable');
    await send('Log.enable');
    await send('Emulation.setDeviceMetricsOverride', { width: o.width, height: o.height, deviceScaleFactor: o.scale, mobile: false });
    if (o.dark) await send('Emulation.setEmulatedMedia', { features: [{ name: 'prefers-color-scheme', value: 'dark' }] });
    const rel = o.path.startsWith('/') ? o.path : `/${o.path}`;
    const url = `${origin}${rel}${o.dark ? (rel.includes('?') ? '&dark=1' : '?dark=1') : ''}`;
    await send('Page.navigate', { url });
    const loadStart = Date.now();
    const gotLoad = await Promise.race([loaded.then(() => true), timeout(15000).then(() => false)]);
    if (!gotLoad) problems.push({ level: 'load', text: 'no load event within 15 s', url });
    const afterLoad = Date.now();
    await sleep(o.budget);
    // Mock pages: also wait (up to 10 s after load) for ui.ready, i.e. the first render.
    if (await evaluate('Boolean(window.__mockLog)')) {
      while (!(await evaluate('window.__mockReady === true')) && Date.now() - afterLoad < Math.max(10000, o.budget)) await sleep(100);
      if (!(await evaluate('window.__mockReady === true'))) problems.push({ level: 'ready', text: 'mock page never sent ui.ready', url });
    }
    const [vw, vh] = JSON.parse(await evaluate('JSON.stringify([innerWidth, innerHeight])'));
    if (vw !== o.width || vh !== o.height) warnings.push({ level: 'warning', text: `viewport is ${vw}x${vh}, expected ${o.width}x${o.height}`, url: '' });
    // Nothing visible (no text, no image/icon/control): usually a render that failed silently. Some
    // surfaces are legitimately empty in their default mock state (toast without toast=…), so this
    // is a warning, not a problem.
    const blank = await evaluate(`(() => {
      const body = document.body;
      if (!body) return true;
      const shown = (el) => {
        const r = el.getBoundingClientRect();
        const s = getComputedStyle(el);
        return r.width > 0 && r.height > 0 && s.visibility !== 'hidden' && Number(s.opacity) > 0;
      };
      if ((body.innerText || '').trim()) return false;
      return ![...body.querySelectorAll('img, svg, canvas, video, input, button, textarea, select')].some(shown);
    })()`);
    if (blank) warnings.push({ level: 'warning', text: 'the page shows no text, icons or controls (blank capture?)', url });
    const shot = await send('Page.captureScreenshot', { format: 'png' });
    await writeFile(o.out, Buffer.from(shot.data, 'base64'));
    return { url, ms: Date.now() - loadStart };
  } finally {
    ws.close();
  }
}

// ------------------------------------------------------------------------------------ main

async function main() {
  let o;
  try {
    o = parseArgs(process.argv.slice(2));
  } catch (e) {
    log(`ui-shot: ${e.message}`);
    return 1;
  }
  o.out = path.resolve(o.out);
  await mkdir(path.dirname(o.out), { recursive: true });
  await rm(o.out, { force: true });

  const problems = [];
  const notes = [];
  const warnings = [];
  const localFailures = new Map();
  let server;
  let edge;
  let tmp;
  let result;
  let failure;
  const cleanup = async () => {
    killTree(edge);
    if (edge) await exited(edge, 5000);
    await server?.close();
    if (tmp) await rm(tmp, { recursive: true, force: true, maxRetries: 10, retryDelay: 200 }).catch(() => {});
  };
  const onSignal = () => cleanup().finally(() => process.exit(130));
  process.once('SIGINT', onSignal);
  process.once('SIGTERM', onSignal);
  try {
    server = await startStaticServer({
      root: path.join(toolsDir, '..', 'ui'),
      onResponse: ({ url, status }) => {
        if (status >= 400 && !/\/favicon\.ico(\?|$)/.test(url)) localFailures.set(url, status);
      },
    });
    tmp = await mkdtemp(path.join(tmpdir(), 'sta-ui-shot-'));
    const profileDir = path.join(tmp, 'edge-profile');
    const edgeArgs = [
      '--headless=new',
      '--disable-gpu',
      '--hide-scrollbars',
      '--no-first-run',
      '--no-default-browser-check',
      '--disable-extensions',
      '--disable-sync',
      '--disable-background-networking',
      `--user-data-dir=${profileDir}`,
      '--remote-debugging-port=0',
      ...(o.dark ? ['--force-dark-mode'] : []),
      'about:blank',
    ];
    edge = spawn(o.edge, edgeArgs, { stdio: 'ignore', windowsHide: true });
    edge.once('error', (e) => warnings.push({ level: 'edge', text: e.message, url: '' }));
    result = await capture(o, server.origin, profileDir, edge, { problems, warnings, notes });
  } catch (e) {
    failure = e;
  } finally {
    await cleanup();
    process.off('SIGINT', onSignal);
    process.off('SIGTERM', onSignal);
  }

  // A 4xx/5xx of a ui/ file is reported once (the server saw it; drop the console duplicate).
  const failedUrls = new Set([...localFailures.keys()].map((u) => `${server?.origin}${u}`));
  for (let i = problems.length - 1; i >= 0; i--) {
    if (problems[i].level === 'network:error' && failedUrls.has(problems[i].url)) problems.splice(i, 1);
  }
  for (const [url, status] of localFailures) problems.push({ level: 'http', text: `${status} for ${url}`, url: '' });
  const print = (m) => log(`[${m.level}] ${m.text}${m.url ? `  (${m.url})` : ''}`);
  if (o.console) notes.forEach(print);
  warnings.forEach(print);
  problems.forEach(print);
  if (failure || !result || !existsSync(o.out)) {
    log(`ui-shot: capture failed: ${failure?.message ?? 'no screenshot written'}`);
    return 1;
  }
  log(`saved ${o.out} (${o.width} x ${o.height}, scale ${o.scale}) <- ${result.url}`);
  log(`console: ${notes.length + problems.length} message(s), ${problems.length} problem(s), ${warnings.length} warning(s)${problems.length && !o.console ? ' (rerun with -Console for every message)' : ''}`);
  return problems.length ? 2 : 0;
}

process.exitCode = await main();
