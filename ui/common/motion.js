// Motion runtime (docs/ARCHITECTURE.md "Motion", FINAL PLAN §5). Every animation a surface plays
// goes through here, keyed by an animation key from `crates/sta-core/src/motion.rs`.
//
// The rules this module exists to keep:
// 1. **Keyed triggers only.** Callers animate because an *id*, an order signature, a `seq`, a
//    `toast.id` or a space id changed — never because a render happened, never from a visibility
//    event and never from `animationend`.
// 2. **Presented surfaces only, and the surface decides.** `usePresence` tells this module whether
//    its surface is on screen, and that answer beats `visibilityState` both ways: a parked sidebar
//    reports `visible` while rendering nothing, and an overlay's view is still hidden when the state
//    that fills it arrives. `enabled()` refuses while a surface says it is away; leaving presentation
//    — and the document going `hidden` — settles what is running or pending, so nothing bursts on the
//    next reveal.
// 3. **Transform and opacity only** in docked surfaces and overlays, and never on a root whose size
//    the shell tracks (`ipc.js trackSurfaceSize`): animate an inner wrapper instead.
// 4. **Ghosts are inert clones**: no `id`, `data-*`, `role`, `aria-*`, `tabindex` or `title`, plus
//    `aria-hidden` and `inert`, in a fixed layer outside every scroller.
// 5. **FLIP measures before it cancels** (`getBoundingClientRect` includes transforms), is
//    suspended during a drag and for bulk changes — counted in *changed ids*, which is what makes a
//    change bulk, not in rows that happened to move — moves only rows that end up inside the box it
//    measured, never further than that box is wide or tall, and snaps at `reduced` like every other
//    position animation.
// 6. **An exit the shell waits for blanks for real.** `blank()` fades a surface out and leaves it
//    invisible, because the shell hides a widget only after its page reported the frame that shows
//    nothing (`ipc.js onSurfaceExit`, PROTOCOL §14) — a hidden page renders no frames, so whatever
//    was on screen last would be shown again on the next reveal.
//
// `window.__motion` exposes `stats()` and the knobs the mock harness (`tools/motion-check.mjs`) and
// the in-app `m.motion` checks drive.

import { useLayoutEffect, useRef } from '/common/vendor/htm-preact.js';

/** Default easing for keyed animations (`--ease-out`). */
export const EASE_OUT = 'cubic-bezier(0.16, 1, 0.3, 1)';
/** A spring, as `tokens.css --ease-spring` spells it. */
export const EASE_SPRING =
  'linear(0, 0.006, 0.026 2.5%, 0.107 5.4%, 0.44 13%, 0.6 16.6%, 0.72, 0.81, 0.89, 0.95, 0.99, 1.02, 1.04 34%, 1.05, 1.04, 1.03, 1.01, 0.999 55%, 0.998, 1.001, 1.002, 1.001, 1)';

/** More changed ids than this in one step is a bulk change: no FLIP (FINAL PLAN rule 7). */
export const FLIP_BULK_LIMIT = 8;
/** Longest a stagger may take in total, and the exception for a page's own enter. */
export const STAGGER_TOTAL_MS = 150;
export const STAGGER_TOTAL_PAGE_MS = 200;

const root = () => document.documentElement;

/** Parsed `data-anim-off`, cached on the attribute string. */
let offCache = { text: null, set: new Set() };

/** The keys that are off right now, from `<html data-anim-off>` (`theme.js applyMotion`). */
export function offKeys() {
  const text = root().dataset.animOff ?? '';
  if (offCache.text !== text) offCache = { text, set: new Set(text.split(/\s+/).filter(Boolean)) };
  return offCache.set;
}

/** `'full' | 'reduced' | 'off'` from `<html data-motion>`; `full` before the first state. */
export function level() {
  const value = root().dataset.motion;
  return value === 'reduced' || value === 'off' ? value : 'full';
}

/** `true` while this surface is actually on screen (see `usePresence`). */
let presented = true;
/** Whether this surface speaks for itself (`usePresence` / `setPresented` was called at all). */
let declaresPresence = false;

/**
 * Whether this surface is presented. Overlays pass "their intent is non-null", the sidebar passes
 * "presence !== hidden": the app's own knowledge, because a parked view keeps `visibilityState`
 * at `visible` while rendering no frames.
 */
