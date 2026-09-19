//! Persisted data model (serialized to `state.json`). Runtime-only data lives in
//! [`crate::store`] (`Runtime`), never here.
//!
//! Sidebar structure (Arc model, see `docs/research/arc_spec.md` §1–§2):
//! - **Favorites**: `State::favorites`, shared by all spaces, max [`MAX_FAVORITES`], tabs only,
//!   each with `pinned_url`.
//! - **Pinned**: `Space::pinned`, per space, may contain tabs and folders (nestable, max depth
//!   [`MAX_FOLDER_DEPTH`]). Pinned tabs always have `pinned_url`. No splits.
//! - **Today**: `Space::today`, per space, tabs and split groups (no folders), auto-archived
//!   after `Settings::archive_after_hours` of inactivity. `pinned_url == None`.
//!
//! Containers hold ordered `Vec<Id>` of item ids; the items themselves live in `State::items`.
//! Every item id appears in exactly one container, except split panes, which are referenced only
//! by their [`Split`] (panes are never direct children of a container). The Peek tab is runtime
//! only and never stored here. All persisted floats are finite (sanitized on apply and load).

use crate::{Id, Millis};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Version of `state.json`. 2: search suggestions work and default to on (a version 1 profile
/// gets `Settings::search_suggestions = true` on load; the setting was hidden and inert before).
pub const STATE_VERSION: u32 = 2;
pub const MAX_FAVORITES: usize = 12;
pub const MAX_FOLDER_DEPTH: usize = 3;
pub const MAX_SPLIT_PANES: usize = 4;
pub const MAX_REOPEN_STACK: usize = 25;
pub const MAX_MRU: usize = 50;
pub const ARCHIVE_RETENTION_MS: Millis = 30 * 24 * 3600 * 1000;
pub const SIDEBAR_MIN_WIDTH: u32 = 200;
pub const SIDEBAR_MAX_WIDTH: u32 = 440;
pub const SIDEBAR_DEFAULT_WIDTH: u32 = 248;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct State {
    pub version: u32,
    pub next_id: Id,
    pub settings: Settings,
    /// Ordered list of spaces. Never empty after load (a default space is created).
    pub spaces: Vec<Space>,
    /// Favorite tab ids (grid order). Max [`MAX_FAVORITES`].
    pub favorites: Vec<Id>,
    /// All tabs, folders and split groups by id.
    pub items: BTreeMap<Id, Item>,
    /// Newest first.
    pub archive: Vec<ArchiveEntry>,
    pub boosts: Vec<Boost>,
    pub window: WindowState,
    /// Manual-close undo stack (newest last). Max [`MAX_REOPEN_STACK`].
    pub reopen: Vec<ReopenEntry>,
    /// Remembered site permission decisions.
    pub site_permissions: Vec<SitePermission>,
    /// Ids of extensions another program registered that the user has already been told about
    /// (D11a, `store/extensions.rs`). Ignoring them is a decision, so it is remembered: the flag
    /// used to live in the runtime half only, which meant a machine whose three registry extensions
    /// the user will never allow was interrupted by the same toast on *every* launch, forever.
    /// Settings › Extensions keeps the standing banner either way.
    pub announced_external: Vec<String>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            version: STATE_VERSION,
            next_id: 1,
            settings: Settings::default(),
            spaces: Vec::new(),
            favorites: Vec::new(),
            items: BTreeMap::new(),
            archive: Vec::new(),
            boosts: Vec::new(),
            window: WindowState::default(),
            reopen: Vec::new(),
            site_permissions: Vec::new(),
            announced_external: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Space {
    pub id: Id,
    pub name: String,
    /// A single emoji (or short text) shown as the space icon.
    pub icon: String,
    pub theme: Theme,
    /// Top-level pinned item ids (tabs, folders).
    pub pinned: Vec<Id>,
    /// Today item ids (tabs, splits), newest at index 0.
    pub today: Vec<Id>,
    /// Last active item (tab or split) in this space; restored when switching to the space.
    /// May be a favorite tab id.
    pub active_item: Option<Id>,
    pub created_at: Millis,
}

impl Default for Space {
    fn default() -> Self {
        Self {
            id: 0,
            name: "Space".into(),
            icon: "✨".into(),
            theme: Theme::default(),
            pinned: Vec::new(),
            today: Vec::new(),
            active_item: None,
            created_at: 0,
        }
    }
}

/// Space color theme, specified in OKLCH hue/chroma. Concrete colors are derived by
/// [`crate::theme::colors`] so native panels and HTML use identical sRGB values.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Theme {
    /// Primary hue in degrees [0, 360).
    pub hue: f32,
    /// Secondary hue for the sidebar gradient.
    pub hue2: f32,
    /// OKLCH chroma, 0.0..=0.08 (clamped when deriving colors).
    pub chroma: f32,
}

impl Default for Theme {
    fn default() -> Self {
        // "Dusk" preset
        Self { hue: 300.0, hue2: 340.0, chroma: 0.06 }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Item {
    Tab(Tab),
    Folder(Folder),
    Split(Split),
}

impl Item {
    pub fn id(&self) -> Id {
        match self {
            Item::Tab(t) => t.id,
            Item::Folder(f) => f.id,
            Item::Split(s) => s.id,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Tab {
    pub id: Id,
    /// Last committed URL (or the URL to load when unloaded; for unloaded pinned tabs this is
    /// `pinned_url`).
    pub url: String,
    /// Page title as reported by the page (may be empty).
    pub title: String,
    /// User rename; overrides `title` for display.
    pub custom_title: Option<String>,
    /// Best favicon URL reported by the page (an opaque `<img src>` value for the UI).
    pub favicon: Option<String>,
    /// `Some` for Favorites and Pinned tabs: the URL the tab resets/reopens to.
    pub pinned_url: Option<String>,
    pub created_at: Millis,
    /// Drives auto-archive and MRU. Updated on activation and on `Tick` while visible.
    pub last_active_at: Millis,
    /// Tab that opened this one; cleared when this tab stops being active after having been
    /// active (so "activate opener on close" only applies if the user never left).
    pub opener: Option<Id>,
    pub muted: bool,
}

impl Default for Tab {
    fn default() -> Self {
        Self {
            id: 0,
            url: String::new(),
            title: String::new(),
            custom_title: None,
            favicon: None,
            pinned_url: None,
            created_at: 0,
            last_active_at: 0,
            opener: None,
            muted: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Folder {
    pub id: Id,
    pub name: String,
    pub collapsed: bool,
    /// Child item ids (tabs, folders), ordered.
    pub children: Vec<Id>,
}

impl Default for Folder {
    fn default() -> Self {
        Self { id: 0, name: "New Folder".into(), collapsed: false, children: Vec::new() }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Split {
    pub id: Id,
    pub orientation: Orientation,
    /// Tab ids of the panes, 2..=[`MAX_SPLIT_PANES`], in visual order (left→right / top→bottom).
    pub panes: Vec<Id>,
    /// Pane sizes; same length as `panes`, each > 0, sum ≈ 1.0.
    pub fractions: Vec<f32>,
    /// Index into `panes` of the focused pane.
    pub focused: usize,
}

impl Default for Split {
    fn default() -> Self {
        Self { id: 0, orientation: Orientation::Horizontal, panes: Vec::new(), fractions: Vec::new(), focused: 0 }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Orientation {
    /// Panes side by side.
    #[default]
    Horizontal,
    /// Panes stacked.
    Vertical,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Section {
    Favorites,
    Pinned,
    Today,
}

/// An archived tab. `id` is the original tab id; restoring reuses it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ArchiveEntry {
    pub id: Id,
    pub url: String,
    pub title: String,
    pub custom_title: Option<String>,
    pub favicon: Option<String>,
    /// Pinned/favorite tabs keep their pinned URL (FolderDeleted / SpaceDeleted).
    pub pinned_url: Option<String>,
    pub archived_at: Millis,
    pub reason: ArchiveReason,
    pub space: Option<Id>,
    pub section: Section,
    /// Folder the tab was in (Pinned only).
    pub folder: Option<Id>,
    /// Index inside its original container at archive time (the split's index for panes).
    pub index: usize,
    /// Set for tabs archived as part of a split group.
    pub split: Option<SplitSnapshot>,
}

impl Default for ArchiveEntry {
    fn default() -> Self {
        Self {
            id: 0,
            url: String::new(),
            title: String::new(),
            custom_title: None,
            favicon: None,
            pinned_url: None,
            archived_at: 0,
            reason: ArchiveReason::UserClosed,
            space: None,
            section: Section::Today,
            folder: None,
            index: 0,
            split: None,
        }
    }
}

/// Enough to rebuild a split group from its archived panes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SplitSnapshot {
    /// The original split id (shared by all panes of the group).
    pub group: Id,
    pub orientation: Orientation,
    pub fractions: Vec<f32>,
    pub focused: usize,
    /// This entry's pane position.
    pub pane_index: usize,
    /// Tab ids of all panes at archive time (this one included), in order. Lets a restored pane
    /// find its neighbors by id and rebuild a split that dissolved into one of them. Empty in
    /// profiles saved before it existed (such panes never rejoin a dissolved split).
    #[serde(default)]
    pub panes: Vec<Id>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ArchiveReason {
    Auto,
    UserClosed,
    ClearToday,
    SpaceDeleted,
    FolderDeleted,
}

/// Undo stack entries for Ctrl+Shift+T (manual closes only; auto-archive never pushes).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum ReopenEntry {
    /// A Today tab (or single split pane) that was archived by the user.
    Archived { archive_id: Id },
    /// A pinned tab / favorite that was unloaded; reload it at `url` (its URL at close time).
    Unloaded { tab: Id, url: String },
    /// Clear Today: restore all in original order, activate none.
    Batch { archive_ids: Vec<Id> },
    /// A whole split group closed: rebuild it and activate it.
    Split { archive_ids: Vec<Id> },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Boost {
    pub id: Id,
    pub name: String,
    /// Host to match; also matches subdomains (`example.com` matches `www.example.com`).
    pub host: String,
    pub enabled: bool,
    pub css: String,
    pub js: String,
    pub created_at: Millis,
    pub updated_at: Millis,
}

impl Default for Boost {
    fn default() -> Self {
        Self {
            id: 0,
            name: String::new(),
            host: String::new(),
            enabled: true,
            css: String::new(),
            js: String::new(),
            created_at: 0,
            updated_at: 0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct WindowState {
    /// Last normal (restored) bounds in DIP screen coordinates.
    pub bounds: Option<Rect>,
    pub maximized: bool,
    pub active_space: Id,
    pub sidebar_visible: bool,
    pub sidebar_width: u32,
    /// Most-recently-used tab ids, most recent first. Max [`MAX_MRU`].
    pub mru: Vec<Id>,
}

impl Default for WindowState {
    fn default() -> Self {
        Self {
            bounds: None,
            maximized: false,
            active_space: 0,
            sidebar_visible: true,
            sidebar_width: SIDEBAR_DEFAULT_WIDTH,
            mru: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    pub search_engine: SearchEngineId,
    /// Template with `{q}` placeholder, used when `search_engine == Custom`.
    pub custom_search_url: String,
    /// Auto-archive idle Today tabs after this many hours: 12, 24, 168 (7d) or 720 (30d).
    pub archive_after_hours: u32,
    pub appearance: Appearance,
    pub startup: Startup,
    /// Open links from Pinned tabs/Favorites to other sites (and feature popups) in Peek.
    pub peek_enabled: bool,
    /// Absolute download directory; `None` = the user's Downloads folder (shell resolves).
    pub download_dir: Option<String>,
    pub ask_download_location: bool,
    /// Fetch search suggestions for command bar input from the selected engine (shell:
    /// `omnibox.suggest`). Engines without a suggestion endpoint (Kagi, Perplexity, Custom) get none.
    pub search_suggestions: bool,
    /// Which animations play, and how much motion at all (`crate::motion`). Not version-bumped:
    /// a profile from before this field loads the defaults (everything on, following Windows).
    pub animations: crate::motion::AnimationSettings,
    // ---------------------------------------------------------------- AI agents (MCP), docs/MCP.md
    /// Whether AI agents may connect through the MCP bridge. Unknown values load as `Off`.
    pub agent_access: AgentAccess,
    /// Which tabs agents see: the tabs they opened (and tabs shared with them), or all tabs.
    pub agent_scope: AgentScope,
    /// Ask before an agent acts on a new site (`Ask`), or allow every site (`All`).
    pub agent_sites: AgentSites,
    /// Whether agents may run page scripts (`evaluate`, not in the MVP tool set).
    pub agent_scripts: AgentScripts,
    /// Agents may search browsing history.
    pub agent_history: bool,
    /// Agents may list downloads (file names and states only).
    pub agent_downloads: bool,
    /// Agents may open private-network hosts (RFC 1918, link-local, `.local`). Loopback is always
    /// allowed.
    pub agent_allow_private_network: bool,
    /// Hosts agents may never open or act on (a host matches itself and its subdomains).
    pub agent_blocked_hosts: Vec<String>,
    /// Sites answered "Always" in a site prompt (registrable domain, or host for IPs/localhost).
    pub agent_allowed_sites: Vec<String>,
    /// Clients answered "Always allow" (keyed by host executable path and Authenticode signer).
    pub agent_trusted_clients: Vec<AgentTrustedClient>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            search_engine: SearchEngineId::Google,
            custom_search_url: String::new(),
            archive_after_hours: 12,
            appearance: Appearance::System,
            startup: Startup::RestoreSession,
            peek_enabled: true,
            download_dir: None,
            ask_download_location: false,
            search_suggestions: true,
            animations: crate::motion::AnimationSettings::default(),
            agent_access: AgentAccess::Off,
            agent_scope: AgentScope::AgentTabs,
            agent_sites: AgentSites::Ask,
            agent_scripts: AgentScripts::Off,
            agent_history: false,
            agent_downloads: false,
            agent_allow_private_network: false,
            agent_blocked_hosts: Vec::new(),
            agent_allowed_sites: Vec::new(),
            agent_trusted_clients: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SearchEngineId {
    #[default]
    Google,
    Bing,
    DuckDuckGo,
    Ecosia,
    Brave,
    Kagi,
    Perplexity,
    Custom,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Appearance {
    #[default]
    System,
    Light,
    Dark,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Startup {
    #[default]
    RestoreSession,
    NewTab,
}

/// History visit transition (core-derived; CEF has no "typed" transition).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Transition {
    Link,
    Typed,
    Bookmark,
    Reload,
    Redirect,
    FormSubmit,
    BackForward,
    Other,
}

/// A download tracked for the sidebar UI (runtime only).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Download {
    /// CEF download item id.
    pub id: u32,
    /// Tab whose browser started the download. CEF stops reporting progress once that browser is
    /// destroyed, so core defers destroying it while the download is in progress.
    pub tab: Option<Id>,
    pub url: String,
    pub file_name: String,
    /// Full target path once known.
    pub path: Option<String>,
    pub received_bytes: i64,
    /// `None` when the server didn't send a length.
    pub total_bytes: Option<i64>,
    pub bytes_per_sec: i64,
    pub state: DownloadState,
    pub started_at: Millis,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DownloadState {
    InProgress,
    Paused,
    Complete,
    Cancelled,
    Interrupted,
}

/// Permission kinds (subset of CEF media access + permission request types we surface).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PermissionKind {
    Camera,
    Microphone,
    ScreenCapture,
    Geolocation,
    Notifications,
    Clipboard,
    MidiSysex,
    StorageAccess,
    Other,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SitePermission {
    /// Origin, e.g. `https://meet.example.com`.
    pub origin: String,
    pub kind: PermissionKind,
    pub allow: bool,
}

// ---------------------------------------------------------------------------------- AI agents (MCP)

/// `Settings::agent_access`. Any unknown value deserializes as `Off` (never quarantines the profile).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AgentAccess {
    /// Read tools only: never loads, shows or changes a tab.
    ReadOnly,
    Full,
    // `#[serde(other)]` must be the last variant.
    #[default]
    #[serde(other)]
    Off,
}

/// `Settings::agent_scope`. Unknown values deserialize as `AgentTabs`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AgentScope {
    AllTabs,
    /// Tabs the agent opened, their popups, and tabs the user shared with it.
    #[default]
    #[serde(other)]
    AgentTabs,
}

/// `Settings::agent_sites`. Unknown values deserialize as `Ask`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AgentSites {
    All,
    #[default]
    #[serde(other)]
    Ask,
}

/// `Settings::agent_scripts`. Unknown values deserialize as `Off`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AgentScripts {
    /// Isolated world only (the page can't see or tamper with the script).
    Isolated,
    /// The page's own world too.
    Main,
    #[default]
    #[serde(other)]
    Off,
}

/// An MCP client the user allowed permanently ("Always"), identified by the executable that
/// hosts the bridge and its Authenticode signer. Unsigned hosts can't be trusted permanently.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AgentTrustedClient {
    /// Display name from the client (e.g. "Claude Code").
    pub name: String,
    /// Full path of the host executable.
    pub exe: String,
    /// Authenticode signer subject of `exe`.
    pub signer: String,
    pub added_at: Millis,
}

impl Default for AgentTrustedClient {
    fn default() -> Self {
        Self { name: String::new(), exe: String::new(), signer: String::new(), added_at: 0 }
    }
}
