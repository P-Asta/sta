//! `Command` dispatch and the handlers that don't belong to a more specific module.

use super::tree::normalize_fractions;
use super::*;
use crate::omnibox::{classify, resolve_input, Classified};
use crate::urls;

impl Store {
    pub(super) fn handle(&mut self, cmd: Command, now: Millis, fx: &mut Vec<Effect>) {
        match cmd {
            // -------------------------------------------------------------- opening & navigation
            Command::OpenInput { text, target } => {
                self.open_input(&text, target, now, fx);
            }
            Command::OpenUrl { url, target, opener } => {
                let typed = self.rt.omnibox_commit;
                self.open_url(url, target, opener, typed, now, fx);
            }
            Command::OpenUrlAt { url, to } => self.open_url_at(url, to, now, fx),
            Command::Navigate { tab, url } => {
                if urls::is_external_scheme(&url) {
                    fx.push(Effect::OpenExternal { url: url.trim().to_string() });
                    return;
                }
                if let Some(t) = tab.or(self.focused_tab()) {
                    let typed = self.rt.omnibox_commit;
                    self.navigate_tab(t, url, typed, fx);
                }
            }
            Command::GoBack { tab } => {
                if let Some(t) = self.live_target(tab) {
                    fx.push(Effect::GoBack { tab: t });
                }
            }
            Command::GoForward { tab } => {
                if let Some(t) = self.live_target(tab) {
                    fx.push(Effect::GoForward { tab: t });
                }
            }
            Command::Reload { tab, ignore_cache } => {
                if let Some(t) = self.live_target(tab) {
                    let r = self.trt(t);
                    r.crashed = false;
                    match r.failed_url.take() {
                        Some(url) => {
                            r.load_error = None;
                            r.browser_url = url.clone();
                            fx.push(Effect::LoadUrl { tab: t, url });
                        }
                        None => fx.push(Effect::Reload { tab: t, ignore_cache }),
                    }
                    self.bump();
                }
            }
            Command::StopLoad { tab } => {
                if let Some(t) = self.live_target(tab) {
                    fx.push(Effect::StopLoad { tab: t });
                }
            }

            // -------------------------------------------------------------- sidebar items
            Command::ActivateItem { id } => {
                let already = self.active_item() == Some(self.top_level_of(id)) && self.peek_tab().is_none();
                if self.activate(id, now, fx) && already {
                    self.rt.focus_request = self.content_focused_tab();
                }
            }
            Command::ActivateNth { n } => {
                if n == 0 {
                    return;
                }
                let order = self.visual_order();
                let target = if n >= 9 { order.last() } else { order.get(n as usize - 1) };
                if let Some(t) = target.copied() {
                    self.activate(t, now, fx);
                }
            }
            Command::ActivateAdjacent { delta } => {
                let order = self.visual_order();
                if order.is_empty() || delta == 0 {
                    return;
                }
                let target = match self.active_item().and_then(|a| order.iter().position(|x| *x == a)) {
                    Some(i) => {
                        let j = i as i64 + delta.signum() as i64;
                        (j >= 0 && (j as usize) < order.len()).then(|| order[j as usize])
                    }
                    None => Some(if delta > 0 { order[0] } else { order[order.len() - 1] }),
                };
                if let Some(t) = target {
                    self.activate(t, now, fx);
                }
            }
            Command::CloseItem { id } => self.close_item(id, now, fx),
            Command::ReopenClosed => self.reopen_closed(now, fx),
            Command::TogglePin { id } => self.toggle_pin(id),
            Command::AddFavorite { id } => {
                let Some(t) = id.or(self.content_focused_tab()) else { return };
                if self.tab_item(t).is_none() {
                    return;
                }
                match self.parent_of(t) {
                    Some((Parent::Favorites | Parent::Split(_), _)) | None => {}
                    Some(_) => {
                        if self.state.favorites.len() >= MAX_FAVORITES {
                            self.toast(format!("Favorites are full ({MAX_FAVORITES})"), None);
                        } else {
                            self.move_to(t, Parent::Favorites, None);
                        }
                    }
                }
            }
            Command::RemoveFavorite { id } => {
                if self.state.favorites.contains(&id) {
                    let space = self.active_space_id();
                    self.move_to(id, Parent::Today(space), Some(0));
                }
            }
            Command::ResetToPinned { id } => {
                if !matches!(self.tab_section(id), Some(Section::Pinned | Section::Favorites)) {
                    return;
                }
                let Some(pinned) = self.tab_item(id).and_then(|t| t.pinned_url.clone()) else { return };
                if self.is_live(id) {
                    if self.tab_item(id).is_some_and(|t| t.url != pinned) || self.rt.tabs.get(&id).is_some_and(|r| r.failed_url.is_some()) {
                        self.navigate_tab(id, pinned, false, fx);
                    }
                } else {
                    self.set_tab_url(id, &pinned);
                }
                self.activate(id, now, fx);
            }
            Command::ReplacePinnedUrl { id } => {
                if !matches!(self.tab_section(id), Some(Section::Pinned | Section::Favorites)) {
                    return;
                }
                if let Some(t) = self.tab_item_mut(id)
                    && t.pinned_url.as_deref() != Some(t.url.as_str())
                {
                    t.pinned_url = Some(t.url.clone());
                    self.touch();
                }
            }
            Command::EditPinned { id, title, url } => {
                self.close_panel(&SidebarPanel::EditPinned { id });
                if !matches!(self.tab_section(id), Some(Section::Pinned | Section::Favorites)) {
                    return;
                }
                let engine = self.state.settings.search_engine;
                let custom = self.state.settings.custom_search_url.clone();
                let new_url = url.filter(|u| !u.trim().is_empty()).map(|u| resolve_input(&u, engine, &custom));
                let live = self.is_live(id);
                if let Some(t) = self.tab_item_mut(id) {
                    if let Some(title) = title {
                        let title = title.trim();
                        t.custom_title = (!title.is_empty()).then(|| title.to_string());
                    }
                    if let Some(u) = new_url {
                        if !live {
                            t.url = u.clone();
                        }
                        t.pinned_url = Some(u);
                    }
                    self.touch();
                }
            }
            Command::RenameItem { id, title } => {
                let title = title.map(|t| t.trim().to_string()).filter(|t| !t.is_empty());
                match self.state.items.get_mut(&id) {
                    Some(Item::Tab(t)) => {
                        t.custom_title = title;
                    }
                    Some(Item::Folder(f)) => match title {
                        Some(name) => f.name = name,
                        None => {
                            self.close_panel(&SidebarPanel::RenameItem { id });
                            return;
                        }
                    },
                    _ => return,
                }
                self.close_panel(&SidebarPanel::RenameItem { id });
                self.touch();
            }
            Command::DuplicateTab { id } => {
                let Some(src) = id.or(self.focused_tab()) else { return };
                let Some(dup) = self.duplicate_tab_item(src, now) else { return };
                let anchor = self.state.items.contains_key(&src).then_some(src);
                match anchor.map(|a| self.parent_of(self.top_level_of(a))) {
                    Some(Some((Parent::Today(_), _))) => self.place_in_today(dup, anchor),
                    Some(Some(_)) => {
                        let space = self.space_of(src).unwrap_or(self.active_space_id());
                        self.insert_into(Parent::Today(space), 0, dup);
                    }
                    _ => self.place_in_today(dup, None),
                }
                self.activate(dup, now, fx);
            }
            Command::MoveItem { id, to } => {
                self.move_item_checked(id, to);
            }
            Command::MoveToSpace { id, space } => self.move_to_space(id, space),
            Command::ClearToday { space } => self.clear_today(space, now, fx),
            Command::NewFolder { space, parent, name } => self.new_folder(space, parent, name),
            Command::ToggleFolder { id } => {
                if let Some(f) = self.folder_mut(id) {
                    f.collapsed = !f.collapsed;
                    self.touch();
                }
            }
            Command::DeleteFolder { id } => {
                let Some(name) = self.folder_item(id).map(|f| f.name.clone()) else { return };
                let archived = self.archive_folder(id, ArchiveReason::FolderDeleted, now, fx);
                if !archived.is_empty() {
                    let n = archived.len();
                    self.toast(format!("Deleted “{name}” · {n} {} archived", if n == 1 { "tab" } else { "tabs" }), None);
                }
            }
            Command::UnloadTab { id } => {
                if self.tab_item(id).is_none() || !(self.is_loaded(id) || self.rt.deferred_destroy.contains(&id)) {
                    return;
                }
                let visible = self.layout_tab_ids().contains(&id);
                match self.parent_of(id) {
                    Some((Parent::Favorites | Parent::Pinned(_) | Parent::Folder(_), _)) => self.unload_pinned(id, false, now, fx),
                    Some(_) => {
                        self.release_browser(id, fx);
                        if visible {
                            let top = self.top_level_of(id);
                            let mut exclude = self.tabs_under(top);
                            exclude.push(top);
                            let space = self.active_space_id();
                            self.fall_back(space, &exclude, None, now, fx);
                        }
                    }
                    None => {}
                }
            }
            Command::ToggleMute { id } => {
                let Some(t) = id.or(self.focused_tab()) else { return };
                let persisted = self.state.items.contains_key(&t);
                let Some(tab) = self.tab_any_mut(t) else { return };
                tab.muted = !tab.muted;
                let muted = tab.muted;
                if persisted {
                    self.touch();
                } else {
                    self.bump();
                }
                if self.is_live(t) {
                    fx.push(Effect::SetAudioMuted { tab: t, muted });
                }
            }
            Command::CopyUrl { id, markdown } => self.copy_url(id, markdown, fx),
            Command::CopyText { text } => {
                fx.push(Effect::CopyToClipboard { text });
                self.toast("Copied", None);
            }

            // -------------------------------------------------------------- updates
            // The shell decides what is possible and reports it back (`UpdateStatusChanged`); these
            // only ask, and only when the status says the ask makes sense — a page cannot start a
            // download by sending `downloadUpdate` twice, or install what is not staged.
            Command::CheckForUpdate => fx.push(Effect::CheckForUpdate),
            Command::DownloadUpdate => {
                if self.rt.update.can_download() {
                    fx.push(Effect::DownloadUpdate);
                }
            }
            Command::InstallUpdate => {
                if self.rt.update.is_ready() {
                    fx.push(Effect::InstallUpdate);
                }
            }

            // -------------------------------------------------------------- spaces
            Command::NewSpace { name, icon, theme } => {
                let id = self.alloc_id();
                let name = name.trim();
                let icon = icon.trim();
                self.state.spaces.push(Space {
                    id,
                    name: if name.is_empty() { format!("Space {}", self.state.spaces.len() + 1) } else { name.to_string() },
                    icon: if icon.is_empty() { "✨".into() } else { icon.to_string() },
                    theme: crate::theme::sanitize_theme(&theme),
                    created_at: now,
                    ..Space::default()
                });
                if matches!(self.rt.sidebar_panel.as_ref().map(|p| &p.panel), Some(SidebarPanel::NewSpace)) {
                    self.rt.sidebar_panel = None;
                }
                self.switch_space(id, fx);
                self.touch();
            }
            Command::UpdateSpace { id, name, icon, theme } => {
                let Some(space) = self.space_mut(id) else { return };
                if let Some(n) = name.map(|n| n.trim().to_string()).filter(|n| !n.is_empty()) {
                    space.name = n;
                }
                if let Some(i) = icon.map(|i| i.trim().to_string()).filter(|i| !i.is_empty()) {
                    space.icon = i;
                }
                if let Some(t) = theme {
                    space.theme = crate::theme::sanitize_theme(&t);
                }
                self.touch();
            }
            Command::DeleteSpace { id } => self.delete_space(id, now, fx),
            Command::SwitchSpace { id } => self.switch_space(id, fx),
            Command::SwitchSpaceNth { n } => {
                if n >= 1
                    && let Some(id) = self.state.spaces.get(n as usize - 1).map(|s| s.id)
                {
                    self.switch_space(id, fx);
                }
            }
            Command::SwitchSpaceAdjacent { delta } => {
                let Some(i) = self.state.spaces.iter().position(|s| s.id == self.active_space_id()) else { return };
                let j = i as i64 + delta.signum() as i64;
                if delta != 0 && j >= 0 && (j as usize) < self.state.spaces.len() {
                    let id = self.state.spaces[j as usize].id;
                    self.switch_space(id, fx);
                }
            }
            Command::MoveSpace { id, index } => {
                let Some(i) = self.state.spaces.iter().position(|s| s.id == id) else { return };
                let space = self.state.spaces.remove(i);
                let j = index.min(self.state.spaces.len());
                self.state.spaces.insert(j, space);
                if i != j {
                    self.touch();
                }
            }

            // -------------------------------------------------------------- split view
            Command::SplitWith { tab, with, side } => {
                self.split_with(tab, with, side, now, fx);
            }
            Command::SplitOpenInput { text, side } => self.split_open_input(&text, side, now, fx),
            Command::FocusPane { index } => self.focus_pane(|_, _| Some(index)),
            Command::FocusPaneAdjacent { delta } => self.focus_pane(|cur, len| {
                let j = cur as i64 + delta.signum() as i64;
                (delta != 0 && j >= 0 && (j as usize) < len).then_some(j as usize)
            }),
            Command::SetSplitFractions { id, fractions } => {
                let sid = self.top_level_of(id);
                let Some(s) = self.split_item(sid) else { return };
                if fractions.len() != s.panes.len() || fractions.iter().any(|f| !f.is_finite()) {
                    return;
                }
                let mut f: Vec<f32> = fractions.iter().map(|v| v.max(0.0)).collect();
                normalize_fractions(&mut f, MIN_PANE_FRACTION);
                if let Some(s) = self.split_mut(sid)
                    && s.fractions != f
                {
                    s.fractions = f;
                    self.touch();
                }
            }
            Command::SeparatePane { tab } => self.separate_pane(tab),
            Command::SeparateAll { id } => self.separate_all(id),

            // -------------------------------------------------------------- archive & history
            Command::RestoreArchived { id, whole_group } => {
                let Some(entry) = self.state.archive.iter().find(|e| e.id == id).cloned() else { return };
                let ids: Vec<Id> = match (&entry.split, whole_group) {
                    (Some(snap), true) => self
                        .state
                        .archive
                        .iter()
                        .filter(|e| e.split.as_ref().is_some_and(|s| s.group == snap.group))
                        .map(|e| e.id)
                        .collect(),
                    _ => vec![id],
                };
                if let Some(first) = self.restore_archived(&ids, now).first().copied() {
                    self.activate(first, now, fx);
                }
            }
            Command::DeleteArchived { id } => {
                let before = self.state.archive.len();
                self.state.archive.retain(|e| e.id != id);
                if self.state.archive.len() != before {
                    self.touch_archive();
                }
            }
            Command::ClearArchive => {
                if !self.state.archive.is_empty() {
                    self.state.archive.clear();
                    self.touch_archive();
                }
            }
            Command::DeleteHistoryEntry { url } => {
                if self.history.get(&url).is_some() {
                    self.history.remove(&url);
                    self.touch_history();
                }
            }
            Command::ClearHistory => {
                if !self.history.urls.is_empty() {
                    self.history.clear();
                    self.touch_history();
                }
            }

            // -------------------------------------------------------------- peek
            Command::ClosePeek { focus_lost } => {
                let Some(popup) = self.rt.peek.as_ref().map(|p| p.popup) else { return };
                // Click outside is ignored while something that took focus on purpose is showing: the
                // command bar, the find bar, or a permission prompt for the Peek / a visible tab.
                if focus_lost && (popup || self.rt.command_bar.is_some() || self.rt.find.is_some() || self.shown_prompt().is_some()) {
                    return;
                }
                self.close_peek(true, fx);
                if !focus_lost {
                    self.rt.focus_request = self.content_focused_tab();
                }
            }
            Command::ExpandPeek { split } => self.expand_peek(split, now, fx),

            // -------------------------------------------------------------- command bar
            Command::OpenCommandBar { mode, split_side } => {
                let focused = self.focused_tab();
                let (mode, text) = match mode {
                    CommandBarMode::EditUrl => match focused.and_then(|t| self.tab(t)) {
                        Some(t) => (CommandBarMode::EditUrl, t.url.clone()),
                        None => (CommandBarMode::NewTab, String::new()),
                    },
                    other => (other, String::new()),
                };
                let split_side = match mode {
                    CommandBarMode::Split => Some(split_side.unwrap_or(SplitSide::Right)),
                    _ => split_side,
                };
                let seq = self.next_seq();
                self.rt.command_bar = Some(CommandBarView { mode, text, split_side, seq });
                self.rt.switcher = None;
                // The picker shows what is installed *now*: an extension the user turned off in
                // another window, or one that finished installing, must not linger in the list.
                if mode == CommandBarMode::Extensions {
                    fx.push(Effect::RefreshExtensions);
                }
                self.bump();
            }
            Command::CloseCommandBar { seq } => {
                // A stale close: the page asked to close the bar it was showing, but that bar is
                // gone and a newer one is open (Esc then Ctrl+T within one push interval).
                if let Some(seq) = seq
                    && self.rt.command_bar.as_ref().is_some_and(|c| c.seq != seq)
                {
                    return;
                }
                if self.rt.command_bar.take().is_some() {
                    self.rt.focus_request = self.focused_tab();
                    self.bump();
                }
            }
            Command::CommitOmnibox { command, alt } => {
                if matches!(*command, Command::CommitOmnibox { .. }) || !command.allowed_from_ui() {
                    return;
                }
                let bar_seq = self.rt.command_bar.as_ref().map(|c| c.seq);
                let panel_seq = self.rt.sidebar_panel.as_ref().map(|p| p.seq);
                let prev = std::mem::replace(&mut self.rt.omnibox_commit, true);
                self.handle(*command, now, fx);
                self.rt.omnibox_commit = prev;
                if alt || self.rt.shutting_down {
                    return;
                }
                let new_bar = self.rt.command_bar.as_ref().map(|c| c.seq);
                let reopened = new_bar.is_some() && new_bar != bar_seq;
                let panel_opened = self.rt.sidebar_panel.is_some() && self.rt.sidebar_panel.as_ref().map(|p| p.seq) != panel_seq;
                if reopened || panel_opened {
                    return;
                }
                if self.rt.command_bar.take().is_some() {
                    if self.rt.focus_request.is_none() {
                        self.rt.focus_request = self.focused_tab();
                    }
                    self.bump();
                }
            }

            // -------------------------------------------------------------- chrome & surfaces
            Command::ToggleSidebar => {
                if self.state.window.sidebar_visible {
                    // Hiding the docked sidebar closes its panel.
                    self.rt.sidebar_panel = None;
                    self.rt.sidebar_revealed = false;
                    self.state.window.sidebar_visible = false;
                } else {
                    // Dock a hidden sidebar, also one floating (hover reveal, transient panel) or
                    // revealed for a panel: that panel stays open, now in the docked sidebar.
                    self.state.window.sidebar_visible = true;
                    self.rt.sidebar_revealed = false;
                }
                self.touch();
            }
            Command::SetSidebarWidth { width } => {
                let w = width.clamp(SIDEBAR_MIN_WIDTH, SIDEBAR_MAX_WIDTH);
                if w != self.state.window.sidebar_width {
                    self.state.window.sidebar_width = w;
                    self.touch();
                } else if w != width {
                    // The live drag applied an out-of-range width: re-send the clamped one.
                    self.rt.emitted.sidebar = None;
                }
            }
            Command::OpenSidebarPanel { panel } => self.open_sidebar_panel(panel),
            Command::ToggleSidebarPanel { panel } => {
                if self.rt.sidebar_panel.as_ref().is_some_and(|p| p.panel == panel) {
                    self.rt.sidebar_panel = None;
                    self.bump();
                } else {
                    self.open_sidebar_panel(panel);
                }
            }
            Command::CloseSidebarPanel => {
                if self.rt.sidebar_panel.take().is_some() {
                    self.bump();
                }
            }
            Command::OpenInternalPage { page } => {
                let typed = self.rt.omnibox_commit;
                self.open_url(page.url().to_string(), OpenTarget::NewTab, None, typed, now, fx);
            }
            Command::OpenFind => {
                let Some(t) = self.focused_tab().filter(|t| self.is_live(*t)) else { return };
                let (text, match_case) = {
                    let r = self.trt(t);
                    (r.find_text.clone(), r.find_match_case)
                };
                let seq = self.next_seq();
                self.rt.find = Some(FindView { tab: t, text, match_case, seq });
                self.bump();
            }
            Command::CloseFind => {
                if let Some(f) = self.rt.find.take() {
                    if self.is_live(f.tab) {
                        fx.push(Effect::StopFinding { tab: f.tab });
                    }
                    self.rt.focus_request = Some(f.tab);
                    self.bump();
                }
            }
            Command::FindInPage { tab, text, forward, match_case, find_next } => {
                let target = tab.or(self.rt.find.as_ref().map(|f| f.tab)).or(self.focused_tab());
                let Some(t) = target.filter(|t| self.is_live(*t)) else { return };
                {
                    let r = self.trt(t);
                    r.find_text = text.clone();
                    r.find_match_case = match_case;
                }
                if let Some(f) = self.rt.find.as_mut().filter(|f| f.tab == t)
                    && (f.text != text || f.match_case != match_case)
                {
                    f.text = text.clone();
                    f.match_case = match_case;
                    self.bump();
                }
                if text.is_empty() {
                    fx.push(Effect::StopFinding { tab: t });
                } else {
                    fx.push(Effect::Find { tab: t, text, forward, match_case, find_next });
                }
            }
            Command::FindNext { forward } => {
                let target = self.rt.find.as_ref().map(|f| f.tab).or(self.focused_tab());
                let Some(t) = target.filter(|t| self.is_live(*t)) else { return };
                let (text, match_case) = {
                    let r = self.trt(t);
                    (r.find_text.clone(), r.find_match_case)
                };
                if !text.is_empty() {
                    fx.push(Effect::Find { tab: t, text, forward, match_case, find_next: true });
                }
            }
            Command::Zoom { direction } => {
                if let Some(t) = self.live_target(None) {
                    self.trt(t).zoom_toast = true;
                    fx.push(Effect::Zoom { tab: t, direction });
                }
            }
            Command::Print => {
                if let Some(t) = self.live_target(None) {
                    fx.push(Effect::Print { tab: t });
                }
            }
            Command::ViewSource => {
                let Some(src) = self.focused_tab() else { return };
                let Some(url) = self.tab(src).map(|t| t.url.clone()) else { return };
                if urls::is_internal(&url) || urls::scheme(&url).is_none_or(|s| s == "view-source" || s == "about" || s == "data") {
                    return;
                }
                let id = self.new_tab_item(&format!("view-source:{url}"), None, now);
                let anchor = self.state.items.contains_key(&src).then_some(src);
                self.place_in_today(id, anchor);
                self.activate(id, now, fx);
            }
            Command::NewBoostForSite { tab } => {
                let Some(t) = tab.or(self.focused_tab()) else { return };
                let Some(host) = self.tab(t).and_then(|t| urls::host(&t.url)) else { return };
                let host = host.strip_prefix("www.").map(str::to_string).unwrap_or(host);
                let id = self.alloc_id();
                self.state.boosts.push(Boost { id, name: host.clone(), host, enabled: true, css: String::new(), js: String::new(), created_at: now, updated_at: now });
                self.touch();
                self.open_url(format!("sta://boosts/?id={id}"), OpenTarget::NewTab, None, false, now, fx);
            }
            Command::WindowControl { action } => match action {
                WindowAction::Close => self.request_quit(now, fx),
                other => fx.push(Effect::Window { action: other }),
            },
            Command::Quit | Command::WindowCloseRequested => self.request_quit(now, fx),
            Command::DismissToast { id } => {
                if self.rt.toast.as_ref().is_some_and(|t| t.id == id) {
                    self.rt.toast = None;
                    self.bump();
                }
            }

            // -------------------------------------------------------------- settings & boosts
            Command::UpdateSettings { patch } => self.update_settings(patch),
            Command::UpsertBoost { boost } => self.upsert_boost(boost, now, fx),
            Command::DeleteBoost { id } => {
                let Some(pos) = self.state.boosts.iter().position(|b| b.id == id) else { return };
                let b = self.state.boosts.remove(pos);
                self.reload_boosted(&[b.host], fx);
                self.touch();
            }
            Command::ToggleBoost { id } => {
                let Some(b) = self.state.boosts.iter_mut().find(|b| b.id == id) else { return };
                b.enabled = !b.enabled;
                b.updated_at = now;
                let host = b.host.clone();
                self.reload_boosted(&[host], fx);
                self.touch();
            }
            Command::ResolvePermission { id, allow, remember } => {
                let Some(pos) = self.rt.permission_prompts.iter().position(|p| p.id == id) else { return };
                let p = self.rt.permission_prompts.remove(pos);
                fx.push(Effect::AnswerPermission { id, allow, remember });
                if remember {
                    for kind in &p.kinds {
                        self.state.site_permissions.retain(|s| !(s.origin == p.origin && s.kind == *kind));
                        self.state.site_permissions.push(SitePermission { origin: p.origin.clone(), kind: *kind, allow });
                    }
                    self.touch();
                }
                self.bump();
            }

            // -------------------------------------------------------------- switcher
            Command::MruStep { forward } => {
                if let Some((tabs, sel)) = self.rt.switcher.as_mut() {
                    let n = tabs.len();
                    *sel = if forward { (*sel + 1) % n } else { (*sel + n - 1) % n };
                    self.bump();
                    return;
                }
                let tabs: Vec<Id> = self
                    .state
                    .window
                    .mru
                    .iter()
                    .copied()
                    .filter(|t| self.tab_item(*t).is_some() && self.section_of(*t).is_some())
                    .take(5)
                    .collect();
                // MRU[0] is normally the focused tab, but not in the empty state or with Peek open:
                // then the first card is already a different tab.
                let focused = self.focused_tab();
                let first_is_focused = tabs.first().is_some_and(|t| Some(*t) == focused);
                if tabs.len() < if first_is_focused { 2 } else { 1 } {
                    return;
                }
                let sel = match (forward, first_is_focused) {
                    (true, true) => 1,
                    (true, false) => 0,
                    (false, _) => tabs.len() - 1,
                };
                self.rt.switcher = Some((tabs, sel));
                self.bump();
            }
            Command::MruSelect { index } => {
                if let Some((tabs, sel)) = self.rt.switcher.as_mut()
                    && index < tabs.len()
                {
                    *sel = index;
                    self.mru_commit(now, fx);
                }
            }
            Command::MruCommit => self.mru_commit(now, fx),
            Command::MruCancel => {
                if self.rt.switcher.take().is_some() {
                    self.bump();
                }
            }

            // -------------------------------------------------------------- downloads
            Command::DownloadControl { id, action } => {
                let Some(d) = self.rt.downloads.iter().find(|d| d.id == id).cloned() else { return };
                match action {
                    DownloadAction::Retry => {
                        let tab = d.tab.filter(|t| self.is_live(*t)).or(self.focused_tab().filter(|t| self.is_live(*t)));
                        let Some(tab) = tab else {
                            self.toast("Open a tab to retry the download", None);
                            return;
                        };
                        if !matches!(d.state, DownloadState::InProgress | DownloadState::Paused) {
                            self.rt.downloads.retain(|x| x.id != id);
                            self.bump();
                        }
                        fx.push(Effect::StartDownload { tab, url: d.url });
                    }
                    other => fx.push(Effect::DownloadControl { id, action: other }),
                }
            }
            Command::DownloadDismiss { id } => {
                let before = self.rt.downloads.len();
                self.rt.downloads.retain(|d| d.id != id || matches!(d.state, DownloadState::InProgress | DownloadState::Paused));
                if self.rt.downloads.len() != before {
                    self.bump();
                }
            }

            // -------------------------------------------------------------- AI agents
            cmd @ (Command::AnswerAgentConnection { .. }
            | Command::AnswerSitePermission { .. }
            | Command::StopAgents
            | Command::ResumeAgents
            | Command::ShareTabWithAgent { .. }
            | Command::ResolveAgentDownload { .. }
            | Command::AgentConnectionRequested { .. }
            | Command::AgentSessionStarted { .. }
            | Command::AgentSessionEnded { .. }
            | Command::AgentActivity { .. }
            | Command::AgentSiteRequested { .. }
            | Command::OpenAgentTab { .. }
            | Command::LoadTab { .. }
            | Command::ShowAgentTab { .. }
            | Command::AgentDownloadHeld { .. }
            | Command::AgentTabAdopted { .. }
            | Command::ToggleAgentPanel
            | Command::CloseAgentPanel { .. }
            | Command::ArchiveAgentTabs
            | Command::AnswerTabAccess { .. }
            | Command::AgentTabAccessRequested { .. }) => self.handle_agent(cmd, now, fx),

            // -------------------------------------------------------------- docked DevTools
            cmd @ (Command::ToggleDevTools
            | Command::FocusDevTools
            | Command::UndockDevTools
            | Command::DevToolsClosed { .. }
            | Command::DevToolsUndockRequested { .. }
            | Command::DevToolsLinkRequested { .. }
            | Command::InspectElement { .. }) => self.handle_devtools(cmd, now, fx),

            // -------------------------------------------------------------- Chrome-created browsers
            cmd @ (Command::ForeignTabRequested { .. } | Command::ExtensionInstalled { .. } | Command::ForeignBlocked { .. }) => {
                self.handle_foreign(cmd, now, fx)
            }

            // -------------------------------------------------------------- extensions (Ctrl+E)
            cmd @ (Command::RunExtension { .. }
            | Command::RequestExtensionDetails { .. }
            | Command::SetExtensionEnabled { .. }
            | Command::RemoveExtension { .. }
            | Command::CloseExtensionPopup
            | Command::ExtensionsChanged { .. }
            | Command::ExtensionDetailsLoaded { .. }
            | Command::ExtensionOpFailed { .. }
            | Command::ExtensionPopupClosed { .. }
            | Command::SafeModeStarted) => self.handle_extensions(cmd, now, fx),

            // -------------------------------------------------------------- shell events
            other => self.handle_event(other, now, fx),
        }
    }

