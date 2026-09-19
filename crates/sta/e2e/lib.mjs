// Shared helpers for the shell end-to-end scripts (Windows, Node 22+).
//
// Everything that talks to a *running* browser goes through **MCP**: JSON-RPC over stdio to
// target/debug/sta-mcp.exe, which forwards to the browser over its named pipe (see mcp.mjs and
// docs/TESTING.md). The suites therefore need a browser built and armed for it:
//
//   cargo build -p sta -p sta-mcp --features test-hooks
//   node crates/sta/e2e/<suite>.mjs
//
// `Instance` launches target/debug/sta.exe with its own data dir, `--sta-test-hooks` and
// `STA_E2E=1`, waits for `<data>/sta/agent-endpoint.json` the first time a helper needs the
// browser, and only ever kills the process tree it started (other instances may run concurrently).
// The data dir alone makes a run unique: no DevTools port is needed any more (`port` is still
// honoured, for shell-e2e's one remaining `/json/version` liveness check).
//
// What each helper now uses (docs/TESTING.md has the full catalog):
//   - `info/state/dispatch/execute/focus/counts`          -> test_info, test_state, test_dispatch, …
//   - `invoke(match, cmd, payload)`                       -> test_invoke, a **real**
//     `window.sta.invoke` in the surface's own frame, so ipc.rs `trusted_frame`, the CEF message
//     router and the `window.sta` shim stay covered;
//   - `eval`                                              -> test_eval (works in sta:// surfaces);
//   - `targets/target/connect(t).send(method, params)`     -> test_targets / test_cdp (raw DevTools);
//   - `keys/hover/mouse`                                  -> test_real_keys / test_hover_input /
//     test_post_mouse (real OS keys are still only delivered while our window is the foreground);
//   - `win/capture/pixels/clipboard/zone`                  -> test_window, test_hit_test,
//     test_window_message, test_capture, test_pixels, test_clipboard_*, test_zone_identifier —
//     which is why **none** of them shells out to PowerShell any more. They are `async` now.
//
// What is deliberately *not* MCP (it is about not having a running browser): launching sta.exe,
// enumerating/killing the process tree, exit codes, startup failures, refused/forwarded second
// launches, the data-folder migration, reading files, and the local HTTP fixtures.
// C:/ast/tmp/s6/cdp-residue.md lists every remaining non-MCP check with its reason.
//
// Instances start with the sidebar hover reveal off (`STA_DEBUG_HOVER_REVEAL=0`), so a cursor
// resting at the window's left edge can't float the hidden sidebar during unrelated checks;
// `hover({enabled: true})` turns it on.
//
// No console window is ever opened: every helper process is spawned with `windowsHide: true` and
// nothing goes through cmd.exe. `consoleBaseline()` / `checkNoConsoleWindows()` assert that at
// runtime — on every console window *shown* while the suite ran, so a window that only flashed, and
// one the Windows 11 default terminal hosts (whose process is nobody's child), both fail the check.

