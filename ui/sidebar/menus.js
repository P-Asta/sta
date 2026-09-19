// Context-menu item builders (PROTOCOL §5.1, arc_spec §2.23). Each returns `Menu` items whose
// `onSelect` dispatches core commands; the menu itself is rendered by the App.

import { sb } from './controller.js';
import { fire, folderTabCount, isInternalUrl } from './lib.js';

const sep = { type: 'separator' };

/**
 * The F2 hint for "Rename…": F2 renames only in the keyboard-focused docked sidebar. The floating
 * sidebar (hidden, revealed by hovering) never has keyboard focus, so F2 there goes to the page.
 */
const renameHint = (state) => (state?.window.sidebarVisible ? 'F2' : undefined);

function moveToSpaceItem(state, id, currentSpace, label = 'Move to Space') {
  const others = state.spaces.filter((s) => s.id !== currentSpace);
  return {
    label,
    icon: 'space',
    disabled: others.length === 0,
    submenu: others.map((s) => ({
      key: `space-${s.id}`,
      label: `${s.icon}  ${s.name}`,
      onSelect: () => fire({ type: 'moveToSpace', id, space: s.id }),
    })),
  };
}

function splitItem(state, tab) {
  const focused = state.focusedTab;
  const current = state.current;
  const inActiveSplit = Boolean(current?.splitPanes) && tab.visible;
  const full = (current?.splitPanes ?? 0) >= 4;
  return {
    label: 'Open in Split View',
    icon: 'split',
    disabled: focused == null || focused === tab.id || inActiveSplit || full,
    onSelect: () => fire({ type: 'splitWith', tab: tab.id, with: focused, side: 'right' }),
  };
}

/** "Share with AI Agents" (PROTOCOL §9): while agents may connect and only see their own tabs. */
function agentShareItem(state, tab) {
  const s = state.settings;
  if (!s || s.agentAccess === 'off' || s.agentScope === 'allTabs' || isInternalUrl(tab.url)) return null;
  return tab.agent
    ? { label: 'Stop Sharing with AI Agents', icon: 'agent', onSelect: () => fire({ type: 'shareTabWithAgent', tab: tab.id, shared: false }) }
    : { label: 'Share with AI Agents', icon: 'agent', onSelect: () => fire({ type: 'shareTabWithAgent', tab: tab.id, shared: true }) };
}

function muteItem(tab) {
  if (!tab.loaded) return null;
  return {
    label: tab.muted ? 'Unmute Tab' : 'Mute Tab',
    icon: tab.muted ? 'speaker' : 'speaker-muted',
    onSelect: () => fire({ type: 'toggleMute', id: tab.id }),
  };
}

function pinnedPageItems(tab) {
  return [
    {
      label: 'Edit Pinned Page',
      icon: 'pin',
      submenu: [
        {
          label: 'Replace with Current URL',
          disabled: !tab.navigated,
          onSelect: () => fire({ type: 'replacePinnedUrl', id: tab.id }),
        },
        { label: 'Edit…', onSelect: () => sb.openEditPinned(tab.id) },
      ],
    },
    {
      label: 'Reset to Pinned URL',
      icon: 'undo',
      disabled: !tab.navigated,
      onSelect: () => fire({ type: 'resetToPinned', id: tab.id }),
    },
  ];
}

const compact = (items) => {
  // Drop nulls and collapse duplicate/leading/trailing separators.
  const out = [];
  for (const item of items) {
    if (!item) continue;
    if (item.type === 'separator' && (out.length === 0 || out.at(-1).type === 'separator')) continue;
    out.push(item);
  }
  while (out.at(-1)?.type === 'separator') out.pop();
  return out;
};

