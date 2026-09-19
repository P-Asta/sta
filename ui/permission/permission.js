// Site permission prompt overlay (PROTOCOL §4, arc_spec §2.25). Shows the first of
// `state.permissionPrompts`: "<host> wants to" + one line per requested kind, Allow / Block and a
// "Remember for this site" checkbox (checked by default, reset for every new prompt).
// Esc blocks without remembering. Enter is deliberately not bound to Allow.
//
// For the first [`GUARD_MS`] after a prompt appears — or after something that covered it went away
// (the extension popup card, SEC-4) — the buttons are disabled and keys are ignored, so a click the
// user aimed at the page underneath cannot land on Allow.

import { html, render, useLayoutEffect, useRef } from '/common/vendor/htm-preact.js';
import { dispatch, startSurface, trackSurfaceSize } from '/common/ipc.js';
import { Icon } from '/common/icons.js';
import { Button, Checkbox, useCountPop } from '/common/components.js';
import * as motion from '/common/motion.js';

const mount = document.getElementById('app');
/** The animation key of every motion on this surface (`crates/sta-core/src/motion.rs`). */
const KEY = 'overlays.permission';

/** PermissionKind → glyph and phrase. */
const KINDS = {
  camera: { icon: 'camera', text: 'Use your camera' },
  microphone: { icon: 'mic', text: 'Use your microphone' },
  screenCapture: { icon: 'screen', text: 'See your screen' },
  geolocation: { icon: 'location', text: 'Know your location' },
  notifications: { icon: 'bell', text: 'Show notifications' },
  clipboard: { icon: 'clipboard', text: 'See text and images copied to the clipboard' },
  midiSysex: { icon: 'speaker', text: 'Control and reprogram your MIDI devices' },
  storageAccess: { icon: 'lock', text: 'Use cookies and site data while embedded' },
  other: { icon: 'info', text: 'Use other device features' },
};

/** How long the prompt ignores input after it appears or is uncovered (SEC-4). */
const GUARD_MS = 400;

const model = {
  prompt: null,
  count: 0,
  remember: true,
  /** Input before this timestamp is ignored (`performance.now()` ms). */
  guardUntil: 0,
  guardTimer: 0,
  /** Prompt id the checkbox state belongs to. */
  rememberFor: null,
  /** Prompt id already answered (ignore double clicks until the state drops it). */
  answered: null,
};

const report = (e) => console.error('[permission]', e);

/** The prompt is still ignoring input (it just appeared, or was just uncovered). */
const guarded = () => performance.now() < model.guardUntil;

/** Starts (or extends) the input guard and re-renders when it ends. */
function startGuard() {
  model.guardUntil = performance.now() + GUARD_MS;
  clearTimeout(model.guardTimer);
  model.guardTimer = setTimeout(() => {
    model.guardTimer = 0;
    rerender();
  }, GUARD_MS);
  rerender();
}

async function resolve(allow, remember) {
  const prompt = model.prompt;
  if (!prompt || model.answered === prompt.id || guarded()) return;
  model.answered = prompt.id;
  // An answer is a page-initiated close: this overlay is activatable, so the shell hides it (or
  // brings up the next prompt in the queue) the moment the command arrives — with no ack and no
  // linger, which leaves the frame this renderer produced last as the frame the *next* prompt's
  // reveal would show, saying what the previous site wanted. The blank frame goes out first
  // (FINAL PLAN §1.3); the 400 ms input guard covers the mis-click risk either way.
  await motion.closeBlank(mount?.querySelector('.perm-inner') ?? null);
  dispatch({ type: 'resolvePermission', id: prompt.id, allow, remember }).catch((e) => {
    model.answered = null;
    report(e);
  });
}

function onKeyDown(event) {
  if (event.key === 'Escape') {
    event.preventDefault();
    resolve(false, false);
  }
}

function Prompt({ prompt, count, remember, busy }) {
  // First, so the fade below already sees the surface as presented.
  motion.usePresence(Boolean(prompt));
  const rootRef = useRef(null);
  const innerRef = useRef(null);
  const queueRef = useRef(null);
  // `.perm` is the tracked root: the shell keeps whatever size it reports, so nothing may ever
  // animate or transform it. Everything that moves lives on `.perm-inner`.
  useLayoutEffect(() => trackSurfaceSize(rootRef.current), []);
  // A fade, and only a fade (FINAL PLAN §2): a prompt that also sprang in would make "clickable
  // before it looks clickable" worse. What protects the buttons is the 400 ms input guard above,
  // which is always on and is not a setting — so this is purely how the card arrives.
  useLayoutEffect(() => {
    if (!prompt) return;
    // The card was left blank by the last answer (`resolve`): this prompt has something to show again.
    motion.unblank();
    motion.animate(innerRef.current, KEY, [{ opacity: 0 }, { opacity: 1 }], { duration: motion.duration(KEY, 120) });
  }, [prompt?.id ?? null]);
  useCountPop(queueRef, count > 1 ? count : null);
  const kinds = prompt ? prompt.kinds.filter((k, i, all) => all.indexOf(k) === i) : [];
  const lead = KINDS[kinds[0]] ?? KINDS.other;
  return html`<div class="perm" ref=${rootRef} role="alertdialog" aria-labelledby="perm-title" tabindex="-1">
    ${prompt &&
    html`<div class="perm-inner" ref=${innerRef}>
      <div class="perm-head">
        <span class="perm-badge" aria-hidden="true"><${Icon} name=${lead.icon} size=${18} /></span>
        <div class="perm-heading" id="perm-title">
          <span class="perm-host">${prompt.host || prompt.origin}</span>
          <span class="perm-wants">wants to</span>
        </div>
        ${count > 1 && html`<span class="perm-queue" ref=${queueRef} title=${`${count} pending requests`}>1 of ${count}</span>`}
      </div>
      <ul class="perm-kinds">
        ${kinds.map((kind) => {
          const k = KINDS[kind] ?? KINDS.other;
          return html`<li key=${kind} class="perm-kind"><${Icon} name=${k.icon} size=${16} /><span>${k.text}</span></li>`;
        })}
      </ul>
      <div class="perm-foot">
        <${Checkbox}
          class="perm-remember"
          checked=${remember}
          label="Remember for this site"
          onChange=${(checked) => {
            model.remember = checked;
            rerender();
          }}
        />
        <div class="perm-actions">
          <${Button} size="sm" disabled=${busy} onClick=${() => resolve(false, model.remember)}>Block<//>
          <${Button} size="sm" variant="primary" disabled=${busy} onClick=${() => resolve(true, model.remember)}>Allow<//>
        </div>
      </div>
    </div>`}
  </div>`;
}

function onState(state) {
  const prompts = state.permissionPrompts ?? [];
  const prompt = prompts[0] ?? null;
  if (prompt?.id !== model.rememberFor) {
    model.rememberFor = prompt?.id ?? null;
    model.remember = true;
  }
  const fresh = prompt && prompt.id !== model.prompt?.id;
  model.prompt = prompt;
  model.count = prompts.length;
  if (!prompt) model.answered = null;
  rerender();
  if (fresh) {
    startGuard();
    mount.querySelector('.perm')?.focus({ preventScroll: true });
  }
}

function rerender() {
  render(html`<${Prompt} prompt=${model.prompt} count=${model.count} remember=${model.remember} busy=${guarded()} />`, mount);
}

window.addEventListener('keydown', onKeyDown);
// Uncovered again (an overlay above it closed, or the window came back): the guard starts over.
window.addEventListener('focus', () => {
  if (model.prompt) startGuard();
});

rerender();
startSurface({ render: onState }).catch(report);
