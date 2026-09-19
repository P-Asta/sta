//! Console windows on the desktop, for `test_console_windows` (design §7.3) — the runtime half of
//! the user's "no cmd window while testing" requirement.
//!
//! A source-level rule (`tools/check-no-console.mjs`) cannot catch a console opened by a child of a
//! child, and polling cannot catch one that **flashes** and is gone again before the next poll. So
//! a system-wide `EVENT_OBJECT_SHOW` hook (out of context, installed on the UI thread) appends
//! every console-class window the moment it is shown, with a timestamp, and the tool reports:
//!
//! - `current` — console windows visible right now (`EnumWindows` over the desktop), each with
//!   `userVisible`, since an `IsWindowVisible` `PseudoConsoleWindow` is still nothing on screen;
//! - `seen` — every console window shown since arming or the last `reset`, flashes included;
//! - `ours` — the subset of `seen` this process's tree can be shown to be responsible for.
//!
//! **`ours` is a hint, never the whole assertion.** Two things make attribution unreliable, and both
//! are why the suites assert on `seen`:
//!
//! 1. a helper that flashes a console and exits has no process entry any more, so its ancestry
//!    cannot be walked afterwards — the chain is therefore resolved *inside the hook*, while the
//!    process is still alive, and stored on the entry;
//! 2. under the Windows 11 default terminal the console window belongs to `WindowsTerminal.exe` /
//!    `OpenConsole.exe` (children of `svchost.exe`), never to the client that wanted the console.
//!    Such a window is marked `hosted`, and the only link back to the client is the window title,
//!    which Terminal sets to the client's image path — matched against our own path below.
//!
//! `userVisible` separates the two classes the user actually sees from `PseudoConsoleWindow`, the
//! internal host window of a pseudo console (which a hidden ConPTY creates too).
//!
//! Only window class, title, pid and handle are read; nothing is captured or closed.

use serde_json::{Value, json};
use std::sync::{Mutex, OnceLock};

/// Window classes of a Windows console: the classic conhost window, a pseudo console, and Windows
/// Terminal's host window. (Only the Windows hook below looks at them — everything in this file
/// that reads the desktop has an inert stand-in elsewhere, since there are no console windows to
/// watch for.)
#[cfg_attr(not(windows), allow(dead_code))]
const CONSOLE_CLASSES: [&str; 3] = ["ConsoleWindowClass", "PseudoConsoleWindow", "CASCADIA_HOSTING_WINDOW_CLASS"];

/// Window classes the *user* sees. `PseudoConsoleWindow` is the internal host window of a pseudo
/// console — a hidden ConPTY (every `windowsHide` spawn, every VS Code terminal) creates one, so it
/// is recorded but never counted as a console window on the desktop.
const USER_VISIBLE_CLASSES: [&str; 2] = ["ConsoleWindowClass", "CASCADIA_HOSTING_WINDOW_CLASS"];

#[derive(Debug, Clone)]
struct Seen {
    hwnd: isize,
    pid: u32,
    class: String,
    title: String,
    shown_at: u64,
    /// The owning process and its ancestors, resolved in the hook **while they are still alive**
    /// (a flash is gone by the time anyone asks) — youngest first.
    chain: Vec<u32>,
}

fn seen_list() -> &'static Mutex<Vec<Seen>> {
    static SEEN: OnceLock<Mutex<Vec<Seen>>> = OnceLock::new();
    SEEN.get_or_init(|| Mutex::new(Vec::new()))
}

fn started() -> std::time::Instant {
    static START: OnceLock<std::time::Instant> = OnceLock::new();
    *START.get_or_init(std::time::Instant::now)
}

fn now_ms() -> u64 {
    started().elapsed().as_millis() as u64
}

fn entry(s: &Seen, roots: &[u32]) -> Value {
    json!({
        "hwnd": s.hwnd,
        "pid": s.pid,
        "className": s.class,
        "title": s.title,
        "shownAt": s.shown_at,
        "chain": s.chain,
        "userVisible": user_visible(&s.class),
        "hosted": s.class != "ConsoleWindowClass",
        "ours": attributed(s, roots),
    })
}

fn user_visible(class: &str) -> bool {
    USER_VISIBLE_CLASSES.contains(&class)
}

