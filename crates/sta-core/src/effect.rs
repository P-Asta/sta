//! Effects: instructions for the shell, returned by [`crate::Store::apply`] in execution order.
//!
//! Guarantees the store gives:
//! - A tab in a `ShowContent`/`ShowPeek` layout is loaded or has a `CreateBrowser` earlier in the
//!   same batch.
//! - `CreateBrowser` is emitted at most once per tab until `TabBrowserClosed` arrives for it. If a
//!   tab must be re-created while its `DestroyBrowser` is still pending, core defers the
//!   `CreateBrowser` (and keeps the tab out of `ShowContent`) until `TabBrowserClosed`.
//! - `DestroyBrowser` is only emitted for loaded tabs and is **not cancelable**: the shell
//!   auto-accepts beforeunload dialogs for browsers it is closing.
//! - Closing a shown Peek emits `HidePeek` *before* the `DestroyBrowser` of its tab, so the
//!   overlay is never visible around a browser being torn down.
//! - UI refresh and persistence are not effects: the shell compares [`crate::Store::revision`] and
//!   calls [`crate::Store::take_dirty`] after each drain.
//! - Overlay *content* (command bar text/mode, find query, toast text, switcher cards, permission
//!   prompts) is in [`crate::UiState`]; the effects here only control native visibility/placement.