import { spawn, execFileSync } from 'node:child_process';
import { existsSync, rmSync, mkdirSync, openSync, readFileSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { McpClient, TEST_ARGS, TEST_ARGS_NO_APPROVE, TEST_ENV, requireArmedBridge, waitForEndpoint } from './mcp.mjs';

export const here = path.dirname(fileURLToPath(import.meta.url));
export const repo = path.resolve(here, '../../..');
export const EXE = path.join(repo, 'target/debug/sta.exe');
export const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
/** Windows of other instances may cover ours: keep rendering (screenshots, rAF-driven pages). */
export const OCCLUSION_FLAG = '--disable-backgrounding-occluded-windows';
/** Environment every e2e instance starts with (see the header). */
export const E2E_ENV = { STA_DEBUG_HOVER_REVEAL: '0', ...TEST_ENV };
export { McpClient, TEST_ARGS, TEST_ARGS_NO_APPROVE, TEST_ENV, requireArmedBridge, waitForEndpoint };

// ------------------------------------------------------------------------------------ reporting

export const results = [];
export function check(section, name, ok, detail) {
  results.push({ section, name, ok: !!ok });
  let d = detail === undefined ? '' : ' ' + (typeof detail === 'string' ? detail : JSON.stringify(detail));
  if (ok && d.length > 240) d = d.slice(0, 240) + '…';
  console.log(`${ok ? 'PASS' : 'FAIL'} [${section}] ${name}${d}`);
  return !!ok;
}

export function summary() {
  const failed = results.filter((r) => !r.ok);
  console.log(`\n${results.length - failed.length}/${results.length} checks passed`);
  for (const f of failed) console.log(`  FAILED [${f.section}] ${f.name}`);
  return failed.length;
}

export async function waitFor(fn, timeoutMs = 5000, stepMs = 100) {
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

// ------------------------------------------------------------------------------------ processes

export function alive(pid) {
  try {
    process.kill(pid, 0);
    return true;
  } catch {
    return false;
  }
}

/**
 * Processes whose command line contains `needle` (to verify our own tree is gone). Not MCP: the
 * point of the check is that no browser is left to answer (cdp-residue.md #43).
 */
export function processesWith(needle) {
  const script = `Get-CimInstance Win32_Process -Filter "Name='sta.exe'" | Where-Object { $_.CommandLine -like '*${needle.replace(/'/g, "''")}*' } | Select-Object -ExpandProperty ProcessId`;
  const out = execFileSync('powershell', ['-NoProfile', '-Command', script], { encoding: 'utf8', windowsHide: true }).trim();
  return out ? out.split(/\s+/).map(Number) : [];
}

/** A PowerShell script, for the few probes that are about *not* having a live browser. */
export function ps(file, args) {
  return execFileSync('powershell', ['-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', file, ...args], {
    encoding: 'utf8',
    windowsHide: true, // a visible console window would steal the foreground from the window under test
  }).trim();
}

export function killTree(pid) {
  try {
    execFileSync('taskkill', ['/PID', String(pid), '/T', '/F'], { stdio: 'ignore', windowsHide: true });
  } catch {
    // already gone
  }
}

// ------------------------------------------------------------------------------------ instance

/**
 * A test-tool target selector for one of sta's own browsers. `{browser: id}` is exact — a URL can
 * be shared by two tabs, and DevTools windows and Chrome-created popups are neither a tab nor a
 * surface — so it is preferred over `{tab}` / `{surface}` / `{match}` here.
 */
function selectorForBrowser(t) {
  if (Number.isInteger(t.browser)) return { browser: t.browser };
  const tab = /^tab:(\d+)$/.exec(t.role || '');
  if (tab) return { tab: Number(tab[1]) };
  const surface = /^surface:(.+)$/.exec(t.role || '');
  if (surface) return { surface: surface[1] };
  return t.targetId ? { targetId: t.targetId } : null;
}

export class Instance {
  /**
   * `data` is required and must not be a real profile (the test surface refuses to arm otherwise).
   * `port` only sets `STA_REMOTE_DEBUGGING_PORT` for the suites that still assert the DevTools port
   * answers; nothing in this file uses it. `approve: false` arms with
   * `--sta-test-hooks-no-approve`, i.e. leaves the connection prompt and agent access exactly as
   * the profile has them (agent-e2e's consent sections).
   */
  constructor({ data, port, exe = EXE, fresh = true, args = [], env = {}, arm = true, approve = true, clientInfo, timeoutMs = 30000, endpointMs = 40000 }) {
    this.data = data;
    this.exe = exe;
    this.port = port === undefined ? undefined : String(port);
    this.fresh = fresh;
    this.args = args;
    this.env = env;
    /** `arm: false` starts an ordinary browser with no test surface — for the checks that are about
     *  a browser which never becomes serviceable (a fatal startup error, a refused second launch). */
    this.arm = arm;
    this.testArgs = arm ? (approve ? TEST_ARGS : TEST_ARGS_NO_APPROVE) : [];
    this.baseEnv = arm ? E2E_ENV : { STA_DEBUG_HOVER_REVEAL: '0' };
    this.clientInfo = clientInfo || { name: 'sta-e2e', title: 'sta e2e', version: '1.0' };
    this.timeoutMs = timeoutMs;
    this.endpointMs = endpointMs;
    /** The MCP session (lazily connected on the first helper call). */
    this.driver = null;
    /** DevTools target ids `test_attach` already opened a session for. */
    this.attached = new Set();
  }

  start(tag = 'run') {
    if (!existsSync(this.exe)) throw new Error(`${this.exe} not found: run cargo build -p sta -p sta-mcp --features test-hooks first`);
    // Before anything is launched: a bridge built without `test-hooks` (what a plain `cargo test`
    // or `cargo clippy` leaves behind) would let the whole suite start and then fail on its first
    // call with `unknown tool: test_info`, which reads like a browser arming failure.
    if (this.arm) requireArmedBridge();
    if (this.fresh) rmSync(this.data, { recursive: true, force: true });
    mkdirSync(path.dirname(this.data), { recursive: true });
    this.closeSockets();
    this.stderrPath = `${this.data}-${tag}-stderr.txt`;
    // console-ok: sta.exe is the GUI child under test; windowsHide (libuv HIDE_GUI) would start it invisible
    this.child = spawn(this.exe, [`--sta-data-dir=${this.data}`, OCCLUSION_FLAG, ...this.testArgs, ...this.args], {
      env: { ...process.env, ...this.baseEnv, ...(this.port ? { STA_REMOTE_DEBUGGING_PORT: this.port } : {}), ...this.env },
      stdio: ['ignore', openSync(`${this.data}-${tag}-stdout.txt`, 'w'), openSync(this.stderrPath, 'w')],
    });
    this.pid = this.child.pid;
    console.log(`launched pid ${this.pid} (data ${this.data}); logs ${this.stderrPath}`);
    return this;
  }

  log() {
    return readFileSync(this.stderrPath, 'utf8');
  }

  kill() {
    this.closeSockets();
    if (this.pid && alive(this.pid)) killTree(this.pid);
  }

  /** Drops the MCP session (and the DevTools sessions it attached); the next call reconnects. */
  closeSockets() {
    this.attached.clear();
    const driver = this.driver;
    this.driver = null;
    if (driver) driver.close().catch(() => null);
  }

  // -------------------------------------------------------------------------------- MCP session

  /**
   * The MCP session, connecting (and waiting for the endpoint file) on first use. Concurrent first
   * callers share one connect (chrome-e2e's gating watch polls while the suite works).
   */
  async mcp() {
    if (this.driver) return this.driver;
    if (!this.arm) throw new Error(`instance ${this.data} was started without the test surface (arm: false)`);
    if (!this.connecting) {
      this.connecting = this.#connect().finally(() => {
        this.connecting = null;
      });
    }
    return this.connecting;
  }

  async #connect() {
    await waitForEndpoint(this.data, this.endpointMs);
    const client = new McpClient({ dataDir: this.data, clientInfo: this.clientInfo, timeoutMs: this.timeoutMs }).start();
    let last;
    for (let attempt = 1; attempt <= 3; attempt++) {
      try {
        await client.initialize();
        this.driver = client;
        return client;
      } catch (e) {
        last = e;
        await sleep(500);
      }
    }
    await client.close();
    throw last;
  }

  /** One test tool: the structured answer, or a `TestToolError`. */
  async t(name, args = {}, opts = {}) {
    return (await this.mcp()).test(name, args, opts);
  }

  /** A tool call with its raw `{text, structured, isError, code}` (the 23 shipped tools included). */
  async call(name, args = {}, opts = {}) {
    return (await this.mcp()).call(name, args, opts);
  }

  /** The stderr of the bridge process, for failure reports. */
  bridgeStderr() {
    return this.driver ? this.driver.stderr : '';
  }

  // -------------------------------------------------------------------------------- native

  /**
   * What `win-probe.ps1` answered, over MCP: `info` (+ `modifiers`), `hittest` ("x,y;x,y" in
   * **device** pixels, like the script), `close` / `restore` / `minimize`, `dialogs` and
   * `closedialogs`. Now `async`.
   */
  async win(cmd = 'info', arg) {
    switch (cmd) {
      case 'info':
        return this.t('test_window');
      case 'modifiers':
        return (await this.t('test_window')).modifiers;
      case 'hittest': {
        const points = (Array.isArray(arg) ? arg : String(arg).split(';'))
          .map((p) => (Array.isArray(p) ? p : String(p).split(',').map(Number)))
          .filter((p) => p.length === 2 && p.every((n) => Number.isFinite(n)));
        return (await this.t('test_hit_test', { points, space: 'device' })).codes;
      }
      case 'close':
      case 'restore':
      case 'minimize':
        return this.t('test_window_message', { message: cmd });
      case 'dialogs':
        return (await this.dialogs()).map((w) => ({ hwnd: w.hwnd, title: w.title, owner: w.owner }));
      case 'owned':
        return this.ownedWindows();
      case 'closedialogs': {
        const list = await this.dialogs();
        for (const w of list) await this.t('test_window_message', { message: 'close', hwnd: w.hwnd });
        return { closed: list.length };
      }
      default:
        throw new Error(`unknown win() command ${cmd}`);
    }
  }

  /** Visible `#32770` windows of the browser process (native dialogs, the fatal error box). */
  async dialogs() {
    const all = await this.t('test_window', { all: true });
    return (all.windows || []).filter((w) => w.className === '#32770' && w.visible);
  }

  /**
   * Visible top-level windows **sta's main window owns** — Chromium's own dialogs (the extension
   * install dialog) are `Chrome_WidgetWin_1` views widgets, not `#32770`, so `dialogs()` never sees
   * them. Each entry carries `style` and `exStyle`, the two words
   * `foreign.rs::is_install_dialog_style` reads.
   */
  async ownedWindows({ any = false } = {}) {
    const all = await this.t('test_window', { all: true });
    const main = all.main && all.main.hwnd;
    const ours = new Set((all.windows || []).map((w) => w.hwnd));
    // `any`: owned by *some* window of sta's — the remove confirmation belongs to the hidden
    // `chrome://extensions` window of an extension operation, not to the main window.
    return (all.windows || []).filter((w) => w.visible && w.hwnd !== main && (w.owner === main || (any && w.owner && ours.has(w.owner))));
  }

  /**
   * Presses a key in a modal dialog sta's main window owns (default: its default button). The only
   * way to accept Chromium's "Add extension?" dialog: its buttons are views, and it holds the
   * foreground itself, so `keys()` (which activates sta's main window first) cannot reach it.
   */
  async acceptDialog(press = 'enter', hwnd) {
    return this.t('test_dialog', hwnd === undefined ? { press } : { press, hwnd });
  }

  /** A PNG of the window (never the whole screen) -> the human-readable summary line. */
  async capture(name) {
    const out = `${this.data}-${name}.png`;
    const shot = await this.t('test_capture', { out });
    return `saved ${shot.path} (${shot.width}x${shot.height})`;
  }

  /** Colors (`#rrggbb`) of a capture made with `capture(name)` at `[[x, y], …]` in DIP. */
  async pixels(name, points) {
    return (await this.t('test_pixels', { path: `${this.data}-${name}.png`, points, space: 'dip' })).colors;
  }

  /** The clipboard, as `Get-Clipboard` / `Set-Clipboard` used to read and write it. */
  async clipboard() {
    return (await this.t('test_clipboard_get')).text;
  }

  async setClipboard(text) {
    await this.t('test_clipboard_set', { text });
  }

  /** A file's `Zone.Identifier` alternate stream (Mark of the Web), or `null`. */
  async zoneIdentifier(file) {
    return (await this.t('test_zone_identifier', { path: file })).zone;
  }

  // -------------------------------------------------------------------------------- targets, CDP

  /**
   * `/json/list`-shaped targets: sta's own browsers (`role` `tab:N` / `surface:host`) merged with
   * what `Target.getTargets` adds (titles, extension pages, service workers). Every entry carries
   * a `sel` selector for the other helpers.
   */
  async targets() {
    const all = await this.t('test_targets', {});
    // `/json/list` used to answer newest target first, and suites rely on it: two tabs can show the
    // same URL, and `find(t => t.url === …)` must pick the one that was just opened. CEF browser ids
    // grow with creation, so descending id reproduces that order.
    const list = [...all].sort((a, b) => (Number.isInteger(a.browser) && Number.isInteger(b.browser) ? b.browser - a.browser : 0));
    const cdp = list.filter((t) => t.role === 'cdp');
    const taken = new Set();
    const out = [];
    for (const t of list) {
      if (t.role === 'cdp') continue;
      let k = -1;
      for (let j = 0; j < cdp.length; j++) {
        if (!taken.has(j) && cdp[j].type === 'page' && cdp[j].url === t.url) {
          k = j;
          break;
        }
      }
      if (k >= 0) taken.add(k);
      const m = k >= 0 ? cdp[k] : null;
      out.push({
        id: t.id,
        type: 'page',
        url: t.url,
        title: m ? m.title : undefined,
        role: t.role,
        browser: t.browser,
        targetId: m ? m.id : undefined,
        sel: selectorForBrowser({ ...t, targetId: m ? m.id : undefined }),
      });
    }
    for (let j = 0; j < cdp.length; j++) {
      if (taken.has(j)) continue;
      out.push({ ...cdp[j], targetId: cdp[j].id, sel: { targetId: cdp[j].id } });
    }
    return out;
  }

  async target(match) {
    const t = (await this.targets()).find((t) => t.type === 'page' && (typeof match === 'function' ? match(t) : t.url.includes(match)));
    if (!t) throw new Error(`no target matching ${match}`);
    return t;
  }

  /**
   * A test-tool target selector for a match string, a target from `targets()`, a tab number or a
   * selector object. A bare DevTools target id is attached first, and a failed attach throws here:
   * without a session the browser answers `no_such_target` (test_hooks/js.rs `resolve`), so an id
   * that cannot be attached must fail loudly rather than silently measuring something else.
   */
  async selector(match) {
    let sel;
    if (typeof match === 'string') sel = { match };
    else if (typeof match === 'number') sel = { tab: match };
    else if (match && match.sel) sel = match.sel;
    else if (match && (match.surface || match.tab !== undefined || match.browser !== undefined || match.match || match.targetId)) sel = match;
    if (!sel) throw new Error(`cannot address target ${JSON.stringify(match)}`);
    if (sel.targetId && !this.attached.has(sel.targetId)) {
      await this.t('test_attach', { targetId: sel.targetId });
      this.attached.add(sel.targetId);
    }
    return sel;
  }

  /**
   * A raw DevTools channel for a target: `send(method, params)` is one `test_cdp` call (no method
   * allowlist, exactly like `debug.cdp`). `close()` is a no-op — there is no socket to close.
   */
  async connect(t) {
    const target = await this.selector(t);
    return {
      target,
      send: async (method, params = {}) => {
        const r = await this.t('test_cdp', { target, method, params });
        if (r.error !== undefined) throw new Error(typeof r.error === 'string' ? r.error : JSON.stringify(r.error));
        return r.result;
      },
      events: async (clear = false) => (await this.t('test_cdp_events', { target, clear })).events,
      close: () => {},
    };
  }

  // -------------------------------------------------------------------------------- JS and IPC

  /** Evaluates `expr` in the first page whose URL contains `match` (or a target object). */
  async eval(match, expr, { gesture = false, timeoutMs } = {}) {
    const target = await this.selector(match);
    const args = { target, expression: expr, userGesture: gesture };
    if (timeoutMs) args.timeoutMs = timeoutMs;
    const r = await this.t('test_eval', args);
    if (r.error) throw new Error(`eval failed in ${JSON.stringify(target)}: ${JSON.stringify(r.error)}`);
    return r.value;
  }

  /** `window.sta.invoke` on a trusted page, the real IPC path → `{ok}` or `{err, msg}`. */
  async invoke(match, cmd, payload = null) {
    const target = await this.selector(match);
    return this.t('test_invoke', { target, cmd, payload });
  }

  /** A test tool that answers like the old `{ok}` / `{err, msg}` IPC reply (never throws). */
  async #reply(name, args) {
    try {
      return { ok: await this.t(name, args) };
    } catch (e) {
      return { err: e.code, msg: e.message };
    }
  }

  async info(sections) {
    return this.t('test_info', sections ? { sections } : {});
  }

  state() {
    return this.t('test_state');
  }

  dispatch(command) {
    return this.#reply('test_dispatch', { command });
  }

  execute(effects) {
    return this.#reply('test_execute', { effects });
  }

  focus(target) {
    return this.#reply('test_focus', target);
  }

  // No wrappers for `test_push_state`, `test_open_tab`, `test_accelerator` or `test_tab_key`: the
  // suites ask for those through `invoke(surface, 'debug.pushState' | 'debug.openTab' |
  // 'debug.accelerator' | 'debug.tabKey')`, which is the *stronger* path — it goes through the
  // trusted-frame check, the message router and `window.sta` on the way to the same code. The tools
  // themselves are exercised by mcp-smoke. A wrapper nothing calls only rots.

  /**
   * Real OS keys (`{combo}` or `{steps}`). Other windows (e.g. other sta instances under test)
   * may take the foreground: a sequence that could not start is retried; one that was interrupted
   * throws an Error with `.interrupted = true` (the shell released the keys it held) so callers can
   * redo the whole measurement.
   */
  async keys(spec, { attempts = 6 } = {}) {
    const payload = typeof spec === 'string' ? { combo: spec } : spec;
    let r = null;
    let busy = null;
    for (let attempt = 1; attempt <= attempts; attempt++) {
      busy = null;
      try {
        r = await this.t('test_real_keys', payload, { timeoutMs: 40000 });
      } catch (e) {
        if (e.code !== 'window_busy') throw new Error(`realKeys ${JSON.stringify(payload)} failed: ${e.message}`);
        busy = e;
        r = null;
      }
      if (!busy && !(r && r.aborted && r.sent === 0)) break;
      console.log(`  (realKeys: foreground busy, retry ${attempt}: ${busy ? busy.message : r.reason})`);
      await sleep(1500);
    }
    if (busy) throw new Error(`realKeys ${JSON.stringify(payload)} failed: ${busy.message}`);
    if (r && r.aborted) {
      await sleep(800); // let the shell release held keys
      throw Object.assign(new Error(`realKeys ${JSON.stringify(payload)} interrupted: ${r.reason}`), { interrupted: true });
    }
    return r;
  }

  async counts() {
    return (await this.t('test_counts')).commandCounts || {};
  }

  /** The sidebar hover reveal: `{enabled?, pointer?}` or `{realCursor}` → the hover snapshot. */
  async hover(payload) {
    return this.t('test_hover_input', payload);
  }

  /**
   * Mouse messages posted to our own window (client DIP), no OS cursor. Steps `move` / `down` /
   * `up` (16 ms apart) or `dblclick` (both clicks in one burst: a real cursor resting over the
   * window would otherwise split paced clicks).
   */
  async mouse(steps) {
    return this.t('test_post_mouse', { steps });
  }

}

