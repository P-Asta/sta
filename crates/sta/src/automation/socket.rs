//! The agent Unix-domain-socket server [owner: automation] (docs/MCP.md "Channel"), the macOS and
//! Linux counterpart of `pipe.rs` — same module API, same events, same threading rules.
//!
//! - `<data>/sta/agent.sock`, created with mode 0600 inside the user's own data directory, so the
//!   file system keeps other users out (the Windows side needs a DACL and an integrity label for
//!   that). A socket file left behind by a crash is replaced only when nothing is listening on it.
//! - Every accepted connection is checked against `getsockopt(SOL_LOCAL, …)`: another user's
//!   process is dropped before it can write a byte, and the bridge's pid (which the bridge checks
//!   against the endpoint file from its side) comes from the kernel, not from the peer.
//! - Blocking I/O on dedicated threads: one listener, and a reader and a writer per connection.
//!   Threads never touch shell state; every event is posted to the UI thread
//!   (`task::post_ui_from_any_thread`) as `(connection id, PipeEvent)`.
//!
//! Public API:
//! - `pub fn endpoint_name() -> Option<String>` — the socket path to publish and bind
//! - `pub fn start(name: &str, on_event: EventSink) -> Result<Server, String>`; `Server::stop(self)`
//! - `pub fn send(conn: u64, bytes: Vec<u8>) -> bool`, `pub fn close(conn: u64)` (after pending writes)
//! - `pub enum PipeEvent { Connected(ClientIdentity), Data(Vec<u8>), Closed }`

use super::unix;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::Shutdown;
use std::os::fd::AsRawFd;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::JoinHandle;

const BUFFER: usize = 64 * 1024;
/// `sun_path` is 104 bytes on macOS (108 on Linux), NUL included.
const MAX_SOCKET_PATH: usize = 103;

/// Who connected: the bridge process and the process that started it (the MCP client host).
#[derive(Debug, Clone, Default)]
pub struct ClientIdentity {
    pub bridge_pid: u32,
    pub host_pid: Option<u32>,
    pub host_exe: Option<String>,
    /// Always `None` here: the Windows side reads an Authenticode signer, and the macOS equivalent
    /// (a `SecStaticCode` signing identity) is not wired up yet.
    pub host_signer: Option<String>,
}

pub enum PipeEvent {
    Connected(ClientIdentity),
    Data(Vec<u8>),
    Closed,
}

pub type EventSink = Arc<dyn Fn(u64, PipeEvent) + Send + Sync>;

/// Where the socket lives: next to the endpoint file in the profile directory, or — when that path
/// would not fit in `sun_path`, as an `--sta-data-dir` deep in a temporary directory can — in this
/// user's temporary directory under a random name.
pub fn endpoint_name() -> Option<String> {
    let profile = crate::paths::try_dirs().map(|d| d.profile.join(sta_core::agent::channel::SOCKET_FILE));
    if let Some(path) = profile.as_ref().and_then(|p| p.to_str())
        && path.len() <= MAX_SOCKET_PATH
    {
        return Some(path.to_string());
    }
    let short = std::env::temp_dir().join(format!("sta-agent-{}.sock", unix::random_hex(8)?));
    let short = short.to_str().filter(|p| p.len() <= MAX_SOCKET_PATH)?;
    Some(short.to_string())
}

enum WriterMsg {
    Data(Vec<u8>),
    Close,
}

struct Conn {
    tx: Sender<WriterMsg>,
    stream: Arc<UnixStream>,
}

fn conns() -> &'static Mutex<HashMap<u64, Conn>> {
    static CONNS: OnceLock<Mutex<HashMap<u64, Conn>>> = OnceLock::new();
    CONNS.get_or_init(|| Mutex::new(HashMap::new()))
}

static NEXT_CONN: AtomicU64 = AtomicU64::new(1);

pub struct Server {
    stop: Arc<AtomicBool>,
    path: PathBuf,
    thread: Option<JoinHandle<()>>,
}

