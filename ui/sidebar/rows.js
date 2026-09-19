// Sidebar rows (PROTOCOL §5, §5.1): tab rows, folder rows, split rows, favorites tiles, the
// "New Tab" row and the Today divider. Rows carry `data-*` descriptors that the drag & drop
// controller and keyboard navigation read (see dnd.js).

import { html, useLayoutEffect, useRef } from '/common/vendor/htm-preact.js';
import { Icon } from '/common/icons.js';
import { AudioBars, Favicon, IconButton, Spinner } from '/common/components.js';
import { classNames, shortcut } from '/common/util.js';
import * as motion from '/common/motion.js';
import { sb } from './controller.js';
import { tabMenuItems, folderMenuItems, splitPaneMenuItems } from './menus.js';
import { fire, internalGlyph, isInternalUrl, json, loadableFavicon, memo } from './lib.js';

// ------------------------------------------------------------------------------------ shared bits

let animateInserts = false;
/** Rows mounted after this is enabled play the insert animation (not on first render / space switch). */
export function setRowInsertAnimation(on) {
  animateInserts = on;
}

/**
 * How the rows that unmount in the **next commit** leave, set by `sidebar.js` from a keyed diff
 * before it renders (its body runs before Preact patches the DOM, so this is always the plan for the
 * unmounts that are about to happen):
 *
 * - `on: false` — no exit ghosts at all: the first render, a space switch (whose own pane ghost
 *   covers the whole list) and a list that is only being rebuilt.
 * - `key` — overrides each row's own key: `sidebar.clearToday` for the Clear Today sweep,
 *   `sidebar.folderExpand` for the children of a folder that just collapsed. `null` means the row
 *   uses its own (`sidebar.tabInsertRemove`, or `sidebar.favorites` for a tile).
 * - `budget` — how many ghosts this commit may leave (rule 7's spirit: a bulk change is not a
 *   movement the eye can follow). 8 for ordinary removals, 12 for Clear Today, 20 for a collapse.
 * - `stagger` — total ms the batch's delays may span (0 = all at once).
 */
let exitMode = { on: false, key: null, budget: 0, stagger: 0 };
let exitCount = 0;

/** @param {{on: boolean, key?: string|null, budget?: number, stagger?: number}} mode */
export function setRowExitMode(mode) {
  exitMode = { on: Boolean(mode?.on), key: mode?.key ?? null, budget: mode?.budget ?? 0, stagger: mode?.stagger ?? 0 };
  exitCount = 0;
}

/** Exit keyframes per key; travel collapses at the `reduced` level (`motion.distance`). */
function exitFrames(key) {
  if (key === 'sidebar.clearToday') {
    return [
      { opacity: 1, translate: 'none' },
      { opacity: 0, translate: `0 ${motion.distance(12)}px` },
    ];
  }
  if (key === 'sidebar.favorites') {
    return [
      { opacity: 1, scale: 1 },
      { opacity: 0, scale: 1 - motion.distance(0.12) },
    ];
  }
  if (key === 'sidebar.folderExpand') return [{ opacity: 1 }, { opacity: 0 }];
  return [
    { opacity: 1, translate: 'none' },
    { opacity: 0, translate: `0 ${-motion.distance(4)}px` },
  ];
}

/**
 * Fades a freshly inserted row in once, and leaves an inert ghost behind when it is removed.
 *
 * Keyed on the mount/unmount, and played through `motion.js` rather than by adding a class and
 * waiting for `animationend`: that event never arrives while the surface renders no frames (a parked
 * sidebar, a hidden overlay), which used to leave `is-new` on the row forever — and it fires for
 * *any* animation on the element, including one a later rule adds. A WAAPI animation with the key as
 * its id needs no cleanup at all, and `motion.finishAll()` can settle it when the sidebar is parked.
 *
 * The exit ghost is a stripped, `inert` clone in the fixed `.motion-ghosts` layer, so the list can
 * shrink (and the followers FLIP up) while the row that is gone fades where it was — and nothing
 * that looks rows up by `data-id`, `[data-nav]` or `[data-row]` can ever find it.
 * @param {{current: Element|null}} ref
 * @param {string} key animation key (`crates/sta-core/src/motion.rs`)
 */
