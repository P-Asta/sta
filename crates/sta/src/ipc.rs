//! UI ⇄ shell IPC, browser side [owner: chrome] (ARCHITECTURE §5, docs/PROTOCOL.md §2–§3,
//! docs/research/ipc.md §4.A/§4.E).
//!
//! Responsibility:
//! - the process-wide `BrowserSideRouter` (`cef::wrapper::message_router`, `__staQuery`);
//! - the trust model: only browsers created with the UI client (registered by
//!   `browsers::on_after_created`), only their **main frame**, only while it shows `sta://`;
//! - push events: one persistent `__subscribe` query per page; [`emit`] succeeds all of them;
//! - request routing (PROTOCOL §2). Rules: `dispatch` only enqueues; queries take a short store
//!   borrow and reply inline; anything touching Views/CEF navigation is posted to the UI thread
//!   (the router holds its map lock while it runs this handler — never call router methods here).
//!
//! Requests: `dispatch`, `state.get`, `ui.ready`, `omnibox.query`, `omnibox.suggest` (command bar
//! only; remote search suggestions answered asynchronously by `suggest.rs`), `omnibox.actions`,
//! `archive.list`, `history.list`, `boosts.get`, `theme.colors`, `surface.setSize`,
//! `surface.exited` (the ack of an acknowledged exit, `motion.rs`),
//! `sidebar.setWidth` and `sidebar.hoverLock` (sidebar surface only), `dialog.pickFolder` (one
//! native picker at a time, owned by the main window, answered asynchronously), `app.info`, plus
//! `debug.*` in debug builds (see `debug.rs`).
//!
//! Public API:
//! - `pub fn router_config() -> MessageRouterConfig` (shared with renderer.rs)
//! - `pub fn router() -> &'static Arc<BrowserSideRouter>`
//! - `pub fn install()` — add the handler (UI thread, in `on_context_initialized`)
//! - `pub fn trust_browser(id: i32)`, `pub fn untrust_browser(id: i32)`, `pub fn is_trusted(id: i32) -> bool`
//! - `pub fn emit<T: Serialize>(event: &str, payload: &T)`, `pub fn emit_raw(event: &str, payload_json: &str)`
//! - `pub fn emit_raw_to(browser_id: i32, event: &str, payload_json: &str)`
//! - `pub fn push_state()` — snapshot `UiState` now and emit `state` (use `controller::request_state_push` for coalescing)
//! - `pub fn subscriber_count() -> usize`, `pub fn clear()`

use crate::browsers::{self, Role, Surface};
use crate::{controller, overlays, paths, platform, scheme, sidebar_hover, suggest, task, window};
use sta_core::{Command, Id, OmniboxRequest, Theme, now_ms};
use cef::wrapper::message_router::{
    BrowserSideCallback, BrowserSideHandler, BrowserSideRouter, MessageRouterBrowserSide, MessageRouterConfig,
};
use cef::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::cell::RefCell;
use std::collections::HashSet;
use std::sync::{Arc, Mutex, OnceLock};

pub const JS_QUERY_FUNCTION: &str = "__staQuery";
pub const JS_CANCEL_FUNCTION: &str = "__staQueryCancel";

type Cb = Arc<Mutex<dyn BrowserSideCallback>>;

/// Router configuration; must be identical in the renderer (`renderer.rs`).
pub fn router_config() -> MessageRouterConfig {
    MessageRouterConfig {
        js_query_function: JS_QUERY_FUNCTION.to_string(),
        js_cancel_function: JS_CANCEL_FUNCTION.to_string(),
        ..Default::default()
    }
}

static ROUTER: OnceLock<Arc<BrowserSideRouter>> = OnceLock::new();

/// The browser-side router. Forward UI clients' process messages, `on_before_browse` (allowed
/// navigations only), `on_before_close` and `on_render_process_terminated` to it.
pub fn router() -> &'static Arc<BrowserSideRouter> {
    ROUTER.get_or_init(|| BrowserSideRouter::new(router_config()))
}

/// Registers the request handler. UI thread.
pub fn install() {
    router().add_handler(Arc::new(UiIpc), false);
}