/// This process's own image path, lowercased — what the default terminal puts in the title bar of a
/// console window it hosts for us.
fn our_exe() -> &'static str {
    static EXE: OnceLock<String> = OnceLock::new();
    EXE.get_or_init(|| std::env::current_exe().map(|p| p.to_string_lossy().to_ascii_lowercase()).unwrap_or_default())
}

/// `s` is a console window one of `roots` is responsible for: either the window's owning process
/// descends from a root (the conhost case), or it is hosted by the default terminal and titled with
/// our own image path (the Windows 11 handoff, where the owner is `WindowsTerminal.exe`).
fn attributed(s: &Seen, roots: &[u32]) -> bool {
    if s.chain.iter().any(|p| roots.contains(p)) {
        return true;
    }
    let exe = our_exe();
    !exe.is_empty() && s.title.to_ascii_lowercase().contains(exe)
}

/// Forgets everything seen so far (the baseline a suite takes at startup).
pub fn reset() {
    if let Ok(mut seen) = seen_list().lock() {
        seen.clear();
    }
}

/// `{current, seen, ours, watching, at}` for `test_console_windows`.
pub fn snapshot(roots: &[u32]) -> Value {
    let seen: Vec<Seen> = seen_list().lock().map(|s| s.clone()).unwrap_or_default();
    json!({
        "current": current(),
        "seen": seen.iter().map(|s| entry(s, roots)).collect::<Vec<Value>>(),
        "ours": seen.iter().filter(|s| attributed(s, roots)).map(|s| entry(s, roots)).collect::<Vec<Value>>(),
        "watching": installed(),
        "at": now_ms(),
    })
}

