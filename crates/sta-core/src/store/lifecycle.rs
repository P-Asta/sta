//! Tab lifecycle: browsers, opening, activation, closing, archiving and restoring.

use super::tree::{equalize, normalize_fractions};
use super::*;
use crate::urls;
use crate::store::extensions::WEB_STORE_TOAST;

impl Store {
    // ------------------------------------------------------------------------------ browsers

    /// URL a tab loads when its browser is (re)created: `pinned_url` for pinned/favorite tabs,
    /// else its `url` (`about:blank` when empty).
    pub(super) fn url_to_load(&self, id: Id) -> String {
        let Some(t) = self.tab(id) else { return "about:blank".into() };
        let pinned = matches!(self.tab_section(id), Some(Section::Pinned | Section::Favorites));
        let url = match (&t.pinned_url, pinned) {
            (Some(p), true) if !p.trim().is_empty() => p.clone(),
            _ => t.url.clone(),
        };
        if url.trim().is_empty() { "about:blank".into() } else { url }
    }

    /// Ensure the tab has a browser loading `url`: emits `CreateBrowser` (or defers it while a
    /// close is pending, or revives a download-kept browser with `LoadUrl`). No-op when live.
    pub(super) fn load_tab(&mut self, id: Id, url: String, fx: &mut Vec<Effect>) {
        if self.tab(id).is_none() {
            return;
        }
        if self.rt.deferred_destroy.remove(&id) {
            self.rt.visit_first_commit.remove(&id);
            let r = self.trt(id);
            if r.browser_url != url {
                r.browser_url = url.clone();
                fx.push(Effect::LoadUrl { tab: id, url: url.clone() });
            }
            self.set_tab_url(id, &url);
            self.bump();
            return;
        }
        if self.rt.closing.contains(&id) {
            self.rt.pending_create.insert(id, url.clone());
            self.set_tab_url(id, &url);
            self.bump();
            return;
        }
        if self.rt.tabs.get(&id).is_some_and(|r| r.loaded) {
            return;
        }
        let muted = self.tab(id).is_some_and(|t| t.muted);
        let internal = urls::is_internal(&url);
        let quiet = !self.rt.visit_first_commit.remove(&id);
        let r = self.trt(id);
        r.loaded = true;
        r.generation += 1;
        r.created = false;
        r.after_close = false;
        r.ignored_close_ticks = None;
        r.internal = internal;
        r.browser_url = url.clone();
        r.committed_url = None;
        r.quiet_first_commit = quiet;
        r.loading = false;
        r.progress = 0.0;
        r.crashed = false;
        r.load_error = None;
        r.failed_url = None;
        fx.push(Effect::CreateBrowser { tab: id, url: url.clone(), internal, muted });
        self.set_tab_url(id, &url);
        self.bump();
    }

    /// Destroy the tab's browser (if any). Deferred while one of its downloads is in progress.
    pub(super) fn release_browser(&mut self, id: Id, fx: &mut Vec<Effect>) {
        self.rt.pending_create.remove(&id);
        let loaded = self.rt.tabs.get(&id).is_some_and(|r| r.loaded);
        if !loaded || self.rt.closing.contains(&id) || self.rt.deferred_destroy.contains(&id) {
            if !loaded {
                // Never had a browser: drop runtime leftovers of removed tabs.
                if self.tab(id).is_none() {
                    self.rt.tabs.remove(&id);
                }
            }
            return;
        }
        {
            let r = self.trt(id);
            r.loading = false;
            r.progress = 0.0;
            r.audible = false;
            r.crashed = false;
            r.load_error = None;
            r.failed_url = None;
            r.can_go_back = false;
            r.can_go_forward = false;
            r.pending_typed = None;
            r.find_text.clear();
        }
        self.rt.permission_prompts.retain(|p| p.tab != id);
        if self.has_active_download(id) {
            self.rt.deferred_destroy.insert(id);
        } else {
            fx.push(Effect::DestroyBrowser { tab: id });
            self.rt.closing.insert(id);
        }
        self.bump();
    }

    pub(super) fn has_active_download(&self, tab: Id) -> bool {
        self.rt
            .downloads
            .iter()
            .any(|d| d.tab == Some(tab) && matches!(d.state, DownloadState::InProgress | DownloadState::Paused))
    }

    /// Drop runtime references to a tab that left the sidebar.
    pub(super) fn forget_tab(&mut self, id: Id) {
        let before = self.state.window.mru.len();
        self.state.window.mru.retain(|x| *x != id);
        if self.state.window.mru.len() != before {
            self.dirty.state = true;
        }
        if self.rt.switcher.as_ref().is_some_and(|(tabs, _)| tabs.contains(&id)) {
            self.rt.switcher = None;
            self.bump();
        }
        if self.rt.last_focused == Some(id) && self.tab(id).is_none() {
            self.rt.last_focused = None;
        }
    }

    // ------------------------------------------------------------------------------ activation

