//! Phase 0 spike requests (debug builds only) [owner: automation]. See docs/research/automation.md.
//!
//! | request | payload | result |
//! |---|---|---|
//! | `debug.cdp` | `{tab, method, params?, sessionId?, timeoutMs?=10000}` | `{result, ms}` or `{error, ms}`: any DevTools method through the in-process session of that tab's browser |
//! | `debug.cdpEvents` | `{tab, clear?}` | the last 200 events of that browser's session (params truncated) |
//! | `debug.tabKey` | `{tab, key?="a"}` | a key press sent to that tab's browser host (`send_key_event`): reaches `on_pre_key_event` like the user's typing (agent takeover tests) without the OS foreground |
//!
//! These bypass the allowlist on purpose (spike measurements). They are only reachable through
//! the debug IPC of trusted UI pages in debug builds; release builds have no `debug.*` requests.

use crate::automation::{cdp, exec};
use crate::ipc::{self, Reply};
use crate::tabs;
use cef::wrapper::message_router::BrowserSideCallback;
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

type Cb = Arc<Mutex<dyn BrowserSideCallback>>;

fn reply(cb: &Cb, value: Value) {
    if let Ok(cb) = cb.lock() {
        cb.success_str(&value.to_string());
    }
}

pub fn handle(cmd: &str, payload: Value, callback: &Cb) -> Reply {
    match cmd {
        "debug.cdp" => {
            #[derive(Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Q {
                tab: u64,
                method: String,
                #[serde(default)]
                params: Value,
                #[serde(default)]
                session_id: Option<String>,
                #[serde(default)]
                timeout_ms: Option<i64>,
            }
            let q: Q = match ipc::parse(payload) {
                Ok(q) => q,
                Err(r) => return r,
            };
            let cb = callback.clone();
            crate::task::post_ui(move || {
                let Some(browser) = tabs::browser_for_tab(q.tab) else {
                    reply(&cb, json!({ "error": "no browser for tab" }));
                    return;
                };
                let id = cef::ImplBrowser::identifier(&browser);
                drop(browser);
                let params = if q.params.is_null() { json!({}) } else { q.params };
                exec::spawn(async move {
                    let start = std::time::Instant::now();
                    let r = cdp::debug_raw(id, q.method, params, q.session_id, q.timeout_ms.unwrap_or(10_000)).await;
                    let ms = start.elapsed().as_millis() as u64;
                    match r {
                        Ok(v) => reply(&cb, json!({ "result": v, "ms": ms })),
                        Err(e) => reply(&cb, json!({ "error": e.to_string(), "ms": ms })),
                    }
                });
            });
            Reply::Deferred
        }
        "debug.cdpEvents" => {
            #[derive(Deserialize)]
            struct Q {
                tab: u64,
                #[serde(default)]
                clear: bool,
            }
            let q: Q = match ipc::parse(payload) {
                Ok(q) => q,
                Err(r) => return r,
            };
            let cb = callback.clone();
            crate::task::post_ui(move || {
                let events = tabs::browser_for_tab(q.tab).map(|b| cdp::debug_events(cef::ImplBrowser::identifier(&b), q.clear)).unwrap_or_default();
                reply(&cb, json!({ "events": events }));
            });
            Reply::Deferred
        }
        "debug.tabKey" => {
            #[derive(Deserialize)]
            struct Q {
                tab: u64,
                #[serde(default)]
                key: Option<String>,
            }
            let q: Q = match ipc::parse(payload) {
                Ok(q) => q,
                Err(r) => return r,
            };
            let ch = q.key.as_deref().and_then(|k| k.chars().next()).unwrap_or('a');
            let cb = callback.clone();
            crate::task::post_ui(move || {
                let Some(host) = tabs::browser_for_tab(q.tab).and_then(|b| cef::ImplBrowser::host(&b)) else {
                    reply(&cb, json!({ "error": "no browser for tab" }));
                    return;
                };
                let code = ch.to_ascii_uppercase() as i32;
                let unit = ch as u32 as u16;
                for type_ in [cef::KeyEventType::RAWKEYDOWN, cef::KeyEventType::CHAR, cef::KeyEventType::KEYUP] {
                    let event = cef::KeyEvent {
                        type_,
                        windows_key_code: if type_ == cef::KeyEventType::CHAR { unit as i32 } else { code },
                        character: unit,
                        unmodified_character: unit,
                        ..Default::default()
                    };
                    cef::ImplBrowserHost::send_key_event(&host, Some(&event));
                }
                reply(&cb, json!({ "sent": true }));
            });
            Reply::Deferred
        }
        _ => Reply::Err(404, format!("unknown debug request: {cmd}")),
    }
}
