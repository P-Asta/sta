// Peek header strip (PROTOCOL §4, arc_spec §2.18): 40px above the peeked page. Buttons in Arc
// order on the left (× Split Expand), favicon + title + host centered. Feature popups (OAuth
// windows, `peek.popup`) only get × and a "popup" note.

import { html, render, useLayoutEffect, useRef } from '/common/vendor/htm-preact.js';
import { dispatch, startSurface } from '/common/ipc.js';
import { Icon } from '/common/icons.js';
import { Favicon, IconButton, Spinner } from '/common/components.js';
import * as motion from '/common/motion.js';
import { shortcut } from '/common/util.js';

const mount = document.getElementById('app');
const report = (e) => console.error('[peek]', e);
const send = (command) => dispatch(command).catch(report);

/**
 * Every command that ends this Peek (close, Split, Expand). The overlay is activatable: the shell
 * hides it the moment the command arrives, with no ack and no linger, so the frame this renderer
 * produced last is the frame the *next* Alt+click would show — the previous site's title and host.
 * The blank frame goes out first, then the command (FINAL PLAN §1.3).
 */
async function leave(command) {
  await motion.closeBlank(mount?.querySelector('.peek') ?? null);
  send(command);
}

/** Whether a Peek was on screen at the last state: a fresh one has something to show again. */
let shown = false;
/** The animation key of every motion on this surface (`crates/sta-core/src/motion.rs`). */
const KEY = 'overlays.peek';

/** Favicon URL the UI's CSP (img-src 'self' data: https:) can load, else null (letter tile). */
const loadableFavicon = (url) => (/^(https:|data:image\/|sta:)/i.test(url ?? '') ? url : null);

/** "Expand" glyph (two diagonal arrows), drawn like the shared 24-grid icons. */
function ExpandIcon() {
  return html`<svg class="icon" viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
    <path d="M14 4.5h5.5V10" /><path d="M19.5 4.5 13.5 10.5" /><path d="M10 19.5H4.5V14" /><path d="M4.5 19.5l6-6" />
  </svg>`;
}

function PeekHeader({ peek }) {
  // First, so the fade below already sees the surface as presented.
  motion.usePresence(Boolean(peek));
  const centerRef = useRef(null);
  // `overlays.peek`: the strip stays put — the shell sized and placed it — and only what it *says*
  // about the page crosses over, keyed on the peeked tab. Alt+clicking a second link into the same
  // Peek swaps the title and host instead of blinking.
  useLayoutEffect(() => {
    if (peek) motion.animate(centerRef.current, KEY, [{ opacity: 0 }, { opacity: 1 }], { duration: motion.duration(KEY, 120) });
  }, [peek?.tab?.id ?? null]);
  if (!peek) return html`<div class="peek" />`;
  const { tab, popup } = peek;
  const secure = /^(https|sta|file|about|data):/i.test(tab.url);
  return html`<div class="peek">
    <div class="peek-actions">
      <${IconButton} icon="close" label="Close (Esc)" class="peek-btn" onClick=${() => leave({ type: 'closePeek', focusLost: false })} />
      ${!popup &&
      html`<${IconButton} icon="split" label="Open in split view" class="peek-btn" onClick=${() => leave({ type: 'expandPeek', split: true })} />
        <button type="button" class="icon-btn peek-btn" aria-label=${`Open as tab (${shortcut('Ctrl+O')})`} title=${`Open as tab (${shortcut('Ctrl+O')})`} onClick=${() => leave({ type: 'expandPeek', split: false })}>
          <${ExpandIcon} />
        </button>`}
    </div>
    <div class="peek-center" ref=${centerRef} title=${tab.url}>
      <span class="peek-fav">
        ${tab.loading ? html`<${Spinner} size=${14} label=${null} />` : html`<${Favicon} src=${loadableFavicon(tab.favicon)} host=${tab.host} size=${16} lazy=${false} />`}
      </span>
      <span class="peek-title">${tab.title || tab.host || tab.url}</span>
      ${tab.host && tab.title && tab.title !== tab.host && html`<span class="peek-host">${!secure && html`<${Icon} name="warning" size=${12} label="Not secure" />`}<span class="peek-host-text">${tab.host}</span></span>`}
    </div>
    <div class="peek-side">
      ${popup && html`<span class="peek-note">Popup window</span>`}
    </div>
  </div>`;
}

function onState(state) {
  const peek = state.peek ?? null;
  // A fresh Peek after a page-initiated close (`leave`) undoes the blank frame that close presented.
  if (peek && !shown) motion.unblank();
  shown = Boolean(peek);
  render(html`<${PeekHeader} peek=${peek} />`, mount);
}

window.addEventListener('keydown', (event) => {
  if (event.key === 'Escape') {
    event.preventDefault();
    leave({ type: 'closePeek', focusLost: false });
  } else if (event.ctrlKey && !event.shiftKey && !event.altKey && (event.key === 'o' || event.key === 'O')) {
    event.preventDefault();
    leave({ type: 'expandPeek', split: false });
  }
});

startSurface({ render: onState }).catch(report);
