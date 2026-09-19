// sta sidebar (docs/PROTOCOL.md §5, docs/research/arc_spec.md §2): top row, URL pill,
// favorites grid, space title, pinned (folders) and Today lists in one overlay-scrolled region,
// bottom bar with downloads / spaces / new space, panels driven by `state.sidebarPanel`, context
// menus, keyboard navigation, drag & drop (dnd.js) and the right-edge resize handle.
//
// While the sidebar is hidden (`!state.window.sidebarVisible`) the same page floats over the content
// when the pointer rests at the window's left edge (the shell's hover reveal). The shell tells it
// with `sidebar.hover {visible, dismiss, gen}` (docs/PROTOCOL.md §3): contents are hidden before the
// shell hides the overlay — the page reports that blank frame back (`surface.exited {gen}`), which is
// what the shell waits for instead of a fixed delay — and slide in when shown;
// `dismiss` closes menus, popovers and drags. While a menu, popover or drag is open the page asks
// the shell to keep the floating sidebar open (`sidebar.hoverLock`). Panels that hold input (space
// sheets, inline rename, edit pinned page) only render docked: core docks a hidden sidebar for them,
// because the floating one can't take keyboard focus.

import { html, render, useEffect, useLayoutEffect, useRef, useState } from '/common/vendor/htm-preact.js';
import { ackSurfaceExit, invoke, on, startSurface } from '/common/ipc.js';
import { Icon } from '/common/icons.js';
import { IconButton, Menu, ProgressRing, dismissFloatingLayers } from '/common/components.js';
import { NavButtons, UrlPill } from '/common/chrome.js';
import { themeStyle } from '/common/theme.js';
import { activeSpace, classNames } from '/common/util.js';
import { sb } from './controller.js';
import { installDragAndDrop } from './dnd.js';
import { listMenuItems, spaceMenuItems } from './menus.js';
import { DownloadCard, DownloadsPanel, EditPinnedPopover, SpaceSheet } from './panels.js';
import * as motion from '/common/motion.js';
import { FavoriteTile, FolderRow, NewTabRow, RenameField, SplitRow, TabRow, TodayDivider, setRowExitMode, setRowInsertAnimation } from './rows.js';
import { OverlayScrollbar } from './scrollbar.js';
import {
  SIDEBAR_DEFAULT,
  SIDEBAR_MAX,
  SIDEBAR_MIN,
  describePermissionKinds,
  favoriteColumns,
  fire,
  folderTabCount,
  json,
  locateItem,
  useExitGhost,
} from './lib.js';

// ------------------------------------------------------------------------------------ hooks

function useViewportWidth() {
  const [w, setW] = useState(window.innerWidth);
  useEffect(() => {
    const onResize = () => setW(window.innerWidth);
    window.addEventListener('resize', onResize);
    return () => window.removeEventListener('resize', onResize);
  }, []);
  return w;
}

/**
 * The width this sidebar is laid out at. While it floats, that is the width its card *will* have,
 * not the viewport it has right now: the shell slides the card in from outside the window, so the
 * window clips it and the viewport grows from a sliver to the full width over the whole slide
 * (`crates/sta/src/motion.rs`). Laying out at the final width — and pinning the layout to the right
 * edge in CSS — is what makes the sidebar *travel* in rather than be stretched into place; laying
 * out at the viewport would reflow every row (and the favourites grid) on every frame of it.
 */
function useLayoutWidth(state) {
  const viewport = useViewportWidth();
  const floating = !state.window.sidebarVisible;
  const target = state.window.sidebarWidth;
  return floating && Number.isFinite(target) && target > 0 ? target : viewport;
}

// ------------------------------------------------------------------------------------ keyed diffs
//
// Every sidebar animation starts from one of these signatures changing — never from a render, a
// visibility event or `animationend` (FINAL PLAN rule 4). They are read from the UiState, which is
// cheap, so a 30 Hz push that changes nothing measures nothing either.

/** Row ids in list order (a collapsed folder hides its children, exactly as the DOM does). */
function listOrder(space) {
  const out = [];
  const walk = (nodes) => {
    for (const node of nodes) {
      out.push(node.id);
      if (node.kind === 'folder' && !node.collapsed) walk(node.children);
    }
  };
  walk(space.pinned);
  for (const node of space.today) out.push(node.id);
  return out.join(',');
}

/** Collapsed folder ids, in tree order: what tells a folder toggle from an insert or a move. */
function collapsedFolders(space) {
  const out = [];
  const walk = (nodes) => {
    for (const node of nodes) {
      if (node.kind !== 'folder') continue;
      if (node.collapsed) out.push(node.id);
      walk(node.children);
    }
  };
  walk(space.pinned);
  return out.join(',');
}

/** Two comma-joined id lists hold the same ids (so the change is a reorder, not an insert). */
function sameMembers(a, b) {
  const x = a.split(',');
  const y = b.split(',');
  return x.length === y.length && x.slice().sort().join() === y.slice().sort().join();
}

/**
 * How many ids one comma-joined list adds or removes against another — what makes a change *bulk*
 * (`motion.FLIP_BULK_LIMIT`). Counting the rows that moved instead would let a list that emptied or
 * refilled through (Clear Today and its Undo change dozens of ids and displace only the survivors).
 */
function changedCount(a, b) {
  const before = new Set(a.split(',').filter(Boolean));
  const now = new Set(b.split(',').filter(Boolean));
  let n = 0;
  for (const id of now) if (!before.has(id)) n++;
  for (const id of before) if (!now.has(id)) n++;
  return n;
}

/**
 * Everything **above or around** the lists whose height moving would move every row at once: the
 * favorites grid, the favorite rename row, the "Drop here to pin" placeholder and the download card.
 * FLIP is suspended while any of it changes, because a whole-list shift is not a rearrangement the
 * eye can follow (FINAL PLAN rule 7, critique issue 11).
 */
function layoutSignature(state, space, favRename, renameId, hasCard) {
  return [
    space?.id ?? '',
    state.favorites.length,
    favRename ?? '',
    renameId ?? '',
    space && space.pinned.length === 0 ? 'e' : '',
    hasCard ? 'd' : '',
  ].join('|');
}

/** The space the old pane slides out towards, set by the render that switches spaces. */
let spaceExitDir = 0;

/**
 * Today rows disappearing in one push for the sweep to read as Clear Today rather than as a handful
 * of closes. Core keeps the visible and audible rows, so the list itself rarely reaches zero, and a
 * ≤ 30 Hz push never coalesces three separate Ctrl+W presses.
 */
