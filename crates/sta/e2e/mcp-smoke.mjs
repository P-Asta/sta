#!/usr/bin/env node
// Smoke test of the debug-only MCP test surface (docs/TESTING.md): launches a browser with the
// surface armed, connects through target/debug/sta-mcp.exe over stdio, and exercises **every**
// `test_*` tool once — plus the four locks and the console-window rule.
//
//   cargo build -p sta -p sta-mcp --features test-hooks
//   node crates/sta/e2e/mcp-smoke.mjs [--keep-open]
//
// It needs no DevTools port and no PowerShell: that is the point of the surface. Data directory
// E2E_DATA_DIR (default C:/ast/tmp/s6/tools/smoke); the HTTP fixture takes an ephemeral port.
// Real OS input (test_real_keys, test_send_key) only works while sta is the foreground window, so
// those two checks accept `window_busy` when something else has the foreground.

import { spawn } from 'node:child_process';
import { existsSync, mkdirSync, openSync, rmSync, writeFileSync } from 'node:fs';
import http from 'node:http';
import path from 'node:path';
import { McpClient, PipeClient, TEST_ARGS, TEST_ENV, repo, waitForEndpoint } from './mcp.mjs';

const EXE = path.join(repo, 'target/debug/sta.exe');
const DATA = process.env.E2E_DATA_DIR || 'C:/ast/tmp/s6/tools/smoke';
const KEEP_OPEN = process.argv.includes('--keep-open');
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

