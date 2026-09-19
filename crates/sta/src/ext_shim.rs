//! "The current tab" for an extension whose popup card is open [owner: chrome]
//! (docs/research/extensions.md §6).
//!
//! Chromium's `tabs.query` / `windows.*` only walk Chrome `Browser` windows, and sta's tabs are
//! Alloy views in none of them — so a popup that asks for the tab it was opened over gets `[]`, and
//! its service worker "No current window". What *does* work natively is `tabs.get(<id>)` with an
//! sta tab's id. So while a popup card is open, sta evaluates one script (`ext_shim.js`) in
//! - **the popup page** — registered for its document before the page's own scripts run
//!   (`Page.addScriptToEvaluateOnNewDocument`), and evaluated again when the page has loaded;
//! - **that extension's service worker** — Chromium attaches the popup's DevTools session to
//!   service workers as they start (`Target.setAutoAttach`); a worker of any other extension is let
//!   go of at once.
//!
//! The script only supplies which tab the card was opened over. The tab's id is found **in the
//! popup page**: tab ids and CEF browser ids are handed out in the same order, so the tab is
//! `popup browser id − tab browser id` ids below the popup's own (`tabs.getCurrent`), and the URL
//! sta knows the tab by has to match what the extension's own `tabs.get` answers. That URL is part
//! of the script's configuration, which the extension can read — so it is only handed to an
//! extension whose manifest lets it read that URL anyway (`ExtensionFiles::may_read_url`). One
//! that may not never finds the tab itself, and everything an extension learns about the tab is
//! what Chromium lets *that extension* see. An id a page found is remembered for
//! its browser, and the last pair tells a service worker where to look (`near`) before the page
//! of a tab nobody asked about yet has answered — a popup's first message is often "which tab?".
//!
//! It ends with the popup: the page is gone, the worker session closes with the popup's browser,
//! and the worker's copy checks that the popup is still open before every answer.
//!
//! The same script lets `tabs.create` fall back to `windows.create` when the profile has no Chrome
//! window ("No current window"): that window is one `foreign.rs` hides and turns into an sta tab,
//! under core's usual verdict and budget. An extension page that is itself an sta tab gets the
//! script for that alone ([`on_extension_tab_document`]): its "Sign in" is a `tabs.create` too.
//!
//! **The toolbar click.** What pressing an extension's button does is the extension's to decide
//! at run time, not its manifest's: `action.setPopup('')` means "no popup — tell me about the
//! click" (1Password without an account opens its sign-in page that way, and its popup page, which
//! was never meant to be seen then, stays on the logo). sta has no button to press, but the
//! worker session can deliver the event: once per card the script asks `action.getPopup`, and an
//! empty popup with `onClicked` listeners gets `onClicked.dispatch(<the card's tab>)` — after which
//! the card, which has nothing to show, closes.
//!
//! Public API:
//! - `pub fn on_popup_created(popup_browser: i32, extension: &str, popup_url: &str, tab: Option<Id>)`
//! - `pub fn on_popup_loaded(popup_browser: i32)`, `pub fn on_popup_closed(popup_browser: i32)`
//! - `pub fn on_extension_tab_document(frame: &Frame, url: &str)`
//! - `pub fn clear()`, `pub fn debug_snapshot() -> serde_json::Value`

use crate::devtools_cdp::{self, User, WorkerTarget};
use crate::{controller, tabs, task};
use cef::*;
use serde_json::{Value, json};
use sta_core::{Command, Id};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;

