// Settings → "AI agents (MCP)" (PROTOCOL §9, docs/MCP.md). Every agent policy setting, approvals
// waiting for an answer (a keyboard-friendly alternative to the overlay: Settings may answer them),
// trusted clients with Revoke, copyable setup for Claude Code, Claude Desktop (incl. the Microsoft
// Store install), VS Code and Cursor, and Test connection (`agent.testConnection`).

import { html, useEffect, useState } from '/common/vendor/htm-preact.js';
import { dispatch, invoke } from '/common/ipc.js';
import { useRequest } from '/common/ipc-hooks.js';
import { Icon } from '/common/icons.js';
import { Button, IconButton, TextField, Toggle } from '/common/components.js';
import { Segmented } from '/common/internal-page.js';
import { allTabs, classNames, formatDate, IS_MAC } from '/common/util.js';
import { AgentGlyph, clientName, exeName } from '/common/agent-ui.js';

const report = (e) => console.error('[settings/agents]', e);
const send = (command) => dispatch(command).catch(report);
const update = (patch) => send({ type: 'updateSettings', patch });

const ACCESS_OPTIONS = [
  { value: 'off', label: 'Off' },
  { value: 'readOnly', label: 'Read only' },
  { value: 'full', label: 'Full' },
];

const SCOPE_OPTIONS = [
  { value: 'agentTabs', label: 'Their own tabs' },
  { value: 'allTabs', label: 'All tabs' },
];

const SCRIPT_OPTIONS = [
  { value: 'off', label: 'Off' },
  { value: 'isolated', label: 'Isolated' },
  { value: 'main', label: 'Page' },
];

const ACCESS_DESCRIPTIONS = {
  off: 'No agent can connect. sta doesn’t listen for agents at all.',
  readOnly: 'Agents can list, read, search and take screenshots of the tabs they may see, and ask you to share one. They can’t open, change or click anything.',
  full: 'Agents can open tabs, navigate, click, type, fill forms and answer page dialogs, on sites you allow.',
};

// ------------------------------------------------------------------------------------ setup snippets

const q = (s) => `"${s}"`;

/**
 * The copyable configuration for each client. `args` = extra bridge arguments (data dir).
 * `bridgePath` is the real path the shell reports; the fallback and the "where" lines below are
 * only for the platform this runs on.
 */
export function setupSnippets(bridgePath, dataDir) {
  const exe = bridgePath || (IS_MAC ? '/Applications/sta.app/Contents/MacOS/sta-mcp' : 'C:\\Program Files\\sta\\sta-mcp.exe');
  const args = dataDir ? ['--data-dir', dataDir] : [];
  const cliArgs = args.length ? ` ${args[0]} ${q(args[1])}` : '';
  const withArgs = (obj) => (args.length ? { ...obj, args } : obj);
  const json = (value) => JSON.stringify(value, null, 2);
  return [
    {
      id: 'claudeCode',
      label: 'Claude Code',
      where: 'Run in a terminal. -s user makes it available in every project.',
      code: `claude mcp add sta -s user -- ${q(exe)}${cliArgs}`,
      after: 'Check it with claude mcp list.',
    },
    {
      id: 'claudeDesktop',
      label: 'Claude Desktop',
      where: IS_MAC
        ? 'Add to ~/Library/Application Support/Claude/claude_desktop_config.json (Settings → Developer → Edit Config), then restart Claude Desktop.'
        : 'Add to %APPDATA%\\Claude\\claude_desktop_config.json (Settings → Developer → Edit Config), then restart Claude Desktop.',
      code: json({ mcpServers: { sta: withArgs({ command: exe }) } }),
      after: IS_MAC
        ? undefined
        : 'Installed from the Microsoft Store? The file is in %LOCALAPPDATA%\\Packages\\Claude_pzs8sxrjxfjjc\\LocalCache\\Roaming\\Claude\\, and Claude can’t start sta for you: open sta first.',
    },
    {
      id: 'vscode',
      label: 'VS Code',
      where: 'Add to .vscode/mcp.json in your workspace (or run MCP: Add Server).',
      code: json({ servers: { sta: withArgs({ type: 'stdio', command: exe }) } }),
    },
    {
      id: 'cursor',
      label: 'Cursor',
      where: IS_MAC ? 'Add to ~/.cursor/mcp.json (or the project’s .cursor/mcp.json).' : 'Add to %USERPROFILE%\\.cursor\\mcp.json (or the project’s .cursor/mcp.json).',
      code: json({ mcpServers: { sta: withArgs({ command: exe }) } }),
    },
  ];
}