const results = [];
function check(section, name, ok, detail) {
  results.push({ section, name, ok: !!ok });
  let d = detail === undefined ? '' : ' ' + (typeof detail === 'string' ? detail : JSON.stringify(detail));
  if (d.length > 200) d = d.slice(0, 200) + '…';
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

/** Runs `fn`, turning a TestToolError into `{error: code}` so one bad tool can't end the run. */
async function attempt(fn) {
  try {
    return await fn();
  } catch (e) {
    return { error: e.code || 'threw', message: e.message };
  }
}

function startSite() {
  const server = http.createServer((req, res) => {
    res.writeHead(200, { 'Content-Type': 'text/html; charset=utf-8', 'Cache-Control': 'no-store' });
    res.end(`<!doctype html><meta charset=utf-8><title>smoke</title><h1 id=h>smoke ${req.url}</h1><input id=i><script>window.smokeValue = 41 + 1;</script>`);
  });
  return new Promise((ok) => server.listen(0, '127.0.0.1', () => ok(server)));
}

/** A browser process with its own data directory; no CDP port, no console helper. */
function launch(dataDir, { args = [], env = {}, tag = 'smoke', hide = false } = {}) {
  mkdirSync(path.dirname(dataDir), { recursive: true });
  // console-ok: sta.exe is the GUI child under test; windowsHide (libuv HIDE_GUI) would start it invisible
  const child = spawn(EXE, [`--sta-data-dir=${dataDir}`, '--disable-backgrounding-occluded-windows', ...args], {
    env: { ...process.env, STA_DEBUG_HOVER_REVEAL: '0', ...env },
    stdio: ['ignore', openSync(`${dataDir}-${tag}-stdout.txt`, 'w'), openSync(`${dataDir}-${tag}-stderr.txt`, 'w')],
    windowsHide: hide, // only for probes that exit at once: a GUI child must not be started hidden
  });
  return child;
}

function killTree(pid) {
  if (!pid) return;
  try {
    // console-ok: taskkill with windowsHide
    spawn('taskkill', ['/PID', String(pid), '/T', '/F'], { stdio: 'ignore', windowsHide: true });
  } catch {
    // already gone
  }
}

async function main() {
  if (!existsSync(EXE)) throw new Error(`${EXE} not found: cargo build -p sta -p sta-mcp --features test-hooks`);
  rmSync(DATA, { recursive: true, force: true });
  const site = await startSite();
  const H = `http://127.0.0.1:${site.address().port}`;

  // ------------------------------------------------------------------ (arm) the four locks
  const browser = launch(DATA, { args: TEST_ARGS, env: TEST_ENV });
  console.log(`launched pid ${browser.pid} (data ${DATA})`);
  const endpoint = await waitForEndpoint(DATA, 40000);
  check('arm', 'the armed browser opened its agent endpoint without a settings change', !!endpoint.pipe, endpoint.pipe?.slice(-8));

  const client = new McpClient({ dataDir: DATA, clientInfo: { name: 'mcp-smoke', title: 'MCP smoke', version: '1.0' } }).start();
  const init = await client.initialize();
  check('arm', 'initialize returns the sta server info', init?.serverInfo?.name === 'sta', init?.serverInfo);
  const before = await client.listTools();
  check('arm', 'an un-connected bridge lists only the 23 shipped tools', before.length === 23 && !before.some((t) => t.name.startsWith('test_')), before.length);

  const info = await client.test('test_info', { sections: ['window', 'tabs'] });
  check('arm', 'test_info answers once the browser welcomed an armed session', typeof info.at === 'number' && !!info.window, Object.keys(info));
  // Baseline for (h): the console windows already on the desktop, by handle. No `reset` — a console
  // that flashed while the browser started must stay in `seen` (see lib.mjs `consoleBaseline`).
  const consoleBase = new Set(((await client.test('test_console_windows', {})).current || []).map((c) => c.hwnd));

  const after = await waitFor(async () => {
    const list = await client.listTools();
    return list.length > 23 ? list : null;
  }, 5000);
  const testTools = after.filter((t) => t.name.startsWith('test_'));
  check('arm', 'the armed tools/list adds all 35 test tools', testTools.length === 35, testTools.length);
  check('arm', 'every test tool is tagged _meta["sta/test"] and is not read-only', testTools.every((t) => t._meta?.['sta/test'] === true && t.annotations?.readOnlyHint === false));
  check('arm', 'the armed list is not cacheable and a list_changed notification was sent', client.toolsResult.ttlMs === 0 && client.sawToolListChanged(), {
    ttlMs: client.toolsResult.ttlMs,
    notified: client.sawToolListChanged(),
  });
  const unknown = await client.call('test_not_a_tool', {});
  check('arm', 'an unknown test name is refused', unknown.isError === true, unknown.text?.slice(0, 80));

  // ------------------------------------------------------------------ (a) core and shell
  const state = await client.test('test_state');
  check('a', 'test_state returns the core UiState', Array.isArray(state.spaces), Object.keys(state).slice(0, 6));
  const counts = await client.test('test_counts');
  check('a', 'test_counts returns the command counters', typeof counts.commandCounts === 'object' && counts.commandCounts !== null);
  const opened = await client.test('test_open_tab', { url: `${H}/page`, show: true });
  check('a', 'test_open_tab opens a tab with a fresh id', Number.isInteger(opened.tab) && opened.tab > 0, opened);
  const tab = opened.tab;
  const loaded = await waitFor(async () => {
    const i = await client.test('test_info', { sections: ['tabs'] });
    return i.tabs.tabs.find((t) => t.tab === tab && t.url?.startsWith(H));
  }, 15000);
  check('a', 'the tab loaded the fixture', !!loaded, loaded && loaded.url);
  // `test_push_state` pushes a `state` event to every IPC subscriber, so the only place it can be
  // *observed* is a subscriber: a counter installed on the topbar surface, with `on('state', …)`
  // rather than `onState`, which would also fire once for the snapshot already cached.
  await client.test('test_eval', {
    target: { surface: 'topbar' },
    expression: `(() => { window.__smokeStates = 0; window.sta.on('state', () => { window.__smokeStates += 1; }); return true; })()`,
  });
  const statesBefore = Number((await client.test('test_eval', { target: { surface: 'topbar' }, expression: 'window.__smokeStates' })).value);
  await client.test('test_push_state');
  const pushed = await waitFor(async () => {
    const v = Number((await client.test('test_eval', { target: { surface: 'topbar' }, expression: 'window.__smokeStates' })).value);
    return v > statesBefore ? v : null;
  }, 5000, 100);
  check('a', 'test_push_state delivers a state event to a surface subscriber', pushed !== null && pushed > statesBefore, { before: statesBefore, after: pushed });
  await client.test('test_dispatch', { command: { type: 'openUrl', url: `${H}/dispatched`, target: 'newTab' } });
  const dispatched = await waitFor(async () => {
    const i = await client.test('test_info', { sections: ['tabs'] });
    return i.tabs.tabs.find((t) => t.url?.includes('/dispatched'));
  }, 10000);
  check('a', 'test_dispatch reaches the core and the shell', !!dispatched, dispatched && dispatched.tab);
  await client.test('test_execute', { effects: [{ type: 'showContent', layout: { type: 'single', tab } }] });
  const shown = await waitFor(async () => (await client.test('test_info', { sections: ['tabs'] })).tabs.tabs.find((t) => t.tab === tab)?.visible, 5000);
  check('a', 'test_execute runs effects', shown === true, shown);
  await client.test('test_focus', { tab });
  // Read the focus back: `debug.info.focus` names the focused browser and its role, so the check
  // is that the *tab's* view took focus, not merely that the call returned without an error.
  const focused = await waitFor(async () => {
    const i = await client.test('test_info', { sections: ['focus'] });
    return i.focus?.role === `Tab(${tab})` ? i.focus : null;
  }, 5000, 100);
  check('a', 'test_focus focuses that tab view (debug.info.focus reads it back)', focused?.role === `Tab(${tab})`, focused);
  const accel = await attempt(() => client.test('test_accelerator', { key: 0x54, ctrl: true }));
  check('a', 'test_accelerator runs the Ctrl+T binding', Number.isInteger(accel.commandId), accel);
  const sent = await attempt(() => client.test('test_send_key', { key: 0x1b }));
  check('a', 'test_send_key reaches the window (or reports window_busy)', sent === null || sent?.error === 'window_busy', sent);
  const reset = await client.test('test_reset_permissions', { origin: H, bits: 1 });
  check('a', 'test_reset_permissions resets content settings', typeof reset.reset === 'number', reset);

  // ------------------------------------------------------------------ (b) JavaScript
  const evalTab = await client.test('test_eval', { target: { tab }, expression: 'window.smokeValue' });
  check('b', 'test_eval runs in a web tab', evalTab.value === 42, evalTab);
  const evalSurface = await client.test('test_eval', { target: { surface: 'topbar' }, expression: 'location.href' });
  check('b', 'test_eval runs inside an sta:// surface (the shipped evaluate refuses those)', String(evalSurface.value).startsWith('sta://topbar'), evalSurface);
  const evalMatch = await client.test('test_eval', { target: { match: 'sta://sidebar' }, expression: 'document.title', userGesture: true });
  check('b', 'test_eval finds a target by URL and can carry a user gesture', evalMatch.error === undefined, evalMatch);
  const threw = await client.test('test_eval', { target: { tab }, expression: 'throw new Error("smoke")' });
  check('b', 'test_eval reports an exception instead of failing the call', !!threw.error && /smoke/.test(threw.error.text), threw.error?.text?.slice(0, 60));
  const invoked = await client.test('test_invoke', { target: { surface: 'topbar' }, cmd: 'debug.info' });
  check('b', 'test_invoke goes through the real window.sta.invoke path', !!invoked.ok && !!invoked.ok.window, Object.keys(invoked.ok || {}).slice(0, 4));

  // ------------------------------------------------------------------ (c) DevTools and targets
  const targets = await client.test('test_targets', {});
  check('c', 'test_targets lists sta browsers with their roles', Array.isArray(targets) && targets.some((t) => t.role?.startsWith('surface:')) && targets.some((t) => t.role === `tab:${tab}`), targets.length);
  const history = await client.test('test_cdp', { target: { tab }, method: 'Page.getNavigationHistory' });
  check('c', 'test_cdp sends any DevTools method (no allowlist)', !!history.result?.entries, history.error || history.result?.entries?.length);
  const events = await client.test('test_cdp_events', { target: { tab } });
  check('c', 'test_cdp_events returns the event buffer', Array.isArray(events.events), events.events.length);
  const pageTarget = (await client.test('test_cdp', { target: { surface: 'topbar' }, method: 'Target.getTargets' })).result?.targetInfos?.find((t) => t.type === 'page');
  const attached = pageTarget ? await attempt(() => client.test('test_attach', { targetId: pageTarget.targetId })) : { error: 'no page target' };
  check('c', 'test_attach returns a flattened DevTools session', typeof attached.sessionId === 'string', attached.sessionId ? attached.sessionId.slice(0, 8) : attached);
  if (attached.sessionId) {
    const inSession = await attempt(() => client.test('test_eval', { target: { targetId: pageTarget.targetId }, expression: '1 + 1' }));
    check('c', 'test_eval runs inside an attached session', inSession.value === 2, inSession);
  }

  // ------------------------------------------------------------------ (d) input
  const mouse = await client.test('test_post_mouse', { steps: [{ type: 'move', x: 200, y: 20 }, { waitMs: 20 }, { type: 'move', x: 220, y: 24 }] });
  check('d', 'test_post_mouse posts mouse messages to our own window', mouse.posted >= 2, mouse);
  const hover = await client.test('test_hover_input', { enabled: true });
  check('d', 'test_hover_input drives the sidebar hover reveal', typeof hover === 'object' && hover !== null, Object.keys(hover).slice(0, 5));
  await client.test('test_hover_input', { enabled: false });
  const tabKey = await client.test('test_tab_key', { tab, key: 'a' });
  check('d', 'test_tab_key sends a key event into a tab', tabKey.sent === true, tabKey);
  const keys = await attempt(() => client.test('test_real_keys', { combo: 'escape' }, { timeoutMs: 20000 }));
  check('d', 'test_real_keys injects real OS keys (or reports window_busy)', keys.sent >= 1 || keys.error === 'window_busy', keys);

  // ------------------------------------------------------------------ (e) native window and pixels
  const win = await client.test('test_window');
  check('e', 'test_window reports the native window', win.hwnd !== 0 && win.width > 0 && typeof win.dpi === 'number' && win.modifiers, { w: win.width, dpi: win.dpi });
  const all = await client.test('test_window', { all: true });
  check('e', 'test_window {all} lists this process top-level windows', Array.isArray(all.windows) && all.windows.some((w) => w.hwnd === win.hwnd), all.windows.length);
  const hits = await client.test('test_hit_test', { points: [[Math.round(win.width / 2), 20], [1, Math.round(win.height / 2)]] });
  check('e', 'test_hit_test returns WM_NCHITTEST codes', Array.isArray(hits.codes) && hits.codes.length === 2 && hits.codes[1] === 10, hits.codes);
  const shot = await client.test('test_capture', { out: `${DATA}-capture.png` });
  check('e', 'test_capture writes a PNG of the window', shot.width === win.width && shot.bytes > 1000, { w: shot.width, h: shot.height, bytes: shot.bytes });
  const inline = await client.test('test_capture', { region: 'client', inline: true });
  check('e', 'test_capture can return the PNG inline', typeof inline.data === 'string' && inline.data.length > 100, inline.data?.length);
  const colors = await client.test('test_pixels', { path: `${DATA}-capture.png`, points: [[10, 10], [50, 50]], space: 'device' });
  check('e', 'test_pixels samples a capture', colors.colors.length === 2 && /^#[0-9a-f]{6}$/.test(colors.colors[0]), colors.colors);
  // The user's clipboard is theirs: save it, round-trip, put it back (chrome-e2e does the same).
  const savedClipboard = (await client.test('test_clipboard_get')).text;
  await client.test('test_clipboard_set', { text: 'sta smoke clipboard' });
  const clip = await client.test('test_clipboard_get');
  check('e', 'test_clipboard_set / test_clipboard_get round-trip', clip.text === 'sta smoke clipboard', clip.text);
  if (savedClipboard) await client.test('test_clipboard_set', { text: savedClipboard });
  const plain = `${DATA}-plain.txt`;
  writeFileSync(plain, 'no mark of the web');
  const zone = await client.test('test_zone_identifier', { path: plain });
  check('e', 'test_zone_identifier reports a file without a Mark of the Web', zone.zone === null, zone);
  // `test_dialog` only ever answers a dialog sta's main window *owns*: with no dialog up, the
  // foreground window is sta's own and the tool refuses instead of typing into it.
  const noDialog = await attempt(() => client.test('test_dialog', { press: 'escape', hwnd: win.hwnd }));
  check('e', 'test_dialog refuses the main window itself', noDialog.error === 'invalid_arguments', noDialog);
  const iconic = await client.test('test_window_message', { message: 'minimize' });
  const minimized = await waitFor(async () => (await client.test('test_window')).iconic, 4000, 200);
  await client.test('test_window_message', { message: 'restore' });
  const restored = await waitFor(async () => !(await client.test('test_window')).iconic, 4000, 200);
  check('e', 'test_window_message posts SC_MINIMIZE and SC_RESTORE', iconic.posted === 'SC_MINIMIZE' && minimized === true && restored === true, { minimized, restored });

  // ------------------------------------------------------------------ (f) Chrome-created browsers
  const foreign = await client.test('test_foreign');
  check('f', 'test_foreign returns the foreign snapshot', typeof foreign === 'object' && foreign !== null, Object.keys(foreign).slice(0, 5));
  const closed = await client.test('test_foreign_close', { id: -1 });
  check('f', 'test_foreign_close answers for an unknown browser', closed.closed === false, closed);
  const triggered = await attempt(() => client.test('test_foreign_trigger', { url: `${H}/foreign` }));
  check('f', 'test_foreign_trigger makes Chromium create a browser', typeof triggered.targetId === 'string', triggered);

  // ------------------------------------------------------------------ (g) batching
  const batch = await client.test('test_batch', {
    calls: [{ name: 'test_info', args: { sections: ['window'] } }, { name: 'test_counts' }, { name: 'test_window' }],
  });
  check('g', 'test_batch runs several calls in one round trip', batch.results.length === 3 && batch.results.every((r) => r.ok), batch.results.map((r) => r.name));
  const guarded = await attempt(() => client.test('test_batch', { calls: [{ name: 'test_real_keys', args: { combo: 'escape' } }] }));
  check('g', 'test_batch refuses test_real_keys and nested batches', guarded.error === 'invalid_arguments', guarded);

  // ------------------------------------------------------------------ (pipe) the raw channel
  const pipe = await new PipeClient({ dataDir: DATA, endpoint }).connect();
  pipe.hello({ name: 'mcp-smoke-pipe' });
  const welcome = await waitFor(async () => {
    const line = await pipe.next(5000);
    return line && line.t === 'welcome' ? line : null;
  }, 8000, 50);
  check('pipe', 'the raw pipe client is welcomed and the armed browser reports testHooks', welcome?.t === 'welcome' && welcome.testHooks === true, welcome);

  // The 8 MiB line limit costs one *call*, never the session. Both halves of that rule:
  //   1. a line the peer should never have sent is dropped and the connection carries on;
  //   2. an answer that does not fit comes back as `too_large` for that call alone.
  // Until channel.rs was fixed, (1) was a protocol error that closed the pipe and (2) produced it
  // — one ~20 MB `test_cdp` result killed the whole MCP session, every other call with it.
  const OVER = 9 * 1024 * 1024;
  pipe.send('x'.repeat(OVER) + '\n');
  pipe.send({ t: 'call', id: 91, tool: 'test_info', args: { sections: ['window'] }, deadlineMs: 30000 });
  const afterOversized = await waitFor(async () => {
    const line = await pipe.next(8000);
    return line && line.t === 'result' && line.id === 91 ? line : null;
  }, 20000, 20);
  check('pipe', 'an oversized line is dropped and the session answers the next call', !!afterOversized && !pipe.closed, afterOversized ? { id: afterOversized.id, error: afterOversized.error } : { closed: pipe.closed, seen: pipe.lines.map((l) => l.t) });
  pipe.close();

  const tooBig = await client.call('test_eval', { target: { tab }, expression: `'x'.repeat(${OVER})` });
  check('pipe', 'a result over the line limit fails that one call with too_large', tooBig.isError && tooBig.code === 'too_large', tooBig.text?.slice(0, 120));
  const stillThere = await attempt(() => client.test('test_info', { sections: ['window'] }));
  check('pipe', 'and the MCP session is still usable right after it', typeof stillThere.at === 'number', stillThere.error || Object.keys(stillThere));

  // ------------------------------------------------------------------ (h) no console window
  const consoles = await client.test('test_console_windows', { roots: [process.pid, browser.pid] });
  // `seen`, not `ours`: a console window the Windows 11 default terminal hosts belongs to
  // WindowsTerminal.exe and descends from no tree of ours (lib.mjs `checkNoConsoleWindows`).
  const shownConsoles = (consoles.seen || []).filter((c) => c.userVisible && !consoleBase.has(c.hwnd));
  check('h', 'no console window was shown while the suite ran', shownConsoles.length === 0, shownConsoles);
  check('h', 'the console watch is installed', consoles.watching === true, consoles.watching);

  // ------------------------------------------------------------------ (i) the locks, from outside
  // Arming without an explicit data directory must exit(2) rather than run un-armed.
  // `LOCALAPPDATA` is redirected into the suite's own folder: the probe exercises the same code path
  // with nothing real in reach, so a regression in the ordering of `arm_from_process` and
  // `paths::resolve` cannot touch the user's profile while this check is still passing.
  const probeLocalAppData = `${DATA}-lock3-localappdata`;
  mkdirSync(probeLocalAppData, { recursive: true });
  const exit2 = await new Promise((ok) => {
    // console-ok: a probe that exits with code 2 before CEF starts, so it never shows a window
    const probe = spawn(EXE, ['--sta-test-hooks', '--disable-backgrounding-occluded-windows'], {
      env: { ...process.env, STA_E2E: '1', STA_DATA_DIR: '', LOCALAPPDATA: probeLocalAppData },
      stdio: 'ignore',
      windowsHide: true, // a probe that exits at once; it never shows a window
    });
    probe.on('exit', (code) => ok(code));
  });
  check('i', 'arming without an explicit data directory exits with code 2', exit2 === 2, exit2);

  const plainDir = `${DATA}-unarmed`;
  rmSync(plainDir, { recursive: true, force: true });
  const unarmed = launch(plainDir, { args: [], env: {}, tag: 'unarmed' });
  const unarmedEndpoint = await attempt(() => waitForEndpoint(plainDir, 12000));
  check('i', 'a browser started without the switches opens no agent endpoint (access is off)', !!unarmedEndpoint.error, unarmedEndpoint.error || unarmedEndpoint);
  killTree(unarmed.pid);

  // ------------------------------------------------------------------ done
  site.close();
  await client.close();
  if (!KEEP_OPEN) killTree(browser.pid);
  const failed = results.filter((r) => !r.ok);
  console.log(`\n${results.length - failed.length}/${results.length} checks passed`);
  for (const f of failed) console.log(`  FAILED [${f.section}] ${f.name}`);
  if (client.stderr.trim()) console.log(`\n[bridge stderr]\n${client.stderr.trim().slice(0, 2000)}`);
  process.exit(failed.length ? 1 : 0);
}

main().catch((e) => {
  console.error(e);
  process.exit(1);
});