export function setPresented(on) {
  declaresPresence = true;
  const next = Boolean(on);
  if (next === presented) return;
  presented = next;
  if (!presented) {
    finishAll();
    clearGhosts();
  }
}

export function isPresented() {
  return presented;
}

/**
 * Whether `key` may animate right now: the level allows finite motion, the key is not switched off,
 * and this surface is on screen. Call it for **every** animation; `key` may be omitted to ask only
 * about the level and the surface.
 *
 * "On screen" is the surface's own answer when it gives one (`usePresence`), and `visibilityState`
 * otherwise. A surface that speaks for itself is believed in **both** directions:
 *
 * - a parked sidebar reports `visible` while rendering nothing, so the flag would say yes when the
 *   answer is no — `presented` says no and that is final;
 * - an overlay's view is still hidden when the state that fills it arrives (the shell shows it right
 *   after, and the switcher's view deliberately 250 ms after), so the flag says no when the answer is
 *   yes. An animation created in that gap stays *pending* and plays on the surface's first frame,
 *   which is exactly the entrance to show; refusing there would mean overlays never animated in.
 *
 * That second case is trusted only while the **window has focus** (`<html data-focused>`), because a
 * minimized window hides pages that still believe they are on screen, and rows arriving there would
 * pile up as pending animations and all play at once on restore. A hidden page in a window that is
 * not in front animates nothing. Pages that never declare presence (the internal pages, which are
 * ordinary tabs) keep the document's own flag.
 * @param {string} [key]
 */
export function enabled(key) {
  if (level() === 'off') return false;
  if (key != null && offKeys().has(key)) return false;
  if (!presented) return false;
  if (document.visibilityState !== 'hidden') return true;
  return declaresPresence && root().dataset.focused !== undefined;
}

/** Travel in px, collapsed to 0 at the `reduced` level (`--motion-distance`). */
export function distance(px) {
  return level() === 'full' ? px : 0;
}

/**
 * Whether `key` may *move* something, rather than only fade it. Most animations express "reduced"
 * by multiplying their travel with `distance()`, which leaves identity transforms. A few cannot:
 * what they animate is a **position** — a selection glider, a ring that follows the selected card —
 * and travelling 0 px would simply leave them in the wrong place. Those ask this instead, and snap
 * at `reduced`.
 * @param {string} [key]
 */
export function moves(key) {
  return enabled(key) && level() === 'full';
}

/**
 * `controls.smoothScroll` as a `ScrollOptions.behavior`: `'smooth'` only when the key is on and the
 * level is `full` — `reduced` explicitly means no smooth scrolling (FINAL PLAN §E).
 * @returns {'smooth'|'auto'}
 */
export function scrollBehavior() {
  return moves('controls.smoothScroll') ? 'smooth' : 'auto';
}

/**
 * The CSS custom property that holds `key`'s duration, derived from the key itself:
 * `sidebar.tabInsertRemove` → `--t-sidebar-tab-insert-remove`. `tokens.css` declares one per
 * registered key and zeroes it while the key is off; `tools/check-motion.mjs` checks both lists.
 */
export function tokenOf(key) {
  return `--t-${String(key).replace(/\./g, '-').replace(/([a-z0-9])([A-Z])/g, '$1-$2').toLowerCase()}`;
}

/** Cached `duration()` results, keyed on the motion attributes they were read under. */
const durations = new Map();
let durationsFor = null;

/** What `durations` was filled for: the two attributes every duration token depends on. */
function motionSignature() {
  const data = root().dataset;
  return `${data.motion ?? ''}|${data.animOff ?? ''}`;
}

/**
 * `key`'s duration in ms, read from its token so CSS and WAAPI can never disagree.
 *
 * The cache is keyed on `data-motion` / `data-anim-off` rather than invalidated by the
 * MutationObserver below, because `applyMotion` and the surface's own render happen in the **same
 * task** (`ipc.js renderState`): a mutation record is delivered at the end of it, so an animation
 * started in that render would otherwise read the duration of the switch that has just changed.
 * @param {string} key
 * @param {number} [fallback] used when the token is missing (a page without `tokens.css`)
 */
