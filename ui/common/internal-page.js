// Shared building blocks for internal pages opened as tabs (settings, archive, history, boosts).
// Styles: /common/internal-page.css. Page-provided strings are rendered as text only.
//
//   import { mountPage, PageHeader, SearchField, ConfirmButton, EmptyState, Segmented, Disclosure,
//            groupByDay, matchesQuery, useListKeyboard, useListRowMotion, useNavIndicator,
//            useStuck } from '/common/internal-page.js';
//
// The pages' own animation keys live here too (`pages.enter`, `pages.listRows`,
// `pages.navIndicator` and the `controls.toggles` disclosure), because every page shares them.

import { html, render, useEffect, useLayoutEffect, useRef, useState } from './vendor/htm-preact.js';
import { startSurface } from './ipc.js';
import { Icon } from './icons.js';
import { Button, Popover, TextField } from './components.js';
import { useLatest } from './hooks.js';
import * as motion from './motion.js';
import { classNames, dayLabel, startOfDay } from './util.js';

/** Animation keys of the internal pages (`crates/sta-core/src/motion.rs`). */
const ENTER_KEY = 'pages.enter';
const ROWS_KEY = 'pages.listRows';
const NAV_KEY = 'pages.navIndicator';
const TOGGLE_KEY = 'controls.toggles';

/** Row ids are joined with this: an id can be a URL, which may hold a comma but never a newline. */
const SEPARATOR = String.fromCharCode(10);

/** At most this many cards take part in a page's enter, and rows in one list change. */
const ENTER_CARDS = 6;
const ROW_CHANGE_LIMIT = 20;

/** The cards `pages.enter` animates, in document order — inside the page, and inside one section. */
const ENTER_SELECTOR_PAGE = '#app .ip-card, #app .ip-group, #app .ip-empty, #app .bst-list, #app .bst-main';
const ENTER_SELECTOR = '.ip-card, .ip-group, .ip-empty, .bst-list, .bst-main';

/**
 * The element a deep link is about to scroll to (`#<section>`, or `?section=<id>` — how the
 * extensions picker links into Settings). The page renders every section into one scroller and
 * scrolls afterwards, so without this the cards that animated would be the ones at the top, which
 * the user never sees, and the section they asked for would arrive with no entrance at all.
 */
function deepLinkTarget() {
  const id = (location.hash.slice(1) || new URLSearchParams(location.search).get('section') || '').trim();
  return id ? document.getElementById(id) : null;
}

/**
 * `pages.enter`: the page's own cards rise into place **once**, when the page first has something to
 * show. That is not "a render": a page's body arrives after its request resolves (the archive list,
 * the history list), and the trigger is the diff from nothing to something, latched so no later
 * render can replay it. At most `ENTER_CARDS` cards take part — the ones the user is about to be
 * looking at — and the whole stagger fits in the 200 ms page-enter budget (FINAL PLAN rule 8).
 */
function PageEnter({ App, state }) {
  const entered = useRef(false);
  useLayoutEffect(() => {
    if (entered.current) return;
    const scope = deepLinkTarget();
    const all = [...document.querySelectorAll(ENTER_SELECTOR_PAGE)];
    const scoped = scope ? [...scope.querySelectorAll(ENTER_SELECTOR)] : [];
    const cards = scoped.length ? scoped : all;
    if (!cards.length) return;
    entered.current = true;
    motion.stagger(
      cards.slice(0, ENTER_CARDS),
      ENTER_KEY,
      [
        { opacity: 0, translate: `0 ${motion.distance(8)}px` },
        { opacity: 1, translate: 'none' },
      ],
      { duration: motion.duration(ENTER_KEY, 200), total: motion.STAGGER_TOTAL_PAGE_MS, step: 28 },
    );
  });
  return html`<${App} state=${state} />`;
}

/**
 * Render `App` with every accepted UiState into `#app` (or `<body>`).
 * @param {(props: {state: any}) => any} App
 */
export function mountPage(App) {
  const mount = document.getElementById('app') ?? document.body;
  return startSurface({ render: (state) => render(html`<${PageEnter} App=${App} state=${state} />`, mount) }).catch((e) =>
    console.error('[page] startup failed', e),
  );
}

/** Page title block: icon tile, title, subtitle, actions on the right. */
export function PageHeader({ icon, title, subtitle, children }) {
  return html`<header class="ip-header">
    ${icon && html`<span class="ip-header-icon" aria-hidden="true"><${Icon} name=${icon} size=${22} /></span>`}
    <div class="ip-header-text">
      <h1 class="ip-title">${title}</h1>
      ${subtitle && html`<p class="ip-subtitle">${subtitle}</p>`}
    </div>
    ${children && html`<div class="ip-header-actions">${children}</div>`}
  </header>`;
}