#[cfg(windows)]
mod sys {
    use super::{CONSOLE_CLASSES, Seen, now_ms, seen_list};
    use serde_json::{Value, json};
    use std::sync::atomic::{AtomicIsize, Ordering};
    use windows_sys::Win32::Foundation::{HWND, LPARAM};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
    };
    use windows_sys::Win32::System::Threading::GetCurrentProcessId;
    use windows_sys::Win32::UI::Accessibility::{HWINEVENTHOOK, SetWinEventHook, UnhookWinEvent};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        EVENT_OBJECT_SHOW, EnumWindows, GetClassNameW, GetWindowThreadProcessId, InternalGetWindowText, IsWindowVisible, OBJID_WINDOW, WINEVENT_OUTOFCONTEXT,
    };

    static HOOK: AtomicIsize = AtomicIsize::new(0);

    fn class_name(hwnd: HWND) -> String {
        let mut buf = [0u16; 128];
        // SAFETY: correctly sized buffer.
        let n = unsafe { GetClassNameW(hwnd, buf.as_mut_ptr(), buf.len() as i32) }.max(0) as usize;
        String::from_utf16_lossy(&buf[..n])
    }

    fn title(hwnd: HWND) -> String {
        let mut buf = [0u16; 160];
        // SAFETY: correctly sized buffer; InternalGetWindowText sends no WM_GETTEXT, so another
        // process's window can never make us wait.
        let n = unsafe { InternalGetWindowText(hwnd, buf.as_mut_ptr(), buf.len() as i32) }.max(0) as usize;
        String::from_utf16_lossy(&buf[..n])
    }

    fn pid_of(hwnd: HWND) -> u32 {
        let mut pid = 0u32;
        // SAFETY: plain Win32 call with a valid out pointer.
        unsafe { GetWindowThreadProcessId(hwnd, &mut pid) };
        pid
    }

    fn is_console(class: &str) -> bool {
        CONSOLE_CLASSES.contains(&class)
    }

    unsafe extern "system" fn on_show(_hook: HWINEVENTHOOK, _event: u32, hwnd: HWND, id_object: i32, id_child: i32, _thread: u32, _time: u32) {
        if id_object != OBJID_WINDOW || id_child != 0 || hwnd.is_null() {
            return;
        }
        record(hwnd as isize, pid_of(hwnd), class_name(hwnd), title(hwnd));
    }

    /// Everything the hook does once Win32 has answered — a plain function, so the whole recording
    /// path (class filter, ancestry, attribution, `snapshot`) is unit-testable without asking the OS
    /// to put a console window on anybody's desktop.
    pub fn record(hwnd: isize, pid: u32, class: String, title: String) {
        if !is_console(&class) {
            return;
        }
        // The ancestry is resolved here, not in `snapshot`: a helper that flashes a console and
        // exits at once is gone from every later process snapshot, and an unknown pid is nobody's
        // child — which is how the flash this hook exists to catch used to drop out of `ours`.
        let seen = Seen { hwnd, pid, class, title, shown_at: now_ms(), chain: super::chain_of(pid) };
        if let Ok(mut list) = seen_list().lock() {
            if list.len() >= 500 {
                list.remove(0);
            }
            list.push(seen);
        }
    }

    pub fn install() {
        if HOOK.load(Ordering::SeqCst) != 0 {
            return;
        }
        super::started();
        // SAFETY: a system-wide out-of-context WinEvent hook whose procedure lives for the whole
        // process; events are delivered to this (UI) thread's message loop.
        let hook = unsafe { SetWinEventHook(EVENT_OBJECT_SHOW, EVENT_OBJECT_SHOW, std::ptr::null_mut(), Some(on_show), 0, 0, WINEVENT_OUTOFCONTEXT) };
        HOOK.store(hook as isize, Ordering::SeqCst);
    }

    pub fn uninstall() {
        let hook = HOOK.swap(0, Ordering::SeqCst);
        if hook != 0 {
            // SAFETY: a handle returned by `SetWinEventHook` above.
            unsafe { UnhookWinEvent(hook as HWINEVENTHOOK) };
        }
    }

    pub fn installed() -> bool {
        HOOK.load(Ordering::SeqCst) != 0
    }

    unsafe extern "system" fn collect(hwnd: HWND, lparam: LPARAM) -> i32 {
        // SAFETY: `lparam` is the &mut Vec `EnumWindows` was called with, valid for the call.
        let out = unsafe { &mut *(lparam as *mut Vec<Value>) };
        // SAFETY: plain Win32 call.
        if unsafe { IsWindowVisible(hwnd) } == 0 {
            return 1;
        }
        let class = class_name(hwnd);
        if is_console(&class) {
            let user_visible = super::user_visible(&class);
            out.push(json!({ "hwnd": hwnd as isize, "pid": pid_of(hwnd), "className": class, "title": title(hwnd), "userVisible": user_visible }));
        }
        1
    }

    pub fn current() -> Vec<Value> {
        let mut out: Vec<Value> = Vec::new();
        // SAFETY: `collect` only writes through the pointer we pass, which outlives the call.
        unsafe { EnumWindows(Some(collect), (&mut out as *mut Vec<Value>) as LPARAM) };
        out
    }

    /// pid → parent pid for every process in this session's snapshot.
    pub fn parent_map() -> Vec<(u32, u32)> {
        let mut pairs = Vec::new();
        // SAFETY: the snapshot handle is closed by the CloseHandle below; PROCESSENTRY32W is
        // zeroed and its dwSize set as the API requires.
        unsafe {
            let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
            if snapshot.is_null() || snapshot as isize == -1 {
                return pairs;
            }
            let mut entry: PROCESSENTRY32W = std::mem::zeroed();
            entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
            let mut ok = Process32FirstW(snapshot, &mut entry);
            while ok != 0 {
                pairs.push((entry.th32ProcessID, entry.th32ParentProcessID));
                ok = Process32NextW(snapshot, &mut entry);
            }
            windows_sys::Win32::Foundation::CloseHandle(snapshot);
        }
        pairs
    }

    pub fn our_pid() -> u32 {
        // SAFETY: plain Win32 call.
        unsafe { GetCurrentProcessId() }
    }
}

#[cfg(not(windows))]
mod sys {
    use serde_json::Value;

    pub fn install() {}
    pub fn uninstall() {}
    #[allow(dead_code)] // the Windows hook's entry point; nothing calls it here
    pub fn record(_hwnd: isize, _pid: u32, _class: String, _title: String) {}
    pub fn installed() -> bool {
        false
    }
    pub fn current() -> Vec<Value> {
        Vec::new()
    }
    #[allow(dead_code)] // only `chain_of` needs it, and only on Windows
    pub fn parent_map() -> Vec<(u32, u32)> {
        Vec::new()
    }
    pub fn our_pid() -> u32 {
        std::process::id()
    }
}

pub use sys::{current, install, installed, our_pid, parent_map, uninstall};