/** Menu for a tab row or favorite tile. `section`: favorites | pinned | today. */
export function tabMenuItems(state, tab, section) {
  const focused = tab.id === state.focusedTab;
  const spaceId = tab.space ?? state.activeSpace;
  const copy = { label: 'Copy URL', icon: 'copy', hint: focused ? 'Ctrl+Shift+C' : undefined, onSelect: () => fire({ type: 'copyUrl', id: tab.id }) };
  const rename = { label: 'Rename…', icon: 'edit', hint: renameHint(state), onSelect: () => sb.startRename(tab.id) };
  const duplicate = { label: 'Duplicate Tab', icon: 'plus', onSelect: () => fire({ type: 'duplicateTab', id: tab.id }) };
  const addFavorite = {
    label: state.favoritesFull ? 'Favorites Are Full' : 'Add to Favorites',
    icon: 'star',
    disabled: state.favoritesFull,
    onSelect: () => fire({ type: 'addFavorite', id: tab.id }),
  };

  if (section === 'favorites') {
    return compact([
      copy,
      rename,
      sep,
      ...pinnedPageItems(tab),
      sep,
      { label: 'Remove from Favorites', icon: 'star', onSelect: () => fire({ type: 'removeFavorite', id: tab.id }) },
      sep,
      splitItem(state, tab),
      duplicate,
      muteItem(tab),
      agentShareItem(state, tab),
      sep,
      { label: 'Close Tab', icon: 'close', hint: focused ? 'Ctrl+W' : undefined, disabled: !tab.loaded, onSelect: () => fire({ type: 'closeItem', id: tab.id }) },
    ]);
  }
  if (section === 'pinned') {
    return compact([
      copy,
      rename,
      sep,
      ...pinnedPageItems(tab),
      sep,
      { label: 'Unpin Tab', icon: 'pin', hint: focused ? 'Ctrl+D' : undefined, onSelect: () => fire({ type: 'togglePin', id: tab.id }) },
      addFavorite,
      moveToSpaceItem(state, tab.id, spaceId),
      sep,
      splitItem(state, tab),
      duplicate,
      muteItem(tab),
      agentShareItem(state, tab),
      sep,
      { label: 'Close Tab', icon: 'close', hint: focused ? 'Ctrl+W' : undefined, disabled: !tab.loaded, onSelect: () => fire({ type: 'closeItem', id: tab.id }) },
    ]);
  }
  return compact([
    copy,
    rename,
    sep,
    { label: 'Pin Tab', icon: 'pin', hint: focused ? 'Ctrl+D' : undefined, onSelect: () => fire({ type: 'togglePin', id: tab.id }) },
    addFavorite,
    moveToSpaceItem(state, tab.id, spaceId),
    sep,
    splitItem(state, tab),
    duplicate,
    muteItem(tab),
    tab.loaded && { label: 'Unload Tab', icon: 'pause', onSelect: () => fire({ type: 'unloadTab', id: tab.id }) },
    agentShareItem(state, tab),
    sep,
    { label: 'Archive Tab', icon: 'archive', hint: focused ? 'Ctrl+W' : undefined, onSelect: () => fire({ type: 'closeItem', id: tab.id }) },
  ]);
}

/** Menu for one pane of a split row. */
export function splitPaneMenuItems(state, split, pane) {
  const spaceId = pane.space ?? state.activeSpace;
  return compact([
    { label: 'Copy URL', icon: 'copy', onSelect: () => fire({ type: 'copyUrl', id: pane.id }) },
    { label: 'Rename…', icon: 'edit', hint: renameHint(state), onSelect: () => sb.startRename(pane.id) },
    sep,
    { label: 'Separate from Split', icon: 'split', onSelect: () => fire({ type: 'separatePane', tab: pane.id }) },
    { label: 'Separate All Tabs', icon: 'split', onSelect: () => fire({ type: 'separateAll', id: split.id }) },
    moveToSpaceItem(state, split.id, spaceId, 'Move Split to Space'),
    sep,
    { label: 'Duplicate Tab', icon: 'plus', onSelect: () => fire({ type: 'duplicateTab', id: pane.id }) },
    muteItem(pane),
    sep,
    { label: 'Close Tab', icon: 'close', onSelect: () => fire({ type: 'closeItem', id: pane.id }) },
    { label: 'Archive Split View', icon: 'archive', onSelect: () => fire({ type: 'closeItem', id: split.id }) },
  ]);
}

