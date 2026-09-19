// Command bar overlay (docs/PROTOCOL.md §6, arc_spec §5 "Command Bar visuals", §6).
//
// The input is uncontrolled (Preact never writes its value), so typing never waits for a render
// or an IPC round trip. Every input event sends `omnibox.query` with an increasing `seq`; stale
// responses are dropped. Core decides everything about results: the page only renders them and
// commits `results[sel].command` (or `altCommand`) with `commitOmnibox`.
//
// Tab / Shift+Tab (and a click on the mode chip) toggle actions mode, keeping the typed text.
//
// Search suggestions (outside actions mode, `settings.searchSuggestions` on): each query is sent
// right away with the suggestions already known for the text (an exact cache hit, else the longest
// cached prefix's suggestions that still start with the text), so the inline completion stays put
// while typing. ~80 ms after the last change `omnibox.suggest` asks the shell for the text's
// remote suggestions; a reply for the text still in the input is cached (LRU) and re-runs the
// query, keeping a row the user selected. Replies for older texts are ignored.

import { html, render, useLayoutEffect, useRef } from '/common/vendor/htm-preact.js';
import { dispatch, invoke, isMock, setSurfaceSize, startSurface } from '/common/ipc.js';
import { Icon } from '/common/icons.js';
import { Favicon, Kbd } from '/common/components.js';
import { classNames } from '/common/util.js';
import * as motion from '/common/motion.js';

/** Fallback input row height (`--cmd-input-h`; 44 inside the shell's rounded card). */
const INPUT_H = 56;
const MAX_VISIBLE_ROWS = 8;

/** Quiet time after the last edit before remote suggestions are requested. */
const SUGGEST_DEBOUNCE_MS = 80;
/** Suggestion replies kept while the bar is open (least recently used first out). */
const SUGGEST_CACHE_SIZE = 50;
/** The shell never fetches suggestions for longer input. */
const SUGGEST_MAX_TEXT = 256;

const GROUP_LABELS = {
  recentTabs: 'Recent Tabs',
  suggestedActions: 'Suggested',
  tabs: 'Tabs',
  actions: 'Actions',
  spaces: 'Spaces',
  history: 'History',
  suggestions: 'Suggestions',
  archive: 'Archive',
  // Ctrl+E (extensions mode)
  extensions: 'Extensions',
  needsOk: 'Needs your OK',
  extensionsOff: 'Off',
  more: 'More',
};

const SIDE_LABELS = { left: 'Left', right: 'Right', top: 'Top', bottom: 'Bottom' };

const MODE_LABELS = { newTab: 'New Tab', editUrl: 'Edit URL', split: 'Split', actions: 'Actions', extensions: 'Extensions' };

const PLACEHOLDERS = {
  newTab: 'Search or enter URL…',
  editUrl: 'Search or edit URL…',
  split: 'Search or enter URL to split…',
  actions: 'Search actions…',
  extensions: 'Search extensions…',
};

/**
 * Schemes whose `scheme:` prefix makes typed text an address even without `//`: the shell never
 * sends such text for suggestions (crates/sta/src/suggest.rs `url_like`).
 */
const ADDRESS_SCHEMES = new Set([
  'http', 'https', 'file', 'sta', 'about', 'data', 'view-source', 'chrome', 'chrome-error', 'devtools',
  'chrome-devtools', 'javascript', 'blob', 'filesystem', 'mailto', 'tel', 'sms', 'ftp', 'ws', 'wss',
]);

/** Animation keys of this surface (`crates/sta-core/src/motion.rs`). */
const KEY_OPEN = 'commandBar.open';
const KEY_RESULTS = 'commandBar.results';
const KEY_SELECTION = 'commandBar.selection';
const KEY_MODE = 'commandBar.modeToggle';
/** Longest the first results' stagger may span in total (FINAL PLAN rule 8 / §2). */
const RESULTS_STAGGER_MS = 100;

/** Favicon URL the UI's CSP (img-src 'self' data: https:) can load, else null (letter tile). */
const loadableFavicon = (url) => (/^(https:|data:image\/|sta:)/i.test(url ?? '') ? url : null);

// ------------------------------------------------------------------------------------ text helpers

/**
 * `text` starts with `prefix`, compared per code point and case-insensitively (never splits a
 * surrogate pair). Returns the number of code points of `text` the prefix covers, or -1.
 */
function foldedPrefixLength(text, prefix) {
  const a = [...text];
  const b = [...prefix];
  if (b.length > a.length) return -1;
  for (let i = 0; i < b.length; i++) {
    if (a[i] !== b[i] && a[i].toLowerCase() !== b[i].toLowerCase()) return -1;
  }
  return b.length;
}

/** An address with an explicit scheme (`https://x`, `mailto:a@b`, `app://…`) or a Windows path. */
function looksLikeUrl(text) {
  const t = text.trimStart();
  if (/^[a-z]:[\\/]/i.test(t) || t.startsWith('\\\\')) return true;
  const m = /^([a-z][a-z0-9+.-]+):/i.exec(t);
  return !!m && (ADDRESS_SCHEMES.has(m[1].toLowerCase()) || t.startsWith('//', m[0].length));
}

