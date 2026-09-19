//! Turning extensions on and off, and removing them, through Chromium's own page [owner: tabs]
//! (ext design FINAL PLAN §4 "Backend"; SEC-6, R-SEC-4; user decision D3b).
//!
//! sta cannot change an extension's state itself: `Secure Preferences` is MAC-protected, and
//! `chrome.management` only exists inside a Chromium page that is allowed to call it. Alloy browsers
//! may not load `chrome://` WebUI at all (design report `extensions.md`, run3). So each operation
//! gets **one Chrome-style window on `chrome://extensions/` that is never shown**, runs one fixed
//! script in it, and is closed again (verified: run12b).
//!
//! Everything about that window is locked down, because it is the one place where sta runs script in
//! a privileged page:
//! - one operation at a time, from a **closed enum** ([`ExtensionOp`]); each variant is a fixed
//!   template whose only inputs are a `[a-p]{32}` id and a bool, inserted as `serde_json` literals;
//!   the script itself throws unless `location.href` starts with `chrome://extensions/`;
//! - the client allows exactly one main-frame URL, refuses popups, downloads and file dialogs, and
//!   drops console messages;
//! - the window is cloaked and cannot be activated ([`crate::platform::hidden_windows`]); if it ever
//!   becomes visible, sta re-hides it and **aborts the operation**;
//! - the browser is in the ordinary browser registry, so shutdown waits for it, and it is closed on a
//!   timeout ([`OP_TIMEOUT_MS`]) whatever happens;
//! - an operation Chromium **confirmed** patches the cached listing at once (`extensions::note_state`)
//!   and the profile is re-read afterwards (`extensions::refresh_later`), which is what the user
//!   finally sees: `Secure Preferences` is committed up to ten seconds later, so waiting for it alone
//!   would leave the row showing the old state for that long (gate S7).
//!
//! **Removing** an extension is the one operation the user answers: Chromium always shows its own
//! "Remove …?" dialog for it (`showConfirmDialog: false` is honoured only for an extension removing
//! itself, and `developerPrivate` has no uninstall at all in 152), so that dialog *is* the
//! confirmation — sta asks nothing of its own, the window stays cloaked, and only its dialog is left
//! visible and answerable ([`hw::allow_dialogs`], cleared when the operation ends).
//!
//! Public API:
//! - `pub fn run(id: String, op: ExtensionOp)` — `Effect::ExtensionOp`
//! - `pub fn close_all()` (shutdown), `pub fn clear()` (teardown)
//! - `pub fn debug_snapshot() -> serde_json::Value`

// `wrap_client!` generates a `new` taking one argument per handler field.
#![allow(clippy::too_many_arguments)]

use crate::platform::hidden_windows as hw;
use crate::{browsers, controller, devtools_cdp, extensions, task, window};
use cef::sys::MSG;
use cef::*;
use serde_json::{Value, json};
use sta_core::extensions::{ExtensionDetails, ExtensionOp};
use sta_core::Command;
use std::cell::{Cell, RefCell};
use std::time::Instant;

/// The only URL the backend window may show.
const BACKEND_URL: &str = "chrome://extensions/";
/// An operation is given this long from creating the window to the answer.
const OP_TIMEOUT_MS: i64 = 5000;
/// Removing an extension waits for the user to answer Chromium's own dialog (see the template).
const UNINSTALL_TIMEOUT_MS: i64 = 120_000;
/// The script itself gets the remaining time, at most this.
const EVAL_TIMEOUT_MS: i64 = 4000;
/// How often the window is checked for having become visible.
const GUARD_POLL_MS: i64 = 150;
/// Chromium writes the preferences a moment after the operation; the listing is re-read twice.
const REREAD_MS: [i64; 2] = [300, 1500];

struct Operation {
    id: String,
    op: ExtensionOp,
    window: Option<Window>,
    browser_id: Option<i32>,
    root: isize,
    started: Instant,
    /// The script was sent (so a second load event does not send it again).
    evaluated: bool,
    /// The operation is over (a reply, an error or the timeout); the window is closing.
    finished: bool,
}