const CLEAR_TODAY_ROWS = 3;

// ------------------------------------------------------------------------------------ favorites

/**
 * Columns actually used by the grid: tiles stretch to fill the row when there are fewer favorites
 * than fit (one favorite = one full-width tile, as in Arc); otherwise the width-based count.
 */
const gridColumns = (columns, count) => Math.max(1, Math.min(columns, count));

function FavoritesGrid({ state, columns, renameSeq, renameId, permissions, entry }) {
  const favorites = state.favorites;
  const renaming = renameId != null ? favorites.find((t) => t.id === renameId) : null;
  return html`<div class="favorites-wrap">
    ${favorites.length > 0
      ? html`<div
          class="favorites"
          role="list"
          aria-label="Favorites"
          style=${{ '--fav-cols': gridColumns(columns, favorites.length) }}
          data-zone="fav-grid"
        >
          ${favorites.map(
            (tab, i) => html`<${FavoriteTile}
              key=${tab.id}
              tab=${tab}
              next=${favorites[i + 1]?.id ?? null}
              permission=${permissions.get(tab.id) ?? null}
              title=${sb.titleOf(tab.id, tab.title)}
              renaming=${renaming?.id === tab.id}
              focusable=${entry === tab.id}
            />`,
          )}
        </div>`
      : html`<div class="favorites-empty" data-zone="fav-grid" aria-hidden="true">
          <${Icon} name="star" size=${14} strokeWidth=${1.75} /><span>Drop here to add a favorite</span>
        </div>`}
    ${renaming &&
    html`<div class="fav-rename-row">
      <span class="fav-rename-label">Rename favorite</span>
      <${RenameField} id=${renaming.id} initial=${sb.titleOf(renaming.id, renaming.title)} seq=${renameSeq} isFolder=${false} />
    </div>`}
  </div>`;
}

// ------------------------------------------------------------------------------------ lists

function pinnedRows(nodes, ctx, container, depth, ancestors, out) {
  nodes.forEach((node, i) => {
    const next = nodes[i + 1]?.id ?? null;
    if (node.kind === 'folder') {
      const name = sb.titleOf(node.id, node.name);
      out.push(html`<${FolderRow}
        key=${node.id}
        folder=${node}
        depth=${depth}
        container=${container}
        next=${next}
        ancestors=${ancestors}
        renameSeq=${ctx.renameId === node.id ? ctx.renameSeq : null}
        name=${name}
        spaceId=${ctx.space.id}
        count=${node.collapsed ? folderTabCount(node) : 0}
        focusable=${ctx.entry === node.id}
      />`);
      if (!node.collapsed) pinnedRows(node.children, ctx, { type: 'folder', id: node.id }, depth + 1, [...ancestors, node.id], out);
    } else if (node.kind === 'tab') {
      out.push(html`<${TabRow}
        key=${node.id}
        tab=${node}
        section="pinned"
        depth=${depth}
        container=${container}
        next=${next}
        ancestors=${ancestors}
        renameSeq=${ctx.renameId === node.id ? ctx.renameSeq : null}
        permission=${ctx.permissions.get(node.id) ?? null}
        title=${sb.titleOf(node.id, node.title)}
        focusable=${ctx.entry === node.id}
      />`);
    }
  });
  return out;
}

function todayRows(nodes, ctx) {
  const container = { type: 'today', space: ctx.space.id };
  return nodes.map((node, i) => {
    const next = nodes[i + 1]?.id ?? null;
    if (node.kind === 'split') {
      const renamePane = node.panes.some((p) => p.id === ctx.renameId) ? { id: ctx.renameId, seq: ctx.renameSeq } : null;
      return html`<${SplitRow}
        key=${node.id}
        split=${node}
        container=${container}
        next=${next}
        renamePane=${renamePane}
        permissions=${node.panes.map((p) => ctx.permissions.get(p.id) ?? null)}
        titles=${node.panes.map((p) => sb.titleOf(p.id, p.title))}
        focusable=${ctx.entry === node.id}
      />`;
    }
    return html`<${TabRow}
      key=${node.id}
      tab=${node}
      section="today"
      depth=${0}
      container=${container}
      next=${next}
      ancestors=${[]}
      renameSeq=${ctx.renameId === node.id ? ctx.renameSeq : null}
      permission=${ctx.permissions.get(node.id) ?? null}
      title=${sb.titleOf(node.id, node.title)}
      focusable=${ctx.entry === node.id}
    />`;
  });
}

/** Scroll positions per space, restored when switching back. */
const scrollMemory = new Map();

