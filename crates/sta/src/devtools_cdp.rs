//! In-process DevTools protocol client for the shell's own features [owner: tabs]
//! (ext design FINAL PLAN §1; docs/MCP.md "DevTools clients").
//!
//! AI agents have their own client (`automation/cdp.rs`) with a typed method allowlist. The shell's
//! other DevTools users — debug requests now, the docked DevTools bridge and the extensions
//! backend later — go through this module instead, never through `automation/` (a unit test keeps
//! `src/automation/` from naming this module):
//! - **closed method lists per user** ([`User`]): nothing else can be sent;
//! - **root message ids ≥ [`ROOT_ID_BASE`]** (`0x4000_0000`). CEF has one DevTools session per
//!   browser and every observer sees every message, so ids must never collide: the agent client
//!   keeps its ids below that value, each client ignores replies with the other's ids, and the one
//!   *untrusted* participant — a docked frontend, whose ids are its own — is **held** below
//!   `ROOT_ID_BASE` by [`bridge_from_frontend`] instead of being trusted to stay there;
//! - the agent client also drops every message carrying a `sessionId` it didn't create (it creates
//!   none), checked on the raw tail before copying;
//! - the observer only copies bytes and posts (it can run inside `send_dev_tools_message`); no
//!   borrow is held across a send; every call has a timeout; a detach fails pending calls.
//!
//! ## The docked DevTools bridge (phase 2, FINAL PLAN §3)
//!
//! [`bridge_attach`] gives a tab's DevTools frontend a child session **S** of the inspected
//! browser's in-process session (`Target.attachToTarget{self, flatten}`), and relays messages both
//! ways without ever exposing the root session (SEC-1):
//! - frontend -> backend ([`bridge_from_frontend`]): the message is judged by `devtools_policy`
//!   (allowlist, `Target.*` rules, nested-session admission); an allowed one gets `S`'s (or the
//!   nested session's) id and goes to Chromium, a refused or unknown one is answered with a protocol
//!   error the frontend shows. The frontend never learns S's id.
//! - backend -> frontend: only messages on S (whose `sessionId` is *removed*, so the frontend sees
//!   its own session) and on the nested sessions this module admitted. `Target.attachedToTarget`
//!   under an admitted session admits or immediately detaches the new one.
//! - `sessionId` is added and removed by **string surgery** on the raw message: Chromium appends it
//!   as the last key of the map, so a heap-snapshot chunk of tens of megabytes costs one copy
//!   instead of a parse and a re-serialization. Only the **top-level** field is ever touched:
//!   appending is all a frontend message needs (a nested session's own id is already the one to
//!   send), because rewriting by "the last `sessionId` in the text" corrupted every message whose
//!   `params` carry one — `Target.detachFromTarget{sessionId}` left as `"params":{,}`.
//! - A message above [`MAX_MESSAGE_BYTES`] cannot be relayed, in either direction: it is counted,
//!   logged once and **answered** with a protocol error, so nothing waits forever for it.
//! - sta's own calls on S (`Inspect`) use [`session_call`], whose ids are in this client's root
//!   range, so a reply of ours is never mistaken for one of the frontend's.
//!
//! Public API:
//! - `pub const ROOT_ID_BASE: i32`, `pub enum User`
//! - `pub fn call(browser_id: i32, user: User, method: &str, params: Value, timeout_ms: i64, done: impl FnOnce(Result<Value, String>) + 'static)`
//! - `pub fn session_call(browser_id: i32, method: &str, params: Value, timeout_ms: i64, done: ...)`
//! - `pub fn bridge_attach(browser_id: i32)`, `pub fn bridge_detach(browser_id: i32)`,
//!   `pub fn bridge_is_attached(browser_id: i32) -> bool`, `pub fn bridge_from_frontend(browser_id: i32, raw: &str)`
//! - `pub struct WorkerTarget { session, url }`, `pub fn watch_workers(browser_id: i32, on_attached: fn(i32, WorkerTarget))`,
//!   `pub fn worker_evaluate(browser_id: i32, session: &str, expression: &str, timeout_ms: i64, done: ...)`,
//!   `pub fn worker_detach(browser_id: i32, session: &str)` — the service workers an extension
//!   popup's session is auto-attached to (`ext_shim.rs`)
//! - `pub fn on_browser_closed(browser_id: i32)`, `pub fn clear()`, `pub fn debug_snapshot() -> Value`

use crate::task;
use cef::*;
use serde_json::{Value, json};
use std::cell::RefCell;
use std::collections::HashMap;

/// First root message id of this client; `automation/cdp.rs` stays below it.
pub const ROOT_ID_BASE: i32 = 0x4000_0000;
/// Last root message id of this client. The test surface's raw CDP client
/// (`test_hooks::js::raw`, debug builds) starts right above it, so the three id spaces partition
/// `1..i32::MAX` instead of merely being unlikely to meet — phase 2 puts the docked DevTools bridge
/// on this client, where a reply delivered to the wrong one is what SEC-1 was written about. The
/// frontend's own ids are kept in `1..ROOT_ID_BASE` by [`bridge_from_frontend`], so the partition
/// holds for the untrusted participant too rather than relying on it never counting that far.
pub const ROOT_ID_MAX: i32 = 0x6FFF_FFFF;
/// Largest DevTools message this module relays, in either direction. Anything larger is answered
/// with a protocol error (a lost reply is what DevTools waits forever for).
const MAX_MESSAGE_BYTES: usize = 64 * 1024 * 1024;
/// Timeout of the bridge's own calls (session setup, detach, Inspect).
const ATTACH_TIMEOUT_MS: i64 = 5000;

/// Who sends, with a closed list of methods each.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum User {
    /// Debug builds' test requests (`debug.foreign`): open a Chrome-created browser like an
    /// extension would.
    #[cfg_attr(not(debug_assertions), allow(dead_code))]
    Debug,
    /// The docked DevTools of a tab: setting up session S and sta's own calls on it (Inspect). The
    /// *frontend's* own traffic is not in this list — it goes through `bridge_from_frontend`, which
    /// judges it with `devtools_policy`.
    DevTools,
    /// The extensions work (phase 3): the hidden `chrome://extensions` backend runs one fixed script
    /// per operation (`ext_backend.rs`), and the action-popup card measures the popup page it hosts
    /// (`ext_popup.rs`). Both are sta's own pages-of-record, and `Runtime.evaluate` is all either
    /// needs — no DOM, no network, no storage. The popup card also gives its page and its own
    /// extension's service worker the "current tab" shim (`ext_shim.rs`): a script for the page's
    /// next document, and auto-attach to service workers, whose sessions only ever get
    /// `Runtime.evaluate` ([`worker_evaluate`]).
    Extensions,
}