const SHIM_JS: &str = include_str!("ext_shim.js");
const CALL_TIMEOUT_MS: i64 = 3000;
/// The popup page is asked for the tab's id from the moment its browser exists: a service worker
/// that is asked about the current tab by the popup's first message needs the answer by then. So
/// the first second is polled closely (an in-process call each), the rest until [`RESOLVE_FOR_MS`]
/// loosely — a page that has not answered by then cannot see the tab's URL at all.
const RESOLVE_EAGER_MS: i64 = 25;
const RESOLVE_LAZY_MS: i64 = 250;
const RESOLVE_EAGER_FOR_MS: i64 = 1000;
const RESOLVE_FOR_MS: i64 = 4000;
/// Tab ids remembered per browser (one never changes while its browser lives, and neither kind of
/// id is reused); emptied when it gets this large.
const KNOWN_MAX: usize = 512;
/// A service worker is asked what the toolbar button would do once it knows the card's tab — or
/// after this long, for a card over no tab it could be told about.
const ACTION_WAIT_MS: i64 = 600;

struct Shim {
    popup_browser: i32,
    /// `chrome-extension://<id>/`: the only workers that get the script.
    origin: String,
    popup_url: String,
    /// The tab the card was opened over: its browser, and its URL **if this extension may read
    /// it** (`ExtensionFiles::may_read_url`). The script's configuration is the extension's to
    /// read, so a URL Chromium would keep from it must not be in there; without one the tab is
    /// only known when another extension's popup found its id before.
    tab: Option<(Option<String>, i32)>,
    tab_id: Option<i64>,
    /// Worker sessions of this extension, in the order they attached.
    workers: Vec<String>,
    /// The toolbar-click question was asked (once per card).
    action_asked: bool,
}

#[derive(Default, Clone, Copy)]
struct Stats {
    popups: u64,
    resolved: u64,
    unresolved: u64,
    workers: u64,
    strangers: u64,
    clicks: u64,
}

thread_local! {
    static CURRENT: RefCell<Option<Shim>> = const { RefCell::new(None) };
    /// Browser id → the tab id extensions know it by, as popup pages found them.
    static KNOWN: RefCell<HashMap<i32, i64>> = RefCell::new(HashMap::new());
    /// The last pair found: where a tab nobody asked about yet probably is (`near`).
    static LAST: Cell<Option<(i32, i64)>> = const { Cell::new(None) };
    static STATS: Cell<Stats> = Cell::new(Stats::default());
}

fn stats(f: impl FnOnce(&mut Stats)) {
    let mut s = STATS.get();
    f(&mut s);
    STATS.set(s);
}

/// The tab's URL as extensions may be told about it: a page of the web, never one of sta's own.
fn shareable(url: &str) -> bool {
    matches!(sta_core::urls::scheme(url).as_deref(), Some("http" | "https" | "file" | "ftp"))
}

/// `(<ext_shim.js>)(<config>)`, for the page (`tab_id: None`: it finds the id) or a worker.
fn script(shim: &Shim, for_worker: bool) -> String {
    let (url, browser) = match &shim.tab {
        Some((url, browser)) => (json!(url), Some(*browser)),
        None => (Value::Null, None),
    };
    let config = match for_worker {
        // Tab ids and browser ids grow together, so the last pair seen says where this tab is —
        // unless Chromium made windows of its own since, which is why the worker looks *around* it.
        true => {
            let near = browser.zip(LAST.get()).map(|(b, (known_browser, known_tab))| known_tab + i64::from(b - known_browser));
            json!({ "popupUrl": shim.popup_url, "url": url, "tabId": shim.tab_id, "near": near })
        }
        false => json!({ "popupUrl": shim.popup_url, "url": url, "tabId": shim.tab_id, "delta": browser.map(|b| shim.popup_browser - b) }),
    };
    format!("({SHIM_JS})({config})")
}

