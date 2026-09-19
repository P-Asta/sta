// sta probe: options tab. Nothing happens unless a test calls `self.probe.*`.
self.probe = {
  id: chrome.runtime.id,
  openOptions: () => chrome.runtime.openOptionsPage().then(() => ({ ok: null }), (e) => ({ err: String((e && e.message) || e) })),
};