export function duration(key, fallback = 200) {
  const signature = motionSignature();
  if (durationsFor !== signature) {
    durations.clear();
    durationsFor = signature;
  }
  if (durations.has(key)) return durations.get(key);
  const raw = getComputedStyle(root()).getPropertyValue(tokenOf(key)).trim();
  const n = Number.parseFloat(raw);
  const ms = !raw || !Number.isFinite(n) ? fallback : raw.endsWith('ms') ? n : n * 1000;
  durations.set(key, ms);
  return ms;
}

/** Our own animations, so `finishAll()` never touches a page's or an indicator's. */
const ours = new Set();
const stats_ = { started: 0, finished: 0, cancelled: 0, replaced: 0, flips: 0, staggers: 0, ghosts: 0, viewTransitions: 0, skipped: 0 };

function isInfinite(anim) {
  const timing = anim.effect?.getComputedTiming?.();
  return timing?.iterations === Infinity;
}

/**
 * Play `frames` on `el` for `key`. The animation's `id` is the key, so a second animation for the
 * same key on the same element **replaces** the first instead of fighting it.
 * @param {Element|null|undefined} el
 * @param {string} key animation key
 * @param {Keyframe[]|PropertyIndexedKeyframes} frames
 * @param {KeyframeAnimationOptions} [opts]
 * @returns {Animation|null} `null` when the key may not animate
 */
export function animate(el, key, frames, opts = {}) {
  if (!el || !enabled(key)) {
    stats_.skipped++;
    return null;
  }
  for (const running of el.getAnimations()) {
    if (running.id === key) {
      running.cancel();
      ours.delete(running);
      stats_.replaced++;
    }
  }
  let anim;
  try {
    anim = el.animate(frames, { duration: 200, easing: EASE_OUT, fill: 'none', ...opts, id: key });
  } catch {
    return null;
  }
  stats_.started++;
  ours.add(anim);
  const done = () => ours.delete(anim);
  anim.finished.then(done, done);
  return anim;
}

/**
 * The same keyframes on several elements, offset in time. The **total** never exceeds
 * `opts.total` (150 ms, or 200 ms for a page's own enter), however many elements there are.
 *
 * `fill: 'backwards'` is the default and the whole reason this helper exists: during its delay an
 * animation with `fill: 'none'` contributes nothing, so an element waiting its turn in an *entrance*
 * would paint at its base style — fully opaque — and then blink out the moment its delay was over.
 * Filling backwards holds the first keyframe instead. It is right for exits too: their first keyframe
 * is the element's visible state, which is exactly what it was painting anyway.
 * @param {Iterable<Element>} elements
 * @param {string} key
 * @param {Keyframe[]|PropertyIndexedKeyframes} frames
 * @param {KeyframeAnimationOptions & {total?: number, step?: number}} [opts]
 */
export function stagger(elements, key, frames, opts = {}) {
  const els = [...elements].filter(Boolean);
  if (!els.length || !enabled(key)) return [];
  const { total = STAGGER_TOTAL_MS, step: wanted = 24, ...rest } = opts;
  const gaps = Math.max(1, els.length - 1);
  const step = Math.min(wanted, total / gaps);
  stats_.staggers++;
  return els.map((el, i) => animate(el, key, frames, { fill: 'backwards', ...rest, delay: (rest.delay ?? 0) + i * step }));
}

/**
 * Finish every animation we started (optionally only those for `key`), plus — when no key is given
 * — the page's own finite CSS animations, which is what a surface leaving the screen needs so they
 * do not replay on the next reveal. Infinite indicators are never touched.
 * @param {string} [key]
 * @returns {number} how many were finished
 */
export function finishAll(key) {
  let n = 0;
  const cssAnimation = typeof CSSAnimation === 'function' ? CSSAnimation : null;
  for (const anim of document.getAnimations()) {
    if (isInfinite(anim)) continue;
    if (key != null ? anim.id !== key : !(ours.has(anim) || (cssAnimation && anim instanceof cssAnimation))) continue;
    try {
      anim.finish();
      n++;
    } catch {
      // An unresolved or otherwise un-finishable animation: leave it alone rather than cancel it,
      // which would throw away the end state the caller is relying on.
    }
  }
  stats_.finished += n;
  return n;
}

// ---------------------------------------------------------------------------------------- FLIP

/** A drag reads row rects live (`dnd.js`), so FLIP must not move anything while one is running. */
function dragging() {
  const cl = root().classList;
  return cl.contains('is-dragging') || cl.contains('is-resizing');
}

