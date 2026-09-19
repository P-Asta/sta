//! The bridge side of the agent channel (docs/MCP.md "Channel"): reads the endpoint file, opens
//! the pipe after the owner, session, integrity and server-process checks, says hello, waits for
//! approval, forwards calls and cancellations, and launches sta when it isn't running.

use sta_core::agent::ErrorCode;
use sta_core::legacy;
use sta_core::agent::channel::{
    APPROVAL_HOLD_MS, BridgeInfo, BridgeMessage, BrowserMessage, ByeReason, ClientInfo, Content, DEFAULT_DEADLINE_MS, ENDPOINT_FILE, Endpoint, LineReader,
    MAX_DEADLINE_MS, PROTOCOL_VERSION, RefusedCode, ToolError, to_line, to_line_checked, too_large,
};
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt, ReadHalf, WriteHalf};
use tokio::sync::{Mutex, oneshot, watch};

#[cfg(windows)]
use tokio::net::windows::named_pipe::{ClientOptions, NamedPipeClient};

/// How long a launched browser gets to write its endpoint file.
const LAUNCH_WAIT: Duration = Duration::from_secs(25);

#[derive(Debug, Clone, PartialEq)]
enum Approval {
    /// Hello sent, nothing heard yet.
    Waiting,
    Pending,
    Welcome,
    Refused(RefusedCode, String),
    /// The connection ended (with the browser's `bye` reason, if any).
    Closed(Option<ByeReason>),
}

struct Conn {
    #[cfg(windows)]
    writer: Mutex<WriteHalf<NamedPipeClient>>,
    pending: StdMutex<HashMap<u64, oneshot::Sender<BrowserMessage>>>,
    approval: watch::Sender<Approval>,
    closed: AtomicBool,
}

struct Inner {
    conn: Option<Arc<Conn>>,
    /// `bye{user_stopped | access_off}` was received: never reconnect.
    final_bye: Option<ByeReason>,
}

pub struct Channel {
    data_dir: PathBuf,
    launch: bool,
    inner: Mutex<Inner>,
    next_id: AtomicU64,
    client: StdMutex<ClientInfo>,
    /// The browser's `welcome` said its debug-only test surface is armed (docs/TESTING.md). Only
    /// then does the bridge serve the `test_*` tools; `test_hooks_changed` tells the server to
    /// send one `notifications/tools/list_changed` after that first welcome.
    test_hooks: Arc<AtomicBool>,
    test_hooks_changed: Arc<AtomicBool>,
}

fn err(code: ErrorCode, message: impl Into<String>) -> ToolError {
    ToolError::new(code, message)
}

fn not_running(message: impl Into<String>) -> ToolError {
    err(ErrorCode::BrowserNotRunning, message)
}

impl Channel {
    pub fn new(data_dir: PathBuf, launch: bool) -> Self {
        Channel {
            data_dir,
            launch,
            inner: Mutex::new(Inner { conn: None, final_bye: None }),
            next_id: AtomicU64::new(1),
            client: StdMutex::new(ClientInfo { name: "unknown".into(), title: None, version: None }),
            test_hooks: Arc::new(AtomicBool::new(false)),
            test_hooks_changed: Arc::new(AtomicBool::new(false)),
        }
    }

    /// The connected browser runs with the debug-only test surface armed.
    pub fn test_hooks(&self) -> bool {
        self.test_hooks.load(Ordering::SeqCst)
    }

    /// `true` once, right after the armed browser's first `welcome`: the tool list grew.
    pub fn take_test_hooks_change(&self) -> bool {
        self.test_hooks_changed.swap(false, Ordering::SeqCst)
    }

    /// Remembers what the MCP client said about itself (sent in the next `hello`).
    pub fn set_client(&self, name: &str, title: Option<&str>, version: Option<&str>) {
        if let Ok(mut c) = self.client.lock() {
            *c = ClientInfo { name: name.chars().take(80).collect(), title: title.map(|t| t.chars().take(80).collect()), version: version.map(|v| v.chars().take(40).collect()) };
        }
    }

    /// Where the browser's endpoint file may be ([`endpoint_candidates`]).
    pub fn endpoint_paths(&self) -> Vec<PathBuf> {
        endpoint_candidates(&self.data_dir, default_data_dir().as_deref(), cfg!(debug_assertions))
    }

    fn read_endpoint(&self) -> Option<Endpoint> {
        #[cfg(windows)]
        let alive = crate::win::process_alive;
        #[cfg(not(windows))]
        let alive = |_: u32| true;
        read_endpoint_from(&self.endpoint_paths(), alive)
    }