    /// Activate a tab or split (a pane id focuses that pane of its split). Switches space if
    /// needed (favorites stay in the current space), closes Peek, requests focus. Browsers are
    /// created and the layout shown by `reconcile`.
    pub(super) fn activate(&mut self, id: Id, now: Millis, fx: &mut Vec<Effect>) -> bool {
        let (item, pane) = match self.parent_of(id) {
            Some((Parent::Split(s), i)) => (s, Some(i)),
            Some(_) => (id, None),
            None => return false,
        };
        if !matches!(self.state.items.get(&item), Some(Item::Tab(_) | Item::Split(_))) {
            return false;
        }
        let space = match self.space_of(item) {
            Some(s) => s,
            None if self.state.favorites.contains(&item) => self.active_space_id(),
            None => return false,
        };
        if let (Some(i), Some(s)) = (pane, self.split_mut(item)) {
            s.focused = i;
        }
        self.close_peek(true, fx);
        self.state.window.active_space = space;
        if let Some(sp) = self.space_mut(space) {
            sp.active_item = Some(item);
        }
        for t in self.tabs_under(item) {
            if let Some(tab) = self.tab_item_mut(t) {
                tab.last_active_at = now;
            }
        }
        self.rt.focus_request = self.focused_of_item(item);
        self.touch();
        true
    }

    /// Close the Peek overlay (destroying its browser when `destroy`). A shown Peek is hidden
    /// (`HidePeek`) before its browser is destroyed.
    pub(super) fn close_peek(&mut self, destroy: bool, fx: &mut Vec<Effect>) {
        let Some(p) = self.rt.peek.take() else { return };
        if self.rt.emitted.peek == Some(p.tab.id) {
            fx.push(Effect::HidePeek { tab: p.tab.id });
            self.rt.emitted.peek = None;
        }
        if destroy {
            self.release_browser(p.tab.id, fx);
        }
        if !self.rt.tabs.get(&p.tab.id).is_some_and(|r| r.loaded) {
            self.rt.tabs.remove(&p.tab.id);
        }
        self.bump();
    }

    /// After the active item of `space` went away: activate the opener / MRU fallback (active
    /// space) or record it (other spaces).
    pub(super) fn fall_back(&mut self, space: Id, exclude: &[Id], opener: Option<Id>, now: Millis, fx: &mut Vec<Effect>) {
        let cand = self.fallback_item(space, exclude, opener);
        if space == self.active_space_id() {
            match cand {
                Some(c) => {
                    self.activate(c, now, fx);
                }
                None => {
                    if let Some(sp) = self.space_mut(space) {
                        sp.active_item = None;
                    }
                    self.touch();
                }
            }
        } else {
            let top = cand.map(|c| self.top_level_of(c));
            if let Some(sp) = self.space_mut(space) {
                sp.active_item = top;
            }
            self.touch();
        }
    }

    // ------------------------------------------------------------------------------ opening

    /// Insert a new Today tab: below `anchor` when the anchor is a Today tab/pane, else at the
    /// top of Today of the anchor's space (or the active space).
    pub(super) fn place_in_today(&mut self, id: Id, anchor: Option<Id>) {
        if let Some(a) = anchor {
            let top = self.top_level_of(a);
            match self.parent_of(top) {
                Some((Parent::Today(space), idx)) => {
                    self.insert_into(Parent::Today(space), idx + 1, id);
                    return;
                }
                Some(_) => {
                    let space = self.space_of(top).unwrap_or(self.active_space_id());
                    self.insert_into(Parent::Today(space), 0, id);
                    return;
                }
                None => {}
            }
        }
        let space = self.active_space_id();
        self.insert_into(Parent::Today(space), 0, id);
    }

    /// Create a tab item (not placed in any container).
    pub(super) fn new_tab_item(&mut self, url: &str, opener: Option<Id>, now: Millis) -> Id {
        let id = self.alloc_id();
        let tab = Tab { id, url: url.to_string(), created_at: now, last_active_at: now, opener, ..Tab::default() };
        self.state.items.insert(id, Item::Tab(tab));
        // Loading a newly opened URL is a history visit.
        self.rt.visit_first_commit.insert(id);
        self.touch();
        id
    }

    /// An open tab showing an internal page with the same host as `url`.
    fn find_internal_tab(&self, url: &str) -> Option<Id> {
        let host = url::Url::parse(url).ok()?.host_str()?.to_string();
        let mut candidates: Vec<Id> = self.state.favorites.clone();
        for s in &self.state.spaces {
            candidates.extend(s.pinned.iter().flat_map(|i| self.tabs_under(*i)));
            candidates.extend(s.today.iter().flat_map(|i| self.tabs_under(*i)));
        }
        candidates.into_iter().find(|t| {
            self.tab_item(*t).is_some_and(|tab| {
                urls::is_internal(&tab.url)
                    && url::Url::parse(&tab.url).ok().and_then(|u| u.host_str().map(str::to_string)).as_deref() == Some(host.as_str())
            })
        })
    }

