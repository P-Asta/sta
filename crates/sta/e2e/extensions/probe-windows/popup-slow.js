// A popup that renders 2 s after it loads (extensions-e2e section `(c)`, P3-E2E-1). An extension page
// may not run inline script (its own CSP), so this lives in a file like the other probe pages.
//
// The delay is deliberately past the last *timed* measurement of `ext_popup.rs` and inside its 3 s
// budget: a card that judged the popup on a stale measurement would call this working popup broken.

const START = Date.now();

setTimeout(() => {
  const host = document.getElementById('host');
  if (!host) return;
  const el = document.createElement('div');
  el.id = 'late';
  el.textContent = `rendered after ${Date.now() - START} ms`;
  host.appendChild(el);
}, 2000);