    /// The focused (or given) tab when it has a live browser.
    pub(super) fn live_target(&self, tab: Option<Id>) -> Option<Id> {
        tab.or(self.focused_tab()).filter(|t| self.is_live(*t))
    }

    fn open_input(&mut self, text: &str, target: OpenTarget, now: Millis, fx: &mut Vec<Effect>) {
        let text = text.trim();
        if text.is_empty() {
            return;
        }
        let url = match classify(text) {
            Classified::Url(u) => u,
            Classified::Search(q) if q.is_empty() => return,
            Classified::Search(q) => crate::omnibox::search_url(self.state.settings.search_engine, &self.state.settings.custom_search_url, &q),
        };
        self.open_url(url, target, None, true, now, fx);
    }

    fn open_url_at(&mut self, url: String, to: DropTarget, now: Millis, fx: &mut Vec<Effect>) {
        let url = url.trim().to_string();
        if url.is_empty() {
            return;
        }
        // A link dragged out of a page: same allowlist as links the page opens itself (no opener,
        // so never `file:`).
        if !urls::web_content_may_open(&url, None) {
            self.refuse_web_url(&url);
            return;
        }
        let dest = match to.container {
            Container::Favorites => {
                if self.state.favorites.len() >= MAX_FAVORITES {
                    self.toast(format!("Favorites are full ({MAX_FAVORITES})"), None);
                    return;
                }
                Parent::Favorites
            }
            Container::Pinned { space } => Parent::Pinned(space),
            Container::Today { space } => Parent::Today(space),
            Container::Folder { id } => Parent::Folder(id),
        };
        let Some(container) = self.container(dest) else { return };
        let index = to.before.and_then(|b| container.iter().position(|x| *x == b));
        let id = self.new_tab_item(&url, None, now);
        if let Some(t) = self.tab_item_mut(id)
            && !matches!(dest, Parent::Today(_))
        {
            t.pinned_url = Some(url.clone());
        }
        let len = self.container(dest).map_or(0, |c| c.len());
        self.insert_into(dest, index.unwrap_or(len), id);
        let space = self.space_of(id).unwrap_or(self.active_space_id());
        if dest == Parent::Favorites || space == self.active_space_id() {
            self.activate(id, now, fx);
        }
    }

