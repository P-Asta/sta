//! Windows checks and the browser launch (docs/MCP.md "Channel").
//!
//! - Before the bridge writes anything to the pipe, the pipe's **owner** must be the current user,
//!   its server must run in the bridge's logon session, the pipe object must be labelled at medium
//!   integrity or above (not created from a sandbox or a low-integrity process), and its server
//!   process must be the one the endpoint file names; otherwise another user, a service or a
//!   low-integrity process squatted the name (`endpoint_untrusted`).
//! - The pipe is opened with `SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION`, so the server can
//!   identify but never impersonate the bridge's token.
//! - sta is launched only as the sibling `sta.exe`, with `--sta-data-dir`, no inherited
//!   handles, without `STA_*` environment variables, broken away from the client's job and with
//!   `CREATE_NO_WINDOW` so neither it nor its CEF subprocesses can put a console window on screen.
//!   A packaged (MSIX) bridge never launches it.

use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::RawHandle;
use std::path::Path;
use std::ptr::{null, null_mut};
use windows_sys::Win32::Foundation::{APPMODEL_ERROR_NO_PACKAGE, CloseHandle, ERROR_INSUFFICIENT_BUFFER, HANDLE, LocalFree};
use windows_sys::Win32::Security::Authorization::{GetSecurityInfo, SE_KERNEL_OBJECT};
use windows_sys::Win32::Security::{
    EqualSid, GetAce, GetSidSubAuthority, GetSidSubAuthorityCount, GetTokenInformation, LABEL_SECURITY_INFORMATION, OWNER_SECURITY_INFORMATION, PSID,
    SYSTEM_MANDATORY_LABEL_ACE, TOKEN_QUERY, TOKEN_USER, TokenUser,
};
use windows_sys::Win32::Storage::Packaging::Appx::GetCurrentPackageFullName;
use windows_sys::Win32::System::Pipes::{GetNamedPipeServerProcessId, GetNamedPipeServerSessionId};
use windows_sys::Win32::System::RemoteDesktop::ProcessIdToSessionId;
use windows_sys::Win32::System::Threading::{
    CREATE_BREAKAWAY_FROM_JOB, CREATE_NEW_PROCESS_GROUP, CREATE_NO_WINDOW, CREATE_UNICODE_ENVIRONMENT, CreateProcessW, GetCurrentProcess,
    GetCurrentProcessId, GetExitCodeProcess, OpenProcess, OpenProcessToken, PROCESS_INFORMATION, PROCESS_QUERY_LIMITED_INFORMATION, STARTUPINFOW,
};

/// `SECURITY_SQOS_PRESENT` (`Win32_Storage_FileSystem`).
pub const SECURITY_SQOS_PRESENT: u32 = 0x0010_0000;
/// `SECURITY_IDENTIFICATION` (`SecurityIdentification << 16`).
pub const SECURITY_IDENTIFICATION: u32 = 0x0001_0000;

fn wide(s: &str) -> Vec<u16> {
    std::ffi::OsStr::new(s).encode_wide().chain(std::iter::once(0)).collect()
}

/// The TOKEN_USER buffer of this process (holds the SID).
fn token_user() -> Option<Vec<u8>> {
    let mut token: HANDLE = null_mut();
    // SAFETY: plain Win32 calls with valid out pointers; the token handle is closed.
    unsafe {
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return None;
        }
        let mut len = 0u32;
        GetTokenInformation(token, TokenUser, null_mut(), 0, &mut len);
        let mut buf = vec![0u8; len.max(1) as usize];
        let ok = GetTokenInformation(token, TokenUser, buf.as_mut_ptr().cast(), len, &mut len) != 0;
        CloseHandle(token);
        ok.then_some(buf)
    }
}

/// The pipe's owner is the current user.
pub fn pipe_owner_is_current_user(pipe: RawHandle) -> bool {
    let Some(user) = token_user() else { return false };
    let mut owner = null_mut();
    let mut sd = null_mut();
    // SAFETY: `pipe` is an open handle; `sd` is freed with LocalFree; `owner` points into it.
    unsafe {
        let err = GetSecurityInfo(pipe as HANDLE, SE_KERNEL_OBJECT, OWNER_SECURITY_INFORMATION, &mut owner, null_mut(), null_mut(), null_mut(), &mut sd);
        if err != 0 || owner.is_null() {
            return false;
        }
        let me = &*(user.as_ptr() as *const TOKEN_USER);
        let same = EqualSid(owner, me.User.Sid) != 0;
        LocalFree(sd);
        same
    }
}

/// The pipe server runs in this process's logon session.
pub fn pipe_server_in_our_session(pipe: RawHandle) -> bool {
    let mut server = u32::MAX;
    let mut ours = 0u32;
    // SAFETY: valid handle and out pointers.
    unsafe { GetNamedPipeServerSessionId(pipe as HANDLE, &mut server) != 0 && ProcessIdToSessionId(GetCurrentProcessId(), &mut ours) != 0 && server == ours }
}