function SetupCard({ info }) {
  const [client, setClient] = useState('claudeCode');
  const [copied, setCopied] = useState(null);
  const snippets = setupSnippets(info?.bridgePath, info && !info.dataDirIsDefault ? info.dataDir : null);
  const current = snippets.find((s) => s.id === client) ?? snippets[0];
  useEffect(() => {
    if (!copied) return undefined;
    const t = setTimeout(() => setCopied(null), 1600);
    return () => clearTimeout(t);
  }, [copied]);
  return html`<div class="ip-card agt-setup">
    <div class="ip-setting is-stacked">
      <div class="ip-setting-text">
        <span class="ip-setting-label">Connect a client</span>
        <span class="ip-setting-desc">
          Register sta’s MCP server with your AI client. It runs on this computer and only talks to sta through a private channel for your
          account.
          ${info && !info.bridgeFound && html` <span class="agt-warn">${IS_MAC ? 'sta-mcp' : 'sta-mcp.exe'} wasn’t found next to sta.</span>`}
        </span>
      </div>
      <${Segmented} label="Client" value=${current.id} options=${snippets.map((s) => ({ value: s.id, label: s.label }))} onChange=${setClient} />
      <div class="agt-snippet">
        <p class="agt-where">${current.where}</p>
        <div class="agt-code-wrap">
          <pre class="agt-code selectable" data-snippet=${current.id}>${current.code}</pre>
          <${Button}
            size="sm"
            class="agt-copy"
            icon=${copied === current.id ? 'check' : 'copy'}
            onClick=${() => {
              send({ type: 'copyText', text: current.code });
              setCopied(current.id);
            }}
          >${copied === current.id ? 'Copied' : 'Copy'}<//>
        </div>
        ${current.after && html`<p class="agt-where">${current.after}</p>`}
      </div>
    </div>
    <${TestConnection} />
  </div>`;
}

function TestConnection() {
  const [test, setTest] = useState({ running: false, result: null, error: null });
  const run = async () => {
    setTest({ running: true, result: null, error: null });
    try {
      const result = await invoke('agent.testConnection');
      setTest({ running: false, result, error: null });
    } catch (e) {
      setTest({ running: false, result: null, error: e.message || String(e) });
    }
  };
  const labels = { access: 'Agent access', endpoint: 'Listening', bridge: 'MCP server', channel: 'Connection' };
  return html`<div class="ip-setting is-stacked agt-test">
    <div class="agt-test-head">
      <div class="ip-setting-text">
        <span class="ip-setting-label">Test connection</span>
        <span class="ip-setting-desc">Runs the MCP server once to check that it can reach sta. No agent connects and nothing is sent anywhere.</span>
      </div>
      <${Button} size="sm" icon="reload" class="agt-test-btn" disabled=${test.running} onClick=${run}>${test.running ? 'Testing…' : 'Test connection'}<//>
    </div>
    ${test.error && html`<p class="agt-test-error" role="alert">Couldn’t run the test: ${test.error}</p>`}
    ${test.result &&
    html`<ul class=${classNames('agt-steps', test.result.ok ? 'is-ok' : 'is-failed')} role="status" aria-label=${test.result.ok ? 'Connection works' : 'Connection test failed'}>
      ${test.result.steps.map(
        (s) => html`<li key=${s.id} class=${classNames('agt-step', s.ok ? 'is-ok' : 'is-failed')} data-step=${s.id}>
          <${Icon} name=${s.ok ? 'check' : 'warning'} size=${14} strokeWidth=${s.ok ? 2 : 1.8} />
          <span class="agt-step-label">${labels[s.id] ?? s.id}</span>
          <span class="agt-step-detail">${s.detail}</span>
        </li>`,
      )}
    </ul>`}
  </div>`;
}

