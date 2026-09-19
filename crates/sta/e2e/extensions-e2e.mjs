#!/usr/bin/env node
// End-to-end checks of the Chrome-created browsers sta adopts or hides (crates/sta/src/foreign.rs,
// ARCHITECTURE §4.5) — the "an extension opened a Chrome window" fix (Windows, Node 22+, debug build).
//
//   cargo build -p sta -p sta-mcp --features test-hooks
//   node crates/sta/e2e/extensions-e2e.mjs [--keep-open] [--only=a,n,p,…]
//
// Env: E2E_DATA_DIR (default C:/ast/tmp/extensions-e2e), E2E_HTTP_PORT (default: an ephemeral port),
// STA_E2E_WEBSTORE=1 (adds the live Chrome Web Store install, which needs the network and a
// throwaway profile). Other sta instances may run at the same time: only the process tree started
// here is probed and killed.
//
// The browser is driven **through MCP** (lib.mjs, docs/TESTING.md), so it must be built with
// `--features test-hooks`; there is no DevTools port and no PowerShell. The service workers of the
// probe extensions — a browser-level DevTools connection, which the shipped agent tools refuse — are
// reached with `test_targets` + `test_attach` + `test_eval`, and the phase-1 `debug.foreign*`
// requests through `test_invoke` (the real trusted-frame path).
//
// The extensions are the in-repo probes (crates/sta/e2e/extensions/, never copies of real ones);
// every assertion is about their own ids. The probes act only when this script calls their service
// worker over the DevTools protocol (a browser-level connection, like an extension's own code).
//
// Sections:
//   (a) adoption: options page in a tab, embedded options rewritten to the extension's page,
//       windows.create (normal), a late tabs.create, Target.createTarget — each opens one sta tab,
//       leaves no visible Chrome window, keeps sta in the foreground and hits the server once
//   (n) post-install: a probe copied into the profile's Extensions directory plus a new-tab-page
//       window (what OnInstallSuccess opens) → "… added" toast within 2 s, window closed after ≥ 3 s
//   (r) an install that lands **before** the +12 s startup re-scan is still announced (its own
//       instance: the window is the first seconds of a run)
//   (p) policy: an undeclared extension page asks (toast with Open) while declared and
//       web-accessible pages open, chrome://settings is dropped, more than 3 *adoptions* in 10 s
//       are rate limited while asks and refusals leave that budget alone (and have a cap of their
//       own), an incognito window is refused
//   (w) windows that stay native: an extension popup window (`_crx_`) stays visible and is not
//       adopted; keys typed while a hidden window exists land in sta; a browser-framed native window
//       is titled "… - sta", never "… - Chromium"
//   (d) a download in progress postpones the close of a hidden window
//   (v) DevTools are refused on sta:// pages — the toast for F12/Ctrl+Shift+I, and no Inspect item
//       in the context menu at all — and still open (and close) on a web tab
//   (e) the Ctrl+E picker: a real Ctrl+E from the page, the rows and their groups, a query typed with
//       a Hangul IME on, Enter into an options tab (one per extension, even for an options page that
//       routes itself on load), the app menu route and the `>` commands
//   (c) the popup card (gates S3 and S4): it appears at the popup's own size under sta's header, the
//       popup page really runs (chrome.storage) and what it sees of sta's tabs is recorded, it cannot
//       leave its origin, Esc closes it, a popup that renders nothing says so while one that paints
//       late is shown (never a 25x25 sliver, never falsely declared broken), the card's client
//       refuses a gesture-less external protocol, a file chooser and a permission request, and a
//       permission prompt takes it away (with the prompt's own 400 ms input guard)
//   (g) Settings › Extensions and the hidden `chrome://extensions` backend (gate S7): the rows and
//       their icons, off/on again, the Turn on disclosure, the refusal for a local CRX, Remove
//   (l) the crash-loop guard: two abnormal exits within a minute → safe mode, tabs restored
//       unloaded, and the banner in Settings
//   (x) shutdown with a hidden and a native Chrome-created browser: clean exit
//   (s) screenshots (light and dark, 100 % and 150 %): the install toasts, the Ctrl+E picker, the
//       popup card (working and failed), Settings › Extensions and the app menu
//   (webstore) opt-in: a real Chrome Web Store install (STA_E2E_WEBSTORE=1), accepted through MCP
//       (`test_dialog`) — the dialog's ownership, placement, toast, adopted post-install page

import http from 'node:http';
import { cpSync, mkdirSync, readFileSync } from 'node:fs';
import path from 'node:path';
import { Instance, check, checkNoConsoleWindows, consoleBaseline, here, overlay, sleep, summary, waitFor } from './lib.mjs';

const DATA = process.env.E2E_DATA_DIR || 'C:/ast/tmp/extensions-e2e';
/** Ephemeral by default: the data dir alone makes a run unique. */
let HTTP = Number(process.env.E2E_HTTP_PORT || 0);
const KEEP_OPEN = process.argv.includes('--keep-open');
const ONLY = (process.argv.find((a) => a.startsWith('--only=')) || '').slice('--only='.length).split(',').filter(Boolean);
const runs = (section) => ONLY.length === 0 || ONLY.includes(section);

const EXT_DIR = path.join(here, 'extensions');
const IDS = JSON.parse(readFileSync(path.join(EXT_DIR, 'ids.json'), 'utf8'));
const WINDOWS_ID = IDS['probe-windows'];
const OPTIONS_ID = IDS['probe-options'];
const INSTALLED_ID = IDS['probe-installed'];

/** sta's own surfaces, addressed by URL for `eval` / `invoke` (lib.mjs `selector`). */
const COMMAND = 'sta://command/';
const SIDEBAR = 'sta://sidebar/';
const SETTINGS = 'sta://settings/';
const PERMISSION = 'sta://permission/';
const EXTENSION = 'sta://extension/';

// ------------------------------------------------------------------------------------ local site

let requests = [];
let slowHold = null;
const server = http.createServer((req, res) => {
  requests.push(req.url);
  if (req.url.startsWith('/slow.bin')) {
    res.writeHead(200, { 'content-type': 'application/octet-stream', 'content-disposition': 'attachment; filename="probe-slow.bin"', 'content-length': String(1024 * 400) });
    let sent = 0;
    slowHold = setInterval(() => {
      if (res.destroyed || sent >= 400) return;
      res.write(Buffer.alloc(1024, 1));
      sent++;
    }, 120);
    req.on('close', () => clearInterval(slowHold));
    return;
  }
  if (req.url.startsWith('/auth')) {
    // A sign-in page that redirects to the extension's redirect URL after a moment, so
    // `identity.launchWebAuthFlow` really shows its window first.
    res.writeHead(200, { 'content-type': 'text/html; charset=utf-8' });
    res.end(`<!doctype html><title>probe sign in</title><meta http-equiv="refresh" content="2;url=https://${WINDOWS_ID}.chromiumapp.org/cb?code=42"><h1>sign in</h1>`);
    return;
  }
  res.writeHead(200, { 'content-type': 'text/html; charset=utf-8' });
  res.end(`<!doctype html><title>probe ${req.url}</title><body style="background:#eef"><h1>${req.url}</h1>`);
});
const url = (p) => `http://127.0.0.1:${HTTP}${p}`; // HTTP is set once the fixture is listening

// ------------------------------------------------------------------------------------ helpers

/**
 * `self.probe.<call>` in an extension's service worker — a **browser-level** DevTools target, which
 * is why this needs the test surface: `test_targets` lists what `Target.getTargets` reports,
 * `test_eval` attaches a flattened session for a `targetId` (lib.mjs `selector`) and evaluates in it.
 * The shipped agent tools refuse anything that is not a tab, by design.
 */
async function probeCaller(inst, extensionId) {
  const target = await waitFor(
    async () => (await inst.targets()).find((t) => t.type === 'service_worker' && (t.url || '').startsWith(`chrome-extension://${extensionId}/`)),
    20000,
    250,
  );
  if (!target) throw new Error(`no service worker for ${extensionId}`);
  return (expression) => inst.eval(target, expression);
}

const foreignSnapshot = async (inst) => (await inst.invoke('sta://topbar/', 'debug.foreign')).ok;
const tabUrls = async (inst) => (await inst.info()).tabs.tabs.map((t) => t.url || '');
const foregroundOurs = async (inst) => (await inst.info()).focus.foreground;

/** A Chrome-created window that is on screen (hidden ones are cloaked). */
const visibleForeignWindows = (snapshot) =>
  (snapshot.browsers || []).filter((b) => b.kind !== 'KeepNative' && b.window && b.window.visible && !b.window.cloaked).map((b) => ({ id: b.id, kind: b.kind, title: b.window.title }));

/** Waits for a tab whose URL contains `needle`. */
const waitForTab = (inst, needle, ms = 12000) => waitFor(async () => (await tabUrls(inst)).find((u) => u.includes(needle)), ms, 200);