#[derive(Default, Clone, Copy)]
struct Stats {
    started: u64,
    ok: u64,
    failed: u64,
    aborted_visible: u64,
    timed_out: u64,
}

thread_local! {
    static CURRENT: RefCell<Option<Operation>> = const { RefCell::new(None) };
    static CLIENT: RefCell<Option<Client>> = const { RefCell::new(None) };
    static STATS: Cell<Stats> = Cell::new(Stats::default());
    static LAST_ERROR: RefCell<Option<String>> = const { RefCell::new(None) };
}

fn stats(f: impl FnOnce(&mut Stats)) {
    let mut s = STATS.get();
    f(&mut s);
    STATS.set(s);
}

// ----------------------------------------------------------------------------------- templates

/// The script for an operation. The id is a JSON literal, never string-pasted, and the page checks
/// its own origin before it touches an API — the same check the browser side makes before each
/// evaluate, because a WebUI page could in principle have been navigated away meanwhile.
///
/// Each script resolves with a JSON **string**: `{"ok":…}` or `{"error":"…"}`. The calls are the ones
/// `chrome://extensions` itself uses (`extensions/service.ts`), verified in run12b.
fn script(id: &str, op: ExtensionOp) -> String {
    let id = Value::String(id.to_string()).to_string();
    let body = match op {
        ExtensionOp::GetInfo => String::from(
            r#"const list = await new Promise((res, rej) => chrome.developerPrivate.getExtensionsInfo({includeDisabled: true, includeTerminated: true}, (l) => chrome.runtime.lastError ? rej(new Error(chrome.runtime.lastError.message)) : res(l)));
  const info = list.find((e) => e.id === id);
  if (!info) throw new Error('not installed');
  const perms = info.permissions || {};
  const warnings = (perms.simplePermissions || []).map((p) => String(p.message || '')).filter(Boolean);
  const hosts = perms.runtimeHostPermissions;
  let hostAccess = '';
  if (hosts) {
    const specific = (hosts.hosts || []).map((h) => String(h.host || '')).filter(Boolean);
    hostAccess = hosts.hasAllHosts ? 'On all sites' : (specific.length ? 'On ' + specific.join(', ') : '');
  }
  const from = String(info.location || '');
  const source = from === 'FROM_STORE' ? 'Chrome Web Store'
    : from === 'UNPACKED' ? 'Loaded from ' + String(info.prettifiedPath || 'a folder')
    : from === 'THIRD_PARTY' ? 'Added by another program, not from the Chrome Web Store'
    : from === 'INSTALLED_BY_DEFAULT' ? 'Installed by default'
    : from;
  return {warnings, hostAccess, source};"#,
        ),
        ExtensionOp::SetEnabled { enabled } => format!(
            r#"await new Promise((res, rej) => chrome.management.setEnabled(id, {enabled}, () => chrome.runtime.lastError ? rej(new Error(chrome.runtime.lastError.message)) : res()));
  return {{done: true}};"#
        ),
        // Chromium's own confirmation, on purpose. `showConfirmDialog: false` is only honoured for an
        // extension uninstalling *itself*, and `developerPrivate` has no uninstall at all in 152
        // (measured: gate S7, the API listing) — so **every** way to remove another extension ends in
        // Chromium's "Remove …?" dialog. sta lets that dialog be the confirmation (deviation 2 in
        // `gates-p3.md`): the backend window stays cloaked, only its dialog is shown, placed over
        // sta's window, and a user who cancels gets no error.
        ExtensionOp::Uninstall => String::from(
            r#"await new Promise((res, rej) => chrome.management.uninstall(id, {showConfirmDialog: true}, () => chrome.runtime.lastError ? rej(new Error(chrome.runtime.lastError.message)) : res()));
  return {done: true};"#,
        ),
    };
    format!(
        r#"(async () => {{
  const id = {id};
  if (!location.href.startsWith('{BACKEND_URL}')) throw new Error('wrong origin');
  if (!/^[a-p]{{32}}$/.test(id)) throw new Error('bad id');
  {body}
}})().then((ok) => JSON.stringify({{ok}}), (e) => JSON.stringify({{error: String((e && e.message) || e)}}))"#
    )
}