    /// `OpenUrl` semantics (see [`Command::OpenUrl`]). Returns the tab that shows the URL.
    pub(super) fn open_url(&mut self, url: String, target: OpenTarget, opener: Option<Id>, typed: bool, now: Millis, fx: &mut Vec<Effect>) -> Option<Id> {
        let url = url.trim().to_string();
        if url.is_empty() || urls::scheme(&url).as_deref() == Some("javascript") {
            return None;
        }
        // `mailto:`, `tel:`, app protocols: the OS opens them; no tab is created or navigated.
        if urls::is_external_scheme(&url) {
            fx.push(Effect::OpenExternal { url });
            return None;
        }
        // The Chrome Web Store tells every browser that is not Chrome to "Switch to Chrome to
        // install extensions and themes", in a banner and a floating card, and at sta's default
        // window width its own layout pushes "Add to Chrome" off to the right. Both are wrong about
        // sta — installing from the store works — and "Get Extensions" is the only route sta offers
        // for getting one, so say it once per run rather than let the page have the last word.
        if urls::is_web_store_url(&url) && !std::mem::replace(&mut self.rt.web_store_noted, true) {
            self.toast(WEB_STORE_TOAST, None);
        }
        if urls::is_internal(&url)
            && let Some(existing) = self.find_internal_tab(&url)
        {
            if self.tab_item(existing).is_some_and(|t| t.url != url) {
                self.navigate_tab(existing, url.clone(), false, fx);
            }
            if target != OpenTarget::BackgroundTab {
                self.activate(existing, now, fx);
            }
            return Some(existing);
        }
        match target {
            OpenTarget::CurrentTab => match self.focused_tab() {
                Some(t) => {
                    self.navigate_tab(t, url, typed, fx);
                    Some(t)
                }
                None => self.open_url(url, OpenTarget::NewTab, opener, typed, now, fx),
            },
            OpenTarget::NewTab => {
                let opener = opener.filter(|o| self.tab(*o).is_some());
                let id = self.new_tab_item(&url, opener, now);
                // Below a Today opener (links), else top of Today.
                let anchor = opener.filter(|o| self.state.items.contains_key(o));
                self.place_in_today(id, anchor);
                if typed {
                    self.trt(id).pending_typed = Some(url);
                }
                self.activate(id, now, fx);
                Some(id)
            }
            OpenTarget::BackgroundTab => {
                let opener = opener.filter(|o| self.tab(*o).is_some());
                let id = self.new_tab_item(&url, opener, now);
                // Below the opener (links); without one (Alt+Enter) at the top of Today.
                let anchor = opener.filter(|o| self.state.items.contains_key(o));
                self.place_in_today(id, anchor);
                self.load_tab(id, url.clone(), fx);
                if typed {
                    self.trt(id).pending_typed = Some(url);
                }
                if !self.sidebar_shown() {
                    self.toast(
                        "New tab opened",
                        Some(ToastAction { label: "Show".into(), command: Box::new(Command::ActivateItem { id }) }),
                    );
                }
                Some(id)
            }
        }
    }

    /// Navigate an existing tab (items or Peek) to `url`, replacing the browser when the
    /// internal-ness changes.
    pub(super) fn navigate_tab(&mut self, id: Id, url: String, typed: bool, fx: &mut Vec<Effect>) {
        if self.tab(id).is_none() || url.trim().is_empty() {
            return;
        }
        if let std::collections::btree_map::Entry::Occupied(mut e) = self.rt.pending_create.entry(id) {
            e.insert(url.clone());
            self.rt.visit_first_commit.insert(id);
            self.set_tab_url(id, &url);
        } else if !self.is_live(id) {
            // A user navigation: the first commit of the browser that loads it is a visit.
            self.rt.visit_first_commit.insert(id);
            self.set_tab_url(id, &url);
            if self.layout_tab_ids().contains(&id) || self.peek_tab() == Some(id) || self.rt.deferred_destroy.contains(&id) {
                self.load_tab(id, url.clone(), fx);
            }
        } else {
            let internal = urls::is_internal(&url);
            let r = self.trt(id);
            r.load_error = None;
            r.failed_url = None;
            r.browser_url = url.clone();
            if r.internal != internal {
                r.internal = internal;
                r.committed_url = None;
                r.quiet_first_commit = false;
                self.close_devtools_before(id, fx);
                fx.push(Effect::ReplaceBrowser { tab: id, url: url.clone(), internal });
                self.set_tab_url(id, &url);
            } else {
                fx.push(Effect::LoadUrl { tab: id, url: url.clone() });
            }
        }
        if typed {
            self.trt(id).pending_typed = Some(url);
        }
        self.bump();
    }

    /// Copy of a tab as a new, unplaced Today tab (URL, title, favicon).
    pub(super) fn duplicate_tab_item(&mut self, src: Id, now: Millis) -> Option<Id> {
        let t = self.tab(src)?.clone();
        let url = if self.is_live(src) || t.pinned_url.is_none() { t.url.clone() } else { self.url_to_load(src) };
        let id = self.new_tab_item(&url, None, now);
        if let Some(tab) = self.tab_item_mut(id) {
            tab.title = t.title;
            tab.favicon = t.favicon;
            tab.muted = t.muted;
        }
        Some(id)
    }

    // ------------------------------------------------------------------------------ closing

