//! Remote search suggestions for the command bar [owner: chrome] (ARCHITECTURE §5.1,
//! docs/PROTOCOL.md §2 `omnibox.suggest`, §6).
//!
//! Responsibility: `omnibox.suggest {text}` → `{text, suggestions}`. The IPC handler checks the
//! caller (command bar only) and posts [`start`]; the answer comes later from the network.
//!
//! - **When**: only with `Settings::search_suggestions` on, an engine that has a suggestion
//!   endpoint (`omnibox::suggest_url`: Google, Bing, DuckDuckGo, Brave, Ecosia), and text that is
//!   1–256 chars, not blank, and not URL-like with an explicit scheme (`http:`, `mailto:`, any
//!   `scheme://`) or a Windows path. A leading `?` (forced search) is stripped and then any text is
//!   fetched. Everything else, and every failure (network error, non-200 status, body over 64 KB,
//!   no answer within 1500 ms, a newer request), resolves `{text, suggestions: []}`: the promise
//!   never rejects for network problems.
//! - **Newest request only**: one slot per calling browser. A new request cancels the one in
//!   flight, which resolves with `[]` at once.
//! - **Network**: a CEF URL request (`urlrequest_create`) on the global request context, so the
//!   Chromium network stack (system proxy settings, TLS) is used. `GET` without
//!   `UR_FLAG_ALLOW_STORED_CREDENTIALS`: no cookies or HTTP auth are sent, and cookies a response
//!   sets are not saved (Bing's endpoint sets tracking cookies; shell-e2e checks both).
//!   `UR_FLAG_DISABLE_CACHE` keeps typed text out of the HTTP cache; `UR_FLAG_NO_RETRY_ON_5XX`.
//!   Typed text is never logged.
//! - **Threading**: everything runs on the UI thread. The request is created there, so CEF calls
//!   the client there too (`cef_urlrequest.h`). No `RefCell` borrow is held across a CEF call (a
//!   cancel may complete the request synchronously); cancelling and dropping a request handle is
//!   posted.
//! - Debug builds: `STA_SUGGEST_URL` replaces the endpoint (a template with `{q}`, the
//!   percent-encoded query) for engines that have one, so tests can use a local server.
//!
//! Public API:
//! - `pub fn start(browser_id: i32, text: String, callback: Cb)` — UI thread, outside the router lock
//! - `pub fn fetch_query(text: &str) -> Option<String>` — the text sent to the engine, if any
//! - `pub fn clear()` — drop every pending request before `cef::shutdown()`
//! - `pub fn debug_snapshot() -> serde_json::Value` (debug builds)

use crate::{controller, platform, task};
use sta_core::omnibox::{parse_suggestions, suggest_url};
use cef::wrapper::message_router::BrowserSideCallback;
use cef::*;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

type Cb = Arc<Mutex<dyn BrowserSideCallback>>;

/// A request without an answer after this long resolves with no suggestions.
pub const TIMEOUT_MS: i64 = 1500;
/// Largest accepted response body.
pub const MAX_BODY_BYTES: usize = 64 * 1024;
/// Longer input is never sent.
pub const MAX_TEXT_CHARS: usize = 256;

struct Pending {
    id: u64,
    /// The text as the page sent it (echoed in the reply).
    text: String,
    /// The text sent to the engine (`?` stripped).
    query: String,
    callback: Cb,
    body: Vec<u8>,
    request: Option<Urlrequest>,
}

#[derive(Default)]
struct Stats {
    started: Cell<u64>,
    succeeded: Cell<u64>,
    failed: Cell<u64>,
    superseded: Cell<u64>,
    timed_out: Cell<u64>,
    skipped: Cell<u64>,
}

thread_local! {
    /// In-flight request per calling browser.
    static PENDING: RefCell<HashMap<i32, Pending>> = RefCell::new(HashMap::new());
    static NEXT_ID: Cell<u64> = const { Cell::new(1) };
    static STATS: Stats = Stats::default();
}

