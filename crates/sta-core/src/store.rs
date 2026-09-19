//! The store: persisted [`State`] + [`History`] + runtime-only data, mutated only by
//! [`Store::apply`]. Single-threaded; the shell keeps it on the CEF UI thread.
//!
//! ## Semantics summary (details on each [`Command`] variant)
//! - Tabs are *loaded* (browser exists) or *unloaded*. Only the active space's active item is
//!   loaded at startup; everything else loads on activation.
//! - Activation updates `last_active_at`, MRU (`WindowState::mru`), `Space::active_item`, emits
//!   `CreateBrowser` (if needed) → `ShowContent` → `FocusBrowser`.
//! - Every mutation that changes anything visible bumps `revision`; persisted-data mutations mark
//!   state and/or history dirty.
//! - Unknown ids / invalid operations are ignored (optionally with a toast), never panic.
//! - After `WindowCloseRequested`/`Quit`/`WindowControl{Close}` (→ `[SaveNow, Quit]`), every
//!   command is ignored and nothing is marked dirty, so browser teardown can't archive tabs.
//!
//! ## Implementation structure
//! Command handlers (`store/handlers.rs`, `lifecycle.rs`, `split.rs`) only mutate the model and
//! emit *imperative* effects (`DestroyBrowser`, `LoadUrl`, `Find`, …). After every command,
//! `reconcile` diffs the model against what was last emitted to the shell and emits the
//! *declarative* effects (`SetChrome`, `SetSidebar`, `CreateBrowser` for visible tabs,
//! `ShowContent`, `ShowPeek`/`HidePeek`, `FocusBrowser`, overlay show/hide) in a fixed order, so
//! "minimal effects" and the effect guarantees hold by construction. Reconcile also does the
//! activation bookkeeping that follows a focus change (MRU, `last_active_at`, clearing the opener
//! of the tab that stopped being active) and repairs dangling `Space::active_item`s.

mod agent;
mod devtools;
mod events;
mod extensions;
mod foreign;
mod handlers;
mod invariants;
mod lifecycle;
mod load;
mod omni;
mod split;
mod tree;
mod view_state;

use crate::command::*;
use crate::effect::*;
use crate::history::History;
use crate::model::*;
use crate::omnibox::{OmniboxRequest, OmniboxResponse};
use crate::view::*;
use crate::{Id, Millis};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

pub(crate) use tree::Parent;

/// Default toast duration.
pub const TOAST_MS: u32 = 2500;
/// Toast duration when the toast has an action button.
pub const TOAST_ACTION_MS: u32 = 6000;
/// Minimum share of a split pane (`SetSplitFractions`).
pub const MIN_PANE_FRACTION: f32 = 0.1;
/// Downloads kept in the runtime list (the UI shows the newest 20).
const MAX_DOWNLOADS: usize = 50;

pub struct Store {
    state: State,
    history: History,
    rt: Runtime,
    revision: u64,
    dirty: DirtyFlags,
}

/// What needs saving.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DirtyFlags {
    pub state: bool,
    pub history: bool,
}

impl DirtyFlags {
    pub fn any(self) -> bool {
        self.state || self.history
    }
}

/// Result of [`Store::load`]. When a file is corrupt the shell must quarantine it
/// (`persist::quarantine`) before the next save overwrites it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LoadReport {
    pub state_corrupt: bool,
    pub history_corrupt: bool,
    /// Human-readable repairs (dropped orphan items, fixed ids, ...).
    pub warnings: Vec<String>,
}

