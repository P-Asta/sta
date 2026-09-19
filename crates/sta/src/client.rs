//! CEF clients and browser handlers [owner: tabs] (ARCHITECTURE §4.1, §5; docs/research/handlers.md).
//!
//! Two clients, one per kind of browser, with cached handler objects:
//! - **UI client** (sidebar, topbar, overlays, internal-page tabs): forwards process messages to
//!   the IPC router; registers trust in `on_after_created`; host-locked `on_before_browse`
//!   (surfaces stay on their own `sta://<host>/`, internal tabs on `sta://`), plus the
//!   `about:blank` guard on address change / load start (`ui_escaped`: surfaces reload their URL,
//!   internal tabs `Navigate`); UI DragHandler → `window::on_draggable_regions_changed`;
//!   popups/new-tab links → `OpenUrl`; context menu reduced to edit commands; crashed surfaces
//!   reload; DevTools popups get a clean `extra_info`; permission prompts are dismissed (never
//!   left pending).
//! - **Tab client** (web tabs): display/load → core events; life span (popups adopted as tabs,
//!   Strategy B, see tabs.rs); request (blocks `sta://`, `intercept_navigation`,
//!   `on_open_urlfrom_tab` → `LinkOpenRequested`, host-matched boosts pushed before same-site
//!   main-frame navigations, crashes → `TabCrashed`); focus (cancel for hidden tabs, `TabFocused`);
//!   JS dialogs (auto-accept beforeunload while closing); downloads/permissions/find delegate to
//!   their modules; themed in-place error pages ([`error_page`]); renderer reports
//!   `sta.media` (debounced `TabAudioChanged`) and `sta.boosts.check`; context menu with
//!   link/image/Inspect items. **No** drag handler and **no** router forwarding.
//! - Both: external protocols (`mailto:` …) are cancelled and, with a user gesture, handed to the
//!   OS (external.rs).
//!
//! Shared by both: keyboard → `keyboard::on_pre_key_event`, focus → `overlays::on_browser_got_focus`,
//! life-span bookkeeping → `browsers::{on_after_created, on_do_close, on_before_close}`, zoom
//! reporting on `on_load_end`.
//!
//! Public API:
//! - `pub fn ui_client() -> Client`, `pub fn tab_client() -> Client` (created once, UI thread)
//! - `pub fn clear()`

// `wrap_client!` generates a `new` taking one argument per handler field.
#![allow(clippy::too_many_arguments)]

#[path = "context_menu.rs"]
mod context_menu;
#[path = "error_page.rs"]
mod error_page;

use crate::browsers::{self, Role, Surface};
use crate::{controller, downloads, external, ipc, keyboard, overlays, permissions, renderer, scheme, tabs, task, window};
use sta_core::{Command, Id, LinkDisposition, OpenTarget};
use cef::wrapper::message_router::MessageRouterBrowserSideHandlerCallbacks;
use cef::*;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

/// Minimum interval between two `TabLoadProgress` commands per browser (≈10/s).
const PROGRESS_INTERVAL: Duration = Duration::from_millis(100);
/// Media play/pause reports are coalesced for this long before `TabAudioChanged`.
const AUDIO_DEBOUNCE_MS: i64 = 250;

#[derive(Default)]
struct MediaState {
    /// Frame id → audible (only audible frames are kept).
    frames: HashSet<String>,
    /// Last value dispatched to core.
    reported: bool,
    /// A navigation or crash happened since the last dispatch (core may have reset `audible`).
    force: bool,
    generation: u64,
}

thread_local! {
    static UI_CLIENT: RefCell<Option<Client>> = const { RefCell::new(None) };
    static TAB_CLIENT: RefCell<Option<Client>> = const { RefCell::new(None) };
    static LAST_PROGRESS: RefCell<HashMap<i32, Instant>> = RefCell::new(HashMap::new());
    /// Web-tab browsers currently showing the shell's error page.
    static ERROR_PAGES: RefCell<HashSet<i32>> = RefCell::new(HashSet::new());
    static MEDIA: RefCell<HashMap<i32, MediaState>> = RefCell::new(HashMap::new());
    /// UI browsers whose main frame left `sta://` without `on_before_browse` (about:blank)
    /// and are being brought back (surface reload / `Navigate` for internal-page tabs).
    static UI_ESCAPES: RefCell<HashSet<i32>> = RefCell::new(HashSet::new());
    /// UI surface browser id → times of recent crash reloads.
    static SURFACE_RELOADS: RefCell<HashMap<i32, Vec<Instant>>> = RefCell::new(HashMap::new());
}

/// See [`allow_surface_reload`].
const SURFACE_RELOADS_PER_MINUTE: usize = 5;

// =================================================================================== UI location guard

/// `UiRequest::on_before_browse` never runs for `about:blank` (and `about:srcdoc`), so a UI page
/// could leave its trusted `sta://` document that way. Checked on every main-frame address
/// change and load start of a UI browser; returns `true` when `url` is such an escape (it must
/// then not be reported to core):
/// - a surface (sidebar, topbar, overlays) reloads its own `sta://<host>/` URL (posted);
/// - an internal-page tab asks core to navigate to `url`, which replaces the trusted browser by a
///   web one (`ReplaceBrowser`), exactly like a blocked `on_before_browse` would.
fn ui_escaped(browser: &Browser, url: &str) -> bool {
    let id = browser.identifier();
    if !browsers::is_ui_browser(id) || window::is_closing() {
        return false;
    }
    let lower = url.to_ascii_lowercase();
    if url.is_empty() || lower.starts_with("chrome-error:") {
        return false; // no document (yet) or Chromium's error page for a failed sta:// load
    }
    let role = browsers::role_of(id);
    let home = match role {
        Some(Role::Surface(surface)) => scheme::host_of(url) == Some(surface.host()),
        Some(Role::Tab(_)) => scheme::is_sta_url(url),
        // Neither a DevTools frontend nor an extension popup is a UI browser (`is_ui_browser`
        // already returned above); these arms only exist so the match stays exhaustive.
        Some(Role::DevTools { .. }) | Some(Role::ExtensionPopup) | None => return false,
    };
    if home {
        UI_ESCAPES.with(|e| e.borrow_mut().remove(&id));
        return false;
    }
    if !UI_ESCAPES.with(|e| e.borrow_mut().insert(id)) {
        return true; // already being handled (load start and address change of one navigation)
    }
    log_warn!("UI browser {id} ({role:?}) left sta:// for {url}; bringing it back");
    match role {
        Some(Role::Surface(surface)) => task::post_ui(move || {
            UI_ESCAPES.with(|e| e.borrow_mut().remove(&id));
            if window::is_closing() {
                return;
            }
            if let Some(frame) = browsers::browser(id).and_then(|b| b.main_frame()) {
                frame.load_url(Some(&CefString::from(surface.url().as_str())));
            }
        }),
        Some(Role::Tab(tab)) if !tabs::is_closing_browser(id) => {
            controller::dispatch(Command::Navigate { tab: Some(tab), url: url.to_string() });
        }
        _ => {}
    }
    true
}

