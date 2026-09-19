// Mock backend for UI development without the shell (docs/PROTOCOL.md §1 "Mock mode").
//
// `ipc.js` imports this module only in mock mode and uses `mockQuery`, which has exactly the
// `window.__staQuery` signature: string requests in, JSON strings out, persistent
// `__subscribe` queries receiving `{event, payload}` pushes. So the whole IPC client (revision
// ordering, JSON parsing, callbacks) is exercised exactly as in the app.
//
// Data comes from `/common/fixtures/*.json` (shaped like the Rust serde types). Commands are
// applied by the forgiving local reducers in `mock-reducers.js`.
//
// Query parameters (all optional; see ui/README.md):
//   mock                 force mock mode (implied outside sta://)
//   fixture=<name>       base UiState fixture file (default uiState, or uiStateDark with dark=1)
//   dark=1|0             dark or light colors
//   space=<id>           active space
//   empty=1              no active item in the active space (empty state, current = null)
//   sidebar=0            sidebar hidden;  width=<px> sidebar width
//   hover=1              with sidebar=0: the floating sidebar is shown (pushes `sidebar.hover`)
//   maximized=1          window maximized
//   commandBar=<mode>    open the command bar (newTab|editUrl|split|actions|extensions); text=<prefill>
//   extPopup=<id|1|failed>  extension popup card: an extension id, `1` for the first one with a
//                        popup, or `failed` for the honest-failure line
//   extensions=<scenario>  extensions list: none|safeMode (default: the fixture's four)
//   panel=<panel>        sidebar panel: downloads|appMenu|newSpace|editSpace[:id]|renameItem:<id>|editPinned:<id>
//   find=1               find bar for the focused tab; text=<query>
//   toast=<message>      show a toast; toastAction=<label> adds an Undo-style action
//   permission=<kinds>   permission prompt, e.g. permission=camera,microphone
//   switcher=1           Ctrl+Tab switcher open
//   peek=1|popup         Peek open (popup = feature popup)
//   agent=<scenario>     AI agent UI: connection|unverified|site|tab|panel|busy|paused (mock-agent.js)
//   agentAccess=<level>  settings.agentAccess: off|readOnly|full
//   latency=<ms>         delay every response (default 0)
//   quiet=1              don't log requests to the console
//
// Test hooks: `window.__mockLog` (every request `{time, cmd, payload}`), `window.__mockExited`
// (the `surface.exited` acks this page sent), `window.__mock` (`state`, `setState(fn)`,
// `emit(event, payload)`, `dispatched()`, `reset()`, `setSuggestDelay(ms)`).

import { deriveThemeColors } from './mock-colors.js';
import { SIDEBAR_MAX_WIDTH, SIDEBAR_MIN_WIDTH, allowedFromUi, asTab, locate, recompute, reducers, refreshMotion, searchUrl, validateCommand } from './mock-reducers.js';
import { animationSettings, applyAnimationsPatch } from './motion-catalog.js';
import { activeSpace, allTabs, clamp, deepEqual, findItem } from './util.js';
import { agentRequests, applyAgentParams } from './mock-agent.js';

const params = new URLSearchParams(location.search);
const flag = (name) => params.has(name) && !['0', 'false', 'no'].includes(params.get(name));
const quiet = flag('quiet');
const latency = Math.max(0, Number(params.get('latency')) || 0);

/** Unix ms the fixture timestamps were authored against (2026-09-16T12:00:00Z). */
const FIXTURE_NOW = 1789560000000;

// ------------------------------------------------------------------------------------ fixtures

async function loadFixture(name) {
  if (!/^[\w-]+$/.test(name)) throw new Error(`[mock] invalid fixture name "${name}"`);
  const url = new URL(`./fixtures/${name}.json`, import.meta.url);
  const res = await fetch(url);
  if (!res.ok) throw new Error(`[mock] fixture ${url} → HTTP ${res.status}`);
  return res.json();
}

/** Shift `*At` timestamps so fixture data is "recent" relative to now. */
function rebaseTimes(value, delta) {
  if (Array.isArray(value)) value.forEach((v) => rebaseTimes(v, delta));
  else if (value && typeof value === 'object') {
    for (const [k, v] of Object.entries(value)) {
      if (typeof v === 'number' && k.endsWith('At') && v > 1e12) value[k] = v + delta;
      else if (v && typeof v === 'object') rebaseTimes(v, delta);
    }
  }
  return value;
}

const wantDark = params.has('dark') ? flag('dark') : null;
const baseName = params.get('fixture') ?? (wantDark ? 'uiStateDark' : 'uiState');
const [base, light, darkFixture, omnibox, omniboxEmpty, omniboxActions, archive, history, boost, appInfo] = await Promise.all(
  [baseName, 'uiState', 'uiStateDark', 'omnibox', 'omniboxEmpty', 'omniboxActions', 'archive', 'history', 'boost', 'appInfo'].map(loadFixture),
);
const timeDelta = Date.now() - FIXTURE_NOW;
const fixtures = rebaseTimes(
  { base, light, dark: darkFixture, omnibox, omniboxEmpty, omniboxActions, archive, history, boost, appInfo },
  timeDelta,
);