/// Runtime-only data (never persisted).
#[derive(Debug, Default)]
struct Runtime {
    tabs: BTreeMap<Id, TabRuntime>,
    /// Tabs for which core emitted `DestroyBrowser` and awaits `TabBrowserClosed`.
    closing: BTreeSet<Id>,
    /// Tabs whose `CreateBrowser` waits for a pending close to finish, with the URL to load.
    pending_create: BTreeMap<Id, String>,
    /// Tabs whose browser must be destroyed but is kept alive for an in-progress download
    /// (CEF stops reporting a download once its browser is gone). Logically unloaded.
    deferred_destroy: BTreeSet<Id>,
    shutting_down: bool,
    /// `Store::startup` ran: view effects may be emitted.
    started: bool,
    system_dark: bool,
    /// Windows "Animation effects" (`SPI_GETCLIENTAREAANIMATION`) as the shell last reported it.
    system_animations: SystemAnimations,
    /// Where the update the shell is working on stands (`crate::update`); runtime only.
    update: crate::update::UpdateStatus,
    window_maximized: bool,
    window_fullscreen: bool,
    window_focused: bool,
    page_fullscreen: Option<Id>,
    command_bar: Option<CommandBarView>,
    find: Option<FindView>,
    toast: Option<ToastView>,
    sidebar_panel: Option<SidebarPanelView>,
    /// The persisted-hidden sidebar is shown only for the open `sidebar_panel` (docked for a panel
    /// that holds input, floating for a transient one).
    sidebar_revealed: bool,
    /// Tabs whose next browser's first commit is a history visit: newly opened tabs and user
    /// navigations of unloaded tabs. Other (re)loads (activation, session restore, archive
    /// restore) create browsers whose first commit is not a visit.
    visit_first_commit: BTreeSet<Id>,
    /// Switcher tabs (MRU order) and selection.
    switcher: Option<(Vec<Id>, usize)>,
    /// Newest first.
    downloads: Vec<Download>,
    /// Oldest first.
    permission_prompts: Vec<PermissionPromptView>,
    peek: Option<PeekState>,
    /// Tab to focus at the end of the current command (if still visible).
    focus_request: Option<Id>,
    /// The command being applied was committed from the command bar (typed transition).
    omnibox_commit: bool,
    /// Content-focused tab seen by the last reconcile (activation bookkeeping).
    last_focused: Option<Id>,
    emitted: Emitted,
    /// Shared counter for command bar / find / sidebar panel `seq`.
    seq: u64,
    toast_seq: u64,
    /// Next candidate of `alloc_id` once `State::next_id` is exhausted (0 = start at `MAX_ID`).
    alloc_down: Id,
    archive_revision: u64,
    history_revision: u64,
    /// Something visible changed during the current command.
    bumped: bool,
    /// AI agents (MCP): sessions, prompts, agent tabs (store/agent.rs).
    agent: agent::AgentRuntime,
    /// `ExtensionInstalled` ids and when (dropped after 60 s; store/foreign.rs).
    recent_installs: BTreeMap<String, Millis>,
    /// Tabs opened for Chrome-created browsers, newest last (store/foreign.rs rate budget; only
    /// the timestamps inside the window are kept).
    foreign_opens: VecDeque<Millis>,
    /// "An extension wants to open …" toasts, same window (its own budget, so the ask path can't
    /// be used to spam toasts).
    foreign_asks: VecDeque<Millis>,
    /// When the "keeps opening windows" toast was last shown.
    foreign_blocked_at: Option<Millis>,
    /// DevTools may open on `sta://` pages (debug builds, `STA_DEVTOOLS_INTERNAL=1`).
    devtools_internal: bool,
    /// Tabs whose DevTools are open, and those whose DevTools are undocked. Never persisted
    /// (store/devtools.rs; Chrome does not restore open DevTools either).
    devtools_open: BTreeSet<Id>,
    devtools_undocked: BTreeSet<Id>,
    /// Installed extensions as the shell last read them (A–Z; store/extensions.rs). The profile is
    /// the state, so nothing here is persisted.
    extensions: Vec<crate::extensions::ExtensionInfo>,
    /// Permission warnings / host access / source loaded on demand for the Turn on disclosure.
    extension_details: BTreeMap<String, crate::extensions::ExtensionDetails>,
    /// When each of those disclosures was loaded: a Turn on is only allowed while its own disclosure
    /// is fresh, and the entry is consumed by it (store/extensions.rs, SEC-P3-5).
    extension_disclosed: BTreeMap<String, Millis>,
    /// The tab each extension's own page was last opened in ("one options tab per extension", even
    /// for a page that routes itself on load).
    extension_pages: BTreeMap<String, Id>,
    /// The backend operation currently running (`id`, what it does): one at a time.
    extension_busy: Option<(String, crate::extensions::ExtensionOp)>,
    /// The open action-popup card.
    extension_popup: Option<crate::extensions::ExtensionPopupView>,
    /// The "added by other programs" toast was shown this run (the *set* the user has been told
    /// about is `state.announced_external`, which survives the run).
    external_announced: bool,
    /// The Web Store's "Switch to Chrome" notice was already answered this run.
    web_store_noted: bool,
    /// The low-disk-space toast was shown in this run (`Command::LowDiskSpace`).
    low_disk_noted: bool,
    /// sta started in safe mode (two crashes; R-SEC-11).
    safe_mode: bool,
}

/// `SPI_GETCLIENTAREAANIMATION`, which is **on** until the shell says otherwise (a `Runtime`
/// built before the first `SystemAnimationsChanged` must not claim Windows dislikes animation).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SystemAnimations(bool);

impl Default for SystemAnimations {
    fn default() -> Self {
        Self(true)
    }
}

