// Shared helpers for sta UI surfaces: formatting, colors, timing and UiState traversal.
// Pure functions only (no DOM access except where noted), safe to import from any surface.

// ------------------------------------------------------------------------------------ numbers

/** Clamp `value` into `[min, max]` (NaN → `min`). */
export function clamp(value, min, max) {
  if (Number.isNaN(value)) return min;
  return Math.min(max, Math.max(min, value));
}

const numberFormats = new Map();
function formatNumber(value, maxFractionDigits) {
  let f = numberFormats.get(maxFractionDigits);
  if (!f) {
    f = new Intl.NumberFormat(undefined, { maximumFractionDigits: maxFractionDigits });
    numberFormats.set(maxFractionDigits, f);
  }
  return f.format(value);
}

const BYTE_UNITS = ['B', 'KB', 'MB', 'GB', 'TB'];

/**
 * Human-readable byte size with binary (1024) steps, Windows/Chromium style:
 * `512 B`, `12.3 MB`, `40 MB`, `1.2 GB`. Values ≥ 100 in their unit drop decimals.
 * Returns `''` for null/undefined/negative input.
 * @param {number|null|undefined} bytes
 */
export function formatBytes(bytes) {
  if (bytes == null || !Number.isFinite(bytes) || bytes < 0) return '';
  let v = bytes;
  let unit = 0;
  while (v >= 1024 && unit < BYTE_UNITS.length - 1) {
    v /= 1024;
    unit++;
  }
  const digits = unit === 0 || v >= 100 ? 0 : 1;
  return `${formatNumber(v, digits)} ${BYTE_UNITS[unit]}`;
}

/** Transfer speed, e.g. `1.2 MB/s`. `''` for null/negative. */
export function formatSpeed(bytesPerSec) {
  const s = formatBytes(bytesPerSec);
  return s && `${s}/s`;
}

/** Compact duration for "time left" labels: `8s`, `3m`, `1h 5m`, `2d`. */
export function formatDuration(seconds) {
  if (!Number.isFinite(seconds) || seconds < 0) return '';
  const s = Math.round(seconds);
  if (s < 60) return `${s}s`;
  const m = Math.round(s / 60);
  if (m < 60) return `${m}m`;
  const h = Math.floor(m / 60);
  if (h < 24) return m % 60 ? `${h}h ${m % 60}m` : `${h}h`;
  return `${Math.round(h / 24)}d`;
}

/**
 * Status line for a `Download` (sidebar downloads panel), e.g.
 * `128 MB of 412 MB · 8.4 MB/s · 34s left`, `3.3 MB · 402 KB/s` (unknown size),
 * `Paused · 5.1 MB of 11.3 MB`, `Failed · 66.8 MB of 238 MB`.
 * @param {object} d `Download` from `UiState.downloads`
 */
export function describeDownload(d) {
  const received = formatBytes(d.receivedBytes);
  const total = d.totalBytes != null ? formatBytes(d.totalBytes) : null;
  const amount = total ? `${received} of ${total}` : received;
  switch (d.state) {
    case 'inProgress': {
      const parts = [amount];
      if (d.bytesPerSec > 0) {
        parts.push(formatSpeed(d.bytesPerSec));
        if (d.totalBytes) parts.push(`${formatDuration((d.totalBytes - d.receivedBytes) / d.bytesPerSec)} left`);
      }
      return parts.join(' · ');
    }
    case 'paused':
      return `Paused · ${amount}`;
    case 'complete':
      return total ?? received;
    case 'cancelled':
      return 'Cancelled';
    case 'interrupted':
      return `Failed · ${amount}`;
    default:
      return amount;
  }
}

/** Download progress in `[0, 1]`, or `null` when the total size is unknown. */
export function downloadFraction(d) {
  if (d.state === 'complete') return 1;
  if (!d.totalBytes) return null;
  return clamp(d.receivedBytes / d.totalBytes, 0, 1);
}

// ------------------------------------------------------------------------------------ time

const MINUTE = 60_000;
const HOUR = 60 * MINUTE;
const DAY = 24 * HOUR;

// Relative phrases are English like the rest of the UI; dates and numbers use the browser locale.
const relFormats = new Map();
function relFormat(style) {
  let f = relFormats.get(style);
  if (!f) {
    f = new Intl.RelativeTimeFormat('en', { numeric: 'auto', style });
    relFormats.set(style, f);
  }
  return f;
}

/** Start of the local calendar day containing `ms`. */
export function startOfDay(ms) {
  const d = new Date(ms);
  d.setHours(0, 0, 0, 0);
  return d.getTime();
}