/** The search terms of typed text: a leading `?` (forced search) is not part of them. */
const searchTerms = (text) => text.trimStart().replace(/^\?/, '').trimStart();

// ------------------------------------------------------------------------------------ model

const model = {
  /** Last `state.commandBar` (null while closed). */
  bar: null,
  /** `commandBar.seq` already applied to the input. */
  handledSeq: null,
  /**
   * Mode toggled with Tab / the chip (until the next seq): `'actions'`, or `'newTab'` when a bar
   * opened in actions mode was toggled off. `null` = the mode the bar was opened in.
   */
  localMode: null,
  /** Text the user typed, without the inline completion. */
  typed: '',
  /** Full text currently shown with an inline completion (`typed` + selected remainder). */
  completion: null,
  querySeq: 0,
  appliedSeq: 0,
  /** Promise of the latest query. */
  pending: null,
  /** `preventInlineAutocomplete` of the latest query (kept when suggestions re-run it). */
  preventInline: false,
  response: { text: '', seq: 0, inlineCompletion: null, results: [] },
  sel: 0,
  /** The user moved the selection (keys or pointer) since the text or mode last changed. */
  selMoved: false,
  /** Scroll the selected row into view after the next render. */
  revealSel: false,
  /** Commit in flight: ignore repeated Enter until the bar closes / reopens. */
  committing: false,
  commitTimer: 0,
  /** Animate the card in after the next render. */
  animateIn: false,
  /** Stagger the first results of a freshly opened bar in after the next render. */
  animateResults: false,
  /** What moved the selection last: only the keyboard glides, a hover snaps (critique issue 14). */
  selBy: 'key',
  /** Where the selection glider stands, in `.cmd-list` coordinates (null = no rows). */
  gliderAt: null,
  /** The mode the chip currently shows, so a change can crossfade. `null` = nothing shown yet. */
  shownMode: null,
  /** Flipped on every mode change, to restart the placeholder's own fade (`data-ph`). */
  phMode: null,
  phFlip: false,
};

const suggest = {
  /** `state.settings.searchSuggestions`. */
  enabled: false,
  /** `state.settings.searchEngine` the cache belongs to. */
  engine: null,
  /** typed text → suggestions (Map order = least recently used first). */
  cache: new Map(),
  timer: 0,
  /** Bumped whenever in-flight replies must be ignored (bar closed, settings changed). */
  generation: 0,
  /** The shell doesn't know `omnibox.suggest` (404): stop asking. */
  unavailable: false,
  /** Requests sent / replies ignored as stale (test hooks). */
  requests: 0,
  stale: 0,
};

/** @type {HTMLInputElement|null} */
let inputEl = null;
const setInputRef = (el) => {
  inputEl = el;
};

const mount = document.getElementById('app');

/** The mode the bar was opened in. */
function baseMode() {
  return model.bar?.mode ?? 'newTab';
}

function currentMode() {
  return model.localMode ?? baseMode();
}

/** The mode shown in the chip (a `>` prefix is actions mode in core too). */
function displayMode() {
  const mode = currentMode();
  return mode !== 'actions' && model.typed.trimStart().startsWith('>') ? 'actions' : mode;
}

// ------------------------------------------------------------------------------------ suggestions

/** Whether remote suggestions apply to `text` in the current mode. */
function wantsSuggestions(text) {
  return (
    suggest.enabled &&
    !suggest.unavailable &&
    displayMode() !== 'actions' &&
    searchTerms(text).trim() !== '' &&
    (text.length <= SUGGEST_MAX_TEXT || [...text].length <= SUGGEST_MAX_TEXT) &&
    !looksLikeUrl(text)
  );
}

/** Suggestions to send with a query for `text`: exact cache hit, else the longest cached prefix's, filtered. */
function suggestionsFor(text) {
  if (!wantsSuggestions(text)) return [];
  const exact = suggest.cache.get(text);
  if (exact) {
    suggest.cache.delete(text);
    suggest.cache.set(text, exact);
    return exact;
  }
  const terms = searchTerms(text);
  const chars = [...text];
  for (let end = chars.length - 1; end > 0; end--) {
    const cached = suggest.cache.get(chars.slice(0, end).join(''));
    if (cached) return cached.filter((s) => foldedPrefixLength(s, terms) >= 0);
  }
  return [];
}

function cacheSuggestions(text, suggestions) {
  suggest.cache.delete(text);
  suggest.cache.set(text, suggestions);
  while (suggest.cache.size > SUGGEST_CACHE_SIZE) suggest.cache.delete(suggest.cache.keys().next().value);
}

function cancelSuggest() {
  clearTimeout(suggest.timer);
  suggest.timer = 0;
  suggest.generation++;
}

