// sta probe: windows. Nothing happens unless a test calls `self.probe.*` over the DevTools protocol.
self.log = [];
const note = (kind, data) => self.log.push({ t: Date.now(), kind, data });
const settle = (p) => p.then((value) => ({ ok: value ?? null }), (e) => ({ err: String((e && e.message) || e) }));
const lite = (t) => t && { id: t.id, windowId: t.windowId, url: t.url || t.pendingUrl };

chrome.tabs.onCreated.addListener((t) => note('tabs.onCreated', lite(t)));
chrome.tabs.onRemoved.addListener((id, info) => note('tabs.onRemoved', { id, windowClosing: info.isWindowClosing }));
chrome.windows.onCreated.addListener((w) => note('windows.onCreated', { id: w.id, type: w.type, incognito: w.incognito }));
chrome.windows.onRemoved.addListener((id) => note('windows.onRemoved', { id }));

self.probe = {
  id: chrome.runtime.id,
  /** `tabs.create` after `delayMs` (resolves when the call itself settles). */
  createTab: (url, delayMs = 0) =>
    settle(new Promise((ok) => setTimeout(ok, delayMs)).then(() => chrome.tabs.create({ url })).then(lite)),
  /** `windows.create({type, url, incognito})`. */
  createWindow: (url, type = 'normal', incognito = false) =>
    settle(chrome.windows.create({ url, type, incognito, width: 420, height: 360 }).then((w) => ({ id: w.id, type: w.type }))),
  openOptions: () => settle(chrome.runtime.openOptionsPage()),
  /** `identity.launchWebAuthFlow`: resolves with the redirect URL (or the error). */
  authFlow: (url) => settle(chrome.identity.launchWebAuthFlow({ url, interactive: true })),
  redirectUrl: () => chrome.identity.getRedirectURL('cb'),
  windows: () => settle(chrome.windows.getAll({ populate: true }).then((ws) => ws.map((w) => ({ id: w.id, type: w.type, tabs: (w.tabs || []).map(lite) })))),
  closeWindow: (id) => settle(chrome.windows.remove(id)),
  page: (path) => chrome.runtime.getURL(path),
};
