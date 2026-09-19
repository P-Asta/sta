//! The agent named-pipe server [owner: automation] (docs/MCP.md "Channel").
//!
//! - `\\.\pipe\sta-agent-<random128>`, created with `FILE_FLAG_FIRST_PIPE_INSTANCE` (a squatted
//!   name fails instead of sharing it), `PIPE_REJECT_REMOTE_CLIENTS`, and the security descriptor of
//!   [`super::win::pipe_sddl`] (current user only, medium label denying lower-integrity readers and
//!   writers).
//! - Overlapped I/O on dedicated threads: one listener, and a reader and a writer per connection.
//!   Threads never touch shell state; every event is posted to the UI thread
//!   (`task::post_ui_from_any_thread`) as `(connection id, PipeEvent)`.
//! - A connection whose client runs in another logon session is dropped at once.
//!
//! Public API:
//! - `pub fn endpoint_name() -> Option<String>` — the pipe name to publish and create
//! - `pub fn start(name: &str, on_event: EventSink) -> Result<Server, String>`; `Server::stop(self)`
//! - `pub fn send(conn: u64, bytes: Vec<u8>) -> bool`, `pub fn close(conn: u64)` (after pending writes)
//! - `pub enum PipeEvent { Connected(ClientIdentity), Data(Vec<u8>), Closed }`

use super::win::{self, SecurityDescriptor};
use std::collections::HashMap;
use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::JoinHandle;
use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_IO_PENDING, ERROR_PIPE_CONNECTED, ERROR_PRIVILEGE_NOT_HELD, GetLastError, HANDLE, INVALID_HANDLE_VALUE, WAIT_OBJECT_0,
};
use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
use windows_sys::Win32::Storage::FileSystem::{FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED, PIPE_ACCESS_DUPLEX, ReadFile, WriteFile};
use windows_sys::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, GetNamedPipeClientProcessId, GetNamedPipeClientSessionId, PIPE_READMODE_BYTE,
    PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_WAIT,
};
use windows_sys::Win32::System::Threading::{CreateEventW, INFINITE, SetEvent, WaitForMultipleObjects};

/// Pipe instances at once (2 sessions plus connections still being refused).
const MAX_INSTANCES: u32 = 4;
const BUFFER: u32 = 64 * 1024;

/// Who connected: the bridge process and the process that started it (the MCP client host).
#[derive(Debug, Clone, Default)]
pub struct ClientIdentity {
    pub bridge_pid: u32,
    pub host_pid: Option<u32>,
    pub host_exe: Option<String>,
    /// Authenticode signer of `host_exe` when its signature verifies.
    pub host_signer: Option<String>,
}

pub enum PipeEvent {
    Connected(ClientIdentity),
    Data(Vec<u8>),
    Closed,
}

pub type EventSink = Arc<dyn Fn(u64, PipeEvent) + Send + Sync>;

struct OwnedHandle(HANDLE);
// SAFETY: kernel handles may be used from any thread.
unsafe impl Send for OwnedHandle {}
unsafe impl Sync for OwnedHandle {}

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        if !self.0.is_null() && self.0 != INVALID_HANDLE_VALUE {
            // SAFETY: we own the handle.
            unsafe { CloseHandle(self.0) };
        }
    }
}

fn event() -> Option<Arc<OwnedHandle>> {
    // SAFETY: an anonymous manual-reset event.
    let h = unsafe { CreateEventW(null(), 1, 0, null()) };
    (!h.is_null()).then(|| Arc::new(OwnedHandle(h)))
}

fn set(event: &OwnedHandle) {
    // SAFETY: a valid event handle.
    unsafe { SetEvent(event.0) };
}

/// Waits for the overlapped operation or `stop`. `true` = the operation completed.
fn wait(op_event: HANDLE, stop: &OwnedHandle) -> bool {
    let handles = [op_event, stop.0];
    // SAFETY: both handles are valid for the duration of the wait.
    let r = unsafe { WaitForMultipleObjects(2, handles.as_ptr(), 0, INFINITE) };
    r == WAIT_OBJECT_0
}

enum WriterMsg {
    Data(Vec<u8>),
    Close,
}

struct Conn {
    tx: Sender<WriterMsg>,
    stop: Arc<OwnedHandle>,
    pipe: Arc<OwnedHandle>,
}