    /// Runs one tool call in the browser. `cancel` resolves when the MCP client cancels it.
    pub async fn call(&self, tool: &str, args: Value, cancel: impl std::future::Future<Output = ()>) -> Result<(Vec<Content>, Option<Value>), ToolError> {
        let conn = self.approved_connection().await?;
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = oneshot::channel();
        if let Ok(mut p) = conn.pending.lock() {
            p.insert(id, tx);
        }
        let deadline_ms = deadline_for(&args);
        // A call whose arguments do not fit in one channel line fails here, on its own: putting a
        // half line on the pipe would desynchronise the framing for every other call.
        let line = match to_line_checked(&BridgeMessage::Call { id, tool: tool.to_string(), args, deadline_ms }) {
            Ok(line) => line,
            Err(bytes) => {
                if let Ok(mut p) = conn.pending.lock() {
                    p.remove(&id);
                }
                return Err(too_large(bytes));
            }
        };
        if write(&conn, &line).await.is_err() {
            return Err(not_running("The connection to sta was lost"));
        }
        tokio::select! {
            result = rx => match result {
                Ok(BrowserMessage::Result { content, structured, error, .. }) => match error {
                    Some(e) => Err(e),
                    None => Ok((content, structured)),
                },
                Ok(_) => Err(err(ErrorCode::Internal, "Unexpected answer from sta")),
                Err(_) => Err(self.closed_error(&conn)),
            },
            _ = cancel => {
                let _ = write(&conn, &to_line(&BridgeMessage::Cancel { id })).await;
                if let Ok(mut p) = conn.pending.lock() {
                    p.remove(&id);
                }
                Err(err(ErrorCode::Timeout, "Cancelled"))
            }
            _ = tokio::time::sleep(Duration::from_millis(deadline_ms + 5_000)) => {
                let _ = write(&conn, &to_line(&BridgeMessage::Cancel { id })).await;
                Err(err(ErrorCode::Timeout, "sta didn't answer in time"))
            }
        }
    }

    fn closed_error(&self, conn: &Conn) -> ToolError {
        match conn.approval.borrow().clone() {
            Approval::Closed(Some(ByeReason::UserStopped)) => err(ErrorCode::Paused, "The user stopped agents in sta"),
            Approval::Closed(Some(ByeReason::AccessOff)) => err(ErrorCode::AccessOff, "AI agent access was turned off in sta"),
            _ => not_running("The connection to sta was lost"),
        }
    }

    /// A welcomed connection. When the browser goes away before answering (it was exiting, or a
    /// stale endpoint), the bridge connects once more (launching sta if allowed).
    async fn approved_connection(&self) -> Result<Arc<Conn>, ToolError> {
        let conn = self.connection().await?;
        match self.await_approval(&conn).await {
            Ok(()) => Ok(conn),
            Err(e) if e.code == ErrorCode::BrowserNotRunning => {
                let conn = self.connection().await?;
                self.await_approval(&conn).await?;
                Ok(conn)
            }
            Err(e) => Err(e),
        }
    }

    async fn await_approval(&self, conn: &Arc<Conn>) -> Result<(), ToolError> {
        let mut rx = conn.approval.subscribe();
        let decided = tokio::time::timeout(Duration::from_millis(APPROVAL_HOLD_MS), async {
            loop {
                let state = rx.borrow_and_update().clone();
                if !matches!(state, Approval::Waiting | Approval::Pending) {
                    return state;
                }
                if rx.changed().await.is_err() {
                    return Approval::Closed(None);
                }
            }
        })
        .await;
        match decided {
            Ok(Approval::Welcome) => Ok(()),
            Ok(Approval::Refused(code, message)) => {
                self.forget(conn).await;
                Err(refused_error(code, &message))
            }
            Ok(Approval::Closed(reason)) => {
                self.forget(conn).await;
                Err(match reason {
                    Some(ByeReason::UserStopped) => err(ErrorCode::Paused, "The user stopped agents in sta"),
                    Some(ByeReason::AccessOff) => err(ErrorCode::AccessOff, "AI agent access is off in sta"),
                    _ => not_running("sta closed the connection"),
                })
            }
            Ok(Approval::Waiting | Approval::Pending) | Err(_) => Err(err(ErrorCode::NotApproved, "Waiting for the user to allow this client in sta")),
        }
    }

    async fn forget(&self, conn: &Arc<Conn>) {
        let mut inner = self.inner.lock().await;
        if inner.conn.as_ref().is_some_and(|c| Arc::ptr_eq(c, conn)) {
            inner.conn = None;
        }
    }

    /// The live connection, (re)connecting once when needed.
    async fn connection(&self) -> Result<Arc<Conn>, ToolError> {
        let mut inner = self.inner.lock().await;
        if let Some(reason) = inner.final_bye {
            return Err(match reason {
                ByeReason::UserStopped => err(ErrorCode::Paused, "The user stopped agents in sta").with_hint("Ask the user to press Resume in sta, then restart this MCP server."),
                _ => err(ErrorCode::AccessOff, "AI agent access was turned off in sta").with_hint("Ask the user to turn on AI agent access, then restart this MCP server."),
            });
        }
        if let Some(c) = inner.conn.clone() {
            if !c.closed.load(Ordering::SeqCst) {
                return Ok(c);
            }
            let closed = c.approval.borrow().clone();
            if let Approval::Closed(Some(reason)) = closed
                && reason.is_final()
            {
                inner.final_bye = Some(reason);
                return Err(self.closed_error(&c));
            }
            inner.conn = None;
        }
        let conn = self.open().await?;
        inner.conn = Some(conn.clone());
        Ok(conn)
    }

    #[cfg(not(windows))]
    async fn open(&self) -> Result<Arc<Conn>, ToolError> {
        Err(not_running("The sta agent channel is only available on Windows"))
    }