// ----------------------------------------------------------------------------------- trust

thread_local! {
    static TRUSTED: RefCell<HashSet<i32>> = RefCell::new(HashSet::new());
}

pub fn trust_browser(browser_id: i32) {
    TRUSTED.with(|t| t.borrow_mut().insert(browser_id));
}

pub fn untrust_browser(browser_id: i32) {
    TRUSTED.with(|t| t.borrow_mut().remove(&browser_id));
}

pub fn is_trusted(browser_id: i32) -> bool {
    TRUSTED.with(|t| t.borrow().contains(&browser_id))
}

fn trusted_frame(browser: &Browser, frame: &Frame) -> bool {
    is_trusted(browser.identifier())
        && frame.is_main() != 0
        && scheme::is_sta_url(&CefString::from(&frame.url()).to_string())
}

// ----------------------------------------------------------------------------------- events

struct Subscriber {
    query_id: i64,
    #[allow(dead_code)] // read by emit_raw_to (stage-2 API)
    browser_id: i32,
    cb: Cb,
}

static SUBSCRIBERS: Mutex<Vec<Subscriber>> = Mutex::new(Vec::new());

fn event_message(event: &str, payload_json: &str) -> String {
    let name = serde_json::to_string(event).unwrap_or_else(|_| "\"\"".into());
    format!("{{\"event\":{name},\"payload\":{payload_json}}}")
}

/// Sends `{event, payload}` to every subscribed page. Callable from any browser-process thread.
pub fn emit_raw(event: &str, payload_json: &str) {
    let msg = event_message(event, payload_json);
    let Ok(subs) = SUBSCRIBERS.lock() else { return };
    for s in subs.iter() {
        if let Ok(cb) = s.cb.lock() {
            cb.success_str(&msg); // posts a UI task; never blocks on router locks
        }
    }
}

/// Like [`emit_raw`] but only to the pages of one browser.
pub fn emit_raw_to(browser_id: i32, event: &str, payload_json: &str) {
    let msg = event_message(event, payload_json);
    let Ok(subs) = SUBSCRIBERS.lock() else { return };
    for s in subs.iter().filter(|s| s.browser_id == browser_id) {
        if let Ok(cb) = s.cb.lock() {
            cb.success_str(&msg);
        }
    }
}

/// Whether a page of `browser_id` is subscribed to events, i.e. whether [`emit_raw_to`] would reach
/// anyone. The acknowledged exits of `motion.rs` ask before they wait for an answer: a surface whose
/// page is gone can never send one.
pub fn has_subscriber(browser_id: i32) -> bool {
    SUBSCRIBERS.lock().is_ok_and(|subs| subs.iter().any(|s| s.browser_id == browser_id))
}

/// Serializes `payload` and emits it.
pub fn emit<T: Serialize + ?Sized>(event: &str, payload: &T) {
    match serde_json::to_string(payload) {
        Ok(json) => emit_raw(event, &json),
        Err(e) => log_error!("emit {event}: serialize failed: {e}"),
    }
}

/// Pushes a fresh `UiState` snapshot to every page now (no coalescing).
pub fn push_state() {
    if let Some(Ok(json)) = controller::with_store(|s| serde_json::to_string(&s.ui_state())) {
        emit_raw("state", &json);
    }
}

#[cfg_attr(not(debug_assertions), allow(dead_code))] // used by debug.rs only
pub fn subscriber_count() -> usize {
    SUBSCRIBERS.lock().map(|s| s.len()).unwrap_or(0)
}

/// Drops all subscriber callbacks (before `cef::shutdown()`).
pub fn clear() {
    let taken = SUBSCRIBERS.lock().map(|mut s| std::mem::take(&mut *s)).unwrap_or_default();
    drop(taken);
    TRUSTED.with(|t| t.borrow_mut().clear());
}

// ----------------------------------------------------------------------------------- requests

#[derive(Deserialize)]
struct Invoke {
    cmd: String,
    #[serde(default)]
    payload: Value,
}

