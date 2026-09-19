//! Hidden top-level windows of Chrome-created browsers [owner: tabs] (foreign.rs,
//! docs/ARCHITECTURE.md §4.5).
//!
//! When an extension or the Web Store makes Chromium create its own `Browser`, the Chrome window
//! must never be seen or take the keyboard. `foreign.rs` hides its root window here:
//! - **cloak** (`DWMWA_CLOAK`): DWM draws nothing, the shell shows no taskbar button or Alt+Tab
//!   entry; the window keeps working (no `ShowWindow(SW_HIDE)`, which Chromium would undo);
//! - a **thread-local `WH_CBT` hook** on the UI thread (Chromium creates and shows its windows
//!   there): `HCBT_ACTIVATE` is refused for hidden roots and every window they own (dialogs, IPH
//!   bubbles), and `HCBT_CREATEWND` cloaks a window owned by a hidden root before its first show;
//! - backstops (`SetWinEventHook`, out of context, this process only): `EVENT_OBJECT_SHOW` cloaks a
//!   hidden or owned window that shows up uncloaked; `EVENT_SYSTEM_FOREGROUND` hands the foreground
//!   back to sta's main window when a hidden window got it anyway.
//!
//! The hook procedures only touch this module's own thread-locals (with `try_borrow`: a hook can
//! run inside any Win32 call) and never call CEF. Listeners are notified through a posted UI task.
//!
//! Public API (Windows; `platform/mod.rs` has inert fallbacks):
//! - `pub fn install(main_hwnd: isize)`, `pub fn uninstall()`
//! - `pub fn hide_root(root: isize) -> bool`, `pub fn show_root(root: isize)`, `pub fn forget_root(root: isize)`
//! - `pub fn is_hidden(hwnd: isize) -> bool` — the window or one of its owners is a hidden root
//! - `pub fn root_of(hwnd: isize) -> isize`, `pub fn window_title(hwnd: isize) -> String`,
//!   `pub fn is_visible(hwnd: isize) -> bool`, `pub fn is_window(hwnd: isize) -> bool`
//! - `pub fn describe(hwnd: isize) -> WindowDescription`
//! - `pub fn set_native_caption(hwnd: isize, dark: bool)` — kept-native windows: dark caption
//! - `pub fn set_sta_owned_listener(f: fn(isize))` — a window owned by sta's main window was shown
//! - `pub fn allow_dialogs(root: isize, allow: bool)` / `pub fn set_dialog_listener(f: fn(isize))` — a
//!   hidden root whose **one owned dialog** is left alone (shown, activatable, foreground) while its
//!   window itself stays cloaked: the one operation that needs it is removing an extension, where
//!   Chromium always shows its own "Remove …?" confirmation (`ext_backend.rs`, gate S7). A second
//!   window the same root opens is cloaked and refused like any other (SEC-P3-4)
//! - `pub fn move_window(hwnd: isize, x: i32, y: i32)`, `pub fn window_rect(hwnd: isize) -> Option<[i32; 4]>`,
//!   `pub fn work_area(hwnd: isize) -> Option<[i32; 4]>`
//! - `pub fn snapshot() -> serde_json::Value` — hidden roots, owned windows, counters, recent windows

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet, VecDeque};
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Dwm::{DWMWA_CLOAK, DWMWA_CLOAKED, DWMWA_USE_IMMERSIVE_DARK_MODE, DwmGetWindowAttribute, DwmSetWindowAttribute};
use windows_sys::Win32::Graphics::Gdi::{GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromWindow};
use windows_sys::Win32::UI::Accessibility::{HWINEVENTHOOK, SetWinEventHook, UnhookWinEvent};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CBT_CREATEWNDW, CallNextHookEx, EVENT_OBJECT_SHOW, EVENT_SYSTEM_FOREGROUND, GA_ROOT, GW_OWNER, GWL_EXSTYLE, GWL_STYLE,
    GetAncestor, GetClassNameW, GetWindow, GetWindowLongPtrW, GetWindowRect, HCBT_ACTIVATE, InternalGetWindowText, HCBT_CREATEWND,
    HHOOK, IsWindow, IsWindowVisible, SWP_NOACTIVATE, SWP_NOSIZE, SWP_NOZORDER, SetForegroundWindow, SetWindowPos,
    SetWindowsHookExW, UnhookWindowsHookEx, WH_CBT, WINEVENT_OUTOFCONTEXT, WS_CHILD,
};