/**
 * `capture` → (mutate the DOM) → `play`, the safe way round:
 * - rects are measured **before** any in-flight FLIP is cancelled, because
 *   `getBoundingClientRect` includes the transform that FLIP is currently applying;
 * - rows are matched by `data-id` (or `data-flip`), never by index;
 * - the individual `translate` property is animated, so a caller's own inline `transform`
 *   (the drag ghost) is left intact;
 * - it is a **position** animation, so it asks `moves(key)` and snaps at `reduced` — travelling
 *   `--motion-distance × the distance` would leave rows in the wrong place.
 *
 * `opts.changed` is how many ids this step adds or removes. That — not how many rows happened to
 * move — is what makes a change bulk: emptying or refilling a list changes dozens of ids while
 * displacing only the survivors, and sliding one of those the length of the list is the opposite of
 * following a rearrangement (the Undo of Clear Today, Ctrl+Shift+T).
 *
 * @param {Element|null} rootEl
 * @param {string} key
 * @param {string} [selector]
 * @param {{changed?: number}} [opts] `changed`: ids added + removed in this step
 * @returns {{play: (opts?: KeyframeAnimationOptions & {pointer?: boolean}) => number}|null}
 */
export const flip = {
  capture(rootEl, key, selector = '[data-flip]', { changed = 0 } = {}) {
    if (!rootEl || !moves(key) || dragging()) return null;
    const nodes = [...rootEl.querySelectorAll(selector)];
    const first = new Map();
    for (const el of nodes) {
      const id = el.dataset.flip || el.dataset.id;
      if (id) first.set(id, el.getBoundingClientRect());
    }
    // Only now: the measurement above had to see the transforms.
    for (const el of nodes) {
      for (const anim of el.getAnimations()) {
        if (anim.id === key) {
          anim.cancel();
          ours.delete(anim);
        }
      }
    }
    return {
      play: (opts = {}) => {
        if (!moves(key) || dragging()) return 0;
        const { pointer = false, changed: changedNow = changed, ...rest } = opts;
        // A bulk change (a space switch, Clear Today and its Undo, a restore batch) is not a
        // rearrangement the eye can follow, whichever rows ended up moving.
        if (changedNow > FLIP_BULK_LIMIT) return 0;
        // The box that was measured, clipped to the window: "visible" means inside *this*, not
        // merely a non-zero rect — a row scrolled far out of its scroller still has a size — and it
        // also bounds a believable move: a row that travels further than the list it lives in is
        // tall or wide belongs to a list that emptied and refilled, not to a rearrangement.
        const box = rootEl.getBoundingClientRect();
        const clip = {
          left: Math.max(box.left, 0),
          top: Math.max(box.top, 0),
          right: Math.min(box.right, window.innerWidth || box.right),
          bottom: Math.min(box.bottom, window.innerHeight || box.bottom),
        };
        const limitX = Math.max(1, clip.right - clip.left);
        const limitY = Math.max(1, clip.bottom - clip.top);
        const shifts = [];
        for (const el of rootEl.querySelectorAll(selector)) {
          const id = el.dataset.flip || el.dataset.id;
          const before = id && first.get(id);
          if (!before) continue;
          const after = el.getBoundingClientRect();
          if (after.width === 0 || after.height === 0) continue;
          if (after.right <= clip.left || after.left >= clip.right || after.bottom <= clip.top || after.top >= clip.bottom) continue;
          const dx = before.left - after.left;
          const dy = before.top - after.top;
          // Only real movement, and only a distance the eye can follow across this box.
          if (Math.abs(dx) < 0.5 && Math.abs(dy) < 0.5) continue;
          if (Math.abs(dx) > limitX || Math.abs(dy) > limitY) continue;
          shifts.push([el, dx, dy]);
        }
        // …and a close made with the pointer in the list must not slide the next row's × under the
        // cursor.
        if (!shifts.length || shifts.length > FLIP_BULK_LIMIT || pointer) return 0;
        stats_.flips++;
        for (const [el, dx, dy] of shifts) {
          animate(el, key, [{ translate: `${dx}px ${dy}px` }, { translate: 'none' }], { duration: 200, ...rest });
        }
        return shifts.length;
      },
    };
  },
};

// --------------------------------------------------------------------------------------- ghosts