    /// `CloseItem` semantics.
    pub(super) fn close_item(&mut self, id: Option<Id>, now: Millis, fx: &mut Vec<Effect>) {
        let target = match id {
            Some(i) => i,
            None => {
                if self.rt.peek.is_some() {
                    self.close_peek(true, fx);
                    self.rt.focus_request = self.content_focused_tab();
                    return;
                }
                match self.content_focused_tab() {
                    Some(t) => t,
                    None => {
                        self.request_quit(now, fx);
                        return;
                    }
                }
            }
        };
        if self.peek_tab() == Some(target) {
            self.close_peek(true, fx);
            self.rt.focus_request = self.content_focused_tab();
            return;
        }
        let Some((parent, _)) = self.parent_of(target) else { return };
        match self.state.items.get(&target) {
            Some(Item::Split(_)) => {
                let Parent::Today(space) = parent else { return };
                let was_active = self.space(space).and_then(|s| s.active_item) == Some(target);
                let panes = self.split_item(target).map(|s| s.panes.clone()).unwrap_or_default();
                let ids = self.archive_split(target, ArchiveReason::UserClosed, None, now, fx);
                if !ids.is_empty() {
                    self.push_reopen(ReopenEntry::Split { archive_ids: ids });
                }
                if was_active {
                    let mut exclude = panes;
                    exclude.push(target);
                    self.fall_back(space, &exclude, None, now, fx);
                }
            }
            Some(Item::Tab(tab)) => {
                let opener = tab.opener;
                match parent {
                    Parent::Favorites | Parent::Pinned(_) | Parent::Folder(_) => self.unload_pinned(target, true, now, fx),
                    Parent::Today(space) => {
                        let was_active = self.space(space).and_then(|s| s.active_item) == Some(target);
                        if self.archive_tab(target, ArchiveReason::UserClosed, None, now, fx).is_some() {
                            self.push_reopen(ReopenEntry::Archived { archive_id: target });
                        }
                        if was_active {
                            self.fall_back(space, &[target], opener, now, fx);
                        }
                    }
                    Parent::Split(sid) => {
                        let was_active = self.active_item() == Some(sid);
                        if self.archive_tab(target, ArchiveReason::UserClosed, None, now, fx).is_some() {
                            self.push_reopen(ReopenEntry::Archived { archive_id: target });
                        }
                        // The split (or the tab it dissolved into) stays active; focus its pane.
                        if was_active {
                            self.rt.focus_request = self.content_focused_tab();
                        }
                    }
                }
            }
            _ => {}
        }
    }

    /// Unload a pinned/favorite tab (Ctrl+W): destroy the browser, keep the row, reset the URL
    /// to `pinned_url`, remember the URL for Ctrl+Shift+T, fall back if it was showing.
    pub(super) fn unload_pinned(&mut self, id: Id, push_reopen: bool, now: Millis, fx: &mut Vec<Effect>) {
        let Some(tab) = self.tab_item(id).cloned() else { return };
        let active = self.active_space_id();
        let visible = self.layout_tab_ids().contains(&id) || self.active_item() == Some(self.top_level_of(id));
        let pinned_url = tab.pinned_url.clone().unwrap_or_else(|| tab.url.clone());
        let loaded = self.is_loaded(id) || self.rt.deferred_destroy.contains(&id);
        if !loaded && !visible && tab.url == pinned_url {
            return;
        }
        self.release_browser(id, fx);
        if push_reopen && loaded {
            self.push_reopen(ReopenEntry::Unloaded { tab: id, url: tab.url.clone() });
        }
        if let Some(t) = self.tab_item_mut(id) {
            t.url = pinned_url;
        }
        self.forget_tab(id);
        self.touch();
        if visible {
            let top = self.top_level_of(id);
            self.fall_back(active, &[id, top], tab.opener, now, fx);
        }
    }

    pub(super) fn push_reopen(&mut self, entry: ReopenEntry) {
        self.state.reopen.push(entry);
        if self.state.reopen.len() > MAX_REOPEN_STACK {
            let excess = self.state.reopen.len() - MAX_REOPEN_STACK;
            self.state.reopen.drain(..excess);
        }
        self.touch();
    }

    /// Request app shutdown: `[SaveNow, Quit]`, then ignore everything.
    pub(super) fn request_quit(&mut self, now: Millis, fx: &mut Vec<Effect>) {
        for t in self.layout_tab_ids() {
            if let Some(tab) = self.tab_item_mut(t) {
                tab.last_active_at = now;
            }
        }
        self.rt.shutting_down = true;
        fx.push(Effect::SaveNow);
        fx.push(Effect::Quit);
    }

    // ------------------------------------------------------------------------------ archive

    /// Archive one tab (any section, including a split pane). `index` overrides the recorded
    /// container index (batch operations record pre-removal positions). Returns the entry id.
    pub(super) fn archive_tab(&mut self, id: Id, reason: ArchiveReason, index: Option<usize>, now: Millis, fx: &mut Vec<Effect>) -> Option<Id> {
        let tab = self.tab_item(id)?.clone();
        let (parent, idx) = self.parent_of(id)?;
        let (space, section, folder, idx, split) = match parent {
            Parent::Favorites => (None, Section::Favorites, None, idx, None),
            Parent::Pinned(s) => (Some(s), Section::Pinned, None, idx, None),
            Parent::Folder(f) => (self.space_of(f), Section::Pinned, Some(f), idx, None),
            Parent::Today(s) => (Some(s), Section::Today, None, idx, None),
            Parent::Split(sid) => {
                let s = self.split_item(sid)?.clone();
                let (space, sidx) = match self.parent_of(sid) {
                    Some((Parent::Today(space), sidx)) => (Some(space), sidx),
                    _ => (None, 0),
                };
                let snap = SplitSnapshot {
                    group: sid,
                    orientation: s.orientation,
                    fractions: s.fractions.clone(),
                    focused: s.focused,
                    pane_index: idx,
                    panes: s.panes.clone(),
                };
                (space, Section::Today, None, sidx, Some(snap))
            }
        };
        let entry = ArchiveEntry {
            id,
            url: tab.url.clone(),
            title: tab.title.clone(),
            custom_title: tab.custom_title.clone(),
            favicon: tab.favicon.clone(),
            pinned_url: tab.pinned_url.clone(),
            archived_at: now,
            reason,
            space,
            section,
            folder,
            index: index.unwrap_or(idx),
            split,
        };
        self.unlink(id);
        if let Parent::Split(sid) = parent {
            self.dissolve_if_needed(sid);
        }
        self.release_browser(id, fx);
        self.state.items.remove(&id);
        self.forget_tab(id);
        self.state.archive.retain(|e| e.id != id);
        self.state.archive.insert(0, entry);
        self.touch_archive();
        Some(id)
    }