function SpacePane({ state, space, direction, renameId, renameSeq, permissions, entry }) {
  const paneRef = useRef(null);
  const scroller = useRef(null);
  const content = useRef(null);
  const moreButton = useRef(null);
  // The slide direction is fixed at mount (the pane is keyed by space id).
  const [enterDir] = useState(direction);

  // `sidebar.spaceSwitch`: the pane that is leaving slides out the way the new one came from, as an
  // inert ghost — the real pane is gone by then, so nothing focusable or findable lingers. Cloning
  // resets scroll offsets, so the ghost is put back where the user was looking.
  useExitGhost(
    'sidebar.spaceSwitch',
    () => paneRef.current,
    () => [
      { opacity: 1, translate: 'none' },
      { opacity: 0, translate: `${-spaceExitDir * motion.distance(26)}px 0` },
    ],
    {
      fallback: 220,
      // A pane ghost is a deep clone of the whole list: switching faster than the fade can finish
      // must not stack them in the layer, so there is only ever one.
      slot: 'sidebar.spaceSwitch',
      prepare: (clone) => {
        const to = clone.querySelector('.space-scroller');
        if (to && scroller.current) to.scrollTop = scroller.current.scrollTop;
      },
    },
  );

  useLayoutEffect(() => {
    const sc = scroller.current;
    if (sc && scrollMemory.has(space.id)) sc.scrollTop = scrollMemory.get(space.id);
    return () => {
      if (sc) scrollMemory.set(space.id, sc.scrollTop);
    };
  }, []);

  const ctx = { space, renameId, renameSeq, permissions, entry };
  const pinned = pinnedRows(space.pinned, ctx, { type: 'pinned', space: space.id }, 0, [], []);
  const firstToday = space.today[0]?.id ?? null;

  const onListContextMenu = (e) => {
    if (e.target.closest('[data-row], button, input')) return;
    e.preventDefault();
    sb.openMenu({ x: e.clientX, y: e.clientY, items: listMenuItems(sb.state, space) });
  };

  return html`<section
    ref=${paneRef}
    class=${classNames('space-pane', enterDir && 'is-entering')}
    style=${enterDir ? { '--enter-dir': enterDir } : undefined}
    aria-label=${`${space.name} space`}
  >
    <div
      class="space-title-row"
      onContextMenu=${(e) => {
        e.preventDefault();
        sb.openMenu({ x: e.clientX, y: e.clientY, items: spaceMenuItems(sb.state, space) });
      }}
      onDblClick=${(e) => {
        if (!e.target.closest('button')) fire({ type: 'openSidebarPanel', panel: { type: 'editSpace', id: space.id } });
      }}
    >
      <span class="space-title-icon emoji" aria-hidden="true">${space.icon}</span>
      <span class="space-title-name">${space.name}</span>
      <${IconButton}
        class="space-title-more"
        icon="more"
        size="sm"
        iconSize=${14}
        label=${`Edit space ${space.name}`}
        title="Edit space (right-click for more)"
        buttonRef=${moreButton}
        onClick=${() => fire({ type: 'openSidebarPanel', panel: { type: 'editSpace', id: space.id } })}
      />
    </div>
    <div class="space-scroll-wrap">
      <div class="space-scroller" ref=${scroller} onContextMenu=${onListContextMenu}>
        <div class="space-scroll-content" ref=${content} role="tree" aria-label=${`${space.name} tabs`}>
          <div class="pinned-list" role="group" aria-label="Pinned">
            ${pinned}
            ${space.pinned.length === 0
              ? html`<div class="pinned-empty" data-zone="pinned-empty" data-container=${json({ type: 'pinned', space: space.id })}>
                  <${Icon} name="pin" size=${14} strokeWidth=${1.75} /><span>Drop here to pin</span>
                </div>`
              : html`<div class="list-end-zone" data-zone="pinned-end" data-container=${json({ type: 'pinned', space: space.id })} />`}
          </div>
          <${TodayDivider} spaceId=${space.id} hasToday=${space.today.length > 0} firstToday=${firstToday} />
          <${NewTabRow} spaceId=${space.id} firstToday=${firstToday} />
          <div class="today-list" role="group" aria-label="Today">${todayRows(space.today, ctx)}</div>
          <div class="today-end-zone" data-zone="today-end" data-container=${json({ type: 'today', space: space.id })} />
        </div>
      </div>
      <${OverlayScrollbar} scrollerRef=${scroller} contentRef=${content} />
    </div>
  </section>`;
}

// ------------------------------------------------------------------------------------ bottom bar

function aggregateProgress(downloads) {
  const active = downloads.filter((d) => d.state === 'inProgress');
  if (!active.length) return undefined;
  if (active.some((d) => !d.totalBytes)) return null;
  const total = active.reduce((s, d) => s + d.totalBytes, 0);
  const received = active.reduce((s, d) => s + d.receivedBytes, 0);
  return total ? received / total : null;
}

/** How long the check mark stays after a download finished (`sidebar.downloads`). */
const DOWNLOAD_DONE_MS = 1600;

/** The check the progress ring turns into: it pops in, so the hand-over reads as one movement. */
function DownloadCheck() {
  const ref = useRef(null);
  useLayoutEffect(() => {
    motion.animate(
      ref.current,
      'sidebar.downloads',
      [
        { opacity: 0, scale: 1 - motion.distance(0.35), rotate: `${-motion.distance(30)}deg` },
        { opacity: 1, scale: 1, rotate: 'none' },
      ],
      { duration: motion.duration('sidebar.downloads', 180), easing: motion.EASE_SPRING },
    );
  }, []);
  return html`<span ref=${ref} class="bb-done" aria-hidden="true"><${Icon} name="check" size=${16} strokeWidth=${2} /></span>`;
}

