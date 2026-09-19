//! An extension's action popup, hosted in sta's own card [owner: chrome] (ext design FINAL PLAN §4
//! "Popup card"; SEC-4, UX8, R-SEC-6).
//!
//! Chrome shows an action popup by clicking a toolbar button, which prebuilt CEF cannot do (D1a).
//! What sta *can* do is load the popup page itself — `chrome-extension://<id>/<default_popup>` in an
//! Alloy BrowserView — and put it in an overlay card under a header strip sta draws
//! (`Overlay::ExtensionPopup`). Verified to work for self-contained popups (design report
//! `extensions.md`, run6/run8); popups whose service worker asks for `tabs`/`windows` fail, and the
//! card then **says so** instead of showing an empty rectangle:
//!
//! - the card is shown only once the page reports a size **larger than the clamp minimum** (gate S4),
//!   measured in the page itself and clamped to Chrome's popup limits (25×25 … 800×600); an empty
//!   document measures as exactly the minimum, which is "nothing yet", not "a size arrived";
//! - if no size arrives within [`FAIL_MS`], or the document never grew past the minimum (a popup
//!   whose script threw "No current window" renders nothing), the card appears with the header and
//!   the line "This popup doesn't work in sta yet · Options" (`ExtensionPopupClosed{failed:true}`).
//!   That verdict is taken from a **fresh** measurement made at the deadline, never from the last
//!   timed one: an MV3 popup waiting for a cold service worker paints a second or two in, and the
//!   card must not call a working popup broken (and then throw its page away);
//! - it closes on Esc, on blur, when the card's tab stops being visible, on a permission prompt, and
//!   when the page calls `window.close()` (which is how a popup dismisses itself).
//!
//! The popup page is **untrusted web content** (`Role::ExtensionPopup`, not a tab):
//! - its main frame may only be `chrome-extension://<id>/`; any other navigation is cancelled;
//! - popups it opens go through core's web-content allowlist (`Command::LinkOpenRequested` is a tab
//!   API, so they are offered as `DevToolsLinkRequested`-style checked opens — here
//!   `Command::OpenUrl` after `urls::web_content_may_open`), external schemes through
//!   `external::open` **with the page's own gesture flag**: a popup that navigates itself to
//!   `ms-settings:` on a timer launches nothing, exactly like a tab (`external.rs`);
//! - downloads, file dialogs and permission requests are refused (a file chooser is cancelled, a
//!   permission prompt is answered `DISMISS` so the page's promise settles instead of hanging), and
//!   it never reaches the IPC surface (no `sta_ui` extra info, and `ipc.rs` checks the frame's origin).
//!
//! Public API:
//! - `pub fn open(id: String, url: String, tab: Option<Id>)` — `Effect::OpenExtensionPopup`
//! - `pub fn close()` — `Effect::HideExtensionPopup`
//! - `pub fn on_browser_closed(browser_id: i32)`
//! - `pub fn clear()`, `pub fn debug_snapshot() -> serde_json::Value`

// `wrap_client!` generates a `new` taking one argument per handler field.
#![allow(clippy::too_many_arguments)]

use crate::browsers::{self, Role};
use crate::{controller, devtools_cdp, external, overlays, task, window};
use cef::*;
use serde_json::{Value, json};
use sta_core::{Command, Id, OpenTarget};
use std::cell::{Cell, RefCell};
use std::time::Instant;

/// The popup gets this long to render something before the card says it doesn't work.
const FAIL_MS: i64 = 3000;
/// When the page is measured after it loads. Popups that render asynchronously (React and friends)
/// grow between these, and the card grows with them. The list reaches close to [`FAIL_MS`] on
/// purpose: a popup that paints at 2 s must be on screen at 2 s, not at the deadline.
const MEASURE_AT_MS: [i64; 7] = [0, 150, 400, 900, 1600, 2100, 2600];
/// Chrome's popup limits, mirrored from the overlay host.
const MIN_SIZE: i32 = 25;

