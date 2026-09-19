// Toast overlay (PROTOCOL §3–§4, arc_spec §2.24): one message, an optional action button and ×.
// Auto width ≤ 480 × 36 via surface.setSize. The page owns the timer: after `durationMs` it
// dispatches `dismissToast {id}`; the timer restarts whenever `toast.id` changes and pauses while
// the pointer is over the toast. The action dispatches `action.command`, then `dismissToast`.
//
// Its exit is **acknowledged**: the shell asks the page to blank (`surface.exit`) and hides the
// overlay when the page reports that frame, so the next toast never flashes the previous one.

import { html, render, useLayoutEffect, useRef } from '/common/vendor/htm-preact.js';
import { dispatch, onSurfaceExit, startSurface, trackSurfaceSize } from '/common/ipc.js';
import { IconButton } from '/common/components.js';
import * as motion from '/common/motion.js';

const mount = document.getElementById('app');
const report = (e) => console.error('[toast]', e);
/** The animation key of every motion on this surface (`crates/sta-core/src/motion.rs`). */
const KEY = 'overlays.toast';

/** Shortest time a toast stays after the pointer leaves it. */
const MIN_AFTER_HOVER_MS = 1500;
/**
 * A resting pointer stops holding the toast after this long. The overlay view is hidden between
 * toasts and never sees the pointer leave, so its hover state can be stale when the next toast
 * shows: pausing only on real pointer movement, and resuming when it rests, keeps a stale (or
 * forgotten) hover from pinning a toast on screen forever.
 */
const IDLE_RESUME_MS = 4000;

const timer = {
  id: null,
  handle: 0,
  deadline: 0,
  remaining: 0,
  paused: false,
  idle: 0,
};

function dismiss(id) {
  clearTimeout(timer.handle);
  timer.handle = 0;
  if (id == null) return;
  dispatch({ type: 'dismissToast', id }).catch(report);
}

function arm(ms) {
  clearTimeout(timer.handle);
  timer.remaining = ms;
  timer.deadline = Date.now() + ms;
  const id = timer.id;
  timer.handle = setTimeout(() => {
    if (timer.id === id) dismiss(id);
  }, ms);
}

function pause() {
  if (timer.id == null) return;
  clearTimeout(timer.idle);
  timer.idle = setTimeout(resume, IDLE_RESUME_MS);
  if (timer.paused) return;
  timer.paused = true;
  timer.remaining = Math.max(0, timer.deadline - Date.now());
  clearTimeout(timer.handle);
}

function resume() {
  clearTimeout(timer.idle);
  if (timer.id == null || !timer.paused) return;
  timer.paused = false;
  arm(Math.max(MIN_AFTER_HOVER_MS, timer.remaining));
}

async function runAction(toast) {
  const id = toast.id;
  try {
    await dispatch(toast.action.command);
  } catch (e) {
    report(e);
  }
  dismiss(id);
}

/** `toast.id` the enter animation already ran for; `null` while no toast is shown. */
let animateFor = null;

function Toast({ toast }) {
  // First, so the enter animation below already sees the surface as presented.
  motion.usePresence(Boolean(toast));
  const rootRef = useRef(null);
  const innerRef = useRef(null);
  const msgRef = useRef(null);
  // `.toast` is the tracked root: the shell keeps whatever size it reports, so nothing may ever
  // transform it. The pill's layout and every animation live on `.toast-inner`.
  useLayoutEffect(() => trackSurfaceSize(rootRef.current, { width: true }), []);
  useLayoutEffect(() => {
    if (!toast || animateFor === toast.id) return;
    const replacing = animateFor != null;
    animateFor = toast.id;
    // A toast shown again while the overlay was lingering out: the shell cancelled its exit, so this
    // pill has to come back from the blank frame the page just presented.
    motion.unblank();
    const options = { duration: motion.duration(KEY, 180) };
    // A replacement is not a new pill arriving: only the text crosses over, so the pill neither
    // moves under the pointer nor re-plays a rise the user is already looking at.
    if (replacing) {
      motion.animate(msgRef.current, KEY, [{ opacity: 0 }, { opacity: 1 }], options);
      return;
    }
    // Inside the shell's card the page is 28 DIP high and the pill's controls all but fill it (the
    // action button has 1 px of slack above and below), and `html, body { overflow: hidden }` clips
    // whatever leaves it: a 4 px rise would slice the Undo button and the × flat against the card's
    // rounded bottom edge for the first frames. There the pill fades, which is what `reduced` does
    // everywhere anyway; outside the card (mock mode) the page is the whole toast and it rises.
    const rise = document.documentElement.classList.contains('native-card') ? 0 : motion.distance(4);
    motion.animate(
      innerRef.current,
      KEY,
      [
        { opacity: 0, translate: `0 ${rise}px` },
        { opacity: 1, translate: 'none' },
      ],
      options,
    );
  });
  return html`<div class="toast" ref=${rootRef}>
    <div
      class="toast-inner"
      ref=${innerRef}
      role="status"
      aria-live="polite"
      onPointerMove=${pause}
      onPointerDown=${pause}
      onPointerLeave=${resume}
    >
      ${toast &&
      html`<span class="toast-msg" ref=${msgRef} title=${toast.message}>${toast.message}</span>
        ${toast.action &&
        html`<button type="button" class="toast-action" onClick=${() => runAction(toast)}>${toast.action.label}</button>`}
        <${IconButton} class="toast-close" icon="close" label="Dismiss" size="sm" iconSize=${14} muted onClick=${() => dismiss(toast.id)} />`}
    </div>
  </div>`;
}

function onState(state) {
  const toast = state.toast ?? null;
  if ((toast?.id ?? null) !== timer.id) {
    timer.id = toast?.id ?? null;
    timer.paused = false;
    clearTimeout(timer.handle);
    clearTimeout(timer.idle);
    if (toast) arm(Math.max(0, Number(toast.durationMs) || 2500));
  }
  // The overlay stays alive (hidden) between toasts, so forget the last id: the next toast is a
  // pill *arriving*, not a replacement of one the user can see.
  if (!toast) animateFor = null;
  render(html`<${Toast} toast=${toast} />`, mount);
}

// The shell hides this overlay only after the pill is gone from the screen (PROTOCOL §14): it asks
// here, the pill fades out over at most `--t-surface-exit`, and `onSurfaceExit` acknowledges the frame
// that shows nothing. The tracked `.toast` root is never touched — only the pill inside it.
onSurfaceExit(() => motion.blank(mount.querySelector('.toast-inner'), KEY));

startSurface({ render: onState }).catch(report);