    fn toggle_pin(&mut self, id: Option<Id>) {
        let Some(t) = id.or(self.content_focused_tab()) else { return };
        if self.tab_item(t).is_none() {
            return;
        }
        match self.parent_of(t) {
            Some((Parent::Today(space), _)) => {
                self.move_to(t, Parent::Pinned(space), None);
                self.toast("Pinned", None);
            }
            Some((Parent::Pinned(_) | Parent::Folder(_), _)) => {
                let space = self.space_of(t).unwrap_or(self.active_space_id());
                self.move_to(t, Parent::Today(space), Some(0));
            }
            Some((Parent::Favorites, _)) => {
                let space = self.active_space_id();
                self.move_to(t, Parent::Today(space), Some(0));
            }
            _ => {}
        }
    }

    /// `MoveItem` with the drag & drop matrix validation (arc_spec §2.6).
    fn move_item_checked(&mut self, id: Id, to: DropTarget) -> bool {
        let dest = match to.container {
            Container::Favorites => Parent::Favorites,
            Container::Pinned { space } => Parent::Pinned(space),
            Container::Today { space } => Parent::Today(space),
            Container::Folder { id } => Parent::Folder(id),
        };
        if self.container(dest).is_none() {
            return false;
        }
        let Some((src, _)) = self.parent_of(id) else { return false };
        let ok = match (self.state.items.get(&id), dest) {
            (Some(Item::Folder(_)), Parent::Pinned(_)) => true,
            (Some(Item::Folder(_)), Parent::Folder(f)) => {
                f != id && !self.is_descendant(id, f) && self.folder_depth(f) + self.folder_height(id) <= MAX_FOLDER_DEPTH
            }
            (Some(Item::Folder(_)), _) => false,
            (Some(Item::Split(_)), Parent::Today(_)) => true,
            (Some(Item::Split(_)), _) => false,
            (Some(Item::Tab(_)), Parent::Favorites) => {
                if src != Parent::Favorites && self.state.favorites.len() >= MAX_FAVORITES {
                    self.toast(format!("Favorites are full ({MAX_FAVORITES})"), None);
                    false
                } else {
                    true
                }
            }
            (Some(Item::Tab(_)), Parent::Folder(f)) => self.folder_item(f).is_some(),
            (Some(Item::Tab(_)), _) => true,
            _ => false,
        };
        if !ok {
            return false;
        }
        // `before` is resolved after removing the item (same-container reorder).
        let before = to.before.filter(|b| *b != id);
        let src_index = self.parent_of(id).map(|p| p.1);
        let same_container = src == dest;
        if same_container {
            let c = self.container(dest).cloned().unwrap_or_default();
            let target_pos = before.and_then(|b| c.iter().position(|x| *x == b));
            let cur = src_index.unwrap_or(0);
            let new_pos = match target_pos {
                Some(p) if p > cur => p - 1,
                Some(p) => p,
                None => c.len() - 1,
            };
            if new_pos == cur {
                return false;
            }
        }
        self.unlink(id);
        if let Parent::Split(sid) = src {
            self.dissolve_if_needed(sid);
        }
        let index = before.and_then(|b| self.container(dest).and_then(|c| c.iter().position(|x| *x == b)));
        let len = self.container(dest).map_or(0, |c| c.len());
        self.insert_into(dest, index.unwrap_or(len), id);
        if let Some(Item::Tab(t)) = self.state.items.get_mut(&id) {
            match dest {
                Parent::Favorites | Parent::Pinned(_) | Parent::Folder(_) => {
                    if t.pinned_url.as_deref().is_none_or(|p| p.trim().is_empty()) {
                        t.pinned_url = Some(t.url.clone());
                    }
                }
                _ => t.pinned_url = None,
            }
        }
        self.touch();
        true
    }