/// The script that measures the page the way Chrome sizes a popup: the document's preferred width
/// (`max-content`, so an explicit `body { width: 360px }` wins and an auto-width popup shrinks to
/// its content), then its height at that width. Restores the styles it touched.
const MEASURE_JS: &str = r#"(() => {
  const de = document.documentElement, b = document.body;
  if (!de) return '0x0';
  const prev = de.getAttribute('style');
  de.style.width = 'max-content';
  let w = Math.ceil(Math.max(de.getBoundingClientRect().width, b ? b.getBoundingClientRect().width : 0, b ? b.scrollWidth : 0));
  w = Math.max(25, Math.min(800, w || 0));
  de.style.width = w + 'px';
  const h = Math.ceil(Math.max(de.scrollHeight, b ? b.scrollHeight : 0));
  if (prev === null) de.removeAttribute('style'); else de.setAttribute('style', prev);
  return w + 'x' + Math.max(25, Math.min(600, h || 0));
})()"#;

struct Popup {
    id: String,
    url: String,
    tab: Option<Id>,
    browser_id: Option<i32>,
    opened: Instant,
    /// Largest size the page reported so far (DIP).
    size: Option<(i32, i32)>,
    /// The card is on screen.
    shown: bool,
    /// The honest-failure line is showing.
    failed: bool,
    /// A generation, so a stale measurement or timeout can't touch a later popup.
    generation: u64,
}

#[derive(Default, Clone, Copy)]
struct Stats {
    opened: u64,
    sized: u64,
    failed: u64,
    closed: u64,
}

thread_local! {
    static CURRENT: RefCell<Option<Popup>> = const { RefCell::new(None) };
    static CLIENT: RefCell<Option<Client>> = const { RefCell::new(None) };
    static GENERATION: Cell<u64> = const { Cell::new(0) };
    static STATS: Cell<Stats> = Cell::new(Stats::default());
}

fn stats(f: impl FnOnce(&mut Stats)) {
    let mut s = STATS.get();
    f(&mut s);
    STATS.set(s);
}

/// `Effect::OpenExtensionPopup`: load the popup page and show the card once it has a size.
pub fn open(id: String, url: String, tab: Option<Id>) {
    close();
    if window::is_closing() {
        return;
    }
    let generation = GENERATION.get() + 1;
    GENERATION.set(generation);
    stats(|s| s.opened += 1);
    log_info!("ext_popup: opening the popup of {id}");
    let mut delegate = PopupViewDelegate::new();
    let mut client = client();
    let settings = BrowserSettings { background_color: window::frame_color(), ..Default::default() };
    let Some(view) = browser_view_create(Some(&mut client), Some(&CefString::from(url.as_str())), Some(&settings), None, None, Some(&mut delegate)) else {
        log_error!("ext_popup: browser_view_create failed");
        controller::dispatch(Command::ExtensionPopupClosed { failed: true });
        return;
    };
    CURRENT.with(|c| {
        *c.borrow_mut() = Some(Popup {
            id,
            url,
            tab,
            browser_id: None,
            opened: Instant::now(),
            size: None,
            shown: false,
            failed: false,
            generation,
        })
    });
    // Parenting the view into the (hidden) card creates its browser.
    overlays::adopt_extension_popup(&view, tab);
    let browser_id = view.browser().map(|b| b.identifier());
    CURRENT.with(|c| {
        if let Some(popup) = c.borrow_mut().as_mut() {
            popup.browser_id = browser_id;
        }
    });
    for at in MEASURE_AT_MS {
        task::post_ui_delayed(at, move || measure(generation, false));
    }
    task::post_ui_delayed(FAIL_MS, move || on_deadline(generation));
}

/// `Effect::HideExtensionPopup`, and every internal close path.
pub fn close() {
    let popup = CURRENT.with(|c| c.borrow_mut().take());
    let Some(popup) = popup else { return };
    stats(|s| s.closed += 1);
    GENERATION.set(GENERATION.get() + 1);
    overlays::hide_extension_popup();
    if let Some(browser_id) = popup.browser_id
        && let Some(host) = browsers::browser(browser_id).and_then(|b| b.host())
    {
        host.close_browser(1);
    }
    drop(popup);
}

