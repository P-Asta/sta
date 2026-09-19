// sta UI ⇄ shell IPC client (docs/PROTOCOL.md §1).
//
//   import { invoke, dispatch, on, onState, getState, startSurface, onSurfaceExit } from '/common/ipc.js';
//
// Transport: `window.__staQuery({request, persistent, onSuccess, onFailure})`, injected by the
// shell's renderer only into trusted `sta://` pages (cef message_router). In mock mode the
// same transport is emulated by `/common/mock.js` over fixture JSON, so everything below runs
// unchanged against fake data.
//
// Mock mode is used only when `location.protocol !== 'sta:'` or the URL has `?mock`. Inside
// sta a missing `__staQuery` is an error: every request rejects and the console says why.
// There is never a silent fallback to fake data.

import { applyMotion, applyTheme, applyWindowState } from './theme.js';

const params = new URLSearchParams(location.search);

/** `true` when this page talks to the fixture-backed mock backend instead of the shell. */
export const isMock = location.protocol !== 'sta:' || params.has('mock');

/**
 * Surfaces the shell hosts in a rounded native card (corners, 1px border, shadow:
 * crates/sta/src/rounded.rs): the overlays, and the sidebar while it floats. Their pages get
 * `html.native-card` and drop their own square edge. `?nativeCard` previews it in mock mode.
 */
const NATIVE_CARD_HOSTS = ['command', 'find', 'permission', 'agent', 'switcher', 'toast', 'peek', 'sidebar'];
if (params.has('nativeCard') || (location.protocol === 'sta:' && NATIVE_CARD_HOSTS.includes(location.hostname))) {
  document.documentElement.classList.add('native-card');
}

/**
 * The query function (`__staQuery` signature), or `null` when IPC is unavailable.
 * @type {((q: {request: string, persistent: boolean, onSuccess: (r: string) => void, onFailure: (code: number, msg: string) => void}) => number) | null}
 */
const query = isMock
  ? (await import('./mock.js')).mockQuery
  : typeof window.__staQuery === 'function'
    ? window.__staQuery.bind(window)
    : null;

/** `false` inside sta when the shell did not inject `__staQuery` (untrusted context). */
export const ipcAvailable = query !== null;

if (!ipcAvailable) {
  console.error(
    '[sta/ipc] window.__staQuery is missing: this page is not running as a trusted sta:// UI surface. ' +
      'All requests will fail. Add ?mock to the URL to use fixture data.',
  );
}

// ------------------------------------------------------------------------------------ requests

/**
 * Send a request to the shell (PROTOCOL.md §2).
 * @param {string} cmd e.g. `'state.get'`, `'omnibox.query'`
 * @param {unknown} [payload] JSON-serializable payload (`null` when omitted)
 * @returns {Promise<any>} parsed JSON response (`""` → `null`); rejects with `Error` carrying `.code`
 */
export function invoke(cmd, payload = null) {
  return new Promise((resolve, reject) => {
    if (!query) {
      reject(Object.assign(new Error(`sta IPC unavailable (${cmd})`), { code: 'unavailable' }));
      return;
    }
    let request;
    try {
      request = JSON.stringify({ cmd, payload });
    } catch (e) {
      reject(e);
      return;
    }
    // Always pass both callbacks: the cef-rs router silently drops queries missing one.
    query({
      request,
      persistent: false,
      onSuccess: (response) => {
        let value;
        try {
          value = response === '' || response == null ? null : JSON.parse(response);
        } catch (e) {
          reject(new Error(`invalid JSON response to ${cmd}: ${e.message}`));
          return;
        }
        if (cmd === 'state.get') {
          // A push that raced this request may be newer than the fetched snapshot: callers
          // always get the newest known state.
          acceptState(value);
          if (currentState) value = currentState;
        }
        resolve(value);
      },
      onFailure: (code, message) => reject(Object.assign(new Error(`${cmd} failed: ${message} (${code})`), { code })),
    });
  });
}

/**
 * Queue a `Command` (see `crates/sta-core/src/command.rs`), e.g.
 * `dispatch({type: 'activateItem', id: 12})`. Resolves `null` once queued; read the effect from the
 * next `state` push. Rejects for malformed commands or shell-only commands.
 * @param {{type: string, [k: string]: unknown}} command
 */
export function dispatch(command) {
  if (!command || typeof command !== 'object' || typeof command.type !== 'string') {
    return Promise.reject(new TypeError(`dispatch expects a command object with a string "type", got ${JSON.stringify(command)}`));
  }
  return invoke('dispatch', command);
}

// ------------------------------------------------------------------------------------ events

/** @type {Map<string, Set<(payload: any) => void>>} */
const listeners = new Map();

/**
 * Subscribe to a push event (`'state'`, `'find.result'`). `state` payloads older than the last
 * accepted snapshot are dropped before listeners run.
 * @returns {() => void} unsubscribe
 */
export function on(event, callback) {
  let set = listeners.get(event);
  if (!set) listeners.set(event, (set = new Set()));
  set.add(callback);
  return () => off(event, callback);
}