/// Result of a request handler.
pub enum Reply {
    /// JSON text to send back (`"null"` for no value).
    Json(String),
    /// `(code, message)` → rejected promise in JS.
    Err(i32, String),
    /// The handler keeps the callback and answers later (from any thread).
    Deferred,
}

impl Reply {
    pub fn null() -> Reply {
        Reply::Json("null".into())
    }

    pub fn value<T: Serialize>(v: &T) -> Reply {
        match serde_json::to_string(v) {
            Ok(s) => Reply::Json(s),
            Err(e) => Reply::Err(500, format!("serialize: {e}")),
        }
    }

    pub fn bad_request(e: impl std::fmt::Display) -> Reply {
        Reply::Err(400, e.to_string())
    }
}

/// Parses a request payload (`null` is treated as `{}` so all-default structs work).
pub fn parse<T: for<'de> Deserialize<'de>>(payload: Value) -> Result<T, Reply> {
    let payload = if payload.is_null() { Value::Object(Default::default()) } else { payload };
    serde_json::from_value(payload).map_err(Reply::bad_request)
}

fn store_reply<T: Serialize>(f: impl FnOnce(&sta_core::Store) -> T) -> Reply {
    match controller::with_store(f) {
        Some(v) => Reply::value(&v),
        None => Reply::Err(503, "store not ready".into()),
    }
}

struct UiIpc;

impl BrowserSideHandler for UiIpc {
    fn on_query_str(
        &self,
        browser: Option<Browser>,
        frame: Option<Frame>,
        query_id: i64,
        request: &str,
        persistent: bool,
        callback: Cb,
    ) -> bool {
        // UI thread, router lock held: no router calls, no synchronous Views/navigation work.
        let (Some(browser), Some(frame)) = (browser, frame) else {
            fail(&callback, 403, "no frame");
            return true;
        };
        if !trusted_frame(&browser, &frame) {
            log_warn!("IPC rejected from untrusted browser {}", browser.identifier());
            fail(&callback, 403, "forbidden");
            return true;
        }
        let req: Invoke = match serde_json::from_str(request) {
            Ok(r) => r,
            Err(e) => {
                fail(&callback, 400, &format!("malformed request: {e}"));
                return true;
            }
        };
        let browser_id = browser.identifier();
        if req.cmd == "__subscribe" {
            if !persistent {
                fail(&callback, 400, "__subscribe must be persistent");
            } else if let Ok(mut subs) = SUBSCRIBERS.lock() {
                subs.push(Subscriber { query_id, browser_id, cb: callback });
            }
            // Intentionally never completed; canceled on navigation/close/crash.
            return true;
        }
        if persistent {
            fail(&callback, 400, "requests must not be persistent");
            return true;
        }
        match handle_request(browser_id, &req.cmd, req.payload, &callback) {
            Reply::Json(s) => {
                if let Ok(cb) = callback.lock() {
                    cb.success_str(&s);
                }
            }
            Reply::Err(code, msg) => fail(&callback, code, &msg),
            Reply::Deferred => {}
        }
        true
    }

    fn on_query_canceled(&self, _browser: Option<Browser>, _frame: Option<Frame>, query_id: i64) {
        // Idempotent (ipc.md gotcha A.5). Drop the callback outside the lock.
        let removed: Vec<Subscriber> = match SUBSCRIBERS.lock() {
            Ok(mut subs) => {
                let (gone, keep): (Vec<_>, Vec<_>) = std::mem::take(&mut *subs).into_iter().partition(|s| s.query_id == query_id);
                *subs = keep;
                gone
            }
            Err(_) => Vec::new(),
        };
        drop(removed);
    }
}

fn fail(cb: &Cb, code: i32, msg: &str) {
    if let Ok(cb) = cb.lock() {
        cb.failure(code, msg);
    }
}