fn is_ui_escaped(browser_id: i32) -> bool {
    UI_ESCAPES.with(|e| e.borrow().contains(&browser_id))
}

/// Crash-reload limiter for UI surfaces: at most [`SURFACE_RELOADS_PER_MINUTE`] automatic reloads
/// per browser per minute, so a renderer that dies on load can't spin the CPU forever.
fn allow_surface_reload(browser_id: i32) -> bool {
    let now = Instant::now();
    SURFACE_RELOADS.with(|r| {
        let mut r = r.borrow_mut();
        let times = r.entry(browser_id).or_default();
        times.retain(|t| now.duration_since(*t) < Duration::from_secs(60));
        let allowed = times.len() < SURFACE_RELOADS_PER_MINUTE;
        if allowed {
            times.push(now);
        }
        allowed
    })
}

/// The trusted UI client.
pub fn ui_client() -> Client {
    if let Some(c) = UI_CLIENT.with(|c| c.borrow().clone()) {
        return c;
    }
    let client = UiClient::new(
        UiLifeSpan::new(),
        UiRequest::new(),
        UiDrag::new(),
        ShellKeyboard::new(),
        ShellFocus::new(),
        TabDisplay::new(),
        TabLoad::new(),
        ShellJsDialog::new(),
        UiPermission::new(),
        context_menu::ui_handler(),
    );
    UI_CLIENT.with(|c| *c.borrow_mut() = Some(client.clone()));
    client
}

/// The web-content client.
pub fn tab_client() -> Client {
    if let Some(c) = TAB_CLIENT.with(|c| c.borrow().clone()) {
        return c;
    }
    let client = TabClient::new(
        TabLifeSpan::new(),
        TabRequest::new(),
        ShellKeyboard::new(),
        ShellFocus::new(),
        TabDisplay::new(),
        TabLoad::new(),
        ShellJsDialog::new(),
        TabDownload::new(),
        TabPermission::new(),
        TabFind::new(),
        context_menu::tab_handler(),
        crate::automation::guards::dialog_handler(),
    );
    TAB_CLIENT.with(|c| *c.borrow_mut() = Some(client.clone()));
    client
}

/// Client for the DevTools browser of `inspected_browser` (its own Chrome-style window): only a
/// keyboard handler, so F12 / Ctrl+Shift+I inside DevTools close them like in Chrome. Everything
/// else keeps CEF's defaults (no role, no trust, no shell focus policy).
pub fn devtools_client(inspected_browser: i32) -> Client {
    DevToolsClient::new(DevToolsKeyboard::new(inspected_browser))
}

wrap_client! {
    struct DevToolsClient {
        keyboard: KeyboardHandler,
    }

    impl Client {
        fn keyboard_handler(&self) -> Option<KeyboardHandler> {
            Some(self.keyboard.clone())
        }
    }
}

wrap_keyboard_handler! {
    struct DevToolsKeyboard {
        inspected: i32,
    }

    impl KeyboardHandler {
        fn on_pre_key_event(
            &self,
            _browser: Option<&mut Browser>,
            event: Option<&KeyEvent>,
            _os_event: crate::platform::OsEvent<'_>,
            _is_keyboard_shortcut: Option<&mut i32>,
        ) -> i32 {
            let Some(event) = event else { return 0 };
            if !keyboard::is_devtools_toggle(event) {
                return 0;
            }
            let inspected = self.inspected;
            // Closing destroys this browser: never inside its own key callback.
            task::post_ui(move || {
                if let Some(host) = browsers::browser(inspected).and_then(|b| b.host()) {
                    host.close_dev_tools();
                }
            });
            1
        }
    }
}

/// Drops the cached clients (before `cef::shutdown()`).
pub fn clear() {
    let ui = UI_CLIENT.with(|c| c.borrow_mut().take());
    let tab = TAB_CLIENT.with(|c| c.borrow_mut().take());
    LAST_PROGRESS.with(|p| p.borrow_mut().clear());
    ERROR_PAGES.with(|e| e.borrow_mut().clear());
    MEDIA.with(|m| m.borrow_mut().clear());
    UI_ESCAPES.with(|e| e.borrow_mut().clear());
    SURFACE_RELOADS.with(|r| r.borrow_mut().clear());
    drop((ui, tab));
}

/// The tab a browser reports for. A browser whose close is in progress (e.g. the old browser of a
/// `ReplaceBrowser`, which keeps its role until `on_before_close`) no longer reports anything.
fn tab_of(browser: &Browser) -> Option<Id> {
    let id = browser.identifier();
    match browsers::role_of(id) {
        Some(Role::Tab(tab)) if !tabs::is_closing_browser(id) => Some(tab),
        _ => None,
    }
}

/// A web tab (tab client), not an internal page.
fn web_tab_of(browser: &Browser) -> Option<Id> {
    tab_of(browser).filter(|_| !browsers::is_ui_browser(browser.identifier()))
}

/// One Alt+click preview per browser per this long. The browser process never sees content-area
/// input, so nothing here can corroborate that a real click happened; this is what keeps a
/// *compromised* renderer from raising Peek after Peek with no user interaction at all. A human
/// cannot click twice this fast on purpose, and the first click has already opened the Peek.
const PREVIEW_INTERVAL: Duration = Duration::from_millis(250);

thread_local! {
    /// When each browser last had a `sta.preview` accepted (UI thread only).
    static LAST_PREVIEW: RefCell<HashMap<i32, Instant>> = RefCell::new(HashMap::new());
}

