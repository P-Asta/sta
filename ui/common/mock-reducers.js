// Mock-mode command reducers: a small, forgiving imitation of core's `Store::apply` that mutates a
// UiState view model in place so pages can be clicked through without the shell. It is NOT the
// source of truth for semantics (see crates/sta-core/src/command.rs); it only needs to be
// plausible enough for UI development and screenshot tests.

import { activeSpace, allTabs, clamp, deepEqual, findItem, walkNodes } from './util.js';
import { agentReducers } from './mock-agent.js';
import { animationSettings, applyAnimationsPatch, motionLevel, motionOffKeys } from './motion-catalog.js';

export const SIDEBAR_MIN_WIDTH = 200;
export const SIDEBAR_MAX_WIDTH = 440;
const MAX_FAVORITES = 12;
const MAX_SPLIT_PANES = 4;
const ZOOM_LEVELS = [25, 33, 50, 67, 75, 80, 90, 100, 110, 125, 150, 175, 200, 250, 300, 400, 500];

/** Commands the real shell rejects when sent from UI pages (`Command::allowed_from_ui`). */
export const SHELL_ONLY_COMMANDS = new Set([
  'tabBrowserCreated', 'tabBrowserClosed', 'tabAddressChanged', 'tabTitleChanged', 'tabFaviconChanged',
  'tabLoadingStateChanged', 'tabLoadProgress', 'tabLoadFailed', 'tabAudioChanged', 'tabCrashed',
  'tabZoomChanged', 'tabFocused', 'popupAdopted', 'linkOpenRequested', 'tabFullscreenChanged',
  'permissionRequested', 'permissionDismissed', 'downloadUpdated', 'downloadInBlankTab', 'windowStateChanged',
  'systemThemeChanged', 'systemAnimationsChanged', 'windowCloseRequested', 'tick',
  // Chrome-created browsers (foreign.rs)
  'foreignTabRequested', 'extensionInstalled', 'foreignBlocked',
  // docked DevTools (devtools.rs)
  'devToolsClosed', 'devToolsUndockRequested', 'devToolsLinkRequested', 'inspectElement',
  // extensions (extensions.rs, ext_backend.rs, ext_popup.rs)
  'extensionsChanged', 'extensionDetailsLoaded', 'extensionOpFailed', 'extensionPopupClosed', 'safeModeStarted',
  'updateStatusChanged',
  // AI agents (MCP)
  'agentConnectionRequested', 'agentSessionStarted', 'agentSessionEnded', 'agentActivity', 'agentSiteRequested',
  'openAgentTab', 'loadTab', 'showAgentTab', 'agentDownloadHeld', 'agentTabAdopted', 'agentTabAccessRequested',
]);

// ------------------------------------------------------------------------------------ command validation

const OPEN_TARGETS = ['currentTab', 'newTab', 'backgroundTab'];
const SPLIT_SIDES = ['left', 'right', 'top', 'bottom'];

/**
 * Required fields of every UI command, derived from `crates/sta-core/src/command.rs`: the
 * fields that are neither `Option<_>` nor `#[serde(default)]`. serde rejects a command whose
 * required field is missing or has the wrong type (the shell answers 400); optional fields are
 * not checked here. Kinds: `id` (u64), `u32`, `usize`, `i32`, `string`, `bool`, `object`,
 * `numbers` (array of numbers), `dropTarget`, `panel` (SidebarPanel), `command`, or an array of
 * allowed enum strings.
 */
export const COMMAND_FIELDS = Object.freeze({
  // opening & navigation
  openInput: { text: 'string', target: OPEN_TARGETS },
  openUrl: { url: 'string', target: OPEN_TARGETS },
  openUrlAt: { url: 'string', to: 'dropTarget' },
  navigate: { url: 'string' },
  goBack: {},
  goForward: {},
  reload: {},
  stopLoad: {},
  // sidebar items
  activateItem: { id: 'id' },
  activateNth: { n: 'u32' },
  activateAdjacent: { delta: 'i32' },
  closeItem: {},
  reopenClosed: {},
  togglePin: {},
  addFavorite: {},
  removeFavorite: { id: 'id' },
  resetToPinned: { id: 'id' },
  replacePinnedUrl: { id: 'id' },
  editPinned: { id: 'id' },
  renameItem: { id: 'id' },
  duplicateTab: {},
  moveItem: { id: 'id', to: 'dropTarget' },
  moveToSpace: { space: 'id' },
  clearToday: {},
  newFolder: {},
  toggleFolder: { id: 'id' },
  deleteFolder: { id: 'id' },
  unloadTab: { id: 'id' },
  toggleMute: {},
  copyUrl: {},
  copyText: { text: 'string' },
  checkForUpdate: {},
  downloadUpdate: {},
  installUpdate: {},
  // spaces
  newSpace: { name: 'string', icon: 'string' },
  updateSpace: { id: 'id' },
  deleteSpace: { id: 'id' },
  switchSpace: { id: 'id' },
  switchSpaceNth: { n: 'u32' },
  switchSpaceAdjacent: { delta: 'i32' },
  moveSpace: { id: 'id', index: 'usize' },
  // split view
  splitWith: { tab: 'id', with: 'id', side: SPLIT_SIDES },
  splitOpenInput: { text: 'string', side: SPLIT_SIDES },
  focusPane: { index: 'usize' },
  focusPaneAdjacent: { delta: 'i32' },
  setSplitFractions: { id: 'id', fractions: 'numbers' },
  separatePane: {},
  separateAll: { id: 'id' },
  // archive & history
  restoreArchived: { id: 'id' },
  deleteArchived: { id: 'id' },
  clearArchive: {},
  deleteHistoryEntry: { url: 'string' },
  clearHistory: {},
  // peek
  closePeek: {},
  expandPeek: {},
  // command bar
  openCommandBar: { mode: ['newTab', 'editUrl', 'split', 'actions', 'extensions'] },
  closeCommandBar: {},
  commitOmnibox: { command: 'command' },
  // other surfaces & chrome
  toggleSidebar: {},
  setSidebarWidth: { width: 'u32' },
  openSidebarPanel: { panel: 'panel' },
  toggleSidebarPanel: { panel: 'panel' },
  closeSidebarPanel: {},
  openInternalPage: { page: ['settings', 'archive', 'history', 'boosts'] },
  openFind: {},
  closeFind: {},
  findInPage: { text: 'string' },
  findNext: {},
  zoom: { direction: ['in', 'out', 'reset'] },
  toggleDevTools: {},
  focusDevTools: {},
  undockDevTools: {},
  print: {},
  viewSource: {},
  newBoostForSite: {},
  // extensions (Ctrl+E)
  runExtension: { id: 'string' },
  requestExtensionDetails: { id: 'string' },
  setExtensionEnabled: { id: 'string', enabled: 'bool' },
  removeExtension: { id: 'string' },
  closeExtensionPopup: {},
  windowControl: { action: ['minimize', 'toggleMaximize', 'close', 'toggleFullscreen'] },
  quit: {},
  dismissToast: { id: 'id' },
  // settings & boosts
  updateSettings: { patch: 'object' },
  upsertBoost: { boost: 'object' },
  deleteBoost: { id: 'id' },
  toggleBoost: { id: 'id' },
  resolvePermission: { id: 'id', allow: 'bool' },
  // recent-tab switcher
  mruStep: { forward: 'bool' },
  mruSelect: { index: 'usize' },
  mruCommit: {},
  mruCancel: {},
  // downloads
  downloadControl: { id: 'u32', action: ['pause', 'resume', 'cancel', 'open', 'showInFolder', 'retry'] },
  downloadDismiss: { id: 'u32' },
  // AI agents (MCP)
  answerAgentConnection: { id: 'id', allow: 'bool' },
  answerSitePermission: { id: 'id', allow: 'bool' },
  stopAgents: {},
  resumeAgents: {},
  shareTabWithAgent: { tab: 'id' },
  resolveAgentDownload: { id: 'u32', keep: 'bool' },
  toggleAgentPanel: {},
  closeAgentPanel: {},
  archiveAgentTabs: {},
  answerTabAccess: { id: 'id', allow: 'bool' },
});