function useRowAnimation(ref, key = 'sidebar.tabInsertRemove') {
  useLayoutEffect(() => {
    if (animateInserts) {
      const frames =
        key === 'sidebar.favorites'
          ? [
              { opacity: 0, scale: 1 - motion.distance(0.14) },
              { opacity: 1, scale: 1 },
            ]
          : [
              { opacity: 0, translate: `0 ${-motion.distance(4)}px` },
              { opacity: 1, translate: 'none' },
            ];
      motion.animate(ref.current, key, frames, {
        duration: motion.duration(key, 180),
        easing: key === 'sidebar.favorites' ? motion.EASE_SPRING : motion.EASE_OUT,
      });
    }
    return () => {
      if (!exitMode.on || exitCount >= exitMode.budget) return;
      const exitKey = exitMode.key ?? key;
      if (!motion.enabled(exitKey)) return;
      const clone = motion.ghost(ref.current);
      if (!clone) return;
      const index = exitCount++;
      const step = exitMode.stagger && exitMode.budget > 1 ? exitMode.stagger / (exitMode.budget - 1) : 0;
      motion.fadeGhost(clone, exitKey, exitFrames(exitKey), {
        duration: motion.duration(exitKey, 180),
        delay: index * step,
      });
    };
  }, []);
}

/** Favicon / spinner / crash glyph for a tab. */
export function TabIcon({ tab, size = 16 }) {
  if (tab.loading) return html`<${Spinner} size=${size - 2} label=${null} class="tab-spinner" />`;
  if (tab.crashed || tab.failed) {
    return html`<span class=${classNames('tab-glyph', tab.crashed ? 'is-crashed' : 'is-failed')}>
      <${Icon} name="warning" size=${size} strokeWidth=${1.75} />
    </span>`;
  }
  if (!tab.favicon && isInternalUrl(tab.url)) {
    return html`<span class=${classNames('tab-glyph is-internal', !tab.loaded && 'is-dim')}>
      <${Icon} name=${internalGlyph(tab.url)} size=${size} strokeWidth=${1.6} />
    </span>`;
  }
  return html`<${Favicon} src=${loadableFavicon(tab.favicon)} host=${tab.host || tab.title} size=${size} dim=${!tab.loaded} lazy=${false} />`;
}

function stateLabel(tab) {
  if (tab.crashed) return 'This tab crashed. Click to reload.';
  if (tab.failed) return 'The page failed to load.';
  return undefined;
}

/** Inline rename input (PROTOCOL §5.1). Focused and selected whenever `seq` changes. */
export function RenameField({ id, initial, seq, isFolder }) {
  const ref = useRef(null);
  const done = useRef(false);
  useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    done.current = false;
    el.value = initial ?? '';
    el.focus({ preventScroll: true });
    el.select();
  }, [seq]);
  const commit = () => {
    if (done.current) return;
    done.current = true;
    sb.commitRename(id, ref.current?.value ?? '', initial, isFolder);
  };
  /** After Enter/Esc the input unmounts: keep keyboard focus on the row (not on <body>). */
  const refocusRow = () => {
    const row = ref.current?.closest('[data-nav]') ?? document.querySelector(`[data-nav][data-id="${id}"]`);
    setTimeout(() => {
      if (row?.isConnected && (document.activeElement === document.body || document.activeElement == null)) row.focus({ preventScroll: true });
    }, 0);
  };
  const stop = (e) => e.stopPropagation();
  return html`<input
    ref=${ref}
    class="row-rename"
    type="text"
    maxlength="200"
    spellcheck=${false}
    autocomplete="off"
    aria-label=${isFolder ? 'Folder name' : 'Tab title'}
    placeholder=${isFolder ? 'Folder name' : 'Page title'}
    onKeyDown=${(e) => {
      e.stopPropagation();
      if (e.key === 'Enter' && !e.isComposing) {
        e.preventDefault();
        refocusRow();
        commit();
      } else if (e.key === 'Escape') {
        e.preventDefault();
        done.current = true;
        refocusRow();
        sb.cancelRename();
      }
    }}
    onBlur=${commit}
    onPointerDown=${stop}
    onClick=${stop}
    onDblClick=${stop}
    onContextMenu=${stop}
  />`;
}

/** Right-click (or the keyboard path, which dispatches a synthetic contextmenu at the row). */
function openRowMenu(e, items) {
  e.preventDefault();
  e.stopPropagation();
  sb.openMenu({ x: e.clientX, y: e.clientY, items });
}