/// Binds the socket (replacing one no one listens on) and starts accepting.
pub fn start(name: &str, on_event: EventSink) -> Result<Server, String> {
    let path = PathBuf::from(name);
    if path.as_os_str().len() > MAX_SOCKET_PATH {
        return Err(format!("the socket path is too long ({} bytes)", path.as_os_str().len()));
    }
    if path.exists() {
        if UnixStream::connect(&path).is_ok() {
            return Err(format!("{} is already in use", path.display()));
        }
        // Nothing is listening: the file is what a crashed run left behind.
        std::fs::remove_file(&path).map_err(|e| format!("cannot remove the stale socket {}: {e}", path.display()))?;
    }
    let listener = UnixListener::bind(&path).map_err(|e| format!("cannot bind {}: {e}", path.display()))?;
    // The directory already keeps other users out; this makes the socket itself say so too. Every
    // connection is checked against the peer's uid regardless (`accept_loop`).
    if let Err(e) = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)) {
        log_warn!("agent socket: cannot set the mode of {} ({e})", path.display());
    }
    let stop = Arc::new(AtomicBool::new(false));
    let thread = std::thread::Builder::new()
        .name("sta-agent-socket".into())
        .spawn({
            let stop = stop.clone();
            move || accept_loop(listener, stop, on_event)
        })
        .map_err(|e| format!("cannot start the socket thread: {e}"))?;
    Ok(Server { stop, path, thread: Some(thread) })
}

impl Server {
    /// Stops accepting connections (open connections are closed by the caller).
    pub fn stop(mut self) {
        let thread = self.thread.take();
        self.shutdown();
        if let Some(t) = thread {
            let _ = t.join();
        }
    }