const ghosts = new Set();
/**
 * Elements a ghost has already been taken of. A farewell is said once: a panel can be wrapped by
 * more than one thing that wants to see it leave — the sidebar ghosts its own panels under
 * `sidebar.panels`, while `Menu` and `Popover` ghost every panel under `menus.popIn` — and two
 * clones of the same element would show as a double image. The outermost component's cleanup runs
 * first (Preact unmounts parents before children), so the more specific key wins.
 */
const ghosted = new WeakSet();
/**
 * Single-slot ghosts: `slot` → the clone that holds it. A whole-pane farewell (a space switch clones
 * its list) is one deep clone of everything on screen, and switching faster than the fade can finish
 * would stack panes in the layer. One slot means one such ghost at a time: the previous one is
 * dropped the moment its replacement is taken.
 */
const slots = new Map();
const GHOST_STRIP = ['id', 'role', 'tabindex', 'title', 'name', 'for'];

function stripNode(node) {
  if (!(node instanceof Element)) return;
  for (const attr of [...node.attributes]) {
    const n = attr.name;
    if (GHOST_STRIP.includes(n) || n.startsWith('data-') || n.startsWith('aria-')) node.removeAttribute(n);
  }
  for (const child of node.children) stripNode(child);
}

function ghostLayer() {
  let layer = document.querySelector('body > .motion-ghosts');
  if (!layer) {
    layer = document.createElement('div');
    layer.className = 'motion-ghosts';
    layer.setAttribute('aria-hidden', 'true');
    document.body.appendChild(layer);
  }
  return layer;
}

/**
 * An inert copy of `el` at its current place, in a fixed layer outside every scroller (list heights
 * shrink the moment the real row is gone). Every identifying attribute is stripped, so nothing that
 * looks up rows by `data-id`, `[data-nav]`, `[data-row]`, `id` or `aria-activedescendant` can ever
 * find it — and a screen reader never reads it.
 * @param {Element|null|undefined} el
 * @param {{slot?: string}} [opts] `slot`: keep at most one ghost for this slot (see `slots`)
 * @returns {HTMLElement|null}
 */
export function ghost(el, { slot } = {}) {
  if (!el || !enabled() || ghosted.has(el)) return null;
  const rect = el.getBoundingClientRect();
  if (rect.width === 0 || rect.height === 0) return null;
  if (slot) {
    const previous = slots.get(slot);
    if (previous) {
      previous.remove();
      ghosts.delete(previous);
    }
    slots.delete(slot);
  }
  ghosted.add(el);
  const clone = el.cloneNode(true);
  if (!(clone instanceof HTMLElement)) return null;
  stripNode(clone);
  clone.setAttribute('aria-hidden', 'true');
  clone.inert = true;
  clone.classList.add('motion-ghost');
  Object.assign(clone.style, {
    position: 'fixed',
    margin: '0',
    left: `${rect.left}px`,
    top: `${rect.top}px`,
    width: `${rect.width}px`,
    height: `${rect.height}px`,
    pointerEvents: 'none',
  });
  ghostLayer().appendChild(clone);
  ghosts.add(clone);
  if (slot) slots.set(slot, clone);
  stats_.ghosts++;
  return clone;
}

/** Removes every ghost now (dismiss, hide, motion turned off, leaving presentation). */
export function clearGhosts() {
  for (const g of ghosts) g.remove();
  ghosts.clear();
  slots.clear();
}

/**
 * Animate a ghost out and remove it when the animation is over (or at once when the key is off).
 * @param {HTMLElement|null} clone
 * @param {string} key
 * @param {Keyframe[]} frames
 * @param {KeyframeAnimationOptions} [opts]
 */
export function fadeGhost(clone, key, frames, opts = {}) {
  if (!clone) return;
  const remove = () => {
    clone.remove();
    ghosts.delete(clone);
    for (const [slot, held] of slots) if (held === clone) slots.delete(slot);
  };
  const anim = animate(clone, key, frames, opts);
  if (!anim) {
    remove();
    return;
  }
  anim.finished.then(remove, remove);
}

// ------------------------------------------------------------------------------------- gliders