/** Middle-click closes; the mousedown default (autoscroll) is suppressed. */
const preventMiddle = (e) => {
  if (e.button === 1) e.preventDefault();
};

/** How long after a pointer close the followers still refuse to move. */
export const POINTER_CLOSE_MS = 400;

/**
 * A close made **with the pointer inside the list** (a row's ×, a middle-click). The followers must
 * not slide up under the cursor afterwards, or the next row's × arrives under the pointer and
 * repeated clicks hit moving targets (FINAL PLAN rule 7; Chrome's tab strip defers the same reflow).
 * `sidebar.js` reads the flag when it plays the followers' FLIP.
 */
function closeByPointer(id) {
  sb.pointerCloseUntil = Date.now() + POINTER_CLOSE_MS;
  fire({ type: 'closeItem', id });
}

// ------------------------------------------------------------------------------------ tab row

/**
 * @param {object} p
 * @param {any} p.tab TabView
 * @param {'pinned'|'today'} p.section
 * @param {number} p.depth nesting depth (folders)
 * @param {object} p.container Container descriptor
 * @param {number|null} p.next next sibling id in the same container
 * @param {number[]} p.ancestors folder ids above the row
 * @param {number|null} p.renameSeq sidebarPanel seq while this row is being renamed
 * @param {string|null} p.permission pending permission description
 * @param {string} p.title display title (optimistic rename applied)
 * @param {boolean} p.focusable tabindex 0 (keyboard entry point)
 */
function TabRowImpl({ tab, section, depth, container, next, ancestors, renameSeq, permission, title, focusable }) {
  const ref = useRef(null);
  useRowAnimation(ref);
  const pinnedLike = section !== 'today';
  const renaming = renameSeq != null;

  const onClick = (e) => {
    if (!sb.clickAllowed() || e.target.closest('button, input')) return;
    fire({ type: 'activateItem', id: tab.id });
  };
  const onAuxClick = (e) => {
    if (e.button !== 1 || !sb.clickAllowed()) return;
    e.preventDefault();
    if (!pinnedLike || tab.loaded) closeByPointer(tab.id);
  };
  const onDblClick = (e) => {
    if (e.target.closest('button, input')) return;
    sb.startRename(tab.id);
  };
  const reset = (e) => {
    e.stopPropagation();
    fire({ type: 'resetToPinned', id: tab.id });
  };

  const icon = html`<${TabIcon} tab=${tab} />`;
  return html`<div
    ref=${ref}
    class=${classNames('row tab-row', {
      'is-active': tab.active,
      'is-visible': tab.visible && !tab.active,
      'is-unloaded': !tab.loaded,
      'is-renaming': renaming,
      'has-permission': permission,
    })}
    role="treeitem"
    aria-selected=${String(tab.active)}
    aria-level=${depth + 1}
    aria-label=${title}
    tabindex=${focusable ? 0 : -1}
    style=${depth ? { '--depth': depth } : undefined}
    data-nav=""
    data-row=""
    data-id=${tab.id}
    data-kind="tab"
    data-section=${section}
    data-container=${json(container)}
    data-next=${next ?? ''}
    data-ancestors=${json(ancestors)}
    data-depth=${depth}
    onClick=${onClick}
    onMouseDown=${preventMiddle}
    onAuxClick=${onAuxClick}
    onDblClick=${onDblClick}
    onContextMenu=${(e) => openRowMenu(e, tabMenuItems(sb.state, tab, section))}
  >
    ${pinnedLike && tab.navigated
      ? html`<button
          type="button"
          class="row-icon is-reset"
          tabindex="-1"
          title="Back to Pinned URL"
          aria-label="Back to pinned URL"
          onClick=${reset}
        >
          <span class="row-icon-face">${icon}</span>
          <span class="row-icon-hover"><${Icon} name="undo" size=${14} strokeWidth=${1.75} /></span>
        </button>`
      : html`<span class="row-icon" title=${stateLabel(tab)}>${icon}</span>`}
    ${permission && html`<span class="row-permission" title=${`Wants to use your ${permission}`} aria-label=${`Wants to use your ${permission}`} />`}
    ${renaming
      ? html`<${RenameField} id=${tab.id} initial=${title} seq=${renameSeq} isFolder=${false} />`
      : html`<span class="row-title">
          ${pinnedLike && tab.navigated && html`<span class="row-slash" aria-hidden="true">/</span>`}${title}
        </span>`}
    ${tab.agent && html`<span class="row-agent" role="img" title="AI agents can use this tab" aria-label="AI agents can use this tab"><${Icon} name="agent" size=${13} strokeWidth=${1.7} /></span>`}
    ${(tab.audible || tab.muted) &&
    html`<button
      type="button"
      class=${classNames('row-audio', tab.muted && 'is-muted')}
      tabindex="-1"
      title=${tab.muted ? 'Unmute tab' : 'Mute tab'}
      aria-label=${tab.muted ? 'Unmute tab' : 'Mute tab'}
      onClick=${(e) => {
        e.stopPropagation();
        fire({ type: 'toggleMute', id: tab.id });
      }}
    >${tab.muted ? html`<${Icon} name="speaker-muted" size=${12} strokeWidth=${1.75} />` : html`<${AudioBars} size=${12} />`}</button>`}
    ${(!pinnedLike || tab.loaded) &&
    !renaming &&
    html`<${IconButton}
      class="row-close"
      icon=${pinnedLike ? 'minus' : 'close'}
      size="sm"
      iconSize=${12}
      tabindex="-1"
      label=${pinnedLike ? 'Close tab (keeps it pinned)' : 'Archive tab'}
      title=${pinnedLike ? 'Close' : `Close (${shortcut('Ctrl+W')})`}
      onClick=${(e) => {
        e.stopPropagation();
        closeByPointer(tab.id);
      }}
    />`}
  </div>`;
}