// ------------------------------------------------------------------------ the one CDP exception
//
// `CdpInstance` is the pre-MCP transport, kept for **exactly one** caller: `agent-e2e.mjs`.
//
// That suite's subject *is* the MCP channel. It asserts that a client is welcomed only after the
// user clicks Allow, that Stop leaves `session.connections` empty, that the bridge never reconnects
// after `bye{user_stopped}`, and that turning access off removes the endpoint file. A harness
// session on that same pipe changes the very numbers under test: it would have to be exempted from
// the connection list, from `disconnect_all`, from the paused refusal and from the chip — four
// places in product code where a real regression could then hide. So the *agent* side of the suite
// runs entirely through MCP (that is what it tests), and only its **driver** — the part that acts as
// the user (trusted clicks on the prompt buttons) and reads state — stays on the DevTools port.
//
// Everything here is documented in C:/ast/tmp/s6/cdp-residue.md. Do not use it anywhere else: the
// other five suites have no such conflict and drive the browser through MCP.
export class CdpInstance extends Instance {
  constructor(opts) {
    super(opts);
    if (!this.port) throw new Error('CdpInstance needs a DevTools port');
    this.conns = new Map();
  }

  closeSockets() {
    for (const c of this.conns ? this.conns.values() : []) c.close();
    this.conns?.clear();
    super.closeSockets();
  }

