// The probe popup's own work, for gate S3 (crates/sta/e2e/extensions-e2e.mjs, section `(c)`):
//
// 1. `chrome.storage` — what a self-contained popup does: it writes and reads back, which proves the
//    popup page really is running as an extension page inside sta's card.
// 2. `chrome.tabs.query({active: true, currentWindow: true})` — what a popup that wants to act on the
//    current tab asks for. With prebuilt CEF (user decision D1a) sta's tabs are not Chromium tabs, so
//    this is expected to come back empty or fail; the page writes down exactly what happened instead
//    of hiding it, and the e2e records it.
//
// Nothing here opens a window or a tab: the probe only ever acts when a test asks.

const show = (id, text) => {
  const el = document.getElementById(id);
  if (el) el.textContent = text;
};

(async () => {
  try {
    const stamp = `popup-${Date.now()}`;
    await chrome.storage.local.set({ staProbePopup: stamp });
    const read = await chrome.storage.local.get('staProbePopup');
    show('work', `storage: ${read.staProbePopup === stamp ? 'ok' : 'mismatch'}`);
  } catch (e) {
    show('work', `storage: failed (${e && e.message})`);
  }
  try {
    const tabs = await chrome.tabs.query({ active: true, currentWindow: true });
    show('tabs', `tabs: ${tabs.length} ${JSON.stringify(tabs.map((t) => t.url || ''))}`);
  } catch (e) {
    show('tabs', `tabs: failed (${e && e.message})`);
  }
})();