/// `OBJID_WINDOW`: WinEvents about the window itself (not a child object).
const OBJID_WINDOW: i32 = 0;
/// Owner chains longer than this are not followed (a dialog owning a bubble is 2).
const MAX_OWNER_DEPTH: usize = 8;
/// Recent window events kept for `debug.info` and the gates.
const RECENT: usize = 48;

#[derive(Default, Clone, Copy)]
struct Stats {
    activations_blocked: u64,
    cloaked_at_create: u64,
    cloak_at_create_failed: u64,
    cloaked_on_show: u64,
    foreground_restored: u64,
    /// Windows a dialog-allowing root created *after* the one dialog sta is waiting for: cloaked and
    /// refused like any other owned window (SEC-P3-4).
    extra_dialogs_cloaked: u64,
}

thread_local! {
    static HIDDEN: RefCell<HashSet<isize>> = RefCell::new(HashSet::new());
    static OWNED: RefCell<HashSet<isize>> = RefCell::new(HashSet::new());
    /// Hidden roots whose one owned dialog is left visible (see [`allow_dialogs`]), each mapped to the
    /// window admitted for it (0 = none yet). Empty except while an extension is being removed.
    static DIALOG_ROOTS: RefCell<HashMap<isize, isize>> = RefCell::new(HashMap::new());
    static DIALOG_LISTENER: Cell<Option<fn(isize)>> = const { Cell::new(None) };
    static MAIN: Cell<isize> = const { Cell::new(0) };
    static CBT_HOOK: Cell<isize> = const { Cell::new(0) };
    static EVENT_HOOKS: Cell<[isize; 2]> = const { Cell::new([0, 0]) };
    static STATS: Cell<Stats> = Cell::new(Stats::default());
    static STA_OWNED_LISTENER: Cell<Option<fn(isize)>> = const { Cell::new(None) };
    static LOG: RefCell<VecDeque<serde_json::Value>> = const { RefCell::new(VecDeque::new()) };
    static START: std::time::Instant = std::time::Instant::now();
}

/// A top-level window, for logs and `debug.info`.
#[derive(Debug, Clone, serde::Serialize)]
pub struct WindowDescription {
    pub hwnd: isize,
    pub class: String,
    pub title: String,
    pub style: String,
    pub ex_style: String,
    pub owner: isize,
    pub visible: bool,
    pub cloaked: bool,
    pub rect: [i32; 4],
}

fn stats(f: impl FnOnce(&mut Stats)) {
    let mut s = STATS.get();
    f(&mut s);
    STATS.set(s);
}

fn owner_of(hwnd: isize) -> isize {
    // SAFETY: plain Win32 call on a window handle (a stale handle returns null).
    unsafe { GetWindow(hwnd as HWND, GW_OWNER) as isize }
}

pub fn root_of(hwnd: isize) -> isize {
    if hwnd == 0 {
        return 0;
    }
    // SAFETY: plain Win32 call.
    unsafe { GetAncestor(hwnd as HWND, GA_ROOT) as isize }
}

pub fn is_visible(hwnd: isize) -> bool {
    // SAFETY: plain Win32 call.
    hwnd != 0 && unsafe { IsWindowVisible(hwnd as HWND) } != 0
}

pub fn is_window(hwnd: isize) -> bool {
    // SAFETY: plain Win32 call.
    hwnd != 0 && unsafe { IsWindow(hwnd as HWND) } != 0
}

pub fn window_title(hwnd: isize) -> String {
    let mut buf = [0u16; 256];
    // SAFETY: correctly sized buffer; InternalGetWindowText sends no WM_GETTEXT (safe inside hooks and
    // for windows that are still being created).
    let n = unsafe { InternalGetWindowText(hwnd as HWND, buf.as_mut_ptr(), buf.len() as i32) }.max(0) as usize;
    String::from_utf16_lossy(&buf[..n])
}

fn class_name(hwnd: isize) -> String {
    let mut buf = [0u16; 128];
    // SAFETY: correctly sized buffer.
    let n = unsafe { GetClassNameW(hwnd as HWND, buf.as_mut_ptr(), buf.len() as i32) }.max(0) as usize;
    String::from_utf16_lossy(&buf[..n])
}

