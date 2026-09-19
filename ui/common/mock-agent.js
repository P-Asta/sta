// Mock mode for the AI agent UI (PROTOCOL §9): reducers for the agent commands, sample agent state
// for screenshots (`?agent=<scenario>`), and the `agent.*` requests of the Settings page.
//
//   agent=connection     a connection prompt from a verified client (Claude Code)
//   agent=unverified     a connection prompt from an unsigned program (with a second one queued)
//   agent=site           a site prompt from a connected agent
//   agent=tab            a tab access prompt (request_tab_access) from a connected agent
//   agent=panel          the activity panel: one agent connected, recent actions, a held download
//   agent=busy           two agents connected and acting (the chip pulses), no overlay
//   agent=paused         Stop was pressed (panel open)
//   agentAccess=<level>  settings.agentAccess (off|readOnly|full); the scenarios above imply full
//
// Imported by mock-reducers.js (reducers) and mock.js (params, requests); no other dependencies.

const now = () => Date.now();

const CLAUDE_CODE = {
  name: 'claude-code',
  title: 'Claude Code',
  version: '2.1.268',
  exe: 'C:\\Users\\you\\AppData\\Local\\Programs\\claude\\claude.exe',
  signer: 'Anthropic, PBC',
  verified: true,
};

const UNSIGNED = {
  name: 'my-mcp-client',
  title: null,
  version: '0.3.0',
  exe: 'C:\\Users\\you\\projects\\agent-runner\\target\\debug\\runner.exe',
  signer: null,
  verified: false,
};

const CURSOR = {
  name: 'cursor-vscode',
  title: 'Cursor',
  version: '1.9.2',
  exe: 'C:\\Users\\you\\AppData\\Local\\Programs\\cursor\\Cursor.exe',
  signer: 'Anysphere, Inc.',
  verified: true,
};

/** An empty `UiState.agent` (older fixtures may lack it). */
export function emptyAgent() {
  return { paused: false, sessions: [], prompts: [], activity: [], heldDownloads: [], panelOpen: false, openedTabs: 0 };
}

function ensureAgent(state) {
  state.agent = { ...emptyAgent(), ...(state.agent ?? {}) };
  return state.agent;
}

/** Sample agent tabs: marks up to `n` loaded Today tabs of the active space as agent tabs. */
function markAgentTabs(state, n) {
  const space = state.spaces.find((s) => s.id === state.activeSpace) ?? state.spaces[0];
  const ids = [];
  for (const node of space?.today ?? []) {
    if (ids.length >= n) break;
    if (node.kind === 'tab') {
      node.agent = true;
      ids.push(node.id);
    }
  }
  return ids;
}