    fn move_to_space(&mut self, id: Option<Id>, space: Id) {
        if self.space(space).is_none() {
            return;
        }
        let Some(id) = id.or(self.active_item()) else { return };
        let Some((parent, _)) = self.parent_of(id) else { return };
        let src_space = self.space_of(id);
        let dest = match (self.state.items.get(&id), parent) {
            (_, Parent::Favorites) => return,
            (Some(Item::Tab(_)), Parent::Split(_)) => Parent::Today(space),
            (Some(Item::Tab(_) | Item::Split(_)), Parent::Today(_)) => Parent::Today(space),
            (Some(Item::Tab(_) | Item::Folder(_)), Parent::Pinned(_) | Parent::Folder(_)) => Parent::Pinned(space),
            _ => return,
        };
        if src_space == Some(space) && !matches!(parent, Parent::Split(_) | Parent::Folder(_)) {
            return;
        }
        let index = match dest {
            Parent::Today(_) => Some(0),
            _ => None,
        };
        if self.move_to(id, dest, index)
            && let Some(s) = self.space(space)
        {
            let label = format!("Moved to {} {}", s.icon, s.name);
            self.toast(label, Some(ToastAction { label: "Show".into(), command: Box::new(Command::ActivateItem { id }) }));
        }
    }

    fn clear_today(&mut self, space: Option<Id>, now: Millis, fx: &mut Vec<Effect>) {
        let space = space.unwrap_or(self.active_space_id());
        let Some(items) = self.space(space).map(|s| s.today.clone()) else { return };
        let visible = if space == self.active_space_id() { self.layout_tab_ids() } else { Vec::new() };
        let mut archived = Vec::new();
        for (index, item) in items.iter().enumerate() {
            let tabs = self.tabs_under(*item);
            let keep = tabs.iter().any(|t| visible.contains(t) || self.rt.tabs.get(t).is_some_and(|r| r.audible));
            if keep {
                continue;
            }
            archived.extend(self.archive_any(*item, ArchiveReason::ClearToday, Some(index), now, fx));
        }
        if archived.is_empty() {
            return;
        }
        let n = archived.len();
        self.push_reopen(ReopenEntry::Batch { archive_ids: archived });
        self.toast(
            format!("Cleared {n} {}", if n == 1 { "tab" } else { "tabs" }),
            Some(ToastAction { label: "Undo".into(), command: Box::new(Command::ReopenClosed) }),
        );
    }

