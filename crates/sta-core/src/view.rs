//! View model pushed to every HTML UI surface as the `state` event (full snapshot, coalesced by the
//! shell to ≤ 30 pushes/s). Everything the sidebar, top bar and overlays render comes from here,
//! including overlay *intents* (command bar text, find query, toast, sidebar panel) so a page that
//! (re)loads late can always recover them from `state.get`. Display strings are computed in core.
//!
//! "seq" fields: monotonically increasing counters; a surface resets/refocuses when the `seq` it
//! last handled changes.

use crate::command::{Command, CommandBarMode, SidebarPanel, SplitSide};
use crate::model::*;
use crate::{Id, Millis};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UiState {
    /// Store revision this snapshot was taken at (monotonic).
    pub revision: u64,
    /// Bumped when the archive list changes (archive page refetches `archive.list`).
    pub archive_revision: u64,
    /// Bumped when history changes (history page refetches `history.list`).
    pub history_revision: u64,
    /// Effective dark mode (settings.appearance resolved against the system theme).
    pub dark: bool,
    pub window: WindowView,
    pub active_space: Id,
    pub spaces: Vec<SpaceView>,
    pub favorites: Vec<TabView>,
    pub favorites_full: bool,
    /// Active item (tab or split id) of the active space.
    pub active_item: Option<Id>,
    /// Focused tab id (the tab itself, or the focused pane of the active split).
    pub focused_tab: Option<Id>,
    /// Details of the focused tab for the URL pill / top bar. `None` = empty state.
    pub current: Option<CurrentView>,
    pub peek: Option<PeekView>,
    /// Newest first (max 20).
    pub downloads: Vec<Download>,
    pub can_reopen: bool,
    pub archive_count: usize,
    /// Recent-tab switcher (open while `Some`).
    pub switcher: Option<SwitcherView>,
    /// Command bar (open while `Some`).
    pub command_bar: Option<CommandBarView>,
    /// Find bar (open while `Some`).
    pub find: Option<FindView>,
    pub toast: Option<ToastView>,
    /// Sidebar panel requested from outside the sidebar (open while `Some`).
    pub sidebar_panel: Option<SidebarPanelView>,
    /// Pending permission prompts, oldest first; the overlay shows the first one.
    pub permission_prompts: Vec<PermissionPromptView>,
    pub page_fullscreen: bool,
    /// Which animations may play, and how much motion at all (`crate::motion`). Every surface
    /// applies it with `theme.js applyMotion(state)` and gates each animation on its key.
    #[serde(default)]
    pub motion: crate::motion::MotionView,
    /// What the browser knows about a newer release (`crate::update`, Settings › About). Runtime
    /// state: never persisted, and `idle` until the first check answers.
    #[serde(default)]
    pub update: crate::update::UpdateStatus,
    pub settings: Settings,
    pub boosts: Vec<BoostSummary>,
    pub search_engines: Vec<SearchEngineInfo>,
    pub theme_presets: Vec<ThemePreset>,
    /// AI agents (MCP): sessions, approval prompts, recent activity.
    #[serde(default)]
    pub agent: crate::agent::AgentView,
    /// Installed extensions (Ctrl+E picker, Settings › Extensions, the popup card).
    #[serde(default)]
    pub extensions: crate::extensions::ExtensionsView,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WindowView {
    pub maximized: bool,
    pub fullscreen: bool,
    pub focused: bool,
    /// The sidebar is docked right now: the persisted setting, or a hidden sidebar docked while a
    /// `sidebarPanel` that holds input (space sheets, inline rename) is open (matches the last
    /// `SetSidebar.visible`). `false` while the hidden sidebar floats over the content (hover
    /// reveal, or pinned open for the downloads / app menu panel).
    pub sidebar_visible: bool,
    pub sidebar_width: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpaceView {
    pub id: Id,
    pub name: String,
    pub icon: String,
    pub theme: Theme,
    /// Colors resolved for the current `dark` mode.
    pub colors: ThemeColors,
    pub pinned: Vec<NodeView>,
    pub today: Vec<NodeView>,
    pub active_item: Option<Id>,
}

/// CSS colors. `frame`, `accent`, `surface` are always opaque `#rrggbb` (usable in `color-mix`);
/// the others may be `rgba(...)` meant to be layered over `frame` (sidebar) or `surface`
/// (overlays). `frame` is also the native panel color, so HTML must paint exactly `frame` at every
/// window-edge-adjacent area.
///
/// CSS custom property mapping used by all pages: `--frame --grad-start --grad-end --accent
/// --text --text-muted --hover --pressed --active-row --divider --surface --border`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThemeColors {
    pub frame: String,
    /// Subtle gradient stops for the sidebar body; both equal `frame` at the edges by design.
    pub gradient_start: String,
    pub gradient_end: String,
    pub accent: String,
    pub text: String,
    pub text_muted: String,
    pub hover: String,
    pub pressed: String,
    pub active_row: String,
    pub divider: String,
    pub surface: String,
    pub border: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum NodeView {
    Tab(TabView),
    Folder(FolderView),
    Split(SplitView),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TabView {
    pub id: Id,
    /// Display title: custom title → page title → host → url.
    pub title: String,
    pub url: String,
    /// Host without `www.` (or page name for internal pages).
    pub host: String,
    /// Opaque `<img src>` value (never interpolate into CSS/HTML).
    pub favicon: Option<String>,
    /// `None` for the Peek tab.
    pub section: Option<Section>,
    /// Owning space (`None` for favorites and Peek).
    pub space: Option<Id>,
    pub loaded: bool,
    pub loading: bool,
    pub audible: bool,
    pub muted: bool,
    pub crashed: bool,
    /// Last main-frame load failed (error page showing).
    pub failed: bool,
    /// Pinned/favorite tab whose URL differs from `pinned_url` (show "/" and reset affordance).
    pub navigated: bool,
    pub pinned_url: Option<String>,
    /// This tab is the active item or the focused pane of the active split.
    pub active: bool,
    /// Visible in the content area (active tab or any pane of the active split).
    pub visible: bool,
    /// In agent scope: opened by an agent or shared with agents (omitted when `false`).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub agent: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FolderView {
    pub id: Id,
    pub name: String,
    pub collapsed: bool,
    pub children: Vec<NodeView>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SplitView {
    pub id: Id,
    pub orientation: Orientation,
    pub panes: Vec<TabView>,
    pub fractions: Vec<f32>,
    pub focused: usize,
    pub active: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CurrentView {
    pub tab: Id,
    pub url: String,
    pub title: String,
    pub host: String,
    /// Text for the URL pill: host without www, "Settings" for internal pages, file name for file:.
    pub pill: String,
    /// https, sta, file, about:, data:, view-source of those → true; http → false.
    pub secure: bool,
    pub internal: bool,
    pub loading: bool,
    /// 0.0..=1.0
    pub progress: f32,
    pub can_go_back: bool,
    pub can_go_forward: bool,
    /// `None` for Peek.
    pub section: Option<Section>,
    pub navigated: bool,
    /// 100 = default.
    pub zoom_percent: u32,
    /// Boosts matching the URL (enabled or not), for the pill's paintbrush toggle.
    pub boosts: Vec<BoostSummary>,
    pub muted: bool,
    pub audible: bool,
    /// Error text when the last main-frame load failed.
    pub load_error: Option<String>,
    /// Number of panes when the active item is a split (0 otherwise).
    pub split_panes: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PeekView {
    pub tab: TabView,
    /// Opened by a feature popup (OAuth etc.): hide Split/Expand, no click-outside dismissal.
    pub popup: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SwitcherView {
    pub tabs: Vec<TabView>,
    pub selected: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandBarView {
    pub mode: CommandBarMode,
    /// Initial input text (e.g. current URL in EditUrl mode). Applied when `seq` changes.
    pub text: String,
    pub split_side: Option<SplitSide>,
    pub seq: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FindView {
    pub tab: Id,
    /// Remembered query for the tab (prefill).
    pub text: String,
    pub match_case: bool,
    /// Bumped on every `OpenFind` (refocus + select all).
    pub seq: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToastView {
    pub id: u64,
    pub message: String,
    pub action: Option<ToastAction>,
    pub duration_ms: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToastAction {
    pub label: String,
    pub command: Box<Command>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SidebarPanelView {
    pub panel: SidebarPanel,
    pub seq: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionPromptView {
    pub id: u64,
    pub tab: Id,
    pub origin: String,
    /// Host without `www.` for display.
    pub host: String,
    pub kinds: Vec<PermissionKind>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BoostSummary {
    pub id: Id,
    pub name: String,
    pub host: String,
    pub enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchEngineInfo {
    pub id: SearchEngineId,
    pub name: String,
    /// Template with `{q}`.
    pub url: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThemePreset {
    pub name: String,
    pub theme: Theme,
    /// Resolved for the current `dark` mode (for swatches and live previews).
    pub colors: ThemeColors,
}

/// Row for the archive page (`invoke('archive.list')`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArchiveEntryView {
    pub id: Id,
    pub url: String,
    pub title: String,
    pub host: String,
    pub favicon: Option<String>,
    pub archived_at: Millis,
    pub reason: ArchiveReason,
    pub space: Option<Id>,
    pub space_icon: Option<String>,
    pub space_name: Option<String>,
    /// Split snapshot group id (offer "Restore split").
    pub group: Option<Id>,
}

/// Row for the history page and command bar history results.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryEntryView {
    pub url: String,
    pub title: String,
    pub host: String,
    pub visit_count: u32,
    pub last_visit_at: Millis,
}