  async mcp() {
    throw new Error('CdpInstance has no MCP session of its own by design (see cdp-residue.md); use probeVia()');
  }

  /**
   * Lends this instance the suite's *own* MCP session for the native probes (`win`, `capture`,
   * `pixels`). `call(name, args)` answers a test tool's structured result and throws on an error.
   *
   * Nothing about a **running** browser should be PowerShell — that is requirement (1) — and the
   * probes are the one part of this class that does not have to be CDP (only acting as the user
   * does). agent-e2e lends its subject session the moment that session is allowed to call a tool;
   * before that (the consent phase, which is *about* not being allowed yet) the probes fall back to
   * the scripts, which is the last live-browser PowerShell in the tree and is documented as such.
   */
  probeVia(call) {
    this.probeCall = call;
    return this;
  }

  /** A test tool over the session `probeVia` lent us. */
  async t(name, args = {}) {
    if (!this.probeCall) throw new Error(`CdpInstance has no MCP session for ${name} (see probeVia / cdp-residue.md)`);
    return this.probeCall(name, args);
  }

  // -------------------------------------------------------------------------------- native

  async win(...args) {
    if (this.probeCall) return super.win(...args);
    return JSON.parse(ps(path.join(here, 'win-probe.ps1'), ['-ProcessId', String(this.pid), ...args]));
  }

