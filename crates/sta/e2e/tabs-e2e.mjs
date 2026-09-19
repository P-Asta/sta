#!/usr/bin/env node
// End-to-end checks of the tabs track (popups, errors, downloads, permissions, find, zoom, audio,
// fullscreen, boosts, close/crash, context menus, DevTools) against a debug build (Windows, Node 22+).
//
//   cargo build -p sta -p sta-mcp --features test-hooks
//   node crates/sta/e2e/tabs-e2e.mjs [--keep-open] [--only=popups,errors,...]
//
// Sections: popups (window.open ± features, target=_blank, middle-click, sta:// block, document
// PiP, tab context menu), intercept (pinned cross-site → Peek), preview (Alt+click / Alt+middle-click
// on a link → Peek: every link kind incl. a cross-origin iframe, no download, refusals, the shapes
// the gesture leaves alone (javascript:, `href="#"`, fragments, an over-long href), a sandboxed
// frame, Alt+Enter (keyboard activation is not a click), a page that cannot forge the gesture,
// Expand, Alt+click inside Peek, real Esc, the popup-Peek rule, peekEnabled off, a
// Content-Disposition attachment, and light/dark captures at 100 % and 150 %), menus (UI context menus), errors
// (in-place error page, escaping, retry, history), downloads (progress, pause/resume, unique name,
// Mark-of-the-Web stream, cancel, retry), permissions (media allow/deny, prompt, Block without
// Remember asks again, remembered block, dismissal on close; no auto-block after repeated one-off
// Blocks, Allow without Remember lasts only while a tab shows the origin, remembered allow persists,
// prompts from UI pages settle), find, zoom, audio,
// fullscreen (incl. from Peek), boosts (reload, extra_info, stale renderer check, toggle), focus,
// replace (web ↔ internal ReplaceBrowser), close (beforeunload), crash (tab + UI surface),
// devtools (the docked frontend: bounds, transport, policy, Inspect, overlays above it, undock,
// real F12/Ctrl+Shift+I/F11), hygiene (no console window appeared during the run).
//
// The `devtools` section sends **real OS keys**, so this suite now needs sta to be the foreground
// window while it runs, like chrome-e2e and agent-e2e (docs/TESTING.md §1).
//
// Launches target/debug/sta.exe with its own data dir (E2E_DATA_DIR, default
// C:/ast/tmp/tabs-data), `--use-fake-device-for-media-stream` and STA_TEST_CONTEXT_MENU="Open Link
// in New Tab" (debug-only: native context menus are replaced by a logged, scripted choice), and
// drives it **through MCP** — JSON-RPC over stdio to target/debug/sta-mcp.exe, forwarded to the
// browser over its named pipe (lib.mjs, docs/TESTING.md). The browser must therefore be built with
// `--features test-hooks`; lib.mjs arms it. No DevTools port and no PowerShell are involved: the
// `debug.*` requests, `window.sta.invoke`, raw DevTools (`Page.crash`,
// `Page.getNavigationHistory`, `Input.dispatchMouseEvent`), window screenshots and the downloaded
// file's `Zone.Identifier` stream all go through `test_*` tools.
//
// Test pages are generated into E2E_WWW (default C:/ast/tmp/tabs/www) and served by a small Node
// HTTP server on an ephemeral port (static files, a slow page and a throttled 5 MB download); a
// second one is started mid-test, on a port reserved up front, for the error-page retry.
// E2E_HTTP_PORT / E2E_RETRY_PORT pin those ports when a run needs fixed ones.
//
// Commands go through the sidebar page's `window.sta` (the real IPC path, via `test_invoke`); tab
// pages are inspected with `test_eval`. Only the process tree started here is ever killed.

import { existsSync, rmSync, mkdirSync, readFileSync, writeFileSync, statSync, readdirSync } from 'node:fs';
import http from 'node:http';
import path from 'node:path';
import { Instance, alive, checkNoConsoleWindows, consoleBaseline, killTree, retryInterrupted, sleep, tabInfo } from './lib.mjs';

const DATA = process.env.E2E_DATA_DIR || 'C:/ast/tmp/tabs-data';
const WWW = process.env.E2E_WWW || 'C:/ast/tmp/tabs/www';
const DL = path.join(path.dirname(WWW), 'downloads').replace(/\\/g, '/');
/** Ephemeral by default (the data dir alone makes a run unique); pinned by env when needed. */
let HTTP_PORT = Number(process.env.E2E_HTTP_PORT || 0);
let RETRY_PORT = Number(process.env.E2E_RETRY_PORT || 0);
let H = '';
const KEEP_OPEN = process.argv.includes('--keep-open');
const ONLY = (process.argv.find((a) => a.startsWith('--only=')) || '').slice(7).split(',').filter(Boolean);
const SB = 'sta://sidebar/';
const want = (section) => ONLY.length === 0 || ONLY.includes(section);

// ------------------------------------------------------------------------------------ reporting

const results = [];
function check(section, name, ok, detail) {
  results.push({ section, name, ok: !!ok });
  let d = detail === undefined ? '' : ' ' + (typeof detail === 'string' ? detail : JSON.stringify(detail));
  if (ok && d.length > 200) d = d.slice(0, 200) + '…';
  console.log(`${ok ? 'PASS' : 'FAIL'} [${section}] ${name}${d}`);
  return !!ok;
}

async function waitFor(fn, timeoutMs = 5000, stepMs = 100) {
  const end = Date.now() + timeoutMs;
  let last;
  while (Date.now() < end) {
    try {
      last = await fn();
      if (last) return last;
    } catch {
      // retry
    }
    await sleep(stepMs);
  }
  return last;
}

// ------------------------------------------------------------------------------------ the browser

/**
 * The instance under test, and the transport helpers, all from lib.mjs: the local names below keep
 * every check body unchanged, only what is underneath them moved to MCP.
 */
let inst;
const servers = [];
const log = () => inst.log();
const capture = async (name) => {
  try {
    return await inst.capture(name);
  } catch (e) {
    return 'capture failed: ' + e.message;
  }
};

// ------------------------------------------------------------------------------------ test pages

function wav(seconds = 20, freq = 440) {
  const rate = 22050;
  const n = rate * seconds;
  const buf = Buffer.alloc(44 + n * 2);
  buf.write('RIFF', 0);
  buf.writeUInt32LE(36 + n * 2, 4);
  buf.write('WAVEfmt ', 8);
  buf.writeUInt32LE(16, 16);
  buf.writeUInt16LE(1, 20);
  buf.writeUInt16LE(1, 22);
  buf.writeUInt32LE(rate, 24);
  buf.writeUInt32LE(rate * 2, 28);
  buf.writeUInt16LE(2, 32);
  buf.writeUInt16LE(16, 34);
  buf.write('data', 36);
  buf.writeUInt32LE(n * 2, 40);
  for (let i = 0; i < n; i++) buf.writeInt16LE(Math.round(Math.sin((2 * Math.PI * freq * i) / rate) * 3000), 44 + i * 2);
  return buf;
}

function writePages() {
  rmSync(WWW, { recursive: true, force: true });
  mkdirSync(WWW, { recursive: true });
  const page = (name, title, body) =>
    writeFileSync(path.join(WWW, name), `<!doctype html><html><head><meta charset="utf-8"><title>${title}</title></head><body>${body}</body></html>`);
  page(
    'opener.html',
    'Opener',
    `<h1>Opener</h1>
<p><a id="blank" href="b.html" target="_blank">target blank</a></p>
<p><a id="mid" href="c.html" style="display:inline-block;padding:20px;font-size:24px">middle click me</a></p>
<p><a id="ctx" href="d.html" style="display:inline-block;padding:20px;font-size:24px">context menu link</a></p>
<p><a id="dl" href="slow.bin" download>download</a></p>
<script>window.__messages = []; addEventListener('message', function (e) { window.__messages.push(e.data); });</script>`,
  );
  page('popup.html', 'Popup', `<h1>Popup</h1><script>if (window.opener) window.opener.postMessage({ from: location.href }, '*');</script>`);
  page('b.html', 'Page B', '<h1>B</h1>');
  page('c.html', 'Page C', '<h1>C</h1>');
  page('d.html', 'Page D', '<h1>D</h1>');
  page('perm.html', 'Permissions', '<h1>Permissions</h1>');
  page('find.html', 'Find', '<p>one needle</p><p>two needle</p><p>three needle</p>');
  page('media.html', 'Media', '<audio id="a" src="tone.wav" loop></audio><h1>Media</h1>');
  page('fs.html', 'Fullscreen', '<h1>Fullscreen</h1>');
  page('boost.html', 'Boost', '<h1 id="t">Boost me</h1>');
  page('boost2.html', 'Boost 2', '<h1 id="t">Boost me too</h1>');
  page('bu.html', 'Beforeunload', `<h1>beforeunload</h1><script>addEventListener('beforeunload', function (e) { e.preventDefault(); e.returnValue = ''; });</script>`);
  page('crash.html', 'Crash', '<h1>Crash</h1>');
  page('ok.html', 'OK page', '<h1>OK</h1>');
  page('frame.html', 'Frame', '<p id="inner" style="margin:0;height:60px">inner frame</p>');
  // Every kind of link the Alt+click preview has to get right, plus `window.__rect(name)` so the
  // suite can aim a click at one (`getElementById` cannot reach into the shadow tree).
  page(
    'links.html',
    'Links',
    `<style>
  body { margin: 0; background: #ddeeff; font: 16px sans-serif }
  a, .box, svg, iframe { display: block; width: 320px; height: 34px; line-height: 34px; background: #eeeeff; border: 0 }
</style>
<a id="plain" href="b.html?preview">plain link</a>
<a id="shot" href="shot.html">strongly coloured page (for the captures)</a>
<a id="blank" href="b.html?blank" target="_blank">target blank</a>
<a id="nohref">anchor without href</a>
<div class="box" id="notalink">not a link at all</div>
<svg id="svgbox" width="320" height="34"><a id="svga" href="b.html?svg"><rect width="320" height="34" fill="#eeffee"/></a></svg>
<div class="box" id="host"></div>
<a id="dlattr" href="b.html?dl" download="saved.html">download= anchor</a>
<a id="mail" href="mailto:nobody@example.com">mailto link</a>
<a id="jsl" href="javascript:void(document.title='JS-RAN')">javascript link</a>
<a id="internal" href="sta://settings/">sta:// link</a>
<iframe id="sub" src="sublinks.html"></iframe>
<a id="attach" href="attach.bin">attachment link (Content-Disposition)</a>
<a id="apphash" href="#">app button, href="#"</a>
<a id="appjs" href="javascript:void(0)">app button, javascript:void(0)</a>
<a id="frag" href="#bottom">in-page fragment</a>
<a id="keyact" href="b.html?keyact">keyboard activation</a>
<a id="longurl" href="b.html?long">href over the cap</a>
<iframe id="xo" src="about:blank"></iframe>
<iframe id="sbox" src="sandlinks.html" sandbox="allow-scripts"></iframe>
<div id="bottom">bottom</div>
<script>
  document.getElementById('host').attachShadow({ mode: 'open' }).innerHTML =
    '<style>a{display:block;width:320px;height:34px;line-height:34px;background:#ffeeee}</style><a id="shadow" href="b.html?shadow">shadow-tree link</a>';
  // The shapes the gesture must leave completely alone are the ones a page handles itself: each of
  // these records the click it received and cancels it, so nothing downloads if it is let through.
  window.__ran = [];
  window.__msgs = [];
  addEventListener('message', function (e) { window.__msgs.push(e.data); });
  ['apphash', 'appjs', 'keyact', 'longurl'].forEach(function (id) {
    document.getElementById(id).addEventListener('click', function (e) {
      window.__ran.push(id);
      e.preventDefault();
    });
  });
  // Record-only: whatever Chromium decides to do with a javascript: href under Alt, the page must
  // see the click (that is the property; running the URL is Chromium's business).
  document.getElementById('jsl').addEventListener('click', function () { window.__ran.push('jsl'); });
  // Longer than the renderer's 64 Ki-character cap.
  document.getElementById('longurl').href = location.origin + '/b.html?' + new Array(70001).join('x');
  // A different host is a different site: this frame gets its own render process (an OOPIF).
  document.getElementById('xo').src = location.origin.replace('127.0.0.1', 'localhost') + '/xolinks.html';
  window.__rect = function (name) {
    var el = document.getElementById(name);
    if (!el) el = document.getElementById('host').shadowRoot.getElementById(name);
    if (!el) return null;
    el.scrollIntoView({ block: 'nearest' });
    var r = el.getBoundingClientRect();
    return { x: r.left + r.width / 2, y: r.top + r.height / 2 };
  };
</script>`,
  );
  page('shot.html', 'Preview shot', '<style>html,body{margin:0;height:100%;background:#3a6ea5}</style><h1 style="color:#fff">previewed</h1>');
  page('sublinks.html', 'Sub links', `<style>body{margin:0}a{display:block;width:300px;height:30px;background:#ffffcc}</style><a id="inframe" href="b.html?iframe">link in an iframe</a>`);
  // Served from `localhost` instead of `127.0.0.1`, so this frame is cross-origin to its embedder.
  page('xolinks.html', 'Cross-origin links', `<style>body{margin:0}a{display:block;width:300px;height:30px;background:#ccffff}</style><a id="xolink" href="b.html?xo">link in a cross-origin iframe</a>`);
  // A sandboxed frame (opaque origin): its embedder took even `target=_blank` away from it, so the
  // preview gesture must not exist in it. It cancels the click itself and tells the parent, which is
  // the only thing it can still do (it can read nothing of the parent).
  page(
    'sandlinks.html',
    'Sandboxed links',
    `<style>body{margin:0}a{display:block;width:300px;height:30px;background:#ffccff}</style>
<a id="sand" href="b.html?sand" target="_blank">link in a sandboxed frame</a>
<script>
  document.getElementById('sand').addEventListener('click', function (e) {
    e.preventDefault();
    parent.postMessage('sand', '*');
  });
</script>`,
  );
  page(
    'devtools.html',
    'DevTools page',
    `<h1>DevTools</h1>
<div id="box" style="width:120px;height:60px;background:#c33">box</div>
<iframe id="f" style="width:220px;height:90px"></iframe>
<script>
// A different host is a different site: the iframe gets its own process (an OOPIF) and therefore
// its own nested DevTools session.
document.getElementById('f').src = location.origin.replace('127.0.0.1', 'localhost') + '/frame.html';
window.__worker = 'pending';
var w = new Worker(URL.createObjectURL(new Blob(['setInterval(function () {}, 2000)'])));
window.__worker = 'ready';
</script>`,
  );
  writeFileSync(path.join(WWW, 'tone.wav'), wav());
}

// ------------------------------------------------------------------------------------ fixtures

const MIME = {
  '.html': 'text/html; charset=utf-8',
  '.js': 'text/javascript; charset=utf-8',
  '.css': 'text/css; charset=utf-8',
  '.wav': 'audio/wav',
  '.png': 'image/png',
  '.bin': 'application/octet-stream',
};

/** `/slow.bin`: 5 MB at 64 KB per 50 ms, with a filename, so a download stays observable. */
function slowDownload(res) {
  const SIZE = 5 * 1024 * 1024;
  const CHUNK = 64 * 1024;
  res.writeHead(200, {
    'Content-Type': 'application/octet-stream',
    'Content-Disposition': 'attachment; filename="slow.bin"',
    'Content-Length': String(SIZE),
  });
  let sent = 0;
  const tick = () => {
    if (res.destroyed || res.writableEnded) return;
    const n = Math.min(CHUNK, SIZE - sent);
    res.write(Buffer.alloc(n, Math.floor(sent / CHUNK) % 251));
    sent += n;
    if (sent >= SIZE) return res.end();
    setTimeout(tick, 50);
  };
  tick();
}

/** `/attach.bin`: a tiny `Content-Disposition: attachment` response — the commonest download link. */
function attachment(res) {
  const body = Buffer.from('sta attachment fixture\n');
  res.writeHead(200, {
    'Content-Type': 'application/octet-stream',
    'Content-Disposition': 'attachment; filename="attach.bin"',
    'Content-Length': String(body.length),
  });
  res.end(body);
}

/**
 * The fixture server: static files from `WWW`, `/slowpage` (1.5 s before the first byte),
 * `/slow.bin` and `/attach.bin`. Node, not python: a `spawn('python', …)` console window used to sit
 * on the desktop for the whole run. `port` 0 takes an ephemeral one.
 */
async function startFileServer(port, root = WWW) {
  const server = http.createServer((req, res) => {
    const url = new URL(req.url, 'http://127.0.0.1');
    if (url.pathname.startsWith('/slowpage')) {
      setTimeout(() => {
        const body = '<!doctype html><title>Slow page</title><h1>slow</h1>';
        res.writeHead(200, { 'Content-Type': 'text/html', 'Content-Length': String(Buffer.byteLength(body)) });
        res.end(body);
      }, 1500);
      return;
    }
    if (url.pathname.startsWith('/slow.bin')) return slowDownload(res);
    if (url.pathname.startsWith('/attach.bin')) return attachment(res);
    const file = path.join(root, path.normalize(decodeURIComponent(url.pathname)).replace(/^[\\/]+/, ''));
    if (!file.startsWith(path.resolve(root)) || !existsSync(file) || statSync(file).isDirectory()) {
      res.writeHead(404, { 'Content-Type': 'text/plain' });
      res.end('not found');
      return;
    }
    const body = readFileSync(file);
    res.writeHead(200, { 'Content-Type': MIME[path.extname(file).toLowerCase()] || 'application/octet-stream', 'Content-Length': String(body.length) });
    res.end(body);
  });
  await new Promise((ok, fail) => {
    server.once('error', fail);
    server.listen(port, '127.0.0.1', ok);
  });
  servers.push(server);
  return server;
}

/** A free port, reserved by binding and releasing it (the retry fixture starts mid-test). */
async function reservePort() {
  const probe = http.createServer(() => {});
  await new Promise((ok) => probe.listen(0, '127.0.0.1', ok));
  const port = probe.address().port;
  await new Promise((ok) => probe.close(ok));
  return port;
}

async function httpOk(url) {
  try {
    return (await fetch(url)).ok;
  } catch {
    return false;
  }
}

// ------------------------------------------------------------------------------------ transport

const targets = () => inst.targets();
const connect = (t) => inst.connect(t);

async function pageTarget(match) {
  const list = await targets();
  const t = list.find((t) => t.type === 'page' && (typeof match === 'function' ? match(t) : t.url === match || t.url.startsWith(match)));
  if (!t) throw new Error(`no target matching ${match}`);
  return t;
}

const evalIn = (match, expr, opts) => inst.eval(match, expr, opts);

/** `window.sta.invoke` in the sidebar — the real trusted-frame IPC path. */
const invoke = (cmd, payload = null) => inst.invoke(SB, cmd, payload);
const dispatch = (command) => invoke('dispatch', command);
const info = () => inst.info();
const st = () => inst.state();
const counts = () => inst.counts();
const count = async (type) => (await counts())[type] || 0;
/** Overlay host entry of `debug.info` (`overlays.hosts`; older builds returned a plain array). */
const overlayOf = (i, name) => (i.overlays.hosts || i.overlays).find((o) => o.overlay === name);