// ----------------------------------------------------------------------------------- lifecycle

/// `Effect::ExtensionOp`: run one operation in a fresh hidden window.
pub fn run(id: String, op: ExtensionOp) {
    if !crate::extension_files::is_extension_id(&id) {
        fail(&id, op, "invalid extension id");
        return;
    }
    if window::is_closing() {
        fail(&id, op, "sta is closing");
        return;
    }
    if CURRENT.with(|c| c.borrow().is_some()) {
        fail(&id, op, "another extension operation is running");
        return;
    }
    stats(|s| s.started += 1);
    log_info!("ext_backend: {} {id}", op.label());
    hw::install(window::hwnd_value());
    // The operation is recorded **before** the window exists: `window_create_top_level` calls
    // `on_window_created` synchronously, which creates the BrowserView and with it the browser, so
    // `on_browser_created` (and even `on_load_end`) can arrive before this call returns. Recording it
    // afterwards lost the browser id, and the operation then waited for a page it could not name.
    CURRENT.with(|c| {
        *c.borrow_mut() = Some(Operation {
            id: id.clone(),
            op,
            window: None,
            browser_id: None,
            root: 0,
            started: Instant::now(),
            evaluated: false,
            finished: false,
        })
    });
    let mut delegate = BackendWindowDelegate::new();
    let Some(cef_window) = window_create_top_level(Some(&mut delegate)) else {
        CURRENT.with(|c| *c.borrow_mut() = None);
        fail(&id, op, "could not create the backend window");
        return;
    };
    let root = cef_window.window_handle().0 as isize;
    let known_root = CURRENT.with(|c| {
        let mut current = c.borrow_mut();
        let Some(current) = current.as_mut() else { return 0 };
        current.window = Some(cef_window);
        if current.root == 0 {
            current.root = root;
        }
        current.root
    });
    // Cloak before anything can show it; the CBT hook then refuses its activation too.
    hw::hide_root(known_root);
    task::post_ui_delayed(GUARD_POLL_MS, guard);
    task::post_ui_delayed(op_timeout(op), move || on_timeout(id, op));
}

/// How long an operation may take: removing waits for a person, everything else for Chromium.
fn op_timeout(op: ExtensionOp) -> i64 {
    if op == ExtensionOp::Uninstall { UNINSTALL_TIMEOUT_MS } else { OP_TIMEOUT_MS }
}

/// Chromium's "Remove …?" dialog appeared: center it over sta's window, where the user is looking.
fn on_dialog_shown(hwnd: isize) {
    task::post_ui(move || {
        let main = window::hwnd_value();
        let (Some(dialog), Some(area)) = (hw::window_rect(hwnd), hw::client_rect(main)) else { return };
        let (mut x, mut y) = (area[0] + (area[2] - dialog[2]) / 2, area[1] + (area[3] - dialog[3]) / 2);
        if let Some([wx, wy, ww, wh]) = hw::work_area(main) {
            x = x.clamp(wx, (wx + ww - dialog[2]).max(wx));
            y = y.clamp(wy, (wy + wh - dialog[3]).max(wy));
        }
        if hw::move_window(hwnd, x, y) {
            log_debug!("ext_backend: the remove dialog moved to {x},{y}");
        }
    });
}

