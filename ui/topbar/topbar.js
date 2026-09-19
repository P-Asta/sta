// sta top bar (PROTOCOL §4): window drag area above the content; when the sidebar is hidden it
// shows the sidebar toggle, back/forward/reload and a centered URL pill; always the split button and
// the Windows 11 caption buttons (46×40, close hover #c42b1c with a white glyph).

import { h, html, render, useLayoutEffect, useRef } from '/common/vendor/htm-preact.js';
import { dispatch, startSurface } from '/common/ipc.js';
import { IconButton } from '/common/components.js';
import { NavButtons, UrlPill } from '/common/chrome.js';
import { classNames } from '/common/util.js';
import * as motion from '/common/motion.js';
import { AgentChip } from './agent-chip.js';

const fire = (command) => dispatch(command).catch((e) => console.error('[topbar] dispatch failed', command, e));

/**
 * `topbar.navFade`. The top bar learns `sidebarVisible` from the state push, but its **width** only
 * changes when the shell relays out the window: sliding the controls in would show them twice and
 * then jump (critique issue 18). So they are held blank for the length of the shell's park delay and
 * then fade - which is also what "after the top bar's resize" means in practice, because the native
 * layout happens in the same shell turn as the push.
 */
const NAV_FADE_DELAY_MS = 60;

// Caption glyphs drawn on a 10×10 pixel grid (Segoe Fluent "Chrome" glyph proportions) so the
// 1px strokes stay crisp at 100% scaling.
const svg = (children) =>
  h(
    'svg',
    { width: 10, height: 10, viewBox: '0 0 10 10', fill: 'none', stroke: 'currentColor', 'stroke-width': 1, 'aria-hidden': 'true', class: 'caption-glyph' },
    children,
  );
const GLYPHS = {
  minimize: () => svg([h('path', { key: 0, d: 'M0 5.5h10' })]),
  maximize: () => svg([h('rect', { key: 0, x: 0.5, y: 0.5, width: 9, height: 9, rx: 1 })]),
  restore: () =>
    svg([
      h('rect', { key: 0, x: 0.5, y: 2.5, width: 7, height: 7, rx: 1 }),
      h('path', { key: 1, d: 'M2.5 2.5v-.5a1.5 1.5 0 0 1 1.5-1.5h4a1.5 1.5 0 0 1 1.5 1.5v4a1.5 1.5 0 0 1-1.5 1.5h-.5' }),
    ]),
  close: () => svg([h('path', { key: 0, d: 'M0.5 0.5l9 9M9.5 0.5l-9 9' })]),
};

function CaptionButton({ kind, label, onClick }) {
  return html`<button
    type="button"
    class=${classNames('caption-btn', `caption-${kind}`)}
    aria-label=${label}
    title=${label}
    tabindex="-1"
    onClick=${onClick}
  >${GLYPHS[kind]()}</button>`;
}

function Topbar({ state }) {
  const { sidebarVisible, maximized, focused } = state.window;
  const current = state.current;
  const rootRef = useRef(null);

  // Keyed on `sidebarVisible` flipping - never on a render, and never on the way back (the controls
  // leave with the resize, they do not fade out).
  const wasVisible = useRef(sidebarVisible);
  const appeared = useRef(false);
  if (wasVisible.current !== sidebarVisible) {
    appeared.current = !sidebarVisible;
    wasVisible.current = sidebarVisible;
  }
  useLayoutEffect(() => {
    if (!appeared.current) return;
    appeared.current = false;
    const key = 'topbar.navFade';
    const duration = motion.duration(key, 90);
    for (const selector of ['.tb-left', '.tb-center']) {
      motion.animate(rootRef.current?.querySelector(selector), key, [{ opacity: 0 }, { opacity: 1 }], {
        duration,
        delay: NAV_FADE_DELAY_MS,
        fill: 'backwards',
      });
    }
  });

  return html`<div ref=${rootRef} class=${classNames('topbar drag', !sidebarVisible && 'is-sidebar-hidden', !focused && 'is-inactive')}>
    <div class="tb-left">
      ${!sidebarVisible &&
      html`<${IconButton} icon="sidebar" label="Show sidebar" title="Show sidebar (Ctrl+S)" onClick=${() => fire({ type: 'toggleSidebar' })} />
        <${NavButtons} current=${current} />`}
    </div>
    <div class="tb-center">${!sidebarVisible && html`<${UrlPill} current=${current} compact />`}</div>
    <div class="tb-right">
      <${AgentChip} agent=${state.agent} />
      <${IconButton}
        class="tb-split"
        icon="split"
        label="Add split view"
        title=${(current?.splitPanes ?? 0) >= 4 ? 'Split view is full (4 panes)' : 'Split view (Ctrl+Shift+=)'}
        disabled=${!current || (current.splitPanes ?? 0) >= 4}
        onClick=${() => fire({ type: 'openCommandBar', mode: 'split' })}
      />
      <div class="caption-buttons" role="group" aria-label="Window">
        <${CaptionButton} kind="minimize" label="Minimize" onClick=${() => fire({ type: 'windowControl', action: 'minimize' })} />
        <${CaptionButton}
          kind=${maximized ? 'restore' : 'maximize'}
          label=${maximized ? 'Restore' : 'Maximize'}
          onClick=${() => fire({ type: 'windowControl', action: 'toggleMaximize' })}
        />
        <${CaptionButton} kind="close" label="Close" onClick=${() => fire({ type: 'windowControl', action: 'close' })} />
      </div>
    </div>
  </div>`;
}

const mountPoint = document.getElementById('app') ?? document.body;
startSurface({ render: (state) => render(html`<${Topbar} state=${state} />`, mountPoint) }).catch(() => {});