fn handle_request(browser_id: i32, cmd: &str, payload: Value, callback: &Cb) -> Reply {
    match cmd {
        "dispatch" => {
            let command: Command = match serde_json::from_value(payload) {
                Ok(c) => c,
                Err(e) => return Reply::bad_request(format!("invalid command: {e}")),
            };
            if !command.allowed_from_ui() {
                return Reply::Err(403, "command not allowed from UI".into());
            }
            // Agent approvals only from the agent overlay or the Settings page
            // (automation::may_answer_prompts).
            if !crate::automation::may_answer_prompts(browser_id, &command) {
                return Reply::Err(403, "agent approvals are not allowed from this page".into());
            }
            controller::dispatch(command);
            Reply::null()
        }
        "state.get" => store_reply(|s| s.ui_state()),
        "ui.ready" => {
            task::post_ui(move || overlays::surface_ready(browser_id));
            Reply::null()
        }
        "omnibox.query" => {
            let req: OmniboxRequest = match parse(payload) {
                Ok(r) => r,
                Err(r) => return r,
            };
            store_reply(|s| s.omnibox(&req, now_ms()))
        }
        "omnibox.suggest" => {
            if browsers::role_of(browser_id) != Some(Role::Surface(Surface::CommandBar)) {
                return Reply::Err(403, "omnibox.suggest is only allowed from the command bar".into());
            }
            #[derive(Deserialize)]
            struct Q {
                text: String,
            }
            let q: Q = match parse(payload) {
                Ok(q) => q,
                Err(r) => return r,
            };
            let cb = callback.clone();
            // Starting (and cancelling) URL requests stays out of the router lock.
            task::post_ui(move || suggest::start(browser_id, q.text, cb));
            Reply::Deferred
        }
        "omnibox.actions" => store_reply(|s| s.omnibox_actions()),
        "archive.list" => store_reply(|s| s.archive_list()),
        "history.list" => {
            #[derive(Deserialize)]
            struct Q {
                #[serde(default)]
                query: String,
                #[serde(default)]
                limit: Option<usize>,
            }
            let q: Q = match parse(payload) {
                Ok(q) => q,
                Err(r) => return r,
            };
            store_reply(|s| s.history_list(&q.query, q.limit.unwrap_or(200).min(10_000), now_ms()))
        }
        "boosts.get" => {
            #[derive(Deserialize)]
            struct Q {
                id: Id,
            }
            let q: Q = match parse(payload) {
                Ok(q) => q,
                Err(r) => return r,
            };
            store_reply(|s| s.boost(q.id))
        }
        "theme.colors" => {
            #[derive(Deserialize)]
            struct Q {
                theme: Theme,
            }
            let q: Q = match parse(payload) {
                Ok(q) => q,
                Err(r) => return r,
            };
            store_reply(|s| s.theme_colors(&q.theme))
        }
        "surface.setSize" => {
            #[derive(Deserialize)]
            struct Q {
                #[serde(default)]
                width: Option<f64>,
                height: f64,
            }
            let q: Q = match parse(payload) {
                Ok(q) => q,
                Err(r) => return r,
            };
            let (w, h) = (q.width.map(|w| w.round() as i32), q.height.round() as i32);
            task::post_ui(move || overlays::set_surface_size(browser_id, w, h));
            Reply::null()
        }
        // The other half of an acknowledged exit (PROTOCOL §14): this surface has rendered the blank
        // frame the shell asked for with `surface.exit {gen}` (the sidebar: `sidebar.hover {gen}`),
        // so the widget may be hidden now. `gen` identifies the exit; one that is no longer pending
        // (cancelled by a show, or already finished at the cap) is counted and ignored.
        "surface.exited" => {
            #[derive(Deserialize)]
            struct Q {
                // `gen` is a reserved word in Rust 2024; the wire name stays `gen`.
                #[serde(rename = "gen")]
                generation: u64,
            }
            let q: Q = match parse(payload) {
                Ok(q) => q,
                Err(r) => return r,
            };
            task::post_ui(move || crate::motion::on_exited(browser_id, q.generation));
            Reply::null()
        }
        "sidebar.setWidth" => {
            if browsers::role_of(browser_id) != Some(Role::Surface(Surface::Sidebar)) {
                return Reply::Err(403, "sidebar.setWidth is only allowed from the sidebar".into());
            }
            #[derive(Deserialize)]
            struct Q {
                width: f64,
            }
            let q: Q = match parse(payload) {
                Ok(q) => q,
                Err(r) => return r,
            };
            let width = q.width.round().clamp(0.0, 10_000.0) as u32;
            task::post_ui(move || window::set_sidebar_width_live(width));
            Reply::null()
        }
        "sidebar.hoverLock" => {
            if browsers::role_of(browser_id) != Some(Role::Surface(Surface::Sidebar)) {
                return Reply::Err(403, "sidebar.hoverLock is only allowed from the sidebar".into());
            }
            #[derive(Deserialize)]
            struct Q {
                locked: bool,
            }
            let q: Q = match parse(payload) {
                Ok(q) => q,
                Err(r) => return r,
            };
            task::post_ui(move || sidebar_hover::set_locked(q.locked));
            Reply::null()
        }
        "dialog.pickFolder" => {
            let cb = callback.clone();
            // The modal dialog runs a nested message loop: never inside the router lock.
            task::post_ui(move || pick_folder(cb));
            Reply::Deferred
        }
        "app.info" => Reply::value(&app_info()),
        c if c.starts_with("agent.") => crate::automation::ui::handle_request(browser_id, c, payload, callback),
        #[cfg(debug_assertions)]
        c if c.starts_with("debug.") => crate::debug::handle(browser_id, c, payload, callback),
        _ => Reply::Err(404, format!("unknown request: {cmd}")),
    }
}