fn set_cloak(hwnd: isize, on: bool) -> bool {
    let value: i32 = on as i32;
    // SAFETY: a 4-byte BOOL attribute; failures are reported, never fatal.
    let hr = unsafe { DwmSetWindowAttribute(hwnd as HWND, DWMWA_CLOAK as u32, (&value as *const i32).cast(), 4) };
    hr == 0
}

fn is_cloaked(hwnd: isize) -> bool {
    let mut value: i32 = 0;
    // SAFETY: a 4-byte out value.
    let hr = unsafe { DwmGetWindowAttribute(hwnd as HWND, DWMWA_CLOAKED as u32, (&mut value as *mut i32).cast(), 4) };
    hr == 0 && value != 0
}

/// `hwnd` is a hidden root, a window already known to be owned by one, or its owner chain
/// reaches one. Never blocks: a busy registry counts as "not hidden".
///
/// An `OWNED` entry is only believed while the window still exists *and* its owner chain still
/// reaches a hidden root: Windows reuses HWND values inside a process, and `OWNED` is not pruned when
/// Chromium destroys a bubble or dialog early. Without the re-check, an unrelated later window that
/// happened to get that handle would be refused activation and cloaked — invisible and unfocusable.
/// A stale entry is dropped on the way out.
/// `root` may show and activate **one** dialog it owns while it stays cloaked itself — the window sta
/// is waiting for (Chromium's "Remove …?"). Anything it owns after that is cloaked and refused like
/// every other owned window, so a page that opens a second window cannot put it on screen (SEC-P3-4).
/// Cleared as soon as the operation that needed it is over — a window that keeps this is a window that
/// can put something on screen sta never asked for.
pub fn allow_dialogs(root: isize, allow: bool) {
    let _ = DIALOG_ROOTS.with(|d| {
        d.try_borrow_mut().map(|mut d| {
            if allow {
                d.entry(root).or_insert(0);
            } else {
                d.remove(&root);
            }
        })
    });
    record(if allow { "dialogs-allowed" } else { "dialogs-refused" }, root, serde_json::Value::Null);
}

/// The first window a dialog-allowing root creates is the dialog sta waits for; a later one is only
/// admitted if that window is already gone. `false` = treat it as an ordinary owned window.
fn admit_dialog(root: isize, hwnd: isize) -> bool {
    DIALOG_ROOTS.with(|d| {
        d.try_borrow_mut().is_ok_and(|mut d| {
            let Some(slot) = d.get_mut(&root) else { return false };
            if *slot == hwnd || *slot == 0 || !is_window(*slot) {
                *slot = hwnd;
                return true;
            }
            false
        })
    })
}

/// Called (posted) when a dialog of such a root is shown, so its owner can place it.
pub fn set_dialog_listener(f: fn(isize)) {
    DIALOG_LISTENER.set(Some(f));
}

fn allows_dialogs(root: isize) -> bool {
    DIALOG_ROOTS.with(|d| d.try_borrow().is_ok_and(|d| d.contains_key(&root)))
}

/// The admitted dialog of a dialog-allowing root, or one of its own windows: the exemption covers the
/// window sta is waiting for and what that window owns, not the whole subtree under the backend root.
fn is_allowed_dialog(hwnd: isize) -> bool {
    if hwnd == 0 {
        return false;
    }
    let admitted: Vec<isize> = DIALOG_ROOTS.with(|d| d.try_borrow().map(|d| d.values().copied().filter(|w| *w != 0).collect()).unwrap_or_default());
    if admitted.is_empty() {
        return false;
    }
    let mut current = hwnd;
    for _ in 0..MAX_OWNER_DEPTH {
        if admitted.contains(&current) {
            return true;
        }
        current = owner_of(current);
        if current == 0 {
            return false;
        }
    }
    false
}

pub fn is_hidden(hwnd: isize) -> bool {
    if hwnd == 0 {
        return false;
    }
    match reaches_hidden_root(hwnd) {
        Some(true) => true,
        // The chain is readable and reaches no hidden root: an `OWNED` entry for this handle is
        // stale (the window was destroyed, or its root became native), so forget it.
        Some(false) => {
            if OWNED.with(|o| o.try_borrow().is_ok_and(|o| o.contains(&hwnd))) {
                let _ = OWNED.with(|o| o.try_borrow_mut().map(|mut o| o.remove(&hwnd)));
                record("owned-forgotten", hwnd, serde_json::Value::Null);
            }
            false
        }
        None => false, // a busy registry counts as "not hidden"
    }
}