use crate::command::{DownloadAction, ZoomDirection};
use crate::model::Orientation;
use crate::Id;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum Effect {
    // ------------------------------------------------------------------ browsers
    /// Create a hidden BrowserView for `tab` loading `url` and attach it (hidden) to the content
    /// area. `internal` = `sta://` page (created with the trusted UI client). The shell
    /// dispatches `TabBrowserCreated` (or `TabBrowserClosed` if creation failed).
    CreateBrowser { tab: Id, url: String, internal: bool, muted: bool },
    /// Replace the tab's browser in place with a new one loading `url` (internal ↔ web switch):
    /// create the new view at the same position/visibility, then force-close the old one
    /// *without* reporting `TabBrowserClosed` for it.
    ReplaceBrowser { tab: Id, url: String, internal: bool },
    /// Force-close the tab's browser; the shell reports `TabBrowserClosed` when it is gone.
    DestroyBrowser { tab: Id },

    // ------------------------------------------------------------------ content area
    /// Show exactly this layout in the content area and hide every other tab view. Views
    /// currently parented to the Peek overlay are moved back into the content area.
    ShowContent { layout: ContentLayout },
    /// Give keyboard focus to the tab's page (async in CEF).
    FocusBrowser { tab: Id },
    /// Move `tab`'s view into the Peek overlay and show it (focused).
    ShowPeek { tab: Id },
    /// Hide the Peek overlay (the view stays parented there until destroyed or re-shown via
    /// `ShowContent`).
    HidePeek { tab: Id },

    // ------------------------------------------------------------------ page actions
    LoadUrl { tab: Id, url: String },
    GoBack { tab: Id },
    GoForward { tab: Id },
    Reload { tab: Id, ignore_cache: bool },
    StopLoad { tab: Id },
    /// Chrome-style zoom step (the shell uses Chrome's preset zoom levels and reports the result
    /// with `TabZoomChanged`). Zoom is remembered per host by Chromium itself.
    Zoom { tab: Id, direction: ZoomDirection },
    SetAudioMuted { tab: Id, muted: bool },
    /// Open DevTools for the tab. `docked`: the frontend runs in a BrowserView inside the tab's
    /// wrapper and the page sits on top of it at the rect the frontend reports (FINAL PLAN §3);
    /// otherwise CEF's own DevTools window opens (undock, and Peek pages). Already-open DevTools are
    /// re-presented (an undock while docked arrives as `CloseDevTools` + `OpenDevTools{docked:false}`).
    OpenDevTools { tab: Id, docked: bool },
    /// Close the tab's DevTools (docked frontend or CEF's window). Emitted before
    /// `ReplaceBrowser`/`DestroyBrowser` of a tab whose DevTools are open, so the page view is back
    /// in its wrapper first.
    CloseDevTools { tab: Id },
    /// Give keyboard focus to the tab's docked DevTools frontend (no-op when undocked).
    FocusDevTools { tab: Id },
    /// Select the node at `x`/`y` (CSS pixels of the tab's page) in the tab's DevTools.
    InspectAt { tab: Id, x: i32, y: i32 },
    /// Translate the tab's page into `target` (an ISO-639-1 code), or, if the shell already
    /// translated it, put the original back. Core does not track which of the two it will be — the
    /// page itself is the record — so the shell reports what happened with `TranslateFinished`.
    /// `images` carries `settings.translate_images`: core owns the policy, the shell just obeys.
    TranslatePage { tab: Id, target: String, images: bool },
    /// Stop the translation running on `tab`. The shell answers `TranslateFinished { cancelled }`.
    CancelTranslate { tab: Id },
    Print { tab: Id },
    Find { tab: Id, text: String, forward: bool, match_case: bool, find_next: bool },
    StopFinding { tab: Id },
    ExitPageFullscreen { tab: Id },
    /// Start downloading `url` in the context of `tab` (download retry).
    StartDownload { tab: Id, url: String },

    // ------------------------------------------------------------------ chrome & overlays
    /// Show/position the command bar overlay and focus it (content from `UiState.commandBar`).
    ShowCommandBar,
    HideCommandBar,
    /// Show the find bar over `tab` and focus it (content from `UiState.find`).
    ShowFindBar { tab: Id },
    HideFindBar,
    /// Show the switcher overlay (content from `UiState.switcher`). The shell delays the actual
    /// show by 250 ms and cancels it if `HideSwitcher` arrives first.
    ShowSwitcher,
    HideSwitcher,
    /// Show the toast overlay (content from `UiState.toast`); the toast page dismisses itself by
    /// dispatching `DismissToast` after `durationMs`.
    ShowToast,
    HideToast,
    /// Show the permission prompt overlay over `tab` (content from `UiState.permissionPrompts[0]`).
    ShowPermissionPrompt { tab: Id },
    HidePermissionPrompt,
    /// Answer a pending CEF permission request.
    /// - `allow`: grant (camera/microphone: the requested devices; prompts: accept).
    /// - `!allow && remember`: a decision that should stick (the user blocked with "Remember", or
    ///   a remembered block answered the request): the shell denies (`DENY` / `cont(0)`).
    /// - `!allow && !remember`: a one-off "no" (Block without Remember, stale or invalid
    ///   requests): the shell dismisses (`DISMISS` / media `cancel()`), so Chromium asks again
    ///   next time instead of blocking the origin for the rest of the session.
    AnswerPermission { id: u64, allow: bool, remember: bool },
    /// Sidebar placement and width (DIP).
    /// - `visible`: the sidebar is docked in the window: the persisted setting, or `true` while a
    ///   hidden sidebar is revealed for an open panel that holds input (new/edit space sheet,
    ///   inline rename), which needs keyboard focus.
    /// - `floating`: the hidden sidebar floats over the content (the hover-reveal overlay) and
    ///   stays open while a *transient* panel (downloads, app menu) is open. Never `true` together
    ///   with `visible`; `false` when omitted (older JSON).
    ///
    /// While neither is set, the shell reveals the floating sidebar when the pointer rests at the
    /// window's left edge and hides it again when the pointer leaves (shell-only state).
    SetSidebar { visible: bool, width: u32, #[serde(default)] floating: bool },
    /// Native chrome colors changed (space switch, theme edit, appearance, system theme):
    /// window/panel backgrounds = `frame_argb`, DWM dark mode = `dark`. The other colors are
    /// [`crate::theme::chrome_argb`] of the active space, opaque ARGB: the focused split pane's ring
    /// (`accent_argb`), the overlay cards' fill and 1 DIP edge (`surface_argb`, `border_argb` =
    /// border over surface) and the floating sidebar card's edge (`frame_border_argb` = border over
    /// frame). Wire compatibility: those four were added later and default to `0` when omitted
    /// (older JSON, `debug.execute`); the shell derives a `0` value from the theme itself.
    SetChrome {
        frame_argb: u32,
        dark: bool,
        #[serde(default)]
        accent_argb: u32,
        #[serde(default)]
        surface_argb: u32,
        #[serde(default)]
        border_argb: u32,
        #[serde(default)]
        frame_border_argb: u32,
    },
    /// HTML5 fullscreen for `tab` (hide sidebar/topbar/insets/other panes, fullscreen the window),
    /// or `None` to restore the previous layout and window state.
    SetPageFullscreen { tab: Option<Id> },

    // ------------------------------------------------------------------ extensions (Ctrl+E)
    /// Show the extension's action popup in the card overlay (`Overlay::ExtensionPopup`), anchored
    /// to `tab`'s pane. `url` is `chrome-extension://<id>/<popup>`, already validated
    /// ([`crate::urls::extension_page_url`]). The card appears only once the page reports its size,
    /// and says so honestly when it never does (FINAL PLAN §4).
    OpenExtensionPopup { id: String, url: String, #[serde(default)] tab: Option<Id> },
    HideExtensionPopup,
    /// Re-read the installed extensions (answered with `Command::ExtensionsChanged`).
    RefreshExtensions,
    /// Run one operation of the hidden `chrome://extensions` backend
    /// (`crates/sta/src/ext_backend.rs`). Answered with `ExtensionsChanged` /
    /// `ExtensionDetailsLoaded`, or `ExtensionOpFailed`.
    ExtensionOp { id: String, op: crate::extensions::ExtensionOp },

    // ------------------------------------------------------------------ OS / app
    CopyToClipboard { text: String },
    /// Updates (`crates/sta/src/update.rs`): fetch the manifest, fetch the release it names, or
    /// hand over to the staged build (which quits sta and starts the new one).
    CheckForUpdate,
    DownloadUpdate,
    InstallUpdate,
    /// Hand an external-protocol URL (`mailto:`, `tel:`, `zoommtg://…`: any scheme Chromium
    /// doesn't load, see [`crate::urls::is_external_scheme`]) to the OS. Emitted instead of
    /// creating or navigating a tab when the user opens such a URL (command bar, `OpenInput`,
    /// `OpenUrl`, `Navigate`, `SplitOpenInput`). It is a user action; the shell still refuses
    /// handler schemes with a history of abuse (`ms-msdt:`, `search-ms:`, …).
    OpenExternal { url: String },
    /// Minimize / ToggleMaximize / ToggleFullscreen (Close never appears here; see `Quit`).
    Window { action: crate::command::WindowAction },
    DownloadControl { id: u32, action: DownloadAction },
    /// Persist state and history synchronously now.
    SaveNow,
    /// Begin app shutdown: force-close all browsers, close the window, quit the message loop.
    Quit,

    // ------------------------------------------------------------------ AI agents (MCP)
    /// Agent access is on (`true`: create the pipe endpoint and write `agent-endpoint.json`) or off
    /// (`false`: `bye{access_off}` to every session, remove the endpoint). Emitted at startup and
    /// whenever `settings.agentAccess` switches between off and on.
    AgentEndpoint { enabled: bool },
    /// Answer an `AgentConnectionRequested` / `AgentSiteRequested` request.
    AgentAnswer { id: u64, allow: bool },
    /// The user pressed Stop: `bye{user_stopped}` to every session, release held input, turn
    /// emulation off.
    AgentDisconnect,
    /// Continue (`keep`) or cancel a held agent download.
    AgentDownload { id: u32, keep: bool },
    /// Show the agent overlay (`sta://agent/`): an approval prompt (`prompt`, the first of
    /// `UiState.agent.prompts`, re-emitted when that changes) or the activity panel.
    ShowAgentOverlay { prompt: bool },
    HideAgentOverlay,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum ContentLayout {
    /// No tab: themed empty state ("Ctrl+T to open a tab").
    Empty,
    Single { tab: Id },
    Split { orientation: Orientation, panes: Vec<Pane>, focused: usize },
}

impl ContentLayout {
    pub fn tabs(&self) -> Vec<Id> {
        match self {
            ContentLayout::Empty => Vec::new(),
            ContentLayout::Single { tab } => vec![*tab],
            ContentLayout::Split { panes, .. } => panes.iter().map(|p| p.tab).collect(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Pane {
    pub tab: Id,
    pub fraction: f32,
}