/** Ask for `text`'s suggestions once the input has been quiet for a moment (not for cached text). */
function scheduleSuggest(text) {
  clearTimeout(suggest.timer);
  suggest.timer = 0;
  if (!wantsSuggestions(text) || suggest.cache.has(text)) return;
  suggest.timer = setTimeout(() => fetchSuggestions(text), SUGGEST_DEBOUNCE_MS);
}

function fetchSuggestions(text) {
  suggest.timer = 0;
  if (text !== model.typed || !wantsSuggestions(text)) return;
  const generation = suggest.generation;
  suggest.requests++;
  invoke('omnibox.suggest', { text }).then(
    (reply) => onSuggestReply(text, reply, generation),
    (error) => {
      if (error?.code === 404) {
        suggest.unavailable = true;
        console.warn('[command] omnibox.suggest is not available: no search suggestions');
      } else {
        console.warn('[command] omnibox.suggest failed', error);
      }
    },
  );
}

/**
 * A suggestion reply for `text`. Used only while `text` is still what the input holds (and the bar
 * wasn't closed or reconfigured meanwhile): cached, then the query re-runs with it.
 * @returns {boolean} whether the reply was used
 */
function onSuggestReply(text, reply, generation = suggest.generation) {
  const list = Array.isArray(reply?.suggestions) ? reply.suggestions.filter((s) => typeof s === 'string') : null;
  if (!list || generation !== suggest.generation || text !== model.typed || (typeof reply.text === 'string' && reply.text !== text)) {
    suggest.stale++;
    return false;
  }
  cacheSuggestions(text, list);
  if (wantsSuggestions(text)) runQuery({ preventInline: model.preventInline });
  return true;
}

/** Follow the suggestion settings; `true` when the current results depend on a change. */
function syncSuggestSettings(settings) {
  const enabled = settings?.searchSuggestions === true;
  const engine = settings?.searchEngine ?? null;
  let changed = false;
  if (engine !== suggest.engine) {
    if (suggest.engine !== null) {
      suggest.cache.clear();
      changed = true;
    }
    suggest.engine = engine;
    cancelSuggest();
  }
  if (enabled !== suggest.enabled) {
    suggest.enabled = enabled;
    if (!enabled) suggest.cache.clear();
    cancelSuggest();
    changed = true;
  }
  return changed;
}

// ------------------------------------------------------------------------------------ queries

/**
 * Browsing the actions list (actions mode, or a lone ">", with nothing typed to filter by):
 * `omnibox.query` caps its results, so the full, A–Z sorted `omnibox.actions` list is shown
 * instead and every action stays reachable by scrolling.
 */
function browsingActions() {
  const text = model.typed.trim();
  const mode = currentMode();
  return (mode === 'actions' && (text === '' || text === '>')) || (mode !== 'split' && text === '>');
}

function runQuery({ preventInline = false } = {}) {
  const seq = ++model.querySeq;
  model.preventInline = preventInline;
  const request = {
    text: model.typed,
    mode: currentMode(),
    splitSide: model.bar?.splitSide ?? null,
    preventInlineAutocomplete: preventInline,
    suggestions: suggestionsFor(model.typed),
    seq,
  };
  scheduleSuggest(model.typed);
  const query = browsingActions()
    ? invoke('omnibox.actions').then(
        (results) => ({ text: request.text, seq, inlineCompletion: null, results: Array.isArray(results) ? results : [] }),
        // A backend without the request (older mock) still gets the capped query results.
        () => invoke('omnibox.query', request),
      )
    : invoke('omnibox.query', request);
  const promise = query.then(
    (response) => {
      if (response && response.seq === model.querySeq) applyResponse(response);
      return response;
    },
    (error) => {
      console.error('[command] omnibox.query failed', error);
      if (seq === model.querySeq) model.appliedSeq = seq;
      return null;
    },
  );
  model.pending = promise;
  return promise;
}

function applyResponse(response) {
  const previous = model.response;
  const previousKey = previous.results[model.sel]?.key;
  model.response = { ...response, results: Array.isArray(response.results) ? response.results : [] };
  model.appliedSeq = response.seq;

  // Results refreshed for the same text (e.g. suggestions arrived): keep a row the user moved to.
  // Otherwise the default row.
  let sel = 0;
  if (model.selMoved && response.text === previous.text && previousKey != null) {
    const i = model.response.results.findIndex((r) => r.key === previousKey);
    if (i >= 0) sel = i;
  }
  model.sel = sel;

  const input = inputEl;
  const typed = model.typed;
  const completion = response.inlineCompletion;
  if (input && response.text === typed) {
    if (completion && completion.length > typed.length && completion.startsWith(typed)) {
      const caretAtEnd = input.value === typed && input.selectionStart === typed.length && input.selectionEnd === typed.length;
      const showingOptimistic = model.completion !== null && input.value === model.completion;
      if (caretAtEnd || showingOptimistic) {
        input.value = completion;
        input.setSelectionRange(typed.length, completion.length);
        model.completion = completion;
      }
    } else if (model.completion !== null && input.value === model.completion) {
      input.value = typed;
      input.setSelectionRange(typed.length, typed.length);
      model.completion = null;
    }
  }
  rerender();
}

