//! Split view and Peek.

use super::tree::equalize;
use super::*;
use crate::omnibox::{classify, Classified};

impl Store {
    /// `SplitWith` semantics. Returns the split id.
    pub(super) fn split_with(&mut self, tab: Id, with: Id, side: SplitSide, now: Millis, fx: &mut Vec<Effect>) -> Option<Id> {
        let with = match self.split_item(with) {
            Some(s) => *s.panes.get(s.focused)?,
            None => with,
        };
        if tab == with || self.tab_item(tab).is_none() || self.tab_item(with).is_none() {
            return None;
        }
        let (with_parent, _) = self.parent_of(with)?;
        let (tab_parent, _) = self.parent_of(tab)?;
        if let Parent::Split(ws) = with_parent {
            let same = tab_parent == Parent::Split(ws);
            if !same && self.split_item(ws).is_some_and(|s| s.panes.len() >= MAX_SPLIT_PANES) {
                self.toast(format!("Split view is full ({MAX_SPLIT_PANES} panes)"), None);
                return None;
            }
        }
        let after = matches!(side, SplitSide::Right | SplitSide::Bottom);
        // Pinned / favorite tabs are duplicated into Today first.
        let with = match with_parent {
            Parent::Favorites | Parent::Pinned(_) | Parent::Folder(_) => {
                let dup = self.duplicate_tab_item(with, now)?;
                let space = self.space_of(with).unwrap_or(self.active_space_id());
                self.insert_into(Parent::Today(space), 0, dup);
                dup
            }
            _ => with,
        };
        let tab = match tab_parent {
            Parent::Favorites | Parent::Pinned(_) | Parent::Folder(_) => {
                let dup = self.duplicate_tab_item(tab, now)?;
                let space = self.space_of(with).unwrap_or(self.active_space_id());
                self.insert_into(Parent::Today(space), 0, dup);
                dup
            }
            _ => tab,
        };
        // Reorder inside the same split.
        if let (Some((Parent::Split(a), _)), Some((Parent::Split(b), _))) = (self.parent_of(tab), self.parent_of(with))
            && a == b
        {
            let s = self.split_mut(a)?;
            s.panes.retain(|p| *p != tab);
            let wi = s.panes.iter().position(|p| *p == with)?;
            let at = if after { wi + 1 } else { wi };
            s.panes.insert(at, tab);
            equalize(&mut s.fractions);
            s.focused = at;
            self.touch();
            self.activate(tab, now, fx);
            return Some(a);
        }
        // Detach `tab` from where it is.
        match self.parent_of(tab) {
            Some((Parent::Split(s2), _)) => {
                self.unlink(tab);
                self.dissolve_if_needed(s2);
            }
            Some((Parent::Today(_), _)) => {
                self.unlink(tab);
            }
            _ => return None,
        }
        let split_id = match self.parent_of(with)? {
            (Parent::Split(ws), wi) => {
                self.insert_into(Parent::Split(ws), if after { wi + 1 } else { wi }, tab);
                let s = self.split_mut(ws)?;
                equalize(&mut s.fractions);
                ws
            }
            (Parent::Today(space), wi) => {
                let sid = self.alloc_id();
                let orientation = match side {
                    SplitSide::Left | SplitSide::Right => Orientation::Horizontal,
                    SplitSide::Top | SplitSide::Bottom => Orientation::Vertical,
                };
                let panes = if after { vec![with, tab] } else { vec![tab, with] };
                let focused = panes.iter().position(|p| *p == tab).unwrap_or(0);
                self.state.items.insert(sid, Item::Split(Split { id: sid, orientation, panes, fractions: vec![0.5, 0.5], focused }));
                if let Some(sp) = self.space_mut(space) {
                    sp.today[wi] = sid;
                }
                for sp in &mut self.state.spaces {
                    if sp.active_item == Some(with) {
                        sp.active_item = Some(sid);
                    }
                }
                sid
            }
            _ => return None,
        };
        self.touch();
        self.activate(tab, now, fx);
        Some(split_id)
    }

