//! Shell → core events (browser lifecycle, page state, downloads, permissions, window, tick).

use super::*;
use crate::urls;

impl Store {
    pub(super) fn handle_event(&mut self, cmd: Command, now: Millis, fx: &mut Vec<Effect>) {
        match cmd {
            Command::TabBrowserCreated { tab } => {
                // The browser exists now; nothing visible changes (it was already "loaded"). A
                // browser created for a tab that vanished without a destroy request is cleaned up.
                let orphan = self.tab(tab).is_none()
                    && !self.rt.closing.contains(&tab)
                    && !self.rt.deferred_destroy.contains(&tab)
                    && self.rt.tabs.get(&tab).is_some_and(|r| r.loaded);
                if orphan {
                    self.rt.closing.insert(tab);
                    fx.push(Effect::DestroyBrowser { tab });
                } else if let Some(r) = self.rt.tabs.get_mut(&tab)
                    && r.loaded
                {
                    // Confirms the current generation: later close reports are about this browser.
                    r.created = true;
                    r.ignored_close_ticks = None;
                }
            }
            Command::TabBrowserClosed { tab } => self.browser_closed(tab, now, fx),
            Command::TabAddressChanged { tab, url } => {
                if !self.accepts_events(tab) || url.is_empty() {
                    return;
                }
                let persisted = self.state.items.contains_key(&tab);
                let Some((prev, title)) = self.tab(tab).map(|t| (t.url.clone(), t.title.clone())) else { return };
                let (typed_match, new_commit, had_error, loading) = {
                    let r = self.trt(tab);
                    let typed = r.pending_typed.take().is_some_and(|p| typed_matches(&p, &url));
                    // The first commit of a browser that lazily (re)loads the tab is not a visit.
                    let quiet = std::mem::take(&mut r.quiet_first_commit) && r.committed_url.is_none();
                    let new_commit = !quiet && r.committed_url.as_deref() != Some(url.as_str());
                    let had_error = r.load_error.is_some() || r.failed_url.is_some();
                    r.committed_url = Some(url.clone());
                    r.browser_url = url.clone();
                    // The shell never reports its own error page: a real page committed.
                    r.load_error = None;
                    r.failed_url = None;
                    (typed, new_commit, had_error, r.loading)
                };
                if had_error {
                    self.bump();
                }
                let cross_document = prev.split('#').next() != url.split('#').next();
                if prev != url {
                    if cross_document && loading {
                        self.trt(tab).audible = false;
                    }
                    if let Some(t) = self.tab_any_mut(tab) {
                        t.url = url.clone();
                    }
                    if persisted {
                        self.touch();
                    } else {
                        self.bump();
                    }
                }
                if (new_commit || typed_match) && crate::history::is_history_url(&url) {
                    let pinned_home = matches!(self.tab_section(tab), Some(Section::Pinned | Section::Favorites))
                        && self.tab(tab).and_then(|t| t.pinned_url.as_deref()).is_some_and(|p| !urls::differs_from_pinned(&url, p));
                    let transition = if typed_match {
                        Transition::Typed
                    } else if pinned_home {
                        Transition::Bookmark
                    } else {
                        Transition::Link
                    };
                    // A cross-document title belongs to the previous page (the new one arrives via
                    // TabTitleChanged); a same-document navigation or a commit of the tab's own
                    // URL (first load of a new tab, typed reload) keeps the known title.
                    let visit_title = if cross_document { "" } else { title.as_str() };
                    self.history.record_visit(&url, visit_title, transition, now);
                    self.touch_history();
                }
            }
            Command::TabTitleChanged { tab, title } => {
                if !self.accepts_events(tab) {
                    return;
                }
                let persisted = self.state.items.contains_key(&tab);
                let Some(t) = self.tab_any_mut(tab) else { return };
                let url = t.url.clone();
                if t.title != title {
                    t.title = title.clone();
                    if persisted {
                        self.touch();
                    } else {
                        self.bump();
                    }
                }
                // Also when the tab title is unchanged: the new page of a cross-document
                // navigation may have the same title as the previous one, and its visit was
                // recorded without a title (it belonged to the previous page).
                if self.history.update_title(&url, &title) {
                    self.touch_history();
                }
            }
            Command::TabFaviconChanged { tab, url } => {
                if !self.accepts_events(tab) {
                    return;
                }
                let persisted = self.state.items.contains_key(&tab);
                let url = url.filter(|u| !u.trim().is_empty());
                let Some(t) = self.tab_any_mut(tab) else { return };
                if t.favicon != url {
                    t.favicon = url;
                    if persisted {
                        self.touch();
                    } else {
                        self.bump();
                    }
                }
            }
            Command::TabLoadingStateChanged { tab, loading, can_go_back, can_go_forward } => {
                if !self.accepts_events(tab) {
                    return;
                }
                let r = self.trt(tab);
                let changed = r.loading != loading || r.can_go_back != can_go_back || r.can_go_forward != can_go_forward;
                if loading && !r.loading {
                    r.progress = 0.0;
                    r.crashed = false;
                }
                if !loading && r.loading {
                    r.progress = 1.0;
                }
                r.loading = loading;
                r.can_go_back = can_go_back;
                r.can_go_forward = can_go_forward;
                if changed {
                    self.bump();
                }
            }
            Command::TabLoadProgress { tab, progress } => {
                if !self.accepts_events(tab) {
                    return;
                }
                let p = if progress.is_finite() { progress.clamp(0.0, 1.0) } else { 0.0 };
                let r = self.trt(tab);
                if r.progress != p {
                    r.progress = p;
                    self.bump();
                }
            }
            Command::TabLoadFailed { tab, url, error_code, error_text } => {
                if !self.accepts_events(tab) {
                    return;
                }
                let text = if error_text.trim().is_empty() { format!("Error {error_code}") } else { error_text };
                {
                    let r = self.trt(tab);
                    r.load_error = Some(text);
                    r.failed_url = Some(url.clone());
                    r.pending_typed = None;
                    r.loading = false;
                }
                if !url.is_empty() {
                    self.set_tab_url(tab, &url);
                }
                self.bump();
            }
            Command::TabAudioChanged { tab, audible } => {
                if !self.accepts_events(tab) {
                    return;
                }
                let r = self.trt(tab);
                if r.audible != audible {
                    r.audible = audible;
                    self.bump();
                }
            }
            Command::TabCrashed { tab } => {
                if !self.accepts_events(tab) {
                    return;
                }
                let r = self.trt(tab);
                r.crashed = true;
                r.loading = false;
                r.audible = false;
                self.bump();
            }
            Command::TabZoomChanged { tab, level } => {
                if !self.accepts_events(tab) || !level.is_finite() {
                    return;
                }
                let r = self.trt(tab);
                let show = std::mem::take(&mut r.zoom_toast);
                let changed = r.zoom_level != level;
                r.zoom_level = level;
                if changed {
                    self.bump();
                }
                if show {
                    self.toast(format!("Zoom {}%", zoom_percent(level)), None);
                }
            }
            Command::TabFocused { tab } => {
                if self.rt.command_bar.take().is_some() {
                    self.bump();
                }
                // Clicking into a page dismisses transient sidebar popovers (downloads, app menu);
                // sheets with input (new/edit space, inline rename) stay open.
                if self.rt.sidebar_panel.as_ref().is_some_and(|p| p.panel.is_transient()) {
                    self.rt.sidebar_panel = None;
                    self.bump();
                }
                if self.rt.peek.as_ref().is_some_and(|p| !p.popup && p.tab.id != tab) {
                    self.close_peek(true, fx);
                }
                if let Some((Parent::Split(sid), i)) = self.parent_of(tab)
                    && self.active_item() == Some(sid)
                    && let Some(s) = self.split_mut(sid)
                    && s.focused != i
                {
                    s.focused = i;
                    self.touch();
                }
            }
            Command::PopupAdopted { tab, opener, url, popup, foreground } => {
                self.adopt_popup(tab, opener, url, popup, foreground, now, fx);
            }
            Command::LinkOpenRequested { opener, url, disposition } => self.link_open(opener, url, disposition, now, fx),
            Command::TabFullscreenChanged { tab, fullscreen } => {
                if fullscreen {
                    let visible = self.layout_tab_ids().contains(&tab) || self.peek_tab() == Some(tab);
                    if !visible || !self.is_live(tab) {
                        if self.is_live(tab) {
                            fx.push(Effect::ExitPageFullscreen { tab });
                        }
                        return;
                    }
                    if self.rt.page_fullscreen != Some(tab) {
                        self.rt.page_fullscreen = Some(tab);
                        self.toast_for("Press Esc to exit full screen", None, 3000);
                        self.bump();
                    }
                } else if self.rt.page_fullscreen == Some(tab) {
                    self.rt.page_fullscreen = None;
                    self.bump();
                }
            }
            Command::PermissionRequested { id, tab, origin, kinds } => {
                let origin = urls::normalize_origin(&origin);
                if self.tab(tab).is_none() || !self.is_live(tab) || self.rt.permission_prompts.iter().any(|p| p.id == id) {
                    fx.push(Effect::AnswerPermission { id, allow: false, remember: false });
                    return;
                }
                let remembered: Vec<Option<bool>> = kinds
                    .iter()
                    .map(|k| self.state.site_permissions.iter().find(|s| s.origin == origin && s.kind == *k).map(|s| s.allow))
                    .collect();
                if !kinds.is_empty() {
                    if remembered.contains(&Some(false)) {
                        fx.push(Effect::AnswerPermission { id, allow: false, remember: true });
                        return;
                    }
                    if remembered.iter().all(|r| *r == Some(true)) {
                        fx.push(Effect::AnswerPermission { id, allow: true, remember: true });
                        return;
                    }
                }
                let host = urls::display_host(&origin);
                self.rt.permission_prompts.push(PermissionPromptView { id, tab, origin, host, kinds });
                self.bump();
            }
            Command::PermissionDismissed { id } => {
                let before = self.rt.permission_prompts.len();
                self.rt.permission_prompts.retain(|p| p.id != id);
                if self.rt.permission_prompts.len() != before {
                    self.bump();
                }
            }
            Command::DownloadUpdated { download } => self.download_updated(download, now, fx),
            Command::DownloadInBlankTab { tab } => self.download_in_blank_tab(tab, now, fx),
            Command::WindowStateChanged { maximized, fullscreen, focused, bounds } => {
                let visible_change = self.rt.window_maximized != maximized || self.rt.window_fullscreen != fullscreen || self.rt.window_focused != focused;
                self.rt.window_maximized = maximized;
                self.rt.window_fullscreen = fullscreen;
                self.rt.window_focused = focused;
                if self.state.window.maximized != maximized {
                    self.state.window.maximized = maximized;
                    self.dirty.state = true;
                }
                if let Some(b) = bounds.filter(|b| b.width > 0 && b.height > 0)
                    && !maximized && !fullscreen && self.state.window.bounds != Some(b)
                {
                    self.state.window.bounds = Some(b);
                    self.dirty.state = true;
                }
                if !focused && self.rt.switcher.take().is_some() {
                    self.bump();
                }
                if visible_change {
                    self.bump();
                }
            }
            Command::SystemThemeChanged { dark }
                if self.rt.system_dark != dark => {
                    self.rt.system_dark = dark;
                    self.bump();
                }
            // Runtime only: `bump` (not `touch`), so nothing is saved and the revision moves only
            // when Windows actually changed its mind.
            Command::SystemAnimationsChanged { enabled } if self.rt.system_animations.0 != enabled => {
                self.rt.system_animations.0 = enabled;
                self.bump();
            }
            // Runtime only, like the animation setting above: the status is the shell's, and the
            // two states worth interrupting somebody for get a toast — once each, because the shell
            // reports every step and only these two are new information.
            Command::UpdateStatusChanged { status } if self.rt.update != status => {
                let announce = match &status {
                    crate::update::UpdateStatus::Available { version, .. } => Some(format!("sta {version} is available")),
                    crate::update::UpdateStatus::Ready { version } => Some(format!("sta {version} is ready — restart to update")),
                    _ => None,
                };
                self.rt.update = status;
                if let Some(message) = announce {
                    self.toast(message, None);
                } else {
                    self.bump();
                }
            }
            Command::Tick => self.tick(now, fx),
            // Every UI command is handled in `handle`.
            _ => {}
        }
    }