    fn new_folder(&mut self, space: Option<Id>, parent: Option<Id>, name: Option<String>) {
        let dest = match parent {
            Some(p) => {
                if self.folder_item(p).is_none() || self.folder_depth(p) >= MAX_FOLDER_DEPTH {
                    return;
                }
                Parent::Folder(p)
            }
            None => {
                let s = space.unwrap_or(self.active_space_id());
                if self.space(s).is_none() {
                    return;
                }
                Parent::Pinned(s)
            }
        };
        let id = self.alloc_id();
        let name = name.map(|n| n.trim().to_string()).filter(|n| !n.is_empty()).unwrap_or_else(|| "New Folder".into());
        self.state.items.insert(id, Item::Folder(Folder { id, name, collapsed: false, children: Vec::new() }));
        self.insert_into(dest, 0, id);
        self.open_sidebar_panel(SidebarPanel::RenameItem { id });
        self.touch();
    }

    fn delete_space(&mut self, id: Id, now: Millis, fx: &mut Vec<Effect>) {
        if self.state.spaces.len() <= 1 {
            self.toast("The last space can't be deleted", None);
            return;
        }
        let Some(idx) = self.state.spaces.iter().position(|s| s.id == id) else { return };
        let space = self.state.spaces[idx].clone();
        let mut n = 0;
        for item in space.pinned.iter().chain(space.today.iter()) {
            n += self.archive_any(*item, ArchiveReason::SpaceDeleted, None, now, fx).len();
        }
        let was_active = self.active_space_id() == id;
        self.state.spaces.retain(|s| s.id != id);
        if was_active {
            let next = self.state.spaces[idx.saturating_sub(1).min(self.state.spaces.len() - 1)].id;
            self.close_peek(true, fx);
            self.state.window.active_space = next;
            self.rt.focus_request = self.content_focused_tab();
        }
        let label = if n > 0 { format!("Deleted “{}” · {n} {} archived", space.name, if n == 1 { "tab" } else { "tabs" }) } else { format!("Deleted “{}”", space.name) };
        self.toast(label, None);
        self.touch();
    }

