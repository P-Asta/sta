//! Win32 helpers for the agent channel [owner: automation]: random pipe names, the current user's
//! SID and the pipe's security descriptor, and the identity of a connecting bridge's host process
//! (image path + Authenticode signer). Blocking calls (`WinVerifyTrust`) run on pipe threads, never
//! on the UI thread.

use std::ffi::c_void;
use std::os::windows::ffi::OsStrExt;
use std::ptr::{null, null_mut};
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE, LocalFree};
use windows_sys::Win32::Security::Authorization::{ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1};
use windows_sys::Win32::Security::Cryptography::{BCRYPT_USE_SYSTEM_PREFERRED_RNG, BCryptGenRandom, CERT_NAME_SIMPLE_DISPLAY_TYPE, CertGetNameStringW};
use windows_sys::Win32::Security::WinTrust::{
    WINTRUST_ACTION_GENERIC_VERIFY_V2, WINTRUST_DATA, WINTRUST_DATA_0, WINTRUST_FILE_INFO, WTD_CACHE_ONLY_URL_RETRIEVAL, WTD_CHOICE_FILE,
    WTD_REVOKE_NONE, WTD_STATEACTION_CLOSE, WTD_STATEACTION_VERIFY, WTD_UI_NONE, WTHelperGetProvSignerFromChain, WTHelperProvDataFromStateData,
    WinVerifyTrust,
};
use windows_sys::Win32::Security::{GetTokenInformation, TOKEN_QUERY, TOKEN_USER, TokenUser};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS};
use windows_sys::Win32::System::RemoteDesktop::ProcessIdToSessionId;
use windows_sys::Win32::System::Threading::{GetCurrentProcess, GetCurrentProcessId, OpenProcess, OpenProcessToken, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW};

pub fn wide(s: &str) -> Vec<u16> {
    std::ffi::OsStr::new(s).encode_wide().chain(std::iter::once(0)).collect()
}

fn from_wide(buf: &[u16]) -> String {
    let len = buf.iter().position(|c| *c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..len])
}

/// `n` bytes from the system CSPRNG as lowercase hex.
pub fn random_hex(n: usize) -> Option<String> {
    let mut buf = vec![0u8; n];
    // SAFETY: the buffer is valid for `n` bytes.
    let status = unsafe { BCryptGenRandom(null_mut(), buf.as_mut_ptr(), n as u32, BCRYPT_USE_SYSTEM_PREFERRED_RNG) };
    (status == 0).then(|| buf.iter().map(|b| format!("{b:02x}")).collect())
}

struct Handle(HANDLE);

impl Drop for Handle {
    fn drop(&mut self) {
        if !self.0.is_null() && self.0 != INVALID_HANDLE_VALUE {
            // SAFETY: we own the handle.
            unsafe { CloseHandle(self.0) };
        }
    }
}

/// The current user's SID as a string (`S-1-5-21-…`).
pub fn current_user_sid() -> Option<String> {
    let mut token: HANDLE = null_mut();
    // SAFETY: plain Win32 calls with valid out pointers; buffers are sized by the first call.
    unsafe {
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return None;
        }
        let token = Handle(token);
        let mut len = 0u32;
        GetTokenInformation(token.0, TokenUser, null_mut(), 0, &mut len);
        if len == 0 {
            return None;
        }
        let mut buf = vec![0u8; len as usize];
        if GetTokenInformation(token.0, TokenUser, buf.as_mut_ptr().cast(), len, &mut len) == 0 {
            return None;
        }
        let user = &*(buf.as_ptr() as *const TOKEN_USER);
        let mut text: *mut u16 = null_mut();
        if ConvertSidToStringSidW(user.User.Sid, &mut text) == 0 || text.is_null() {
            return None;
        }
        let mut n = 0;
        while *text.add(n) != 0 {
            n += 1;
        }
        let s = String::from_utf16_lossy(std::slice::from_raw_parts(text, n));
        LocalFree(text.cast());
        Some(s)
    }
}

