// Sidebar helpers: memoization, UiState lookups and drop-target descriptors shared by the row
// components, the drag & drop controller and the menus.

import { Component, h } from '/common/vendor/htm-preact.js';
import { dispatch } from '/common/ipc.js';
import { deepEqual } from '/common/util.js';

/** Dispatch a command, logging (never throwing) on failure. */
export function fire(command) {
  return dispatch(command).catch((e) => console.error('[sidebar] dispatch failed', command, e));
}

/**
 * `memo` for the vendored Preact (which has none): re-render only when a prop changed
 * structurally. Function props are ignored (handlers are stable module functions or refs).
 */
export function memo(render, name = render.name) {
  class Memo extends Component {
    shouldComponentUpdate(next) {
      const prev = this.props;
      const keys = new Set([...Object.keys(prev), ...Object.keys(next)]);
      for (const k of keys) {
        if (typeof prev[k] === 'function' && typeof next[k] === 'function') continue;
        if (prev[k] !== next[k] && !deepEqual(prev[k], next[k])) return true;
      }
      return false;
    }
    render(props) {
      return h(render, props);
    }
  }
  Memo.displayName = `memo(${name})`;
  return Memo;
}

export const SIDEBAR_MIN = 200;
export const SIDEBAR_MAX = 440;
export const SIDEBAR_DEFAULT = 248;

/** Favorites grid columns (arc_spec §5): clamp(floor((w − 16 + 8) / (52 + 8)), 3, 6). */
export function favoriteColumns(width) {
  return Math.min(6, Math.max(3, Math.floor((width - 16 + 8) / 60)));
}

/** Favicon URL the UI's CSP (img-src 'self' data: https:) can load, else null (letter tile). */
export const loadableFavicon = (url) => (/^(https:|data:image\/|sta:)/i.test(url ?? '') ? url : null);

export const isInternalUrl = (url) => /^sta:/i.test(url ?? '');

/** Glyph for an internal page tab (`sta://settings/` → `settings`). */
export function internalGlyph(url) {
  const host = /^sta:\/\/([a-z]+)/i.exec(url ?? '')?.[1]?.toLowerCase();
  return { settings: 'settings', archive: 'archive', history: 'history', boosts: 'boost' }[host] ?? 'star';
}

/** Container descriptors (the `Container` enum of command.rs). */
export const containers = {
  favorites: () => ({ type: 'favorites' }),
  pinned: (space) => ({ type: 'pinned', space }),
  today: (space) => ({ type: 'today', space }),
  folder: (id) => ({ type: 'folder', id }),
};

/** Readable permission kinds for tooltips. */
export function describePermissionKinds(kinds) {
  const names = {
    camera: 'camera',
    microphone: 'microphone',
    screenCapture: 'screen',
    geolocation: 'location',
    notifications: 'notifications',
    clipboard: 'clipboard',
    midiSysex: 'MIDI devices',
    storageAccess: 'storage',
    other: 'a permission',
  };
  const list = [...new Set(kinds.map((k) => names[k] ?? k))];
  if (list.length <= 1) return list[0] ?? 'a permission';
  return `${list.slice(0, -1).join(', ')} and ${list.at(-1)}`;
}

/** Where `id` lives in the state: `{node, section, container, space, ancestors, split}` or null. */
export function locateItem(state, id) {
  if (!state || id == null) return null;
  const fav = state.favorites.find((t) => t.id === id);
  if (fav) return { node: fav, section: 'favorites', container: containers.favorites(), space: null, ancestors: [], split: null };
  for (const space of state.spaces) {
    for (const section of ['pinned', 'today']) {
      const found = locateIn(space[section], id, section, space, containers[section](space.id), []);
      if (found) return found;
    }
  }
  return null;
}

function locateIn(nodes, id, section, space, container, ancestors) {
  for (const node of nodes) {
    if (node.id === id) return { node, section, container, space, ancestors, split: null };
    if (node.kind === 'folder') {
      const found = locateIn(node.children, id, section, space, containers.folder(node.id), [...ancestors, node.id]);
      if (found) return found;
    } else if (node.kind === 'split') {
      const pane = node.panes.find((p) => p.id === id);
      if (pane) return { node: pane, section, container, space, ancestors, split: node };
    }
  }
  return null;
}

/** Number of tabs inside a folder, recursively. */
export function folderTabCount(folder) {
  let n = 0;
  for (const c of folder.children) n += c.kind === 'folder' ? folderTabCount(c) : 1;
  return n;
}

/** Stable JSON for data attributes. */
export const json = (v) => JSON.stringify(v);

/** Read a `data-*` JSON attribute (null when missing/invalid). */
export function readJson(el, name) {
  const raw = el?.getAttribute(name);
  if (!raw) return null;
  try {
    return JSON.parse(raw);
  } catch {
    return null;
  }
}

// ------------------------------------------------------------------------------------ motion

/**
 * Leave an inert ghost behind when this component unmounts (`sidebar.panels`,
 * `sidebar.spaceSwitch`). The hook itself lives in the motion runtime, because the menus, the
 * overlays and the internal pages need exactly the same thing; it is re-exported here so the
 * sidebar's own modules keep importing it from one place.
 */
export { useExitGhost } from '/common/motion.js';