    /// Archive every pane of a split (shared snapshot). Returns entry ids in pane order.
    pub(super) fn archive_split(&mut self, sid: Id, reason: ArchiveReason, index: Option<usize>, now: Millis, fx: &mut Vec<Effect>) -> Vec<Id> {
        let Some(s) = self.split_item(sid).cloned() else { return Vec::new() };
        let Some((parent, sidx)) = self.parent_of(sid) else { return Vec::new() };
        let space = match parent {
            Parent::Today(space) => Some(space),
            _ => self.space_of(sid),
        };
        let mut entries = Vec::new();
        for (i, pane) in s.panes.iter().enumerate() {
            let Some(tab) = self.tab_item(*pane).cloned() else { continue };
            entries.push(ArchiveEntry {
                id: tab.id,
                url: tab.url,
                title: tab.title,
                custom_title: tab.custom_title,
                favicon: tab.favicon,
                pinned_url: None,
                archived_at: now,
                reason,
                space,
                section: Section::Today,
                folder: None,
                index: index.unwrap_or(sidx),
                split: Some(SplitSnapshot {
                    group: sid,
                    orientation: s.orientation,
                    fractions: s.fractions.clone(),
                    focused: s.focused,
                    pane_index: i,
                    panes: s.panes.clone(),
                }),
            });
        }
        self.unlink(sid);
        self.state.items.remove(&sid);
        for pane in &s.panes {
            self.release_browser(*pane, fx);
            self.state.items.remove(pane);
            self.forget_tab(*pane);
        }
        let ids: Vec<Id> = entries.iter().map(|e| e.id).collect();
        self.state.archive.retain(|e| !ids.contains(&e.id));
        for e in entries.into_iter().rev() {
            self.state.archive.insert(0, e);
        }
        self.touch_archive();
        ids
    }

    /// Archive a folder's tabs (recursively) and delete the folder and its subfolders.
    pub(super) fn archive_folder(&mut self, fid: Id, reason: ArchiveReason, now: Millis, fx: &mut Vec<Effect>) -> Vec<Id> {
        let Some(folder) = self.folder_item(fid).cloned() else { return Vec::new() };
        let mut ids = Vec::new();
        for child in folder.children.iter().rev() {
            match self.state.items.get(child) {
                Some(Item::Tab(_)) => ids.extend(self.archive_tab(*child, reason, None, now, fx)),
                Some(Item::Folder(_)) => ids.extend(self.archive_folder(*child, reason, now, fx)),
                Some(Item::Split(_)) => ids.extend(self.archive_split(*child, reason, None, now, fx)),
                None => {}
            }
        }
        self.unlink(fid);
        self.state.items.remove(&fid);
        self.touch();
        ids
    }

    /// Archive any top-level item.
    pub(super) fn archive_any(&mut self, id: Id, reason: ArchiveReason, index: Option<usize>, now: Millis, fx: &mut Vec<Effect>) -> Vec<Id> {
        match self.state.items.get(&id) {
            Some(Item::Tab(_)) => self.archive_tab(id, reason, index, now, fx).into_iter().collect(),
            Some(Item::Split(_)) => self.archive_split(id, reason, index, now, fx),
            Some(Item::Folder(_)) => self.archive_folder(id, reason, now, fx),
            None => Vec::new(),
        }
    }

    /// Auto-archive pass (arc_spec §2.8): idle Today tabs/splits that aren't visible or audible.
    pub(super) fn auto_archive(&mut self, now: Millis, fx: &mut Vec<Effect>) {
        let limit = self.state.settings.archive_after_hours.max(1) as Millis * 3600 * 1000;
        let visible = self.layout_tab_ids();
        let mut victims = Vec::new();
        for space in &self.state.spaces {
            for item in &space.today {
                let tabs = self.tabs_under(*item);
                if tabs.is_empty() || tabs.iter().any(|t| visible.contains(t) || self.rt.tabs.get(t).is_some_and(|r| r.audible)) {
                    continue;
                }
                let last = tabs
                    .iter()
                    .filter_map(|t| self.tab_item(*t))
                    .map(|t| t.last_active_at.max(t.created_at))
                    .max()
                    .unwrap_or(now);
                if now.saturating_sub(last) >= limit {
                    victims.push(*item);
                }
            }
        }
        for v in victims {
            self.archive_any(v, ArchiveReason::Auto, None, now, fx);
        }
    }

    // ------------------------------------------------------------------------------ restore

    fn id_is_free(&self, id: Id) -> bool {
        id != 0
            && !self.state.items.contains_key(&id)
            && !self.state.spaces.iter().any(|s| s.id == id)
            && !self.state.boosts.iter().any(|b| b.id == id)
            && self.peek_tab() != Some(id)
    }

    fn claim_id(&mut self, wanted: Id) -> Id {
        if wanted <= crate::MAX_ID && self.id_is_free(wanted) {
            if self.state.next_id <= wanted {
                self.state.next_id = wanted + 1;
            }
            wanted
        } else {
            self.alloc_id()
        }
    }