/**
 * Search input with a leading glyph and a clear button. Ctrl+F and "/" (outside fields) focus it;
 * Esc clears it.
 */
export function SearchField({ value, onInput, placeholder = 'Search', label = 'Search', autoFocus = false }) {
  const ref = useRef(null);
  const latest = useLatest({ value, onInput });
  useEffect(() => {
    const onKey = (e) => {
      const inField = e.target instanceof HTMLElement && e.target.closest('input, textarea, [contenteditable="true"]');
      if ((e.ctrlKey && !e.shiftKey && !e.altKey && (e.key === 'f' || e.key === 'F')) || (e.key === '/' && !inField)) {
        e.preventDefault();
        ref.current?.focus();
        ref.current?.select();
      }
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, []);
  return html`<${TextField}
    class="ip-search"
    inputRef=${ref}
    type="search"
    icon="search"
    clearable
    value=${value}
    placeholder=${placeholder}
    aria-label=${label}
    autoFocus=${autoFocus}
    onInput=${onInput}
    onCancel=${() => {
      if (latest.current.value) latest.current.onInput('');
      else ref.current?.blur();
    }}
  />`;
}

/**
 * Button that asks for confirmation in an anchored popover before running `onConfirm`.
 * @param {{label: string, icon?: string, variant?: string, size?: string, title: string, message?: string, confirmLabel?: string, onConfirm: () => void, disabled?: boolean}} props
 */
export function ConfirmButton({ label, icon, variant = 'default', size = 'md', title, message, confirmLabel, onConfirm, disabled = false }) {
  const [open, setOpen] = useState(false);
  const anchor = useRef(null);
  return html`<${Button} buttonRef=${anchor} variant=${variant} size=${size} icon=${icon} disabled=${disabled} aria-haspopup="dialog" aria-expanded=${String(open)} onClick=${() => setOpen((o) => !o)}>${label}<//>
    <${Popover} open=${open} anchor=${anchor.current} placement="bottom-end" label=${title} class="ip-confirm" onClose=${() => setOpen(false)}>
      <div class="ip-confirm-title">${title}</div>
      ${message && html`<p class="ip-confirm-text">${message}</p>`}
      <div class="ip-confirm-actions">
        <${Button} size="sm" variant="ghost" autofocus onClick=${() => setOpen(false)}>Cancel<//>
        <${Button}
          size="sm"
          variant="danger"
          onClick=${() => {
            setOpen(false);
            onConfirm();
          }}
        >${confirmLabel ?? label}<//>
      </div>
    <//>`;
}

/** Centered empty state with a large glyph. */
export function EmptyState({ icon, title, children }) {
  return html`<div class="ip-empty" role="status">
    ${icon && html`<span class="ip-empty-icon" aria-hidden="true"><${Icon} name=${icon} size=${28} /></span>`}
    <div class="ip-empty-title">${title}</div>
    ${children && html`<div class="ip-empty-text">${children}</div>`}
  </div>`;
}

/**
 * Radio-group styled as a segmented control. ←/→ move the selection.
 * @param {{value: any, options: Array<{value: any, label: string}>, onChange: (v: any) => void, label: string}} props
 */
export function Segmented({ value, options, onChange, label }) {
  const onKeyDown = (e) => {
    const i = options.findIndex((o) => o.value === value);
    let next = -1;
    if (e.key === 'ArrowRight' || e.key === 'ArrowDown') next = Math.min(options.length - 1, i + 1);
    else if (e.key === 'ArrowLeft' || e.key === 'ArrowUp') next = Math.max(0, i - 1);
    else if (e.key === 'Home') next = 0;
    else if (e.key === 'End') next = options.length - 1;
    else return;
    e.preventDefault();
    if (next >= 0 && next !== i) {
      onChange(options[next].value);
      const buttons = e.currentTarget.querySelectorAll('[role="radio"]');
      buttons[next]?.focus();
    }
  };
  return html`<div class="ip-segmented" role="radiogroup" aria-label=${label} onKeyDown=${onKeyDown}>
    ${options.map(
      (o) => html`<button
        key=${String(o.value)}
        type="button"
        role="radio"
        class="ip-segment"
        aria-checked=${String(o.value === value)}
        tabindex=${o.value === value ? 0 : -1}
        onClick=${() => o.value !== value && onChange(o.value)}
      >${o.label}</button>`,
    )}
  </div>`;
}

/**
 * `controls.toggles`: a disclosure whose content grows and shrinks instead of blinking in and out.
 * Height *is* a layout animation, which rule 5 allows only in internal pages — this component is
 * one of them. The trigger is the `open` diff, and the element is `hidden` (so nothing inside it is
 * focusable or announced) whenever it is not opening, open, or closing.
 *
 * @param {{open: boolean, id?: string, class?: string, children?: any}} props
 */
export function Disclosure({ open, id, class: className, children }) {
  const ref = useRef(null);
  const was = useRef(open);
  useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    if (was.current === open) {
      // First run, or a render that did not change the state: keep the attribute honest.
      if (!el.getAnimations().some((a) => a.id === TOGGLE_KEY)) el.hidden = !open;
      return;
    }
    const wasOpen = was.current;
    was.current = open;
    el.hidden = false;
    const height = el.scrollHeight;
    const frames = wasOpen ? [{ height: `${height}px` }, { height: '0px' }] : [{ height: '0px' }, { height: `${height}px` }];
    const anim = motion.animate(el, TOGGLE_KEY, frames, { duration: motion.duration(TOGGLE_KEY, 140) });
    if (!anim) {
      el.hidden = !open;
      return;
    }
    el.style.overflow = 'hidden';
    const settle = () => {
      // A reopen mid-close cancels this one (which rejects `finished`) and starts its own: the
      // newer animation owns the clip and the attribute from here on.
      if (el.getAnimations().some((a) => a.id === TOGGLE_KEY)) return;
      el.style.overflow = '';
      el.hidden = !was.current;
    };
    anim.finished.then(settle, settle);
  });
  return html`<div ref=${ref} id=${id} class=${className}>${children}</div>`;
}