  async capture(name) {
    if (this.probeCall) return super.capture(name);
    return ps(path.join(repo, 'tools/capture-window.ps1'), ['-ProcessId', String(this.pid), '-Out', `${this.data}-${name}.png`]);
  }

  async pixels(name, points) {
    if (this.probeCall) return super.pixels(name, points);
    const scale = (await this.win('info')).dpi / 96;
    const list = points.map(([x, y]) => `${Math.round(x * scale)},${Math.round(y * scale)}`).join(';');
    return JSON.parse(ps(path.join(here, 'png-pixel.ps1'), ['-Path', `${this.data}-${name}.png`, '-Points', list]));
  }

  // -------------------------------------------------------------------------------- targets, CDP

  async targets() {
    return (await fetch(`http://127.0.0.1:${this.port}/json/list`)).json();
  }

  async target(match) {
    const t = (await this.targets()).find((t) => t.type === 'page' && (typeof match === 'function' ? match(t) : t.url.includes(match)));
    if (!t) throw new Error(`no target matching ${match}`);
    return t;
  }

  async connect(t) {
    const target = typeof t === 'object' && t.webSocketDebuggerUrl ? t : await this.target(t);
    if (this.conns.has(target.id)) return this.conns.get(target.id);
    const ws = new WebSocket(target.webSocketDebuggerUrl);
    await new Promise((ok, err) => {
      ws.onopen = ok;
      ws.onerror = err;
    });
    let seq = 0;
    const pending = new Map();
    ws.onmessage = (m) => {
      const msg = JSON.parse(m.data);
      if (msg.id && pending.has(msg.id)) {
        const { ok, err } = pending.get(msg.id);
        pending.delete(msg.id);
        msg.error ? err(new Error(JSON.stringify(msg.error))) : ok(msg.result);
      }
    };
    ws.onclose = () => {
      this.conns.delete(target.id);
      for (const { err } of pending.values()) err(new Error('socket closed'));
    };
    const conn = {
      send: (method, params = {}) =>
        new Promise((ok, err) => {
          const id = ++seq;
          pending.set(id, { ok, err });
          ws.send(JSON.stringify({ id, method, params }));
        }),
      close: () => ws.close(),
    };
    this.conns.set(target.id, conn);
    return conn;
  }