/// The tab an `sta.preview` message may preview in, or `None` when the browser side refuses it
/// (renderer.rs `MSG_PREVIEW`, PROTOCOL §13):
/// - the sender must be a **web tab**, and not an **agent-controlled** one — the same rule
///   `intercept_navigation` follows (`guards::allow_peek`), so nothing an automation session does in
///   a tab it drives can raise an overlay in front of the user;
/// - not faster than [`PREVIEW_INTERVAL`];
/// - a `file:` URL only from a `file:` **frame**. Core's allowlist compares against the *tab's*
///   URL, which would let a remote iframe inside a locally saved page borrow its `file:` origin.
fn preview_target(browser: &Browser, frame_url: &str, url: &str) -> Option<Id> {
    let id = browser.identifier();
    let tab = web_tab_of(browser)?;
    if !crate::automation::guards::allow_peek(id) {
        log_info!("agent guard: preview gesture ignored in tab {tab}");
        return None;
    }
    if sta_core::urls::parsed_scheme(url).as_deref() == Some("file") && sta_core::urls::parsed_scheme(frame_url).as_deref() != Some("file") {
        log_warn!("preview: file: URL refused from a non-file: frame of tab {tab}");
        return None;
    }
    let now = Instant::now();
    LAST_PREVIEW.with(|m| {
        let mut m = m.borrow_mut();
        if m.get(&id).is_some_and(|t| now.duration_since(*t) < PREVIEW_INTERVAL) {
            return None;
        }
        m.retain(|_, t| now.duration_since(*t) < Duration::from_secs(60));
        m.insert(id, now);
        Some(tab)
    })
}

fn cef_str(s: Option<&CefString>) -> String {
    s.map(CefString::to_string).unwrap_or_default()
}

/// Maps a CEF disposition to a core link disposition (`None` = handle in place).
fn link_disposition(d: WindowOpenDisposition) -> Option<LinkDisposition> {
    if d == WindowOpenDisposition::NEW_FOREGROUND_TAB || d == WindowOpenDisposition::OFF_THE_RECORD {
        Some(LinkDisposition::ForegroundTab)
    } else if d == WindowOpenDisposition::NEW_BACKGROUND_TAB {
        Some(LinkDisposition::BackgroundTab)
    } else if d == WindowOpenDisposition::NEW_WINDOW
        || d == WindowOpenDisposition::NEW_POPUP
        || d == WindowOpenDisposition::NEW_SPLIT_VIEW
    {
        Some(LinkDisposition::NewWindow)
    } else {
        None
    }
}

// =================================================================================== media

/// `sta.media` from a frame of a web tab.
fn on_media_report(browser_id: i32, frame_id: String, audible: bool) {
    let generation = MEDIA.with(|m| {
        let mut m = m.borrow_mut();
        let state = m.entry(browser_id).or_default();
        if audible {
            state.frames.insert(frame_id);
        } else {
            state.frames.remove(&frame_id);
        }
        state.generation += 1;
        state.generation
    });
    task::post_ui_delayed(AUDIO_DEBOUNCE_MS, move || flush_media(browser_id, generation));
}

/// A new main-frame document or a crashed renderer: every frame is silent now.
fn reset_media(browser_id: i32) {
    let generation = MEDIA.with(|m| {
        let mut m = m.borrow_mut();
        let state = m.get_mut(&browser_id)?;
        state.frames.clear();
        state.force = true;
        state.generation += 1;
        Some(state.generation)
    });
    if let Some(generation) = generation {
        task::post_ui_delayed(AUDIO_DEBOUNCE_MS, move || flush_media(browser_id, generation));
    }
}

fn flush_media(browser_id: i32, generation: u64) {
    let dispatch = MEDIA.with(|m| {
        let mut m = m.borrow_mut();
        let state = m.get_mut(&browser_id)?;
        if state.generation != generation {
            return None;
        }
        let audible = !state.frames.is_empty();
        if audible == state.reported && !state.force {
            return None;
        }
        state.reported = audible;
        state.force = false;
        Some(audible)
    });
    let Some(audible) = dispatch else { return };
    if let Some(tab) = tabs::tab_for_browser(browser_id) {
        controller::dispatch(Command::TabAudioChanged { tab, audible });
    }
}

/// Per-browser cleanup for web tabs (`on_before_close`).
fn forget_browser(browser_id: i32) {
    LAST_PROGRESS.with(|p| p.borrow_mut().remove(&browser_id));
    ERROR_PAGES.with(|e| e.borrow_mut().remove(&browser_id));
    MEDIA.with(|m| m.borrow_mut().remove(&browser_id));
    permissions::on_browser_closed(browser_id);
    crate::automation::on_browser_closed(browser_id);
}

// =================================================================================== clients

wrap_client! {
    struct UiClient {
        life_span: LifeSpanHandler,
        request: RequestHandler,
        drag: DragHandler,
        keyboard: KeyboardHandler,
        focus: FocusHandler,
        display: DisplayHandler,
        load: LoadHandler,
        jsdialog: JsdialogHandler,
        permission: PermissionHandler,
        context_menu: ContextMenuHandler,
    }

    impl Client {
        fn life_span_handler(&self) -> Option<LifeSpanHandler> {
            Some(self.life_span.clone())
        }

        fn request_handler(&self) -> Option<RequestHandler> {
            Some(self.request.clone())
        }

        fn drag_handler(&self) -> Option<DragHandler> {
            Some(self.drag.clone())
        }

        fn keyboard_handler(&self) -> Option<KeyboardHandler> {
            Some(self.keyboard.clone())
        }

        fn focus_handler(&self) -> Option<FocusHandler> {
            Some(self.focus.clone())
        }

        fn display_handler(&self) -> Option<DisplayHandler> {
            Some(self.display.clone())
        }

        fn load_handler(&self) -> Option<LoadHandler> {
            Some(self.load.clone())
        }

        fn jsdialog_handler(&self) -> Option<JsdialogHandler> {
            Some(self.jsdialog.clone())
        }

        fn permission_handler(&self) -> Option<PermissionHandler> {
            Some(self.permission.clone())
        }

        fn context_menu_handler(&self) -> Option<ContextMenuHandler> {
            Some(self.context_menu.clone())
        }

        fn on_process_message_received(
            &self,
            browser: Option<&mut Browser>,
            frame: Option<&mut Frame>,
            source_process: ProcessId,
            message: Option<&mut ProcessMessage>,
        ) -> i32 {
            ipc::router().on_process_message_received(browser.cloned(), frame.cloned(), source_process, message.cloned()) as i32
        }
    }
}