fn conns() -> &'static Mutex<HashMap<u64, Conn>> {
    static CONNS: OnceLock<Mutex<HashMap<u64, Conn>>> = OnceLock::new();
    CONNS.get_or_init(|| Mutex::new(HashMap::new()))
}

static NEXT_CONN: AtomicU64 = AtomicU64::new(1);

pub struct Server {
    stop: Arc<OwnedHandle>,
    thread: Option<JoinHandle<()>>,
}

fn create_instance(name: &[u16], sd: &SecurityDescriptor, first: bool) -> Result<OwnedHandle, u32> {
    let sa = SECURITY_ATTRIBUTES { nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32, lpSecurityDescriptor: sd.as_ptr(), bInheritHandle: 0 };
    let open_mode = PIPE_ACCESS_DUPLEX | FILE_FLAG_OVERLAPPED | if first { FILE_FLAG_FIRST_PIPE_INSTANCE } else { 0 };
    let pipe_mode = PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS;
    // SAFETY: valid name and security attributes for the duration of the call.
    let h = unsafe { CreateNamedPipeW(name.as_ptr(), open_mode, pipe_mode, MAX_INSTANCES, BUFFER, BUFFER, 0, &sa) };
    if h == INVALID_HANDLE_VALUE {
        // SAFETY: no arguments.
        return Err(unsafe { GetLastError() });
    }
    Ok(OwnedHandle(h))
}

/// The pipe to publish and create: one instance of a name no one can guess.
pub fn endpoint_name() -> Option<String> {
    Some(format!("{}{}", sta_core::agent::channel::PIPE_PREFIX, win::random_hex(16)?))
}

/// Creates the first pipe instance (failing if the name exists) and starts listening.
pub fn start(name: &str, on_event: EventSink) -> Result<Server, String> {
    let sid = win::current_user_sid().ok_or("cannot read the current user's SID")?;
    let mut sd = SecurityDescriptor::from_sddl(&win::pipe_sddl(&sid)).ok_or("cannot build the pipe security descriptor")?;
    let wide = win::wide(name);
    let first = match create_instance(&wide, &sd, true) {
        Ok(h) => h,
        Err(ERROR_PRIVILEGE_NOT_HELD) => {
            log_warn!("agent pipe: integrity label not allowed here; using the user-only DACL without it");
            sd = SecurityDescriptor::from_sddl(&win::pipe_sddl_without_label(&sid)).ok_or("cannot build the pipe security descriptor")?;
            create_instance(&wide, &sd, true).map_err(|e| format!("CreateNamedPipeW failed ({e})"))?
        }
        Err(e) => return Err(format!("CreateNamedPipeW failed ({e}); is the name in use?")),
    };
    let stop = event().ok_or("CreateEventW failed")?;
    let thread_stop = stop.clone();
    let thread = std::thread::Builder::new()
        .name("sta-agent-pipe".into())
        .spawn(move || listen(wide, sd, first, thread_stop, on_event))
        .map_err(|e| format!("cannot start the pipe thread: {e}"))?;
    Ok(Server { stop, thread: Some(thread) })
}

impl Server {
    /// Stops accepting connections (open connections are closed by the caller).
    pub fn stop(mut self) {
        set(&self.stop);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        set(&self.stop);
    }
}

fn listen(name: Vec<u16>, sd: SecurityDescriptor, first: OwnedHandle, stop: Arc<OwnedHandle>, on_event: EventSink) {
    let mut next = Some(first);
    loop {
        let pipe = match next.take() {
            Some(p) => p,
            None => match create_instance(&name, &sd, false) {
                Ok(p) => p,
                Err(e) => {
                    // All instances busy (or a transient error): retry shortly unless stopping.
                    log_debug!("agent pipe: no free instance ({e})");
                    let Some(timer) = event() else { return };
                    let handles = [stop.0, timer.0];
                    // SAFETY: valid handles; a 250 ms timeout.
                    if unsafe { WaitForMultipleObjects(2, handles.as_ptr(), 0, 250) } == WAIT_OBJECT_0 {
                        return;
                    }
                    continue;
                }
            },
        };
        let Some(connected_event) = event() else { return };
        let mut ov = OVERLAPPED { hEvent: connected_event.0, ..Default::default() };
        // SAFETY: `ov` and its event outlive the operation (it is cancelled before they drop).
        let ok = unsafe { ConnectNamedPipe(pipe.0, &mut ov) };
        let connected = if ok != 0 {
            true
        } else {
            // SAFETY: no arguments.
            match unsafe { GetLastError() } {
                ERROR_PIPE_CONNECTED => true,
                ERROR_IO_PENDING => {
                    if wait(connected_event.0, &stop) {
                        let mut n = 0u32;
                        // SAFETY: the operation completed.
                        (unsafe { GetOverlappedResult(pipe.0, &ov, &mut n, 0) }) != 0
                    } else {
                        // SAFETY: cancel our pending connect before `ov` goes away.
                        unsafe { CancelIoEx(pipe.0, &ov) };
                        let mut n = 0u32;
                        unsafe { GetOverlappedResult(pipe.0, &ov, &mut n, 1) };
                        return;
                    }
                }
                e => {
                    log_warn!("agent pipe: ConnectNamedPipe failed ({e})");
                    false
                }
            }
        };
        if !connected {
            continue;
        }
        let conn_id = NEXT_CONN.fetch_add(1, Ordering::SeqCst);
        if let Err(e) = start_connection(conn_id, pipe, on_event.clone()) {
            log_error!("agent pipe: cannot start connection threads: {e}");
        }
    }
}