impl User {
    pub fn methods(self) -> &'static [&'static str] {
        match self {
            User::Debug => &["Target.createTarget"],
            User::Extensions => &["Runtime.evaluate", "Page.enable", "Page.addScriptToEvaluateOnNewDocument", "Target.setAutoAttach", "Target.detachFromTarget"],
            User::DevTools => &[
                "DOM.describeNode",
                "Emulation.setEmulatedMedia",
                "DOM.getBoxModel",
                "DOM.getNodeForLocation",
                "Target.attachToTarget",
                "Target.detachFromTarget",
                "Target.getTargetInfo",
            ],
        }
    }

    pub fn allows(self, method: &str) -> bool {
        self.methods().contains(&method)
    }
}

type Done = Box<dyn FnOnce(Result<Value, String>)>;

struct Client {
    _registration: Option<Registration>,
    next_id: i32,
    pending: HashMap<i32, Done>,
}

thread_local! {
    static CLIENTS: RefCell<HashMap<i32, Client>> = RefCell::new(HashMap::new());
}

/// The next root id after `id`, wrapping inside `ROOT_ID_BASE..=ROOT_ID_MAX`.
fn next_root_id(id: i32) -> i32 {
    if (ROOT_ID_BASE..ROOT_ID_MAX).contains(&id) { id + 1 } else { ROOT_ID_BASE }
}

wrap_dev_tools_message_observer! {
    struct Observer {
        browser_id: i32,
    }

    impl DevToolsMessageObserver {
        fn on_dev_tools_message(&self, _browser: Option<&mut Browser>, message: Option<&[u8]>) -> i32 {
            // 1 = handled: CEF still gives the message to every other observer, only its own
            // parse for this observer is skipped.
            let Some(bytes) = message else { return 1 };
            if bytes.len() > MAX_MESSAGE_BYTES {
                // Only the head is copied: enough to find whose call this answers, so the caller
                // (or the frontend) gets an error instead of silence.
                let (id, head, len) = (self.browser_id, bytes[..bytes.len().min(64)].to_vec(), bytes.len());
                task::post_ui(move || oversize(id, &head, len));
                return 1;
            }
            let own = is_own_reply(bytes);
            // The one root-session event this client reads: a service worker Chromium attached an
            // extension popup's session to (`watch_workers`).
            if !own && is_worker_attach(bytes) && watches_workers(self.browser_id) {
                let (id, bytes) = (self.browser_id, bytes.to_vec());
                task::post_ui(move || worker_attached(id, &bytes));
                return 1;
            }
            // Root-session events and the agent client's replies are never ours; a message on a
            // session only matters while a docked DevTools bridge exists for this browser.
            if !own && !bridge_is_attached(self.browser_id) {
                return 1;
            }
            if !own && !has_session_id(bytes) {
                return 1;
            }
            let (id, bytes) = (self.browser_id, bytes.to_vec());
            task::post_ui(move || match own {
                true => deliver(id, &bytes),
                false => from_backend(id, bytes),
            });
            1
        }

        fn on_dev_tools_agent_detached(&self, _browser: Option<&mut Browser>) {
            let id = self.browser_id;
            task::post_ui(move || fail_all(id, "DevTools agent detached"));
        }
    }
}

/// The `id` of a raw reply, read from its head (`{"id":N,` is how Chromium serializes one) without
/// parsing or copying it. `None` for an event, which has no id.
fn head_id(bytes: &[u8]) -> Option<i64> {
    let head = &bytes[..bytes.len().min(32)];
    let rest = head.strip_prefix(b"{\"id\":")?;
    let digits: Vec<u8> = rest.iter().copied().take_while(u8::is_ascii_digit).collect();
    std::str::from_utf8(&digits).ok()?.parse::<i64>().ok()
}

/// A root reply with an id of this client.
fn is_own_reply(bytes: &[u8]) -> bool {
    head_id(bytes).is_some_and(|id| (ROOT_ID_BASE as i64..=ROOT_ID_MAX as i64).contains(&id))
}

/// A message above [`MAX_MESSAGE_BYTES`]: the bytes are gone, but whoever waits for them is told
/// (T5 — a 66 MiB reply used to vanish with no error, no log and no counter). A nested session's
/// oversize reply is answered on S, which is the only session the frontend can be told about here.
fn oversize(browser_id: i32, head: &[u8], len: usize) {
    log_warn!("devtools_cdp: a {len}-byte message for browser {browser_id} is over the {MAX_MESSAGE_BYTES}-byte limit and was dropped");
    let id = head_id(head);
    if is_own_reply(head) {
        if let Some(id) = id {
            fail_pending(browser_id, id as i32, "the DevTools answer was too large");
        }
        return;
    }
    if !bridge_is_attached(browser_id) {
        return;
    }
    BRIDGES.with(|b| {
        if let Some(br) = b.borrow_mut().get_mut(&browser_id) {
            br.dropped += 1;
        }
    });
    protocol_error(browser_id, id, None, "the answer was too large for sta to relay");
}

/// A reply to one of *this* client's calls — root (`call`) or on a session (`session_call`), both
/// of which use ids in this client's range, so a `sessionId` here is one we sent ourselves.
fn deliver(browser_id: i32, bytes: &[u8]) {
    let Ok(msg) = serde_json::from_slice::<Value>(bytes) else { return };
    let Some(id) = msg.get("id").and_then(Value::as_i64) else { return };
    let done = CLIENTS.with(|c| c.borrow_mut().get_mut(&browser_id).and_then(|cl| cl.pending.remove(&(id as i32))));
    let Some(done) = done else { return };
    let result = match msg.get("error") {
        Some(err) => Err(err.get("message").and_then(Value::as_str).unwrap_or("DevTools error").to_string()),
        None => Ok(msg.get("result").cloned().unwrap_or(Value::Null)),
    };
    done(result);
}

fn fail_all(browser_id: i32, why: &str) {
    let pending = CLIENTS.with(|c| c.borrow_mut().get_mut(&browser_id).map(|cl| std::mem::take(&mut cl.pending)).unwrap_or_default());
    for (_, done) in pending {
        done(Err(why.to_string()));
    }
}

