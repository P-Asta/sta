//! JavaScript, DevTools and target tools of the test surface (buckets B and E of the design).
//!
//! `test_eval` / `test_invoke` run in **any** target — sta's own `sta://` surfaces included, which
//! the shipped `evaluate` tool refuses by design (`internal_page`) — and `test_cdp` sends any
//! DevTools method, the way `debug.cdp` does today.
//!
//! The raw client at the bottom is a third in-process DevTools client, next to the agent's
//! (`automation/cdp.rs`, root ids `1..=0x3FFF_FFFF`, drops everything with a `sessionId`) and the
//! shell's (`devtools_cdp.rs`, root ids from `0x4000_0000`, also drops `sessionId` traffic). It
//! takes ids from `0x7000_0000` and is the only one that **keeps** session traffic, so
//! `Target.attachToTarget` + a flattened session (an extension's service worker) works over MCP at
//! all. Both other clients ignore its replies: ids above their range, or a `sessionId` they never
//! created.

use super::{bool_arg, err, invalid, opt_str, out, str_arg, timeout_arg};
use crate::automation::tools::Output;
use crate::{browsers, tabs};
use cef::{CefString, ImplBrowser, ImplFrame};
use serde_json::{Value, json};
use sta_core::agent::ErrorCode;
use sta_core::agent::channel::ToolError;

/// A resolved target: which browser's DevTools session carries the message, and the flattened
/// child session inside it, if any.
struct Target {
    browser: i32,
    session: Option<String>,
    what: String,
}

fn browser_url(browser_id: i32) -> String {
    browsers::browser(browser_id)
        .and_then(|b| ImplBrowser::main_frame(&b))
        .map(|f| CefString::from(&ImplFrame::url(&f)).to_string())
        .unwrap_or_default()
}

fn role_of(browser_id: i32) -> String {
    match browsers::role_of(browser_id) {
        Some(browsers::Role::Surface(s)) => format!("surface:{}", s.host()),
        Some(browsers::Role::Tab(t)) => format!("tab:{t}"),
        Some(browsers::Role::DevTools { tab }) => format!("devtools:{tab}"),
        Some(browsers::Role::ExtensionPopup) => "extensionPopup".into(),
        None => "other".into(),
    }
}

/// `{surface} | {tab} | {browser} | {targetId} | {match}` (+ an explicit `sessionId`).
fn resolve(args: &Value) -> Result<Target, ToolError> {
    let selector = args.get("target").cloned().unwrap_or_else(|| json!({}));
    let session = opt_str(args, "sessionId");
    if let Some(id) = selector.get("browser").and_then(Value::as_i64) {
        // A CEF browser identifier straight from `test_targets` — the only way to address a browser
        // that is neither a tab nor a surface (a DevTools window, a Chrome-created popup).
        let id = id as i32;
        browsers::browser(id).ok_or_else(|| err(ErrorCode::NoSuchTarget, format!("no live browser {id}")))?;
        return Ok(Target { browser: id, session, what: format!("browser {id}") });
    }
    if let Some(host) = selector.get("surface").and_then(Value::as_str) {
        let surface = browsers::Surface::from_host(host).ok_or_else(|| invalid(format!("unknown surface {host:?}")))?;
        let browser = browsers::surface_browser(surface).ok_or_else(|| err(ErrorCode::NoSuchTarget, format!("surface {host} has no browser")))?;
        return Ok(Target { browser: browser.identifier(), session, what: format!("surface {host}") });
    }
    if let Some(tab) = selector.get("tab").and_then(Value::as_u64) {
        let browser = tabs::browser_for_tab(tab).ok_or_else(|| err(ErrorCode::NoSuchTarget, format!("tab {tab} has no browser")))?;
        return Ok(Target { browser: browser.identifier(), session, what: format!("tab {tab}") });
    }
    if let Some(id) = selector.get("targetId").and_then(Value::as_str) {
        // Only a target `test_attach` opened a session for, or one the caller names a `sessionId`
        // for. There used to be a fallback to the shell's own page here, which made a *wrong* id
        // answer from `sta://topbar/` instead of failing — a check that asserts an absence in such a
        // target would then pass while measuring the shell (pre-MCP a socket to a dead target simply
        // failed to open). `test_attach` does not come through here: it sends
        // `Target.attachToTarget` on the shell's client itself.
        let known = raw::attached(id);
        let Some((browser, remembered)) = known else {
            return match session {
                Some(session) => Ok(Target { browser: super::shell_browser()?, session: Some(session), what: format!("target {id}") }),
                None => Err(err(ErrorCode::NoSuchTarget, format!("no session for target {id}: call test_attach first"))),
            };
        };
        return Ok(Target { browser, session: session.or(Some(remembered)), what: format!("target {id}") });
    }
    if let Some(needle) = selector.get("match").and_then(Value::as_str) {
        let found = browsers::live_browsers()
            .into_iter()
            .map(|b| b.identifier())
            .find(|id| browser_url(*id).contains(needle))
            .ok_or_else(|| err(ErrorCode::NoSuchTarget, format!("no target whose URL contains {needle:?}")))?;
        return Ok(Target { browser: found, session, what: format!("match {needle:?}") });
    }
    Err(invalid("target takes one of surface, tab, targetId or match"))
}

