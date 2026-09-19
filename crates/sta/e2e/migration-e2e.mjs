#!/usr/bin/env node
// End-to-end checks of the migration from Astatine (the product's name before the rename) to sta:
// the default data folder, the profile subfolder, persisted internal URLs and typed or command-line
// legacy URLs
// (Windows, Node 22+, debug build).
//
// rename:keep-file — every "astatine" / "Astatine" here is a legacy name on purpose.
//
//   cargo build -p sta -p sta-mcp --features test-hooks
//   node crates/sta/e2e/migration-e2e.mjs [--keep-open]
//
// Env: E2E_DATA_DIR (default C:/ast/tmp/migration-e2e), CDP_PORT (default 9339; launches that must not
// stay (refused, forwarded) use CDP_PORT + 1), E2E_EXE (default target/debug/sta.exe), E2E_OLD_EXE (a build from before the
// rename, default target/debug/astatine.exe; without it the legacy profile is written by hand and
// the "running Astatine" checks are skipped).
//
// sta resolves its default data folder from %LOCALAPPDATA%; every launch here gets LOCALAPPDATA =
// <E2E_DATA_DIR>/LocalAppData (or LocalAppData2) and no --sta-data-dir except the default folder
// of that test LocalAppData, so the user's real folders are never used (the suite only checks that
// they didn't appear or disappear). Needs the OS foreground for
// the error box: run it through the desktop lock when other suites run.
//
// Sections:
//   (p) a legacy profile in <LocalAppData>/Astatine Dev: made by the old build (pinned
//       astatine://settings/, favorite astatine://history/, Today astatine://boosts/ and a web
//       tab), whose singleton lockfile is held while it runs
//   (r) sta started while that Astatine runs: error box "sta", exit code 1, nothing moved or
//       created, the old browser got no URL
//   (m) sta started after Astatine quit: the folder is moved to "sta Dev" (profile subfolder
//       astatine/ → sta/), pinned / favorite / Today pages come back as sta://, the pages load
//       with IPC, a typed astatine:// URL opens its sta:// page, astatine:// on a later launch's
//       command line is forwarded as its sta:// page (not to the OS), the saved state has no
//       legacy URL
//   (s) second start: nothing to migrate, no legacy folder recreated; astatine:// on its command
//       line opens the sta:// page
//   (n) a new "Astatine Dev" next to the existing "sta Dev": left alone, never merged
//   (i) LocalAppData2, whose "Astatine Dev" can't be moved (a file inside is open): sta uses it in
//       place and holds sta-in-place.lock; a second launch meanwhile is forwarded to it (exit 0, no
//       error box, URL received); after both quit, --sta-data-dir=<LocalAppData2>/sta Dev (the
//       default folder passed explicitly, as a launcher may) moves it like a launch without it