/// `MEASURE_JS`'s answer, or `None` when the page had nothing to say (no document yet, a dropped
/// call, an unreadable reply). At the deadline, `None` is a failure; before it, it is "not yet".
fn parse_size(value: &Value) -> Option<(i32, i32)> {
    let text = value.get("result").and_then(|r| r.get("value")).and_then(Value::as_str)?;
    let (w, h) = text.split_once('x')?;
    let (w, h) = (w.parse::<i32>().ok()?, h.parse::<i32>().ok()?);
    (w > 0 && h > 0).then_some((w, h))
}

/// A size at or below the clamp minimum in both directions is an **empty** document: `MEASURE_JS`
/// clamps to 25×25, so "no size yet" and "a size arrived" are otherwise indistinguishable.
fn rendered(size: Option<(i32, i32)>) -> bool {
    size.is_some_and(|(w, h)| w > MIN_SIZE || h > MIN_SIZE)
}

/// Measures the page and grows the card; the first real size shows it (S4). `last` marks the
/// measurement taken at [`FAIL_MS`], whose answer decides whether the popup failed.
fn measure(generation: u64, last: bool) {
    let Some((browser_id, current)) = CURRENT.with(|c| c.borrow().as_ref().filter(|p| p.generation == generation).map(|p| (p.browser_id, p.size))) else {
        return;
    };
    let Some(browser_id) = browser_id else {
        if last {
            declare_failed(generation);
        }
        return;
    };
    devtools_cdp::call(browser_id, devtools_cdp::User::Extensions, "Runtime.evaluate", json!({ "expression": MEASURE_JS, "returnByValue": true }), 1500, move |result| {
        let size = result.ok().as_ref().and_then(parse_size);
        let Some((w, h)) = size else {
            // The call is answered exactly once, timeout included: a page that cannot be measured at
            // the deadline is a page with nothing on screen.
            if last {
                declare_failed(generation);
            }
            return;
        };
        // A popup only ever grows while it renders: never take a card away from under the pointer.
        let (w, h) = match current {
            Some((pw, ph)) => (w.max(pw), h.max(ph)),
            None => (w, h),
        };
        on_size(generation, w, h);
        if last && !rendered(Some((w, h))) {
            declare_failed(generation);
        }
    });
}

fn on_size(generation: u64, width: i32, height: i32) {
    let show = CURRENT.with(|c| {
        let mut current = c.borrow_mut();
        let popup = current.as_mut().filter(|p| p.generation == generation)?;
        popup.size = Some((width, height));
        // An empty document measures as the clamp minimum: showing the card on that answer put a
        // 25×25 sliver with a clipped header on screen for as long as the popup stayed blank (UX1).
        let first = !popup.shown && rendered(popup.size);
        popup.shown |= first;
        Some((first, popup.tab))
    });
    let Some((first, tab)) = show else { return };
    overlays::set_extension_popup_size(width, height);
    if first {
        stats(|s| s.sized += 1);
        log_debug!("ext_popup: sized {width}x{height}; showing the card");
        overlays::show_extension_popup(tab);
    }
}

/// [`FAIL_MS`] after opening: nothing on screen yet, so take **one more measurement** and let that
/// decide. A popup whose service worker was cold paints late, and the verdict must be about the page
/// as it is now, not as it was 1.4 s ago.
fn on_deadline(generation: u64) {
    let state = CURRENT.with(|c| c.borrow().as_ref().filter(|p| p.generation == generation).map(|p| p.size));
    let Some(size) = state else { return };
    if rendered(size) {
        return;
    }
    measure(generation, true);
}