/**
 * Move a positioned element (a selection glider, the switcher's ring) to `to = {x, y}` and glide it
 * there from wherever it was, by animating the individual `translate` property. The **end state is
 * written first**, so a glide that is refused (the key is off, the level is `reduced`, the surface
 * is not presented) still leaves the element in the right place; a glide that is interrupted is
 * replaced rather than stacked, because `animate()` keys on the key.
 *
 * @param {HTMLElement|null|undefined} el the glider; its `style.translate` is this module's to own
 * @param {string} key
 * @param {{x: number, y: number}} to offset from the glider's own layout position, in px
 * @param {KeyframeAnimationOptions} [opts]
 * @returns {Animation|null} `null` when it snapped instead
 */
export function glide(el, key, to, opts = {}) {
  if (!el) return null;
  const from = el.style.translate || '';
  const next = `${Math.round(to.x)}px ${Math.round(to.y)}px`;
  el.style.translate = next;
  if (!from || from === next || !moves(key)) return null;
  return animate(el, key, [{ translate: from }, { translate: next }], { duration: duration(key, 140), ...opts });
}

/**
 * A short pop for a value that changed (a counter, a badge): scale out and back, on the individual
 * `scale` property so an element's own `transform` survives. Collapses to nothing at `reduced`.
 * @param {Element|null|undefined} el
 * @param {string} key
 * @param {{amount?: number} & KeyframeAnimationOptions} [opts]
 */
export function pop(el, key, { amount = 0.18, ...opts } = {}) {
  const grow = 1 + (level() === 'full' ? amount : 0);
  if (grow === 1) return null;
  return animate(el, key, [{ scale: 1 }, { scale: grow, offset: 0.4 }, { scale: 1 }], {
    duration: duration(key, 160),
    easing: 'ease-out',
    ...opts,
  });
}

// --------------------------------------------------------------------------- acknowledged exits

/** One rendered frame. */
const frame = () => new Promise((resolve) => requestAnimationFrame(resolve));
const wait = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

/**
 * `--t-surface-exit`: the longest exit fade a surface may play before the shell hides its widget.
 * The shell's own wait is `fade + an ack allowance`, floored at 50/60 ms and capped at 120 ms
 * (`crates/sta/src/motion.rs`), so a fade that outlasted this would leave a half-faded frame to show
 * on the next reveal — which is the whole thing the blank frame exists to prevent.
 */
export function surfaceExitMs() {
  const raw = getComputedStyle(root()).getPropertyValue('--t-surface-exit').trim();
  const n = Number.parseFloat(raw);
  if (!raw || !Number.isFinite(n)) return 60;
  return raw.endsWith('ms') ? n : n * 1000;
}

/**
 * How long `key`'s **exit** fade may take: its own duration, capped at `surfaceExitMs()`, and 0 when
 * the key (or all motion) is off or this surface is not presented — then the surface simply blanks.
 * @param {string} key
 */
export function exitMs(key) {
  return enabled(key) ? Math.min(duration(key, 180), surfaceExitMs()) : 0;
}

/** Elements `blank()` faded out → the animation that did it, so `unblank()` can undo both. */
const blanked = new Map();

/**
 * Present a blank frame for an acknowledged exit (`ipc.js onSurfaceExit`, PROTOCOL §14): fade
 * `targets` out over `exitMs(key)` and **leave them invisible**, resolving when the fade is over.
 *
 * The end state is written to the elements themselves, never left to the animation's `fill`: motion
 * being switched off mid-flight settles animations, and a surface that came back to opacity 1 there
 * would be exactly the stale frame the shell is waiting to avoid. They also stop taking pointer
 * events, because the widget stays up for the length of the wait and an invisible pill is not a
 * target. Pass the elements that *paint* — never a tracked overlay root (`ipc.js trackSurfaceSize`),
 * whose children are what shows.
 *
 * @param {Element|Iterable<Element>|null|undefined} targets
 * @param {string} key
 * @returns {Promise<void>}
 */
export function blank(targets, key) {
  const list = targets == null ? [] : targets instanceof Element ? [targets] : [...targets];
  const ms = exitMs(key);
  const running = [];
  for (const el of list) {
    if (!(el instanceof HTMLElement)) continue;
    const from = getComputedStyle(el).opacity;
    const anim = ms > 0 ? animate(el, key, [{ opacity: from }, { opacity: 0 }], { duration: ms, easing: 'linear', fill: 'both' }) : null;
    el.style.opacity = '0';
    // A surface on its way out must not swallow the click that lands where it used to be: the widget
    // is still up for the length of the wait, and an invisible pill is not a target.
    el.style.pointerEvents = 'none';
    blanked.set(el, anim);
    if (anim) running.push(anim.finished.catch(() => {}));
  }
  return Promise.all(running).then(() => undefined);
}

