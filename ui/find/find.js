// Find bar overlay (PROTOCOL §3 seq handling, §4; arc_spec §2.27). A 360×44 card at the top-right of
// the focused pane. The input is uncontrolled so typing never waits for a render.
//
// - `find.seq` changes: prefill `find.text`, select all, focus. Opening the bar for a tab (closed →
//   open, or another tab) re-runs the remembered search so the counter and highlights come back.
// - input → `findInPage {text, matchCase}`; Enter / Shift+Enter and ↓ / ↑ → `findNext {forward}`.
// - `find.result {tab, count, active, final}` events drive the "n/m" counter.
// - Esc and × → `closeFind`.

import { html, render } from '/common/vendor/htm-preact.js';
import { dispatch, on, setSurfaceSize, startSurface } from '/common/ipc.js';
import { IconButton } from '/common/components.js';
import * as motion from '/common/motion.js';
import { classNames } from '/common/util.js';

const mount = document.getElementById('app');
/** The animation key of every motion on this surface (`crates/sta-core/src/motion.rs`). */
const KEY = 'overlays.find';

const model = {
  find: null,
  handledSeq: null,
  /** Tab the bar was last opened for (re-search when it changes). */
  openTab: null,
  text: '',
  matchCase: false,
  /** Latest `find.result` for the current tab: `{count, active, final}`. */
  result: null,
  /** The query this bar has already reported as having no matches (see `shakeIfEmpty`). */
  emptyFor: null,
};

/** @type {HTMLInputElement|null} */
let inputEl = null;
const setInputRef = (el) => {
  inputEl = el;
};
/** @type {HTMLElement|null} the bar itself, which is what fades in and shakes */
let barEl = null;
const setBarRef = (el) => {
  barEl = el;
};
/** The IME is composing right now: nothing may move the bar (it anchors the candidate window). */
let composing = false;

const report = (e) => console.error('[find]', e);

/**
 * `overlays.find`: the bar fades its content in when it opens. The bar holds a focused text input,
 * so this is **opacity only** — a transform would move the caret and the IME candidate window.
 */
function fadeIn() {
  // The bar was left blank by the last page-initiated close (`close`): it has something to show again.
  motion.unblank();
  motion.animate(barEl, KEY, [{ opacity: 0 }, { opacity: 1 }], { duration: motion.duration(KEY, 120) });
}

/**
 * A shake for "this search has no matches", and only for a deliberate ask — Enter, Shift+Enter, ↑/↓
 * or F3 — never while the user is still typing: a `find.result` arrives on *every* keystroke, and a
 * bar that shakes as you type moves the caret with it.
 *
 * F3 is a shell accelerator (`keyboard.rs`), so the page never sees the key: what marks an ask as
 * deliberate is that the search is one this bar has **already** reported as empty (`emptyFor`). The
 * first final zero-result for a new query only records it.
 *
 * Suppressed while the IME is composing, for the same reason the enter animation never transforms
 * the bar: it anchors the candidate window.
 */
function shakeIfEmpty() {
  if (composing || !motion.moves(KEY)) return;
  const d = motion.distance(3);
  motion.animate(
    barEl,
    KEY,
    [
      { translate: '0' },
      { translate: `${-d}px 0`, offset: 0.25 },
      { translate: `${d}px 0`, offset: 0.55 },
      { translate: `${-d * 0.5}px 0`, offset: 0.8 },
      { translate: '0' },
    ],
    { duration: motion.duration(KEY, 120) * 2, easing: 'ease-in-out' },
  );
}

function search(text, { matchCase = model.matchCase } = {}) {
  model.text = text;
  model.matchCase = matchCase;
  if (!text) model.result = null;
  dispatch({ type: 'findInPage', text, forward: true, matchCase, findNext: false }).catch(report);
  rerender();
}

function findNext(forward) {
  if (!model.text) return;
  // Already known to have no matches: the answer is instant, so shake without waiting for a result.
  if (model.emptyFor === model.text) shakeIfEmpty();
  dispatch({ type: 'findNext', forward }).catch(report);
}

/**
 * Esc and ×. This overlay is activatable: the shell hides it the moment the command arrives, with no
 * ack and no linger, so the last frame this renderer produced is the frame the *next* Ctrl+F would
 * show — the previous search, in the previous tab. The blank frame goes out first (FINAL PLAN §1.3).
 */
async function close() {
  await motion.closeBlank(barEl);
  dispatch({ type: 'closeFind' }).catch(report);
}

function onInput() {
  search(inputEl.value);
}

function onKeyDown(event) {
  if (event.isComposing) return;
  if (event.key === 'Enter') {
    findNext(!event.shiftKey);
  } else if (event.key === 'Escape') {
    close();
  } else if (event.key === 'ArrowDown' && !event.altKey) {
    findNext(true);
  } else if (event.key === 'ArrowUp' && !event.altKey) {
    findNext(false);
  } else if (event.altKey && (event.key === 'c' || event.key === 'C')) {
    search(model.text, { matchCase: !model.matchCase });
  } else {
    return;
  }
  event.preventDefault();
}