const waitForToast = (inst, needle, ms = 6000) =>
  waitFor(async () => {
    const t = (await inst.state()).toast;
    return t && t.message.includes(needle) ? t : null;
  }, ms, 150);

/**
 * Every toast phase 1 can show, light and dark, at one display scale (`tag`): the two install
 * toasts (the external one carries the longest message), the flood toast and the question — the
 * only one with an action button, so the only different layout.
 *
 * The flood toast is shown at most once per 10 s (`store/foreign.rs`), which is why the themes are
 * 11 s apart rather than back to back.
 */
async function toastShots(target, tag) {
  // A name at (well past) the budget core allows, so the shot shows the layout the longest possible
  // question produces: the name ellipsizes, the sentence still reads whole.
  const asking = { id: WINDOWS_ID, name: 'Privacy Guard Pro for Chrome — Ads, Trackers & Cookies', pages: ['options.html'], webAccessible: [], recentlyInstalled: false };
  const toasts = [
    ['install', { type: 'extensionInstalled', id: INSTALLED_ID, name: 'sta probe: installed', external: false }],
    ['external', { type: 'extensionInstalled', id: INSTALLED_ID, name: 'sta probe: installed', external: true }],
    ['blocked', { type: 'foreignBlocked', reason: 'rateLimited' }],
    ['ask', { type: 'foreignTabRequested', url: `chrome-extension://${WINDOWS_ID}/app/shot.html`, extension: asking }],
  ];
  for (const dark of [false, true]) {
    if (dark) await sleep(11000);
    const theme = dark ? 'dark' : 'light';
    await target.dispatch({ type: 'systemThemeChanged', dark });
    await sleep(400);
    for (const [name, command] of toasts) {
      await target.dispatch(command);
      await sleep(700);
      const out = await target.capture(`toast-${name}-${theme}-${tag}`);
      check('s', `${name} toast captured (${theme}, ${tag} %)`, !!out, out);
      await sleep(300);
    }
  }
  await target.dispatch({ type: 'systemThemeChanged', dark: false });
}

/**
 * Every phase-3 surface, light and dark, at one display scale (`tag`): the Ctrl+E picker, the popup
 * card (working and failed), Settings › Extensions and the app menu entry. The card needs a page to
 * anchor to, so a tab is opened first.
 */
async function extensionShots(target, tag) {
  const blank = { id: WINDOWS_ID, name: 'sta probe: windows', shortName: '', version: '1.0', description: '', state: 'enabled', install: 'unpacked', sourceLabel: 'Loaded from a folder', popup: 'popup-blank.html', options: 'options.html', sidePanel: null, commands: [] };
  await target.dispatch({ type: 'openUrl', url: url('/shot-host'), target: 'newTab' });
  await waitFor(async () => (await target.state()).current, 12000, 250);
  await sleep(600);
  for (const dark of [false, true]) {
    const theme = dark ? 'dark' : 'light';
    await target.dispatch({ type: 'systemThemeChanged', dark });
    await sleep(500);

    await target.dispatch({ type: 'openCommandBar', mode: 'extensions' });
    await sleep(900);
    let out = await target.capture(`ext-picker-${theme}-${tag}`);
    check('s', `the Ctrl+E picker captured (${theme}, ${tag} %)`, !!out, out);
    await target.dispatch({ type: 'closeCommandBar' });
    await sleep(300);

    await target.dispatch({ type: 'runExtension', id: WINDOWS_ID, action: 'popup' });
    await waitFor(async () => (await target.state()).extensions.popup, 10000, 200);
    await sleep(900);
    out = await target.capture(`ext-card-${theme}-${tag}`);
    check('s', `the popup card captured (${theme}, ${tag} %)`, !!out, out);
    await target.dispatch({ type: 'closeExtensionPopup' });
    await sleep(300);

    // The honest-failure card: a listing pointing at the probe page that renders nothing.
    await target.dispatch({ type: 'extensionsChanged', extensions: [blank] });
    await target.dispatch({ type: 'runExtension', id: WINDOWS_ID, action: 'popup' });
    await waitFor(async () => (await target.state()).extensions.popup?.failed, 9000, 250);
    await sleep(500);
    out = await target.capture(`ext-card-failed-${theme}-${tag}`);
    check('s', `the failed popup card captured (${theme}, ${tag} %)`, !!out, out);
    await target.dispatch({ type: 'closeExtensionPopup' });
    await target.execute([{ type: 'refreshExtensions' }]);
    await sleep(600);

    await target.dispatch({ type: 'openUrl', url: 'sta://settings/?section=extensions', target: 'newTab' });
    await waitFor(async () => (await target.state()).current?.url.startsWith('sta://settings'), 12000, 250);
    await sleep(1400);
    out = await target.capture(`ext-settings-${theme}-${tag}`);
    check('s', `Settings › Extensions captured (${theme}, ${tag} %)`, !!out, out);

    await target.dispatch({ type: 'openSidebarPanel', panel: { type: 'appMenu' } });
    await sleep(800);
    out = await target.capture(`ext-appmenu-${theme}-${tag}`);
    check('s', `the app menu captured (${theme}, ${tag} %)`, !!out, out);
    await target.dispatch({ type: 'closeSidebarPanel' });
    await sleep(300);
  }
  await target.dispatch({ type: 'systemThemeChanged', dark: false });
}

// ------------------------------------------------------------------------------------ run

/** Both unpacked probes, for every instance this suite starts (the 150 % one shoots the same cards). */
const PROBE_ARGS = `--load-extension=${path.join(EXT_DIR, 'probe-windows')},${path.join(EXT_DIR, 'probe-options')}`;

// `STA_TEST_EXTERNAL_PROTOCOL=1` (debug builds): an external-protocol launch is logged instead of
// handed to Windows, which is how section (c) can assert what the popup card may and may not launch.
const inst = new Instance({ data: DATA, args: [PROBE_ARGS], env: { STA_TEST_EXTERNAL_PROTOCOL: '1' } });
let consoles = null;