    pub(super) fn split_open_input(&mut self, text: &str, side: SplitSide, now: Millis, fx: &mut Vec<Effect>) {
        let text = text.trim();
        if text.is_empty() {
            return;
        }
        let Some(focused) = self.content_focused_tab() else {
            let prev = std::mem::replace(&mut self.rt.omnibox_commit, true);
            self.handle(Command::OpenInput { text: text.to_string(), target: OpenTarget::NewTab }, now, fx);
            self.rt.omnibox_commit = prev;
            return;
        };
        let url = match classify(text) {
            Classified::Url(u) => u,
            Classified::Search(q) => crate::omnibox::search_url(self.state.settings.search_engine, &self.state.settings.custom_search_url, &q),
        };
        // An external protocol never becomes a pane: the OS opens it.
        if crate::urls::is_external_scheme(&url) {
            fx.push(Effect::OpenExternal { url });
            return;
        }
        if let Some(Parent::Split(sid)) = self.parent_of(focused).map(|p| p.0)
            && self.split_item(sid).is_some_and(|s| s.panes.len() >= MAX_SPLIT_PANES)
        {
            self.toast(format!("Split view is full ({MAX_SPLIT_PANES} panes)"), None);
            return;
        }
        let id = self.new_tab_item(&url, None, now);
        let space = self.active_space_id();
        self.insert_into(Parent::Today(space), 0, id);
        self.trt(id).pending_typed = Some(url);
        if self.split_with(id, focused, side, now, fx).is_none() {
            // Could not split (shouldn't happen after the checks): keep it as a normal tab.
            self.activate(id, now, fx);
        }
    }

    /// `SeparatePane`: the pane becomes a Today tab right below its split. Separating the focused
    /// pane of the active split keeps that tab active (it is the one the user was using); another
    /// pane leaves the split (or its survivor) active.
    pub(super) fn separate_pane(&mut self, tab: Option<Id>) {
        let Some(t) = tab.or(self.content_focused_tab()) else { return };
        let Some((Parent::Split(sid), _)) = self.parent_of(t) else { return };
        let Some((Parent::Today(space), sidx)) = self.parent_of(sid) else { return };
        let was_active = self.active_item() == Some(sid);
        let was_focused = was_active && self.content_focused_tab() == Some(t);
        self.unlink(t);
        self.dissolve_if_needed(sid);
        self.insert_into(Parent::Today(space), sidx + 1, t);
        if was_focused && let Some(sp) = self.space_mut(space) {
            sp.active_item = Some(t);
        }
        if was_active {
            self.rt.focus_request = self.content_focused_tab();
        }
        self.touch();
    }

    /// `SeparateAll`: every pane becomes a Today tab where the split was.
    pub(super) fn separate_all(&mut self, id: Id) {
        let sid = self.top_level_of(id);
        let Some(s) = self.split_item(sid).cloned() else { return };
        let Some((Parent::Today(space), sidx)) = self.parent_of(sid) else { return };
        let focused = s.panes.get(s.focused).copied().or(s.panes.first().copied());
        if let Some(sp) = self.space_mut(space) {
            sp.today.remove(sidx);
            for (i, p) in s.panes.iter().enumerate() {
                sp.today.insert(sidx + i, *p);
            }
        }
        self.state.items.remove(&sid);
        for sp in &mut self.state.spaces {
            if sp.active_item == Some(sid) {
                sp.active_item = focused;
            }
        }
        if self.active_item() == focused {
            self.rt.focus_request = focused;
        }
        self.touch();
    }

    // ------------------------------------------------------------------------------ peek

    fn peek_is_popup(&self) -> bool {
        self.rt.peek.as_ref().is_some_and(|p| p.popup)
    }