/** Every TabView in a UiState (favorites, pinned/today incl. folders and split panes, Peek). */
function allTabs(s) {
  const out = [];
  const walk = (nodes) => {
    for (const n of nodes || []) {
      if (n.kind === 'tab') out.push(n);
      else if (n.kind === 'folder') walk(n.children);
      else if (n.kind === 'split') out.push(...n.panes);
    }
  };
  out.push(...(s.favorites || []));
  for (const sp of s.spaces || []) {
    walk(sp.pinned);
    walk(sp.today);
  }
  if (s.peek) out.push(s.peek.tab);
  return out;
}
const tabById = (s, id) => allTabs(s).find((t) => t.id === id);

async function waitState(pred, timeoutMs = 6000) {
  return waitFor(async () => {
    const s = await st();
    return pred(s) ? s : null;
  }, timeoutMs, 120);
}

/** Opens `url` as a new foreground tab and waits until it finished loading. */
async function openTab(url, { load = true } = {}) {
  await dispatch({ type: 'openInput', text: url, target: 'newTab' });
  const s = await waitState((s) => s.current && s.current.url === url && (!load || !s.current.loading), 10000);
  if (!s) throw new Error(`tab for ${url} did not open`);
  if (load) await waitFor(async () => (await targets()).some((t) => t.type === 'page' && t.url === url), 5000);
  return s.current.tab;
}

async function click(t, selector, button = 'left') {
  const c = await connect(t);
  const rect = await evalIn(t, `(function () { var r = document.querySelector(${JSON.stringify(selector)}).getBoundingClientRect(); return { x: r.left + r.width / 2, y: r.top + r.height / 2 }; })()`);
  const buttons = { left: 1, right: 2, middle: 4 }[button];
  await c.send('Input.dispatchMouseEvent', { type: 'mouseMoved', x: rect.x, y: rect.y });
  await c.send('Input.dispatchMouseEvent', { type: 'mousePressed', x: rect.x, y: rect.y, button, buttons, clickCount: 1 });
  await c.send('Input.dispatchMouseEvent', { type: 'mouseReleased', x: rect.x, y: rect.y, button, buttons: 0, clickCount: 1 });
}

/** `#aarrggbb` from `debug.info` → `#rrggbb` as `test_pixels` reports it. */
const argbHex = (c) => `#${String(c).slice(3)}`;

/** CDP modifier bits (`Input.dispatchMouseEvent`). */
const MOD = { alt: 1, ctrl: 2, shift: 8 };

/**
 * A **trusted** click on the element `links.html` names `name`, with modifiers held. CDP input is
 * the only way to hold Alt without the OS foreground, and Chromium marks it `isTrusted` exactly as
 * it marks a real click — which is what the renderer's gesture listener insists on.
 */
async function modClick(t, name, { button = 'left', modifiers = MOD.alt } = {}) {
  const c = await connect(t);
  const rect = await evalIn(t, `window.__rect(${JSON.stringify(name)})`);
  if (!rect) throw new Error(`links.html has no ${name}`);
  const buttons = { left: 1, right: 2, middle: 4 }[button];
  await c.send('Input.dispatchMouseEvent', { type: 'mouseMoved', x: rect.x, y: rect.y, modifiers });
  await c.send('Input.dispatchMouseEvent', { type: 'mousePressed', x: rect.x, y: rect.y, button, buttons, clickCount: 1, modifiers });
  await c.send('Input.dispatchMouseEvent', { type: 'mouseReleased', x: rect.x, y: rect.y, button, buttons: 0, clickCount: 1, modifiers });
}

// ------------------------------------------------------------------------------------ scenarios

async function popups() {
  const S = 'popups';
  const opener = await openTab(`${H}/opener.html`);
  const openerT = await pageTarget(`${H}/opener.html`);
  const adoptedBefore = await count('popupAdopted');

  // window.open without features → foreground Today tab, window.opener kept.
  await evalIn(openerT, `window.open('${H}/popup.html?plain'); 'ok'`, { gesture: true });
  let s = await waitState((s) => s.current && s.current.url === `${H}/popup.html?plain`, 8000);
  check(S, 'window.open() → PopupAdopted → foreground Today tab', s && s.current.tab !== opener && (await count('popupAdopted')) === adoptedBefore + 1, s && s.current);
  const plainTab = s && s.current.tab;
  const i1 = await info();
  const plainInfo = plainTab && tabInfo(i1, plainTab);
  check(S, 'adopted popup lives in its own wrapper, visible, web (not internal)', plainInfo && plainInfo.visible && !plainInfo.internal && plainInfo.browserId, plainInfo);
  const msg1 = await waitFor(async () => (await evalIn(openerT, 'window.__messages')).find((m) => m.from.includes('?plain')), 5000);
  check(S, 'popup can postMessage to its opener (window.opener kept)', msg1, msg1);
  const plainT = await pageTarget(`${H}/popup.html?plain`);
  check(S, 'popup page has window.opener', await evalIn(plainT, '!!window.opener'));
  if (s) {
    const today = s.spaces.find((sp) => sp.id === s.activeSpace).today.map((n) => n.id);
    check(S, 'popup tab placed directly below its opener in Today', today.indexOf(plainTab) === today.indexOf(opener) + 1, today);
  }

  // window.open with features → popup → Peek (popup Peek).
  await dispatch({ type: 'activateItem', id: opener });
  await waitState((s) => s.current && s.current.tab === opener);
  await evalIn(openerT, `window.__feature = window.open('${H}/popup.html?feature', 'feat', 'width=420,height=320'); 'ok'`, { gesture: true });
  s = await waitState((s) => s.peek && s.peek.tab.url === `${H}/popup.html?feature`, 8000);
  check(S, 'window.open(url, name, "width=…") → popup Peek', s && s.peek.popup === true, s && s.peek);
  const featureTab = s && s.peek.tab.id;
  let i = await waitFor(async () => {
    const x = await info();
    const t = tabInfo(x, featureTab);
    return t && t.inPeek && overlayOf(x, 'Peek').visible && x;
  }, 5000);
  check(S, 'Peek overlay visible with the popup view moved in', i);
  const msg2 = await waitFor(async () => (await evalIn(openerT, 'window.__messages')).find((m) => m.from.includes('?feature')), 5000);
  check(S, 'feature popup posts to its opener too', msg2, msg2);
  console.log('  ' + (await capture('popup-peek')));

  // The opener closes its popup (window.close from script): browser gone → core closes Peek.
  const closedBefore = await count('tabBrowserClosed');
  await evalIn(openerT, `window.__feature.close(); 'ok'`);
  s = await waitState((s) => !s.peek, 8000);
  i = await waitFor(async () => {
    const x = await info();
    return !tabInfo(x, featureTab) && x.tabs.closing.length === 0 && x;
  }, 6000);
  check(S, 'window.close() of a Peek popup: view detached from Peek, TabBrowserClosed once, Peek closed', s && i && (await count('tabBrowserClosed')) === closedBefore + 1);

  // DestroyBrowser for a view parented in Peek (closePeek).
  await evalIn(openerT, `window.__feature2 = window.open('${H}/popup.html?feature2', 'feat2', 'width=420,height=320'); 'ok'`, { gesture: true });
  s = await waitState((s) => s.peek && s.peek.tab.url === `${H}/popup.html?feature2`, 8000);
  const feature2 = s && s.peek.tab.id;
  await waitFor(async () => tabInfo(await info(), feature2)?.inPeek, 4000);
  const closed2 = await count('tabBrowserClosed');
  await dispatch({ type: 'closePeek' });
  i = await waitFor(async () => {
    const x = await info();
    return !tabInfo(x, feature2) && x.tabs.closing.length === 0 && !overlayOf(x, 'Peek').visible && x;
  }, 6000);
  check(S, 'closePeek → DestroyBrowser of a Peek-parented view works (removed from Peek, closed)', i && (await count('tabBrowserClosed')) === closed2 + 1 && !(await targets()).some((t) => t.url.includes('feature2')));

  // target=_blank link.
  await dispatch({ type: 'activateItem', id: opener });
  await waitState((s) => s.current && s.current.tab === opener);
  await evalIn(openerT, `document.getElementById('blank').click(); 'ok'`, { gesture: true });
  s = await waitState((s) => allTabs(s).some((t) => t.url === `${H}/b.html`), 8000);
  check(S, 'target=_blank link opens a new tab', s && s.current.url === `${H}/b.html`, s && s.current);

  // Middle-click → on_open_urlfrom_tab → LinkOpenRequested{BackgroundTab}.
  await dispatch({ type: 'activateItem', id: opener });
  await waitState((s) => s.current && s.current.tab === opener);
  await sleep(300);
  const linkBefore = await count('linkOpenRequested');
  await click(openerT, '#mid', 'middle');
  s = await waitState((s) => allTabs(s).some((t) => t.url === `${H}/c.html`), 8000);
  check(S, 'middle-click opens the link in a background tab', s && s.current.tab === opener && (await count('linkOpenRequested')) === linkBefore + 1, s && s.current && s.current.url);
  check(S, 'the opener page did not navigate', (await evalIn(openerT, 'location.href')) === `${H}/opener.html`);

  // Web content can never open sta://.
  const adopted = await count('popupAdopted');
  await evalIn(openerT, `window.open('sta://settings/'); 'ok'`, { gesture: true });
  await sleep(1000);
  // (Chromium's DISPLAY_ISOLATED check usually refuses it in the renderer; on_before_popup blocks the rest.)
  check(S, 'window.open("sta://…") is blocked', (await count('popupAdopted')) === adopted && (await count('openUrl')) === 0 && !(await targets()).some((t) => t.url.startsWith('sta://settings')));

  // Document Picture-in-Picture keeps CEF's own window (never adopted as a tab).
  const pipAdopted = await count('popupAdopted');
  const pip = await evalIn(openerT, `(window.documentPictureInPicture ? documentPictureInPicture.requestWindow({ width: 320, height: 200 }).then(function (w) { window.__pip = w; w.document.body.textContent = 'pip'; return 'opened'; }, function (e) { return 'err:' + e.message; }) : Promise.resolve('unsupported'))`, { gesture: true });
  if (pip === 'opened') {
    await sleep(800);
    check(S, 'document PiP window opens without PopupAdopted', (await count('popupAdopted')) === pipAdopted && (await evalIn(openerT, '!!window.__pip && !window.__pip.closed')));
    await evalIn(openerT, `window.__pip.close(); 'ok'`);
    const pipClosed = await waitFor(async () => evalIn(openerT, 'window.__pip.closed'), 4000);
    check(S, 'document PiP window closes; opener tab unaffected', pipClosed && (await st()).current.tab === opener);
  } else {
    check(S, `document PiP not available here (${pip}); nothing adopted`, (await count('popupAdopted')) === pipAdopted, pip);
  }

  // Context menu (scripted, debug hook): link items present; "Open Link in New Tab" runs.
  const menuLink = await count('linkOpenRequested');
  await click(openerT, '#ctx', 'right');
  s = await waitState((s) => allTabs(s).some((t) => t.url === `${H}/d.html`), 6000);
  const menuLine = log().split('\n').find((l) => l.includes('context menu (tab):') && l.includes('Open Link in New Tab'));
  check(S, 'tab context menu has Open Link in New Tab / Open Link in Peek / Copy Link Address / Inspect', menuLine && menuLine.includes('Open Link in Peek') && menuLine.includes('Copy Link Address') && menuLine.includes('Inspect'), menuLine);
  check(S, 'context menu "Open Link in New Tab" → LinkOpenRequested background tab', s && s.current.tab === opener && (await count('linkOpenRequested')) === menuLink + 1);
  return { opener, openerT };
}

async function intercept() {
  const S = 'intercept';
  // A pinned tab: a cross-site link click (user gesture) is cancelled in on_before_browse and
  // reported as LinkOpenRequested{PinnedCrossSite} → Peek.
  const tab = await openTab(`${H}/opener.html?pinned`);
  await dispatch({ type: 'togglePin', id: tab });
  await waitState((s) => tabById(s, tab)?.section === 'pinned', 5000);
  const t = await pageTarget(`${H}/opener.html?pinned`);
  const before = await count('linkOpenRequested');
  const cross = `http://localhost:${HTTP_PORT}/c.html?cross`;
  await evalIn(t, `(function () { var a = document.createElement('a'); a.id = 'cross'; a.href = ${JSON.stringify(cross)}; a.textContent = 'cross'; document.body.appendChild(a); a.click(); return 'ok'; })()`, { gesture: true });
  const s = await waitState((s) => s.peek && s.peek.tab.url === cross, 8000);
  check(S, 'pinned tab: cross-site link → cancelled + LinkOpenRequested{PinnedCrossSite} → Peek', s && (await count('linkOpenRequested')) === before + 1 && log().includes('disposition: PinnedCrossSite'), s && s.peek);
  check(S, 'the pinned tab itself did not navigate', (await evalIn(t, 'location.href')) === `${H}/opener.html?pinned`);
  await dispatch({ type: 'closePeek' });
  await waitState((s) => !s.peek, 5000);
  // Same-site navigation stays in the pinned tab.
  await evalIn(t, `location.href = '${H}/b.html?same'; 'ok'`, { gesture: true });
  const same = await waitState((s) => tabById(s, tab)?.url === `${H}/b.html?same`, 6000);
  check(S, 'pinned tab: same-site navigation stays in place', same && !same.peek);
  await dispatch({ type: 'togglePin', id: tab });
  await waitState((s) => tabById(s, tab)?.section === 'today', 5000);
  await dispatch({ type: 'closeItem', id: tab });
}

/**
 * Alt+click (and Alt+middle-click) on a link previews it in Peek (PROTOCOL §13): the renderer's
 * capture-phase listener cancels the click — which is what stops Chromium's Alt+click *download* —
 * and reports the URL as `LinkOpenRequested{preview}`.
 */