function onState(state) {
  const find = state.find ?? null;
  model.find = find;
  // The overlay view stays alive between searches; while no bar is up it renders no frames, so an
  // animation created there would stay pending and play on the next reveal.
  motion.setPresented(Boolean(find));
  if (!find) {
    if (model.openTab !== null) {
      model.openTab = null;
      model.result = null;
      model.emptyFor = null;
    }
    rerender();
    return;
  }
  const opening = find.seq !== model.handledSeq;
  if (find.seq !== model.handledSeq) {
    model.handledSeq = find.seq;
    const reopened = model.openTab !== find.tab;
    model.openTab = find.tab;
    model.matchCase = Boolean(find.matchCase);
    if (inputEl) {
      inputEl.value = find.text ?? '';
      inputEl.focus({ preventScroll: true });
      inputEl.select();
    }
    model.text = find.text ?? '';
    if (reopened) {
      model.result = null;
      if (model.text) search(model.text, { matchCase: model.matchCase });
    }
  } else if (Boolean(find.matchCase) !== model.matchCase && find.text === model.text) {
    model.matchCase = Boolean(find.matchCase);
  }
  rerender();
  // After the render, so the fade runs on the bar this `seq` brought up.
  if (opening) fadeIn();
}

on('find.result', (payload) => {
  if (!payload || !model.find || payload.tab !== model.find.tab) return;
  model.result = { count: payload.count ?? 0, active: payload.active ?? 0, final: Boolean(payload.final) };
  if (!model.result.final) {
    rerender();
    return;
  }
  if (model.result.count > 0) {
    model.emptyFor = null;
  } else if (model.text) {
    // A repeat of a search this bar already said was empty: the user asked again (Enter, F3).
    if (model.emptyFor === model.text) shakeIfEmpty();
    else model.emptyFor = model.text;
  }
  rerender();
});

// Buttons must not take focus from the input.
const keepFocus = (event) => event.preventDefault();

function FindBar({ text, matchCase, result }) {
  const hasText = text.length > 0;
  const count = result?.count ?? 0;
  const noMatches = hasText && result && result.final && count === 0;
  const counter = !hasText || !result ? '' : `${count ? Math.max(result.active, 1) : 0}/${count}`;
  return html`<div class=${classNames('find', noMatches && 'is-empty')} ref=${setBarRef} onMouseDown=${(e) => e.target !== inputEl && keepFocus(e)}>
    <input
      id="input"
      ref=${setInputRef}
      class="find-input"
      type="text"
      autocomplete="off"
      spellcheck=${false}
      placeholder="Find in page"
      aria-label="Find in page"
      onInput=${onInput}
      onKeyDown=${onKeyDown}
    />
    <span class="find-count" role="status" aria-live="polite">${counter}</span>
    <span class="find-sep" aria-hidden="true" />
    <${IconButton}
      icon="case"
      label="Match case (Alt+C)"
      iconSize=${16}
      class="find-btn"
      pressed=${matchCase}
      onMouseDown=${keepFocus}
      onClick=${() => search(inputEl?.value ?? model.text, { matchCase: !matchCase })}
    />
    <${IconButton} icon="chevron-up" label="Previous match (Shift+Enter)" class="find-btn" disabled=${!count} onMouseDown=${keepFocus} onClick=${() => findNext(false)} />
    <${IconButton} icon="chevron-down" label="Next match (Enter)" class="find-btn" disabled=${!count} onMouseDown=${keepFocus} onClick=${() => findNext(true)} />
    <${IconButton} icon="close" label="Close (Esc)" class="find-btn" onMouseDown=${keepFocus} onClick=${close} />
  </div>`;
}

function rerender() {
  render(html`<${FindBar} text=${model.text} matchCase=${model.matchCase} result=${model.result} />`, mount);
}

window.addEventListener('focus', () => {
  if (inputEl && document.activeElement !== inputEl) inputEl.focus({ preventScroll: true });
});

// Composition state is read from the window, not from a prop on the input: whatever inside the bar
// the IME is composing in, nothing may move while it is (the candidate window is anchored to the
// caret). A composition starting mid-shake settles it at its end state rather than leaving it off.
window.addEventListener('compositionstart', () => {
  composing = true;
  motion.finishAll(KEY);
});
window.addEventListener('compositionend', () => {
  composing = false;
});

rerender();
// Inside the shell's rounded card (html.native-card), which adds 8px left and right and 4px above
// and below, the page is 344×36 so the card stays 360×44.
const nativeCard = document.documentElement.classList.contains('native-card');
setSurfaceSize(nativeCard ? { height: 36, width: 344 } : { height: 44, width: 360 }).catch(report);
startSurface({ render: onState }).catch(report);