wrap_client! {
    struct TabClient {
        life_span: LifeSpanHandler,
        request: RequestHandler,
        keyboard: KeyboardHandler,
        focus: FocusHandler,
        display: DisplayHandler,
        load: LoadHandler,
        jsdialog: JsdialogHandler,
        download: DownloadHandler,
        permission: PermissionHandler,
        find: FindHandler,
        context_menu: ContextMenuHandler,
        // Agent-controlled tabs never show native file choosers (automation/guards.rs).
        dialog: DialogHandler,
    }

    impl Client {
        fn life_span_handler(&self) -> Option<LifeSpanHandler> {
            Some(self.life_span.clone())
        }

        fn request_handler(&self) -> Option<RequestHandler> {
            Some(self.request.clone())
        }

        fn keyboard_handler(&self) -> Option<KeyboardHandler> {
            Some(self.keyboard.clone())
        }

        fn focus_handler(&self) -> Option<FocusHandler> {
            Some(self.focus.clone())
        }

        fn display_handler(&self) -> Option<DisplayHandler> {
            Some(self.display.clone())
        }

        fn load_handler(&self) -> Option<LoadHandler> {
            Some(self.load.clone())
        }

        fn jsdialog_handler(&self) -> Option<JsdialogHandler> {
            Some(self.jsdialog.clone())
        }

        fn dialog_handler(&self) -> Option<DialogHandler> {
            Some(self.dialog.clone())
        }

        fn download_handler(&self) -> Option<DownloadHandler> {
            Some(self.download.clone())
        }

        fn permission_handler(&self) -> Option<PermissionHandler> {
            Some(self.permission.clone())
        }

        fn find_handler(&self) -> Option<FindHandler> {
            Some(self.find.clone())
        }

        fn context_menu_handler(&self) -> Option<ContextMenuHandler> {
            Some(self.context_menu.clone())
        }

        fn on_process_message_received(
            &self,
            browser: Option<&mut Browser>,
            frame: Option<&mut Frame>,
            _source_process: ProcessId,
            message: Option<&mut ProcessMessage>,
        ) -> i32 {
            // Web content never reaches the IPC router. Only shell-defined renderer reports.
            let (Some(browser), Some(frame), Some(message)) = (browser, frame, message) else { return 0 };
            let name = CefString::from(&message.name()).to_string();
            if name == renderer::MSG_MEDIA {
                let audible = message.argument_list().is_some_and(|args| args.bool(0) != 0);
                if web_tab_of(browser).is_some() {
                    let frame_id = CefString::from(&frame.identifier()).to_string();
                    on_media_report(browser.identifier(), frame_id, audible);
                }
                return 1;
            }
            if name == renderer::MSG_BOOSTS_CHECK {
                let version = message.argument_list().map(|args| CefString::from(&args.string(0)).to_string()).unwrap_or_default();
                if frame.is_main() != 0 && web_tab_of(browser).is_some() {
                    tabs::on_boosts_check(browser, frame, &version);
                }
                return 1;
            }
            if name == renderer::MSG_PREVIEW {
                // Alt+click on a link (PROTOCOL §13). Any frame of a web tab may ask (`preview_target`
                // decides); the renderer has already cancelled the click, and core applies the same
                // URL allowlist and the same Peek rules as every other link
                // (`urls::web_content_may_open`).
                let url = message.argument_list().map(|args| CefString::from(&args.string(0)).to_string()).unwrap_or_default();
                if !url.is_empty()
                    && let Some(tab) = preview_target(browser, &CefString::from(&frame.url()).to_string(), &url)
                {
                    controller::dispatch(Command::LinkOpenRequested { opener: tab, url, disposition: LinkDisposition::Preview });
                }
                return 1;
            }
            0
        }
    }
}

// =================================================================================== UI handlers

wrap_life_span_handler! {
    struct UiLifeSpan;

    impl LifeSpanHandler {
        fn on_before_popup(
            &self,
            _browser: Option<&mut Browser>,
            _frame: Option<&mut Frame>,
            _popup_id: i32,
            target_url: Option<&CefString>,
            _target_frame_name: Option<&CefString>,
            target_disposition: WindowOpenDisposition,
            user_gesture: i32,
            _popup_features: Option<&PopupFeatures>,
            _window_info: Option<&mut WindowInfo>,
            _client: Option<&mut Option<Client>>,
            _settings: Option<&mut BrowserSettings>,
            _extra_info: Option<&mut Option<DictionaryValue>>,
            _no_javascript_access: Option<&mut i32>,
        ) -> i32 {
            // UI pages never open windows: open the URL as a tab instead.
            let url = cef_str(target_url);
            if external::is_external(&url) {
                external::open(&url, user_gesture != 0, "UI popup");
            } else if !url.is_empty() {
                let target = if target_disposition == WindowOpenDisposition::NEW_BACKGROUND_TAB {
                    OpenTarget::BackgroundTab
                } else {
                    OpenTarget::NewTab
                };
                controller::dispatch(Command::OpenUrl { url, target, opener: None });
            }
            1
        }

        fn on_before_dev_tools_popup(
            &self,
            _browser: Option<&mut Browser>,
            _window_info: Option<&mut WindowInfo>,
            _client: Option<&mut Option<Client>>,
            _settings: Option<&mut BrowserSettings>,
            extra_info: Option<&mut Option<DictionaryValue>>,
            _use_default_window: Option<&mut i32>,
        ) {
            // DevTools of an internal page must not inherit `sta_ui` (renderer trust). It gets no
            // `sta_devtools_shim`: this window's embedder is Chromium's own DevToolsWindow, which is
            // already wired to an agent host — sta's shim would replace `sendMessageToBackend` with
            // a message nothing in this window answers (see renderer.rs).
            if let (Some(extra), Some(info)) = (extra_info, dictionary_value_create()) {
                info.set_bool(Some(&CefString::from(renderer::EXTRA_DEVTOOLS)), 1);
                *extra = Some(info);
            }
        }

        fn on_after_created(&self, browser: Option<&mut Browser>) {
            if let Some(browser) = browser {
                browsers::on_after_created(browser, true);
            }
        }

        fn do_close(&self, browser: Option<&mut Browser>) -> i32 {
            browser.map(|b| browsers::on_do_close(b)).unwrap_or(1)
        }

        fn on_before_close(&self, browser: Option<&mut Browser>) {
            if let Some(browser) = browser {
                LAST_PROGRESS.with(|p| p.borrow_mut().remove(&browser.identifier()));
                UI_ESCAPES.with(|e| e.borrow_mut().remove(&browser.identifier()));
                SURFACE_RELOADS.with(|r| r.borrow_mut().remove(&browser.identifier()));
                browsers::on_before_close(browser);
            }
        }
    }
}