#[derive(Debug, Clone, Default)]
struct TabRuntime {
    /// A browser exists (or its creation was requested).
    loaded: bool,
    loading: bool,
    progress: f32,
    can_go_back: bool,
    can_go_forward: bool,
    audible: bool,
    crashed: bool,
    load_error: Option<String>,
    /// URL of the last failed main-frame load (Reload retries it).
    failed_url: Option<String>,
    zoom_level: f64,
    /// Show a zoom toast for the next `TabZoomChanged` (the zoom came from a `Zoom` command).
    zoom_toast: bool,
    /// Incremented on every `CreateBrowser`, so `ShowContent` is re-sent for a new view.
    generation: u64,
    /// `TabBrowserCreated` arrived for the current browser generation (adopted popups: at once).
    created: bool,
    /// The current generation's `CreateBrowser` completed a deferred create when the previous
    /// browser's `TabBrowserClosed` arrived. Until `TabBrowserCreated` confirms it, another close
    /// report is a stale duplicate for that previous browser and is ignored.
    after_close: bool,
    /// A close report was ignored as stale for this unconfirmed generation: `Tick`s seen since.
    /// If the browser is still unconfirmed at the second `Tick`, the report was a creation failure
    /// after all and is handled like one.
    ignored_close_ticks: Option<u8>,
    /// The browser was created with the trusted UI client (`sta://`).
    internal: bool,
    /// URL the browser was created with / last told to load.
    browser_url: String,
    /// Main-frame URL last committed by the *current* browser (`None` right after creation).
    /// A commit of a different URL is a history visit; a same-URL commit (reload) is not.
    committed_url: Option<String>,
    /// The current browser lazily (re)loads the tab: its first commit is not a history visit.
    quiet_first_commit: bool,
    /// Typed navigation awaiting its commit (history transition `Typed`).
    pending_typed: Option<String>,
    find_text: String,
    find_match_case: bool,
    /// What "Translate page" is doing to this tab's *current document*. Runtime only: it is cleared
    /// on a cross-document commit, on unload and when the browser is rebuilt, because the page
    /// script that holds the originals dies with the document.
    translate: crate::translate::TranslateStatus,
}

#[derive(Debug, Clone)]
struct PeekState {
    tab: Tab,
    popup: bool,
}

/// What the shell was last told (diffed by `reconcile`).
#[derive(Debug, Default)]
struct Emitted {
    chrome: Option<(crate::theme::ChromeArgb, bool)>,
    /// `(docked, width, floating)` of the last `SetSidebar`.
    sidebar: Option<(bool, u32, bool)>,
    layout: Option<ContentLayout>,
    generations: Vec<(Id, u64)>,
    peek: Option<Id>,
    page_fullscreen: Option<Id>,
    command_bar: Option<u64>,
    find: Option<(Id, u64)>,
    permission: Option<(u64, Id)>,
    switcher: bool,
    toast: Option<u64>,
}

/// Chrome's zoom percentage for a CEF zoom level (`100 × 1.2^level`, rounded).
pub fn zoom_percent(level: f64) -> u32 {
    if !level.is_finite() {
        return 100;
    }
    (100.0 * 1.2f64.powf(level)).round().clamp(1.0, 100_000.0) as u32
}

impl Store {
    /// Fresh profile: one space ("Home", 🏠, default theme), no tabs.
    pub fn new(now: Millis) -> Self {
        let mut state = State::default();
        let id = state.next_id;
        state.next_id += 1;
        state.spaces.push(Space { id, name: "Home".into(), icon: "🏠".into(), created_at: now, ..Space::default() });
        state.window.active_space = id;
        Self::from_state(state, History::default(), DirtyFlags { state: true, history: false })
    }

    fn from_state(state: State, history: History, dirty: DirtyFlags) -> Self {
        Self { state, history, rt: Runtime { window_focused: true, ..Runtime::default() }, revision: 1, dirty }
    }

    /// Load from saved JSON (either may be `None` = no file). Tolerant: parses items one by one,
    /// drops orphans and broken references, repairs invariants (at least one space, unique ids,
    /// favorites ≤ 12, splits only in Today, `next_id` > max id, finite floats). Ids above
    /// `MAX_ID / 2` (or a `nextId` that high) make it renumber every id to `1..=n` in order,
    /// rewriting every reference; the report then carries a "renumbered" warning.
    pub fn load(state_json: Option<&str>, history_json: Option<&str>, now: Millis) -> (Self, LoadReport) {
        load::load(state_json, history_json, now)
    }

    /// Effects to run once the window exists (after `SystemThemeChanged` was applied):
    /// `SetChrome`, `SetSidebar`, then either restore the session (`Startup::RestoreSession`:
    /// load + show the active space's active item) or show `Empty`; then open `urls`
    /// (command-line arguments, already absolute URLs) as new foreground Today tabs. Also runs the
    /// auto-archive pass for tabs that expired while closed.
    ///
    /// With `Startup::NewTab` and no `urls`, the command bar opens in `NewTab` mode.
    pub fn startup(&mut self, urls: Vec<String>, now: Millis) -> Vec<Effect> {
        if self.rt.shutting_down {
            return Vec::new();
        }
        let mut fx = Vec::new();
        self.rt.started = true;
        self.rt.emitted = Emitted::default();
        self.repair_active_items(now);
        // Safe mode (two crashes in a row): every tab stays unloaded until the user picks one.
        self.apply_safe_mode_startup();
        match self.state.settings.startup {
            Startup::RestoreSession => self.rt.focus_request = self.content_focused_tab(),
            Startup::NewTab => {
                let active = self.state.window.active_space;
                if let Some(space) = self.space_mut(active)
                    && space.active_item.take().is_some()
                {
                    self.dirty.state = true;
                }
                if urls.iter().all(|u| u.trim().is_empty()) {
                    self.open_command_bar(CommandBarMode::NewTab, None);
                }
            }
        }
        for url in urls {
            if let Some(id) = self.open_url(url, OpenTarget::NewTab, None, false, now, &mut fx) {
                // Every command-line URL loads, not just the last (visible) one.
                if !self.is_live(id) {
                    let u = self.url_to_load(id);
                    self.load_tab(id, u, &mut fx);
                }
            }
        }
        self.auto_archive(now, &mut fx);
        self.reconcile(now, &mut fx);
        self.finish_command();
        fx
    }