function BottomBar({ state, downloadsButton, downloadsOpen, onDownloads }) {
  const progress = aggregateProgress(state.downloads);
  const strip = useRef(null);

  // Keyed on the newest finished download's id, never on a render: a 30 Hz push that only moves bytes
  // must not re-pop the check.
  const newestDone = state.downloads.find((d) => d.state === 'complete')?.id ?? null;
  const seenDone = useRef(newestDone);
  const [justDone, setJustDone] = useState(false);
  useEffect(() => {
    if (seenDone.current === newestDone) return undefined;
    seenDone.current = newestDone;
    if (newestDone === null) return undefined;
    setJustDone(true);
    const t = setTimeout(() => setJustDone(false), DOWNLOAD_DONE_MS);
    return () => clearTimeout(t);
  }, [newestDone]);

  useEffect(() => {
    // Keep the active space icon visible when the strip scrolls.
    strip.current?.querySelector('.space-btn.is-active')?.scrollIntoView({ block: 'nearest', inline: 'nearest' });
  }, [state.activeSpace]);

  // Soft edge fades on an overflowing strip, only on the sides that can scroll.
  useLayoutEffect(() => {
    const el = strip.current;
    if (!el) return undefined;
    const update = () => {
      const max = el.scrollWidth - el.clientWidth;
      el.classList.toggle('fade-left', max > 1 && el.scrollLeft > 1);
      el.classList.toggle('fade-right', max > 1 && el.scrollLeft < max - 1);
    };
    update();
    el.addEventListener('scroll', update, { passive: true });
    window.addEventListener('resize', update);
    return () => {
      el.removeEventListener('scroll', update);
      window.removeEventListener('resize', update);
    };
  }, [state.spaces.length]);

  return html`<footer class="bottom-bar">
    <button
      ref=${downloadsButton}
      type="button"
      class=${classNames('icon-btn bb-downloads', progress !== undefined && 'is-busy', justDone && 'is-done')}
      aria-label="Downloads"
      title="Downloads (Ctrl+J)"
      aria-expanded=${String(downloadsOpen)}
      onClick=${onDownloads}
    >
      ${progress !== undefined
        ? html`<${ProgressRing} value=${progress} size=${20} stroke=${2} label="Downloading">
            <${Icon} name="arrow-down" size=${11} strokeWidth=${2.25} />
          <//>`
        : justDone
          ? html`<${DownloadCheck} />`
          : html`<${Icon} name="download" size=${16} strokeWidth=${1.6} />`}
    </button>
    <div
      class="space-strip"
      ref=${strip}
      role="tablist"
      aria-label="Spaces"
      onWheel=${(e) => {
        const el = strip.current;
        if (el && el.scrollWidth > el.clientWidth && Math.abs(e.deltaY) > Math.abs(e.deltaX)) {
          el.scrollLeft += e.deltaY;
          e.preventDefault();
        }
      }}
    >
      ${state.spaces.map(
        (space, i) => html`<button
          key=${space.id}
          type="button"
          role="tab"
          class=${classNames('space-btn emoji', space.id === state.activeSpace && 'is-active')}
          aria-selected=${String(space.id === state.activeSpace)}
          aria-label=${space.name}
          title=${`${space.name}${i < 9 ? ` (Alt+${i + 1})` : ''}`}
          data-space-id=${space.id}
          style=${{ '--space-accent': space.colors.accent }}
          onClick=${() => sb.clickAllowed() && space.id !== state.activeSpace && fire({ type: 'switchSpace', id: space.id })}
          onContextMenu=${(e) => {
            e.preventDefault();
            fire({ type: 'openSidebarPanel', panel: { type: 'editSpace', id: space.id } });
          }}
        >${space.icon}</button>`,
      )}
    </div>
    <${IconButton}
      class="bb-new-space"
      icon="plus"
      label="New space"
      title="New Space"
      onClick=${() => fire({ type: 'openSidebarPanel', panel: { type: 'newSpace' } })}
    />
  </footer>`;
}

// ------------------------------------------------------------------------------------ resize handle

function ResizeHandle() {
  const onPointerDown = (e) => {
    if (e.button !== 0) return;
    e.preventDefault();
    const el = e.currentTarget;
    el.setPointerCapture(e.pointerId);
    const startX = e.screenX;
    const startW = window.innerWidth;
    let last = startW;
    let pending = null;
    let raf = 0;
    document.documentElement.classList.add('is-resizing');
    const send = () => {
      raf = 0;
      if (pending != null) invoke('sidebar.setWidth', { width: pending }).catch(() => {});
      pending = null;
    };
    const move = (ev) => {
      const w = Math.round(Math.min(SIDEBAR_MAX, Math.max(SIDEBAR_MIN, startW + ev.screenX - startX)));
      if (w === last) return;
      last = w;
      pending = w;
      if (!raf) raf = requestAnimationFrame(send);
    };
    const up = () => {
      el.removeEventListener('pointermove', move);
      el.removeEventListener('pointerup', up);
      el.removeEventListener('pointercancel', up);
      cancelAnimationFrame(raf);
      document.documentElement.classList.remove('is-resizing');
      if (last !== startW) {
        invoke('sidebar.setWidth', { width: last }).catch(() => {});
        fire({ type: 'setSidebarWidth', width: last });
      }
    };
    el.addEventListener('pointermove', move);
    el.addEventListener('pointerup', up);
    el.addEventListener('pointercancel', up);
  };
  return html`<div
    class="resize-handle"
    role="separator"
    aria-orientation="vertical"
    aria-label="Resize sidebar"
    title="Drag to resize · double-click to reset"
    onPointerDown=${onPointerDown}
    onDblClick=${() => {
      invoke('sidebar.setWidth', { width: SIDEBAR_DEFAULT }).catch(() => {});
      fire({ type: 'setSidebarWidth', width: SIDEBAR_DEFAULT });
    }}
  />`;
}

// ------------------------------------------------------------------------------------ keyboard navigation

const isShown = (el) => el.offsetParent !== null || el.getClientRects().length > 0;

function focusNav(el) {
  if (!el) return;
  el.focus({ preventScroll: true });
  el.scrollIntoView({ block: 'nearest', inline: 'nearest' });
}

function onNavKeyDown(e, root, columns) {
  if (e.defaultPrevented || e.altKey || e.ctrlKey || e.metaKey) return;
  const nav = e.target.closest?.('[data-nav]');
  if (!nav || e.target !== nav) return;
  const items = [...root.querySelectorAll('.sidebar-main [data-nav]')].filter(isShown);
  const i = items.indexOf(nav);
  const kind = nav.dataset.kind;
  const id = nav.dataset.id ? Number(nav.dataset.id) : null;
  const favs = items.filter((el) => el.dataset.kind === 'fav');
  const inGrid = kind === 'fav';
  const handled = () => {
    e.preventDefault();
    e.stopPropagation();
  };

  switch (e.key) {
    case 'ArrowDown':
      handled();
      if (inGrid) {
        const fi = favs.indexOf(nav);
        focusNav(fi + columns < favs.length ? favs[fi + columns] : items[items.indexOf(favs.at(-1)) + 1] ?? nav);
      } else focusNav(items[i + 1] ?? nav);
      return;
    case 'ArrowUp':
      handled();
      if (inGrid) {
        const fi = favs.indexOf(nav);
        if (fi - columns >= 0) focusNav(favs[fi - columns]);
      } else {
        const prev = items[i - 1];
        if (prev?.dataset.kind === 'fav') focusNav(favs[Math.floor((favs.length - 1) / columns) * columns] ?? prev);
        else focusNav(prev ?? nav);
      }
      return;
    case 'ArrowRight':
      if (inGrid) {
        handled();
        focusNav(favs[favs.indexOf(nav) + 1] ?? nav);
      } else if (kind === 'folder' && nav.dataset.expanded !== '1') {
        handled();
        fire({ type: 'toggleFolder', id });
      } else if (kind === 'folder') {
        handled();
        focusNav(items[i + 1]);
      }
      return;
    case 'ArrowLeft':
      if (inGrid) {
        handled();
        focusNav(favs[favs.indexOf(nav) - 1] ?? nav);
      } else if (kind === 'folder' && nav.dataset.expanded === '1') {
        handled();
        fire({ type: 'toggleFolder', id });
      } else {
        const parent = JSON.parse(nav.dataset.ancestors || '[]').at(-1);
        if (parent != null) {
          handled();
          focusNav(root.querySelector(`[data-kind="folder"][data-id="${parent}"]`));
        }
      }
      return;
    case 'Home':
      handled();
      focusNav(items[0]);
      return;
    case 'End':
      handled();
      focusNav(items.at(-1));
      return;
    case 'Enter':
    case ' ':
      handled();
      if (kind === 'split') (nav.querySelector('.split-seg.is-focused') ?? nav.querySelector('.split-seg'))?.click();
      else nav.click();
      return;
    case 'F2':
      if (id == null) return;
      handled();
      if (kind === 'split') {
        const pane = nav.querySelector('.split-seg.is-focused') ?? nav.querySelector('.split-seg');
        if (pane) sb.startRename(Number(pane.dataset.pane));
      } else sb.startRename(id);
      return;
    case 'Delete':
      if (id == null || kind === 'folder') return;
      handled();
      if (kind === 'split') {
        const pane = nav.querySelector('.split-seg.is-focused');
        fire({ type: 'closeItem', id: pane ? Number(pane.dataset.pane) : id });
      } else {
        focusNav(items[i + 1] ?? items[i - 1]);
        fire({ type: 'closeItem', id });
      }
      return;
    case 'ContextMenu':
    case 'F10':
      if (e.key === 'F10' && !e.shiftKey) return;
      handled();
      {
        const target = kind === 'split' ? (nav.querySelector('.split-seg.is-focused') ?? nav) : nav;
        const r = target.getBoundingClientRect();
        target.dispatchEvent(new MouseEvent('contextmenu', { bubbles: true, cancelable: true, clientX: r.left + 24, clientY: r.bottom - 4 }));
      }
      return;
    default:
  }
}

// ------------------------------------------------------------------------------------ app

/**
 * Replace submenus by items that re-open the menu in place with the submenu's entries. The
 * drilled-down menu starts with a "‹ Parent" item that goes back to the original menu.
 */
function drillDown(items, spec) {
  return items.map((item) =>
    item.submenu
      ? {
          ...item,
          submenu: undefined,
          hint: '›',
          onSelect: () =>
            sb.openMenu({
              ...spec,
              items: [
                { key: 'drill-back', label: item.label, icon: 'chevron-left', onSelect: () => sb.openMenu(spec) },
                { type: 'separator' },
                ...item.submenu,
              ],
            }),
        }
      : item,
  );
}

/** Sidebar panels that float with the hidden sidebar (core's `SidebarPanel::is_transient`). */
const TRANSIENT_PANELS = new Set(['downloads', 'appMenu']);

/**
 * Reports whether an HTML menu, popover or drag is open in this page (`sidebar.hoverLock`), so the
 * shell keeps the floating sidebar open meanwhile (e.g. the pointer moving over the page while a
 * context menu waits for a choice). Every Menu and Popover renders into a `body > .portal-host`;
 * drags mark `<html>` with `is-dragging` (dnd.js) or `is-resizing` (resize handle). Only changes
 * are sent; the shell drops the lock itself when this page reloads.
 */
function installHoverLock() {
  let locked = false;
  const update = () => {
    const html = document.documentElement.classList;
    const next = Boolean(document.querySelector('body > .portal-host')) || html.contains('is-dragging') || html.contains('is-resizing');
    if (next === locked) return;
    locked = next;
    invoke('sidebar.hoverLock', { locked }).catch(() => {});
  };
  const observer = new MutationObserver(update);
  observer.observe(document.body, { childList: true });
  observer.observe(document.documentElement, { attributes: true, attributeFilter: ['class'] });
  return () => observer.disconnect();
}

/**
 * `hidden` → `entering` (slide-in) → `shown`. Whether the slide actually moves is decided in CSS by
 * `sidebar.hoverReveal`'s own duration token, so this stays a pure presence state machine.
 */
const revealed = (presence) => (presence === 'hidden' ? 'entering' : presence);

/**
 * Presence of the sidebar contents: hidden while the floating sidebar is hidden or about to be
 * parked, animated in when the shell shows it (or docks it). `onDismiss` runs for `dismiss`.
 */
function useHoverPresence(docked, onDismiss) {
  const [presence, setPresence] = useState(() => (docked ? 'shown' : 'hidden'));
  const dismiss = useRef(onDismiss);
  dismiss.current = onDismiss;
  // A layout effect subscribes during the first render, before `ui.ready` is sent: the shell only
  // emits `sidebar.hover` after it (e.g. right away to a page reloaded while it floats).
  useLayoutEffect(
    () =>
      on('sidebar.hover', (payload) => {
        if (payload?.dismiss) dismiss.current();
        setPresence((p) => (payload?.visible ? revealed(p) : 'hidden'));
        // The shell is waiting for the frame that shows nothing before it hides (or parks) this
        // surface: `gen` is that exit. The contents fade out in CSS (`.is-hover-hidden`), so the
        // acknowledgement is the fade's own time plus a frame (PROTOCOL §14).
        if (!payload?.visible && typeof payload?.gen === 'number') {
          ackSurfaceExit(payload.gen, motion.settle('sidebar.hoverReveal'));
        }
      }),
    [],
  );
  // Docked contents are never left hidden (e.g. a park that was cancelled).
  useEffect(() => {
    if (docked) setPresence(revealed);
  }, [docked]);
  useEffect(() => {
    if (presence !== 'entering') return undefined;
    const t = setTimeout(() => setPresence((p) => (p === 'entering' ? 'shown' : p)), 220);
    return () => clearTimeout(t);
  }, [presence]);
  return presence;
}

/**
 * Leaves an exit ghost behind for a panel that a shared component renders into a portal
 * (`sidebar.panels`): the app menu, the downloads popover and "Edit Pinned Page". It **wraps** the
 * panel rather than standing beside it, because Preact runs a component's own hook cleanups before
 * it recurses into its children (`options.unmount` → `U()` in the vendored bundle) — so a parent
 * always still sees its subtree in the document, whatever order siblings are unmounted in.
 */
function PanelGhost({ selector, children }) {
  useExitGhost('sidebar.panels', () => document.querySelector(selector), () => [
    { opacity: 1, scale: 1 },
    { opacity: 0, scale: 1 - motion.distance(0.02) },
  ]);
  return children;
}

function Sidebar({ state }) {
  sb.state = state;
  sb.settlePendingTitles(state.revision);

  const rootRef = useRef(null);
  const dndRef = useRef(null);
  /** Last render's keyed signatures (`listOrder` & co.), and the FLIP captures they asked for. */
  const signatures = useRef(null);
  const flips = useRef([]);
  const menuButton = useRef(null);
  const downloadsButton = useRef(null);
  const [mounted, setMounted] = useState(false);
  const [menu, setMenu] = useState(null);
  const [dismissedSeq, setDismissedSeq] = useState(null);
  const [preview, setPreview] = useState(null);
  const floating = !state.window.sidebarVisible;
  const width = useLayoutWidth(state);
  const columns = favoriteColumns(width);
  const presence = useHoverPresence(!floating, () => {
    setMenu(null);
    dismissFloatingLayers(); // other menus (URL pill, download rows) close too
    dndRef.current?.cancel();
  });
  // A parked sidebar renders no frames while `is_drawn` stays 1, so `visibilityState` cannot be
  // trusted: the page's own presence is what gates motion. Rows inserted while it is hidden
  // (Ctrl+T five times) must not all animate at once on the next reveal.
  motion.usePresence(presence !== 'hidden');

  const space = activeSpace(state);
  // Panels that hold input render only in a docked sidebar: typing into the floating one would go
  // to the page (core docks the sidebar for them; this guards a stale combination).
  const panelView =
    state.sidebarPanel && state.sidebarPanel.seq !== dismissedSeq && (!floating || TRANSIENT_PANELS.has(state.sidebarPanel.panel.type))
      ? state.sidebarPanel
      : null;
  const panel = panelView?.panel ?? null;
  const renameId = panel?.type === 'renameItem' ? panel.id : null;
  const renameSeq = renameId != null ? panelView.seq : null;

  // Space switch direction (slide in from the side of the target space).
  const prevSpace = useRef({ id: state.activeSpace, index: state.spaces.findIndex((s) => s.id === state.activeSpace) });
  let direction = 0;
  const index = state.spaces.findIndex((s) => s.id === state.activeSpace);
  if (prevSpace.current.id !== state.activeSpace) {
    direction = index >= prevSpace.current.index ? 1 : -1;
    setRowInsertAnimation(false);
  }
  useEffect(() => {
    prevSpace.current = { id: state.activeSpace, index };
    const t = setTimeout(() => setRowInsertAnimation(true), 50);
    return () => clearTimeout(t);
  }, [state.activeSpace]);

  // Register controller hooks once.
  useLayoutEffect(() => {
    sb.openMenu = (spec) => {
      if (!spec?.items?.length) {
        setMenu(null);
        return;
      }
      // A sidebar is too narrow for side-by-side submenus: drill down in place instead.
      const items = window.innerWidth < 420 ? drillDown(spec.items, spec) : spec.items;
      setMenu({ ...spec, items, key: Date.now() });
    };
    sb.dismissPanelLocally = () => {
      const seq = sb.state?.sidebarPanel?.seq;
      if (seq != null) setDismissedSeq(seq);
    };
    const dnd = installDragAndDrop(rootRef.current);
    dndRef.current = dnd;
    const stopHoverLock = installHoverLock();
    setMounted(true);
    const t = setTimeout(() => setRowInsertAnimation(true), 300);
    return () => {
      clearTimeout(t);
      dnd.cancel();
      stopHoverLock();
    };
  }, []);

  // A press in the floating sidebar closes the command bar, as focusing the docked sidebar does (the
  // floating one never takes focus, so the shell's focus rule can't see it). Sent by the page, so it
  // is ordered before the click's own command (e.g. "New Tab" opening the bar again).
  useEffect(() => {
    const onPointerDown = () => {
      const s = sb.state;
      // With the bar's own `seq`: a close for a bar that is already gone (Esc then Ctrl+T) must
      // never close the new one (docs/PROTOCOL.md §8, `closeCommandBar`).
      if (s && !s.window.sidebarVisible && s.commandBar) fire({ type: 'closeCommandBar', seq: s.commandBar.seq });
    };
    document.addEventListener('pointerdown', onPointerDown, true);
    return () => document.removeEventListener('pointerdown', onPointerDown, true);
  }, []);

  // Keep the active row visible (Ctrl+Tab, Ctrl+1..9, activation from the command bar).
  useEffect(() => {
    if (state.activeItem == null) return;
    const el = rootRef.current?.querySelector(`.space-scroller [data-id="${state.activeItem}"]`);
    el?.scrollIntoView({ block: 'nearest', behavior: motion.scrollBehavior() });
  }, [state.activeItem, state.activeSpace]);

  // Scroll a row that is being renamed into view. A row hidden inside collapsed folders (e.g. "New
  // Subfolder" on a collapsed folder) is revealed first by expanding them, otherwise the rename
  // field would never appear.
  useEffect(() => {
    if (renameId == null) return;
    const loc = locateItem(state, renameId);
    for (const folderId of loc?.ancestors ?? []) {
      if (locateItem(state, folderId)?.node.collapsed) fire({ type: 'toggleFolder', id: folderId });
    }
    rootRef.current?.querySelector(`[data-id="${renameId}"], [data-pane="${renameId}"]`)?.scrollIntoView({ block: 'nearest' });
  }, [renameSeq]);

  const closePanel = () => {
    if (!panelView) return;
    setDismissedSeq(panelView.seq);
    fire({ type: 'closeSidebarPanel' });
  };

  // Closing popovers when the sidebar loses focus (a click into the page).
  useEffect(() => {
    if (!panel || (panel.type !== 'downloads' && panel.type !== 'appMenu')) return undefined;
    const onBlur = () => closePanel();
    window.addEventListener('blur', onBlur);
    return () => window.removeEventListener('blur', onBlur);
  }, [panelView?.seq]);

  // Mouse back/forward buttons switch spaces; horizontal swipes too (arc_spec §2.10).
  const swipe = useRef({ dx: 0, at: 0, lockUntil: 0 });
  const onMouseUp = (e) => {
    if (e.button === 3 || e.button === 4) {
      e.preventDefault();
      fire({ type: 'switchSpaceAdjacent', delta: e.button === 3 ? -1 : 1 });
    }
  };
  const onWheel = (e) => {
    if (Math.abs(e.deltaX) <= Math.abs(e.deltaY) || e.target.closest('.space-strip, .menu, .popover, .space-sheet')) return;
    const s = swipe.current;
    const now = performance.now();
    if (now < s.lockUntil) return;
    if (now - s.at > 250) s.dx = 0;
    s.at = now;
    s.dx += e.deltaX;
    if (Math.abs(s.dx) > window.innerWidth * 0.3) {
      fire({ type: 'switchSpaceAdjacent', delta: s.dx > 0 ? 1 : -1 });
      s.dx = 0;
      s.lockUntil = now + 450;
    }
  };

  const permissionKinds = new Map();
  for (const p of state.permissionPrompts ?? []) permissionKinds.set(p.tab, [...(permissionKinds.get(p.tab) ?? []), ...p.kinds]);
  const permissions = new Map([...permissionKinds].map(([tab, kinds]) => [tab, describePermissionKinds(kinds)]));

  // Keyboard entry point (tabindex 0): the active item if it is shown, else the first row.
  const activeLoc = locateItem(state, state.activeItem);
  const entry =
    activeLoc && !activeLoc.split && (activeLoc.section === 'favorites' || activeLoc.space?.id === space?.id)
      ? state.activeItem
      : (state.favorites[0]?.id ?? space?.pinned[0]?.id ?? space?.today[0]?.id ?? null);

  const downloadsOpen = panel?.type === 'downloads';
  const activeDownload = state.downloads.find((d) => d.state === 'inProgress');
  const sheetSpace = panel?.type === 'editSpace' ? state.spaces.find((s) => s.id === panel.id) : null;
  const sheetOpen = panel?.type === 'newSpace' || Boolean(sheetSpace);
  const favRename = renameId != null && state.favorites.some((t) => t.id === renameId) ? renameId : null;
  const editPinnedTab = panel?.type === 'editPinned' ? locateItem(state, panel.id)?.node : null;

  // ------------------------------------------------------------ keyed diffs → this render's motion
  //
  // This runs in the component body, which Preact executes **before** it patches the DOM: the FLIP
  // rects measured here are the "first" ones, and the rows that are about to unmount are still there
  // for `rows.js` to ghost. Nothing is measured unless a signature actually changed.
  const hasCard = Boolean(activeDownload) && !downloadsOpen;
  const sig = {
    space: space?.id ?? null,
    list: space ? listOrder(space) : '',
    collapsed: space ? collapsedFolders(space) : '',
    favorites: state.favorites.map((t) => t.id).join(','),
    layout: layoutSignature(state, space, favRename, renameId, hasCard),
    today: space ? space.today.length : 0,
  };
  const previous = signatures.current;
  signatures.current = sig;
  const switched = previous !== null && previous.space !== sig.space;
  if (switched) spaceExitDir = direction;
  let exitPlan = { on: false };
  let listFlipKey = null;
  let favFlipKey = null;
  if (previous && !switched) {
    const collapseChanged = previous.collapsed !== sig.collapsed;
    if (previous.today - sig.today >= CLEAR_TODAY_ROWS) {
      // Clear Today (which keeps the visible and audible rows, so the list rarely reaches zero):
      // one sweep of ghosts sliding down, never one fade per row.
      exitPlan = { on: true, key: 'sidebar.clearToday', budget: 12, stagger: motion.STAGGER_TOTAL_MS };
    } else if (collapseChanged) {
      exitPlan = { on: true, key: 'sidebar.folderExpand', budget: 20 };
    } else {
      exitPlan = { on: true, key: null, budget: motion.FLIP_BULK_LIMIT };
    }
    if (previous.layout === sig.layout) {
      if (collapseChanged) listFlipKey = 'sidebar.folderExpand';
      else if (previous.list !== sig.list) listFlipKey = sameMembers(previous.list, sig.list) ? 'sidebar.reorder' : 'sidebar.tabInsertRemove';
      if (previous.favorites !== sig.favorites && sameMembers(previous.favorites, sig.favorites)) favFlipKey = 'sidebar.favorites';
    }
  }
  setRowExitMode(exitPlan);
  // The list's own scroll offset before the change. A shrinking list clamps `scrollTop`, which moves
  // *every* row by the same amount: a scroll, not a rearrangement, and never something to animate.
  const scrolled = rootRef.current?.querySelector('.space-scroller')?.scrollTop ?? 0;
  // How many ids the step changes, so a list that emptied or refilled is refused as the bulk change
  // it is however few of its rows ended up moving (a reorder and a folder toggle change none).
  const listChanged = previous && !switched ? changedCount(previous.list, sig.list) : 0;
  const favChanged = previous && !switched ? changedCount(previous.favorites, sig.favorites) : 0;
  if (listFlipKey)
    flips.current.push([listFlipKey, motion.flip.capture(rootRef.current, listFlipKey, '.space-scroll-content [data-row]', { changed: listChanged })]);
  if (favFlipKey) flips.current.push([favFlipKey, motion.flip.capture(rootRef.current, favFlipKey, '.favorites [data-row]', { changed: favChanged })]);

  // …and play them once the new layout is in place.
  useLayoutEffect(() => {
    const pending = flips.current;
    flips.current = [];
    if (!pending.length) return;
    if ((rootRef.current?.querySelector('.space-scroller')?.scrollTop ?? 0) !== scrolled) return;
    // A close made with the pointer in the list does not pull the next row's × under the cursor.
    const pointer = Date.now() < sb.pointerCloseUntil;
    for (const [key, captured] of pending) {
      captured?.play({ duration: motion.duration(key, 200), pointer: pointer && key === 'sidebar.tabInsertRemove' });
    }
  });

  // `--float-width` is the width the floating card settles at; the CSS pins the contents to the
  // right edge at exactly that width while the window still clips the card (`useLayoutWidth`).
  const rootStyle = { ...(preview ? themeStyle(preview) : null), ...(floating ? { '--float-width': `${width}px` } : null) };

  const appMenuItems = [
    { label: 'New Tab', icon: 'plus', hint: 'Ctrl+T', onSelect: () => fire({ type: 'openCommandBar', mode: 'newTab' }) },
    { label: 'New Space', icon: 'space', onSelect: () => fire({ type: 'openSidebarPanel', panel: { type: 'newSpace' } }) },
    { label: 'New Folder', icon: 'folder-plus', onSelect: () => fire({ type: 'newFolder', space: state.activeSpace }) },
    { type: 'separator' },
    { label: 'Downloads', icon: 'download', hint: 'Ctrl+J', onSelect: () => fire({ type: 'openSidebarPanel', panel: { type: 'downloads' } }) },
    { label: 'History', icon: 'history', hint: 'Ctrl+H', onSelect: () => fire({ type: 'openInternalPage', page: 'history' }) },
    { label: 'Archive', icon: 'archive', hint: state.archiveCount ? String(state.archiveCount) : undefined, onSelect: () => fire({ type: 'openInternalPage', page: 'archive' }) },
    { label: 'Boosts', icon: 'boost', onSelect: () => fire({ type: 'openInternalPage', page: 'boosts' }) },
    { label: 'Settings', icon: 'settings', hint: 'Ctrl+,', onSelect: () => fire({ type: 'openInternalPage', page: 'settings' }) },
    { type: 'separator' },
    // Ctrl+E is page-first (D5a), so the menu is the way in for pages that take the key themselves.
    { label: 'Extensions', icon: 'puzzle', hint: 'Ctrl+E', onSelect: () => fire({ type: 'openCommandBar', mode: 'extensions' }) },
    { label: 'Developer Tools', icon: 'code', hint: 'F12', onSelect: () => fire({ type: 'toggleDevTools' }) },
    { type: 'header', label: 'Appearance' },
    ...[
      ['system', 'System', 'screen'],
      ['light', 'Light', 'sun'],
      ['dark', 'Dark', 'moon'],
    ].map(([value, label]) => ({
      key: `appearance-${value}`,
      label,
      checked: state.settings.appearance === value,
      onSelect: () => fire({ type: 'updateSettings', patch: { appearance: value } }),
    })),
    { type: 'separator' },
    { label: 'Quit sta', icon: 'quit', hint: 'Ctrl+Shift+W', onSelect: () => fire({ type: 'quit' }) },
  ];

  return html`<div
    ref=${rootRef}
    class=${classNames(
      'sidebar',
      floating && 'is-floating',
      presence === 'hidden' && 'is-hover-hidden',
      presence === 'entering' && 'is-hover-entering',
      preview && 'theme-scope is-previewing',
    )}
    data-theme=${preview ? (state.dark ? 'dark' : 'light') : undefined}
    style=${rootStyle}
    onKeyDown=${(e) => onNavKeyDown(e, rootRef.current, gridColumns(columns, state.favorites.length))}
    onMouseUp=${onMouseUp}
    onWheel=${onWheel}
  >
    <div class="sidebar-bg" aria-hidden="true" />
    <div class="top-row drag">
      <${IconButton}
        class="top-menu"
        icon="menu"
        label="sta menu"
        title="Menu (Alt+F)"
        buttonRef=${menuButton}
        aria-expanded=${String(panel?.type === 'appMenu')}
        onClick=${() => (panel?.type === 'appMenu' ? closePanel() : fire({ type: 'openSidebarPanel', panel: { type: 'appMenu' } }))}
      />
      <span class="top-row-spacer" />
      <${IconButton}
        icon="sidebar"
        label=${floating ? 'Keep sidebar open' : 'Hide sidebar'}
        title=${floating ? 'Keep sidebar open (Ctrl+S)' : 'Hide sidebar (Ctrl+S)'}
        onClick=${() => fire({ type: 'toggleSidebar' })}
      />
      <${NavButtons} current=${state.current} />
    </div>
    <div class="pill-row">
      <${UrlPill} current=${state.current} />
    </div>
    <div class="sidebar-main">
      <${FavoritesGrid} state=${state} columns=${columns} renameSeq=${renameSeq} renameId=${favRename} permissions=${permissions} entry=${entry} />
      ${space &&
      html`<${SpacePane}
        key=${space.id}
        state=${state}
        space=${space}
        direction=${direction}
        renameId=${favRename == null ? renameId : null}
        renameSeq=${renameSeq}
        permissions=${permissions}
        entry=${entry}
      />`}
    </div>
    <div class="bottom-area">
      ${activeDownload && !downloadsOpen && html`<${DownloadCard} download=${activeDownload} onOpen=${() => fire({ type: 'openSidebarPanel', panel: { type: 'downloads' } })} />`}
      <${BottomBar}
        state=${state}
        downloadsButton=${downloadsButton}
        downloadsOpen=${downloadsOpen}
        onDownloads=${() => (downloadsOpen ? closePanel() : fire({ type: 'openSidebarPanel', panel: { type: 'downloads' } }))}
      />
    </div>
    <${ResizeHandle} />

    ${mounted &&
    sheetOpen &&
    html`<${SpaceSheet}
      key=${panelView.seq}
      state=${state}
      space=${sheetSpace}
      onClose=${(reason) => (reason === 'created' || reason === 'deleted' ? setDismissedSeq(panelView.seq) : closePanel())}
      onPreview=${setPreview}
    />`}
  </div>
  ${mounted &&
  downloadsOpen &&
  html`<${PanelGhost} selector=".portal-host .downloads-popover">
    <${DownloadsPanel} key=${panelView.seq} downloads=${state.downloads} anchor=${downloadsButton.current} onClose=${closePanel} />
  <//>`}
  ${mounted &&
  panel?.type === 'appMenu' &&
  html`<${PanelGhost} selector=".portal-host .app-menu"><${Menu}
    key=${panelView.seq}
    anchor=${menuButton.current}
    placement="bottom-start"
    label="sta menu"
    class="app-menu"
    closeOnDismiss=${false}
    items=${appMenuItems}
    onClose=${() => {
      // Runs before a selected item's own command, so e.g. "New Space" re-opens a panel after it.
      setDismissedSeq(panelView.seq);
      fire({ type: 'closeSidebarPanel' });
    }}
  /><//>`}
  ${menu &&
  html`<${Menu}
    key=${menu.key}
    x=${menu.x}
    y=${menu.y}
    anchor=${menu.anchor}
    placement=${menu.placement}
    items=${menu.items}
    label="Context menu"
    onClose=${() => setMenu(null)}
  />`}
  ${mounted &&
  editPinnedTab &&
  html`<${PanelGhost} selector=".portal-host .edit-pinned"><${EditPinnedPopover}
    key=${panelView.seq}
    tab=${editPinnedTab}
    anchor=${rootRef.current?.querySelector(`[data-row][data-id="${editPinnedTab.id}"]`) ?? rootRef.current}
    onClose=${(reason) => (reason === 'saved' ? setDismissedSeq(panelView.seq) : closePanel())}
  /><//>`}`;
}

// ------------------------------------------------------------------------------------ start

const mountPoint = document.getElementById('app') ?? document.body;
startSurface({ render: (state) => render(html`<${Sidebar} state=${state} />`, mountPoint) }).catch(() => {});

// Automation hook kept from the shell skeleton's placeholder page: crates/sta/e2e/shell-e2e.mjs
// counts `state` pushes through `window.sta.events`.
const events = [];
on('state', () => {
  events.push('state');
  if (events.length > 5000) events.splice(0, 2500);
});
window.sta = Object.freeze({ ...window.sta, events });
