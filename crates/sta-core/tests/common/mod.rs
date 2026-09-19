//! Scenario-test harness: a fake shell that executes effects against a model of live browsers,
//! checks the effect guarantees, the store invariants, revision bumps and dirty flags after every
//! command, and (optionally) answers `CreateBrowser`/`DestroyBrowser` with the shell events.
#![allow(dead_code)]

use sta_core::*;
use std::collections::{BTreeSet, VecDeque};

pub const T0: Millis = 1_700_000_000_000;
pub const MIN: Millis = 60_000;
pub const HOUR: Millis = 3_600_000;
pub const DAY: Millis = 24 * HOUR;

pub struct Harness {
    pub store: Store,
    pub now: Millis,
    /// Browsers that exist in the fake shell.
    pub live: BTreeSet<Id>,
    /// Browsers the fake shell is closing.
    pub closing: BTreeSet<Id>,
    /// Answer CreateBrowser / DestroyBrowser automatically.
    pub auto: bool,
    pub queue: VecDeque<Command>,
    pub layout: Option<ContentLayout>,
    pub peek_shown: Option<Id>,
    pub chrome: Option<(u32, bool)>,
    /// `(docked, width, floating)` of the last `SetSidebar`.
    pub sidebar: Option<(bool, u32, bool)>,
    /// Every effect executed since the harness was created.
    pub history: Vec<Effect>,
}

impl Harness {
    /// Fresh store + startup.
    pub fn new() -> Self {
        Self::start(Store::new(T0), Vec::new())
    }

    pub fn start(store: Store, urls: Vec<String>) -> Self {
        Self::start_at(store, urls, T0)
    }

    /// Startup at a given wall-clock time.
    pub fn start_at(store: Store, urls: Vec<String>, now: Millis) -> Self {
        let mut h = Harness {
            store,
            now,
            live: BTreeSet::new(),
            closing: BTreeSet::new(),
            auto: true,
            queue: VecDeque::new(),
            layout: None,
            peek_shown: None,
            chrome: None,
            sidebar: None,
            history: Vec::new(),
        };
        h.store.apply(Command::SystemThemeChanged { dark: false }, h.now);
        let fx = h.store.startup(urls, h.now);
        h.execute(&fx, "startup");
        h.verify("startup");
        h.drain();
        h
    }

    pub fn advance(&mut self, ms: Millis) {
        self.now += ms;
    }

    /// Apply one command (plus the automatic shell answers it triggers); returns the effects of
    /// this command only.
    pub fn apply(&mut self, cmd: Command) -> Vec<Effect> {
        let fx = self.apply_one(cmd);
        self.drain();
        fx
    }

    /// Apply one command and return the effects of it and of all automatic follow-ups.
    pub fn apply_all(&mut self, cmd: Command) -> Vec<Effect> {
        let mut fx = self.apply_one(cmd);
        while let Some(next) = self.queue.pop_front() {
            fx.extend(self.apply_one(next));
        }
        fx
    }

    fn drain(&mut self) {
        while let Some(next) = self.queue.pop_front() {
            self.apply_one(next);
        }
    }

    fn apply_one(&mut self, cmd: Command) -> Vec<Effect> {
        let label = format!("{cmd:?}");
        if let Command::TabBrowserClosed { tab } = &cmd {
            // The fake shell's browser is gone before the store hears about it.
            self.closing.remove(tab);
            self.live.remove(tab);
        }
        let mut before_ui = self.store.ui_state();
        let before_rev = self.store.revision();
        let before_state = self.store.state_json();
        let before_history = self.store.history_json();
        self.store.take_dirty();
        let fx = self.store.apply(cmd, self.now);
        self.execute(&fx, &label);
        let dirty = self.store.take_dirty();
        if !self.store.is_shutting_down() {
            let mut after_ui = self.store.ui_state();
            before_ui.revision = 0;
            after_ui.revision = 0;
            if before_ui != after_ui {
                assert!(self.store.revision() > before_rev, "{label}: UiState changed without a revision bump");
            }
            if self.store.state_json() != before_state {
                assert!(dirty.state, "{label}: state changed without dirty.state");
            }
            if self.store.history_json() != before_history {
                assert!(dirty.history, "{label}: history changed without dirty.history");
            }
            self.verify(&label);
        } else {
            assert!(!dirty.any() || fx.contains(&Effect::Quit), "{label}: dirty after shutdown");
        }
        fx
    }