fn start_connection(conn_id: u64, pipe: OwnedHandle, on_event: EventSink) -> Result<(), String> {
    let pipe = Arc::new(pipe);
    let mut client_pid = 0u32;
    let mut client_session = 0u32;
    // SAFETY: valid pipe handle and out pointers.
    let (pid_ok, session_ok) = unsafe {
        (GetNamedPipeClientProcessId(pipe.0, &mut client_pid) != 0, GetNamedPipeClientSessionId(pipe.0, &mut client_session) != 0)
    };
    let our_session = win::current_session_id();
    if !pid_ok || !session_ok || Some(client_session) != our_session {
        log_warn!("agent pipe: client from another session (pid {client_pid}, session {client_session}) refused");
        // SAFETY: valid pipe handle.
        unsafe { DisconnectNamedPipe(pipe.0) };
        return Ok(());
    }
    let stop = event().ok_or("CreateEventW failed")?;
    let (tx, rx) = channel::<WriterMsg>();
    if let Ok(mut map) = conns().lock() {
        map.insert(conn_id, Conn { tx, stop: stop.clone(), pipe: pipe.clone() });
    }
    let closed = Arc::new(AtomicBool::new(false));
    {
        let (pipe, stop, on_event, closed) = (pipe.clone(), stop.clone(), on_event.clone(), closed.clone());
        std::thread::Builder::new()
            .name("sta-agent-read".into())
            .spawn(move || {
                // The host identity may take a moment (signature check): off the listener thread.
                let host_pid = win::parent_process_id(client_pid);
                let host_exe = host_pid.and_then(win::process_image_path);
                let host_signer = host_exe.as_deref().and_then(win::authenticode_signer);
                on_event(conn_id, PipeEvent::Connected(ClientIdentity { bridge_pid: client_pid, host_pid, host_exe, host_signer }));
                read_loop(conn_id, &pipe, &stop, &on_event);
                finish(conn_id, &stop, &closed, &on_event);
            })
            .map_err(|e| e.to_string())?;
    }
    std::thread::Builder::new()
        .name("sta-agent-write".into())
        .spawn(move || {
            write_loop(&pipe, &stop, rx);
            finish(conn_id, &stop, &closed, &on_event);
        })
        .map_err(|e| e.to_string())?;
    Ok(())
}

fn finish(conn_id: u64, stop: &OwnedHandle, closed: &AtomicBool, on_event: &EventSink) {
    set(stop);
    if closed.swap(true, Ordering::SeqCst) {
        return;
    }
    let conn = conns().lock().ok().and_then(|mut m| m.remove(&conn_id));
    if let Some(conn) = &conn {
        // Wakes the other thread's pending I/O. No `DisconnectNamedPipe`: it would discard data the
        // client hasn't read yet (a final `bye`); closing the handle keeps it readable.
        // SAFETY: valid handle.
        unsafe { CancelIoEx(conn.pipe.0, null()) };
    }
    drop(conn);
    on_event(conn_id, PipeEvent::Closed);
}