    /// Apply one command. Never panics on bad input.
    pub fn apply(&mut self, cmd: Command, now: Millis) -> Vec<Effect> {
        if self.rt.shutting_down {
            return Vec::new();
        }
        let mut fx = Vec::new();
        self.handle(cmd, now, &mut fx);
        if !self.rt.shutting_down {
            self.reconcile(now, &mut fx);
        }
        self.finish_command();
        fx
    }

    fn finish_command(&mut self) {
        if std::mem::take(&mut self.rt.bumped) {
            self.revision += 1;
        }
    }

    /// Full snapshot for the HTML UI.
    pub fn ui_state(&self) -> UiState {
        self.build_ui_state()
    }

    /// What may animate right now (`UiState.motion`), for the shell's own timing decisions
    /// (hide fades, the `SetChrome` midpoint) without building a whole snapshot.
    pub fn motion_view(&self) -> crate::motion::MotionView {
        crate::motion::MotionView::of(&self.state.settings.animations, self.rt.system_animations.0)
    }

    /// Command bar results for the request (pure; does not change revision).
    pub fn omnibox(&self, req: &OmniboxRequest, now: Millis) -> OmniboxResponse {
        omni::query(self, req, now)
    }

    /// Every command bar action available in the current state (group `Actions`, sorted A–Z),
    /// including per-space "Go to Space: X" / "Move Tab to Space: X".
    pub fn omnibox_actions(&self) -> Vec<crate::omnibox::OmniboxResult> {
        omni::all_actions(self)
    }

    /// Archive page rows, newest first.
    pub fn archive_list(&self) -> Vec<ArchiveEntryView> {
        self.build_archive_list()
    }

    /// History page rows (fuzzy `query`, empty = most recent).
    pub fn history_list(&self, query: &str, limit: usize, now: Millis) -> Vec<HistoryEntryView> {
        self.history
            .search(query, limit, now)
            .into_iter()
            .map(|u| HistoryEntryView {
                url: u.url.clone(),
                title: crate::urls::display_title(None, &u.title, &u.url),
                host: crate::urls::display_host(&u.url),
                visit_count: u.visit_count,
                last_visit_at: u.last_visit_at,
            })
            .collect()
    }

    /// Full boost (with CSS/JS) for the boosts page (`invoke('boosts.get', {id})`).
    pub fn boost(&self, id: Id) -> Option<Boost> {
        self.state.boosts.iter().find(|b| b.id == id).cloned()
    }

    /// Colors for an arbitrary theme in the current mode (`invoke('theme.colors', {theme})`).
    pub fn theme_colors(&self, theme: &Theme) -> ThemeColors {
        crate::theme::colors(theme, self.is_dark())
    }

    /// Monotonic counter bumped whenever `ui_state()` would change.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// What changed since the last call (and resets the flags).
    pub fn take_dirty(&mut self) -> DirtyFlags {
        std::mem::take(&mut self.dirty)
    }

    /// Serialized `state.json` contents (pretty-printed).
    pub fn state_json(&self) -> String {
        serde_json::to_string_pretty(&self.state).unwrap_or_default()
    }

    /// Serialized `history.json` contents (compact).
    pub fn history_json(&self) -> String {
        serde_json::to_string(&self.history).unwrap_or_default()
    }

    /// Allocate a fresh id (used by the shell for adopted popups before `PopupAdopted`). Never
    /// returns 0 or more than [`crate::MAX_ID`].
    pub fn alloc_id(&mut self) -> Id {
        let id = self.state.next_id.max(1);
        if id <= crate::MAX_ID {
            self.state.next_id = id + 1;
            self.dirty.state = true;
            return id;
        }
        // The counter is exhausted. `Store::load` renumbers profiles long before that, so this
        // needs an id at the very top handed to `PopupAdopted`: hand out unused ids from the top
        // down (ids there were never allocated by the counter), never the same one twice.
        let used = self.ids_in_use();
        let mut candidate = match self.rt.alloc_down {
            0 => crate::MAX_ID,
            c => c,
        };
        for _ in 0..=used.len() {
            if candidate == 0 {
                candidate = crate::MAX_ID;
            }
            if !used.contains(&candidate) {
                break;
            }
            candidate -= 1;
        }
        self.rt.alloc_down = candidate.saturating_sub(1);
        candidate
    }