/**
 * Localized relative time: "just now", "5 minutes ago", "yesterday", "3 days ago"; dates older
 * than a week render as a short date ("Sep 3" / "Sep 3, 2025").
 * @param {number} ms Unix ms
 * @param {{now?: number, style?: 'long'|'short'|'narrow'}} [opts]
 */
export function relativeTime(ms, { now = Date.now(), style = 'long' } = {}) {
  if (!Number.isFinite(ms)) return '';
  const diff = ms - now;
  const abs = Math.abs(diff);
  if (abs < 45_000) return 'just now';
  if (abs < HOUR) return relFormat(style).format(Math.round(diff / MINUTE), 'minute');
  if (abs < DAY && startOfDay(ms) === startOfDay(now)) return relFormat(style).format(Math.round(diff / HOUR), 'hour');
  const days = Math.round((startOfDay(ms) - startOfDay(now)) / DAY);
  if (Math.abs(days) < 7) return relFormat(style).format(days, 'day');
  return formatDate(ms, { now });
}

/** Short localized date: "Sep 3" (current year) or "Sep 3, 2025". */
export function formatDate(ms, { now = Date.now() } = {}) {
  const d = new Date(ms);
  const sameYear = d.getFullYear() === new Date(now).getFullYear();
  return d.toLocaleDateString(undefined, sameYear ? { month: 'short', day: 'numeric' } : { month: 'short', day: 'numeric', year: 'numeric' });
}

/** Localized clock time, e.g. "14:05" / "2:05 PM". */
export function formatTime(ms) {
  return new Date(ms).toLocaleTimeString(undefined, { hour: 'numeric', minute: '2-digit' });
}

/** Group label for day-grouped lists (history, archive): "Today", "Yesterday", "Monday, September 14" (date part localized). */
export function dayLabel(ms, { now = Date.now() } = {}) {
  const days = Math.round((startOfDay(now) - startOfDay(ms)) / DAY);
  if (days === 0) return 'Today';
  if (days === 1) return 'Yesterday';
  const d = new Date(ms);
  const sameYear = d.getFullYear() === new Date(now).getFullYear();
  return d.toLocaleDateString(undefined, sameYear ? { weekday: 'long', month: 'long', day: 'numeric' } : { year: 'numeric', month: 'long', day: 'numeric' });
}

// ------------------------------------------------------------------------------------ hosts & colors

/** FNV-1a 32-bit hash of a string (deterministic across sessions). */
export function hashString(str) {
  let h = 0x811c9dc5;
  for (let i = 0; i < str.length; i++) {
    h ^= str.charCodeAt(i);
    h = Math.imul(h, 0x01000193);
  }
  return h >>> 0;
}

const normalizeHost = (host) => String(host ?? '').trim().toLowerCase().replace(/^www\./, '');

/** Deterministic hue in [0, 360) for a host (ignores a leading `www.`). */
export function hostHue(host) {
  return hashString(normalizeHost(host)) % 360;
}

/** Letter-tile background for a host: `oklch(0.7 0.08 <hash>)` (arc_spec §5 "Favicons"). */
export function hostLetterColor(host) {
  return `oklch(0.7 0.08 ${hostHue(host)})`;
}

const segmenter = typeof Intl.Segmenter === 'function' ? new Intl.Segmenter(undefined, { granularity: 'grapheme' }) : null;

/** The first `count` user-perceived characters (grapheme clusters) of `text`. */
export function firstGraphemes(text, count = 1) {
  const s = String(text ?? '');
  if (!segmenter) return Array.from(s).slice(0, count).join('');
  let out = '';
  let n = 0;
  for (const { segment } of segmenter.segment(s)) {
    if (n++ >= count) break;
    out += segment;
  }
  return out;
}

/** Uppercase first letter of a host for letter tiles (`www.github.com` → `G`, `''` → `?`). */
export function hostLetter(host) {
  const h = normalizeHost(host);
  return h ? firstGraphemes(h, 1).toLocaleUpperCase() : '?';
}

/** Host of an URL without `www.` (`null` if unparsable or host-less). */
export function hostOf(url) {
  try {
    const h = new URL(url).hostname;
    return h ? h.replace(/^www\./, '') : null;
  } catch {
    return null;
  }
}

// ------------------------------------------------------------------------------------ functions

/**
 * Trailing-edge debounce. The returned function has `.cancel()` and `.flush()`.
 * @template {(...args: any[]) => void} F
 * @param {F} fn
 * @param {number} wait ms
 */