fn bump(counter: impl Fn(&Stats) -> &Cell<u64>) {
    STATS.with(|s| counter(s).set(counter(s).get() + 1));
}

/// Why a request ended without suggestions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Failure {
    Network,
    Status(i32),
    TooLarge,
    TimedOut,
    Superseded,
}

/// `omnibox.suggest` (UI thread task, outside the router lock). Supersedes the caller's request in
/// flight, then either answers `[]` right away or starts a URL request that answers later.
pub fn start(browser_id: i32, text: String, callback: Cb) {
    let previous = PENDING.with(|p| p.borrow_mut().remove(&browser_id));
    if let Some(previous) = previous {
        bump(|s| &s.superseded);
        conclude(previous, Err(Failure::Superseded));
    }
    let Some((url, query)) = endpoint(&text) else {
        bump(|s| &s.skipped);
        reply(&callback, &text, Vec::new());
        return;
    };
    let id = NEXT_ID.with(|n| n.replace(n.get() + 1));
    PENDING.with(|p| {
        p.borrow_mut().insert(browser_id, Pending { id, text, query, callback, body: Vec::new(), request: None });
    });
    bump(|s| &s.started);
    // No borrow held: CEF may complete a request synchronously (e.g. an unsupported URL).
    match create_request(&url, browser_id, id) {
        Some(request) => {
            let orphan = PENDING.with(|p| match p.borrow_mut().get_mut(&browser_id) {
                Some(pending) if pending.id == id => {
                    pending.request = Some(request);
                    None
                }
                _ => Some(request),
            });
            if let Some(request) = orphan {
                task::post_ui(move || drop(request));
            }
            task::post_ui_delayed(TIMEOUT_MS, move || finish(browser_id, id, Err(Failure::TimedOut)));
        }
        None => {
            log_warn!("suggest: cannot create a URL request");
            finish(browser_id, id, Err(Failure::Network));
        }
    }
}

/// URL to fetch and the query it carries, or `None` when nothing may be sent.
fn endpoint(text: &str) -> Option<(String, String)> {
    let (enabled, engine) = controller::with_store(|s| (s.settings().search_suggestions, s.settings().search_engine))?;
    if !enabled {
        return None;
    }
    let query = fetch_query(text)?;
    let url = suggest_url(engine, &query, ui_language())?;
    #[cfg(debug_assertions)]
    if let Some(template) = test_endpoint() {
        let encoded = sta_core::omnibox::encode_uri_component(&query);
        return Some((template.replace("{q}", &encoded), query));
    }
    Some((url, query))
}

/// The text sent to the search engine for `text`: `None` when it is blank, longer than
/// [`MAX_TEXT_CHARS`], or URL-like with an explicit scheme (`https://…`, `mailto:…`,
/// `zoommtg://…`) or a Windows path (`C:\…`, `\\server\…`). A leading `?` (forced search) is
/// stripped, and whatever follows it is a search, so it is sent. Trailing whitespace is kept
/// (engines suggest the next word for `rust `).
pub fn fetch_query(text: &str) -> Option<String> {
    if text.chars().count() > MAX_TEXT_CHARS {
        return None;
    }
    let t = text.trim_start();
    let query = match t.strip_prefix('?') {
        Some(rest) => rest.trim_start(),
        None if url_like(t) => return None,
        None => t,
    };
    (!query.trim().is_empty()).then(|| query.to_string())
}

/// Schemes that make typed text an address even without `//`.
const ADDRESS_SCHEMES: &[&str] = &[
    "http", "https", "file", "sta", "about", "data", "view-source", "chrome", "chrome-error", "devtools", "chrome-devtools",
    "javascript", "blob", "filesystem", "mailto", "tel", "sms", "ftp", "ws", "wss",
];

fn url_like(t: &str) -> bool {
    let b = t.as_bytes();
    let drive_path = b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && (b[2] == b'\\' || b[2] == b'/');
    if drive_path || t.starts_with("\\\\") {
        return true;
    }
    match sta_core::urls::scheme(t) {
        Some(scheme) if scheme.len() >= 2 => ADDRESS_SCHEMES.contains(&scheme.as_str()) || t[scheme.len() + 1..].starts_with("//"),
        _ => false,
    }
}