// ------------------------------------------------------------------------------------ actions

function releaseCommit() {
  clearTimeout(model.commitTimer);
  model.committing = false;
}

/** What this page paints: the card. `motion.closeBlank` empties it before a close (FINAL PLAN §1.3). */
const cardEl = () => mount?.querySelector('.cmd') ?? null;

/**
 * Close the bar the page is showing. This overlay is activatable: the shell hides it the moment the
 * command arrives, with no ack and no linger, so the last frame this renderer produced is the one the
 * next Ctrl+T would show — the blank frame goes out first, and the `seq` is the bar's *now*, before
 * the frame is waited for (docs/PROTOCOL.md §8: a close must never reach the bar that replaced it).
 */
async function closeBar() {
  const seq = model.bar?.seq ?? null;
  await motion.closeBlank(cardEl());
  dispatch({ type: 'closeCommandBar', seq }).catch((e) => console.error('[command] closeCommandBar failed', e));
}

async function commit(index, alt) {
  if (model.committing) return;
  model.committing = true;
  clearTimeout(model.commitTimer);
  // Safety net: never stay locked if the state push doesn't arrive.
  model.commitTimer = setTimeout(releaseCommit, 1500);
  let target = index;
  if (model.appliedSeq !== model.querySeq && model.pending) {
    // Enter right after typing: wait for the results of what is actually in the input.
    await model.pending;
    target = model.sel;
  }
  const result = model.response.results[target];
  if (!result) {
    releaseCommit();
    return;
  }
  const command = alt ? (result.altCommand ?? result.command) : result.command;
  // A commit that closes the bar is a page-initiated close: the frame that shows nothing goes out
  // first (FINAL PLAN §1.3). Alt+Enter keeps the bar open, so it keeps its content.
  if (!alt) await motion.closeBlank(cardEl());
  try {
    await dispatch({ type: 'commitOmnibox', command, alt: Boolean(alt) });
  } catch (error) {
    console.error('[command] commitOmnibox failed', error);
    releaseCommit();
    return;
  }
  // Alt+Enter keeps the bar open: accept further commits right away.
  if (alt) releaseCommit();
}

function select(index, { reveal = true, by = 'key' } = {}) {
  const n = model.response.results.length;
  if (!n) return;
  const next = ((index % n) + n) % n;
  if (next === model.sel) return;
  model.sel = next;
  model.selMoved = true;
  model.revealSel = reveal;
  model.selBy = by;
  rerender();
}

/** Re-query `text` in `mode` (a mode switch): the input shows `text`, without a completion. */
function switchMode(localMode, text = model.typed) {
  const input = inputEl;
  model.localMode = localMode;
  model.typed = text;
  model.completion = null;
  if (input && input.value !== text) {
    // Dropping a shown completion (or a `>` prefix): caret at the end. An unchanged value keeps its
    // selection, e.g. the selected URL of Edit URL mode.
    input.value = text;
    input.setSelectionRange(text.length, text.length);
  }
  model.sel = 0;
  model.selMoved = false;
  model.revealSel = true;
  // A completion can only be shown after a caret at the end (not over selected text).
  runQuery({ preventInline: !input || input.selectionStart !== text.length || input.selectionEnd !== text.length });
  rerender();
}

/** Tab / Shift+Tab / chip click: actions mode on or off, keeping the typed text. */
function toggleActions() {
  // Extensions mode (Ctrl+E) has no Tab affordance: `>` still switches to the actions list, but Tab
  // does nothing, because "back to search" is not where Ctrl+E came from (FINAL PLAN §4).
  if (baseMode() === 'extensions' && displayMode() !== 'actions') return;
  if (displayMode() === 'actions') {
    // Back to the mode the bar was opened in (New Tab for a bar opened in actions mode). A `>`
    // prefix means actions too, so it goes as well.
    const lead = /^\s*>\s*/.exec(model.typed);
    switchMode(baseMode() === 'actions' ? 'newTab' : null, lead ? model.typed.slice(lead[0].length) : model.typed);
  } else {
    switchMode(baseMode() === 'actions' ? null : 'actions');
  }
}

function onInput(event) {
  const input = inputEl;
  if (!input) return;
  const previousCompletion = model.completion;
  const value = input.value;
  const deletion = typeof event.inputType === 'string' && event.inputType.startsWith('delete');
  // An edit in the middle of the text gets no completion (it couldn't be shown after the caret).
  const caretAtEnd = input.selectionStart === value.length && input.selectionEnd === value.length;
  model.typed = value;
  model.completion = null;
  model.selMoved = false;
  // Typing the next characters of the shown completion keeps it (no flicker while the query runs).
  if (!deletion && previousCompletion && previousCompletion.length > value.length && previousCompletion.startsWith(value)) {
    input.value = previousCompletion;
    input.setSelectionRange(value.length, previousCompletion.length);
    model.completion = previousCompletion;
  }
  runQuery({ preventInline: deletion || !caretAtEnd });
  rerender();
}

