//! In-process DevTools protocol client, one per browser [owner: automation]
//! (docs/research/automation.md, docs/MCP.md "Threat model").
//!
//! The CEF DevTools session is **trusted** (Target in regular mode, local file read/write): the
//! only methods this module can send are the [`CdpMethod`] allowlist, each with a typed parameter
//! struct ([`Params`]). A unit test fails on any other quoted `Domain.method` string literal under
//! `src/automation/`. Never sent: `Target.*` (except `setAutoAttach`), `Browser.*`, `Storage.*`,
//! `Network.*`, `Fetch.*`, `Security.*`, `IO.*`, `Tracing.*`, `DOM.setFileInputFiles`,
//! `Input.dispatchDragEvent`, `Page.setDownloadBehavior`, `Page.addScriptToEvaluateOnNewDocument`,
//! `Runtime.enable`, `Debugger.*`, `Console.*`. Isolated worlds are always created with
//! `grantUniveralAccess: false`.
//!
//! Re-entrancy (CEF runs `OnDevToolsAgentAttached` and browser-side replies synchronously inside
//! the first send): the observer is registered before the first send and only copies bytes and
//! posts a UI task; no borrow is held across `send_dev_tools_message`. One id allocator per
//! browser; every call has its own timeout. On detach, pending calls fail with `Detached` and the
//! client generation changes (refs and isolated worlds of that generation are stale).
//!
//! Public API:
//! - `pub enum CdpMethod`, `pub trait Params`, `pub mod p` (parameter structs)
//! - `pub async fn call<P: Params>(browser_id: i32, params: P, timeout_ms: i64) -> Result<Value, CdpError>`
//! - `pub fn generation(browser_id: i32) -> u64`
//! - `pub fn on_browser_closed(browser_id: i32)`, `pub fn clear()`, `pub fn debug_snapshot()`
//! - debug builds: `pub async fn debug_raw(...)` (the `debug.cdp` spike request; any method)

use crate::automation::exec;
use crate::{browsers, task};
use cef::*;
use serde::Serialize;
use serde_json::{Value, json};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
#[cfg(debug_assertions)] // the event ring of `debug.cdpEvents` only
use std::collections::VecDeque;

/// Largest DevTools message accepted from the observer (screenshots, AX trees).
const MAX_MESSAGE_BYTES: usize = 64 * 1024 * 1024;
/// Largest root message id of this client. CEF shares one DevTools session per browser between
/// every in-process client and shows each client every message: the shell's other client (docked
/// DevTools, extensions backend, debug requests) uses ids from `0x4000_0000` up, so replies never
/// collide.
pub const MAX_ROOT_ID: i32 = 0x3FFF_FFFF;
/// How far from the end of a raw message a `"sessionId"` key is looked for (Chromium appends it
/// to flattened child-session messages).
const SESSION_TAIL_BYTES: usize = 256;

/// `true` for a message that belongs to a DevTools session this client didn't create (it creates
/// none): such messages are dropped before they are copied (another client's child sessions, e.g.
/// the docked DevTools frontend, reuse small ids like ours).
fn is_foreign_session_message(bytes: &[u8]) -> bool {
    let tail = &bytes[bytes.len().saturating_sub(SESSION_TAIL_BYTES)..];
    tail.windows(12).any(|w| w == b"\"sessionId\":")
}

/// `true` for a root reply whose id is above this client's range (read from the message head).
fn is_other_client_reply(bytes: &[u8]) -> bool {
    let head = &bytes[..bytes.len().min(32)];
    let Some(rest) = head.strip_prefix(b"{\"id\":") else { return false };
    let digits: Vec<u8> = rest.iter().copied().take_while(u8::is_ascii_digit).collect();
    std::str::from_utf8(&digits).ok().and_then(|d| d.parse::<i64>().ok()).is_some_and(|id| id > MAX_ROOT_ID as i64)
}

#[derive(Debug, Clone, PartialEq)]
pub enum CdpError {
    /// The browser is gone or has no host.
    NoBrowser,
    /// `send_dev_tools_message` refused the message (not on the UI thread / malformed).
    SendFailed,
    Timeout,
    /// The DevTools agent detached (navigation to a new process with a new agent, crash, close).
    Detached,
    /// A protocol error answer.
    Protocol { code: i64, message: String },
}