const clone = (v) => structuredClone(v);

/**
 * ThemeColors for a theme: exact fixture values when a fixture space/preset has the same theme,
 * otherwise the mock approximation of core's derivation.
 */
function colorsFor(theme, dark) {
  const source = dark ? fixtures.dark : fixtures.light;
  const hit = [...source.spaces, ...source.themePresets].find((x) => deepEqual(x.theme, theme));
  return clone(hit?.colors ?? deriveThemeColors(theme, dark));
}

// ------------------------------------------------------------------------------------ state

/** @type {any} */
let ctx;

function createContext() {
  let state = clone(fixtures.base);
  let maxId = 0;
  const scanIds = (v) => {
    if (Array.isArray(v)) v.forEach(scanIds);
    else if (v && typeof v === 'object') {
      if (typeof v.id === 'number' && v.id < 1e9) maxId = Math.max(maxId, v.id);
      Object.values(v).forEach(scanIds);
    }
  };
  scanIds([state, fixtures.archive]);
  let seq = 100;
  let toastId = 1;

  const c = {
    get state() {
      return state;
    },
    set state(s) {
      state = s;
    },
    archive: clone(fixtures.archive),
    history: clone(fixtures.history),
    boosts: new Map([[fixtures.boost.id, clone(fixtures.boost)]]),
    currentExtras: new Map(state.current ? [[state.current.tab, clone(state.current)]] : []),
    findText: new Map(),
    findActive: 0,
    /** The sidebar is visible only for the open panel (see `openSidebarPanel`). */
    sidebarRevealed: false,
    nextId: () => ++maxId,
    nextSeq: () => ++seq,
    nextToastId: () => toastId++,
    colorsFor,
    emit,
    /** Apply a command without committing (used by commitOmnibox). */
    apply(command) {
      if (Object.hasOwn(reducers, command.type)) reducers[command.type](c, command);
      else console.warn(`[mock] command "${command.type}" is valid but not simulated (accepted as a no-op)`);
    },
    /** Run `fn(state)` after `ms`, then push the new state unless `commit` is false. */
    later(ms, fn, commit = true) {
      setTimeout(() => {
        fn(state);
        if (commit) commitState();
      }, ms);
    },
  };
  return c;
}