/// The visibility guard and the shutdown check, every [`GUARD_POLL_MS`] while an operation runs.
fn guard() {
    let Some((root, id, op, finished)) = CURRENT.with(|c| c.borrow().as_ref().map(|o| (o.root, o.id.clone(), o.op, o.finished))) else {
        return;
    };
    if finished {
        return;
    }
    if hw::is_window(root) && hw::is_visible(root) && !hw::describe(root).cloaked {
        // R-SEC-4: a window that became visible is re-hidden and the operation is abandoned — the
        // user must never be handed a Chrome settings window they did not ask for.
        stats(|s| s.aborted_visible += 1);
        log_warn!("ext_backend: the backend window became visible; re-hiding it and aborting");
        hw::hide_root(root);
        finish(&id, op, Err("the hidden Chromium window became visible".into()));
        return;
    }
    task::post_ui_delayed(GUARD_POLL_MS, guard);
}

fn on_timeout(id: String, op: ExtensionOp) {
    let running = CURRENT.with(|c| c.borrow().as_ref().is_some_and(|o| o.id == id && o.op == op && !o.finished));
    if running {
        stats(|s| s.timed_out += 1);
        finish(&id, op, Err(format!("Chromium did not answer within {} s", op_timeout(op) / 1000)));
    }
}

/// The backend browser was created: remember it and make sure its window stays hidden.
fn on_browser_created(browser: &Browser) {
    let browser_id = browser.identifier();
    // In the ordinary browser registry, not `extra_*`: sta hosts this browser (it is in a BrowserView
    // of a Window sta created), shutdown counts it there, and the DevTools client addresses browsers
    // through that registry — a backend browser outside it could never be sent a message.
    browsers::on_after_created(browser, false);
    let root = browser.host().map(|h| hw::root_of(h.window_handle().0 as isize)).unwrap_or(0);
    CURRENT.with(|c| {
        if let Some(op) = c.borrow_mut().as_mut() {
            op.browser_id = Some(browser_id);
            if root != 0 {
                op.root = root;
            }
        }
    });
    if root != 0 {
        hw::hide_root(root);
    }
    log_debug!("ext_backend: browser {browser_id} created (root {root})");
}

/// `chrome://extensions/` finished loading: run the script once.
fn on_loaded(browser_id: i32, url: &str) {
    if !url.starts_with(BACKEND_URL) {
        return;
    }
    let Some((id, op)) = CURRENT.with(|c| {
        let mut current = c.borrow_mut();
        let op = current.as_mut().filter(|o| o.browser_id == Some(browser_id) && !o.evaluated && !o.finished)?;
        op.evaluated = true;
        Some((op.id.clone(), op.op))
    }) else {
        return;
    };
    // Removing an extension shows Chromium's own confirmation, owned by this window: it is left
    // visible and answerable while the window itself stays cloaked (R-SEC-4 is about the *window*).
    // The exemption is granted **here**, with the script, and not when the window is created: until
    // the page is loaded and asked to uninstall something there is no dialog to wait for, and an
    // exemption that covers the whole 120 s budget is an exemption for windows sta never asked for
    // (SEC-P3-4). `hidden_windows` admits exactly one owned window under it; a second one is cloaked
    // like any other.
    if op == ExtensionOp::Uninstall {
        let root = CURRENT.with(|c| c.borrow().as_ref().map(|o| o.root).unwrap_or(0));
        if root != 0 {
            hw::set_dialog_listener(on_dialog_shown);
            hw::allow_dialogs(root, true);
        }
    }
    let elapsed = CURRENT.with(|c| c.borrow().as_ref().map(|o| o.started.elapsed().as_millis() as i64).unwrap_or(0));
    let timeout = (op_timeout(op) - elapsed).clamp(500, if op == ExtensionOp::Uninstall { UNINSTALL_TIMEOUT_MS } else { EVAL_TIMEOUT_MS });
    let expression = script(&id, op);
    let params = json!({ "expression": expression, "awaitPromise": true, "returnByValue": true, "userGesture": true });
    let (reply_id, reply_op) = (id.clone(), op);
    devtools_cdp::call(browser_id, devtools_cdp::User::Extensions, "Runtime.evaluate", params, timeout, move |result| {
        on_result(reply_id, reply_op, result);
    });
}