/// Primary OS UI language (`ko-KR`, `en-US`; empty when unknown), read once.
fn ui_language() -> &'static str {
    static LANG: OnceLock<String> = OnceLock::new();
    LANG.get_or_init(platform::os_ui_locale)
}

#[cfg(debug_assertions)]
fn test_endpoint() -> Option<&'static str> {
    static URL: OnceLock<Option<String>> = OnceLock::new();
    URL.get_or_init(|| std::env::var("STA_SUGGEST_URL").ok().filter(|u| !u.trim().is_empty())).as_deref()
}

const UR_FLAG_DISABLE_CACHE: i32 = sys::cef_urlrequest_flags_t::UR_FLAG_DISABLE_CACHE.0 as i32;
const UR_FLAG_NO_RETRY_ON_5XX: i32 = sys::cef_urlrequest_flags_t::UR_FLAG_NO_RETRY_ON_5XX.0 as i32;

/// `GET url` on the global request context, reported to a [`SuggestClient`] for
/// `(browser_id, id)`. `None` when CEF can't create the request.
fn create_request(url: &str, browser_id: i32, id: u64) -> Option<Urlrequest> {
    let mut request = request_create()?;
    request.set_url(Some(&CefString::from(url)));
    request.set_method(Some(&CefString::from("GET")));
    // Deliberately without UR_FLAG_ALLOW_STORED_CREDENTIALS: no cookies or HTTP auth are sent, and
    // no cookies are saved from the response.
    request.set_flags(UR_FLAG_DISABLE_CACHE | UR_FLAG_NO_RETRY_ON_5XX);
    let mut client = SuggestClient::new(browser_id, id);
    let mut context = request_context_get_global_context();
    urlrequest_create(Some(&mut request), Some(&mut client), context.as_mut())
}

wrap_urlrequest_client! {
    struct SuggestClient {
        browser_id: i32,
        id: u64,
    }

    impl UrlrequestClient {
        fn on_download_data(&self, _request: Option<&mut Urlrequest>, data: *const u8, data_length: usize) {
            if data.is_null() || data_length == 0 {
                return;
            }
            // SAFETY: CEF passes `data_length` readable bytes that stay valid for this call.
            let chunk = unsafe { std::slice::from_raw_parts(data, data_length) };
            let too_large = PENDING.with(|p| match p.borrow_mut().get_mut(&self.browser_id) {
                Some(pending) if pending.id == self.id => {
                    if pending.body.len() + chunk.len() > MAX_BODY_BYTES {
                        true
                    } else {
                        pending.body.extend_from_slice(chunk);
                        false
                    }
                }
                _ => false,
            });
            if too_large {
                finish(self.browser_id, self.id, Err(Failure::TooLarge));
            }
        }

        fn on_request_complete(&self, request: Option<&mut Urlrequest>) {
            let result = match request {
                Some(r) if r.request_status() == UrlrequestStatus::SUCCESS => match r.response().map(|resp| resp.status()) {
                    Some(200) => Ok(()),
                    Some(status) => Err(Failure::Status(status)),
                    None => Err(Failure::Network),
                },
                _ => Err(Failure::Network),
            };
            finish(self.browser_id, self.id, result);
        }
    }
}

/// Ends request `id` of `browser_id` if it is still the pending one (otherwise a no-op).
fn finish(browser_id: i32, id: u64, result: Result<(), Failure>) {
    let taken = PENDING.with(|p| {
        let mut p = p.borrow_mut();
        if p.get(&browser_id).is_some_and(|pending| pending.id == id) { p.remove(&browser_id) } else { None }
    });
    if let Some(pending) = taken {
        match result {
            Ok(()) => bump(|s| &s.succeeded),
            Err(Failure::TimedOut) => bump(|s| &s.timed_out),
            Err(_) => bump(|s| &s.failed),
        }
        conclude(pending, result);
    }
}

