// sta empty state (PROTOCOL §4): shown in the content area when the active space has no active
// item. A themed card with a large faint mark, the space name and keyboard hints.

import { h, html, render } from '/common/vendor/htm-preact.js';
import { startSurface } from '/common/ipc.js';
import { Kbd } from '/common/components.js';
import { AppMark } from '/common/icons.js';
import { activeSpace } from '/common/util.js';

/** The sta mark (`crates/sta/res/icon.svg`), faint and large behind the hints. */
const Mark = () => h(AppMark, { class: 'empty-mark' });

const HINTS = [
  { keys: ['Ctrl', 'T'], label: 'New tab' },
  { keys: ['Alt', '1–9'], label: 'Switch space' },
  { keys: ['Ctrl', 'S'], label: 'Toggle sidebar' },
];

function Empty({ state }) {
  const space = activeSpace(state);
  return html`<main class="empty" aria-label="No tab open">
    <div class="empty-inner">
      <${Mark} />
      ${space &&
      html`<div class="empty-space">
        <span class="emoji" aria-hidden="true">${space.icon}</span>
        <span>${space.name}</span>
      </div>`}
      <ul class="empty-hints">
        ${HINTS.map(
          (hint) => html`<li key=${hint.label} class="empty-hint">
            <${Kbd} keys=${hint.keys} />
            <span class="empty-hint-label">${hint.label}</span>
          </li>`,
        )}
      </ul>
    </div>
  </main>`;
}

const mountPoint = document.getElementById('app') ?? document.body;
startSurface({ render: (state) => render(html`<${Empty} state=${state} />`, mountPoint) }).catch(() => {});
