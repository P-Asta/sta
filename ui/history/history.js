// History page (sta://history/). `history.list {query, limit}` runs with a debounced query
// and again whenever `state.historyRevision` changes. Without a query, rows are the most recent
// pages grouped by day; with a query, core ranks them, so they stay in that order.
// Click opens the page in a new tab (middle/Ctrl+click: background tab).

import { html, useEffect, useMemo, useRef, useState } from '/common/vendor/htm-preact.js';
import { dispatch } from '/common/ipc.js';
import { useRequest } from '/common/ipc-hooks.js';
import { useDebouncedValue } from '/common/hooks.js';
import { Button, Favicon, IconButton, Menu } from '/common/components.js';
import {
  ConfirmButton,
  EmptyState,
  PageHeader,
  SearchField,
  faviconsByHost,
  groupByDay,
  mountPage,
  useListKeyboard,
  useListRowMotion,
  useStuck,
} from '/common/internal-page.js';
import { classNames, formatDate, formatTime } from '/common/util.js';

const report = (e) => console.error('[history]', e);
const send = (command) => dispatch(command).catch(report);

const PAGE = 300;
const SEARCH_DEBOUNCE_MS = 150;

const open = (entry, background = false) => send({ type: 'openUrl', url: entry.url, target: background ? 'backgroundTab' : 'newTab' });
const remove = (entry) => send({ type: 'deleteHistoryEntry', url: entry.url });

function focusNeighbor(row) {
  const rows = [...document.querySelectorAll('[data-row]')];
  const i = rows.indexOf(row);
  (rows[i + 1] ?? rows[i - 1])?.focus();
}

function EntryRow({ entry, favicon, showDate, menuOpen, onMenu }) {
  const onKeyDown = (e) => {
    if (e.target !== e.currentTarget) return;
    if (e.key === 'Enter') {
      e.preventDefault();
      open(entry, e.ctrlKey || e.altKey);
    } else if (e.key === 'Delete') {
      e.preventDefault();
      focusNeighbor(e.currentTarget);
      remove(entry);
    } else if (e.key === 'ContextMenu' || (e.shiftKey && e.key === 'F10')) {
      e.preventDefault();
      onMenu(entry, { anchor: e.currentTarget });
    }
  };
  const visits = entry.visitCount > 1 ? `${entry.visitCount} visits` : null;
  return html`<div
    class=${classNames('ip-row hist-row', menuOpen && 'is-menu-open')}
    data-row
    data-id=${entry.url}
    tabindex="0"
    role="link"
    aria-label=${entry.title || entry.url}
    title=${entry.url}
    onClick=${(e) => open(entry, e.ctrlKey)}
    onAuxClick=${(e) => {
      if (e.button === 1) {
        e.preventDefault();
        open(entry, true);
      }
    }}
    onMouseDown=${(e) => e.button === 1 && e.preventDefault()}
    onKeyDown=${onKeyDown}
    onContextMenu=${(e) => {
      e.preventDefault();
      onMenu(entry, { x: e.clientX, y: e.clientY });
    }}
  >
    <span class="ip-row-icon"><${Favicon} src=${favicon ?? null} host=${entry.host} size=${16} lazy=${false} /></span>
    <span class="ip-row-main">
      <span class="ip-row-title">${entry.title || entry.url}</span>
      <span class="ip-row-sub">
        <span class="ip-row-host">${entry.host || entry.url}</span>
        ${visits && html`<span class="ip-dot" /><span>${visits}</span>`}
      </span>
    </span>
    <span class="ip-row-meta">
      ${showDate && html`<span>${formatDate(entry.lastVisitAt)}</span>`}
      <span class="hist-time">${formatTime(entry.lastVisitAt)}</span>
    </span>
    <span class="ip-row-actions">
      <${IconButton}
        icon="trash"
        label="Remove from history"
        size="sm"
        muted
        onClick=${(e) => {
          e.stopPropagation();
          remove(entry);
        }}
      />
      <${IconButton}
        icon="more"
        label="More actions"
        size="sm"
        aria-haspopup="menu"
        aria-expanded=${String(menuOpen)}
        onClick=${(e) => {
          e.stopPropagation();
          onMenu(entry, { anchor: e.currentTarget });
        }}
      />
    </span>
  </div>`;
}

