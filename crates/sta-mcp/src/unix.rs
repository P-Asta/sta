//! The Unix half of the bridge's channel (docs/MCP.md "Channel"), the counterpart of `win.rs`.
//!
//! The browser's socket lives in the user's own data directory at mode 0600, so the file system
//! keeps other users out. What is left for the bridge to check is that the process on the other end
//! is the one the endpoint file names, and that it belongs to this user — both straight from the
//! kernel (`getsockopt(SOL_LOCAL, …)`), never from anything the peer says.
//!
//! Public API:
//! - `pub fn process_alive(pid: u32) -> bool`
//! - `pub fn peer_pid(fd) -> Option<u32>`, `pub fn peer_uid(fd) -> Option<u32>`, `pub fn current_uid() -> u32`
//! - `pub fn launch(exe: &Path, data_dir: &Path) -> Result<u32, String>`

use std::ffi::c_void;
use std::os::fd::RawFd;
use std::path::Path;

const SOL_LOCAL: i32 = 0;
const LOCAL_PEERCRED: i32 = 0x001;
const LOCAL_PEERPID: i32 = 0x002;

/// `struct xucred` (`sys/ucred.h`): what `LOCAL_PEERCRED` fills in.
#[repr(C)]
#[derive(Default)]
struct XUCred {
    cr_version: u32,
    cr_uid: u32,
    cr_ngroups: i16,
    cr_groups: [u32; 16],
}

unsafe extern "C" {
    fn getsockopt(socket: i32, level: i32, name: i32, value: *mut c_void, len: *mut u32) -> i32;
    fn getuid() -> u32;
    fn kill(pid: i32, sig: i32) -> i32;
}

/// A live process with that id (signal 0 asks without sending anything). A process of another user
/// answers `EPERM`, which still means it exists.
pub fn process_alive(pid: u32) -> bool {
    let Ok(pid) = i32::try_from(pid) else { return false };
    if pid <= 0 {
        return false;
    }
    // SAFETY: `kill` with signal 0 only checks; it changes nothing.
    if unsafe { kill(pid, 0) } == 0 {
        return true;
    }
    std::io::Error::last_os_error().kind() == std::io::ErrorKind::PermissionDenied
}

/// The process id on the other end of a connected socket.
pub fn peer_pid(fd: RawFd) -> Option<u32> {
    let mut pid = 0i32;
    let mut len = std::mem::size_of::<i32>() as u32;
    // SAFETY: a live socket descriptor and an out buffer of the size the kernel is told.
    let ok = unsafe { getsockopt(fd, SOL_LOCAL, LOCAL_PEERPID, std::ptr::from_mut(&mut pid).cast(), &mut len) };
    (ok == 0 && pid > 0).then_some(pid as u32)
}

/// The effective user id on the other end of a connected socket.
pub fn peer_uid(fd: RawFd) -> Option<u32> {
    let mut cred = XUCred::default();
    let mut len = std::mem::size_of::<XUCred>() as u32;
    // SAFETY: a live socket descriptor and an out buffer of the size the kernel is told.
    let ok = unsafe { getsockopt(fd, SOL_LOCAL, LOCAL_PEERCRED, std::ptr::from_mut(&mut cred).cast(), &mut len) };
    (ok == 0).then_some(cred.cr_uid)
}

pub fn current_uid() -> u32 {
    // SAFETY: no arguments, cannot fail.
    unsafe { getuid() }
}

/// Starts the browser with its own data directory, detached from the bridge: its own session (so it
/// does not die with the MCP client's terminal) and no stdio of ours. Returns the pid.
pub fn launch(exe: &Path, data_dir: &Path) -> Result<u32, String> {
    use std::os::unix::process::CommandExt;
    unsafe extern "C" {
        fn setsid() -> i32;
    }

    // console-ok: Unix-only code — there are no console windows to hide here.
    let mut command = std::process::Command::new(exe);
    command
        .arg(format!("--sta-data-dir={}", data_dir.display()))
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .env_remove("STA_E2E");
    for (key, _) in std::env::vars().filter(|(k, _)| k.starts_with("STA_")) {
        command.env_remove(key);
    }
    if let Some(dir) = exe.parent() {
        command.current_dir(dir);
    }
    // SAFETY: `setsid` is async-signal-safe and is all this hook does between fork and exec.
    unsafe {
        command.pre_exec(|| {
            setsid();
            Ok(())
        })
    };
    match command.spawn() {
        Ok(child) => Ok(child.id()),
        Err(e) => Err(format!("could not start sta ({e})")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn this_process_is_alive_and_pid_1_is_not_free() {
        assert!(process_alive(std::process::id()));
        // pid 1 (launchd) belongs to root: `kill` refuses it, which still means "alive".
        assert!(process_alive(1));
        assert!(!process_alive(0));
        // A pid no process can have (the max is 99999 by default).
        assert!(!process_alive(u32::MAX - 1));
    }
}