    /// Every id referenced by persisted or runtime data (items, spaces, boosts, archive entries
    /// and their split snapshots, the Peek tab).
    fn ids_in_use(&self) -> BTreeSet<Id> {
        let st = &self.state;
        let mut used: BTreeSet<Id> = st.items.keys().copied().collect();
        used.extend(st.spaces.iter().map(|s| s.id));
        used.extend(st.boosts.iter().map(|b| b.id));
        for e in &st.archive {
            used.insert(e.id);
            if let Some(snap) = &e.split {
                used.insert(snap.group);
                used.extend(snap.panes.iter().copied());
            }
        }
        used.extend(self.peek_tab());
        used.extend(self.rt.tabs.keys().copied());
        used
    }

    pub fn is_shutting_down(&self) -> bool {
        self.rt.shutting_down
    }

    pub fn settings(&self) -> &Settings {
        &self.state.settings
    }

    /// Persisted window state. `sidebar_visible` is the docked setting; while a hidden sidebar is
    /// docked for a panel that holds input, `ui_state().window.sidebar_visible` (and
    /// `SetSidebar.visible`) say `true`.
    pub fn window_state(&self) -> &WindowState {
        &self.state.window
    }

    /// A tab by id, including the runtime-only Peek tab.
    pub fn tab(&self, id: Id) -> Option<&Tab> {
        match self.state.items.get(&id) {
            Some(Item::Tab(t)) => Some(t),
            Some(_) => None,
            None => self.rt.peek.as_ref().map(|p| &p.tab).filter(|t| t.id == id),
        }
    }

    /// Section of a tab (a split pane reports Today; the Peek tab reports `None`).
    pub fn tab_section(&self, id: Id) -> Option<Section> {
        match self.state.items.get(&id) {
            Some(Item::Tab(_)) => self.section_of(id),
            _ => None,
        }
    }

    /// Focused tab (Peek if open, else active tab, or focused pane of the active split).
    pub fn focused_tab(&self) -> Option<Id> {
        self.peek_tab().or_else(|| self.content_focused_tab())
    }

    pub fn peek_tab(&self) -> Option<Id> {
        self.rt.peek.as_ref().map(|p| p.tab.id)
    }

    /// The open sidebar panel (`UiState.sidebarPanel`), if any.
    pub fn sidebar_panel(&self) -> Option<&SidebarPanel> {
        self.rt.sidebar_panel.as_ref().map(|p| &p.panel)
    }

    /// Synchronous read-only check used by the shell in `RequestHandler::on_before_browse` for a
    /// main-frame navigation of `tab`. Returns `Some(disposition)` when the navigation should be
    /// cancelled and reported as `LinkOpenRequested` instead: currently a user-gesture,
    /// non-redirect link navigation from a pinned/favorite tab to a different registrable domain
    /// while Peek is enabled → `PinnedCrossSite`.
    ///
    /// The site of the pinned tab is taken from `pinned_url` (IPs/localhost compare by host).
    pub fn intercept_navigation(&self, tab: Id, url: &str, user_gesture: bool, is_redirect: bool) -> Option<LinkDisposition> {
        if !user_gesture || is_redirect || !self.state.settings.peek_enabled || !crate::urls::is_web(url) {
            return None;
        }
        if !matches!(self.tab_section(tab), Some(Section::Pinned | Section::Favorites)) {
            return None;
        }
        let pinned = self.tab(tab)?.pinned_url.as_deref()?;
        let from = crate::urls::site_key(pinned)?;
        let to = crate::urls::site_key(url)?;
        (from != to).then_some(LinkDisposition::PinnedCrossSite)
    }

    /// Enabled boosts whose host matches `url`.
    pub fn boosts_for_url(&self, url: &str) -> Vec<Boost> {
        self.state
            .boosts
            .iter()
            .filter(|b| b.enabled && crate::urls::host_matches(&b.host, url))
            .cloned()
            .collect()
    }

    /// Effective dark mode.
    pub fn is_dark(&self) -> bool {
        match self.state.settings.appearance {
            Appearance::System => self.rt.system_dark,
            Appearance::Light => false,
            Appearance::Dark => true,
        }
    }

    fn active_theme(&self) -> Theme {
        self.state
            .spaces
            .iter()
            .find(|s| s.id == self.state.window.active_space)
            .map(|s| s.theme.clone())
            .unwrap_or_default()
    }

    /// Native frame color (ARGB) of the active space in the effective mode.
    pub fn frame_argb(&self) -> u32 {
        crate::theme::frame_argb(&self.active_theme(), self.is_dark())
    }

    /// Accent color (ARGB) of the active space: the focused split pane's wrapper background.
    pub fn accent_argb(&self) -> u32 {
        crate::theme::accent_argb(&self.active_theme(), self.is_dark())
    }

    /// Every native chrome color (ARGB) of the active space in the effective mode (`SetChrome`).
    pub fn chrome_argb(&self) -> crate::theme::ChromeArgb {
        crate::theme::chrome_argb(&self.active_theme(), self.is_dark())
    }

    /// Read-only access for tests and diagnostics.
    pub fn state(&self) -> &State {
        &self.state
    }