/// The honest-failure line, for a popup that really has nothing on screen (UX1).
fn declare_failed(generation: u64) {
    let state = CURRENT.with(|c| c.borrow().as_ref().filter(|p| p.generation == generation && !p.failed).map(|p| (p.size, p.tab, p.id.clone())));
    let Some((size, tab, id)) = state else { return };
    if rendered(size) {
        return;
    }
    stats(|s| s.failed += 1);
    log_warn!("ext_popup: the popup of {id} did not render within {FAIL_MS} ms");
    // Marked before the page is taken out of the card: dropping its view closes its browser, and
    // `on_browser_closed` must not read that as the popup dismissing itself.
    CURRENT.with(|c| {
        if let Some(popup) = c.borrow_mut().as_mut() {
            popup.failed = true;
            popup.shown = true;
        }
    });
    // Core marks the card failed; its header page shows the line, and the card appears with just
    // that (no page height).
    controller::dispatch(Command::ExtensionPopupClosed { failed: true });
    overlays::extension_popup_failed();
    overlays::show_extension_popup(tab);
}

/// The popup's browser is gone: either sta closed it, or the page called `window.close()`.
pub fn on_browser_closed(browser_id: i32) {
    let ours = CURRENT.with(|c| c.borrow().as_ref().is_some_and(|p| p.browser_id == Some(browser_id)));
    if !ours {
        return;
    }
    // A card that already said the popup does not work took the (blank) page out itself: its browser
    // closing is sta's own doing, not the page dismissing itself, and the card stays up.
    let failed = CURRENT.with(|c| {
        let mut current = c.borrow_mut();
        let Some(popup) = current.as_mut() else { return false };
        if popup.failed {
            popup.browser_id = None;
        }
        popup.failed
    });
    if failed {
        return;
    }
    log_debug!("ext_popup: the popup page closed itself");
    CURRENT.with(|c| *c.borrow_mut() = None);
    overlays::hide_extension_popup();
    controller::dispatch(Command::ExtensionPopupClosed { failed: false });
}

pub fn clear() {
    let current = CURRENT.with(|c| c.borrow_mut().take());
    let client = CLIENT.with(|c| c.borrow_mut().take());
    drop((current, client));
}

#[cfg_attr(not(debug_assertions), allow(dead_code))] // debug.rs only
pub fn debug_snapshot() -> Value {
    let s = STATS.get();
    let current = CURRENT.with(|c| {
        c.borrow().as_ref().map(|p| {
            json!({
                "id": p.id,
                "url": p.url,
                "tab": p.tab,
                "browserId": p.browser_id,
                "ageMs": p.opened.elapsed().as_millis() as u64,
                "size": p.size.map(|(w, h)| [w, h]),
                "shown": p.shown,
                "failed": p.failed,
            })
        })
    });
    json!({
        "current": current,
        "stats": { "opened": s.opened, "sized": s.sized, "failed": s.failed, "closed": s.closed },
    })
}

// ----------------------------------------------------------------------------------- client

fn client() -> Client {
    if let Some(c) = CLIENT.with(|c| c.borrow().clone()) {
        return c;
    }
    let client = PopupClient::new(
        PopupLifeSpan::new(),
        PopupRequest::new(),
        PopupLoad::new(),
        PopupKeyboard::new(),
        PopupDownload::new(),
        PopupDialog::new(),
        PopupPermission::new(),
        PopupFocus::new(),
    );
    CLIENT.with(|c| *c.borrow_mut() = Some(client.clone()));
    client
}

/// The origin the card is locked to: `chrome-extension://<id>/`.
fn own_origin() -> Option<String> {
    CURRENT.with(|c| c.borrow().as_ref().map(|p| format!("chrome-extension://{}/", p.id)))
}

wrap_browser_view_delegate! {
    struct PopupViewDelegate {}

    impl ViewDelegate {
        fn preferred_size(&self, _view: Option<&mut View>) -> Size {
            // The card's box layout gives it the rest of the card; this only has to be non-zero.
            Size { width: MIN_SIZE, height: MIN_SIZE }
        }
    }

    impl BrowserViewDelegate {
        fn on_browser_created(&self, _browser_view: Option<&mut BrowserView>, browser: Option<&mut Browser>) {
            if let Some(browser) = browser {
                browsers::on_after_created(browser, false);
                browsers::set_role(browser.identifier(), Role::ExtensionPopup);
            }
        }

        fn browser_runtime_style(&self) -> RuntimeStyle {
            RuntimeStyle::ALLOY
        }
    }
}