    #[cfg(windows)]
    async fn open(&self) -> Result<Arc<Conn>, ToolError> {
        let mut launched = false;
        loop {
            let endpoint = match self.read_endpoint() {
                Some(e) => e,
                None if self.launch && !launched => {
                    self.launch_browser(None).await?;
                    launched = true;
                    continue;
                }
                None if launched => return Err(not_running("sta started, but AI agent access is off").with_hint("Ask the user to turn on AI agent access in sta Settings.")),
                None => return Err(not_running("sta isn't running, or AI agent access is off")),
            };
            if endpoint.protocol != PROTOCOL_VERSION {
                return Err(err(ErrorCode::VersionMismatch, format!("sta speaks channel version {}, this bridge {PROTOCOL_VERSION}", endpoint.protocol)));
            }
            if !endpoint.pipe_is_valid() {
                return Err(err(ErrorCode::EndpointUntrusted, "The endpoint file names an unexpected pipe"));
            }
            // An endpoint left behind by a browser that is gone (crash, killed).
            if !crate::win::process_alive(endpoint.pid) {
                if self.launch && !launched {
                    self.launch_browser(Some(endpoint.pipe.clone())).await?;
                    launched = true;
                    continue;
                }
                return Err(not_running("sta isn't running"));
            }
            match open_pipe(&endpoint.pipe).await {
                Ok(pipe) => return self.start(pipe, endpoint.pid).await,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound && self.launch && !launched => {
                    // A stale endpoint (the browser exited without removing it).
                    self.launch_browser(Some(endpoint.pipe.clone())).await?;
                    launched = true;
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(not_running("sta isn't running")),
                Err(e) => return Err(not_running(format!("Can't open the sta agent channel ({e})"))),
            }
        }
    }

    /// `--check` (Settings → Test connection): the endpoint file, the pipe and its owner, session,
    /// integrity and server-process checks, exactly as a tool call would reach them, then disconnects without a `hello` (so no
    /// approval prompt). Never launches sta. `Ok` = a readable description.
    #[cfg(windows)]
    pub async fn check(&self) -> Result<String, ToolError> {
        use std::os::windows::io::AsRawHandle;
        let Some(endpoint) = self.read_endpoint() else {
            return Err(not_running("sta isn't running, or AI agent access is off"));
        };
        if endpoint.protocol != PROTOCOL_VERSION {
            return Err(err(ErrorCode::VersionMismatch, format!("sta speaks channel version {}, this bridge {PROTOCOL_VERSION}", endpoint.protocol)));
        }
        if !endpoint.pipe_is_valid() {
            return Err(err(ErrorCode::EndpointUntrusted, "The endpoint file names an unexpected pipe"));
        }
        if !crate::win::process_alive(endpoint.pid) {
            return Err(not_running("sta isn't running (stale endpoint file)"));
        }
        let pipe = open_pipe(&endpoint.pipe).await.map_err(|e| not_running(format!("Can't open the sta agent channel ({e})")))?;
        verify_pipe(pipe.as_raw_handle(), endpoint.pid)?;
        drop(pipe);
        Ok(format!("The MCP server reached sta (pid {}) and verified its pipe", endpoint.pid))
    }

    #[cfg(not(windows))]
    pub async fn check(&self) -> Result<String, ToolError> {
        Err(not_running("The sta agent channel is only available on Windows"))
    }

    #[cfg(windows)]
    async fn start(&self, pipe: NamedPipeClient, endpoint_pid: u32) -> Result<Arc<Conn>, ToolError> {
        use std::os::windows::io::AsRawHandle;
        verify_pipe(pipe.as_raw_handle(), endpoint_pid)?;
        let (reader, writer) = tokio::io::split(pipe);
        let (approval, _) = watch::channel(Approval::Waiting);
        let conn = Arc::new(Conn { writer: Mutex::new(writer), pending: StdMutex::new(HashMap::new()), approval, closed: AtomicBool::new(false) });
        tokio::spawn(read_loop(reader, conn.clone(), self.test_hooks.clone(), self.test_hooks_changed.clone()));
        let client = self.client.lock().map(|c| c.clone()).unwrap_or_default();
        let hello = BridgeMessage::Hello {
            v: PROTOCOL_VERSION,
            build: env!("CARGO_PKG_VERSION").to_string(),
            bridge: BridgeInfo { version: env!("CARGO_PKG_VERSION").to_string(), pid: std::process::id() },
            client,
        };
        write(&conn, &to_line(&hello)).await.map_err(|_| not_running("The connection to sta was lost"))?;
        Ok(conn)
    }

    #[cfg(windows)]
    async fn launch_browser(&self, stale_pipe: Option<String>) -> Result<(), ToolError> {
        if crate::win::has_package_identity() {
            return Err(not_running("sta isn't running").with_hint("Open sta yourself: this client runs packaged and can't start it."));
        }
        let exe = std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.join("sta.exe"))).filter(|p| p.is_file());
        let Some(exe) = exe else {
            return Err(not_running("sta isn't running (sta.exe was not found next to sta-mcp.exe)"));
        };
        let pid = crate::win::launch(&exe, &self.data_dir).map_err(|e| not_running(e).with_hint("Ask the user to open sta."))?;
        eprintln!("[sta-mcp] started sta (pid {pid})");
        let started = std::time::Instant::now();
        while started.elapsed() < LAUNCH_WAIT {
            if let Some(e) = self.read_endpoint()
                && Some(&e.pipe) != stale_pipe.as_ref()
            {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        Ok(())
    }
}

/// The call deadline: 30 s, or longer when the tool itself is asked to wait longer (`timeoutMs`,
/// `timeMs`), at most 120 s.
fn deadline_for(args: &Value) -> u64 {
    let wait = ["timeoutMs", "timeMs"].iter().filter_map(|k| args.get(*k).and_then(Value::as_u64)).max().unwrap_or(0);
    (wait.saturating_add(5_000)).clamp(DEFAULT_DEADLINE_MS, MAX_DEADLINE_MS)
}

fn refused_error(code: RefusedCode, message: &str) -> ToolError {
    match code {
        RefusedCode::AccessOff => err(ErrorCode::AccessOff, message),
        RefusedCode::ApprovalDenied => err(ErrorCode::NotApproved, message).with_hint("The user denied this client; don't retry unless the user asks."),
        RefusedCode::VersionMismatch => err(ErrorCode::VersionMismatch, message),
        RefusedCode::TooManySessions => err(ErrorCode::Busy, message).with_hint("Two agents are already connected; try again later."),
        RefusedCode::Paused => err(ErrorCode::Paused, message),
        RefusedCode::ApprovalBusy => err(ErrorCode::NotApproved, message).with_hint("Another client is waiting for approval, or the user just denied one; retry in a minute."),
        RefusedCode::ProtocolError => err(ErrorCode::Internal, message),
    }
}

/// The checks before the bridge writes anything to an opened pipe: owned by this user, served
/// from this logon session, labelled at medium integrity or above (a low-integrity or sandboxed
/// process can't have created it), and served by the process the endpoint file names.
#[cfg(windows)]
fn verify_pipe(raw: std::os::windows::io::RawHandle, endpoint_pid: u32) -> Result<(), ToolError> {
    let hint = "Another user or program may have taken the pipe name; ask the user to restart sta.";
    if !crate::win::pipe_owner_is_current_user(raw) || !crate::win::pipe_server_in_our_session(raw) {
        return Err(err(ErrorCode::EndpointUntrusted, "The sta agent pipe isn't owned by you in this session").with_hint(hint));
    }
    match crate::win::pipe_integrity_rid(raw) {
        Some(rid) if rid >= crate::win::MANDATORY_MEDIUM_RID => {}
        _ => return Err(err(ErrorCode::EndpointUntrusted, "The sta agent pipe was created below medium integrity (not by sta)").with_hint(hint)),
    }
    match crate::win::pipe_server_pid(raw) {
        Some(pid) if pid == endpoint_pid => Ok(()),
        other => Err(err(ErrorCode::EndpointUntrusted, format!("The sta agent pipe is served by another process (pid {}, not {endpoint_pid})", other.map_or("unknown".to_string(), |p| p.to_string()))).with_hint(hint)),
    }
}

#[cfg(windows)]
async fn open_pipe(name: &str) -> std::io::Result<NamedPipeClient> {
    const ERROR_PIPE_BUSY: i32 = 231;
    let started = std::time::Instant::now();
    loop {
        let r = ClientOptions::new().security_qos_flags(crate::win::SECURITY_IDENTIFICATION | crate::win::SECURITY_SQOS_PRESENT).open(name);
        match r {
            Err(e) if e.raw_os_error() == Some(ERROR_PIPE_BUSY) && started.elapsed() < Duration::from_secs(3) => {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            other => return other,
        }
    }
}

#[cfg(windows)]
async fn write(conn: &Conn, line: &[u8]) -> std::io::Result<()> {
    let mut w = conn.writer.lock().await;
    w.write_all(line).await?;
    w.flush().await
}

#[cfg(not(windows))]
async fn write(_conn: &Conn, _line: &[u8]) -> std::io::Result<()> {
    Err(std::io::Error::other("unsupported"))
}

#[cfg(windows)]
async fn read_loop(mut reader: ReadHalf<NamedPipeClient>, conn: Arc<Conn>, test_hooks: Arc<AtomicBool>, test_hooks_changed: Arc<AtomicBool>) {
    let mut lines = LineReader::default();
    let mut buf = vec![0u8; 64 * 1024];
    let mut bye = None;
    'outer: loop {
        let n = match reader.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        let batch = lines.push(&buf[..n]);
        if batch.skipped > 0 {
            // The browser broke the framing rule. Its own `send` turns an oversized result into a
            // `too_large` error, so this is belt and braces: the calls those lines belonged to time
            // out, and the session keeps serving everything else.
            eprintln!("[sta-mcp] dropped {} oversized message(s) from sta", batch.skipped);
        }
        for line in batch.lines {
            let Ok(msg) = serde_json::from_slice::<BrowserMessage>(&line) else {
                eprintln!("[sta-mcp] unparsable message from sta; closing");
                break 'outer;
            };
            match msg {
                BrowserMessage::Pending { .. } => {
                    conn.approval.send_replace(Approval::Pending);
                }
                BrowserMessage::Welcome { v, test_hooks: armed, .. } if v == PROTOCOL_VERSION => {
                    // An armed browser (docs/TESTING.md) grows this session's tool list once.
                    if armed && !test_hooks.swap(true, Ordering::SeqCst) {
                        test_hooks_changed.store(true, Ordering::SeqCst);
                        eprintln!("[sta-mcp] sta runs with the debug-only test surface armed: serving the test_* tools");
                    }
                    conn.approval.send_replace(Approval::Welcome);
                }
                BrowserMessage::Welcome { .. } => break 'outer,
                BrowserMessage::Refused { code, message } => {
                    conn.approval.send_replace(Approval::Refused(code, message));
                }
                BrowserMessage::Progress { .. } => {}
                BrowserMessage::Result { id, .. } => {
                    let tx = conn.pending.lock().ok().and_then(|mut p| p.remove(&id));
                    if let Some(tx) = tx {
                        let _ = tx.send(msg);
                    }
                }
                BrowserMessage::Bye { reason } => {
                    bye = Some(reason);
                    break 'outer;
                }
            }
        }
    }
    conn.closed.store(true, Ordering::SeqCst);
    let refused = matches!(*conn.approval.borrow(), Approval::Refused(..));
    if !refused {
        conn.approval.send_replace(Approval::Closed(bye));
    }
    if let Ok(mut p) = conn.pending.lock() {
        p.clear(); // dropping the senders fails the waiting calls
    }
}

/// The folder under `%LOCALAPPDATA%` of the matching sta build's default profile.
pub const DATA_DIR_NAME: &str = if cfg!(debug_assertions) { "sta Dev" } else { "sta" };

/// The default data directory of the matching sta build.
pub fn default_data_dir() -> Option<PathBuf> {
    let local = std::env::var_os("LOCALAPPDATA")?;
    Some(Path::new(&local).join(DATA_DIR_NAME))
}

/// The profile subfolder of a data directory, where the browser writes [`ENDPOINT_FILE`] (the
/// browser's `paths::PROFILE_DIR`).
const PROFILE_DIR: &str = "sta";

/// Where the browser's endpoint file may be for `data_dir`, in the order they are tried:
/// `<data_dir>/sta/`, then the layouts of a data folder from before the rename that sta uses in
/// place because it couldn't move it yet (README, migration notes; the browser's `paths::resolve`
/// keeps such a folder's old layout for that run): the legacy profile subfolder in `data_dir`
/// and, when `data_dir` is the build's default folder `default_dir`, the legacy default folder
/// next to it with either profile subfolder. The browser never writes the file anywhere else, and
/// migrating moves a legacy folder only when no browser uses it, so at most one of them is live.
pub fn endpoint_candidates(data_dir: &Path, default_dir: Option<&Path>, debug: bool) -> Vec<PathBuf> {
    let mut bases = vec![data_dir.to_path_buf()];
    if let Some(parent) = default_dir.filter(|d| same_folder(d, data_dir)).and_then(Path::parent) {
        bases.push(parent.join(legacy::data_dir_name(debug)));
    }
    bases.iter().flat_map(|base| [PROFILE_DIR, legacy::PROFILE_DIR].map(|profile| base.join(profile).join(ENDPOINT_FILE))).collect()
}

/// The same folder on Windows' case-insensitive file systems (separator style and trailing
/// separators ignored; the browser's `paths::same_folder`).
fn same_folder(a: &Path, b: &Path) -> bool {
    let normalized = |p: &Path| {
        let p = std::path::absolute(p).unwrap_or_else(|_| p.to_path_buf());
        p.to_string_lossy().replace('/', "\\").trim_end_matches('\\').to_lowercase()
    };
    normalized(a) == normalized(b)
}

/// Reads the endpoint files among `candidates` (see [`endpoint_candidates`]) and picks one: the
/// first whose browser process is `alive`, else the first one found (a stale file: the caller
/// relaunches sta or reports it). Only the first candidate may name a pipe sta never creates
/// (reported as untrusted); in the legacy layouts such a file belongs to a browser from before
/// the rename and is ignored.
pub fn read_endpoint_from(candidates: &[PathBuf], alive: impl Fn(u32) -> bool) -> Option<Endpoint> {
    let found: Vec<Endpoint> = candidates
        .iter()
        .enumerate()
        .filter_map(|(i, path)| {
            let text = std::fs::read_to_string(path).ok()?;
            let endpoint: Endpoint = serde_json::from_str(&text).ok()?;
            (i == 0 || endpoint.pipe_is_valid()).then_some(endpoint)
        })
        .collect();
    let live = found.iter().position(|e| e.pipe_is_valid() && alive(e.pid));
    let index = live.or((!found.is_empty()).then_some(0))?;
    found.into_iter().nth(index)
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use tokio::io::{AsyncBufReadExt, BufReader};
    use tokio::net::windows::named_pipe::ServerOptions;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("sta-mcp-test-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sta")).unwrap();
        dir
    }

    fn pipe_name() -> String {
        let n: u128 = (std::process::id() as u128) << 64 | std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos() & 0xffff_ffff_ffff_ffff;
        format!(r"\\.\pipe\sta-agent-{n:032x}")
    }

    fn write_endpoint(dir: &Path, pipe: &str) {
        let e = Endpoint { pipe: pipe.into(), pid: std::process::id(), protocol: PROTOCOL_VERSION, build: "test".into() };
        std::fs::write(dir.join("sta").join(ENDPOINT_FILE), serde_json::to_string(&e).unwrap()).unwrap();
    }

    /// A fake browser: answers hello with `first`, then every call with `answer(tool)`.
    async fn fake_server(name: String, first: Vec<BrowserMessage>, calls: usize, answer: fn(u64, &str) -> BrowserMessage) -> Vec<BridgeMessage> {
        let server = ServerOptions::new().first_pipe_instance(true).create(&name).unwrap();
        server.connect().await.unwrap();
        let (r, mut w) = tokio::io::split(server);
        let mut lines = BufReader::new(r).lines();
        let mut seen = Vec::new();
        let hello: BridgeMessage = serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
        seen.push(hello);
        for m in first {
            w.write_all(&to_line(&m)).await.unwrap();
        }
        for _ in 0..calls {
            let Some(line) = lines.next_line().await.unwrap() else { break };
            let msg: BridgeMessage = serde_json::from_str(&line).unwrap();
            if let BridgeMessage::Call { id, tool, .. } = &msg {
                w.write_all(&to_line(&answer(*id, tool))).await.unwrap();
            }
            seen.push(msg);
        }
        seen
    }

    fn ok_answer(id: u64, tool: &str) -> BrowserMessage {
        BrowserMessage::Result { id, content: vec![Content::Text { text: format!("ran {tool}") }], structured: Some(serde_json::json!({"ok": true})), error: None }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn forwards_calls_after_welcome() {
        let dir = temp_dir("welcome");
        let name = pipe_name();
        let server = tokio::spawn(fake_server(name.clone(), vec![BrowserMessage::Pending { reason: sta_core::agent::channel::PendingReason::Approval }, BrowserMessage::Welcome { v: 1, session: 3, access: sta_core::AgentAccess::Full, test_hooks: false }], 2, ok_answer));
        tokio::time::sleep(Duration::from_millis(100)).await;
        write_endpoint(&dir, &name);
        let channel = Channel::new(dir.clone(), false);
        channel.set_client("claude-code", Some("Claude Code"), Some("2.1.268"));
        let (content, structured) = channel.call("tabs_list", Value::Null, std::future::pending()).await.expect("call");
        assert_eq!(content, vec![Content::Text { text: "ran tabs_list".into() }]);
        assert_eq!(structured, Some(serde_json::json!({"ok": true})));
        channel.call("page_text", serde_json::json!({"tab": 4}), std::future::pending()).await.expect("second call on the same connection");
        let seen = server.await.unwrap();
        match &seen[0] {
            BridgeMessage::Hello { v, client, .. } => {
                assert_eq!(*v, PROTOCOL_VERSION);
                assert_eq!(client.title.as_deref(), Some("Claude Code"));
            }
            other => panic!("first message {other:?}"),
        }
        assert!(matches!(&seen[2], BridgeMessage::Call { tool, args, .. } if tool == "page_text" && args["tab"] == 4));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn refusals_and_final_bye() {
        let dir = temp_dir("refused");
        let name = pipe_name();
        let server = tokio::spawn(fake_server(name.clone(), vec![BrowserMessage::Refused { code: RefusedCode::AccessOff, message: "off".into() }], 0, ok_answer));
        tokio::time::sleep(Duration::from_millis(100)).await;
        write_endpoint(&dir, &name);
        let channel = Channel::new(dir.clone(), false);
        let e = channel.call("tabs_list", Value::Null, std::future::pending()).await.unwrap_err();
        assert_eq!(e.code, ErrorCode::AccessOff);
        server.await.unwrap();

        // bye{user_stopped}: the call fails and the bridge never reconnects.
        let name = pipe_name();
        let server = tokio::spawn(fake_server(name.clone(), vec![BrowserMessage::Welcome { v: 1, session: 1, access: sta_core::AgentAccess::Full, test_hooks: false }, BrowserMessage::Bye { reason: ByeReason::UserStopped }], 0, ok_answer));
        tokio::time::sleep(Duration::from_millis(100)).await;
        write_endpoint(&dir, &name);
        let channel = Channel::new(dir.clone(), false);
        let first = channel.call("tabs_list", Value::Null, std::future::pending()).await;
        server.await.unwrap();
        // Depending on timing the first call sees the welcome then the closed pipe, or the bye.
        assert!(first.is_err());
        tokio::time::sleep(Duration::from_millis(100)).await;
        let again = channel.call("tabs_list", Value::Null, std::future::pending()).await.unwrap_err();
        assert_eq!(again.code, ErrorCode::Paused, "{again:?}");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn endpoint_candidates_include_the_legacy_layouts() {
        let debug = cfg!(debug_assertions);
        let local = Path::new(r"C:\Users\someone\AppData\Local");
        let default = local.join(DATA_DIR_NAME);
        let legacy_default = local.join(legacy::data_dir_name(debug));
        let at = |base: &Path, profile: &str| base.join(profile).join(ENDPOINT_FILE);
        // The default folder, also as a launcher may spell it: the sta layout first, then the
        // legacy profile subfolder, then the legacy default folder.
        let spelled = PathBuf::from(format!("{}/", default.to_string_lossy().to_uppercase().replace('\\', "/")));
        for dir in [default.clone(), spelled] {
            assert_eq!(
                endpoint_candidates(&dir, Some(&default), debug),
                vec![at(&dir, PROFILE_DIR), at(&dir, legacy::PROFILE_DIR), at(&legacy_default, PROFILE_DIR), at(&legacy_default, legacy::PROFILE_DIR)],
                "{dir:?}"
            );
        }
        // Another data directory (`--data-dir`): only its own profile subfolders.
        let other = Path::new(r"D:\profiles\work");
        assert_eq!(endpoint_candidates(other, Some(&default), debug), vec![at(other, PROFILE_DIR), at(other, legacy::PROFILE_DIR)]);
        assert_eq!(endpoint_candidates(&default, None, debug).len(), 2, "no LOCALAPPDATA: no default folder to compare with");
    }

    /// MANUAL §4 of the rename: a sta that couldn't move the legacy data folder runs it in place
    /// with its old layout, so its endpoint file isn't under the new default folder.
    #[test]
    fn finds_a_sta_that_runs_a_legacy_folder_in_place() {
        let root = temp_dir("legacy");
        let debug = cfg!(debug_assertions);
        let local = root.join("LocalAppData");
        let default = local.join(DATA_DIR_NAME);
        let legacy_default = local.join(legacy::data_dir_name(debug));
        let write = |path: &Path, pipe: &str, pid: u32| {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            let e = Endpoint { pipe: pipe.into(), pid, protocol: PROTOCOL_VERSION, build: "test".into() };
            std::fs::write(path, serde_json::to_string(&e).unwrap()).unwrap();
        };
        let candidates = endpoint_candidates(&default, Some(&default), debug);
        assert!(read_endpoint_from(&candidates, |_| true).is_none(), "nothing written");

        // sta runs the legacy default folder in place, with its legacy profile subfolder.
        let in_place = legacy_default.join(legacy::PROFILE_DIR).join(ENDPOINT_FILE);
        let live = pipe_name();
        write(&in_place, &live, 42);
        assert_eq!(read_endpoint_from(&candidates, |pid| pid == 42).map(|e| e.pipe), Some(live.clone()));
        // A stale file in the new layout (a run that crashed) doesn't hide it, and is picked when
        // no browser runs (the caller relaunches sta).
        let primary = default.join(PROFILE_DIR).join(ENDPOINT_FILE);
        write(&primary, &pipe_name(), 7);
        assert_eq!(read_endpoint_from(&candidates, |pid| pid == 42).map(|e| e.pid), Some(42));
        assert_eq!(read_endpoint_from(&candidates, |_| false).map(|e| e.pid), Some(7));
        // A legacy-layout file naming a pipe sta never creates (a browser from before the rename)
        // is ignored; in the sta layout it is still reported, as untrusted.
        std::fs::remove_file(&primary).unwrap();
        write(&in_place, r"\\.\pipe\not-sta", 42);
        assert!(read_endpoint_from(&candidates, |_| true).is_none());
        write(&primary, r"\\.\pipe\not-sta", 7);
        assert!(read_endpoint_from(&candidates, |_| true).is_some_and(|e| !e.pipe_is_valid()));
        std::fs::remove_file(&primary).unwrap();

        // An explicit data directory whose profile subfolder couldn't be renamed.
        let custom = root.join("custom");
        let custom_candidates = endpoint_candidates(&custom, Some(&default), debug);
        write(&in_place, &live, 42);
        assert!(read_endpoint_from(&custom_candidates, |_| true).is_none(), "the legacy default folder belongs to the default data directory only");
        write(&custom.join(legacy::PROFILE_DIR).join(ENDPOINT_FILE), &live, 43);
        assert_eq!(read_endpoint_from(&custom_candidates, |pid| pid == 43).map(|e| e.pid), Some(43));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn deadlines_follow_the_requested_wait() {
        assert_eq!(deadline_for(&Value::Null), DEFAULT_DEADLINE_MS);
        assert_eq!(deadline_for(&serde_json::json!({"timeoutMs": 60000})), 65_000);
        assert_eq!(deadline_for(&serde_json::json!({"timeMs": 1000})), DEFAULT_DEADLINE_MS);
        assert_eq!(deadline_for(&serde_json::json!({"timeoutMs": 10_000_000})), MAX_DEADLINE_MS);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn missing_or_bad_endpoints() {
        let dir = temp_dir("missing");
        let channel = Channel::new(dir.clone(), false);
        assert_eq!(channel.call("tabs_list", Value::Null, std::future::pending()).await.unwrap_err().code, ErrorCode::BrowserNotRunning);
        write_endpoint(&dir, r"\\.\pipe\not-sta");
        assert_eq!(channel.call("tabs_list", Value::Null, std::future::pending()).await.unwrap_err().code, ErrorCode::EndpointUntrusted);
        write_endpoint(&dir, &pipe_name());
        assert_eq!(channel.call("tabs_list", Value::Null, std::future::pending()).await.unwrap_err().code, ErrorCode::BrowserNotRunning, "no server behind the pipe name");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn cancellation_sends_cancel() {
        let dir = temp_dir("cancel");
        let name = pipe_name();
        let server_name = name.clone();
        let server = tokio::spawn(async move {
            let server = ServerOptions::new().first_pipe_instance(true).create(&server_name).unwrap();
            server.connect().await.unwrap();
            let (r, mut w) = tokio::io::split(server);
            let mut lines = BufReader::new(r).lines();
            let _hello = lines.next_line().await.unwrap();
            w.write_all(&to_line(&BrowserMessage::Welcome { v: 1, session: 1, access: sta_core::AgentAccess::Full, test_hooks: false })).await.unwrap();
            let call: BridgeMessage = serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
            let cancel: BridgeMessage = serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
            (call, cancel)
        });
        tokio::time::sleep(Duration::from_millis(100)).await;
        write_endpoint(&dir, &name);
        let channel = Channel::new(dir.clone(), false);
        let e = channel.call("wait_for", serde_json::json!({"timeMs": 5000}), tokio::time::sleep(Duration::from_millis(300))).await.unwrap_err();
        assert_eq!(e.code, ErrorCode::Timeout);
        match server.await.unwrap() {
            (BridgeMessage::Call { id, .. }, BridgeMessage::Cancel { id: cancelled }) => assert_eq!(id, cancelled),
            other => panic!("{other:?}"),
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A pipe server that accepts one client and returns the bytes it received before EOF. With
    /// `low_label`, the pipe carries a low mandatory label, like a pipe created by a low-integrity
    /// process.
    async fn silent_server(name: String, low_label: bool) -> usize {
        use tokio::io::AsyncReadExt;
        let server = if low_label {
            use windows_sys::Win32::Security::Authorization::ConvertStringSecurityDescriptorToSecurityDescriptorW;
            use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
            let sddl: Vec<u16> = "D:P(A;;GA;;;WD)S:(ML;;NW;;;LW)".encode_utf16().chain(std::iter::once(0)).collect();
            let mut sd = std::ptr::null_mut();
            // SAFETY: a NUL-terminated SDDL string and valid out pointers; the descriptor is leaked
            // for the test's lifetime.
            let ok = unsafe { ConvertStringSecurityDescriptorToSecurityDescriptorW(sddl.as_ptr(), 1, &mut sd, std::ptr::null_mut()) };
            assert_ne!(ok, 0, "SDDL");
            let mut sa = SECURITY_ATTRIBUTES { nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32, lpSecurityDescriptor: sd, bInheritHandle: 0 };
            // SAFETY: `sa` outlives the call.
            unsafe { ServerOptions::new().first_pipe_instance(true).create_with_security_attributes_raw(&name, (&mut sa as *mut SECURITY_ATTRIBUTES).cast()) }.unwrap()
        } else {
            ServerOptions::new().first_pipe_instance(true).create(&name).unwrap()
        };
        server.connect().await.unwrap();
        let mut server = server;
        let mut buf = Vec::new();
        let _ = tokio::time::timeout(Duration::from_secs(5), server.read_to_end(&mut buf)).await;
        buf.len()
    }

    #[tokio::test(flavor = "current_thread")]
    async fn untrusted_pipes_get_no_hello() {
        // A pipe labelled below medium integrity (a squatter running at low integrity).
        let dir = temp_dir("low");
        let name = pipe_name();
        let server = tokio::spawn(silent_server(name.clone(), true));
        tokio::time::sleep(Duration::from_millis(100)).await;
        write_endpoint(&dir, &name);
        let channel = Channel::new(dir.clone(), false);
        let e = channel.call("tabs_list", Value::Null, std::future::pending()).await.unwrap_err();
        assert_eq!(e.code, ErrorCode::EndpointUntrusted, "{e:?}");
        assert!(e.message.contains("integrity"), "{e:?}");
        drop(channel);
        assert_eq!(server.await.unwrap(), 0, "nothing was written to the squatted pipe");

        // A pipe served by another process than the one the endpoint file names.
        let name = pipe_name();
        let server = tokio::spawn(silent_server(name.clone(), false));
        tokio::time::sleep(Duration::from_millis(100)).await;
        use std::os::windows::process::CommandExt;
        let mut other = std::process::Command::new("ping")
            .args(["-n", "6", "127.0.0.1"])
            .stdout(std::process::Stdio::null())
            .creation_flags(0x0800_0000) // CREATE_NO_WINDOW: `cargo test` must not flash a console
            .spawn()
            .unwrap();
        let e = Endpoint { pipe: name.clone(), pid: other.id(), protocol: PROTOCOL_VERSION, build: "test".into() };
        std::fs::write(dir.join("sta").join(ENDPOINT_FILE), serde_json::to_string(&e).unwrap()).unwrap();
        let channel = Channel::new(dir.clone(), false);
        let e = channel.call("tabs_list", Value::Null, std::future::pending()).await.unwrap_err();
        assert_eq!(e.code, ErrorCode::EndpointUntrusted, "{e:?}");
        assert!(e.message.contains("another process"), "{e:?}");
        drop(channel);
        let _ = other.kill();
        let _ = other.wait();
        assert_eq!(server.await.unwrap(), 0);
        let _ = std::fs::remove_dir_all(dir);
    }
}
