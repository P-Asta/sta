// Archive page (sta://archive/, arc_spec §2.9). `archive.list` is refetched whenever
// `state.archiveRevision` changes. Entries are filtered locally and grouped by day. Click (or
// Enter) restores an entry; the row menu has Restore, Restore split, Copy URL and Delete.

import { html, useMemo, useRef, useState } from '/common/vendor/htm-preact.js';
import { dispatch } from '/common/ipc.js';
import { useRequest } from '/common/ipc-hooks.js';
import { Icon } from '/common/icons.js';
import { Button, Favicon, IconButton, Menu, useCountPop } from '/common/components.js';
import {
  ConfirmButton,
  EmptyState,
  PageHeader,
  SearchField,
  groupByDay,
  loadableFavicon,
  matchesQuery,
  mountPage,
  useListKeyboard,
  useListRowMotion,
  useStuck,
} from '/common/internal-page.js';
import { classNames, formatTime } from '/common/util.js';

const report = (e) => console.error('[archive]', e);
const send = (command) => dispatch(command).catch(report);

const REASONS = {
  auto: { label: 'Auto', title: 'Archived automatically after a period of inactivity' },
  userClosed: { label: 'Closed', title: 'You closed this tab' },
  clearToday: { label: 'Cleared', title: 'Archived by Clear Today' },
  spaceDeleted: { label: 'Space deleted', title: 'Its space was deleted' },
  folderDeleted: { label: 'Folder deleted', title: 'Its folder was deleted' },
};

export function archiveAfterLabel(hours) {
  if (hours >= 24 && hours % 24 === 0) {
    const days = hours / 24;
    return days === 1 ? '24 hours' : `${days} days`;
  }
  return `${hours} hours`;
}

const restore = (entry, wholeGroup = false) => send({ type: 'restoreArchived', id: entry.id, wholeGroup });
const remove = (entry) => send({ type: 'deleteArchived', id: entry.id });

function focusNeighbor(row) {
  const rows = [...document.querySelectorAll('[data-row]')];
  const i = rows.indexOf(row);
  (rows[i + 1] ?? rows[i - 1])?.focus();
}

