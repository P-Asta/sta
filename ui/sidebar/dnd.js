// Sidebar drag & drop (PROTOCOL §5.1, arc_spec §2.6).
//
// Pointer-event based (no HTML5 DnD for our own rows): 4 px threshold, a ghost that follows the
// pointer, a 2 px accent insertion line with a 6 px dot, folder hover highlight after 300 ms (and
// auto-expand of collapsed folders after 900 ms), spring-loaded space icons (500 ms), center drops
// on Today rows → split view, auto-scroll within 32 px of the list edges (≤ 600 px/s), Esc cancels.
// Links dragged in from web pages (HTML5 DnD with text/uri-list) use the same targets → openUrlAt.
//
// Rows describe themselves with data attributes (rows.js):
//   [data-row] data-id data-kind(tab|folder|split|fav) data-section data-container(JSON)
//   data-next data-ancestors(JSON) data-depth, folders: data-expanded data-first-child,
//   split segments: [data-pane]; space icons: [data-space-id];
//   drop zones: [data-zone](today-top|divider|pinned-end|today-end|fav-grid|pinned-empty)
//   with data-container / data-before (divider also data-pinned).
// Space icons can also be dragged along the bottom bar to reorder spaces (moveSpace).

import * as motion from '/common/motion.js';
import { sb } from './controller.js';
import { fire, readJson } from './lib.js';

const THRESHOLD = 4;
const FOLDER_HOVER_MS = 300;
const FOLDER_EXPAND_MS = 900;
const SPRING_MS = 500;
const EDGE = 32;
const MAX_SPEED = 600; // px/s
const INDENT = 12;

const num = (v) => (v === '' || v == null ? null : Number(v));

function sourceFrom(el) {
  if (el.dataset.spaceId) {
    return { el, id: Number(el.dataset.spaceId), kind: 'space', section: null, container: null, next: null, ancestors: [], label: el.getAttribute('aria-label') ?? '' };
  }
  const kind = el.dataset.kind;
  return {
    el,
    id: Number(el.dataset.id),
    kind,
    section: el.dataset.section,
    container: readJson(el, 'data-container'),
    next: num(el.dataset.next),
    ancestors: readJson(el, 'data-ancestors') ?? [],
    label: el.getAttribute('aria-label') ?? '',
  };
}

/** Whether `source` may be dropped into `container` (mirrors core's matrix; core re-validates). */
function allowed(source, container, targetAncestors = []) {
  if (!container) return false;
  if (source.kind === 'link') return true;
  const kind = source.kind === 'fav' ? 'tab' : source.kind;
  const state = sb.state;
  switch (container.type) {
    case 'favorites':
      return kind === 'tab' && (source.section === 'favorites' || !state?.favoritesFull);
    case 'pinned':
      return kind !== 'split';
    case 'folder':
      if (kind === 'split') return false;
      if (kind === 'folder' && (container.id === source.id || targetAncestors.includes(source.id))) return false;
      return true;
    case 'today':
      return kind !== 'folder';
    default:
      return false;
  }
}

const sameContainer = (a, b) => a && b && a.type === b.type && a.space === b.space && a.id === b.id;