    fn tab_from_entry(&mut self, e: &ArchiveEntry, now: Millis) -> Tab {
        let id = self.claim_id(e.id);
        Tab {
            id,
            url: e.url.clone(),
            title: e.title.clone(),
            custom_title: e.custom_title.clone(),
            favicon: e.favicon.clone(),
            pinned_url: None,
            created_at: now,
            last_active_at: now,
            opener: None,
            muted: false,
        }
    }

    /// Restore archive entries (removing them). Entries sharing a split snapshot are rebuilt as
    /// a split. Returns what was restored in restore order: tab ids for single entries (possibly
    /// panes put back into their split), split ids for rebuilt groups.
    pub(super) fn restore_archived(&mut self, ids: &[Id], now: Millis) -> Vec<Id> {
        let mut entries: Vec<ArchiveEntry> = Vec::new();
        for id in ids {
            if let Some(pos) = self.state.archive.iter().position(|e| e.id == *id) {
                entries.push(self.state.archive.remove(pos));
            }
        }
        if entries.is_empty() {
            return Vec::new();
        }
        entries.sort_by_key(|e| (e.index, e.split.as_ref().map_or(0, |s| s.pane_index)));
        let mut restored = Vec::new();
        let mut done = vec![false; entries.len()];
        for i in 0..entries.len() {
            if done[i] {
                continue;
            }
            let group = entries[i].split.as_ref().map(|s| s.group);
            let members: Vec<usize> = match group {
                Some(g) => (i..entries.len()).filter(|j| !done[*j] && entries[*j].split.as_ref().map(|s| s.group) == Some(g)).collect(),
                None => vec![i],
            };
            if members.len() >= 2 && group.is_some_and(|g| self.split_item(g).is_none()) {
                let group_entries: Vec<ArchiveEntry> = members.iter().map(|j| entries[*j].clone()).collect();
                for j in &members {
                    done[*j] = true;
                }
                restored.extend(self.rebuild_split(&group_entries, now));
            } else {
                for j in members {
                    done[j] = true;
                    let e = entries[j].clone();
                    if let Some(id) = self.restore_single(&e, now) {
                        restored.push(id);
                    }
                }
            }
        }
        self.touch_archive();
        restored
    }

    fn restore_single(&mut self, e: &ArchiveEntry, now: Millis) -> Option<Id> {
        let mut tab = self.tab_from_entry(e, now);
        let id = tab.id;
        let space_ok = e.space.filter(|s| self.space(*s).is_some());
        let active = self.active_space_id();
        // A pane whose split dissolved: rejoin the pane it dissolved into, wherever it is now.
        if e.section == Section::Today
            && let (Some(snap), Some(space)) = (e.split.as_ref(), space_ok)
            && self.split_item(snap.group).is_none()
            && let Some(survivor) = self.dissolved_split_survivor(snap, space, e.index, e.id)
        {
            self.state.items.insert(id, Item::Tab(tab));
            self.rejoin_split(snap, space, survivor, id);
            self.touch();
            return Some(id);
        }
        let pinned_url = e.pinned_url.clone().unwrap_or_else(|| e.url.clone());
        let placement: (Parent, usize) = match e.section {
            Section::Favorites if self.state.favorites.len() < MAX_FAVORITES => {
                tab.pinned_url = Some(pinned_url);
                (Parent::Favorites, e.index)
            }
            Section::Pinned if space_ok.is_some() && e.folder.is_none_or(|f| self.folder_item(f).is_some()) => {
                tab.pinned_url = Some(pinned_url);
                match e.folder {
                    Some(f) => (Parent::Folder(f), e.index),
                    None => (Parent::Pinned(space_ok.unwrap_or(active)), e.index),
                }
            }
            Section::Today => match e.split.as_ref().filter(|s| self.split_item(s.group).is_some_and(|sp| sp.panes.len() < MAX_SPLIT_PANES)) {
                // Back between the panes it sat between (by id; the split may have changed since).
                Some(snap) => {
                    let current = self.split_item(snap.group).map(|s| s.panes.clone()).unwrap_or_default();
                    (Parent::Split(snap.group), position_among(&snap.panes, e.id, &current).unwrap_or(snap.pane_index))
                }
                None => match space_ok {
                    Some(s) => (Parent::Today(s), e.index),
                    None => (Parent::Today(active), 0),
                },
            },
            _ => (Parent::Today(space_ok.unwrap_or(active)), 0),
        };
        self.state.items.insert(id, Item::Tab(tab));
        if !self.insert_into(placement.0, placement.1, id) {
            self.insert_into(Parent::Today(active), 0, id);
        }
        self.touch();
        Some(id)
    }

    /// The plain Today tab of `space` a split dissolved into: one of the snapshot's other panes,
    /// found by id wherever it sits now. With several candidates, the one at the split's old
    /// `index` wins, then the pane nearest to the restored one in the snapshot. Snapshots from
    /// older profiles don't list their panes and never rejoin: an index alone can't tell the
    /// surviving pane from an unrelated tab.
    fn dissolved_split_survivor(&self, snap: &SplitSnapshot, space: Id, index: usize, restored: Id) -> Option<Id> {
        let today = &self.space(space)?.today;
        let candidates: Vec<(usize, usize, Id)> = today
            .iter()
            .enumerate()
            .filter(|(_, t)| **t != restored && self.tab_item(**t).is_some())
            .filter_map(|(i, t)| snap.panes.iter().position(|p| p == t).map(|pos| (i, pos, *t)))
            .collect();
        candidates
            .iter()
            .find(|(i, _, _)| *i == index)
            .or_else(|| candidates.iter().min_by_key(|(_, pos, _)| (pos.abs_diff(snap.pane_index), *pos)))
            .map(|(_, _, t)| *t)
    }