    /// Events from a browser that is being destroyed (or kept only for a download) are stale.
    fn accepts_events(&self, tab: Id) -> bool {
        self.tab(tab).is_some() && !self.rt.closing.contains(&tab) && !self.rt.deferred_destroy.contains(&tab)
    }

    fn browser_closed(&mut self, tab: Id, now: Millis, fx: &mut Vec<Effect>) {
        let requested = self.rt.closing.contains(&tab);
        let deferred = self.rt.deferred_destroy.contains(&tab);
        let loaded = self.rt.tabs.get(&tab).is_some_and(|r| r.loaded);
        if !requested && !deferred && !loaded && !self.rt.pending_create.contains_key(&tab) {
            // Late or duplicate report for a browser core already considers gone: the tab is
            // unloaded (or unknown), so there is nothing to close and nothing to archive.
            return;
        }
        if !requested
            && !deferred
            && let Some(r) = self.rt.tabs.get_mut(&tab)
            && r.loaded
            && r.after_close
            && !r.created
        {
            // The browser created when the previous one's close was reported isn't confirmed
            // yet: this report is a stale duplicate for the previous browser, not a close of the
            // re-created one. (A genuine creation failure is picked up by `tick`.)
            r.ignored_close_ticks.get_or_insert(0);
            return;
        }
        self.rt.closing.remove(&tab);
        self.rt.deferred_destroy.remove(&tab);
        // DevTools belong to the tab's browser: closed, unloaded, archived or crashed all end here.
        self.devtools_gone(tab);
        if let Some(r) = self.rt.tabs.get_mut(&tab) {
            let (zoom, find_text, match_case) = (r.zoom_level, std::mem::take(&mut r.find_text), r.find_match_case);
            *r = TabRuntime { zoom_level: zoom, find_text, find_match_case: match_case, generation: r.generation, ..TabRuntime::default() };
        }
        self.rt.permission_prompts.retain(|p| p.tab != tab);
        self.bump();
        if let Some(url) = self.rt.pending_create.remove(&tab)
            && self.tab(tab).is_some()
        {
            let generation = self.rt.tabs.get(&tab).map_or(0, |r| r.generation);
            self.load_tab(tab, url, fx);
            if let Some(r) = self.rt.tabs.get_mut(&tab)
                && r.loaded
                && r.generation != generation
            {
                r.after_close = true;
            }
            return;
        }
        if requested || deferred {
            if self.tab(tab).is_none() {
                self.rt.tabs.remove(&tab);
            }
            return;
        }
        // Not requested by core: the page closed itself or creation failed.
        if self.peek_tab() == Some(tab) {
            self.close_peek(false, fx);
            self.rt.focus_request = self.content_focused_tab();
            return;
        }
        if self.state.items.contains_key(&tab) {
            self.close_item(Some(tab), now, fx);
        } else {
            self.rt.tabs.remove(&tab);
        }
    }