/** Apply URL-parameter scenarios to the initial state. */
function applyParams(c) {
  const s = c.state;
  if (wantDark !== null && s.dark !== wantDark) {
    s.dark = wantDark;
    for (const space of s.spaces) space.colors = colorsFor(space.theme, wantDark);
    for (const preset of s.themePresets) preset.colors = colorsFor(preset.theme, wantDark);
  }
  if (params.has('space')) {
    const id = Number(params.get('space'));
    if (s.spaces.some((sp) => sp.id === id)) s.activeSpace = id;
  }
  if (flag('empty')) activeSpace(s).activeItem = null;
  if (params.has('sidebar')) s.window.sidebarVisible = flag('sidebar');
  if (params.has('width')) s.window.sidebarWidth = clamp(Number(params.get('width')) || 248, SIDEBAR_MIN_WIDTH, SIDEBAR_MAX_WIDTH);
  if (flag('maximized')) s.window.maximized = true;
  recompute(s, c);

  const text = params.get('text') ?? '';
  const mode = params.get('commandBar');
  if (mode) {
    s.commandBar = { mode, text: text || (mode === 'editUrl' ? (s.current?.url ?? '') : ''), splitSide: mode === 'split' ? 'right' : null, seq: c.nextSeq() };
  }
  const panel = params.get('panel');
  if (panel) {
    const [type, arg] = panel.split(':');
    const id = Number(arg);
    const p = { type };
    if (type === 'editSpace') p.id = Number.isFinite(id) && arg ? id : s.activeSpace;
    if (type === 'renameItem' || type === 'editPinned') p.id = Number.isFinite(id) && arg ? id : (s.focusedTab ?? 0);
    s.sidebarPanel = { panel: p, seq: c.nextSeq() };
    // sidebar=0&panel=…: revealed for the panel, like core (docked or floating: `recompute`).
    if (!s.window.sidebarVisible) c.sidebarRevealed = true;
  }
  if (flag('find') && s.focusedTab != null) {
    s.find = { tab: s.focusedTab, text, matchCase: false, seq: c.nextSeq() };
    if (text) c.findText.set(s.focusedTab, text);
  }
  const toastMessage = params.get('toast');
  if (toastMessage) {
    const label = params.get('toastAction');
    s.toast = {
      id: c.nextToastId(),
      message: toastMessage === '1' ? 'Cleared 4 tabs' : toastMessage,
      action: label ? { label, command: { type: 'reopenClosed' } } : null,
      durationMs: label ? 6000 : 2500,
    };
  }
  const permission = params.get('permission');
  if (permission) {
    const kinds = permission === '1' ? ['camera', 'microphone'] : permission.split(',');
    s.permissionPrompts = [{ id: 1, tab: s.focusedTab ?? 0, origin: 'https://meet.example.com', host: 'meet.example.com', kinds }];
  }
  if (flag('switcher')) {
    const tabs = allTabs(s).filter((t) => t.loaded).slice(0, 5).map((t) => asTab(clone(t)));
    s.switcher = { tabs, selected: Math.min(1, tabs.length - 1) };
  }
  const peek = params.get('peek');
  if (peek && peek !== '0') {
    s.peek = {
      tab: {
        id: c.nextId(),
        title: 'Sign in – Example Accounts',
        url: 'https://accounts.example.com/signin?continue=app',
        host: 'accounts.example.com',
        favicon: null,
        section: null,
        space: null,
        loaded: true,
        loading: false,
        audible: false,
        muted: false,
        crashed: false,
        failed: false,
        navigated: false,
        pinnedUrl: null,
        active: false,
        visible: false,
      },
      popup: peek === 'popup',
    };
  }
  const extScenario = params.get('extensions');
  if (extScenario === 'none') {
    s.extensions = { items: [], details: [], busy: null, popup: null, safeMode: false, needsOk: 0 };
  } else if (extScenario === 'safeMode') {
    s.extensions.safeMode = true;
  }
  const extPopup = params.get('extPopup');
  if (extPopup && extPopup !== '0') {
    const items = s.extensions?.items ?? [];
    const item = items.find((e) => e.id === extPopup) ?? items.find((e) => e.popup) ?? items[0];
    if (item) {
      s.extensions.popup = {
        id: item.id,
        name: item.shortName || item.name,
        // No `sta://` icon in mock mode (http there; the shell serves it inside sta).
        icon: '',
        tab: s.focusedTab ?? null,
        hasOptions: Boolean(item.options),
        failed: extPopup === 'failed',
        seq: c.nextSeq(),
      };
    }
  }
  applyAgentParams(s, params);

  // Motion (`UiState.motion`): `?motion=off|reduced|full` sets the level through the settings the
  // real patch would write, and `?systemAnimations=0` stands in for Windows having animation
  // effects switched off. `?animOff=<key>,<key>` turns individual keys off.
  const wantLevel = params.get('motion');
  if (params.has('systemAnimations')) s.motion = { ...(s.motion ?? {}), systemAnimations: flag('systemAnimations') };
  if (wantLevel === 'off') s.settings.animations = { ...animationSettings(s.settings), enabled: false };
  else if (wantLevel === 'reduced') {
    s.settings.animations = { ...animationSettings(s.settings), enabled: true, followSystem: true };
    s.motion = { ...(s.motion ?? {}), systemAnimations: false };
  } else if (wantLevel === 'full') {
    s.settings.animations = { ...animationSettings(s.settings), enabled: true, followSystem: false };
  }
  const animOff = (params.get('animOff') ?? '').split(',').map((k) => k.trim()).filter(Boolean);
  if (animOff.length) {
    const a = animationSettings(s.settings);
    applyAnimationsPatch(a, { set: Object.fromEntries(animOff.map((k) => [k, false])) });
    s.settings.animations = a;
  }
  refreshMotion(s);

  recompute(s, c);
}

// ------------------------------------------------------------------------------------ push events

/** Persistent `__subscribe` sinks. */
const subscribers = new Set();
let pushScheduled = false;

function emit(event, payload) {
  const raw = JSON.stringify({ event, payload });
  for (const onSuccess of subscribers) setTimeout(() => onSuccess(raw), latency);
}

/** Bump the revision and push one coalesced `state` event (like the shell's ≤30 Hz push). */
function commitState() {
  recompute(ctx.state, ctx);
  ctx.state.revision++;
  if (pushScheduled) return;
  pushScheduled = true;
  setTimeout(() => {
    pushScheduled = false;
    emit('state', ctx.state);
  }, 0);
}

// ------------------------------------------------------------------------------------ requests

const GROUP_ORDER = ['topHit', 'go', 'recentTabs', 'suggestedActions', 'tabs', 'actions', 'spaces', 'history', 'suggestions', 'archive'];
const GROUP_CAPS = { tabs: 4, actions: 3, spaces: 2, history: 4, suggestions: 4, archive: 2 };

