// Header strip of the extension popup card (PROTOCOL §4; ext design FINAL PLAN §4 "Popup card"):
// 40px above the extension's own popup page, drawn by sta so the card always says whose popup this
// is and always has a way out — the page below is the extension's, and it may render nothing at all.
//
// When the popup never came up (`extensions.popup.failed`), the strip is all there is: it then says
// so in one line and offers Options, instead of leaving an empty rectangle on screen.

import { html, render } from '/common/vendor/htm-preact.js';
import { dispatch, startSurface } from '/common/ipc.js';
import { Favicon, IconButton } from '/common/components.js';

const mount = document.getElementById('app');
const report = (e) => console.error('[extension]', e);
const send = (command) => dispatch(command).catch(report);

/** What the card says when the popup did not work (mirrors `extensions::POPUP_FAILED_TEXT`). */
const FAILED_TEXT = "This popup doesn't work in sta yet";
/** An extension that counts on `activeTab` alone: sta tells a popup which tab it was opened over, but
 * `activeTab` is granted by pressing a toolbar button sta does not have (D1a), so this one still gets
 * no URL and cannot reach into the page. The card says so even when the page paints something —
 * which for such an extension is usually its *own* error page, with nothing from sta anywhere. */
const LIMITED_TEXT = 'Needs the current tab';
const LIMITED_TITLE = "sta can't grant an extension 'activeTab' yet, so parts of this popup may not work.";

/** Icon URLs the UI's CSP can load: sta's own `__ext-icon` route, or a data URL. */
const loadableIcon = (url) => (/^(sta:|data:image\/|https:)/i.test(url ?? '') ? url : null);

function PopupHeader({ popup }) {
  if (!popup) return html`<div class="xp" />`;
  const { name, icon, hasOptions, failed, needsCurrentTab, id } = popup;
  // The card is a floating, dismissible panel over the page: it has to be able to say whose it is,
  // and the extension's name is the only thing that identifies it.
  return html`<div class=${failed ? 'xp is-failed' : 'xp'} role="group" aria-label=${`${name} popup`}>
    <div class="xp-head">
      <span class="xp-icon"><${Favicon} src=${loadableIcon(icon)} host=${name} size=${16} lazy=${false} /></span>
      <span class="xp-name" title=${name}>${name}</span>
      ${!failed && needsCurrentTab && html`<span class="xp-limited" title=${LIMITED_TITLE}>${LIMITED_TEXT}</span>`}
      <div class="xp-actions">
        ${hasOptions &&
        html`<button type="button" class="xp-link" onClick=${() => send({ type: 'runExtension', id, action: 'options' })}>Options</button>`}
        <${IconButton} icon="close" label="Close (Esc)" class="xp-btn" onClick=${() => send({ type: 'closeExtensionPopup' })} />
      </div>
    </div>
    ${failed && html`<p class="xp-failed">${FAILED_TEXT}</p>`}
  </div>`;
}

function onState(state) {
  const popup = state.extensions?.popup ?? null;
  document.title = popup?.name ? `${popup.name} — popup` : 'Extension popup';
  render(html`<${PopupHeader} popup=${popup} />`, mount);
}

window.addEventListener('keydown', (event) => {
  if (event.key === 'Escape') {
    event.preventDefault();
    send({ type: 'closeExtensionPopup' });
  }
});

startSurface({ render: onState }).catch(report);