  // -------------------------------------------------------------------------------- JS and IPC

  async eval(match, expr, { gesture = false } = {}) {
    const t = typeof match === 'object' ? match : await this.target(match);
    const c = await this.connect(t);
    const r = await c.send('Runtime.evaluate', { expression: expr, awaitPromise: true, returnByValue: true, userGesture: gesture });
    if (r.exceptionDetails) throw new Error(`eval failed in ${t.url}: ${JSON.stringify(r.exceptionDetails)}`);
    return r.result.value;
  }

  invoke(match, cmd, payload = null) {
    return this.eval(
      match,
      `window.sta.invoke(${JSON.stringify(cmd)}, ${JSON.stringify(payload)})` +
        `.then(function (r) { return { ok: r }; }, function (e) { return { err: e.code, msg: e.message }; })`,
    );
  }

  async info() {
    const r = await this.invoke('sta://topbar/', 'debug.info');
    if (r.err !== undefined) throw new Error('debug.info failed: ' + r.msg);
    return r.ok;
  }

  async state() {
    const r = await this.invoke('sta://topbar/', 'state.get');
    if (r.err !== undefined) throw new Error('state.get failed: ' + r.msg);
    return r.ok;
  }

  dispatch(command) {
    return this.invoke('sta://topbar/', 'debug.dispatch', command);
  }