wrap_request_handler! {
    struct UiRequest;

    impl RequestHandler {
        fn on_before_browse(
            &self,
            browser: Option<&mut Browser>,
            frame: Option<&mut Frame>,
            request: Option<&mut Request>,
            user_gesture: i32,
            _is_redirect: i32,
        ) -> i32 {
            let (Some(browser), Some(frame)) = (browser, frame) else { return 1 };
            let url = request.map(|r| CefString::from(&r.url()).to_string()).unwrap_or_default();
            if frame.is_main() != 0 {
                let role = browsers::role_of(browser.identifier());
                let allowed = match role {
                    Some(Role::Surface(surface)) => scheme::host_of(&url) == Some(surface.host()),
                    // Internal pages may move between sta:// hosts only.
                    Some(Role::Tab(_)) | None => scheme::is_sta_url(&url),
                    // The UI client never serves a DevTools frontend or an extension popup page
                    // (devtools.rs and ext_popup.rs have their own).
                    Some(Role::DevTools { .. }) | Some(Role::ExtensionPopup) => false,
                };
                if !allowed {
                    log_debug!("UI navigation blocked ({role:?}): {url}");
                    if external::is_external(&url) {
                        external::open(&url, user_gesture != 0, "UI navigation");
                        return 1;
                    }
                    match role {
                        Some(Role::Tab(tab)) => controller::dispatch(Command::Navigate { tab: Some(tab), url }),
                        _ if !url.is_empty() => {
                            controller::dispatch(Command::OpenUrl { url, target: OpenTarget::NewTab, opener: None })
                        }
                        _ => {}
                    }
                    return 1;
                }
            }
            ipc::router().on_before_browse(Some(browser.clone()), Some(frame.clone()));
            0
        }

        fn on_open_urlfrom_tab(
            &self,
            _browser: Option<&mut Browser>,
            _frame: Option<&mut Frame>,
            target_url: Option<&CefString>,
            target_disposition: WindowOpenDisposition,
            user_gesture: i32,
        ) -> i32 {
            let url = cef_str(target_url);
            let target = if target_disposition == WindowOpenDisposition::NEW_BACKGROUND_TAB {
                OpenTarget::BackgroundTab
            } else {
                OpenTarget::NewTab
            };
            if external::is_external(&url) {
                external::open(&url, user_gesture != 0, "UI link");
            } else if !url.is_empty() {
                controller::dispatch(Command::OpenUrl { url, target, opener: None });
            }
            1
        }

        fn on_render_process_terminated(
            &self,
            browser: Option<&mut Browser>,
            _status: TerminationStatus,
            error_code: i32,
            _error_string: Option<&CefString>,
        ) {
            let Some(browser) = browser else { return };
            ipc::router().on_render_process_terminated(Some(browser.clone()));
            let id = browser.identifier();
            log_warn!("UI renderer of browser {id} terminated ({error_code})");
            match browsers::role_of(id) {
                Some(Role::Tab(tab)) => controller::dispatch(Command::TabCrashed { tab }),
                Some(Role::Surface(surface)) if !window::is_closing() => {
                    if surface == Surface::Sidebar {
                        // No floating sidebar with a dead page; it shows again after `ui.ready`.
                        task::post_ui(crate::sidebar_hover::on_sidebar_gone);
                    }
                    if !allow_surface_reload(id) {
                        log_error!("{surface:?} renderer keeps crashing; not reloading it again for a minute");
                        return;
                    }
                    // Reload the surface outside the callback.
                    task::post_ui(move || {
                        if let Some(b) = browsers::browser(id) {
                            b.reload();
                        }
                    });
                }
                _ => {}
            }
        }
    }
}

wrap_drag_handler! {
    struct UiDrag;

    impl DragHandler {
        fn on_draggable_regions_changed(
            &self,
            browser: Option<&mut Browser>,
            frame: Option<&mut Frame>,
            regions: Option<&[DraggableRegion]>,
        ) {
            let (Some(browser), Some(frame)) = (browser, frame) else { return };
            if frame.is_main() == 0 {
                return;
            }
            let regions = regions.map(<[DraggableRegion]>::to_vec).unwrap_or_default();
            if matches!(browsers::role_of(browser.identifier()), Some(Role::Surface(Surface::Sidebar | Surface::Topbar))) {
                log_debug!("draggable regions from browser {}: {}", browser.identifier(), regions.len());
            }
            window::on_draggable_regions_changed(browser.identifier(), regions);
        }
    }
}

// =================================================================================== shared handlers

wrap_keyboard_handler! {
    struct ShellKeyboard;

    impl KeyboardHandler {
        /// A key the page left unhandled, on its way to the normal-priority accelerators. Keys
        /// injected through the DevTools protocol (no OS message) stop here: see keyboard.rs.
        fn on_key_event(&self, browser: Option<&mut Browser>, event: Option<&KeyEvent>, os_event: crate::platform::OsEvent<'_>) -> i32 {
            let (Some(browser), Some(event)) = (browser, event) else { return 0 };
            keyboard::on_key_event(browser.identifier(), event, crate::platform::os_event_present(&os_event)) as i32
        }

        fn on_pre_key_event(
            &self,
            browser: Option<&mut Browser>,
            event: Option<&KeyEvent>,
            os_event: crate::platform::OsEvent<'_>,
            _is_keyboard_shortcut: Option<&mut i32>,
        ) -> i32 {
            let (Some(browser), Some(event)) = (browser, event) else { return 0 };
            // Real keyboard input (with a native message) in a tab hands control back to the user.
            crate::automation::on_pre_key_event(browser.identifier(), crate::platform::os_event_present(&os_event));
            keyboard::on_pre_key_event(browser.identifier(), event) as i32
        }
    }
}

wrap_focus_handler! {
    struct ShellFocus;

    impl FocusHandler {
        fn on_set_focus(&self, browser: Option<&mut Browser>, _source: FocusSource) -> i32 {
            // Background tabs must not steal focus when they navigate.
            let Some(browser) = browser else { return 0 };
            let id = browser.identifier();
            match browsers::role_of(id) {
                Some(Role::Tab(tab)) => (tabs::is_closing_browser(id) || !tabs::is_tab_visible(tab)) as i32,
                _ => 0,
            }
        }

        fn on_got_focus(&self, browser: Option<&mut Browser>) {
            let Some(browser) = browser else { return };
            if let Some(tab) = tab_of(browser)
                && !overlays::focus_events_suppressed()
            {
                controller::dispatch(Command::TabFocused { tab });
            }
            overlays::on_browser_got_focus(browser.identifier());
        }
    }
}