/** Remove a listener added with `on`. */
export function off(event, callback) {
  listeners.get(event)?.delete(callback);
}

function emit(event, payload) {
  const set = listeners.get(event);
  if (!set) return;
  for (const cb of [...set]) {
    try {
      cb(payload);
    } catch (e) {
      console.error(`[sta/ipc] "${event}" listener threw`, e);
    }
  }
}

// ------------------------------------------------------------------------------------ state

/** @type {any} last accepted UiState */
let currentState = null;

/** Accept a snapshot if it is not older than the current one; notify `state` listeners. */
function acceptState(state) {
  if (!state || typeof state !== 'object') return false;
  if (currentState && state.revision < currentState.revision) return false;
  currentState = state;
  emit('state', state);
  return true;
}

/** The last received `UiState`, or `null` before the first snapshot. */
export function getState() {
  return currentState;
}

/**
 * Like `on('state', cb)`, but also calls `cb` right away when a snapshot is already known.
 * @param {(state: any) => void} callback
 * @returns {() => void} unsubscribe
 */
export function onState(callback) {
  const unsubscribe = on('state', callback);
  if (currentState) callback(currentState);
  return unsubscribe;
}

/** Fetch a fresh snapshot with `state.get` (listeners are notified if it is newer). */
export function refreshState() {
  return invoke('state.get');
}

// ------------------------------------------------------------------------------------ subscription

/** Consecutive failed resubscribe attempts (reset once a new stream works again). */
let subscribeAttempts = 0;
/** Id of the current event stream; a failed stream sets `streamFailed`. */
let streamId = 0;
let streamFailed = false;

/** One persistent `__subscribe` query per page; each success delivers `{event, payload}`. */
function subscribe() {
  if (!query) return;
  const id = ++streamId;
  streamFailed = false;
  query({
    request: JSON.stringify({ cmd: '__subscribe', payload: null }),
    persistent: true,
    onSuccess: (raw) => {
      if (id === streamId) subscribeAttempts = 0;
      let msg;
      try {
        msg = JSON.parse(raw);
      } catch (e) {
        console.error('[sta/ipc] malformed event', e, raw);
        return;
      }
      if (msg?.event === 'state') acceptState(msg.payload);
      else if (msg?.event) emit(msg.event, msg.payload);
    },
    onFailure: (code, message) => {
      if (id !== streamId) return;
      streamFailed = true;
      // The stream closed (shell restarted the handler, or refused). Retry a few times with
      // backoff and resync state, since pushes may have been missed meanwhile.
      if (subscribeAttempts >= 5) {
        console.error('[sta/ipc] event stream closed permanently', code, message);
        return;
      }
      const delay = 250 * 2 ** subscribeAttempts++;
      console.warn(`[sta/ipc] event stream closed (${code}: ${message}); resubscribing in ${delay} ms`);
      setTimeout(() => {
        subscribe();
        const resubscribed = streamId;
        // Once the new stream is open and state resynced, a later close starts a fresh retry
        // budget (a stream that stays quiet would otherwise never reset the counter).
        refreshState().then(
          () => {
            if (resubscribed === streamId && !streamFailed) subscribeAttempts = 0;
          },
          () => {},
        );
      }, delay);
    },
  });
}

subscribe();

// ------------------------------------------------------------------------------------ surface helpers

let readyPromise = null;

/**
 * Tell the shell this surface finished its first render (`ui.ready`). Overlays are only shown
 * after it. Idempotent while pending or after success (later calls return the same promise);
 * after a failure the next call sends it again.
 */
export function ready() {
  readyPromise ??= invoke('ui.ready').catch((e) => {
    readyPromise = null;
    throw e;
  });
  return readyPromise;
}

let lastSize = '';

/**
 * Ask the shell to size this overlay surface (`surface.setSize`, DIP). Identical consecutive
 * sizes are not re-sent, unless the previous request failed.
 * @param {{height: number, width?: number}} size
 */
export function setSurfaceSize({ height, width }) {
  const payload = { height: Math.ceil(height) };
  if (width != null) payload.width = Math.ceil(width);
  const key = JSON.stringify(payload);
  if (key === lastSize) return Promise.resolve(null);
  lastSize = key;
  return invoke('surface.setSize', payload).catch((e) => {
    // Not applied: let the same size be sent again.
    if (lastSize === key) lastSize = '';
    throw e;
  });
}

/**
 * Keep the overlay sized to `element`'s border box (height, and width when `opts.width`).
 * Uses ResizeObserver, which keeps working while the overlay is hidden (unlike rAF).
 *
 * The size is the element's **layout** box — `ResizeObserver`'s `borderBoxSize`, or
 * `offsetWidth`/`offsetHeight` if an engine ever leaves it out — and never
 * `getBoundingClientRect`, which includes transforms. A scale or translate anywhere on or above a
 * tracked root would otherwise send the shell a wrong size the moment the observer happened to
 * fire mid-animation, and the native card would *keep* it. Animate an inner wrapper instead of a
 * tracked root (docs/ARCHITECTURE.md "Motion").
 * @param {Element} element
 * @param {{width?: boolean}} [opts]
 * @returns {() => void} stop observing
 */
