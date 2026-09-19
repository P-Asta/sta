// The "current tab" shim sta evaluates in an extension's popup page and service worker while that
// extension's popup card is open (ext_shim.rs; docs/research/extensions.md §6).
//
// Chromium's `tabs.query` / `windows.*` walk Chrome `Browser` windows, and sta's tabs are Alloy views
// in none of them, so "the current tab" is `[]` / "No current window" for every extension. What
// *does* work natively is `tabs.get(<id>)` for an sta tab. This shim therefore only supplies the
// one thing Chromium cannot know — which tab the popup was opened over — and asks the extension's
// own `chrome.tabs.get` for everything else, so what an extension may see of that tab (its URL,
// its title) is still decided by Chromium from the extension's own permissions.
//
// It is an expression: `(<this file>)(<config>)`, with
//   { popupUrl, url, delta, tabId? }   in the popup page (it finds the tab id itself, see `resolve`)
//   { popupUrl, url, tabId?, near? }   in the service worker (`tabId` is what a page found)
//   { popupUrl: null, url: null }      in an extension page that is itself an sta tab: no tab is
//                                      named (it may be in the background), only `tabs.create`
//                                      gets its fallback
// and it answers 'installed' | 'updated' | 'no-extension-api'.
(cfg) => {
  'use strict';
  const api = typeof chrome === 'object' && chrome ? chrome : null;
  if (!api || !api.tabs || !api.runtime || !api.runtime.id) return 'no-extension-api';

  const KEY = Symbol.for('sta.currentTab');
  const existing = globalThis[KEY];
  if (existing) {
    // A tab id the page already found stays good for as long as it is about the same tab.
    if (typeof cfg.tabId === 'number' || existing.cfg.url !== cfg.url || existing.cfg.delta !== cfg.delta || existing.cfg.near !== cfg.near) existing.tabId = typeof cfg.tabId === 'number' ? cfg.tabId : undefined;
    existing.cfg = cfg;
    return 'updated';
  }
  const state = (globalThis[KEY] = { cfg, tabId: typeof cfg.tabId === 'number' ? cfg.tabId : undefined });

  // The window sta's tabs pretend to be in. Real window ids are SessionIDs (large), so 1 is free.
  const WINDOW_ID = 1;
  // How many SessionIDs Chromium may have handed to something that is not an sta browser (a
  // Chrome-created window) between the tab and the popup.
  const MAX_SKEW = 16;
  const isPage = typeof document === 'object';

  const tabs = api.tabs;
  // Not every extension context has `chrome.windows`; the tab answers do not depend on it.
  const windows = api.windows || null;
  const WINDOW_ID_CURRENT = windows && typeof windows.WINDOW_ID_CURRENT === 'number' ? windows.WINDOW_ID_CURRENT : -2;
  const native = {
    query: tabs.query.bind(tabs),
    get: tabs.get.bind(tabs),
    getCurrent: tabs.getCurrent ? tabs.getCurrent.bind(tabs) : null,
    create: tabs.create.bind(tabs),
    ...(windows && {
      winGet: windows.get.bind(windows),
      winGetCurrent: windows.getCurrent.bind(windows),
      winGetLastFocused: windows.getLastFocused.bind(windows),
      winGetAll: windows.getAll.bind(windows),
      winCreate: windows.create.bind(windows),
      winUpdate: windows.update.bind(windows),
    }),
  };

  const sameUrl = (a, b) => typeof a === 'string' && typeof b === 'string' && (a === b || a.split('#')[0] === b.split('#')[0]);

  // Is the popup this state belongs to still open? The page *is* the popup. A service worker
  // outlives it, and must stop answering for a tab the user has long left.
  const alive = async () => {
    if (isPage) return true;
    if (!api.runtime.getContexts || typeof state.cfg.popupUrl !== 'string') return false;
    try {
      const contexts = await api.runtime.getContexts({});
      return contexts.some((c) => typeof c.documentUrl === 'string' && c.documentUrl.split(/[?#]/)[0] === state.cfg.popupUrl.split(/[?#]/)[0]);
    } catch {
      return false;
    }
  };

  // Tab ids and CEF browser ids are handed out in the same order, so the tab is `delta` ids below
  // the popup — further when Chromium created windows of its own in between. The URL sta knows
  // the tab by is the proof; an extension that may not read URLs gets no answer (it could not use
  // one: nothing in sta grants `activeTab`).
  //
  // A service worker has no tab of its own to count from. Until the page's answer reaches it, it
  // looks around `near`: where the tab would be if nothing had moved since sta last saw an id.
  const resolve = async () => {
    if (typeof state.tabId === 'number') return state.tabId;
    const cfg = state.cfg;
    const candidates = [];
    if (isPage && native.getCurrent && typeof cfg.delta === 'number') {
      const self = await native.getCurrent();
      if (self && typeof self.id === 'number') for (let skew = 0; skew <= MAX_SKEW; skew++) candidates.push(self.id - cfg.delta - skew);
    } else if (typeof cfg.near === 'number') {
      candidates.push(cfg.near);
      for (let skew = 1; skew <= MAX_SKEW; skew++) candidates.push(cfg.near + skew, cfg.near - skew);
    }
    for (const id of candidates) {
      const tab = await native.get(id).catch(() => null);
      if (tab && sameUrl(tab.url || tab.pendingUrl, cfg.url)) {
        if (state.cfg === cfg) state.tabId = tab.id;
        return tab.id;
      }
    }
    return undefined;
  };
  // ext_shim.rs asks the page for the id, to hand it to the service worker.
  state.resolve = resolve;

  const currentTab = async () => {
    if (!(await alive())) return null;
    const id = await resolve();
    if (typeof id !== 'number') return null;
    const tab = await native.get(id).catch(() => null);
    return tab ? { ...tab, active: true, highlighted: true, selected: true, index: 0, windowId: WINDOW_ID } : null;
  };

  const fakeWindow = (tab, populate) => ({
    id: WINDOW_ID,
    focused: true,
    incognito: false,
    alwaysOnTop: false,
    state: 'normal',
    type: 'normal',
    top: 0,
    left: 0,
    width: tab.width || 1280,
    height: tab.height || 800,
    ...(populate ? { tabs: [tab] } : {}),
  });

  const matchesPattern = (pattern, url) => {
    if (pattern === '<all_urls>') return /^(https?|file|ftp|wss?):/.test(url);
    const escaped = pattern.replace(/[.+?^${}()|[\]\\]/g, '\\$&').replace(/\*/g, '.*');
    return new RegExp(`^${escaped}$`).test(url);
  };

  // A query is about "the current tab" when it names the current window, or the active tab of no
  // particular window. Everything else (`{}`, another window's tabs) stays Chromium's.
  const asksForCurrent = (q) => {
    if (!q || typeof q !== 'object') return false;
    if (q.active === false || q.highlighted === false || q.currentWindow === false || q.lastFocusedWindow === false) return false;
    if (q.pinned === true || q.audible === true || q.muted === true || q.discarded === true || q.frozen === true) return false;
    if (typeof q.index === 'number' && q.index !== 0) return false;
    if (typeof q.groupId === 'number' && q.groupId !== -1) return false;
    if (typeof q.windowType === 'string' && q.windowType !== 'normal') return false;
    if (typeof q.windowId === 'number') return q.windowId === WINDOW_ID || q.windowId === WINDOW_ID_CURRENT;
    return q.active === true || q.currentWindow === true || q.lastFocusedWindow === true;
  };

  const passesFilters = (q, tab) => {
    if (q.url !== undefined) {
      const patterns = Array.isArray(q.url) ? q.url : [q.url];
      if (typeof tab.url !== 'string' || !patterns.some((p) => matchesPattern(String(p), tab.url))) return false;
    }
    if (typeof q.status === 'string' && q.status !== tab.status) return false;
    if (typeof q.title === 'string' && !matchesPattern(q.title, tab.title || '')) return false;
    return true;
  };

  // `ours` answers with a value, or `undefined` for "not mine" — then the native call runs with the
  // caller's own arguments, callback included, so its result and `runtime.lastError` are Chromium's.
  const route = (nativeFn, ours) =>
    function (...args) {
      const callback = typeof args[args.length - 1] === 'function' ? args[args.length - 1] : null;
      const plain = callback ? args.slice(0, -1) : args;
      const answer = Promise.resolve()
        .then(() => ours(...plain))
        .catch(() => undefined);
      if (callback) {
        answer.then((value) => (value === undefined ? nativeFn(...args) : callback(value)));
        return undefined;
      }
      return answer.then((value) => (value === undefined ? nativeFn(...plain) : value));
    };

  tabs.query = route(native.query, async (q) => {
    if (!asksForCurrent(q)) return undefined;
    const tab = await currentTab();
    if (!tab) return undefined;
    return passesFilters(q, tab) ? [tab] : [];
  });

  const currentWindow = async (options) => {
    const tab = await currentTab();
    return tab ? fakeWindow(tab, !!(options && options.populate)) : undefined;
  };
  if (windows) {
    windows.getCurrent = route(native.winGetCurrent, currentWindow);
    windows.getLastFocused = route(native.winGetLastFocused, currentWindow);
    windows.get = route(native.winGet, async (id, options) => (id === WINDOW_ID ? currentWindow(options) : undefined));
    windows.update = route(native.winUpdate, async (id) => (id === WINDOW_ID ? currentWindow() : undefined));
    windows.getAll = route(native.winGetAll, async (options) => {
      const ours = await currentWindow(options);
      if (!ours) return undefined;
      const rest = await native.winGetAll(options).catch(() => []);
      return [ours, ...rest];
    });
  }

  // A profile without a Chrome window cannot open a tab ("No current window"). `windows.create`
  // can: Chromium makes a window of its own, which sta hides and turns into one of its tabs
  // (foreign.rs), under the same verdict and budget as every other tab an extension asks for.
  tabs.create = route(native.create, async (props) => {
    const wanted = props && typeof props === 'object' ? { ...props } : {};
    if (wanted.windowId === WINDOW_ID) delete wanted.windowId;
    if (typeof wanted.windowId === 'number' && wanted.windowId !== WINDOW_ID_CURRENT) return undefined;
    try {
      return await native.create(wanted);
    } catch (e) {
      if (!/No current window/i.test(String(e && e.message))) return undefined;
    }
    if (!native.winCreate) return undefined;
    const created = await native.winCreate({ url: wanted.url, focused: wanted.active !== false, type: 'normal' });
    const tab = created && created.tabs && created.tabs[0];
    return tab ? { ...tab, windowId: WINDOW_ID } : undefined;
  });

  // What pressing the extension's toolbar button would do right now — which is the extension's to
  // decide at run time, not its manifest's: `action.setPopup('')` means "no popup, tell me about
  // the click" (1Password without an account opens its sign-in page that way). sta has no toolbar
  // button, so ext_shim.rs asks the service worker this once per card, and an empty popup gets the
  // click it was waiting for.
  state.action = async () => {
    const action = api.action || api.browserAction;
    if (!action || !action.getPopup || !action.onClicked) return { popup: null, clicked: false };
    const tab = await currentTab();
    let popup;
    try {
      popup = await action.getPopup(tab ? { tabId: tab.id } : {});
    } catch {
      popup = await action.getPopup({}).catch(() => null);
    }
    if (popup !== '') return { popup, clicked: false };
    if (!action.onClicked.hasListeners() || typeof action.onClicked.dispatch !== 'function') return { popup, clicked: false };
    const none = typeof tabs.TAB_ID_NONE === 'number' ? tabs.TAB_ID_NONE : -1;
    action.onClicked.dispatch(tab || { id: none, index: -1, windowId: -1, active: true, highlighted: true, selected: true, pinned: false, incognito: false, discarded: false, autoDiscardable: true, groupId: -1 });
    return { popup, clicked: true };
  };

  // The page's answer is what the service worker waits for: have it ready when sta asks.
  if (isPage) resolve().catch(() => {});
  return 'installed';
}