/// The popup card's browser exists (its page has not committed yet).
pub fn on_popup_created(popup_browser: i32, extension: &str, popup_url: &str, tab: Option<Id>) {
    let tab = tab.and_then(tabs::browser_for_tab).and_then(|b| {
        let url = b.main_frame().map(|f| CefString::from(&f.url()).to_string()).unwrap_or_default();
        if !shareable(&url) || b.identifier() >= popup_browser {
            return None;
        }
        let readable = crate::extension_files::find(extension).is_some_and(|files| files.may_read_url(&url));
        Some((readable.then_some(url), b.identifier()))
    });
    let tab_id = tab.as_ref().and_then(|(_, browser)| KNOWN.with(|k| k.borrow().get(browser).copied()));
    let shim = Shim { popup_browser, origin: format!("chrome-extension://{extension}/"), popup_url: popup_url.to_string(), tab, tab_id, workers: Vec::new(), action_asked: false };
    let source = script(&shim, false);
    CURRENT.with(|c| *c.borrow_mut() = Some(shim));
    stats(|s| s.popups += 1);
    // The Page agent only runs registered scripts while it is enabled.
    devtools_cdp::call(popup_browser, User::Extensions, "Page.enable", json!({}), CALL_TIMEOUT_MS, |_| {});
    devtools_cdp::call(popup_browser, User::Extensions, "Page.addScriptToEvaluateOnNewDocument", json!({ "source": source, "runImmediately": true }), CALL_TIMEOUT_MS, |result| {
        if let Err(e) = result {
            log_warn!("ext_shim: the popup page's script was not registered: {e}");
        }
    });
    devtools_cdp::watch_workers(popup_browser, on_worker_attached);
    let params = json!({ "autoAttach": true, "waitForDebuggerOnStart": false, "flatten": true, "filter": [{ "type": "service_worker", "exclude": false }] });
    devtools_cdp::call(popup_browser, User::Extensions, "Target.setAutoAttach", params, CALL_TIMEOUT_MS, |result| {
        if let Err(e) = result {
            log_warn!("ext_shim: no service worker auto-attach: {e}");
        }
    });
    resolve(popup_browser, 0);
}

/// The popup page's main frame loaded: the one moment it can certainly answer.
pub fn on_popup_loaded(popup_browser: i32) {
    resolve(popup_browser, RESOLVE_FOR_MS);
}

/// Asks the popup page which tab id the card's tab has, again until it knows (`waited` ms so
/// far); from `RESOLVE_FOR_MS` on it is a single question.
fn resolve(popup_browser: i32, waited: i64) {
    // Only a page that may read the tab's URL can prove which tab it is.
    let asks = |s: &&Shim| s.popup_browser == popup_browser && s.tab_id.is_none() && s.tab.as_ref().is_some_and(|(url, _)| url.is_some());
    let source = CURRENT.with(|c| c.borrow().as_ref().filter(asks).map(|s| script(s, false)));
    let Some(source) = source else { return };
    // Installing is idempotent, so a page whose document beat the registered script still gets it.
    let expression = format!("(async () => {{ {source}; const s = globalThis[Symbol.for('sta.currentTab')]; const id = s && s.resolve ? await s.resolve() : undefined; return typeof id === 'number' ? id : null; }})()");
    let params = json!({ "expression": expression, "returnByValue": true, "awaitPromise": true });
    devtools_cdp::call(popup_browser, User::Extensions, "Runtime.evaluate", params, CALL_TIMEOUT_MS, move |result| {
        let id = result.ok().as_ref().and_then(|v| v.get("result")).and_then(|r| r.get("value")).and_then(Value::as_i64);
        match id {
            Some(id) => on_resolved(popup_browser, id),
            None if waited >= RESOLVE_FOR_MS => {}
            None => {
                let delay = if waited < RESOLVE_EAGER_FOR_MS { RESOLVE_EAGER_MS } else { RESOLVE_LAZY_MS };
                if waited + delay >= RESOLVE_FOR_MS {
                    stats(|s| s.unresolved += 1);
                    log_debug!("ext_shim: the popup of browser {popup_browser} cannot see its tab (no URL access, or the tab moved on)");
                    return;
                }
                task::post_ui_delayed(delay, move || resolve(popup_browser, waited + delay));
            }
        }
    });
}