/// `Runtime.evaluate`'s answer: `{"result":{"type":"string","value":"{\"ok\":…}"}}`.
fn on_result(id: String, op: ExtensionOp, result: Result<Value, String>) {
    let outcome = result.and_then(|value| {
        if let Some(details) = value.get("exceptionDetails") {
            let text = details.get("text").and_then(Value::as_str).unwrap_or("the script failed");
            return Err(text.to_string());
        }
        let text = value.get("result").and_then(|r| r.get("value")).and_then(Value::as_str).unwrap_or_default();
        let parsed: Value = serde_json::from_str(text).map_err(|_| "Chromium's answer was not readable".to_string())?;
        match parsed.get("error").and_then(Value::as_str) {
            Some(error) => Err(error.to_string()),
            None => Ok(parsed.get("ok").cloned().unwrap_or(Value::Null)),
        }
    });
    finish(&id, op, outcome);
}

/// Ends the operation exactly once: reports it, closes the window, re-reads the listing.
fn finish(id: &str, op: ExtensionOp, outcome: Result<Value, String>) {
    let closing = CURRENT.with(|c| {
        let mut current = c.borrow_mut();
        let current = current.as_mut().filter(|o| o.id == id && o.op == op && !o.finished)?;
        current.finished = true;
        Some((current.window.clone(), current.browser_id, current.root))
    });
    let Some((cef_window, browser_id, root)) = closing else { return };
    match &outcome {
        Ok(value) => {
            stats(|s| s.ok += 1);
            log_info!("ext_backend: {} {id} done", op.label());
            report(id, op, value);
        }
        // A user who answers "Cancel" in Chromium's remove dialog decided something; that is not a
        // failure, and it gets no toast.
        Err(error) if error.to_lowercase().contains("cancel") => {
            log_info!("ext_backend: {} {id} was cancelled", op.label());
            LAST_ERROR.with(|e| *e.borrow_mut() = Some(error.clone()));
            controller::dispatch(Command::ExtensionsChanged { extensions: extensions::list() });
        }
        Err(error) => {
            stats(|s| s.failed += 1);
            log_warn!("ext_backend: {} {id} failed: {error}", op.label());
            LAST_ERROR.with(|e| *e.borrow_mut() = Some(error.clone()));
            controller::dispatch(Command::ExtensionOpFailed { id: id.to_string(), op, message: error.clone() });
        }
    }
    // The window is closed whatever happened; the browser reports its own close.
    if let Some(browser_id) = browser_id
        && let Some(host) = browsers::browser(browser_id).and_then(|b| b.host())
    {
        host.close_browser(1);
    }
    if let Some(cef_window) = cef_window {
        cef_window.close();
    }
    hw::forget_root(root);
    CURRENT.with(|c| *c.borrow_mut() = None);
    // Whatever Chromium said, what the user sees comes from the profile.
    for delay in REREAD_MS {
        extensions::refresh_later(delay);
    }
}

fn report(id: &str, op: ExtensionOp, value: &Value) {
    match op {
        ExtensionOp::GetInfo => {
            let details = ExtensionDetails {
                id: id.to_string(),
                warnings: value
                    .get("warnings")
                    .and_then(Value::as_array)
                    .map(|list| list.iter().filter_map(Value::as_str).map(str::to_string).collect())
                    .unwrap_or_default(),
                host_access: value.get("hostAccess").and_then(Value::as_str).unwrap_or_default().to_string(),
                source: value.get("source").and_then(Value::as_str).unwrap_or_default().to_string(),
            };
            controller::dispatch(Command::ExtensionDetailsLoaded { details });
        }
        // Chromium confirmed the write, so the listing is patched at once and the profile re-reads
        // below only confirm it: `Secure Preferences` is committed up to ten seconds later, and the
        // row must not keep showing the old state until then (gate S7).
        ExtensionOp::SetEnabled { enabled } => extensions::note_state(id, Some(enabled)),
        ExtensionOp::Uninstall => extensions::note_state(id, None),
    }
}