// ------------------------------------------------------------------------------------ lists

function HostList({ label, desc, hosts, placeholder, onChange, addable, empty }) {
  const [draft, setDraft] = useState('');
  const add = (text) => {
    const t = text.trim();
    if (!t) return;
    onChange([...hosts, t]);
    setDraft('');
  };
  return html`<div class="ip-setting is-stacked agt-hosts">
    <div class="ip-setting-text">
      <span class="ip-setting-label">${label}</span>
      <span class="ip-setting-desc">${desc}</span>
    </div>
    ${hosts.length
      ? html`<ul class="agt-chips" aria-label=${label}>
          ${hosts.map(
            (h) => html`<li key=${h} class="agt-chip">
              <span>${h}</span>
              <${IconButton} icon="close" size="sm" iconSize=${12} label=${`Remove ${h}`} muted onClick=${() => onChange(hosts.filter((x) => x !== h))} />
            </li>`,
          )}
        </ul>`
      : html`<span class="agt-none">${empty}</span>`}
    ${addable &&
    html`<div class="agt-add">
      <${TextField} value=${draft} placeholder=${placeholder} spellcheck=${false} onInput=${setDraft} onCommit=${add} aria-label=${`Add to ${label}`} />
      <${Button} size="sm" icon="plus" disabled=${!draft.trim()} onClick=${() => add(draft)}>Add<//>
    </div>`}
  </div>`;
}

function TrustedClients({ clients }) {
  return html`<div class="ip-setting is-stacked">
    <div class="ip-setting-text">
      <span class="ip-setting-label">Trusted clients</span>
      <span class="ip-setting-desc">Programs you allowed with “Always allow” connect without asking. Only signed programs can be trusted; revoke one to be asked again.</span>
    </div>
    ${clients.length
      ? html`<ul class="agt-trusted">
          ${clients.map(
            (c) => html`<li key=${`${c.exe}|${c.signer}`} class="agt-client">
              <span class="agt-client-icon" aria-hidden="true"><${AgentGlyph} size=${16} /></span>
              <div class="agt-client-text">
                <span class="agt-client-name">${c.name || exeName(c.exe)} <span class="agent-verified"><${Icon} name="check" size=${11} strokeWidth=${2} />${c.signer}</span></span>
                <span class="agt-client-exe mono-path" title=${c.exe}>${c.exe}</span>
                <span class="agt-client-date">Trusted ${formatDate(c.addedAt)}</span>
              </div>
              <${Button}
                size="sm"
                variant="ghost"
                class="agt-revoke"
                onClick=${() => update({ agentTrustedClients: clients.filter((x) => !(x.exe === c.exe && x.signer === c.signer)) })}
              >Revoke<//>
            </li>`,
          )}
        </ul>`
      : html`<span class="agt-none">No trusted clients.</span>`}
  </div>`;
}

function PendingApprovals({ agent, state }) {
  if (!agent.prompts.length) return null;
  const answer = (p, allow, remember = false) =>
    send(
      p.kind === 'tab'
        ? { type: 'answerTabAccess', id: p.id, allow }
        : { type: p.kind === 'connection' ? 'answerAgentConnection' : 'answerSitePermission', id: p.id, allow, remember },
    );
  const agentName = (p) => {
    const session = agent.sessions.find((s) => s.session === p.session);
    return session ? clientName(session.client) : 'An agent';
  };
  return html`<div class="ip-card agt-pending" role="region" aria-label="Waiting for your approval">
    ${agent.prompts.map((p) => {
      const tab = p.kind === 'tab' ? allTabs(state).find((t) => t.id === p.tab) : null;
      const title =
        p.kind === 'connection'
          ? `${clientName(p.client)} wants to use sta`
          : p.kind === 'tab'
            ? `${agentName(p)} asks for “${tab ? tab.title || tab.host || tab.url : 'a tab'}”`
            : `Allow ${p.session != null ? agentName(p) : 'an agent'} on ${p.site}?`;
      const sub =
        p.kind === 'connection'
          ? `${p.client.exe ? exeName(p.client.exe) : 'Unknown program'} · ${p.client.verified ? `signed by ${p.client.signer}` : 'unverified'}`
          : p.kind === 'tab'
            ? `The agent says: “${p.reason}”`
            : 'The agent wants to open or act on this site.';
      return html`<div key=${p.id} class="ip-setting agt-prompt" data-prompt=${p.id}>
        <span class="agt-client-icon is-asking" aria-hidden="true"><${AgentGlyph} size=${16} /></span>
        <div class="ip-setting-text">
          <span class="ip-setting-label">${title}</span>
          <span class="ip-setting-desc">${sub}</span>
        </div>
        <div class="ip-setting-control">
          <${Button} size="sm" onClick=${() => answer(p, false)}>Deny<//>
          <${Button} size="sm" variant="primary" onClick=${() => answer(p, true)}>${p.kind === 'connection' ? 'Allow for this session' : p.kind === 'tab' ? 'Share tab' : 'This session'}<//>
          ${(p.kind === 'site' || (p.kind === 'connection' && p.client.verified)) && html`<${Button} size="sm" onClick=${() => answer(p, true, true)}>Always<//>`}
        </div>
      </div>`;
    })}
  </div>`;
}