/// `hwnd` or one of its owners (up to [`MAX_OWNER_DEPTH`]) is a hidden root; `None` while the
/// registry is borrowed.
fn reaches_hidden_root(hwnd: isize) -> Option<bool> {
    HIDDEN.with(|h| {
        let hidden = h.try_borrow().ok()?;
        if hidden.is_empty() {
            return Some(false);
        }
        let mut current = hwnd;
        for _ in 0..MAX_OWNER_DEPTH {
            if hidden.contains(&current) {
                return Some(true);
            }
            // GetWindow sends no messages: no hook or window procedure runs while borrowed.
            current = owner_of(current);
            if current == 0 {
                break;
            }
        }
        Some(false)
    })
}

fn record(kind: &str, hwnd: isize, extra: serde_json::Value) {
    let t = START.with(|s| s.elapsed().as_millis() as u64);
    let d = describe(hwnd);
    let _ = LOG.with(|l| {
        l.try_borrow_mut().map(|mut l| {
            l.push_back(serde_json::json!({ "t": t, "kind": kind, "window": d, "extra": extra }));
            while l.len() > RECENT {
                l.pop_front();
            }
        })
    });
}

pub fn describe(hwnd: isize) -> WindowDescription {
    let mut r = RECT { left: 0, top: 0, right: 0, bottom: 0 };
    // SAFETY: plain Win32 getters on a window handle.
    let (style, ex) = unsafe {
        GetWindowRect(hwnd as HWND, &mut r);
        (GetWindowLongPtrW(hwnd as HWND, GWL_STYLE), GetWindowLongPtrW(hwnd as HWND, GWL_EXSTYLE))
    };
    WindowDescription {
        hwnd,
        class: class_name(hwnd),
        title: window_title(hwnd),
        style: format!("{:#x}", style as u32),
        ex_style: format!("{:#x}", ex as u32),
        owner: owner_of(hwnd),
        visible: is_visible(hwnd),
        cloaked: is_cloaked(hwnd),
        rect: [r.left, r.top, r.right - r.left, r.bottom - r.top],
    }
}

/// Raw window styles `(style, ex_style)`.
pub fn styles(hwnd: isize) -> (u32, u32) {
    // SAFETY: plain Win32 getters.
    unsafe { (GetWindowLongPtrW(hwnd as HWND, GWL_STYLE) as u32, GetWindowLongPtrW(hwnd as HWND, GWL_EXSTYLE) as u32) }
}

unsafe extern "system" fn cbt_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0 {
        match code as u32 {
            HCBT_ACTIVATE => {
                let hwnd = wparam as isize;
                if is_hidden(hwnd) && !is_allowed_dialog(hwnd) {
                    stats(|s| s.activations_blocked += 1);
                    record("activate-blocked", hwnd, serde_json::Value::Null);
                    return 1; // prevent the activation
                }
            }
            HCBT_CREATEWND => {
                let hwnd = wparam as isize;
                // SAFETY: for HCBT_CREATEWND, lParam points to a CBT_CREATEWNDW whose lpcs is valid
                // for the duration of the hook call.
                let (style, ex_style, owner) = unsafe {
                    let cw = lparam as *const CBT_CREATEWNDW;
                    if cw.is_null() || (*cw).lpcs.is_null() {
                        (WS_CHILD, 0, 0)
                    } else {
                        ((*(*cw).lpcs).style as u32, (*(*cw).lpcs).dwExStyle, (*(*cw).lpcs).hwndParent as isize)
                    }
                };
                if style & WS_CHILD == 0 {
                    // Styles from the CREATESTRUCT: the window is not laid out yet (its rect and
                    // title are empty), and this is what the install-dialog signature is read from.
                    let created = serde_json::json!({ "owner": owner, "style": format!("{style:#x}"), "exStyle": format!("{ex_style:#x}") });
                    let dialog = owner != 0 && is_hidden(owner) && allows_dialogs(owner) && admit_dialog(owner, hwnd);
                    if dialog {
                        // The one dialog of a root that asked for its dialogs to be left alone: it is
                        // the window the user has to answer, so it is neither cloaked nor refused.
                        record("dialog-created", hwnd, created);
                    } else if owner != 0 && is_hidden(owner) {
                        let extra = allows_dialogs(owner);
                        let _ = OWNED.with(|o| o.try_borrow_mut().map(|mut o| o.insert(hwnd)));
                        // DWM can answer with a failure HRESULT for a window that is still being
                        // created and cloak it all the same: the read-back decides.
                        if set_cloak(hwnd, true) || is_cloaked(hwnd) {
                            stats(|s| s.cloaked_at_create += 1);
                        } else {
                            stats(|s| s.cloak_at_create_failed += 1);
                        }
                        if extra {
                            stats(|s| s.extra_dialogs_cloaked += 1);
                        }
                        record(if extra { "extra-dialog-cloaked" } else { "owned-created" }, hwnd, created);
                    } else if owner != 0 && owner == MAIN.get() {
                        record("sta-owned-created", hwnd, created);
                    } else {
                        record("top-level-created", hwnd, created);
                    }
                }
            }
            _ => {}
        }
    }
    // SAFETY: passing the hook chain on unchanged.
    unsafe { CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam) }
}