fn fail(id: &str, op: ExtensionOp, why: &str) {
    log_warn!("ext_backend: {} {id} refused: {why}", op.label());
    stats(|s| s.failed += 1);
    LAST_ERROR.with(|e| *e.borrow_mut() = Some(why.to_string()));
    controller::dispatch(Command::ExtensionOpFailed { id: id.to_string(), op, message: why.to_string() });
}

/// A backend browser closed (its own client, or shutdown).
fn on_browser_closed(browser: &Browser) {
    let browser_id = browser.identifier();
    devtools_cdp::on_browser_closed(browser_id);
    // Removes it from the registry and tells the window one more browser is gone.
    browsers::on_before_close(browser);
    let pending = CURRENT.with(|c| c.borrow().as_ref().filter(|o| o.browser_id == Some(browser_id) && !o.finished).map(|o| (o.id.clone(), o.op)));
    if let Some((id, op)) = pending {
        finish(&id, op, Err("the Chromium window closed".into()));
    }
}

/// Shutdown: abandon a running operation and close its window.
pub fn close_all() {
    let pending = CURRENT.with(|c| c.borrow().as_ref().filter(|o| !o.finished).map(|o| (o.id.clone(), o.op)));
    if let Some((id, op)) = pending {
        log_info!("shutdown: abandoning the extension {} of {id}", op.label());
        finish(&id, op, Err("sta is closing".into()));
    }
}

/// Teardown: drop every handle.
pub fn clear() {
    let current = CURRENT.with(|c| c.borrow_mut().take());
    let client = CLIENT.with(|c| c.borrow_mut().take());
    drop((current, client));
}

#[cfg_attr(not(debug_assertions), allow(dead_code))] // debug.rs only
pub fn debug_snapshot() -> Value {
    let s = STATS.get();
    let current = CURRENT.with(|c| {
        c.borrow().as_ref().map(|o| {
            json!({
                "id": o.id,
                "op": format!("{:?}", o.op),
                "browserId": o.browser_id,
                "ageMs": o.started.elapsed().as_millis() as u64,
                "evaluated": o.evaluated,
                "finished": o.finished,
                "window": hw::describe(o.root),
            })
        })
    });
    json!({
        "current": current,
        "stats": { "started": s.started, "ok": s.ok, "failed": s.failed, "abortedVisible": s.aborted_visible, "timedOut": s.timed_out },
        "lastError": LAST_ERROR.with(|e| e.borrow().clone()),
    })
}

// ----------------------------------------------------------------------------------- window

wrap_window_delegate! {
    struct BackendWindowDelegate {}

    impl ViewDelegate {}
    impl PanelDelegate {}

    impl WindowDelegate {
        fn on_window_created(&self, cef_window: Option<&mut Window>) {
            let Some(cef_window) = cef_window else { return };
            // A layout manager, so the view actually gets the window's bounds: a Chrome-style browser
            // with a zero-sized view never commits its navigation, and the operation then times out
            // with nothing in the log (measured while building this).
            cef_window.set_to_fill_layout();
            // Never `show()`: the window exists only to host a Chromium page.
            let mut delegate = BackendViewDelegate::new();
            let mut client = client();
            let settings = BrowserSettings::default();
            let Some(view) = browser_view_create(Some(&mut client), Some(&CefString::from(BACKEND_URL)), Some(&settings), None, None, Some(&mut delegate))
            else {
                log_error!("ext_backend: browser_view_create failed");
                return;
            };
            cef_window.add_child_view(Some(&mut View::from(&view)));
            cef_window.layout();
        }

        fn initial_bounds(&self, _window: Option<&mut Window>) -> Rect {
            // A real size, so the page lays out as Chromium expects; it is never drawn.
            Rect { x: 0, y: 0, width: 1100, height: 760 }
        }

        fn can_resize(&self, _window: Option<&mut Window>) -> i32 {
            0
        }

        fn can_close(&self, _window: Option<&mut Window>) -> i32 {
            1
        }

        /// Chrome-style: a `chrome://` WebUI page cannot load in an Alloy browser (run3).
        fn window_runtime_style(&self) -> RuntimeStyle {
            RuntimeStyle::CHROME
        }
    }
}