wrap_client! {
    struct PopupClient {
        life_span: LifeSpanHandler,
        request: RequestHandler,
        load: LoadHandler,
        keyboard: KeyboardHandler,
        download: DownloadHandler,
        dialog: DialogHandler,
        permission: PermissionHandler,
        focus: FocusHandler,
    }

    impl Client {
        fn life_span_handler(&self) -> Option<LifeSpanHandler> {
            Some(self.life_span.clone())
        }

        fn request_handler(&self) -> Option<RequestHandler> {
            Some(self.request.clone())
        }

        fn load_handler(&self) -> Option<LoadHandler> {
            Some(self.load.clone())
        }

        fn keyboard_handler(&self) -> Option<KeyboardHandler> {
            Some(self.keyboard.clone())
        }

        fn download_handler(&self) -> Option<DownloadHandler> {
            Some(self.download.clone())
        }

        fn dialog_handler(&self) -> Option<DialogHandler> {
            Some(self.dialog.clone())
        }

        fn permission_handler(&self) -> Option<PermissionHandler> {
            Some(self.permission.clone())
        }

        fn focus_handler(&self) -> Option<FocusHandler> {
            Some(self.focus.clone())
        }
    }
}

wrap_life_span_handler! {
    struct PopupLifeSpan;

    impl LifeSpanHandler {
        fn on_before_popup(
            &self,
            browser: Option<&mut Browser>,
            _frame: Option<&mut Frame>,
            _popup_id: i32,
            target_url: Option<&CefString>,
            _target_frame_name: Option<&CefString>,
            _target_disposition: WindowOpenDisposition,
            user_gesture: i32,
            _popup_features: Option<&PopupFeatures>,
            _window_info: Option<&mut WindowInfo>,
            _client: Option<&mut Option<Client>>,
            _settings: Option<&mut BrowserSettings>,
            _extra_info: Option<&mut Option<DictionaryValue>>,
            _no_javascript_access: Option<&mut i32>,
        ) -> i32 {
            // A popup's own "open in a tab" links become sta tabs, judged like any link web content
            // offers; nothing opens as a Chromium window (R-SEC-6).
            let url = target_url.map(CefString::to_string).unwrap_or_default();
            let gesture = browser.map(|b| gesture_of(b.identifier(), user_gesture)).unwrap_or(false);
            task::post_ui(move || offer_link(url, gesture));
            1
        }

        fn do_close(&self, _browser: Option<&mut Browser>) -> i32 {
            0
        }

        fn on_before_close(&self, browser: Option<&mut Browser>) {
            if let Some(browser) = browser {
                browsers::on_before_close(browser);
            }
        }
    }
}

/// The gesture an external launch from the card gets: the page's own, through the same guard every
/// other client uses. A card is not a tab, so the agent guard is a no-op here — it stays in the path
/// so this can never become the one surface that skips it.
fn gesture_of(browser_id: i32, user_gesture: i32) -> bool {
    crate::automation::guards::external_gesture(browser_id, user_gesture != 0)
}

/// A link the popup wants to open elsewhere: external schemes go to the OS **only with a real user
/// gesture** (`external.rs`: the popup page is third-party code in an overlay with no address bar),
/// web URLs become tabs, anything else is dropped. The popup card closes either way, like Chrome's
/// popup does.
fn offer_link(url: String, user_gesture: bool) {
    let url = url.trim().to_string();
    if url.is_empty() {
        return;
    }
    if sta_core::urls::is_external_scheme(&url) {
        external::open(&url, user_gesture, "extension popup");
    } else if sta_core::urls::web_content_may_open(&url, None) {
        controller::dispatch(Command::OpenUrl { url, target: OpenTarget::NewTab, opener: None });
    } else {
        log_warn!("ext_popup: refused to open a {} link", sta_core::urls::scheme(&url).unwrap_or_default());
    }
    controller::dispatch(Command::CloseExtensionPopup);
}