    /// Replace `survivor` (a plain Today tab of `space`) with a 2-pane split of it and the
    /// restored tab, reusing the snapshot's group id, orientation, relative order and sizes.
    fn rejoin_split(&mut self, snap: &SplitSnapshot, space: Id, survivor: Id, restored: Id) -> Id {
        let survivor_index = snap.panes.iter().position(|p| *p == survivor).unwrap_or(if snap.pane_index == 0 { 1 } else { 0 });
        let restored_first = snap.pane_index < survivor_index;
        let panes = if restored_first { vec![restored, survivor] } else { vec![survivor, restored] };
        let mut fractions = match (snap.fractions.get(snap.pane_index), snap.fractions.get(survivor_index)) {
            (Some(r), Some(s)) if restored_first => vec![*r, *s],
            (Some(r), Some(s)) => vec![*s, *r],
            _ => vec![0.5, 0.5],
        };
        normalize_fractions(&mut fractions, 0.0);
        let restored_focused = snap.focused == snap.pane_index;
        let focused = usize::from(restored_first != restored_focused);
        let sid = self.claim_id(snap.group);
        self.state.items.insert(sid, Item::Split(Split { id: sid, orientation: snap.orientation, panes, fractions, focused }));
        if let Some(sp) = self.space_mut(space)
            && let Some(slot) = sp.today.iter_mut().find(|t| **t == survivor)
        {
            *slot = sid;
        }
        for sp in &mut self.state.spaces {
            if sp.active_item == Some(survivor) {
                sp.active_item = Some(sid);
            }
        }
        sid
    }

    /// Rebuild a split from the archive entries of one group (at least two). The most recently
    /// archived entries (the split as it was last closed) become the panes, up to
    /// [`MAX_SPLIT_PANES`]; older entries of the same group (panes closed one by one before) that
    /// don't fit are restored as Today tabs right below the split. Returns the split id followed
    /// by those tab ids.
    ///
    /// Order: the most recent snapshot's pane list, with panes that only older snapshots know
    /// merged in next to the neighbors they had then, so the original left-to-right order is
    /// kept. Snapshots from older profiles (no pane list) fall back to recorded pane positions.
    fn rebuild_split(&mut self, group: &[ArchiveEntry], now: Millis) -> Vec<Id> {
        let pane_index = |e: &ArchiveEntry| e.split.as_ref().map_or(usize::MAX, |s| s.pane_index);
        let mut by_age: Vec<&ArchiveEntry> = group.iter().filter(|e| e.split.is_some()).collect();
        by_age.sort_by_key(|e| (std::cmp::Reverse(e.archived_at), pane_index(e), e.id));
        let Some(newest) = by_age.first().copied() else { return Vec::new() };
        let Some(snap) = newest.split.clone() else { return Vec::new() };
        let legacy = snap.panes.is_empty();
        let mut order: Vec<Id> = snap.panes.clone();
        if !legacy {
            for e in &by_age {
                let Some(s) = e.split.as_ref().filter(|_| !order.contains(&e.id)) else { continue };
                let at = position_among(&s.panes, e.id, &order).unwrap_or(order.len());
                order.insert(at, e.id);
            }
        }
        let rank = |e: &ArchiveEntry| if legacy { pane_index(e) } else { order.iter().position(|p| *p == e.id).unwrap_or(usize::MAX) };
        let fit = by_age.len().min(MAX_SPLIT_PANES);
        let mut pane_entries: Vec<&ArchiveEntry> = by_age[..fit].to_vec();
        let mut extra: Vec<&ArchiveEntry> = by_age[fit..].to_vec();
        // Stable sorts: legacy entries recorded at the same position (from different closes) keep
        // the newer one first; the split's pane positions are renumbered 0..n.
        pane_entries.sort_by_key(|e| rank(e));
        extra.sort_by_key(|e| rank(e));
        if pane_entries.len() < 2 {
            return Vec::new();
        }
        let entry_ids: Vec<Id> = pane_entries.iter().map(|e| e.id).collect();
        let active = self.active_space_id();
        let space = newest.space.filter(|s| self.space(*s).is_some()).unwrap_or(active);
        let mut panes = Vec::new();
        for e in &pane_entries {
            let tab = self.tab_from_entry(e, now);
            panes.push(tab.id);
            self.state.items.insert(tab.id, Item::Tab(tab));
        }
        // The snapshot's sizes only fit when the panes are exactly the ones it describes.
        let exact = if legacy { pane_entries.windows(2).all(|w| pane_index(w[0]) != pane_index(w[1])) } else { entry_ids == snap.panes };
        let mut fractions = vec![0.0; panes.len()];
        if exact && snap.fractions.len() == panes.len() {
            fractions.copy_from_slice(&snap.fractions);
        } else {
            equalize(&mut fractions);
        }
        normalize_fractions(&mut fractions, 0.0);
        let focused = if legacy {
            pane_entries.iter().position(|e| e.archived_at == newest.archived_at && pane_index(e) == snap.focused)
        } else {
            snap.panes.get(snap.focused).and_then(|f| entry_ids.iter().position(|id| id == f))
        }
        .unwrap_or(snap.focused.min(panes.len() - 1));
        let sid = self.claim_id(snap.group);
        self.state.items.insert(sid, Item::Split(Split { id: sid, orientation: snap.orientation, panes, fractions, focused }));
        let index = if newest.space == Some(space) { newest.index } else { 0 };
        let at = index.min(self.space(space).map_or(0, |s| s.today.len()));
        self.insert_into(Parent::Today(space), at, sid);
        let mut restored = vec![sid];
        for (k, e) in extra.iter().enumerate() {
            let tab = self.tab_from_entry(e, now);
            let id = tab.id;
            self.state.items.insert(id, Item::Tab(tab));
            self.insert_into(Parent::Today(space), at + 1 + k, id);
            restored.push(id);
        }
        self.touch();
        restored
    }

