//! Agent connections and sessions [owner: automation] (docs/MCP.md "Channel", "Approval").
//!
//! One `Conn` per pipe connection: `hello` (5 s) → approval (core prompt, or a trusted client, or
//! the debug-only `STA_DEBUG_AGENT_AUTO_APPROVE=1`) → `welcome` → calls. At most 2
//! connections, one pending approval at a time, 60 s back-off after a Deny. Calls run as UI tasks
//! with their deadline and can be cancelled; results go back as `result` lines. Every call and
//! connection is logged to `agent.log` without content.

use super::tools::{self, Ctx, Output};
use super::{endpoint, exec, guards, pipe};
use crate::{controller, task};
use sta_core::agent::channel::{
    self as channel, APPROVAL_HOLD_MS, BridgeMessage, BrowserMessage, ByeReason, Content, HELLO_TIMEOUT_MS, LineReader, MAX_DEADLINE_MS,
    PROTOCOL_VERSION, PendingReason, RefusedCode, ToolError, to_line,
};
use sta_core::agent::{AgentClientInfo, ErrorCode, tools as catalog};
use sta_core::agent::policy;
use sta_core::{AgentAccess, Command, Id};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

const MAX_CONNECTIONS: usize = 2;
/// A connection prompt waits this long for an answer.
const PROMPT_TIMEOUT_MS: i64 = 120_000;
/// No new connection prompt this long after the user denied one.
const DENY_BACKOFF: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    AwaitHello,
    Pending { request: u64 },
    Active { session: u64 },
}

struct Bucket {
    tokens: f64,
    last: Instant,
    per_sec: f64,
    burst: f64,
}

impl Bucket {
    fn new(per_sec: f64, burst: f64) -> Self {
        Bucket { tokens: burst, last: Instant::now(), per_sec, burst }
    }

