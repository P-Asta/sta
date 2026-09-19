// AI agent overlay (sta://agent/, PROTOCOL §4 and §9, docs/MCP.md "Approval"). The shell shows
// it at the top-right of the content for the first of `state.agent.prompts`, else for the activity
// panel (`state.agent.panelOpen`, the topbar chip).
//
// Approval prompts:
// - connection: "<client> wants to use sta" with the program that started the MCP server and
//   its signer (verified or "Unverified"), the access level, what that means, and
//   Deny / Allow for this session / Always allow (only for a verified, i.e. signed, program);
// - site: "Allow <client> on <site>?" with Deny / This session / Always;
// - tab: "<client> asks for a tab" (request_tab_access) with the tab, the agent's own reason, and
//   Deny / Share tab.
// Input protection: no button is focused or triggered by Enter; every button is inert for 1 s after
// a prompt appears and for 1 s after any key press in this page (someone typing when the prompt
// pops up can't answer it by accident). Esc denies once the prompt is armed. The shell denies an
// unanswered prompt after 2 minutes. Answers are accepted only from this page and Settings (403
// elsewhere).
//
// Activity panel: connected agents, Stop / Resume, the last 5 actions (tool, site, tab), downloads
// waiting for Keep / Discard, "Archive N agent tabs" and a link to Settings. Esc or a click elsewhere
// closes it.

import { html, render, useLayoutEffect, useRef } from '/common/vendor/htm-preact.js';
import { dispatch, startSurface, trackSurfaceSize } from '/common/ipc.js';
import { Icon } from '/common/icons.js';
import { Button, IconButton } from '/common/components.js';
import { classNames, hostLetter, hostLetterColor } from '/common/util.js';
import { ACCESS_LABELS, AgentGlyph, ago, clientName, errorPhrase, exeName, isReadTool, tabTitle, toolPhrase } from '/common/agent-ui.js';

const mount = document.getElementById('app');
const report = (e) => console.error('[agent]', e);
const send = (command) => dispatch(command).catch(report);

/** Buttons stay inert this long after a prompt appears and after each key press. */
export const ARM_MS = 1000;
/** The shell denies a prompt nobody answers after this long (automation/session.rs). */
export const PROMPT_TIMEOUT_MS = 120_000;

const model = {
  state: null,
  /** Prompt id → performance.now() when its buttons arm. */
  promptId: null,
  armedAt: 0,
  armTimer: 0,
  /** Prompt id already answered (ignore double clicks until the state drops it). */
  answered: null,
  tick: 0,
};

const isArmed = () => model.promptId != null && performance.now() >= model.armedAt;

function arm(delay = ARM_MS) {
  model.armedAt = Math.max(model.armedAt, performance.now() + delay);
  clearTimeout(model.armTimer);
  model.armTimer = setTimeout(rerender, Math.max(0, model.armedAt - performance.now()) + 16);
  rerender();
}

function answer(prompt, allow, remember) {
  if (!prompt || model.answered === prompt.id || !isArmed()) return;
  model.answered = prompt.id;
  const command =
    prompt.kind === 'connection'
      ? { type: 'answerAgentConnection', id: prompt.id, allow, remember }
      : prompt.kind === 'tab'
        ? { type: 'answerTabAccess', id: prompt.id, allow }
        : { type: 'answerSitePermission', id: prompt.id, allow, remember };
  dispatch(command).catch((e) => {
    model.answered = null;
    report(e);
  });
}

function onKeyDown(event) {
  const agent = model.state?.agent;
  const prompt = agent?.prompts?.[0];
  if (prompt) {
    const armed = isArmed();
    const onButton = event.target instanceof Element && event.target.closest('button');
    if (event.key === 'Escape') {
      event.preventDefault();
      if (armed) answer(prompt, false, false);
      else arm();
      return;
    }
    // Enter outside a button does nothing (no default button).
    if (event.key === 'Enter' && !onButton) event.preventDefault();
    // Space / Enter on a focused button of an armed prompt answers; every other key (and those
    // keys while inert) keeps the buttons inert for another second.
    const activates = onButton && (event.key === ' ' || event.key === 'Enter');
    if (!(armed && activates) && event.key !== 'Tab' && event.key !== 'Shift') arm();
    return;
  }
  if (event.key === 'Escape' && agent?.panelOpen) {
    event.preventDefault();
    send({ type: 'closeAgentPanel' });
  }
}