fn read_loop(conn_id: u64, pipe: &OwnedHandle, stop: &OwnedHandle, on_event: &EventSink) {
    let Some(read_event) = event() else { return };
    let mut buf = vec![0u8; BUFFER as usize];
    loop {
        let mut ov = OVERLAPPED { hEvent: read_event.0, ..Default::default() };
        let mut n = 0u32;
        // SAFETY: `buf` and `ov` outlive the operation; it is cancelled before returning.
        let ok = unsafe { ReadFile(pipe.0, buf.as_mut_ptr(), buf.len() as u32, null_mut(), &mut ov) };
        if ok == 0 {
            // SAFETY: no arguments.
            if unsafe { GetLastError() } != ERROR_IO_PENDING {
                return;
            }
            if !wait(read_event.0, stop) {
                // SAFETY: cancel and wait for our own read before `buf`/`ov` go away.
                unsafe {
                    CancelIoEx(pipe.0, &ov);
                    GetOverlappedResult(pipe.0, &ov, &mut n, 1);
                }
                return;
            }
        }
        // SAFETY: the operation completed.
        if unsafe { GetOverlappedResult(pipe.0, &ov, &mut n, 0) } == 0 || n == 0 {
            return;
        }
        on_event(conn_id, PipeEvent::Data(buf[..n as usize].to_vec()));
    }
}