/** Applies `?agent=` / `?agentAccess=` (mock.js `applyParams`). */
export function applyAgentParams(state, params) {
  const agent = ensureAgent(state);
  const scenario = params.get('agent');
  if (scenario) state.settings.agentAccess = 'full';
  if (params.has('agentAccess')) state.settings.agentAccess = params.get('agentAccess');
  if (!scenario) return;
  const t = now();
  const tabs = markAgentTabs(state, 2);
  const session = (id, client, startedAgo) => ({ session: id, client, access: 'full', startedAt: t - startedAgo });
  let activity = [
    { session: 1, tool: 'click', tab: tabs[0] ?? null, site: 'github.com', at: t - 1_200 },
    { session: 1, tool: 'page_snapshot', tab: tabs[0] ?? null, site: 'github.com', at: t - 4_000 },
    { session: 1, tool: 'type', tab: tabs[0] ?? null, site: 'github.com', at: t - 9_000 },
    { session: 1, tool: 'tab_navigate', tab: null, site: null, at: t - 20_000, error: 'site_blocked' },
    { session: 1, tool: 'tab_open', tab: tabs[1] ?? null, site: 'docs.rs', at: t - 31_000 },
    { session: 1, tool: 'page_text', tab: tabs[1] ?? null, site: 'docs.rs', at: t - 64_000 },
  ];
  activity = activity.slice(0, 5);
  switch (scenario) {
    case 'connection':
      agent.prompts = [{ id: 11, kind: 'connection', client: CLAUDE_CODE, requestedAt: t - 2_000 }];
      break;
    case 'unverified':
      agent.prompts = [
        { id: 12, kind: 'connection', client: UNSIGNED, requestedAt: t - 3_000 },
        { id: 13, kind: 'connection', client: CURSOR, requestedAt: t - 1_000 },
      ];
      break;
    case 'site':
      agent.sessions = [session(1, CLAUDE_CODE, 180_000)];
      agent.prompts = [{ id: 14, kind: 'site', session: 1, site: 'github.com', tab: tabs[0] ?? null, requestedAt: t - 1_500 }];
      agent.activity = activity.slice(3);
      agent.openedTabs = tabs.length;
      break;
    case 'tab': {
      agent.sessions = [session(1, CLAUDE_CODE, 240_000)];
      const space = state.spaces.find((s) => s.id === state.activeSpace) ?? state.spaces[0];
      const userTab = (space?.today ?? []).find((n) => n.kind === 'tab' && !n.agent) ?? (space?.today ?? []).find((n) => n.kind === 'tab');
      agent.prompts = [
        {
          id: 15,
          kind: 'tab',
          session: 1,
          tab: userTab?.id ?? 1,
          reason: 'You asked me to summarize the page you have open; I need to read it.',
          requestedAt: t - 1_200,
        },
      ];
      agent.activity = activity.slice(0, 2);
      agent.openedTabs = tabs.length;
      break;
    }
    case 'panel':
      agent.sessions = [session(1, CLAUDE_CODE, 420_000)];
      agent.activity = activity;
      agent.heldDownloads = [{ id: 7, tab: tabs[0] ?? null, fileName: 'release-notes-2026-09.pdf' }];
      agent.panelOpen = true;
      agent.openedTabs = tabs.length;
      break;
    case 'busy':
      agent.sessions = [session(1, CLAUDE_CODE, 420_000), session(2, CURSOR, 60_000)];
      agent.activity = activity.map((a) => ({ ...a, at: a.at + 1_000 }));
      agent.openedTabs = tabs.length;
      break;
    case 'paused':
      agent.paused = true;
      agent.activity = activity.map((a) => ({ ...a, at: a.at - 20_000 }));
      agent.panelOpen = true;
      agent.openedTabs = tabs.length;
      break;
    default:
      console.warn(`[mock] unknown agent scenario "${scenario}"`);
  }
}

const removeAgentFlags = (state, ids) => {
  const walk = (nodes) => {
    for (const n of nodes ?? []) {
      if (ids.includes(n.id)) delete n.agent;
      if (n.children) walk(n.children);
      if (n.panes) walk(n.panes);
    }
  };
  for (const s of state.spaces) walk(s.today);
};