const isObject = (v) => v !== null && typeof v === 'object' && !Array.isArray(v);
const isUint = (v, max) => Number.isInteger(v) && v >= 0 && v <= max;

/** Why `value` doesn't deserialize as `kind`, or `null` when it does. */
function fieldError(kind, value) {
  if (Array.isArray(kind)) return kind.includes(value) ? null : `expected one of ${kind.join('|')}`;
  switch (kind) {
    case 'id':
    case 'usize':
      return isUint(value, Number.MAX_SAFE_INTEGER) ? null : 'expected a non-negative integer';
    case 'u32':
      return isUint(value, 0xffff_ffff) ? null : 'expected a u32';
    case 'i32':
      return Number.isInteger(value) && value >= -0x8000_0000 && value <= 0x7fff_ffff ? null : 'expected an i32';
    case 'string':
      return typeof value === 'string' ? null : 'expected a string';
    case 'bool':
      return typeof value === 'boolean' ? null : 'expected a boolean';
    case 'object':
      return isObject(value) ? null : 'expected an object';
    case 'numbers':
      return Array.isArray(value) && value.every((x) => typeof x === 'number') ? null : 'expected an array of numbers';
    case 'dropTarget': {
      if (!isObject(value) || !isObject(value.container)) return 'expected {container, before?}';
      const c = value.container;
      const need = { favorites: null, pinned: 'space', today: 'space', folder: 'id' };
      if (typeof c.type !== 'string' || !Object.hasOwn(need, c.type)) return 'unknown container type';
      return need[c.type] && !isUint(c[need[c.type]], Number.MAX_SAFE_INTEGER) ? `container.${need[c.type]} must be an id` : null;
    }
    case 'panel': {
      const need = { downloads: false, appMenu: false, newSpace: false, editSpace: true, renameItem: true, editPinned: true };
      if (!isObject(value) || typeof value.type !== 'string' || !Object.hasOwn(need, value.type)) return 'unknown sidebar panel';
      return need[value.type] && !isUint(value.id, Number.MAX_SAFE_INTEGER) ? 'panel.id must be an id' : null;
    }
    case 'command':
      return validateCommand(value)?.message ?? null;
    default:
      return `unknown field kind ${kind}`;
  }
}

/**
 * Validate a dispatched command like the shell's serde parse (`400` errors): known `type`
 * (own keys of `COMMAND_FIELDS` only, no prototype lookups), required fields present with the right
 * JSON type. Whether the mock simulates the command doesn't matter here: a valid command without a
 * reducer is accepted (and only warned about when applied), exactly like the shell accepts it.
 * `tools/check-mock-commands.mjs` keeps `COMMAND_FIELDS` in sync with `command.rs`.
 * @returns {{code: number, message: string} | null}
 */
export function validateCommand(command) {
  if (!isObject(command) || typeof command.type !== 'string') return { code: 400, message: 'malformed command' };
  if (SHELL_ONLY_COMMANDS.has(command.type)) return null;
  if (!Object.hasOwn(COMMAND_FIELDS, command.type)) {
    return { code: 400, message: `unknown command type "${command.type}"` };
  }
  for (const [field, kind] of Object.entries(COMMAND_FIELDS[command.type])) {
    if (!Object.hasOwn(command, field)) return { code: 400, message: `${command.type}: missing field "${field}"` };
    const error = fieldError(kind, command[field]);
    if (error) return { code: 400, message: `${command.type}.${field}: ${error}` };
  }
  return null;
}

/** `Command::allowed_from_ui`: no shell events, and `commitOmnibox` only around an allowed, non-commit command. */
export function allowedFromUi(command) {
  if (SHELL_ONLY_COMMANDS.has(command.type)) return false;
  if (command.type === 'commitOmnibox') return command.command.type !== 'commitOmnibox' && allowedFromUi(command.command);
  return true;
}

// ------------------------------------------------------------------------------------ motion

/**
 * Recompute `state.motion` from `state.settings.animations`, exactly as `Store::motion_view` does.
 * `systemAnimations` is runtime state the shell reports (`SPI_GETCLIENTAREAANIMATION`); the mock
 * keeps whatever is already there, so a test can set it with `__mock.setState`.
 */
export function refreshMotion(state) {
  const a = animationSettings(state.settings);
  const systemAnimations = state.motion?.systemAnimations !== false;
  state.motion = { level: motionLevel(a, systemAnimations), off: motionOffKeys(a), systemAnimations };
  return state.motion;
}

// ------------------------------------------------------------------------------------ structure helpers

/** `{kind:'tab', ...tab}` for space lists; favorites and split panes hold bare TabViews. */
const asNode = (tab) => (tab.kind ? tab : { kind: 'tab', ...tab });
export const asTab = (node) => {
  if (!node.kind) return node;
  const { kind: _kind, ...tab } = node;
  return tab;
};

/**
 * Where an item lives. For split panes `list` is `split.panes` and `split` is set.
 * @returns {{list: any[], index: number, node: any, space: any|null, section: string, folder: any|null, split: any|null} | null}
 */
export function locate(state, id) {
  if (id == null) return null;
  const favIndex = state.favorites.findIndex((t) => t.id === id);
  if (favIndex >= 0) {
    return { list: state.favorites, index: favIndex, node: state.favorites[favIndex], space: null, section: 'favorites', folder: null, split: null };
  }
  for (const space of state.spaces) {
    for (const section of ['pinned', 'today']) {
      const found = locateIn(space[section], id, space, section, null);
      if (found) return found;
    }
  }
  return null;
}

function locateIn(list, id, space, section, folder) {
  for (let i = 0; i < list.length; i++) {
    const node = list[i];
    if (node.id === id) return { list, index: i, node, space, section, folder, split: null };
    if (node.kind === 'folder') {
      const found = locateIn(node.children, id, space, section, node);
      if (found) return found;
    } else if (node.kind === 'split') {
      const p = node.panes.findIndex((t) => t.id === id);
      if (p >= 0) return { list: node.panes, index: p, node: node.panes[p], space, section, folder, split: node };
    }
  }
  return null;
}

function forEachTab(state, fn) {
  for (const tab of allTabs(state)) fn(tab);
}

/** Set section/space/pinnedUrl of a node (and its descendants) for its new container. */
function convertFor(node, section, spaceId) {
  const fixTab = (t) => {
    t.section = section;
    t.space = section === 'favorites' ? null : spaceId;
    if (section === 'today') {
      t.pinnedUrl = null;
      t.navigated = false;
    } else {
      t.pinnedUrl ??= t.url;
    }
  };
  if (node.kind === 'folder') {
    walkNodes(node.children, (n, ctx) => {
      if (n.kind === 'tab' || ctx.split) fixTab(n);
    });
    return node;
  }
  if (node.kind === 'split') {
    node.panes.forEach(fixTab);
    return node;
  }
  const moved = section === 'favorites' ? asTab(node) : asNode(node);
  fixTab(moved);
  return moved;
}

function countTabs(node) {
  if (node.kind === 'split') return node.panes.length;
  if (node.kind === 'folder') {
    let n = 0;
    walkNodes(node.children, (c) => {
      if (c.kind === 'tab') n++;
    });
    return n;
  }
  return 1;
}