thread_local! {
    static PICKING_FOLDER: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// `dialog.pickFolder` (UI thread task): one native folder picker at a time, modal to the main
/// window, starting in the current download folder. Replies `string | null`; 409 while another
/// picker is open.
///
/// Like Chromium's own file dialogs, the `IFileOpenDialog` runs on a dedicated STA thread (with
/// our HWND as owner, which disables the window while it is open): a modal loop on the UI thread
/// would stall every CEF task until it closes. The result is posted back to the UI thread.
fn pick_folder(cb: Cb) {
    if PICKING_FOLDER.replace(true) {
        fail(&cb, 409, "a folder picker is already open");
        return;
    }
    let (owner, initial) = (window::hwnd_value(), download_dir());
    let reply = cb.clone();
    let spawned = std::thread::Builder::new().name("sta-pick-folder".into()).spawn(move || {
        let path = platform::pick_folder(owner, "Choose a download folder", initial.as_deref());
        task::post_ui_from_any_thread(move || {
            PICKING_FOLDER.set(false);
            let json = serde_json::to_string(&path).unwrap_or_else(|_| "null".into());
            if let Ok(cb) = reply.lock() {
                cb.success_str(&json);
            }
        });
    });
    if let Err(e) = spawned {
        PICKING_FOLDER.set(false);
        fail(&cb, 500, &format!("cannot open the folder picker: {e}"));
    }
}

/// Configured download folder, else the user's Downloads folder.
fn download_dir() -> Option<String> {
    let configured = controller::with_store(|s| s.settings().download_dir.clone()).flatten();
    configured
        .filter(|d| !d.is_empty())
        .or_else(|| platform::downloads_dir().map(|p| p.to_string_lossy().into_owned()))
}

fn app_info() -> Value {
    let cef_version = std::str::from_utf8(cef::sys::CEF_VERSION)
        .unwrap_or_default()
        .trim_end_matches('\0')
        .to_string();
    let chromium_version = format!(
        "{}.{}.{}.{}",
        cef::sys::CHROME_VERSION_MAJOR,
        cef::sys::CHROME_VERSION_MINOR,
        cef::sys::CHROME_VERSION_BUILD,
        cef::sys::CHROME_VERSION_PATCH
    );
    let data_dir = paths::try_dirs().map(|d| d.base.to_string_lossy().into_owned());
    let download_dir = download_dir();
    serde_json::json!({
        "version": env!("CARGO_PKG_VERSION"),
        "cefVersion": cef_version,
        "chromiumVersion": chromium_version,
        "dataDir": data_dir,
        "downloadDir": download_dir,
    })
}