function EntryRow({ entry, splitSize, menuOpen, onMenu }) {
  const reason = REASONS[entry.reason] ?? { label: entry.reason, title: '' };
  const inSplit = entry.group != null && splitSize > 1;
  const onKeyDown = (e) => {
    if (e.target !== e.currentTarget) return;
    if (e.key === 'Enter' || e.key === ' ') {
      e.preventDefault();
      restore(entry);
    } else if (e.key === 'Delete') {
      e.preventDefault();
      focusNeighbor(e.currentTarget);
      remove(entry);
    } else if (e.key === 'ContextMenu' || (e.shiftKey && e.key === 'F10')) {
      e.preventDefault();
      onMenu(entry, { anchor: e.currentTarget.querySelector('.arc-more') ?? e.currentTarget });
    }
  };
  return html`<div
    class=${classNames('ip-row arc-row', menuOpen && 'is-menu-open')}
    data-row
    data-id=${entry.id}
    tabindex="0"
    role="button"
    aria-label=${`Restore ${entry.title || entry.url}`}
    title=${entry.url}
    onClick=${() => restore(entry)}
    onKeyDown=${onKeyDown}
    onContextMenu=${(e) => {
      e.preventDefault();
      onMenu(entry, { x: e.clientX, y: e.clientY });
    }}
  >
    <span class="ip-row-icon"><${Favicon} src=${loadableFavicon(entry.favicon)} host=${entry.host} size=${16} /></span>
    <span class="ip-row-main">
      <span class="ip-row-title">${entry.title || entry.url}</span>
      <span class="ip-row-sub">
        <span class="ip-row-host">${entry.host || entry.url}</span>
        ${inSplit && html`<span class="ip-badge is-accent" title=${`Part of a split view with ${splitSize} tabs`}><${Icon} name="split" size=${11} />Split</span>`}
      </span>
    </span>
    <span class="ip-row-meta">
      ${entry.spaceIcon && html`<span class="arc-space emoji" title=${entry.spaceName ?? ''}>${entry.spaceIcon}</span>`}
      <span class=${classNames('ip-badge', entry.reason === 'auto' && 'is-auto')} title=${reason.title}>${reason.label}</span>
      <span class="arc-time">${formatTime(entry.archivedAt)}</span>
    </span>
    <span class="ip-row-actions">
      ${inSplit &&
      html`<${Button}
        size="sm"
        variant="ghost"
        icon="split"
        onClick=${(e) => {
          e.stopPropagation();
          restore(entry, true);
        }}
      >Restore split<//>`}
      <${IconButton}
        class="arc-more"
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

function Archive({ state }) {
  const [query, setQuery] = useState('');
  const [menu, setMenu] = useState(null);
  const toolbarRef = useRef(null);
  const listRef = useRef(null);
  const countRef = useRef(null);
  const stuck = useStuck(toolbarRef);
  useListKeyboard(listRef);
  const { data, error } = useRequest('archive.list', null, [state.archiveRevision]);
  const entries = Array.isArray(data) ? data : [];

  const splitSizes = useMemo(() => {
    const sizes = new Map();
    for (const e of entries) if (e.group != null) sizes.set(e.group, (sizes.get(e.group) ?? 0) + 1);
    return sizes;
  }, [data]);
  const filtered = useMemo(() => entries.filter((e) => matchesQuery(query, e.title, e.url, e.host, e.spaceName)), [data, query]);
  const groups = useMemo(() => groupByDay(filtered, (e) => e.archivedAt), [filtered]);
  // `pages.listRows`: restoring or deleting an entry fades that row out and glides the rest up.
  // Called with the ids this render is about to show, from the body — that is the only moment the
  // rows that are leaving can still be measured and cloned.
  useListRowMotion(listRef, filtered.map((e) => e.id));

  const menuItems = (entry) => {
    const items = [{ label: 'Restore', icon: 'restore', onSelect: () => restore(entry) }];
    if (entry.group != null && (splitSizes.get(entry.group) ?? 0) > 1) items.push({ label: 'Restore split', icon: 'split', onSelect: () => restore(entry, true) });
    items.push(
      { label: 'Copy URL', icon: 'copy', onSelect: () => send({ type: 'copyText', text: entry.url }) },
      { type: 'separator' },
      { label: 'Delete from archive', icon: 'trash', danger: true, onSelect: () => remove(entry) },
    );
    return items;
  };

  const count = entries.length;
  // `indicators.badges`: the count pops when the archive itself changed. Keyed on the total, never
  // on the filtered number, which changes on every keystroke in the search field.
  useCountPop(countRef, count);
  let body;
  if (data === undefined && !error) {
    body = null;
  } else if (error && data === undefined) {
    body = html`<${EmptyState} icon="warning" title="Couldn't load the archive">${String(error.message ?? error)}<//>`;
  } else if (!count) {
    body = html`<${EmptyState} icon="archive" title="Your archive is empty">
      Tabs you close, and Today tabs you haven't used for ${archiveAfterLabel(state.settings.archiveAfterHours)}, are kept here for 30 days.
    <//>`;
  } else if (!filtered.length) {
    body = html`<${EmptyState} icon="search" title="No matches">No archived tabs match “${query.trim()}”.<//>`;
  } else {
    body = groups.map(
      (g) => html`<section key=${g.key} class="ip-group" aria-label=${g.label}>
        <h2 class="ip-group-label">${g.label}<span class="ip-group-count">${g.items.length}</span></h2>
        <div class="ip-list">
          ${g.items.map(
            (entry) => html`<${EntryRow}
              key=${entry.id}
              entry=${entry}
              splitSize=${entry.group != null ? (splitSizes.get(entry.group) ?? 0) : 0}
              menuOpen=${menu?.entry.id === entry.id}
              onMenu=${(e, where) => setMenu({ entry: e, ...where })}
            />`,
          )}
        </div>
      </section>`,
    );
  }

  return html`<div class="ip">
    <${PageHeader} icon="archive" title="Archive" subtitle="Tabs you closed or that were archived automatically, kept for 30 days.">
      <${ConfirmButton}
        label="Clear archive"
        icon="trash"
        disabled=${!count}
        title="Clear the archive?"
        message=${`This permanently removes ${count} archived tab${count === 1 ? '' : 's'}. You can't undo this.`}
        confirmLabel="Clear archive"
        onConfirm=${() => send({ type: 'clearArchive' })}
      />
    <//>
    <div class=${classNames('ip-toolbar', stuck && 'is-stuck')} ref=${toolbarRef}>
      <${SearchField} value=${query} onInput=${setQuery} placeholder="Search archive" label="Search archive" />
      ${count > 0 && html`<span class="ip-toolbar-meta" ref=${countRef}>${query.trim() ? `${filtered.length} of ${count}` : `${count} tab${count === 1 ? '' : 's'}`}</span>`}
    </div>
    <div ref=${listRef}>${body}</div>
    ${menu &&
    html`<${Menu}
      items=${menuItems(menu.entry)}
      x=${menu.x}
      y=${menu.y}
      anchor=${menu.anchor}
      placement=${menu.anchor ? 'bottom-end' : 'bottom-start'}
      label="Archived tab"
      onClose=${() => setMenu(null)}
    />`}
  </div>`;
}

mountPage(Archive);