    pub(super) fn switch_space(&mut self, id: Id, fx: &mut Vec<Effect>) {
        if id == self.active_space_id() || self.space(id).is_none() {
            return;
        }
        self.close_peek(true, fx);
        self.state.window.active_space = id;
        self.rt.focus_request = self.content_focused_tab();
        self.touch();
    }

    fn focus_pane(&mut self, pick: impl FnOnce(usize, usize) -> Option<usize>) {
        let Some(sid) = self.active_item().filter(|a| self.split_item(*a).is_some()) else { return };
        let Some((cur, len)) = self.split_item(sid).map(|s| (s.focused, s.panes.len())) else { return };
        let Some(i) = pick(cur, len).filter(|i| *i < len) else { return };
        if let Some(s) = self.split_mut(sid)
            && s.focused != i
        {
            s.focused = i;
        }
        self.rt.focus_request = self.content_focused_tab();
        self.touch();
    }

    fn open_sidebar_panel(&mut self, panel: SidebarPanel) {
        let valid = match &panel {
            SidebarPanel::EditSpace { id } => self.space(*id).is_some(),
            SidebarPanel::RenameItem { id } => matches!(self.state.items.get(id), Some(Item::Tab(_) | Item::Folder(_))),
            SidebarPanel::EditPinned { id } => matches!(self.tab_section(*id), Some(Section::Pinned | Section::Favorites)),
            SidebarPanel::Downloads | SidebarPanel::AppMenu | SidebarPanel::NewSpace => true,
        };
        if !valid {
            return;
        }
        let seq = self.next_seq();
        self.rt.sidebar_panel = Some(SidebarPanelView { panel, seq });
        if !self.state.window.sidebar_visible {
            // Reveal for the panel only (runtime): closing the panel hides the sidebar again.
            // Transient panels float (`SetSidebar.floating`), panels that hold input dock.
            self.rt.sidebar_revealed = true;
        }
        self.bump();
    }

