// Generic Preact hooks shared by components and surfaces. No IPC here (see ipc-hooks.js), so
// components.js stays usable without a backend.

import { useEffect, useRef, useState } from './vendor/htm-preact.js';

/** A ref whose `.current` is always the latest `value` (for stable event handlers). */
export function useLatest(value) {
  const ref = useRef(value);
  ref.current = value;
  return ref;
}

/** The value from the previous render (`undefined` on the first). */
export function usePrevious(value) {
  const ref = useRef();
  const previous = ref.current;
  ref.current = value;
  return previous;
}

let idCounter = 0;

/** A unique, render-stable DOM id, e.g. for `aria-labelledby`. */
export function useStableId(prefix = 'ast') {
  const ref = useRef(null);
  if (ref.current === null) ref.current = `${prefix}-${++idCounter}`;
  return ref.current;
}

/**
 * Add an event listener for the component's lifetime. `target` may be an EventTarget, a ref, or
 * null (no listener). The latest `handler` is always called; re-subscribes only when
 * `target`, `type` or `capture` change.
 */
export function useEventListener(target, type, handler, { capture = false, passive } = {}) {
  const latest = useLatest(handler);
  useEffect(() => {
    const el = target && 'current' in target ? target.current : target;
    if (!el?.addEventListener) return undefined;
    const listener = (e) => latest.current(e);
    el.addEventListener(type, listener, { capture, passive });
    return () => el.removeEventListener(type, listener, { capture });
  }, [target, type, capture, passive]);
}

/**
 * Call `handler(event)` on a pointerdown outside every element in `refs` (refs or elements).
 * Only while `enabled`.
 */
export function useOutsidePointerDown(refs, handler, enabled = true) {
  const latest = useLatest({ refs, handler });
  useEffect(() => {
    if (!enabled) return undefined;
    const listener = (e) => {
      const inside = latest.current.refs.some((r) => {
        const el = r && 'current' in r ? r.current : r;
        return el?.contains?.(e.target);
      });
      if (!inside) latest.current.handler(e);
    };
    document.addEventListener('pointerdown', listener, true);
    return () => document.removeEventListener('pointerdown', listener, true);
  }, [enabled]);
}

/** `value`, updated only after it stopped changing for `ms`. */
export function useDebouncedValue(value, ms) {
  const [debounced, setDebounced] = useState(value);
  useEffect(() => {
    const t = setTimeout(() => setDebounced(value), ms);
    return () => clearTimeout(t);
  }, [value, ms]);
  return debounced;
}

/** Live `matchMedia(query).matches`, e.g. `useMediaQuery('(prefers-reduced-motion: reduce)')`. */
export function useMediaQuery(query) {
  const [matches, setMatches] = useState(() => matchMedia(query).matches);
  useEffect(() => {
    const mql = matchMedia(query);
    const update = () => setMatches(mql.matches);
    update();
    mql.addEventListener('change', update);
    return () => mql.removeEventListener('change', update);
  }, [query]);
  return matches;
}