    /// Show `tab` (a runtime-only tab) in Peek, replacing a previous non-popup Peek.
    pub(super) fn open_peek(&mut self, id: Id, url: &str, opener: Option<Id>, popup: bool, now: Millis, fx: &mut Vec<Effect>) {
        self.close_peek(true, fx);
        let tab = Tab { id, url: url.to_string(), created_at: now, last_active_at: now, opener, ..Tab::default() };
        self.rt.peek = Some(PeekState { tab, popup });
        self.rt.visit_first_commit.insert(id);
        self.bump();
    }

    pub(super) fn expand_peek(&mut self, split: bool, now: Millis, fx: &mut Vec<Effect>) {
        let Some(p) = self.rt.peek.take() else { return };
        let mut tab = p.tab;
        let id = tab.id;
        let opener = tab.opener;
        tab.last_active_at = now;
        self.state.items.insert(id, Item::Tab(tab));
        let space = self.active_space_id();
        self.insert_into(Parent::Today(space), 0, id);
        self.touch();
        if split
            && let Some(o) = opener.filter(|o| self.tab_item(*o).is_some())
            && self.split_with(id, o, SplitSide::Right, now, fx).is_some()
        {
            return;
        }
        self.activate(id, now, fx);
    }

    /// `PopupAdopted` rules (arguments mirror the command's fields).
    #[allow(clippy::too_many_arguments)]
    pub(super) fn adopt_popup(&mut self, tab: Id, opener: Option<Id>, url: String, popup: bool, foreground: bool, now: Millis, fx: &mut Vec<Effect>) {
        if self.state.items.contains_key(&tab) || self.peek_tab() == Some(tab) || tab == 0 || tab > crate::MAX_ID {
            return;
        }
        if self.state.next_id <= tab {
            self.state.next_id = tab + 1;
            self.dirty.state = true;
        }
        let opener = opener.filter(|o| self.tab(*o).is_some());
        let opener_url = opener.and_then(|o| self.tab(o)).map(|t| t.url.clone());
        let mut url = url.trim().to_string();
        {
            let r = self.trt(tab);
            r.loaded = true;
            r.generation += 1;
            r.internal = false;
            // The browser exists already: nothing to wait for.
            r.created = true;
            r.after_close = false;
            r.ignored_close_ticks = None;
            r.browser_url = url.clone();
        }
        if url.is_empty() {
            // `window.open()` without a URL (often followed by `document.write`): the popup shows
            // its own content, so it must not be navigated.
            url = "about:blank".into();
            self.trt(tab).browser_url = url.clone();
        } else if !crate::urls::web_content_may_open(&url, opener_url.as_deref()) {
            self.refuse_web_url(&url);
            url = "about:blank".into();
            fx.push(Effect::LoadUrl { tab, url: url.clone() });
            self.trt(tab).browser_url = url.clone();
        }
        let pinned_opener = opener.is_some_and(|o| matches!(self.tab_section(o), Some(Section::Pinned | Section::Favorites)));
        let cross_site = || {
            let from = opener.and_then(|o| self.tab(o)).and_then(|t| t.pinned_url.clone().or(Some(t.url.clone())));
            match (from.and_then(|f| crate::urls::site_key(&f)), crate::urls::site_key(&url)) {
                (Some(a), Some(b)) => a != b,
                (Some(_), None) => url != "about:blank",
                _ => false,
            }
        };
        let peek_candidate = self.state.settings.peek_enabled && (popup || (pinned_opener && cross_site()));
        if peek_candidate && !self.peek_is_popup() {
            self.open_peek(tab, &url, opener, popup, now, fx);
            return;
        }
        // A popup Peek (login flow) is never replaced, and activating a tab would close it: the
        // new tab opens in the background instead.
        let keep_popup_peek = peek_candidate && self.peek_is_popup();
        let item = Tab { id: tab, url, created_at: now, last_active_at: now, opener, ..Tab::default() };
        self.state.items.insert(tab, Item::Tab(item));
        let anchor = opener.filter(|o| self.state.items.contains_key(o));
        self.place_in_today(tab, anchor);
        self.touch();
        if (foreground || popup) && !keep_popup_peek {
            self.activate(tab, now, fx);
        } else if !self.sidebar_shown() {
            self.toast(
                "New tab opened",
                Some(ToastAction { label: "Show".into(), command: Box::new(Command::ActivateItem { id: tab }) }),
            );
        }
    }