async function preview() {
  const S = 'preview';
  // What this section must hand back untouched: the sections after it run against the tab that was
  // active here (a permission request from a *background* tab waits until that tab is shown, so
  // leaving another tab in front silently changes what `downloads` measures).
  const wasActive = (await st()).activeItem;
  const wasToday = new Set(((await st()).spaces[0].today || []).map((n) => n.id));
  const tab = await openTab(`${H}/links.html`);
  const t = await pageTarget(`${H}/links.html`);
  const at = `${H}/links.html`;
  const downloadsBefore = ((await st()).downloads || []).length;
  const filesBefore = existsSync(DL) ? readdirSync(DL).join('|') : '';

  /** Alt+clicks `name` and answers what the browser did: `{peek, url, toast, today, downloads}`. */
  async function gesture(name, opts, { settle = 1200 } = {}) {
    await dispatch({ type: 'closePeek' });
    await waitState((s) => !s.peek, 4000);
    const toastBefore = (await st()).toast?.id;
    const before = await st();
    await modClick(t, name, opts);
    await sleep(settle);
    const s = await st();
    const toast = s.toast && s.toast.id !== toastBefore ? s.toast.message : null;
    const url = await evalIn(t, 'location.href').catch(() => '?');
    const out = {
      peek: s.peek ? s.peek.tab.url : null,
      popupPeek: !!(s.peek && s.peek.popup),
      toast,
      url,
      title: await evalIn(t, 'document.title').catch(() => '?'),
      today: (s.spaces[0].today || []).length,
      todayGrew: (s.spaces[0].today || []).length - (before.spaces[0].today || []).length,
      downloads: (s.downloads || []).length,
    };
    if (out.url !== at) {
      await evalIn(t, `location.href = ${JSON.stringify(at)}; 'back'`).catch(() => null);
      await waitFor(async () => (await evalIn(t, 'location.href').catch(() => '')) === at, 8000, 150);
    }
    return out;
  }

  const linkOpensBefore = await count('linkOpenRequested');
  const plain = await gesture('plain');
  check(S, 'Alt+click on a link opens it in Peek (Split/Expand, not a popup Peek)', plain.peek === `${H}/b.html?preview` && !plain.popupPeek, plain);
  check(S, 'the page it came from neither navigated nor downloaded, and no tab was opened', plain.url === at && plain.downloads === downloadsBefore && plain.todayGrew === 0, plain);
  check(S, 'it arrives as one LinkOpenRequested{preview}', (await count('linkOpenRequested')) === linkOpensBefore + 1 && log().includes('disposition: Preview'), await count('linkOpenRequested'));

  for (const [name, expected, what] of [
    ['blank', `${H}/b.html?blank`, 'a target=_blank link previews instead of opening its own tab'],
    ['svga', `${H}/b.html?svg`, 'an SVG <a> previews'],
    ['shadow', `${H}/b.html?shadow`, 'a link inside a shadow tree previews'],
    ['dlattr', `${H}/b.html?dl`, 'a download= anchor previews and downloads nothing'],
    ['sub', `${H}/b.html?iframe`, 'a link inside an iframe previews'],
    // A different host is a different site: this frame has its own render process, which only knows
    // it belongs to a web tab from the `extra_info` its browser was created with.
    [`xo`, `http://localhost:${HTTP_PORT}/b.html?xo`, 'a link inside a CROSS-ORIGIN iframe previews too'],
  ]) {
    const r = await gesture(name);
    check(S, what, r.peek === expected && r.downloads === downloadsBefore && r.todayGrew === 0, r);
  }

  const mid = await gesture('plain', { button: 'middle' });
  check(S, 'Alt+middle-click previews too', mid.peek === `${H}/b.html?preview` && mid.todayGrew === 0, mid);

  for (const [name, what] of [
    ['nohref', 'an anchor without an href does nothing'],
    ['notalink', 'Alt+click on something that is not a link does nothing'],
  ]) {
    const r = await gesture(name);
    // Only *this* gesture's outcome matters: an unrelated toast (extensions another program added)
    // can be up at any moment, so the assertion is that nothing was refused or opened.
    check(S, what, r.peek === null && !/^Blocked a/.test(r.toast || '') && r.url === at && r.todayGrew === 0 && r.downloads === downloadsBefore, r);
  }

  for (const [name, message, what] of [
    ['mail', 'Blocked a mailto: link', 'an external protocol is refused, not handed to the OS'],
    ['internal', 'Blocked a sta: link', 'an sta:// link is refused'],
  ]) {
    const r = await gesture(name);
    check(S, what, r.peek === null && r.toast === message && r.url === at && r.todayGrew === 0, r);
  }
  check(S, 'and no external protocol was handed to the OS', !/external protocol \((link|navigation|test)/.test(log()));

  // A link with nothing to preview still reaches the page — the button idiom of web apps is
  // `javascript:void(0)` or `href="#"` with the page's own click handler, and an Alt+click that never
  // arrives is an app button that silently stopped working. What must *not* happen is Chromium
  // getting the click back: it answers an Alt+click by saving the page to disk (measured), so
  // everything except a `javascript:` href is cancelled first.
  for (const [name, what] of [
    ['jsl', 'a javascript: link reaches the page as a click instead of being refused with a toast'],
    ['apphash', 'an app button (href="#") runs the page\'s own handler instead of previewing the page it is on'],
    ['appjs', 'an app button (javascript:void(0)) runs the page\'s own handler and is not refused'],
  ]) {
    await evalIn(t, `window.__ran = []; 'ok'`);
    const r = await gesture(name);
    const ran = await evalIn(t, 'JSON.stringify(window.__ran)').catch(() => '?');
    check(S, what, r.peek === null && ran === `["${name}"]` && !/^Blocked a/.test(r.toast || '') && r.url === at && r.downloads === downloadsBefore, { r, ran });
  }
  await evalIn(t, `document.title = 'Links'; 'ok'`);
  await evalIn(t, `window.__ran = []; 'ok'`);
  const frag = await gesture('frag');
  check(
    S,
    'an in-page fragment link previews nothing and, above all, saves nothing to disk',
    frag.peek === null && frag.url === at && frag.todayGrew === 0 && frag.downloads === downloadsBefore,
    frag,
  );
  // An href longer than the renderer's cap: the click stays cancelled (letting Chromium have it back
  // would save the page), the renderer drops the report (a stderr warning), and the link does
  // nothing at all — never a file on disk.
  const longCalls = await count('linkOpenRequested');
  const long = await gesture('longurl');
  check(
    S,
    'an href over the cap reaches core not at all and downloads nothing',
    long.peek === null && long.url === at && long.downloads === downloadsBefore && (await count('linkOpenRequested')) === longCalls && !/^Blocked a/.test(long.toast || ''),
    long,
  );

  // The listener takes Alt and nothing else: the other modified clicks are untouched.
  const shift = await gesture('plain', { modifiers: MOD.shift });
  check(S, 'Shift+click still peeks through its own path (NewWindow)', shift.peek === `${H}/b.html?preview`, shift);
  const ctrl = await gesture('plain', { modifiers: MOD.ctrl });
  check(S, 'Ctrl+click still opens a background tab', ctrl.peek === null && ctrl.todayGrew === 1 && ctrl.url === at, ctrl);
  const none = await gesture('plain', { modifiers: 0 }, { settle: 2000 });
  check(S, 'a plain click still navigates the tab', none.peek === null && none.todayGrew === 0, none);

  // A page can neither fake the gesture nor steer it by redefining what the listener reads.
  await dispatch({ type: 'closePeek' });
  await waitState((s) => !s.peek, 4000);
  const before = await count('linkOpenRequested');
  await evalIn(t, `document.getElementById('plain').dispatchEvent(new MouseEvent('click', { bubbles: true, cancelable: true, altKey: true })); 'sent'`);
  await sleep(1200);
  let s = await st();
  check(S, "a page's own synthetic Alt+click opens no Peek (the event is not trusted)", !s.peek && (await count('linkOpenRequested')) === before, { peek: s.peek?.tab.url, calls: await count('linkOpenRequested') });
  await evalIn(t, `location.href = ${JSON.stringify(at)}; 'back'`).catch(() => null);
  await waitFor(async () => (await evalIn(t, 'location.href').catch(() => '')) === at, 8000, 150);
  const patched = await evalIn(
    t,
    `(function () {
       var r = { proto: 'ok', instance: 'ok' };
       try { Object.defineProperty(Event.prototype, 'isTrusted', { configurable: true, get: function () { return true; } }); } catch (e) { r.proto = 'threw'; }
       try { Object.defineProperty(MouseEvent.prototype, 'altKey', { configurable: true, get: function () { return true; } }); } catch (e) { r.instance = 'threw'; }
       document.getElementById('plain').dispatchEvent(new MouseEvent('click', { bubbles: true, cancelable: true }));
       return r;
     })()`,
  );
  await sleep(1200);
  s = await st();
  check(S, 'nor after redefining Event.prototype.isTrusted and MouseEvent.prototype.altKey', !s.peek, { patched, peek: s.peek?.tab.url });
  await evalIn(t, `location.href = ${JSON.stringify(at)}; 'back'`).catch(() => null);
  await waitFor(async () => (await evalIn(t, 'location.href').catch(() => '')) === at, 8000, 150);
  // …and the patched page's *real* plain click must still navigate rather than preview: the
  // listener reads the `altKey` getter it captured before any page script ran.
  const afterPatch = await gesture('plain', { modifiers: 0 }, { settle: 2000 });
  check(S, 'a real plain click in the patched page navigates, it does not preview', afterPatch.peek === null, afterPatch);

  // A **sandboxed** frame has an opaque origin: its embedder took even `target=_blank` away from it,
  // so the gesture does not exist there and one user Alt+click cannot become a top-level page of the
  // frame's choosing. The frame cancels the click itself, so nothing downloads either.
  await evalIn(t, `window.__msgs = []; 'ok'`);
  const sand = await gesture('sbox');
  const msgs = await evalIn(t, 'JSON.stringify(window.__msgs)').catch(() => '?');
  check(
    S,
    'a link in a sandboxed frame gets no preview (the frame sees the click, nothing else happens)',
    sand.peek === null && msgs === '["sand"]' && sand.todayGrew === 0 && sand.downloads === downloadsBefore,
    { sand, msgs },
  );

  // It is an Alt+**click**: Blink's *keyboard* activation of a focused link produces an equally
  // trusted click (`detail === 0`), and pressing keys in a page is something a shipped agent tool
  // can do. Same key pair `automation/tools.rs::key_press` sends.
  await dispatch({ type: 'closePeek' });
  await waitState((x) => !x.peek, 4000);
  await evalIn(t, `window.__ran = []; document.getElementById('keyact').focus(); 'ok'`);
  const kc = await connect(t);
  for (const type of ['rawKeyDown', 'keyUp']) {
    await kc.send('Input.dispatchKeyEvent', { type, modifiers: MOD.alt, key: 'Enter', code: 'Enter', windowsVirtualKeyCode: 13, nativeVirtualKeyCode: 13 });
  }
  await sleep(1200);
  s = await st();
  const keyRan = await evalIn(t, 'JSON.stringify(window.__ran)').catch(() => '?');
  check(
    S,
    'Alt+Enter on a focused link activates the link in the page and opens no preview',
    !s.peek && keyRan === '["keyact"]' && s.downloads.length === downloadsBefore,
    { peek: s.peek?.tab.url, keyRan },
  );

  // Peek's own UX on a preview. Ctrl+O is the accelerator, so it runs through the real keyboard
  // table and handler (`debug.accelerator`) without needing the OS foreground.
  const ctrlO = await gesture('plain');
  check(S, 'a preview is open before Ctrl+O', ctrlO.peek === `${H}/b.html?preview`, ctrlO);
  await invoke('debug.accelerator', { key: 0x4f, ctrl: true });
  let expanded = await waitState((x) => !x.peek && x.current && x.current.url === `${H}/b.html?preview`, 8000);
  check(S, 'Ctrl+O expands the preview into a Today tab at the top', !!expanded && (expanded.spaces[0].today || [])[0].id === expanded.current.tab, expanded && expanded.current && expanded.current.url);
  if (expanded) await dispatch({ type: 'closeItem', id: expanded.current.tab });
  await dispatch({ type: 'activateItem', id: tab });
  await waitState((x) => x.current && x.current.tab === tab, 6000);

  // Expand into a split uses the tab the preview came from.
  const p = await gesture('plain');
  check(S, 'a fresh preview is open before Expand', p.peek === `${H}/b.html?preview`, p);
  const peekTab = (await st()).peek.tab.id;
  await dispatch({ type: 'expandPeek', split: true });
  s = await waitState((x) => !x.peek && x.current && x.current.splitPanes === 2, 8000);
  check(S, 'Ctrl+O / Expand turns the preview into a split pane next to its opener', !!s && (await evalIn(await pageTarget(`${H}/b.html?preview`), 'location.href')) === `${H}/b.html?preview`, s && s.current);
  check(S, 'the previewed page kept its browser (no reload)', !log().includes(`reloading ${peekTab}`));
  await dispatch({ type: 'separateAll', id: (await st()).activeItem });
  await waitState((x) => x.current && x.current.splitPanes === 0, 8000);

  // Alt+click *inside* Peek replaces the page Peek shows instead of nesting a second one.
  await dispatch({ type: 'activateItem', id: tab });
  await waitState((x) => x.current && x.current.tab === tab, 6000);
  const first = await gesture('plain');
  check(S, 'a preview to click inside', first.peek === `${H}/b.html?preview`, first);
  const inner = await pageTarget(`${H}/b.html?preview`);
  await evalIn(inner, `document.body.innerHTML = '<a id="deep" href="c.html?deep" style="display:block;width:280px;height:40px;background:#ffdd99">deeper</a>'; window.__rect = function (n) { var r = document.getElementById(n).getBoundingClientRect(); return { x: r.left + r.width / 2, y: r.top + r.height / 2 }; }; 'ok'`);
  await modClick(inner, 'deep');
  s = await waitState((x) => x.peek && x.peek.tab.url === `${H}/c.html?deep`, 8000);
  check(S, 'Alt+click inside Peek replaces the page it shows instead of nesting', !!s && !!s.peek, s && s.peek && s.peek.tab.url);
  check(S, 'and still only one Peek exists', !!s && !!s.peek && ((await st()).spaces[0].today || []).length === first.today, s && s.peek);

  // Esc closes a preview. Esc is not in the accelerator table (`debug.accelerator` answers 404 for
  // it): it runs through `keyboard.rs::escape_chain`, so only real keys reach it.
  const esc = await gesture('plain');
  check(S, 'a preview is open before Esc', esc.peek === `${H}/b.html?preview`, esc);
  const escClosed = await retryInterrupted(async () => {
    await inst.keys('escape');
    return await waitState((x) => !x.peek, 6000);
  });
  check(S, 'real Esc closes the preview', !!escClosed, escClosed && escClosed.peek);

  // The popup (sign-in) Peek rule, reached the way a user would: the Alt+click happens in the tab
  // *behind* the overlay, because activating that tab would close the Peek first.
  await dispatch({ type: 'activateItem', id: tab });
  await waitState((x) => x.current && x.current.tab === tab, 6000);
  await evalIn(t, `window.open('popup.html', 'w', 'width=420,height=320'); 'ok'`, { gesture: true });
  const pop = await waitState((x) => x.peek && x.peek.popup, 10000);
  check(S, 'a popup Peek (a sign-in window) is up', !!pop && pop.peek.tab.url === `${H}/popup.html`, pop && pop.peek);
  const todayBeforePopup = ((await st()).spaces[0].today || []).length;
  await modClick(t, 'plain');
  await sleep(1500);
  s = await st();
  check(
    S,
    'an Alt+click in the tab behind a popup Peek keeps the flow and opens a background tab instead',
    !!s.peek && s.peek.popup && s.peek.tab.url === `${H}/popup.html` && (s.spaces[0].today || []).length === todayBeforePopup + 1 && s.activeItem === tab,
    { peek: s.peek && s.peek.tab.url, popup: s.peek && s.peek.popup, today: (s.spaces[0].today || []).length, active: s.activeItem },
  );
  // …but an Alt+click *inside* it is the user asking for that Peek to move on.
  const pt = await pageTarget(`${H}/popup.html`);
  await evalIn(
    pt,
    `document.body.innerHTML = '<a id="go" href="c.html?popmove" style="display:block;width:280px;height:40px;background:#ccffcc">move on</a>'; window.__rect = function (n) { var r = document.getElementById(n).getBoundingClientRect(); return { x: r.left + r.width / 2, y: r.top + r.height / 2 }; }; 'ok'`,
  );
  await modClick(pt, 'go');
  s = await waitState((x) => x.peek && x.peek.tab.url === `${H}/c.html?popmove`, 8000);
  check(S, 'an Alt+click inside the popup Peek replaces it, and it stops being a popup Peek', !!s && !s.peek.popup, s && s.peek);
  await dispatch({ type: 'closePeek' });
  await waitState((x) => !x.peek, 5000);

  // With Peek off there is no overlay to preview into: a foreground tab, exactly like Shift+click.
  await dispatch({ type: 'updateSettings', patch: { peekEnabled: false } });
  await waitState((x) => x.settings.peekEnabled === false, 5000);
  const off = await gesture('plain', {}, { settle: 2000 });
  check(S, 'with peekEnabled off the gesture opens a foreground tab', off.peek === null && off.todayGrew === 1, off);
  const offCurrent = (await st()).current;
  check(S, 'and that tab is the previewed URL, in front', !!offCurrent && offCurrent.url === `${H}/b.html?preview`, offCurrent && offCurrent.url);
  if (offCurrent) await dispatch({ type: 'closeItem', id: offCurrent.tab });
  await dispatch({ type: 'updateSettings', patch: { peekEnabled: true } });
  await waitState((x) => x.settings.peekEnabled === true, 5000);
  await dispatch({ type: 'activateItem', id: tab });
  await waitState((x) => x.current && x.current.tab === tab, 6000);

  await dispatch({ type: 'closePeek' });
  await waitState((x) => !x.peek, 5000);
  const end = await st();
  check(S, 'nothing was ever downloaded by the whole section', (end.downloads || []).length === downloadsBefore && (await info()).downloadsInProgress === 0, (end.downloads || []).map((d) => d.url));
  const filesAfter = existsSync(DL) ? readdirSync(DL) : [];
  // `downloadDir` is set at startup (main), so this really is the folder the browser downloads into.
  check(S, 'and no file reached the download folder', filesAfter.join('|') === filesBefore && end.settings.downloadDir === DL, { filesAfter, downloadDir: end.settings.downloadDir });

  // Last, because it is the one gesture in this section that *does* download: a link the server
  // sends as an attachment cannot be previewed, so it downloads like a plain click and the preview
  // that has nothing to show throws itself away instead of leaving an empty card over the page.
  const att = await gesture('attach', {}, { settle: 2500 });
  const done = await waitState((x) => (x.downloads || []).some((d) => d.fileName === 'attach.bin' && d.state === 'complete') && !x.peek, 15000);
  check(S, 'an attachment link downloads, and no empty preview is left behind', !!done && (done.downloads || []).length === downloadsBefore + 1, { att, downloads: done && (done.downloads || []).map((d) => `${d.fileName}:${d.state}`), peek: done && done.peek });
  const attFiles = existsSync(DL) ? readdirSync(DL) : [];
  check(S, 'and the file it downloaded is in the download folder', attFiles.includes('attach.bin'), attFiles);
  await waitFor(async () => (await info()).downloadsInProgress === 0, 8000);
  return { tab, t, wasActive, wasToday };
}

/** Light and dark, 100 % and 150 %: what an Alt+click preview looks like. */
async function previewShots(ctx) {
  const S = 'preview.shots';
  const t = ctx.t;
  const shot = async (instance, name, target, i) => {
    await instance.capture(name);
    const peek = (i.overlays.hosts || i.overlays).find((o) => o.overlay === 'Peek');
    return peek;
  };
  for (const appearance of ['light', 'dark']) {
    await dispatch({ type: 'updateSettings', patch: { appearance } });
    await sleep(400);
    await dispatch({ type: 'closePeek' });
    await waitState((s) => !s.peek, 4000);
    await modClick(t, 'shot');
    const ok = await waitState((s) => s.peek && s.peek.tab.url === `${H}/shot.html`, 8000);
    await waitFor(async () => (await info()).rounded.peekMasks.filter((m) => m.visible).length === 4, 5000, 100);
    await sleep(700);
    const i = await info();
    const peek = await shot(inst, `preview-${appearance}`, t, i);
    const surface = argbHex(i.rounded.colors.surface);
    const [qx, qy, qw, qh] = peek.peekViewRect;
    const [corner, middle] = await inst.pixels(`preview-${appearance}`, [[qx + 1, qy + 1], [qx + Math.floor(qw / 2), qy + Math.floor(qh / 2)]]);
    check(S, `100 % ${appearance}: the preview card is drawn with rounded page corners over the previewed page`, !!ok && corner === surface && middle === '#3a6ea5', { appearance, corner, middle, surface, peekViewRect: peek.peekViewRect });
  }
  await dispatch({ type: 'updateSettings', patch: { appearance: 'system' } });
  await dispatch({ type: 'closePeek' });
  await waitState((s) => !s.peek, 4000);

  // 150 %: its own instance, because the scale is a command-line switch.
  const SCALE = 1.5;
  const hi = new Instance({ data: `${DATA}-scale150`, args: ['--force-device-scale-factor=1.5'] }).start('scale150');
  try {
    const up = await waitFor(async () => {
      try {
        return (await hi.info()).window ? true : null;
      } catch {
        return null;
      }
    }, 30000, 250);
    check(S, 'a second instance at --force-device-scale-factor=1.5 started', !!up);
    for (const appearance of ['light', 'dark']) {
      await hi.dispatch({ type: 'updateSettings', patch: { appearance } });
      await hi.dispatch({ type: 'openUrl', url: `${H}/links.html`, target: 'newTab' });
      await waitFor(async () => {
        const s = await hi.state();
        return s.current && s.current.url === `${H}/links.html` && !s.current.loading;
      }, 15000, 200);
      const ht = await waitFor(async () => (await hi.targets()).find((x) => x.url === `${H}/links.html`), 8000, 200);
      const c = await hi.connect(ht);
      const rect = await hi.eval(ht, `window.__rect('shot')`);
      for (const type of ['mousePressed', 'mouseReleased']) {
        await c.send('Input.dispatchMouseEvent', { type, x: rect.x, y: rect.y, button: 'left', buttons: type === 'mousePressed' ? 1 : 0, clickCount: 1, modifiers: MOD.alt });
      }
      const ok = await waitFor(async () => {
        const s = await hi.state();
        return s.peek && s.peek.tab.url === `${H}/shot.html` ? s : null;
      }, 10000, 200);
      await waitFor(async () => (await hi.info()).rounded.peekMasks.filter((m) => m.visible).length === 4, 5000, 100);
      await sleep(800);
      const i = await hi.info();
      await hi.capture(`preview150-${appearance}`);
      const peek = (i.overlays.hosts || i.overlays).find((o) => o.overlay === 'Peek');
      const surface = argbHex(i.rounded.colors.surface);
      const [qx, qy, qw, qh] = peek.peekViewRect;
      const pts = [[qx + 1, qy + 1], [qx + Math.floor(qw / 2), qy + Math.floor(qh / 2)]].map(([x, y]) => [Math.floor(x * SCALE), Math.floor(y * SCALE)]);
      const [corner, middle] = (await hi.t('test_pixels', { path: `${hi.data}-preview150-${appearance}.png`, points: pts, space: 'device' })).colors;
      check(S, `150 % ${appearance}: Alt+click previews and the card's corners are on device pixels`, !!ok && corner === surface && middle === '#3a6ea5', { appearance, corner, middle, surface, peekViewRect: peek.peekViewRect });
      await hi.dispatch({ type: 'closePeek' });
      await sleep(400);
    }
  } finally {
    hi.closeSockets();
    if (alive(hi.pid)) killTree(hi.pid);
  }

  // Put the browser back the way this section found it (see `preview`).
  await dispatch({ type: 'closePeek' });
  await waitState((s) => !s.peek, 4000);
  for (const n of ((await st()).spaces[0].today || [])) {
    if (!ctx.wasToday.has(n.id)) {
      await dispatch({ type: 'closeItem', id: n.id });
      await sleep(150);
    }
  }
  if (ctx.wasActive) await dispatch({ type: 'activateItem', id: ctx.wasActive });
  const back = await waitState((s) => s.activeItem === ctx.wasActive && (s.spaces[0].today || []).every((n) => ctx.wasToday.has(n.id)), 8000);
  check(S, 'the section leaves the tabs and the active item as it found them', !!back, back && { active: back.activeItem, today: (back.spaces[0].today || []).map((n) => n.id) });
}

async function contextMenusUi() {
  const S = 'menus';
  // UI surfaces: outside editable fields the model ends up empty, so no menu is ever shown
  // (run_context_menu, which logs "INFO context menu (ui)", is not reached).
  const shownLines = () => log().split('\n').filter((l) => l.includes('INFO  context menu (ui):'));
  const sb = await pageTarget(SB);
  const c = await connect(sb);
  const rightClick = async (x, y) => {
    for (const type of ['mousePressed', 'mouseReleased']) {
      await c.send('Input.dispatchMouseEvent', { type, x, y, button: 'right', buttons: type === 'mousePressed' ? 2 : 0, clickCount: 1 });
    }
  };
  // A plain text block on the sidebar page (the page's own contextmenu handlers are bypassed).
  await evalIn(sb, `(function () { var d = document.createElement('div'); d.id = '__e2e_text'; d.textContent = 'plain text'; d.style.cssText = 'position:fixed;left:10px;top:260px;width:200px;height:30px;z-index:99999;background:#888'; d.addEventListener('contextmenu', function (e) { e.stopImmediatePropagation(); }, true); document.body.appendChild(d); window.getSelection().removeAllRanges(); return 'ok'; })()`);
  const shownBefore = shownLines().length;
  await rightClick(60, 275);
  await sleep(800);
  const debugLine = log().split('\n').reverse().find((l) => l.includes('DEBUG context menu (ui):'));
  check(S, 'UI page right-click on text: default menu suppressed (no native menu)', shownLines().length === shownBefore && (!debugLine || /context menu \(ui\):\s*$/.test(debugLine)), debugLine);
  // An editable field in a UI page keeps only edit commands.
  await evalIn(sb, `(function () { var i = document.createElement('input'); i.id = '__e2e_input'; i.value = 'hello'; i.style.cssText = 'position:fixed;left:10px;top:200px;width:200px;height:30px;z-index:99999'; i.addEventListener('contextmenu', function (e) { e.stopImmediatePropagation(); }, true); document.body.appendChild(i); i.focus(); i.select(); return 'ok'; })()`);
  await rightClick(60, 215);
  const line = await waitFor(() => shownLines()[shownBefore], 3000);
  const plain = line && line.replace(/&/g, '');
  check(S, 'editable field in a UI page: only edit commands (undo/cut/copy/paste/select all)', plain && /Cut/.test(plain) && /Paste/.test(plain) && /Select all/i.test(plain) && !/Back|Reload|Print|View|Inspect/.test(plain), line);
  await evalIn(sb, `document.getElementById('__e2e_input').remove(); document.getElementById('__e2e_text').remove(); 'ok'`);
}

async function errors() {
  const S = 'errors';
  const url1 = 'http://127.0.0.1:1/';
  const tab = await openTab(url1);
  let s = await waitState((s) => s.current && s.current.tab === tab && s.current.loadError, 8000);
  check(S, 'load error → TabLoadFailed; tab keeps the failed URL', s && s.current.url === url1 && /ERR_UNSAFE_PORT/.test(s.current.loadError), s && s.current);
  const t1 = await pageTarget(url1);
  const page = await waitFor(async () => {
    const r = await evalIn(t1, `(function () { var m = document.querySelector('[data-sta-error]'); return m && { code: m.getAttribute('data-sta-error'), text: m.textContent, retry: !!document.getElementById('sta-retry'), proto: location.protocol, bg: getComputedStyle(document.body).backgroundColor }; })()`);
    return r;
  }, 5000);
  check(S, 'themed error page replaces Chromium error document (code, URL text, Retry)', page && page.retry && page.code === 'ERR_UNSAFE_PORT' && page.text.includes(url1) && page.proto === 'chrome-error:', page);
  check(S, 'error page title/address are not reported (title falls back to the host)', s && s.current.title === s.current.host && !/chrome-error|data:text/.test(log().split('\n').filter((l) => l.includes('TabAddressChanged')).join('\n')), s && s.current.title);
  console.log('  ' + (await capture('error-page')));

  // Escaping: a URL with markup is shown as text.
  const evil = `http://127.0.0.1:1/<img src=x onerror=alert(1)>`;
  await dispatch({ type: 'navigate', tab, url: evil });
  const escaped = await waitFor(async () => {
    const t = (await targets()).find((t) => t.type === 'page' && t.url.startsWith('http://127.0.0.1:1/%3Cimg'));
    if (!t) return null;
    return evalIn(t, `(function () { var m = document.querySelector('[data-sta-error]'); return m && { imgs: document.images.length, text: m.querySelector('.url').textContent }; })()`);
  }, 6000);
  check(S, 'failed URL rendered as text (no markup injection)', escaped && escaped.imgs === 0 && escaped.text.includes('<img') || (escaped && escaped.imgs === 0 && escaped.text.includes('%3Cimg')), escaped);

  // Retry after the server comes up.
  const url2 = `http://127.0.0.1:${RETRY_PORT}/ok.html`;
  await dispatch({ type: 'navigate', tab, url: url2 });
  s = await waitState((s) => s.current && s.current.url === url2 && s.current.loadError, 8000);
  check(S, 'connection refused → error state', s && /ERR_CONNECTION_REFUSED/.test(s.current.loadError), s && s.current.loadError);
  const t2 = await waitFor(async () => (await targets()).find((t) => t.type === 'page' && t.url === url2), 5000);
  const hasRetry = await waitFor(() => evalIn(t2, `!!document.getElementById('sta-retry')`), 5000);
  check(S, 'error page shown for the refused URL', hasRetry);
  await startFileServer(RETRY_PORT);
  const up = await waitFor(() => httpOk(url2), 8000, 200);
  check(S, 'retry server started', up);
  await evalIn(t2, `document.getElementById('sta-retry').click(); 'ok'`, { gesture: true }).catch(() => null);
  s = await waitState((s) => s.current && s.current.tab === tab && !s.current.loadError && s.current.title === 'OK page', 10000);
  check(S, 'Retry reloads the failed URL → page loads, error cleared, title reported again', s, s && s.current);
  const t3 = await pageTarget(url2);
  const hist = await (await connect(t3)).send('Page.getNavigationHistory');
  const urls = hist.entries.map((e) => e.url);
  check(S, 'history has no data:/chrome-error entries (error page was in place)', !urls.some((u) => u.startsWith('data:') || u.startsWith('chrome-error')) && urls[urls.length - 1] === url2, urls);
  await dispatch({ type: 'goBack', tab });
  s = await waitState((s) => s.current && s.current.tab === tab && s.current.url.startsWith('http://127.0.0.1:1/'), 8000);
  check(S, 'Back from the recovered page returns to the previous (failed) entry', s, s && s.current && s.current.url);
  await dispatch({ type: 'closeItem', id: tab });
}

async function downloads(openerT) {
  const S = 'downloads';
  rmSync(DL, { recursive: true, force: true });
  mkdirSync(DL, { recursive: true });
  await dispatch({ type: 'updateSettings', patch: { downloadDir: DL } });
  await waitState((s) => s.settings.downloadDir === DL);
  const before = (await st()).downloads.length;
  await evalIn(openerT, `document.getElementById('dl').click(); 'ok'`, { gesture: true });
  let s = await waitState((s) => s.downloads.length > before && s.downloads[0].state === 'inProgress' && s.downloads[0].receivedBytes > 0, 8000);
  const d0 = s && s.downloads[0];
  check(S, 'DownloadUpdated in progress (received < total, total known, path in download dir)', d0 && d0.receivedBytes < d0.totalBytes && d0.totalBytes === 5 * 1024 * 1024 && d0.fileName === 'slow.bin' && d0.path && d0.path.replace(/\\/g, '/').toLowerCase().startsWith(DL.toLowerCase()), d0);
  const id = d0 && d0.id;
  await dispatch({ type: 'downloadControl', id, action: 'pause' });
  s = await waitState((s) => s.downloads.find((d) => d.id === id)?.state === 'paused', 5000);
  const pausedAt = s && s.downloads.find((d) => d.id === id).receivedBytes;
  await sleep(800);
  s = await st();
  check(S, 'pause → state paused, progress stops', pausedAt !== undefined && s.downloads.find((d) => d.id === id).receivedBytes - pausedAt < 256 * 1024, { pausedAt, now: s.downloads.find((d) => d.id === id).receivedBytes });
  await dispatch({ type: 'downloadControl', id, action: 'resume' });
  s = await waitState((s) => s.downloads.find((d) => d.id === id)?.state === 'complete', 20000);
  const done = s && s.downloads.find((d) => d.id === id);
  check(S, 'resume → complete; file on disk has 5 MB', done && done.receivedBytes === 5 * 1024 * 1024 && existsSync(done.path) && statSync(done.path).size === 5 * 1024 * 1024, done);
  if (done) {
    // Mark-of-the-Web: written before the completion is reported. `test_zone_identifier` reads the
    // file's alternate stream in the browser process (what `Get-Content -Stream` used to do).
    const referrer = await evalIn(openerT, 'location.href');
    let zone = null;
    try {
      zone = await inst.zoneIdentifier(done.path);
    } catch (e) {
      zone = 'error: ' + e.message;
    }
    const expected = `[ZoneTransfer]\r\nZoneId=3\r\nReferrerUrl=${referrer}\r\nHostUrl=${H}/slow.bin\r\n`;
    check(S, 'completed file carries a Zone.Identifier stream (ZoneId=3, ReferrerUrl = page, HostUrl = download URL)', zone === expected, { zone, expected });
  }
  check(S, 'progress events were reported (several DownloadUpdated)', (await count('downloadUpdated')) >= 4, await count('downloadUpdated'));
  // Second download of the same name → de-duplicated path; cancel it. Chromium asks before a page
  // starts another automatic download (permission prompt, kind "other"): allow it.
  await evalIn(openerT, `document.getElementById('dl').click(); 'ok'`, { gesture: true });
  s = await waitState((s) => s.downloads.some((d) => d.id !== id && d.state === 'inProgress' && d.path) || s.permissionPrompts.length > 0, 8000);
  const multi = s && s.permissionPrompts.find((p) => JSON.stringify(p.kinds) === '["other"]');
  if (multi) {
    check(S, 'a second automatic download asks first (PermissionRequested kinds [other])', multi.origin === H, multi);
    await dispatch({ type: 'resolvePermission', id: multi.id, allow: true });
  }
  s = await waitState((s) => s.downloads.some((d) => d.id !== id && d.state === 'inProgress' && d.path), 8000);
  const d2 = s && s.downloads.find((d) => d.id !== id && d.state === 'inProgress');
  check(S, 'second download gets a unique file name', d2 && /slow \(1\)\.bin$/.test(d2.path), d2 && d2.path);
  if (d2) {
    await dispatch({ type: 'downloadControl', id: d2.id, action: 'cancel' });
    s = await waitState((s) => s.downloads.find((d) => d.id === d2.id)?.state === 'cancelled', 6000);
    check(S, 'cancel → state cancelled', s);
    // Retry: core turns it into StartDownload{tab, url} → host.start_download.
    const ids = new Set((await st()).downloads.map((d) => d.id));
    await dispatch({ type: 'downloadControl', id: d2.id, action: 'retry' });
    s = await waitState((s) => s.downloads.some((d) => !ids.has(d.id) && d.url === `${H}/slow.bin`), 8000);
    const d3 = s && s.downloads.find((d) => !ids.has(d.id));
    check(S, 'retry (StartDownload) starts a new download of the same URL', d3, d3);
    if (d3) {
      await dispatch({ type: 'downloadControl', id: d3.id, action: 'cancel' });
      await waitState((s) => s.downloads.find((d) => d.id === d3.id)?.state === 'cancelled', 6000);
    }
  }
  check(S, 'downloads land only in the configured folder', readdirSync(DL).every((f) => f.startsWith('slow')), readdirSync(DL));
}

async function replace() {
  const S = 'replace';
  const tab = await openTab(`${H}/b.html?replace`);
  let i = await info();
  const webBrowser = tabInfo(i, tab).browserId;
  const closedBefore = await count('tabBrowserClosed');
  await dispatch({ type: 'navigate', tab, url: 'sta://settings/' });
  i = await waitFor(async () => {
    const x = await info();
    const t = tabInfo(x, tab);
    return t && t.internal && t.browserId && t.browserId !== webBrowser && !x.browsers.live.some((b) => b.id === webBrowser) && x.tabs.closing.length === 0 && x;
  }, 8000);
  check(S, 'web → internal URL: ReplaceBrowser swaps in a trusted UI browser; old one closed silently', i && tabInfo(i, tab).visible && (await count('tabBrowserClosed')) === closedBefore && i.browsers.live.find((b) => b.id === tabInfo(i, tab).browserId)?.ui === true, i && tabInfo(i, tab));
  const internalBrowser = i && tabInfo(i, tab).browserId;
  const settingsIpc = await waitFor(async () => (await evalIn('sta://settings/', 'typeof window.__staQuery')) === 'function', 6000);
  check(S, 'the internal page has IPC', settingsIpc);
  await dispatch({ type: 'navigate', tab, url: `${H}/c.html?back` });
  i = await waitFor(async () => {
    const x = await info();
    const t = tabInfo(x, tab);
    return t && !t.internal && t.browserId !== internalBrowser && !x.browsers.live.some((b) => b.id === internalBrowser) && x.tabs.closing.length === 0 && x;
  }, 8000);
  const s = await waitState((s) => s.current && s.current.tab === tab && s.current.url === `${H}/c.html?back` && !s.current.loading, 8000);
  check(S, 'internal → web: replaced again, page loads, no TabBrowserClosed', i && s && (await count('tabBrowserClosed')) === closedBefore, i && tabInfo(i, tab));
  await dispatch({ type: 'closeItem', id: tab });
}

async function permissions() {
  const S = 'permissions';
  const tab = await openTab(`${H}/perm.html`);
  const t = await pageTarget(`${H}/perm.html`);
  await evalIn(t, `window.__gum = 'pending'; navigator.mediaDevices.getUserMedia({ audio: true }).then(function (s) { window.__gum = 'ok:' + s.getAudioTracks().length; }, function (e) { window.__gum = 'err:' + e.name; }); 'started'`, { gesture: true });
  const promptFor = (s) => s.permissionPrompts.find((p) => p.tab === tab);
  let s = await waitState((s) => promptFor(s), 8000);
  const p = s && promptFor(s);
  check(S, 'getUserMedia(audio) → PermissionRequested (microphone) in state.permissionPrompts', p && p.tab === tab && JSON.stringify(p.kinds) === '["microphone"]' && p.origin === H, p);
  if (p) {
    await dispatch({ type: 'resolvePermission', id: p.id, allow: true });
    const r = await waitFor(async () => {
      const v = await evalIn(t, 'window.__gum');
      return v !== 'pending' && v;
    }, 6000);
    check(S, 'resolvePermission allow → callback → getUserMedia resolves', r === 'ok:1', r);
    s = await st();
    check(S, 'prompt removed from state', !promptFor(s));
  }
  await evalIn(t, `window.__cam = 'pending'; navigator.mediaDevices.getUserMedia({ video: true }).then(function () { window.__cam = 'ok'; }, function (e) { window.__cam = 'err:' + e.name; }); 'started'`, { gesture: true });
  s = await waitState((s) => promptFor(s), 8000);
  const p2 = s && promptFor(s);
  check(S, 'getUserMedia(video) → camera prompt', p2 && JSON.stringify(p2.kinds) === '["camera"]', p2);
  if (p2) {
    await dispatch({ type: 'resolvePermission', id: p2.id, allow: false });
    const r = await waitFor(async () => {
      const v = await evalIn(t, 'window.__cam');
      return v !== 'pending' && v;
    }, 6000);
    check(S, 'resolvePermission deny → NotAllowedError', r === 'err:NotAllowedError', r);
  }
  await evalIn(t, `window.__geo = 'pending'; navigator.geolocation.getCurrentPosition(function () { window.__geo = 'ok'; }, function (e) { window.__geo = 'err:' + e.code; }, { timeout: 15000 }); 'started'`, { gesture: true });
  s = await waitState((s) => promptFor(s), 8000);
  const p3 = s && promptFor(s);
  check(S, 'geolocation → on_show_permission_prompt → geolocation prompt', p3 && JSON.stringify(p3.kinds) === '["geolocation"]', p3 || (await st()).permissionPrompts);
  if (p3) {
    await dispatch({ type: 'resolvePermission', id: p3.id, allow: false });
    const r = await waitFor(async () => {
      const v = await evalIn(t, 'window.__geo');
      return v !== 'pending' && v;
    }, 6000);
    check(S, 'prompt deny → PERMISSION_DENIED (code 1)', r === 'err:1', r);
    // Block without Remember is a one-off answer (DISMISS): the next request asks again.
    await evalIn(t, `window.__geo2 = 'pending'; navigator.geolocation.getCurrentPosition(function () { window.__geo2 = 'ok'; }, function (e) { window.__geo2 = 'err:' + e.code; }, { timeout: 15000 }); 'started'`, { gesture: true });
    s = await waitState((s) => promptFor(s), 8000);
    const p4 = s && promptFor(s);
    check(S, 'geolocation again after Block without Remember → prompts again', p4 && JSON.stringify(p4.kinds) === '["geolocation"]' && p4.id !== p3.id, p4 || { state: (await st()).permissionPrompts, geo2: await evalIn(t, 'window.__geo2') });
    if (p4) {
      // Block + Remember: a lasting DENY; the next request is answered without a prompt.
      await dispatch({ type: 'resolvePermission', id: p4.id, allow: false, remember: true });
      const r2 = await waitFor(async () => {
        const v = await evalIn(t, 'window.__geo2');
        return v !== 'pending' && v;
      }, 6000);
      check(S, 'Block + Remember → PERMISSION_DENIED', r2 === 'err:1', r2);
      const requestedBefore = await count('permissionRequested');
      await evalIn(t, `window.__geo3 = 'pending'; navigator.geolocation.getCurrentPosition(function () { window.__geo3 = 'ok'; }, function (e) { window.__geo3 = 'err:' + e.code; }, { timeout: 15000 }); 'started'`, { gesture: true });
      const r3 = await waitFor(async () => {
        const v = await evalIn(t, 'window.__geo3');
        return v !== 'pending' && v;
      }, 8000);
      const st3 = await st();
      check(S, 'remembered block: denied again without showing a prompt', r3 === 'err:1' && !promptFor(st3), { r3, prompts: st3.permissionPrompts, requested: (await count('permissionRequested')) - requestedBefore });
    }
  }
  // Camera: Block without Remember → the next getUserMedia asks again.
  await evalIn(t, `window.__cam2 = 'pending'; navigator.mediaDevices.getUserMedia({ video: true }).then(function () { window.__cam2 = 'ok'; }, function (e) { window.__cam2 = 'err:' + e.name; }); 'started'`, { gesture: true });
  s = await waitState((s) => promptFor(s), 8000);
  const p5 = s && promptFor(s);
  check(S, 'getUserMedia(video) after Block without Remember → prompts again', p5 && JSON.stringify(p5.kinds) === '["camera"]', p5 || (await st()).permissionPrompts);
  if (p5) {
    await dispatch({ type: 'resolvePermission', id: p5.id, allow: false });
    const r = await waitFor(async () => {
      const v = await evalIn(t, 'window.__cam2');
      return v !== 'pending' && v;
    }, 6000);
    check(S, 'camera Block without Remember → getUserMedia rejects (NotAllowedError)', r === 'err:NotAllowedError', r);
  }
  // A pending prompt is dismissed when the tab closes.
  await evalIn(t, `navigator.mediaDevices.getUserMedia({ audio: true }).catch(function () {}); 'started'`, { gesture: true });
  s = await waitState((s) => promptFor(s), 8000);
  await dispatch({ type: 'closeItem', id: tab });
  s = await waitState((s) => !promptFor(s) && !tabById(s, tab), 8000);
  check(S, 'closing the tab drops its pending prompt', s);
}

/**
 * One-off answers stay one-off: no auto-block after repeated one-off Blocks (Chromium's
 * PermissionDecisionAutoBlocker), Allow without Remember lasts only while a tab shows the origin,
 * remembered allows persist, and prompts nobody shows never leave a promise pending. Runs on
 * `http://localhost:<port>` so the remembered geolocation block of `permissions()` on H doesn't
 * interfere.
 */
async function permissionGrants() {
  const S = 'permissions.grants';
  const L = `http://localhost:${HTTP_PORT}`;
  const perm = (t, name) => evalIn(t, `navigator.permissions.query({ name: '${name}' }).then(function (s) { return s.state; }, function (e) { return 'err:' + e; })`);
  const promptOf = (tab, ms = 6000) => waitFor(async () => (await st()).permissionPrompts.find((p) => p.tab === tab), ms);
  const started = (t, v, js) => evalIn(t, `window.${v} = 'pending'; ${js}; 'started'`, { gesture: true });
  const geo = (t, v) => started(t, v, `navigator.geolocation.getCurrentPosition(function () { window.${v} = 'ok'; }, function (e) { window.${v} = 'err:' + e.code; }, { timeout: 8000 })`);
  const notif = (t, v) => started(t, v, `Notification.requestPermission().then(function (r) { window.${v} = r; }, function (e) { window.${v} = 'err:' + e; })`);
  const settled = (t, v, ms = 10000) => waitFor(async () => {
    const x = await evalIn(t, `window.${v}`);
    return x !== 'pending' && x;
  }, ms);
  const grants = async () => (await info()).permissions;
  const grantsFile = path.join(DATA, 'sta', 'one-time-permissions.json');
  const readGrantsFile = () => (existsSync(grantsFile) ? JSON.parse(readFileSync(grantsFile, 'utf8')).grants : null);

  // (1) Four consecutive one-off Blocks each show a prompt (no embargo after three dismissals).
  let url = `${L}/perm.html?autoblock`;
  let tab = await openTab(url);
  let t = await pageTarget(url);
  const clearsBefore = (await grants()).autoblockClears;
  for (const [kind, request, blocked] of [['geolocation', geo, 'err:1'], ['notifications', notif, 'default']]) {
    const seen = [];
    for (let i = 1; i <= 4; i++) {
      await request(t, `__ab${i}`);
      const p = await promptOf(tab);
      if (p) await dispatch({ type: 'resolvePermission', id: p.id, allow: false, remember: false });
      seen.push({ prompt: !!p && JSON.stringify(p.kinds) === `["${kind}"]`, result: await settled(t, `__ab${i}`) });
    }
    check(S, `${kind}: four consecutive one-off Blocks each show a prompt (no auto-block)`, seen.every((x) => x.prompt && x.result === blocked), seen);
    check(S, `${kind}: still asks afterwards (permission state "prompt")`, (await perm(t, kind)) === 'prompt');
  }
  check(S, 'the auto-blocker data of the origin was cleared after the dismissals', (await grants()).autoblockClears > clearsBefore, await grants());
  await dispatch({ type: 'closeItem', id: tab });
  await waitState((s) => !tabById(s, tab), 6000);

  // (2) Allow without Remember lasts while a tab shows the origin.
  url = `${L}/perm.html?once`;
  tab = await openTab(url);
  t = await pageTarget(url);
  await geo(t, '__once');
  let p = await promptOf(tab);
  check(S, 'geolocation prompt on localhost', p && JSON.stringify(p.kinds) === '["geolocation"]', p);
  if (p) await dispatch({ type: 'resolvePermission', id: p.id, allow: true, remember: false });
  const once = await settled(t, '__once');
  check(S, 'Allow without Remember grants it (no PERMISSION_DENIED)', once && once !== 'err:1', once);
  let g = await waitFor(async () => {
    const x = await grants();
    return x.oneTimeGrants.some((o) => o.origin === `${L}/` && o.bits === 256) && x;
  }, 3000);
  check(S, 'recorded as a one-time grant (debug.info) and on disk before answering', g && (readGrantsFile() || []).some((o) => o.origin === `${L}/` && o.topLevel === `${L}/` && o.bits === 256), { info: g, file: readGrantsFile() });
  const resetsBefore = (await grants()).grantResets;
  // Reload: the page stays on the origin, so the grant stays.
  await dispatch({ type: 'reload', tab });
  await waitFor(async () => (await evalIn(t, 'window.__once')) === undefined, 6000);
  await waitState((s) => s.current && s.current.tab === tab && !s.current.loading, 6000);
  const requestedBefore = await count('permissionRequested');
  await geo(t, '__again');
  const again = await settled(t, '__again');
  check(S, 'after a reload: still granted, no new prompt', (await perm(t, 'geolocation')) === 'granted' && again !== 'err:1' && (await count('permissionRequested')) === requestedBefore, { again, requested: (await count('permissionRequested')) - requestedBefore });
  // A second tab of the same origin keeps it alive when the first one closes.
  const url2 = `${L}/perm.html?second`;
  const tab2 = await openTab(url2);
  const t2 = await pageTarget(url2);
  await dispatch({ type: 'closeItem', id: tab });
  await waitState((s) => !tabById(s, tab), 6000);
  await sleep(600);
  check(S, 'closing one of two tabs of the origin keeps the grant', (await perm(t2, 'geolocation')) === 'granted' && (await grants()).grantResets === resetsBefore, await grants());
  // Closing the last tab of the origin resets it.
  await dispatch({ type: 'closeItem', id: tab2 });
  g = await waitFor(async () => {
    const x = await grants();
    return x.grantResets > resetsBefore && !x.oneTimeGrants.some((o) => o.origin === `${L}/`) && x;
  }, 6000);
  // The record stays on disk marked `resetAt` until Chromium has flushed the reset (crash safety).
  check(S, 'closing the last tab of the origin resets the grant (debug.info, file keeps it only as a reset record)', g && !(readGrantsFile() || []).some((o) => o.origin === `${L}/` && !o.resetAt), { info: g, file: readGrantsFile() });
  url = `${L}/perm.html?reopen`;
  tab = await openTab(url);
  t = await pageTarget(url);
  const state = await perm(t, 'geolocation');
  await geo(t, '__reopen');
  p = await promptOf(tab);
  check(S, 'reopening the origin: permission state "prompt" and a new prompt', state === 'prompt' && p && JSON.stringify(p.kinds) === '["geolocation"]', { state, prompt: p });
  if (p) await dispatch({ type: 'resolvePermission', id: p.id, allow: false, remember: false });
  await settled(t, '__reopen');

  // (3) Navigating the last tab of the origin elsewhere resets it too.
  await notif(t, '__n1');
  p = await promptOf(tab);
  if (p) await dispatch({ type: 'resolvePermission', id: p.id, allow: true, remember: false });
  check(S, 'notifications: Allow without Remember → granted', p && (await settled(t, '__n1')) === 'granted');
  const navResets = (await grants()).grantResets;
  await dispatch({ type: 'navigate', tab, url: `${H}/b.html?away` });
  g = await waitFor(async () => {
    const x = await grants();
    return x.grantResets > navResets && !x.oneTimeGrants.some((o) => o.origin === `${L}/`) && x;
  }, 6000);
  check(S, 'navigating the tab to another origin resets the grant', g, await grants());
  const back = `${L}/perm.html?back`;
  await dispatch({ type: 'navigate', tab, url: back });
  await waitState((s) => s.current && s.current.url === back && !s.current.loading, 8000);
  t = await pageTarget(back);
  check(S, 'back on the origin: notifications ask again', (await perm(t, 'notifications')) === 'prompt');

  // (4) Allow + Remember persists (no one-time record, no reset when the tab closes).
  await notif(t, '__n2');
  p = await promptOf(tab);
  if (p) await dispatch({ type: 'resolvePermission', id: p.id, allow: true, remember: true });
  check(S, 'notifications: Allow + Remember → granted, not recorded as one-time', p && (await settled(t, '__n2')) === 'granted' && !(await grants()).oneTimeGrants.some((o) => o.origin === `${L}/`));
  await dispatch({ type: 'closeItem', id: tab });
  await waitState((s) => !tabById(s, tab), 6000);
  await sleep(600);
  url = `${L}/perm.html?remembered`;
  tab = await openTab(url);
  t = await pageTarget(url);
  const remembered = await perm(t, 'notifications');
  const reqBefore = await count('permissionRequested');
  await notif(t, '__n3');
  check(S, 'remembered allow survives closing the tab: granted without a prompt', remembered === 'granted' && (await settled(t, '__n3')) === 'granted' && !(await st()).permissionPrompts.some((x) => x.tab === tab) && (await count('permissionRequested')) === reqBefore, { remembered });
  await dispatch({ type: 'closeItem', id: tab });

  // (5) Resetting every content setting the CEF request bits map to is safe (a wrong type would
  // CHECK-crash the browser process).
  const all = await invoke('debug.resetPermissions', { origin: `${L}/`, bits: 0x1fffffff });
  await sleep(500);
  check(S, 'debug.resetPermissions with every request bit: the browser stays alive', all.ok > 20 && (await st()).revision > 0, all);

  // (6) Prompts nobody shows (UI pages) settle instead of hanging.
  await dispatch({ type: 'openInternalPage', page: 'settings' });
  await waitFor(async () => (await targets()).some((x) => x.type === 'page' && x.url.startsWith('sta://settings/')), 8000);
  const ui = await evalIn('sta://settings/', `Promise.race([Notification.requestPermission().then(function (r) { return 'res:' + r; }), new Promise(function (ok) { setTimeout(function () { ok('pending'); }, 5000); })])`, { gesture: true });
  check(S, 'Notification.requestPermission() in a UI page settles (dismissed, not pending)', ui === 'res:default', ui);
  const uiGeo = await evalIn('sta://settings/', `new Promise(function (ok) { navigator.geolocation.getCurrentPosition(function () { ok('ok'); }, function (e) { ok('err:' + e.code); }); setTimeout(function () { ok('pending'); }, 5000); })`, { gesture: true });
  check(S, 'geolocation in a UI page settles with PERMISSION_DENIED', uiGeo === 'err:1', uiGeo);
  const settings = (await st()).current;
  if (settings && settings.url.startsWith('sta://settings')) await dispatch({ type: 'closeItem', id: settings.tab });
}

async function find() {
  const S = 'find';
  const tab = await openTab(`${H}/find.html`);
  await evalIn(SB, `window.__finds = []; window.__unsubFind = window.sta.on('find.result', function (r) { window.__finds.push(r); }); 'ok'`);
  await dispatch({ type: 'findInPage', tab, text: 'needle', forward: true, matchCase: false, findNext: false });
  const r = await waitFor(async () => (await evalIn(SB, 'window.__finds')).find((x) => x.final && x.tab === tab), 6000);
  check(S, 'find.result event {tab, count, active, final} reaches the UI', r && r.count === 3 && r.active >= 1, await evalIn(SB, 'window.__finds'));
  await dispatch({ type: 'findInPage', tab, text: 'needle', forward: true, matchCase: false, findNext: true });
  const r2 = await waitFor(async () => {
    const all = (await evalIn(SB, 'window.__finds')).filter((x) => x.final && x.tab === tab);
    return all.length >= 2 && all[all.length - 1];
  }, 6000);
  check(S, 'find next moves the active match', r2 && r2.count === 3 && r2.active === 2, r2);
  await evalIn(SB, `window.__finds.length = 0; 'ok'`);
  await dispatch({ type: 'findInPage', tab, text: 'needle', forward: true, matchCase: false, findNext: false });
  const r3 = await waitFor(async () => (await evalIn(SB, 'window.__finds')).find((x) => x.final && x.tab === tab), 6000);
  check(S, 're-issuing the same query restarts at the first match (result reported again)', r3 && r3.count === 3 && r3.active === 1, r3);
  await dispatch({ type: 'findInPage', tab, text: '', forward: true, matchCase: false, findNext: false });
  await evalIn(SB, `window.__unsubFind && window.__unsubFind(); 'ok'`);
  return tab;
}

async function zoom(tab) {
  const S = 'zoom';
  await dispatch({ type: 'activateItem', id: tab });
  await waitState((s) => s.current && s.current.tab === tab);
  const steps = [
    ['in', 110],
    ['in', 125],
    ['out', 110],
    ['out', 100],
    ['out', 90],
    ['reset', 100],
  ];
  for (const [direction, pct] of steps) {
    await dispatch({ type: 'zoom', direction });
    const s = await waitState((s) => s.current && s.current.zoomPercent === pct, 4000);
    check(S, `zoom ${direction} → ${pct}%`, s, s ? undefined : (await st()).current.zoomPercent);
  }
  await dispatch({ type: 'zoom', direction: 'in' });
  await waitState((s) => s.current.zoomPercent === 110);
  await dispatch({ type: 'reload' });
  await sleep(1500);
  const s = await st();
  check(S, 'zoom survives a reload (reported again on load end)', s.current.zoomPercent === 110, s.current.zoomPercent);
  await dispatch({ type: 'zoom', direction: 'reset' });
  await waitState((s) => s.current.zoomPercent === 100);
}

async function audio() {
  const S = 'audio';
  const tab = await openTab(`${H}/media.html`);
  const t = await pageTarget(`${H}/media.html`);
  const played = await evalIn(t, `document.getElementById('a').play().then(function () { return 'playing'; }, function (e) { return 'err:' + e.name; })`, { gesture: true });
  check(S, 'audio element plays', played === 'playing', played);
  let s = await waitState((s) => tabById(s, tab)?.audible === true, 5000);
  check(S, 'play → TabAudioChanged → audible true', s);
  await evalIn(t, `document.getElementById('a').pause(); 'ok'`);
  s = await waitState((s) => tabById(s, tab)?.audible === false, 5000);
  check(S, 'pause → audible false', s);
  await evalIn(t, `var a = document.getElementById('a'); a.muted = true; a.play(); 'ok'`, { gesture: true });
  await sleep(900);
  check(S, 'muted element playing is not audible', tabById(await st(), tab)?.audible === false);
  await evalIn(t, `document.getElementById('a').pause(); document.getElementById('a').muted = false; 'ok'`);
  await evalIn(t, `window.__detached = new Audio('tone.wav'); window.__detached.loop = true; window.__detached.play().then(function () { return 'ok'; })`, { gesture: true });
  s = await waitState((s) => tabById(s, tab)?.audible === true, 5000);
  check(S, 'detached new Audio() playing → audible', s);
  await evalIn(t, `window.__detached.pause(); 'ok'`);
  s = await waitState((s) => tabById(s, tab)?.audible === false, 5000);
  check(S, 'detached audio paused → not audible', s);
  const changes = await count('tabAudioChanged');
  check(S, 'TabAudioChanged only on changes (debounced)', changes >= 4 && changes <= 6, changes);
  await evalIn(t, `document.getElementById('a').play().then(function () { return 'ok'; })`, { gesture: true });
  await waitState((s) => tabById(s, tab)?.audible === true, 5000);
  await dispatch({ type: 'navigate', tab, url: `${H}/b.html` });
  s = await waitState((s) => tabById(s, tab)?.url === `${H}/b.html` && tabById(s, tab)?.audible === false, 6000);
  check(S, 'navigating away from a playing page → audible false', s);
  await dispatch({ type: 'closeItem', id: tab });
}

async function fullscreen() {
  const S = 'fullscreen';
  const tab = await openTab(`${H}/fs.html`);
  const t = await pageTarget(`${H}/fs.html`);
  const before = await count('tabFullscreenChanged');
  const r = await evalIn(t, `document.documentElement.requestFullscreen().then(function () { return 'ok'; }, function (e) { return 'err:' + e.message; })`, { gesture: true });
  let s = await waitState((s) => s.pageFullscreen === true, 5000);
  check(S, 'requestFullscreen (user gesture) → TabFullscreenChanged → pageFullscreen', s && (await count('tabFullscreenChanged')) > before, { r });
  let last;
  let i = await waitFor(async () => {
    const x = (last = await info());
    const ti = tabInfo(x, tab);
    return x.tabs.pageFullscreen && x.tabs.pageFullscreen.tab === tab && ti.visible && JSON.stringify(ti.viewOrigin) === '[0,0]' && x;
  }, 4000);
  check(S, 'set_page_fullscreen_tab: only that wrapper visible, border inset removed', i && i.tabs.tabs.filter((x) => x.visible).length === 1, last && { fs: last.tabs.pageFullscreen, tabs: last.tabs.tabs.map((x) => [x.tab, x.visible, x.viewOrigin, x.wrapperBounds]) });
  console.log('  ' + (await capture('fullscreen')));
  await evalIn(t, `document.exitFullscreen().then(function () { return 'ok'; }, function (e) { return 'err:' + e.message; })`);
  s = await waitState((s) => s.pageFullscreen === false, 5000);
  i = await waitFor(async () => {
    const x = await info();
    const ti = tabInfo(x, tab);
    return !x.tabs.pageFullscreen && JSON.stringify(ti.viewOrigin) === '[2,2]' && x;
  }, 4000);
  check(S, 'exitFullscreen → restored layout with the 2px inset', s && i, i && tabInfo(i, tab));
  // Esc / core-driven exit: ExitPageFullscreen effect.
  const again = await evalIn(t, `document.documentElement.requestFullscreen().then(function () { return 'ok'; }, function (e) { return 'err:' + e.message; })`, { gesture: true });
  const fsAgain = await waitState((s) => s.pageFullscreen === true, 5000);
  const other = await openTab(`${H}/b.html`);
  s = await waitState((s) => s.pageFullscreen === false && s.current.tab === other, 6000);
  // (document.fullscreenElement of the now hidden page only updates with its next rendering
  // opportunity, so check the browser side: CEF confirms the exit.)
  const confirmed = await waitFor(() => {
    const lines = log().split('\n');
    const exit = lines.findLastIndex((l) => l.includes(`effect ExitPageFullscreen { tab: ${tab} }`));
    return exit >= 0 && lines.slice(exit).some((l) => l.includes(`TabFullscreenChanged { tab: ${tab}, fullscreen: false }`));
  }, 4000);
  check(S, 'activating another tab exits page fullscreen (ExitPageFullscreen → host.exit_fullscreen → confirmed)', fsAgain && s && confirmed, { again, fsAgain: !!fsAgain, state: !!s, confirmed });
  await dispatch({ type: 'closeItem', id: other });
  await dispatch({ type: 'closeItem', id: tab });

  // Fullscreen from a Peek popup, then the Peek closes: the content layout comes back.
  const host = await openTab(`${H}/opener.html?fshost`);
  const hostT = await pageTarget(`${H}/opener.html?fshost`);
  await evalIn(hostT, `window.open('${H}/fs.html?peek', 'fsp', 'width=400,height=300'); 'ok'`, { gesture: true });
  s = await waitState((s) => s.peek && s.peek.tab.url === `${H}/fs.html?peek` && !s.peek.tab.loading, 8000);
  const peekTab = s && s.peek.tab.id;
  const peekT = await waitFor(async () => (await targets()).find((x) => x.url === `${H}/fs.html?peek`), 4000);
  await waitFor(async () => tabInfo(await info(), peekTab)?.inPeek, 4000);
  await evalIn(peekT, `document.documentElement.requestFullscreen().then(function () { return 'ok'; }, function (e) { return 'err:' + e.message; })`, { gesture: true });
  i = await waitFor(async () => {
    const x = await info();
    const pi = tabInfo(x, peekTab);
    return x.tabs.pageFullscreen && x.tabs.pageFullscreen.tab === peekTab && x.tabs.pageFullscreen.fromPeek && pi.visible && !pi.inPeek && !tabInfo(x, host).visible && x;
  }, 5000);
  check(S, 'Peek tab fullscreen: view moved out of Peek into its wrapper, alone in the content', i, i && i.tabs.pageFullscreen);
  await evalIn(peekT, `document.exitFullscreen().then(function () { return 'ok'; }, function (e) { return 'err:' + e.message; })`);
  i = await waitFor(async () => {
    const x = await info();
    return !x.tabs.pageFullscreen && tabInfo(x, peekTab)?.inPeek && overlayOf(x, 'Peek').visible && tabInfo(x, host).visible && x;
  }, 5000);
  check(S, 'exiting fullscreen puts the view back into the visible Peek', i && (await st()).peek?.tab.id === peekTab);
  await evalIn(peekT, `document.documentElement.requestFullscreen().then(function () { return 'ok'; }, function (e) { return 'err:' + e.message; })`, { gesture: true });
  await waitFor(async () => (await info()).tabs.pageFullscreen?.tab === peekTab, 5000);
  await dispatch({ type: 'closePeek' });
  i = await waitFor(async () => {
    const x = await info();
    return !tabInfo(x, peekTab) && !x.tabs.pageFullscreen && tabInfo(x, host).visible && x;
  }, 6000);
  s = await st();
  check(S, 'closing the fullscreen Peek restores the underlying tab layout', i && !s.pageFullscreen && !s.peek, i && i.tabs.tabs.map((x) => [x.tab, x.visible]));
  await dispatch({ type: 'closeItem', id: host });
}

async function boosts() {
  const S = 'boosts';
  const tab3 = await openTab(`http://localhost:${HTTP_PORT}/boost.html`);
  await evalIn(`http://localhost:${HTTP_PORT}/boost.html`, `window.__marker = 'kept'; 'ok'`);
  const tab = await openTab(`${H}/boost.html`);
  const t = await pageTarget(`${H}/boost.html`);
  const color = (target) => evalIn(target, `getComputedStyle(document.getElementById('t')).color`);
  check(S, 'baseline: heading not red', (await color(t)) !== 'rgb(255, 0, 0)');
  const boost = { id: 0, name: 'e2e', host: '127.0.0.1', enabled: true, css: '#t { color: rgb(255, 0, 0) !important; }', js: 'document.body.dataset.boosted = "yes"; // trailing comment', createdAt: 0, updatedAt: 0 };
  await dispatch({ type: 'upsertBoost', boost });
  const applied = await waitFor(async () => {
    const tt = await pageTarget(`${H}/boost.html`);
    const r = await evalIn(tt, `({ color: getComputedStyle(document.getElementById('t')).color, boosted: document.body.dataset.boosted, styles: document.querySelectorAll('style#sta-boost').length })`);
    return r.color === 'rgb(255, 0, 0)' && r.boosted === 'yes' && r.styles === 1 && r;
  }, 10000, 200);
  check(S, 'upsertBoost → affected tab reloads → CSS injected once + JS ran after DOMContentLoaded', applied, applied);
  const tab2 = await openTab(`${H}/boost2.html`);
  const t2 = await pageTarget(`${H}/boost2.html`);
  const fresh = await waitFor(async () => {
    const r = await evalIn(t2, `({ color: getComputedStyle(document.getElementById('t')).color, boosted: document.body.dataset.boosted, last: document.documentElement.lastElementChild.id })`);
    return r.color === 'rgb(255, 0, 0)' && r.boosted === 'yes' && r;
  }, 6000);
  check(S, 'new tab gets the boost from extra_info (style moved last at DOMContentLoaded)', fresh && fresh.last === 'sta-boost', fresh);
  // Non-matching host (this browser was created before the boost existed: stale extra_info).
  const t3 = await pageTarget(`http://localhost:${HTTP_PORT}/boost.html`);
  check(S, 'other host (localhost) is not boosted and was not reloaded', (await color(t3)) !== 'rgb(255, 0, 0)' && (await evalIn(t3, 'document.body.dataset.boosted')) === undefined && (await evalIn(t3, 'window.__marker')) === 'kept');
  // Renderers only get the boosts of their own host: the shell's record of what each renderer holds
  // matches the host-filtered list (1 boost for 127.0.0.1, none for localhost).
  const bi = await info();
  const b127 = tabInfo(bi, tab2)?.boosts;
  const bLocal = tabInfo(bi, tab3)?.boosts;
  check(S, 'boost lists are per host: 127.0.0.1 renderer holds 1 boost, localhost renderer none', b127 && b127.count === 1 && b127.sent === b127.expected && bLocal && bLocal.count === 0 && bLocal.sent === bLocal.expected && bLocal.expected !== b127.expected, { b127, bLocal });
  // That tab navigates to the boosted host: new site → new renderer process that only knows the
  // creation-time extra_info → sta.boosts.check → the browser answers → boost applied.
  const checksBefore = log().split('\n').filter((l) => l.includes('boosts: renderer of browser')).length;
  await dispatch({ type: 'navigate', tab: tab3, url: `${H}/boost.html?nav` });
  await waitState((s) => tabById(s, tab3)?.url === `${H}/boost.html?nav` && !s.current.loading, 6000);
  const t3b = await waitFor(async () => (await targets()).find((x) => x.url === `${H}/boost.html?nav`), 4000);
  const navBoosted = await waitFor(async () => {
    const r = await evalIn(t3b, `({ color: getComputedStyle(document.getElementById('t')).color, boosted: document.body.dataset.boosted })`);
    return r.color === 'rgb(255, 0, 0)' && r.boosted === 'yes' && r;
  }, 4000);
  const checked = log().split('\n').filter((l) => l.includes('boosts: renderer of browser')).length > checksBefore;
  check(S, 'navigation to a boosted host in a new renderer (stale extra_info) gets the boost via check', navBoosted && checked, { navBoosted, checked });
  // Toggle off → reload → not applied.
  const id = (await st()).boosts.find((b) => b.name === 'e2e').id;
  await dispatch({ type: 'toggleBoost', id });
  const off = await waitFor(async () => {
    const tt = await pageTarget(`${H}/boost.html`);
    const r = await evalIn(tt, `({ color: getComputedStyle(document.getElementById('t')).color, boosted: document.body.dataset.boosted, styles: document.querySelectorAll('style#sta-boost').length })`);
    return r.color !== 'rgb(255, 0, 0)' && r.boosted === undefined && r.styles === 0 && r;
  }, 10000, 200);
  check(S, 'toggleBoost off → reload → boost gone', off, off);
  check(S, 'boost scripts never reach UI pages', (await evalIn(SB, `document.querySelectorAll('style#sta-boost').length`)) === 0);
  await dispatch({ type: 'deleteBoost', id });
  for (const x of [tab, tab2, tab3]) await dispatch({ type: 'closeItem', id: x });
}

async function focus() {
  const S = 'focus';
  const tab = await openTab(`${H}/b.html?focus`);
  // The command bar keeps focus while the page underneath commits a navigation.
  await dispatch({ type: 'navigate', tab, url: `${H}/slowpage.html?1` });
  await sleep(200);
  await dispatch({ type: 'openCommandBar', mode: 'editUrl' });
  const opened = await waitFor(async () => overlayOf(await info(), 'CommandBar').visible, 4000);
  const loaded = await waitState((s) => s.current && s.current.url === `${H}/slowpage.html?1` && !s.current.loading, 8000);
  await sleep(600);
  const s = await st();
  check(S, 'a navigation committing under an open command bar does not steal focus / close it', opened && loaded && s.commandBar !== null, { opened: !!opened, loaded: !!loaded, commandBar: s.commandBar });
  await dispatch({ type: 'closeCommandBar' });
  await waitState((s) => s.commandBar === null, 4000);
  // A background tab that loads never takes focus from the visible one.
  const bgBefore = await count('tabFocused');
  await dispatch({ type: 'openInput', text: `${H}/slowpage.html?bg`, target: 'backgroundTab' });
  const bg = await waitState((s) => allTabs(s).some((t) => t.url === `${H}/slowpage.html?bg` && t.loaded && !t.loading), 8000);
  await sleep(400);
  const s2 = await st();
  const bgTab = bg && allTabs(bg).find((t) => t.url === `${H}/slowpage.html?bg`);
  check(S, 'background tab load: no focus steal, active tab unchanged', bg && s2.current.tab === tab && !log().split('\n').some((l) => bgTab && l.includes(`TabFocused { tab: ${bgTab.id} }`)), { bgFocused: (await count('tabFocused')) - bgBefore });
  if (bgTab) await dispatch({ type: 'closeItem', id: bgTab.id });
  await dispatch({ type: 'closeItem', id: tab });
}

async function closing() {
  const S = 'close';
  const tab = await openTab(`${H}/bu.html`);
  const t = await pageTarget(`${H}/bu.html`);
  check(S, 'beforeunload page has user activation', await evalIn(t, 'navigator.userActivation.hasBeenActive', { gesture: true }));
  const before = await count('tabBrowserClosed');
  await dispatch({ type: 'closeItem', id: tab });
  const i = await waitFor(async () => {
    const x = await info();
    return !tabInfo(x, tab) && x.tabs.closing.length === 0 && x;
  }, 8000);
  const s = await st();
  check(S, 'closeItem on a beforeunload page closes it (auto-accepted), TabBrowserClosed exactly once', i && !tabById(s, tab) && (await count('tabBrowserClosed')) === before + 1 && !(await targets()).some((x) => x.url === `${H}/bu.html`));
  await sleep(500);
  check(S, 'no duplicate TabBrowserClosed reported', !log().includes('duplicate TabBrowserClosed'));
}

async function crash() {
  const S = 'crash';
  const tab = await openTab(`${H}/crash.html`);
  const t = await pageTarget(`${H}/crash.html`);
  (await connect(t)).send('Page.crash').catch(() => null);
  let s = await waitState((s) => tabById(s, tab)?.crashed === true, 8000);
  check(S, 'renderer crash (CDP Page.crash) → TabCrashed → crashed state', s && (await count('tabCrashed')) >= 1);
  await dispatch({ type: 'reload', tab });
  s = await waitState((s) => tabById(s, tab)?.crashed === false && !s.current.loading, 8000);
  const back = await waitFor(async () => {
    const x = (await targets()).find((x) => x.type === 'page' && x.url === `${H}/crash.html`);
    return x && (await evalIn(x, 'document.title')) === 'Crash';
  }, 8000);
  check(S, 'reload recovers the crashed tab', s && back);
  await dispatch({ type: 'closeItem', id: tab });

  // A crashed UI surface reloads by itself.
  const topbar = await pageTarget('sta://topbar/');
  (await connect(topbar)).send('Page.crash').catch(() => null);
  const revived = await waitFor(async () => {
    const x = (await targets()).find((x) => x.type === 'page' && x.url.startsWith('sta://topbar/'));
    return x && (await evalIn(x, `typeof window.__staQuery`)) === 'function';
  }, 10000, 250);
  check(S, 'crashed UI surface (topbar) reloads automatically with IPC', revived && log().includes('UI renderer of browser'));
}

// ------------------------------------------------------------------ (devtools) docked DevTools
//
// FINAL PLAN §3/§7: the frontend runs in a BrowserView inside the tab's wrapper, the page sits on
// top of it at the rect the frontend reports, and the protocol goes through process messages —
// no socket, no second window. Everything here is driven through MCP.

/** The frontend's own protocol channel, used as the frontend uses it (shim → bridge → session S). */
const DEVTOOLS_PROBE = `window.__staProbe = function (method, params, session) {
  return new Promise(function (resolve) {
    var id = 900000 + Math.floor(Math.random() * 90000);
    var api = window.DevToolsAPI, original = api.dispatchMessage.bind(api), t0 = performance.now(), done = false;
    api.dispatchMessage = function (text) {
      if (!done && text.indexOf('"id":' + id) >= 0) {
        done = true; api.dispatchMessage = original;
        resolve(JSON.stringify({ ms: Math.round((performance.now() - t0) * 100) / 100, bytes: text.length, text: text.slice(0, 300) }));
        return;
      }
      return original(text);
    };
    var message = { id: id, method: method, params: params || {} };
    if (session) message.sessionId = session;
    window.InspectorFrontendHost.sendMessageToBackend(JSON.stringify(message));
    setTimeout(function () { if (!done) { done = true; api.dispatchMessage = original; resolve(JSON.stringify({ ms: -1, bytes: 0, text: 'timeout' })); } }, 20000);
  });
};
window.__staSeen = [];
(function () {
  var api = window.DevToolsAPI, original = api.dispatchMessage.bind(api);
  api.dispatchMessage = function (text) {
    if (text.indexOf('inspectNodeRequested') >= 0) window.__staSeen.push(text.slice(0, 200));
    return original(text);
  };
})();
'ok'`;

async function devtools(tab) {
  const S = 'devtools';
  const dt = async () => (await inst.info(['devtools', 'devtoolsCdp'])).devtools;
  const bridge = async () => ((await inst.info(['devtoolsCdp'])).devtoolsCdp.bridges || [])[0] || {};
  const dock = async () => (await dt()).docks[0];
  const frontendTarget = () => pageTarget((t) => t.url.startsWith('devtools://'));
  const openDocked = async () => {
    await dispatch({ type: 'toggleDevTools' });
    return waitFor(async () => {
      const d = await dock();
      return d && d.session && d.reportedPageRect ? d : null;
    }, 20000, 200);
  };
  const closeDocked = async () => {
    await dispatch({ type: 'toggleDevTools' });
    return waitFor(async () => ((await dt()).docks.length === 0 ? true : null), 10000, 150);
  };
  const windowCount = async () => ((await inst.t('test_window', { all: true })).windows || []).filter((w) => w.visible).length;

  await dispatch({ type: 'activateItem', id: tab });
  await waitState((s) => s.current && s.current.tab === tab);
  await dispatch({ type: 'navigate', tab, url: `${H}/devtools.html` });
  await waitState((s) => s.current && s.current.url === `${H}/devtools.html` && !s.current.loading, 12000);
  await sleep(600);
  const windowsBefore = await windowCount();

  // ---------------------------------------------------------------- open, one browser, one window
  const d = await openDocked();
  check(S, 'F12 docks a DevTools frontend (session attached, bounds reported)', !!d, d && { page: d.pageBounds, session: !!d.session });
  const i = await inst.info(['devtools', 'browsers', 'tabs']);
  const roles = (i.browsers.live || []).filter((b) => String(b.role).startsWith('DevTools'));
  check(S, 'exactly one Role::DevTools browser, for this tab', roles.length === 1 && String(roles[0].role).includes(`tab: ${tab}`), roles.map((r) => r.role));
  check(S, 'no new top-level window', (await windowCount()) === windowsBefore, { before: windowsBefore, now: await windowCount() });
  const page = d && d.pageBounds;
  const reported = d && d.reportedPageRect;
  check(S, 'the page view sits exactly at the reported rect', JSON.stringify(page) === JSON.stringify(reported), { page, reported });
  const inner = await evalIn(tab, 'String(innerWidth)');
  check(S, 'the page really is that narrow', Math.abs(Number(inner) - page[2]) <= 2, { inner, page });
  const stack = d.stackBounds;
  check(S, 'the frontend fills the card behind it', d.frontendBounds[2] === stack[2] && d.frontendBounds[3] === stack[3], { frontend: d.frontendBounds, stack });

  // The layout must settle: a write per layout pass would spin at ~500 Hz (gates-p2.md S11).
  const settled = await dock();
  await sleep(1500);
  const later = await dock();
  check(S, 'the layout settles (no relayout loop)', later.boundsWrites === settled.boundsWrites && later.boundsWrites < 10, {
    writes: [settled.boundsWrites, later.boundsWrites],
    relayouts: [settled.relayouts, later.relayouts],
  });

  // ---------------------------------------------------------------- the frontend really works
  let front = await frontendTarget();
  await evalIn(front.sel, DEVTOOLS_PROBE, { timeoutMs: 15000 });
  const probe = async (method, params, session) =>
    JSON.parse(
      await evalIn(front.sel, `window.__staProbe(${JSON.stringify(method)}, ${JSON.stringify(params || {})}${session ? `, ${JSON.stringify(session)}` : ''})`, {
        timeoutMs: 30000,
      }),
    );
  const doc = await probe('DOM.getDocument', { depth: 1 });
  check(S, 'the frontend can talk protocol over the process-message transport', doc.text.includes('"#document"') && doc.text.includes('"nodeType":9'), doc.text.slice(0, 120));
  const round = await probe('Runtime.evaluate', { expression: '6*7', returnByValue: true });
  check(S, 'a round trip is fast', round.ms >= 0 && round.ms < 50, `${round.ms} ms`);
  const big = await probe('Runtime.evaluate', { expression: `'x'.repeat(4*1024*1024)`, returnByValue: true });
  check(S, 'a 4 MB answer arrives whole (chunked)', big.bytes > 4 * 1024 * 1024, `${big.bytes} bytes in ${big.ms} ms`);

  const b = await bridge();
  check(S, 'the frontend drove a real session (panels, DOM, CSS, network)', b.fromFrontend > 30 && (b.byMethodOut || []).some((m) => m[0] === 'DOM.getDocument'), {
    from: b.fromFrontend,
    to: b.toFrontend,
  });
  check(S, 'nested sessions exist for the OOPIF and the worker', b.nested >= 2, { nested: b.nested });
  check(S, 'nothing was dropped and no method was unknown', b.dropped === 0 && (b.refused || []).length === 0, { dropped: b.dropped, refused: b.refused });

  // ---------------------------------------------------------------- the policy (SEC-1)
  const forbidden = [
    ['Browser.close', 'the browser target'],
    ['Target.createTarget', 'a Chromium window'],
    ['SystemInfo.getInfo', 'system info'],
    ['Network.getAllCookies', 'other sites’ cookies'],
    ['DOM.setFileInputFiles', 'the user’s files'],
  ];
  for (const [method] of forbidden) {
    const r = await probe(method, method === 'Target.createTarget' ? { url: 'https://example.com/' } : {});
    check(S, `the policy refuses ${method}`, r.text.includes('"error"'), r.text.slice(0, 140));
  }
  const nav = await probe('Page.navigate', { url: 'sta://settings/' });
  check(S, 'Page.navigate is limited to real pages', nav.text.includes('"error"'), nav.text.slice(0, 140));
  check(S, 'nothing opened a window while the policy refused', (await windowCount()) === windowsBefore);
  const stillOne = (await inst.info(['browsers'])).browsers.live.filter((x) => String(x.role).startsWith('DevTools')).length;
  check(S, 'the frontend survived the refusals', stillOne === 1);

  // ---------------------------------------------------------------- MCP while docked (S10, R-SEC-5)
  const storm = [];
  for (let n = 0; n < 12; n++) storm.push(inst.t('test_cdp', { target: { tab }, method: 'Runtime.evaluate', params: { expression: `${n}+1`, returnByValue: true } }));
  const answers = await Promise.all(storm);
  const correct = answers.every((r, n) => r.result && r.result.result && r.result.result.value === n + 1);
  check(S, 'MCP calls on the same page keep working (and answer correctly) while docked', correct, answers.map((a) => a.result?.result?.value));
  const afterStorm = await bridge();
  check(S, 'the MCP storm reached no DevTools session', afterStorm.dropped === 0, { dropped: afterStorm.dropped });
  // An agent's key events never reach sta's own shortcuts (R-SEC-5). A key injected through the
  // protocol carries no OS message, so `keyboard::on_key_event` consumes it before Views matches
  // the accelerators — without it, `Input.dispatchKeyEvent` could close tabs (Ctrl+W), open the
  // command bar (Ctrl+T) or toggle DevTools, which is exactly what agent/keys.rs promises it cannot.
  const countsBefore = await counts();
  const blockedBefore = (await inst.info(['keyboard'])).keyboard.injectedBlocked;
  const c = await connect(await pageTarget(`${H}/devtools.html`));
  for (const [key, code, modifiers] of [['F12', 123, 0], ['I', 73, 10], ['T', 84, 2], ['W', 87, 2]]) {
    for (const type of ['rawKeyDown', 'keyUp']) {
      await c.send('Input.dispatchKeyEvent', { type, key, code: `Key${key}`, windowsVirtualKeyCode: code, nativeVirtualKeyCode: code, modifiers });
    }
    await sleep(250);
  }
  await sleep(400);
  const countsAfter = await counts();
  const fired = ['toggleDevTools', 'focusDevTools', 'openCommandBar', 'toggleCommandBar', 'closeItem'].filter((k) => (countsAfter[k] || 0) !== (countsBefore[k] || 0));
  const blockedAfter = (await inst.info(['keyboard'])).keyboard.injectedBlocked;
  check(S, 'agent key events reach no sta shortcut (F12, Ctrl+Shift+I, Ctrl+T, Ctrl+W)', fired.length === 0 && blockedAfter > blockedBefore, { fired, blocked: [blockedBefore, blockedAfter] });
  check(S, 'and the tab and its DevTools are still there', (await dt()).docks.length === 1 && !!tabById(await st(), tab));

  // ---------------------------------------------------------------- Inspect (F8)
  const boxAt = async (zoom) => {
    const r = await evalIn(tab, `(function () { var b = document.getElementById('box').getBoundingClientRect(); return JSON.stringify([Math.round(b.left + b.width / 2), Math.round(b.top + b.height / 2)]); })()`);
    const [x, y] = JSON.parse(r);
    return [Math.round(x * zoom), Math.round(y * zoom)];
  };
  const inspected = async () => {
    const seen = await evalIn(front.sel, 'JSON.stringify(window.__staSeen)');
    const list = JSON.parse(seen);
    const last = list[list.length - 1];
    const m = last && /"backendNodeId":(\d+)/.exec(last);
    return m ? Number(m[1]) : null;
  };
  await evalIn(front.sel, 'window.__staSeen = []; "ok"');
  const [bx, by] = await boxAt(1);
  // A shell event (the context menu's "Inspect"), so it goes through test_dispatch, not the UI path.
  check(S, 'Inspect is not a UI command', (await dispatch({ type: 'inspectElement', tab, x: bx, y: by })).err !== undefined);
  await inst.dispatch({ type: 'inspectElement', tab, x: bx, y: by });
  const node = await waitFor(inspected, 8000, 150);
  let described = node ? await inst.t('test_cdp', { target: { tab }, method: 'DOM.describeNode', params: { backendNodeId: node } }) : null;
  check(S, 'Inspect selects the element under the point', described && described.result.node.attributes?.includes('box'), described && described.result.node);

  // Inspect with DevTools closed opens them first (the request waits for session S).
  await closeDocked();
  await sleep(400);
  await inst.dispatch({ type: 'inspectElement', tab, x: bx, y: by });
  const opened = await waitFor(async () => {
    const x = await dock();
    return x && x.session ? x : null;
  }, 20000, 200);
  check(S, 'Inspect with DevTools closed opens them first', !!opened);
  // That frontend is a new browser: re-install the recorder in it before asking again.
  front = await waitFor(async () => {
    const t = await frontendTarget().catch(() => null);
    return t && (!opened || t.browser === opened.frontendBrowser) ? t : null;
  }, 10000, 200);
  await evalIn(front.sel, DEVTOOLS_PROBE, { timeoutMs: 15000 });
  await inst.dispatch({ type: 'inspectElement', tab, x: bx, y: by });
  const late = await waitFor(async () => {
    const list = JSON.parse(await evalIn(front.sel, 'JSON.stringify(window.__staSeen || [])'));
    return list.length ? list[list.length - 1] : null;
  }, 10000, 200);
  check(S, 'and selects the node in the frontend it just opened', !!late && /"backendNodeId":\d+/.test(late), late && late.slice(0, 120));

  // …at 150 % page zoom, where the click point is in view pixels.
  await dispatch({ type: 'zoom', direction: 'in' }); // 110 %
  await dispatch({ type: 'zoom', direction: 'in' }); // 125 %
  await dispatch({ type: 'zoom', direction: 'in' }); // 150 %
  await waitState((s) => s.current && s.current.zoomPercent === 150, 6000);
  await sleep(500);
  await evalIn(front.sel, 'window.__staSeen = []; "ok"');
  const [zx, zy] = await boxAt(1.5);
  await inst.dispatch({ type: 'inspectElement', tab, x: zx, y: zy });
  const zoomNode = await waitFor(inspected, 8000, 150);
  described = zoomNode ? await inst.t('test_cdp', { target: { tab }, method: 'DOM.describeNode', params: { backendNodeId: zoomNode } }) : null;
  check(S, 'Inspect at 150 % zoom still hits the same element', described && described.result.node.attributes?.includes('box'), { zoomNode, node });
  await dispatch({ type: 'zoom', direction: 'reset' });
  await waitState((s) => s.current && s.current.zoomPercent === 100, 6000);

  // …and inside a cross-origin iframe, whose node lives in a nested session.
  await evalIn(front.sel, 'window.__staSeen = []; "ok"');
  const framePoint = await evalIn(tab, `(function () { var b = document.getElementById('f').getBoundingClientRect(); return JSON.stringify([Math.round(b.left + 30), Math.round(b.top + 20)]); })()`);
  const [fx, fy] = JSON.parse(framePoint);
  await inst.dispatch({ type: 'inspectElement', tab, x: fx, y: fy });
  const frameSeen = await waitFor(async () => {
    const list = JSON.parse(await evalIn(front.sel, 'JSON.stringify(window.__staSeen)'));
    return list.length ? list[list.length - 1] : null;
  }, 8000, 150);
  check(S, 'Inspect inside a cross-origin iframe answers in its own session', !!frameSeen && frameSeen.includes('sessionId'), frameSeen && frameSeen.slice(0, 160));

  // ---------------------------------------------------------------- an arbitrary reported rect
  // (what device mode does: the page floats inside the frontend's device frame).
  await evalIn(front.sel, `window.InspectorFrontendHost.setInspectedPageBounds({ x: 40, y: 60, width: 300, height: 400 }); 'ok'`);
  const floated = await waitFor(async () => {
    const x = await dock();
    return x && JSON.stringify(x.pageBounds) === JSON.stringify([40, 60, 300, 400]) ? x : null;
  }, 6000, 150);
  check(S, 'the page follows an inset rect (device mode)', !!floated, floated && floated.pageBounds);
  const floatedInner = await evalIn(tab, 'String(innerWidth)');
  check(S, 'the page is laid out at that size', Math.abs(Number(floatedInner) - 300) <= 2, floatedInner);
  await evalIn(front.sel, `window.InspectorFrontendHost.setInspectedPageBounds({ x: 0, y: 0, width: 465, height: ${floated ? floated.stackBounds[3] : 700} }); 'ok'`);
  await sleep(400);

  // ---------------------------------------------------------------- overlays above DevTools
  await dispatch({ type: 'openCommandBar', mode: 'newTab' });
  await waitFor(async () => (overlayOf(await info(), 'CommandBar') || {}).visible, 5000);
  await sleep(400);
  const shot = await capture('devtools-commandbar');
  const bar = overlayOf(await info(), 'CommandBar');
  const rect = bar && bar.bounds;
  let colors = [];
  if (rect) {
    colors = await inst.pixels('devtools-commandbar', [[rect[0] + rect[2] - 30, rect[1] + 20]]);
  }
  check(S, 'the command bar draws above the docked frontend', colors.length > 0 && colors[0] !== '#282828' && colors[0] !== '#1f1f1f', { colors, shot });
  await dispatch({ type: 'closeCommandBar' });
  await sleep(300);

  // ---------------------------------------------------------------- page fullscreen, Peek, split
  await inst.execute({ type: 'setPageFullscreen', tab });
  const fullscreen = await waitFor(async () => {
    const x = await dock();
    return x && !x.frontendVisible ? x : null;
  }, 6000, 150);
  check(S, 'page fullscreen hides the frontend and gives the page the whole card', !!fullscreen && fullscreen.pageBounds[2] === fullscreen.stackBounds[2], fullscreen && { page: fullscreen.pageBounds, stack: fullscreen.stackBounds });
  await inst.execute({ type: 'setPageFullscreen', tab: null });
  await waitFor(async () => {
    const x = await dock();
    return x && x.frontendVisible ? x : null;
  }, 6000, 150);
  check(S, 'leaving fullscreen brings the frontend back', !!(await dock()).frontendVisible);

  // ---------------------------------------------------------------- split and Peek
  // A split pane of this window is ~505 DIP wide, and DevTools' right dock never lets its own panel
  // below ~355 whatever it is given — which used to leave the inspected page a 150 px strip
  // rendering one word per line, with the pane's accent ring framing it. Below `MIN_DOCK_WIDTH`
  // (devtools.rs) sta moves DevTools into their own window and says why.
  await dispatch({ type: 'splitOpenInput', text: `${H}/b.html`, side: 'right' });
  const split = await waitState((s) => s.current && s.current.splitPanes === 2, 10000);
  check(S, 'a split pane opened next to the docked tab', !!split);
  const narrow = await waitFor(async () => {
    const d = await dt();
    return d.docks.length === 0 && d.undocked.includes(tab) ? d : null;
  }, 15000, 200);
  const paneRect = tabInfo(await info(), tab).wrapperBounds;
  check(S, 'the pane really is narrower than a dock needs', paneRect[2] > 0 && paneRect[2] < 640, paneRect);
  check(S, 'a pane too narrow to dock in moves DevTools into their own window', !!narrow, { narrow, pane: paneRect });
  const narrowToast = await waitFor(async () => {
    const s2 = await st();
    return s2.toast && /too narrow/.test(s2.toast.message) ? s2.toast : null;
  }, 8000, 200);
  check(S, 'and the toast says why', !!narrowToast, narrowToast);
  await dispatch({ type: 'separateAll', id: tab });
  await waitState((s) => s.current && s.current.splitPanes === 0, 8000);
  await dispatch({ type: 'activateItem', id: tab });
  await waitState((s) => s.current && s.current.tab === tab);
  await sleep(700);
  const afterSplit = await dt();
  check(S, 'and they stay in their own window after the split is gone (the undock lasts that session)', afterSplit.docks.length === 0 && afterSplit.undocked.includes(tab), afterSplit.undocked);
  // Back to a dock for the rest of the section: closing DevTools forgets the undock (D8).
  await dispatch({ type: 'toggleDevTools' });
  await waitFor(async () => ((await dt()).undocked.length === 0 ? true : null), 10000, 200);
  const redocked = await openDocked();
  check(S, 'closing them and pressing F12 again docks in the full-width card', !!redocked && redocked.pageBounds[2] < redocked.stackBounds[2], redocked && { page: redocked.pageBounds, stack: redocked.stackBounds });
  // That is a new frontend browser: `front` and the protocol recorder must follow it, or every
  // later `probe()` talks to a browser that is gone.
  front = await waitFor(async () => {
    const t = await frontendTarget().catch(() => null);
    return t && redocked && t.browser === redocked.frontendBrowser ? t : null;
  }, 15000, 200);
  await evalIn(front.sel, DEVTOOLS_PROBE, { timeoutMs: 15000 });

  // A Peek over a docked tab (a Shift+click link) leaves the dock alone.
  await inst.dispatch({ type: 'linkOpenRequested', opener: tab, url: `${H}/ok.html`, disposition: 'newWindow' });
  const peeked = await waitFor(async () => ((overlayOf(await info(), 'Peek') || {}).visible ? true : null), 8000, 150);
  check(S, 'a Peek opens over a docked tab', !!peeked);
  await sleep(600);
  check(S, 'and leaves the dock alone', (await dt()).docks.length === 1);
  await dispatch({ type: 'closePeek' });
  await waitFor(async () => !(overlayOf(await info(), 'Peek') || {}).visible, 8000);
  await sleep(700);
  const afterPeek = await dock();
  check(S, 'and the page is still where the frontend asked', !!afterPeek && afterPeek.pageBounds[2] === afterPeek.reportedPageRect[2], afterPeek && { page: afterPeek.pageBounds, reported: afterPeek.reportedPageRect });

  // ------------------------------------------------- the fixture, the parameters and the id space
  // Every one of these was reachable from an ordinary DevTools action and answered with a protocol
  // error (or, worse, with another site's data) until the phase-2 fixes.
  const refusedBefore = ((await bridge()).refused || []).length;
  // FID-2/FID-3/FID-7: methods *using* a panel needs, which opening 27 panels never sent.
  for (const [method, params] of [
    ['Runtime.compileScript', { expression: 'if (true) {', sourceURL: '', persistScript: false }],
    ['Emulation.resetPageScaleFactor', {}],
    ['Fetch.enable', { patterns: [] }],
    ['Fetch.disable', {}],
    ['DOM.getDetachedDomNodes', {}],
    ['Emulation.setSensorOverrideEnabled', { type: 'gyroscope', enabled: true }],
  ]) {
    const r = await probe(method, params);
    check(S, `${method} reaches Chromium`, !r.text.includes('not available in sta'), r.text.slice(0, 140));
  }
  check(S, 'and the fixture still reports no unknown method', ((await bridge()).refused || []).length === refusedBefore, (await bridge()).refused);
  await probe('Emulation.setSensorOverrideEnabled', { type: 'gyroscope', enabled: false });

  // T1/T3: a method that names an origin may only name one of the inspected page's own (its frames
  // are 127.0.0.1 and localhost here). Chromium honours `urls` verbatim, so this is the whole rule.
  const ownCookies = await probe('Network.getCookies', {});
  check(S, 'Network.getCookies without urls is the page’s own jar', ownCookies.text.includes('"cookies"'), ownCookies.text.slice(0, 100));
  const ownUrl = await probe('Network.getCookies', { urls: [`${H}/devtools.html`] });
  check(S, 'and its own frame URL is allowed', ownUrl.text.includes('"cookies"'), ownUrl.text.slice(0, 100));
  for (const [method, params] of [
    ['Network.getCookies', { urls: ['https://other.example/'] }],
    ['Network.deleteCookies', { name: 'x', url: 'https://other.example/' }],
    ['Storage.clearDataForOrigin', { origin: 'https://other.example', storageTypes: 'cookies' }],
    ['Storage.clearDataForStorageKey', { storageKey: 'https://other.example/' }],
    ['Storage.deleteStorageBucket', { bucket: { storageKey: 'https://other.example/', name: 'b' } }],
  ]) {
    const r = await probe(method, params);
    check(S, `${method} cannot name another site`, r.text.includes('own origins'), r.text.slice(0, 160));
  }
  const ownStorage = await probe('Storage.clearDataForOrigin', { origin: H, storageTypes: 'local_storage' });
  check(S, 'while the page’s own origin still works', !ownStorage.text.includes('"error"'), ownStorage.text.slice(0, 140));

  // T4: the frontend's ids are held inside their own space instead of being trusted to stay there —
  // an id in the shell client's range was swallowed by `devtools_cdp::deliver` and never answered.
  const reserved = await evalIn(
    front.sel,
    `new Promise(function (resolve) {
      var api = window.DevToolsAPI, original = api.dispatchMessage.bind(api), done = false;
      api.dispatchMessage = function (text) {
        if (!done && text.indexOf('"id":1073741824') >= 0) { done = true; api.dispatchMessage = original; resolve(text.slice(0, 200)); return; }
        return original(text);
      };
      window.InspectorFrontendHost.sendMessageToBackend(JSON.stringify({ id: 0x40000000, method: 'Runtime.evaluate', params: { expression: '1+1' } }));
      setTimeout(function () { if (!done) { done = true; api.dispatchMessage = original; resolve('TIMEOUT'); } }, 8000);
    })`,
    { timeoutMs: 20000 },
  );
  check(S, 'a message id from another client’s range is answered, not swallowed', reserved.includes('reserved in sta'), reserved.slice(0, 160));
  const stillAnswers = await probe('Runtime.evaluate', { expression: '5*5', returnByValue: true });
  check(S, 'and the session keeps working after it', stillAnswers.text.includes('"value":25'), stillAnswers.text.slice(0, 120));

  // T2: the one nested-session method the plan grants the frontend. The old `sessionId` surgery
  // rewrote "the last sessionId in the text", which for this very message is the one in `params`,
  // and left invalid JSON that Chromium answered nothing to.
  const nestedSessions = (await bridge()).nestedSessions || [];
  const victim = nestedSessions[nestedSessions.length - 1];
  const onNested = victim ? await probe('Runtime.evaluate', { expression: '1+1', returnByValue: true }, victim[0]) : null;
  check(S, 'a command on a nested session answers in that session', !!onNested && onNested.text.includes(victim[0]), onNested && onNested.text.slice(0, 140));
  const detach = victim ? await probe('Target.detachFromTarget', { sessionId: victim[0] }) : null;
  check(S, 'Target.detachFromTarget{sessionId} works', !!detach && detach.text.includes('"result"'), detach && detach.text.slice(0, 160));
  const dropped = await waitFor(async () => ((await bridge()).nestedSessions || []).every((x) => x[0] !== victim[0]), 6000, 200);
  check(S, 'and the bridge forgets that session', !!dropped, (await bridge()).nestedSessions);

  // FID-4: DevTools' own zoom. Chromium's implementation needs a ZoomController that only a
  // Chrome-style browser has, so the docked frontend zoomed by nothing until sta wrapped it.
  const frontendWidth = async () => Number(await evalIn(front.sel, 'String(document.documentElement.clientWidth)'));
  const zoom0 = await frontendWidth();
  await evalIn(front.sel, 'window.InspectorFrontendHost.zoomIn(); "ok"');
  const zoomed = await waitFor(async () => {
    const w = await frontendWidth();
    return w < zoom0 ? w : null;
  }, 6000, 200);
  check(S, 'zoomIn really zooms the frontend', !!zoomed, { before: zoom0, after: zoomed });
  await evalIn(front.sel, 'window.InspectorFrontendHost.resetZoom(); "ok"');
  const unzoomed = await waitFor(async () => {
    const w = await frontendWidth();
    return w === zoom0 ? w : null;
  }, 6000, 200);
  check(S, 'and resetZoom puts it back', !!unzoomed, { before: zoom0, after: unzoomed });

  // LAY-3: a minimized window lays its contents out at 1×1. Writing those insets back would hand
  // the page the whole card for one frame on restore (a flash over DevTools, two page reflows).
  await evalIn(tab, 'window.__sizes = []; addEventListener("resize", function () { window.__sizes.push([innerWidth, innerHeight]); }); "ok"');
  const writesBefore = (await dock()).boundsWrites;
  await inst.t('test_window_message', { message: 'minimize' });
  await sleep(1200);
  await inst.t('test_window_message', { message: 'restore' });
  await sleep(1500);
  const restored = await dock();
  const sizes = JSON.parse(await evalIn(tab, 'JSON.stringify(window.__sizes || [])'));
  check(
    S,
    'minimize and restore never lay the page out at the full card',
    !sizes.some(([w]) => w >= restored.stackBounds[2] - 2) && JSON.stringify(restored.pageBounds) === JSON.stringify(restored.reportedPageRect),
    { sizes, page: restored.pageBounds, reported: restored.reportedPageRect, writes: [writesBefore, restored.boundsWrites] },
  );

  // LAY-2: the frontend's own close names its tab. An untargeted ToggleDevTools resolved against
  // the focused pane, which from an unfocused split pane opened a second dock on the other one.
  await evalIn(front.sel, 'window.InspectorFrontendHost.closeWindow(); "ok"');
  const selfClosed = await waitFor(async () => ((await dt()).docks.length === 0 ? true : null), 10000, 200);
  check(S, 'the frontend’s own closeWindow closes its own dock', !!selfClosed);
  const reDocked = await openDocked();
  check(S, 'and F12 opens a fresh one', !!reDocked);
  front = await frontendTarget();
  await evalIn(front.sel, DEVTOOLS_PROBE, { timeoutMs: 15000 });

  // ---------------------------------------------------------------- a fresh dock after a crash
  const frontBrowser = (await dock()).frontendBrowser;
  await inst.t('test_cdp', { target: { browser: frontBrowser }, method: 'Page.crash', params: {} }).catch(() => null);
  const gone = await waitFor(async () => ((await dt()).docks.length === 0 ? true : null), 12000, 200);
  check(S, 'a crashed frontend closes the dock', !!gone);
  check(S, 'the page and its workers survive the crash', (await evalIn(tab, 'String(window.__worker)')) === 'ready');
  const reopened = await openDocked();
  check(S, 'DevTools open again after the crash', !!reopened && reopened.frontendBrowser !== frontBrowser);

  // ---------------------------------------------------------------- undock → close → dock again
  await dispatch({ type: 'undockDevTools' });
  const undockedWindow = await waitFor(async () => ((await targets()).some((t) => t.url.startsWith('devtools://devtools/bundled/devtools_app.html') && !t.role?.startsWith('devtools')) ? true : null), 10000, 200);
  const undockedState = await dt();
  check(S, 'Undock closes the dock and opens CEF’s own window', undockedState.docks.length === 0 && undockedState.undocked.includes(tab), { undockedState, undockedWindow });
  check(S, 'the undocked window is a second top-level window', (await windowCount()) > windowsBefore);
  // FID-1: the window must show the *page*, not just DevTools' chrome. It did not: sta's embedder
  // shim was installed in Chromium's own DevTools window too and swallowed its whole protocol, so
  // this window rendered its panels empty with nothing logged — and a target plus a window is all
  // this section used to assert.
  const undockedFrontend = await waitFor(async () => {
    const t = await pageTarget((x) => x.url.startsWith('devtools://')).catch(() => null);
    if (!t) return null;
    const text = await evalIn(
      t.sel,
      `(function () { var roots = [document], seen = new Set(), out = [];
        for (var i = 0; i < roots.length; i++) { var all = roots[i].querySelectorAll('*'); for (var j = 0; j < all.length; j++) { var e = all[j]; if (e.shadowRoot && !seen.has(e.shadowRoot)) { seen.add(e.shadowRoot); roots.push(e.shadowRoot); } } }
        for (var i = 0; i < roots.length; i++) { var w = document.createTreeWalker(roots[i], NodeFilter.SHOW_TEXT, null), n; while ((n = w.nextNode())) { var p = n.parentNode && n.parentNode.nodeName; if (p === 'STYLE' || p === 'SCRIPT') continue; var v = n.nodeValue; if (v && v.trim()) out.push(v.trim()); } }
        return out.join(' | ').slice(0, 800); })()`,
      { timeoutMs: 20000 },
    ).catch(() => '');
    // The page's own doctype node: DevTools' chrome alone (what a disconnected window shows) has no
    // such text anywhere, and CSS is skipped so a `box-*` property cannot pass for the page's `box`.
    return /DOCTYPE/.test(String(text)) ? text : null;
  }, 20000, 500);
  check(S, 'the undocked frontend is connected (the page’s DOM is in its Elements tree)', !!undockedFrontend, String(undockedFrontend).slice(0, 160));
  const undockedShim = await evalIn(await inst.selector(await pageTarget((x) => x.url.startsWith('devtools://'))), 'String(typeof window.__staDevToolsDispatch)').catch((e) => 'failed');
  check(S, 'and it keeps Chromium’s own embedder (no sta shim)', undockedShim === 'undefined', undockedShim);
  await closeDocked();
  await sleep(500);
  check(S, 'closing it leaves no DevTools window', (await windowCount()) === windowsBefore, await inst.t('test_window', { all: true }).then((w) => w.windows.filter((x) => x.visible).map((x) => x.title)));
  const back = await openDocked();
  check(S, 'F12 docks again after an undock (the undock lasts for that DevTools only)', !!back && (await dt()).undocked.length === 0);

  // ---------------------------------------------------------------- Peek and replace close it
  await dispatch({ type: 'navigate', tab, url: 'sta://settings/' });
  const replaced = await waitFor(async () => ((await dt()).docks.length === 0 ? true : null), 10000, 200);
  check(S, 'replacing the browser (web → sta://) closes DevTools first', !!replaced);
  await dispatch({ type: 'toggleDevTools' });
  await sleep(600);
  const s2 = await st();
  check(
    S,
    'F12 on an sta:// page opens nothing and says why',
    (await dt()).docks.length === 0 && !!s2.toast && s2.toast.message === "DevTools isn't available on sta pages",
    s2.toast && s2.toast.message,
  );
  await dispatch({ type: 'navigate', tab, url: `${H}/devtools.html` });
  await waitState((s) => s.current && s.current.url === `${H}/devtools.html` && !s.current.loading, 12000);
  await sleep(400);

  // ---------------------------------------------------------------- real keys (foreground)
  await retryInterrupted(async () => {
    await inst.focus({ tab });
    await sleep(200);
    const opened = await (async () => {
      await inst.keys('f12');
      return waitFor(async () => {
        const x = await dock();
        return x && x.session ? x : null;
      }, 12000, 200);
    })();
    check(S, 'a real F12 docks DevTools', !!opened);
    if (!opened) return;
    // Ctrl+Shift+I with the page focused focuses the frontend; again (focused) closes it.
    await inst.focus({ tab });
    await sleep(300);
    await inst.keys('ctrl+shift+i');
    const focused = await waitFor(async () => {
      const f = (await inst.info(['focus'])).focus;
      return String(f.role || '').startsWith('DevTools') ? f : null;
    }, 8000, 150);
    check(S, 'Ctrl+Shift+I focuses the open frontend', !!focused, focused);
    await inst.keys('ctrl+shift+i');
    const closedByKey = await waitFor(async () => ((await dt()).docks.length === 0 ? true : null), 8000, 200);
    check(S, 'Ctrl+Shift+I again (frontend focused) closes DevTools', !!closedByKey);
  });

  // LAY-1: closing the dock must give the page its keyboard back. `build` focuses the frontend, and
  // the view that had focus is destroyed, so without handing it back the page had none at all:
  // typing went nowhere until the user clicked. No `inst.focus()` here — that would hide the bug.
  await retryInterrupted(async () => {
    await inst.focus({ tab });
    await sleep(250);
    await evalIn(tab, 'window.__typed = []; addEventListener("keydown", function (e) { window.__typed.push(e.key); }); "ok"');
    await inst.keys('f12');
    const dockedByKey = await waitFor(async () => {
      const x = await dock();
      return x && x.session ? x : null;
    }, 15000, 200);
    check(S, 'a real F12 docks (again)', !!dockedByKey);
    await inst.keys('f12');
    await waitFor(async () => ((await dt()).docks.length === 0 ? true : null), 10000, 200);
    await sleep(500);
    await evalIn(tab, 'window.__typed = []; "ok"');
    await inst.keys('b');
    await sleep(400);
    const typed = JSON.parse(await evalIn(tab, 'JSON.stringify(window.__typed || [])'));
    const focused = await evalIn(tab, 'String(document.hasFocus())');
    check(S, 'the page has the keyboard again after F12 → F12', typed.includes('b') && focused === 'true', { typed, focused, sta: (await inst.info(['focus'])).focus });
  });

  // FID-6: Ctrl+Shift+C is Chrome's element picker where a developer presses it (a dock is open) and
  // sta's Copy URL everywhere else.
  await retryInterrupted(async () => {
    await inst.setClipboard('nothing-yet');
    await inst.focus({ tab });
    await sleep(250);
    await inst.keys('ctrl+shift+c');
    const copied = await waitFor(async () => ((await inst.clipboard()).includes('devtools.html') ? true : null), 6000, 200);
    check(S, 'Ctrl+Shift+C with no dock copies the URL', !!copied, await inst.clipboard());
    const picker = await openDocked();
    front = await frontendTarget();
    await evalIn(front.sel, DEVTOOLS_PROBE, { timeoutMs: 15000 });
    const inspectBefore = ((await bridge()).byMethodOut || []).find((m) => m[0] === 'Overlay.setInspectMode');
    await inst.setClipboard('nothing-yet');
    await inst.focus({ tab });
    await sleep(250);
    await inst.keys('ctrl+shift+c');
    const picking = await waitFor(async () => {
      const m = ((await bridge()).byMethodOut || []).find((x) => x[0] === 'Overlay.setInspectMode');
      return m && (!inspectBefore || m[1] > inspectBefore[1]) ? m : null;
    }, 8000, 200);
    check(S, 'Ctrl+Shift+C with a dock starts the element picker instead', !!picker && !!picking, { picking, inspectBefore });
    check(S, 'and does not touch the clipboard', (await inst.clipboard()) === 'nothing-yet');
    await closeDocked();
  });

  // Page-first keys reach the frontend, high-priority ones stay sta's (S2).
  const reopen = await openDocked();
  if (reopen) {
    await retryInterrupted(async () => {
      const f2 = await frontendTarget();
      await inst.focus({ tab });
      await sleep(200);
      await inst.keys('ctrl+shift+i'); // focuses the open frontend (UX17)
      const hasFocus = await waitFor(async () => (String((await inst.info(['focus'])).focus.role || '').startsWith('DevTools') ? true : null), 8000, 150);
      check(S, 'the frontend has keyboard focus', !!hasFocus);
      await inst.keys('ctrl+f');
      await sleep(600);
      const findShown = (overlayOf(await info(), 'FindBar') || {}).visible;
      check(S, 'Ctrl+F with the frontend focused stays in DevTools (no sta find bar)', !findShown, { findShown });
      await inst.keys('ctrl+t');
      const barShown = await waitFor(async () => ((overlayOf(await info(), 'CommandBar') || {}).visible ? true : null), 6000, 150);
      check(S, 'Ctrl+T from the frontend still opens sta’s command bar', !!barShown);
      await dispatch({ type: 'closeCommandBar' });
      await sleep(300);
      // F11 while paused steps into instead of toggling fullscreen (D9).
      await evalIn(f2.sel, 'window.__staSteps = 0; "ok"');
      await inst.t('test_cdp', { target: { tab }, method: 'Runtime.evaluate', params: { expression: 'setTimeout(function () { debugger; }, 50)' } });
      const paused = await waitFor(async () => {
        const br = await bridge();
        return (br.byMethod || []).some((m) => m[0] === 'Debugger.paused') ? true : null;
      }, 8000, 200);
      check(S, 'the debugger pauses the page (the frontend is attached)', !!paused);
      await sleep(300);
      const beforeStep = ((await bridge()).byMethodOut || []).some((m) => m[0] === 'Debugger.stepInto');
      await inst.keys('f11');
      const stepped = await waitFor(async () => {
        const br = await bridge();
        return (br.byMethodOut || []).some((m) => m[0] === 'Debugger.stepInto' || m[0] === 'Debugger.stepOver' || m[0] === 'Debugger.resume') ? true : null;
      }, 8000, 200);
      check(S, 'F11 with the frontend focused steps in the debugger instead of going fullscreen', !!stepped && !beforeStep, { stepped, beforeStep });
      check(S, 'and the window did not go fullscreen', (await info()).window.fullscreen === false);
      await inst.t('test_cdp', { target: { tab }, method: 'Debugger.resume', params: {} }).catch(() => null);
    });
  }

  // ---------------------------------------------------------------- screenshots
  await sleep(600);
  check(S, 'screenshot (docked, dark)', !!(await capture('devtools-docked-dark')));
  // The theme follows sta's dark mode live (UX16): the shell emulates `prefers-color-scheme` in the
  // frontend's own page, which DevTools re-reads on every change.
  const themeOf = async () => evalIn((await frontendTarget()).sel, `String(document.documentElement.classList.contains('theme-with-dark-background'))`).catch((e) => 'failed: ' + e.message);
  await dispatch({ type: 'updateSettings', patch: { appearance: 'light' } });
  const turnedLight = await waitFor(async () => ((await themeOf()) === 'false' ? true : null), 8000, 200);
  check(S, 'an open frontend follows sta into light mode', !!turnedLight);
  await sleep(700);
  check(S, 'screenshot (docked, light)', !!(await capture('devtools-docked-light')));
  await closeDocked();
  await openDocked();
  await sleep(1200);
  check(S, 'and a frontend opened in light mode is light', (await themeOf()) === 'false');
  await dispatch({ type: 'updateSettings', patch: { appearance: 'dark' } });
  const turnedDark = await waitFor(async () => ((await themeOf()) === 'true' ? true : null), 8000, 200);
  check(S, 'and back to dark', !!turnedDark);
  await sleep(500);
  await closeDocked();
  const closed = await dock();
  check(S, 'closing DevTools gives the page the whole card back', (await dt()).docks.length === 0 && !closed);
  const home = tabInfo(await info(), tab);
  // Back in the wrapper means back at the wrapper's 2 px accent inset (inside the dock it sat at
  // the stack's origin instead).
  check(S, 'the page view is back in its wrapper, at its border inset', home && !home.devtoolsDocked && home.viewOrigin[0] === 2 && home.viewOrigin[1] === 2, home && { origin: home.viewOrigin, docked: home.devtoolsDocked });

  // Left open on purpose: the suite's shutdown checks then cover quitting with a docked frontend.
  await openDocked();
  check(S, 'DevTools left open for the shutdown checks', (await dt()).docks.length === 1);
}

// ------------------------------------------------------------------------------------ run

async function main() {
  writePages();
  const site = await startFileServer(HTTP_PORT);
  HTTP_PORT = site.address().port;
  H = `http://127.0.0.1:${HTTP_PORT}`;
  // The retry fixture starts mid-test, but its URL is loaded (and refused) before that: reserve the
  // port now so the URL is known.
  RETRY_PORT = RETRY_PORT || (await reservePort());
  check('setup', 'test HTTP server up', await waitFor(() => httpOk(`${H}/b.html`), 8000, 200));

  inst = new Instance({ data: DATA, args: ['--use-fake-device-for-media-stream'], env: { STA_TEST_CONTEXT_MENU: 'Open Link in New Tab' } });
  inst.start();
  writeFileSync(`${DATA}-pid.txt`, String(inst.pid));
  const ready = await waitFor(async () => (await targets()).some((t) => t.url.startsWith(SB)) && (await invoke('state.get')).ok, 40000, 250);
  check('setup', 'sidebar ready (IPC)', ready);
  if (!ready) throw new Error('startup failed');
  const consoles = await consoleBaseline(inst);

  // The download folder is pointed at DL before anything can download, so every section that lists
  // it (`preview`, `downloads`) measures the folder this browser actually writes into.
  rmSync(DL, { recursive: true, force: true });
  mkdirSync(DL, { recursive: true });
  await dispatch({ type: 'updateSettings', patch: { downloadDir: DL } });
  check('setup', "downloads go to the suite's own folder", !!(await waitState((s) => s.settings.downloadDir === DL, 6000)), DL);

  let ctx = {};
  if (want('popups') || want('downloads')) ctx = await popups();
  if (want('intercept')) await intercept();
  if (want('preview')) {
    const pctx = await preview();
    await previewShots(pctx);
  }
  if (want('menus')) await contextMenusUi();
  if (want('errors')) await errors();
  if (want('downloads')) await downloads(ctx.openerT || (await pageTarget(`${H}/opener.html`)));
  if (want('permissions')) await permissions();
  if (want('permissions')) await permissionGrants();
  let findTab;
  if (want('find') || want('zoom') || want('devtools')) findTab = await find();
  if (want('zoom')) await zoom(findTab);
  if (want('audio')) await audio();
  if (want('fullscreen')) await fullscreen();
  if (want('boosts')) await boosts();
  if (want('focus')) await focus();
  if (want('replace')) await replace();
  if (want('close')) await closing();
  if (want('crash')) await crash();
  if (want('devtools')) await devtools(findTab);

  // ------------------------------------------------- (hygiene) no console window during the run
  await checkNoConsoleWindows(inst, consoles, check);

  if (KEEP_OPEN) {
    console.log('--keep-open: leaving the browser running');
    return;
  }
  // The sidebar closes while answering: the reply may never arrive.
  await Promise.race([dispatch({ type: 'quit' }).catch(() => null), sleep(2000)]);
  const exited = await waitFor(() => !alive(inst.pid), 20000, 200);
  check('shutdown', 'quit → process exits', exited);
  const text = log();
  check('shutdown', 'log: exited cleanly', text.includes('exited cleanly'));
  check('shutdown', 'no panics', !text.includes('PANIC') && !text.includes('panicked'));
}

try {
  await main();
} catch (e) {
  check('run', 'unexpected error', false, e.stack || String(e));
} finally {
  inst?.closeSockets();
  if (!KEEP_OPEN && inst?.pid && alive(inst.pid)) {
    console.log('killing leftover process tree');
    killTree(inst.pid);
  }
  for (const s of servers) {
    s.closeAllConnections?.();
    s.close();
  }
  const failed = results.filter((r) => !r.ok);
  console.log(`\n${results.length - failed.length}/${results.length} checks passed`);
  process.exit(failed.length ? 1 : 0);
}