  execute(effects) {
    return this.invoke('sta://topbar/', 'debug.execute', effects);
  }

  focus(target) {
    return this.invoke('sta://topbar/', 'debug.focus', target);
  }

  async counts() {
    return (await this.info()).controller.commandCounts || {};
  }
}

/** Runs `fn` again (up to `n` times) when a real key sequence was interrupted by another window. */
export async function retryInterrupted(fn, n = 3) {
  for (let attempt = 1; ; attempt++) {
    try {
      return await fn();
    } catch (e) {
      if (!e.interrupted || attempt >= n) throw e;
      console.log(`  (interrupted by another window; redoing the step, attempt ${attempt + 1})`);
    }
  }
}

// ------------------------------------------------------------------------- console-window hygiene

/**
 * Baseline for the console-window assertion: the console windows already on the developer's desktop
 * (the terminal the suite was started from is one), remembered by handle so that restoring one later
 * cannot fail a run.
 *
 * Deliberately **no** `reset`: the watcher starts with the browser, and a console that flashed while
 * the browser came up is exactly what must not be forgotten — `reset: true` used to whitelist it.
 */
export async function consoleBaseline(inst) {
  const w = await inst.t('test_console_windows', {});
  return { known: (w.current || []).map((c) => c.hwnd), at: w.at };
}