/// The process id of the pipe's server.
pub fn pipe_server_pid(pipe: RawHandle) -> Option<u32> {
    let mut pid = 0u32;
    // SAFETY: valid handle and out pointer.
    (unsafe { GetNamedPipeServerProcessId(pipe as HANDLE, &mut pid) } != 0).then_some(pid)
}

/// `SYSTEM_MANDATORY_LABEL_ACE_TYPE` (`Win32_System_SystemServices`).
const SYSTEM_MANDATORY_LABEL_ACE_TYPE: u8 = 0x11;
/// `SECURITY_MANDATORY_MEDIUM_RID`.
pub const MANDATORY_MEDIUM_RID: u32 = 0x2000;

/// The integrity level (mandatory label RID) of the pipe object. An object without a label counts
/// as medium; an object created by a low-integrity process always carries its creator's low label,
/// and no process can label an object above its own level, so a pipe squatted from a sandbox or a
/// low-integrity process reads below [`MANDATORY_MEDIUM_RID`]. `None` when it can't be read.
pub fn pipe_integrity_rid(pipe: RawHandle) -> Option<u32> {
    let mut sacl = null_mut();
    let mut sd = null_mut();
    // SAFETY: `pipe` is an open handle (opened for read, which includes READ_CONTROL); `sd` is freed
    // with LocalFree and `sacl` and the ACEs point into it.
    unsafe {
        if GetSecurityInfo(pipe as HANDLE, SE_KERNEL_OBJECT, LABEL_SECURITY_INFORMATION, null_mut(), null_mut(), null_mut(), &mut sacl, &mut sd) != 0 {
            return None;
        }
        let mut rid = MANDATORY_MEDIUM_RID;
        if !sacl.is_null() {
            for i in 0..(*sacl).AceCount as u32 {
                let mut ace = null_mut();
                if GetAce(sacl, i, &mut ace) == 0 || ace.is_null() {
                    continue;
                }
                let label = &*(ace as *const SYSTEM_MANDATORY_LABEL_ACE);
                if label.Header.AceType != SYSTEM_MANDATORY_LABEL_ACE_TYPE {
                    continue;
                }
                let sid = &label.SidStart as *const u32 as PSID;
                let count = *GetSidSubAuthorityCount(sid);
                if count > 0 {
                    rid = *GetSidSubAuthority(sid, count as u32 - 1);
                }
                break;
            }
        }
        LocalFree(sd);
        Some(rid)
    }
}

/// The process `pid` is running.
pub fn process_alive(pid: u32) -> bool {
    const STILL_ACTIVE: u32 = 259;
    // SAFETY: the handle is closed; valid out pointer.
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if h.is_null() {
            return false;
        }
        let mut code = 0u32;
        let ok = GetExitCodeProcess(h, &mut code) != 0;
        CloseHandle(h);
        ok && code == STILL_ACTIVE
    }
}

/// The bridge runs with package identity (e.g. started by an MSIX-packaged client): launching the
/// browser from inside the package container is not attempted.
pub fn has_package_identity() -> bool {
    let mut len = 0u32;
    // SAFETY: a zero-length query.
    let r = unsafe { GetCurrentPackageFullName(&mut len, null_mut()) };
    r != APPMODEL_ERROR_NO_PACKAGE && (r == ERROR_INSUFFICIENT_BUFFER || r == 0)
}

/// `"exe" --sta-data-dir="<dir>"` with Windows command-line quoting.
pub fn launch_command_line(exe: &Path, data_dir: &Path) -> String {
    fn quote(arg: &str) -> String {
        let mut out = String::from("\"");
        let mut backslashes = 0;
        for c in arg.chars() {
            match c {
                '\\' => backslashes += 1,
                '"' => {
                    out.push_str(&"\\".repeat(backslashes * 2 + 1));
                    out.push('"');
                    backslashes = 0;
                }
                _ => {
                    out.push_str(&"\\".repeat(backslashes));
                    out.push(c);
                    backslashes = 0;
                }
            }
        }
        out.push_str(&"\\".repeat(backslashes * 2));
        out.push('"');
        out
    }
    format!("{} {}", quote(&exe.to_string_lossy()), quote(&format!("--sta-data-dir={}", data_dir.to_string_lossy())))
}

/// The environment block for the browser: this process's environment without `STA_*`.
pub fn launch_environment() -> Vec<u16> {
    let mut block = Vec::new();
    let mut vars: Vec<(String, String)> = std::env::vars().filter(|(k, _)| !k.to_ascii_uppercase().starts_with("STA_")).collect();
    vars.sort_by_key(|(k, _)| k.to_ascii_uppercase());
    for (k, v) in vars {
        block.extend(format!("{k}={v}").encode_utf16());
        block.push(0);
    }
    block.push(0);
    block
}