impl std::fmt::Display for CdpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CdpError::NoBrowser => write!(f, "browser is gone"),
            CdpError::SendFailed => write!(f, "DevTools message not accepted"),
            CdpError::Timeout => write!(f, "DevTools call timed out"),
            CdpError::Detached => write!(f, "DevTools agent detached"),
            CdpError::Protocol { code, message } => write!(f, "DevTools error {code}: {message}"),
        }
    }
}

// ----------------------------------------------------------------------------------- allowlist

/// Every DevTools method the automation module may send. Nothing else can be expressed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
///
/// This is the MVP subset of the plan's list: methods the MVP tools don't need
/// (`Page.enable`, `Page.handleJavaScriptDialog` — dialogs go through CEF's `JsdialogHandler` —,
/// `DOM.describeNode`, `Accessibility.enable/queryAXTree/getChildAXNodes`,
/// `Emulation.setFocusEmulationEnabled`, `Target.setAutoAttach`) are added with the tools that use
/// them.
pub enum CdpMethod {
    PageGetFrameTree,
    PageCreateIsolatedWorld,
    PageCaptureScreenshot,
    DomGetDocument,
    DomResolveNode,
    DomGetContentQuads,
    DomScrollIntoViewIfNeeded,
    DomGetNodeForLocation,
    DomFocus,
    AccessibilityDisable,
    AccessibilityGetFullAxTree,
    RuntimeCallFunctionOn,
    RuntimeReleaseObject,
    InputDispatchMouseEvent,
    InputDispatchKeyEvent,
    InputInsertText,
}

impl CdpMethod {
    #[cfg_attr(not(test), allow(dead_code))] // the allowlist tests
    pub const ALL: [CdpMethod; 16] = [
        CdpMethod::PageGetFrameTree,
        CdpMethod::PageCreateIsolatedWorld,
        CdpMethod::PageCaptureScreenshot,
        CdpMethod::DomGetDocument,
        CdpMethod::DomResolveNode,
        CdpMethod::DomGetContentQuads,
        CdpMethod::DomScrollIntoViewIfNeeded,
        CdpMethod::DomGetNodeForLocation,
        CdpMethod::DomFocus,
        CdpMethod::AccessibilityDisable,
        CdpMethod::AccessibilityGetFullAxTree,
        CdpMethod::RuntimeCallFunctionOn,
        CdpMethod::RuntimeReleaseObject,
        CdpMethod::InputDispatchMouseEvent,
        CdpMethod::InputDispatchKeyEvent,
        CdpMethod::InputInsertText,
    ];

    pub fn name(self) -> &'static str {
        match self {
            CdpMethod::PageGetFrameTree => "Page.getFrameTree",
            CdpMethod::PageCreateIsolatedWorld => "Page.createIsolatedWorld",
            CdpMethod::PageCaptureScreenshot => "Page.captureScreenshot",
            CdpMethod::DomGetDocument => "DOM.getDocument",
            CdpMethod::DomResolveNode => "DOM.resolveNode",
            CdpMethod::DomGetContentQuads => "DOM.getContentQuads",
            CdpMethod::DomScrollIntoViewIfNeeded => "DOM.scrollIntoViewIfNeeded",
            CdpMethod::DomGetNodeForLocation => "DOM.getNodeForLocation",
            CdpMethod::DomFocus => "DOM.focus",
            CdpMethod::AccessibilityDisable => "Accessibility.disable",
            CdpMethod::AccessibilityGetFullAxTree => "Accessibility.getFullAXTree",
            CdpMethod::RuntimeCallFunctionOn => "Runtime.callFunctionOn",
            CdpMethod::RuntimeReleaseObject => "Runtime.releaseObject",
            CdpMethod::InputDispatchMouseEvent => "Input.dispatchMouseEvent",
            CdpMethod::InputDispatchKeyEvent => "Input.dispatchKeyEvent",
            CdpMethod::InputInsertText => "Input.insertText",
        }
    }
}

/// Typed parameters of one allowlisted method.
pub trait Params: Serialize {
    const METHOD: CdpMethod;
}

/// Parameter structs, one per [`CdpMethod`].
pub mod p {
    use super::{CdpMethod, Params};
    use serde::Serialize;