fn on_resolved(popup_browser: i32, tab_id: i64) {
    let workers = CURRENT.with(|c| {
        let mut current = c.borrow_mut();
        let shim = current.as_mut().filter(|s| s.popup_browser == popup_browser && s.tab_id.is_none())?;
        shim.tab_id = Some(tab_id);
        Some((shim.workers.clone(), shim.tab.as_ref().map(|(_, browser)| *browser)))
    });
    let Some((workers, tab_browser)) = workers else { return };
    if let Some(browser) = tab_browser {
        KNOWN.with(|k| {
            let mut known = k.borrow_mut();
            if known.len() >= KNOWN_MAX {
                known.clear();
            }
            known.insert(browser, tab_id);
        });
        LAST.set(Some((browser, tab_id)));
    }
    stats(|s| s.resolved += 1);
    log_debug!("ext_shim: the card's tab is tab {tab_id} to the extension");
    for session in workers {
        install_in_worker(popup_browser, &session);
    }
}

/// Asks the extension's service worker what its toolbar button would do now, once per card: when
/// the worker has the script and knows the card's tab (`forced`: or has waited long enough).
fn ask_action(popup_browser: i32, forced: bool) {
    let session = CURRENT.with(|c| {
        let mut current = c.borrow_mut();
        let shim = current.as_mut().filter(|s| s.popup_browser == popup_browser && !s.action_asked)?;
        let waits_for_tab = shim.tab_id.is_none() && shim.tab.as_ref().is_some_and(|(url, _)| url.is_some());
        if waits_for_tab && !forced {
            return None;
        }
        let session = shim.workers.first()?.clone();
        shim.action_asked = true;
        Some(session)
    });
    let Some(session) = session else { return };
    let expression = "(async () => { const s = globalThis[Symbol.for('sta.currentTab')]; return s && s.action ? JSON.stringify(await s.action()) : null; })()";
    devtools_cdp::worker_evaluate(popup_browser, &session, expression, CALL_TIMEOUT_MS, move |result| {
        let answer = result.ok().as_ref().and_then(|v| v.get("result")).and_then(|r| r.get("value")).and_then(Value::as_str).and_then(|text| serde_json::from_str::<Value>(text).ok());
        let clicked = answer.as_ref().and_then(|a| a.get("clicked")).and_then(Value::as_bool).unwrap_or(false);
        if !clicked {
            return;
        }
        let ours = CURRENT.with(|c| c.borrow().as_ref().is_some_and(|s| s.popup_browser == popup_browser));
        if ours {
            stats(|s| s.clicks += 1);
            log_info!("ext_shim: the extension has no popup right now; its toolbar click was delivered instead");
            controller::dispatch(Command::CloseExtensionPopup);
        }
    });
}

fn on_worker_attached(popup_browser: i32, target: WorkerTarget) {
    let ours = CURRENT.with(|c| {
        let mut current = c.borrow_mut();
        let Some(shim) = current.as_mut().filter(|s| s.popup_browser == popup_browser && target.url.starts_with(&s.origin)) else { return false };
        shim.workers.push(target.session.clone());
        true
    });
    if !ours {
        stats(|s| s.strangers += 1);
        devtools_cdp::worker_detach(popup_browser, &target.session);
        return;
    }
    stats(|s| s.workers += 1);
    install_in_worker(popup_browser, &target.session);
    task::post_ui_delayed(ACTION_WAIT_MS, move || ask_action(popup_browser, true));
}

fn install_in_worker(popup_browser: i32, session: &str) {
    let source = CURRENT.with(|c| c.borrow().as_ref().filter(|s| s.popup_browser == popup_browser).map(|s| script(s, true)));
    let Some(source) = source else { return };
    devtools_cdp::worker_evaluate(popup_browser, session, &source, CALL_TIMEOUT_MS, move |result| match result {
        Ok(_) => ask_action(popup_browser, false),
        Err(e) => log_warn!("ext_shim: the service worker did not take the script: {e}"),
    });
}