/// Starts `exe` with no console window and no inherited handles, broken away from the job, in its
/// own process group. Returns the pid.
///
/// `CREATE_NO_WINDOW` — **not** `DETACHED_PROCESS`. A debug `sta.exe` is a console-subsystem binary
/// (`main.rs` only asks for the Windows subsystem in release), and a child that is given no console
/// at all makes Windows allocate a fresh one for it and for every CEF subprocess — which Windows 11
/// hands to Windows Terminal, i.e. ~10 visible console windows per launch. With `CREATE_NO_WINDOW`
/// the browser gets one console that never has a window and every child inherits it. The two flags
/// are mutually exclusive (`CREATE_NO_WINDOW` is ignored next to `DETACHED_PROCESS`), so this is a
/// swap, not an addition; the browser is still detached from *our* console, since the flag allocates
/// a new one rather than inheriting the bridge's.
pub fn launch(exe: &Path, data_dir: &Path) -> Result<u32, String> {
    let app = wide(&exe.to_string_lossy());
    let mut cmd = wide(&launch_command_line(exe, data_dir));
    let env = launch_environment();
    let cwd = exe.parent().map(|p| wide(&p.to_string_lossy()));
    let si = STARTUPINFOW { cb: std::mem::size_of::<STARTUPINFOW>() as u32, ..Default::default() };
    let mut pi = PROCESS_INFORMATION::default();
    let flags = CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP | CREATE_BREAKAWAY_FROM_JOB | CREATE_UNICODE_ENVIRONMENT;
    // SAFETY: all buffers are NUL-terminated and outlive the call; handles in `pi` are closed.
    unsafe {
        let ok = CreateProcessW(
            app.as_ptr(),
            cmd.as_mut_ptr(),
            null(),
            null(),
            0,
            flags,
            env.as_ptr().cast(),
            cwd.as_ref().map_or(null(), |c| c.as_ptr()),
            &si,
            &mut pi,
        );
        if ok == 0 {
            let e = std::io::Error::last_os_error();
            return Err(format!("could not start sta ({e})"));
        }
        CloseHandle(pi.hThread);
        CloseHandle(pi.hProcess);
        Ok(pi.dwProcessId)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sqos_flags_match_the_sdk_values() {
        assert_eq!(SECURITY_SQOS_PRESENT, windows_sys_sqos().0);
        assert_eq!(SECURITY_IDENTIFICATION, windows_sys_sqos().1);
    }

    fn windows_sys_sqos() -> (u32, u32) {
        // SecurityIdentification = 1, shifted into the SQOS bits.
        (0x0010_0000, 1 << 16)
    }

    #[test]
    fn command_line_quoting() {
        let line = launch_command_line(Path::new(r"C:\Program Files\sta\sta.exe"), Path::new(r"C:\Users\me\data dir\"));
        assert_eq!(line, r#""C:\Program Files\sta\sta.exe" "--sta-data-dir=C:\Users\me\data dir\\""#);
    }

    #[test]
    fn environment_drops_sta_variables() {
        // SAFETY: tests in this module don't read these variables concurrently.
        unsafe {
            std::env::set_var("STA_REMOTE_DEBUGGING_PORT", "9222");
            std::env::set_var("STA_DATA_DIR", r"C:\elsewhere");
            std::env::set_var("Sta_Test_Mixed_Case", "1");
            std::env::set_var("KEEP_STA_TEST_VALUE", r"C:\work\sta_files");
        }
        let block = String::from_utf16_lossy(&launch_environment());
        assert!(block.ends_with("\0\0"));
        // Only names count: values (paths such as `…\sta_mcp-….exe`) may contain "sta_". A name
        // can start with '=' (`=C:=C:\…`), so the separator is the first '=' after the first char.
        let names: Vec<&str> = block
            .split('\0')
            .filter(|entry| !entry.is_empty())
            .map(|entry| entry.char_indices().skip(1).find(|&(_, c)| c == '=').map_or(entry, |(i, _)| &entry[..i]))
            .collect();
        let leaked: Vec<&&str> = names.iter().filter(|n| n.to_ascii_uppercase().starts_with("STA_")).collect();
        assert!(leaked.is_empty(), "STA_ variables passed to the browser: {leaked:?}");
        assert!(names.contains(&"KEEP_STA_TEST_VALUE"), "a variable that only mentions sta_ in its value is kept");
        assert!(!has_package_identity(), "tests don't run packaged");
    }

    #[test]
    fn process_liveness() {
        assert!(process_alive(std::process::id()));
        assert!(!process_alive(0xFFFF_FFF0));
    }
}