/// The pipe's security descriptor: only the current user, with FILE_GENERIC_READ |
/// FILE_GENERIC_WRITE (0x12019f; no WRITE_DAC, WRITE_OWNER or DELETE), and a medium mandatory label
/// that denies lower-integrity processes both writes and reads.
///
/// The ACE includes `FILE_CREATE_PIPE_INSTANCE` (0x4): Windows checks it against this descriptor
/// when the server creates its second and later instances, so without it only one client could be
/// connected at a time (a second agent, or Settings → Test connection while an agent is connected,
/// found no listening instance). Only this user at medium integrity or above can create an
/// instance (the label's no-write-up covers it); same-user processes are outside the threat model
/// (docs/MCP.md), and bridges verify the pipe's owner and server session before writing.
pub fn pipe_sddl(user_sid: &str) -> String {
    format!("D:P(A;;0x12019f;;;{user_sid})S:(ML;;NWNR;;;ME)")
}

/// The same without the integrity label (fallback when a label can't be set).
pub fn pipe_sddl_without_label(user_sid: &str) -> String {
    format!("D:P(A;;0x12019f;;;{user_sid})")
}

/// A self-relative security descriptor parsed from SDDL (freed on drop).
pub struct SecurityDescriptor(*mut c_void);

// SAFETY: the descriptor is immutable after creation and only read by CreateNamedPipeW.
unsafe impl Send for SecurityDescriptor {}
unsafe impl Sync for SecurityDescriptor {}

impl SecurityDescriptor {
    pub fn from_sddl(sddl: &str) -> Option<SecurityDescriptor> {
        let w = wide(sddl);
        let mut sd: *mut c_void = null_mut();
        // SAFETY: valid NUL-terminated string and out pointer.
        let ok = unsafe { ConvertStringSecurityDescriptorToSecurityDescriptorW(w.as_ptr(), SDDL_REVISION_1, &mut sd, null_mut()) };
        (ok != 0 && !sd.is_null()).then_some(SecurityDescriptor(sd))
    }

    pub fn as_ptr(&self) -> *mut c_void {
        self.0
    }
}

impl Drop for SecurityDescriptor {
    fn drop(&mut self) {
        // SAFETY: allocated by ConvertStringSecurityDescriptorToSecurityDescriptorW.
        unsafe { LocalFree(self.0) };
    }
}

pub fn process_session_id(pid: u32) -> Option<u32> {
    let mut session = 0u32;
    // SAFETY: valid out pointer.
    (unsafe { ProcessIdToSessionId(pid, &mut session) } != 0).then_some(session)
}

pub fn current_session_id() -> Option<u32> {
    // SAFETY: no arguments.
    process_session_id(unsafe { GetCurrentProcessId() })
}

/// Parent process id of `pid` (toolhelp snapshot; the parent may have exited and its id reused).
pub fn parent_process_id(pid: u32) -> Option<u32> {
    // SAFETY: snapshot handle closed by `Handle`; the entry struct is sized.
    unsafe {
        let snap = Handle(CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0));
        if snap.0 == INVALID_HANDLE_VALUE {
            return None;
        }
        let mut entry = PROCESSENTRY32W { dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
        let mut ok = Process32FirstW(snap.0, &mut entry);
        while ok != 0 {
            if entry.th32ProcessID == pid {
                return Some(entry.th32ParentProcessID);
            }
            ok = Process32NextW(snap.0, &mut entry);
        }
        None
    }
}

/// Full image path of a process.
pub fn process_image_path(pid: u32) -> Option<String> {
    // SAFETY: the handle is closed by `Handle`; the buffer length is passed in and out.
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if h.is_null() {
            return None;
        }
        let h = Handle(h);
        let mut buf = vec![0u16; 32768];
        let mut len = buf.len() as u32;
        if QueryFullProcessImageNameW(h.0, PROCESS_NAME_WIN32, buf.as_mut_ptr(), &mut len) == 0 {
            return None;
        }
        Some(String::from_utf16_lossy(&buf[..len as usize]))
    }
}