async fn evaluate(target: &Target, expression: String, args: &Value) -> Result<Value, ToolError> {
    let params = json!({
        "expression": expression,
        "awaitPromise": bool_arg(args, "awaitPromise", true),
        "returnByValue": bool_arg(args, "returnByValue", true),
        "userGesture": bool_arg(args, "userGesture", false),
    });
    raw::call(target.browser, "Runtime.evaluate", params, target.session.clone(), timeout_arg(args))
        .await
        .map_err(|e| err(ErrorCode::Internal, format!("Runtime.evaluate in {} failed: {e}", target.what)))
}

/// `test_eval`: `{value}`, or `{error: {text, stack}}` when the expression threw.
pub async fn eval(args: &Value) -> Result<Output, ToolError> {
    let target = resolve(args)?;
    let expression = str_arg(args, "expression")?;
    let result = evaluate(&target, expression, args).await?;
    Ok(out(format!("evaluated in {}", target.what), shape_evaluate(&result)))
}

fn shape_evaluate(result: &Value) -> Value {
    // `undefined` has no `value` in a CDP `returnByValue` result, and a caller that compares against
    // `undefined` must keep seeing it: leave the key out rather than turning it into `null`.
    if result.get("exceptionDetails").is_none() && result.pointer("/result/type").and_then(Value::as_str) == Some("undefined") {
        return json!({});
    }
    if let Some(details) = result.get("exceptionDetails") {
        let text = details
            .pointer("/exception/description")
            .and_then(Value::as_str)
            .or_else(|| details.get("text").and_then(Value::as_str))
            .unwrap_or("uncaught exception");
        return json!({ "error": { "text": text, "stack": details.get("stackTrace").cloned().unwrap_or(Value::Null) } });
    }
    json!({ "value": result.pointer("/result/value").cloned().unwrap_or(Value::Null) })
}

/// `test_invoke`: a **real** `window.sta.invoke(cmd, payload)` inside the target's own frame, so
/// the trusted-frame check, the CEF message router and the surface's `window.sta` shim stay
/// covered (design §10.2).
pub async fn invoke(args: &Value) -> Result<Output, ToolError> {
    let target = resolve(args)?;
    let cmd = str_arg(args, "cmd")?;
    let payload = args.get("payload").cloned().unwrap_or(Value::Null);
    let expression = format!(
        "window.sta.invoke({}, {}).then(function (r) {{ return {{ ok: r }}; }}, function (e) {{ return {{ err: e.code, msg: e.message }}; }})",
        serde_json::to_string(&cmd).unwrap_or_default(),
        serde_json::to_string(&payload).unwrap_or_else(|_| "null".into())
    );
    let result = evaluate(&target, expression, args).await?;
    let shaped = shape_evaluate(&result);
    if let Some(error) = shaped.get("error") {
        return Err(err(ErrorCode::ScriptError, format!("window.sta.invoke({cmd}) failed in {}: {error}", target.what)));
    }
    Ok(out(format!("{cmd} in {}", target.what), shaped.get("value").cloned().unwrap_or(Value::Null)))
}