unsafe extern "system" fn event_proc(_hook: HWINEVENTHOOK, event: u32, hwnd: HWND, id_object: i32, _id_child: i32, _thread: u32, _time: u32) {
    if id_object != OBJID_WINDOW || hwnd.is_null() {
        return;
    }
    let hwnd = hwnd as isize;
    match event {
        EVENT_OBJECT_SHOW => {
            if root_of(hwnd) != hwnd {
                return; // child windows follow their root
            }
            if is_allowed_dialog(hwnd) {
                record("dialog-shown", hwnd, serde_json::Value::Null);
                if let Some(listener) = DIALOG_LISTENER.get() {
                    listener(hwnd);
                }
                return;
            }
            if is_hidden(hwnd) {
                let known = HIDDEN.with(|h| h.try_borrow().is_ok_and(|h| h.contains(&hwnd)));
                if !known {
                    let _ = OWNED.with(|o| o.try_borrow_mut().map(|mut o| o.insert(hwnd)));
                }
                if !is_cloaked(hwnd) {
                    set_cloak(hwnd, true);
                    stats(|s| s.cloaked_on_show += 1);
                    record("cloaked-on-show", hwnd, serde_json::Value::Null);
                }
            } else if owner_of(hwnd) != 0 {
                let sta_owned = owner_of(hwnd) == MAIN.get();
                record(if sta_owned { "sta-owned-shown" } else { "owned-shown" }, hwnd, serde_json::Value::Null);
                if sta_owned && let Some(listener) = STA_OWNED_LISTENER.get() {
                    listener(hwnd);
                }
            }
        }
        EVENT_SYSTEM_FOREGROUND => {
            let main = MAIN.get();
            if main != 0 && is_hidden(hwnd) && !is_allowed_dialog(hwnd) {
                stats(|s| s.foreground_restored += 1);
                record("foreground-restored", hwnd, serde_json::Value::Null);
                // SAFETY: the foreground belongs to this process, so it may hand it on.
                unsafe { SetForegroundWindow(main as HWND) };
            }
        }
        _ => {}
    }
}

/// Installs the hooks on the calling (UI) thread once. `main_hwnd` is sta's main window.
pub fn install(main_hwnd: isize) {
    if main_hwnd != 0 {
        MAIN.set(main_hwnd);
    }
    if CBT_HOOK.get() != 0 {
        return;
    }
    // SAFETY: a thread-local hook (hmod null, this thread's id) with a procedure that lives for the
    // whole process; WinEvent hooks out of context for this process only.
    unsafe {
        let thread = windows_sys::Win32::System::Threading::GetCurrentThreadId();
        let hook = SetWindowsHookExW(WH_CBT, Some(cbt_proc), std::ptr::null_mut(), thread);
        CBT_HOOK.set(hook as isize);
        let pid = windows_sys::Win32::System::Threading::GetCurrentProcessId();
        let show = SetWinEventHook(EVENT_OBJECT_SHOW, EVENT_OBJECT_SHOW, std::ptr::null_mut(), Some(event_proc), pid, 0, WINEVENT_OUTOFCONTEXT);
        let fg = SetWinEventHook(EVENT_SYSTEM_FOREGROUND, EVENT_SYSTEM_FOREGROUND, std::ptr::null_mut(), Some(event_proc), pid, 0, WINEVENT_OUTOFCONTEXT);
        EVENT_HOOKS.set([show as isize, fg as isize]);
    }
}