    /// Toast for a URL web content may not open (`urls::web_content_may_open`).
    pub(super) fn refuse_web_url(&mut self, url: &str) {
        let message = match crate::urls::scheme(url.trim()) {
            Some(scheme) if scheme.len() <= 24 => format!("Blocked a {scheme}: link"),
            _ => "Blocked a link that can't be opened".to_string(),
        };
        self.toast(message, None);
    }

    /// `LinkOpenRequested` rules.
    pub(super) fn link_open(&mut self, opener: Id, url: String, disposition: LinkDisposition, now: Millis, fx: &mut Vec<Effect>) {
        let url = url.trim().to_string();
        if url.is_empty() {
            return;
        }
        let opener_url = self.tab(opener).map(|t| t.url.clone());
        if !crate::urls::web_content_may_open(&url, opener_url.as_deref()) {
            self.refuse_web_url(&url);
            return;
        }
        let opener_opt = self.tab(opener).is_some().then_some(opener);
        match disposition {
            LinkDisposition::Preview => self.preview_link(url, opener_opt, now, fx),
            LinkDisposition::ForegroundTab => {
                self.open_url(url, OpenTarget::NewTab, opener_opt, false, now, fx);
            }
            LinkDisposition::BackgroundTab => {
                self.open_url(url, OpenTarget::BackgroundTab, opener_opt, false, now, fx);
            }
            LinkDisposition::NewWindow | LinkDisposition::PinnedCrossSite => {
                let pinned = opener_opt.is_some_and(|o| matches!(self.tab_section(o), Some(Section::Pinned | Section::Favorites)));
                let candidate = self.state.settings.peek_enabled && (disposition == LinkDisposition::NewWindow || pinned);
                if candidate && !self.peek_is_popup() {
                    let id = self.alloc_id();
                    self.open_peek(id, &url, opener_opt, false, now, fx);
                } else if candidate {
                    // Never replace (or close, by activating a tab) a popup Peek.
                    self.open_url(url, OpenTarget::BackgroundTab, opener_opt, false, now, fx);
                } else {
                    self.open_url(url, OpenTarget::NewTab, opener_opt, false, now, fx);
                }
            }
        }
    }

    /// `LinkDisposition::Preview` (Alt+click): the user asked for *this* URL in Peek, whatever tab
    /// the link is in.
    ///
    /// - Peek off in settings → a foreground Today tab, like Shift+click with Peek off.
    /// - Alt+click **inside Peek** replaces the page Peek shows instead of nesting, and keeps the
    ///   Peek's own opener so Split still splits against the tab the Peek came from.
    /// - A **popup** Peek (a sign-in window) is only replaced by a gesture made inside it; an
    ///   Alt+click in another tab must not throw away a flow the user is in the middle of, so that
    ///   link opens in a background tab.
    fn preview_link(&mut self, url: String, opener: Option<Id>, now: Millis, fx: &mut Vec<Effect>) {
        if !self.state.settings.peek_enabled {
            self.open_url(url, OpenTarget::NewTab, opener, false, now, fx);
            return;
        }
        let from_peek = opener.is_some() && opener == self.peek_tab();
        if self.peek_is_popup() && !from_peek {
            self.open_url(url, OpenTarget::BackgroundTab, opener, false, now, fx);
            return;
        }
        // Inside Peek the gesture's opener is the Peek tab itself, which `close_peek` is about to
        // destroy: inherit the Peek's opener so Expand-to-split still has a tab to split with.
        let opener = if from_peek { self.rt.peek.as_ref().and_then(|p| p.tab.opener) } else { opener };
        let opener = opener.filter(|o| self.tab(*o).is_some());
        let id = self.alloc_id();
        self.open_peek(id, &url, opener, false, now, fx);
    }
}