/**
 * Undo every `blank()`: the surface has something to show again (a new toast, another switcher). The
 * filled fade is cancelled as well — a finished `fill: both` animation keeps overriding the inline
 * style it was paired with.
 */
export function unblank() {
  for (const [el, anim] of blanked) {
    try {
      anim?.cancel();
    } catch {
      // Already gone with its element; the inline style below is what matters.
    }
    el.style.opacity = '';
    el.style.pointerEvents = '';
  }
  blanked.clear();
}

/** Whether anything is blanked right now (`tools/motion-check.mjs`). */
export function isBlanked() {
  return blanked.size > 0;
}

/**
 * A page-initiated close of an **activatable** overlay (the command bar, the find bar, the permission
 * prompt, Peek): blank what paints **synchronously**, give the renderer one frame to produce that
 * blank frame, and only then dispatch (FINAL PLAN §1.3, `crates/sta/src/motion.rs`).
 *
 * These four close *instantly* — no ack, no linger, nothing waits for them — so the last frame their
 * renderer produced is the frame the shell shows on the next open, which is the previous open's
 * content. There is no fade for the same reason: instant is the point.
 *
 * The frame is raced against a short timeout, because a hidden page never gets one and a close that
 * never dispatched would leave the overlay up for good.
 *
 * @param {Element|Iterable<Element>|null|undefined} targets the elements that paint
 * @returns {Promise<void>} resolves on the frame that shows nothing
 */
export function closeBlank(targets) {
  const list = targets == null ? [] : targets instanceof Element ? [targets] : [...targets];
  for (const el of list) {
    if (!(el instanceof HTMLElement)) continue;
    el.style.opacity = '0';
    el.style.pointerEvents = 'none';
    blanked.set(el, null);
  }
  return Promise.race([frame(), wait(32)]).then(() => undefined);
}

/**
 * Resolves once `key`'s exit fade has had its time, counted from the next rendered frame — for a
 * surface whose blanking is a **CSS** transition started by a render (the floating sidebar's
 * `is-hover-hidden`), where there is no WAAPI animation to await.
 * @param {string} key
 */
export function settle(key) {
  const ms = exitMs(key);
  return frame().then(() => (ms > 0 ? wait(ms) : undefined));
}

// ----------------------------------------------------------------------------- view transitions

/**
 * A View Transition for `key`, or a plain update when it may not run.
 *
 * `update` must not wait for anything but the framework's own render: the page is frozen from the
 * old snapshot until the promise it returns resolves, so an IPC round trip in there would stop the
 * page dead (fetch **before** starting the transition). A component framework that renders on a
 * microtask — this one does — needs exactly that one tick, and returning it is what puts the *new*
 * DOM into the new snapshot instead of the old one.
 * @param {string} key
 * @param {() => void|Promise<void>} update
 */
export function viewTransition(key, update) {
  const usable = enabled(key) && level() === 'full' && typeof document.startViewTransition === 'function';
  if (!usable) {
    update();
    return null;
  }
  stats_.viewTransitions++;
  try {
    return document.startViewTransition(update);
  } catch {
    update();
    return null;
  }
}

// ----------------------------------------------------------------------------------- presence

/**
 * Preact hook: tell the motion runtime whether this surface is on screen. The sidebar passes
 * `presence !== 'hidden'`, an overlay passes "its intent is non-null". Leaving presentation
 * finishes every pending animation and drops every ghost.
 *
 * **Declare it first** in the component. It is a layout effect, so it runs before the component's
 * other layout effects — which is what lets an enter animation in one of them see the surface as
 * presented on the very render that brought it back.
 * @param {boolean} present
 */
export function usePresence(present) {
  useLayoutEffect(() => {
    setPresented(present);
  }, [present]);
}