export function installDragAndDrop(root) {
  /** Pending press before the threshold: {pointerId, x, y, el} */
  let press = null;
  /** Active drag: {source, pointerId?, ghost, offsetX, offsetY, x, y, target, folder, spring, raf} */
  let drag = null;

  const line = document.createElement('div');
  line.className = 'drop-line';
  line.hidden = true;
  document.body.appendChild(line);

  // ---------------------------------------------------------------- indicators

  let marked = [];
  function mark(el, cls, side) {
    el.classList.add(cls);
    if (side) el.dataset.splitSide = side;
    marked.push([el, cls]);
  }
  function clearMarks() {
    for (const [el, cls] of marked) {
      el.classList.remove(cls);
      delete el.dataset.splitSide;
    }
    marked = [];
    line.hidden = true;
  }
  /** Where the insertion line stands while it is visible, so the next target glides to it. */
  let lineAt = null;
  function showLine({ x, y, w, h, vertical }) {
    line.hidden = false;
    line.classList.toggle('is-vertical', Boolean(vertical));
    const nx = Math.round(x);
    const ny = Math.round(y);
    // `sidebar.dragDrop`: the line glides between insertion points. The individual `translate`
    // property (not `transform`), so the glide never fights the width/height set in the same call,
    // and keyed on the position actually changing - never on a pointer move that lands in the same
    // gap.
    if (lineAt && (lineAt.x !== nx || lineAt.y !== ny)) {
      motion.animate(
        line,
        'sidebar.dragDrop',
        [{ translate: `${lineAt.x}px ${lineAt.y}px` }, { translate: `${nx}px ${ny}px` }],
        { duration: motion.duration('sidebar.dragDrop', 120) },
      );
    }
    line.style.translate = `${nx}px ${ny}px`;
    line.style.width = vertical ? '' : `${Math.max(0, Math.round(w))}px`;
    line.style.height = vertical ? `${Math.max(0, Math.round(h))}px` : '';
    lineAt = { x: nx, y: ny };
  }

  // ---------------------------------------------------------------- hit testing

  const indentX = (rect, depth) => rect.left + 6 + depth * INDENT;

  /** Insertion before `row` in its container. */
  function beforeRow(row) {
    const r = row.getBoundingClientRect();
    const depth = Number(row.dataset.depth) || 0;
    const x = indentX(r, depth);
    return {
      kind: 'insert',
      container: readJson(row, 'data-container'),
      before: Number(row.dataset.id),
      ancestors: readJson(row, 'data-ancestors') ?? [],
      line: { x, y: r.top - 2, w: r.right - x - 4 },
    };
  }

  /** Insertion after `row` (into an expanded folder: before its first child). */
  function afterRow(row) {
    const r = row.getBoundingClientRect();
    const depth = Number(row.dataset.depth) || 0;
    if (row.dataset.kind === 'folder' && row.dataset.expanded === '1') {
      const x = indentX(r, depth + 1);
      return {
        kind: 'insert',
        container: { type: 'folder', id: Number(row.dataset.id) },
        before: num(row.dataset.firstChild),
        ancestors: [...(readJson(row, 'data-ancestors') ?? []), Number(row.dataset.id)],
        line: { x, y: r.bottom, w: r.right - x - 4 },
      };
    }
    const x = indentX(r, depth);
    return {
      kind: 'insert',
      container: readJson(row, 'data-container'),
      before: num(row.dataset.next),
      ancestors: readJson(row, 'data-ancestors') ?? [],
      line: { x, y: r.bottom, w: r.right - x - 4 },
    };
  }

  function zoneTarget(zone, y) {
    const r = zone.getBoundingClientRect();
    const type = zone.dataset.zone;
    const x = r.left + 6;
    const w = r.width - 10;
    switch (type) {
      case 'today-top':
        return { kind: 'insert', container: readJson(zone, 'data-container'), before: num(zone.dataset.before), line: { x, y: r.bottom, w } };
      case 'divider':
        if (y < r.top + r.height / 2) {
          return { kind: 'insert', container: readJson(zone, 'data-pinned'), before: null, line: { x, y: r.top, w } };
        }
        return null;
      case 'pinned-end':
      case 'today-end':
        return { kind: 'insert', container: readJson(zone, 'data-container'), before: null, line: { x, y: r.top, w } };
      case 'pinned-empty':
        return { kind: 'insert', container: readJson(zone, 'data-container'), before: null, highlight: zone };
      case 'fav-grid': {
        const tiles = zone.querySelectorAll('[data-kind="fav"]');
        const last = tiles[tiles.length - 1];
        if (!last) return { kind: 'insert', container: { type: 'favorites' }, before: null, highlight: zone };
        const lr = last.getBoundingClientRect();
        return { kind: 'insert', container: { type: 'favorites' }, before: null, line: { x: lr.right + 3, y: lr.top + 4, h: lr.height - 8, vertical: true } };
      }
      default:
        return null;
    }
  }

  function rowTarget(row, x, y, el) {
    const source = drag.source;
    const r = row.getBoundingClientRect();
    const rel = (y - r.top) / Math.max(1, r.height);
    const kind = row.dataset.kind;
    const id = Number(row.dataset.id);

    if (kind === 'fav') {
      const before = x < r.left + r.width / 2;
      return {
        kind: 'insert',
        container: { type: 'favorites' },
        before: before ? id : num(row.dataset.next),
        // Centered in the 8 px gap between tiles.
        line: { x: before ? r.left - 5 : r.right + 3, y: r.top + 4, h: r.height - 8, vertical: true },
      };
    }
    const canSplit = (source.kind === 'tab' || source.kind === 'fav') && row.dataset.section === 'today' && id !== source.id;
    if (kind === 'split') {
      if (rel > 0.25 && rel < 0.75 && canSplit) {
        const seg = el.closest('[data-pane]') ?? row.querySelector('[data-pane]');
        const withId = Number(seg.dataset.pane);
        if (withId === source.id) return null;
        const sr = seg.getBoundingClientRect();
        return { kind: 'split', withId, side: x < sr.left + sr.width / 2 ? 'left' : 'right', highlight: seg };
      }
      return rel < 0.5 ? beforeRow(row) : afterRow(row);
    }
    if (kind === 'folder') {
      if (rel >= 0.25 && rel <= 0.75 && id !== source.id) {
        return { kind: 'folder', folderId: id, row, collapsed: row.dataset.expanded !== '1', ancestors: [...(readJson(row, 'data-ancestors') ?? []), id] };
      }
      return rel < 0.5 ? beforeRow(row) : afterRow(row);
    }
    // tab row
    if (canSplit && rel > 0.28 && rel < 0.72) {
      return { kind: 'split', withId: id, side: x < r.left + r.width / 2 ? 'left' : 'right', highlight: row };
    }
    return rel < 0.5 ? beforeRow(row) : afterRow(row);
  }

  /** Reordering space icons: insertion before/after the hovered icon → `moveSpace {id, index}`. */
  function spaceOrderTarget(btn, x) {
    const buttons = [...root.querySelectorAll('[data-space-id]')];
    const from = buttons.findIndex((b) => Number(b.dataset.spaceId) === drag.source.id);
    const i = buttons.indexOf(btn);
    const r = btn.getBoundingClientRect();
    const after = x >= r.left + r.width / 2;
    const insertAt = after ? i + 1 : i;
    const index = insertAt > from ? insertAt - 1 : insertAt;
    const gap = 2; // half of --space-icon-gap
    return {
      kind: 'space-order',
      index,
      noop: index === from,
      line: { x: (after ? r.right + gap : r.left - gap) - 1, y: r.top + 3, h: r.height - 6, vertical: true },
    };
  }

  function computeTarget(x, y) {
    const el = document.elementFromPoint(x, y);
    if (!el || !root.contains(el)) return null;
    const source = drag.source;

    const spaceBtn = el.closest('[data-space-id]');
    if (source.kind === 'space') return spaceBtn ? spaceOrderTarget(spaceBtn, x) : null;
    if (spaceBtn) {
      const space = Number(spaceBtn.dataset.spaceId);
      if (source.kind === 'fav') return { kind: 'space', space, el: spaceBtn, invalid: true };
      return { kind: 'space', space, el: spaceBtn };
    }
    const row = el.closest('[data-row]');
    if (row) return rowTarget(row, x, y, el);
    const zone = el.closest('[data-zone]');
    if (zone) return zoneTarget(zone, y);
    return null;
  }

  /** Validate a raw target against the dragged source; null = nothing to show. */
  function validate(t) {
    const source = drag.source;
    if (!t) return null;
    if (t.kind === 'space-order') return t;
    if (t.kind === 'space') return t.invalid ? { ...t, rejected: true } : t;
    if (t.kind === 'split') return source.kind === 'link' ? null : t;
    if (t.kind === 'folder') {
      const container = { type: 'folder', id: t.folderId };
      return allowed(source, container, t.ancestors) ? { ...t, container } : null;
    }
    if (t.kind === 'insert') {
      if (!allowed(source, t.container, t.ancestors ?? [])) {
        return t.container?.type === 'favorites' ? { ...t, rejected: true } : null;
      }
      if (source.kind !== 'link' && sameContainer(source.container, t.container) && (t.before === source.id || (t.before ?? null) === source.next)) {
        return { ...t, noop: true };
      }
      return t;
    }
    return null;
  }

  // ---------------------------------------------------------------- folder hover & spring loading

  function clearTimers() {
    if (drag?.folder) clearTimeout(drag.folder.timer);
    if (drag?.spring) clearTimeout(drag.spring.timer);
    if (drag) {
      drag.folder = null;
      drag.spring = null;
    }
  }

  function updateTarget() {
    updateTargetInner();
    // The line ended this pass hidden (no insertion point under the pointer): the next one appears
    // where it is, rather than gliding from wherever the last one stood.
    if (line.hidden) lineAt = null;
  }

  function updateTargetInner() {
    if (!drag) return;
    const raw = validate(computeTarget(drag.x, drag.y));
    clearMarks();

    // Folder hover: highlight after 300 ms, expand a collapsed folder after 900 ms.
    if (raw?.kind === 'folder') {
      if (drag.folder?.id !== raw.folderId) {
        if (drag.folder) clearTimeout(drag.folder.timer);
        const folder = { id: raw.folderId, armed: false, expanded: false };
        folder.timer = setTimeout(() => {
          folder.armed = true;
          updateTarget();
          if (raw.collapsed) {
            folder.timer = setTimeout(() => {
              if (drag?.folder === folder && !folder.expanded) {
                folder.expanded = true;
                fire({ type: 'toggleFolder', id: folder.id });
              }
            }, FOLDER_EXPAND_MS - FOLDER_HOVER_MS);
          }
        }, FOLDER_HOVER_MS);
        drag.folder = folder;
      }
    } else if (drag.folder) {
      clearTimeout(drag.folder.timer);
      drag.folder = null;
    }

    // Spring-loaded space icons.
    if (raw?.kind === 'space' && !raw.rejected && raw.space !== sb.state?.activeSpace) {
      if (drag.spring?.space !== raw.space) {
        if (drag.spring) clearTimeout(drag.spring.timer);
        const spring = { space: raw.space };
        spring.timer = setTimeout(() => {
          if (drag?.spring === spring) fire({ type: 'switchSpace', id: spring.space });
        }, SPRING_MS);
        drag.spring = spring;
      }
    } else if (drag.spring) {
      clearTimeout(drag.spring.timer);
      drag.spring = null;
    }

    let target = raw;
    if (raw?.kind === 'folder' && !drag.folder?.armed) target = null;
    drag.target = target && !target.rejected && !target.noop ? target : null;
    // A drop onto full Favorites is still sent so core explains it with its "Favorites are full" toast.
    drag.rejected = target?.rejected && target.kind === 'insert' && target.container?.type === 'favorites' ? target : null;
    drag.ghost?.classList.toggle('is-rejected', Boolean(raw?.rejected));

    if (!target || target.noop) return;
    if (target.rejected) {
      const el = target.el ?? root.querySelector('.favorites, .favorites-empty');
      if (el) mark(el, 'is-drop-rejected');
      return;
    }
    if (target.kind === 'space') mark(target.el, 'is-drop-target');
    else if (target.kind === 'folder') mark(target.row, 'is-drop-into');
    else if (target.kind === 'split') mark(target.highlight, 'is-split-target', target.side);
    else if (target.highlight) mark(target.highlight, 'is-drop-into');
    else if (target.line) showLine(target.line);
  }

  // ---------------------------------------------------------------- auto-scroll

  function scroller() {
    return root.querySelector('.space-scroller');
  }

  function autoScrollTick(now) {
    if (!drag) return;
    const sc = scroller();
    let speed = 0;
    if (sc) {
      const r = sc.getBoundingClientRect();
      if (drag.x >= r.left && drag.x <= r.right) {
        if (drag.y < r.top + EDGE && drag.y > r.top - EDGE) speed = -MAX_SPEED * Math.min(1, (r.top + EDGE - drag.y) / EDGE);
        else if (drag.y > r.bottom - EDGE && drag.y < r.bottom + EDGE) speed = MAX_SPEED * Math.min(1, (drag.y - (r.bottom - EDGE)) / EDGE);
      }
    }
    const dt = drag.lastTick ? Math.min(50, now - drag.lastTick) : 16;
    drag.lastTick = now;
    if (speed && sc) {
      const before = sc.scrollTop;
      sc.scrollTop += (speed * dt) / 1000;
      if (sc.scrollTop !== before) updateTarget();
    }
    drag.raf = requestAnimationFrame(autoScrollTick);
  }

  // ---------------------------------------------------------------- ghost

  function buildGhost(source, x, y) {
    const r = source.el.getBoundingClientRect();
    const ghost = document.createElement('div');
    ghost.className = `drag-ghost kind-${source.kind}`;
    const icons =
      source.kind === 'split'
        ? [...source.el.querySelectorAll('.split-seg .row-icon')].slice(0, 2)
        : source.kind === 'space'
          ? [source.el]
          : [source.el.querySelector('.row-icon, .favicon, .tab-glyph, .spinner')];
    for (const icon of icons) {
      if (!icon) continue;
      const wrap = document.createElement('span');
      wrap.className = 'drag-ghost-icon';
      wrap.appendChild(icon.cloneNode(true));
      ghost.appendChild(wrap);
    }
    if (source.kind !== 'fav' && source.kind !== 'space') {
      const label = document.createElement('span');
      label.className = 'drag-ghost-label';
      label.textContent = source.kind === 'split' ? 'Split View' : source.label;
      ghost.appendChild(label);
      ghost.style.width = `${Math.min(r.width, 220)}px`;
    }
    document.body.appendChild(ghost);
    // The ghost trails below-right of the pointer so the row, line or icon under the pointer stays
    // visible; near the bottom edge it flips above.
    return { ghost, offsetX: 14, offsetY: -12, ghostH: ghost.offsetHeight };
  }

  function moveGhost() {
    if (!drag?.ghost) return;
    let top = drag.y - drag.offsetY;
    if (top + drag.ghostH > window.innerHeight - 2) top = drag.y - 12 - drag.ghostH;
    const left = Math.min(drag.x - drag.offsetX, window.innerWidth - 40);
    drag.ghost.style.transform = `translate(${Math.round(left)}px, ${Math.round(top)}px)`;
  }

  // ---------------------------------------------------------------- lifecycle

  function begin(source, x, y, pointerId) {
    drag = { source, pointerId, x, y, target: null, folder: null, spring: null, raf: 0, lastTick: 0 };
    if (source.kind !== 'link') {
      Object.assign(drag, buildGhost(source, x, y));
      source.el.classList.add('is-drag-source');
      moveGhost();
    }
    document.documentElement.classList.add('is-dragging');
    document.documentElement.dataset.dragKind = source.kind;
    drag.raf = requestAnimationFrame(autoScrollTick);
    updateTarget();
  }

  /**
   * `sidebar.reorder`: a dropped row's ghost settles where it was let go, because core's state push
   * (and the followers' FLIP) only arrive a frame or two later. A refused or cancelled drag gets
   * nothing at all - the plan is explicit that nothing plays after a rejected drop.
   */
  function settleGhost(ghost, settle) {
    if (!ghost) return;
    const anim = settle
      ? motion.animate(
          ghost,
          'sidebar.reorder',
          [
            { opacity: 0.94, scale: 1 + motion.distance(0.03) },
            { opacity: 0, scale: 1 },
          ],
          { duration: motion.duration('sidebar.reorder', 200) },
        )
      : null;
    if (!anim) {
      ghost.remove();
      return;
    }
    const drop = () => ghost.remove();
    anim.finished.then(drop, drop);
  }

  function end(commit) {
    if (!drag) return;
    const d = drag;
    clearTimers();
    cancelAnimationFrame(d.raf);
    clearMarks();
    lineAt = null;
    settleGhost(d.ghost, Boolean(commit && d.target));
    d.source.el?.classList.remove('is-drag-source');
    document.documentElement.classList.remove('is-dragging');
    delete document.documentElement.dataset.dragKind;
    drag = null;
    if (d.pointerId != null && root.hasPointerCapture?.(d.pointerId)) root.releasePointerCapture(d.pointerId);
    if (d.source.kind !== 'link') sb.suppressClickUntil = Date.now() + 250;
    if (commit && d.target) perform(d.source, d.target);
    else if (commit && d.rejected && d.source.kind !== 'link') perform(d.source, d.rejected);
  }

  function perform(source, t) {
    if (source.kind === 'link') {
      const url = source.url;
      if (!url) return;
      if (t.kind === 'insert') fire({ type: 'openUrlAt', url, to: { container: t.container, before: t.before ?? null } });
      else if (t.kind === 'folder') fire({ type: 'openUrlAt', url, to: { container: t.container, before: null } });
      else if (t.kind === 'space') {
        // Like a tab moved to a space (and a new tab): at the top of that space's Today.
        const first = sb.state?.spaces.find((s) => s.id === t.space)?.today[0]?.id ?? null;
        fire({ type: 'openUrlAt', url, to: { container: { type: 'today', space: t.space }, before: first } });
      }
      return;
    }
    if (t.kind === 'space-order') fire({ type: 'moveSpace', id: source.id, index: t.index });
    else if (t.kind === 'insert') fire({ type: 'moveItem', id: source.id, to: { container: t.container, before: t.before ?? null } });
    else if (t.kind === 'folder') fire({ type: 'moveItem', id: source.id, to: { container: t.container, before: null } });
    else if (t.kind === 'split') fire({ type: 'splitWith', tab: source.id, with: t.withId, side: t.side });
    else if (t.kind === 'space') fire({ type: 'moveToSpace', id: source.id, space: t.space });
  }

  // ---------------------------------------------------------------- pointer events

  function onPointerDown(e) {
    if (e.button !== 0 || drag || e.pointerType === 'touch') return;
    const spaceBtn = e.target.closest('[data-space-id]');
    if (spaceBtn && root.contains(spaceBtn)) {
      press = { pointerId: e.pointerId, x: e.clientX, y: e.clientY, el: spaceBtn };
      return;
    }
    const row = e.target.closest('[data-row]');
    if (!row || !root.contains(row) || e.target.closest('button, input, textarea, .row-rename')) return;
    press = { pointerId: e.pointerId, x: e.clientX, y: e.clientY, el: row };
  }

  function onPointerMove(e) {
    if (drag && e.pointerId === drag.pointerId) {
      drag.x = e.clientX;
      drag.y = e.clientY;
      moveGhost();
      updateTarget();
      return;
    }
    if (!press || e.pointerId !== press.pointerId) return;
    if (Math.hypot(e.clientX - press.x, e.clientY - press.y) < THRESHOLD) return;
    if (!(e.buttons & 1)) {
      press = null;
      return;
    }
    const source = sourceFrom(press.el);
    press = null;
    try {
      root.setPointerCapture(e.pointerId);
    } catch {
      // the pointer may already be gone
    }
    sb.openMenu(null);
    begin(source, e.clientX, e.clientY, e.pointerId);
  }

  function onPointerUp(e) {
    press = null;
    if (drag && e.pointerId === drag.pointerId) {
      drag.x = e.clientX;
      drag.y = e.clientY;
      updateTarget();
      end(true);
    }
  }

  function onPointerCancel(e) {
    press = null;
    if (drag && e.pointerId === drag.pointerId) end(false);
  }

  function onKeyDown(e) {
    if (drag && e.key === 'Escape') {
      e.preventDefault();
      e.stopPropagation();
      end(false);
    }
  }

  // ---------------------------------------------------------------- links from web pages

  const hasLink = (dt) => Boolean(dt && [...dt.types].includes('text/uri-list'));

  function extractUrl(dt) {
    const raw = dt.getData('text/uri-list') || dt.getData('text/plain') || '';
    const line = raw.split(/\r?\n/).map((l) => l.trim()).find((l) => l && !l.startsWith('#'));
    if (!line) return null;
    try {
      const u = new URL(line);
      return ['http:', 'https:', 'file:'].includes(u.protocol) ? u.href : null;
    } catch {
      return null;
    }
  }

  /** Pending end of a link drag after `dragleave` (cancelled by the next `dragover`). */
  let leaveTimer = 0;

  function onDragOver(e) {
    if (!hasLink(e.dataTransfer)) return;
    e.preventDefault();
    e.dataTransfer.dropEffect = 'copy';
    clearTimeout(leaveTimer);
    if (!drag) begin({ kind: 'link', id: null, section: null, container: null, next: null, ancestors: [] }, e.clientX, e.clientY, null);
    if (drag.source.kind !== 'link') return;
    drag.x = e.clientX;
    drag.y = e.clientY;
    updateTarget();
  }

  // Chromium fires `dragleave` (with a null relatedTarget) every time the pointer crosses from one
  // child element to another, so ending the drag right away would flicker the indicator and reset
  // the folder-hover / spring-loading timers. End it only if no `dragover` follows (the pointer
  // really left the sidebar, or the drag was cancelled).
  function onDragLeave() {
    if (drag?.source.kind !== 'link') return;
    clearTimeout(leaveTimer);
    leaveTimer = setTimeout(() => {
      if (drag?.source.kind === 'link') end(false);
    }, 120);
  }

  function onDrop(e) {
    if (drag?.source.kind !== 'link') return;
    clearTimeout(leaveTimer);
    e.preventDefault();
    drag.source.url = extractUrl(e.dataTransfer);
    drag.x = e.clientX;
    drag.y = e.clientY;
    updateTarget();
    end(true);
  }

  root.addEventListener('pointerdown', onPointerDown);
  window.addEventListener('pointermove', onPointerMove);
  window.addEventListener('pointerup', onPointerUp);
  window.addEventListener('pointercancel', onPointerCancel);
  root.addEventListener('lostpointercapture', (e) => {
    if (drag && e.pointerId === drag.pointerId) end(false);
  });
  window.addEventListener('keydown', onKeyDown, true);
  window.addEventListener('blur', () => end(false));
  root.addEventListener('dragover', onDragOver);
  root.addEventListener('dragleave', onDragLeave);
  root.addEventListener('drop', onDrop);

  return {
    get active() {
      return Boolean(drag);
    },
    cancel: () => end(false),
  };
}