wrap_browser_view_delegate! {
    struct BackendViewDelegate {}

    impl ViewDelegate {}

    impl BrowserViewDelegate {
        fn on_browser_created(&self, _browser_view: Option<&mut BrowserView>, browser: Option<&mut Browser>) {
            if let Some(browser) = browser {
                on_browser_created(browser);
            }
        }

        fn browser_runtime_style(&self) -> RuntimeStyle {
            RuntimeStyle::CHROME
        }
    }
}

// ----------------------------------------------------------------------------------- client

fn client() -> Client {
    if let Some(c) = CLIENT.with(|c| c.borrow().clone()) {
        return c;
    }
    let client = BackendClient::new(BackendLifeSpan::new(), BackendRequest::new(), BackendLoad::new(), BackendKeyboard::new(), BackendDisplay::new(), BackendDownload::new());
    CLIENT.with(|c| *c.borrow_mut() = Some(client.clone()));
    client
}

/// The browser of the operation that is running.
fn is_backend_browser(browser_id: i32) -> bool {
    CURRENT.with(|c| c.borrow().as_ref().is_some_and(|o| o.browser_id == Some(browser_id)))
}

wrap_client! {
    struct BackendClient {
        life_span: LifeSpanHandler,
        request: RequestHandler,
        load: LoadHandler,
        keyboard: KeyboardHandler,
        display: DisplayHandler,
        download: DownloadHandler,
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

        fn display_handler(&self) -> Option<DisplayHandler> {
            Some(self.display.clone())
        }

        fn download_handler(&self) -> Option<DownloadHandler> {
            Some(self.download.clone())
        }
    }
}

wrap_life_span_handler! {
    struct BackendLifeSpan;

    impl LifeSpanHandler {
        fn on_before_popup(
            &self,
            _browser: Option<&mut Browser>,
            _frame: Option<&mut Frame>,
            _popup_id: i32,
            _target_url: Option<&CefString>,
            _target_frame_name: Option<&CefString>,
            _target_disposition: WindowOpenDisposition,
            _user_gesture: i32,
            _popup_features: Option<&PopupFeatures>,
            _window_info: Option<&mut WindowInfo>,
            _client: Option<&mut Option<Client>>,
            _settings: Option<&mut BrowserSettings>,
            _extra_info: Option<&mut Option<DictionaryValue>>,
            _no_javascript_access: Option<&mut i32>,
        ) -> i32 {
            1 // nothing opens out of the backend window
        }

        fn do_close(&self, browser: Option<&mut Browser>) -> i32 {
            // A Views-hosted browser must return 1, or closing it would close the whole Window
            // (docs/research/views.md §5); `finish` closes the Window itself.
            browser.map(|b| browsers::on_do_close(b)).unwrap_or(1)
        }

        fn on_before_close(&self, browser: Option<&mut Browser>) {
            if let Some(browser) = browser {
                on_browser_closed(browser);
            }
        }
    }
}

wrap_request_handler! {
    struct BackendRequest;

    impl RequestHandler {
        fn on_before_browse(
            &self,
            browser: Option<&mut Browser>,
            frame: Option<&mut Frame>,
            request: Option<&mut Request>,
            _user_gesture: i32,
            _is_redirect: i32,
        ) -> i32 {
            let (Some(browser), Some(frame)) = (browser, frame) else { return 1 };
            if frame.is_main() == 0 {
                return 0; // the WebUI's own iframes
            }
            let url = request.map(|r| CefString::from(&r.url()).to_string()).unwrap_or_default();
            if url.starts_with(BACKEND_URL) {
                return 0;
            }
            log_warn!("ext_backend: browser {} tried to navigate away; cancelled", browser.identifier());
            1
        }
    }
}