export function debounce(fn, wait) {
  let timer = null;
  let lastArgs = null;
  const debounced = (...args) => {
    lastArgs = args;
    clearTimeout(timer);
    timer = setTimeout(() => {
      timer = null;
      fn(...lastArgs);
    }, wait);
  };
  debounced.cancel = () => {
    clearTimeout(timer);
    timer = null;
  };
  debounced.flush = () => {
    if (timer !== null) {
      debounced.cancel();
      fn(...lastArgs);
    }
  };
  return debounced;
}

/**
 * Throttle with leading and trailing calls: runs at most once per `wait` ms, and always runs
 * once more with the latest arguments after a burst. Has `.cancel()`.
 */
export function throttle(fn, wait) {
  let last = 0;
  let timer = null;
  let lastArgs = null;
  const throttled = (...args) => {
    lastArgs = args;
    const remaining = wait - (Date.now() - last);
    if (remaining <= 0) {
      clearTimeout(timer);
      timer = null;
      last = Date.now();
      fn(...args);
    } else if (timer === null) {
      timer = setTimeout(() => {
        timer = null;
        last = Date.now();
        fn(...lastArgs);
      }, remaining);
    }
  };
  throttled.cancel = () => {
    clearTimeout(timer);
    timer = null;
  };
  return throttled;
}

/**
 * Join class names: strings, arrays and `{name: condition}` objects; falsy values are skipped.
 * `classNames('row', {active: a}, ['x', false && 'y'])` → `"row active x"`.
 */
export function classNames(...args) {
  const out = [];
  const add = (a) => {
    if (!a) return;
    if (typeof a === 'string' || typeof a === 'number') out.push(a);
    else if (Array.isArray(a)) a.forEach(add);
    else if (typeof a === 'object') for (const k in a) if (a[k]) out.push(k);
  };
  args.forEach(add);
  return out.join(' ');
}

/** Structural equality for JSON-like values (objects, arrays, primitives). */
export function deepEqual(a, b) {
  if (a === b) return true;
  if (typeof a !== 'object' || typeof b !== 'object' || a === null || b === null) return false;
  if (Array.isArray(a) !== Array.isArray(b)) return false;
  const ka = Object.keys(a);
  if (ka.length !== Object.keys(b).length) return false;
  return ka.every((k) => Object.prototype.hasOwnProperty.call(b, k) && deepEqual(a[k], b[k]));
}

// ------------------------------------------------------------------------------------ UiState helpers

/** The active `SpaceView` (falls back to the first space). */
export function activeSpace(state) {
  if (!state?.spaces?.length) return null;
  return state.spaces.find((s) => s.id === state.activeSpace) ?? state.spaces[0];
}

/**
 * Depth-first walk over `NodeView`s (folders' children and split panes included).
 * `visit(node, ctx)` receives tab/folder/split nodes; pane tabs are passed as
 * `{kind: 'tab', ...pane}`-like TabViews with `ctx.split` set. Return `false` to stop.
 * @returns {boolean} false if stopped early
 */
export function walkNodes(nodes, visit, ctx = { depth: 0, parent: null, split: null }) {
  for (const node of nodes ?? []) {
    if (visit(node, ctx) === false) return false;
    if (node.kind === 'folder') {
      if (walkNodes(node.children, visit, { depth: ctx.depth + 1, parent: node, split: null }) === false) return false;
    } else if (node.kind === 'split') {
      for (const pane of node.panes) {
        if (visit(pane, { depth: ctx.depth + 1, parent: node, split: node }) === false) return false;
      }
    }
  }
  return true;
}

/** Every `TabView` in the state: favorites, all spaces (pinned, folders, today, split panes) and Peek. */
export function allTabs(state) {
  const tabs = [...(state?.favorites ?? [])];
  for (const space of state?.spaces ?? []) {
    for (const list of [space.pinned, space.today]) {
      walkNodes(list, (n, ctx) => {
        if (ctx.split || n.kind === 'tab') tabs.push(n);
      });
    }
  }
  if (state?.peek) tabs.push(state.peek.tab);
  return tabs;
}

/** Find a tab, folder or split view by id anywhere in the state (`null` if absent). */
export function findItem(state, id) {
  if (id == null || !state) return null;
  const fav = state.favorites?.find((t) => t.id === id);
  if (fav) return fav;
  if (state.peek?.tab.id === id) return state.peek.tab;
  let found = null;
  for (const space of state.spaces ?? []) {
    for (const list of [space.pinned, space.today]) {
      walkNodes(list, (n) => {
        if (n.id === id) {
          found = n;
          return false;
        }
      });
      if (found) return found;
    }
  }
  return null;
}