export const TabRow = memo(TabRowImpl, 'TabRow');

// ------------------------------------------------------------------------------------ folder row

function FolderRowImpl({ folder, depth, container, next, ancestors, renameSeq, name, spaceId, count, focusable }) {
  const ref = useRef(null);
  useRowAnimation(ref);
  const renaming = renameSeq != null;
  const expanded = !folder.collapsed;
  const firstChild = expanded ? (folder.children[0]?.id ?? '') : '';

  const onClick = (e) => {
    if (!sb.clickAllowed() || e.target.closest('input') || e.detail > 1) return;
    fire({ type: 'toggleFolder', id: folder.id });
  };
  return html`<div
    ref=${ref}
    class=${classNames('row folder-row', { 'is-collapsed': folder.collapsed, 'is-renaming': renaming })}
    role="treeitem"
    aria-expanded=${String(expanded)}
    aria-level=${depth + 1}
    aria-label=${name}
    tabindex=${focusable ? 0 : -1}
    style=${depth ? { '--depth': depth } : undefined}
    data-nav=""
    data-row=""
    data-id=${folder.id}
    data-kind="folder"
    data-section="pinned"
    data-container=${json(container)}
    data-next=${next ?? ''}
    data-expanded=${expanded ? '1' : '0'}
    data-first-child=${firstChild}
    data-ancestors=${json(ancestors)}
    data-depth=${depth}
    onClick=${onClick}
    onDblClick=${(e) => {
      if (!e.target.closest('input')) sb.startRename(folder.id);
    }}
    onContextMenu=${(e) => openRowMenu(e, folderMenuItems(sb.state, folder, depth, spaceId))}
  >
    <span class="row-icon folder-glyph" aria-hidden="true">
      <${Icon} name=${expanded ? 'folder-open' : 'folder'} size=${16} strokeWidth=${1.6} />
    </span>
    ${renaming
      ? html`<${RenameField} id=${folder.id} initial=${name} seq=${renameSeq} isFolder=${true} />`
      : html`<span class="row-title">${name}</span>`}
    ${!renaming && folder.collapsed && count > 0 && html`<span class="folder-count" aria-label=${`${count} tabs`}>${count}</span>`}
    <span class="folder-chevron" aria-hidden="true"><${Icon} name="chevron-right" size=${12} strokeWidth=${2} /></span>
  </div>`;
}

export const FolderRow = memo(FolderRowImpl, 'FolderRow');

// ------------------------------------------------------------------------------------ split row

