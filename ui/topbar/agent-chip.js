// Topbar agent chip (PROTOCOL §9, docs/MCP.md "Seeing what agents do"): shown while an agent is
// connected, asks for approval, is paused, or left downloads waiting. The chip opens the agent
// activity panel (`toggleAgentPanel`); Stop is always one click away next to it, and Resume while
// paused. A ring pulses around the glyph while an agent is acting.

import { html, useEffect, useState } from '/common/vendor/htm-preact.js';
import { dispatch } from '/common/ipc.js';
import { Icon } from '/common/icons.js';
import { classNames } from '/common/util.js';
import { AgentGlyph, agentChipVisible, clientName, recentlyActive, toolPhrase } from '/common/agent-ui.js';

const PULSE_MS = 2500;
const fire = (command) => dispatch(command).catch((e) => console.error('[topbar] dispatch failed', command, e));

/** Re-renders once when the pulse of the latest action should end. */
function usePulse(agent) {
  const [, setTick] = useState(0);
  const last = agent?.activity?.[0]?.at ?? 0;
  const active = recentlyActive(agent, PULSE_MS);
  useEffect(() => {
    if (!active) return undefined;
    const t = setTimeout(() => setTick((n) => n + 1), Math.max(50, last + PULSE_MS - Date.now()));
    return () => clearTimeout(t);
  }, [last, active]);
  return active;
}

export function AgentChip({ agent }) {
  const active = usePulse(agent);
  if (!agentChipVisible(agent)) return null;
  const sessions = agent.sessions;
  const waiting = agent.prompts.length;
  let label;
  let tone = 'is-live';
  let glyph = html`<span class=${classNames('agent-pulse', active && 'is-active')}><${AgentGlyph} size=${15} strokeWidth=${1.7} /></span>`;
  if (agent.paused) {
    tone = 'is-paused';
    label = 'Agents paused';
    glyph = html`<${Icon} name="pause" size=${14} strokeWidth=${1.8} />`;
  } else if (waiting) {
    tone = 'is-asking';
    label = waiting === 1 ? 'Approval needed' : `${waiting} approvals needed`;
  } else if (sessions.length) {
    label = clientName(sessions[0].client) + (sessions.length > 1 ? ` +${sessions.length - 1}` : '');
  } else {
    tone = 'is-idle';
    const n = agent.heldDownloads.length;
    label = `${n} ${n === 1 ? 'download' : 'downloads'} waiting`;
    glyph = html`<${Icon} name="download" size=${14} strokeWidth=${1.8} />`;
  }
  const last = agent.activity[0];
  const title = [
    agent.paused ? 'AI agents are paused' : sessions.length ? `AI agents: ${sessions.map((s) => clientName(s.client)).join(', ')}` : 'AI agents',
    last && !agent.paused ? `Last: ${toolPhrase(last.tool)}${last.site ? ` on ${last.site}` : ''}` : null,
    'Click for recent actions',
  ]
    .filter(Boolean)
    .join('\n');
  return html`<div class=${classNames('agent-chip', tone, active && 'is-acting')} role="group" aria-label="AI agents">
    <button
      type="button"
      class="agent-chip-main"
      tabindex="-1"
      title=${title}
      aria-expanded=${String(!!agent.panelOpen)}
      onClick=${() => fire({ type: 'toggleAgentPanel' })}
    >
      ${glyph}
      <span class="agent-chip-label">${label}</span>
    </button>
    ${agent.paused
      ? html`<button type="button" class="agent-chip-action is-resume" tabindex="-1" title="Let agents connect again" onClick=${() => fire({ type: 'resumeAgents' })}>
          <${Icon} name="play" size=${12} strokeWidth=${1.9} /><span>Resume</span>
        </button>`
      : (sessions.length > 0 || waiting > 0) &&
        html`<button type="button" class="agent-chip-action is-stop" tabindex="-1" title="Stop all agents" aria-label="Stop all agents" onClick=${() => fire({ type: 'stopAgents' })}>
          <span class="agent-stop-square" aria-hidden="true" /><span>Stop</span>
        </button>`}
  </div>`;
}