/// `test_targets`: sta's own browsers, and (by default) what `Target.getTargets` adds — extension
/// service workers, DevTools frontends, Chrome-created pages.
pub async fn targets(args: &Value) -> Result<Output, ToolError> {
    let mut list: Vec<Value> = browsers::live_browsers()
        .into_iter()
        .map(|b| b.identifier())
        .map(|id| json!({ "id": format!("browser:{id}"), "browser": id, "type": "page", "url": browser_url(id), "role": role_of(id) }))
        .collect();
    if bool_arg(args, "cdp", true) {
        let shell = super::shell_browser()?;
        let discovered = raw::call(shell, "Target.getTargets", json!({}), None, timeout_arg(args)).await;
        if let Ok(value) = discovered
            && let Some(infos) = value.get("targetInfos").and_then(Value::as_array)
        {
            for info in infos {
                let url = info.get("url").and_then(Value::as_str).unwrap_or_default();
                list.push(json!({
                    "id": info.get("targetId").cloned().unwrap_or(Value::Null),
                    "targetId": info.get("targetId").cloned().unwrap_or(Value::Null),
                    "browser": Value::Null,
                    "type": info.get("type").cloned().unwrap_or(Value::Null),
                    "url": url,
                    "title": info.get("title").cloned().unwrap_or(Value::Null),
                    "role": "cdp",
                }));
            }
        }
    }
    Ok(out(format!("{} target(s)", list.len()), json!(list)))
}

/// `test_cdp`: any method on any target — `{result, ms}` or `{error, ms}`, exactly the shape
/// `debug.cdp` answers with today.
pub async fn cdp(args: &Value) -> Result<Output, ToolError> {
    let target = resolve(args)?;
    let method = str_arg(args, "method")?;
    let params = args.get("params").cloned().filter(|p| !p.is_null()).unwrap_or_else(|| json!({}));
    let started = std::time::Instant::now();
    let result = raw::call(target.browser, &method, params, target.session.clone(), timeout_arg(args)).await;
    let ms = started.elapsed().as_millis() as u64;
    let structured = match result {
        Ok(value) => json!({ "result": value, "ms": ms }),
        Err(e) => json!({ "error": e, "ms": ms }),
    };
    Ok(out(format!("{method} on {} ({ms} ms)", target.what), structured))
}

/// `test_cdp_events`: the test client's own ring buffer, falling back to the agent client's (what
/// `debug.cdpEvents` reads) while only that one is attached.
pub async fn cdp_events(args: &Value) -> Result<Output, ToolError> {
    let target = resolve(args)?;
    let clear = bool_arg(args, "clear", false);
    let mut events = raw::events(target.browser, clear);
    if events.is_empty() {
        events = crate::automation::cdp::debug_events(target.browser, clear);
    }
    Ok(out(format!("{} event(s) of {}", events.len(), target.what), json!({ "events": events })))
}

/// `test_attach`: `Target.attachToTarget` with a flattened session; the sessionId is remembered for
/// `{target: {targetId}}` selectors.
pub async fn attach(args: &Value) -> Result<Output, ToolError> {
    let target_id = str_arg(args, "targetId")?;
    let on = match args.get("target") {
        Some(t) if !t.is_null() => resolve(args)?.browser,
        _ => super::shell_browser()?,
    };
    let value = raw::call(on, "Target.attachToTarget", json!({ "targetId": target_id, "flatten": true }), None, timeout_arg(args))
        .await
        .map_err(|e| err(ErrorCode::NoSuchTarget, format!("Target.attachToTarget({target_id}) failed: {e}")))?;
    let session = value
        .get("sessionId")
        .and_then(Value::as_str)
        .ok_or_else(|| err(ErrorCode::Internal, format!("Target.attachToTarget({target_id}) returned no sessionId")))?
        .to_string();
    raw::remember(&target_id, on, &session);
    Ok(out(format!("attached to {target_id}"), json!({ "sessionId": session, "targetId": target_id, "browser": on })))
}