    fn verify(&mut self, label: &str) {
        if let Err(errors) = self.store.check_invariants() {
            panic!("{label}: invariants violated:\n  {}", errors.join("\n  "));
        }
        if self.store.is_shutting_down() {
            return;
        }
        let chrome = (self.store.frame_argb(), self.store.is_dark());
        assert_eq!(self.chrome, Some(chrome), "{label}: SetChrome not in sync");
        // Effective placement: a hidden sidebar is revealed while a sidebar panel is open, docked
        // for a panel that holds input and floating for a transient one.
        let ui = self.store.ui_state();
        let w = &ui.window;
        let persisted = self.store.window_state().sidebar_visible;
        let transient_panel = ui.sidebar_panel.as_ref().is_some_and(|p| p.panel.is_transient());
        let floating = !persisted && transient_panel;
        assert_eq!(self.sidebar, Some((w.sidebar_visible, w.sidebar_width, floating)), "{label}: SetSidebar not in sync");
        assert_eq!(w.sidebar_visible, persisted || (ui.sidebar_panel.is_some() && !transient_panel), "{label}: docked state");
        assert!(!(w.sidebar_visible && floating), "{label}: docked and floating at once");
        assert_eq!(self.layout.as_ref(), Some(&self.store.content_layout()), "{label}: shown layout not in sync");
        assert_eq!(self.peek_shown, self.store.peek_tab(), "{label}: peek visibility not in sync");
    }

    fn assert_live(&self, tab: Id, what: &str, label: &str) {
        assert!(self.live.contains(&tab) && !self.closing.contains(&tab), "{label}: {what} for tab {tab} without a live browser");
    }

    fn execute(&mut self, fx: &[Effect], label: &str) {
        for e in fx {
            self.history.push(e.clone());
            match e {
                Effect::CreateBrowser { tab, .. } => {
                    assert!(!self.live.contains(tab), "{label}: CreateBrowser twice for tab {tab}");
                    self.live.insert(*tab);
                    if self.auto {
                        self.queue.push_back(Command::TabBrowserCreated { tab: *tab });
                    }
                }
                Effect::ReplaceBrowser { tab, .. } => self.assert_live(*tab, "ReplaceBrowser", label),
                Effect::DestroyBrowser { tab } => {
                    self.assert_live(*tab, "DestroyBrowser", label);
                    self.closing.insert(*tab);
                    if self.auto {
                        self.queue.push_back(Command::TabBrowserClosed { tab: *tab });
                    }
                }
                Effect::ShowContent { layout } => {
                    for t in layout.tabs() {
                        self.assert_live(t, "ShowContent", label);
                    }
                    if let ContentLayout::Split { panes, focused, .. } = layout {
                        assert!(panes.len() >= 2 && *focused < panes.len(), "{label}: bad split layout");
                        let sum: f32 = panes.iter().map(|p| p.fraction).sum();
                        assert!((sum - 1.0).abs() < 0.01, "{label}: fractions sum {sum}");
                    }
                    if self.peek_shown.is_some_and(|p| layout.tabs().contains(&p)) {
                        self.peek_shown = None;
                    }
                    self.layout = Some(layout.clone());
                }
                Effect::ShowPeek { tab } => {
                    self.assert_live(*tab, "ShowPeek", label);
                    self.peek_shown = Some(*tab);
                }
                Effect::HidePeek { tab } => {
                    assert_eq!(self.peek_shown, Some(*tab), "{label}: HidePeek for a peek that isn't shown");
                    self.peek_shown = None;
                }
                Effect::FocusBrowser { tab }
                | Effect::LoadUrl { tab, .. }
                | Effect::GoBack { tab }
                | Effect::GoForward { tab }
                | Effect::Reload { tab, .. }
                | Effect::StopLoad { tab }
                | Effect::Zoom { tab, .. }
                | Effect::SetAudioMuted { tab, .. }
                | Effect::OpenDevTools { tab, .. }
                | Effect::CloseDevTools { tab }
                | Effect::FocusDevTools { tab }
                | Effect::InspectAt { tab, .. }
                | Effect::Print { tab }
                | Effect::Find { tab, .. }
                | Effect::StopFinding { tab }
                | Effect::ExitPageFullscreen { tab }
                | Effect::StartDownload { tab, .. }
                | Effect::ShowFindBar { tab }
                | Effect::ShowPermissionPrompt { tab } => self.assert_live(*tab, &format!("{e:?}"), label),
                Effect::SetChrome { frame_argb, dark, .. } => self.chrome = Some((*frame_argb, *dark)),
                Effect::SetSidebar { visible, width, floating } => self.sidebar = Some((*visible, *width, *floating)),
                _ => {}
            }
        }
    }