pub fn uninstall() {
    // SAFETY: handles returned by the calls in `install` (0 = not installed).
    unsafe {
        let hook = CBT_HOOK.replace(0);
        if hook != 0 {
            UnhookWindowsHookEx(hook as HHOOK);
        }
        for h in EVENT_HOOKS.replace([0, 0]) {
            if h != 0 {
                UnhookWinEvent(h as HWINEVENTHOOK);
            }
        }
    }
    HIDDEN.with(|h| h.borrow_mut().clear());
    OWNED.with(|o| o.borrow_mut().clear());
    DIALOG_ROOTS.with(|d| d.borrow_mut().clear());
}

/// A window owned by sta's main window was shown (called from the WinEvent hook: no CEF calls).
pub fn set_sta_owned_listener(f: fn(isize)) {
    STA_OWNED_LISTENER.set(Some(f));
}

/// Cloaks `root` and blocks its activation until [`show_root`] or [`forget_root`].
pub fn hide_root(root: isize) -> bool {
    if root == 0 || root == MAIN.get() {
        return false;
    }
    HIDDEN.with(|h| h.borrow_mut().insert(root));
    let ok = set_cloak(root, true);
    record("hidden", root, serde_json::json!({ "cloakOk": ok }));
    ok
}

/// Uncloaks `root` and the windows it owns (a window that stays native).
pub fn show_root(root: isize) {
    if root == 0 {
        return;
    }
    HIDDEN.with(|h| h.borrow_mut().remove(&root));
    let owned: Vec<isize> = OWNED.with(|o| {
        let mut o = o.borrow_mut();
        let mine: Vec<isize> = o.iter().copied().filter(|w| owner_chain_contains(*w, root)).collect();
        for w in &mine {
            o.remove(w);
        }
        mine
    });
    set_cloak(root, false);
    for w in owned {
        if is_window(w) {
            set_cloak(w, false);
        }
    }
    record("shown", root, serde_json::Value::Null);
}

fn owner_chain_contains(hwnd: isize, root: isize) -> bool {
    let mut current = hwnd;
    for _ in 0..MAX_OWNER_DEPTH {
        if current == root {
            return true;
        }
        current = owner_of(current);
        if current == 0 {
            return false;
        }
    }
    false
}

/// The root's browsers are gone: drop it and the windows it owned (destroyed with it).
pub fn forget_root(root: isize) {
    allow_dialogs(root, false);
    HIDDEN.with(|h| h.borrow_mut().remove(&root));
    OWNED.with(|o| o.borrow_mut().retain(|w| is_window(*w) && !owner_chain_contains(*w, root)));
}

/// Dark (or light) DWM caption and sta's application icon for a Chrome window that stays native.
pub fn set_native_caption(hwnd: isize, dark: bool) {
    if hwnd == 0 {
        return;
    }
    let value: i32 = dark as i32;
    // SAFETY: a 4-byte BOOL attribute; failures are ignored.
    unsafe { DwmSetWindowAttribute(hwnd as HWND, DWMWA_USE_IMMERSIVE_DARK_MODE as u32, (&value as *const i32).cast(), 4) };
    set_native_icon(hwnd);
}

/// sta's icon (resource 1, the one `Settings.chrome_app_icon_id` names) in the window's caption
/// and Alt+Tab entry.
fn set_native_icon(hwnd: isize) {
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        ICON_BIG, ICON_SMALL, IMAGE_ICON, LR_DEFAULTCOLOR, LoadImageW, SM_CXICON, SM_CXSMICON, SM_CYICON, SM_CYSMICON, SendMessageW, WM_SETICON,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::GetSystemMetrics;
    // SAFETY: loads this module's icon resource 1 and sets it on our own window; a failed load
    // (null handle) is simply not applied.
    unsafe {
        let module = GetModuleHandleW(std::ptr::null());
        // `MAKEINTRESOURCEW(1)`: an integer resource id in place of a name pointer.
        let resource = std::ptr::without_provenance::<u16>(1);
        for (kind, cx, cy) in [(ICON_BIG, SM_CXICON, SM_CYICON), (ICON_SMALL, SM_CXSMICON, SM_CYSMICON)] {
            let icon = LoadImageW(module, resource, IMAGE_ICON, GetSystemMetrics(cx), GetSystemMetrics(cy), LR_DEFAULTCOLOR);
            if !icon.is_null() {
                SendMessageW(hwnd as HWND, WM_SETICON, kind as usize, icon as isize);
            }
        }
    }
}