    /// Wakes the listener out of `accept` and takes the socket file with it.
    fn shutdown(&self) {
        self.stop.store(true, Ordering::SeqCst);
        // `accept` only returns for a connection, so make one; the listener sees the flag first.
        let _ = UnixStream::connect(&self.path);
        let _ = std::fs::remove_file(&self.path);
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn accept_loop(listener: UnixListener, stop: Arc<AtomicBool>, on_event: EventSink) {
    let our_uid = unix::current_uid();
    for stream in listener.incoming() {
        if stop.load(Ordering::SeqCst) {
            return;
        }
        let stream = match stream {
            Ok(s) => s,
            Err(e) => {
                log_warn!("agent socket: accept failed ({e})");
                continue;
            }
        };
        let fd = stream.as_raw_fd();
        let (peer_uid, peer_pid) = (unix::peer_uid(fd), unix::peer_pid(fd));
        let (Some(uid), Some(pid)) = (peer_uid, peer_pid) else {
            log_warn!("agent socket: a client the kernel would not identify was refused");
            continue;
        };
        if uid != our_uid {
            log_warn!("agent socket: client of another user (uid {uid}, pid {pid}) refused");
            continue;
        }
        let conn_id = NEXT_CONN.fetch_add(1, Ordering::SeqCst);
        if let Err(e) = start_connection(conn_id, stream, pid, on_event.clone()) {
            log_error!("agent socket: cannot start connection threads: {e}");
        }
    }
}

fn start_connection(conn_id: u64, stream: UnixStream, client_pid: u32, on_event: EventSink) -> Result<(), String> {
    let stream = Arc::new(stream);
    let (tx, rx) = channel::<WriterMsg>();
    if let Ok(mut map) = conns().lock() {
        map.insert(conn_id, Conn { tx, stream: stream.clone() });
    }
    let closed = Arc::new(AtomicBool::new(false));
    {
        let (stream, on_event, closed) = (stream.clone(), on_event.clone(), closed.clone());
        std::thread::Builder::new()
            .name("sta-agent-read".into())
            .spawn(move || {
                // The host identity shells out to `ps`: off the listener thread.
                let host_pid = unix::parent_process_id(client_pid);
                let host_exe = host_pid.and_then(unix::process_image_path);
                on_event(conn_id, PipeEvent::Connected(ClientIdentity { bridge_pid: client_pid, host_pid, host_exe, host_signer: None }));
                read_loop(conn_id, &stream, &on_event);
                finish(conn_id, &closed, &on_event);
            })
            .map_err(|e| e.to_string())?;
    }
    std::thread::Builder::new()
        .name("sta-agent-write".into())
        .spawn(move || {
            write_loop(&stream, rx);
            finish(conn_id, &closed, &on_event);
        })
        .map_err(|e| e.to_string())?;
    Ok(())
}

fn finish(conn_id: u64, closed: &AtomicBool, on_event: &EventSink) {
    if closed.swap(true, Ordering::SeqCst) {
        return;
    }
    let conn = conns().lock().ok().and_then(|mut m| m.remove(&conn_id));
    if let Some(conn) = &conn {
        // Wakes the other thread out of its blocking read or write.
        let _ = conn.stream.shutdown(Shutdown::Both);
    }
    drop(conn);
    on_event(conn_id, PipeEvent::Closed);
}

fn read_loop(conn_id: u64, stream: &UnixStream, on_event: &EventSink) {
    let mut reader = stream;
    let mut buf = vec![0u8; BUFFER];
    loop {
        match reader.read(&mut buf) {
            Ok(0) | Err(_) => return,
            Ok(n) => on_event(conn_id, PipeEvent::Data(buf[..n].to_vec())),
        }
    }
}

fn write_loop(stream: &UnixStream, rx: Receiver<WriterMsg>) {
    let mut writer = stream;
    while let Ok(msg) = rx.recv() {
        let WriterMsg::Data(bytes) = msg else { return };
        if writer.write_all(&bytes).is_err() || writer.flush().is_err() {
            return;
        }
    }
}

/// Queues bytes for a connection. `false` when it is gone.
pub fn send(conn: u64, bytes: Vec<u8>) -> bool {
    conns().lock().ok().and_then(|m| m.get(&conn).map(|c| c.tx.send(WriterMsg::Data(bytes)).is_ok())).unwrap_or(false)
}

/// Closes a connection after the writes queued so far.
pub fn close(conn: u64) {
    if let Ok(m) = conns().lock()
        && let Some(c) = m.get(&conn)
    {
        let _ = c.tx.send(WriterMsg::Close);
    }
}

/// Closes every connection now (writes still queued may be lost).
pub fn close_all() {
    let all: Vec<Arc<UnixStream>> = conns().lock().map(|m| m.values().map(|c| c.stream.clone()).collect()).unwrap_or_default();
    for stream in all {
        let _ = stream.shutdown(Shutdown::Both);
    }
}

pub fn connection_count() -> usize {
    conns().lock().map(|m| m.len()).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::Sender as StdSender;
    use std::time::Duration;

    fn temp_socket(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("sta-sock-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("agent.sock")
    }

    /// Collects every event of every connection into a channel.
    fn sink(tx: StdSender<(u64, String)>) -> EventSink {
        Arc::new(move |conn, event| {
            let text = match event {
                PipeEvent::Connected(id) => format!("connected:{}", id.bridge_pid),
                PipeEvent::Data(bytes) => format!("data:{}", String::from_utf8_lossy(&bytes)),
                PipeEvent::Closed => "closed".to_string(),
            };
            let _ = tx.send((conn, text));
        })
    }

    fn next(rx: &Receiver<(u64, String)>) -> (u64, String) {
        rx.recv_timeout(Duration::from_secs(5)).expect("event")
    }

    #[test]
    fn a_client_connects_talks_and_is_closed() {
        let path = temp_socket("talk");
        let (tx, rx) = channel();
        let server = start(path.to_str().unwrap(), sink(tx)).unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);

        let mut client = UnixStream::connect(&path).unwrap();
        // The connection id comes from a counter shared with every other connection this process
        // makes, so the test takes it from the event instead of assuming it.
        let (conn, connected) = next(&rx);
        assert_eq!(connected, format!("connected:{}", std::process::id()));
        client.write_all(b"{\"t\":\"hello\"}\n").unwrap();
        assert_eq!(next(&rx), (conn, "data:{\"t\":\"hello\"}\n".to_string()));

        assert!(send(conn, b"{\"t\":\"welcome\"}\n".to_vec()));
        let mut buf = [0u8; 64];
        let n = client.read(&mut buf).unwrap();
        assert_eq!(&buf[..n], b"{\"t\":\"welcome\"}\n");

        close(conn);
        assert_eq!(next(&rx), (conn, "closed".to_string()));
        server.stop();
        assert!(!path.exists(), "the socket file goes with the server");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn a_socket_left_by_a_crash_is_replaced_but_a_live_one_is_not() {
        let path = temp_socket("stale");
        std::fs::write(&path, b"not a socket").unwrap();
        let (tx, _rx) = channel();
        let server = start(path.to_str().unwrap(), sink(tx)).unwrap();

        let (tx2, _rx2) = channel();
        assert!(start(path.to_str().unwrap(), sink(tx2)).is_err(), "a listening socket is not taken over");
        server.stop();
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }
}
