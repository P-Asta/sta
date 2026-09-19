//! Unix process and socket identity for the agent channel [owner: automation] (docs/MCP.md
//! "Channel"), the counterpart of `win.rs`.
//!
//! Where Windows names a pipe randomly and protects it with a DACL and an integrity label, Unix
//! puts the socket in the user's own data directory at mode 0600 and asks the kernel who is on the
//! other end: [`peer_uid`] refuses anyone but this user, [`peer_pid`] is the bridge process the
//! endpoint file has to match.
//!
//! Public API:
//! - `pub fn random_hex(n: usize) -> Option<String>` — `2n` hex digits from `arc4random_buf`
//! - `pub fn peer_uid(fd) -> Option<u32>`, `pub fn peer_pid(fd) -> Option<u32>` (`SOL_LOCAL`)
//! - `pub fn parent_process_id(pid) -> Option<u32>`, `pub fn process_image_path(pid) -> Option<String>`

use std::ffi::c_void;
use std::os::fd::RawFd;

/// `2n` lowercase hex digits from the OS CSPRNG (channel names, ref ids).
pub fn random_hex(n: usize) -> Option<String> {
    unsafe extern "C" {
        fn arc4random_buf(buf: *mut c_void, nbytes: usize);
    }
    let mut bytes = vec![0u8; n];
    // SAFETY: `bytes` is `n` writable bytes; `arc4random_buf` cannot fail.
    unsafe { arc4random_buf(bytes.as_mut_ptr().cast(), n) };
    Some(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

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
}

/// The effective user id of the process on the other end.
pub fn peer_uid(fd: RawFd) -> Option<u32> {
    let mut cred = XUCred::default();
    let mut len = std::mem::size_of::<XUCred>() as u32;
    // SAFETY: a live socket descriptor and an out buffer of the size the kernel is told.
    let ok = unsafe { getsockopt(fd, SOL_LOCAL, LOCAL_PEERCRED, std::ptr::from_mut(&mut cred).cast(), &mut len) };
    (ok == 0).then_some(cred.cr_uid)
}

/// The process id on the other end (at the time it connected).
pub fn peer_pid(fd: RawFd) -> Option<u32> {
    let mut pid = 0i32;
    let mut len = std::mem::size_of::<i32>() as u32;
    // SAFETY: a live socket descriptor and an out buffer of the size the kernel is told.
    let ok = unsafe { getsockopt(fd, SOL_LOCAL, LOCAL_PEERPID, std::ptr::from_mut(&mut pid).cast(), &mut len) };
    (ok == 0 && pid > 0).then_some(pid as u32)
}

/// This process's user id (a connection from another user is refused).
pub fn current_uid() -> u32 {
    // SAFETY: no arguments, cannot fail.
    unsafe { getuid() }
}

/// The parent of `pid` — the MCP client that started the bridge.
///
/// Through `ps`: the alternatives are `proc_pidinfo` and `sysctl(KERN_PROC)`, both of which mean
/// spelling out a kernel struct by hand, and this runs once per connection, off the UI thread.
pub fn parent_process_id(pid: u32) -> Option<u32> {
    // console-ok: Unix-only code — there are no console windows to hide here.
    let out = std::process::Command::new("/bin/ps").args(["-o", "ppid=", "-p", &pid.to_string()]).output().ok()?;
    String::from_utf8_lossy(&out.stdout).trim().parse().ok()
}

/// The executable path of a process (`proc_pidpath`, in libSystem).
pub fn process_image_path(pid: u32) -> Option<String> {
    /// `PROC_PIDPATHINFO_MAXSIZE`.
    const MAX_PATH: usize = 4 * 1024;
    unsafe extern "C" {
        fn proc_pidpath(pid: i32, buffer: *mut c_void, buffersize: u32) -> i32;
    }
    let mut buf = vec![0u8; MAX_PATH];
    // SAFETY: `buf` is `MAX_PATH` writable bytes, which is what the call is told.
    let len = unsafe { proc_pidpath(pid as i32, buf.as_mut_ptr().cast(), MAX_PATH as u32) };
    if len <= 0 {
        return None;
    }
    buf.truncate(len as usize);
    String::from_utf8(buf).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn random_hex_is_hex_and_changes() {
        let a = random_hex(16).unwrap();
        assert_eq!(a.len(), 32);
        assert!(a.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()), "{a}");
        assert_ne!(a, random_hex(16).unwrap());
    }

    #[test]
    fn this_process_is_found_by_pid() {
        let me = std::process::id();
        assert!(process_image_path(me).is_some_and(|p| p.contains('/')));
        assert!(parent_process_id(me).is_some());
    }
}