function nextZoom(current, direction) {
  if (direction === 'reset') return 100;
  const i = ZOOM_LEVELS.findIndex((z) => z >= current);
  if (direction === 'in') return ZOOM_LEVELS[Math.min(ZOOM_LEVELS.length - 1, ZOOM_LEVELS[i] === current ? i + 1 : i)];
  return ZOOM_LEVELS[Math.max(0, i - 1)];
}

const hostMatches = (boostHost, host) => {
  const b = boostHost.replace(/^www\./, '').toLowerCase();
  const h = String(host).toLowerCase();
  return !!b && (h === b || h.endsWith(`.${b}`));
};

// ------------------------------------------------------------------------------------ derived fields

/**
 * Recompute everything core derives: `active`/`visible` flags, `activeItem`, `focusedTab`,
 * `current`, `favoritesFull`. Call after every mutation.
 * @param {any} state
 * @param {{currentExtras: Map<number, any>}} ctx
 */
export function recompute(state, ctx) {
  // A hidden sidebar is revealed while a panel is open (core: not persisted): docked for a panel
  // that holds input, floating (`sidebarVisible` stays false) for a transient one. It hides again
  // once no panel is open.
  if (ctx.sidebarRevealed) {
    if (!state.sidebarPanel) {
      ctx.sidebarRevealed = false;
      state.window.sidebarVisible = false;
    } else {
      state.window.sidebarVisible = !isTransientPanel(state.sidebarPanel.panel);
    }
  }
  // Drop active items that no longer exist in their space (favorites are valid everywhere).
  for (const space of state.spaces) {
    const loc = locate(state, space.activeItem);
    if (!loc || (loc.space && loc.space.id !== space.id) || loc.node.kind === 'folder') space.activeItem = null;
  }
  forEachTab(state, (t) => {
    t.active = false;
    t.visible = false;
  });
  for (const space of state.spaces) {
    for (const node of space.today) if (node.kind === 'split') node.active = false;
  }

  const space = activeSpace(state);
  state.activeSpace = space?.id ?? state.activeSpace;
  state.activeItem = space?.activeItem ?? null;

  let focused = null;
  let splitPanes = 0;
  const loc = locate(state, state.activeItem);
  if (loc?.node.kind === 'split') {
    const split = loc.node;
    split.active = true;
    split.focused = clamp(split.focused, 0, split.panes.length - 1);
    for (const pane of split.panes) {
      pane.visible = true;
      pane.loaded = true;
    }
    focused = split.panes[split.focused];
    focused.active = true;
    splitPanes = split.panes.length;
  } else if (loc) {
    loc.node.active = true;
    loc.node.visible = true;
    loc.node.loaded = true;
    focused = loc.node;
  }
  if (state.peek) {
    focused = state.peek.tab;
    splitPanes = 0;
  }
  state.focusedTab = focused?.id ?? null;
  state.current = focused ? currentView(state, ctx, focused, splitPanes) : null;
  state.favoritesFull = state.favorites.length >= MAX_FAVORITES;
  if (state.find && !findItem(state, state.find.tab)) state.find = null;
}

function currentView(state, ctx, tab, splitPanes) {
  const extra = ctx.currentExtras.get(tab.id) ?? {};
  const internal = /^sta:/i.test(tab.url);
  return {
    tab: tab.id,
    url: tab.url,
    title: tab.title,
    host: tab.host,
    pill: tab.host || tab.url,
    secure: internal || /^(https|file|about|data):/i.test(tab.url) || /^view-source:(https|sta|file):/i.test(tab.url),
    internal,
    loading: tab.loading,
    progress: tab.loading ? (extra.progress ?? 0.35) : 1,
    canGoBack: extra.canGoBack ?? tab.loaded,
    canGoForward: extra.canGoForward ?? false,
    section: tab.section,
    navigated: tab.navigated,
    zoomPercent: extra.zoomPercent ?? 100,
    boosts: state.boosts.filter((b) => hostMatches(b.host, tab.host)).map((b) => ({ ...b })),
    muted: tab.muted,
    audible: tab.audible,
    loadError: tab.failed ? 'net::ERR_NAME_NOT_RESOLVED' : null,
    splitPanes,
  };
}

// ------------------------------------------------------------------------------------ reducer helpers

function focusedId(state, id) {
  return id ?? state.focusedTab;
}

function newTabView(ctx, url, section, spaceId) {
  const host = hostForUrl(url);
  return {
    kind: 'tab',
    id: ctx.nextId(),
    title: internalName(url) ?? host ?? url,
    url,
    host: internalName(url) ?? host ?? url,
    favicon: null,
    section,
    space: spaceId,
    loaded: false,
    loading: false,
    audible: false,
    muted: false,
    crashed: false,
    failed: false,
    navigated: false,
    pinnedUrl: null,
    active: false,
    visible: false,
  };
}

function internalName(url) {
  const m = /^sta:\/\/([a-z]+)/i.exec(url);
  return m ? m[1][0].toUpperCase() + m[1].slice(1) : null;
}

function hostForUrl(url) {
  try {
    return new URL(url).hostname.replace(/^www\./, '') || null;
  } catch {
    return null;
  }
}

/** Mark a tab loaded and "loading" for a moment, like a real navigation. */
function simulateLoad(ctx, tab, url) {
  if (url) {
    tab.url = url;
    tab.host = internalName(url) ?? hostForUrl(url) ?? url;
    tab.title = tab.host;
  }
  tab.loaded = true;
  tab.loading = true;
  tab.failed = false;
  tab.crashed = false;
  const id = tab.id;
  ctx.later(700, (state) => {
    const t = findItem(state, id);
    if (t) t.loading = false;
  });
}

function activate(ctx, id) {
  const state = ctx.state;
  const loc = locate(state, id);
  if (!loc || loc.node.kind === 'folder') return;
  let target = id;
  if (loc.split) {
    loc.split.focused = loc.index;
    target = loc.split.id;
  }
  const space = loc.space ?? activeSpace(state);
  state.activeSpace = space.id;
  space.activeItem = target;
  state.peek = null;
  const tabs = loc.node.kind === 'split' ? loc.node.panes : loc.split ? loc.split.panes : [loc.node];
  for (const t of tabs) {
    if (!t.loaded) simulateLoad(ctx, t, t.pinnedUrl && !t.navigated ? t.pinnedUrl : undefined);
  }
}

/** Activate something sensible after the active item disappeared (core uses opener → MRU). */
function fallbackActivation(space) {
  const next = space.today.find((n) => n.kind === 'split' || n.loaded) ?? space.today[0] ?? null;
  space.activeItem = next?.id ?? null;
}

function archiveTabs(ctx, n, entries = []) {
  const state = ctx.state;
  state.archiveCount += n;
  state.archiveRevision++;
  state.canReopen = true;
  const now = Date.now();
  for (const t of entries) {
    ctx.archive.unshift({
      id: t.id,
      url: t.url,
      title: t.title,
      host: t.host,
      favicon: t.favicon,
      archivedAt: now,
      reason: 'userClosed',
      space: t.space,
      spaceIcon: state.spaces.find((s) => s.id === t.space)?.icon ?? null,
      spaceName: state.spaces.find((s) => s.id === t.space)?.name ?? null,
      group: null,
    });
  }
}

function removePane(state, loc) {
  const split = loc.split;
  split.panes.splice(loc.index, 1);
  split.fractions = split.panes.map(() => 1 / split.panes.length);
  split.focused = clamp(split.focused, 0, split.panes.length - 1);
  if (split.panes.length === 1) {
    const spaceList = loc.space.today;
    const i = spaceList.indexOf(split);
    const remaining = asNode(split.panes[0]);
    spaceList.splice(i, 1, remaining);
    if (loc.space.activeItem === split.id) loc.space.activeItem = remaining.id;
  }
}

function toast(ctx, message, action = null, durationMs = action ? 6000 : 2500) {
  ctx.state.toast = { id: ctx.nextToastId(), message, action, durationMs };
}