wrap_jsdialog_handler! {
    struct ShellJsDialog;

    impl JsdialogHandler {
        fn on_jsdialog(
            &self,
            browser: Option<&mut Browser>,
            _origin_url: Option<&CefString>,
            dialog_type: JsdialogType,
            message_text: Option<&CefString>,
            default_prompt_text: Option<&CefString>,
            callback: Option<&mut JsdialogCallback>,
            _suppress_message: Option<&mut i32>,
        ) -> i32 {
            // Agent-controlled tabs: the dialog waits for `handle_dialog` (automation/guards.rs).
            let (Some(browser), Some(callback)) = (browser, callback) else { return 0 };
            crate::automation::guards::on_jsdialog(browser.identifier(), dialog_type, &cef_str(message_text), &cef_str(default_prompt_text), callback) as i32
        }

        fn on_before_unload_dialog(
            &self,
            browser: Option<&mut Browser>,
            _message_text: Option<&CefString>,
            _is_reload: i32,
            callback: Option<&mut JsdialogCallback>,
        ) -> i32 {
            // Closes requested by core are not cancelable: auto-accept.
            let Some(browser) = browser else { return 0 };
            if window::is_closing() || tabs::is_closing_browser(browser.identifier()) {
                if let Some(cb) = callback {
                    cb.cont(1, None);
                }
                return 1;
            }
            // Agent navigations leave the page without asking.
            if let Some(cb) = callback
                && crate::automation::guards::on_before_unload(browser.identifier(), cb)
            {
                return 1;
            }
            0
        }

        fn on_reset_dialog_state(&self, browser: Option<&mut Browser>) {
            if let Some(browser) = browser {
                crate::automation::guards::on_dialog_state_reset(browser.identifier());
            }
        }
    }
}

wrap_display_handler! {
    struct TabDisplay;

    impl DisplayHandler {
        fn on_address_change(&self, browser: Option<&mut Browser>, frame: Option<&mut Frame>, url: Option<&CefString>) {
            let (Some(browser), Some(frame)) = (browser, frame) else { return };
            if frame.is_main() == 0 {
                return;
            }
            let url = cef_str(url);
            if ui_escaped(browser, &url) {
                return;
            }
            let Some(tab) = tab_of(browser) else { return };
            // One-time grants of the previous origin may expire; no auto-blocker data may linger.
            permissions::on_tab_navigated(&url);
            if url.to_ascii_lowercase().starts_with("chrome-error:") {
                return; // the error page is never reported
            }
            // A real document committed: the error page (if any) is gone.
            ERROR_PAGES.with(|e| e.borrow_mut().remove(&browser.identifier()));
            controller::dispatch(Command::TabAddressChanged { tab, url });
        }

        fn on_title_change(&self, browser: Option<&mut Browser>, title: Option<&CefString>) {
            let Some(browser) = browser else { return };
            let Some(tab) = tab_of(browser) else { return };
            if ERROR_PAGES.with(|e| e.borrow().contains(&browser.identifier())) || is_ui_escaped(browser.identifier()) {
                return; // titles of Chromium's / the shell's error document, or of an escaped UI page
            }
            controller::dispatch(Command::TabTitleChanged { tab, title: cef_str(title) });
        }

        fn on_favicon_urlchange(&self, browser: Option<&mut Browser>, icon_urls: Option<&mut CefStringList>) {
            let Some(browser) = browser else { return };
            let Some(tab) = tab_of(browser) else { return };
            // Never clone a borrowed CefStringList (handlers.md §3): move it out.
            let urls: Vec<String> = icon_urls.map(|l| std::mem::take(l).into_iter().collect()).unwrap_or_default();
            if ERROR_PAGES.with(|e| e.borrow().contains(&browser.identifier())) || is_ui_escaped(browser.identifier()) {
                return;
            }
            controller::dispatch(Command::TabFaviconChanged { tab, url: urls.into_iter().next() });
        }

        fn on_console_message(&self, browser: Option<&mut Browser>, level: LogSeverity, message: Option<&CefString>, source: Option<&CefString>, line: i32) -> i32 {
            if let Some(b) = browser {
                crate::automation::on_console_message(b.identifier(), level, &cef_str(message), &cef_str(source), line);
            }
            0
        }

        fn on_fullscreen_mode_change(&self, browser: Option<&mut Browser>, fullscreen: i32) {
            if let Some(b) = browser.as_deref()
                && crate::automation::guards::on_fullscreen(b.identifier(), fullscreen != 0)
            {
                return; // agent-controlled tabs never go fullscreen
            }
            let Some(tab) = browser.and_then(|b| tab_of(b)) else { return };
            controller::dispatch(Command::TabFullscreenChanged { tab, fullscreen: fullscreen != 0 });
        }

        fn on_loading_progress_change(&self, browser: Option<&mut Browser>, progress: f64) {
            let Some(browser) = browser else { return };
            let Some(tab) = tab_of(browser) else { return };
            let id = browser.identifier();
            let now = Instant::now();
            let send = progress >= 1.0
                || LAST_PROGRESS.with(|p| {
                    let mut p = p.borrow_mut();
                    let due = p.get(&id).is_none_or(|t| now.duration_since(*t) >= PROGRESS_INTERVAL);
                    if due {
                        p.insert(id, now);
                    }
                    due
                });
            if send {
                controller::dispatch(Command::TabLoadProgress { tab, progress: progress.clamp(0.0, 1.0) as f32 });
            }
        }

        fn on_contents_bounds_change(&self, _browser: Option<&mut Browser>, _new_bounds: Option<&Rect>) -> i32 {
            1 // ignore window.moveTo/resizeTo
        }
    }
}

