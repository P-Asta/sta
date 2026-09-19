#!/usr/bin/env node
// End-to-end checks of the CEF shell against a debug build (Windows, Node 22+).
//
//   cargo build -p sta -p sta-mcp --features test-hooks
//   node crates/sta/e2e/shell-e2e.mjs [--keep-open]
//
// Launches target/debug/sta.exe (real sta-core, no stand-ins) with a throw-away data dir
// (E2E_DATA_DIR, default C:/ast/tmp/shell-e2e) and drives it **through MCP**: JSON-RPC over stdio
// to target/debug/sta-mcp.exe, which forwards to the browser over its named pipe. The browser must
// therefore be built with `--features test-hooks`; lib.mjs arms it with `--sta-test-hooks` and
// `STA_E2E=1` (docs/TESTING.md). The `debug.*` IPC requests the suite drives the shell with are
// reached as the `test_*` tools, `window.sta.invoke` through `test_invoke` (the real trusted-frame
// path), raw DevTools through `test_cdp`, and the native window probes and screenshots through
// `test_window` / `test_hit_test` / `test_window_message` / `test_capture` — so no PowerShell and no
// console window. Other sta instances may run concurrently: only the process tree started here is
// probed and killed.
//
// STA_REMOTE_DEBUGGING_PORT (CDP_PORT, default 9333) is still passed for the single remaining
// DevTools-port check in (c); nothing else in the suite uses it. See C:/ast/tmp/s6/cdp-residue.md
// for the checks that cannot go through MCP (they are about *not* having a running browser).
//
// Sections follow the skeleton acceptance list: (b) layout, (c) IPC, (d) security, (e) command bar,
// (e2) search suggestions, (f) window, (g) tab lifecycle, (h) single instance, (hygiene) no console
// window, (f2) shutdown. Exit code 0 = all checks passed.
//
// (e2) points STA_SUGGEST_URL (debug builds) at a local HTTP server started here, which
// answers canned OpenSearch suggestion bodies and records every request. E2E_EXE overrides the
// executable (default target/debug/sta.exe).
//
// Real keyboard input is covered by chrome-e2e.mjs (`test_real_keys`); here accelerators are
// exercised with `debug.accelerator` (same table + handler).

import { spawn } from 'node:child_process';
import http from 'node:http';
import { existsSync, rmSync } from 'node:fs';
import path from 'node:path';
import { EXE as DEFAULT_EXE, Instance, alive, checkNoConsoleWindows, consoleBaseline, killTree, overlay, processesWith, sleep, tabInfo } from './lib.mjs';

const EXE = process.env.E2E_EXE ? path.resolve(process.env.E2E_EXE) : DEFAULT_EXE;
const PORT = process.env.CDP_PORT || '9333';
const DATA = process.env.E2E_DATA_DIR || 'C:/ast/tmp/shell-e2e';
const KEEP_OPEN = process.argv.includes('--keep-open');

// ------------------------------------------------------------------------------------ reporting