/**
 * Group items by local calendar day, keeping their order.
 * @template T
 * @param {T[]} items
 * @param {(item: T) => number} timeOf Unix ms
 * @returns {Array<{key: number, label: string, items: T[]}>}
 */
export function groupByDay(items, timeOf) {
  const groups = [];
  const byDay = new Map();
  const now = Date.now();
  for (const item of items) {
    const t = timeOf(item);
    const day = startOfDay(t);
    let g = byDay.get(day);
    if (!g) {
      g = { key: day, label: dayLabel(t, { now }), items: [] };
      byDay.set(day, g);
      groups.push(g);
    }
    g.items.push(item);
  }
  return groups;
}

/** Favicon URL the UI's CSP (img-src 'self' data: https:) can load, else null (letter tile). */
export const loadableFavicon = (url) => (/^(https:|data:image\/|sta:)/i.test(url ?? '') ? url : null);

/**
 * Favicons of the tabs in `state` (favorites, pinned and folders, Today and split panes), by
 * `host`. History entries carry no favicon; a page the user still has open lends its icon.
 * @returns {Map<string, string>}
 */
export function faviconsByHost(state) {
  const map = new Map();
  const add = (tab) => {
    if (loadableFavicon(tab?.favicon) && tab.host && !map.has(tab.host)) map.set(tab.host, tab.favicon);
  };
  const walk = (nodes) => {
    for (const node of nodes ?? []) {
      if (node.kind === 'folder') walk(node.children);
      else if (node.kind === 'split') node.panes.forEach(add);
      else add(node);
    }
  };
  (state?.favorites ?? []).forEach(add);
  for (const space of state?.spaces ?? []) {
    walk(space.pinned);
    walk(space.today);
  }
  return map;
}

/** Every whitespace-separated token of `query` appears (case-insensitively) in one of `fields`. */
export function matchesQuery(query, ...fields) {
  const tokens = String(query ?? '').toLowerCase().split(/\s+/).filter(Boolean);
  if (!tokens.length) return true;
  const hay = fields.map((f) => String(f ?? '').toLowerCase()).join('\n');
  return tokens.every((t) => hay.includes(t));
}

/**
 * ↑/↓/Home/End move focus between `[data-row]` elements inside `ref`.
 * @param {{current: HTMLElement|null}} ref
 */
export function useListKeyboard(ref) {
  useEffect(() => {
    const el = ref.current;
    if (!el) return undefined;
    const onKey = (e) => {
      if (!['ArrowDown', 'ArrowUp', 'Home', 'End'].includes(e.key)) return;
      const row = e.target instanceof HTMLElement ? e.target.closest('[data-row]') : null;
      if (!row || e.target !== row) return;
      const rows = [...el.querySelectorAll('[data-row]')];
      const i = rows.indexOf(row);
      const next = e.key === 'ArrowDown' ? i + 1 : e.key === 'ArrowUp' ? i - 1 : e.key === 'Home' ? 0 : rows.length - 1;
      if (rows[next]) {
        e.preventDefault();
        rows[next].focus();
      }
    };
    el.addEventListener('keydown', onKey);
    return () => el.removeEventListener('keydown', onKey);
  }, [ref.current]);
}