function onKeyDown(event) {
  const key = event.key;
  if (event.isComposing) {
    // Tab while an IME composes (e.g. Hangul) must not move focus out of the input.
    if (key === 'Tab') event.preventDefault();
    return;
  }
  const ctrlOnly = event.ctrlKey && !event.altKey && !event.shiftKey && !event.metaKey;
  let handled = true;
  if (key === 'ArrowDown' || (ctrlOnly && (key === 'n' || key === 'N'))) {
    select(model.sel + 1);
  } else if (key === 'ArrowUp' || (ctrlOnly && (key === 'p' || key === 'P'))) {
    select(model.sel - 1);
  } else if (key === 'PageDown') {
    select(Math.min(model.sel + MAX_VISIBLE_ROWS, model.response.results.length - 1));
  } else if (key === 'PageUp') {
    select(Math.max(model.sel - MAX_VISIBLE_ROWS, 0));
  } else if (key === 'Enter') {
    if (!event.ctrlKey && !event.shiftKey && !event.metaKey) commit(model.sel, event.altKey);
  } else if (key === 'Escape') {
    // Esc followed quickly by Ctrl+T opens a new bar, and this close must not reach it — `closeBar`
    // takes the `seq` of the bar this page is showing (docs/PROTOCOL.md §8, `closeCommandBar`).
    closeBar();
  } else if (key === 'Tab') {
    // Ctrl+Tab is the shell's recent-tab switcher; Alt+Tab belongs to Windows.
    if (!event.ctrlKey && !event.altKey && !event.metaKey) toggleActions();
    else handled = false;
  } else if (key === 'Backspace' && inputEl.value === '' && model.localMode === 'actions') {
    switchMode(null, '');
  } else if ((key === 'ArrowRight' || key === 'End') && !event.shiftKey && model.completion !== null) {
    // Accept the completion; the caret moves to the end as usual.
    model.typed = inputEl.value;
    model.completion = null;
    model.selMoved = false;
    runQuery({ preventInline: true });
    handled = false;
  } else {
    handled = false;
  }
  if (handled) {
    event.preventDefault();
    event.stopPropagation();
  }
}

function onModeChipClick() {
  toggleActions();
  inputEl?.focus({ preventScroll: true });
}

// ------------------------------------------------------------------------------------ state

function resetClosed() {
  model.bar = null;
  model.localMode = null;
  model.typed = '';
  model.completion = null;
  model.sel = 0;
  model.selMoved = false;
  model.gliderAt = null;
  model.shownMode = null;
  releaseCommit();
  cancelSuggest();
  // Typed text isn't kept after the bar closes (and a failed fetch's empty reply isn't reused).
  suggest.cache.clear();
  if (inputEl) inputEl.value = '';
  // Pre-warm the most common case (Ctrl+T) so the next open already has the right content/size.
  runQuery();
}

let started = false;

function onState(state) {
  const settingsChanged = syncSuggestSettings(state.settings);
  const bar = state.commandBar ?? null;
  // The overlay is hidden between opens and renders no frames while it is: an animation created
  // then would stay pending and play on the next open (FINAL PLAN rule 4).
  motion.setPresented(Boolean(bar));
  if (bar) {
    const fresh = bar.seq !== model.handledSeq;
    model.bar = bar;
    if (fresh) {
      model.handledSeq = bar.seq;
      // The card was left blank by the last page-initiated close (`closeBar`, `commit`): this open
      // has something to show again.
      motion.unblank();
      model.localMode = null;
      model.typed = bar.text ?? '';
      model.completion = null;
      model.sel = 0;
      model.selMoved = false;
      model.revealSel = true;
      releaseCommit();
      cancelSuggest();
      if (inputEl) {
        inputEl.value = model.typed;
        inputEl.focus({ preventScroll: true });
        inputEl.select();
      }
      model.animateIn = started;
      model.animateResults = started;
      // The open animation covers the first mode the chip shows: no crossfade on top of it.
      model.shownMode = null;
      model.gliderAt = null;
      model.selBy = 'key';
      // Prefilled text is selected, so it gets no inline completion (Enter opens it as shown).
      runQuery({ preventInline: model.typed !== '' });
    } else if (settingsChanged && model.typed.trim()) {
      // Suggestions switched on/off or another engine: refresh the rows for the same text.
      runQuery({ preventInline: model.preventInline });
    }
  } else if (model.bar || !started) {
    resetClosed();
  }
  started = true;
  rerender();
}

// ------------------------------------------------------------------------------------ view

/** Split `text` around the first case-insensitive occurrence of `query` (text nodes only). */
function highlight(text, query) {
  const q = query.trim().replace(/^[>?]\s*/, '');
  if (!q || !text) return text;
  const i = text.toLowerCase().indexOf(q.toLowerCase());
  if (i < 0) return text;
  return html`${text.slice(0, i)}<mark class="cmd-hl">${text.slice(i, i + q.length)}</mark>${text.slice(i + q.length)}`;
}

/**
 * Search suggestion title, like Chrome: what the user typed in normal weight, the words the
 * suggestion adds emphasized. The typed terms are matched as a prefix, else at a word start.
 */