function History({ state }) {
  const [query, setQuery] = useState('');
  const [limit, setLimit] = useState(PAGE);
  const [menu, setMenu] = useState(null);
  const toolbarRef = useRef(null);
  const listRef = useRef(null);
  const stuck = useStuck(toolbarRef);
  useListKeyboard(listRef);

  const debounced = useDebouncedValue(query.trim(), SEARCH_DEBOUNCE_MS);
  // An emptied search box applies at once; typing is debounced.
  const effectiveQuery = query.trim() === '' ? '' : debounced;
  useEffect(() => setLimit(PAGE), [effectiveQuery]);
  const { data, error } = useRequest('history.list', { query: effectiveQuery, limit }, [state.historyRevision]);

  // Keep the previous rows while a new query runs (no flash of the empty state).
  const last = useRef({ rows: [], query: '' });
  if (Array.isArray(data)) last.current = { rows: data, query: effectiveQuery };
  const rows = last.current.rows;
  const searching = last.current.query !== '';
  const groups = useMemo(() => (searching ? null : groupByDay(rows, (e) => e.lastVisitAt)), [rows, searching]);
  const favicons = useMemo(() => faviconsByHost(state), [state.revision]);
  // `pages.listRows`: removing an entry fades that row out and glides the rest up. Called from the
  // body with the ids this render is about to show, which is the only moment a row that is leaving
  // can still be measured and cloned. History rows are keyed by URL, as they are in the DOM.
  useListRowMotion(listRef, rows.map((e) => e.url));

  const menuItems = (entry) => [
    { label: 'Open in new tab', icon: 'plus', onSelect: () => open(entry) },
    { label: 'Open in background', icon: 'external', onSelect: () => open(entry, true) },
    { label: 'Copy URL', icon: 'copy', onSelect: () => send({ type: 'copyText', text: entry.url }) },
    { type: 'separator' },
    { label: 'Remove from history', icon: 'trash', danger: true, onSelect: () => remove(entry) },
  ];

  const renderRow = (entry, showDate) => html`<${EntryRow}
    key=${entry.url}
    entry=${entry}
    favicon=${favicons.get(entry.host)}
    showDate=${showDate}
    menuOpen=${menu?.entry.url === entry.url}
    onMenu=${(e, where) => setMenu({ entry: e, ...where })}
  />`;

  let body;
  if (data === undefined && !error && !rows.length) {
    body = null;
  } else if (error && !Array.isArray(data) && !rows.length) {
    body = html`<${EmptyState} icon="warning" title="Couldn't load history">${String(error.message ?? error)}<//>`;
  } else if (!rows.length) {
    body = searching
      ? html`<${EmptyState} icon="search" title="No matches">No pages in your history match “${last.current.query}”.<//>`
      : html`<${EmptyState} icon="history" title="No history yet">Pages you visit show up here, so you can find them again.<//>`;
  } else if (groups) {
    body = groups.map(
      (g) => html`<section key=${g.key} class="ip-group" aria-label=${g.label}>
        <h2 class="ip-group-label">${g.label}</h2>
        <div class="ip-list">${g.items.map((entry) => renderRow(entry, false))}</div>
      </section>`,
    );
  } else {
    body = html`<section class="ip-group" aria-label="Search results">
      <h2 class="ip-group-label">Best matches<span class="ip-group-count">${rows.length}${rows.length >= limit ? '+' : ''}</span></h2>
      <div class="ip-list">${rows.map((entry) => renderRow(entry, true))}</div>
    </section>`;
  }

  return html`<div class="ip">
    <${PageHeader} icon="history" title="History" subtitle="Pages you visited, most recent first.">
      <${ConfirmButton}
        label="Clear history"
        icon="trash"
        disabled=${!rows.length && !searching}
        title="Clear all history?"
        message="This removes every page from your history, including suggestions in the command bar. Open tabs and your archive are not affected."
        confirmLabel="Clear history"
        onConfirm=${() => send({ type: 'clearHistory' })}
      />
    <//>
    <div class=${classNames('ip-toolbar', stuck && 'is-stuck')} ref=${toolbarRef}>
      <${SearchField} value=${query} onInput=${setQuery} placeholder="Search history" label="Search history" autoFocus />
    </div>
    <div ref=${listRef}>${body}</div>
    ${rows.length >= limit &&
    html`<${Button} class="ip-more" variant="ghost" onClick=${() => setLimit((l) => l + PAGE)}>Show more<//>`}
    ${menu &&
    html`<${Menu}
      items=${menuItems(menu.entry)}
      x=${menu.x}
      y=${menu.y}
      anchor=${menu.anchor}
      placement=${menu.anchor ? 'bottom-end' : 'bottom-start'}
      label="History entry"
      onClose=${() => setMenu(null)}
    />`}
  </div>`;
}

mountPage(History);