// ----------------------------------------------------------------------------------- raw client

pub mod raw {
    use crate::automation::exec;
    use crate::task;
    use cef::*;
    use serde_json::{Value, json};
    use std::cell::RefCell;
    use std::collections::{HashMap, VecDeque};

    /// Message ids of this client: above the agent client's range (`..=0x3FFF_FFFF`) and above the
    /// shell client's (`0x4000_0000..=0x6FFF_FFFF`), so the three id spaces **partition** `1..i32::MAX`
    /// instead of overlapping. `devtools_cdp::root_ids_stay_in_their_range` asserts it.
    pub(crate) const ID_BASE: i32 = crate::devtools_cdp::ROOT_ID_MAX + 1;
    const ID_MAX: i32 = i32::MAX - 1;
    const MAX_EVENTS: usize = 200;

    struct Client {
        _registration: Option<Registration>,
        next_id: i32,
        pending: HashMap<i32, exec::Sender<Result<Value, String>>>,
        events: VecDeque<Value>,
    }

    thread_local! {
        static CLIENTS: RefCell<HashMap<i32, Client>> = RefCell::new(HashMap::new());
        /// targetId → (browser whose session attached, sessionId).
        static SESSIONS: RefCell<HashMap<String, (i32, String)>> = RefCell::new(HashMap::new());
    }

    pub fn remember(target_id: &str, browser: i32, session: &str) {
        SESSIONS.with(|s| s.borrow_mut().insert(target_id.to_string(), (browser, session.to_string())));
    }

    pub fn attached(target_id: &str) -> Option<(i32, String)> {
        SESSIONS.with(|s| s.borrow().get(target_id).cloned())
    }

    wrap_dev_tools_message_observer! {
        struct Observer {
            browser_id: i32,
        }

        impl DevToolsMessageObserver {
            fn on_dev_tools_message(&self, _browser: Option<&mut Browser>, message: Option<&[u8]>) -> i32 {
                let Some(bytes) = message else { return 1 };
                if bytes.len() > 64 * 1024 * 1024 {
                    return 1;
                }
                let head = &bytes[..bytes.len().min(32)];
                let ours = head
                    .strip_prefix(b"{\"id\":")
                    .map(|rest| rest.iter().copied().take_while(u8::is_ascii_digit).collect::<Vec<u8>>())
                    .and_then(|d| String::from_utf8(d).ok())
                    .and_then(|d| d.parse::<i64>().ok())
                    .is_some_and(|id| (ID_BASE as i64..=ID_MAX as i64).contains(&id));
                let is_event = !head.starts_with(b"{\"id\":");
                if !ours && !is_event {
                    return 1;
                }
                let (id, bytes) = (self.browser_id, bytes.to_vec());
                task::post_ui(move || deliver(id, &bytes));
                1
            }

            fn on_dev_tools_agent_detached(&self, _browser: Option<&mut Browser>) {
                let id = self.browser_id;
                task::post_ui(move || fail_all(id, "DevTools agent detached"));
            }
        }
    }