    /// Read-only history access for tests and diagnostics.
    pub fn history(&self) -> &History {
        &self.history
    }

    /// Whether the tab currently has a browser (or one is being created for it).
    pub fn is_loaded(&self, tab: Id) -> bool {
        self.is_live(tab) || self.rt.pending_create.contains_key(&tab)
    }

    /// Layout the content area should currently show.
    pub fn content_layout(&self) -> ContentLayout {
        self.desired_layout()
    }

    /// Debug invariant checker (tests, diagnostics): every item in exactly one container, splits
    /// only in Today, favorites ≤ 12, folders only in Pinned, pinned tabs have `pinned_url`, Today
    /// tabs don't, `next_id` > every id, active items exist, visible tabs are loaded, …
    pub fn check_invariants(&self) -> Result<(), Vec<String>> {
        invariants::check(self)
    }

    // ------------------------------------------------------------------------------ helpers

    /// Mark a visible change (revision bump at the end of the command).
    fn bump(&mut self) {
        self.rt.bumped = true;
    }

    /// Mark a persisted, visible change.
    fn touch(&mut self) {
        self.rt.bumped = true;
        self.dirty.state = true;
    }

    fn touch_archive(&mut self) {
        self.touch();
        self.rt.archive_revision += 1;
    }

    fn touch_history(&mut self) {
        self.rt.bumped = true;
        self.dirty.history = true;
        self.rt.history_revision += 1;
    }

    fn next_seq(&mut self) -> u64 {
        self.rt.seq += 1;
        self.rt.seq
    }

    fn trt(&mut self, id: Id) -> &mut TabRuntime {
        self.rt.tabs.entry(id).or_default()
    }

    /// Browser exists, isn't being destroyed and isn't a download-kept zombie.
    fn is_live(&self, id: Id) -> bool {
        self.rt.tabs.get(&id).is_some_and(|r| r.loaded)
            && !self.rt.closing.contains(&id)
            && !self.rt.deferred_destroy.contains(&id)
    }

    /// The sidebar is visible: docked, or floating open for a transient panel.
    fn sidebar_shown(&self) -> bool {
        self.sidebar_docked() || self.sidebar_floating()
    }

    /// The sidebar is docked in the window: the persisted setting, or a hidden sidebar revealed
    /// for an open panel that holds input (space sheets, inline rename: they need keyboard focus,
    /// which the floating overlay can't take).
    fn sidebar_docked(&self) -> bool {
        self.state.window.sidebar_visible
            || (self.rt.sidebar_revealed && self.rt.sidebar_panel.as_ref().is_some_and(|p| !p.panel.is_transient()))
    }

    /// A hidden sidebar floats over the content, pinned open for a transient panel (downloads,
    /// app menu) while the page keeps keyboard focus.
    fn sidebar_floating(&self) -> bool {
        !self.state.window.sidebar_visible
            && self.rt.sidebar_revealed
            && self.rt.sidebar_panel.as_ref().is_some_and(|p| p.panel.is_transient())
    }

    fn toast(&mut self, message: impl Into<String>, action: Option<ToastAction>) {
        let duration = if action.is_some() { TOAST_ACTION_MS } else { TOAST_MS };
        self.toast_for(message, action, duration);
    }

    fn toast_for(&mut self, message: impl Into<String>, action: Option<ToastAction>, duration_ms: u32) {
        self.rt.toast_seq += 1;
        self.rt.toast = Some(ToastView { id: self.rt.toast_seq, message: message.into(), action, duration_ms });
        self.bump();
    }

    fn open_command_bar(&mut self, mode: CommandBarMode, split_side: Option<SplitSide>) {
        let seq = self.next_seq();
        self.rt.command_bar = Some(CommandBarView { mode, text: String::new(), split_side, seq });
        self.bump();
    }

    /// Mutable tab (items or Peek).
    fn tab_any_mut(&mut self, id: Id) -> Option<&mut Tab> {
        if let Some(Item::Tab(t)) = self.state.items.get_mut(&id) {
            return Some(t);
        }
        self.rt.peek.as_mut().map(|p| &mut p.tab).filter(|t| t.id == id)
    }

    /// Set a tab's URL (persisted for items, runtime for Peek).
    fn set_tab_url(&mut self, id: Id, url: &str) {
        let persisted = self.state.items.contains_key(&id);
        if let Some(t) = self.tab_any_mut(id)
            && t.url != url
        {
            t.url = url.to_string();
            if persisted {
                self.touch();
            } else {
                self.bump();
            }
        }
    }

    // ------------------------------------------------------------------------------ reconcile