function suggestionTitle(text, query) {
  const q = searchTerms(query).trim();
  if (!q || !text) return text;
  let start = -1;
  let length = foldedPrefixLength(text, q);
  if (length >= 0) {
    start = 0;
  } else {
    const lower = text.toLowerCase();
    const ql = q.toLowerCase();
    // Index mapping below needs case folding that keeps UTF-16 lengths (true for almost all text).
    if (lower.length !== text.length || ql.length !== q.length) return text;
    for (let from = 0; ; from++) {
      const i = lower.indexOf(ql, from);
      if (i < 0) break;
      if (i === 0 || /[\s\p{P}]/u.test(lower[i - 1])) {
        start = i;
        break;
      }
      from = i;
    }
    if (start < 0) return html`<span class="cmd-em">${text}</span>`;
    length = -1;
  }
  let end;
  if (length >= 0) {
    end = [...text].slice(0, length).join('').length;
  } else {
    end = start + q.length;
  }
  const before = text.slice(0, start);
  const after = text.slice(end);
  return html`${before && html`<span class="cmd-em">${before}</span>`}${text.slice(start, end)}${after && html`<span class="cmd-em">${after}</span>`}`;
}

const SHORTCUT = /^(Ctrl|Alt|Shift|Win|Esc|Tab|F\d{1,2})(\+|$)/;

function Hint({ hint }) {
  if (!hint) return null;
  if (hint === '↵' || SHORTCUT.test(hint)) return html`<${Kbd} class="cmd-kbd" keys=${hint} />`;
  return html`<span class="cmd-chip">${hint}</span>`;
}

/** Internal pages have no favicon; core reports their page name as the host. */
const INTERNAL_PAGE_GLYPHS = { Settings: 'settings', Archive: 'archive', History: 'history', Boosts: 'boost' };

function ResultIcon({ icon }) {
  switch (icon?.type) {
    case 'favicon':
      if (!icon.url && INTERNAL_PAGE_GLYPHS[icon.host]) return html`<${Icon} name=${INTERNAL_PAGE_GLYPHS[icon.host]} size=${16} />`;
      return html`<${Favicon} src=${loadableFavicon(icon.url)} host=${icon.host ?? ''} size=${16} lazy=${false} />`;
    case 'emoji':
      return html`<span class="cmd-emoji emoji" aria-hidden="true">${icon.emoji}</span>`;
    case 'glyph':
      return html`<${Icon} name=${icon.name} size=${16} />`;
    default:
      return html`<${Icon} name="search" size=${16} />`;
  }
}

function Row({ result, index, selected, query, completed }) {
  // Suggestion rows, and the default search row while it completes a suggestion, read like search
  // queries: the typed part plain, the rest emphasized.
  const searchLike = result.group === 'suggestions' || (completed && result.group === 'go' && result.key === 'search');
  return html`<div
    id=${`cmd-row-${index}`}
    class=${classNames('cmd-row', selected && 'is-selected', searchLike && 'is-search')}
    data-group=${result.group}
    data-key=${result.key}
    data-index=${index}
    role="option"
    aria-selected=${String(selected)}
  >
    <span class="cmd-icon">${html`<${ResultIcon} icon=${result.icon} />`}</span>
    <span class="cmd-text">
      <span class="cmd-title">${searchLike ? suggestionTitle(result.title, query) : highlight(result.title, query)}</span>
      ${result.subtitle && html`<span class="cmd-sub">${result.subtitle}</span>`}
    </span>
    ${result.hint && html`<span class="cmd-hint"><${Hint} hint=${result.hint} /></span>`}
  </div>`;
}

let lastPointer = { x: -1, y: -1 };

function onListPointerMove(event) {
  if (event.clientX === lastPointer.x && event.clientY === lastPointer.y) return;
  lastPointer = { x: event.clientX, y: event.clientY };
  const row = event.target.closest?.('.cmd-row');
  if (row) select(Number(row.dataset.index), { reveal: false, by: 'pointer' });
}

function onListClick(event) {
  const row = event.target.closest?.('.cmd-row');
  if (!row) return;
  const index = Number(row.dataset.index);
  model.sel = index;
  commit(index, event.altKey);
  inputEl?.focus({ preventScroll: true });
}

function onListAuxClick(event) {
  // Middle click: the Alt+Enter command (e.g. background tab).
  if (event.button !== 1) return;
  const row = event.target.closest?.('.cmd-row');
  if (!row) return;
  event.preventDefault();
  commit(Number(row.dataset.index), true);
}