    fn deliver(browser_id: i32, bytes: &[u8]) {
        let Ok(msg) = serde_json::from_slice::<Value>(bytes) else { return };
        if let Some(id) = msg.get("id").and_then(Value::as_i64) {
            let tx = CLIENTS.with(|c| c.borrow_mut().get_mut(&browser_id).and_then(|cl| cl.pending.remove(&(id as i32))));
            let Some(tx) = tx else { return };
            let result = match msg.get("error") {
                Some(e) => Err(e.get("message").and_then(Value::as_str).unwrap_or("DevTools error").to_string()),
                None => Ok(msg.get("result").cloned().unwrap_or(Value::Null)),
            };
            tx.send(result);
            return;
        }
        let Some(method) = msg.get("method").and_then(Value::as_str) else { return };
        let mut params = msg.get("params").map(Value::to_string).unwrap_or_default();
        if params.len() > 2000 {
            params.truncate(params.floor_char_boundary(2000));
        }
        let entry = json!({ "method": method, "params": params, "sessionId": msg.get("sessionId").cloned().unwrap_or(Value::Null) });
        CLIENTS.with(|c| {
            if let Some(cl) = c.borrow_mut().get_mut(&browser_id) {
                cl.events.push_back(entry);
                while cl.events.len() > MAX_EVENTS {
                    cl.events.pop_front();
                }
            }
        });
    }

    fn fail_all(browser_id: i32, why: &str) {
        let pending = CLIENTS.with(|c| c.borrow_mut().get_mut(&browser_id).map(|cl| std::mem::take(&mut cl.pending)).unwrap_or_default());
        for (_, tx) in pending {
            tx.send(Err(why.to_string()));
        }
    }

    fn ensure(browser_id: i32) -> Option<BrowserHost> {
        let host = crate::browsers::browser(browser_id)?.host()?;
        if CLIENTS.with(|c| c.borrow().contains_key(&browser_id)) {
            return Some(host);
        }
        let mut observer = Observer::new(browser_id);
        let registration = host.add_dev_tools_message_observer(Some(&mut observer));
        registration.as_ref()?;
        CLIENTS.with(|c| c.borrow_mut().insert(browser_id, Client { _registration: registration, next_id: ID_BASE, pending: HashMap::new(), events: VecDeque::new() }));
        Some(host)
    }

    /// Sends one DevTools message and waits for its reply (`Err` = a protocol error, a detach, a
    /// timeout, or no browser).
    pub async fn call(browser_id: i32, method: &str, params: Value, session_id: Option<String>, timeout_ms: i64) -> Result<Value, String> {
        let host = ensure(browser_id).ok_or_else(|| "no DevTools client for that browser".to_string())?;
        let (tx, rx) = exec::oneshot();
        let id = CLIENTS
            .with(|c| {
                let mut c = c.borrow_mut();
                let client = c.get_mut(&browser_id)?;
                let id = client.next_id;
                client.next_id = if id >= ID_MAX { ID_BASE } else { id + 1 };
                client.pending.insert(id, tx);
                Some(id)
            })
            .ok_or_else(|| "no DevTools client for that browser".to_string())?;
        let mut message = json!({ "id": id, "method": method, "params": params });
        if let Some(session) = session_id {
            message["sessionId"] = Value::String(session);
        }
        let bytes = serde_json::to_vec(&message).map_err(|e| e.to_string())?;
        // No borrow is held here: observer callbacks can run inside this call.
        let sent = host.send_dev_tools_message(Some(&bytes));
        drop(host);
        if sent == 0 {
            CLIENTS.with(|c| {
                if let Some(cl) = c.borrow_mut().get_mut(&browser_id) {
                    cl.pending.remove(&id);
                }
            });
            return Err("send_dev_tools_message refused the message".into());
        }
        match exec::timeout(timeout_ms, rx).await {
            Some(Some(result)) => result,
            Some(None) => Err("the DevTools session was dropped".into()),
            None => {
                CLIENTS.with(|c| {
                    if let Some(cl) = c.borrow_mut().get_mut(&browser_id) {
                        cl.pending.remove(&id);
                    }
                });
                Err(format!("no answer within {timeout_ms} ms"))
            }
        }
    }

    pub fn events(browser_id: i32, clear: bool) -> Vec<Value> {
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

    /// Teardown: drops every registration before `cef::shutdown`.
    pub fn clear() {
        let clients = CLIENTS.with(|c| std::mem::take(&mut *c.borrow_mut()));
        SESSIONS.with(|s| s.borrow_mut().clear());
        drop(clients);
    }
}