/// Sends `method` for `user` to `browser_id`'s DevTools session; `done` runs (posted or on
/// timeout) exactly once.
pub fn call(browser_id: i32, user: User, method: &str, params: Value, timeout_ms: i64, done: impl FnOnce(Result<Value, String>) + 'static) {
    if !user.allows(method) {
        log_warn!("devtools_cdp: {method} is not allowed for {user:?}");
        done(Err(format!("{method} is not allowed")));
        return;
    }
    let Some(host) = crate::browsers::browser(browser_id).and_then(|b| b.host()) else {
        done(Err("browser is gone".into()));
        return;
    };
    drop(host);
    let id = match register_pending(browser_id, Box::new(done)) {
        Some(id) => id,
        None => return,
    };
    let message = json!({ "id": id, "method": method, "params": params });
    let Ok(text) = serde_json::to_string(&message) else { return };
    if !send_raw(browser_id, &text) {
        fail_pending(browser_id, id, "DevTools message not accepted");
        return;
    }
    arm_timeout(browser_id, id, timeout_ms);
}

/// Registers `done` under a fresh id of this client's range, creating the observer on first use.
fn register_pending(browser_id: i32, done: Done) -> Option<i32> {
    let registered = CLIENTS.with(|c| c.borrow().contains_key(&browser_id));
    if !registered {
        let Some(host) = crate::browsers::browser(browser_id).and_then(|b| b.host()) else {
            done(Err("browser is gone".into()));
            return None;
        };
        let mut observer = Observer::new(browser_id);
        let registration = host.add_dev_tools_message_observer(Some(&mut observer));
        if registration.is_none() {
            done(Err("no DevTools observer".into()));
            return None;
        }
        CLIENTS.with(|c| c.borrow_mut().insert(browser_id, Client { _registration: registration, next_id: ROOT_ID_BASE, pending: HashMap::new() }));
    }
    CLIENTS.with(|c| {
        let mut c = c.borrow_mut();
        let client = c.get_mut(&browser_id)?;
        let id = client.next_id;
        client.next_id = next_root_id(id);
        client.pending.insert(id, done);
        Some(id)
    })
}

/// The next id of this client's range without a pending entry (messages that expect no reply).
fn next_id(browser_id: i32) -> i32 {
    CLIENTS.with(|c| {
        let mut c = c.borrow_mut();
        match c.get_mut(&browser_id) {
            Some(client) => {
                let id = client.next_id;
                client.next_id = next_root_id(id);
                id
            }
            None => ROOT_ID_BASE,
        }
    })
}

/// Sends one already-serialized message. No borrow is held: observer callbacks may run inside.
fn send_raw(browser_id: i32, message: &str) -> bool {
    let Some(host) = crate::browsers::browser(browser_id).and_then(|b| b.host()) else { return false };
    host.send_dev_tools_message(Some(message.as_bytes())) != 0
}

fn fail_pending(browser_id: i32, id: i32, why: &str) {
    let done = CLIENTS.with(|c| c.borrow_mut().get_mut(&browser_id).and_then(|cl| cl.pending.remove(&id)));
    if let Some(done) = done {
        done(Err(why.to_string()));
    }
}

fn arm_timeout(browser_id: i32, id: i32, timeout_ms: i64) {
    task::post_ui_delayed(timeout_ms.max(1), move || fail_pending(browser_id, id, "DevTools call timed out"));
}

/// `on_before_close` of a browser this client may have used.
#[allow(dead_code)] // wired by the docked DevTools phase; clear() covers shutdown today
pub fn on_browser_closed(browser_id: i32) {
    BRIDGES.with(|b| b.borrow_mut().remove(&browser_id));
    WORKER_WATCHES.with(|w| w.borrow_mut().remove(&browser_id));
    let client = CLIENTS.with(|c| c.borrow_mut().remove(&browser_id));
    if let Some(mut client) = client {
        for (_, done) in std::mem::take(&mut client.pending) {
            done(Err("browser is gone".into()));
        }
        task::post_ui(move || drop(client));
    }
}

pub fn clear() {
    let clients = CLIENTS.with(|c| std::mem::take(&mut *c.borrow_mut()));
    BRIDGES.with(|b| b.borrow_mut().clear());
    WORKER_WATCHES.with(|w| w.borrow_mut().clear());
    drop(clients);
}

// ============================================================================ extension workers
//
// `ext_shim.rs` turns on `Target.setAutoAttach` (service workers only) on an extension popup's
// browser. Chromium then announces each worker with a root-session `Target.attachedToTarget`; this
// section hands those to the watcher and lets it evaluate in the sessions it was told about — and
// in no other.

/// How a root-session `Target.attachedToTarget` starts (Chromium writes `method` first).
const WORKER_ATTACHED_HEAD: &[u8] = b"{\"method\":\"Target.attachedToTarget\"";

/// A service worker Chromium attached a watching browser's session to.
pub struct WorkerTarget {
    pub session: String,
    pub url: String,
}

struct WorkerWatch {
    on_attached: fn(i32, WorkerTarget),
    sessions: std::collections::HashSet<String>,
}

thread_local! {
    static WORKER_WATCHES: RefCell<HashMap<i32, WorkerWatch>> = RefCell::new(HashMap::new());
}

/// `on_attached` runs for every service worker `browser_id`'s session gets attached to, until the
/// browser closes. Turning auto-attach on is the caller's `call`.
pub fn watch_workers(browser_id: i32, on_attached: fn(i32, WorkerTarget)) {
    WORKER_WATCHES.with(|w| w.borrow_mut().insert(browser_id, WorkerWatch { on_attached, sessions: Default::default() }));
}

fn watches_workers(browser_id: i32) -> bool {
    WORKER_WATCHES.with(|w| w.borrow().contains_key(&browser_id))
}

fn is_worker_attach(bytes: &[u8]) -> bool {
    bytes.starts_with(WORKER_ATTACHED_HEAD)
}

fn worker_attached(browser_id: i32, bytes: &[u8]) {
    let Ok(msg) = serde_json::from_slice::<Value>(bytes) else { return };
    let params = msg.get("params");
    let Some(session) = params.and_then(|p| p.get("sessionId")).and_then(Value::as_str).filter(|s| !s.is_empty() && s.len() <= MAX_SESSION_ID) else { return };
    let info = params.and_then(|p| p.get("targetInfo"));
    let kind = info.and_then(|i| i.get("type")).and_then(Value::as_str).unwrap_or_default();
    let url = info.and_then(|i| i.get("url")).and_then(Value::as_str).unwrap_or_default().to_string();
    let session = session.to_string();
    let handler = WORKER_WATCHES.with(|w| {
        let mut w = w.borrow_mut();
        let watch = w.get_mut(&browser_id)?;
        watch.sessions.insert(session.clone());
        Some(watch.on_attached)
    });
    let Some(handler) = handler else { return };
    if kind != "service_worker" {
        // The filter asks for service workers only; anything else is let go of at once.
        worker_detach(browser_id, &session);
        return;
    }
    handler(browser_id, WorkerTarget { session, url });
}

