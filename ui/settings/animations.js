// Settings › Animations (FINAL PLAN §6): the master switch, "Follow Windows animation effects" with
// a live status line, the eight collapsible groups, and Reset to defaults.
//
// The catalog itself lives in `/common/motion-catalog.js`, which the mock backend and
// `tools/check-motion.mjs` read too. Every switch writes one
// `updateSettings {patch: {animations: …}}` and re-renders from the next state push.
//
// What a switch stores is the user's *choice*, not a difference from today's default: if a later
// release flips a default, a user who explicitly picked the old value keeps it. "Reset to defaults"
// is what clears choices again.

import { html, useRef, useState } from '/common/vendor/htm-preact.js';
import { dispatch } from '/common/ipc.js';
import { Icon } from '/common/icons.js';
import { Button, Toggle, useCountPop } from '/common/components.js';
import { Disclosure } from '/common/internal-page.js';
import { classNames } from '/common/util.js';
import { ANIMATION_GROUPS, animationSettings, groupOn, isDefaultAnimations, keyOwnValue } from '/common/motion-catalog.js';

const report = (e) => console.error('[settings]', e);
const patch = (animations) => dispatch({ type: 'updateSettings', patch: { animations } }).catch(report);

// ------------------------------------------------------------------------------------ section

function GroupRow({ group, animations, masterOff, reduced, expanded, onExpand }) {
  const on = groupOn(animations, group.id);
  const total = group.keys.length;
  const count = group.keys.filter((k) => keyOwnValue(animations, k)).length;
  const listId = `anim-${group.id}-keys`;
  // Dimmed for the same reason in both cases: nothing below this row is running. With the master
  // switch off that is sta's own doing; at `reduced` it is Windows', and the count says which —
  // otherwise all 36 switches read "on" while essentially nothing moves, which is what this
  // machine's default state looks like (P11).
  const dimmed = masterOff || reduced;
  const countRef = useRef(null);
  // `indicators.badges`: the "n of m on" line pops when a switch below it changes the count.
  useCountPop(countRef, count);
  return html`<div class=${classNames('set-anim-group', !on && 'is-group-off')}>
    <div class=${classNames('ip-setting set-anim-head', dimmed && 'is-dimmed')} aria-disabled=${dimmed ? 'true' : undefined}>
      <button
        type="button"
        class="set-anim-disclosure"
        aria-expanded=${String(expanded)}
        aria-controls=${listId}
        onClick=${() => onExpand(!expanded)}
      >
        <span class=${classNames('set-anim-chevron', expanded && 'is-open')} aria-hidden="true">
          <${Icon} name="chevron-right" size=${14} strokeWidth=${2} />
        </span>
        <span class="ip-setting-text">
          <span class="ip-setting-label">${group.label}</span>
          <span class="ip-setting-desc" ref=${countRef}>${count} of ${total} on${reduced ? ' · reduced by Windows' : ''}</span>
        </span>
      </button>
      <div class="ip-setting-control">
        <${Toggle}
          checked=${on}
          ariaLabel=${`${group.label} animations`}
          onChange=${(v) => patch({ groups: { [group.id]: v } })}
        />
      </div>
    </div>
    <${Disclosure} id=${listId} class="set-anim-keys" open=${expanded}>
      ${group.keys.map((entry) => {
        // An off parent greys its children but never locks them: pre-configuring a key while the
        // master switch or the group is off has to stay possible (`aria-disabled`, not `inert`).
        const childDimmed = dimmed || !on;
        return html`<div
          key=${entry.key}
          class=${classNames('ip-setting set-anim-key', childDimmed && 'is-dimmed')}
          aria-disabled=${childDimmed ? 'true' : undefined}
        >
          <div class="ip-setting-text">
            <span class="ip-setting-label">${entry.label}</span>
            <span class="ip-setting-desc">${entry.desc}</span>
          </div>
          <div class="ip-setting-control">
            <${Toggle}
              checked=${keyOwnValue(animations, entry)}
              ariaLabel=${entry.label}
              onChange=${(v) => patch({ set: { [entry.key]: v } })}
            />
          </div>
        </div>`;
      })}
    <//>
  </div>`;
}

/**
 * Settings › Animations: the master switch, "Follow Windows animation effects" with a live status
 * line, the eight groups, and Reset to defaults.
 * @param {{state: any}} props
 */
export function AnimationsSection({ state }) {
  const [expanded, setExpanded] = useState(() => new Set());
  const animations = animationSettings(state.settings);
  const level = state.motion?.level ?? 'full';
  const atDefaults = isDefaultAnimations(animations);
  const toggleGroup = (id, open) =>
    setExpanded((prev) => {
      const next = new Set(prev);
      if (open) next.add(id);
      else next.delete(id);
      return next;
    });

  // The live line is gated on the level core resolved, never on a second copy of the level rule:
  // `reduced` *is* "on, following Windows, and Windows says no", and with the master switch off the
  // app is at `off` — where claiming reduced motion would be wrong, and is announced (role=status).
  const followDesc = html`<span
      >Windows has one switch for animation in every app (Settings › Accessibility › Visual effects).
      With this on, turning it off there gives sta reduced motion: things fade instead of
      moving.</span
    >
    ${level === 'reduced' &&
    html`<span class="set-note" role="status">
      <${Icon} name="info" size=${14} />
      <span>Windows has animation effects turned off, so sta is using reduced motion right now.</span>
    </span>`}`;

  return html`<section id="animations" class="set-section" aria-labelledby="animations-title">
    <div class="set-section-head">
      <h2 class="set-section-title" id="animations-title">Animations</h2>
      <${Button}
        variant="ghost"
        size="sm"
        icon="undo"
        disabled=${atDefaults}
        title=${atDefaults ? 'Every animation is already at its default' : 'Turn every animation back on and follow Windows again'}
        onClick=${() => patch({ reset: true })}
        >Reset to defaults<//
      >
    </div>
    <div class="ip-card">
      <div class="ip-setting">
        <div class="ip-setting-text">
          <span class="ip-setting-label">Animations</span>
          <span class="ip-setting-desc">
            Off keeps every state change instant. Loading still shows — as a still ring or bar,
            rather than nothing.
          </span>
        </div>
        <div class="ip-setting-control">
          <${Toggle} checked=${animations.enabled} ariaLabel="Animations" onChange=${(v) => patch({ enabled: v })} />
        </div>
      </div>
      <div class="ip-setting">
        <div class="ip-setting-text">
          <span class="ip-setting-label">Follow Windows animation effects</span>
          <span class="ip-setting-desc">${followDesc}</span>
        </div>
        <div class="ip-setting-control">
          <${Toggle}
            checked=${animations.followSystem}
            ariaLabel="Follow Windows animation effects"
            onChange=${(v) => patch({ followSystem: v })}
          />
        </div>
      </div>
    </div>

    <div class=${classNames('ip-card set-anim-groups', level === 'off' && 'is-all-off')} data-motion-level=${level}>
      ${ANIMATION_GROUPS.map(
        (group) => html`<${GroupRow}
          key=${group.id}
          group=${group}
          animations=${animations}
          masterOff=${!animations.enabled}
          reduced=${level === 'reduced'}
          expanded=${expanded.has(group.id)}
          onExpand=${(open) => toggleGroup(group.id, open)}
        />`,
      )}
    </div>
  </section>`;
}