/// The Authenticode signer (certificate display name) of `path` when its signature verifies;
/// `None` for unsigned or invalid files. No revocation check over the network.
pub fn authenticode_signer(path: &str) -> Option<String> {
    let w = wide(path);
    let mut file = WINTRUST_FILE_INFO { cbStruct: std::mem::size_of::<WINTRUST_FILE_INFO>() as u32, pcwszFilePath: w.as_ptr(), hFile: null_mut(), pgKnownSubject: null_mut() };
    let mut data = WINTRUST_DATA {
        cbStruct: std::mem::size_of::<WINTRUST_DATA>() as u32,
        pPolicyCallbackData: null_mut(),
        pSIPClientData: null_mut(),
        dwUIChoice: WTD_UI_NONE,
        fdwRevocationChecks: WTD_REVOKE_NONE,
        dwUnionChoice: WTD_CHOICE_FILE,
        Anonymous: WINTRUST_DATA_0 { pFile: &mut file },
        dwStateAction: WTD_STATEACTION_VERIFY,
        hWVTStateData: null_mut(),
        pwszURLReference: null_mut(),
        dwProvFlags: WTD_CACHE_ONLY_URL_RETRIEVAL,
        dwUIContext: 0,
        pSignatureSettings: null_mut(),
    };
    let mut action = WINTRUST_ACTION_GENERIC_VERIFY_V2;
    // SAFETY: the structures outlive both WinVerifyTrust calls; provider data is only read while
    // the state is open, and the state is always closed.
    unsafe {
        let status = WinVerifyTrust(INVALID_HANDLE_VALUE, &mut action, (&mut data as *mut WINTRUST_DATA).cast());
        let mut signer = None;
        if status == 0 {
            let prov = WTHelperProvDataFromStateData(data.hWVTStateData);
            if !prov.is_null() {
                let sgnr = WTHelperGetProvSignerFromChain(prov, 0, 0, 0);
                if !sgnr.is_null() && (*sgnr).csCertChain > 0 && !(*sgnr).pasCertChain.is_null() {
                    let cert = (*(*sgnr).pasCertChain).pCert;
                    if !cert.is_null() {
                        let mut buf = vec![0u16; 512];
                        let n = CertGetNameStringW(cert, CERT_NAME_SIMPLE_DISPLAY_TYPE, 0, null(), buf.as_mut_ptr(), buf.len() as u32);
                        if n > 1 {
                            signer = Some(from_wide(&buf));
                        }
                    }
                }
            }
        }
        data.dwStateAction = WTD_STATEACTION_CLOSE;
        WinVerifyTrust(INVALID_HANDLE_VALUE, &mut action, (&mut data as *mut WINTRUST_DATA).cast());
        signer.filter(|s| !s.trim().is_empty())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sddl_strings() {
        let sid = "S-1-5-21-1-2-3-1001";
        assert_eq!(pipe_sddl(sid), "D:P(A;;0x12019f;;;S-1-5-21-1-2-3-1001)S:(ML;;NWNR;;;ME)");
        // 0x12019f = SYNCHRONIZE | READ_CONTROL | FILE_WRITE_ATTRIBUTES | FILE_READ_ATTRIBUTES |
        // FILE_WRITE_EA | FILE_READ_EA | FILE_CREATE_PIPE_INSTANCE (= FILE_APPEND_DATA) |
        // FILE_WRITE_DATA | FILE_READ_DATA: FILE_GENERIC_READ | FILE_GENERIC_WRITE. GA/GW would add
        // WRITE_DAC and friends.
        let mask = 0x12019fu32;
        assert_eq!(mask & 0x4, 0x4, "FILE_CREATE_PIPE_INSTANCE: the server's later instances");
        assert_eq!(mask & 0x0004_0000, 0, "no WRITE_DAC");
        assert_eq!(mask & 0x0008_0000, 0, "no WRITE_OWNER");
        assert_eq!(mask & 0x0001_0000, 0, "no DELETE");
        let me = current_user_sid().expect("user sid");
        assert!(me.starts_with("S-1-"), "{me}");
        assert!(SecurityDescriptor::from_sddl(&pipe_sddl(&me)).is_some());
        assert!(SecurityDescriptor::from_sddl("not sddl").is_none());
    }

    #[test]
    fn randomness_and_process_identity() {
        let a = random_hex(16).unwrap();
        let b = random_hex(16).unwrap();
        assert_eq!(a.len(), 32);
        assert_ne!(a, b);
        let me = std::process::id();
        assert!(process_image_path(me).is_some_and(|p| p.to_ascii_lowercase().ends_with(".exe")));
        assert!(parent_process_id(me).is_some());
        assert_eq!(process_session_id(me), current_session_id());
        // A test binary isn't signed.
        assert_eq!(authenticode_signer(&process_image_path(me).unwrap()), None);
    }
}