/// `Runtime.evaluate` in a worker session [`watch_workers`] announced for this browser.
pub fn worker_evaluate(browser_id: i32, session: &str, expression: &str, timeout_ms: i64, done: impl FnOnce(Result<Value, String>) + 'static) {
    let known = WORKER_WATCHES.with(|w| w.borrow().get(&browser_id).is_some_and(|watch| watch.sessions.contains(session)));
    if !known {
        done(Err("unknown worker session".into()));
        return;
    }
    let id = match register_pending(browser_id, Box::new(done)) {
        Some(id) => id,
        None => return,
    };
    let params = json!({ "expression": expression, "returnByValue": true, "awaitPromise": true });
    let message = json!({ "id": id, "method": "Runtime.evaluate", "params": params, "sessionId": session }).to_string();
    if !send_raw(browser_id, &message) {
        fail_pending(browser_id, id, "DevTools message not accepted");
        return;
    }
    arm_timeout(browser_id, id, timeout_ms);
}

/// Lets go of a worker session (one that is not the popup's own extension's).
pub fn worker_detach(browser_id: i32, session: &str) {
    let known = WORKER_WATCHES.with(|w| w.borrow_mut().get_mut(&browser_id).is_some_and(|watch| watch.sessions.remove(session)));
    if known {
        call(browser_id, User::Extensions, "Target.detachFromTarget", json!({ "sessionId": session }), ATTACH_TIMEOUT_MS, |_| {});
    }
}

#[cfg_attr(not(debug_assertions), allow(dead_code))] // debug.rs only
pub fn debug_snapshot() -> Value {
    let clients: Vec<Value> = CLIENTS.with(|c| c.borrow().iter().map(|(id, cl)| json!({ "browser": id, "pending": cl.pending.len(), "nextId": cl.next_id })).collect());
    let bridges: Vec<Value> = BRIDGES.with(|b| {
        b.borrow()
            .iter()
            .map(|(id, br)| {
                json!({
                    "browser": id,
                    "session": br.session,
                    "queued": br.queued.len(),
                    "nested": br.nested.len(),
                    "nestedSessions": br.nested.iter().map(|(session, target)| json!([session, target])).collect::<Vec<Value>>(),
                    "refused": br.refused,
                    "fromFrontend": br.from_frontend,
                    "toFrontend": br.to_frontend,
                    "dropped": br.dropped,
                    "maxBytes": br.max_bytes,
                    "bytesToFrontend": br.bytes_to_frontend,
                    "byMethod": br.by_method.iter().map(|(m, n)| json!([m, n])).collect::<Vec<Value>>(),
                    "byMethodOut": br.by_method_out.iter().map(|(m, n)| json!([m, n])).collect::<Vec<Value>>(),
                })
            })
            .collect()
    });
    json!({ "clients": clients, "bridges": bridges })
}

// ===================================================================================== the bridge
//
// One `Bridge` per inspected browser, created by `bridge_attach` when a docked DevTools frontend
// opens and dropped by `bridge_detach`. Messages leave this module through
// `crate::devtools::deliver_to_frontend`, always outside the `BRIDGES` borrow.

/// Longest `sessionId` this module accepts (Chromium's are 32 hex characters).
const MAX_SESSION_ID: usize = 64;
/// Unknown method names remembered for `debug.info` (`devtools.refused`).
const MAX_REFUSED: usize = 32;
/// Messages kept while session S is still being attached, and the largest one kept.
const MAX_QUEUED: usize = 256;
const MAX_QUEUED_BYTES: usize = 256 * 1024;

struct Bridge {
    /// Session S, once `Target.attachToTarget` answered.
    session: Option<String>,
    /// Sessions admitted under S (`Target.attachedToTarget`): session id → its target id (for an
    /// iframe target that is the frame id, which is how Inspect finds a frame's session).
    nested: HashMap<String, String>,
    /// Messages the frontend sent before S existed. The frontend starts talking as soon as its
    /// document loads, which is a frame or two before `Target.attachToTarget` answers, and those
    /// first messages are its `enable` calls: dropping them leaves panels half-initialised.
    queued: Vec<String>,
    /// Method names refused as unknown (newest last, capped).
    refused: Vec<String>,
    from_frontend: u64,
    to_frontend: u64,
    dropped: u64,
    /// Largest message relayed to the frontend, and the total relayed (S11).
    max_bytes: usize,
    bytes_to_frontend: u64,
    /// How many messages of each event (or `(reply)`) went to the frontend, so `debug.info` can say
    /// *what* a busy session is busy with, and what the frontend sent.
    by_method: std::collections::BTreeMap<String, u64>,
    by_method_out: std::collections::BTreeMap<String, u64>,
}

thread_local! {
    static BRIDGES: RefCell<HashMap<i32, Bridge>> = RefCell::new(HashMap::new());
}

/// Starts a bridge for `browser_id` and attaches session S. Idempotent.
pub fn bridge_attach(browser_id: i32) {
    let fresh = BRIDGES.with(|b| {
        let mut b = b.borrow_mut();
        if b.contains_key(&browser_id) {
            return false;
        }
        b.insert(browser_id, Bridge {
            session: None,
            nested: HashMap::new(),
            queued: Vec::new(),
            refused: Vec::new(),
            from_frontend: 0,
            to_frontend: 0,
            dropped: 0,
            max_bytes: 0,
            bytes_to_frontend: 0,
            by_method: std::collections::BTreeMap::new(),
            by_method_out: std::collections::BTreeMap::new(),
        });
        true
    });
    if !fresh {
        return;
    }
    // The page's own target id, then a flat child session on it.
    call(browser_id, User::DevTools, "Target.getTargetInfo", json!({}), ATTACH_TIMEOUT_MS, move |result| {
        let Some(target) = result.ok().and_then(|v| v.get("targetInfo").and_then(|t| t.get("targetId")).and_then(Value::as_str).map(str::to_string)) else {
            log_warn!("devtools bridge: browser {browser_id} has no target id");
            return;
        };
        let params = json!({ "targetId": target, "flatten": true });
        call(browser_id, User::DevTools, "Target.attachToTarget", params, ATTACH_TIMEOUT_MS, move |result| {
            match result.ok().and_then(|v| v.get("sessionId").and_then(Value::as_str).map(str::to_string)) {
                Some(session) => {
                    log_info!("devtools bridge: browser {browser_id} session {session}");
                    let queued = BRIDGES.with(|b| {
                        let mut b = b.borrow_mut();
                        let bridge = b.get_mut(&browser_id)?;
                        bridge.session = Some(session);
                        Some(std::mem::take(&mut bridge.queued))
                    });
                    let Some(queued) = queued else { return };
                    for raw in queued {
                        bridge_from_frontend(browser_id, &raw);
                    }
                    crate::devtools::on_session_ready(browser_id);
                }
                None => log_warn!("devtools bridge: browser {browser_id} could not attach a session"),
            }
        });
    });
}