const results = [];
function check(section, name, ok, detail) {
  results.push({ section, name, ok: !!ok });
  let d = detail === undefined ? '' : ' ' + (typeof detail === 'string' ? detail : JSON.stringify(detail));
  if (ok && d.length > 240) d = d.slice(0, 240) + '…';
  console.log(`${ok ? 'PASS' : 'FAIL'} [${section}] ${name}${d}`);
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

/** sta.exe processes started with our data dir (other instances may run concurrently). */
const ourProcesses = () => processesWith(DATA);

/**
 * The instance under test and the MCP helpers, all from lib.mjs. The local names below keep the
 * check bodies unchanged: only the transport underneath them moved.
 */
let inst;
const win = (...args) => inst.win(...args);
const capture = (name) => inst.capture(name);
const targets = () => inst.targets();
const targetId = (match) => inst.target(match);
const connect = (t) => inst.connect(t);
const evalIn = (match, expr, opts) => inst.eval(match, expr, opts);
const invoke = (match, cmd, payload = null) => inst.invoke(match, cmd, payload);
const info = () => inst.info();
const execute = (effects) => inst.execute(effects);
const counts = () => inst.counts();

// ------------------------------------------------------------------------------------ suggest server

/**
 * Local OpenSearch suggestion endpoint for `STA_SUGGEST_URL`. `GET /complete?q=…` answers
 * `[q, [q + " one", Q + " ONE" (a duplicate), q, "", q + " two", q + "gramming language"], …]`,
 * except: `q` starting with "slow" answers after 1 s, "hang" never answers (until the server
 * closes), "big" answers a body over 64 KB, "status" answers 500, "garbage" answers non-JSON.
 * `GET /setcookie` sets a cookie for 127.0.0.1 (suggestion answers try to set one too). Every
 * request is recorded as `{path, q, cookie}`.
 */
async function startSuggestServer() {
  const requests = [];
  const hanging = new Set();
  const server = http.createServer((req, res) => {
    const url = new URL(req.url, 'http://127.0.0.1');
    const q = url.searchParams.get('q');
    requests.push({ path: url.pathname, q, cookie: req.headers.cookie || null });
    if (url.pathname === '/setcookie') {
      res.writeHead(200, { 'Content-Type': 'text/html', 'Set-Cookie': 'e2esuggest=1; Path=/; Max-Age=3600' });
      res.end('<title>E2E-COOKIE</title>cookie set');
      return;
    }
    if (url.pathname !== '/complete' || q === null) {
      res.writeHead(404, { 'Content-Type': 'text/plain' });
      res.end('not found');
      return;
    }
    const json = (status, body) => {
      // Suggestion answers try to store a cookie too (must not reach the profile).
      res.writeHead(status, { 'Content-Type': 'application/json; charset=utf-8', 'Set-Cookie': 'e2efromsuggest=1; Path=/; Max-Age=3600' });
      res.end(body);
    };
    if (q.startsWith('hang')) {
      hanging.add(res);
      return;
    }
    if (q.startsWith('big')) return json(200, JSON.stringify([q, [q + ' ' + 'x'.repeat(70 * 1024)]]));
    if (q.startsWith('status')) return json(500, JSON.stringify([q, [q + ' one']]));
    if (q.startsWith('garbage')) return json(200, '<html>not json');
    const body = JSON.stringify([q, [q + ' one', (q + ' one').toUpperCase(), q, '', q + ' two', q + 'gramming language'], [], { 'google:suggestsubtypes': [] }]);
    if (q.startsWith('slow')) {
      setTimeout(() => json(200, body), 1000);
      return;
    }
    json(200, body);
  });
  await new Promise((ok) => server.listen(0, '127.0.0.1', ok));
  const port = server.address().port;
  return {
    port,
    requests,
    template: `http://127.0.0.1:${port}/complete?q={q}`,
    completions: () => requests.filter((r) => r.path === '/complete'),
    count: () => requests.filter((r) => r.path === '/complete').length,
    close: () => {
      for (const res of hanging) res.destroy();
      server.closeAllConnections?.();
      server.close();
    },
  };
}

let suggestServer;

// ------------------------------------------------------------------------------------ run

async function main() {
  if (!existsSync(EXE)) throw new Error(`${EXE} not found: run cargo build -p sta -p sta-mcp --features test-hooks first`);
  if (ourProcesses().length) throw new Error(`a sta.exe process already uses ${DATA}; stop it first`);

  suggestServer = await startSuggestServer();
  // External protocol launches are only logged (debug hook), never handed to Windows; search
  // suggestions come from the local server.
  inst = new Instance({
    data: DATA,
    port: PORT,
    exe: EXE,
    env: { STA_TEST_EXTERNAL_PROTOCOL: '1', STA_SUGGEST_URL: suggestServer.template },
  });
  inst.start();
  const log = () => inst.log();

  // ---------------------------------------------------------------- (b) layout
  const ready = await waitFor(async () => {
    const urls = (await targets()).map((t) => t.url);
    return ['sidebar', 'topbar', 'empty', 'command'].every((h) => urls.some((u) => u.startsWith(`sta://${h}/`))) && urls;
  }, 30000, 250);
  check('b', 'UI surfaces loaded (sidebar, topbar, empty, command)', ready, ready || 'timeout');
  if (!ready) throw new Error('startup failed');
  const consoles = await consoleBaseline(inst);
  await waitFor(async () => overlay(await info(), 'CommandBar').ready, 5000);
  let i = await info();
  const sidebarWidth = i.window.sidebar.width;
  check('b', 'window exists and is not closing', i.window.exists && !i.window.closing);
  check('b', 'content rect starts right of the sidebar, below the topbar', i.window.contentRect[0] === sidebarWidth && i.window.contentRect[1] === 40, i.window.contentRect);
  const roles = i.browsers.live.map((b) => b.role);
  check('b', 'empty-state view visible with no tab', i.window.emptyVisible === true);
  check('b', 'live UI browsers are trusted surfaces', ['Surface(Sidebar)', 'Surface(Topbar)', 'Surface(Empty)', 'Surface(CommandBar)'].every((r) => roles.includes(r)) && i.browsers.live.every((b) => b.ui), roles);
  const native = await win('info');
  check('b', 'native window is resizable (WS_THICKFRAME) and frameless', native.thickFrame, native);
  console.log('  ' + (await capture('layout')));

  // ---------------------------------------------------------------- (c) IPC + persistence
  const profile = path.join(DATA, 'sta');
  const saved = await waitFor(() => existsSync(path.join(profile, 'state.json')), 4000, 200);
  check('c', 'debounced save writes only the dirty file (state.json, not history.json) ~1 s after startup', saved && !existsSync(path.join(profile, 'history.json')));
  // The last use of STA_REMOTE_DEBUGGING_PORT in the whole test tree: the debug build still opens
  // the DevTools port, and this is the only check that proves it (harness-design §10.1). Everything
  // else, this suite included, drives the browser over MCP.
  const version = await waitFor(async () => (await fetch(`http://127.0.0.1:${PORT}/json/version`)).json(), 8000, 250);
  check('c', `the debug build still answers the DevTools port ${PORT} (/json/version)`, !!version && typeof version.Browser === 'string' && version.Browser.length > 0, version && version.Browser);
  const SB = 'sta://sidebar/';
  const st = await invoke(SB, 'state.get');
  check('c', 'state.get returns UiState', st.ok && typeof st.ok.revision === 'number' && Array.isArray(st.ok.spaces), st.ok && { revision: st.ok.revision, keys: Object.keys(st.ok).length });
  const allowed = await invoke(SB, 'dispatch', { type: 'toggleFolder', id: 1 });
  check('c', 'dispatch of an allowed command resolves null', allowed.ok === null && allowed.err === undefined, allowed);
  const shellEvent = await invoke(SB, 'dispatch', { type: 'tabBrowserCreated', tab: 1 });
  check('c', 'dispatch of a shell event is rejected (403)', shellEvent.err === 403, shellEvent);
  const bogus = await invoke(SB, 'dispatch', { type: 'noSuchCommand' });
  check('c', 'malformed command is rejected (400)', bogus.err === 400, bogus);
  const unknown = await invoke(SB, 'nope');
  check('c', 'unknown request is rejected (404)', unknown.err === 404, unknown);
  for (const [cmd, payload] of [
    ['omnibox.query', { text: 'rust', mode: 'newTab', preventInlineAutocomplete: false, seq: 1 }],
    ['archive.list', null],
    ['history.list', { query: '', limit: 10 }],
    ['boosts.get', { id: 1 }],
    ['theme.colors', { theme: { hue: 200, hue2: 220, chroma: 0.05 } }],
    ['app.info', null],
  ]) {
    const r = await invoke(SB, cmd, payload);
    check('c', `${cmd} answers`, r.err === undefined, r.err === undefined ? undefined : r);
  }
  await evalIn(SB, `window.__e2ePushes = 0; window.sta.on('state', function () { window.__e2ePushes++; }); 'ok'`);
  const statePushes = () => evalIn(SB, 'window.__e2ePushes');
  const before = await statePushes();
  await invoke(SB, 'debug.pushState');
  const after = await waitFor(async () => {
    const n = await statePushes();
    return n > before && n;
  }, 3000);
  check('c', 'state push reaches the page (debug.pushState)', after > before, { before, after });

  // ---------------------------------------------------------------- (d) security
  const pageA = 'data:text/html,<title>E2E-A</title><h1>E2E tab A</h1>';
  const openA = await invoke(SB, 'debug.openTab', { url: pageA, show: true });
  const tabA = openA.ok && openA.ok.tab;
  check('d', 'debug.openTab creates a web tab', typeof tabA === 'number', openA);
  const tA = await waitFor(async () => (await targets()).find((t) => t.url.includes('E2E-A')), 8000);
  check('d', 'web tab loaded', tA);
  check('d', 'web tab has no __staQuery', (await evalIn(tA, 'typeof window.__staQuery')) === 'undefined');
  i = await info();
  const liveA = i.browsers.live.find((b) => b.role === `Tab(${tabA})`);
  check('d', 'web tab browser is untrusted (ui=false)', liveA && liveA.ui === false, liveA);
  // Renderer-initiated: Chromium already refuses it (DISPLAY_ISOLATED: "Not allowed to load local resource").
  await evalIn(tA, `location.href = 'sta://sidebar/'; 'navigating'`);
  await sleep(1000);
  check('d', 'web tab cannot navigate to sta:// (renderer-initiated)', (await evalIn(tA, 'location.href')).startsWith('data:text/html'));
  // Browser-initiated (CDP Page.navigate) reaches TabRequest::on_before_browse, the shell's own guard.
  await (await connect(tA)).send('Page.navigate', { url: 'sta://settings/' }).catch(() => null);
  await sleep(1000);
  check('d', 'web tab cannot navigate to sta:// (browser-initiated, on_before_browse)', (await evalIn(tA, 'location.href')).startsWith('data:text/html') && log().includes('blocked sta:// navigation in a web tab'));
  const fetchResult = await evalIn(tA, `fetch('sta://sidebar/').then(function () { return 'loaded'; }, function (e) { return 'blocked'; })`);
  check('d', 'web tab cannot fetch sta:// resources', fetchResult === 'blocked', fetchResult);
  const openUrlBefore = (await counts()).openUrl || 0;
  // .invalid never resolves: the tab core opens for it makes no network traffic.
  await evalIn(SB, `location.href = 'https://e2e-sidebar-escape.invalid/'; 'navigating'`);
  await sleep(1000);
  const sbTarget = (await targets()).find((t) => t.url.startsWith('sta://sidebar/'));
  check('d', 'sidebar stays on sta://sidebar/ after navigating away', !!sbTarget);
  const openUrlAfter = await waitFor(async () => {
    const n = (await counts()).openUrl || 0;
    return n > openUrlBefore && n;
  }, 3000);
  check('d', 'the blocked sidebar navigation became OpenUrl{NewTab}', openUrlAfter === openUrlBefore + 1, { openUrlBefore, openUrlAfter });
  // Core opened that URL as a Today tab: close it again so the content area is empty.
  await invoke(SB, 'dispatch', { type: 'closeItem' });
  const emptyAgain = await waitFor(async () => (await info()).window.emptyVisible, 5000);
  check('d', 'closing the core tab shows the empty state again', emptyAgain);

  // ---------------------------------------------------------------- (d2) UI escapes, external protocols
  // about:blank never reaches on_before_browse: the address/load-start guard brings UI pages back.
  await evalIn(SB, `location.href = 'about:blank'; 'navigating'`);
  const sidebarBack = await waitFor(async () => {
    const t = (await targets()).find((x) => x.url.startsWith('sta://sidebar/'));
    return t && (await evalIn(t, 'typeof window.__staQuery')) === 'function' && (await invoke(SB, 'state.get')).ok;
  }, 8000, 200);
  check('d2', 'sidebar navigated to about:blank reloads sta://sidebar/ with IPC', sidebarBack && log().includes('left sta:// for about:blank'));
  const navigateBefore = (await counts()).navigate || 0;
  await invoke(SB, 'dispatch', { type: 'openInternalPage', page: 'settings' });
  const settingsT = await waitFor(async () => (await targets()).find((x) => x.url.startsWith('sta://settings/')), 8000);
  await waitFor(async () => (await evalIn(settingsT, 'typeof window.__staQuery')) === 'function', 5000);
  await evalIn(settingsT, `location.href = 'about:blank'; 'navigating'`);
  const settingsTab = (await invoke(SB, 'state.get')).ok.focusedTab;
  const replaced = await waitFor(async () => {
    const x = await info();
    const t = tabInfo(x, settingsTab);
    const live = t && x.browsers.live.find((b) => b.id === t.browserId);
    return t && !t.internal && live && live.ui === false && x;
  }, 8000);
  check('d2', 'internal page navigated to about:blank: Navigate → replaced by an untrusted web browser', replaced && ((await counts()).navigate || 0) === navigateBefore + 1, replaced && tabInfo(replaced, settingsTab));
  const blankT = await waitFor(async () => (await targets()).find((x) => x.url === 'about:blank'), 5000);
  check('d2', 'the about:blank page has no IPC', blankT && (await evalIn(blankT, 'typeof window.__staQuery')) === 'undefined');
  await invoke(SB, 'dispatch', { type: 'closeItem', id: settingsTab });

  // External protocols in a web tab (STA_TEST_EXTERNAL_PROTOCOL=1: launches are only logged).
  const extPage = 'data:text/html,<title>E2E-EXT</title><a id="m" href="mailto:e2e-link@example.invalid">mail</a>' +
    '<a id="b" target="_blank" href="mailto:e2e-blank@example.invalid">blank</a><a id="d" href="ms-msdt:/id%20PCWDiagnostic">msdt</a>';
  const openExt = await invoke(SB, 'debug.openTab', { url: extPage, show: true });
  const tExt = await waitFor(async () => (await targets()).find((t) => t.title === 'E2E-EXT'), 8000);
  const launched = (u) => log().includes(`external protocol (test): ${u}`);
  await evalIn(tExt, `document.getElementById('m').click(); 'clicked'`, { gesture: true });
  check('d2', 'mailto: link click (user gesture) → handed to the OS, page stays', await waitFor(() => launched('mailto:e2e-link@example.invalid'), 3000) && (await evalIn(tExt, 'document.title')) === 'E2E-EXT');
  await evalIn(tExt, `location.href = 'mailto:e2e-script@example.invalid'; 'x'`);
  await sleep(800);
  check('d2', 'mailto: navigation without a user gesture → not launched, no error page', !launched('mailto:e2e-script@example.invalid') && log().includes('without a user gesture ignored (navigation): mailto:e2e-script') && (await evalIn(tExt, 'location.protocol')) === 'data:');
  const adoptedBefore = (await counts()).popupAdopted || 0;
  await evalIn(tExt, `document.getElementById('b').click(); 'clicked'`, { gesture: true });
  check('d2', 'target=_blank mailto: → launched, no popup tab adopted', await waitFor(() => launched('mailto:e2e-blank@example.invalid'), 3000) && ((await counts()).popupAdopted || 0) === adoptedBefore);
  await evalIn(tExt, `document.getElementById('d').click(); 'clicked'`, { gesture: true });
  await sleep(800);
  check('d2', 'denied handler scheme (ms-msdt:) is never launched', !log().includes('external protocol (test): ms-msdt') && log().includes('not launched (navigation, denied scheme)'));
  // debug.openTab bypassed core: put core's layout (Empty) back after closing it.
  await execute([{ type: 'destroyBrowser', tab: openExt.ok.tab }, { type: 'showContent', layout: { type: 'empty' } }]);
  check('d2', 'empty state again', await waitFor(async () => (await info()).window.emptyVisible, 5000));
  // Typed external protocols (command bar / openInput / openUrl / navigate): Effect::OpenExternal,
  // handed to the OS without creating or navigating a tab.
  const tabsBefore = (await invoke(SB, 'state.get')).ok.spaces.flatMap((sp) => sp.today).length;
  const externalBefore = log().split('effect OpenExternal').length - 1;
  await invoke(SB, 'dispatch', { type: 'openInput', text: 'mailto:e2e-typed@example.invalid', target: 'newTab' });
  await invoke(SB, 'dispatch', { type: 'commitOmnibox', command: { type: 'openInput', text: 'tel:+15550100', target: 'currentTab' }, alt: false });
  await invoke(SB, 'dispatch', { type: 'openUrl', url: 'zoommtg://zoom.us/join?confno=1', target: 'newTab' });
  await invoke(SB, 'dispatch', { type: 'openUrl', url: 'ms-msdt:/id PCWDiagnostic', target: 'newTab' });
  const typedOk = await waitFor(() => launched('mailto:e2e-typed@example.invalid') && launched('tel:+15550100') && launched('zoommtg://zoom.us/join?confno=1'), 6000);
  await sleep(500);
  const afterTyped = (await invoke(SB, 'state.get')).ok;
  check('d2', 'typed mailto:/tel:/app URLs → OpenExternal launches them (logged in test mode)', typedOk && log().split('effect OpenExternal').length - 1 === externalBefore + 4, { typedOk, openExternalEffects: log().split('effect OpenExternal').length - 1 });
  check('d2', '… without creating or navigating a tab (empty state stays)', afterTyped.spaces.flatMap((sp) => sp.today).length === tabsBefore && !afterTyped.current && (await info()).window.emptyVisible);
  check('d2', 'a typed denied handler (ms-msdt:) is still never launched', !log().includes('external protocol (test): ms-msdt') && log().includes('not launched (typed URL, denied scheme)'));

  // ---------------------------------------------------------------- (e) command bar
  const CMD = 'sta://command/';
  const ctrlT = { key: 84, ctrl: true };
  const accel = await invoke(SB, 'debug.accelerator', ctrlT);
  check('e', 'Ctrl+T is bound', accel.ok && accel.ok.commandId >= 1000, accel);
  i = await waitFor(async () => {
    const x = await info();
    return overlay(x, 'CommandBar').visible && x;
  }, 3000);
  const bar = i && overlay(i, 'CommandBar');
  check('e', 'Ctrl+T shows the command bar', bar && bar.visible, bar);
  if (bar) {
    const [cx, cy, cw, ch] = i.window.contentRect;
    const [x, y, w, h] = bar.bounds;
    const expectedW = Math.max(480, Math.min(680, Math.floor(cw * 0.56)));
    check('e', 'command bar is centered over the content, width clamp(480, 56%, 680)', x >= cx && x + w <= cx + cw && Math.abs(x + w / 2 - (cx + cw / 2)) <= 1 && w === Math.min(expectedW, cw - 16), { bar: bar.bounds, content: i.window.contentRect });
    check('e', 'command bar top = content top + max(72, 14%), height from the page', y === cy + Math.max(72, Math.floor(ch * 0.14)) && h >= 56 && y + h <= cy + ch, bar.bounds);
    // The hole is the overlay host: the visible card (`bounds`) plus its shadow (`hostBounds`).
    const [hx, hy, hw, hh] = bar.hostBounds;
    const hole = await waitFor(async () =>
      (await info()).window.draggableRegions.some((r) => r[0] === hx && r[1] === hy && r[2] === hw && r[3] === hh && r[4] === 0),
    3000);
    check('e', 'the host is the card plus its 8 DIP shadow', hx <= x - 8 && hy <= y - 8 && hx + hw >= x + w + 8 && hy + hh >= y + h + 8, { card: bar.bounds, host: bar.hostBounds });
    check('e', 'visible overlay punches a no-drag hole into the drag regions', hole);
    console.log('  ' + (await capture('command')));
  }
  await evalIn(CMD, `document.querySelector('input').focus(); 'focused'`);
  const cmdConn = await connect(await targetId(CMD));
  for (const type of ['rawKeyDown', 'keyUp']) {
    await cmdConn.send('Input.dispatchKeyEvent', { type, key: 'Escape', code: 'Escape', windowsVirtualKeyCode: 27 });
  }
  const hiddenByEsc = await waitFor(async () => !overlay(await info(), 'CommandBar').visible, 3000);
  check('e', 'Esc in the command bar closes it (closeCommandBar)', hiddenByEsc);
  await invoke(SB, 'debug.accelerator', ctrlT);
  const shown2 = await waitFor(async () => overlay(await info(), 'CommandBar').visible, 3000);
  const seq1 = (await invoke(SB, 'state.get')).ok.commandBar?.seq;
  await invoke(SB, 'debug.accelerator', ctrlT);
  const reissued = await waitFor(async () => (await invoke(SB, 'state.get')).ok.commandBar?.seq > seq1, 3000);
  check('e', 'Ctrl+T while open re-issues the bar (new seq) and keeps it visible', shown2 && reissued && overlay(await info(), 'CommandBar').visible, { seq1 });
  await invoke(SB, 'dispatch', { type: 'closeCommandBar' });
  const hidden2 = await waitFor(async () => !overlay(await info(), 'CommandBar').visible, 3000);
  check('e', 'closeCommandBar hides it', hidden2);
  await invoke(SB, 'debug.accelerator', { key: 76, ctrl: true });
  const shownL = await waitFor(async () => overlay(await info(), 'CommandBar').visible, 3000);
  await execute({ type: 'hideCommandBar' });
  const hiddenL = await waitFor(async () => !overlay(await info(), 'CommandBar').visible, 3000);
  check('e', 'Ctrl+L opens it; Effect::HideCommandBar hides it', shownL && hiddenL);
  await invoke(SB, 'dispatch', { type: 'closeCommandBar' }); // resync core

  // ---------------------------------------------------------------- (e2) search suggestions
  const suggestInfo = async () => (await info()).suggest;
  const suggest = (text, match = CMD) => invoke(match, 'omnibox.suggest', { text });
  const settingsNow = async () => (await invoke(SB, 'state.get')).ok.settings;
  const lastQuery = () => suggestServer.completions().at(-1)?.q;
  check('e2', 'search suggestions are on by default (fresh profile)', (await settingsNow()).searchSuggestions === true);
  // A cookie for the suggestion host exists in the global context: it must not be sent.
  const cookieTab = await invoke(SB, 'debug.openTab', { url: `http://127.0.0.1:${suggestServer.port}/setcookie`, show: false });
  const cookieT = await waitFor(async () => (await targets()).find((t) => t.title === 'E2E-COOKIE'), 8000);
  const cookieSet = cookieT && (await waitFor(async () => (await evalIn(cookieT, 'document.cookie')).includes('e2esuggest=1'), 3000));
  check('e2', 'a cookie is stored for the suggestion host (setup)', cookieSet);
  await execute([{ type: 'destroyBrowser', tab: cookieTab.ok.tab }, { type: 'showContent', layout: { type: 'empty' } }]);

  let n0 = suggestServer.count();
  const rust = await suggest('rust');
  const rustReq = suggestServer.completions().at(-1);
  check('e2', 'omnibox.suggest from the command bar returns the parsed OpenSearch list (trimmed, deduped, without the query)', rust.ok && rust.ok.text === 'rust' && JSON.stringify(rust.ok.suggestions) === JSON.stringify(['rust one', 'rust two', 'rustgramming language']), rust);
  check('e2', '… fetched once through the endpoint, without cookies', suggestServer.count() === n0 + 1 && rustReq && rustReq.q === 'rust' && rustReq.cookie === null, rustReq);
  const korean = await suggest('러스트 게');
  check('e2', 'Korean text is UTF-8 percent-encoded and the reply decoded', korean.ok && korean.ok.text === '러스트 게' && korean.ok.suggestions[0] === '러스트 게 one' && lastQuery() === '러스트 게', { korean, q: lastQuery() });
  const forced = await suggest('?rust lang ');
  check('e2', 'a leading "?" is stripped before fetching (the reply echoes the typed text)', forced.ok && forced.ok.text === '?rust lang ' && lastQuery() === 'rust lang ' && forced.ok.suggestions[0] === 'rust lang  one', { forced, q: lastQuery() });

  const suggestFromSidebar = await suggest('rust', SB);
  const suggestFromTopbar = await suggest('rust', 'sta://topbar/');
  check('e2', 'omnibox.suggest from the sidebar / topbar is rejected (403)', suggestFromSidebar.err === 403 && suggestFromTopbar.err === 403, { suggestFromSidebar, suggestFromTopbar });
  const badPayload = await invoke(CMD, 'omnibox.suggest', { query: 'x' });
  check('e2', 'omnibox.suggest without text is rejected (400)', badPayload.err === 400, badPayload);

  n0 = suggestServer.count();
  const skippedBefore = (await suggestInfo()).skipped;
  const noFetch = ['https://example.com/x', 'mailto:me@example.com', 'C:\\Users\\me', 'zoommtg://zoom.us/join', '', '   ', '?', 'a'.repeat(257)];
  const noFetchReplies = [];
  for (const text of noFetch) noFetchReplies.push(await suggest(text));
  check('e2', 'URL-like, blank and over-long text → [] without a request', noFetchReplies.every((r, k) => r.ok && r.ok.text === noFetch[k] && r.ok.suggestions.length === 0) && suggestServer.count() === n0 && (await suggestInfo()).skipped === skippedBefore + noFetch.length, { replies: noFetchReplies.map((r) => (r.ok ? r.ok.suggestions.length : r)), requests: suggestServer.count() - n0 });

  // Superseded: a newer request resolves the one in flight with [] at once.
  n0 = suggestServer.count();
  const supersededBefore = (await suggestInfo()).superseded;
  // The first request is left pending in the page until the server has it (the server answers it
  // only after 1 s); then the second one is sent.
  await evalIn(CMD, `window.__e2eFirst = window.sta.invoke('omnibox.suggest', { text: 'slow first' }).then(function (r) { return { r: r, at: Date.now() }; }); 'sent'`);
  const firstArrived = await waitFor(() => suggestServer.completions().some((r) => r.q === 'slow first'), 3000, 20);
  const newer = await evalIn(CMD, `(function () { var sent = Date.now(); return window.sta.invoke('omnibox.suggest', { text: 'rust' }).then(function (r) { return { r: r, sent: sent }; }); })()`);
  const older = await evalIn(CMD, 'window.__e2eFirst');
  check('e2', 'a newer request supersedes the one in flight: that resolves [] at once, the newer one answers', firstArrived && older.r.text === 'slow first' && older.r.suggestions.length === 0 && older.at - newer.sent < 500 && newer.r.suggestions.length === 3 && suggestServer.count() === n0 + 2 && (await suggestInfo()).superseded === supersededBefore + 1, { older, newer: newer.r, afterNewerMs: older.at - newer.sent });

  const timedOutBefore = (await suggestInfo()).timedOut;
  const hang = await evalIn(CMD, `(function () { var t0 = Date.now(); return window.sta.invoke('omnibox.suggest', { text: 'hang' }).then(function (r) { return { r: r, ms: Date.now() - t0 }; }); })()`);
  check('e2', 'no answer within 1500 ms → [] (timeout)', hang.r.suggestions.length === 0 && hang.ms >= 1400 && hang.ms < 3000 && (await suggestInfo()).timedOut === timedOutBefore + 1, hang);
  const failures = [];
  for (const text of ['big body', 'status 500', 'garbage']) failures.push(await suggest(text));
  check('e2', 'a body over 64 KB, HTTP 500 and non-JSON → []', failures.every((r) => r.ok && r.ok.suggestions.length === 0), failures.map((r) => (r.ok ? r.ok.suggestions.length : r)));
  const idle = await waitFor(async () => (await suggestInfo()).inFlight === 0, 3000);
  check('e2', 'nothing left in flight', idle, await suggestInfo());

  // Disabled setting, and engines without a suggestion endpoint: [] without a request.
  await invoke(SB, 'dispatch', { type: 'updateSettings', patch: { searchSuggestions: false } });
  await waitFor(async () => (await settingsNow()).searchSuggestions === false, 3000);
  n0 = suggestServer.count();
  const disabled = await suggest('rust');
  check('e2', 'searchSuggestions off → [] without a request', disabled.ok && disabled.ok.suggestions.length === 0 && suggestServer.count() === n0, disabled);
  await invoke(SB, 'dispatch', { type: 'updateSettings', patch: { searchSuggestions: true, searchEngine: 'kagi' } });
  await waitFor(async () => (await settingsNow()).searchEngine === 'kagi', 3000);
  const kagi = await suggest('rust');
  check('e2', 'an engine without suggestions (Kagi) → [] without a request', kagi.ok && kagi.ok.suggestions.length === 0 && suggestServer.count() === n0, kagi);
  await invoke(SB, 'dispatch', { type: 'updateSettings', patch: { searchEngine: 'duckDuckGo' } });
  await waitFor(async () => (await settingsNow()).searchEngine === 'duckDuckGo', 3000);
  const ddg = await suggest('rust');
  check('e2', 'DuckDuckGo has suggestions (the test endpoint replaces its URL)', ddg.ok && ddg.ok.suggestions.length === 3 && suggestServer.count() === n0 + 1, ddg);
  await invoke(SB, 'dispatch', { type: 'updateSettings', patch: { searchEngine: 'google' } });
  await waitFor(async () => (await settingsNow()).searchEngine === 'google', 3000);

  // The fetched list feeds omnibox.query: Suggestions rows and an inline completion.
  const pro = await suggest('rust pro');
  const q = await invoke(CMD, 'omnibox.query', { text: 'rust pro', mode: 'newTab', preventInlineAutocomplete: false, suggestions: pro.ok.suggestions, seq: 5 });
  const rows = q.ok && q.ok.results.filter((r) => r.group === 'suggestions').map((r) => r.title);
  check('e2', 'omnibox.query with the fetched suggestions: inline completion from the first extending one', q.ok && q.ok.inlineCompletion === 'rust pro one' && q.ok.seq === 5, { suggestions: pro.ok.suggestions, inline: q.ok && q.ok.inlineCompletion });
  check('e2', '… Enter searches the completion; the other suggestions are rows', q.ok && q.ok.results[0].key === 'search' && q.ok.results[0].command.type === 'openUrl' && q.ok.results[0].command.url === 'https://www.google.com/search?q=rust%20pro%20one' && JSON.stringify(rows) === JSON.stringify(['rust pro two', 'rust programming language']), q.ok && { first: q.ok.results[0], rows });
  const deleted = await invoke(CMD, 'omnibox.query', { text: 'rust pro', mode: 'newTab', preventInlineAutocomplete: true, suggestions: pro.ok.suggestions, seq: 6 });
  check('e2', 'after a deletion: no inline completion, the suggestion rows still show', deleted.ok && deleted.ok.inlineCompletion === null && deleted.ok.results.filter((r) => r.group === 'suggestions').length === 3, deleted.ok && deleted.ok.inlineCompletion);
  check('e2', 'typed text is never logged', !log().includes('rust pro') && !log().includes('slow first'));
  const cookieTab2 = await invoke(SB, 'debug.openTab', { url: `http://127.0.0.1:${suggestServer.port}/setcookie?again=1`, show: false });
  const cookieT2 = await waitFor(async () => (await targets()).find((t) => t.title === 'E2E-COOKIE' && t.url.includes('again=1')), 8000);
  const jar = cookieT2 && (await waitFor(async () => { const c = await evalIn(cookieT2, 'document.cookie'); return c.includes('e2esuggest=1') && c; }, 3000));
  const suggestCookies = suggestServer.completions().filter((r) => r.cookie !== null);
  check('e2', 'suggestion requests never sent a cookie, and their Set-Cookie was not stored', jar && !jar.includes('e2efromsuggest') && suggestCookies.length === 0, { jar, suggestCookies });
  await execute([{ type: 'destroyBrowser', tab: cookieTab2.ok.tab }, { type: 'showContent', layout: { type: 'empty' } }]);

  // ---------------------------------------------------------------- (f) window
  i = await info();
  const scale = native.dpi / 96;
  const px = (v) => Math.round(v * scale);
  const [ww, wh] = [native.width, native.height];
  const hit = await win('hittest', [
    `${px(sidebarWidth + 200)},${px(20)}`, // topbar drag area
    `${px(60)},${px(20)}`, // sidebar top row drag area
    `${ww - px(20)},${px(20)}`, // close caption button
    `${px(sidebarWidth + 300)},${px(300)}`, // content
    `1,${Math.floor(wh / 2)}`,
    `${ww - 2},${Math.floor(wh / 2)}`,
    `${Math.floor(ww / 2)},1`,
    `${Math.floor(ww / 2)},${wh - 2}`,
    `1,1`,
    `${ww - 2},${wh - 2}`,
  ].join(';'));
  check('f', 'drag regions: topbar + sidebar top row are CAPTION; caption button and content are CLIENT', hit[0] === 2 && hit[1] === 2 && hit[2] === 1 && hit[3] === 1, hit.slice(0, 4));
  check('f', 'resize edges: LEFT RIGHT TOP BOTTOM TOPLEFT BOTTOMRIGHT', JSON.stringify(hit.slice(4)) === JSON.stringify([10, 11, 12, 15, 13, 17]), hit.slice(4));
  check('f', 'draggable regions were applied to the window', i.window.draggableRegions.some((r) => r[4] === 1 && r[0] === sidebarWidth && r[1] === 0), i.window.draggableRegions.length);

  const stateChangesBefore = (await counts()).windowStateChanged || 0;
  await invoke('sta://topbar/', 'dispatch', { type: 'windowControl', action: 'toggleMaximize' });
  const maxed = await waitFor(async () => (await info()).window.maximized, 4000);
  check('f', 'windowControl toggleMaximize maximizes (UI dispatch → core → Effect::Window)', maxed && (await win('info')).zoomed);
  await execute({ type: 'window', action: 'toggleMaximize' });
  const restored = await waitFor(async () => (await info()).window.maximized === false, 4000);
  check('f', 'Effect::Window toggleMaximize restores (window::window_action)', restored && !(await win('info')).zoomed);
  await execute({ type: 'window', action: 'minimize' });
  const minimized = await waitFor(async () => (await win('info')).iconic, 4000, 200);
  check('f', 'Effect::Window minimize minimizes', minimized);
  await win('restore');
  const unminimized = await waitFor(async () => !(await win('info')).iconic, 4000, 200);
  check('f', 'SC_RESTORE restores the window', unminimized);
  const stateChangesAfter = await waitFor(async () => {
    const n = (await counts()).windowStateChanged || 0;
    return n > stateChangesBefore && n;
  }, 3000);
  check('f', 'WindowStateChanged is dispatched on maximize/restore', stateChangesAfter > stateChangesBefore, { stateChangesBefore, stateChangesAfter });

  await invoke(SB, 'dispatch', { type: 'toggleSidebar' });
  const hiddenSidebar = await waitFor(async () => {
    const x = await info();
    return !x.window.sidebar.visible && x.window.contentRect[0] === 8 && x;
  }, 3000);
  check('f', 'toggleSidebar hides the sidebar; content gets an 8px left inset', hiddenSidebar, hiddenSidebar && hiddenSidebar.window.contentRect);
  await invoke('sta://topbar/', 'debug.accelerator', { key: 83, ctrl: true });
  const shownSidebar = await waitFor(async () => {
    const x = await info();
    return x.window.sidebar.visible && x.window.contentRect[0] === sidebarWidth;
  }, 3000);
  check('f', 'Ctrl+S shows it again', shownSidebar);
  await invoke(SB, 'sidebar.setWidth', { width: 320 });
  const wide = await waitFor(async () => (await info()).window.contentRect[0] === 320, 3000);
  await invoke(SB, 'sidebar.setWidth', { width: sidebarWidth });
  const narrow = await waitFor(async () => (await info()).window.contentRect[0] === sidebarWidth, 3000);
  check('f', 'sidebar.setWidth resizes live', wide && narrow);
  const fromTopbar = await invoke('sta://topbar/', 'sidebar.setWidth', { width: 300 });
  check('f', 'sidebar.setWidth from another surface is rejected (403)', fromTopbar.err === 403, fromTopbar);

  // ---------------------------------------------------------------- (g) tabs
  const pageB =
    'data:text/html,<title>E2E-B</title><body style="background:%23cde"><h1>E2E tab B</h1>' +
    '<script>addEventListener("beforeunload", function (e) { e.preventDefault(); e.returnValue = ""; })</script>';
  const openB = await invoke(SB, 'debug.openTab', { url: pageB, show: false });
  const tabB = openB.ok && openB.ok.tab;
  const tB = await waitFor(async () => (await targets()).find((t) => t.url.includes('E2E-B')), 8000);
  check('g', 'second tab created hidden', tB && typeof tabB === 'number' && !tabInfo(await info(), tabB).visible);
  // beforeunload only prompts after a user activation.
  const activated = await evalIn(tB, 'navigator.userActivation.hasBeenActive', { gesture: true });
  check('g', 'tab B has user activation (beforeunload will prompt)', activated === true);

  await execute({ type: 'showContent', layout: { type: 'single', tab: tabB } });
  i = await waitFor(async () => {
    const x = await info();
    return tabInfo(x, tabB).visible && !tabInfo(x, tabA).visible && x;
  }, 3000);
  const [, , cw, ch] = (i || (await info())).window.contentRect;
  check('g', 'ShowContent Single B: only B visible, fills the content', i && JSON.stringify(tabInfo(i, tabB).wrapperBounds) === JSON.stringify([0, 0, cw, ch]), i && tabInfo(i, tabB).wrapperBounds);
  check('g', 'empty-state view hidden while a tab is shown', i && i.window.emptyVisible === false);
  await execute({ type: 'showContent', layout: { type: 'single', tab: tabA } });
  const switched = await waitFor(async () => {
    const x = await info();
    return tabInfo(x, tabA).visible && !tabInfo(x, tabB).visible;
  }, 3000);
  check('g', 'switch to A', switched);

  await execute({
    type: 'showContent',
    layout: { type: 'split', orientation: 'horizontal', panes: [{ tab: tabA, fraction: 0.6 }, { tab: tabB, fraction: 0.4 }], focused: 1 },
  });
  i = await waitFor(async () => {
    const x = await info();
    return tabInfo(x, tabA).visible && tabInfo(x, tabB).visible && x;
  }, 3000);
  if (i) {
    const a = tabInfo(i, tabA).wrapperBounds;
    const b = tabInfo(i, tabB).wrapperBounds;
    check('g', 'horizontal split: A left, B right, 6px gap, ~60/40', a[0] === 0 && b[0] === a[2] + 6 && Math.abs(a[2] / (a[2] + b[2]) - 0.6) < 0.02 && a[3] === ch && b[3] === ch, { a, b });
    check('g', 'focused pane wrapper uses the accent color', tabInfo(i, tabB).wrapperColor !== tabInfo(i, tabA).wrapperColor, [tabInfo(i, tabA).wrapperColor, tabInfo(i, tabB).wrapperColor]);
    console.log('  ' + (await capture('split')));
  } else {
    check('g', 'split shows both panes', false);
  }
  await execute({
    type: 'showContent',
    layout: { type: 'split', orientation: 'vertical', panes: [{ tab: tabA, fraction: 0.5 }, { tab: tabB, fraction: 0.5 }], focused: 0 },
  });
  i = await waitFor(async () => {
    const x = await info();
    return tabInfo(x, tabB).wrapperBounds[1] > 0 && x;
  }, 3000);
  check('g', 'vertical split stacks the panes', i && tabInfo(i, tabA).wrapperBounds[1] === 0 && tabInfo(i, tabB).wrapperBounds[0] === 0, i && [tabInfo(i, tabA).wrapperBounds, tabInfo(i, tabB).wrapperBounds]);

  const closedBefore = (await counts()).tabBrowserClosed || 0;
  const browserB = tabInfo(await info(), tabB).browserId;
  await execute({ type: 'destroyBrowser', tab: tabB });
  i = await waitFor(async () => {
    const x = await info();
    return !tabInfo(x, tabB) && x.tabs.closing.length === 0 && (x.controller.commandCounts.tabBrowserClosed || 0) === closedBefore + 1 && x;
  }, 6000);
  check('g', 'DestroyBrowser B: close flow ends with on_before_close + TabBrowserClosed (beforeunload auto-accepted)', i && log().includes(`browser ${browserB} closed (role=Some(Tab(${tabB})))`));
  check('g', 'window stays open after closing a tab', i && i.window.exists && !i.window.closing);
  await execute({ type: 'showContent', layout: { type: 'single', tab: tabA } });
  i = await waitFor(async () => {
    const x = await info();
    return JSON.stringify(tabInfo(x, tabA).wrapperBounds) === JSON.stringify([0, 0, cw, ch]) && x;
  }, 3000);
  check('g', 'A fills the content again', i);

  const browserA = tabInfo(await info(), tabA).browserId;
  await execute({ type: 'replaceBrowser', tab: tabA, url: 'data:text/html,<title>E2E-A2</title><h1>replaced</h1>', internal: false });
  i = await waitFor(async () => {
    const x = await info();
    const t = tabInfo(x, tabA);
    return t && t.browserId && t.browserId !== browserA && t.visible && !x.browsers.live.some((b) => b.id === browserA) && x;
  }, 6000);
  check('g', 'ReplaceBrowser swaps the view in place; old browser closes silently', i && (i.controller.commandCounts.tabBrowserClosed || 0) === closedBefore + 1, i && tabInfo(i, tabA));

  // ---------------------------------------------------------------- (h) single instance
  const openUrlBeforeRelaunch = (await counts()).openUrl || 0;
  // console-ok: sta.exe is the GUI child under test; windowsHide (libuv HIDE_GUI) would start it invisible
  const second = spawn(EXE, [`--sta-data-dir=${DATA}`, 'data:text/html,<title>E2E-RELAUNCH</title>relaunched'], { stdio: 'ignore' });
  const secondExit = await new Promise((resolve) => {
    const timer = setTimeout(() => resolve('timeout'), 15000);
    second.on('exit', (code) => {
      clearTimeout(timer);
      resolve(code);
    });
  });
  check('h', 'second launch with the same data dir forwards its command line and exits 0', secondExit === 0, { exitCode: secondExit });
  const relaunched = await waitFor(async () => ((await counts()).openUrl || 0) === openUrlBeforeRelaunch + 1, 5000);
  check('h', 'running instance got on_already_running_app_relaunch → OpenUrl', relaunched && log().includes('relaunch forwarded 1 url(s)'));
  const relaunchTab = await waitFor(async () => (await targets()).find((t) => t.title === 'E2E-RELAUNCH'), 8000);
  check('h', 'the forwarded URL opened as a tab', relaunchTab);

  // ------------------------------------------------- (hygiene) no console window during the run
  await checkNoConsoleWindows(inst, consoles, check);

  // ---------------------------------------------------------------- (f2) shutdown
  if (KEEP_OPEN) {
    console.log('--keep-open: leaving the browser running');
    return;
  }
  const statePath = path.join(DATA, 'sta', 'state.json');
  rmSync(statePath, { force: true });
  await win('close'); // WM_CLOSE: same path as Alt+F4 / taskbar close
  const t0 = Date.now();
  const exited = await waitFor(() => !alive(inst.pid), 15000, 100);
  check('f2', 'WM_CLOSE → can_close → WindowCloseRequested → [SaveNow, Quit] → process exits', exited, `${Date.now() - t0} ms`);
  const gone = await waitFor(() => ourProcesses().length === 0, 10000, 250);
  check('f2', 'no sta.exe processes of this data dir remain', gone);
  const text = log();
  for (const line of ['command WindowCloseRequested', 'effect SaveNow', 'effect Quit', 'shutdown: all browsers closed', 'window destroyed', 'exited cleanly']) {
    check('f2', `log: ${line}`, text.includes(line));
  }
  check('f2', 'no panics', !text.includes('PANIC') && !text.includes('panicked'));
  check('f2', 'state.json written by SaveNow', existsSync(statePath));
}

try {
  await main();
} catch (e) {
  check('run', 'unexpected error', false, e.stack || String(e));
} finally {
  inst?.closeSockets();
  suggestServer?.close();
  if (!KEEP_OPEN && inst?.pid && alive(inst.pid)) {
    console.log('killing leftover process tree');
    killTree(inst.pid);
  }
  const failed = results.filter((r) => !r.ok);
  console.log(`\n${results.length - failed.length}/${results.length} checks passed`);
  process.exit(failed.length ? 1 : 0);
}