function SplitRowImpl({ split, container, next, renamePane, permissions, titles, focusable }) {
  const ref = useRef(null);
  useRowAnimation(ref);
  const narrow = split.panes.length >= 3;
  const paneIds = split.panes.map((p) => p.id);

  // `sidebar.splitRow`: panes fade as they are added or removed. Keyed on the pane ids, never on a
  // render. The removed segments are ghosted from the component body, which Preact runs *before* it
  // patches the DOM, so they are still there to clone; the added ones fade in from a layout effect.
  const panes = useRef(null);
  const previous = panes.current;
  panes.current = paneIds;
  const added = [];
  if (previous && previous.join() !== paneIds.join()) {
    for (const id of paneIds) if (!previous.includes(id)) added.push(id);
    if (motion.enabled('sidebar.splitRow')) {
      for (const id of previous) {
        if (paneIds.includes(id)) continue;
        motion.fadeGhost(motion.ghost(ref.current?.querySelector(`[data-pane="${id}"]`)), 'sidebar.splitRow', [{ opacity: 1 }, { opacity: 0 }], {
          duration: motion.duration('sidebar.splitRow', 140),
        });
      }
    }
  }
  useLayoutEffect(() => {
    for (const id of added) {
      motion.animate(ref.current?.querySelector(`[data-pane="${id}"]`), 'sidebar.splitRow', [{ opacity: 0 }, { opacity: 1 }], {
        duration: motion.duration('sidebar.splitRow', 140),
      });
    }
  });

  return html`<div
    ref=${ref}
    class=${classNames('row split-row', { 'is-active': split.active, 'is-narrow': narrow })}
    role="treeitem"
    aria-selected=${String(split.active)}
    aria-label=${`Split view: ${titles.join(', ')}`}
    tabindex=${focusable ? 0 : -1}
    data-nav=""
    data-row=""
    data-id=${split.id}
    data-kind="split"
    data-section="today"
    data-container=${json(container)}
    data-next=${next ?? ''}
    data-ancestors="[]"
    data-depth="0"
    onMouseDown=${preventMiddle}
  >
    <span
      class="split-glider"
      aria-hidden="true"
      style=${{ '--seg-count': split.panes.length, '--seg-index': Math.min(Math.max(split.focused ?? 0, 0), split.panes.length - 1) }}
    />
    ${split.panes.map((pane, i) => {
      const focused = split.focused === i;
      const renaming = renamePane?.id === pane.id;
      return html`<div
        key=${pane.id}
        class=${classNames('split-seg', { 'is-focused': focused, 'is-unloaded': !pane.loaded, 'has-permission': permissions[i] })}
        data-pane=${pane.id}
        title=${titles[i]}
        onClick=${(e) => {
          if (!sb.clickAllowed() || e.target.closest('button, input')) return;
          fire({ type: 'activateItem', id: pane.id });
        }}
        onAuxClick=${(e) => {
          if (e.button !== 1 || !sb.clickAllowed()) return;
          e.preventDefault();
          closeByPointer(pane.id);
        }}
        onDblClick=${(e) => {
          if (!e.target.closest('button, input')) sb.startRename(pane.id);
        }}
        onContextMenu=${(e) => openRowMenu(e, splitPaneMenuItems(sb.state, split, pane))}
      >
        ${i > 0 && html`<span class="split-sep" aria-hidden="true" />`}
        <span class="row-icon"><${TabIcon} tab=${pane} /></span>
        ${permissions[i] && html`<span class="row-permission" title=${`Wants to use your ${permissions[i]}`} />`}
        ${renaming
          ? html`<${RenameField} id=${pane.id} initial=${titles[i]} seq=${renamePane.seq} isFolder=${false} />`
          : html`<span class="row-title">${titles[i]}</span>`}
        ${(pane.audible || pane.muted) &&
        html`<button
          type="button"
          class=${classNames('row-audio', pane.muted && 'is-muted')}
          tabindex="-1"
          title=${pane.muted ? 'Unmute tab' : 'Mute tab'}
          aria-label=${pane.muted ? 'Unmute tab' : 'Mute tab'}
          onClick=${(e) => {
            e.stopPropagation();
            fire({ type: 'toggleMute', id: pane.id });
          }}
        >${pane.muted ? html`<${Icon} name="speaker-muted" size=${12} strokeWidth=${1.75} />` : html`<${AudioBars} size=${12} />`}</button>`}
        ${!renaming &&
        html`<${IconButton}
          class="row-close"
          icon="close"
          size="sm"
          iconSize=${12}
          tabindex="-1"
          label=${`Close ${titles[i]}`}
          title="Close tab"
          onClick=${(e) => {
            e.stopPropagation();
            closeByPointer(pane.id);
          }}
        />`}
      </div>`;
    })}
  </div>`;
}

export const SplitRow = memo(SplitRowImpl, 'SplitRow');

// ------------------------------------------------------------------------------------ favorites