pub fn bridge_detach(browser_id: i32) {
    let bridge = BRIDGES.with(|b| b.borrow_mut().remove(&browser_id));
    let Some(bridge) = bridge else { return };
    // Detaching S also drops every session under it.
    if let Some(session) = bridge.session
        && crate::browsers::browser(browser_id).is_some()
    {
        send_raw(browser_id, &json!({ "id": next_id(browser_id), "method": "Target.detachFromTarget", "params": { "sessionId": session } }).to_string());
    }
}

pub fn bridge_is_attached(browser_id: i32) -> bool {
    BRIDGES.with(|b| b.borrow().contains_key(&browser_id))
}

/// Session S of a bridge, once it exists.
pub fn bridge_session(browser_id: i32) -> Option<String> {
    BRIDGES.with(|b| b.borrow().get(&browser_id).and_then(|br| br.session.clone()))
}

/// Sends `method` on session S (sta's own calls, e.g. `DOM.getNodeForLocation` for Inspect).
/// `session` picks a nested session instead of S.
pub fn session_call(
    browser_id: i32,
    session: Option<&str>,
    method: &str,
    params: Value,
    timeout_ms: i64,
    done: impl FnOnce(Result<Value, String>) + 'static,
) {
    if !User::DevTools.allows(method) {
        log_warn!("devtools_cdp: {method} is not allowed for the docked DevTools");
        done(Err(format!("{method} is not allowed")));
        return;
    }
    let target = match session {
        Some(s) => {
            let known = BRIDGES.with(|b| b.borrow().get(&browser_id).is_some_and(|br| br.nested.contains_key(s)));
            if !known {
                done(Err("unknown session".into()));
                return;
            }
            s.to_string()
        }
        None => match bridge_session(browser_id) {
            Some(s) => s,
            None => {
                done(Err("no DevTools session".into()));
                return;
            }
        },
    };
    let id = match register_pending(browser_id, Box::new(done)) {
        Some(id) => id,
        None => return,
    };
    let message = json!({ "id": id, "method": method, "params": params, "sessionId": target }).to_string();
    if !send_raw(browser_id, &message) {
        fail_pending(browser_id, id, "DevTools message not accepted");
        return;
    }
    arm_timeout(browser_id, id, timeout_ms);
}

/// The origins of every frame of the inspected browser, for the few policy rules that judge a
/// message by the origin, storage key or cookie URL it names (`devtools_policy::needs_origins`).
/// Read on demand: a frame walk per protocol message would be absurd, and only cookie and storage
/// calls need it.
fn page_origins(browser_id: i32) -> Vec<String> {
    let Some(browser) = crate::browsers::browser(browser_id) else { return Vec::new() };
    let mut identifiers = CefStringList::new();
    browser.frame_identifiers(Some(&mut identifiers));
    let mut origins: Vec<String> = Vec::new();
    for identifier in identifiers {
        let url = browser
            .frame_by_identifier(Some(&CefString::from(identifier.as_str())))
            .map(|f| CefString::from(&f.url()).to_string())
            .unwrap_or_default();
        if let Some(origin) = crate::devtools_policy::origin_of(&url)
            && !origins.contains(&origin)
        {
            origins.push(origin);
        }
    }
    origins
}

/// One raw message the frontend sent (`InspectorFrontendHost.sendMessageToBackend`).
pub fn bridge_from_frontend(browser_id: i32, raw: &str) {
    use crate::devtools_policy::{Request, Session, Verdict};
    let Some(session) = bridge_session(browser_id) else {
        // Still attaching: keep the message (see `Bridge::queued`). A bounded queue, because a
        // frontend that never gets a session must not grow one without end.
        BRIDGES.with(|b| {
            if let Some(bridge) = b.borrow_mut().get_mut(&browser_id)
                && bridge.queued.len() < MAX_QUEUED
                && raw.len() <= MAX_QUEUED_BYTES
            {
                bridge.queued.push(raw.to_string());
            }
        });
        return;
    };
    // A command is small by construction; one this large is either a mistake or an attack, and it
    // is the same cap the other direction has (FINAL PLAN §3's "overflow", as far as a single
    // message goes — see `gates-p2.md` deviation 8 for the rate the plan also asked for).
    if raw.len() > MAX_MESSAGE_BYTES {
        log_warn!("devtools bridge: a {}-byte message from the frontend of browser {browser_id} was dropped", raw.len());
        note_dropped(browser_id);
        return;
    }
    // Small by construction (a protocol command); parsing is what the policy needs.
    let Ok(msg) = serde_json::from_str::<Value>(raw) else {
        log_warn!("devtools bridge: the frontend of browser {browser_id} sent something that is not a protocol message");
        note_dropped(browser_id);
        return;
    };
    let id = msg.get("id").and_then(Value::as_i64);
    let method = msg.get("method").and_then(Value::as_str).unwrap_or_default().to_string();
    let asked = msg.get("sessionId").and_then(Value::as_str).map(str::to_string);
    // The frontend's ids stay in their own space (`ROOT_ID_BASE`): the three clients of one browser
    // then really partition the id space, instead of the untrusted one being trusted to count
    // politely. A reply with a shell id would otherwise be delivered to a pending shell call.
    if id.is_some_and(|id| !(1..ROOT_ID_BASE as i64).contains(&id)) {
        log_warn!("devtools bridge: the frontend of browser {browser_id} used the message id {id:?}, which is not its own");
        note_dropped(browser_id);
        protocol_error(browser_id, id, asked.as_deref(), "this message id is reserved in sta");
        return;
    }
    let (kind, target) = match &asked {
        None => (Session::Own, session),
        Some(s) if BRIDGES.with(|b| b.borrow().get(&browser_id).is_some_and(|br| br.nested.contains_key(s))) => (Session::Nested, s.clone()),
        Some(_) => {
            protocol_error(browser_id, id, asked.as_deref(), "unknown session");
            return;
        }
    };
    let origins = if crate::devtools_policy::needs_origins(&method) { page_origins(browser_id) } else { Vec::new() };
    match crate::devtools_policy::check(&Request { method: &method, session: kind, params: msg.get("params"), origins: &origins }) {
        Verdict::Allow => {}
        Verdict::Refuse(why) => {
            log_warn!("devtools bridge: refused {method} from browser {browser_id}: {why}");
            protocol_error(browser_id, id, asked.as_deref(), why);
            return;
        }
        Verdict::Unknown => {
            log_warn!("devtools bridge: {method} is not in the DevTools method inventory (browser {browser_id})");
            note_refused(browser_id, &method);
            protocol_error(browser_id, id, asked.as_deref(), "this DevTools feature is not available in sta");
            return;
        }
    }
    // The frontend's own id stays; only the session is added. A message for a nested session already
    // carries exactly the id to send (`target == asked`), so it goes **as it is** — the old rewrite
    // stripped "the last `sessionId` in the text", which for `Target.detachFromTarget{sessionId}`
    // was the one inside `params` and left invalid JSON that Chromium answered nothing to (T2).
    let out = match &asked {
        Some(_) => raw.to_string(),
        None => append_session_id(raw, &target),
    };
    BRIDGES.with(|b| {
        if let Some(br) = b.borrow_mut().get_mut(&browser_id) {
            br.from_frontend += 1;
            *br.by_method_out.entry(method.clone()).or_insert(0) += 1;
        }
    });
    if !send_raw(browser_id, &out) {
        protocol_error(browser_id, id, asked.as_deref(), "DevTools is not available");
    }
}