/** Typed text core's `classify` navigates to (roughly): a scheme, localhost, or `host.tld[:port][/path]`. */
function isUrlInput(t) {
  return (
    (/^[a-z][a-z0-9+.-]*:\S*$/i.test(t) && !/^[^:/]+:\d+/.test(t)) ||
    /^localhost(:\d+)?([/?#].*)?$/i.test(t) ||
    (!/\s/.test(t) && /^[^\s/?#@]+\.[a-z]{2,}(:\d{1,5})?([/?#].*)?$/i.test(t))
  );
}

const engineName = (s) => s.searchEngines.find((e) => e.id === s.settings.searchEngine)?.name ?? 'the web';

/** Commands core builds to search a remote suggestion (store/omni.rs). */
function suggestionCommands(req, suggestion) {
  const url = searchUrl(ctx.state, suggestion);
  const command =
    req.mode === 'split'
      ? { type: 'splitOpenInput', text: '?' + suggestion, side: req.splitSide ?? 'right' }
      : { type: 'openUrl', url, target: req.mode === 'editUrl' ? 'currentTab' : 'newTab', opener: null };
  return { command, altCommand: { type: 'openUrl', url, target: 'backgroundTab', opener: null } };
}

function whatYouTyped(req, text, { completedSuggestion = null } = {}) {
  const s = ctx.state;
  if (completedSuggestion != null) {
    // The inline completion comes from a suggestion: the default row searches it.
    return {
      key: 'search',
      group: 'go',
      title: completedSuggestion,
      subtitle: 'Search ' + engineName(s),
      icon: { type: 'glyph', name: 'search' },
      hint: '↵',
      ...suggestionCommands(req, completedSuggestion),
    };
  }
  const t = text.trim();
  const isUrl = isUrlInput(t);
  const engine = engineName(s);
  const split = req.mode === 'split';
  return {
    key: isUrl ? 'go' : 'search',
    group: 'go',
    title: t,
    subtitle: isUrl ? null : `Search ${engine}`,
    icon: { type: 'glyph', name: isUrl ? 'globe' : 'search' },
    hint: '↵',
    command: split
      ? { type: 'splitOpenInput', text: t, side: req.splitSide ?? 'right' }
      : { type: 'openInput', text: t, target: req.mode === 'editUrl' ? 'currentTab' : 'newTab' },
    // Like core: Alt+Enter opens a background tab in every mode.
    altCommand: { type: 'openInput', text: t, target: 'backgroundTab' },
  };
}

/** Core's `content_focused_tab`: the active item's tab or focused split pane (Peek is ignored). */
function contentFocusedTab(s) {
  const loc = locate(s, s.activeItem);
  if (!loc) return null;
  if (loc.node.kind === 'split') return loc.node.panes[loc.node.focused]?.id ?? null;
  return loc.node.kind === 'folder' ? null : loc.node.id;
}

/** A tab row's command and hint for the mode (core's `tab_result`). */
function tabRowFor(row, id, mode, focused, side, loaded) {
  const split = mode === 'split';
  return {
    ...row,
    hint: split ? 'Split' : loaded ? 'Switch to Tab' : 'Open',
    command: split && focused != null && focused !== id ? { type: 'splitWith', tab: id, with: focused, side } : { type: 'activateItem', id },
    altCommand: null,
  };
}

/**
 * Fixture history/suggestion rows were generated for New Tab mode: give them the commands core
 * builds for `mode` (`url_command` and the suggestion commands in `store/omni.rs`).
 */
function retargetPageRow(row, mode, side) {
  const url = row.command?.url;
  if (typeof url !== 'string' || mode === 'newTab' || mode === 'actions') return row;
  const suggestion = row.group === 'suggestions';
  const command =
    mode === 'split'
      ? { type: 'splitOpenInput', text: suggestion ? `?${row.title}` : url, side }
      : { type: 'openUrl', url, target: 'currentTab', opener: null };
  return { ...row, command, altCommand: { type: 'openUrl', url, target: 'backgroundTab', opener: null } };
}

/** Per-space action rows core derives from the live state (fixture rows reflect the fixture's active space). */
function spaceActions(s) {
  const rows = [];
  const favorite = s.favorites.some((t) => t.id === s.activeItem);
  s.spaces.forEach((sp, i) => {
    if (sp.id === s.activeSpace) return;
    rows.push({
      key: `action:space.goto:${sp.id}`,
      group: 'actions',
      title: `Go to Space: ${sp.name}`,
      subtitle: null,
      icon: { type: 'emoji', emoji: sp.icon },
      hint: i < 9 ? `Alt+${i + 1}` : null,
      command: { type: 'switchSpace', id: sp.id },
      altCommand: null,
    });
    if (s.activeItem != null && !favorite) {
      rows.push({
        key: `action:tab.move_to_space:${sp.id}`,
        group: 'actions',
        title: `Move Tab to Space: ${sp.name}`,
        subtitle: null,
        icon: { type: 'emoji', emoji: sp.icon },
        hint: null,
        command: { type: 'moveToSpace', id: s.activeItem, space: sp.id },
        altCommand: null,
      });
    }
  });
  return rows;
}

const isSpaceAction = (r) => r.key.startsWith('action:space.goto:') || r.key.startsWith('action:tab.move_to_space:');
const byTitle = (a, b) => (a.title.toLowerCase() < b.title.toLowerCase() ? -1 : a.title.toLowerCase() > b.title.toLowerCase() ? 1 : 0);

/**
 * Every command bar action for the current mock state, A–Z like `Store::omnibox_actions`: the
 * fixture rows, with the per-space "Go to Space: X" / "Move Tab to Space: X" rows rebuilt from the
 * live state. Answers `omnibox.actions` and feeds actions mode of `omnibox.query`.
 */
function allActions() {
  return [...fixtures.omniboxActions.results.filter((r) => !isSpaceAction(r)), ...spaceActions(ctx.state)].sort(byTitle);
}

/**
 * Extensions mode (Ctrl+E), like `store/omni.rs::extensions_query`: groups for the empty query,
 * name matching otherwise, and the two "More" rows last. The jamo retry is core's; the mock matches
 * on the text as typed.
 */
function extensionRows(query) {
  const items = ctx.state.extensions?.items ?? [];
  const q = query.toLowerCase();
  const groupOf = (e) => (e.state === 'enabled' ? 'extensions' : e.state === 'needsApproval' ? 'needsOk' : 'extensionsOff');
  // Mirrors `store/omni.rs::extension_result`: under the picker's own "Needs your OK" heading the row
  // does not repeat it, and the row whose Enter leaves sta says so before it is pressed.
  const blocked = {
    policy: 'Turned off by your organization',
    unsupported: 'Not supported by this version of Chrome',
    damaged: 'This extension looks damaged',
    safety: 'Chrome turned this off for safety',
    requirement: "This extension needs something sta doesn't have",
    custodian: "Needs a parent's approval",
  };
  const status = (e) =>
    e.state === 'needsApproval'
      ? 'Added by another program'
      : e.state === 'off'
        ? 'Off'
        : e.state === 'blocked'
          ? (blocked[e.blocked] ?? 'Chrome turned this off')
          : !e.popup && !e.options
            ? "Toolbar click isn't supported in sta · ↵ opens its Web Store page"
            : null;
  const rows = [];
  for (const group of ['extensions', 'needsOk', 'extensionsOff']) {
    for (const e of items.filter((x) => groupOf(x) === group)) {
      const name = e.shortName || e.name;
      if (q && !name.toLowerCase().includes(q)) continue;
      rows.push({
        key: `ext:${e.id}`,
        group,
        title: name,
        subtitle: status(e),
        // No `sta://` icon in mock mode: the page is on http there, where it is a CSP violation (the
        // real shell serves it from the extension's own directory).
        icon: { type: 'favicon', url: null, host: name },
        hint: '↵',
        command: { type: 'runExtension', id: e.id, action: 'primary' },
        altCommand: e.options ? { type: 'runExtension', id: e.id, action: 'options' } : null,
      });
    }
  }
  const more = [
    {
      key: 'ext.manage',
      group: 'more',
      title: 'Manage Extensions',
      subtitle: null,
      icon: { type: 'glyph', name: 'settings' },
      hint: '↵',
      command: { type: 'openUrl', url: 'sta://settings/?section=extensions', target: 'newTab', opener: null },
      altCommand: null,
    },
    {
      key: 'ext.get',
      group: 'more',
      title: 'Get Extensions',
      subtitle: 'Chrome Web Store',
      icon: { type: 'glyph', name: 'plus' },
      hint: '↵',
      command: { type: 'openUrl', url: 'https://chromewebstore.google.com/', target: 'newTab', opener: null },
      altCommand: null,
    },
  ];
  rows.push(...more.filter((r) => !q || r.title.toLowerCase().includes(q)));
  return rows;
}

function omniboxQuery(req) {
  const s = ctx.state;
  const text = String(req?.text ?? '');
  const seq = Number(req?.seq) || 0;
  const mode = req?.mode ?? 'newTab';
  const trimmed = text.trim();
  if (mode === 'extensions' && !trimmed.startsWith('>')) {
    return { text, seq, inlineCompletion: null, results: clone(extensionRows(trimmed)) };
  }
  const actionsOnly = mode === 'actions' || trimmed.startsWith('>');
  const q = trimmed.replace(/^>\s*/, '').toLowerCase();
  const matches = (r) => !q || r.title.toLowerCase().includes(q) || (r.subtitle ?? '').toLowerCase().includes(q);
  const actions = allActions();
  let inlineCompletion = null;
  let results;

  const split = mode === 'split';
  const side = req?.splitSide ?? 'right';
  const focused = contentFocusedTab(s);
  if (actionsOnly) {
    results = actions.filter(matches);
  } else if (!q) {
    // Recent tabs (never the focused one), then suggested actions; split mode lists the recent
    // tabs only, as split commands.
    results = clone(fixtures.omniboxEmpty.results)
      .filter((r) => !(r.group === 'recentTabs' && r.key === `tab:${focused}`) && !(split && r.group !== 'recentTabs'))
      .map((r) => {
        if (r.group !== 'recentTabs') return r;
        const id = Number(r.key.slice('tab:'.length));
        return tabRowFor(r, id, mode, focused, side, findItem(s, id)?.loaded ?? r.hint === 'Switch to Tab');
      });
    if (mode === 'editUrl' && s.current) {
      const copy = fixtures.omniboxActions.results.find((r) => r.key === 'action:tab.copy_url');
      if (copy) results.unshift({ ...copy, group: 'suggestedActions' });
    }
  } else {
    // Live tabs from the mock state (not the focused one), then actions (not "Go to Space", which
    // core lists in actions mode only), spaces, fixture history/archive and the request's
    // remote suggestions.
    const tabs = allTabs(s)
      .filter((t) => t !== s.peek?.tab && t.id !== focused)
      .map((t) =>
        tabRowFor(
          { key: `tab:${t.id}`, group: 'tabs', title: t.title, subtitle: t.host, icon: { type: 'favicon', url: t.favicon, host: t.host } },
          t.id,
          mode,
          focused,
          side,
          t.loaded,
        ),
      );
    const spaces = s.spaces
      .filter((sp) => sp.id !== s.activeSpace)
      .map((sp) => ({
        key: `space:${sp.id}`,
        group: 'spaces',
        title: sp.name,
        subtitle: 'Space',
        icon: { type: 'emoji', emoji: sp.icon },
        hint: 'Go to Space',
        command: { type: 'switchSpace', id: sp.id },
        altCommand: null,
      }));
    const pool = [
      ...tabs,
      ...(split ? [] : actions.filter((r) => !r.key.startsWith('action:space.goto:'))),
      ...(split ? [] : spaces),
      ...fixtures.omnibox.results
        .filter((r) => r.group === 'history' || (!split && r.group === 'archive'))
        .map((r) => retargetPageRow(r, mode, side)),
    ].filter(matches);
    // Inline completion like core: only for the exact input (no surrounding or inner whitespace,
    // not a `?` search or a URL with a scheme); the typed part keeps the user's case.
    let typed = trimmed;
    if (!req?.preventInlineAutocomplete && text === trimmed && !/\s/.test(trimmed) && !trimmed.startsWith('?') && !trimmed.includes('://')) {
      const hosts = [...tabs.map((r) => r.subtitle), ...fixtures.history.map((h) => h.host)];
      const host = hosts.find((h) => h && h.toLowerCase().startsWith(q) && h.length > q.length);
      if (host) {
        inlineCompletion = trimmed + host.slice(trimmed.length);
        typed = inlineCompletion;
      }
    }

    // Remote suggestions come only from the request (the command bar fetches them with
    // omnibox.suggest). Without a host completion, the first one that extends the text as typed
    // (spaces allowed, case-insensitive per character, not an address like "naver.com") completes
    // it: the typed part keeps the user's case and the Enter row searches the completed text.
    const requested = (Array.isArray(req?.suggestions) ? req.suggestions : []).filter((x) => typeof x === 'string').map((x) => x.trim());
    let completedSuggestion = null;
    if (!inlineCompletion && !req?.preventInlineAutocomplete && trimmed && !trimmed.startsWith('?') && !isUrlInput(trimmed)) {
      const typedChars = [...text];
      const hit = requested.find((x) => {
        const chars = [...x];
        return chars.length > typedChars.length && !isUrlInput(x) && typedChars.every((c, i) => c === chars[i] || c.toLowerCase() === chars[i].toLowerCase());
      });
      if (hit) {
        inlineCompletion = text + [...hit].slice(typedChars.length).join('');
        completedSuggestion = inlineCompletion;
      }
    }
    // Suggestion rows like core: no empties, duplicates, the typed text or the completed suggestion
    // (already the Enter row).
    const seen = new Set([trimmed, completedSuggestion ?? ''].map((x) => x.toLowerCase()));
    pool.push(
      ...requested
        .filter((x) => x && !seen.has(x.toLowerCase()) && seen.add(x.toLowerCase()))
        .map((x) => ({
          key: `suggest:${x}`,
          group: 'suggestions',
          title: x,
          subtitle: null,
          icon: { type: 'glyph', name: 'search' },
          hint: 'Search',
          ...suggestionCommands(req ?? {}, x),
        })),
    );
    const counts = {};
    const capped = pool
      .sort((a, b) => GROUP_ORDER.indexOf(a.group) - GROUP_ORDER.indexOf(b.group))
      .filter((r) => (counts[r.group] = (counts[r.group] ?? 0) + 1) <= (GROUP_CAPS[r.group] ?? 12));
    results = [whatYouTyped(req ?? {}, typed, { completedSuggestion }), ...capped].slice(0, 12);
  }
  return { text, seq, inlineCompletion, results: clone(results) };
}

// ------------------------------------------------------------------------------------ search suggestions

/** Engines without a suggest endpoint (core's `omnibox::suggest_url` returns None). */
const ENGINES_WITHOUT_SUGGESTIONS = new Set(['kagi', 'perplexity', 'custom']);

/** Canned engine suggestions: those starting with the query, else generic ones built from it. */
const CANNED_SUGGESTIONS = ['rust programming language', 'rust tutorial', 'rust vs go', 'github copilot', 'git rebase interactive', 'gitignore template'];

let suggestDelay = 60;
/** The in-flight omnibox.suggest (one per caller; a newer request resolves it with no suggestions). */
let pendingSuggest = null;

/** Like the shell (suggest.rs `url_like`): addresses with an explicit scheme and Windows paths are never sent. */
const isExplicitUrl = (t) =>
  /^[a-z]:[\\/]/i.test(t) ||
  t.startsWith('\\\\') ||
  /^(https?|file|sta|about|data|view-source|chrome|chrome-error|devtools|chrome-devtools|javascript|blob|filesystem|mailto|tel|sms|ftp|wss?):/i.test(t) ||
  /^[a-z][a-z0-9+.-]+:\/\//i.test(t);

/** Deterministic fake engine reply, cleaned like core's parse_suggestions (trim, dedupe, not the query, max 8). */
function fakeSuggestions(query) {
  const lower = query.toLowerCase();
  const canned = CANNED_SUGGESTIONS.filter((x) => x.startsWith(lower));
  const base = query.trimEnd();
  const raw = canned.length ? canned : [base + ' tutorial', base + ' meaning', base + ' 뜻'];
  const seen = new Set([lower]);
  return raw
    .map((x) => x.trim())
    .filter((x) => x && !seen.has(x.toLowerCase()) && seen.add(x.toLowerCase()))
    .slice(0, 8);
}

/** omnibox.suggest {text} → {text, suggestions} after ~60 ms (PROTOCOL §2). */
function omniboxSuggest(payload) {
  if (!location.pathname.startsWith('/command/')) {
    throw Object.assign(new Error('omnibox.suggest is only available to the command bar'), { code: 403 });
  }
  if (typeof payload?.text !== 'string') throw Object.assign(new Error('omnibox.suggest: missing text'), { code: 400 });
  const text = payload.text;
  if (pendingSuggest) {
    clearTimeout(pendingSuggest.timer);
    pendingSuggest.resolve({ text: pendingSuggest.text, suggestions: [] });
    pendingSuggest = null;
  }
  const settings = ctx.state.settings;
  // A leading "?" (forced search) is stripped, and then any text is sent.
  const forced = text.trimStart().startsWith('?');
  // Trailing whitespace is kept (engines suggest the next word for "rust ").
  const query = forced ? text.trimStart().slice(1).trimStart() : text.trimStart();
  const skip = !settings.searchSuggestions || ENGINES_WITHOUT_SUGGESTIONS.has(settings.searchEngine) || !query || [...text].length > 256 || (!forced && isExplicitUrl(query));
  return new Promise((resolve) => {
    if (skip) {
      setTimeout(() => resolve({ text, suggestions: [] }), 0);
      return;
    }
    const entry = { text, resolve, timer: 0 };
    entry.timer = setTimeout(() => {
      if (pendingSuggest === entry) pendingSuggest = null;
      resolve({ text, suggestions: fakeSuggestions(query) });
    }, suggestDelay);
    pendingSuggest = entry;
  });
}

/**
 * Request handlers: `(payload) => result` (result undefined/null → ""), or a Promise of it.
 * Throw (or reject) to fail.
 */
const handlers = {
  'state.get': () => ctx.state,
  'ui.ready': () => {
    window.__mockReady = true;
    return null;
  },
  dispatch: (command) => {
    // Like the shell: a command that doesn't parse (unknown type, missing or mistyped required
    // field) → 400; a shell-only command (also inside commitOmnibox) → 403.
    const invalid = validateCommand(command);
    if (invalid) throw Object.assign(new Error(`invalid command: ${invalid.message}`), { code: invalid.code });
    if (!allowedFromUi(command)) {
      throw Object.assign(new Error(`command "${command.type}" is not allowed from UI pages`), { code: 403 });
    }
    if (!quiet) console.info(`%c[mock] dispatch%c ${command.type}`, 'color:#0080ca;font-weight:600', 'font-weight:600', command);
    ctx.apply(command);
    commitState();
    return null;
  },
  'omnibox.query': omniboxQuery,
  'omnibox.actions': () => clone(allActions()),
  'omnibox.suggest': omniboxSuggest,
  'archive.list': () => ctx.archive,
  'history.list': (payload) => {
    const q = String(payload?.query ?? '').trim().toLowerCase();
    const limit = Number(payload?.limit) || 100;
    return ctx.history.filter((h) => !q || h.title.toLowerCase().includes(q) || h.url.toLowerCase().includes(q)).slice(0, limit);
  },
  'boosts.get': (payload) => {
    const id = payload?.id;
    const full = ctx.boosts.get(id);
    if (full) return full;
    const summary = ctx.state.boosts.find((b) => b.id === id);
    return summary ? { ...summary, css: '', js: '', createdAt: Date.now(), updatedAt: Date.now() } : null;
  },
  'theme.colors': (payload) => colorsFor(payload?.theme ?? {}, ctx.state.dark),
  'surface.setSize': (payload) => {
    window.__mockSurfaceSize = payload;
    return null;
  },
  // The ack of an acknowledged exit (PROTOCOL §14). The mock has no widget to hide, so it only
  // records the generation: `__mock.exited` is what `tools/motion-check.mjs` reads after emitting
  // `surface.exit` (or `sidebar.hover {gen}`) to check that the page really answered.
  'surface.exited': (payload) => {
    window.__mockExited.push({ gen: payload?.gen ?? null, time: Date.now() });
    return null;
  },
  'sidebar.setWidth': () => null,
  'sidebar.hoverLock': () => null,
  'dialog.pickFolder': () => 'C:\\Users\\you\\Documents\\Downloads',
  'app.info': () => fixtures.appInfo,
  ...agentRequests(() => ctx.state),
};

let queryId = 0;

/**
 * Drop-in replacement for `window.__staQuery`.
 * @param {{request: string, persistent?: boolean, onSuccess: Function, onFailure: Function}} q
 * @returns {number} query id
 */
export function mockQuery({ request, persistent = false, onSuccess, onFailure } = {}) {
  if (typeof onSuccess !== 'function' || typeof onFailure !== 'function') {
    // The real cef-rs router silently drops such queries; be loud about it here instead.
    console.error('[mock] query dropped: both onSuccess and onFailure are required', request);
    return 0;
  }
  const id = ++queryId;
  let cmd;
  let payload;
  try {
    ({ cmd, payload = null } = JSON.parse(request));
  } catch {
    setTimeout(() => onFailure(400, 'malformed request JSON'), latency);
    return id;
  }
  window.__mockLog.push({ time: Date.now(), cmd, payload });

  if (cmd === '__subscribe') {
    if (!persistent) {
      setTimeout(() => onFailure(400, '__subscribe must be persistent'), latency);
      return id;
    }
    subscribers.add(onSuccess);
    // hover=1: the shell showed the floating sidebar.
    if (flag('hover')) setTimeout(() => onSuccess(JSON.stringify({ event: 'sidebar.hover', payload: { visible: true, dismiss: false } })), latency + 50);
    return id;
  }
  const handler = typeof cmd === 'string' && Object.hasOwn(handlers, cmd) ? handlers[cmd] : null;
  if (!handler) {
    console.warn(`[mock] unknown request "${cmd}"`);
    setTimeout(() => onFailure(-1, `unknown request "${cmd}"`), latency);
    return id;
  }
  if (!quiet && cmd !== 'dispatch') console.debug(`[mock] ${cmd}`, payload);
  const fail = (e) => setTimeout(() => onFailure(e?.code ?? 500, e?.message ?? String(e)), latency);
  const succeed = (result) => {
    const response = result == null ? '' : JSON.stringify(result);
    setTimeout(() => onSuccess(response), latency);
  };
  let result;
  try {
    result = handler(payload);
  } catch (e) {
    fail(e);
    return id;
  }
  if (typeof result?.then === 'function') result.then(succeed, fail);
  else succeed(result);
  return id;
}

// ------------------------------------------------------------------------------------ init & test hooks

window.__mockLog = [];
/** `surface.exited` acks this page sent, oldest first (acknowledged exits, PROTOCOL §14). */
window.__mockExited = [];
ctx = createContext();
applyParams(ctx);

window.__mock = Object.freeze({
  /** The live mock UiState (mutate via `setState`). */
  get state() {
    return ctx.state;
  },
  /** Mutate the state (`fn(state)`), recompute derived fields and push it. */
  setState(fn) {
    fn(ctx.state);
    commitState();
  },
  /** Push an arbitrary event to subscribers, e.g. `emit('find.result', {...})`. */
  emit,
  /** Commands dispatched so far, oldest first. */
  dispatched: () => window.__mockLog.filter((e) => e.cmd === 'dispatch').map((e) => e.payload),
  /** Delay of omnibox.suggest replies in ms (default 60; latency= adds to it). */
  setSuggestDelay(ms) {
    suggestDelay = Math.max(0, Number(ms) || 0);
  },
  /** Restore the fixture state (keeps the log) and push it. */
  reset() {
    // Keep the revision monotonic, or ipc.js would drop the reset snapshot as stale.
    const revision = ctx.state.revision;
    ctx = createContext();
    applyParams(ctx);
    ctx.state.revision = revision;
    commitState();
  },
});

if (!quiet) {
  console.info(
    `%c[mock]%c sta UI mock mode · fixture "${baseName}"${wantDark ? ' (dark)' : ''} · window.__mock / window.__mockLog`,
    'color:#0080ca;font-weight:700',
    'color:inherit',
  );
}