/** Menu for a folder row. `depth` = nesting level of the folder (0 = top level). */
export function folderMenuItems(state, folder, depth, spaceId) {
  const tabs = folderTabCount(folder);
  return compact([
    { label: 'Rename…', icon: 'edit', hint: renameHint(state), onSelect: () => sb.startRename(folder.id) },
    {
      label: 'New Subfolder',
      icon: 'folder-plus',
      disabled: depth >= 2,
      onSelect: () => fire({ type: 'newFolder', space: spaceId, parent: folder.id }),
    },
    {
      label: folder.collapsed ? 'Expand Folder' : 'Collapse Folder',
      icon: folder.collapsed ? 'folder-open' : 'folder',
      onSelect: () => fire({ type: 'toggleFolder', id: folder.id }),
    },
    moveToSpaceItem(state, folder.id, spaceId),
    sep,
    {
      label: 'Delete Folder',
      icon: 'trash',
      danger: true,
      submenu: [
        { type: 'header', label: tabs ? `${tabs} tab${tabs === 1 ? '' : 's'} will be archived` : 'The folder is empty' },
        { label: `Delete “${folder.name}”`, icon: 'trash', danger: true, onSelect: () => fire({ type: 'deleteFolder', id: folder.id }) },
      ],
    },
  ]);
}

/** Menu for the space title row. */
export function spaceMenuItems(state, space) {
  const index = state.spaces.findIndex((s) => s.id === space.id);
  const last = state.spaces.length <= 1;
  return compact([
    { label: 'Edit Space…', icon: 'palette', onSelect: () => fire({ type: 'openSidebarPanel', panel: { type: 'editSpace', id: space.id } }) },
    { label: 'New Folder', icon: 'folder-plus', onSelect: () => fire({ type: 'newFolder', space: space.id }) },
    {
      label: 'Clear Today',
      icon: 'arrow-down',
      hint: space.id === state.activeSpace ? 'Ctrl+Shift+K' : undefined,
      disabled: space.today.length === 0,
      onSelect: () => fire({ type: 'clearToday', space: space.id }),
    },
    sep,
    { label: 'Move Space Left', icon: 'chevron-left', disabled: index <= 0, onSelect: () => fire({ type: 'moveSpace', id: space.id, index: index - 1 }) },
    { label: 'Move Space Right', icon: 'chevron-right', disabled: index >= state.spaces.length - 1, onSelect: () => fire({ type: 'moveSpace', id: space.id, index: index + 1 }) },
    sep,
    {
      label: 'Delete Space',
      icon: 'trash',
      danger: true,
      disabled: last,
      submenu: [
        { type: 'header', label: 'Its tabs will be archived' },
        { label: `Delete “${space.name}”`, icon: 'trash', danger: true, onSelect: () => fire({ type: 'deleteSpace', id: space.id }) },
      ],
    },
  ]);
}

/** Menu for empty space in the pinned/today lists. */
export function listMenuItems(state, space) {
  return [
    { label: 'New Tab', icon: 'plus', hint: 'Ctrl+T', onSelect: () => fire({ type: 'openCommandBar', mode: 'newTab' }) },
    { label: 'New Folder', icon: 'folder-plus', onSelect: () => fire({ type: 'newFolder', space: space.id }) },
    sep,
    {
      label: 'Clear Today',
      icon: 'arrow-down',
      hint: 'Ctrl+Shift+K',
      disabled: space.today.length === 0,
      onSelect: () => fire({ type: 'clearToday', space: space.id }),
    },
  ];
}