/// Answers a request that has left the pending map and releases its URL request.
fn conclude(mut pending: Pending, result: Result<(), Failure>) {
    let suggestions = match result {
        Ok(()) => parse_suggestions(&pending.body, &pending.query),
        Err(Failure::Superseded) => Vec::new(),
        Err(failure) => {
            log_debug!("suggest: request {} ended without suggestions ({failure:?})", pending.id);
            Vec::new()
        }
    };
    reply(&pending.callback, &pending.text, suggestions);
    if let Some(request) = pending.request.take() {
        let cancel = result.is_err();
        // Posted: cancelling may call the client synchronously, and the handle shouldn't be
        // released inside its own callback.
        task::post_ui(move || {
            if cancel && request.request_status() == UrlrequestStatus::IO_PENDING {
                request.cancel();
            }
            drop(request);
        });
    }
}

fn reply(callback: &Cb, text: &str, suggestions: Vec<String>) {
    let json = serde_json::json!({ "text": text, "suggestions": suggestions }).to_string();
    if let Ok(cb) = callback.lock() {
        cb.success_str(&json);
    }
}

/// Drops every pending request (after the message loop ended, before `cef::shutdown()`).
pub fn clear() {
    let taken = PENDING.with(|p| std::mem::take(&mut *p.borrow_mut()));
    drop(taken);
}

#[cfg_attr(not(debug_assertions), allow(dead_code))] // used by debug.rs only
pub fn debug_snapshot() -> serde_json::Value {
    let in_flight = PENDING.with(|p| p.borrow().len());
    STATS.with(|s| {
        serde_json::json!({
            "inFlight": in_flight,
            "started": s.started.get(),
            "succeeded": s.succeeded.get(),
            "failed": s.failed.get(),
            "superseded": s.superseded.get(),
            "timedOut": s.timed_out.get(),
            "skipped": s.skipped.get(),
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fetch_query_rules() {
        assert_eq!(fetch_query("rust").as_deref(), Some("rust"));
        assert_eq!(fetch_query("  rust pro ").as_deref(), Some("rust pro "), "leading whitespace dropped, trailing kept");
        assert_eq!(fetch_query("러스트").as_deref(), Some("러스트"));
        assert_eq!(fetch_query("example.com").as_deref(), Some("example.com"), "no explicit scheme");
        assert_eq!(fetch_query("localhost:3000").as_deref(), Some("localhost:3000"));
        assert_eq!(fetch_query("c++: tutorial").as_deref(), Some("c++: tutorial"));
        assert_eq!(fetch_query("rust: book").as_deref(), Some("rust: book"));
        // Forced search: `?` stripped, then anything goes.
        assert_eq!(fetch_query("?rust").as_deref(), Some("rust"));
        assert_eq!(fetch_query(" ? what is rust").as_deref(), Some("what is rust"));
        assert_eq!(fetch_query("?https://x.com").as_deref(), Some("https://x.com"));
        // Nothing to send.
        for text in [
            "", "   ", "?", " ? ", "http://example.com", "https:", "HTTPS://Example.com/a b", "sta://settings", "file:///C:/x",
            "about:blank", "data:text/html,hi", "view-source:https://x.com", "chrome://version", "mailto:me@example.com", "tel:+1555",
            "javascript:alert(1)", "zoommtg://zoom.us/join", "C:\\Users\\me", "c:/temp", "\\\\server\\share",
        ] {
            assert_eq!(fetch_query(text), None, "{text:?}");
        }
        let long = "a".repeat(MAX_TEXT_CHARS);
        assert_eq!(fetch_query(&long).as_deref(), Some(long.as_str()));
        assert_eq!(fetch_query(&format!("{long}b")), None);
        let korean = "가".repeat(MAX_TEXT_CHARS);
        assert!(fetch_query(&korean).is_some(), "the limit counts characters, not bytes");
    }
}
