// Browser-chrome pieces shared by the sidebar and the top bar (docs/PROTOCOL.md §4, §5):
// navigation buttons and the URL pill. Styles: /common/chrome.css.
//
//   import { NavButtons, UrlPill } from '/common/chrome.js';
//   html`<${NavButtons} current=${state.current} />`
//   html`<${UrlPill} current=${state.current} />`
//
// Every string shown here (pill text, URL tooltip, boost names) comes from core and is rendered as
// text nodes or attributes only.

import { html, useEffect, useLayoutEffect, useRef, useState } from './vendor/htm-preact.js';
import { dispatch } from './ipc.js';
import { Icon } from './icons.js';
import { IconButton, Menu } from './components.js';
import { classNames } from './util.js';
import * as motion from './motion.js';

const fire = (command) => dispatch(command).catch((e) => console.error('[chrome] dispatch failed', command, e));

// ------------------------------------------------------------------------------------ nav buttons

/**
 * Back / forward / reload-or-stop for the focused tab (`state.current`). Shift+click on reload
 * bypasses the cache. Disabled without a current tab.
 * @param {{current: any, size?: 'sm'|'md'}} props
 */
export function NavButtons({ current, size = 'md' }) {
  const loading = Boolean(current?.loading);
  return html`<div class="nav-buttons" role="group" aria-label="Navigation">
    <${IconButton}
      icon="back"
      label="Back"
      title="Back (Alt+Left)"
      size=${size}
      disabled=${!current?.canGoBack}
      onClick=${() => fire({ type: 'goBack' })}
    />
    <${IconButton}
      icon="forward"
      label="Forward"
      title="Forward (Alt+Right)"
      size=${size}
      disabled=${!current?.canGoForward}
      onClick=${() => fire({ type: 'goForward' })}
    />
    <${IconButton}
      icon=${loading ? 'stop' : 'reload'}
      label=${loading ? 'Stop' : 'Reload'}
      title=${loading ? 'Stop loading (Esc)' : 'Reload (Ctrl+R)'}
      size=${size}
      disabled=${!current}
      onClick=${(e) => fire(loading ? { type: 'stopLoad' } : { type: 'reload', ignoreCache: e.shiftKey })}
    />
  </div>`;
}

// ------------------------------------------------------------------------------------ URL pill

const INTERNAL_GLYPHS = { settings: 'settings', archive: 'archive', history: 'history', boosts: 'boost' };

/** `{glyph, label, tone}` for the pill's leading security glyph. */
function securityOf(current) {
  if (!current) return { glyph: 'search', label: null, tone: 'muted' };
  if (current.loadError) return { glyph: 'warning', label: 'Page failed to load', tone: 'warning' };
  if (current.internal) {
    const host = /^sta:\/\/([a-z]+)/i.exec(current.url)?.[1]?.toLowerCase();
    return { glyph: INTERNAL_GLYPHS[host] ?? 'star', label: 'sta page', tone: 'accent' };
  }
  if (/^file:/i.test(current.url)) return { glyph: 'folder', label: 'Local file', tone: 'muted' };
  if (current.secure) return { glyph: 'lock', label: 'Connection is secure', tone: 'muted' };
  return { glyph: 'info', label: 'Connection is not secure', tone: 'muted' };
}

/**
 * Loading bar state: visible while loading, then completes to 100% and fades out ~200 ms after the
 * load finished (arc_spec §2.16).
 */
function useLoadBar(loading, progress) {
  const [bar, setBar] = useState({ visible: loading, value: loading ? progress : 0 });
  const timer = useRef(0);
  useEffect(() => {
    clearTimeout(timer.current);
    if (loading) {
      setBar({ visible: true, value: Math.max(0.08, Math.min(1, progress || 0)) });
    } else {
      setBar((b) => (b.visible ? { visible: true, value: 1, done: true } : b));
      timer.current = setTimeout(() => setBar({ visible: false, value: 0 }), 380);
    }
    return () => clearTimeout(timer.current);
  }, [loading, progress]);
  return bar;
}

/**
 * The URL pill (PROTOCOL §5.2): security glyph + `current.pill`; hover shows copy and boost
 * buttons; click opens the command bar in edit-URL mode (new-tab mode without a tab); 2px loading
 * bar along the bottom.
 * @param {{current: any, compact?: boolean, class?: string}} props
 */