function ModeChip({ mode, splitSide }) {
  const actions = mode === 'actions';
  // Extensions mode is a label, not a toggle: no Tab keycap, nothing to press (FINAL PLAN §4).
  const extensions = mode === 'extensions';
  const label =
    mode === 'split'
      ? html`<${Icon} name="split" size=${12} />Split<${Icon} name="chevron-right" size=${10} strokeWidth=${2} />${SIDE_LABELS[splitSide] ?? 'Right'}`
      : (MODE_LABELS[mode] ?? MODE_LABELS.newTab);
  if (extensions) {
    return html`<span class="cmd-mode is-static"><span class="cmd-mode-label">${label}</span></span>`;
  }
  return html`<button
    type="button"
    tabindex="-1"
    class=${classNames('cmd-mode', actions && 'is-actions')}
    aria-pressed=${String(actions)}
    aria-label=${actions ? 'Actions mode on. Tab turns it off' : 'Actions mode off. Tab turns it on'}
    title=${actions ? 'Back to search (Tab)' : 'Search actions (Tab)'}
    onClick=${onModeChipClick}
  >
    <span class="cmd-mode-label">${label}</span>
    <kbd class="cmd-mode-kbd" aria-hidden="true">Tab</kbd>
  </button>`;
}

const LEAD_ICONS = { newTab: 'search', editUrl: 'globe', split: 'split', actions: 'chevron-right', extensions: 'puzzle' };

function CommandBar({ view }) {
  const rootRef = useRef(null);
  const listRef = useRef(null);
  const { results, sel, mode, splitSide, query, completed, emptyText, ph } = view;

  useLayoutEffect(() => {
    const list = listRef.current;
    let listHeight = 0;
    if (list) {
      const rows = list.getElementsByClassName('cmd-row');
      const padBottom = parseFloat(getComputedStyle(list).paddingBottom) || 0;
      if (rows.length > MAX_VISIBLE_ROWS) {
        const last = rows[MAX_VISIBLE_ROWS - 1];
        const cap = last.offsetTop + last.offsetHeight + padBottom;
        list.style.setProperty('--cmd-list-cap', `${cap}px`);
        listHeight = Math.min(list.scrollHeight, cap);
      } else {
        list.style.removeProperty('--cmd-list-cap');
        listHeight = list.scrollHeight;
      }
      if (model.revealSel) {
        model.revealSel = false;
        const row = rows[sel];
        const top = row ? row.offsetTop : 0;
        const bottom = row ? top + row.offsetHeight : 0;
        const viewTop = list.scrollTop;
        const viewBottom = viewTop + Math.min(list.clientHeight || listHeight, listHeight);
        if (sel === 0) list.scrollTop = 0;
        else if (top < viewTop + 6) list.scrollTop = Math.max(0, top - 6 - (row?.previousElementSibling?.classList.contains('cmd-group') ? 26 : 0));
        else if (bottom > viewBottom - padBottom) list.scrollTop = bottom - Math.min(list.clientHeight || listHeight, listHeight) + padBottom;
      }
    }
    const inputRow = rootRef.current?.querySelector('.cmd-input-row');
    const height = (inputRow ? inputRow.offsetHeight : INPUT_H) + (list ? 1 + listHeight : 0);
    setSurfaceSize({ height }).catch((e) => console.error('[command] surface.setSize failed', e));

    // `commandBar.selection`: the highlight is one layer that glides between rows on a keyboard move
    // and snaps on hover - a hover glide lags the pointer, and a held arrow key at repeat speed never
    // catches up (critique issue 14). Positioned from the row's own layout box, so it follows the
    // list's scrolling for free.
    const glider = rootRef.current?.querySelector('.cmd-glider');
    if (glider) {
      const row = list?.getElementsByClassName('cmd-row')[sel];
      if (!row) {
        glider.style.opacity = '0';
        model.gliderAt = null;
      } else {
        const top = row.offsetTop;
        const from = model.gliderAt;
        glider.style.height = `${row.offsetHeight}px`;
        glider.style.opacity = '1';
        glider.style.translate = `0 ${top}px`;
        model.gliderAt = top;
        if (from !== null && from !== top && model.selBy === 'key') {
          motion.animate(glider, KEY_SELECTION, [{ translate: `0 ${from}px` }, { translate: `0 ${top}px` }], {
            duration: motion.duration(KEY_SELECTION, 90),
          });
        }
      }
    }

    // `commandBar.results`: only the **first** results after an open fade in, and only the rows that
    // are actually on screen. Later queries replace the list while the user types; staggering those
    // would flicker under every keystroke.
    if (model.animateResults && list) {
      const rows = list.getElementsByClassName('cmd-row');
      if (rows.length) {
        model.animateResults = false;
        motion.stagger([...rows].slice(0, MAX_VISIBLE_ROWS), KEY_RESULTS, [{ opacity: 0 }, { opacity: 1 }], {
          duration: motion.duration(KEY_RESULTS, 100),
          total: RESULTS_STAGGER_MS,
          step: 18,
        });
      }
    }

    // `commandBar.modeToggle`: the chip's label and the leading glyph crossfade; the chip's width
    // changes in one step (no morph). The placeholder is a native `::placeholder`, which no script can
    // animate, so `data-ph` restarts its own fade in CSS instead.
    if (model.shownMode !== mode) {
      const previous = model.shownMode;
      model.shownMode = mode;
      if (previous !== null) {
        const duration = motion.duration(KEY_MODE, 120);
        for (const selector of ['.cmd-mode-label', '.cmd-lead']) {
          motion.animate(rootRef.current?.querySelector(selector), KEY_MODE, [{ opacity: 0 }, { opacity: 1 }], { duration });
        }
      }
    }

    if (model.animateIn && rootRef.current) {
      model.animateIn = false;
      // The card fades and the results rise; the input row only fades. Two reasons it is never one
      // transform on the whole card: the height the card reports is measured from this page, and a
      // transformed text input moves the IME candidate window away from the caret — and Korean
      // input starts the moment the bar opens.
      const duration = motion.duration(KEY_OPEN, 140);
      motion.animate(rootRef.current, KEY_OPEN, [{ opacity: 0 }, { opacity: 1 }], { duration });
      motion.animate(
        rootRef.current.querySelector('.cmd-list'),
        KEY_OPEN,
        [
          { translate: `0 ${-motion.distance(4)}px` },
          { translate: 'none' },
        ],
        { duration },
      );
    }
  });

  const rows = [];
  let group = null;
  results.forEach((result, index) => {
    if (result.group !== group) {
      group = result.group;
      const label = GROUP_LABELS[group];
      if (label) rows.push(html`<div key=${`g:${group}:${index}`} class="cmd-group" role="presentation">${label}</div>`);
    }
    rows.push(html`<${Row} key=${result.key} result=${result} index=${index} selected=${index === sel} query=${query} completed=${completed} />`);
  });

  const showList = results.length > 0 || emptyText;
  return html`<div class=${classNames('cmd', mode === 'actions' && 'is-actions', mode === 'extensions' && 'is-extensions')} ref=${rootRef}>
    <div class="cmd-input-row">
      <span class="cmd-lead"><${Icon} name=${LEAD_ICONS[mode] ?? 'search'} size=${20} /></span>
      <input
        id="input"
        ref=${setInputRef}
        class="cmd-input"
        type="text"
        autocomplete="off"
        autocorrect="off"
        autocapitalize="off"
        spellcheck=${false}
        role="combobox"
        aria-expanded=${String(results.length > 0)}
        aria-controls="cmd-list"
        aria-autocomplete="both"
        aria-activedescendant=${results.length ? `cmd-row-${sel}` : undefined}
        aria-label=${mode === 'actions' ? 'Search actions' : mode === 'extensions' ? 'Search extensions' : 'Search or enter URL'}
        placeholder=${PLACEHOLDERS[mode] ?? PLACEHOLDERS.newTab}
        data-ph=${ph}
        onInput=${onInput}
        onKeyDown=${onKeyDown}
      />
      <${ModeChip} mode=${mode} splitSide=${splitSide} />
    </div>
    ${showList &&
    html`<div class="cmd-divider" />
      <div
        id="cmd-list"
        class="cmd-list"
        ref=${listRef}
        role="listbox"
        aria-label="Results"
        onPointerMove=${onListPointerMove}
        onMouseDown=${(e) => e.preventDefault()}
        onClick=${onListClick}
        onAuxClick=${onListAuxClick}
      >
        <div class="cmd-glider" aria-hidden="true" />
        ${results.length ? rows : html`<div class="cmd-empty">${emptyText}</div>`}
      </div>`}
  </div>`;
}