    /// Closes the open sidebar panel if it is `panel` (a panel whose own command committed it).
    fn close_panel(&mut self, panel: &SidebarPanel) {
        if self.rt.sidebar_panel.as_ref().is_some_and(|p| p.panel == *panel) {
            self.rt.sidebar_panel = None;
            self.bump();
        }
    }

    fn copy_url(&mut self, id: Option<Id>, markdown: bool, fx: &mut Vec<Effect>) {
        let Some(target) = id.or(self.focused_tab()) else { return };
        let (url, title) = if let Some(t) = self.tab(target) {
            (t.url.clone(), urls::display_title(t.custom_title.as_deref(), &t.title, &t.url))
        } else if let Some(e) = self.state.archive.iter().find(|e| e.id == target) {
            (e.url.clone(), urls::display_title(e.custom_title.as_deref(), &e.title, &e.url))
        } else {
            return;
        };
        let url = urls::clean_url(&url);
        let text = if markdown {
            let escaped = title.replace('\\', "\\\\").replace('[', "\\[").replace(']', "\\]");
            format!("[{escaped}]({})", url.replace(')', "%29").replace(' ', "%20"))
        } else {
            url
        };
        fx.push(Effect::CopyToClipboard { text });
        self.toast(if markdown { "Copied URL as Markdown" } else { "Copied URL" }, None);
    }