wrap_request_handler! {
    struct PopupRequest;

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
            if frame.is_main() == 0 {
                return 0; // the popup's own iframes
            }
            let url = request.map(|r| CefString::from(&r.url()).to_string()).unwrap_or_default();
            let Some(origin) = own_origin() else { return 1 };
            if url.starts_with(&origin) {
                return 0;
            }
            let id = browser.identifier();
            log_warn!("ext_popup: browser {id} tried to leave its own origin; cancelled");
            let url_for_link = url.clone();
            let gesture = gesture_of(id, user_gesture);
            task::post_ui(move || offer_link(url_for_link, gesture));
            1
        }
    }
}

wrap_load_handler! {
    struct PopupLoad;

    impl LoadHandler {
        fn on_load_end(&self, browser: Option<&mut Browser>, frame: Option<&mut Frame>, _http_status_code: i32) {
            let (Some(browser), Some(frame)) = (browser, frame) else { return };
            if frame.is_main() == 0 {
                return;
            }
            let ours = CURRENT.with(|c| c.borrow().as_ref().filter(|p| p.browser_id == Some(browser.identifier())).map(|p| p.generation));
            // The first measurement the page can actually answer (the timed ones cover async renders).
            if let Some(generation) = ours {
                task::post_ui(move || measure(generation, false));
            }
        }
    }
}

wrap_keyboard_handler! {
    struct PopupKeyboard;

    impl KeyboardHandler {
        fn on_pre_key_event(
            &self,
            browser: Option<&mut Browser>,
            event: Option<&KeyEvent>,
            _os_event: crate::platform::OsEvent<'_>,
            _is_keyboard_shortcut: Option<&mut i32>,
        ) -> i32 {
            let (Some(browser), Some(event)) = (browser, event) else { return 0 };
            crate::keyboard::on_pre_key_event(browser.identifier(), event) as i32
        }
    }
}

wrap_download_handler! {
    struct PopupDownload;

    impl DownloadHandler {
        fn can_download(&self, _browser: Option<&mut Browser>, _url: Option<&CefString>, _request_method: Option<&CefString>) -> i32 {
            0 // a popup card is not a place to start a download
        }

        fn on_before_download(
            &self,
            _browser: Option<&mut Browser>,
            _download_item: Option<&mut DownloadItem>,
            _suggested_name: Option<&CefString>,
            _callback: Option<&mut BeforeDownloadCallback>,
        ) -> i32 {
            0
        }
    }
}

// A file chooser is cancelled: the card has no address bar, so a native "Open" dialog over it would
// name no origin at all — and CEF's default is to show one (FINAL PLAN §4 "Popup card").
wrap_dialog_handler! {
    struct PopupDialog;

    impl DialogHandler {
        fn on_file_dialog(
            &self,
            _browser: Option<&mut Browser>,
            _mode: FileDialogMode,
            _title: Option<&CefString>,
            _default_file_path: Option<&CefString>,
            _accept_filters: Option<&mut CefStringList>,
            _accept_extensions: Option<&mut CefStringList>,
            _accept_descriptions: Option<&mut CefStringList>,
            callback: Option<&mut FileDialogCallback>,
        ) -> i32 {
            let Some(callback) = callback else { return 1 };
            log_info!("ext_popup: a file chooser was cancelled");
            callback.cancel();
            1
        }
    }
}

// Site permissions are **refused**, not ignored: Alloy's default (IGNORE) leaves the page's promise
// pending forever, and a popup awaiting it would then be reported as broken for the wrong reason.
wrap_permission_handler! {
    struct PopupPermission;

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
            let origin = requesting_origin.map(CefString::to_string).unwrap_or_default();
            log_info!("ext_popup: a permission request was refused");
            crate::permissions::dismiss_unhandled_prompt(&origin, callback);
            1
        }
    }
}

wrap_focus_handler! {
    struct PopupFocus;

    impl FocusHandler {
        fn on_got_focus(&self, browser: Option<&mut Browser>) {
            if let Some(browser) = browser {
                let id = browser.identifier();
                task::post_ui(move || overlays::on_browser_got_focus(id));
            }
        }
    }
}