/**
 * `pages.listRows`: animate what changed in a list of `[data-row][data-id]` rows.
 *
 * Call it in the component **body** with the ids it is about to render: Preact runs the body before
 * it patches the DOM, so the rows that are about to leave can still be measured and cloned, and the
 * FLIP capture sees the "first" rects. Nothing happens on the first list (that is the page's own
 * enter), when more than `ROW_CHANGE_LIMIT` rows changed (a search, a "clear all": not a change the
 * eye can follow), or while the page is hidden — `motion.enabled` refuses then anyway.
 *
 * @param {{current: HTMLElement|null}} ref the element that contains the rows
 * @param {Array<string|number>} ids the row ids of this render, in order
 */
export function useListRowMotion(ref, ids) {
  const previous = useRef(null);
  const pending = useRef(null);
  const signature = ids.join(SEPARATOR);
  const last = previous.current;
  previous.current = signature;
  const root = ref.current;
  if (last != null && last !== signature && root && motion.enabled(ROWS_KEY)) {
    const before = new Set(last.split(SEPARATOR).filter(Boolean));
    const now = new Set(ids.map(String));
    const added = [...now].filter((id) => !before.has(id));
    const removed = [...before].filter((id) => !now.has(id));
    if (added.length + removed.length <= ROW_CHANGE_LIMIT) {
      // The ghosts first, while the rows are still in the document and in place.
      for (const id of removed) {
        const row = root.querySelector(`[data-row][data-id="${CSS.escape(id)}"]`);
        motion.fadeGhost(
          motion.ghost(row),
          ROWS_KEY,
          [
            { opacity: 1, translate: 'none' },
            { opacity: 0, translate: `${motion.distance(12)}px 0` },
          ],
          { duration: motion.duration(ROWS_KEY, 160) },
        );
      }
      pending.current = {
        added,
        flip: motion.flip.capture(root, ROWS_KEY, '[data-row][data-id]', { changed: added.length + removed.length }),
      };
    }
  }
  useLayoutEffect(() => {
    const job = pending.current;
    pending.current = null;
    if (!job || !ref.current) return;
    job.flip?.play({ duration: motion.duration(ROWS_KEY, 160) });
    const fresh = job.added
      .map((id) => ref.current.querySelector(`[data-row][data-id="${CSS.escape(id)}"]`))
      .filter(Boolean);
    motion.stagger(
      fresh,
      ROWS_KEY,
      [
        { opacity: 0, translate: `0 ${motion.distance(-6)}px` },
        { opacity: 1, translate: 'none' },
      ],
      { duration: motion.duration(ROWS_KEY, 160) },
    );
  });
}

/**
 * `pages.navIndicator`: one element that marks the current item of a list of links or buttons and
 * glides to it. The indicator is placed from the active element's own layout box, so it is always
 * exactly right — the animation is only the travel between two of those places, and it snaps at
 * `reduced` (`motion.glide`). Its container must be `position: relative`.
 *
 * It runs after every render rather than on a selection prop, so a resize or a list that grew leaves
 * the indicator exactly on its item; `motion.glide` animates only when the place actually changed,
 * which is the keyed diff here.
 *
 * @param {{current: HTMLElement|null}} listRef the positioned container
 * @param {{current: HTMLElement|null}} indicatorRef the indicator element inside it
 * @param {string} activeSelector selector of the active item, e.g. `.set-nav-link.is-active`
 */
export function useNavIndicator(listRef, indicatorRef, activeSelector) {
  useLayoutEffect(() => {
    const list = listRef.current;
    const bar = indicatorRef.current;
    if (!list || !bar) return;
    const el = list.querySelector(activeSelector);
    bar.style.display = el ? '' : 'none';
    if (!el) return;
    bar.style.width = `${el.offsetWidth}px`;
    bar.style.height = `${el.offsetHeight}px`;
    motion.glide(bar, NAV_KEY, { x: el.offsetLeft, y: el.offsetTop }, { duration: motion.duration(NAV_KEY, 160) });
  });
}

/** `true` while the element (a sticky toolbar) is stuck to the top of the scrolling page. */
export function useStuck(ref) {
  const [stuck, setStuck] = useState(false);
  useEffect(() => {
    const onScroll = () => {
      const el = ref.current;
      if (!el) return;
      const next = el.getBoundingClientRect().top <= 0.5 && window.scrollY > 0;
      setStuck((s) => (s === next ? s : next));
    };
    onScroll();
    window.addEventListener('scroll', onScroll, { passive: true, capture: true });
    return () => window.removeEventListener('scroll', onScroll, { capture: true });
  }, []);
  return stuck;
}

export { classNames };