function FavoriteTileImpl({ tab, next, permission, title, focusable, renaming }) {
  const ref = useRef(null);
  useRowAnimation(ref, 'sidebar.favorites');
  return html`<div
    ref=${ref}
    class=${classNames('fav-tile', {
      'is-active': tab.active,
      'is-visible': tab.visible && !tab.active,
      'is-unloaded': !tab.loaded,
      'has-permission': permission,
      'is-renaming': renaming,
    })}
    role="button"
    aria-pressed=${String(tab.active)}
    aria-label=${title}
    title=${`${title}${tab.navigated ? '\nDouble-click to go back to the pinned page' : ''}`}
    tabindex=${focusable ? 0 : -1}
    data-nav=""
    data-row=""
    data-id=${tab.id}
    data-kind="fav"
    data-section="favorites"
    data-container='{"type":"favorites"}'
    data-next=${next ?? ''}
    data-ancestors="[]"
    data-depth="0"
    onClick=${(e) => {
      if (!sb.clickAllowed() || e.detail > 1 || e.target.closest('input, button')) return;
      fire({ type: 'activateItem', id: tab.id });
    }}
    onDblClick=${(e) => {
      if (e.target.closest('input, button')) return;
      fire({ type: 'resetToPinned', id: tab.id });
    }}
    onMouseDown=${preventMiddle}
    onAuxClick=${(e) => {
      if (e.button !== 1 || !sb.clickAllowed()) return;
      e.preventDefault();
      if (tab.loaded) closeByPointer(tab.id);
    }}
    onContextMenu=${(e) => openRowMenu(e, tabMenuItems(sb.state, tab, 'favorites'))}
  >
    <${TabIcon} tab=${tab} size=${20} />
    ${(tab.audible || tab.muted) &&
    html`<button
      type="button"
      class=${classNames('fav-badge fav-audio', tab.muted && 'is-muted')}
      tabindex="-1"
      title=${tab.muted ? 'Unmute tab' : 'Mute tab'}
      aria-label=${tab.muted ? 'Unmute tab' : 'Mute tab'}
      onClick=${(e) => {
        e.stopPropagation();
        fire({ type: 'toggleMute', id: tab.id });
      }}
    >${tab.muted ? html`<${Icon} name="speaker-muted" size=${11} strokeWidth=${1.8} />` : html`<${AudioBars} size=${11} />`}</button>`}
    ${permission && html`<span class="row-permission" title=${`Wants to use your ${permission}`} />`}
    ${tab.navigated && html`<span class="fav-navigated" aria-hidden="true" />`}
  </div>`;
}

export const FavoriteTile = memo(FavoriteTileImpl, 'FavoriteTile');

// ------------------------------------------------------------------------------------ misc rows

export function NewTabRow({ spaceId, firstToday }) {
  return html`<div
    class="row new-tab-row"
    role="button"
    tabindex="-1"
    data-nav=""
    data-zone="today-top"
    data-container=${json({ type: 'today', space: spaceId })}
    data-before=${firstToday ?? ''}
    onClick=${() => fire({ type: 'openCommandBar', mode: 'newTab' })}
    onKeyDown=${(e) => {
      if (e.key === 'Enter' || e.key === ' ') {
        e.preventDefault();
        fire({ type: 'openCommandBar', mode: 'newTab' });
      }
    }}
  >
    <span class="row-icon"><${Icon} name="plus" size=${16} strokeWidth=${1.75} /></span>
    <span class="row-title">New Tab</span>
    <span class="row-hint" aria-hidden="true">${shortcut('Ctrl+T')}</span>
  </div>`;
}

export function TodayDivider({ spaceId, hasToday, firstToday }) {
  return html`<div
    class=${classNames('today-divider', hasToday && 'can-clear')}
    data-zone="divider"
    data-container=${json({ type: 'today', space: spaceId })}
    data-before=${firstToday ?? ''}
    data-pinned=${json({ type: 'pinned', space: spaceId })}
  >
    <span class="today-divider-line" />
    ${hasToday &&
    html`<button
      type="button"
      class="today-clear"
      title=${`Archive Today's tabs (${shortcut('Ctrl+Shift+K')})`}
      onClick=${() => fire({ type: 'clearToday', space: spaceId })}
    ><${Icon} name="arrow-down" size=${11} strokeWidth=${2} /><span>Clear</span></button>`}
  </div>`;
}