    /// Deliver `TabBrowserClosed` for a browser the shell was closing (manual mode).
    pub fn finish_close(&mut self, tab: Id) -> Vec<Effect> {
        assert!(self.closing.contains(&tab), "tab {tab} is not closing");
        self.closing.remove(&tab);
        self.live.remove(&tab);
        self.apply(Command::TabBrowserClosed { tab })
    }

    // ------------------------------------------------------------------ helpers

    pub fn ui(&self) -> UiState {
        self.store.ui_state()
    }

    pub fn space(&self) -> Id {
        self.store.window_state().active_space
    }

    pub fn space_data(&self, id: Id) -> Space {
        self.store.state().spaces.iter().find(|s| s.id == id).cloned().expect("space")
    }

    pub fn today(&self) -> Vec<Id> {
        self.space_data(self.space()).today
    }

    pub fn pinned(&self) -> Vec<Id> {
        self.space_data(self.space()).pinned
    }

    pub fn favorites(&self) -> Vec<Id> {
        self.store.state().favorites.clone()
    }

    pub fn active(&self) -> Option<Id> {
        self.space_data(self.space()).active_item
    }

    pub fn focused(&self) -> Option<Id> {
        self.store.focused_tab()
    }

    pub fn tab(&self, id: Id) -> Tab {
        self.store.tab(id).cloned().unwrap_or_else(|| panic!("tab {id}"))
    }

    pub fn split(&self, id: Id) -> Split {
        match self.store.state().items.get(&id) {
            Some(Item::Split(s)) => s.clone(),
            other => panic!("split {id}: {other:?}"),
        }
    }

    pub fn folder(&self, id: Id) -> Folder {
        match self.store.state().items.get(&id) {
            Some(Item::Folder(f)) => f.clone(),
            other => panic!("folder {id}: {other:?}"),
        }
    }

    /// Open a foreground tab and return its id.
    pub fn open(&mut self, url: &str) -> Id {
        self.apply(Command::OpenUrl { url: url.into(), target: OpenTarget::NewTab, opener: None });
        self.focused().expect("opened tab focused")
    }

    /// Open a tab and pin it; returns its id.
    pub fn open_pinned(&mut self, url: &str) -> Id {
        let id = self.open(url);
        self.apply(Command::TogglePin { id: Some(id) });
        id
    }

    /// Open a tab and make it a favorite.
    pub fn open_favorite(&mut self, url: &str) -> Id {
        let id = self.open(url);
        self.apply(Command::AddFavorite { id: Some(id) });
        id
    }

    /// Simulate the page committing a URL and title.
    pub fn commit(&mut self, tab: Id, url: &str, title: &str) {
        self.apply(Command::TabAddressChanged { tab, url: url.into() });
        self.apply(Command::TabTitleChanged { tab, title: title.into() });
    }

    /// The shell adopts a popup (its browser exists already).
    pub fn popup(&mut self, opener: Option<Id>, url: &str, popup: bool, foreground: bool) -> (Id, Vec<Effect>) {
        let id = self.store.alloc_id();
        self.live.insert(id);
        let fx = self.apply(Command::PopupAdopted { tab: id, opener, url: url.into(), popup, foreground });
        (id, fx)
    }

    /// The page closed itself (window.close()) or its creation failed.
    pub fn page_closed(&mut self, tab: Id) -> Vec<Effect> {
        self.live.remove(&tab);
        self.closing.remove(&tab);
        self.apply(Command::TabBrowserClosed { tab })
    }

    pub fn new_space(&mut self, name: &str, icon: &str, theme: Theme) -> Id {
        self.apply(Command::NewSpace { name: name.into(), icon: icon.into(), theme });
        self.space()
    }

    pub fn toast(&self) -> Option<ToastView> {
        self.ui().toast
    }
}

impl Default for Harness {
    fn default() -> Self {
        Self::new()
    }
}

pub fn is_create(e: &Effect, tab: Id) -> bool {
    matches!(e, Effect::CreateBrowser { tab: t, .. } if *t == tab)
}

pub fn is_destroy(e: &Effect, tab: Id) -> bool {
    matches!(e, Effect::DestroyBrowser { tab: t } if *t == tab)
}

pub fn has<F: Fn(&Effect) -> bool>(fx: &[Effect], f: F) -> bool {
    fx.iter().any(f)
}

pub fn position<F: Fn(&Effect) -> bool>(fx: &[Effect], f: F) -> Option<usize> {
    fx.iter().position(f)
}

pub fn shows(fx: &[Effect]) -> Option<ContentLayout> {
    fx.iter().rev().find_map(|e| if let Effect::ShowContent { layout } = e { Some(layout.clone()) } else { None })
}