/** Core's `SidebarPanel::is_transient`: downloads and the app menu (no input). */
export function isTransientPanel(panel) {
  return panel?.type === 'downloads' || panel?.type === 'appMenu';
}

function openSidebarPanel(ctx, panel) {
  ctx.state.sidebarPanel = { panel, seq: ctx.nextSeq() };
  // Like core: a hidden sidebar is revealed only while the panel is open (`recompute` docks it for
  // panels that hold input, floats it for transient ones, and hides it again).
  if (!ctx.state.window.sidebarVisible) ctx.sidebarRevealed = true;
}

/** The search URL for `query` with the selected engine (core's `omnibox::search_url`, simplified). */
export function searchUrl(state, query) {
  const engine = state.searchEngines.find((e) => e.id === state.settings.searchEngine);
  const template = state.settings.searchEngine === 'custom' && state.settings.customSearchUrl.includes('{q}')
    ? state.settings.customSearchUrl
    : engine?.url || 'https://www.google.com/search?q={q}';
  return template.replace('{q}', encodeURIComponent(query));
}

function resolveInput(state, text) {
  const t = text.trim();
  if (t.startsWith('?')) return searchUrl(state, t.slice(1).trim());
  if (/^[a-z][a-z0-9+.-]*:\S*$/i.test(t) && !/^[^:]+:\d+/.test(t)) return t;
  if (/^localhost(:\d+)?([/?#].*)?$/i.test(t) || /^\d{1,3}(\.\d{1,3}){3}(:\d+)?([/?#].*)?$/.test(t)) return `http://${t}`;
  if (!/\s/.test(t) && /^[^\s/?#@]+\.[a-z]{2,}(:\d{1,5})?([/?#].*)?$/i.test(t)) return `https://${t}`;
  return searchUrl(state, t);
}

function openUrl(ctx, url, target, opener = null) {
  const state = ctx.state;
  const space = activeSpace(state);
  if (target === 'currentTab') {
    const tab = findItem(state, state.focusedTab);
    if (tab) {
      simulateLoad(ctx, tab, url);
      if (tab.pinnedUrl) tab.navigated = tab.url !== tab.pinnedUrl;
      return;
    }
  }
  if (/^sta:/i.test(url)) {
    const existing = allTabs(state).find((t) => t.url.split(/[?#]/)[0] === url.split(/[?#]/)[0]);
    if (existing) {
      activate(ctx, existing.id);
      return;
    }
  }
  const tab = newTabView(ctx, url, 'today', space.id);
  if (target === 'backgroundTab') {
    const openerIndex = space.today.findIndex((n) => n.id === opener);
    space.today.splice(openerIndex >= 0 ? openerIndex + 1 : 0, 0, tab);
    simulateLoad(ctx, tab);
    if (!state.window.sidebarVisible && !ctx.sidebarRevealed) toast(ctx, 'New tab opened');
  } else {
    space.today.unshift(tab);
    activate(ctx, tab.id);
  }
}

/** Sidebar order used by Ctrl+1..9 / Ctrl+PgUp/PgDn: favorites, visible pinned rows (children of collapsed folders skipped), today. */
function visualOrder(state) {
  const space = activeSpace(state);
  const order = state.favorites.map((t) => t.id);
  const visitPinned = (nodes) => {
    for (const node of nodes) {
      if (node.kind === 'tab') order.push(node.id);
      else if (node.kind === 'folder' && !node.collapsed) visitPinned(node.children);
    }
  };
  if (space) {
    visitPinned(space.pinned);
    for (const node of space.today) order.push(node.id);
  }
  return order;
}

function recolor(ctx) {
  const state = ctx.state;
  for (const space of state.spaces) space.colors = ctx.colorsFor(space.theme, state.dark);
  for (const preset of state.themePresets) preset.colors = ctx.colorsFor(preset.theme, state.dark);
}

function splitWith(ctx, tabId, withId, side) {
  const state = ctx.state;
  const withLoc = locate(state, withId);
  const tabLoc = locate(state, tabId);
  if (!withLoc || !tabLoc || tabId === withId) return;
  // Moving panes between splits, and folders/splits as the dragged item, are not simulated.
  if (tabLoc.split || (tabLoc.node.kind && tabLoc.node.kind !== 'tab')) return;
  const space = withLoc.space ?? activeSpace(state);
  const before = side === 'left' || side === 'top';
  // Only Today tabs move; pinned tabs/favorites are duplicated into Today (like core).
  let pane;
  if (tabLoc.section === 'today') {
    tabLoc.list.splice(tabLoc.index, 1);
    pane = asTab(tabLoc.node);
  } else {
    pane = { ...asTab(tabLoc.node), id: ctx.nextId(), section: 'today', space: space.id, pinnedUrl: null, navigated: false };
  }
  pane.space = space.id;
  if (withLoc.split) {
    const split = withLoc.split;
    if (split.panes.length >= MAX_SPLIT_PANES) {
      toast(ctx, 'A split view can have at most 4 tabs');
      return;
    }
    split.panes.splice(before ? withLoc.index : withLoc.index + 1, 0, pane);
    split.fractions = split.panes.map(() => 1 / split.panes.length);
    split.focused = split.panes.indexOf(pane);
    space.activeItem = split.id;
  } else {
    if (withLoc.section !== 'today') return; // splitting with a pinned tab is not simulated
    const withTab = asTab(withLoc.node);
    const split = {
      kind: 'split',
      id: ctx.nextId(),
      orientation: side === 'top' || side === 'bottom' ? 'vertical' : 'horizontal',
      panes: before ? [pane, withTab] : [withTab, pane],
      fractions: [0.5, 0.5],
      focused: before ? 0 : 1,
      active: false,
    };
    const i = space.today.findIndex((n) => n.id === withId);
    space.today.splice(i, 1, split);
    space.activeItem = split.id;
  }
  state.activeSpace = space.id;
  if (!pane.loaded) simulateLoad(ctx, pane);
}

// ------------------------------------------------------------------------------------ reducers

/**
 * Command reducers keyed by `type`. Each receives `(ctx, command)` and mutates `ctx.state`.
 * @type {Record<string, (ctx: any, cmd: any) => void>}
 */
export const reducers = {
  // ---------------------------------------------------------------- opening & navigation
  openInput: (ctx, { text, target }) => openUrl(ctx, resolveInput(ctx.state, String(text ?? '')), target),
  openUrl: (ctx, { url, target, opener }) => openUrl(ctx, url, target, opener),
  openUrlAt: (ctx, { url, to }) => {
    // Open in the background at Today's top, then move it where the link was dropped.
    const space = activeSpace(ctx.state);
    openUrl(ctx, url, 'backgroundTab');
    const tab = space.today.find((n) => n.url === url && n.kind === 'tab');
    if (tab && to?.container) reducers.moveItem(ctx, { id: tab.id, to });
  },
  navigate: (ctx, { tab, url }) => {
    const t = findItem(ctx.state, focusedId(ctx.state, tab));
    if (t && t.kind !== 'folder' && t.kind !== 'split') simulateLoad(ctx, t, url);
  },
  goBack: (ctx, { tab }) => reducers.reload(ctx, { tab }),
  goForward: (ctx, { tab }) => reducers.reload(ctx, { tab }),
  reload: (ctx, { tab }) => {
    const t = findItem(ctx.state, focusedId(ctx.state, tab));
    if (t?.loaded) simulateLoad(ctx, t);
  },
  stopLoad: (ctx, { tab }) => {
    const t = findItem(ctx.state, focusedId(ctx.state, tab));
    if (t) t.loading = false;
  },

  // ---------------------------------------------------------------- sidebar items
  activateItem: (ctx, { id }) => activate(ctx, id),
  activateNth: (ctx, { n }) => {
    const order = visualOrder(ctx.state);
    const id = n >= 9 ? order.at(-1) : order[n - 1];
    if (id != null) activate(ctx, id);
  },
  activateAdjacent: (ctx, { delta }) => {
    const order = visualOrder(ctx.state);
    if (!order.length || !delta) return;
    const current = order.indexOf(ctx.state.activeItem);
    const index = current < 0 ? (delta > 0 ? 0 : order.length - 1) : (current + Math.sign(delta) + order.length) % order.length;
    activate(ctx, order[index]);
  },
  closeItem: (ctx, { id }) => {
    const state = ctx.state;
    if (id == null && state.peek) {
      state.peek = null;
      return;
    }
    const targetId = focusedId(state, id);
    const loc = locate(state, targetId);
    if (!loc) {
      console.info('[mock] closeItem with nothing focused: the real app would close the window');
      return;
    }
    if (loc.node.kind === 'folder') return;
    const space = loc.space ?? activeSpace(state);
    if (loc.split) {
      archiveTabs(ctx, 1, [loc.node]);
      removePane(state, loc);
    } else if (loc.section === 'today') {
      loc.list.splice(loc.index, 1);
      archiveTabs(ctx, countTabs(loc.node), loc.node.kind === 'split' ? loc.node.panes : [loc.node]);
      if (space.activeItem === targetId) fallbackActivation(space);
    } else {
      // Pinned tab / favorite: unload, keep the row.
      Object.assign(loc.node, { loaded: false, loading: false, audible: false, navigated: false, url: loc.node.pinnedUrl ?? loc.node.url });
      state.canReopen = true;
      if (activeSpace(state).activeItem === targetId) fallbackActivation(activeSpace(state));
    }
  },
  reopenClosed: (ctx) => {
    const entry = ctx.archive[0];
    if (entry) reducers.restoreArchived(ctx, { id: entry.id });
  },
  togglePin: (ctx, { id }) => {
    const state = ctx.state;
    const loc = locate(state, focusedId(state, id));
    if (!loc || loc.split || (loc.node.kind && loc.node.kind !== 'tab')) return;
    loc.list.splice(loc.index, 1);
    const space = loc.space ?? activeSpace(state);
    if (loc.section === 'today') {
      space.pinned.push(convertFor(loc.node, 'pinned', space.id));
    } else {
      space.today.unshift(convertFor(loc.node, 'today', space.id));
    }
  },
  addFavorite: (ctx, { id }) => {
    const state = ctx.state;
    const loc = locate(state, focusedId(state, id));
    if (!loc || loc.split || loc.section === 'favorites' || loc.node.kind !== 'tab') return;
    if (state.favorites.length >= MAX_FAVORITES) {
      toast(ctx, 'Favorites are full');
      return;
    }
    loc.list.splice(loc.index, 1);
    state.favorites.push(convertFor(loc.node, 'favorites', null));
  },
  removeFavorite: (ctx, { id }) => {
    const state = ctx.state;
    const loc = locate(state, id);
    if (loc?.section !== 'favorites') return;
    loc.list.splice(loc.index, 1);
    const space = activeSpace(state);
    space.today.unshift(convertFor(loc.node, 'today', space.id));
  },
  resetToPinned: (ctx, { id }) => {
    const t = findItem(ctx.state, id);
    if (!t?.pinnedUrl) return;
    t.navigated = false;
    simulateLoad(ctx, t, t.pinnedUrl);
    activate(ctx, id);
  },
  replacePinnedUrl: (ctx, { id }) => {
    const t = findItem(ctx.state, id);
    if (t?.pinnedUrl) {
      t.pinnedUrl = t.url;
      t.navigated = false;
    }
  },
  editPinned: (ctx, { id, title, url }) => {
    // Core: saving closes the "Edit Pinned Page" panel of that tab.
    if (ctx.state.sidebarPanel?.panel.type === 'editPinned' && ctx.state.sidebarPanel.panel.id === id) ctx.state.sidebarPanel = null;
    const t = findItem(ctx.state, id);
    if (!t?.pinnedUrl) return;
    if (title != null) t.title = title || t.host;
    if (url) t.pinnedUrl = url;
    t.navigated = t.url !== t.pinnedUrl;
  },
  renameItem: (ctx, { id, title }) => {
    const item = findItem(ctx.state, id);
    if (!item) return;
    if (item.kind === 'folder') {
      if (title?.trim()) item.name = title.trim();
    } else if (item.kind !== 'split') {
      item.title = title?.trim() || item.host;
    }
    if (ctx.state.sidebarPanel?.panel.type === 'renameItem') ctx.state.sidebarPanel = null;
  },
  duplicateTab: (ctx, { id }) => {
    const state = ctx.state;
    const t = findItem(state, focusedId(state, id));
    if (!t || t.kind === 'folder' || t.kind === 'split') return;
    reducers.openUrl(ctx, { url: t.url, target: 'newTab' });
  },
  moveItem: (ctx, { id, to }) => {
    const state = ctx.state;
    const loc = locate(state, id);
    if (!loc || loc.split || !to?.container) return;
    const node = loc.node;
    const c = to.container;
    let target;
    if (c.type === 'favorites') target = { list: state.favorites, section: 'favorites', space: null };
    else if (c.type === 'pinned' || c.type === 'today') {
      const space = state.spaces.find((s) => s.id === c.space);
      if (space) target = { list: space[c.type], section: c.type, space };
    } else if (c.type === 'folder') {
      const f = locate(state, c.id);
      if (f?.node.kind === 'folder') target = { list: f.node.children, section: 'pinned', space: f.space, folder: f.node };
    }
    if (!target) return;
    if (node.kind === 'folder' && target.section !== 'pinned') return;
    if (node.kind === 'split' && target.section !== 'today') return;
    if (target.section === 'favorites' && node.kind && node.kind !== 'tab') return;
    if (target.section === 'favorites' && loc.section !== 'favorites' && state.favorites.length >= MAX_FAVORITES) return;
    if (node.kind === 'folder' && target.folder) {
      let inside = node.id === target.folder.id;
      walkNodes(node.children, (n) => {
        if (n.id === target.folder.id) inside = true;
      });
      if (inside) return;
    }
    loc.list.splice(loc.index, 1);
    const moved = convertFor(node, target.section, target.space?.id ?? null);
    let index = to.before == null ? -1 : target.list.findIndex((n) => n.id === to.before);
    if (index < 0) index = target.list.length;
    target.list.splice(index, 0, moved);
  },
  moveToSpace: (ctx, { id, space: spaceId }) => {
    const state = ctx.state;
    const loc = locate(state, focusedId(state, id));
    const space = state.spaces.find((s) => s.id === spaceId);
    if (!loc || !space || loc.split || loc.section === 'favorites' || loc.space === space) return;
    loc.list.splice(loc.index, 1);
    if (loc.section === 'today') space.today.unshift(convertFor(loc.node, 'today', space.id));
    else space.pinned.push(convertFor(loc.node, 'pinned', space.id));
    if (loc.space.activeItem === loc.node.id) fallbackActivation(loc.space);
  },
  clearToday: (ctx, { space: spaceId }) => {
    const state = ctx.state;
    const space = state.spaces.find((s) => s.id === spaceId) ?? activeSpace(state);
    const keep = (n) => (n.kind === 'split' ? n.active || n.panes.some((p) => p.audible) : n.visible || n.audible);
    const removed = space.today.filter((n) => !keep(n));
    if (!removed.length) return;
    space.today = space.today.filter(keep);
    archiveTabs(ctx, removed.reduce((sum, n) => sum + countTabs(n), 0), removed.flatMap((n) => (n.kind === 'split' ? n.panes : [n])));
    toast(ctx, `Cleared ${removed.length} tab${removed.length === 1 ? '' : 's'}`, { label: 'Undo', command: { type: 'reopenClosed' } });
  },
  newFolder: (ctx, { space: spaceId, parent, name }) => {
    const state = ctx.state;
    const folder = { kind: 'folder', id: ctx.nextId(), name: name || 'New Folder', collapsed: false, children: [] };
    const parentLoc = locate(state, parent);
    if (parentLoc?.node.kind === 'folder') parentLoc.node.children.unshift(folder);
    else (state.spaces.find((s) => s.id === spaceId) ?? activeSpace(state)).pinned.unshift(folder);
    openSidebarPanel(ctx, { type: 'renameItem', id: folder.id });
  },
  toggleFolder: (ctx, { id }) => {
    const f = findItem(ctx.state, id);
    if (f?.kind === 'folder') f.collapsed = !f.collapsed;
  },
  deleteFolder: (ctx, { id }) => {
    const loc = locate(ctx.state, id);
    if (loc?.node.kind !== 'folder') return;
    loc.list.splice(loc.index, 1);
    archiveTabs(ctx, countTabs(loc.node));
  },
  unloadTab: (ctx, { id }) => {
    const state = ctx.state;
    const t = findItem(state, id);
    if (!t || t.kind === 'folder' || t.kind === 'split') return;
    Object.assign(t, { loaded: false, loading: false, audible: false });
    if (t.visible) fallbackActivation(activeSpace(state));
  },
  toggleMute: (ctx, { id }) => {
    const t = findItem(ctx.state, focusedId(ctx.state, id));
    if (t && t.kind !== 'folder' && t.kind !== 'split') t.muted = !t.muted;
  },
  copyUrl: (ctx, { markdown }) => toast(ctx, markdown ? 'Copied URL as Markdown' : 'Copied URL'),
  copyText: (ctx) => toast(ctx, 'Copied'),
  // The shell owns updates; the mock only pretends far enough for Settings › About to be usable.
  checkForUpdate: (ctx) => {
    ctx.state.update = { stage: 'checking' };
  },
  downloadUpdate: (ctx) => {
    const version = ctx.state.update?.version ?? '0.0.0';
    ctx.state.update = { stage: 'downloading', version, received: 0, total: 0 };
  },
  installUpdate: (ctx) => toast(ctx, 'Restarting to update'),

  // ---------------------------------------------------------------- spaces
  newSpace: (ctx, { name, icon, theme }) => {
    const state = ctx.state;
    const t = theme ?? { hue: 300, hue2: 340, chroma: 0.06 };
    const space = { id: ctx.nextId(), name: name || 'Space', icon: icon || '✨', theme: t, colors: ctx.colorsFor(t, state.dark), pinned: [], today: [], activeItem: null };
    state.spaces.push(space);
    state.activeSpace = space.id;
    state.sidebarPanel = null;
  },
  updateSpace: (ctx, { id, name, icon, theme }) => {
    const space = ctx.state.spaces.find((s) => s.id === id);
    if (!space) return;
    if (name != null && name.trim()) space.name = name.trim();
    if (icon != null && icon.trim()) space.icon = icon.trim();
    if (theme) {
      space.theme = theme;
      space.colors = ctx.colorsFor(theme, ctx.state.dark);
    }
  },
  deleteSpace: (ctx, { id }) => {
    const state = ctx.state;
    if (state.spaces.length <= 1) return;
    const i = state.spaces.findIndex((s) => s.id === id);
    if (i < 0) return;
    const [space] = state.spaces.splice(i, 1);
    archiveTabs(ctx, [...space.pinned, ...space.today].reduce((sum, n) => sum + countTabs(n), 0));
    if (state.activeSpace === id) state.activeSpace = state.spaces[Math.max(0, i - 1)].id;
    state.sidebarPanel = null;
  },
  switchSpace: (ctx, { id }) => {
    const state = ctx.state;
    if (!state.spaces.some((s) => s.id === id)) return;
    state.activeSpace = id;
    state.peek = null;
    const space = activeSpace(state);
    if (space.activeItem != null) activate(ctx, space.activeItem);
  },
  switchSpaceNth: (ctx, { n }) => {
    const space = ctx.state.spaces[n - 1];
    if (space) reducers.switchSpace(ctx, { id: space.id });
  },
  switchSpaceAdjacent: (ctx, { delta }) => {
    const spaces = ctx.state.spaces;
    const i = spaces.findIndex((s) => s.id === ctx.state.activeSpace) + Math.sign(delta);
    if (i >= 0 && i < spaces.length) reducers.switchSpace(ctx, { id: spaces[i].id });
  },
  moveSpace: (ctx, { id, index }) => {
    const spaces = ctx.state.spaces;
    const i = spaces.findIndex((s) => s.id === id);
    if (i < 0) return;
    const [space] = spaces.splice(i, 1);
    spaces.splice(clamp(index, 0, spaces.length), 0, space);
  },

  // ---------------------------------------------------------------- split view
  splitWith: (ctx, { tab, with: withId, side }) => splitWith(ctx, tab, withId, side),
  splitOpenInput: (ctx, { text, side }) => {
    const state = ctx.state;
    const focused = state.focusedTab;
    if (focused == null) {
      reducers.openInput(ctx, { text, target: 'newTab' });
      return;
    }
    const space = activeSpace(state);
    const tab = newTabView(ctx, resolveInput(state, String(text ?? '')), 'today', space.id);
    space.today.unshift(tab);
    splitWith(ctx, tab.id, focused, side ?? 'right');
    if (locate(state, tab.id)?.split == null) activate(ctx, tab.id);
  },
  focusPane: (ctx, { index }) => {
    const loc = locate(ctx.state, ctx.state.activeItem);
    if (loc?.node.kind === 'split' && index < loc.node.panes.length) loc.node.focused = index;
  },
  focusPaneAdjacent: (ctx, { delta }) => {
    const loc = locate(ctx.state, ctx.state.activeItem);
    if (loc?.node.kind === 'split') loc.node.focused = clamp(loc.node.focused + Math.sign(delta), 0, loc.node.panes.length - 1);
  },
  setSplitFractions: (ctx, { id, fractions }) => {
    const split = findItem(ctx.state, id);
    if (split?.kind !== 'split' || !Array.isArray(fractions) || fractions.length !== split.panes.length) return;
    if (!fractions.every((f) => Number.isFinite(f) && f > 0)) return;
    const sum = fractions.reduce((a, b) => a + b, 0);
    split.fractions = fractions.map((f) => f / sum);
  },
  separatePane: (ctx, { tab }) => {
    const state = ctx.state;
    const loc = locate(state, focusedId(state, tab));
    if (!loc?.split) return;
    const pane = loc.node;
    removePane(state, loc);
    const list = loc.space.today;
    const splitIndex = list.findIndex((n) => n.id === loc.split.id || n.id === loc.split.panes[0]?.id);
    list.splice(splitIndex + 1, 0, asNode(pane));
  },
  separateAll: (ctx, { id }) => {
    const loc = locate(ctx.state, id);
    if (loc?.node.kind !== 'split') return;
    loc.list.splice(loc.index, 1, ...loc.node.panes.map(asNode));
    if (loc.space.activeItem === id) loc.space.activeItem = loc.node.panes[loc.node.focused].id;
  },

  // ---------------------------------------------------------------- archive & history
  restoreArchived: (ctx, { id }) => {
    const state = ctx.state;
    const i = ctx.archive.findIndex((e) => e.id === id);
    if (i < 0) return;
    const [entry] = ctx.archive.splice(i, 1);
    const space = state.spaces.find((s) => s.id === entry.space) ?? activeSpace(state);
    const tab = { ...newTabView(ctx, entry.url, 'today', space.id), id: entry.id, title: entry.title, host: entry.host, favicon: entry.favicon };
    space.today.unshift(tab);
    state.archiveCount = Math.max(0, state.archiveCount - 1);
    state.archiveRevision++;
    activate(ctx, tab.id);
  },
  deleteArchived: (ctx, { id }) => {
    const i = ctx.archive.findIndex((e) => e.id === id);
    if (i < 0) return;
    ctx.archive.splice(i, 1);
    ctx.state.archiveCount = Math.max(0, ctx.state.archiveCount - 1);
    ctx.state.archiveRevision++;
  },
  clearArchive: (ctx) => {
    ctx.archive.length = 0;
    ctx.state.archiveCount = 0;
    ctx.state.archiveRevision++;
  },
  deleteHistoryEntry: (ctx, { url }) => {
    const i = ctx.history.findIndex((h) => h.url === url);
    if (i >= 0) ctx.history.splice(i, 1);
    ctx.state.historyRevision++;
  },
  clearHistory: (ctx) => {
    ctx.history.length = 0;
    ctx.state.historyRevision++;
  },

  // ---------------------------------------------------------------- peek
  closePeek: (ctx, { focusLost }) => {
    if (ctx.state.peek && !(focusLost && ctx.state.peek.popup)) ctx.state.peek = null;
  },
  expandPeek: (ctx) => {
    const state = ctx.state;
    if (!state.peek) return;
    const space = activeSpace(state);
    const tab = convertFor({ ...state.peek.tab }, 'today', space.id);
    state.peek = null;
    space.today.unshift(tab);
    activate(ctx, tab.id);
  },

  // ---------------------------------------------------------------- command bar
  openCommandBar: (ctx, { mode, splitSide }) => {
    const state = ctx.state;
    const effective = mode === 'editUrl' && !state.current ? 'newTab' : mode;
    state.commandBar = {
      mode: effective,
      text: effective === 'editUrl' ? state.current.url : '',
      splitSide: effective === 'split' ? (splitSide ?? 'right') : null,
      seq: ctx.nextSeq(),
    };
  },
  closeCommandBar: (ctx, { seq }) => {
    // A stale close (the page asked to close the bar it was showing, but a newer one is open) is
    // ignored, exactly as `Command::CloseCommandBar` does (docs/PROTOCOL.md §8, §14).
    if (typeof seq === 'number' && ctx.state.commandBar && ctx.state.commandBar.seq !== seq) return;
    ctx.state.commandBar = null;
  },
  commitOmnibox: (ctx, { command, alt }) => {
    if (!command || typeof command.type !== 'string') return;
    const barBefore = ctx.state.commandBar;
    ctx.apply(command);
    const reopened = ctx.state.commandBar !== barBefore;
    const keepsBar = ['openCommandBar', 'openSidebarPanel', 'toggleSidebarPanel'].includes(command.type);
    if (!alt && !keepsBar && !reopened) ctx.state.commandBar = null;
  },

  // ---------------------------------------------------------------- chrome & surfaces
  toggleSidebar: (ctx) => {
    const w = ctx.state.window;
    if (w.sidebarVisible && !ctx.sidebarRevealed) {
      // Hiding the docked sidebar closes its panel.
      ctx.state.sidebarPanel = null;
      w.sidebarVisible = false;
    } else {
      // Docks a hidden one, also while it floats or is revealed for a panel (the panel stays).
      ctx.sidebarRevealed = false;
      w.sidebarVisible = true;
    }
  },
  setSidebarWidth: (ctx, { width }) => {
    if (Number.isFinite(width)) ctx.state.window.sidebarWidth = Math.round(clamp(width, SIDEBAR_MIN_WIDTH, SIDEBAR_MAX_WIDTH));
  },
  openSidebarPanel: (ctx, { panel }) => {
    if (panel?.type) openSidebarPanel(ctx, panel);
  },
  toggleSidebarPanel: (ctx, { panel }) => {
    if (!panel?.type) return;
    if (ctx.state.sidebarPanel && deepEqual(ctx.state.sidebarPanel.panel, panel)) ctx.state.sidebarPanel = null;
    else openSidebarPanel(ctx, panel);
  },
  closeSidebarPanel: (ctx) => {
    ctx.state.sidebarPanel = null;
  },
  openInternalPage: (ctx, { page }) => openUrl(ctx, `sta://${page}/`, 'newTab'),
  openFind: (ctx) => {
    const state = ctx.state;
    if (state.focusedTab == null) return;
    state.find = { tab: state.focusedTab, text: ctx.findText.get(state.focusedTab) ?? '', matchCase: false, seq: ctx.nextSeq() };
  },
  closeFind: (ctx) => {
    ctx.state.find = null;
  },
  findInPage: (ctx, { tab, text, matchCase }) => {
    const state = ctx.state;
    const id = tab ?? state.find?.tab ?? state.focusedTab;
    if (id == null) return;
    ctx.findText.set(id, text);
    if (state.find?.tab === id) Object.assign(state.find, { text, matchCase: !!matchCase });
    const count = text ? (text.length * 7) % 23 : 0;
    ctx.findActive = count ? 1 : 0;
    ctx.later(40, () => ctx.emit('find.result', { tab: id, count, active: ctx.findActive, final: true }), false);
  },
  findNext: (ctx, { forward = true }) => {
    const state = ctx.state;
    const id = state.find?.tab ?? state.focusedTab;
    const text = ctx.findText.get(id) ?? '';
    const count = text ? (text.length * 7) % 23 : 0;
    if (!count) return;
    ctx.findActive = ((ctx.findActive - 1 + (forward ? 1 : -1) + count) % count) + 1;
    ctx.later(20, () => ctx.emit('find.result', { tab: id, count, active: ctx.findActive, final: true }), false);
  },
  zoom: (ctx, { direction }) => {
    const id = ctx.state.focusedTab;
    if (id == null) return;
    const extra = ctx.currentExtras.get(id) ?? {};
    extra.zoomPercent = nextZoom(extra.zoomPercent ?? 100, direction);
    ctx.currentExtras.set(id, extra);
    if (extra.zoomPercent !== 100) toast(ctx, `Zoom ${extra.zoomPercent}%`);
  },
  windowControl: (ctx, { action }) => {
    const w = ctx.state.window;
    if (action === 'toggleMaximize') w.maximized = !w.maximized;
    else if (action === 'toggleFullscreen') w.fullscreen = !w.fullscreen;
    else console.info(`[mock] windowControl ${action}: not simulated`);
  },
  dismissToast: (ctx, { id }) => {
    if (ctx.state.toast?.id === id) ctx.state.toast = null;
  },

  // ---------------------------------------------------------------- settings & boosts
  updateSettings: (ctx, { patch }) => {
    const state = ctx.state;
    for (const [key, value] of Object.entries(patch ?? {})) {
      // `animations` is a patch of its own shape, not a value to assign (see below).
      if (key !== 'animations' && value != null && key in state.settings) state.settings[key] = value;
    }
    if (patch?.downloadDir === '') state.settings.downloadDir = null;
    if (patch?.animations) {
      state.settings.animations = applyAnimationsPatch(animationSettings(state.settings), patch.animations);
      refreshMotion(state);
    }
    if (patch?.appearance) {
      const dark =
        patch.appearance === 'dark' ||
        (patch.appearance === 'system' && matchMedia('(prefers-color-scheme: dark)').matches);
      if (dark !== state.dark) {
        state.dark = dark;
        recolor(ctx);
      }
    }
  },
  upsertBoost: (ctx, { boost }) => {
    const state = ctx.state;
    const full = { ...boost, id: boost.id || ctx.nextId(), updatedAt: Date.now() };
    ctx.boosts.set(full.id, full);
    const summary = { id: full.id, name: full.name, host: full.host, enabled: full.enabled };
    const i = state.boosts.findIndex((b) => b.id === full.id);
    if (i >= 0) state.boosts[i] = summary;
    else state.boosts.push(summary);
  },
  deleteBoost: (ctx, { id }) => {
    ctx.state.boosts = ctx.state.boosts.filter((b) => b.id !== id);
    ctx.boosts.delete(id);
  },
  toggleBoost: (ctx, { id }) => {
    const b = ctx.state.boosts.find((x) => x.id === id);
    if (!b) return;
    b.enabled = !b.enabled;
    const full = ctx.boosts.get(id);
    if (full) full.enabled = b.enabled;
  },
  resolvePermission: (ctx, { id }) => {
    ctx.state.permissionPrompts = ctx.state.permissionPrompts.filter((p) => p.id !== id);
  },

  // ---------------------------------------------------------------- recent-tab switcher
  mruStep: (ctx, { forward }) => {
    const state = ctx.state;
    if (!state.switcher) {
      const tabs = allTabs(state)
        .filter((t) => t.loaded)
        .sort((a, b) => Number(b.active) - Number(a.active))
        .slice(0, 5)
        .map((t) => ({ ...asTab(t) }));
      // Like core: the first card is usually the focused tab, but not in the empty state or with
      // Peek open; then forward starts on the first card. Backward starts on the last card.
      const firstIsFocused = tabs.length > 0 && tabs[0].id === state.focusedTab;
      if (tabs.length < (firstIsFocused ? 2 : 1)) return;
      state.switcher = { tabs, selected: forward ? (firstIsFocused ? 1 : 0) : tabs.length - 1 };
      return;
    }
    const n = state.switcher.tabs.length;
    state.switcher.selected = (state.switcher.selected + (forward ? 1 : -1) + n) % n;
  },
  mruSelect: (ctx, { index }) => {
    if (ctx.state.switcher && index < ctx.state.switcher.tabs.length) ctx.state.switcher.selected = index;
  },
  mruCommit: (ctx) => {
    const sw = ctx.state.switcher;
    ctx.state.switcher = null;
    if (sw) activate(ctx, sw.tabs[sw.selected].id);
  },
  mruCancel: (ctx) => {
    ctx.state.switcher = null;
  },

  // ---------------------------------------------------------------- downloads
  downloadControl: (ctx, { id, action }) => {
    const state = ctx.state;
    const d = state.downloads.find((x) => x.id === id);
    if (!d) return;
    switch (action) {
      case 'pause':
        if (d.state === 'inProgress') Object.assign(d, { state: 'paused', bytesPerSec: 0 });
        break;
      case 'resume':
        if (d.state === 'paused') Object.assign(d, { state: 'inProgress', bytesPerSec: 1_250_000 });
        break;
      case 'cancel':
        if (d.state === 'inProgress' || d.state === 'paused') Object.assign(d, { state: 'cancelled', bytesPerSec: 0 });
        break;
      case 'retry':
        state.downloads.unshift({ ...d, id: Math.max(...state.downloads.map((x) => x.id)) + 1, receivedBytes: 0, bytesPerSec: 900_000, state: 'inProgress', startedAt: Date.now() });
        state.downloads.length = Math.min(state.downloads.length, 20);
        break;
      default:
        console.info(`[mock] downloadControl ${action}: not simulated`);
    }
  },
  downloadDismiss: (ctx, { id }) => {
    ctx.state.downloads = ctx.state.downloads.filter((d) => d.id !== id);
  },

  // ---------------------------------------------------------------- extensions (Ctrl+E)
  // `store/extensions.rs`: Enter opens the popup card, else the options tab, else the Web Store
  // page; an extension that is off (or waiting for the user's OK) opens Settings › Extensions at its
  // row. It never turns one on — that is `setExtensionEnabled`, from Settings only.
  runExtension: (ctx, { id, action = 'primary' }) => {
    const state = ctx.state;
    const item = (state.extensions?.items ?? []).find((e) => e.id === id);
    if (!item) return;
    const manage = () => reducers.openUrl(ctx, { url: `sta://settings/?section=extensions&ext=${id}`, target: 'newTab' });
    const page = (p) => reducers.openUrl(ctx, { url: `chrome-extension://${id}/${p}`, target: 'newTab' });
    const card = () => {
      state.extensions.popup = {
        id,
        name: item.shortName || item.name,
        // No `sta://` icon in mock mode (http there; the shell serves it inside sta).
        icon: '',
        tab: state.focusedTab ?? null,
        hasOptions: Boolean(item.options),
        failed: false,
        seq: (state.extensions.popup?.seq ?? 0) + 1,
      };
    };
    if (action === 'manage') return manage();
    if (action === 'webStore') return reducers.openUrl(ctx, { url: `https://chromewebstore.google.com/detail/${id}`, target: 'newTab' });
    if (item.state !== 'enabled') return manage();
    if (action === 'options') return item.options ? page(item.options) : toast(ctx, 'This extension has no options page');
    if (action === 'popup') return item.popup ? card() : toast(ctx, "Toolbar click isn't supported in sta");
    if (item.popup) return card();
    if (item.options) return page(item.options);
    // No toast: the row already said "… · ↵ opens its Web Store page" (store/extensions.rs, UXV-5).
    reducers.openUrl(ctx, { url: `https://chromewebstore.google.com/detail/${id}`, target: 'newTab' });
  },
  requestExtensionDetails: (ctx, { id }) => {
    const ext = ctx.state.extensions;
    if (!ext || !(ext.items ?? []).some((e) => e.id === id)) return;
    // The real shell reads these out of Chromium (`ext_backend.rs`, `GetInfo`); the mock answers with
    // plausible words so the disclosure flow can be seen and shot.
    ext.details = [
      ...(ext.details ?? []).filter((d) => d.id !== id),
      {
        id,
        warnings: ['Read and change all your data on all websites', 'Read your browsing history'],
        hostAccess: 'On all sites',
        source: 'Added by another program, not from the Chrome Web Store',
      },
    ];
  },
  setExtensionEnabled: (ctx, { id, enabled }) => {
    const state = ctx.state;
    const item = (state.extensions?.items ?? []).find((e) => e.id === id);
    if (!item) return;
    if (enabled && (item.install === 'externalLocal' || item.install === 'managed')) {
      toast(ctx, item.install === 'managed' ? 'Your organization manages this extension' : "sta can't turn this on: another program installed it from a file");
      return;
    }
    item.state = enabled ? 'enabled' : 'off';
    state.extensions.needsOk = state.extensions.items.filter((e) => e.state === 'needsApproval').length;
    if (!enabled && state.extensions.popup?.id === id) state.extensions.popup = null;
  },
  removeExtension: (ctx, { id }) => {
    const state = ctx.state;
    const item = (state.extensions?.items ?? []).find((e) => e.id === id);
    if (!item) return;
    if (item.install === 'managed') {
      toast(ctx, 'Your organization manages this extension');
      return;
    }
    state.extensions.items = state.extensions.items.filter((e) => e.id !== id);
    state.extensions.details = (state.extensions.details ?? []).filter((d) => d.id !== id);
    state.extensions.needsOk = state.extensions.items.filter((e) => e.state === 'needsApproval').length;
    if (state.extensions.popup?.id === id) state.extensions.popup = null;
  },
  closeExtensionPopup: (ctx) => {
    if (ctx.state.extensions) ctx.state.extensions.popup = null;
  },

  // ---------------------------------------------------------------- no visible effect in mock mode
  toggleDevTools: () => {},
  focusDevTools: () => {},
  undockDevTools: () => {},
  print: () => {},
  viewSource: () => {},
  newBoostForSite: () => {},
  quit: () => console.info('[mock] quit: the real app would shut down'),

  // ---------------------------------------------------------------- AI agents (mock-agent.js)
  ...agentReducers,
};