    fn take(&mut self) -> bool {
        let now = Instant::now();
        self.tokens = (self.tokens + now.duration_since(self.last).as_secs_f64() * self.per_sec).min(self.burst);
        self.last = now;
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

struct Conn {
    state: State,
    reader: LineReader,
    client: AgentClientInfo,
    identity: Option<pipe::ClientIdentity>,
    current_tab: Option<Id>,
    calls: HashMap<u64, exec::Sender<()>>,
    actions: Bucket,
    screenshots: Bucket,
    new_tabs: Bucket,
}

enum Request {
    Connection { conn: u64 },
    Site { waiter: exec::Sender<bool> },
    Tab { waiter: exec::Sender<bool> },
}

thread_local! {
    static CONNS: RefCell<HashMap<u64, Conn>> = RefCell::new(HashMap::new());
    static SERVER: RefCell<Option<pipe::Server>> = const { RefCell::new(None) };
    static REQUESTS: RefCell<HashMap<u64, Request>> = RefCell::new(HashMap::new());
    static NEXT_REQUEST: Cell<u64> = const { Cell::new(1) };
    static NEXT_SESSION: Cell<u64> = const { Cell::new(1) };
    static LAST_DENY: Cell<Option<Instant>> = const { Cell::new(None) };
    static CALLS_DONE: Cell<u64> = const { Cell::new(0) };
}

/// Debug builds: `STA_DEBUG_AGENT_AUTO_APPROVE=1` approves connections and sites (tests). An armed
/// test surface implies it as well (docs/TESTING.md), unless it was armed with
/// `--sta-test-hooks-no-approve`.
fn auto_approve() -> bool {
    #[cfg(all(debug_assertions, feature = "test-hooks"))]
    if crate::test_hooks::auto_approve() {
        return true;
    }
    cfg!(debug_assertions) && std::env::var("STA_DEBUG_AGENT_AUTO_APPROVE").is_ok_and(|v| v == "1")
}

/// The test surface is armed **and** arming implies approval (not
/// `--sta-test-hooks-no-approve`).
fn test_hooks_approves() -> bool {
    #[cfg(all(debug_assertions, feature = "test-hooks"))]
    return crate::test_hooks::auto_approve();
    #[cfg(not(all(debug_assertions, feature = "test-hooks")))]
    false
}


/// Starts a session at once, without the core's approval prompt (armed test surface only).
fn welcome_without_a_prompt(conn: u64, client: AgentClientInfo) {
    let session = NEXT_SESSION.replace(NEXT_SESSION.get() + 1);
    let access = access();
    CONNS.with(|c| {
        if let Some(x) = c.borrow_mut().get_mut(&conn) {
            x.state = State::Active { session };
        }
    });
    send(conn, &BrowserMessage::Welcome { v: PROTOCOL_VERSION, session, access, test_hooks: test_hooks_armed() });
    super::frame::schedule_refresh();
    controller::dispatch(Command::AgentSessionStarted { session, client: client.clone(), access });
    log_warn!("agent: session {session} started for {} without an approval prompt (test hooks)", client.display_name());
    endpoint::log(&format!("session={session} started client={:?} access={access:?} testHooks=1", client.display_name()));
}

/// The test surface is armed and may serve `test_*` tools to this browser's sessions.
fn test_hooks_armed() -> bool {
    #[cfg(all(debug_assertions, feature = "test-hooks"))]
    return crate::test_hooks::armed();
    #[cfg(not(all(debug_assertions, feature = "test-hooks")))]
    false
}

fn next_request() -> u64 {
    NEXT_REQUEST.replace(NEXT_REQUEST.get() + 1)
}

/// Writes one message to the connection. A `result` that does not fit in one channel line
/// (`MAX_LINE_BYTES`) is replaced by a `too_large` error **for that call**: the session and every
/// other call in flight survive, which is what the reader's old "line too long" teardown did not.
/// Anything else that grew too big (it cannot: the other messages are bounded) is dropped with a
/// warning rather than put on the pipe, because a half-line would desynchronise the framing.
fn send(conn: u64, msg: &BrowserMessage) {
    let bytes = match channel::to_line_checked(msg) {
        Ok(line) => {
            pipe::send(conn, line);
            return;
        }
        Err(bytes) => bytes,
    };
    log_warn!("agent: a {bytes}-byte channel message does not fit in one line");
    let BrowserMessage::Result { id, .. } = msg else {
        endpoint::log(&format!("conn={conn} dropped an oversized message ({bytes} bytes)"));
        return;
    };
    let error = channel::too_large(bytes);
    endpoint::log(&format!("conn={conn} id={id} result too large ({bytes} bytes)"));
    let replacement = BrowserMessage::Result { id: *id, content: vec![Content::Text { text: error.to_text() }], structured: None, error: Some(error) };
    pipe::send(conn, to_line(&replacement));
}

fn refuse(conn: u64, code: RefusedCode, message: &str) {
    send(conn, &BrowserMessage::Refused { code, message: message.to_string() });
    pipe::close(conn);
    endpoint::log(&format!("conn={conn} refused code={}", serde_json::to_string(&code).unwrap_or_default()));
}

/// The access level every gate must agree on — `start_call`'s `check_access` and
/// `tools::full_access_required` both ask this one function, so an armed run cannot pass the first
/// and be refused by the second (`read_only`).
pub(super) fn access() -> AgentAccess {
    // An armed test surface pre-sets full access **in memory for this run only** — never written
    // to state.json and never shown as a settings change (docs/TESTING.md, design §4.2).
    #[cfg(all(debug_assertions, feature = "test-hooks"))]
    if crate::test_hooks::auto_approve() {
        return AgentAccess::Full;
    }
    controller::with_store(|s| s.settings().agent_access).unwrap_or_default()
}

// ----------------------------------------------------------------------------------- endpoint

/// `Effect::AgentEndpoint`.
pub fn set_endpoint(enabled: bool) {
    let running = SERVER.with(|s| s.borrow().is_some());
    if enabled && !running {
        let Some(name) = pipe::endpoint_name() else {
            log_error!("agent: cannot name the agent channel");
            return;
        };
        let sink: pipe::EventSink = Arc::new(|conn, event| task::post_ui_from_any_thread(move || on_pipe_event(conn, event)));
        match pipe::start(&name, sink) {
            Ok(server) => {
                SERVER.with(|s| *s.borrow_mut() = Some(server));
                endpoint::write(&name);
                log_info!("agent: endpoint open");
                endpoint::log("endpoint open");
            }
            Err(e) => log_error!("agent: cannot open the endpoint: {e}"),
        }
    } else if !enabled && running {
        say_bye_to_all(ByeReason::AccessOff);
        let server = SERVER.with(|s| s.borrow_mut().take());
        if let Some(server) = server {
            server.stop();
        }
        endpoint::remove();
        guards::release_all();
        super::page::release_all_ax();
        super::console::clear();
        log_info!("agent: endpoint closed");
        endpoint::log("endpoint closed (access off)");
    } else if !enabled {
        // Startup with access off: an endpoint file left by a crashed session must not linger.
        endpoint::remove();
    }
}

fn say_bye_to_all(reason: ByeReason) {
    let conns: Vec<(u64, State)> = CONNS.with(|c| c.borrow().iter().map(|(id, x)| (*id, x.state)).collect());
    for (conn, state) in conns {
        send(conn, &BrowserMessage::Bye { reason });
        pipe::close(conn);
        drop_conn(conn, state);
    }
}

/// `Effect::AgentDisconnect` (Stop).
pub fn disconnect_all() {
    say_bye_to_all(ByeReason::UserStopped);
    guards::release_all();
    super::page::release_all_ax();
    endpoint::log("stopped by the user");
}

/// Shutdown: `bye{shutdown}`, close the endpoint.
pub fn shutdown() {
    say_bye_to_all(ByeReason::Shutdown);
    let server = SERVER.with(|s| s.borrow_mut().take());
    if let Some(server) = server {
        // Give the writer threads a moment for the byes.
        std::thread::sleep(Duration::from_millis(50));
        pipe::close_all();
        server.stop();
        endpoint::remove();
    }
}

// ----------------------------------------------------------------------------------- pipe events

fn on_pipe_event(conn: u64, event: pipe::PipeEvent) {
    match event {
        pipe::PipeEvent::Connected(identity) => {
            if CONNS.with(|c| c.borrow().len()) >= MAX_CONNECTIONS {
                refuse(conn, RefusedCode::TooManySessions, "Two agents are already connected to sta");
                return;
            }
            log_info!(
                "agent: connection {conn} from bridge pid {} (host pid {:?} {:?}, signed: {})",
                identity.bridge_pid,
                identity.host_pid,
                identity.host_exe,
                identity.host_signer.is_some()
            );
            CONNS.with(|c| {
                c.borrow_mut().insert(
                    conn,
                    Conn {
                        state: State::AwaitHello,
                        reader: LineReader::default(),
                        client: AgentClientInfo::default(),
                        identity: Some(identity),
                        current_tab: None,
                        calls: HashMap::new(),
                        actions: Bucket::new(10.0, 20.0),
                        screenshots: Bucket::new(2.0, 2.0),
                        new_tabs: Bucket::new(20.0 / 60.0, 20.0),
                    },
                )
            });
            task::post_ui_delayed(HELLO_TIMEOUT_MS as i64, move || {
                if CONNS.with(|c| c.borrow().get(&conn).is_some_and(|x| x.state == State::AwaitHello)) {
                    refuse(conn, RefusedCode::ProtocolError, "No hello");
                    CONNS.with(|c| c.borrow_mut().remove(&conn));
                }
            });
        }
        pipe::PipeEvent::Data(bytes) => {
            let batch = CONNS.with(|c| c.borrow_mut().get_mut(&conn).map(|x| x.reader.push(&bytes)));
            if let Some(batch) = batch {
                // An over-long line is dropped by the reader, not a protocol error: the call it
                // belonged to fails on its deadline and the session stays up.
                if batch.skipped > 0 {
                    log_warn!("agent: dropped {} oversized line(s) from connection {conn}", batch.skipped);
                    endpoint::log(&format!("conn={conn} dropped {} oversized line(s)", batch.skipped));
                }
                for line in batch.lines {
                    match serde_json::from_slice::<BridgeMessage>(&line) {
                        Ok(msg) => on_message(conn, msg),
                        Err(e) => {
                            protocol_error(conn, &e.to_string());
                            return;
                        }
                    }
                }
            }
        }
        pipe::PipeEvent::Closed => {
            let state = CONNS.with(|c| c.borrow().get(&conn).map(|x| x.state));
            if let Some(state) = state {
                drop_conn(conn, state);
            }
        }
    }
}

fn protocol_error(conn: u64, detail: &str) {
    log_warn!("agent: protocol error on connection {conn}: {detail}");
    let state = CONNS.with(|c| c.borrow().get(&conn).map(|x| x.state));
    send(conn, &BrowserMessage::Bye { reason: ByeReason::ProtocolError });
    pipe::close(conn);
    if let Some(state) = state {
        drop_conn(conn, state);
    }
}

/// Forgets a connection: ends its session, cancels its calls, withdraws its prompt.
fn drop_conn(conn: u64, state: State) {
    let removed = CONNS.with(|c| c.borrow_mut().remove(&conn));
    let Some(removed) = removed else { return };
    drop(removed.calls); // dropping the senders cancels the calls
    match state {
        State::Active { session } => {
            guards::release_session(session);
            controller::dispatch(Command::AgentSessionEnded { session });
            endpoint::log(&format!("session={session} ended"));
            if !CONNS.with(|c| c.borrow().values().any(|x| matches!(x.state, State::Active { .. }))) {
                super::page::release_all_ax();
            }
        }
        State::Pending { request } => {
            REQUESTS.with(|r| r.borrow_mut().remove(&request));
            controller::dispatch(Command::AnswerAgentConnection { id: request, allow: false, remember: false });
        }
        State::AwaitHello => {}
    }
}

fn on_message(conn: u64, msg: BridgeMessage) {
    let Some(state) = CONNS.with(|c| c.borrow().get(&conn).map(|x| x.state)) else { return };
    match msg {
        BridgeMessage::Hello { v, build, bridge: _, client } => {
            if state != State::AwaitHello {
                return protocol_error(conn, "second hello");
            }
            if v != PROTOCOL_VERSION {
                return refuse(conn, RefusedCode::VersionMismatch, &format!("sta speaks channel version {PROTOCOL_VERSION}, the bridge {v}"));
            }
            if build != env!("CARGO_PKG_VERSION") {
                log_warn!("agent: bridge build {build} differs from the browser build {}", env!("CARGO_PKG_VERSION"));
            }
            if access() == AgentAccess::Off {
                return refuse(conn, RefusedCode::AccessOff, "AI agent access is off in sta");
            }
            if controller::with_store(|s| s.agents_paused()).unwrap_or(false) {
                return refuse(conn, RefusedCode::Paused, "The user stopped agents in sta");
            }
            let identity = CONNS.with(|c| c.borrow().get(&conn).and_then(|x| x.identity.clone())).unwrap_or_default();
            let info = AgentClientInfo {
                name: client.name.chars().take(80).collect(),
                title: client.title.map(|t| t.chars().take(80).collect()),
                version: client.version.map(|t| t.chars().take(40).collect()),
                verified: identity.host_signer.is_some(),
                exe: identity.host_exe,
                signer: identity.host_signer,
            };
            CONNS.with(|c| {
                if let Some(x) = c.borrow_mut().get_mut(&conn) {
                    x.client = info.clone();
                }
            });
            // An armed test surface welcomes its harness itself (docs/TESTING.md): the core's
            // connection prompt needs `agent_access` on disk, and arming must not write the user's
            // settings. Nothing else is relaxed — `--sta-test-hooks-no-approve` keeps the real
            // prompt, which is what `agent-e2e` asserts on.
            if test_hooks_approves() {
                return welcome_without_a_prompt(conn, info);
            }
            let pending_elsewhere = REQUESTS.with(|r| r.borrow().values().any(|q| matches!(q, Request::Connection { .. })));
            let backoff = LAST_DENY.get().is_some_and(|t| t.elapsed() < DENY_BACKOFF);
            if !auto_approve() && (pending_elsewhere || backoff) {
                return refuse(conn, RefusedCode::ApprovalBusy, "Another approval is pending, or the user denied a client a moment ago");
            }
            let request = next_request();
            REQUESTS.with(|r| r.borrow_mut().insert(request, Request::Connection { conn }));
            CONNS.with(|c| {
                if let Some(x) = c.borrow_mut().get_mut(&conn) {
                    x.state = State::Pending { request };
                }
            });
            send(conn, &BrowserMessage::Pending { reason: PendingReason::Approval });
            endpoint::log(&format!("conn={conn} hello client={:?} signed={}", info.display_name(), info.verified));
            controller::dispatch(Command::AgentConnectionRequested { id: request, client: info });
            if auto_approve() {
                controller::dispatch(Command::AnswerAgentConnection { id: request, allow: true, remember: false });
            }
            task::post_ui_delayed(PROMPT_TIMEOUT_MS, move || {
                if REQUESTS.with(|r| r.borrow().contains_key(&request)) {
                    controller::dispatch(Command::AnswerAgentConnection { id: request, allow: false, remember: false });
                }
            });
        }
        BridgeMessage::Call { id, tool, args, deadline_ms } => match state {
            State::Active { session } => start_call(conn, session, id, tool, args, deadline_ms),
            State::Pending { .. } => send_error(conn, id, ToolError::new(ErrorCode::NotApproved, "Waiting for the user's approval in sta")),
            State::AwaitHello => protocol_error(conn, "call before hello"),
        },
        BridgeMessage::Cancel { id } => {
            let sender = CONNS.with(|c| c.borrow_mut().get_mut(&conn).and_then(|x| x.calls.remove(&id)));
            if let Some(tx) = sender {
                tx.send(());
            }
        }
    }
}

/// `Effect::AgentAnswer`.
pub fn answer(id: u64, allow: bool) {
    let Some(request) = REQUESTS.with(|r| r.borrow_mut().remove(&id)) else { return };
    match request {
        Request::Site { waiter } | Request::Tab { waiter } => waiter.send(allow),
        Request::Connection { conn } => {
            let Some((state, client)) = CONNS.with(|c| c.borrow().get(&conn).map(|x| (x.state, x.client.clone()))) else { return };
            if state != (State::Pending { request: id }) {
                return;
            }
            if !allow {
                LAST_DENY.set(Some(Instant::now()));
                CONNS.with(|c| c.borrow_mut().remove(&conn));
                let code = if access() == AgentAccess::Off {
                    RefusedCode::AccessOff
                } else if controller::with_store(|s| s.agents_paused()).unwrap_or(false) {
                    RefusedCode::Paused
                } else {
                    RefusedCode::ApprovalDenied
                };
                refuse(conn, code, "The user did not allow this client");
                return;
            }
            let session = NEXT_SESSION.replace(NEXT_SESSION.get() + 1);
            let access = access();
            CONNS.with(|c| {
                if let Some(x) = c.borrow_mut().get_mut(&conn) {
                    x.state = State::Active { session };
                }
            });
            send(conn, &BrowserMessage::Welcome { v: PROTOCOL_VERSION, session, access, test_hooks: test_hooks_armed() });
            super::frame::schedule_refresh();
            controller::dispatch(Command::AgentSessionStarted { session, client: client.clone(), access });
            log_info!("agent: session {session} started ({})", client.display_name());
            endpoint::log(&format!("session={session} started client={:?} access={access:?}", client.display_name()));
        }
    }
}

// ----------------------------------------------------------------------------------- calls

fn send_error(conn: u64, id: u64, error: ToolError) {
    send(conn, &BrowserMessage::Result { id, content: vec![Content::Text { text: error.to_text() }], structured: None, error: Some(error) });
}

fn start_call(conn: u64, session: u64, id: u64, tool: String, args: serde_json::Value, deadline_ms: u64) {
    // The debug-only test surface is a separate arm *before* every policy check (design §4.2:
    // separation, not relaxation). Only names starting with `test_` take it, only in a debug build
    // with the `test-hooks` feature, and only while the browser is armed — an un-armed build's
    // `catalog::find` answers `unknown_tool` for them like any other typo.
    let is_test_call = cfg!(all(debug_assertions, feature = "test-hooks")) && tool.starts_with("test_");
    if !is_test_call {
        let Some(def) = catalog::find(&tool) else {
            return send_error(conn, id, ToolError::new(ErrorCode::UnknownTool, format!("sta has no tool {tool:?}")));
        };
        if let Err(e) = policy::check_access(access(), def.read_only) {
            return send_error(conn, id, e);
        }
        if controller::with_store(|s| s.agents_paused()).unwrap_or(true) {
            return send_error(conn, id, ToolError::new(ErrorCode::Paused, "The user stopped agents"));
        }
    }
    let (cancel_tx, cancel_rx) = exec::oneshot::<()>();
    let duplicate = CONNS.with(|c| {
        let mut c = c.borrow_mut();
        let Some(x) = c.get_mut(&conn) else { return true };
        x.calls.insert(id, cancel_tx).is_some()
    });
    if duplicate {
        log_warn!("agent: call id {id} reused on connection {conn}");
    }
    let deadline_ms = deadline_ms.clamp(1_000, MAX_DEADLINE_MS);
    exec::spawn(async move {
        let started = Instant::now();
        let ctx = Ctx { conn, session, deadline: started + Duration::from_millis(deadline_ms) };
        let tool_name = tool.clone();
        let run = run_tool(&ctx, &tool_name, &args);
        let mut run = std::pin::pin!(run);
        let mut cancel = std::pin::pin!(cancel_rx);
        let mut timer = std::pin::pin!(exec::sleep(deadline_ms as i64 + 250));
        enum Outcome {
            Done(Result<Output, ToolError>),
            Cancelled,
            Deadline,
        }
        let outcome = std::future::poll_fn(|cx| {
            use std::task::Poll;
            if let Poll::Ready(r) = run.as_mut().poll(cx) {
                return Poll::Ready(Outcome::Done(r));
            }
            if cancel.as_mut().poll(cx).is_ready() {
                return Poll::Ready(Outcome::Cancelled);
            }
            if let Poll::Ready(()) = timer.as_mut().poll(cx) {
                return Poll::Ready(Outcome::Deadline);
            }
            Poll::Pending
        })
        .await;
        CONNS.with(|c| {
            if let Some(x) = c.borrow_mut().get_mut(&conn) {
                x.calls.remove(&id);
            }
        });
        CALLS_DONE.set(CALLS_DONE.get() + 1);
        let ms = started.elapsed().as_millis();
        let (result_text, tab, site): (String, Option<Id>, Option<String>) = match outcome {
            Outcome::Cancelled => {
                endpoint::log(&format!("session={session} tool={tool} ms={ms} result=cancelled"));
                return;
            }
            Outcome::Deadline => {
                send_error(conn, id, ToolError::new(ErrorCode::Timeout, format!("{tool} did not finish within {deadline_ms} ms")));
                ("error:timeout".to_string(), None, None)
            }
            Outcome::Done(Err(e)) => {
                let code = e.code.as_str().to_string();
                send_error(conn, id, e);
                (format!("error:{code}"), None, None)
            }
            Outcome::Done(Ok(out)) => {
                let (tab, site) = (out.tab, out.site.clone());
                send(conn, &BrowserMessage::Result { id, content: out.content, structured: out.structured, error: None });
                ("ok".to_string(), tab, site)
            }
        };
        let tab_text = tab.map(|t| t.to_string()).unwrap_or_else(|| "-".into());
        endpoint::log(&format!("session={session} tool={tool} tab={tab_text} site={} ms={ms} result={result_text}", site.as_deref().unwrap_or("-")));
        if is_test_call {
            // The harness is not an agent: it never appears in the user's activity list, and it
            // never marks a tab agent-controlled (design §4.2, §10.4).
            return;
        }
        let error = result_text.strip_prefix("error:").map(str::to_string);
        controller::dispatch(Command::AgentActivity { session, tool, tab, site, error });
    });
}

/// One tool call: the test surface first (armed debug builds only), else the 23 shipped tools.
async fn run_tool(ctx: &Ctx, tool: &str, args: &serde_json::Value) -> Result<Output, ToolError> {
    #[cfg(all(debug_assertions, feature = "test-hooks"))]
    if crate::test_hooks::is_test_tool(tool) {
        return crate::test_hooks::run(ctx, tool, args).await;
    }
    tools::run(ctx, tool, args).await
}

// ----------------------------------------------------------------------------------- for tools

/// The agent pipe endpoint is open (access is on).
pub fn endpoint_open() -> bool {
    SERVER.with(|s| s.borrow().is_some())
}

/// `session` belongs to a connected, approved agent.
pub fn is_active_session(session: u64) -> bool {
    CONNS.with(|c| c.borrow().values().any(|x| x.state == State::Active { session }))
}

pub fn current_tab(conn: u64) -> Option<Id> {
    CONNS.with(|c| c.borrow().get(&conn).and_then(|x| x.current_tab))
}

pub fn set_current_tab(conn: u64, tab: Id) {
    CONNS.with(|c| {
        if let Some(x) = c.borrow_mut().get_mut(&conn) {
            x.current_tab = Some(tab);
        }
    });
}

pub fn clear_current_tab(conn: u64) {
    CONNS.with(|c| {
        if let Some(x) = c.borrow_mut().get_mut(&conn) {
            x.current_tab = None;
        }
    });
}

fn take(conn: u64, pick: impl FnOnce(&mut Conn) -> &mut Bucket, what: &str) -> Result<(), ToolError> {
    let ok = CONNS.with(|c| c.borrow_mut().get_mut(&conn).map(|x| pick(x).take())).unwrap_or(true);
    if ok { Ok(()) } else { Err(ToolError::new(ErrorCode::RateLimited, format!("Too many {what}"))) }
}

pub fn take_action_token(conn: u64) -> Result<(), ToolError> {
    take(conn, |x| &mut x.actions, "actions per second")
}

pub fn take_screenshot_token(conn: u64) -> Result<(), ToolError> {
    take(conn, |x| &mut x.screenshots, "screenshots per second")
}

pub fn take_tab_token(conn: u64) -> Result<(), ToolError> {
    take(conn, |x| &mut x.new_tabs, "new tabs per minute")
}

/// Makes sure `site` is approved for the session: at once when it already is, else a site prompt
/// held at most [`APPROVAL_HOLD_MS`] (the prompt stays up afterwards).
pub async fn ensure_site(conn: u64, session: u64, tab: Option<Id>, site: &str, deadline: Instant) -> Result<(), ToolError> {
    let _ = conn;
    if controller::with_store(|s| s.agent_site_approved(session, site)).unwrap_or(false) {
        return Ok(());
    }
    // An armed test surface's session has no core prompt to answer (see `on_message`).
    if test_hooks_approves() {
        return Ok(());
    }
    let request = next_request();
    let (tx, rx) = exec::oneshot::<bool>();
    REQUESTS.with(|r| r.borrow_mut().insert(request, Request::Site { waiter: tx }));
    controller::dispatch(Command::AgentSiteRequested { id: request, session, tab, site: site.to_string() });
    // Like connection prompts, an unanswered site prompt goes away after a while (as a denial).
    task::post_ui_delayed(PROMPT_TIMEOUT_MS, move || {
        if controller::with_store(|s| s.agent_prompt_pending(request)).unwrap_or(false) {
            controller::dispatch(Command::AnswerSitePermission { id: request, allow: false, remember: false });
        }
    });
    if auto_approve() {
        controller::dispatch(Command::AnswerSitePermission { id: request, allow: true, remember: false });
    }
    let hold = deadline.saturating_duration_since(Instant::now()).as_millis().min(APPROVAL_HOLD_MS as u128) as i64;
    match exec::timeout(hold.max(1), rx).await {
        Some(Some(true)) => Ok(()),
        Some(_) => Err(ToolError::new(ErrorCode::SiteNotApproved, format!("The user did not allow agents on {site}"))),
        None => {
            // The prompt stays up; a later call finds the answer.
            REQUESTS.with(|r| r.borrow_mut().remove(&request));
            Err(ToolError::new(ErrorCode::SiteNotApproved, format!("Waiting for the user to allow agents on {site}")))
        }
    }
}

/// `request_tab_access`: asks the user to share `tab` with the session's agent: at once when it
/// is already in scope, else a tab prompt held at most [`APPROVAL_HOLD_MS`] (the prompt stays up
/// afterwards; answering it later still shares the tab).
pub async fn ensure_tab_access(session: u64, tab: Id, reason: &str, deadline: Instant) -> Result<(), ToolError> {
    let request = next_request();
    let (tx, rx) = exec::oneshot::<bool>();
    REQUESTS.with(|r| r.borrow_mut().insert(request, Request::Tab { waiter: tx }));
    controller::dispatch(Command::AgentTabAccessRequested { id: request, session, tab, reason: reason.to_string() });
    task::post_ui_delayed(PROMPT_TIMEOUT_MS, move || {
        if controller::with_store(|s| s.agent_prompt_pending(request)).unwrap_or(false) {
            controller::dispatch(Command::AnswerTabAccess { id: request, allow: false });
        }
    });
    let hold = deadline.saturating_duration_since(Instant::now()).as_millis().min(APPROVAL_HOLD_MS as u128) as i64;
    match exec::timeout(hold.max(1), rx).await {
        Some(Some(true)) => Ok(()),
        Some(_) => Err(ToolError::new(ErrorCode::NotApproved, format!("The user declined to share tab {tab}")).with_hint("Don't ask again for this tab unless the user tells you to.")),
        None => {
            REQUESTS.with(|r| r.borrow_mut().remove(&request));
            Err(ToolError::new(ErrorCode::NotApproved, format!("The user hasn't answered yet (tab {tab})"))
                .with_hint("The request stays up in sta for 2 minutes; call tabs_list later to see whether the tab was shared."))
        }
    }
}

pub fn clear() {
    CONNS.with(|c| c.borrow_mut().clear());
    REQUESTS.with(|r| r.borrow_mut().clear());
    SERVER.with(|s| s.borrow_mut().take());
}

pub fn debug_snapshot() -> serde_json::Value {
    let conns: Vec<serde_json::Value> = CONNS.with(|c| {
        c.borrow()
            .iter()
            .map(|(id, x)| {
                let state = match x.state {
                    State::AwaitHello => "awaitHello".to_string(),
                    State::Pending { request } => format!("pending:{request}"),
                    State::Active { session } => format!("active:{session}"),
                };
                serde_json::json!({ "conn": id, "state": state, "calls": x.calls.len(), "currentTab": x.current_tab, "client": x.client.display_name(), "signed": x.client.verified })
            })
            .collect()
    });
    serde_json::json!({
        "endpoint": SERVER.with(|s| s.borrow().is_some()),
        "connections": conns,
        "pipeConnections": pipe::connection_count(),
        "requests": REQUESTS.with(|r| r.borrow().len()),
        "callsDone": CALLS_DONE.get(),
    })
}
