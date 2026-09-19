// Ctrl+Tab recent-tab switcher overlay (PROTOCOL §4, arc_spec §2.13). Up to 5 cards (132×150):
// a large favicon on the tab's space colors, title, host and space icon. The overlay can't take
// focus (the Ctrl key-up must reach the focused browser), so it is keyboard-passive; clicking a
// card selects and commits it. The page reports its own width and height.
//
// Its exit is **acknowledged**: the shell asks the page to blank (`surface.exit`) and hides the
// overlay when the page reports that frame, so the next hold never flashes the previous cards.

import { html, render, useLayoutEffect, useRef } from '/common/vendor/htm-preact.js';
import { dispatch, onSurfaceExit, startSurface, trackSurfaceSize } from '/common/ipc.js';
import { Icon } from '/common/icons.js';
import { AudioBars, Favicon } from '/common/components.js';
import * as motion from '/common/motion.js';
import { activeSpace, classNames } from '/common/util.js';

const mount = document.getElementById('app');
const report = (e) => console.error('[switcher]', e);
/** The animation key of every motion on this surface (`crates/sta-core/src/motion.rs`). */
const KEY = 'overlays.switcher';

/** Favicon URL the UI's CSP (img-src 'self' data: https:) can load, else null (letter tile). */
const loadableFavicon = (url) => (/^(https:|data:image\/|sta:)/i.test(url ?? '') ? url : null);

let lastState = null;

async function choose(index) {
  try {
    await dispatch({ type: 'mruSelect', index });
    await dispatch({ type: 'mruCommit' });
  } catch (e) {
    report(e);
  }
}

/** Glyphs for internal pages (they have no favicon). */
const INTERNAL_PAGE_GLYPHS = { settings: 'settings', archive: 'archive', history: 'history', boosts: 'boost' };

function internalGlyph(url) {
  const m = /^sta:\/\/([a-z]+)/i.exec(url ?? '');
  return m ? INTERNAL_PAGE_GLYPHS[m[1].toLowerCase()] : null;
}

function Card({ tab, index, selected, space }) {
  const colors = space?.colors;
  const glyph = !tab.favicon && internalGlyph(tab.url);
  const art = colors
    ? { background: `linear-gradient(145deg, ${colors.gradientStart}, ${colors.gradientEnd})` }
    : undefined;
  return html`<button
    type="button"
    class=${classNames('sw-card', selected && 'is-selected', tab.loading && 'is-loading')}
    role="option"
    aria-selected=${String(selected)}
    title=${tab.url}
    tabindex="-1"
    onClick=${() => choose(index)}
  >
    <span class="sw-art" style=${art}>
      <span class=${classNames('sw-fav', glyph && 'is-glyph')}>
        ${glyph ? html`<${Icon} name=${glyph} size=${26} />` : html`<${Favicon} src=${loadableFavicon(tab.favicon)} host=${tab.host} size=${32} lazy=${false} />`}
      </span>
      ${tab.audible &&
      html`<span class="sw-audio" aria-label=${tab.muted ? 'Muted' : 'Playing audio'}>
        ${tab.muted ? html`<${Icon} name="speaker-muted" size=${12} />` : html`<${AudioBars} size=${12} />`}
      </span>`}
    </span>
    <span class="sw-meta">
      <span class="sw-title">${tab.title || tab.host || tab.url}</span>
      <span class="sw-sub">
        ${tab.section === 'favorites'
          ? html`<${Icon} class="sw-space-glyph" name="star" size=${11} />`
          : space && html`<span class="sw-space emoji" title=${space.name}>${space.icon}</span>`}
        <span class="sw-host">${tab.host}</span>
      </span>
    </span>
  </button>`;
}

/** `switcher.tabs` the cards' fade already ran for; `null` while the switcher is closed. */
let fadedFor = null;

function Switcher({ state }) {
  // First, so the fade below already sees the surface as presented.
  const switcher = state?.switcher;
  motion.usePresence(Boolean(switcher));
  const rootRef = useRef(null);
  const ringRef = useRef(null);
  useLayoutEffect(() => trackSurfaceSize(rootRef.current, { width: true }), []);
  const tabs = switcher?.tabs ?? [];
  const fallbackSpace = activeSpace(state);
  const signature = tabs.map((t) => t.id).join(',');

  // The cards fade in together (100 ms). A stagger would add ~265 ms to an overlay that already
  // waits 250 ms for the Ctrl hold, so the whole list is one fade — and it is keyed on the *list*,
  // never on a render: holding Ctrl and stepping through the cards must not re-fade them.
  useLayoutEffect(() => {
    if (!switcher || fadedFor === signature) return;
    fadedFor = signature;
    // Held again while the overlay was lingering out: the cards and the ring come back from the blank
    // frame this page presented for the shell.
    motion.unblank();
    motion.stagger(rootRef.current?.querySelectorAll('.sw-card') ?? [], KEY, [{ opacity: 0 }, { opacity: 1 }], {
      duration: motion.duration(KEY, 100),
      step: 0,
    });
  });

  // The selection ring is one element that glides between cards, so the accent does not jump from
  // box to box. It is placed from the selected card's own layout box, so it is right even when the
  // glide is refused (`reduced`, the key off).
  useLayoutEffect(() => {
    const ring = ringRef.current;
    const card = rootRef.current?.querySelectorAll('.sw-card')[switcher?.selected ?? -1] ?? null;
    if (!ring) return;
    ring.style.display = card ? '' : 'none';
    if (!card) return;
    ring.style.width = `${card.offsetWidth}px`;
    ring.style.height = `${card.offsetHeight}px`;
    motion.glide(ring, KEY, { x: card.offsetLeft, y: card.offsetTop }, { duration: motion.duration(KEY, 100) });
  });

  return html`<div class="sw" ref=${rootRef} role="listbox" aria-label="Recent tabs">
    <span key="ring" class="sw-ring" ref=${ringRef} aria-hidden="true" />
    ${tabs.map((tab, index) => {
      const space = state.spaces?.find((s) => s.id === tab.space) ?? fallbackSpace;
      return html`<${Card} key=${tab.id} tab=${tab} index=${index} selected=${index === switcher.selected} space=${space} />`;
    })}
  </div>`;
}

function onState(state) {
  lastState = state;
  // The overlay stays alive (hidden) between holds, so forget the last list: the next one is a
  // switcher *opening*, not a selection moving inside one.
  if (!state?.switcher) fadedFor = null;
  render(html`<${Switcher} state=${lastState} />`, mount);
}

// The shell hides this overlay only after the cards are gone from the screen (PROTOCOL §14). The
// `.sw` root is what `trackSurfaceSize` measures, so what fades is what paints: the cards and the
// selection ring inside it.
onSurfaceExit(() => motion.blank(mount.querySelectorAll('.sw-card, .sw-ring'), KEY));

startSurface({ render: onState }).catch(report);