/// A main-frame document of a tab: an extension's own page gets the script without a tab to name
/// (the page may be in the background), for the `tabs.create` fallback alone.
pub fn on_extension_tab_document(frame: &Frame, url: &str) {
    if !url.starts_with("chrome-extension://") {
        return;
    }
    let source = format!("({SHIM_JS})({})", json!({ "popupUrl": null, "url": null }));
    frame.execute_java_script(Some(&CefString::from(source.as_str())), Some(&CefString::from("")), 1);
}

/// The popup's browser is gone (sta closed the card, or the page closed itself). Its DevTools
/// session, and with it every worker session, went with it.
pub fn on_popup_closed(popup_browser: i32) {
    CURRENT.with(|c| {
        let mut current = c.borrow_mut();
        if current.as_ref().is_some_and(|s| s.popup_browser == popup_browser) {
            *current = None;
        }
    });
    devtools_cdp::on_browser_closed(popup_browser);
}

pub fn clear() {
    CURRENT.with(|c| *c.borrow_mut() = None);
    KNOWN.with(|k| k.borrow_mut().clear());
    LAST.set(None);
}

#[cfg_attr(not(debug_assertions), allow(dead_code))] // debug.rs only
pub fn debug_snapshot() -> Value {
    let s = STATS.get();
    let current = CURRENT.with(|c| {
        c.borrow().as_ref().map(|shim| {
            json!({
                "popupBrowser": shim.popup_browser,
                "origin": shim.origin,
                "tab": shim.tab.as_ref().map(|(url, browser)| json!({ "url": url, "browser": browser })),
                "tabId": shim.tab_id,
                "workers": shim.workers.len(),
            })
        })
    });
    json!({
        "current": current,
        "known": KNOWN.with(|k| k.borrow().len()),
        "stats": { "popups": s.popups, "resolved": s.resolved, "unresolved": s.unresolved, "workers": s.workers, "strangers": s.strangers, "clicks": s.clicks },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_web_pages_are_shared_with_an_extension() {
        for url in ["https://example.com/login", "http://127.0.0.1:8080/", "file:///C:/page.html"] {
            assert!(shareable(url), "{url}");
        }
        for url in ["sta://settings/", "chrome://extensions/", "devtools://devtools/x", "about:blank", "javascript:1", ""] {
            assert!(!shareable(url), "{url}");
        }
    }

    #[test]
    fn the_page_finds_the_id_and_the_worker_is_given_it() {
        let mut shim = Shim { popup_browser: 9, origin: "chrome-extension://abc/".into(), popup_url: "chrome-extension://abc/popup.html".into(), tab: Some((Some("https://example.com/\"x".into()), 6)), tab_id: None, workers: Vec::new(), action_asked: false };
        // The script is an expression applied to its configuration: `(<file>)(<config>)`.
        let config = |script: String| script.rsplit_once(")(").map(|(_, c)| c.to_string()).unwrap_or_default();
        let page = config(script(&shim, false));
        assert!(page.contains(r#""delta":3"#) && page.contains(r#""tabId":null"#), "{page}");
        assert!(page.contains(r#"https://example.com/\"x"#), "the URL is JSON, not pasted: {page}");
        // A tab nobody asked about yet is expected where the last known pair says it is.
        LAST.set(Some((4, 687099968)));
        assert!(config(script(&shim, true)).contains(r#""near":687099970"#));
        LAST.set(None);
        shim.tab_id = Some(687099970);
        let worker = config(script(&shim, true));
        assert!(worker.contains(r#""tabId":687099970"#) && worker.contains(r#""near":null"#) && !worker.contains("delta"), "{worker}");
        // An extension that may not read the URL is not handed it: the id alone, if one is known.
        shim.tab = Some((None, 6));
        let blind = config(script(&shim, false));
        assert!(blind.contains(r#""url":null"#) && blind.contains(r#""tabId":687099970"#) && !blind.contains("example.com"), "{blind}");
        shim.tab = None;
        assert!(config(script(&shim, false)).contains(r#""url":null"#));
    }
}