import { spawn } from 'node:child_process';
import { closeSync, existsSync, mkdirSync, openSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import path from 'node:path';
import { CdpInstance, OCCLUSION_FLAG, alive, check, here, killTree, ps, repo, sleep, summary, waitFor } from './lib.mjs';

const ROOT = path.resolve(process.env.E2E_DATA_DIR || 'C:/ast/tmp/migration-e2e');
const LOCAL = path.join(ROOT, 'LocalAppData');
const LEGACY = path.join(LOCAL, 'Astatine Dev');
const NEW = path.join(LOCAL, 'sta Dev');
const LOGS = path.join(ROOT, 'logs');
const PORT = Number(process.env.CDP_PORT || '9339');
const EXE = process.env.E2E_EXE ? path.resolve(process.env.E2E_EXE) : path.join(repo, 'target/debug/sta.exe');
const OLD_EXE = process.env.E2E_OLD_EXE ? path.resolve(process.env.E2E_OLD_EXE) : path.join(repo, 'target/debug/astatine.exe');
const KEEP_OPEN = process.argv.includes('--keep-open');
const WEB = 'data:text/html,<title>E2E-MIGRATION-WEB</title>web';

const REAL_LOCAL = process.env.LOCALAPPDATA;
const REAL_FOLDERS = REAL_LOCAL ? ['Astatine', 'Astatine Dev', 'sta', 'sta Dev'].map((n) => path.join(REAL_LOCAL, n)) : [];
if (!REAL_LOCAL || path.resolve(REAL_LOCAL).toLowerCase() === LOCAL.toLowerCase() || LOCAL.toLowerCase().startsWith(path.resolve(REAL_LOCAL).toLowerCase())) {
  throw new Error(`refusing to run: the test LocalAppData (${LOCAL}) must be outside the real one (${REAL_LOCAL})`);
}
const realBefore = REAL_FOLDERS.map((p) => existsSync(p));

/** The environment of a launch: LOCALAPPDATA → a test folder, no data dir overrides. */
function env(extra, local = LOCAL) {
  const drop = new Set(['LOCALAPPDATA', 'STA_DATA_DIR', 'ASTATINE_DATA_DIR', ...Object.keys(extra).map((k) => k.toUpperCase())]);
  const out = Object.fromEntries(Object.entries(process.env).filter(([k]) => !drop.has(k.toUpperCase())));
  return { ...out, LOCALAPPDATA: local, ...extra };
}

/** A sta launch's environment: its CDP port, external protocols logged instead of launched. */
const staEnv = (port, local = LOCAL) => env({ STA_REMOTE_DEBUGGING_PORT: String(port), STA_DEBUG_HOVER_REVEAL: '0', STA_TEST_EXTERNAL_PROTOCOL: '1' }, local);

/** Waits for `inst` to exit; closes (and reports) error boxes it shows meanwhile. */
async function exitOf(inst, timeoutMs = 20000) {
  const code = await Promise.race([inst.exitCode, sleep(timeoutMs).then(() => 'timeout')]);
  if (code !== 'timeout') return { code, dialogs: [] };
  const dialogs = JSON.parse(ps(path.join(here, 'win-probe.ps1'), ['-ProcessId', String(inst.pid), 'dialogs']) || '[]');
  ps(path.join(here, 'win-probe.ps1'), ['-ProcessId', String(inst.pid), 'closedialogs']);
  const after = await Promise.race([inst.exitCode, sleep(5000).then(() => 'timeout')]);
  if (after === 'timeout') killTree(inst.pid);
  return { code: `timeout (then ${after})`, dialogs };
}

/**
 * A launch of the default folder under LOCALAPPDATA (no data dir switch unless `args` has one).
 *
 * The one place besides `agent-e2e` where the transport is **not** MCP (`CdpInstance`, lib.mjs, and
 * C:/ast/tmp/s6/cdp-residue.md): arming the test surface requires an explicit `--sta-data-dir` (or
 * `STA_DATA_DIR`), and the whole point of this suite is the launch that passes **neither** and has to
 * resolve, move and lock its own default folder. Half its checks are also "sta did not start" —
 * an error box, an exit code, a folder that did not move — where there is nothing to talk to.
 */
class DefaultDirInstance extends CdpInstance {
  constructor({ exe, port, env: e, args = [], name, legacy = false }) {
    super({ data: path.join(LOGS, name), port, fresh: false, args, env: e });
    this.exe = exe;
    this.legacy = legacy;
  }

  start(tag = 'run') {
    mkdirSync(LOGS, { recursive: true });
    this.stderrPath = `${this.data}-${tag}-stderr.txt`;
    // console-ok: sta.exe is the GUI child under test; windowsHide (libuv HIDE_GUI) would start it invisible
    this.child = spawn(this.exe, [OCCLUSION_FLAG, ...this.args], {
      env: this.env,
      stdio: ['ignore', openSync(`${this.data}-${tag}-stdout.txt`, 'w'), openSync(this.stderrPath, 'w')],
    });
    this.pid = this.child.pid;
    this.exitCode = new Promise((resolve) => this.child.on('exit', (code) => resolve(code)));
    console.log(`launched ${path.basename(this.exe)} pid ${this.pid} (port ${this.port}); logs ${this.stderrPath}`);
    sampleConsoles(`after start ${path.basename(this.exe)} ${tag}`);
    return this;
  }

  get host() {
    return this.legacy ? 'astatine://topbar/' : 'sta://topbar/';
  }

  call(cmd, payload = null) {
    if (this.legacy) {
      // A debug build reads ui/ from the checkout, which is sta's now (mock mode under
      // astatine://), so talk to the old shell's message router directly.
      const request = JSON.stringify(JSON.stringify({ cmd, payload }));
      return this.eval(
        this.host,
        `new Promise(function (ok) { window.__astatineQuery({ request: ${request}, persistent: false,` +
          ` onSuccess: function (r) { ok({ ok: r ? JSON.parse(r) : null }); }, onFailure: function (c, m) { ok({ err: c, msg: m }); } }); })`,
      );
    }
    const api = 'window.sta';
    return this.eval(
      this.host,
      `${api}.invoke(${JSON.stringify(cmd)}, ${JSON.stringify(payload)})` +
        `.then(function (r) { return { ok: r }; }, function (e) { return { err: e.code, msg: e.message }; })`,
    );
  }

  async ui() {
    const r = await this.call('state.get');
    if (r.err !== undefined) throw new Error(`state.get: ${r.msg}`);
    return r.ok;
  }

  send(command) {
    return this.call('debug.dispatch', command);
  }

  async ready() {
    return waitFor(async () => (await this.targets()).some((t) => t.url.startsWith(this.host)) && (await this.ui()), 30000, 250);
  }

  async quit() {
    await Promise.race([this.send({ type: 'quit' }).catch(() => null), sleep(2000)]);
    this.closeSockets();
    const code = await Promise.race([this.exitCode, sleep(20000).then(() => 'timeout')]);
    if (code === 'timeout') killTree(this.pid);
    sampleConsoles(`after quit ${path.basename(this.exe)}`);
    return code;
  }
}

const tabsOf = (s) => [...s.favorites, ...s.spaces.flatMap((sp) => [...sp.pinned, ...sp.today])].flatMap((n) => (n.kind === 'folder' ? n.children ?? [] : n.kind === 'split' ? n.panes ?? [] : [n]));
const urls = (s) => tabsOf(s).map((t) => t.url);

// ------------------------------------------------------------------- console-window hygiene
//
// The other suites hook the desktop from inside an armed browser (`test_console_windows`, lib.mjs
// `checkNoConsoleWindows`), which catches even a window that only flashed. This suite's whole
// subject is a launch that passes no `--sta-data-dir`, and lock 3 refuses to arm the test surface
// without one — so there is nothing here to host that watcher. What it can do without any browser
// cooperation is *scan* the desktop, which is what `win-probe.ps1 consoles` does: every
// console-class top-level window, whoever owns it. It is sampled around every launch (the moment a
// console would appear) and once at the end, so a window that is up at a sample point, or still up
// afterwards, fails the run; one that flashes entirely between two samples can still slip through.
// docs/TESTING.md §5 and README name this as the one suite with the weaker half.
const consoleSamples = [];
let consoleBaseline = null;
function sampleConsoles(where) {
  let list;
  try {
    list = JSON.parse(ps(path.join(here, 'win-probe.ps1'), ['-ProcessId', '0', 'consoles']) || '[]');
  } catch (e) {
    list = [{ hwnd: -1, cls: 'probe-failed', title: String(e && e.message), visible: true }];
  }
  if (consoleBaseline === null) consoleBaseline = new Set(list.map((c) => c.hwnd));
  else for (const c of list) if (c.visible && !consoleBaseline.has(c.hwnd)) consoleSamples.push({ ...c, where });
  return list;
}

let failed = 0;
const running = [];
try {
  sampleConsoles('baseline');
  rmSync(ROOT, { recursive: true, force: true });
  mkdirSync(LOCAL, { recursive: true });
  mkdirSync(LOGS, { recursive: true });

  // ------------------------------------------------------------------------------ (p) legacy
  const haveOld = existsSync(OLD_EXE);
  let old = null;
  if (haveOld) {
    old = new DefaultDirInstance({ exe: OLD_EXE, port: PORT, name: 'astatine', legacy: true, env: env({ ASTATINE_REMOTE_DEBUGGING_PORT: String(PORT), ASTATINE_DEBUG_HOVER_REVEAL: '0' }) }).start('old');
    running.push(old);
    check('p', 'the old build starts with its default folder "Astatine Dev"', await old.ready(), { exe: OLD_EXE });
    const open = async (url) => {
      await old.send({ type: 'openUrl', url, target: 'newTab' });
      return waitFor(async () => {
        const s = await old.ui();
        const t = tabsOf(s).find((x) => x.id === s.focusedTab);
        return t && t.url === url && t.id;
      }, 8000);
    };
    const settings = await open('astatine://settings/');
    await old.send({ type: 'togglePin', id: settings });
    const history = await open('astatine://history/');
    await old.send({ type: 'addFavorite', id: history });
    await open('astatine://boosts/');
    await open(WEB);
    const s = await old.ui();
    const pinned = s.spaces.flatMap((sp) => sp.pinned).map((t) => t.url);
    check('p', 'legacy profile: pinned astatine://settings/, favorite astatine://history/, Today astatine://boosts/ + a web tab',
      pinned.includes('astatine://settings/') && s.favorites.some((t) => t.url === 'astatine://history/') && urls(s).includes('astatine://boosts/') && urls(s).includes(WEB), urls(s));
    check('p', 'the running old browser holds <Astatine Dev>/User Data/lockfile', existsSync(path.join(LEGACY, 'User Data', 'lockfile')));

    // ---------------------------------------------------------------------------- (r) refused
    const before = urls(await old.ui()).length;
    const refused = new DefaultDirInstance({ exe: EXE, port: PORT + 1, name: 'sta-refused', env: env({ STA_REMOTE_DEBUGGING_PORT: String(PORT + 1) }), args: ['data:text/html,<title>E2E-NOT-FORWARDED</title>x'] }).start('refused');
    running.push(refused);
    const dialogs = await waitFor(() => {
      const d = JSON.parse(ps(path.join(here, 'win-probe.ps1'), ['-ProcessId', String(refused.pid), 'dialogs']));
      return d.length && d;
    }, 20000, 300);
    check('r', 'sta shows an error box while Astatine runs', dialogs && dialogs[0].title === 'sta', dialogs);
    check('r', 'stderr names the busy legacy folder', refused.log().includes('is in use by a running Astatine'), refused.log().slice(0, 300));
    check('r', 'nothing moved or created while the box is up', existsSync(LEGACY) && !existsSync(NEW));
    ps(path.join(here, 'win-probe.ps1'), ['-ProcessId', String(refused.pid), 'closedialogs']);
    const code = await Promise.race([refused.exitCode, sleep(10000).then(() => 'timeout')]);
    check('r', 'after the box sta exits with code 1', code === 1, { code });
    await sleep(1000);
    check('r', 'the running Astatine got no URL from sta', urls(await old.ui()).length === before && !urls(await old.ui()).some((u) => u.includes('E2E-NOT-FORWARDED')));
    check('r', 'still nothing moved or created after the exit', existsSync(path.join(LEGACY, 'User Data')) && !existsSync(NEW), { legacy: existsSync(LEGACY), new: existsSync(NEW) });

    const oldCode = await old.quit();
    check('p', 'the old build quits and saves', oldCode !== 'timeout' && existsSync(path.join(LEGACY, 'astatine', 'state.json')), { oldCode });
    check('p', 'its lockfile is gone', await waitFor(() => !existsSync(path.join(LEGACY, 'User Data', 'lockfile')), 5000));
    const saved = readFileSync(path.join(LEGACY, 'astatine', 'state.json'), 'utf8');
    check('p', 'legacy state.json has astatine:// URLs', saved.includes('"astatine://settings/"') && saved.includes('"astatine://history/"'));
  } else {
    console.log(`SKIP (p)/(r) with the old build: ${OLD_EXE} not found; writing the legacy profile by hand`);
    mkdirSync(path.join(LEGACY, 'astatine'), { recursive: true });
    mkdirSync(path.join(LEGACY, 'User Data'), { recursive: true });
    const state = {
      version: 2,
      nextId: 10,
      window: { activeSpace: 1 },
      spaces: [{ id: 1, name: 'Home', icon: '🏠', pinned: [2], today: [3, 4], activeItem: 4 }],
      favorites: [5],
      items: {
        2: { kind: 'tab', id: 2, url: 'astatine://settings/', pinnedUrl: 'astatine://settings/', title: 'Settings' },
        3: { kind: 'tab', id: 3, url: 'astatine://boosts/', title: 'Boosts' },
        4: { kind: 'tab', id: 4, url: WEB, title: 'E2E-MIGRATION-WEB' },
        5: { kind: 'tab', id: 5, url: 'astatine://history/', pinnedUrl: 'astatine://history/', title: 'History' },
      },
    };
    writeFileSync(path.join(LEGACY, 'astatine', 'state.json'), JSON.stringify(state));
    check('p', 'hand-written legacy profile', existsSync(path.join(LEGACY, 'astatine', 'state.json')));
  }
  writeFileSync(path.join(LEGACY, 'User Data', 'e2e-marker.txt'), 'moved with the folder');

  // ----------------------------------------------------------------------------- (m) migrated
  const sta = new DefaultDirInstance({ exe: EXE, port: PORT, name: 'sta-migrated', env: staEnv(PORT) }).start('migrated');
  running.push(sta);
  const s = await sta.ready();
  check('m', 'sta starts without a data dir switch', !!s);
  check('m', '"Astatine Dev" was moved to "sta Dev" with its contents', !existsSync(LEGACY) && existsSync(NEW) && readFileSync(path.join(NEW, 'User Data', 'e2e-marker.txt'), 'utf8') === 'moved with the folder');
  check('m', 'profile subfolder astatine/ is now sta/', existsSync(path.join(NEW, 'sta', 'state.json')) && !existsSync(path.join(NEW, 'astatine')));
  const log = readFileSync(path.join(NEW, 'Logs', 'sta.log'), 'utf8');
  check('m', 'log: both moves and the URL upgrade', log.includes(`data from before the rename moved: ${LEGACY} -> ${NEW}`) && log.includes(`${path.join(NEW, 'astatine')} -> ${path.join(NEW, 'sta')}`) && log.includes('internal URL(s) from before the rename upgraded to sta://'), log.split('\n').filter((l) => l.includes('rename')));
  if (s) {
    const pinned = s.spaces.flatMap((sp) => sp.pinned);
    const settingsTab = pinned.find((t) => t.url === 'sta://settings/');
    check('m', 'pinned page is sta://settings/ (pinned URL too)', settingsTab && settingsTab.pinnedUrl === 'sta://settings/', pinned.map((t) => [t.url, t.pinnedUrl]));
    check('m', 'favorite is sta://history/', s.favorites.some((t) => t.url === 'sta://history/'), s.favorites.map((t) => t.url));
    check('m', 'Today has sta://boosts/ and the web tab', urls(s).includes('sta://boosts/') && urls(s).includes(WEB), urls(s));
    check('m', 'no legacy URL anywhere in the UI state', !JSON.stringify(s).includes('astatine:'));
    if (settingsTab) {
      await sta.send({ type: 'activateItem', id: settingsTab.id });
      const page = await waitFor(async () => (await sta.targets()).find((t) => t.type === 'page' && t.url.startsWith('sta://settings/')), 10000, 200);
      const ipc = page && (await waitFor(async () => (await sta.eval(page, 'typeof window.__staQuery')) === 'function', 8000));
      check('m', 'the pinned page loads as a trusted sta:// page (IPC available)', !!ipc, page && page.url);
    }
    await sta.send({ type: 'openInput', text: 'astatine://archive/', target: 'newTab' });
    const archive = await waitFor(async () => (await sta.targets()).find((t) => t.type === 'page' && t.url.startsWith('sta://archive/')), 10000, 200);
    check('m', 'typed astatine://archive/ opens sta://archive/', !!archive);

    // A legacy URL on a later launch's command line is handed to this instance as its sta:// page.
    const relaunch = new DefaultDirInstance({ exe: EXE, port: PORT + 1, name: 'sta-relaunch', env: staEnv(PORT + 1), args: ['astatine://history/?e2e=relaunch'] }).start('relaunch');
    running.push(relaunch);
    const r = await exitOf(relaunch);
    check('m', 'a launch with astatine://history/?e2e=relaunch exits 0 (forwarded)', r.code === 0, r);
    // The favorite sta://history/ tab is the existing page for that host: it's activated and loads.
    const forwarded = await waitFor(async () => {
      const ui = await sta.ui();
      const focused = tabsOf(ui).find((t) => t.id === ui.focusedTab);
      return focused && focused.url.startsWith('sta://history/') && (await sta.targets()).some((t) => t.type === 'page' && t.url.startsWith('sta://history/'));
    }, 10000, 200);
    check('m', 'the running sta opens it as sta://history/…; nothing went to the OS protocol handler',
      !!forwarded && sta.log().includes('OpenUrl { url: "sta://history/?e2e=relaunch"') && !sta.log().includes('external protocol (test)'),
      sta.log().split('\n').filter((l) => l.includes('external protocol') || l.includes('relaunch')));
  }
  const code = await sta.quit();
  check('m', 'sta quits', code !== 'timeout', { code });
  const saved = readFileSync(path.join(NEW, 'sta', 'state.json'), 'utf8');
  check('m', 'saved state.json: sta:// URLs, no legacy URL', saved.includes('"sta://settings/"') && !saved.includes('astatine:'));

  // ------------------------------------------------------------------------------ (s) second
  const again = new DefaultDirInstance({ exe: EXE, port: PORT, name: 'sta-second', env: staEnv(PORT), args: ['astatine://boosts/?e2e=first'] }).start('second');
  running.push(again);
  const s2 = await again.ready();
  check('s', 'second start restores the sta:// pages', s2 && urls(s2).some((u) => u.startsWith('sta://settings/')) && urls(s2).some((u) => u.startsWith('sta://history/')), s2 && urls(s2));
  const log2 = again.log();
  check('s', 'nothing to migrate: no move, no URL upgrade, no legacy folder', !log2.includes('from before the rename') && !existsSync(LEGACY), log2.split('\n').filter((l) => l.includes('rename')));
  const firstArg = await waitFor(async () => (await again.targets()).find((t) => t.type === 'page' && t.url.startsWith('sta://boosts/?e2e=first')), 10000, 200);
  check('s', 'astatine://boosts/?e2e=first on the command line opens sta://boosts/?e2e=first; nothing went to the OS protocol handler', !!firstArg && !again.log().includes('external protocol (test)'),
    again.log().split('\n').filter((l) => l.includes('external protocol')));
  await again.quit();

  // ----------------------------------------------------------------------------- (n) no merge
  mkdirSync(path.join(LEGACY, 'astatine'), { recursive: true });
  writeFileSync(path.join(LEGACY, 'astatine', 'state.json'), '{"version":2}');
  const both = new DefaultDirInstance({ exe: EXE, port: PORT, name: 'sta-both', env: staEnv(PORT) }).start('both');
  running.push(both);
  const s3 = await both.ready();
  check('n', 'with both folders sta uses "sta Dev"', s3 && urls(s3).includes('sta://settings/'));
  check('n', 'the new "Astatine Dev" is left alone (not merged, not moved)', readFileSync(path.join(LEGACY, 'astatine', 'state.json'), 'utf8') === '{"version":2}' && !existsSync(path.join(NEW, 'astatine')));
  check('n', 'log: left alone', both.log().includes(`${LEGACY} from before the rename left alone`));
  await both.quit();

  // ------------------------------------------------------------------ (i) in place, shared
  // A second LocalAppData whose "Astatine Dev" can't be moved: this process keeps a file inside open.
  const LOCAL2 = path.join(ROOT, 'LocalAppData2');
  const LEGACY2 = path.join(LOCAL2, 'Astatine Dev');
  const NEW2 = path.join(LOCAL2, 'sta Dev');
  const LOCK2 = path.join(LEGACY2, 'sta-in-place.lock');
  mkdirSync(path.join(LEGACY2, 'astatine'), { recursive: true });
  const pinnedState = {
    version: 2,
    nextId: 10,
    window: { activeSpace: 1 },
    spaces: [{ id: 1, name: 'Home', icon: '🏠', pinned: [2], today: [], activeItem: 2 }],
    favorites: [],
    items: { 2: { kind: 'tab', id: 2, url: 'astatine://settings/', pinnedUrl: 'astatine://settings/', title: 'Settings' } },
  };
  writeFileSync(path.join(LEGACY2, 'astatine', 'state.json'), JSON.stringify(pinnedState));
  let held = openSync(path.join(LEGACY2, 'held-by-e2e.txt'), 'w');
  const inPlace = new DefaultDirInstance({ exe: EXE, port: PORT, name: 'sta-in-place', env: staEnv(PORT, LOCAL2) }).start('in-place');
  running.push(inPlace);
  const s4 = await inPlace.ready();
  check('i', 'sta starts with the legacy folder in place when its move fails', !!s4 && existsSync(LEGACY2) && !existsSync(NEW2));
  check('i', 'log: could not move, using it in place', inPlace.log().includes(`could not move ${LEGACY2} to its new name`), inPlace.log().split('\n').filter((l) => l.includes('move')));
  check('i', 'its pinned page is sta://settings/ (legacy layout kept: astatine/ subfolder)', !!s4 && urls(s4).includes('sta://settings/') && !existsSync(path.join(LEGACY2, 'sta')), s4 && urls(s4));
  check('i', 'it holds <Astatine Dev>/sta-in-place.lock', existsSync(LOCK2));

  const FWD = 'data:text/html,<title>E2E-IN-PLACE-FORWARDED</title>x';
  const second = new DefaultDirInstance({ exe: EXE, port: PORT + 1, name: 'sta-in-place-second', env: staEnv(PORT + 1, LOCAL2), args: [FWD] }).start('in-place-second');
  running.push(second);
  const r2 = await exitOf(second);
  check('i', 'a second launch meanwhile exits 0 without an error box (forwarded)', r2.code === 0 && !r2.dialogs.length, r2);
  check('i', 'its log: the legacy folder is used in place by a running sta', second.log().includes(`${LEGACY2} is used in place by a running sta`), second.log().split('\n').filter((l) => l.includes('place')));
  const got = await waitFor(async () => urls(await inPlace.ui()).includes(FWD), 10000, 200);
  check('i', 'the running sta got its URL', !!got, urls(await inPlace.ui()));

  const inPlaceCode = await inPlace.quit();
  check('i', 'the in-place run quits; its lock is gone', inPlaceCode !== 'timeout' && (await waitFor(() => !existsSync(LOCK2), 5000)), { inPlaceCode });
  closeSync(held);
  held = null;

  // The default folder passed explicitly (as a launcher may do) migrates like the default.
  const explicit = new DefaultDirInstance({ exe: EXE, port: PORT, name: 'sta-explicit-default', env: staEnv(PORT, LOCAL2), args: [`--sta-data-dir=${NEW2}`] }).start('explicit-default');
  running.push(explicit);
  const s5 = await explicit.ready();
  check('i', '--sta-data-dir=<the default folder>: "Astatine Dev" is moved to "sta Dev" now', !!s5 && !existsSync(LEGACY2) && existsSync(path.join(NEW2, 'sta', 'state.json')) && !existsSync(path.join(NEW2, 'astatine')));
  check('i', 'with its pinned sta://settings/ and the forwarded URL; no in-place lock left', !!s5 && urls(s5).includes('sta://settings/') && urls(s5).includes(FWD) && !existsSync(path.join(NEW2, 'sta-in-place.lock')), s5 && urls(s5));
  if (!KEEP_OPEN) await explicit.quit();
} catch (e) {
  console.error(e);
  check('suite', `no exception (${e.message})`, false);
} finally {
  if (!KEEP_OPEN) for (const i of running) if (i.pid && alive(i.pid)) killTree(i.pid);
  const nowConsoles = sampleConsoles('end');
  const addedConsoles = consoleBaseline ? nowConsoles.filter((c) => c.visible && !consoleBaseline.has(c.hwnd)) : [];
  check('hygiene', `no console window was on the desktop at any sample point while the suite ran (desktop scan only: this suite cannot arm the in-browser watcher)`, consoleSamples.length === 0, consoleSamples.slice(0, 6));
  check('hygiene', 'no new console window appeared on the desktop', addedConsoles.length === 0, addedConsoles);
  const realAfter = REAL_FOLDERS.map((p) => existsSync(p));
  check('guard', 'the real %LOCALAPPDATA% folders were not created or removed', realAfter.every((v, i) => v === realBefore[i]), Object.fromEntries(REAL_FOLDERS.map((p, i) => [p, [realBefore[i], realAfter[i]]])));
  failed = summary();
}
process.exit(failed ? 1 : 0);