wrap_load_handler! {
    struct TabLoad;

    impl LoadHandler {
        fn on_loading_state_change(&self, browser: Option<&mut Browser>, is_loading: i32, can_go_back: i32, can_go_forward: i32) {
            let Some(tab) = browser.and_then(|b| tab_of(b)) else { return };
            controller::dispatch(Command::TabLoadingStateChanged {
                tab,
                loading: is_loading != 0,
                can_go_back: can_go_back != 0,
                can_go_forward: can_go_forward != 0,
            });
        }

        fn on_load_start(&self, browser: Option<&mut Browser>, frame: Option<&mut Frame>, _transition_type: TransitionType) {
            let (Some(browser), Some(frame)) = (browser, frame) else { return };
            if frame.is_main() == 0 {
                return;
            }
            if browsers::is_ui_browser(browser.identifier()) {
                if browsers::role_of(browser.identifier()) == Some(Role::Surface(Surface::Sidebar)) {
                    // A reload or navigation: the old document's menus (its hover lock) are gone.
                    task::post_ui(crate::sidebar_hover::on_sidebar_load_start);
                }
                ui_escaped(browser, &CefString::from(&frame.url()).to_string());
            } else if web_tab_of(browser).is_some() {
                // A new main-frame document: media of the old one is gone.
                reset_media(browser.identifier());
                crate::automation::on_load_start(browser.identifier());
            }
        }

        fn on_load_end(&self, browser: Option<&mut Browser>, frame: Option<&mut Frame>, _http_status_code: i32) {
            let (Some(browser), Some(frame)) = (browser, frame) else { return };
            if frame.is_main() == 0 {
                return;
            }
            if let Some(tab) = tab_of(browser) {
                tabs::report_zoom_later(tab);
            }
        }

        fn on_load_error(
            &self,
            browser: Option<&mut Browser>,
            frame: Option<&mut Frame>,
            error_code: Errorcode,
            error_text: Option<&CefString>,
            failed_url: Option<&CefString>,
        ) {
            if error_code == Errorcode::ABORTED {
                return;
            }
            let (Some(browser), Some(frame)) = (browser, frame) else { return };
            if frame.is_main() == 0 {
                return;
            }
            let Some(tab) = tab_of(browser) else { return };
            let code = cef::sys::cef_errorcode_t::from(error_code) as i32;
            let url = cef_str(failed_url);
            let text = cef_str(error_text);
            log_debug!("tab {tab}: load error {code} {text} for {url}");
            crate::automation::on_load_error(browser.identifier(), code, &text);
            controller::dispatch(Command::TabLoadFailed { tab, url: url.clone(), error_code: code, error_text: text.clone() });
            if web_tab_of(browser).is_none() {
                return; // internal pages keep Chromium's behaviour
            }
            // Replace Chromium's committed error document in place (see error_page.rs); its title
            // and favicon are never reported, and the tab title falls back to the host.
            ERROR_PAGES.with(|e| e.borrow_mut().insert(browser.identifier()));
            controller::dispatch(Command::TabTitleChanged { tab, title: String::new() });
            let script = error_page::script(&url, code, &text);
            frame.execute_java_script(Some(&CefString::from(script.as_str())), Some(&CefString::from("")), 1);
        }
    }
}

// =================================================================================== tab handlers

wrap_life_span_handler! {
    struct TabLifeSpan;

    impl LifeSpanHandler {
        fn on_before_popup(
            &self,
            browser: Option<&mut Browser>,
            _frame: Option<&mut Frame>,
            popup_id: i32,
            target_url: Option<&CefString>,
            _target_frame_name: Option<&CefString>,
            target_disposition: WindowOpenDisposition,
            user_gesture: i32,
            popup_features: Option<&PopupFeatures>,
            _window_info: Option<&mut WindowInfo>,
            _client: Option<&mut Option<Client>>,
            _settings: Option<&mut BrowserSettings>,
            extra_info: Option<&mut Option<DictionaryValue>>,
            _no_javascript_access: Option<&mut i32>,
        ) -> i32 {
            let Some(browser) = browser else { return 1 };
            if target_disposition == WindowOpenDisposition::NEW_PICTURE_IN_PICTURE {
                tabs::skip_popup(browser.identifier(), popup_id);
                return 0; // CEF's default PiP window
            }
            let Some(opener) = tab_of(browser) else { return 1 };
            let url = cef_str(target_url);
            if scheme::is_sta_url(&url) {
                log_warn!("blocked sta:// popup from web tab {opener}: {url}");
                return 1;
            }
            if external::is_external(&url) {
                // `target=_blank` mailto: links etc.: no (blank) popup tab, the OS handles it.
                let gesture = crate::automation::guards::external_gesture(browser.identifier(), user_gesture != 0);
                external::open(&url, gesture, "popup");
                return 1;
            }
            // Strategy B: allow; tabs.rs adopts the popup BrowserView (window.opener survives).
            let features_popup = popup_features.is_some_and(|f| f.is_popup != 0);
            match tabs::prepare_popup(browser.identifier(), popup_id, &url, target_disposition, features_popup) {
                Some(info) => {
                    if let Some(extra) = extra_info {
                        *extra = Some(info);
                    }
                    0
                }
                None => 1,
            }
        }

        fn on_before_popup_aborted(&self, browser: Option<&mut Browser>, popup_id: i32) {
            if let Some(browser) = browser {
                tabs::popup_aborted(browser.identifier(), popup_id);
            }
        }

        fn on_before_dev_tools_popup(
            &self,
            _browser: Option<&mut Browser>,
            _window_info: Option<&mut WindowInfo>,
            _client: Option<&mut Option<Client>>,
            _settings: Option<&mut BrowserSettings>,
            extra_info: Option<&mut Option<DictionaryValue>>,
            _use_default_window: Option<&mut i32>,
        ) {
            // DevTools would otherwise inherit the tab's extra_info (renderer web-tab features).
            // No `sta_devtools_shim`: an undocked window is Chromium's own DevToolsWindow and keeps
            // its real `InspectorFrontendHost` (renderer.rs; the shim once swallowed its protocol).
            if let (Some(extra), Some(info)) = (extra_info, dictionary_value_create()) {
                info.set_bool(Some(&CefString::from(renderer::EXTRA_DEVTOOLS)), 1);
                *extra = Some(info);
            }
        }

        fn on_after_created(&self, browser: Option<&mut Browser>) {
            if let Some(browser) = browser {
                browsers::on_after_created(browser, false);
            }
        }

        fn do_close(&self, browser: Option<&mut Browser>) -> i32 {
            browser.map(|b| browsers::on_do_close(b)).unwrap_or(0)
        }

        fn on_before_close(&self, browser: Option<&mut Browser>) {
            if let Some(browser) = browser {
                forget_browser(browser.identifier());
                browsers::on_before_close(browser);
            }
        }
    }
}