    fn update_settings(&mut self, patch: SettingsPatch) {
        self.patch_agent_settings(&patch);
        let s = &mut self.state.settings;
        if let Some(v) = patch.search_engine {
            s.search_engine = v;
        }
        if let Some(v) = patch.custom_search_url {
            s.custom_search_url = v.trim().to_string();
        }
        if let Some(v) = patch.archive_after_hours {
            s.archive_after_hours = normalize_archive_hours(v);
        }
        if let Some(v) = patch.appearance {
            s.appearance = v;
        }
        if let Some(v) = patch.startup {
            s.startup = v;
        }
        if let Some(v) = patch.peek_enabled {
            s.peek_enabled = v;
        }
        if let Some(v) = patch.download_dir {
            let v = v.trim();
            s.download_dir = (!v.is_empty()).then(|| v.to_string());
        }
        if let Some(v) = patch.ask_download_location {
            s.ask_download_location = v;
        }
        if let Some(v) = patch.search_suggestions {
            s.search_suggestions = v;
        }
        if let Some(a) = patch.animations.as_ref() {
            s.animations.apply_patch(a);
        }
        self.touch();
    }

    fn upsert_boost(&mut self, boost: Boost, now: Millis, fx: &mut Vec<Effect>) {
        let host = boost.host.trim().trim_start_matches("https://").trim_start_matches("http://");
        let host = host.split(['/', '?', '#']).next().unwrap_or("").trim_start_matches("www.").to_ascii_lowercase();
        let existing = (boost.id != 0).then(|| self.state.boosts.iter().position(|b| b.id == boost.id)).flatten();
        let mut hosts = vec![host.clone()];
        match existing {
            Some(pos) => {
                let b = &mut self.state.boosts[pos];
                hosts.push(b.host.clone());
                b.name = boost.name;
                b.host = host;
                b.enabled = boost.enabled;
                b.css = boost.css;
                b.js = boost.js;
                b.updated_at = now;
            }
            None => {
                let id = self.alloc_id();
                self.state.boosts.push(Boost { id, host, created_at: now, updated_at: now, ..boost });
            }
        }
        self.reload_boosted(&hosts, fx);
        self.touch();
    }

    /// Reload loaded tabs matching any of the boost hosts.
    fn reload_boosted(&mut self, hosts: &[String], fx: &mut Vec<Effect>) {
        let mut tabs: Vec<Id> = self.state.items.values().filter_map(|i| if let Item::Tab(t) = i { Some(t.id) } else { None }).collect();
        tabs.extend(self.peek_tab());
        for t in tabs {
            if !self.is_live(t) {
                continue;
            }
            let Some(url) = self.tab(t).map(|t| t.url.clone()) else { continue };
            if hosts.iter().any(|h| urls::host_matches(h, &url)) {
                fx.push(Effect::Reload { tab: t, ignore_cache: false });
            }
        }
    }

    fn mru_commit(&mut self, now: Millis, fx: &mut Vec<Effect>) {
        let Some((tabs, sel)) = self.rt.switcher.take() else { return };
        self.bump();
        if let Some(t) = tabs.get(sel).copied() {
            self.activate(t, now, fx);
        }
    }
}

/// Snap an archive delay to the supported values (12h, 24h, 7d, 30d).
pub(super) fn normalize_archive_hours(v: u32) -> u32 {
    const ALLOWED: [u32; 4] = [12, 24, 168, 720];
    *ALLOWED.iter().min_by_key(|a| (**a as i64 - v as i64).abs()).unwrap_or(&12)
}