async function main() {
  await new Promise((r) => server.listen(HTTP, '127.0.0.1', r));
  HTTP = server.address().port;
  inst.start('extensions');
  const ready = await waitFor(() => inst.info(), 40000, 300);
  check('start', 'browser ready', !!ready);
  consoles = await consoleBaseline(inst);
  const windowsProbe = await probeCaller(inst, WINDOWS_ID);
  const optionsProbe = await probeCaller(inst, OPTIONS_ID);
  check('start', 'probe service workers answer', !!(await windowsProbe('self.probe.id')) && !!(await optionsProbe('self.probe.id')));

  // ---------------------------------------------------------------------------------- (a) adoption
  if (runs('a')) {
    const before = await foregroundOurs(inst);
    requests = [];
    // options_page (a tab): runtime.openOptionsPage opens a Chrome window with the extension page.
    const optionsResult = await optionsProbe('self.probe.openOptions()');
    const optionsTab = await waitForTab(inst, `chrome-extension://${OPTIONS_ID}/options.html`);
    check('a', 'options page opens as an sta tab', !!optionsTab, { optionsResult, optionsTab });
    let snap = await foreignSnapshot(inst);
    check('a', 'no visible Chrome window', visibleForeignWindows(snap).length === 0, visibleForeignWindows(snap));
    check('a', 'sta keeps the foreground', !before || (await foregroundOurs(inst)), { before });

    // Embedded options_ui → chrome://extensions/?options=<id>, rewritten to the extension's page.
    await windowsProbe('self.probe.openOptions()');
    const embedded = await waitForTab(inst, `chrome-extension://${WINDOWS_ID}/options.html`);
    check('a', 'embedded options are rewritten to the extension page', !!embedded, embedded);

    // windows.create({type:'normal'}) with a web page.
    await windowsProbe(`self.probe.createWindow(${JSON.stringify(url('/adopted'))})`);
    const adopted = await waitForTab(inst, '/adopted');
    check('a', 'windows.create opens an sta tab', !!adopted, adopted);
    check('a', 'the page was requested once', requests.filter((r) => r === '/adopted').length === 1, requests);

    // Target.createTarget (what the DevTools protocol does) goes through the same path. It is the
    // fourth window here, so the rate limit's 10 s window has to pass first (checked in (p)).
    await sleep(11000);
    const trigger = await inst.invoke('sta://topbar/', 'debug.foreign.trigger', { url: url('/triggered') });
    const triggered = await waitForTab(inst, '/triggered');
    check('a', 'Target.createTarget opens an sta tab', !!triggered, { trigger, triggered });

    snap = await foreignSnapshot(inst);
    check('a', 'every hidden window is closed again', await waitFor(async () => (await foreignSnapshot(inst)).browsers.filter((b) => b.kind !== 'KeepNative').length === 0, 15000, 500), snap.browsers);
    check('a', 'activations of hidden windows were blocked', snap.windows.activationsBlocked > 0, snap.windows.activationsBlocked);
  }

  // ---------------------------------------------------------------------------------- (n) post-install
  if (runs('n')) {
    // The startup re-scan (12 s) takes the extensions other programs registered into the known set;
    // the probe is copied in afterwards, so it is this session's install.
    await sleep(Math.max(0, 13000 - Date.now() + startedAt));
    const target = path.join(DATA, 'User Data', 'Default', 'Extensions', INSTALLED_ID, '1.0_0');
    mkdirSync(target, { recursive: true });
    cpSync(path.join(EXT_DIR, 'probe-installed'), target, { recursive: true });
    const t0 = Date.now();
    await inst.invoke('sta://topbar/', 'debug.foreign.trigger', { url: 'chrome://newtab/' });
    // UX10 asks for the toast within 2 s; the wait is longer so a slow-but-present toast fails this
    // check loudly instead of timing out silently.
    const toast = await waitForToast(inst, 'added', 6000);
    check('n', 'the install toast names the extension within 2 s', !!toast && Date.now() - t0 < 2000, { toast, ms: Date.now() - t0 });
    check('n', 'the toast is not the "added by another program" one', !!toast && !toast.message.includes('another program'), toast && toast.message);
    let snap = await foreignSnapshot(inst);
    const postInstall = snap.browsers.find((b) => b.kind === 'PostInstall');
    check('n', 'the new-tab-page window is hidden (never shown)', !postInstall || (!postInstall.window.visible || postInstall.window.cloaked), postInstall);
    check('n', 'no tab was opened for the new tab page', !(await tabUrls(inst)).some((u) => u.includes('newtab') || u.includes('new-tab-page')), await tabUrls(inst));

    // A welcome tab that arrives late (an extension's onInstalled → tabs.create) is still adopted.
    await windowsProbe(`self.probe.createTab(${JSON.stringify(url('/welcome-late'))}, 1500)`);
    const welcome = await waitForTab(inst, '/welcome-late', 15000);
    check('n', 'a tab created 1.5 s later is adopted', !!welcome, welcome);
    check(
      'n',
      'the post-install window closes once it is quiet',
      await waitFor(async () => (await foreignSnapshot(inst)).browsers.filter((b) => b.kind !== 'KeepNative').length === 0, 20000, 500),
      (await foreignSnapshot(inst)).browsers,
    );
    snap = await foreignSnapshot(inst);
    check('n', 'the install was reported once', snap.stats.installs === 1, snap.stats);
  }

  // ------------------------------------------------- (r) an install during the first seconds of a run
  if (runs('r')) {
    // `foreign.rs` re-scans the Extensions directory silently 12 s after startup so that extensions
    // *other programs* registered never look like this session's installs. An extension whose files
    // land before that re-scan must still be announced: it takes an instance of its own, because the
    // window is the first 12 s of a run.
    const early = new Instance({ data: `${DATA}-startup` });
    try {
      early.start('startup');
      const scanned = await waitFor(() => early.log().includes('installed extension(s) at startup'), 40000, 200);
      check('r', 'the startup scan ran', !!scanned);
      // After the startup scan (so the probe is not "always been there") and before the re-scan.
      const target = path.join(`${DATA}-startup`, 'User Data', 'Default', 'Extensions', INSTALLED_ID, '1.0_0');
      mkdirSync(target, { recursive: true });
      cpSync(path.join(EXT_DIR, 'probe-installed'), target, { recursive: true });
      const up = await waitFor(() => early.info(), 40000, 300);
      check('r', 'the instance is serviceable', !!up);
      const rescanned = await waitFor(() => early.log().includes('installed extension(s) after startup'), 30000, 250);
      check('r', 'the startup re-scan ran', !!rescanned);
      await early.invoke('sta://topbar/', 'debug.foreign.trigger', { url: 'chrome://newtab/' });
      const toast = await waitForToast(early, 'added', 8000);
      check('r', 'an install from before the re-scan is still announced', !!toast && !toast.message.includes('another program'), toast);
      const snap = await foreignSnapshot(early);
      check('r', 'and counted once', snap.stats.installs === 1, snap.stats);
    } finally {
      early.kill();
    }
  }

  // ---------------------------------------------------------------------------------- (p) policy
  if (runs('p')) {
    const before = (await tabUrls(inst)).length;
    // A page the manifest doesn't declare: a toast offers Open, nothing loads.
    await windowsProbe(`self.probe.createWindow(self.probe.page('undeclared.html'))`);
    const ask = await waitForToast(inst, 'wants to open', 8000);
    check('p', 'an undeclared extension page asks first', !!ask, ask);
    check('p', 'nothing was opened for it', (await tabUrls(inst)).length === before, await tabUrls(inst));
    if (ask && ask.action) {
      await inst.dispatch({ type: 'dismissToast', id: ask.id });
    }
    // A web-accessible page opens — and the ask just before it cost the adoption budget nothing:
    // core spends the budget on the verdict, not on the request (sta-core/src/store/foreign.rs,
    // `FOREIGN_OPEN_MAX`), so a page the user is only asked about can't block the next window.
    await windowsProbe(`self.probe.createWindow(self.probe.page('welcome.html'))`);
    const welcome = await waitForTab(inst, `chrome-extension://${WINDOWS_ID}/welcome.html`, 10000);
    check('p', 'a web-accessible page opens after an unanswered ask', !!welcome, welcome);

    // A chrome:// page sta has no answer for is dropped with a warning.
    const countBefore = (await tabUrls(inst)).length;
    await inst.invoke('sta://topbar/', 'debug.foreign.trigger', { url: 'chrome://settings/' });
    await sleep(2500);
    check('p', 'chrome://settings opens nothing', (await tabUrls(inst)).length === countBefore, await tabUrls(inst));
    check('p', 'it was logged as ignored', (await foreignSnapshot(inst)).events.some((e) => e.kind === 'ignored'), (await foreignSnapshot(inst)).stats);

    // Rate limit: more than 3 adoptions in 10 s. Distinct URLs, so a tab per window is countable.
    await sleep(11000); // start from an empty budget
    const floodBefore = (await tabUrls(inst)).filter((u) => u.includes('/flood')).length;
    for (let i = 0; i < 6; i++) {
      await windowsProbe(`self.probe.createWindow(${JSON.stringify(url(`/flood?i=${i}`))})`);
      await sleep(200);
    }
    const blocked = await waitForToast(inst, 'keeps opening windows', 10000);
    check('p', 'a window flood is blocked with a toast', !!blocked, blocked);
    const flooded = (await tabUrls(inst)).filter((u) => u.includes('/flood')).length - floodBefore;
    // Exactly `FOREIGN_OPEN_MAX`: the budget window was emptied first and the windows are 200 ms
    // apart, so a regression that let fewer through has to fail here too.
    check('p', 'exactly three of the six windows became tabs', flooded === 3, { flooded });

    // The ask path is spam-proof too: it has a budget of its own, and running out of it shows the
    // same toast instead of a third question.
    await sleep(11000);
    const asksBefore = (await tabUrls(inst)).length;
    for (const page of ['a.html', 'b.html', 'c.html']) {
      await windowsProbe(`self.probe.createWindow(self.probe.page('app/${page}'))`);
      await sleep(200);
    }
    const askFlood = await waitForToast(inst, 'keeps opening windows', 10000);
    check('p', 'a burst of asks ends in the same toast, not a third question', !!askFlood, askFlood);
    check('p', 'no ask ever opened a tab', (await tabUrls(inst)).length === asksBefore, await tabUrls(inst));
    // …and none of those questions cost the *adoption* budget: the next legitimate window, inside the
    // same 10 s, still opens. This is the phase-1 rate-limit decision (core spends the budget on the
    // verdict, not on the request); with the budget spent on the request, this check fails.
    await windowsProbe(`self.probe.createWindow(${JSON.stringify(url('/after-asks'))})`);
    const afterAsks = await waitForTab(inst, '/after-asks', 10000);
    check('p', 'a legitimate window opens right after a burst of asks', !!afterAsks, afterAsks);

    // Incognito windows have no place in sta.
    await sleep(11000); // let the rate-limit window pass
    await windowsProbe(`self.probe.createWindow(${JSON.stringify(url('/private'))}, 'normal', true)`);
    const incognito = await waitForToast(inst, 'private windows', 10000);
    check('p', 'an incognito window is refused', !!incognito, incognito);
    check('p', 'no private tab was opened', !(await tabUrls(inst)).some((u) => u.includes('/private')), await tabUrls(inst));
  }

  // ---------------------------------------------------------------------------------- (w) native windows + keys
  if (runs('w')) {
    await windowsProbe(`self.probe.createWindow(${JSON.stringify(url('/popup'))}, 'popup')`);
    const native = await waitFor(async () => (await foreignSnapshot(inst)).browsers.find((b) => b.kind === 'KeepNative'), 10000, 300);
    check('w', 'an extension popup window stays native', !!native && native.note.includes('popup'), native);
    check('w', 'it is visible and not cloaked', !!native && native.window.visible && !native.window.cloaked, native && native.window);
    check('w', 'it was not adopted as a tab', !(await tabUrls(inst)).some((u) => u.includes('/popup')), await tabUrls(inst));

    // Keys reach sta while a hidden Chrome window exists.
    await windowsProbe(`self.probe.createWindow(${JSON.stringify(url('/while-typing'))})`);
    await sleep(400);
    try {
      await inst.keys('ctrl+t');
      await sleep(300);
      await inst.keys('s');
      await sleep(200);
      const typed = await inst.eval('sta://command/', 'document.querySelector("input") && document.querySelector("input").value');
      check('w', 'real keys land in sta while a window is hidden', typeof typed === 'string' && typed.startsWith('s'), { typed });
      await inst.keys('escape');
    } catch (e) {
      check('w', 'real keys land in sta while a window is hidden', false, String(e));
    }
    // A sign-in flow (identity.launchWebAuthFlow) keeps its own window and gets its redirect.
    const auth = windowsProbe(`self.probe.authFlow(${JSON.stringify(url('/auth'))})`);
    const authWindow = await waitFor(async () => (await foreignSnapshot(inst)).browsers.find((b) => b.kind === 'KeepNative' && b.note.includes('no window of its own')), 8000, 250);
    check('w', 'a sign-in window stays native', !!authWindow, authWindow);
    // It is Chromium's own window frame, so Chromium writes its title — with sta's product name
    // (app.rs `branding_string`), never "… - Chromium": the user asked for no Chrome-looking windows.
    const nativeTitle = await waitFor(
      async () => (await foreignSnapshot(inst)).browsers.find((b) => b.kind === 'KeepNative' && (b.window.title || '').includes(' - '))?.window.title,
      8000,
      250,
    );
    check('w', 'a browser-framed native window is titled "… - sta"', !!nativeTitle && nativeTitle.endsWith(' - sta'), nativeTitle);
    const authResult = await Promise.race([auth, sleep(20000).then(() => 'timeout')]);
    check('w', 'the sign-in redirect reaches the extension', !!(authResult && authResult.ok && String(authResult.ok).includes('code=42')), authResult);
    check('w', 'the sign-in page was not adopted as a tab', !(await tabUrls(inst)).some((u) => u.includes('/auth')), await tabUrls(inst));

    // Close the native popup again (it would otherwise keep the window count up).
    for (const popup of (await foreignSnapshot(inst)).browsers.filter((b) => b.kind === 'KeepNative')) {
      await inst.invoke('sta://topbar/', 'debug.foreign.close', { id: popup.id });
      await sleep(500);
    }
  }

  // ---------------------------------------------------------------------------------- (d) downloads
  if (runs('d')) {
    const tab = (await inst.info()).tabs.tabs.find((t) => (t.url || '').startsWith('http://127.0.0.1'));
    await inst.execute({ type: 'startDownload', tab: tab.tab, url: url('/slow.bin') });
    const downloading = await waitFor(async () => (await inst.info()).downloadsInProgress > 0, 10000, 200);
    check('d', 'a download is in progress', !!downloading);
    await windowsProbe(`self.probe.createWindow(${JSON.stringify(url('/during-download'))})`);
    await waitForTab(inst, '/during-download', 10000);
    await sleep(5000);
    const snap = await foreignSnapshot(inst);
    check('d', 'the hidden window stays open while a download runs', snap.browsers.some((b) => b.kind === 'Handled'), snap.browsers);
    check('d', 'and says why', snap.events.some((e) => e.kind === 'close-deferred'), snap.events.slice(-3));
    const st = await inst.state();
    const dl = (st.downloads || [])[0];
    await inst.dispatch({ type: 'downloadControl', id: dl.id, action: 'cancel' });
    check(
      'd',
      'it closes once the download ends',
      await waitFor(async () => (await foreignSnapshot(inst)).browsers.filter((b) => b.kind !== 'KeepNative').length === 0, 15000, 500),
      (await foreignSnapshot(inst)).browsers,
    );
  }

  // ---------------------------------------------------------------------------------- (v) DevTools
  if (runs('v')) {
    await inst.dispatch({ type: 'openInternalPage', page: 'settings' });
    const settings = await waitForTab(inst, 'sta://settings', 10000);
    await sleep(600);
    await inst.dispatch({ type: 'toggleDevTools' });
    const refused = await waitForToast(inst, "DevTools isn't available", 5000);
    check('v', 'DevTools are refused on sta pages', !!refused, refused);
    let targets = await inst.targets();
    check('v', 'no DevTools window opened', !targets.some((t) => (t.url || '').startsWith('devtools://')), targets.map((t) => t.url).slice(0, 6));

    // The other route to DevTools is the context menu's Inspect. An sta:// page runs on the
    // trusted UI client, whose menu keeps only the edit commands — Inspect is never even offered
    // (and `tabs::show_dev_tools` refuses a UI browser a second time, in every build).
    const uiMenus = () => inst.log().split('\n').filter((l) => l.includes('context menu (ui):'));
    const before = uiMenus().length;
    const page = await inst.connect(settings);
    await inst.eval(
      settings,
      `(function () { var d = document.createElement('div'); d.id = '__e2e_ctx'; d.textContent = 'plain'; d.style.cssText = 'position:fixed;left:8px;bottom:8px;width:160px;height:28px;z-index:99999;background:#888'; d.addEventListener('contextmenu', function (e) { e.stopImmediatePropagation(); }, true); document.body.appendChild(d); return 'ok'; })()`,
    );
    const box = await inst.eval(settings, `(function () { var r = document.getElementById('__e2e_ctx').getBoundingClientRect(); return { x: Math.round(r.x + r.width / 2), y: Math.round(r.y + r.height / 2) }; })()`);
    for (const type of ['mousePressed', 'mouseReleased']) {
      await page.send('Input.dispatchMouseEvent', { type, x: box.x, y: box.y, button: 'right', buttons: type === 'mousePressed' ? 2 : 0, clickCount: 1 });
    }
    const menu = await waitFor(() => uiMenus()[before], 4000);
    check('v', 'right-click on an sta page offers no Inspect', !!menu && !/Inspect/.test(menu), menu);
    await inst.eval(settings, `document.getElementById('__e2e_ctx').remove(); 'ok'`);

    // …while a web tab still gets DevTools (the refusal must not be a blanket one).
    await inst.dispatch({ type: 'openUrl', url: url('/devtools'), target: 'newTab' });
    await waitForTab(inst, '/devtools', 10000);
    await sleep(800);
    await inst.dispatch({ type: 'toggleDevTools' });
    const frontend = await waitFor(async () => (await inst.targets()).find((t) => (t.url || '').startsWith('devtools://')), 12000, 300);
    check('v', 'DevTools open on a web tab', !!frontend, frontend && frontend.url);
    await inst.dispatch({ type: 'toggleDevTools' });
    check(
      'v',
      'closing them leaves no DevTools target',
      await waitFor(async () => !(await inst.targets()).some((t) => (t.url || '').startsWith('devtools://')), 10000, 300),
      (await inst.targets()).map((t) => t.url).slice(0, 8),
    );
  }


  // ------------------------------------------------------------------------------- (e) Ctrl+E picker
  if (runs('e')) {
    // A real Ctrl+E, from the page: the accelerator is page-first (D5a), so this goes through the OS,
    // Views' focus manager and the accelerator table.
    await inst.dispatch({ type: 'closeCommandBar' });
    await sleep(300);
    await inst.keys('ctrl+e');
    const bar = await waitFor(async () => {
      const b = (await inst.state()).commandBar;
      return b && b.mode === 'extensions' ? b : null;
    }, 8000, 150);
    check('e', 'real Ctrl+E opens the extensions picker', !!bar, bar);

    const rows = async (text) => (await inst.invoke(COMMAND, 'omnibox.query', { text, mode: 'extensions', seq: 1 })).ok.results;
    const empty = await rows('');
    const keyOf = (r) => r.key;
    check('e', 'the picker lists the installed probes and the More rows', empty.some((r) => r.key === `ext:${WINDOWS_ID}`) && empty.some((r) => r.key === `ext:${OPTIONS_ID}`) && empty.some((r) => r.key === 'ext.manage') && empty.some((r) => r.key === 'ext.get'), empty.map(keyOf));
    const groups = [...new Set(empty.map((r) => r.group))];
    check('e', 'rows are grouped (extensions … more)', groups[0] === 'extensions' && groups[groups.length - 1] === 'more', groups);
    const icon = empty.find((r) => r.key === `ext:${WINDOWS_ID}`)?.icon;
    check('e', "the row icon is sta's own same-origin route", icon?.type === 'favicon' && icon.url === `sta://command/__ext-icon/${WINDOWS_ID}/32`, icon);

    // A name typed with a Hangul IME on: "sta" on the 2-set keyboard is ㄴㅅㅁ. Core retries the query
    // through the layout, so the same extension is found (UX15).
    const latin = await rows('windows');
    const jamo = await rows('ㄴㅅㅁ');
    check('e', 'a query typed in Hangul mode still finds the extension', jamo.some((r) => r.key === `ext:${WINDOWS_ID}`) && latin.some((r) => r.key === `ext:${WINDOWS_ID}`), { jamo: jamo.map(keyOf), latin: latin.map(keyOf) });

    // Enter on the row for an extension whose options page opens in a tab.
    const optionsRow = empty.find((r) => r.key === `ext:${OPTIONS_ID}`);
    await inst.dispatch({ type: 'commitOmnibox', command: optionsRow.command });
    const optTab = await waitForTab(inst, `chrome-extension://${OPTIONS_ID}/options.html`, 10000);
    check('e', "Enter opens an extension's options page as a tab", !!optTab, optTab);

    // The app menu is the way in for pages that take Ctrl+E themselves (D5a).
    await inst.dispatch({ type: 'openSidebarPanel', panel: { type: 'appMenu' } });
    const menu = await waitFor(
      () =>
        inst.eval(
          SIDEBAR,
          `(() => { const row = [...document.querySelectorAll('.menu-item')].find((e) => e.querySelector('.menu-label') && e.querySelector('.menu-label').textContent === 'Extensions'); return row ? row.textContent : null; })()`,
        ),
      6000,
      200,
    );
    check('e', 'the app menu offers Extensions · Ctrl+E', !!menu && menu.includes('Ctrl+E'), menu);
    await inst.dispatch({ type: 'closeSidebarPanel' });

    // The `>` list reaches the same places.
    const actions = (await inst.invoke(COMMAND, 'omnibox.query', { text: '>extensions', mode: 'newTab', seq: 2 })).ok.results;
    check('e', '> Show Extensions and > Manage Extensions exist', actions.some((r) => r.title === 'Show Extensions') && actions.some((r) => r.title === 'Manage Extensions'), actions.map((r) => r.title));

    // P3-E2E-4: "one options tab per extension" has to survive an options page that routes itself on
    // load (`location.replace(pathname + '#general')`), which is what nearly every real one does — the
    // tab's URL is then no longer the URL sta opened. The listing is injected so the probe's
    // self-routing page is the one Enter opens.
    const routing = { id: WINDOWS_ID, name: 'sta probe: windows', shortName: '', version: '1.0', description: '', state: 'enabled', install: 'unpacked', sourceLabel: 'Loaded from a folder', popup: null, options: 'options-hash.html', sidePanel: null, commands: [] };
    await inst.dispatch({ type: 'extensionsChanged', extensions: [routing] });
    await inst.dispatch({ type: 'runExtension', id: WINDOWS_ID, action: 'options' });
    const routed = await waitForTab(inst, 'options-hash.html#general', 10000);
    const countHash = async () => (await tabUrls(inst)).filter((u) => u.includes('options-hash')).length;
    const afterFirst = await countHash();
    await inst.dispatch({ type: 'runExtension', id: WINDOWS_ID, action: 'options' });
    await inst.dispatch({ type: 'runExtension', id: WINDOWS_ID, action: 'primary' });
    await sleep(800);
    const afterMore = await countHash();
    check('e', 'P3-E2E-4: an options page that routes itself still gets one tab', !!routed && afterFirst === 1 && afterMore === 1, { routed, afterFirst, afterMore, tabs: await tabUrls(inst) });
    await inst.execute([{ type: 'refreshExtensions' }]);
    await waitFor(async () => (await inst.state()).extensions.items.some((e) => e.id === OPTIONS_ID), 6000, 200);
  }

  // -------------------------------------------------------------------- (c) the popup card (S3, S4)
  if (runs('c')) {
    await inst.dispatch({ type: 'closeCommandBar' });
    // A page to anchor the card to, so the pane rect is a tab's and not the empty state's.
    await inst.dispatch({ type: 'openUrl', url: url('/card-host'), target: 'newTab' });
    await waitForTab(inst, '/card-host', 10000);
    await sleep(500);

    const t0 = Date.now();
    await inst.dispatch({ type: 'runExtension', id: WINDOWS_ID, action: 'primary' });
    const card = await waitFor(async () => {
      const o = overlay(await inst.info(['overlays']), 'ExtensionPopup');
      return o && o.visible ? o : null;
    }, 10000, 150);
    const shownMs = Date.now() - t0;
    check('c', 'the popup card appears for an extension with a popup', !!card, { card, shownMs });
    // S4: the card is the popup's own size (the probe popup is 280 DIP wide), plus sta's header.
    const [pw, ph] = card?.pageSize ?? [];
    check('c', "S4: the card takes the popup's reported size (280 DIP wide, > 25 high)", pw === 280 && ph > 25, { pageSize: card?.pageSize, bounds: card?.bounds, shownMs });
    check('c', 'sta draws the header above the page', card?.viewRect && card.peekViewRect && card.viewRect[3] === 40 && card.peekViewRect[1] === card.viewRect[1] + 40, { header: card?.viewRect, page: card?.peekViewRect });
    const popupView = await inst.state();
    check('c', 'the card names the extension and offers its options', popupView.extensions.popup?.id === WINDOWS_ID && popupView.extensions.popup?.hasOptions === true && popupView.extensions.popup?.failed === false, popupView.extensions.popup);

    // S3: what the popup page itself can do inside sta's card, and what it sees of sta's tabs.
    const work = await waitFor(() => inst.eval({ match: 'popup.html' }, `document.getElementById('work') && document.getElementById('work').textContent`), 8000, 200);
    const tabsLine = await inst.eval({ match: 'popup.html' }, `document.getElementById('tabs') && document.getElementById('tabs').textContent`);
    check('c', 'S3: the popup runs as an extension page (chrome.storage works)', work === 'storage: ok', { work, tabsLine });
    // Prebuilt CEF (D1a): sta's tabs are not Chromium tabs, so this is expected to find none. The
    // check records what actually happened rather than asserting the aspiration.
    check('c', `S3: chrome.tabs.query from the popup answers "${tabsLine}" (D1a: sta tabs are invisible to extensions)`, typeof tabsLine === 'string' && tabsLine.startsWith('tabs:'), tabsLine);

    // The client is locked to the extension's own origin: a navigation away is cancelled and offered
    // as a tab instead (R-SEC-6), and the popup document stays where it was.
    const tabsBefore = (await tabUrls(inst)).length;
    await inst.eval({ match: 'popup.html' }, `location.href = ${JSON.stringify(url('/popup-escape'))}`, { gesture: true });
    const escaped = await waitForTab(inst, '/popup-escape', 8000);
    check('c', 'the popup card cannot leave its origin; the link becomes a tab', !!escaped && (await tabUrls(inst)).length === tabsBefore + 1, { escaped });

    // Esc closes it (first in the Esc chain).
    await inst.dispatch({ type: 'runExtension', id: WINDOWS_ID, action: 'popup' });
    await waitFor(async () => overlay(await inst.info(['overlays']), 'ExtensionPopup')?.visible, 8000, 150);
    await inst.keys('escape');
    const closed = await waitFor(async () => !(await inst.state()).extensions.popup, 6000, 150);
    check('c', 'Esc closes the popup card', !!closed);

    // A popup that renders nothing says so instead of showing an empty rectangle (UX1). The listing
    // is injected (a shell event) so the card points at the probe's blank page.
    const blank = { id: WINDOWS_ID, name: 'sta probe: blank popup', shortName: '', version: '1.0', description: '', state: 'enabled', install: 'unpacked', sourceLabel: 'Loaded from a folder', popup: 'popup-blank.html', options: 'options.html', sidePanel: null, commands: [] };
    await inst.dispatch({ type: 'extensionsChanged', extensions: [blank] });
    await inst.dispatch({ type: 'runExtension', id: WINDOWS_ID, action: 'primary' });
    const blankSamples = [];
    const failed = await waitFor(async () => {
      const o = overlay(await inst.info(['overlays']), 'ExtensionPopup');
      const s = await inst.state();
      blankSamples.push({ visible: !!(o && o.visible), size: o && o.pageSize });
      return s.extensions.popup?.failed ? s.extensions.popup : null;
    }, 9000, 250);
    const failedCard = overlay(await inst.info(['overlays']), 'ExtensionPopup');
    check('c', 'a popup that never renders is shown as a failure, not as an empty card', !!failed && failedCard?.visible === true, { failed, visible: failedCard?.visible });
    // …and it was not on screen *before* that verdict either: an empty document measures as the
    // 25x25 clamp minimum, which used to be taken for "a size arrived" and showed a sliver with a
    // clipped header for the whole 3 s (P3-E2E-3 / SEC-P3-6).
    const slivers = blankSamples.filter((x) => x.visible && x.size && x.size[0] <= 25 && x.size[1] <= 25);
    check('c', 'and the card was never shown at the 25x25 clamp minimum', slivers.length === 0, { slivers: slivers.slice(0, 3), samples: blankSamples.slice(0, 4) });
    const line = await inst.eval(EXTENSION, `document.body.innerText.trim()`);
    check('c', 'the card says so in its own words', typeof line === 'string' && line.includes("doesn't work in sta yet"), line);
    await inst.dispatch({ type: 'closeExtensionPopup' });

    // A popup that paints **late** (2 s: an MV3 popup waiting for a cold service worker) is the other
    // half of that rule. It must end up in the card at its own size and never be declared broken —
    // the failure verdict comes from a measurement taken at the deadline, not from the last timed one
    // 1.4 s earlier (P3-E2E-1).
    const slow = { ...blank, name: 'sta probe: slow popup', popup: 'popup-slow.html' };
    await inst.dispatch({ type: 'extensionsChanged', extensions: [slow] });
    await inst.dispatch({ type: 'runExtension', id: WINDOWS_ID, action: 'primary' });
    const slowSamples = [];
    const slowStart = Date.now();
    while (Date.now() - slowStart < 5000) {
      const o = overlay(await inst.info(['overlays']), 'ExtensionPopup');
      const s = await inst.state();
      slowSamples.push({ ms: Date.now() - slowStart, visible: !!(o && o.visible), size: o && o.pageSize, failed: !!(s.extensions.popup && s.extensions.popup.failed) });
      await sleep(150);
    }
    const painted = slowSamples.find((x) => x.visible && x.size && x.size[0] > 100);
    const calledBroken = slowSamples.filter((x) => x.failed);
    check('c', 'P3-E2E-1: a popup that paints at 2 s is shown, not declared broken', !!painted && calledBroken.length === 0, { painted, calledBroken: calledBroken.slice(0, 2) });
    check('c', 'and its page is still alive after the deadline', (await inst.eval({ match: 'popup-slow.html' }, `document.getElementById('late') ? document.getElementById('late').textContent : 'gone'`)).startsWith('rendered after'));
    const slowSliver = slowSamples.filter((x) => x.visible && x.size && x.size[0] <= 25 && x.size[1] <= 25);
    check('c', 'and the card waited for a real size instead of showing a sliver', slowSliver.length === 0, slowSliver.slice(0, 3));
    await inst.dispatch({ type: 'closeExtensionPopup' });

    // -------- the card's client: the popup page is untrusted code in an overlay with no address bar
    await inst.dispatch({ type: 'extensionsChanged', extensions: [{ ...blank, popup: 'popup.html' }] });
    // Waits for the *card* (a visible overlay means the page reported a real size, so its target
    // exists), not just for core's state: `extensions.popup` is set the moment the command is applied.
    const reopen = async () => {
      await inst.dispatch({ type: 'runExtension', id: WINDOWS_ID, action: 'popup' });
      return waitFor(async () => {
        const o = overlay(await inst.info(['overlays']), 'ExtensionPopup');
        return o && o.visible ? o : null;
      }, 10000, 150);
    };
    const cardUp = await reopen();
    check('c', 'the card is up again for the client checks', !!cardUp, cardUp && cardUp.pageSize);
    // SEC-P3-1: an external protocol needs a **user gesture**, exactly like a tab. A popup that
    // navigates itself to `zoommtg:` / `ms-settings:` on a timer must launch nothing.
    await inst.eval({ match: 'popup.html' }, `location.href = 'zoommtg://p3fix/nogesture'`);
    await sleep(700);
    const launched = (u) => inst.log().includes(`external protocol (test): ${u}`);
    check(
      'c',
      'SEC-P3-1: a gesture-less external protocol from the card is not launched',
      !launched('zoommtg://p3fix/nogesture') && inst.log().includes('without a user gesture ignored (extension popup)'),
      inst.log().split('\n').filter((l) => l.includes('external protocol')).slice(-2),
    );
    await reopen();
    await inst.eval(
      { match: 'popup.html' },
      `(() => { const a = document.createElement('a'); a.href = 'zoommtg://p3fix/gesture'; a.textContent = 'x'; document.body.appendChild(a); a.click(); return 'clicked'; })()`,
      { gesture: true },
    );
    check('c', '…and a real click in the popup still opens the app', await waitFor(() => launched('zoommtg://p3fix/gesture'), 4000, 200), inst.log().split('\n').filter((l) => l.includes('external protocol')).slice(-2));
    // SEC-P3-2: a file chooser is cancelled — a native "Open" dialog over a card with no address bar
    // names no origin at all.
    await reopen();
    const dialogsBefore = await inst.dialogs();
    await inst.eval({ match: 'popup.html' }, `(() => { const i = document.createElement('input'); i.type = 'file'; document.body.appendChild(i); i.click(); return 'clicked'; })()`, { gesture: true });
    await sleep(1000);
    const dialogsAfter = await inst.dialogs();
    check('c', 'SEC-P3-2: a file chooser from the popup card is cancelled', dialogsAfter.length === dialogsBefore.length, { before: dialogsBefore, after: dialogsAfter });
    if (dialogsAfter.length > dialogsBefore.length) await inst.win('closedialogs');
    // SEC-P3-3: a permission request is *refused*, not ignored — Alloy's default would leave the
    // page's promise pending forever, and a popup awaiting it would look broken for the wrong reason.
    const answered = await inst.eval(
      { match: 'popup.html' },
      `Promise.race([Notification.requestPermission().then((r) => 'answered:' + r), new Promise((r) => setTimeout(() => r('pending'), 2500))])`,
    );
    check('c', 'SEC-P3-3: a permission request from the card is answered, not left pending', typeof answered === 'string' && answered.startsWith('answered:'), answered);
    await inst.dispatch({ type: 'closeExtensionPopup' });

    // Back to the truth (the injected listings were test fixtures).
    await inst.execute([{ type: 'refreshExtensions' }]);
    await waitFor(async () => (await inst.state()).extensions.items.some((e) => e.id === OPTIONS_ID), 6000, 200);

    // A permission prompt on the same pane takes the card away (SEC-4) and the prompt ignores input
    // for 400 ms after it appears.
    const host = (await inst.info(['tabs'])).tabs.tabs.find((t) => (t.url || '').includes('/card-host'));
    if (host) {
      await inst.dispatch({ type: 'activateItem', id: host.tab });
      await sleep(400);
      // The prompt surface is created on its first show, and the guard is 400 ms long: warm it up
      // with a prompt of its own first, so the measured one can be sampled from the first frame.
      const disabledStates = async () => {
        try {
          return await inst.eval(PERMISSION, `(() => { const b = [...document.querySelectorAll('.perm-actions button')]; return b.length ? JSON.stringify(b.map((x) => x.disabled)) : null; })()`);
        } catch {
          return null;
        }
      };
      await inst.dispatch({ type: 'permissionRequested', id: 9000, tab: host.tab, origin: `http://127.0.0.1:${HTTP}`, kinds: ['geolocation'] });
      const warm = await waitFor(disabledStates, 8000, 200);
      await inst.dispatch({ type: 'resolvePermission', id: 9000, allow: false, remember: false });
      await waitFor(async () => (await inst.state()).permissionPrompts.length === 0, 6000, 150);
      check('c', 'the permission prompt surface is up', !!warm, warm);

      await inst.dispatch({ type: 'runExtension', id: WINDOWS_ID, action: 'popup' });
      await waitFor(async () => (await inst.state()).extensions.popup, 8000, 150);
      await inst.dispatch({ type: 'permissionRequested', id: 9001, tab: host.tab, origin: `http://127.0.0.1:${HTTP}`, kinds: ['camera'] });
      // Sample as fast as MCP allows: the guard is 400 ms, so an early sample must find the buttons
      // disabled and a later one must find them usable again.
      const samples = [];
      const until = Date.now() + 1500;
      while (Date.now() < until) {
        samples.push({ at: Date.now(), states: await disabledStates() });
        if (samples.some((s) => s.states === '[true,true]') && samples[samples.length - 1].states === '[false,false]') break;
        await sleep(40);
      }
      const gone = await waitFor(async () => !(await inst.state()).extensions.popup, 6000, 150);
      check('c', 'a permission prompt closes the popup card (it never sits over a prompt)', !!gone);
      const guarded = samples.find((s) => s.states === '[true,true]');
      const usable = samples.find((s) => s.states === '[false,false]');
      check(
        'c',
        'the prompt ignores input for 400 ms after it appears, then answers again',
        !!guarded && !!usable && usable.at > guarded.at,
        samples.map((s) => `${s.at - samples[0].at}ms ${s.states}`),
      );
      await inst.dispatch({ type: 'resolvePermission', id: 9001, allow: false, remember: false });
    }
  }

  // ------------------------------------------------------ (g) Settings › Extensions and the backend
  if (runs('g')) {
    await inst.dispatch({ type: 'openUrl', url: 'sta://settings/?section=extensions', target: 'newTab' });
    const settings = await waitForTab(inst, 'sta://settings', 10000);
    await sleep(900);
    const listed = await inst.eval(SETTINGS, `(() => { const s = document.getElementById('extensions'); return s ? s.innerText : null; })()`);
    check('g', 'Settings › Extensions lists the installed probes', typeof listed === 'string' && listed.includes('sta probe: windows') && listed.includes('sta probe: options tab'), (listed || '').slice(0, 400));
    const iconInfo = await inst.eval(SETTINGS, `(() => { const i = [...document.querySelectorAll('#extensions img.favicon')].map((x) => ({ src: x.getAttribute('src'), w: x.naturalWidth })); return JSON.stringify(i); })()`);
    check('g', "UX7: the extension's own icon loads from sta's route", typeof iconInfo === 'string' && JSON.parse(iconInfo).some((x) => x.src.includes('__ext-icon') && x.w > 0), iconInfo);

    // S7: the real backend. Turning an unpacked probe off and on again goes through a hidden
    // chrome://extensions window, and the listing that follows comes from the profile on disk.
    const stateOf = async (id) => (await inst.state()).extensions.items.find((e) => e.id === id)?.state;
    const t0 = Date.now();
    await inst.dispatch({ type: 'setExtensionEnabled', id: OPTIONS_ID, enabled: false });
    const off = await waitFor(async () => ((await stateOf(OPTIONS_ID)) === 'off' ? true : null), 15000, 250);
    const offMs = Date.now() - t0;
    const backend = (await inst.info(['extBackend'])).extBackend;
    check('g', 'S7: the hidden backend turns an extension off', !!off, { offMs, stats: backend.stats, lastError: backend.lastError });
    check('g', 'S7: the backend window was never shown and never aborted', backend.stats.abortedVisible === 0 && backend.stats.timedOut === 0 && backend.stats.ok >= 1, backend.stats);
    const foreignNow = await foreignSnapshot(inst);
    check('g', 'S7: no visible Chrome window while the backend ran', visibleForeignWindows(foreignNow).length === 0, visibleForeignWindows(foreignNow));
    check('g', 'S7: sta kept the foreground', await foregroundOurs(inst));
    const t1 = Date.now();
    await inst.dispatch({ type: 'setExtensionEnabled', id: OPTIONS_ID, enabled: true });
    // Only meaningful when the extension really was off a moment ago (otherwise core answers a
    // no-op), so the previous check gates this one.
    const on = off ? await waitFor(async () => ((await stateOf(OPTIONS_ID)) === 'enabled' ? true : null), 15000, 250) : null;
    check('g', 'S7: and on again', !!off && !!on, { onMs: Date.now() - t1, off: !!off });

    // P3-E2E-2: a state Chromium confirmed **stays** confirmed. Chromium commits `Secure Preferences`
    // 8-12 s later, and the operation's own re-reads (300 ms, 1.5 s) used to rebuild the row from
    // those uncommitted preferences — so the row flipped back to "On" for seconds after the user
    // turned it off.
    await inst.dispatch({ type: 'setExtensionEnabled', id: OPTIONS_ID, enabled: false });
    const offAgain = await waitFor(async () => ((await stateOf(OPTIONS_ID)) === 'off' ? true : null), 15000, 250);
    const stickySamples = [];
    const stickyStart = Date.now();
    while (Date.now() - stickyStart < 6000) {
      stickySamples.push({ ms: Date.now() - stickyStart, state: await stateOf(OPTIONS_ID) });
      await sleep(250);
    }
    const flipped = stickySamples.filter((x) => x.state !== 'off');
    check('g', 'P3-E2E-2: the confirmed state does not flip back while the profile catches up', !!offAgain && flipped.length === 0, { offAgain: !!offAgain, flipped: flipped.slice(0, 4) });
    await inst.dispatch({ type: 'setExtensionEnabled', id: OPTIONS_ID, enabled: true });
    await waitFor(async () => ((await stateOf(OPTIONS_ID)) === 'enabled' ? true : null), 15000, 250);

    // The disclosure: an extension another program added is turned on only after Chrome's own
    // warnings were loaded. The listing is injected to put a real, installed extension into that
    // state; the GetInfo that follows is the real backend reading the real extension.
    const external = { id: WINDOWS_ID, name: 'sta probe: windows', shortName: '', version: '1.0', description: '', state: 'needsApproval', install: 'externalStore', sourceLabel: 'Added by another program · Chrome Web Store', popup: 'popup.html', options: 'options.html', sidePanel: null, commands: [] };
    // Let the previous operation's re-reads of the profile land first: they would replace this
    // injected listing a moment after it is installed.
    await sleep(2500);
    await inst.dispatch({ type: 'extensionsChanged', extensions: [external] });
    await inst.dispatch({ type: 'setExtensionEnabled', id: WINDOWS_ID, enabled: true });
    // Read at once: the operation's own re-read of the profile is seconds away and will replace this
    // injected listing with the truth (which is what it is for).
    const rightAfter = await stateOf(WINDOWS_ID);
    check('g', 'the extension is still off after that first press (Enter never enables)', rightAfter === 'needsApproval', rightAfter);
    const details = await waitFor(async () => (await inst.state()).extensions.details.find((d) => d.id === WINDOWS_ID), 15000, 250);
    check('g', "S7: Turn on loads Chrome's own warnings, host access and source first", !!details && typeof details.source === 'string' && details.source.length > 0, details);

    // A local CRX another program registered can only be removed (D6a).
    const local = { ...external, install: 'externalLocal', sourceLabel: 'Added by another program · C:\\\\Program Files\\\\Probe' };
    await inst.dispatch({ type: 'extensionsChanged', extensions: [local] });
    await inst.dispatch({ type: 'setExtensionEnabled', id: WINDOWS_ID, enabled: true });
    const refusal = await waitForToast(inst, "can't turn this on", 6000);
    check('g', 'a local CRX another program added is never turned on', !!refusal, refusal);
    await inst.execute([{ type: 'refreshExtensions' }]);
    await waitFor(async () => (await inst.state()).extensions.items.length >= 2, 8000, 200);

    // Remove, on the options probe (the last thing this section does with it): Chromium really
    // uninstalls it, so the row goes away. Its files are sta's own, and the next launch loads it
    // again from `--load-extension`.
    const present = await waitFor(async () => ((await inst.state()).extensions.items.some((e) => e.id === OPTIONS_ID) ? true : null), 8000, 250);
    await inst.dispatch({ type: 'removeExtension', id: OPTIONS_ID });
    // Chromium always confirms a removal itself (gate S7: `showConfirmDialog: false` is only for an
    // extension removing itself, and developerPrivate has no uninstall at all in 152). sta lets that
    // dialog be the confirmation; here the suite answers it, the way a person would.
    const dialogs = await waitFor(async () => {
      const owned = await inst.ownedWindows({ any: true });
      return owned.length ? owned : null;
    }, 15000, 250);
    check('g', "Chromium's own remove confirmation is shown, owned by the hidden backend window", !!dialogs && dialogs.length === 1, dialogs);
    const backendWindow = (await inst.info(['extBackend'])).extBackend.current;
    check('g', 'and the backend window itself stayed cloaked', !!backendWindow && backendWindow.window && backendWindow.window.cloaked === true, backendWindow && backendWindow.window);
    if (dialogs) {
      await sleep(900); // Chromium's input-protection delay
      const pressed = await inst.acceptDialog('enter', dialogs[0].hwnd);
      check('g', 'the remove dialog was accepted through MCP', !!pressed && pressed.sent, pressed);
    }
    const removed = await waitFor(async () => (!(await inst.state()).extensions.items.some((e) => e.id === OPTIONS_ID) ? true : null), 20000, 300);
    const after = (await inst.info(['extBackend'])).extBackend;
    check('g', 'Remove uninstalls the extension through the backend', !!present && !!removed, { stats: after.stats, lastError: after.lastError });
    const gone = await inst.eval(SETTINGS, `(() => { const s = document.getElementById('extensions'); return s ? s.innerText.includes('sta probe: options tab') : null; })()`);
    check('g', 'and the row is gone from Settings › Extensions', gone === false, gone);
  }

  // ------------------------------------------------------------------- (l) crash loop → safe mode
  if (runs('l')) {
    const dir = `${DATA}-safemode`;
    const launch = (tag, fresh) => new Instance({ data: dir, fresh, args: [] }).start(tag);
    let crashed = 0;
    for (const [tag, fresh] of [['crash1', true], ['crash2', false]]) {
      const victim = launch(tag, fresh);
      try {
        const up = await waitFor(() => victim.info(), 40000, 300);
        if (up && crashed === 0) {
          // A tab to restore, so "restored unloaded" is observable in the third run.
          await victim.dispatch({ type: 'openUrl', url: url('/safe-mode-tab'), target: 'newTab' });
          await waitFor(async () => (await victim.state()).current, 10000, 200);
          await sleep(1200); // the debounced save
        }
        if (up) crashed++;
      } finally {
        victim.kill(); // no clean shutdown: the launch marker stays behind
      }
      await sleep(800);
    }
    check('l', 'two runs ended abnormally within a minute of launching', crashed === 2, { crashed });
    const third = launch('safemode', false);
    try {
      const up = await waitFor(() => third.info(), 40000, 300);
      const state = up ? await third.state() : null;
      check('l', 'the third run starts in safe mode', !!state && state.extensions.safeMode === true, state && state.extensions.safeMode);
      check('l', 'safe mode restores the tabs without loading them', !!state && state.current === null && state.spaces.some((s) => s.today.length > 0), { current: state && state.current, today: state && state.spaces.map((s) => s.today.length) });
      const guard = up ? (await third.info(['safeMode'])).safeMode : null;
      check('l', 'the crash-loop guard cleared its counter for the next launch', !!guard && guard.crashes === 0 && guard.safeMode === true, guard);
      if (up) {
        await third.dispatch({ type: 'openUrl', url: 'sta://settings/?section=extensions', target: 'newTab' });
        await waitFor(async () => (await third.state()).current?.url.startsWith('sta://settings'), 10000, 250);
        await sleep(900);
        const text = await third.eval(SETTINGS, `(() => { const s = document.getElementById('extensions'); return s ? s.innerText : null; })()`);
        check('l', 'Settings › Extensions shows the safe-mode banner', typeof text === 'string' && text.includes('safe mode'), (text || '').slice(0, 300));
      }
    } finally {
      third.kill();
    }
  }

  // ---------------------------------------------------------------------------------- (s) screenshots
  if (runs('s')) {
    await toastShots(inst, '100');
    await extensionShots(inst, '100');
    // The same four toasts at 150 % display scaling, in an instance of its own (the scale is a
    // launch switch), like chrome-e2e's (o.round150).
    const hi = new Instance({ data: `${DATA}-scale150`, args: ['--force-device-scale-factor=1.5', PROBE_ARGS] }).start('scale150');
    try {
      const up = await waitFor(() => hi.info(), 40000, 300);
      check('s', 'a 150 % instance started', !!up);
      if (up) {
        await toastShots(hi, '150');
        await extensionShots(hi, '150');
      }
    } finally {
      hi.kill();
    }
  }

  // ---------------------------------------------------------------------------------- (webstore) opt-in
  if (process.env.STA_E2E_WEBSTORE === '1' && runs('webstore')) {
    const item = process.env.STA_E2E_WEBSTORE_ITEM || 'https://chromewebstore.google.com/detail/adblock-%E2%80%94-block-ads-acros/gighmmpiobklfepjocnamgkkbiglidom';
    await inst.dispatch({ type: 'openUrl', url: item, target: 'newTab' });
    const store = await waitForTab(inst, 'chromewebstore.google.com', 30000);
    check('webstore', 'the Web Store page opened', !!store);
    await sleep(6000);
    await inst.eval('chromewebstore.google.com', `(function(){const n=[...document.querySelectorAll('button,[role=button]')].find((e)=>/no thanks/i.test(e.textContent||''));if(n)n.click();})()`, { gesture: true });
    await sleep(500);
    const clicked = await inst.eval(
      'chromewebstore.google.com',
      `(function(){const b=[...document.querySelectorAll('button,[role=button]')].find((e)=>/add to chrome/i.test(e.textContent||''));if(!b)return 'no button';b.scrollIntoView({block:'center'});b.click();return 'clicked';})()`,
      { gesture: true },
    );
    check('webstore', 'Add to Chrome clicked', clicked === 'clicked', clicked);
    // Chromium's install dialog is a `Chrome_WidgetWin_1` views widget owned by sta's main window, so
    // `win('dialogs')` (visible `#32770` windows) can never see it — that check was unfalsifiable.
    // `ownedWindows()` answers on ownership, and on the two style words gate S17 measured.
    const WS_CHILD = 0x4000_0000;
    const WS_EX_DLGMODALFRAME = 0x1;
    const WS_EX_TOOLWINDOW = 0x80;
    const installDialogs = await waitFor(async () => {
      const owned = await inst.ownedWindows();
      const dialogs = owned.filter((w) => (w.style & WS_CHILD) === 0 && (w.exStyle & WS_EX_DLGMODALFRAME) !== 0 && (w.exStyle & WS_EX_TOOLWINDOW) === 0);
      return dialogs.length ? dialogs : null;
    }, 15000, 300);
    check('webstore', 'the install dialog is owned by sta', !!installDialogs && installDialogs.length === 1, installDialogs);
    // UX9: it was moved onto the Web Store pane instead of staying where Chromium put it.
    const placed = (await foreignSnapshot(inst)).events.filter((e) => e.kind === 'install-dialog-placed');
    check('webstore', 'the install dialog was placed over the pane', placed.length >= 1, placed.slice(-1));
    if (installDialogs) {
      // The dialog opens with **Cancel** focused and has no default button, on purpose: Enter does
      // nothing and Space cancels (both measured, C:/ast/tmp/s7/p1-fix/logs/dlgprobe-*.log). Tab
      // moves the focus to "Add extension"; Space presses it. The wait is Chromium's input-protection
      // delay (~275 ms after the dialog appears).
      await sleep(1200);
      const tabbed = await inst.acceptDialog('tab', installDialogs[0].hwnd);
      const pressed = await inst.acceptDialog('space', installDialogs[0].hwnd);
      check('webstore', 'the Add extension dialog was accepted through MCP', !!pressed && pressed.sent && !!tabbed && tabbed.sent, { tabbed, pressed });
      // Not `waitForToast(inst, 'added')`: on a machine with registry extensions the startup toast
      // ("N extensions were added by other programs") matches that too, and this check then measured
      // nothing. The install toast names the extension and offers the one thing it needs to say.
      const installed = await waitFor(async () => {
        const t = (await inst.state()).toast;
        return t && /added/.test(t.message) && !/another program|other programs/.test(t.message) ? t : null;
      }, 40000, 250);
      check('webstore', 'the install toast names the extension and says how to use it', !!installed && /adblock/i.test(installed.message) && installed.message.includes('Ctrl+E'), installed);
      // The toast promises "· Ctrl+E" and lives 2.5 s. Chromium writes the extension's preferences
      // ~11 s after the install, and until it does, `one()` refuses to list an extension it can say
      // nothing certain about — so the picker did not have the row for another 10.8 s, i.e. the one
      // instruction the toast gives was false for its whole life and for eight seconds after it
      // (P3). `extensions.rs` `FRESH` now lists a user install of this session right away.
      const listedAt = Date.now();
      const listed = await waitFor(async () => {
        const items = (await inst.state()).extensions?.items || [];
        return items.find((e) => /adblock/i.test(e.name) || /adblock/i.test(e.shortName || '')) || null;
      }, 2500, 200);
      check('webstore', 'and the picker has the row while that toast is still up (no 11 s preferences wait)', !!listed, { ms: Date.now() - listedAt, listed: listed && { id: listed.id, state: listed.state, install: listed.install } });
      const post = await waitFor(async () => (await tabUrls(inst)).find((u) => !u.includes('chromewebstore.google.com') && (u.startsWith('chrome-extension://') || u.includes('adblock'))), 40000, 500);
      check('webstore', 'the post-install page opened as an sta tab', !!post, post);
      const snap = await foreignSnapshot(inst);
      check('webstore', 'no Chrome window was ever on screen', visibleForeignWindows(snap).length === 0, visibleForeignWindows(snap));
      check('webstore', 'sta kept the foreground', await foregroundOurs(inst));
    }
  }

  // ------------------------------------------------- (hygiene) no console window during the run
  await checkNoConsoleWindows(inst, consoles, check);

  // ---------------------------------------------------------------------------------- (x) shutdown
  if (runs('x') && !KEEP_OPEN) {
    await windowsProbe(`self.probe.createWindow(${JSON.stringify(url('/at-shutdown'))}, 'popup')`);
    await windowsProbe(`self.probe.createWindow(${JSON.stringify(url('/hidden-at-shutdown'))})`);
    await sleep(1200);
    const snap = await foreignSnapshot(inst);
    check('x', 'two Chrome-created browsers are open', snap.browsers.length >= 2, snap.browsers.map((b) => b.kind));
    const t0 = Date.now();
    await inst.win('close'); // WM_CLOSE, the Alt+F4 / taskbar path
    inst.closeSockets(); // the MCP session was needed for the message above; let go of it now
    const gone = await waitFor(() => {
      try {
        process.kill(inst.pid, 0);
        return false;
      } catch {
        return true;
      }
    }, 20000, 200);
    check('x', 'the process exits with Chrome-created browsers open', !!gone, { ms: Date.now() - t0 });
    const log = inst.log();
    check('x', 'the shutdown closed them', log.includes('Chrome-created browser(s)') && log.includes('all browsers closed'), log.split('\n').filter((l) => l.includes('shutdown')).slice(-3));
    check('x', 'no shutdown timeout', !log.includes('shutdown timed out'));
  }
}

const startedAt = Date.now();
let code = 1;
try {
  await main();
  code = summary();
} catch (e) {
  console.error(e);
  code = 1;
} finally {
  if (slowHold) clearInterval(slowHold);
  server.close();
  if (!KEEP_OPEN) inst.kill();
  process.exit(code);
}