    fn reconcile(&mut self, now: Millis, fx: &mut Vec<Effect>) {
        if !self.rt.started {
            return;
        }
        self.repair_active_items(now);
        self.validate_runtime(fx);
        self.track_focus(now);

        let peek = self.peek_tab();
        // 1. Peek closed or replaced.
        if let Some(p) = self.rt.emitted.peek
            && peek != Some(p)
        {
            fx.push(Effect::HidePeek { tab: p });
            self.rt.emitted.peek = None;
        }
        // 2. Overlays that closed.
        if self.rt.emitted.command_bar.is_some() && self.rt.command_bar.is_none() {
            fx.push(Effect::HideCommandBar);
            self.rt.emitted.command_bar = None;
        }
        if self.rt.emitted.find.is_some() && self.rt.find.is_none() {
            fx.push(Effect::HideFindBar);
            self.rt.emitted.find = None;
        }
        if self.rt.emitted.switcher && self.rt.switcher.is_none() {
            fx.push(Effect::HideSwitcher);
            self.rt.emitted.switcher = false;
        }
        if self.rt.emitted.toast.is_some() && self.rt.toast.is_none() {
            fx.push(Effect::HideToast);
            self.rt.emitted.toast = None;
        }
        // 3. Native chrome.
        let chrome = (self.chrome_argb(), self.is_dark());
        if self.rt.emitted.chrome != Some(chrome) {
            let (c, dark) = chrome;
            fx.push(Effect::SetChrome {
                frame_argb: c.frame,
                dark,
                accent_argb: c.accent,
                surface_argb: c.surface,
                border_argb: c.border,
                frame_border_argb: c.frame_border,
            });
            self.rt.emitted.chrome = Some(chrome);
        }
        if self.rt.sidebar_revealed && self.rt.sidebar_panel.is_none() {
            self.rt.sidebar_revealed = false;
            self.bump();
        }
        let sidebar = (self.sidebar_docked(), self.state.window.sidebar_width, self.sidebar_floating());
        if self.rt.emitted.sidebar != Some(sidebar) {
            fx.push(Effect::SetSidebar { visible: sidebar.0, width: sidebar.1, floating: sidebar.2 });
            self.rt.emitted.sidebar = Some(sidebar);
        }
        // 4. Leaving page fullscreen.
        if self.rt.emitted.page_fullscreen.is_some() && self.rt.page_fullscreen.is_none() {
            fx.push(Effect::SetPageFullscreen { tab: None });
            self.rt.emitted.page_fullscreen = None;
        }
        // 5. Content: create browsers for visible tabs, then show the layout.
        for t in self.layout_tab_ids() {
            if !self.is_live(t) && !self.rt.pending_create.contains_key(&t) {
                let url = self.url_to_load(t);
                self.load_tab(t, url, fx);
            }
        }
        if let Some(p) = peek
            && !self.is_live(p) && !self.rt.pending_create.contains_key(&p)
        {
            let url = self.url_to_load(p);
            self.load_tab(p, url, fx);
        }
        let layout = self.desired_layout();
        let generations: Vec<(Id, u64)> =
            layout.tabs().into_iter().map(|t| (t, self.rt.tabs.get(&t).map_or(0, |r| r.generation))).collect();
        if self.rt.emitted.layout.as_ref() != Some(&layout) || self.rt.emitted.generations != generations {
            fx.push(Effect::ShowContent { layout: layout.clone() });
            self.rt.emitted.layout = Some(layout.clone());
            self.rt.emitted.generations = generations;
        }
        // 6. Peek shown.
        if let Some(p) = peek
            && self.rt.emitted.peek != Some(p) && self.is_live(p)
        {
            fx.push(Effect::ShowPeek { tab: p });
            self.rt.emitted.peek = Some(p);
        }
        // 7. Entering page fullscreen.
        if let Some(t) = self.rt.page_fullscreen
            && self.rt.emitted.page_fullscreen != Some(t)
        {
            fx.push(Effect::SetPageFullscreen { tab: Some(t) });
            self.rt.emitted.page_fullscreen = Some(t);
        }
        // 8. Focus.
        if let Some(t) = self.rt.focus_request.take() {
            let visible = layout.tabs().contains(&t) || peek == Some(t);
            let peek_ok = peek.is_none() || peek == Some(t);
            if visible && peek_ok && self.rt.command_bar.is_none() && self.is_live(t) {
                fx.push(Effect::FocusBrowser { tab: t });
            }
        }
        // 9. Overlays shown / re-targeted.
        let prompt = self.shown_prompt();
        if let Some(seq) = self.rt.command_bar.as_ref().map(|c| c.seq)
            && self.rt.emitted.command_bar != Some(seq)
        {
            fx.push(Effect::ShowCommandBar);
            self.rt.emitted.command_bar = Some(seq);
        }
        if let Some(key) = self.rt.find.as_ref().map(|f| (f.tab, f.seq))
            && self.rt.emitted.find != Some(key)
        {
            fx.push(Effect::ShowFindBar { tab: key.0 });
            self.rt.emitted.find = Some(key);
        }
        match prompt {
            Some(key) if self.rt.emitted.permission != Some(key) => {
                fx.push(Effect::ShowPermissionPrompt { tab: key.1 });
                self.rt.emitted.permission = Some(key);
            }
            None if self.rt.emitted.permission.is_some() => {
                fx.push(Effect::HidePermissionPrompt);
                self.rt.emitted.permission = None;
            }
            _ => {}
        }
        if self.rt.switcher.is_some() && !self.rt.emitted.switcher {
            fx.push(Effect::ShowSwitcher);
            self.rt.emitted.switcher = true;
        }
        if let Some(id) = self.rt.toast.as_ref().map(|t| t.id)
            && self.rt.emitted.toast != Some(id)
        {
            fx.push(Effect::ShowToast);
            self.rt.emitted.toast = Some(id);
        }
        // 10. AI agents.
        self.reconcile_agent(fx);
    }