/**
 * Two checks: no console window was *shown* while the suite ran (a flash included) and none is on
 * the desktop that was not there at the baseline. `check` is the suite's own reporter.
 *
 * The first one asserts on `seen`, not on `ours`: under the Windows 11 default terminal a console
 * window belongs to `WindowsTerminal.exe`, which descends from `svchost.exe` and from no tree of
 * ours, so ancestry alone can miss the very case the rule exists for. Each offender carries `ours`,
 * `chain` and `title` so a failure says who opened it. A console window the *user* opens during a
 * run fails the check too — that is the price of being able to see the real thing, and foreground
 * suites own the machine anyway (they hold the desktop lock).
 */
export async function checkNoConsoleWindows(inst, baseline, check, section = 'hygiene') {
  const w = await inst.t('test_console_windows', { roots: [process.pid, inst.pid] });
  const known = new Set(baseline?.known || []);
  const shown = (w.seen || []).filter((c) => c.userVisible && !known.has(c.hwnd));
  const added = (w.current || []).filter((c) => c.userVisible && !known.has(c.hwnd));
  check(section, 'no console window was shown while the suite ran', w.watching === true && shown.length === 0, { watching: w.watching, shown });
  check(section, 'no new console window appeared on the desktop', added.length === 0, added);
  return w;
}

export const overlay = (i, name) => (i.overlays.hosts || i.overlays).find((o) => o.overlay === name);
export const tabInfo = (i, tab) => i.tabs.tabs.find((t) => t.tab === tab);
