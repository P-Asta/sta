// Sidebar-local UI controller: one per page. Holds the latest UiState for event handlers and the
// hooks the App component registers (context menus, local popovers, panel dismissal), so memoized
// rows can reach them without re-rendering when unrelated state changes.

import { fire } from './lib.js';

export const sb = {
  /** Latest UiState (set by the App on every render). */
  state: null,
  /** Optimistic titles after an inline rename: id → {title, revision}. */
  pendingTitles: new Map(),
  /** Timestamp until which row clicks are ignored (right after a drag ended). */
  suppressClickUntil: 0,
  /**
   * Timestamp until which a close made with the pointer in the list still suppresses the followers'
   * FLIP (`rows.js closeByPointer`, FINAL PLAN rule 7): the next row's × must not slide under the
   * cursor while the user is clicking.
   */
  pointerCloseUntil: 0,

  // ---- registered by the App component
  /** Open a context menu: `{x, y}` or `{anchor}` plus `items`. */
  openMenu: (_spec) => {},
  /** Hide the current sidebar panel locally (optimistic) before core confirms. */
  dismissPanelLocally: () => {},
  /** Remember the element to restore focus to after a panel closes. */
  focusRow: (_id) => {},

  /** Title to display for an item, honoring a just-committed rename. */
  titleOf(id, title) {
    const pending = this.pendingTitles.get(id);
    return pending ? pending.title : title;
  },

  /** Drop optimistic titles once a newer snapshot has arrived. */
  settlePendingTitles(revision) {
    for (const [id, p] of this.pendingTitles) if (revision > p.revision) this.pendingTitles.delete(id);
  },

  /** "Edit Pinned Page" of a pinned tab or favorite: a core panel that docks a hidden sidebar. */
  openEditPinned(id) {
    fire({ type: 'openSidebarPanel', panel: { type: 'editPinned', id } });
  },

  /** Start inline rename (F2 / double-click / menu). */
  startRename(id) {
    fire({ type: 'openSidebarPanel', panel: { type: 'renameItem', id } });
  },

  /**
   * Finish an inline rename. Unchanged text only closes the panel; tabs accept an empty title
   * (clears the custom title), folders ignore it.
   */
  commitRename(id, value, original, isFolder) {
    this.dismissPanelLocally();
    const next = String(value ?? '').trim();
    if (next === String(original ?? '').trim() || (isFolder && !next)) {
      fire({ type: 'closeSidebarPanel' });
      return;
    }
    if (next) this.pendingTitles.set(id, { title: next, revision: this.state?.revision ?? 0 });
    fire({ type: 'renameItem', id, title: next || null });
  },

  cancelRename() {
    this.dismissPanelLocally();
    fire({ type: 'closeSidebarPanel' });
  },

  /** Row click guard: ignore the click that ends a drag. */
  clickAllowed() {
    return Date.now() >= this.suppressClickUntil;
  },
};