/// `pid` and its ancestors, youngest first (at most 32 generations, so a cycle cannot hang us).
/// Resolved from a live process snapshot, so it must be taken while the process still exists.
/// Called from the Windows hook only.
#[cfg_attr(not(windows), allow(dead_code))]
pub fn chain_of(pid: u32) -> Vec<u32> {
    chain_in(&parent_map(), pid)
}

/// `chain_of` against a given pid → parent-pid table.
#[cfg_attr(not(windows), allow(dead_code))]
fn chain_in(parents: &[(u32, u32)], pid: u32) -> Vec<u32> {
    let mut chain = vec![pid];
    let mut current = pid;
    for _ in 0..32 {
        let Some((_, parent)) = parents.iter().find(|(p, _)| *p == current) else { break };
        if *parent == 0 || chain.contains(parent) {
            break;
        }
        chain.push(*parent);
        current = *parent;
    }
    chain
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ancestry_walks_up_and_stops() {
        let parents = [(100, 50), (50, 4), (4, 0), (7, 7)];
        assert_eq!(chain_in(&parents, 100), vec![100, 50, 4], "youngest first, up to the root");
        assert_eq!(chain_in(&parents, 7), vec![7], "a self-parenting entry cannot loop");
        assert_eq!(chain_in(&parents, 12345), vec![12345], "an unknown pid is only itself");
        let seen = fake("ConsoleWindowClass", chain_in(&parents, 100));
        assert!(attributed(&seen, &[50]) && attributed(&seen, &[4]) && attributed(&seen, &[100]));
        assert!(!attributed(&seen, &[999]));
    }

    fn fake(class: &str, chain: Vec<u32>) -> Seen {
        Seen { hwnd: 4242, pid: chain[0], class: class.into(), title: String::new(), shown_at: 1, chain }
    }

    #[test]
    fn attribution_survives_a_process_that_already_exited() {
        let ours = our_pid();
        // A helper that flashed a console and exited: no process entry is left anywhere, but the
        // chain was resolved in the hook, so the flash is still ours.
        assert!(attributed(&fake("ConsoleWindowClass", vec![424_242, ours]), &[ours]));
        assert!(!attributed(&fake("ConsoleWindowClass", vec![424_242, 424_243]), &[ours]));
        // Windows 11 hands the console to Windows Terminal, whose process descends from svchost:
        // the only link back to us is the title, which is the client's image path.
        let mut hosted = fake("CASCADIA_HOSTING_WINDOW_CLASS", vec![424_242]);
        assert!(!attributed(&hosted, &[ours]));
        hosted.title = std::env::current_exe().unwrap().to_string_lossy().to_uppercase();
        assert!(attributed(&hosted, &[ours]), "a hosted console titled with our own image path is ours");
    }

    #[test]
    fn a_seen_console_window_is_reported_with_its_attribution() {
        let ours = our_pid();
        assert_eq!(chain_of(ours).first(), Some(&ours), "a chain starts with the process itself");
        reset();
        // Straight through what the hook calls, so this is a positive control of the recording path
        // (the hook itself is `SetWinEventHook`, and `watching` reports whether it took).
        super::sys::record(4242, ours, "ConsoleWindowClass".into(), "conhost".into());
        super::sys::record(4243, ours, "PseudoConsoleWindow".into(), String::new());
        super::sys::record(4244, ours, "Chrome_WidgetWin_1".into(), "sta".into());
        let taken = snapshot(&[ours]);
        assert_eq!(taken["seen"].as_array().map(Vec::len), Some(2), "both console windows, not the browser window");
        assert_eq!(taken["ours"].as_array().map(Vec::len), Some(2), "both are ours");
        assert_eq!(taken["seen"][0]["chain"][0], json!(ours), "the ancestry is resolved when the window is recorded");
        assert_eq!(taken["seen"][0]["userVisible"], json!(true), "a conhost window is one the user sees");
        assert_eq!(taken["seen"][1]["userVisible"], json!(false), "a pseudo console's host window is not");
        assert_eq!(taken["seen"][0]["ours"], json!(true));
        reset();
        assert_eq!(snapshot(&[ours])["seen"].as_array().map(Vec::len), Some(0));
    }

    #[test]
    fn the_desktops_console_windows_are_listable() {
        // No assertion on the contents: the developer running the tests may have a terminal open.
        let snapshot = snapshot(&[our_pid()]);
        assert!(snapshot["current"].is_array() && snapshot["seen"].is_array() && snapshot["ours"].is_array());
    }
}