    macro_rules! params {
        ($name:ident => $method:ident { $($(#[$attr:meta])* $field:ident : $ty:ty),* $(,)? }) => {
            #[derive(Debug, Clone, Serialize)]
            #[serde(rename_all = "camelCase")]
            pub struct $name { $($(#[$attr])* pub $field: $ty),* }
            impl Params for $name {
                const METHOD: CdpMethod = CdpMethod::$method;
            }
        };
    }

    params!(GetFrameTree => PageGetFrameTree {});
    params!(CaptureScreenshot => PageCaptureScreenshot {
        format: &'static str,
        #[serde(skip_serializing_if = "Option::is_none")] quality: Option<u32>,
        #[serde(skip_serializing_if = "Option::is_none")] clip: Option<Clip>,
        from_surface: bool,
        capture_beyond_viewport: bool,
    });
    params!(GetDocument => DomGetDocument { depth: i32 });
    params!(ResolveNode => DomResolveNode {
        backend_node_id: i64,
        #[serde(skip_serializing_if = "Option::is_none")] execution_context_id: Option<i64>,
    });
    params!(GetContentQuads => DomGetContentQuads { backend_node_id: i64 });
    params!(ScrollIntoViewIfNeeded => DomScrollIntoViewIfNeeded { backend_node_id: i64 });
    params!(GetNodeForLocation => DomGetNodeForLocation {
        x: i64,
        y: i64,
        include_user_agent_shadow_dom: bool,
        ignore_pointer_events_none: bool,
    });
    params!(Focus => DomFocus { backend_node_id: i64 });
    params!(AccessibilityDisable => AccessibilityDisable {});
    params!(GetFullAxTree => AccessibilityGetFullAxTree {
        #[serde(skip_serializing_if = "Option::is_none")] depth: Option<i32>,
    });
    params!(CallFunctionOn => RuntimeCallFunctionOn {
        function_declaration: String,
        object_id: String,
        arguments: Vec<CallArgument>,
        return_by_value: bool,
        await_promise: bool,
        silent: bool,
    });
    params!(ReleaseObject => RuntimeReleaseObject { object_id: String });
    params!(DispatchMouseEvent => InputDispatchMouseEvent {
        #[serde(rename = "type")] kind: &'static str,
        x: f64,
        y: f64,
        button: &'static str,
        buttons: i32,
        click_count: i32,
        modifiers: i32,
    });
    params!(DispatchKeyEvent => InputDispatchKeyEvent {
        #[serde(rename = "type")] kind: &'static str,
        modifiers: i32,
        key: String,
        code: String,
        windows_virtual_key_code: i32,
        #[serde(skip_serializing_if = "Option::is_none")] text: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")] unmodified_text: Option<String>,
    });
    params!(InsertText => InputInsertText { text: String });

    /// `Page.createIsolatedWorld`: `grantUniveralAccess` is always sent as `false` (the field is
    /// not settable).
    #[derive(Debug, Clone)]
    pub struct CreateIsolatedWorld {
        pub frame_id: String,
        pub world_name: String,
    }

    impl Serialize for CreateIsolatedWorld {
        fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
            #[derive(Serialize)]
            #[serde(rename_all = "camelCase")]
            struct Wire<'a> {
                frame_id: &'a str,
                world_name: &'a str,
                // Chromium's own (misspelled) parameter name.
                #[serde(rename = "grantUniveralAccess")]
                grant_univeral_access: bool,
            }
            Wire { frame_id: &self.frame_id, world_name: &self.world_name, grant_univeral_access: false }.serialize(s)
        }
    }

    impl Params for CreateIsolatedWorld {
        const METHOD: CdpMethod = CdpMethod::PageCreateIsolatedWorld;
    }

    #[derive(Debug, Clone, Serialize)]
    pub struct Clip {
        pub x: f64,
        pub y: f64,
        pub width: f64,
        pub height: f64,
        pub scale: f64,
    }

    /// A `Runtime.CallArgument`: an object id from the same world, or a JSON value.
    #[derive(Debug, Clone, Serialize)]
    #[serde(rename_all = "camelCase")]
    pub struct CallArgument {
        #[serde(skip_serializing_if = "Option::is_none")]
        pub object_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub value: Option<serde_json::Value>,
    }
}

// ----------------------------------------------------------------------------------- clients

struct Client {
    /// Keeps the observer registered (dropped with the client).
    _registration: Option<Registration>,
    next_id: i32,
    pending: HashMap<i32, exec::Sender<Result<Value, CdpError>>>,
    /// Bumped on every agent detach.
    generation: u64,
    attached: bool,
    #[cfg(debug_assertions)]
    events: VecDeque<Value>,
}

thread_local! {
    static CLIENTS: RefCell<HashMap<i32, Client>> = RefCell::new(HashMap::new());
    /// `send_dev_tools_message` is running (spike: observer callbacks inside the send).
    static IN_SEND: Cell<bool> = const { Cell::new(false) };
    static SYNC_CALLBACKS: Cell<u64> = const { Cell::new(0) };
}

wrap_dev_tools_message_observer! {
    struct Observer {
        browser_id: i32,
    }

    impl DevToolsMessageObserver {
        fn on_dev_tools_message(&self, _browser: Option<&mut Browser>, message: Option<&[u8]>) -> i32 {
            // Copy and post: this can run inside `send_dev_tools_message`.
            if IN_SEND.get() {
                SYNC_CALLBACKS.set(SYNC_CALLBACKS.get() + 1);
            }
            let Some(bytes) = message else { return 1 };
            // Not ours: another client's child session or root reply. Checked on the raw bytes,
            // before anything is copied.
            if is_foreign_session_message(bytes) || is_other_client_reply(bytes) {
                return 1;
            }
            if bytes.len() > MAX_MESSAGE_BYTES {
                log_warn!("automation: DevTools message of {} bytes dropped (browser {})", bytes.len(), self.browser_id);
                return 1;
            }
            let (id, bytes) = (self.browser_id, bytes.to_vec());
            task::post_ui(move || deliver(id, &bytes));
            1
        }

        fn on_dev_tools_agent_attached(&self, _browser: Option<&mut Browser>) {
            if IN_SEND.get() {
                SYNC_CALLBACKS.set(SYNC_CALLBACKS.get() + 1);
            }
            let id = self.browser_id;
            task::post_ui(move || set_attached(id, true));
        }

        fn on_dev_tools_agent_detached(&self, _browser: Option<&mut Browser>) {
            let id = self.browser_id;
            task::post_ui(move || set_attached(id, false));
        }
    }
}

/// Registers the observer for `browser_id` if needed and returns its host.
fn ensure_client(browser_id: i32) -> Option<BrowserHost> {
    let host = browsers::browser(browser_id)?.host()?;
    if CLIENTS.with(|c| c.borrow().contains_key(&browser_id)) {
        return Some(host);
    }
    let mut observer = Observer::new(browser_id);
    let registration = host.add_dev_tools_message_observer(Some(&mut observer));
    if registration.is_none() {
        log_warn!("automation: no DevTools observer registration for browser {browser_id}");
        return None;
    }
    CLIENTS.with(|c| {
        c.borrow_mut().insert(
            browser_id,
            Client {
                _registration: registration,
                next_id: 1,
                pending: HashMap::new(),
                generation: 1,
                attached: false,
                #[cfg(debug_assertions)]
                events: VecDeque::new(),
            },
        )
    });
    Some(host)
}

fn set_attached(browser_id: i32, attached: bool) {
    let failed = CLIENTS.with(|c| {
        let mut c = c.borrow_mut();
        let client = c.get_mut(&browser_id)?;
        let was = client.attached;
        client.attached = attached;
        if attached || !was {
            return None;
        }
        client.generation += 1;
        Some(std::mem::take(&mut client.pending))
    });
    if let Some(pending) = failed {
        log_debug!("automation: DevTools agent of browser {browser_id} detached ({} pending)", pending.len());
        for (_, tx) in pending {
            tx.send(Err(CdpError::Detached));
        }
        crate::automation::on_agent_detached(browser_id);
    }
}

fn deliver(browser_id: i32, bytes: &[u8]) {
    let Ok(msg) = serde_json::from_slice::<Value>(bytes) else {
        log_warn!("automation: unparsable DevTools message ({} bytes)", bytes.len());
        return;
    };
    if let Some(id) = msg.get("id").and_then(Value::as_i64) {
        let tx = CLIENTS.with(|c| c.borrow_mut().get_mut(&browser_id).and_then(|cl| cl.pending.remove(&(id as i32))));
        let Some(tx) = tx else { return };
        let result = match msg.get("error") {
            Some(err) => Err(CdpError::Protocol {
                code: err.get("code").and_then(Value::as_i64).unwrap_or(0),
                message: err.get("message").and_then(Value::as_str).unwrap_or_default().to_string(),
            }),
            None => Ok(msg.get("result").cloned().unwrap_or(Value::Null)),
        };
        tx.send(result);
        return;
    }
    // Events: the MVP tools subscribe to none; debug builds keep the last ones for `debug.cdpEvents`.
    #[cfg(debug_assertions)]
    if let Some(method) = msg.get("method").and_then(Value::as_str) {
        let mut text = msg.get("params").map(Value::to_string).unwrap_or_default();
        if text.len() > 2000 {
            text.truncate(text.floor_char_boundary(2000));
        }
        CLIENTS.with(|c| {
            if let Some(cl) = c.borrow_mut().get_mut(&browser_id) {
                cl.events.push_back(json!({ "method": method, "params": text }));
                while cl.events.len() > 200 {
                    cl.events.pop_front();
                }
            }
        });
    }
}

async fn send_message(browser_id: i32, method: &str, params: Value, session_id: Option<&str>, timeout_ms: i64) -> Result<Value, CdpError> {
    let host = ensure_client(browser_id).ok_or(CdpError::NoBrowser)?;
    let (tx, rx) = exec::oneshot();
    let id = CLIENTS
        .with(|c| {
            let mut c = c.borrow_mut();
            let client = c.get_mut(&browser_id)?;
            let id = client.next_id;
            client.next_id = next_id_after(client.next_id);
            client.pending.insert(id, tx);
            Some(id)
        })
        .ok_or(CdpError::NoBrowser)?;
    let mut message = json!({ "id": id, "method": method, "params": params });
    if let Some(session) = session_id {
        message["sessionId"] = Value::String(session.to_string());
    }
    let bytes = serde_json::to_vec(&message).map_err(|_| CdpError::SendFailed)?;
    // No borrow is held here: observer callbacks may run inside this call.
    IN_SEND.set(true);
    let sent = host.send_dev_tools_message(Some(&bytes));
    IN_SEND.set(false);
    drop(host);
    if sent == 0 {
        let tx = CLIENTS.with(|c| c.borrow_mut().get_mut(&browser_id).and_then(|cl| cl.pending.remove(&id)));
        drop(tx);
        return Err(CdpError::SendFailed);
    }
    match exec::timeout(timeout_ms, rx).await {
        Some(Some(result)) => result,
        Some(None) => Err(CdpError::Detached),
        None => {
            let tx = CLIENTS.with(|c| c.borrow_mut().get_mut(&browser_id).and_then(|cl| cl.pending.remove(&id)));
            drop(tx);
            Err(CdpError::Timeout)
        }
    }
}

/// The id after `id`, wrapping inside `1..=MAX_ROOT_ID`.
fn next_id_after(id: i32) -> i32 {
    if !(1..MAX_ROOT_ID).contains(&id) { 1 } else { id + 1 }
}

/// Whether an agent may drive this browser at all (FINAL PLAN §1, "MCP rules"): **tabs only**. A UI
/// surface, a DevTools frontend, or one of the browsers Chromium creates for extensions (`foreign.rs`,
/// registered through `browsers::extra_*` and so never given a role) is refused before anything is
/// sent. Tested both ways by `only_tab_browsers_may_be_driven`.
fn may_be_driven(browser_id: i32) -> bool {
    matches!(browsers::role_of(browser_id), Some(browsers::Role::Tab(_)))
}

/// Sends one allowlisted method and waits for its result (at most `timeout_ms`). Only tab browsers:
/// UI surfaces, Chrome-created browsers and DevTools frontends are refused (`NoBrowser`).
pub async fn call<P: Params>(browser_id: i32, params: P, timeout_ms: i64) -> Result<Value, CdpError> {
    if !may_be_driven(browser_id) {
        log_warn!("automation: DevTools call {} refused for browser {browser_id} (not a tab)", P::METHOD.name());
        return Err(CdpError::NoBrowser);
    }
    let value = serde_json::to_value(&params).map_err(|_| CdpError::SendFailed)?;
    send_message(browser_id, P::METHOD.name(), value, None, timeout_ms).await
}

/// Debug builds only (`debug.cdp`, the Phase 0 spike): any method, any params. Only reachable
/// through the debug IPC of a trusted UI page in a debug build.
#[cfg(debug_assertions)]
pub async fn debug_raw(browser_id: i32, method: String, params: Value, session_id: Option<String>, timeout_ms: i64) -> Result<Value, CdpError> {
    send_message(browser_id, &method, params, session_id.as_deref(), timeout_ms).await
}

/// Recent events of a browser (debug builds; newest last).
#[cfg(debug_assertions)]
pub fn debug_events(browser_id: i32, clear: bool) -> Vec<Value> {
    CLIENTS.with(|c| {
        let mut c = c.borrow_mut();
        let Some(cl) = c.get_mut(&browser_id) else { return Vec::new() };
        let list: Vec<Value> = cl.events.iter().cloned().collect();
        if clear {
            cl.events.clear();
        }
        list
    })
}

/// Client generation of a browser (0 = no client yet). Changes when the agent detaches.
pub fn generation(browser_id: i32) -> u64 {
    CLIENTS.with(|c| c.borrow().get(&browser_id).map_or(0, |cl| cl.generation))
}

/// `on_before_close`: fail pending calls and drop the registration (posted, outside the callback).
pub fn on_browser_closed(browser_id: i32) {
    let client = CLIENTS.with(|c| c.borrow_mut().remove(&browser_id));
    if let Some(mut client) = client {
        let pending = std::mem::take(&mut client.pending);
        for (_, tx) in pending {
            tx.send(Err(CdpError::NoBrowser));
        }
        task::post_ui(move || drop(client));
    }
}

/// Drops every registration (before `cef::shutdown`).
pub fn clear() {
    let clients = CLIENTS.with(|c| std::mem::take(&mut *c.borrow_mut()));
    drop(clients);
}

pub fn debug_snapshot() -> Value {
    let clients: Vec<Value> = CLIENTS.with(|c| {
        c.borrow()
            .iter()
            .map(|(id, cl)| json!({ "browser": id, "attached": cl.attached, "pending": cl.pending.len(), "generation": cl.generation }))
            .collect()
    });
    json!({ "clients": clients, "syncCallbacks": SYNC_CALLBACKS.get() })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    /// Every `"Domain.method"` string literal in `src/automation/`.
    fn method_literals(dir: &Path, out: &mut Vec<(String, String)>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                method_literals(&path, out);
                continue;
            }
            if path.extension().and_then(|e| e.to_str()) != Some("rs") && path.extension().and_then(|e| e.to_str()) != Some("js") {
                continue;
            }
            let text = std::fs::read_to_string(&path).unwrap();
            // Test modules may name denied methods on purpose.
            let text = text.split("#[cfg(test)]\nmod tests").next().unwrap_or_default();
            for (i, _) in text.match_indices('"') {
                let rest = &text[i + 1..];
                let Some(end) = rest.find('"') else { continue };
                let lit = &rest[..end];
                let Some((domain, method)) = lit.split_once('.') else { continue };
                let domain_ok = domain.len() >= 2
                    && domain.chars().next().is_some_and(|c| c.is_ascii_uppercase())
                    && domain.chars().all(|c| c.is_ascii_alphanumeric());
                let method_ok = !method.is_empty()
                    && method.chars().next().is_some_and(|c| c.is_ascii_lowercase())
                    && method.chars().all(|c| c.is_ascii_alphanumeric());
                if domain_ok && method_ok {
                    out.push((path.display().to_string(), lit.to_string()));
                }
            }
        }
    }

    #[test]
    fn only_allowlisted_methods_appear_in_automation_sources() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src").join("automation");
        let mut found = Vec::new();
        method_literals(&dir, &mut found);
        assert!(found.iter().any(|(_, m)| m == "DOM.resolveNode"), "scanner found nothing: {found:?}");
        let allowed: Vec<&str> = CdpMethod::ALL.iter().map(|m| m.name()).collect();
        // Rust paths that look like method names (`Domain.method` only in CDP casing).
        let bad: Vec<_> = found.iter().filter(|(_, m)| !allowed.contains(&m.as_str())).collect();
        assert!(bad.is_empty(), "DevTools methods outside the allowlist: {bad:?}");
    }

    #[test]
    fn denied_methods_are_not_expressible() {
        let names: Vec<&str> = CdpMethod::ALL.iter().map(|m| m.name()).collect();
        for denied in [
            "Target.attachToTarget",
            "Target.getTargets",
            "Browser.setDownloadBehavior",
            "Network.getAllCookies",
            "Fetch.enable",
            "Security.setIgnoreCertificateErrors",
            "DOM.setFileInputFiles",
            "Input.dispatchDragEvent",
            "Page.setDownloadBehavior",
            "Page.addScriptToEvaluateOnNewDocument",
            "Page.handleJavaScriptDialog",
            "Runtime.enable",
            "Runtime.evaluate",
            "Debugger.enable",
            "Storage.getCookies",
            "IO.read",
            "Tracing.start",
        ] {
            assert!(!names.contains(&denied), "{denied} must not be allowlisted");
        }
        let unique: std::collections::HashSet<_> = names.iter().collect();
        assert_eq!(unique.len(), names.len());
    }

    #[test]
    fn ids_stay_in_the_agent_range_and_other_sessions_are_dropped() {
        assert_eq!(next_id_after(1), 2);
        assert_eq!(next_id_after(MAX_ROOT_ID - 1), MAX_ROOT_ID);
        assert_eq!(next_id_after(MAX_ROOT_ID), 1);
        assert_eq!(next_id_after(i32::MAX - 1), 1);
        assert_eq!(next_id_after(0), 1);
        assert_eq!(MAX_ROOT_ID + 1, 0x4000_0000);
        // A child-session reply with an id this client also uses (the docked DevTools frontend).
        let child = br#"{"id":3,"result":{"root":{"nodeId":1}},"sessionId":"8C2B"}"#;
        assert!(is_foreign_session_message(child));
        let mut big = br#"{"id":4,"result":{"data":""#.to_vec();
        big.extend(std::iter::repeat_n(b'A', 100_000));
        big.extend_from_slice(br#""},"sessionId":"E1"}"#);
        assert!(is_foreign_session_message(&big));
        assert!(!is_foreign_session_message(br#"{"id":3,"result":{}}"#));
        assert!(!is_foreign_session_message(br#"{"method":"Page.frameNavigated","params":{"frame":{}}}"#));
        // The shell client's replies.
        assert!(is_other_client_reply(br#"{"id":1073741824,"result":{}}"#));
        assert!(!is_other_client_reply(br#"{"id":1073741823,"result":{}}"#));
        assert!(!is_other_client_reply(br#"{"method":"X.y","params":{}}"#));
    }

    /// FINAL PLAN §1: *"`cdp::call` refuses non-Tab roles"* — the rule that keeps an agent off sta's
    /// own UI surfaces, off DevTools frontends and off the Chrome-created browsers phase 1 introduces
    /// (`foreign.rs` never gives those a role).
    #[test]
    fn only_tab_browsers_may_be_driven() {
        use crate::browsers::{self, Role, Surface};
        // Ids of this test only; `browsers` is thread-local and the runtime never sees them.
        assert!(!may_be_driven(9401), "a browser with no role (a Chrome-created window) is refused");
        for surface in [Surface::Topbar, Surface::CommandBar, Surface::Agent, Surface::Toast] {
            browsers::set_role(9402, Role::Surface(surface));
            assert!(!may_be_driven(9402), "{surface:?} is refused");
        }
        browsers::set_role(9403, Role::Tab(7));
        assert!(may_be_driven(9403), "a tab is allowed");
        // …and `call` is the only door: it must ask before sending anything.
        let source = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src").join("automation").join("cdp.rs")).unwrap();
        // The definition: at the start of a line, unlike the module doc's copy of the signature and
        // this test's own mention of it.
        let at = source.rfind("\npub async fn call<P: Params>(").expect("call()");
        let body = &source[at..];
        let guard = body.find("may_be_driven(browser_id)").expect("call() must consult may_be_driven");
        let send = body.find("send_message(").expect("call() sends");
        assert!(guard < send, "the role check must come before the send");
    }

    #[test]
    fn isolated_worlds_never_get_universal_access() {
        let v = serde_json::to_value(p::CreateIsolatedWorld { frame_id: "F".into(), world_name: "sta-agent".into() }).unwrap();
        assert_eq!(v, json!({ "frameId": "F", "worldName": "sta-agent", "grantUniveralAccess": false }));
        assert_eq!(<p::CreateIsolatedWorld as Params>::METHOD.name(), "Page.createIsolatedWorld");
    }

    #[test]
    fn params_serialize_camel_case() {
        let v = serde_json::to_value(p::DispatchMouseEvent { kind: "mousePressed", x: 1.5, y: 2.0, button: "left", buttons: 1, click_count: 1, modifiers: 0 }).unwrap();
        assert_eq!(v, json!({ "type": "mousePressed", "x": 1.5, "y": 2.0, "button": "left", "buttons": 1, "clickCount": 1, "modifiers": 0 }));
        let v = serde_json::to_value(p::ResolveNode { backend_node_id: 7, execution_context_id: None }).unwrap();
        assert_eq!(v, json!({ "backendNodeId": 7 }));
    }
}