export function trackSurfaceSize(element, { width = false } = {}) {
  const ro = new ResizeObserver((entries) => {
    const box = entries[entries.length - 1]?.borderBoxSize?.[0];
    const height = box ? box.blockSize : element.offsetHeight;
    const w = box ? box.inlineSize : element.offsetWidth;
    setSurfaceSize(width ? { height, width: w } : { height }).catch((e) =>
      console.error('[sta/ipc] surface.setSize failed', e),
    );
  });
  ro.observe(element);
  return () => ro.disconnect();
}

// --------------------------------------------------------------------------- acknowledged exits

/**
 * Tell the shell this surface has presented the blank frame it asked for (`surface.exited {gen}`,
 * PROTOCOL §14). Sent one animation frame after `whenBlank` resolves, which is the frame that shows
 * nothing; the shell hides the widget then — never sooner than its own 50/60 ms floor, and at the cap
 * if this never arrives (`crates/sta/src/motion.rs`).
 * @param {number} gen the generation the shell sent
 * @param {Promise<unknown>|unknown} whenBlank resolves once nothing is left to see
 */
export function ackSurfaceExit(gen, whenBlank) {
  return Promise.resolve(whenBlank)
    .catch((e) => console.error('[sta/ipc] surface exit failed', e))
    .then(() => new Promise((resolve) => requestAnimationFrame(resolve)))
    .then(() => invoke('surface.exited', { gen }))
    .catch(() => {
      // The shell hides at its cap anyway; a failed ack must not break the page.
    });
}

/**
 * Handle `surface.exit {gen}`: the shell is about to hide this surface's widget and waits for a blank
 * frame first (the non-activatable toast and switcher; the floating sidebar is asked through
 * `sidebar.hover {gen}` instead, because it already blanks there).
 *
 * `run` blanks whatever is on screen (`motion.js blank`) and resolves when the fade is over; the ack
 * follows one frame later. A `gen` this page does not understand is ignored rather than acked.
 * @param {(payload: {gen: number}) => void|Promise<void>} run
 * @returns {() => void} unsubscribe
 */
export function onSurfaceExit(run) {
  return on('surface.exit', (payload) => {
    const gen = payload?.gen;
    if (typeof gen !== 'number') return;
    ackSurfaceExit(
      gen,
      (async () => run(payload))(),
    );
  });
}

/**
 * Standard surface startup (PROTOCOL.md §1): the event subscription is already open, then
 * `state.get` → `applyTheme` → `render(state)` → `ui.ready`. `render` is called again for
 * every newer snapshot.
 *
 * ```js
 * startSurface({ render: (state) => render(html`<Sidebar state=${state} />`, document.body) });
 * ```
 *
 * @param {object} opts
 * @param {(state: any) => void} opts.render called with each accepted UiState
 * @param {boolean} [opts.theme=true] apply `theme.js` `applyTheme(state)`, `applyMotion(state)` and
 *   `applyWindowState(state)` before each render
 * @param {boolean} [opts.ready=true] send `ui.ready` after the first successful render (retried
 *   after the next successful render if it fails)
 * @returns {Promise<() => void>} resolves after the first render attempt; the function stops rendering
 */
export async function startSurface({ render, theme = true, ready: sendReady = true }) {
  let rendered = false;
  /** @type {Promise<void> | null} pending or successful ui.ready */
  let readySent = null;
  const renderState = (state) => {
    rendered = true;
    try {
      if (theme) {
        applyTheme(state);
        // Motion goes with the theme: one attribute pair on <html> that tokens.css and motion.js
        // both read, plus `data-focused` for the indicators that only run in a focused window
        // (docs/ARCHITECTURE.md "Motion").
        applyMotion(state);
        applyWindowState(state);
      }
      render(state);
    } catch (e) {
      console.error('[sta/ipc] surface render failed', e);
      return;
    }
    // Not gated on requestAnimationFrame: hidden overlay views don't run frames until shown, and
    // they are only shown after ui.ready. Preact renders synchronously, so the DOM is complete.
    if (sendReady && !readySent) {
      readySent = ready().catch((e) => {
        readySent = null;
        console.error('[sta/ipc] ui.ready failed', e);
      });
    }
  };
  const unsubscribe = on('state', renderState);
  try {
    // A push that raced state.get may already be newer; acceptState drops the older snapshot,
    // so render whatever is current if nothing was rendered through the listener.
    await refreshState();
  } catch (e) {
    unsubscribe();
    console.error('[sta/ipc] state.get failed; surface not started', e);
    throw e;
  }
  if (!rendered && currentState) renderState(currentState);
  if (readySent) await readySent;
  return unsubscribe;
}

// ------------------------------------------------------------------------------------ automation

window.sta = Object.freeze({ invoke, dispatch, on, off, getState, onState, refreshState, isMock });