/**
 * Leave an inert ghost behind when this component unmounts. Preact runs every hook cleanup *before*
 * it removes the DOM node (`options.unmount` → `U()` in the vendored bundle), so `find()` still
 * returns a live element that can be cloned and measured — the only moment an exit animation can be
 * set up without keeping the real, focusable panel on screen. A component that renders into a
 * portal (a `Menu`, a `Popover`) has to **wrap** the thing it ghosts rather than stand beside it,
 * because a parent's cleanup runs before it recurses into its children.
 *
 * @param {string} key animation key (`crates/sta-core/src/motion.rs`)
 * @param {() => Element|null|undefined} find the element to leave behind
 * @param {Keyframe[]|(() => Keyframe[])} frames exit keyframes, played on the clone; a function is
 *   called at unmount time, for keyframes that depend on what replaced this component
 * @param {{fallback?: number, slot?: string, prepare?: (clone: HTMLElement, el: Element) => void}} [opts]
 *   `prepare` runs on the fresh clone (e.g. to copy a scroller's `scrollTop`, which cloning resets);
 *   `slot` keeps at most one of these ghosts alive at a time (`ghost()`).
 */
export function useExitGhost(key, find, frames, opts = {}) {
  const latest = useRef(null);
  latest.current = { find, frames, opts };
  useLayoutEffect(
    () => () => {
      const { find: locate, frames: keyframes, opts: options } = latest.current;
      if (!enabled(key)) return;
      const el = locate();
      const clone = ghost(el, { slot: options.slot });
      if (!clone) return;
      options.prepare?.(clone, el);
      fadeGhost(clone, key, typeof keyframes === 'function' ? keyframes() : keyframes, {
        duration: duration(key, options.fallback ?? 160),
      });
    },
    [],
  );
}

// -------------------------------------------------------------------------------------- wiring

/** Counters for `tools/motion-check.mjs` and the in-app `m.motion` checks. */
export function stats() {
  return {
    level: level(),
    off: [...offKeys()],
    presented,
    declaresPresence,
    running: ours.size,
    ghosts: ghosts.size,
    blanked: blanked.size,
    ...stats_,
  };
}

// Ghosts are page-lifetime objects, so they follow the page's own dismissal signals: a press the
// page never saw (`sidebar.hover {dismiss}` → `dismissFloatingLayers()`), and motion being turned
// off while one is on screen.
document.addEventListener('sta:dismiss', clearGhosts);

// The document going away settles whatever is running or still pending, at its end state. This is
// not a trigger — nothing starts here (rule 4) — it is the other half of trusting a surface's own
// presence: a window that is minimized hides pages that still believe they are on screen, and an
// animation created while one of them is hidden would otherwise stay *pending* and burst on the
// next reveal. The reverse direction (hidden → shown) is left alone, because that pending frame is
// exactly the entrance an overlay is supposed to play.
document.addEventListener('visibilitychange', () => {
  if (document.visibilityState === 'hidden') {
    finishAll();
    clearGhosts();
  }
});

// Turning animations off mid-flight settles what is running at its end state (never cancels it,
// which would throw that end state away) and drops every ghost. A **single** key being switched off
// settles that key's animations only — the switch on the settings page has to mean the same thing
// while something is playing as it does before it starts — and drops the ghosts, which belong to no
// one key once they are in the layer.
let offBefore = new Set(offKeys());
const motionObserver = new MutationObserver(() => {
  // The signature check in `duration()` covers a switch that changes the attributes; this covers a
  // stylesheet that changed a token behind them and re-set `data-motion` to say so (the harnesses'
  // `slow()`), which is a mutation record even when the value is the same.
  durations.clear();
  const now = new Set(offKeys());
  if (level() === 'off') {
    finishAll();
    clearGhosts();
    offBefore = now;
    return;
  }
  let grew = false;
  for (const key of now) {
    if (offBefore.has(key)) continue;
    grew = true;
    finishAll(key);
  }
  offBefore = now;
  if (grew) clearGhosts();
});
motionObserver.observe(root(), { attributes: true, attributeFilter: ['data-motion', 'data-anim-off'] });

Object.defineProperty(window, '__motion', {
  value: Object.freeze({
    stats,
    enabled,
    level,
    offKeys,
    finishAll,
    clearGhosts,
    setPresented,
    isPresented,
    distance,
    moves,
    scrollBehavior,
    duration,
    tokenOf,
    animate,
    stagger,
    flip,
    glide,
    pop,
    ghost,
    fadeGhost,
    blank,
    unblank,
    isBlanked,
    closeBlank,
    exitMs,
    surfaceExitMs,
    settle,
    viewTransition,
    EASE_OUT,
    EASE_SPRING,
  }),
  configurable: false,
  writable: false,
});