/// Top-left of a window's client area in screen pixels.
pub fn client_origin(hwnd: isize) -> Option<(i32, i32)> {
    client_rect(hwnd).map(|[x, y, _, _]| (x, y))
}

/// A window's client area in screen pixels `[x, y, w, h]`.
pub fn client_rect(hwnd: isize) -> Option<[i32; 4]> {
    use windows_sys::Win32::UI::WindowsAndMessaging::{GetWindowInfo, WINDOWINFO};
    if hwnd == 0 {
        return None;
    }
    // SAFETY: plain Win32 call with a correctly sized out struct.
    unsafe {
        let mut info: WINDOWINFO = std::mem::zeroed();
        info.cbSize = std::mem::size_of::<WINDOWINFO>() as u32;
        if GetWindowInfo(hwnd as HWND, &mut info) == 0 {
            return None;
        }
        let c = info.rcClient;
        Some([c.left, c.top, c.right - c.left, c.bottom - c.top])
    }
}

pub fn window_rect(hwnd: isize) -> Option<[i32; 4]> {
    let mut r = RECT { left: 0, top: 0, right: 0, bottom: 0 };
    // SAFETY: valid out pointer.
    (unsafe { GetWindowRect(hwnd as HWND, &mut r) } != 0).then_some([r.left, r.top, r.right - r.left, r.bottom - r.top])
}

/// Work area of the monitor nearest to `hwnd`, in screen pixels `[x, y, w, h]`.
pub fn work_area(hwnd: isize) -> Option<[i32; 4]> {
    // SAFETY: plain Win32 calls with a correctly sized MONITORINFO.
    unsafe {
        let monitor = MonitorFromWindow(hwnd as HWND, MONITOR_DEFAULTTONEAREST);
        let mut info: MONITORINFO = std::mem::zeroed();
        info.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
        if GetMonitorInfoW(monitor, &mut info) == 0 {
            return None;
        }
        let w = info.rcWork;
        Some([w.left, w.top, w.right - w.left, w.bottom - w.top])
    }
}

/// Moves a top-level window (screen pixels) without activating, resizing or restacking it.
pub fn move_window(hwnd: isize, x: i32, y: i32) -> bool {
    // SAFETY: plain Win32 call.
    unsafe { SetWindowPos(hwnd as HWND, std::ptr::null_mut(), x, y, 0, 0, SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE) != 0 }
}

#[cfg_attr(not(debug_assertions), allow(dead_code))] // `foreign::debug_snapshot` (debug.rs) only
pub fn snapshot() -> serde_json::Value {
    let s = STATS.get();
    let hidden: Vec<WindowDescription> = HIDDEN.with(|h| h.borrow().iter().map(|w| describe(*w)).collect());
    let owned: Vec<WindowDescription> = OWNED.with(|o| o.borrow().iter().filter(|w| is_window(**w)).map(|w| describe(*w)).collect());
    let recent: Vec<serde_json::Value> = LOG.with(|l| l.borrow().iter().cloned().collect());
    serde_json::json!({
        "installed": CBT_HOOK.get() != 0,
        "hidden": hidden,
        "owned": owned,
        "activationsBlocked": s.activations_blocked,
        "cloakedAtCreate": s.cloaked_at_create,
        "cloakAtCreateFailed": s.cloak_at_create_failed,
        "cloakedOnShow": s.cloaked_on_show,
        "foregroundRestored": s.foreground_restored,
        "extraDialogsCloaked": s.extra_dialogs_cloaked,
        "dialogRoots": DIALOG_ROOTS.with(|d| d.borrow().iter().map(|(root, dialog)| serde_json::json!({ "root": root, "dialog": dialog })).collect::<Vec<_>>()),
        "recent": recent,
    })
}