    /// Close overlays / drop runtime references that no longer make sense.
    fn validate_runtime(&mut self, fx: &mut Vec<Effect>) {
        let focused = self.focused_tab();
        if let Some(f) = &self.rt.find
            && (Some(f.tab) != focused || !self.is_live(f.tab))
        {
            let tab = f.tab;
            if self.is_live(tab) {
                fx.push(Effect::StopFinding { tab });
            }
            self.rt.find = None;
            self.bump();
        }
        if let Some((tabs, _)) = &self.rt.switcher
            && tabs.iter().any(|t| self.tab(*t).is_none())
        {
            self.rt.switcher = None;
            self.bump();
        }
        if let Some(t) = self.rt.page_fullscreen {
            let visible = self.layout_tab_ids().contains(&t) || self.peek_tab() == Some(t);
            if !visible || !self.is_live(t) {
                if self.is_live(t) {
                    fx.push(Effect::ExitPageFullscreen { tab: t });
                }
                self.rt.page_fullscreen = None;
                self.bump();
            }
        }
        let before = self.rt.permission_prompts.len();
        let gone: Vec<u64> = self
            .rt
            .permission_prompts
            .iter()
            .filter(|p| self.tab(p.tab).is_none() && !self.rt.tabs.get(&p.tab).is_some_and(|r| r.loaded))
            .map(|p| p.id)
            .collect();
        self.rt.permission_prompts.retain(|p| !gone.contains(&p.id));
        if self.rt.permission_prompts.len() != before {
            self.bump();
        }
        let panel_valid = match self.rt.sidebar_panel.as_ref().map(|p| &p.panel) {
            Some(SidebarPanel::EditSpace { id }) => self.space(*id).is_some(),
            Some(SidebarPanel::RenameItem { id }) => {
                matches!(self.state.items.get(id), Some(Item::Tab(_) | Item::Folder(_)))
            }
            // A tab that was unpinned, archived or closed meanwhile has no pinned page to edit.
            Some(SidebarPanel::EditPinned { id }) => matches!(self.tab_section(*id), Some(Section::Pinned | Section::Favorites)),
            _ => true,
        };
        if !panel_valid {
            self.rt.sidebar_panel = None;
            self.bump();
        }
        // The popup card belongs to the pane it was opened over: a tab switch, a split change or a
        // permission prompt on that pane takes it away (SEC-4: the card never sits over a prompt).
        if self.rt.extension_popup.is_some() {
            let tab = self.extension_popup_tab();
            let gone = tab.is_some_and(|t| !(self.layout_tab_ids().contains(&t) || self.peek_tab() == Some(t)) || !self.is_live(t));
            let prompted = self.shown_prompt().is_some();
            if gone || prompted {
                self.close_extension_popup_if_open(fx);
            }
        }
        if !self.rt.visit_first_commit.is_empty() {
            let gone: Vec<Id> = self.rt.visit_first_commit.iter().copied().filter(|t| self.tab(*t).is_none()).collect();
            for t in gone {
                self.rt.visit_first_commit.remove(&t);
            }
        }
    }

    /// Activation bookkeeping when the content-focused tab changed.
    fn track_focus(&mut self, now: Millis) {
        let cur = self.content_focused_tab();
        if cur == self.rt.last_focused {
            return;
        }
        if let Some(prev) = self.rt.last_focused
            && Some(prev) != cur
            && let Some(Item::Tab(t)) = self.state.items.get_mut(&prev)
            && t.opener.take().is_some()
        {
            self.dirty.state = true;
        }
        if let Some(c) = cur {
            let mru = &mut self.state.window.mru;
            mru.retain(|x| *x != c);
            mru.insert(0, c);
            mru.truncate(MAX_MRU);
            for t in self.layout_tab_ids() {
                if let Some(Item::Tab(tab)) = self.state.items.get_mut(&t) {
                    tab.last_active_at = now;
                }
            }
            self.dirty.state = true;
        }
        self.rt.last_focused = cur;
    }

    /// The permission prompt the overlay shows: the oldest one whose tab is visible.
    fn shown_prompt(&self) -> Option<(u64, Id)> {
        let visible = self.layout_tab_ids();
        let peek = self.peek_tab();
        self.rt
            .permission_prompts
            .iter()
            .find(|p| (visible.contains(&p.tab) || peek == Some(p.tab)) && self.is_live(p.tab))
            .map(|p| (p.id, p.tab))
    }
}