export function UrlPill({ current, compact = false, class: className }) {
  const [copied, setCopied] = useState(false);
  const [boostMenu, setBoostMenu] = useState(false);
  const boostButton = useRef(null);
  const copyButton = useRef(null);
  const pillRef = useRef(null);
  const copyTimer = useRef(0);
  const bar = useLoadBar(Boolean(current?.loading), current?.progress ?? 0);
  useEffect(() => () => clearTimeout(copyTimer.current), []);

  // `sidebar.urlPill`: the host crossfades when **the same tab** moves to another site. Keyed on
  // `current.tab` staying put while `current.host` changes, so Ctrl+Tab and Ctrl+1..9 - which put a
  // different tab's host in the pill - change it instantly, as they should (critique issue 18).
  // The ghost is taken here, in the component body, which Preact runs before it patches the DOM.
  const site = useRef(null);
  const enterHost = useRef(false);
  const seen = site.current;
  site.current = { tab: current?.tab ?? null, host: current?.host ?? null };
  if (seen && seen.tab === site.current.tab && seen.host !== site.current.host && motion.enabled('sidebar.urlPill')) {
    enterHost.current = true;
    motion.fadeGhost(motion.ghost(pillRef.current?.querySelector('.url-pill-text')), 'sidebar.urlPill', [{ opacity: 1 }, { opacity: 0 }], {
      duration: motion.duration('sidebar.urlPill', 200),
    });
  }

  // The copy button's check pops in; the host's new text fades up under the outgoing ghost.
  const wasCopied = useRef(copied);
  useLayoutEffect(() => {
    if (enterHost.current) {
      enterHost.current = false;
      motion.animate(pillRef.current?.querySelector('.url-pill-text'), 'sidebar.urlPill', [{ opacity: 0 }, { opacity: 1 }], {
        duration: motion.duration('sidebar.urlPill', 200),
      });
    }
    if (wasCopied.current === copied) return;
    wasCopied.current = copied;
    if (!copied) return;
    motion.animate(
      copyButton.current,
      'sidebar.urlPill',
      [
        { scale: 1 - motion.distance(0.25), rotate: `${-motion.distance(20)}deg` },
        { scale: 1, rotate: 'none' },
      ],
      { duration: motion.duration('sidebar.urlPill', 200), easing: motion.EASE_SPRING },
    );
  });

  const security = securityOf(current);
  const boosts = current?.boosts ?? [];
  const anyEnabled = boosts.some((b) => b.enabled);
  const zoom = current?.zoomPercent ?? 100;
  const zoomed = Boolean(current) && zoom !== 100;
  // Space the always-visible chips (zoom level, enabled boost) take at the pill's right end, so
  // the host text never runs under them.
  const reserve = (zoomed ? 46 : 0) + (anyEnabled ? 26 : 0);
  const pillStyle = reserve ? { '--pill-reserve': `${reserve}px`, '--pill-zoom': `${zoomed ? 46 : 0}px` } : undefined;

  const open = () => fire({ type: 'openCommandBar', mode: current ? 'editUrl' : 'newTab' });
  const copy = (e) => {
    e.stopPropagation();
    fire({ type: 'copyUrl', markdown: e.altKey });
    setCopied(true);
    clearTimeout(copyTimer.current);
    copyTimer.current = setTimeout(() => setCopied(false), 1400);
  };
  const onBoost = (e) => {
    e.stopPropagation();
    if (boosts.length === 1) fire({ type: 'toggleBoost', id: boosts[0].id });
    else setBoostMenu((v) => !v);
  };

  return html`<div
    ref=${pillRef}
    class=${classNames('url-pill no-drag', compact && 'is-compact', !current && 'is-empty', zoomed && 'has-zoom', (boostMenu || copied) && 'is-engaged', className)}
    style=${pillStyle}
  >
    <button
      type="button"
      class="url-pill-main"
      title=${current ? current.url : 'Search or enter URL (Ctrl+T)'}
      aria-label=${current ? `Address: ${current.url}. Edit (Ctrl+L)` : 'Search or enter URL'}
      onClick=${open}
    >
      <span class=${`url-pill-glyph tone-${security.tone}`} title=${security.label ?? undefined}>
        <${Icon} name=${security.glyph} size=${compact ? 13 : 14} strokeWidth=${1.75} />
      </span>
      <span class="url-pill-text">${current ? current.pill : 'Search or enter URL…'}</span>
    </button>
    ${current &&
    html`<div class="url-pill-actions">
      ${zoomed &&
      html`<button
        type="button"
        class="url-pill-zoom"
        title=${`Zoom ${zoom}% · click to reset (Ctrl+0)`}
        aria-label=${`Zoom ${zoom}%. Reset zoom`}
        onClick=${(e) => {
          e.stopPropagation();
          fire({ type: 'zoom', direction: 'reset' });
        }}
      >${zoom}%</button>`}
      ${boosts.length > 0 &&
      html`<${IconButton}
        class=${classNames('url-pill-boost', anyEnabled && 'is-on')}
        icon="boost"
        size="sm"
        iconSize=${13}
        label=${boosts.length === 1 ? `${boosts[0].enabled ? 'Disable' : 'Enable'} boost “${boosts[0].name || boosts[0].host}”` : 'Boosts'}
        pressed=${anyEnabled}
        buttonRef=${boostButton}
        onClick=${onBoost}
      />`}
      <${IconButton}
        class=${classNames('url-pill-copy', copied && 'is-done')}
        icon=${copied ? 'check' : 'copy'}
        size="sm"
        iconSize=${13}
        label="Copy URL"
        title="Copy URL (Ctrl+Shift+C)"
        buttonRef=${copyButton}
        onClick=${copy}
      />
    </div>`}
    ${bar.visible &&
    html`<div class=${classNames('url-pill-progress', bar.done && 'is-done')} aria-hidden="true">
      <div class="url-pill-progress-fill" style=${{ transform: `scaleX(${bar.value})` }} />
    </div>`}
    ${boostMenu &&
    html`<${Menu}
      anchor=${boostButton.current}
      placement="bottom-end"
      label="Boosts"
      items=${[
        { type: 'header', label: 'Boosts for this site' },
        ...boosts.map((b) => ({
          key: `boost-${b.id}`,
          label: b.name || b.host,
          checked: b.enabled,
          onSelect: () => fire({ type: 'toggleBoost', id: b.id }),
        })),
      ]}
      onClose=${() => setBoostMenu(false)}
    />`}
  </div>`;
}