// ------------------------------------------------------------------------------------ section

export function AgentsSection({ state }) {
  const s = state.settings;
  const agent = state.agent ?? { paused: false, sessions: [], prompts: [], activity: [], heldDownloads: [] };
  const info = useRequest('agent.info', null, [s.agentAccess]);
  const off = s.agentAccess === 'off';
  const connected = agent.sessions.length;
  let status;
  if (agent.paused) status = html`<span class="agt-status is-paused"><${Icon} name="pause" size=${13} />Paused: agents can’t connect or act until you resume.</span>`;
  else if (off) status = null;
  else if (connected) status = html`<span class="agt-status is-live"><span class="agt-dot" />${connected === 1 ? `${clientName(agent.sessions[0].client)} is connected` : `${connected} agents are connected`}</span>`;
  else status = html`<span class="agt-status"><span class="agt-dot is-idle" />Ready. No agent is connected.</span>`;

  return html`<section id="agents" class="set-section agt" aria-labelledby="agents-title">
    <h2 class="set-section-title" id="agents-title">AI agents (MCP)</h2>
    <${PendingApprovals} agent=${agent} state=${state} />
    <div class="ip-card">
      <div class="ip-setting is-stacked">
        <div class="agt-access-head">
          <div class="ip-setting-text">
            <span class="ip-setting-label">Agent access</span>
            <span class="ip-setting-desc">
              Let AI agents such as Claude Code, Claude Desktop, VS Code and Cursor use sta through its MCP server. You approve every new client.
            </span>
          </div>
          <${Segmented} label="Agent access" value=${s.agentAccess} options=${ACCESS_OPTIONS} onChange=${(v) => update({ agentAccess: v })} />
        </div>
        <p class="agt-access-desc">${ACCESS_DESCRIPTIONS[s.agentAccess] ?? ''}</p>
        ${(status || agent.paused || connected > 0) &&
        html`<div class="agt-status-row">
          ${status}
          ${agent.paused
            ? html`<${Button} size="sm" icon="play" onClick=${() => send({ type: 'resumeAgents' })}>Resume<//>`
            : connected > 0 && html`<${Button} size="sm" variant="danger" icon="pause" onClick=${() => send({ type: 'stopAgents' })}>Stop all agents<//>`}
        </div>`}
        <div class="agt-privacy" role="note">
          <${Icon} name="info" size=${15} />
          <span>
            What an agent reads — page text, screenshots, what you typed into forms — goes to the AI provider behind your client. Agents act as you, with
            your sign-ins, on the sites you allow. sta keeps a log of what agents did (never page content or typed text).
          </span>
        </div>
      </div>
    </div>

    <h3 class="agt-subtitle">What agents can use</h3>
    <div class=${classNames('ip-card', off && 'is-off')}>
      <div class="ip-setting">
        <div class="ip-setting-text">
          <span class="ip-setting-label">Tabs agents can see</span>
          <span class="ip-setting-desc">
            ${s.agentScope === 'allTabs'
              ? 'Every tab in every space, including what you’re signed in to.'
              : 'Only tabs agents opened (any agent, in any session) and tabs you share from their right-click menu.'}
          </span>
        </div>
        <${Segmented} label="Tabs agents can see" value=${s.agentScope} options=${SCOPE_OPTIONS} onChange=${(v) => update({ agentScope: v })} />
      </div>
      <div class="ip-setting">
        <div class="ip-setting-text">
          <span class="ip-setting-label">Ask before a new site</span>
          <span class="ip-setting-desc">An agent needs your OK the first time it opens or acts on a site. Turn off to allow every site.</span>
        </div>
        <${Toggle} checked=${s.agentSites !== 'all'} ariaLabel="Ask before a new site" onChange=${(v) => update({ agentSites: v ? 'ask' : 'all' })} />
      </div>
      <${HostList}
        label="Always allowed sites"
        desc="Sites you allowed with “Always”."
        hosts=${s.agentAllowedSites ?? []}
        empty="None yet."
        onChange=${(list) => update({ agentAllowedSites: list })}
      />
      <${HostList}
        label="Blocked sites"
        desc="Agents never open or act on these sites (subdomains included), even when you allow everything else."
        hosts=${s.agentBlockedHosts ?? []}
        placeholder="bank.example.com"
        addable=${true}
        empty="No blocked sites."
        onChange=${(list) => update({ agentBlockedHosts: list })}
      />
      <div class="ip-setting">
        <div class="ip-setting-text">
          <span class="ip-setting-label">Devices on your network</span>
          <span class="ip-setting-desc">Routers, printers, NAS and other local addresses (192.168.x.x, .local names). This computer (localhost) is always allowed.</span>
        </div>
        <${Toggle}
          checked=${s.agentAllowPrivateNetwork}
          ariaLabel="Devices on your network"
          onChange=${(v) => update({ agentAllowPrivateNetwork: v })}
        />
      </div>
    </div>

    <h3 class="agt-subtitle">Scripts, history and downloads</h3>
    <div class=${classNames('ip-card', off && 'is-off')}>
      <div class="ip-setting">
        <div class="ip-setting-text">
          <span class="ip-setting-label">Page scripts</span>
          <span class="ip-setting-desc">
            Lets agents run their own JavaScript in pages (the evaluate tool). A script can read anything the page can — including form fields, passwords
            you typed and the page’s cookies — and send requests as you. “Isolated” hides the site’s own JavaScript from the script, but the script can still
            add code to the page that runs with the site’s scripts; “Page” runs it with them directly.
          </span>
        </div>
        <${Segmented} label="Page scripts" value=${s.agentScripts} options=${SCRIPT_OPTIONS} onChange=${(v) => update({ agentScripts: v })} />
      </div>
      <div class="ip-setting">
        <div class="ip-setting-text">
          <span class="ip-setting-label">Browsing history</span>
          <span class="ip-setting-desc">Let agents search the titles and addresses of pages you visited (history_search). Pages agents can’t open are never listed.</span>
        </div>
        <${Toggle} checked=${s.agentHistory} ariaLabel="Browsing history" onChange=${(v) => update({ agentHistory: v })} />
      </div>
      <div class="ip-setting">
        <div class="ip-setting-text">
          <span class="ip-setting-label">Downloads list</span>
          <span class="ip-setting-desc">Let agents see file names and states of your downloads (downloads_list), never folders or addresses.</span>
        </div>
        <${Toggle} checked=${s.agentDownloads} ariaLabel="Downloads list" onChange=${(v) => update({ agentDownloads: v })} />
      </div>
    </div>

    <h3 class="agt-subtitle">Clients</h3>
    <div class="ip-card">
      <${TrustedClients} clients=${s.agentTrustedClients ?? []} />
    </div>
    <${SetupCard} info=${info.data} />
    ${info.data?.logPath &&
    html`<p class="agt-log">
      Activity log: <span class="mono-path selectable" title=${info.data.logPath}>${info.data.logPath}</span>
      <${IconButton} icon="copy" size="sm" label="Copy log path" muted onClick=${() => send({ type: 'copyText', text: info.data.logPath })} />
    </p>`}
  </section>`;
}