    fn download_updated(&mut self, mut d: Download, now: Millis, fx: &mut Vec<Effect>) {
        d.received_bytes = d.received_bytes.max(0);
        d.total_bytes = d.total_bytes.filter(|t| *t > 0);
        d.bytes_per_sec = d.bytes_per_sec.max(0);
        if d.started_at == 0 {
            d.started_at = now;
        }
        let finished = |s: DownloadState| !matches!(s, DownloadState::InProgress | DownloadState::Paused);
        let prev = self.rt.downloads.iter().position(|x| x.id == d.id);
        let was = prev.map(|i| self.rt.downloads[i].state);
        match prev {
            Some(i) => {
                if self.rt.downloads[i] == d {
                    return;
                }
                if d.tab.is_none() {
                    d.tab = self.rt.downloads[i].tab;
                }
                self.rt.downloads[i] = d.clone();
            }
            None => {
                self.rt.downloads.insert(0, d.clone());
                // Keep active downloads; drop the oldest finished ones beyond the cap.
                while self.rt.downloads.len() > MAX_DOWNLOADS {
                    match self.rt.downloads.iter().rposition(|x| finished(x.state)) {
                        Some(i) => {
                            self.rt.downloads.remove(i);
                        }
                        None => break,
                    }
                }
            }
        }
        self.bump();
        if d.state == DownloadState::Complete && was != Some(DownloadState::Complete) {
            let name = if d.file_name.is_empty() { "file".to_string() } else { d.file_name.clone() };
            self.toast(
                format!("Downloaded {name}"),
                Some(ToastAction { label: "Open".into(), command: Box::new(Command::DownloadControl { id: d.id, action: DownloadAction::Open }) }),
            );
        }
        if finished(d.state)
            && let Some(tab) = d.tab
            && self.rt.deferred_destroy.contains(&tab)
            && !self.has_active_download(tab)
        {
            self.rt.deferred_destroy.remove(&tab);
            self.rt.closing.insert(tab);
            fx.push(Effect::DestroyBrowser { tab });
        }
    }