/// A message from Chromium that carries a `sessionId` (the observer already decided it is not one of
/// this client's own replies).
fn from_backend(browser_id: i32, bytes: Vec<u8>) {
    let Some(session) = bridge_session(browser_id) else { return };
    let Some(found) = tail_session_id(&bytes) else { return };
    let own = found == session;
    let nested = !own && BRIDGES.with(|b| b.borrow().get(&browser_id).is_some_and(|br| br.nested.contains_key(found)));
    if !own && !nested {
        // A session neither S nor one we admitted: never the frontend's business.
        BRIDGES.with(|b| {
            if let Some(br) = b.borrow_mut().get_mut(&browser_id) {
                br.dropped += 1;
            }
        });
        return;
    }
    let found = found.to_string();
    // `Target.attachedToTarget` / `detachedFromTarget` maintain the nested set. Only these two are
    // parsed; everything else is relayed by surgery alone.
    match head_method(&bytes) {
        // A target sta will not let the frontend debug: not admitted, and not announced either.
        Some("Target.attachedToTarget") if !admit_nested(browser_id, &bytes) => return,
        Some("Target.detachedFromTarget") => forget_nested(browser_id, &bytes),
        _ => {}
    }
    // Messages on S arrive at the frontend without a session; nested ones keep theirs.
    let method = head_method(&bytes).unwrap_or("(reply)").to_string();
    let out = if own { without_session_id(&bytes, &found) } else { String::from_utf8_lossy(&bytes).into_owned() };
    BRIDGES.with(|b| {
        if let Some(br) = b.borrow_mut().get_mut(&browser_id) {
            br.to_frontend += 1;
            br.bytes_to_frontend += out.len() as u64;
            br.max_bytes = br.max_bytes.max(out.len());
            *br.by_method.entry(method).or_insert(0) += 1;
        }
    });
    crate::devtools::deliver_to_frontend(browser_id, out);
}

/// `Target.attachedToTarget`: admit the new session or detach it at once. Returns whether the event
/// may reach the frontend.
fn admit_nested(browser_id: i32, bytes: &[u8]) -> bool {
    let Ok(msg) = serde_json::from_slice::<Value>(bytes) else { return false };
    let params = msg.get("params");
    let Some(id) = params.and_then(|p| p.get("sessionId")).and_then(Value::as_str) else { return false };
    if id.len() > MAX_SESSION_ID {
        return false;
    }
    let info = params.and_then(|p| p.get("targetInfo"));
    let kind = info.and_then(|t| t.get("type")).and_then(Value::as_str).unwrap_or_default();
    let url = info.and_then(|t| t.get("url")).and_then(Value::as_str).unwrap_or_default();
    if !crate::devtools_policy::nested_target_allowed(kind, url) {
        log_warn!("devtools bridge: detaching a {kind} session of browser {browser_id}");
        let params = json!({ "sessionId": id });
        session_call(browser_id, None, "Target.detachFromTarget", params, ATTACH_TIMEOUT_MS, |_| {});
        return false;
    }
    let target = info.and_then(|t| t.get("targetId")).and_then(Value::as_str).unwrap_or_default().to_string();
    let id = id.to_string();
    BRIDGES.with(|b| {
        if let Some(br) = b.borrow_mut().get_mut(&browser_id) {
            br.nested.insert(id, target);
        }
    });
    true
}

/// The nested session whose target is `target_id` (for an iframe target, the frame id).
pub fn nested_session_for_target(browser_id: i32, target_id: &str) -> Option<String> {
    BRIDGES.with(|b| {
        b.borrow()
            .get(&browser_id)
            .and_then(|br| br.nested.iter().find(|(_, target)| target.as_str() == target_id).map(|(session, _)| session.clone()))
    })
}

fn forget_nested(browser_id: i32, bytes: &[u8]) {
    let Ok(msg) = serde_json::from_slice::<Value>(bytes) else { return };
    let Some(id) = msg.get("params").and_then(|p| p.get("sessionId")).and_then(Value::as_str) else { return };
    let id = id.to_string();
    BRIDGES.with(|b| {
        if let Some(br) = b.borrow_mut().get_mut(&browser_id) {
            br.nested.remove(&id);
        }
    });
}

/// One message the bridge did not pass on (too large, unparsable, or carrying a reserved id).
fn note_dropped(browser_id: i32) {
    BRIDGES.with(|b| {
        if let Some(br) = b.borrow_mut().get_mut(&browser_id) {
            br.dropped += 1;
        }
    });
}

fn note_refused(browser_id: i32, method: &str) {
    BRIDGES.with(|b| {
        if let Some(br) = b.borrow_mut().get_mut(&browser_id)
            && !br.refused.iter().any(|m| m == method)
        {
            if br.refused.len() >= MAX_REFUSED {
                br.refused.remove(0);
            }
            br.refused.push(method.to_string());
        }
    });
}