fn write_loop(pipe: &OwnedHandle, stop: &OwnedHandle, rx: Receiver<WriterMsg>) {
    let Some(write_event) = event() else { return };
    while let Ok(msg) = rx.recv() {
        let WriterMsg::Data(bytes) = msg else { return };
        let mut offset = 0usize;
        while offset < bytes.len() {
            let mut ov = OVERLAPPED { hEvent: write_event.0, ..Default::default() };
            let chunk = &bytes[offset..];
            let len = chunk.len().min(1 << 20) as u32;
            let mut n = 0u32;
            // SAFETY: `bytes` and `ov` outlive the operation; it is cancelled before returning.
            let ok = unsafe { WriteFile(pipe.0, chunk.as_ptr(), len, null_mut(), &mut ov) };
            if ok == 0 {
                // SAFETY: no arguments.
                if unsafe { GetLastError() } != ERROR_IO_PENDING {
                    return;
                }
                if !wait(write_event.0, stop) {
                    // SAFETY: cancel and wait for our own write.
                    unsafe {
                        CancelIoEx(pipe.0, &ov);
                        GetOverlappedResult(pipe.0, &ov, &mut n, 1);
                    }
                    return;
                }
            }
            // SAFETY: the operation completed.
            if unsafe { GetOverlappedResult(pipe.0, &ov, &mut n, 0) } == 0 || n == 0 {
                return;
            }
            offset += n as usize;
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
    let all: Vec<(Arc<OwnedHandle>, Arc<OwnedHandle>)> = conns().lock().map(|m| m.values().map(|c| (c.stop.clone(), c.pipe.clone())).collect()).unwrap_or_default();
    for (stop, pipe) in all {
        set(&stop);
        // SAFETY: valid handle.
        unsafe { CancelIoEx(pipe.0, null()) };
    }
}

pub fn connection_count() -> usize {
    conns().lock().map(|m| m.len()).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows_sys::Win32::Foundation::{ERROR_ACCESS_DENIED, GENERIC_READ, GENERIC_WRITE};
    use windows_sys::Win32::Security::Authorization::ConvertStringSidToSidW;
    use windows_sys::Win32::Security::{
        DuplicateTokenEx, ImpersonateLoggedOnUser, RevertToSelf, SID_AND_ATTRIBUTES, SetTokenInformation, TOKEN_ADJUST_DEFAULT, TOKEN_ASSIGN_PRIMARY,
        TOKEN_DUPLICATE, TOKEN_IMPERSONATE, TOKEN_MANDATORY_LABEL, TOKEN_QUERY, SecurityImpersonation, TokenImpersonation, TokenIntegrityLevel,
    };
    use windows_sys::Win32::Storage::FileSystem::{CreateFileW, OPEN_EXISTING};
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    /// `CreateFileW` on the pipe: the open client handle, else the Win32 error.
    fn open(name: &str, access: u32) -> Result<OwnedHandle, u32> {
        let w = win::wide(name);
        // SAFETY: valid name; the handle is owned (closed on drop).
        unsafe {
            let h = CreateFileW(w.as_ptr(), access, 0, null(), OPEN_EXISTING, 0, null_mut());
            if h == INVALID_HANDLE_VALUE {
                return Err(GetLastError());
            }
            Ok(OwnedHandle(h))
        }
    }

    /// Opens and closes right away: `Ok(())` when it opened.
    fn try_open(name: &str, access: u32) -> Result<(), u32> {
        open(name, access).map(drop)
    }

    /// Runs `f` on this thread impersonating the current user at low integrity.
    fn as_low_integrity<R>(f: impl FnOnce() -> R) -> R {
        // SAFETY: token handles are closed; the SID from ConvertStringSidToSidW is freed; the
        // impersonation is reverted before returning.
        unsafe {
            let mut token = null_mut();
            assert!(OpenProcessToken(GetCurrentProcess(), TOKEN_DUPLICATE | TOKEN_QUERY | TOKEN_ADJUST_DEFAULT | TOKEN_ASSIGN_PRIMARY | TOKEN_IMPERSONATE, &mut token) != 0);
            let mut dup = null_mut();
            assert!(DuplicateTokenEx(token, TOKEN_QUERY | TOKEN_ADJUST_DEFAULT | TOKEN_IMPERSONATE, null(), SecurityImpersonation, TokenImpersonation, &mut dup) != 0);
            CloseHandle(token);
            let mut sid = null_mut();
            let low = win::wide("S-1-16-4096");
            assert!(ConvertStringSidToSidW(low.as_ptr(), &mut sid) != 0);
            let label = TOKEN_MANDATORY_LABEL { Label: SID_AND_ATTRIBUTES { Sid: sid, Attributes: 0x20 /* SE_GROUP_INTEGRITY */ } };
            assert!(SetTokenInformation(dup, TokenIntegrityLevel, (&label as *const TOKEN_MANDATORY_LABEL).cast(), std::mem::size_of::<TOKEN_MANDATORY_LABEL>() as u32) != 0);
            assert!(ImpersonateLoggedOnUser(dup) != 0);
            let r = f();
            RevertToSelf();
            windows_sys::Win32::Foundation::LocalFree(sid);
            CloseHandle(dup);
            r
        }
    }

    #[test]
    fn only_the_user_at_medium_integrity_can_open_the_pipe() {
        let name = format!("{}{}", sta_core::agent::channel::PIPE_PREFIX, win::random_hex(16).unwrap());
        let connected = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let seen = connected.clone();
        let sink: EventSink = Arc::new(move |_, e| {
            if matches!(e, PipeEvent::Connected(_)) {
                seen.fetch_add(1, Ordering::SeqCst);
            }
        });
        let server = start(&name, sink.clone()).expect("pipe server");
        // A second server for the same name fails (FILE_FLAG_FIRST_PIPE_INSTANCE: no squatting).
        assert!(start(&name, sink).is_err());
        // Low integrity (same user): denied for read-write and for read-only opens.
        assert_eq!(as_low_integrity(|| try_open(&name, GENERIC_READ | GENERIC_WRITE)), Err(ERROR_ACCESS_DENIED));
        assert_eq!(as_low_integrity(|| try_open(&name, GENERIC_READ)), Err(ERROR_ACCESS_DENIED));
        // The user at medium integrity connects. The client stays open like a real bridge: a client
        // that is already gone when the listener checks its process and session is dropped.
        let client = open(&name, GENERIC_READ | GENERIC_WRITE);
        assert!(client.is_ok(), "open failed: {:?}", client.as_ref().err());
        let wait_for = |n: usize| {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            while connected.load(Ordering::SeqCst) < n && std::time::Instant::now() < deadline {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            connected.load(Ordering::SeqCst)
        };
        assert_eq!(wait_for(1), 1, "the connection reached the event sink");
        // A second client while the first stays connected: the server creates another instance
        // (needs FILE_CREATE_PIPE_INSTANCE in its own descriptor).
        let second = (0..50)
            .find_map(|_| match open(&name, GENERIC_READ | GENERIC_WRITE) {
                Ok(h) => Some(Ok(h)),
                Err(e) if e == windows_sys::Win32::Foundation::ERROR_PIPE_BUSY || e == windows_sys::Win32::Foundation::ERROR_FILE_NOT_FOUND => {
                    std::thread::sleep(std::time::Duration::from_millis(20));
                    None
                }
                Err(e) => Some(Err(e)),
            })
            .unwrap_or(Err(0));
        assert!(second.is_ok(), "second client: {:?}", second.as_ref().err());
        assert_eq!(wait_for(2), 2, "both connections reached the event sink");
        drop(second);
        drop(client);
        close_all();
        server.stop();
    }
}