    /// `DownloadInBlankTab`: the link a Peek was opened for turned out to be a file, so the overlay
    /// has nothing to show — an empty card over the page, which is what an Alt+click (or Shift+click)
    /// on a `Content-Disposition` attachment used to leave behind. Throw it away and let the
    /// download toast be the outcome. `close_peek` destroys the browser, which `release_browser`
    /// defers while the download is in progress, so the file still arrives.
    fn download_in_blank_tab(&mut self, tab: Id, now: Millis, fx: &mut Vec<Effect>) {
        if self.peek_tab() == Some(tab) {
            self.close_peek(true, fx);
            return;
        }
        // The same rule for a *tab*. Opening a download URL as a page (the command bar, a bookmark,
        // a link that turns out to be an attachment) downloaded the file and then left the tab
        // behind for good: a blank white rectangle titled by host, whose URL pill named a file that
        // was never on screen. The toast — "Downloaded <name> · Open" — is the whole outcome here
        // too. Archived with `Auto`, so Ctrl+Shift+T does not offer to start the download again.
        // A pinned tab or a favorite is a *place* rather than a result, so those are left alone.
        let Some((parent, _)) = self.parent_of(tab) else { return };
        match parent {
            Parent::Today(space) => {
                let was_active = self.space(space).and_then(|s| s.active_item) == Some(tab);
                let opener = self.tab_item(tab).and_then(|t| t.opener);
                if self.archive_tab(tab, ArchiveReason::Auto, None, now, fx).is_some() && was_active {
                    self.fall_back(space, &[tab], opener, now, fx);
                }
            }
            Parent::Split(sid) => {
                let was_active = self.active_item() == Some(sid);
                if self.archive_tab(tab, ArchiveReason::Auto, None, now, fx).is_some() && was_active {
                    self.rt.focus_request = self.content_focused_tab();
                }
            }
            Parent::Favorites | Parent::Pinned(_) | Parent::Folder(_) => {}
        }
    }