function rerender() {
  const mode = displayMode();
  if (mode !== model.phMode) {
    model.phMode = mode;
    model.phFlip = !model.phFlip;
  }
  const response = model.response;
  const results = response.results;
  const text = model.typed.trim();
  const noExtensions = mode === 'extensions' && !results.length && model.appliedSeq === model.querySeq;
  const emptyText = noExtensions
    ? text
      ? 'No matching extensions'
      : 'No extensions installed'
    : !results.length && text && model.appliedSeq === model.querySeq
      ? mode === 'actions'
        ? 'No matching actions'
        : 'No results'
      : '';
  const view = {
    results,
    sel: Math.min(model.sel, Math.max(0, results.length - 1)),
    mode,
    splitSide: model.bar?.splitSide ?? null,
    query: model.typed,
    completed: response.text === model.typed && typeof response.inlineCompletion === 'string',
    emptyText,
    ph: model.phFlip ? 'a' : 'b',
  };
  render(html`<${CommandBar} view=${view} />`, mount);
}

// ------------------------------------------------------------------------------------ startup

// The shell focuses the view when it shows the overlay: keep the caret in the input.
window.addEventListener('focus', () => {
  if (inputEl && document.activeElement !== inputEl) inputEl.focus({ preventScroll: true });
});
document.addEventListener('mousedown', (event) => {
  // Clicks on the card's chrome (the mode chip included) must not blur the input.
  if (event.target !== inputEl) event.preventDefault();
});

rerender();
startSurface({ render: onState }).catch((e) => console.error('[command] startup failed', e));

if (isMock) {
  // Test hooks for mock-mode automation only.
  window.__commandBar = { model, suggest, applyResponse, rerender, onSuggestReply, toggleActions, suggestionsFor };
}