// ------------------------------------------------------------------------------------ prompts

function Queue({ count }) {
  return count > 1 ? html`<span class="ag-queue" title=${`${count} requests are waiting`}>1 of ${count}</span>` : null;
}

function Expiry({ prompt }) {
  const left = Math.ceil((prompt.requestedAt + PROMPT_TIMEOUT_MS - Date.now()) / 1000);
  if (left > 30) return null;
  return html`<span class="ag-expiry">${left > 0 ? `Closes in ${left} s` : 'Closing…'}</span>`;
}

function PromptButtons({ prompt, choices }) {
  const armed = isArmed();
  return html`<div class=${classNames('ag-actions', !armed && 'is-arming')} style=${{ '--arm-ms': `${ARM_MS}ms` }}>
    <span class="ag-arm" aria-hidden="true" key=${`${prompt.id}:${model.armedAt}`} />
    ${choices.map(
      (c) => html`<${Button}
        key=${c.id}
        size="sm"
        variant=${c.variant}
        class=${classNames('ag-choice', `ag-choice-${c.id}`)}
        aria-disabled=${armed ? undefined : 'true'}
        data-choice=${c.id}
        onClick=${() => answer(prompt, c.allow, c.remember)}
      >${c.label}<//>`,
    )}
  </div>`;
}

function ConnectionPrompt({ prompt, count, state }) {
  const client = prompt.client ?? {};
  const name = clientName(client);
  const access = state.settings?.agentAccess ?? 'full';
  const reported = [client.name, client.version].filter(Boolean).join(' ');
  const choices = [
    { id: 'deny', label: 'Deny', allow: false, remember: false, variant: 'default' },
    { id: 'session', label: 'Allow for this session', allow: true, remember: false, variant: 'primary' },
  ];
  if (client.verified) choices.push({ id: 'always', label: 'Always allow', allow: true, remember: true, variant: 'default' });
  return html`<div class="ag ag-prompt" role="alertdialog" aria-labelledby="ag-title" aria-describedby="ag-desc" tabindex="-1" data-kind="connection">
    <div class="ag-head">
      <span class="ag-badge" aria-hidden="true"><${AgentGlyph} size=${20} /></span>
      <div class="ag-heading">
        <span class="ag-title" id="ag-title"><span class="ag-name">${name}</span> wants to use sta</span>
        <span class="ag-sub">${access === 'readOnly' ? 'Would get read-only access' : 'Would get full access'}</span>
      </div>
      <${Queue} count=${count} />
    </div>
    <dl class="ag-facts">
      <div class="ag-fact">
        <dt>Client</dt>
        <dd>${reported ? html`<span class="ag-reported">${reported}</span>` : html`<span class="ag-muted">No name</span>`}<span class="ag-muted ag-note">as it reports itself</span></dd>
      </div>
      <div class="ag-fact">
        <dt>Program</dt>
        <dd title=${client.exe ?? ''}>
          ${client.exe ? html`<span class="ag-exe">${exeName(client.exe)}</span><span class="ag-path">${client.exe}</span>` : html`<span class="ag-muted">Unknown</span>`}
        </dd>
      </div>
      <div class="ag-fact">
        <dt>Signed by</dt>
        <dd>
          ${client.verified
            ? html`<span class="ag-signer">${client.signer}</span><span class="agent-verified"><${Icon} name="check" size=${12} strokeWidth=${2} />Verified</span>`
            : html`<span class="agent-unverified" title="The program has no valid digital signature, so sta can't tell who made it. It can't be allowed permanently."><${Icon} name="warning" size=${12} strokeWidth=${1.9} />Unverified</span>`}
        </dd>
      </div>
    </dl>
    <p class="ag-desc" id="ag-desc">
      ${access === 'readOnly'
        ? 'It can read the tabs it may see. What it reads is sent to its AI provider.'
        : 'It can open tabs and read, click and type in them, on sites you allow. What it reads is sent to its AI provider.'}
      ${!client.verified && html` <b>Only allow it if you just started it yourself.</b>`}
    </p>
    <${PromptButtons} prompt=${prompt} choices=${choices} />
    <div class="ag-foot"><${Expiry} prompt=${prompt} /></div>
  </div>`;
}

function SitePrompt({ prompt, count, state }) {
  const session = state.agent.sessions.find((s) => s.session === prompt.session);
  const name = session ? clientName(session.client) : 'An agent';
  const title = tabTitle(state, prompt.tab);
  const choices = [
    { id: 'deny', label: 'Deny', allow: false, remember: false, variant: 'default' },
    { id: 'session', label: 'This session', allow: true, remember: false, variant: 'primary' },
    { id: 'always', label: 'Always', allow: true, remember: true, variant: 'default' },
  ];
  return html`<div class="ag ag-prompt" role="alertdialog" aria-labelledby="ag-title" aria-describedby="ag-desc" tabindex="-1" data-kind="site">
    <div class="ag-head">
      <span class="ag-site-letter" aria-hidden="true" style=${{ background: hostLetterColor(prompt.site) }}>${hostLetter(prompt.site)}</span>
      <div class="ag-heading">
        <span class="ag-title" id="ag-title">Allow ${name} on <span class="ag-site">${prompt.site}</span>?</span>
        <span class="ag-sub"><${AgentGlyph} size=${12} /><span class="ag-tab">${title ? `In “${title}”` : 'In a new tab'}</span></span>
      </div>
      <${Queue} count=${count} />
    </div>
    <p class="ag-desc" id="ag-desc">The agent wants to open or act on this site. “Always” keeps it allowed for every agent session.</p>
    <${PromptButtons} prompt=${prompt} choices=${choices} />
    <div class="ag-foot"><${Expiry} prompt=${prompt} /></div>
  </div>`;
}

/** The tab of a prompt in the state (title, host, url), if it is still open. */
function findTab(state, id) {
  const walk = (nodes) => {
    for (const n of nodes ?? []) {
      if (n.id === id && n.kind !== 'folder' && n.kind !== 'split') return n;
      const inner = walk(n.children) ?? walk(n.panes);
      if (inner) return inner;
    }
    return null;
  };
  let found = walk(state.favorites);
  for (const s of state.spaces ?? []) found = found ?? walk(s.pinned) ?? walk(s.today);
  return found ?? (state.peek?.tab?.id === id ? state.peek.tab : null);
}

function TabPrompt({ prompt, count, state }) {
  const session = state.agent.sessions.find((s) => s.session === prompt.session);
  const name = session ? clientName(session.client) : 'An agent';
  const tab = findTab(state, prompt.tab);
  const host = tab?.host || '';
  const choices = [
    { id: 'deny', label: 'Deny', allow: false, remember: false, variant: 'default' },
    { id: 'share', label: 'Share tab', allow: true, remember: false, variant: 'primary' },
  ];
  return html`<div class="ag ag-prompt" role="alertdialog" aria-labelledby="ag-title" aria-describedby="ag-desc" tabindex="-1" data-kind="tab">
    <div class="ag-head">
      <span class="ag-badge" aria-hidden="true"><${AgentGlyph} size=${20} /></span>
      <div class="ag-heading">
        <span class="ag-title" id="ag-title"><span class="ag-name">${name}</span> asks for a tab</span>
        <span class="ag-sub">It can only use tabs you share</span>
      </div>
      <${Queue} count=${count} />
    </div>
    <div class="ag-tabcard">
      <span class="ag-site-letter is-small" aria-hidden="true" style=${{ background: hostLetterColor(host || '?') }}>${hostLetter(host || '?')}</span>
      <span class="ag-tabcard-text">
        <span class="ag-tabcard-title">${tab ? tab.title || host || tab.url : 'A closed tab'}</span>
        ${host && html`<span class="ag-tabcard-host">${host}</span>`}
      </span>
    </div>
    <div class="ag-reason">
      <span class="ag-reason-label">The agent says</span>
      <q class="ag-reason-text">${prompt.reason}</q>
    </div>
    <p class="ag-desc" id="ag-desc">If you share it, the agent can read this tab and, with full access, click and type in it. What it reads is sent to its AI provider. Stop sharing from the tab’s right-click menu.</p>
    <${PromptButtons} prompt=${prompt} choices=${choices} />
    <div class="ag-foot"><${Expiry} prompt=${prompt} /></div>
  </div>`;
}

// ------------------------------------------------------------------------------------ activity panel

function SessionRow({ session }) {
  const c = session.client;
  return html`<li class="ag-session">
    <span class="ag-dot" aria-hidden="true" />
    <span class="ag-session-name">${clientName(c)}</span>
    ${c.verified
      ? html`<span class="agent-verified" title=${`Signed by ${c.signer}`}><${Icon} name="check" size=${11} strokeWidth=${2} />Verified</span>`
      : html`<span class="agent-unverified" title="The program that started it has no valid signature">Unverified</span>`}
    <span class="ag-session-meta">${ACCESS_LABELS[session.access] ?? session.access} · ${ago(session.startedAt)}</span>
  </li>`;
}

function ActivityRow({ entry, state }) {
  const title = tabTitle(state, entry.tab);
  const open = () => entry.tab != null && title && send({ type: 'activateItem', id: entry.tab });
  return html`<li>
    <button type="button" class="ag-activity" tabindex="0" disabled=${!title} onClick=${open} title=${title ? `Show “${title}”` : undefined}>
      <span class=${classNames('ag-tool', isReadTool(entry.tool) && 'is-read', entry.error && 'is-failed')} aria-hidden="true">
        <${Icon} name=${entry.error ? 'warning' : isReadTool(entry.tool) ? 'find' : entry.tool === 'tab_open' || entry.tool === 'tab_navigate' ? 'globe' : 'edit'} size=${13} />
      </span>
      <span class="ag-activity-text">
        <span class="ag-activity-what">${toolPhrase(entry.tool)}${entry.site && html` <span class="ag-muted">on</span> ${entry.site}`}</span>
        ${entry.error
          ? html`<span class="ag-activity-tab is-failed">Didn’t work: ${errorPhrase(entry.error)}</span>`
          : title && html`<span class="ag-activity-tab">${title}</span>`}
      </span>
      <span class="ag-ago">${ago(entry.at)}</span>
    </button>
  </li>`;
}

function Panel({ state }) {
  const agent = state.agent;
  const access = state.settings?.agentAccess ?? 'off';
  const connected = agent.sessions.length;
  let status;
  if (agent.paused) status = html`<p class="ag-status is-paused"><${Icon} name="pause" size=${14} />Paused. Agents can't connect or act until you resume.</p>`;
  else if (access === 'off') status = html`<p class="ag-status">AI agent access is off.</p>`;
  else if (!connected) status = html`<p class="ag-status">No agent is connected. ${ACCESS_LABELS[access]} is on.</p>`;
  return html`<div class="ag ag-panel" role="dialog" aria-label="AI agents" tabindex="-1">
    <div class="ag-panel-head">
      <span class="ag-badge is-small" aria-hidden="true"><${AgentGlyph} size=${16} /></span>
      <h1 class="ag-panel-title">AI agents</h1>
      ${agent.paused
        ? html`<${Button} size="sm" variant="primary" icon="play" class="ag-resume" onClick=${() => send({ type: 'resumeAgents' })}>Resume<//>`
        : html`<${Button} size="sm" variant="danger" icon="pause" class="ag-stop" disabled=${!connected && !agent.prompts.length} onClick=${() => send({ type: 'stopAgents' })}>Stop<//>`}
      <${IconButton} icon="close" size="sm" label="Close" onClick=${() => send({ type: 'closeAgentPanel' })} />
    </div>
    ${status}
    ${connected > 0 && html`<ul class="ag-sessions">${agent.sessions.map((s) => html`<${SessionRow} key=${s.session} session=${s} />`)}</ul>`}
    <section class="ag-section" aria-labelledby="ag-recent">
      <h2 class="ag-section-title" id="ag-recent">Recent actions</h2>
      ${agent.activity.length
        ? html`<ul class="ag-activities">${agent.activity.map((e, i) => html`<${ActivityRow} key=${`${e.at}-${i}`} entry=${e} state=${state} />`)}</ul>`
        : html`<p class="ag-empty">Nothing yet. Actions show up here as agents work.</p>`}
    </section>
    ${agent.heldDownloads.length > 0 &&
    html`<section class="ag-section" aria-labelledby="ag-downloads">
      <h2 class="ag-section-title" id="ag-downloads">Downloads waiting for you</h2>
      <ul class="ag-downloads">
        ${agent.heldDownloads.map(
          (d) => html`<li key=${d.id} class="ag-download">
            <${Icon} name="download" size=${14} />
            <span class="ag-download-name" title=${d.fileName}>${d.fileName}</span>
            <${Button} size="sm" variant="ghost" onClick=${() => send({ type: 'resolveAgentDownload', id: d.id, keep: false })}>Discard<//>
            <${Button} size="sm" onClick=${() => send({ type: 'resolveAgentDownload', id: d.id, keep: true })}>Keep<//>
          </li>`,
        )}
      </ul>
    </section>`}
    <div class="ag-panel-foot">
      ${agent.openedTabs > 0 &&
      html`<${Button} size="sm" variant="ghost" icon="archive" class="ag-archive" onClick=${() => send({ type: 'archiveAgentTabs' })}
        >Archive ${agent.openedTabs} agent ${agent.openedTabs === 1 ? 'tab' : 'tabs'}<//
      >`}
      <span class="ag-spacer" />
      <${Button}
        size="sm"
        variant="ghost"
        iconEnd="chevron-right"
        class="ag-settings"
        onClick=${() => {
          send({ type: 'closeAgentPanel' });
          send({ type: 'openUrl', url: 'sta://settings/#agents', target: 'newTab' });
        }}
      >Agent settings<//>
    </div>
  </div>`;
}

// ------------------------------------------------------------------------------------ page

function App({ state }) {
  const rootRef = useRef(null);
  useLayoutEffect(() => trackSurfaceSize(rootRef.current), []);
  const agent = state?.agent;
  const prompt = agent?.prompts?.[0];
  let body = null;
  if (prompt?.kind === 'connection') body = html`<${ConnectionPrompt} prompt=${prompt} count=${agent.prompts.length} state=${state} />`;
  else if (prompt?.kind === 'site') body = html`<${SitePrompt} prompt=${prompt} count=${agent.prompts.length} state=${state} />`;
  else if (prompt?.kind === 'tab') body = html`<${TabPrompt} prompt=${prompt} count=${agent.prompts.length} state=${state} />`;
  else if (agent?.panelOpen) body = html`<${Panel} state=${state} />`;
  return html`<div class="ag-root" ref=${rootRef}>${body}</div>`;
}

function rerender() {
  render(html`<${App} state=${model.state} />`, mount);
}

function onState(state) {
  const agent = state.agent;
  const prompt = agent?.prompts?.[0] ?? null;
  const previous = model.state?.agent;
  model.state = state;
  if ((prompt?.id ?? null) !== model.promptId) {
    model.promptId = prompt?.id ?? null;
    model.answered = null;
    model.armedAt = 0;
    if (prompt) arm();
  }
  const panelOpened = !prompt && agent?.panelOpen && !(previous?.panelOpen && !previous?.prompts?.length);
  rerender();
  if (prompt && model.promptId !== previous?.prompts?.[0]?.id) mount.querySelector('.ag')?.focus({ preventScroll: true });
  else if (panelOpened) mount.querySelector('.ag')?.focus({ preventScroll: true });
}

// Expiry countdowns and "ago" labels.
setInterval(() => {
  const agent = model.state?.agent;
  if (agent && (agent.prompts.length || agent.panelOpen)) rerender();
}, 1000);

window.addEventListener('keydown', onKeyDown, true);
window.__agentOverlay = Object.freeze({ isArmed, get promptId() { return model.promptId; } });

rerender();
startSurface({ render: onState }).catch(report);