/// Answers one frontend message with a protocol error, so DevTools shows a failed call instead of
/// waiting forever.
fn protocol_error(browser_id: i32, id: Option<i64>, session: Option<&str>, message: &str) {
    let Some(id) = id else { return };
    let mut reply = json!({ "id": id, "error": { "code": -32601, "message": message } });
    if let Some(s) = session {
        reply["sessionId"] = Value::String(s.to_string());
    }
    crate::devtools::deliver_to_frontend(browser_id, reply.to_string());
}

// ------------------------------------------------------------------------------- string surgery
//
// Chromium appends `sessionId` as the **last** key of a protocol message (`crdtp`'s
// `AppendString8EntryToCBORMap` on the encoded map, which the JSON conversion keeps last), which is
// what lets these three functions avoid parsing multi-megabyte messages. Each falls back to doing
// nothing (or to a parse, for the reader) if the assumption ever fails.

/// The `sessionId` of a raw message, looked for in its tail first.
fn tail_session_id(bytes: &[u8]) -> Option<&str> {
    const NEEDLE: &[u8] = b"\"sessionId\":\"";
    let tail_start = bytes.len().saturating_sub(MAX_SESSION_ID + NEEDLE.len() + 4);
    let at = find(&bytes[tail_start..], NEEDLE).map(|i| tail_start + i).or_else(|| find(bytes, NEEDLE))?;
    let rest = &bytes[at + NEEDLE.len()..];
    let end = rest.iter().position(|b| *b == b'"')?;
    if end > MAX_SESSION_ID {
        return None;
    }
    std::str::from_utf8(&rest[..end]).ok()
}

/// The `method` of a raw event, read from its head (Chromium serializes `{"method":"X","params":…`).
/// Only the two `Target.*` events the bridge must act on are looked up this way; everything else is
/// relayed without a parse.
fn head_method(bytes: &[u8]) -> Option<&str> {
    const NEEDLE: &[u8] = b"\"method\":\"";
    let head = &bytes[..bytes.len().min(256)];
    let at = find(head, NEEDLE)?;
    let rest = &head[at + NEEDLE.len()..];
    let end = rest.iter().position(|b| *b == b'"')?;
    std::str::from_utf8(&rest[..end]).ok()
}

/// Whether a raw message carries a `sessionId` at all (the observer's cheap filter).
fn has_session_id(bytes: &[u8]) -> bool {
    tail_session_id(bytes).is_some()
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.len() > haystack.len() {
        return None;
    }
    (0..=haystack.len() - needle.len()).find(|i| &haystack[*i..*i + needle.len()] == needle)
}

/// `raw` with `"sessionId":"<session>"` appended as its last top-level key. Nothing inside the
/// message is read or rewritten, so a `params` key that happens to be called `sessionId` (which is
/// exactly what `Target.detachFromTarget` sends) survives untouched.
fn append_session_id(raw: &str, session: &str) -> String {
    let trimmed = raw.trim_end();
    if !trimmed.ends_with('}') || !trimmed.starts_with('{') {
        return raw.to_string();
    }
    let body = &trimmed[..trimmed.len() - 1];
    let separator = if body.trim_end().ends_with('{') { "" } else { "," };
    format!("{body}{separator}\"sessionId\":\"{session}\"}}")
}