/** Command reducers (merged into mock-reducers.js `reducers`). */
export const agentReducers = {
  answerAgentConnection: (ctx, { id, allow, remember }) => {
    const agent = ensureAgent(ctx.state);
    const prompt = agent.prompts.find((p) => p.id === id && p.kind === 'connection');
    if (!prompt) return;
    agent.prompts = agent.prompts.filter((p) => p !== prompt);
    if (!allow) return;
    const session = Math.max(0, ...agent.sessions.map((s) => s.session)) + 1;
    agent.sessions.push({ session, client: prompt.client, access: ctx.state.settings.agentAccess === 'readOnly' ? 'readOnly' : 'full', startedAt: now() });
    if (remember && prompt.client.verified) {
      const list = ctx.state.settings.agentTrustedClients;
      if (!list.some((c) => c.exe.toLowerCase() === prompt.client.exe.toLowerCase() && c.signer === prompt.client.signer)) {
        list.push({ name: prompt.client.title || prompt.client.name, exe: prompt.client.exe, signer: prompt.client.signer, addedAt: now() });
      }
    }
  },
  answerSitePermission: (ctx, { id, allow, remember }) => {
    const agent = ensureAgent(ctx.state);
    const prompt = agent.prompts.find((p) => p.id === id && p.kind === 'site');
    if (!prompt) return;
    agent.prompts = agent.prompts.filter((p) => !(p.kind === 'site' && p.session === prompt.session && p.site === prompt.site));
    const sites = ctx.state.settings.agentAllowedSites;
    if (allow && remember && !sites.includes(prompt.site)) sites.push(prompt.site);
  },
  answerTabAccess: (ctx, { id, allow }) => {
    const agent = ensureAgent(ctx.state);
    const prompt = agent.prompts.find((p) => p.id === id && p.kind === 'tab');
    if (!prompt) return;
    agent.prompts = agent.prompts.filter((p) => !(p.kind === 'tab' && p.session === prompt.session && p.tab === prompt.tab));
    if (allow) agentReducers.shareTabWithAgent(ctx, { tab: prompt.tab, shared: true });
  },
  stopAgents: (ctx) => {
    const agent = ensureAgent(ctx.state);
    agent.paused = true;
    agent.sessions = [];
    agent.prompts = [];
  },
  resumeAgents: (ctx) => {
    ensureAgent(ctx.state).paused = false;
  },
  shareTabWithAgent: (ctx, { tab, shared = true }) => {
    const walk = (nodes) => {
      for (const n of nodes ?? []) {
        if (n.id === tab && n.kind !== 'folder' && n.kind !== 'split') {
          if (shared) n.agent = true;
          else delete n.agent;
        }
        if (n.children) walk(n.children);
        if (n.panes) walk(n.panes);
      }
    };
    walk(ctx.state.favorites);
    for (const s of ctx.state.spaces) {
      walk(s.pinned);
      walk(s.today);
    }
  },
  resolveAgentDownload: (ctx, { id }) => {
    const agent = ensureAgent(ctx.state);
    agent.heldDownloads = agent.heldDownloads.filter((d) => d.id !== id);
  },
  toggleAgentPanel: (ctx) => {
    const agent = ensureAgent(ctx.state);
    agent.panelOpen = !agent.panelOpen;
  },
  closeAgentPanel: (ctx) => {
    ensureAgent(ctx.state).panelOpen = false;
  },
  archiveAgentTabs: (ctx) => {
    const state = ctx.state;
    const agent = ensureAgent(state);
    const space = state.spaces.find((s) => s.id === state.activeSpace);
    if (!space) return;
    const ids = space.today.filter((n) => n.kind === 'tab' && n.agent && !n.active).map((n) => n.id);
    space.today = space.today.filter((n) => !ids.includes(n.id));
    removeAgentFlags(state, ids);
    agent.openedTabs = 0;
    if (ids.length) state.toast = { id: Date.now(), message: `Archived ${ids.length} agent ${ids.length === 1 ? 'tab' : 'tabs'}`, action: { label: 'Undo', command: { type: 'reopenClosed' } }, durationMs: 6000 };
  },
};

/** `agent.info` / `agent.testConnection` (Settings). */
export const agentRequests = (getState) => ({
  'agent.info': () => ({
    bridgePath: 'C:\\Program Files\\sta\\sta-mcp.exe',
    bridgeFound: true,
    dataDir: 'C:\\Users\\you\\AppData\\Local\\sta',
    dataDirIsDefault: true,
    endpointOpen: getState().settings.agentAccess !== 'off',
    logPath: 'C:\\Users\\you\\AppData\\Local\\sta\\Logs\\agent.log',
    build: '0.1.0',
  }),
  'agent.testConnection': () =>
    new Promise((resolve) => {
      const on = getState().settings.agentAccess !== 'off';
      const steps = [
        on ? { id: 'access', ok: true, detail: 'Full access is on' } : { id: 'access', ok: false, detail: 'AI agent access is off. Choose Read only or Full access above.' },
        on ? { id: 'endpoint', ok: true, detail: 'sta is listening for agents on a private pipe' } : { id: 'endpoint', ok: false, detail: "sta isn't listening for agents (turn access on)" },
        { id: 'bridge', ok: true, detail: 'C:\\Program Files\\sta\\sta-mcp.exe' },
      ];
      if (on) steps.push({ id: 'channel', ok: true, detail: 'The MCP server reached sta (pid 12345) and verified its channel' });
      setTimeout(() => resolve({ ok: steps.every((s) => s.ok), steps, ms: 420 }), 450);
    }),
});
