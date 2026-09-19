//! Commands: every state change enters the store through [`crate::Store::apply`].
//!
//! Sources:
//! - HTML UI over IPC (`dispatch(command)`), as JSON: `{"type": "activateItem", "id": 5}`.
//!   Only commands for which [`Command::allowed_from_ui`] is `true` are accepted from IPC.
//! - Shell keyboard accelerators (mapped in the shell's keyboard table).
//! - Shell CEF callbacks (the "shell → core events" at the bottom).
//!
//! Conventions:
//! - `Option<Id>` targets mean "the focused tab/item" when `None` (focused = active item of the
//!   active space; for a split, its focused pane; Peek counts as focused while open).
//! - Commands referring to unknown ids, or invalid in the current state, are ignored (no effects,
//!   no panic), optionally with a toast.
//! - While the app is shutting down (after `WindowCloseRequested`/`Quit`), every command is ignored.

use crate::agent::AgentClientInfo;
use crate::extensions::{ExtensionAction, ExtensionDetails, ExtensionInfo, ExtensionOp};
use crate::model::*;
use crate::{Id, Millis};
use serde::{Deserialize, Serialize};

fn primary_action() -> ExtensionAction {
    ExtensionAction::Primary
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum Command {
    // ------------------------------------------------------------------ opening & navigation
    /// Text committed by the user (URL or search). Classified with
    /// [`crate::omnibox::classify`], then handled exactly like `OpenUrl`.
    OpenInput { text: String, target: OpenTarget },
    /// Open an exact URL.
    /// - `NewTab`: new Today tab at the top of the active space's Today, activated, page focused.
    /// - `BackgroundTab`: new Today tab directly below `opener` (else top of Today); browser is
    ///   created (hidden) but the active item is unchanged; toast "New tab opened" if the sidebar
    ///   is hidden.
    /// - `CurrentTab`: navigate the focused tab (falls back to `NewTab` if none). If the tab's
    ///   internal-ness (`sta://` vs web) differs from the URL's, use `Effect::ReplaceBrowser`.
    ///
    /// Internal pages already open anywhere are activated (and navigated if the URL differs,
    /// e.g. `sta://boosts/?id=5`) instead of duplicated.
    ///
    /// An external-protocol URL (`mailto:`, `tel:`, `zoommtg://…`: a scheme Chromium doesn't load,
    /// [`crate::urls::is_external_scheme`]) never creates or navigates a tab, whatever the target:
    /// core emits `Effect::OpenExternal{url}` and the OS opens it.
    ///
    /// URLs that come from web content (`LinkOpenRequested`, `PopupAdopted`, `OpenUrlAt`) must pass
    /// the allowlist [`crate::urls::web_content_may_open`]: `http:`/`https:`, `about:blank`,
    /// `blob:` of an http(s) origin, and `file:` only when the opening tab's own URL is `file:`.
    /// Anything else is refused with a "Blocked a `scheme:` link" toast.
    OpenUrl { url: String, target: OpenTarget, #[serde(default)] opener: Option<Id> },
    /// Open `url` as a new tab at a sidebar drop position (link dragged from a page). Refused with
    /// a toast for URLs outside the web-content allowlist (see `OpenUrl`; there is no opener, so
    /// `file:` is refused too).
    OpenUrlAt { url: String, to: DropTarget },
    /// Navigate `tab` (or the focused tab) to `url` (no classification); internal-ness rule as
    /// in `OpenUrl::CurrentTab`. External-protocol URLs → `Effect::OpenExternal` (see `OpenUrl`).
    Navigate { #[serde(default)] tab: Option<Id>, url: String },
    GoBack { #[serde(default)] tab: Option<Id> },
    GoForward { #[serde(default)] tab: Option<Id> },
    /// For a tab whose last load failed (`TabLoadFailed`), reloads the failed URL via `LoadUrl`.
    Reload { #[serde(default)] tab: Option<Id>, #[serde(default)] ignore_cache: bool },
    StopLoad { #[serde(default)] tab: Option<Id> },

    // ------------------------------------------------------------------ sidebar items
    /// Activate a tab or split: switches space if needed (favorites stay in the current space),
    /// loads the browser(s) if unloaded (pinned/favorite tabs load `pinned_url`), shows it,
    /// focuses it, updates MRU and `last_active_at`. Activating a pane tab id activates its split
    /// and focuses that pane. Closes Peek if open.
    ActivateItem { id: Id },
    /// Ctrl+1..9 (1-based): nth item in visual order (favorites, visible pinned rows depth-first
    /// skipping collapsed folders' children, today). `n >= 9` = last item.
    ActivateNth { n: u32 },
    /// Ctrl+Alt+Up/Down: previous/next item in visual order, no wrap.
    ActivateAdjacent { delta: i32 },
    /// Close (Ctrl+W semantics). `None` = Peek if open, else the focused pane / active item; if
    /// there is no active item either, behaves like `WindowCloseRequested` (Arc closes the window).
    /// - Today tab: archive (reason UserClosed), push `ReopenEntry::Archived`, destroy browser.
    /// - Pinned tab / favorite: unload (destroy browser, keep row, `url = pinned_url`), push
    ///   `ReopenEntry::Unloaded{url_at_close}`.
    /// - Split id: archive every pane (shared `SplitSnapshot`), push `ReopenEntry::Split`.
    /// - Split pane tab id: archive that pane only; a split left with one pane dissolves into a
    ///   normal Today tab in the split's place.
    /// - Folder: ignored (use `DeleteFolder`).
    ///
    /// After closing the active item, activate: its opener (if still in `items` and the closed
    /// tab was never left since it was opened — core clears `Tab.opener` when a tab stops being
    /// active) → most recent MRU tab of the same space (favorites count for every space) → Empty.
    CloseItem { #[serde(default)] id: Option<Id> },
    /// Ctrl+Shift+T: pop the reopen stack (skipping entries whose archive entries are gone),
    /// restore to the original location, activate.
    ReopenClosed,
    /// Ctrl+D. Today tab → bottom of Pinned (top level), `pinned_url = url`.
    /// Pinned tab → top of Today, `pinned_url = None`. Favorite → top of Today.
    /// Ignored for splits and split panes.
    TogglePin { #[serde(default)] id: Option<Id> },
    /// Add a tab to the end of Favorites. Rejected with a toast when full (12). A Today tab is
    /// moved; a pinned tab is moved. Splits/panes are ignored.
    AddFavorite { #[serde(default)] id: Option<Id> },
    /// Favorite → top of Today of the active space.
    RemoveFavorite { id: Id },
    /// Navigate a pinned/favorite tab back to its `pinned_url` (favicon click / double-click);
    /// loads it if unloaded.
    ResetToPinned { id: Id },
    /// `pinned_url = url` for a pinned/favorite tab.
    ReplacePinnedUrl { id: Id },
    /// "Edit Pinned Page" panel (`SidebarPanel::EditPinned`) of a pinned/favorite tab: set custom
    /// title and/or pinned URL (`Some("")` clears the custom title). Closes that panel.
    EditPinned { id: Id, #[serde(default)] title: Option<String>, #[serde(default)] url: Option<String> },
    /// Rename a tab (custom title; `None`/empty clears it) or a folder (empty ignored).
    RenameItem { id: Id, #[serde(default)] title: Option<String> },
    /// Duplicate a tab as a new Today tab (below it if it is in Today, else top of Today), activated.
    DuplicateTab { #[serde(default)] id: Option<Id> },
    /// Drag & drop / reorder. Crossing sections converts the item (arc_spec §2.6): into
    /// Favorites/Pinned sets `pinned_url = url` if unset; into Today clears it. Folders only live in
    /// Pinned (max depth 3); splits only live in Today; a Favorite can't go to a space icon.
    /// Invalid drops are ignored. `before: None` = append at the end.
    MoveItem { id: Id, to: DropTarget },
    /// Move an item to another space (Today → top of that space's Today; Pinned/folder → bottom of
    /// its Pinned). If it was the source space's active item, the source falls back to its MRU.
    MoveToSpace { #[serde(default)] id: Option<Id>, space: Id },
    /// Ctrl+Shift+K: archive all Today items of the space except visible and audible ones; one
    /// `ReopenEntry::Batch`; toast "Cleared N tabs" with Undo (`ReopenClosed`).
    ClearToday { #[serde(default)] space: Option<Id> },
    /// New folder in Pinned of `space` (active space if `None`), at the top, or at the top of
    /// folder `parent`. Opens the `RenameItem` sidebar panel for it.
    NewFolder { #[serde(default)] space: Option<Id>, #[serde(default)] parent: Option<Id>, #[serde(default)] name: Option<String> },
    ToggleFolder { id: Id },
    /// Delete a folder: its tabs go to the archive (reason FolderDeleted), subfolders too.
    DeleteFolder { id: Id },
    /// Destroy the tab's browser but keep the row (any section). If it is visible, the content
    /// falls back like `CloseItem`.
    UnloadTab { id: Id },
    ToggleMute { #[serde(default)] id: Option<Id> },
    /// Copy the (tracker-cleaned) URL of a tab or archive entry; `markdown` → `[title](url)`.
    CopyUrl { #[serde(default)] id: Option<Id>, #[serde(default)] markdown: bool },
    /// Copy arbitrary text (download link etc.) with a "Copied" toast.
    CopyText { text: String },

    // ------------------------------------------------------------------ spaces
    /// Append a new space and switch to it.
    NewSpace { name: String, icon: String, #[serde(default)] theme: Theme },
    UpdateSpace { id: Id, #[serde(default)] name: Option<String>, #[serde(default)] icon: Option<String>, #[serde(default)] theme: Option<Theme> },
    /// Delete a space (not the last one). Its tabs and folders are archived (reason SpaceDeleted).
    DeleteSpace { id: Id },
    /// Switch: shows the target space's `active_item` (loading it if needed) or Empty.
    SwitchSpace { id: Id },
    /// Alt+1..9 (1-based).
    SwitchSpaceNth { n: u32 },
    /// Ctrl+Alt+Left/Right, no wrap.
    SwitchSpaceAdjacent { delta: i32 },
    MoveSpace { id: Id, index: usize },

    // ------------------------------------------------------------------ split view
    /// Put `tab` into a split with `with`. Splits live only in Today: a Pinned/Favorite `tab` or
    /// `with` is duplicated into Today first. If `with` is not in a split, a new split replaces
    /// `with` at its Today position (or top of Today). `side` is relative to `with`
    /// (Left/Right → Horizontal, Top/Bottom → Vertical when creating a 2-pane split; an existing
    /// split keeps its orientation and inserts before (Left/Top) or after (Right/Bottom) `with`).
    /// Max 4 panes (toast when full). Fractions are equalized. Activates the split.
    SplitWith { tab: Id, with: Id, side: SplitSide },
    /// Command bar "Split" mode commit: open `text` (classified) as a new pane at `side` of the
    /// focused pane (same rules as `SplitWith`); no focused tab → `OpenInput{NewTab}`.
    SplitOpenInput { text: String, side: SplitSide },
    /// Ctrl+Shift+1..4 (0-based index).
    FocusPane { index: usize },
    /// Ctrl+Shift+[ / ].
    FocusPaneAdjacent { delta: i32 },
    /// Clamped so each pane keeps a minimum share; renormalized to sum 1.0; non-finite rejected.
    SetSplitFractions { id: Id, fractions: Vec<f32> },
    /// Ctrl+Shift+-: move the pane tab out into its own Today tab directly below the split.
    /// Separating the focused pane of the active split keeps that tab active and focused (it is
    /// the one the user was using); separating another pane leaves the split (or, when it
    /// dissolves, its remaining pane) active.
    SeparatePane { #[serde(default)] tab: Option<Id> },
    /// Dissolve the split: every pane becomes a Today tab (in order) where the group was.
    SeparateAll { id: Id },

    // ------------------------------------------------------------------ archive & history
    /// Restore an archive entry (reusing the original tab id) to its original location
    /// (fallbacks: top of Today of original space → active space), activate it, remove the entry.
    /// A pane whose split still exists goes back between the panes it sat between (by id). A
    /// pane whose split dissolved rebuilds a 2-pane split with the surviving pane, found by id
    /// among the plain Today tabs of its space wherever it sits now (snapshots from profiles that
    /// predate `SplitSnapshot::panes` never rejoin).
    /// `whole_group`: restore every entry of the same split snapshot as the split, in the original
    /// pane order (the most recent snapshot's pane list, with panes closed earlier merged in next
    /// to their old neighbors). When the group has more entries than [`MAX_SPLIT_PANES`] (panes
    /// closed one by one, then the whole split), the most recently archived ones form the split
    /// and the rest become Today tabs below it.
    RestoreArchived { id: Id, #[serde(default)] whole_group: bool },
    DeleteArchived { id: Id },
    ClearArchive,
    DeleteHistoryEntry { url: String },
    ClearHistory,

    // ------------------------------------------------------------------ peek
    /// Close Peek (`HidePeek`, then destroys its browser). `focus_lost` = sent by the shell because
    /// another browser got focus ("click outside"); ignored for popup Peeks (OAuth windows), while
    /// the command bar or find bar is open, and while a permission prompt for the Peek tab or a
    /// visible tab is showing.
    ClosePeek { #[serde(default)] focus_lost: bool },
    /// Turn Peek into a Today tab at the top of Today keeping the browser (Ctrl+O / Expand).
    /// `split`: instead put it in a split with its opener (if the opener still exists).
    ExpandPeek { #[serde(default)] split: bool },

    // ------------------------------------------------------------------ command bar
    /// Open (or re-target) the command bar. `EditUrl` prefills the focused tab's URL (no tab →
    /// behaves as `NewTab`). `split_side` is used by `Split` mode (default Right).
    OpenCommandBar { mode: CommandBarMode, #[serde(default)] split_side: Option<SplitSide> },
    /// What the **keyboard** sends for Ctrl+T, Ctrl+L / Alt+D / F6 and Ctrl+E: the same shortcut
    /// pressed again closes what it opened. A bar open in the same mode (`EditUrl` with no tab counts
    /// as `NewTab`, like `OpenCommandBar`) is closed exactly as `CloseCommandBar` closes it; anything
    /// else — no bar, or a bar in another mode — is `OpenCommandBar {mode}`. Buttons keep sending
    /// `OpenCommandBar`: a click always opens.
    ToggleCommandBar { mode: CommandBarMode },
    /// Esc, blur, or click outside. Refocuses the focused tab.
    ///
    /// `seq` is the `commandBar.seq` the page was showing when it decided to close. A close whose
    /// `seq` is not the open bar's is **ignored**: the bar the page meant is already gone and a new
    /// one was opened meanwhile (Esc immediately followed by Ctrl+T, where the accelerator wins the
    /// race against a ≤ 33 ms coalesced `state` push). Omitting it closes whatever is open, which
    /// is what the shell's own Esc chain and focus rules want.
    CloseCommandBar { #[serde(default)] seq: Option<u64> },
    /// The user committed an omnibox result. Core applies `command`, then hides the bar unless
    /// `alt` (Alt+Enter keeps it open) or `command` itself opened a bar mode / sidebar panel.
    CommitOmnibox { command: Box<Command>, #[serde(default)] alt: bool },

    // ------------------------------------------------------------------ other surfaces & chrome
    /// Ctrl+S / the ◧ button. A docked sidebar (persisted setting) hides and closes its panel. A
    /// hidden one docks (persisted), also while it floats (hover reveal) or is revealed for a
    /// panel; an open panel stays open.
    ToggleSidebar,
    SetSidebarWidth { width: u32 },
    /// Sidebar-local panels that other surfaces or shortcuts need to open (Ctrl+J downloads,
    /// app menu, new/edit space sheet, inline rename, edit pinned page). A hidden sidebar is
    /// revealed only while the panel is open (not persisted); closing the panel hides it again.
    /// Transient panels (downloads, app menu) show it floating over the content
    /// (`SetSidebar{floating:true}`, the page keeps keyboard focus); panels that hold input dock it
    /// (`SetSidebar{visible:true}`).
    OpenSidebarPanel { panel: SidebarPanel },
    ToggleSidebarPanel { panel: SidebarPanel },
    /// Esc / click outside in the sidebar, or Esc in any other browser while a panel is open
    /// (the shell's Esc chain, after page fullscreen, Peek, find bar, switcher and command bar).
    CloseSidebarPanel,
    OpenInternalPage { page: InternalPage },
    /// Open the find bar for the focused tab (re-focus/select if already open).
    OpenFind,
    /// Ctrl+F: `OpenFind`, or `CloseFind` when the find bar is already open **for the focused tab**
    /// (a bar left open on another tab is re-targeted, not closed).
    ToggleFind,
    /// Ctrl+, / Ctrl+H: `OpenInternalPage`, or — when that page is the focused tab already — close it
    /// (`CloseItem`), so the key that showed the page puts it away again.
    ToggleInternalPage { page: InternalPage },
    CloseFind,
    /// Search in `tab` (or the find bar's tab / focused tab). Core remembers `text`/`match_case`
    /// for that tab so `FindNext` works.
    FindInPage { #[serde(default)] tab: Option<Id>, text: String, #[serde(default = "yes")] forward: bool, #[serde(default)] match_case: bool, #[serde(default)] find_next: bool },
    /// F3 / Shift+F3 / Enter in the find bar: repeat the remembered search.
    FindNext { #[serde(default = "yes")] forward: bool },
    Zoom { direction: ZoomDirection },
    /// F12: open DevTools for the focused tab (docked, unless it is undocked or in Peek) or close
    /// them. Refused with a toast on `sta://` pages (`store/devtools.rs`, `store/foreign.rs`).
    ToggleDevTools,
    /// Ctrl+Shift+I: open DevTools, or give them keyboard focus when they are already open. The
    /// third step of UX17 — closing when the frontend *has* focus — is resolved in the shell
    /// (`keyboard.rs`), which dispatches `ToggleDevTools` instead in that case.
    FocusDevTools,
    /// "Undock DevTools": close the docked frontend and reopen DevTools in CEF's own window. Lasts
    /// for that tab until its DevTools close again (D8: per tab, per session, never persisted).
    UndockDevTools,
    Print,
    /// Opens `view-source:<url>` as a new Today tab below the focused tab.
    ViewSource,
    /// Allocate a boost for the focused tab's host and open `sta://boosts/?id=<id>`.
    NewBoostForSite { #[serde(default)] tab: Option<Id> },
    WindowControl { action: WindowAction },
    /// Quit the app (same path as `WindowCloseRequested`).
    Quit,
    /// Toast timed out or was dismissed.
    DismissToast { id: u64 },

    // ------------------------------------------------------------------ settings & boosts
    UpdateSettings { patch: SettingsPatch },
    /// Insert (`id == 0` → allocate) or replace a boost; reloads loaded tabs it applies to
    /// (before or after the change).
    UpsertBoost { boost: Boost },
    DeleteBoost { id: Id },
    ToggleBoost { id: Id },
    /// Answer a site permission prompt. `remember` stores the decision for the origin. The answer
    /// is `Effect::AnswerPermission{allow, remember}`: a Block without `remember` is a one-off
    /// "not now" (the shell dismisses the request, so the site can ask again), an Allow without
    /// `remember` is "allow this time" (the shell revokes it once no tab shows the origin, and at
    /// the next start).
    ResolvePermission { id: u64, allow: bool, #[serde(default)] remember: bool },

    // ------------------------------------------------------------------ extensions (Ctrl+E)
    /// Use an installed extension from the Ctrl+E picker, Settings or the popup card's header
    /// (`crate::extensions`). `Primary` opens the popup card, else the options tab, else the Web
    /// Store page; for an extension that is off or waiting for the user's OK it opens
    /// Settings › Extensions at that item. **It never turns an extension on** (SEC-7): that is a
    /// decision the user makes in Settings after seeing Chrome's warnings and the source.
    RunExtension { id: String, #[serde(default = "primary_action")] action: ExtensionAction },
    /// Load the permission warnings, host access and source of `id` through the hidden
    /// `chrome://extensions` backend (`Effect::ExtensionOp{GetInfo}`), for the Turn on disclosure.
    RequestExtensionDetails { id: String },
    /// Turn an extension on or off (Settings only). Turning one on needs its details to have been
    /// loaded and confirmed; sta refuses it for a local CRX another program registered (D6a).
    SetExtensionEnabled { id: String, enabled: bool },
    /// Uninstall an extension (Settings only). Chromium always shows its own "Remove …?"
    /// confirmation for this — `showConfirmDialog: false` is honoured only for an extension removing
    /// itself — so that dialog **is** the confirmation and sta asks nothing of its own.
    RemoveExtension { id: String },
    /// Close the popup card (its × button, and the shell's own close rules).
    CloseExtensionPopup,

    // ------------------------------------------------------------------ recent-tab switcher
    /// Ctrl+Tab (`forward`) / Ctrl+Shift+Tab. First step opens the switcher state (MRU of up to 5
    /// tabs; selection = 1 forward / last backward, or 0 forward when `MRU[0]` is not the focused tab,
    /// e.g. in the empty state or with Peek open); the shell delays *showing* the overlay by 250 ms.
    MruStep { forward: bool },
    /// Click on a switcher card.
    MruSelect { index: usize },
    /// Ctrl released: activate the selection (MRU order updates only here).
    MruCommit,
    /// Esc or window deactivation.
    MruCancel,

    // ------------------------------------------------------------------ downloads
    DownloadControl { id: u32, action: DownloadAction },
    /// Remove a finished download from the list.
    DownloadDismiss { id: u32 },

    // ------------------------------------------------------------------ AI agents (MCP), docs/MCP.md
    /// Answer a connection prompt (`UiState.agent.prompts`). `remember` ("Always") trusts the
    /// client's host executable permanently, only when it is signed (`client.verified`). The shell
    /// accepts it only from the Settings page (and the agent overlay); 403 from other surfaces.
    AnswerAgentConnection { id: u64, allow: bool, #[serde(default)] remember: bool },
    /// Answer a site prompt; `remember` allows the site for every later session too. Same caller
    /// check as `AnswerAgentConnection`.
    AnswerSitePermission { id: u64, allow: bool, #[serde(default)] remember: bool },
    /// Stop: disconnect every agent session and refuse new ones until `ResumeAgents` (not saved).
    StopAgents,
    ResumeAgents,
    /// Share a tab with agents (or stop sharing it) under the "agent tabs" scope.
    ShareTabWithAgent { tab: Id, #[serde(default = "yes")] shared: bool },
    /// Keep or discard a download an agent action started.
    ResolveAgentDownload { id: u32, keep: bool },
    /// The topbar agent chip: open or close the agent activity panel (`sta://agent/`).
    ToggleAgentPanel,
    /// Close the agent activity panel; `focus_lost` when another browser took focus (a toggle
    /// right after that is the same click on the chip and keeps it closed).
    CloseAgentPanel { #[serde(default)] focus_lost: bool },
    /// Archive the Today tabs agents opened (the session-end toast, the activity panel).
    ArchiveAgentTabs,
    /// Answer a tab access prompt (`request_tab_access`): `allow` shares the tab with agents. Same
    /// caller check as `AnswerAgentConnection`.
    AnswerTabAccess { id: u64, allow: bool },

    // ================================================================== shell → core events
    /// The shell created the tab's browser (after `CreateBrowser`/`ReplaceBrowser`).
    TabBrowserCreated { tab: Id },
    /// The tab's browser is gone: after `DestroyBrowser`, after a failed `CreateBrowser`, or the
    /// page closed itself (`window.close()`). Marks the tab unloaded and completes any deferred
    /// `CreateBrowser`. Not requested by core: Peek tab → `ClosePeek`; otherwise `CloseItem`.
    /// A late or duplicate report for a tab core already considers unloaded is ignored.
    /// Browser generations are tracked per tab: when a deferred `CreateBrowser` was issued because
    /// the previous browser's close arrived, another report before `TabBrowserCreated` is taken as
    /// a stale duplicate for the previous browser and ignored. If that browser still isn't
    /// confirmed at the second `Tick` afterwards, the report is handled as a creation failure.
    TabBrowserClosed { tab: Id },
    /// Main-frame URL changed (including same-document navigations). Core updates `Tab.url`,
    /// clears `audible` on cross-document changes, and records a history visit when the URL
    /// differs from the previous one (transition `Typed` if it matches core's pending typed
    /// navigation for that tab, else `Link`). The first commit of a browser created to lazily
    /// (re)load an existing tab (activation of an unloaded tab, session restore, archive restore)
    /// is not a visit; the first commit of a newly opened tab or of a user navigation is. The
    /// shell never reports its own error pages.
    TabAddressChanged { tab: Id, url: String },
    /// Page title of the tab's main frame. Also names the history entry of the tab's current URL
    /// when the title is unchanged (the next page of a navigation can have the same title, and
    /// its visit is recorded before its title arrives).
    TabTitleChanged { tab: Id, title: String },
    TabFaviconChanged { tab: Id, #[serde(default)] url: Option<String> },
    TabLoadingStateChanged { tab: Id, loading: bool, can_go_back: bool, can_go_forward: bool },
    /// 0.0..=1.0
    TabLoadProgress { tab: Id, progress: f32 },
    /// Main-frame load failed (not ERR_ABORTED). The shell shows its error page without reporting
    /// its address; `Tab.url` stays `url`.
    TabLoadFailed { tab: Id, url: String, error_code: i32, error_text: String },
    /// Media started/stopped playing (renderer media events via process message; see shell).
    TabAudioChanged { tab: Id, audible: bool },
    TabCrashed { tab: Id },
    /// Zoom level of the tab changed (after `Zoom`, Ctrl+wheel, or load); CEF zoom level units.
    TabZoomChanged { tab: Id, level: f64 },
    /// A tab browser received focus from the user. Never causes `FocusBrowser`. Updates split
    /// focus; closes the command bar; closes a non-popup Peek when the focused tab isn't Peek;
    /// closes the transient sidebar panels (`Downloads`, `AppMenu`; not the space sheets or an
    /// inline rename, which hold user input).
    TabFocused { tab: Id },
    /// A popup created by CEF (window.open / target=_blank) was adopted under a shell-allocated
    /// id (`Store::alloc_id`) — its browser already exists and is attached hidden.
    /// Rules: `popup` (window features, e.g. OAuth) → Peek (if `peek_enabled`) else foreground
    /// Today tab; non-popup from a pinned/favorite opener to another site with Peek enabled →
    /// Peek; otherwise Today tab below the opener (unknown opener → top of Today), activated iff
    /// `foreground`. Never ignored (the browser exists), except for an id already in use or
    /// outside `1..=MAX_ID`. An empty URL or `about:blank` (`window.open()` +
    /// `document.write`) is shown as `about:blank` and never navigated; any other URL outside the
    /// web-content allowlist (see `OpenUrl`) is replaced by `about:blank` (`LoadUrl`) with a toast.
    PopupAdopted { tab: Id, #[serde(default)] opener: Option<Id>, url: String, popup: bool, foreground: bool },
    /// A link wants to open outside the current tab (middle/Ctrl+click via on_open_urlfrom_tab,
    /// a cross-site link from a pinned tab caught by `Store::intercept_navigation`, or the
    /// renderer's Alt+click preview gesture → `Preview`). Refused with a toast outside the
    /// web-content allowlist (see `OpenUrl`).
    LinkOpenRequested { opener: Id, url: String, disposition: LinkDisposition },
    /// Page (HTML5) fullscreen request from a tab.
    TabFullscreenChanged { tab: Id, fullscreen: bool },
    /// Site permission request (Alloy shows no UI). Core queues a prompt unless a remembered
    /// decision exists (then answers immediately with `AnswerPermission{remember: true}`).
    /// Requests for unknown or unloaded tabs and duplicate ids are refused as one-off answers
    /// (`allow: false, remember: false`).
    PermissionRequested { id: u64, tab: Id, origin: String, kinds: Vec<PermissionKind> },
    /// CEF dismissed a pending request (navigation, tab closed).
    PermissionDismissed { id: u64 },
    DownloadUpdated { download: Download },
    /// A download started in `tab` while its browser had **no document at all**: the link the user
    /// asked to *see* turned out to be a file (a `Content-Disposition` attachment). Dispatched once
    /// per download, after its first `DownloadUpdated`, so the browser is already kept alive for it.
    /// A Peek showing that empty card closes itself; the download toast is the whole outcome.
    DownloadInBlankTab { tab: Id },
    /// Maximize/fullscreen/focus/bounds changed. `bounds` only when in the normal (restored)
    /// state. Losing focus cancels the switcher. Bounds-only changes don't bump revision.
    WindowStateChanged { maximized: bool, fullscreen: bool, focused: bool, #[serde(default)] bounds: Option<Rect> },
    /// Dispatched once before `Store::startup` and whenever the OS theme changes.
    SystemThemeChanged { dark: bool },
    /// The Windows "Animation effects" setting (`SPI_GETCLIENTAREAANIMATION`), read at init, on
    /// `WM_SETTINGCHANGE` and in the 5 s heartbeat. **Runtime only**: it never marks state dirty,
    /// is never saved, and bumps the revision only when the value actually changes
    /// (`UiState.motion.systemAnimations`, `crate::motion`).
    SystemAnimationsChanged { enabled: bool },
    /// Alt+F4 / taskbar close / caption close: the shell's `can_close` asks core. Core sets the
    /// shutting-down flag and returns `[SaveNow, Quit]`; afterwards all commands are ignored.
    WindowCloseRequested,
    /// Periodic (every 60 s) and on resume: refresh `last_active_at` of visible tabs, run
    /// auto-archive, purge archive entries older than 30 days, drop stale permission prompts.
    Tick,

    // ------------------------------------------------------------------ Chrome-created browsers (shell)
    /// A browser Chromium created on its own (an extension's `tabs.create`, `windows.create`,
    /// `runtime.openOptionsPage`; the Web Store's post-install window) tried to load `url`. The
    /// shell cancelled it and hid the Chrome window (ARCHITECTURE §4.5). Core opens a foreground
    /// Today tab after the active item when [`crate::urls::foreign_tab_verdict`] allows it:
    /// - `http(s)` with a host;
    /// - `chrome-extension://<id>/<page>` when `extension` (what the shell read from that
    ///   extension's files) names the same id and the page is one of its options, popup or side
    ///   panel pages, is web-accessible to every site, or the extension was installed in the last
    ///   60 s (`recentlyInstalled`, or an `ExtensionInstalled` for it). Otherwise a toast "An
    ///   extension wants to open a page of <name>" offers Open (`OpenUrl`); <name> is the page's owner,
    ///   never the extension that asked (the shell cannot see that: `urls::foreign_tab_verdict`).
    ///
    /// Anything else is ignored. Both answers are rate limited (`store/foreign.rs`,
    /// `FOREIGN_OPEN_MAX`): the budget is spent on the verdict, so an ask the user never answers,
    /// and an ignored URL, leave the tab budget for the next window.
    ForeignTabRequested { url: String, #[serde(default)] extension: Option<ForeignExtension> },
    /// An extension was installed (the post-install browser appeared and a new extension directory
    /// was found). Toast "<name> added", or "<name> was added by another program, off until you
    /// allow it" when `external`. Remembered for 60 s (see `ForeignTabRequested`).
    ExtensionInstalled { id: String, name: String, #[serde(default)] external: bool },
    /// The shell refused a Chrome-created browser (toast only).
    ForeignBlocked { reason: ForeignBlockReason },
    /// The volume that holds the profile is nearly full while a Chrome Web Store page is open
    /// (`crates/sta/src/disk.rs`). Toast, once per run: Chromium unpacks an extension into the
    /// profile and reports a full disk as "Could not unzip extension" / "Package is invalid", which
    /// sends people looking for a bug in the extension (AdBlock unpacks to 340 MB).
    LowDiskSpace { free_mb: u64 },

    // ------------------------------------------------------------------ updates
    /// Look for a newer release now (Settings › About). The shell answers with
    /// [`Command::UpdateStatusChanged`]; one check runs by itself a few seconds after startup.
    CheckForUpdate,
    /// Fetch the release the last check found (Settings › About). Ignored unless the status says
    /// a download is possible ([`crate::update::UpdateStatus::can_download`]).
    DownloadUpdate,
    /// Apply the staged update: sta quits and the new build starts in its place. Ignored unless an
    /// update is `ready`.
    InstallUpdate,
    /// The shell's report on all of the above (`crates/sta/src/update.rs`): the only thing that
    /// ever writes `UiState.update`.
    UpdateStatusChanged { status: crate::update::UpdateStatus },

    // ------------------------------------------------------------------ extensions (shell)
    /// The shell re-read the profile's extensions (startup, a watched change, after an operation,
    /// +11 s after an install). Replaces the whole list; components are already filtered out.
    ExtensionsChanged { extensions: Vec<ExtensionInfo> },
    /// The backend answered a `GetInfo` (`Effect::ExtensionOp`).
    ExtensionDetailsLoaded { details: ExtensionDetails },
    /// A backend operation failed or timed out (toast; the row stops being busy).
    ExtensionOpFailed { id: String, op: ExtensionOp, #[serde(default)] message: String },
    /// The popup card is gone: the page closed itself, the frontend never sized it, or the shell
    /// closed it (blur, Esc, tab switch). `failed` shows the honest failure line instead.
    ExtensionPopupClosed { #[serde(default)] failed: bool },
    /// sta started in safe mode: two abnormal exits within 60 s of launch (R-SEC-11). Tabs are
    /// restored unloaded and Settings › Extensions shows a banner.
    SafeModeStarted,

    // ------------------------------------------------------------------ docked DevTools (shell)
    /// The tab's DevTools are gone: the frontend browser closed or crashed, the undocked window was
    /// closed, or the shell could not open them at all (`store/devtools.rs`).
    DevToolsClosed { tab: Id },
    /// The frontend's own Undock button (same as `UndockDevTools`, but for that tab). `narrow` is
    /// the shell's own request: the tab's card is too small for a dock (a split pane, a narrow
    /// window — `crates/sta/src/devtools.rs` `MIN_DOCK_WIDTH`), and a toast says so.
    DevToolsUndockRequested { tab: Id, #[serde(default)] narrow: bool },
    /// The frontend offered a link (`openInNewTab`) or, with `search`, a query
    /// (`openSearchResultsInNewTab`). Checked like any link web content offers
    /// (`urls::web_content_may_open`); external protocols go to the OS.
    DevToolsLinkRequested { tab: Id, url: String, #[serde(default)] search: bool },
    /// Context menu "Inspect" at page coordinates `x`/`y` (CSS pixels of the tab's page): opens
    /// DevTools if needed and selects the node at that point.
    InspectElement { tab: Id, x: i32, y: i32 },

    // ------------------------------------------------------------------ AI agent events (shell)
    /// An agent client said hello (shell-allocated request id). Core answers at once
    /// (`Effect::AgentAnswer`) when access is off, agents are paused, or the client is trusted
    /// ("Always"); otherwise it queues a connection prompt.
    AgentConnectionRequested { id: u64, client: AgentClientInfo },
    /// An approved session runs.
    AgentSessionStarted { session: u64, client: AgentClientInfo, access: AgentAccess },
    AgentSessionEnded { session: u64 },
    /// A tool ran on `tab` / `site` (activity list; never page content, typed text or full URLs).
    /// `error` = the error code when the call failed (`site_not_approved`, …).
    AgentActivity { session: u64, tool: String, #[serde(default)] tab: Option<Id>, #[serde(default)] site: Option<String>, #[serde(default)] error: Option<String> },
    /// An agent needs approval for `site` (answered with `Effect::AgentAnswer`, at once when the
    /// site is already allowed).
    AgentSiteRequested { id: u64, session: u64, #[serde(default)] tab: Option<Id>, site: String },
    /// `tab_open`: a background tab at the top of the active space's Today list under a
    /// shell-allocated id (`Store::alloc_id`), in agent scope. Only `http(s)` and `about:blank`.
    OpenAgentTab { tab: Id, url: String },
    /// Load an unloaded tab without showing it.
    LoadTab { tab: Id },
    /// `tab_show`: activate the tab (switching space if needed) without requesting keyboard focus.
    ShowAgentTab { tab: Id },
    /// A download started by an agent-controlled tab waits for Keep / Discard.
    AgentDownloadHeld { id: u32, #[serde(default)] tab: Option<Id>, file_name: String },
    /// A popup of an agent-controlled tab was adopted as `tab` (after its `PopupAdopted`): it is an
    /// agent tab too.
    AgentTabAdopted { tab: Id },
    /// `request_tab_access`: an agent session asks the user to share `tab` (answered with
    /// `Effect::AgentAnswer`, at once when the tab is already in scope, gone, or agents are off).
    AgentTabAccessRequested { id: u64, session: u64, tab: Id, reason: String },
}

fn yes() -> bool {
    true
}

impl Command {
    /// Whether this command may be sent by HTML UI pages over IPC. Shell events (the variants
    /// after `DownloadDismiss`) are shell-only: a compromised UI page must not be able to forge
    /// browser lifecycle events. `CommitOmnibox` is allowed only when its inner command is (and
    /// is not itself a `CommitOmnibox`), so a shell event can't be smuggled inside one.
    pub fn allowed_from_ui(&self) -> bool {
        if let Command::CommitOmnibox { command, .. } = self {
            return !matches!(**command, Command::CommitOmnibox { .. }) && command.allowed_from_ui();
        }
        !matches!(
            self,
            Command::TabBrowserCreated { .. }
                | Command::TabBrowserClosed { .. }
                | Command::TabAddressChanged { .. }
                | Command::TabTitleChanged { .. }
                | Command::TabFaviconChanged { .. }
                | Command::TabLoadingStateChanged { .. }
                | Command::TabLoadProgress { .. }
                | Command::TabLoadFailed { .. }
                | Command::TabAudioChanged { .. }
                | Command::TabCrashed { .. }
                | Command::TabZoomChanged { .. }
                | Command::TabFocused { .. }
                | Command::PopupAdopted { .. }
                | Command::LinkOpenRequested { .. }
                | Command::TabFullscreenChanged { .. }
                | Command::PermissionRequested { .. }
                | Command::PermissionDismissed { .. }
                | Command::DownloadUpdated { .. }
                | Command::DownloadInBlankTab { .. }
                | Command::WindowStateChanged { .. }
                | Command::SystemThemeChanged { .. }
                | Command::SystemAnimationsChanged { .. }
                | Command::WindowCloseRequested
                | Command::Tick
                | Command::ForeignTabRequested { .. }
                | Command::ExtensionInstalled { .. }
                | Command::ForeignBlocked { .. }
                | Command::LowDiskSpace { .. }
                | Command::UpdateStatusChanged { .. }
                | Command::ExtensionsChanged { .. }
                | Command::ExtensionDetailsLoaded { .. }
                | Command::ExtensionOpFailed { .. }
                | Command::ExtensionPopupClosed { .. }
                | Command::SafeModeStarted
                | Command::DevToolsClosed { .. }
                | Command::DevToolsUndockRequested { .. }
                | Command::DevToolsLinkRequested { .. }
                | Command::InspectElement { .. }
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
                | Command::AgentTabAccessRequested { .. }
        )
    }
}

/// What the shell read from disk about the extension a `chrome-extension://` URL names
/// (`ForeignTabRequested`). Only its own files: never the page that asked.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ForeignExtension {
    pub id: String,
    pub name: String,
    /// Relative paths of its options, popup and side panel pages.
    #[serde(default)]
    pub pages: Vec<String>,
    /// Resource patterns web-accessible to every site (`*` matches any run of characters).
    #[serde(default)]
    pub web_accessible: Vec<String>,
    /// Its files were written in the last 60 s (a fresh install).
    #[serde(default)]
    pub recently_installed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ForeignBlockReason {
    /// A flood of windows: the shell drops navigations it won't even classify
    /// (`foreign.rs`, `HANDLE_MAX`). Core raises the same toast, with the same once-per-10 s
    /// dedupe, when its own budget runs out (`store/foreign::FOREIGN_OPEN_MAX`).
    RateLimited,
    /// A private (incognito) Chrome window: sta has none.
    Incognito,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum OpenTarget {
    CurrentTab,
    NewTab,
    BackgroundTab,
}

/// How a link wants to open. Shell mapping from CEF `WindowOpenDisposition`:
/// NEW_FOREGROUND_TAB → ForegroundTab; NEW_BACKGROUND_TAB → BackgroundTab;
/// NEW_WINDOW / NEW_POPUP / NEW_SPLIT_VIEW → NewWindow; CURRENT_TAB / SINGLETON_TAB /
/// SWITCH_TO_TAB → handled in-place (not reported); SAVE_TO_DISK / OFF_THE_RECORD /
/// IGNORE_ACTION → ForegroundTab or ignored; NEW_PICTURE_IN_PICTURE → never intercepted.
/// `Preview` has no CEF disposition: it comes from the renderer's Alt+click gesture
/// (`renderer.rs`, `MSG_PREVIEW`), which cancels the click before Chromium can act on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum LinkDisposition {
    ForegroundTab,
    BackgroundTab,
    /// Shift+click / new window: Peek when enabled, else foreground tab.
    NewWindow,
    /// Cross-site link from a pinned tab or favorite (Peek candidate).
    PinnedCrossSite,
    /// **Alt+click / Alt+middle-click on a link** (PROTOCOL §13): the user asked to *preview* this
    /// URL, from any tab of any kind. Always Peek while Peek is enabled — replacing an open Peek
    /// rather than nesting, so an Alt+click inside Peek swaps the page it shows — and a foreground
    /// tab when Peek is off. The one Peek it never replaces is a popup (sign-in) Peek opened from
    /// *another* tab; that one keeps its window and the link opens in a background tab instead.
    Preview,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SplitSide {
    Left,
    Right,
    Top,
    Bottom,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CommandBarMode {
    /// Ctrl+T: empty input; commit opens a new tab.
    NewTab,
    /// Ctrl+L / Alt+D / URL pill: prefilled with the focused tab's URL; commit navigates it.
    EditUrl,
    /// Ctrl+Shift+=: commit opens a split pane.
    Split,
    /// ">" actions list.
    Actions,
    /// Ctrl+E: the installed extensions (`crate::extensions`). Page-first (D5a), so a page that
    /// uses Ctrl+E itself keeps it; the app menu and `>` open it everywhere.
    Extensions,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum InternalPage {
    Settings,
    Archive,
    History,
    Boosts,
}

impl InternalPage {
    /// Canonical URL of the page. Each page is its own host of the `sta` scheme.
    pub fn url(self) -> &'static str {
        match self {
            InternalPage::Settings => "sta://settings/",
            InternalPage::Archive => "sta://archive/",
            InternalPage::History => "sta://history/",
            InternalPage::Boosts => "sta://boosts/",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ZoomDirection {
    In,
    Out,
    Reset,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum WindowAction {
    Minimize,
    ToggleMaximize,
    /// Same as `WindowCloseRequested`.
    Close,
    ToggleFullscreen,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DownloadAction {
    Pause,
    Resume,
    Cancel,
    Open,
    ShowInFolder,
    /// Start the same URL again (new download id).
    Retry,
}

/// Sidebar-local panels that can be opened from outside the sidebar.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum SidebarPanel {
    /// Ctrl+J downloads popover.
    Downloads,
    /// ≡ / Alt+F app menu.
    AppMenu,
    /// New space sheet (emoji, name, theme).
    NewSpace,
    /// Edit space sheet (rename, icon, theme, delete).
    EditSpace { id: Id },
    /// Inline rename of a tab or folder row.
    RenameItem { id: Id },
    /// "Edit Pinned Page" popover (title and pinned URL) of a pinned or favorite tab.
    EditPinned { id: Id },
}

impl SidebarPanel {
    /// A popover for a quick look (downloads, app menu): it closes when a page takes focus
    /// (`TabFocused`) and on Esc in a page (the shell's Esc chain). Panels that hold user input (new
    /// or edit space, inline rename, edit pinned page) are not transient: they dock a hidden
    /// sidebar, because the floating sidebar can't take keyboard focus.
    pub fn is_transient(&self) -> bool {
        matches!(self, SidebarPanel::Downloads | SidebarPanel::AppMenu)
    }
}

/// Where a dragged item lands.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DropTarget {
    pub container: Container,
    /// Insert before this sibling item id; `None` = append at the end. A `before` that is the
    /// dragged item itself or not in the container is treated as `None`.
    #[serde(default)]
    pub before: Option<Id>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum Container {
    Favorites,
    Pinned { space: Id },
    Today { space: Id },
    Folder { id: Id },
}

/// Partial settings update; `None` fields are left unchanged.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SettingsPatch {
    pub search_engine: Option<SearchEngineId>,
    pub custom_search_url: Option<String>,
    pub archive_after_hours: Option<u32>,
    pub appearance: Option<Appearance>,
    pub startup: Option<Startup>,
    pub peek_enabled: Option<bool>,
    /// Empty string resets to the default Downloads folder.
    pub download_dir: Option<String>,
    pub ask_download_location: Option<bool>,
    pub search_suggestions: Option<bool>,
    /// Animation settings (`crate::motion`): applied as `reset`, then the scalars, then the maps.
    pub animations: Option<crate::motion::AnimationsPatch>,
    pub agent_access: Option<AgentAccess>,
    pub agent_scope: Option<AgentScope>,
    pub agent_sites: Option<AgentSites>,
    pub agent_scripts: Option<AgentScripts>,
    pub agent_history: Option<bool>,
    pub agent_downloads: Option<bool>,
    pub agent_allow_private_network: Option<bool>,
    /// Normalized to host names (see `agent::policy::normalize_host_entry`), at most 500.
    pub agent_blocked_hosts: Option<Vec<String>>,
    pub agent_allowed_sites: Option<Vec<String>>,
    /// Only removes entries (Revoke): entries not already trusted are ignored.
    pub agent_trusted_clients: Option<Vec<AgentTrustedClient>>,
}

/// Helper for callers (the store itself never reads the clock): wall clock in Unix ms.
pub fn now_ms() -> Millis {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as Millis)
        .unwrap_or(0)
}