    /// Whether `ReopenClosed` would restore something (entries whose archive entries or pinned
    /// tabs are gone are skipped by it).
    pub(super) fn can_reopen(&self) -> bool {
        let archived = |id: &Id| self.state.archive.iter().any(|e| e.id == *id);
        self.state.reopen.iter().any(|entry| match entry {
            ReopenEntry::Archived { archive_id } => archived(archive_id),
            ReopenEntry::Unloaded { tab, .. } => {
                self.tab_item(*tab).is_some() && matches!(self.parent_of(*tab), Some((Parent::Favorites | Parent::Pinned(_) | Parent::Folder(_), _)))
            }
            ReopenEntry::Batch { archive_ids } | ReopenEntry::Split { archive_ids } => archive_ids.iter().any(archived),
        })
    }

    /// `ReopenClosed`: pop the stack until an entry can be restored.
    pub(super) fn reopen_closed(&mut self, now: Millis, fx: &mut Vec<Effect>) {
        while let Some(entry) = self.state.reopen.pop() {
            self.touch();
            match entry {
                ReopenEntry::Archived { archive_id } => {
                    if !self.state.archive.iter().any(|e| e.id == archive_id) {
                        continue;
                    }
                    if let Some(first) = self.restore_archived(&[archive_id], now).first().copied() {
                        self.activate(first, now, fx);
                    }
                    return;
                }
                ReopenEntry::Unloaded { tab, url } => {
                    let pinned = matches!(self.parent_of(tab), Some((Parent::Favorites | Parent::Pinned(_) | Parent::Folder(_), _)));
                    if !pinned || self.tab_item(tab).is_none() {
                        continue;
                    }
                    if !self.is_live(tab) && !self.rt.pending_create.contains_key(&tab) {
                        self.load_tab(tab, url, fx);
                    }
                    self.activate(tab, now, fx);
                    return;
                }
                ReopenEntry::Batch { archive_ids } => {
                    if !archive_ids.iter().any(|id| self.state.archive.iter().any(|e| e.id == *id)) {
                        continue;
                    }
                    self.restore_archived(&archive_ids, now);
                    return;
                }
                ReopenEntry::Split { archive_ids } => {
                    if !archive_ids.iter().any(|id| self.state.archive.iter().any(|e| e.id == *id)) {
                        continue;
                    }
                    if let Some(first) = self.restore_archived(&archive_ids, now).first().copied() {
                        self.activate(first, now, fx);
                    }
                    return;
                }
            }
        }
    }

    // ------------------------------------------------------------------------------ moving

    /// Move an item into `dest` at `index` (end when `None`), converting tabs between sections
    /// (`pinned_url` set into Favorites/Pinned, cleared into Today). A pane leaves its split
    /// (which may dissolve). No validation of the DnD matrix (see `move_item_checked`).
    pub(super) fn move_to(&mut self, id: Id, dest: Parent, index: Option<usize>) -> bool {
        if self.container(dest).is_none() {
            return false;
        }
        let Some((src, _)) = self.parent_of(id) else { return false };
        self.unlink(id);
        if let Parent::Split(sid) = src {
            self.dissolve_if_needed(sid);
        }
        let len = self.container(dest).map_or(0, |c| c.len());
        self.insert_into(dest, index.unwrap_or(len).min(len), id);
        if let Some(Item::Tab(t)) = self.state.items.get_mut(&id) {
            match dest {
                Parent::Favorites | Parent::Pinned(_) | Parent::Folder(_) => {
                    if t.pinned_url.as_deref().is_none_or(|p| p.trim().is_empty()) {
                        t.pinned_url = Some(t.url.clone());
                    }
                }
                Parent::Today(_) | Parent::Split(_) => t.pinned_url = None,
            }
        }
        self.touch();
        true
    }
}

/// Where `id` goes in `current` so it keeps its place from `snapshot` (an older ordering that
/// contains it): right after the nearest earlier snapshot neighbor present in `current`, else right
/// before the nearest later one. `None` when `id` isn't in `snapshot` or no neighbor is present.
fn position_among(snapshot: &[Id], id: Id, current: &[Id]) -> Option<usize> {
    let at = snapshot.iter().position(|p| *p == id)?;
    let index_in_current = |p: &Id| current.iter().position(|c| c == p);
    snapshot[..at]
        .iter()
        .rev()
        .find_map(|p| index_in_current(p).map(|i| i + 1))
        .or_else(|| snapshot[at + 1..].iter().find_map(index_in_current))
}
