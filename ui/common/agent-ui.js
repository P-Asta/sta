// AI agents (MCP): shared helpers of the agent overlay, the topbar chip, the sidebar glyph and the
// Settings section (PROTOCOL §9, docs/MCP.md). Pure view helpers: no IPC.

import { h } from './vendor/htm-preact.js';
import { allTabs } from './util.js';

/** A four-point sparkle: "an AI agent". `strokeWidth` follows icons.js. */
export function AgentGlyph({ size = 16, strokeWidth = 1.6, class: className }) {
  return h(
    'svg',
    {
      class: ['agent-glyph', className].filter(Boolean).join(' '),
      width: size,
      height: size,
      viewBox: '0 0 24 24',
      fill: 'none',
      stroke: 'currentColor',
      'stroke-width': strokeWidth,
      'stroke-linejoin': 'round',
      'stroke-linecap': 'round',
      'aria-hidden': 'true',
      focusable: 'false',
    },
    [
      h('path', { key: 0, d: 'M11 3.5c.5 3.9 2.6 6 6.5 6.5-3.9.5-6 2.6-6.5 6.5-.5-3.9-2.6-6-6.5-6.5 3.9-.5 6-2.6 6.5-6.5Z' }),
      h('path', { key: 1, d: 'M18 14.5c.25 1.8 1.2 2.75 3 3-1.8.25-2.75 1.2-3 3-.25-1.8-1.2-2.75-3-3 1.8-.25 2.75-1.2 3-3Z' }),
    ],
  );
}

/** What the client calls itself (self-reported): title, else name. */
export function clientName(client) {
  const t = String(client?.title ?? '').trim();
  const n = String(client?.name ?? '').trim();
  return (t || n || 'Unknown client').slice(0, 80);
}

/** File name of a Windows path (`C:\\a\\b.exe` → `b.exe`). */
export function exeName(path) {
  const p = String(path ?? '');
  return p.slice(Math.max(p.lastIndexOf('\\'), p.lastIndexOf('/')) + 1) || p;
}

export const ACCESS_LABELS = { off: 'Off', readOnly: 'Read only', full: 'Full access' };

/** Past-tense phrases for the activity list (tool names from the MCP tool catalog). */
const TOOL_PHRASES = {
  tabs_list: 'Listed tabs',
  tab_open: 'Opened a tab',
  tab_navigate: 'Navigated',
  tab_show: 'Showed a tab',
  tab_close: 'Closed a tab',
  request_tab_access: 'Asked for a tab',
  page_snapshot: 'Read the page',
  page_text: 'Read the text',
  page_find: 'Searched the page',
  page_screenshot: 'Took a screenshot',
  click: 'Clicked',
  hover: 'Hovered',
  type: 'Typed',
  press_key: 'Pressed a key',
  select_option: 'Chose an option',
  scroll: 'Scrolled',
  fill_form: 'Filled a form',
  wait_for: 'Waited',
  handle_dialog: 'Answered a dialog',
  evaluate: 'Ran a script',
  console_messages: 'Read the console',
  history_search: 'Searched history',
  downloads_list: 'Listed downloads',
};

export function toolPhrase(tool) {
  return TOOL_PHRASES[tool] ?? String(tool ?? '').replace(/_/g, ' ');
}

/** Short reasons for failed actions in the activity list (tool error codes, docs/MCP.md §4). */
const ERROR_PHRASES = {
  site_not_approved: 'needs your OK for the site',
  site_blocked: 'blocked site',
  not_in_scope: 'not a tab it may use',
  url_not_allowed: 'address not allowed',
  internal_page: 'sta page',
  paused: 'agents paused',
  read_only: 'read-only access',
  access_off: 'access is off',
  tab_not_visible: 'tab not on screen',
  user_active: 'you were typing',
  file_chooser_blocked: 'file upload blocked',
  stale_ref: 'page changed',
  timeout: 'timed out',
  not_approved: 'you didn’t approve it',
  scripts_disabled: 'scripts are off',
  history_disabled: 'history is off',
  downloads_disabled: 'downloads list is off',
  script_error: 'its script failed',
  element_obscured: 'something covered it',
  dialog_open: 'a dialog is open',
  busy: 'too many requests',
};

export function errorPhrase(code) {
  return ERROR_PHRASES[code] ?? String(code ?? '').replace(/_/g, ' ');
}

/** Reading tools (never change a page). */
const READ_TOOLS = new Set(['tabs_list', 'page_snapshot', 'page_text', 'page_find', 'page_screenshot', 'wait_for', 'console_messages', 'history_search', 'downloads_list']);
export const isReadTool = (tool) => READ_TOOLS.has(tool);

/** "now", "12 s", "3 min", "2 h" (compact, for activity rows). */
export function ago(ms, now = Date.now()) {
  const s = Math.max(0, Math.round((now - ms) / 1000));
  if (s < 3) return 'now';
  if (s < 60) return `${s} s`;
  const m = Math.round(s / 60);
  if (m < 60) return `${m} min`;
  return `${Math.round(m / 60)} h`;
}

/** Title of a tab in the state, if it is still open. */
export function tabTitle(state, id) {
  if (id == null) return null;
  const tab = allTabs(state).find((t) => t.id === id);
  return tab ? tab.title || tab.host || tab.url : null;
}

/** An agent acted within the last `ms` (the chip's pulse). */
export function recentlyActive(agent, ms = 2500, now = Date.now()) {
  const last = agent?.activity?.[0];
  return !!last && now - last.at < ms;
}

/** The chip is shown when there is anything agent-related to see or do. */
export function agentChipVisible(agent) {
  return !!agent && (agent.paused || agent.sessions.length > 0 || agent.prompts.length > 0 || agent.heldDownloads.length > 0);
}