wrap_load_handler! {
    struct BackendLoad;

    impl LoadHandler {
        fn on_load_end(&self, browser: Option<&mut Browser>, frame: Option<&mut Frame>, _http_status_code: i32) {
            let (Some(browser), Some(frame)) = (browser, frame) else { return };
            if frame.is_main() == 0 {
                return;
            }
            let (id, url) = (browser.identifier(), CefString::from(&frame.url()).to_string());
            // Out of the CEF callback: the evaluate goes through the DevTools client.
            task::post_ui(move || on_loaded(id, &url));
        }
    }
}

wrap_keyboard_handler! {
    struct BackendKeyboard;

    impl KeyboardHandler {
        fn on_pre_key_event(
            &self,
            browser: Option<&mut Browser>,
            _event: Option<&KeyEvent>,
            _os_event: Option<&mut MSG>,
            _is_keyboard_shortcut: Option<&mut i32>,
        ) -> i32 {
            // The window cannot be activated, so this should never fire; if it does, it eats the key.
            browser.is_some_and(|b| is_backend_browser(b.identifier())) as i32
        }
    }
}

wrap_display_handler! {
    struct BackendDisplay;

    impl DisplayHandler {
        fn on_console_message(&self, _browser: Option<&mut Browser>, _level: LogSeverity, _message: Option<&CefString>, _source: Option<&CefString>, _line: i32) -> i32 {
            1 // the WebUI's own logging is not sta's
        }
    }
}

wrap_download_handler! {
    struct BackendDownload;

    impl DownloadHandler {
        fn can_download(&self, _browser: Option<&mut Browser>, _url: Option<&CefString>, _request_method: Option<&CefString>) -> i32 {
            0
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The templates are the whole security surface of this module: the id is a JSON literal, the
    /// origin is checked inside the page, and nothing else varies.
    #[test]
    fn templates_quote_their_arguments_and_check_the_origin() {
        let id = "abcdefghijklmnopabcdefghijklmnop";
        for op in [ExtensionOp::GetInfo, ExtensionOp::SetEnabled { enabled: true }, ExtensionOp::SetEnabled { enabled: false }, ExtensionOp::Uninstall] {
            let s = script(id, op);
            assert!(s.contains(&format!(r#"const id = "{id}""#)), "{op:?}: {s}");
            assert!(s.contains("if (!location.href.startsWith('chrome://extensions/')) throw"), "{op:?}");
            assert!(s.contains("/^[a-p]{32}$/.test(id)"), "{op:?}");
            assert!(s.contains("JSON.stringify"), "{op:?}");
        }
        assert!(script(id, ExtensionOp::SetEnabled { enabled: true }).contains("setEnabled(id, true"));
        assert!(script(id, ExtensionOp::SetEnabled { enabled: false }).contains("setEnabled(id, false"));
        // Removing goes through Chromium's own confirmation: `showConfirmDialog: false` is honoured
        // only for an extension removing itself, and `developerPrivate` has no uninstall at all
        // (gate S7). The dialog is what the user answers.
        let uninstall = script(id, ExtensionOp::Uninstall);
        assert!(uninstall.contains("management.uninstall") && uninstall.contains("showConfirmDialog: true"), "{uninstall}");
        // A hostile id would have to survive JSON encoding to break out of the literal.
        let nasty = script(r#"a";chrome.management.uninstall("b"#, ExtensionOp::Uninstall);
        assert!(nasty.contains(r#"const id = "a\";chrome.management.uninstall(\"b""#), "{nasty}");
        // …and the regex in the page refuses it anyway.
        assert!(nasty.contains("/^[a-p]{32}$/.test(id)"));
    }
}