/// `bytes` without its `"sessionId":"<session>"` entry (for messages on S, which the frontend must
/// see as its own session). The entry is looked for at the **end** of the object first, which is
/// where Chromium puts it: a body that happens to contain the same text earlier (a response body
/// quoting it) then cannot be confused for the real field.
fn without_session_id(bytes: &[u8], session: &str) -> String {
    let text = String::from_utf8_lossy(bytes);
    let trimmed = text.trim_end();
    let field = format!("\"sessionId\":\"{session}\"");
    if !trimmed.ends_with('}') {
        return text.into_owned();
    }
    let body = &trimmed[..trimmed.len() - 1];
    let at = match body.trim_end().strip_suffix(&field) {
        Some(before) => before.len(),
        None => match body.rfind(&field) {
            Some(at) => at,
            None => return text.into_owned(),
        },
    };
    let before = body[..at].trim_end();
    let before = before.strip_suffix(',').unwrap_or(before);
    let after = body[at + field.len()..].trim_start();
    let after = after.strip_prefix(',').unwrap_or(after);
    format!("{before}{}{after}}}", if after.is_empty() || before.ends_with('{') { "" } else { "," })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    /// The three DevTools clients of one browser partition the id space: agent `1..=0x3FFF_FFFF`,
    /// shell `0x4000_0000..=0x6FFF_FFFF`, test surface `0x7000_0000..`. Disjoint by construction, not
    /// by "the counter would have to wrap half a billion times first".
    #[test]
    fn root_ids_stay_in_their_range() {
        assert_eq!(next_root_id(ROOT_ID_BASE), ROOT_ID_BASE + 1);
        assert_eq!(next_root_id(ROOT_ID_MAX), ROOT_ID_BASE, "the counter wraps inside the range");
        assert_eq!(next_root_id(ROOT_ID_MAX + 1), ROOT_ID_BASE, "the test surface's range is never entered");
        assert_eq!(next_root_id(i32::MAX - 1), ROOT_ID_BASE);
        assert_eq!(next_root_id(5), ROOT_ID_BASE);
        assert_eq!(next_root_id(crate::automation::cdp::MAX_ROOT_ID), ROOT_ID_BASE);
        const { assert!(crate::automation::cdp::MAX_ROOT_ID < ROOT_ID_BASE) };
        const { assert!(ROOT_ID_BASE < ROOT_ID_MAX && ROOT_ID_MAX < i32::MAX) };
        #[cfg(all(debug_assertions, feature = "test-hooks"))]
        const {
            assert!(crate::test_hooks::js::raw::ID_BASE == ROOT_ID_MAX + 1);
        };
    }

    #[test]
    fn replies_are_recognized_from_their_head() {
        assert!(is_own_reply(br#"{"id":1073741824,"result":{}}"#));
        assert!(is_own_reply(br#"{"id":1879048191,"result":{}}"#)); // ROOT_ID_MAX
        assert!(!is_own_reply(br#"{"id":1879048192,"result":{}}"#), "the test surface's first id");
        assert!(!is_own_reply(br#"{"id":2147483646,"result":{}}"#));
        assert!(!is_own_reply(br#"{"id":7,"result":{}}"#));
        assert!(!is_own_reply(br#"{"method":"Page.loadEventFired","params":{}}"#));
        assert!(!is_own_reply(br#"{"id":99999999999,"result":{}}"#));
    }

    #[test]
    fn method_lists_are_closed() {
        assert!(User::Debug.allows("Target.createTarget"));
        for denied in ["Runtime.evaluate", "Target.attachToTarget", "Browser.close", "Storage.getCookies", ""] {
            assert!(!User::Debug.allows(denied), "{denied}");
        }
    }

    /// The popup card's user may register its page script and watch service workers; the worker
    /// sessions themselves are only reachable through `worker_evaluate`, which sends one method.
    #[test]
    fn the_extensions_user_cannot_attach_or_navigate() {
        for allowed in ["Runtime.evaluate", "Page.enable", "Page.addScriptToEvaluateOnNewDocument", "Target.setAutoAttach", "Target.detachFromTarget"] {
            assert!(User::Extensions.allows(allowed), "{allowed}");
        }
        for denied in ["Target.attachToTarget", "Target.createTarget", "Page.navigate", "Network.getCookies", "Storage.getCookies", "Runtime.addBinding"] {
            assert!(!User::Extensions.allows(denied), "{denied}");
        }
    }

    #[test]
    fn a_worker_attach_is_recognized_from_its_head() {
        assert!(is_worker_attach(br#"{"method":"Target.attachedToTarget","params":{"sessionId":"AB","targetInfo":{"type":"service_worker"},"waitingForDebugger":false}}"#));
        assert!(!is_worker_attach(br#"{"method":"Target.detachedFromTarget","params":{"sessionId":"AB"}}"#));
        assert!(!is_worker_attach(br#"{"id":1073741824,"result":{"method":"Target.attachedToTarget"}}"#));
        // A session nobody watches for is never evaluated in.
        let answer = std::rc::Rc::new(RefCell::new(None));
        let seen = answer.clone();
        worker_evaluate(1, "AB", "1", 10, move |r| *seen.borrow_mut() = Some(r));
        assert_eq!(*answer.borrow(), Some(Err("unknown worker session".to_string())));
    }

    /// T2: the session is **appended**, and nothing inside the message is touched — the one
    /// nested-session method the frontend may send carries a `sessionId` in its own `params`.
    #[test]
    fn a_session_id_inside_params_survives_the_surgery() {
        let detach = r#"{"id":7,"method":"Target.detachFromTarget","params":{"sessionId":"90ACEA89"}}"#;
        assert_eq!(
            append_session_id(detach, "SSSS"),
            r#"{"id":7,"method":"Target.detachFromTarget","params":{"sessionId":"90ACEA89"},"sessionId":"SSSS"}"#
        );
        let flatten = r#"{"id":8,"method":"Target.detachFromTarget","params":{"sessionId":"90ACEA89","flatten":true}}"#;
        assert_eq!(
            append_session_id(flatten, "SSSS"),
            r#"{"id":8,"method":"Target.detachFromTarget","params":{"sessionId":"90ACEA89","flatten":true},"sessionId":"SSSS"}"#
        );
        let deep = r#"{"id":9,"method":"DOM.getDocument","params":{"depth":0,"x":{"sessionId":"ZZ"}}}"#;
        assert_eq!(append_session_id(deep, "S"), r#"{"id":9,"method":"DOM.getDocument","params":{"depth":0,"x":{"sessionId":"ZZ"}},"sessionId":"S"}"#);
        // Every result is still JSON, which is what the old rewrite stopped being.
        for raw in [detach, flatten, deep, "{}", r#"{"id":1}"#] {
            let out = append_session_id(raw, "SSSS");
            let parsed: Value = serde_json::from_str(&out).unwrap_or_else(|e| panic!("{out} is not JSON: {e}"));
            assert_eq!(parsed.get("sessionId").and_then(Value::as_str), Some("SSSS"), "{out}");
        }
        assert_eq!(append_session_id("{}", "S"), r#"{"sessionId":"S"}"#);
        assert_eq!(append_session_id("not json", "S"), "not json");
    }

    /// The other direction: a message on S reaches the frontend as its own session's.
    #[test]
    fn the_session_of_a_message_on_s_is_removed_from_its_tail() {
        let reply = r#"{"id":3,"result":{"value":1},"sessionId":"ABCD"}"#;
        assert_eq!(without_session_id(reply.as_bytes(), "ABCD"), r#"{"id":3,"result":{"value":1}}"#);
        let only = r#"{"sessionId":"ABCD"}"#;
        assert_eq!(without_session_id(only.as_bytes(), "ABCD"), "{}");
        // A body quoting the same text is not the field: the real one is the last key of the object.
        let quoting = r#"{"id":4,"result":{"text":"\"sessionId\":\"ABCD\""},"sessionId":"ABCD"}"#;
        let out = without_session_id(quoting.as_bytes(), "ABCD");
        assert!(out.ends_with(r#"}}"#), "{out}");
        assert!(serde_json::from_str::<Value>(&out).is_ok(), "{out}");
        // Another session's message is left exactly as it is (nested sessions keep theirs).
        let nested = r#"{"method":"Page.loadEventFired","params":{},"sessionId":"OTHER"}"#;
        assert_eq!(without_session_id(nested.as_bytes(), "ABCD"), nested);
    }

    /// T5: an oversize message is recognized from its head, so its caller can be answered.
    #[test]
    fn the_head_of_a_message_gives_up_its_id() {
        assert_eq!(head_id(br#"{"id":1073741824,"result":{}}"#), Some(1_073_741_824));
        assert_eq!(head_id(br#"{"id":5,"result":{}}"#), Some(5));
        assert_eq!(head_id(br#"{"method":"Page.loadEventFired","params":{}}"#), None);
        assert_eq!(head_id(b"{"), None);
    }

    /// The agent client (`src/automation/`) never uses the shell's DevTools client.
    #[test]
    fn automation_never_uses_this_client() {
        fn walk(dir: &Path, hits: &mut Vec<String>) {
            for entry in std::fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    walk(&path, hits);
                } else if path.extension().is_some_and(|e| e == "rs") {
                    let text = std::fs::read_to_string(&path).unwrap();
                    if text.contains("devtools_cdp") {
                        hits.push(path.display().to_string());
                    }
                }
            }
        }
        let mut hits = Vec::new();
        walk(&Path::new(env!("CARGO_MANIFEST_DIR")).join("src").join("automation"), &mut hits);
        assert!(hits.is_empty(), "automation/ names devtools_cdp: {hits:?}");
    }
}