wrap_request_handler! {
    struct TabRequest;

    impl RequestHandler {
        fn on_before_browse(
            &self,
            browser: Option<&mut Browser>,
            frame: Option<&mut Frame>,
            request: Option<&mut Request>,
            user_gesture: i32,
            is_redirect: i32,
        ) -> i32 {
            let url = request.map(|r| CefString::from(&r.url()).to_string()).unwrap_or_default();
            // Web content can never load shell pages, in any frame.
            if scheme::is_sta_url(&url) {
                log_warn!("blocked sta:// navigation in a web tab: {url}");
                return 1;
            }
            let (Some(browser), Some(frame)) = (browser, frame) else { return 0 };
            // Agent-controlled tabs: blocked hosts (any frame), unapproved sites (automation/guards.rs).
            if crate::automation::guards::cancel_navigation(browser.identifier(), &url, frame.is_main() != 0) {
                return 1;
            }
            if frame.is_main() == 0 {
                return 0;
            }
            if external::is_external(&url) {
                // Chromium would commit an error page: hand gesture navigations to the OS and keep
                // the current page either way.
                let gesture = crate::automation::guards::external_gesture(browser.identifier(), user_gesture != 0);
                external::open(&url, gesture, "navigation");
                return 1;
            }
            let Some(tab) = tab_of(browser) else { return 0 };
            let peek = crate::automation::guards::allow_peek(browser.identifier());
            let intercept =
                controller::with_store(|s| s.intercept_navigation(tab, &url, user_gesture != 0, is_redirect != 0)).flatten().filter(|_| peek);
            if let Some(disposition) = intercept {
                controller::dispatch(Command::LinkOpenRequested { opener: tab, url, disposition });
                return 1;
            }
            // The next document should see the boosts of its host (same-site renderers only).
            tabs::sync_boosts(browser, &url);
            0
        }

        fn on_open_urlfrom_tab(
            &self,
            browser: Option<&mut Browser>,
            _frame: Option<&mut Frame>,
            target_url: Option<&CefString>,
            target_disposition: WindowOpenDisposition,
            user_gesture: i32,
        ) -> i32 {
            // With Alloy, not cancelling loads the URL in the *current* tab.
            let Some(disposition) = link_disposition(target_disposition) else { return 0 };
            let url = cef_str(target_url);
            if external::is_external(&url) {
                let gesture = browser.as_deref().map_or(user_gesture != 0, |b| crate::automation::guards::external_gesture(b.identifier(), user_gesture != 0));
                external::open(&url, gesture, "link");
                return 1;
            }
            let Some(opener) = browser.and_then(|b| tab_of(b)) else { return 1 };
            // Web content never opens shell pages, not even in a new tab.
            if !scheme::is_sta_url(&url) {
                controller::dispatch(Command::LinkOpenRequested { opener, url, disposition });
            }
            1
        }

        fn on_render_process_terminated(
            &self,
            browser: Option<&mut Browser>,
            status: TerminationStatus,
            error_code: i32,
            _error_string: Option<&CefString>,
        ) {
            let Some(browser) = browser else { return };
            let Some(tab) = tab_of(browser) else { return };
            let raw = cef::sys::cef_termination_status_t::from(status) as i32;
            log_warn!("renderer of tab {tab} terminated (status {raw}, code {error_code})");
            ERROR_PAGES.with(|e| e.borrow_mut().remove(&browser.identifier()));
            reset_media(browser.identifier());
            controller::dispatch(Command::TabCrashed { tab });
        }
    }
}

wrap_download_handler! {
    struct TabDownload;

    impl DownloadHandler {
        fn can_download(&self, browser: Option<&mut Browser>, url: Option<&CefString>, request_method: Option<&CefString>) -> i32 {
            // The Rust default is 0 (cancels every download).
            let id = browser.map(|b| b.identifier()).unwrap_or(0);
            downloads::can_download(id, &cef_str(url), &cef_str(request_method)) as i32
        }

        fn on_before_download(
            &self,
            browser: Option<&mut Browser>,
            download_item: Option<&mut DownloadItem>,
            suggested_name: Option<&CefString>,
            callback: Option<&mut BeforeDownloadCallback>,
        ) -> i32 {
            let (Some(item), Some(callback)) = (download_item, callback) else { return 0 };
            let id = browser.map(|b| b.identifier()).unwrap_or(0);
            downloads::on_before_download(id, item, &cef_str(suggested_name), callback) as i32
        }

        fn on_download_updated(
            &self,
            browser: Option<&mut Browser>,
            download_item: Option<&mut DownloadItem>,
            callback: Option<&mut DownloadItemCallback>,
        ) {
            let Some(item) = download_item else { return };
            let id = browser.map(|b| b.identifier()).unwrap_or(0);
            downloads::on_download_updated(id, item, callback.cloned());
        }
    }
}

wrap_permission_handler! {
    struct TabPermission;

    impl PermissionHandler {
        fn on_request_media_access_permission(
            &self,
            browser: Option<&mut Browser>,
            _frame: Option<&mut Frame>,
            requesting_origin: Option<&CefString>,
            requested_permissions: u32,
            callback: Option<&mut MediaAccessCallback>,
        ) -> i32 {
            let (Some(browser), Some(callback)) = (browser, callback) else { return 0 };
            permissions::on_request_media_access(browser.identifier(), &cef_str(requesting_origin), requested_permissions, callback.clone())
                as i32
        }

        fn on_show_permission_prompt(
            &self,
            browser: Option<&mut Browser>,
            prompt_id: u64,
            requesting_origin: Option<&CefString>,
            requested_permissions: u32,
            callback: Option<&mut PermissionPromptCallback>,
        ) -> i32 {
            let (Some(browser), Some(callback)) = (browser, callback) else { return 0 };
            permissions::on_show_permission_prompt(
                browser.identifier(),
                prompt_id,
                &cef_str(requesting_origin),
                requested_permissions,
                callback.clone(),
            ) as i32
        }

        fn on_dismiss_permission_prompt(&self, _browser: Option<&mut Browser>, prompt_id: u64, _result: PermissionRequestResult) {
            permissions::on_dismiss_permission_prompt(prompt_id);
        }
    }
}

// UI pages never get site permissions: media keeps CEF's default (deny) and prompts are dismissed
// at once (Alloy's default IGNORE would leave the page's promise pending).
wrap_permission_handler! {
    struct UiPermission;

    impl PermissionHandler {
        fn on_show_permission_prompt(
            &self,
            _browser: Option<&mut Browser>,
            _prompt_id: u64,
            requesting_origin: Option<&CefString>,
            _requested_permissions: u32,
            callback: Option<&mut PermissionPromptCallback>,
        ) -> i32 {
            let Some(callback) = callback else { return 0 };
            permissions::dismiss_unhandled_prompt(&cef_str(requesting_origin), callback);
            1
        }
    }
}

wrap_find_handler! {
    struct TabFind;

    impl FindHandler {
        fn on_find_result(
            &self,
            browser: Option<&mut Browser>,
            _identifier: i32,
            count: i32,
            _selection_rect: Option<&Rect>,
            active_match_ordinal: i32,
            final_update: i32,
        ) {
            let Some(tab) = browser.and_then(|b| tab_of(b)) else { return };
            ipc::emit(
                "find.result",
                &serde_json::json!({ "tab": tab, "count": count, "active": active_match_ordinal, "final": final_update != 0 }),
            );
        }
    }
}