    fn tick(&mut self, now: Millis, fx: &mut Vec<Effect>) {
        // A close report ignored as stale while a re-created browser was unconfirmed: if that
        // browser still isn't confirmed two ticks later, its creation failed (the shell reports a
        // creation failure as `TabBrowserClosed` instead of `TabBrowserCreated`).
        let unconfirmed: Vec<Id> = self
            .rt
            .tabs
            .iter()
            .filter(|(_, r)| r.ignored_close_ticks.is_some() && r.loaded && !r.created)
            .map(|(t, _)| *t)
            .collect();
        for t in unconfirmed {
            let r = self.trt(t);
            let ticks = r.ignored_close_ticks.unwrap_or(0) + 1;
            if ticks >= 2 {
                r.ignored_close_ticks = None;
                r.after_close = false;
                self.browser_closed(t, now, fx);
            } else {
                r.ignored_close_ticks = Some(ticks);
            }
        }
        for t in self.layout_tab_ids() {
            if let Some(tab) = self.tab_item_mut(t)
                && tab.last_active_at != now
            {
                tab.last_active_at = now;
                self.dirty.state = true;
            }
        }
        self.auto_archive(now, fx);
        let cutoff = now.saturating_sub(ARCHIVE_RETENTION_MS);
        let before = self.state.archive.len();
        self.state.archive.retain(|e| e.archived_at >= cutoff);
        if self.state.archive.len() != before {
            self.touch_archive();
        }
        let stale: Vec<u64> = self.rt.permission_prompts.iter().filter(|p| !self.is_live(p.tab)).map(|p| p.id).collect();
        for id in stale {
            self.rt.permission_prompts.retain(|p| p.id != id);
            fx.push(Effect::AnswerPermission { id, allow: false, remember: false });
            self.bump();
        }
    }
}

/// Does a committed URL correspond to a typed navigation? Exact match, a normalized match
/// (trailing slash, scheme/host case), or a same-site redirect (`google.com` → `www.google.com`).
fn typed_matches(pending: &str, committed: &str) -> bool {
    if pending == committed {
        return true;
    }
    let norm = |u: &str| url::Url::parse(u).map(|p| p.to_string()).unwrap_or_else(|_| u.to_string());
    if urls::dedupe_key(&norm(pending)) == urls::dedupe_key(&norm(committed)) {
        return true;
    }
    urls::same_site(pending, committed) || matches!((urls::host(pending), urls::host(committed)), (Some(a), Some(b)) if a == b)
}
