//! Windows implementation of the platform facade [owner: chrome]. See `platform/mod.rs`.
//!
//! Implemented: dark mode (registry), DWM chrome (immersive dark mode + rounded corners),
//! clipboard (`CF_UNICODETEXT`), OS UI languages, Downloads known folder, shell open
//! (`ShellExecuteW`), show in folder (`SHOpenFolderAndSelectItems`), the modern folder picker
//! (`IFileOpenDialog` + `FOS_PICKFOLDERS`, hand-written COM vtables: windows-sys has no COM
//! interfaces), a fatal-error message box, and (debug builds) foreground + `SendInput` helpers for
//! real-keyboard tests.

use std::ffi::c_void;
use std::path::PathBuf;
use windows_sys::Win32::Foundation::{ERROR_SUCCESS, GENERIC_WRITE, GlobalFree, HWND};
use windows_sys::Win32::Globalization::{GetUserPreferredUILanguages, MUI_LANGUAGE_NAME};
use windows_sys::Win32::Graphics::Dwm::{
    DWMWA_USE_IMMERSIVE_DARK_MODE, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND, DwmSetWindowAttribute,
};
use windows_sys::Win32::Storage::FileSystem::{DELETE, FILE_FLAG_DELETE_ON_CLOSE, FILE_SHARE_READ, GetDiskFreeSpaceExW, MoveFileExW};
use windows_sys::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoCreateInstance, CoInitializeEx,
    CoTaskMemFree, CoUninitialize,
};
use windows_sys::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole};
use windows_sys::Win32::System::DataExchange::{CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData};
use windows_sys::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock};
use windows_sys::Win32::System::Ole::CF_UNICODETEXT;
use windows_sys::Win32::System::Registry::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, RRF_RT_REG_DWORD, RRF_RT_REG_SZ, RegGetValueW};
use windows_sys::Win32::UI::Shell::{
    FOLDERID_Downloads, FOS_FORCEFILESYSTEM, FOS_PATHMUSTEXIST, FOS_PICKFOLDERS, FileOpenDialog, ILCreateFromPathW, ILFree,
    SHCreateItemFromParsingName, SHGetKnownFolderPath, SHOpenFolderAndSelectItems, SIGDN_FILESYSPATH, ShellExecuteW,
};
use windows_sys::Win32::System::Threading::GetCurrentThreadId;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CWPSTRUCT, CallNextHookEx, HHOOK, MB_ICONERROR, MB_OK, MB_SETFOREGROUND, MB_TOPMOST, MessageBoxW,
    SPI_GETCLIENTAREAANIMATION, SW_SHOWNORMAL, SetWindowsHookExW, SystemParametersInfoW, UnhookWindowsHookEx,
    WH_CALLWNDPROC, WM_SETTINGCHANGE,
};
use windows_sys::core::{GUID, HRESULT};

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn from_wide_ptr(p: *const u16) -> String {
    if p.is_null() {
        return String::new();
    }
    // SAFETY: `p` is a NUL-terminated UTF-16 string owned by the caller.
    unsafe {
        let len = (0..).take_while(|&i| *p.add(i) != 0).count();
        String::from_utf16_lossy(std::slice::from_raw_parts(p, len))
    }
}

const THEME_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize";

/// `AppsUseLightTheme` (DWORD) or `None` when the value is missing.
fn apps_use_light_theme() -> Option<u32> {
    let key = wide(THEME_KEY);
    let value = wide("AppsUseLightTheme");
    let mut data: u32 = 1;
    let mut size = std::mem::size_of::<u32>() as u32;
    // SAFETY: valid NUL-terminated strings and a u32 out buffer of the declared size.
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_DWORD,
            std::ptr::null_mut(),
            (&mut data as *mut u32).cast::<c_void>(),
            &mut size,
        )
    };
    (status == ERROR_SUCCESS).then_some(data)
}

/// `true` when Windows apps use the dark theme (`AppsUseLightTheme == 0`; missing = light).
pub fn system_dark_mode() -> bool {
    apps_use_light_theme() == Some(0)
}

/// The Windows **Animation effects** setting (Settings › Accessibility › Visual effects), i.e.
/// `SPI_GETCLIENTAREAANIMATION`. `true` (animate) when the call fails, which is Windows' own
/// default and the only safe answer: a failed read must never make sta look broken.
pub fn system_animations() -> bool {
    let mut enabled: i32 = 1;
    // SAFETY: `SPI_GETCLIENTAREAANIMATION` writes one `BOOL` (i32) into `pvParam`; `uiParam` and
    // `fWinIni` are unused for a read.
    let ok = unsafe {
        SystemParametersInfoW(SPI_GETCLIENTAREAANIMATION, 0, (&mut enabled as *mut i32).cast::<c_void>(), 0)
    };
    ok == 0 || enabled != 0
}

thread_local! {
    /// The `WH_CALLWNDPROC` hook watching for `WM_SETTINGCHANGE` (0 = not installed).
    static SETTING_HOOK: std::cell::Cell<isize> = const { std::cell::Cell::new(0) };
    /// What to call when one arrives. A plain `fn` so the hook holds no captured state.
    static SETTING_CB: std::cell::Cell<Option<fn()>> = const { std::cell::Cell::new(None) };
}

/// `WH_CALLWNDPROC` runs for **every** message sent to a window of this thread, so it does the
/// cheapest possible test and never touches shell state itself: the callback only posts a task.
unsafe extern "system" fn setting_change_proc(code: i32, wparam: usize, lparam: isize) -> isize {
    if code >= 0 && lparam != 0 {
        // SAFETY: for `code >= 0`, `lparam` is a `CWPSTRUCT` owned by Windows for this call.
        let message = unsafe { (*(lparam as *const CWPSTRUCT)).message };
        if message == WM_SETTINGCHANGE
            && let Some(cb) = SETTING_CB.get()
        {
            cb();
        }
    }
    // SAFETY: passing the call on with the parameters we were given.
    unsafe { CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam) }
}

/// Watch `WM_SETTINGCHANGE` on the calling thread (the CEF UI thread) with a thread-local
/// `WH_CALLWNDPROC` hook: an *observer*, which never changes how a message is handled and reaches
/// no other process. Idempotent; the second call only replaces the callback.
pub fn watch_setting_change(f: fn()) {
    SETTING_CB.set(Some(f));
    if SETTING_HOOK.get() != 0 {
        return;
    }
    // SAFETY: a thread-local hook for our own thread; the proc is a valid `extern "system"` fn.
    let hook = unsafe { SetWindowsHookExW(WH_CALLWNDPROC, Some(setting_change_proc), std::ptr::null_mut(), GetCurrentThreadId()) };
    SETTING_HOOK.set(hook as isize);
}

/// Removes the hook installed by [`watch_setting_change`].
pub fn unwatch_setting_change() {
    let hook = SETTING_HOOK.replace(0);
    SETTING_CB.set(None);
    if hook != 0 {
        // SAFETY: a hook handle this thread installed and has not unhooked yet.
        unsafe { UnhookWindowsHookEx(hook as HHOOK) };
    }
}

fn set_dwm_attr<T>(hwnd: HWND, attr: i32, value: &T) {
    // SAFETY: `value` is readable for size_of::<T>() bytes; failures (older Windows) are ignored.
    unsafe {
        DwmSetWindowAttribute(hwnd, attr as u32, (value as *const T).cast::<c_void>(), std::mem::size_of::<T>() as u32);
    }
}

/// Dark DWM frame/shadow tint and Windows 11 rounded corners for the frameless window.
pub fn apply_window_chrome(hwnd: isize, dark: bool) {
    if hwnd == 0 {
        return;
    }
    let hwnd = hwnd as HWND;
    set_dwm_attr(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, &(dark as i32));
    set_dwm_attr(hwnd, DWMWA_WINDOW_CORNER_PREFERENCE, &DWMWCP_ROUND);
}

/// Puts Unicode text on the clipboard.
pub fn set_clipboard_text(text: &str) -> bool {
    let data = wide(text);
    // SAFETY: standard clipboard protocol; the global memory is owned by the clipboard on success
    // and freed by us on failure.
    unsafe {
        let mut opened = false;
        for _ in 0..10 {
            if OpenClipboard(std::ptr::null_mut()) != 0 {
                opened = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        if !opened {
            return false;
        }
        let mut ok = false;
        if EmptyClipboard() != 0 {
            let bytes = data.len() * 2;
            let mem = GlobalAlloc(GMEM_MOVEABLE, bytes);
            if !mem.is_null() {
                let ptr = GlobalLock(mem).cast::<u16>();
                if !ptr.is_null() {
                    std::ptr::copy_nonoverlapping(data.as_ptr(), ptr, data.len());
                    GlobalUnlock(mem);
                    if !SetClipboardData(CF_UNICODETEXT as u32, mem).is_null() {
                        ok = true;
                    }
                }
                if !ok {
                    GlobalFree(mem);
                }
            }
        }
        CloseClipboard();
        ok
    }
}

// ----------------------------------------------------------------------------------- COM

/// COM for the current thread, balanced on drop (also when it was already initialized).
struct ComApartment(bool);

impl ComApartment {
    fn init() -> ComApartment {
        // SAFETY: plain COM initialization; S_FALSE (already initialized) must also be balanced.
        let hr = unsafe { CoInitializeEx(std::ptr::null(), (COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) as u32) };
        ComApartment(hr >= 0)
    }
}

impl Drop for ComApartment {
    fn drop(&mut self) {
        if self.0 {
            // SAFETY: balances the successful CoInitializeEx above on the same thread.
            unsafe { CoUninitialize() };
        }
    }
}

const IID_IFILE_OPEN_DIALOG: GUID = GUID::from_u128(0xd57c7288_d4ad_4768_be02_9d969532d960);
const IID_ISHELL_ITEM: GUID = GUID::from_u128(0x43826d1e_e718_42ee_bc55_a1e261c37bfe);

#[repr(C)]
struct IUnknownVtbl {
    query_interface: usize,
    add_ref: usize,
    release: unsafe extern "system" fn(*mut c_void) -> u32,
}

/// `IFileDialog` (+ `IModalWindow`) up to `GetResult`; `IFileOpenDialog` extends it.
#[repr(C)]
struct IFileDialogVtbl {
    base: IUnknownVtbl,
    show: unsafe extern "system" fn(*mut c_void, HWND) -> HRESULT,
    set_file_types: usize,
    set_file_type_index: usize,
    get_file_type_index: usize,
    advise: usize,
    unadvise: usize,
    set_options: unsafe extern "system" fn(*mut c_void, u32) -> HRESULT,
    get_options: unsafe extern "system" fn(*mut c_void, *mut u32) -> HRESULT,
    set_default_folder: usize,
    set_folder: unsafe extern "system" fn(*mut c_void, *mut c_void) -> HRESULT,
    get_folder: usize,
    get_current_selection: usize,
    set_file_name: usize,
    get_file_name: usize,
    set_title: unsafe extern "system" fn(*mut c_void, *const u16) -> HRESULT,
    set_ok_button_label: usize,
    set_file_name_label: usize,
    get_result: unsafe extern "system" fn(*mut c_void, *mut *mut c_void) -> HRESULT,
}

#[repr(C)]
struct IShellItemVtbl {
    base: IUnknownVtbl,
    bind_to_handler: usize,
    get_parent: usize,
    get_display_name: unsafe extern "system" fn(*mut c_void, i32, *mut *mut u16) -> HRESULT,
}

/// An owned COM interface pointer, released on drop.
struct ComPtr(*mut c_void);

impl ComPtr {
    /// SAFETY: `self.0` must be a live interface whose vtable starts with `T`'s layout.
    unsafe fn vtbl<T>(&self) -> &T {
        unsafe { &**(self.0 as *mut *const T) }
    }
}

impl Drop for ComPtr {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: every ComPtr holds one reference to a live COM object.
            unsafe { (self.vtbl::<IUnknownVtbl>().release)(self.0) };
        }
    }
}

/// Native folder picker (`IFileOpenDialog` with `FOS_PICKFOLDERS`), modal to `owner_hwnd`.
/// **Blocks** until the dialog closes: call it on a dedicated thread (see `ipc.rs`), never on the
/// CEF UI thread. `None` when cancelled or on failure.
pub fn pick_folder(owner_hwnd: isize, title: &str, initial_dir: Option<&str>) -> Option<String> {
    let _com = ComApartment::init();
    // SAFETY: COM calls through the documented IFileOpenDialog/IShellItem vtable layouts; every
    // interface pointer is owned by a ComPtr and strings returned by the shell are freed with
    // CoTaskMemFree.
    unsafe {
        let mut raw = std::ptr::null_mut();
        let hr = CoCreateInstance(&FileOpenDialog, std::ptr::null_mut(), CLSCTX_INPROC_SERVER, &IID_IFILE_OPEN_DIALOG, &mut raw);
        if hr < 0 || raw.is_null() {
            log_error!("pick_folder: CoCreateInstance(FileOpenDialog) failed: {hr:#x}");
            return None;
        }
        let dialog = ComPtr(raw);
        let vt = dialog.vtbl::<IFileDialogVtbl>();
        let mut options = 0u32;
        (vt.get_options)(dialog.0, &mut options);
        (vt.set_options)(dialog.0, options | FOS_PICKFOLDERS | FOS_FORCEFILESYSTEM | FOS_PATHMUSTEXIST);
        let title = wide(title);
        (vt.set_title)(dialog.0, title.as_ptr());
        if let Some(dir) = initial_dir.filter(|d| std::path::Path::new(d).is_dir()) {
            let dir = wide(&dir.replace('/', "\\"));
            let mut item = std::ptr::null_mut();
            if SHCreateItemFromParsingName(dir.as_ptr(), std::ptr::null_mut(), &IID_ISHELL_ITEM, &mut item) >= 0 && !item.is_null() {
                let item = ComPtr(item);
                (vt.set_folder)(dialog.0, item.0);
            }
        }
        let hr = (vt.show)(dialog.0, owner_hwnd as HWND);
        if hr < 0 {
            // HRESULT_FROM_WIN32(ERROR_CANCELLED) = 0x800704C7 when the user cancels.
            if hr as u32 != 0x8007_04C7 {
                log_warn!("pick_folder: Show failed: {:#x}", hr as u32);
            }
            return None;
        }
        let mut result = std::ptr::null_mut();
        if (vt.get_result)(dialog.0, &mut result) < 0 || result.is_null() {
            return None;
        }
        let item = ComPtr(result);
        let mut name: *mut u16 = std::ptr::null_mut();
        if (item.vtbl::<IShellItemVtbl>().get_display_name)(item.0, SIGDN_FILESYSPATH, &mut name) < 0 || name.is_null() {
            return None;
        }
        let path = from_wide_ptr(name);
        CoTaskMemFree(name as *const c_void);
        (!path.is_empty()).then_some(path)
    }
}

/// Opens a file or URL with its default handler. Can block (handler start-up, DDE): callers run it
/// on a worker thread.
pub fn shell_open(path: &str) -> bool {
    // ShellExecute may use COM (protocol and shell-extension handlers).
    let _com = ComApartment::init();
    let op = wide("open");
    let file = wide(path);
    // SAFETY: valid NUL-terminated strings; return value > 32 means success.
    let r = unsafe {
        ShellExecuteW(std::ptr::null_mut(), op.as_ptr(), file.as_ptr(), std::ptr::null(), std::ptr::null(), SW_SHOWNORMAL)
    };
    r as isize > 32
}

/// Opens an Explorer window on the containing folder with `path` selected.
pub fn show_in_folder(path: &str) -> bool {
    let native = wide(&path.replace('/', "\\"));
    let _com = ComApartment::init();
    // SAFETY: the PIDL comes from ILCreateFromPathW and is freed with ILFree.
    let ok = unsafe {
        let pidl = ILCreateFromPathW(native.as_ptr());
        if pidl.is_null() {
            false
        } else {
            let hr = SHOpenFolderAndSelectItems(pidl, 0, std::ptr::null(), 0);
            ILFree(pidl);
            hr >= 0
        }
    };
    if !ok {
        log_warn!("show_in_folder: SHOpenFolderAndSelectItems failed for {path}");
    }
    ok
}

/// Renames a directory within its volume. Fails when `to` exists (never replaces or merges) or is
/// on another volume (never copies).
pub fn move_dir(from: &std::path::Path, to: &std::path::Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    let wide_path = |p: &std::path::Path| p.as_os_str().encode_wide().chain(std::iter::once(0)).collect::<Vec<u16>>();
    let (from, to) = (wide_path(from), wide_path(to));
    // SAFETY: valid NUL-terminated UTF-16 paths; flags 0 = no replace, no copy.
    if unsafe { MoveFileExW(from.as_ptr(), to.as_ptr(), 0) } != 0 { Ok(()) } else { Err(std::io::Error::last_os_error()) }
}

/// Where the .msi installed sta (`HKLM\Software\sta` › `InstallDir`, written by
/// `tools/installer/sta.wxs`), or `None` when no such install is registered. A copy of sta running
/// from that directory cannot replace its own files and updates through the next .msi
/// (`update.rs`).
pub fn msi_install_dir() -> Option<PathBuf> {
    let (key, value) = (wide(r"Software\sta"), wide("InstallDir"));
    let mut buffer = [0u16; 1024];
    let mut size = std::mem::size_of_val(&buffer) as u32;
    // SAFETY: valid NUL-terminated strings and an out buffer of the declared size in bytes;
    // `RRF_RT_REG_SZ` guarantees a NUL-terminated string in it on success.
    let status = unsafe {
        RegGetValueW(HKEY_LOCAL_MACHINE, key.as_ptr(), value.as_ptr(), RRF_RT_REG_SZ, std::ptr::null_mut(), buffer.as_mut_ptr().cast::<c_void>(), &mut size)
    };
    if status != ERROR_SUCCESS {
        return None;
    }
    let len = buffer.iter().position(|c| *c == 0).unwrap_or(0);
    (len > 0).then(|| PathBuf::from(String::from_utf16_lossy(&buffer[..len])))
}

/// Bytes this user may still write on the volume that holds `dir` (quotas included), or `None`
/// when Windows cannot say.
pub fn free_disk_bytes(dir: &std::path::Path) -> Option<u64> {
    use std::os::windows::ffi::OsStrExt;
    let wide: Vec<u16> = dir.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
    let mut available: u64 = 0;
    // SAFETY: a NUL-terminated UTF-16 directory path and one valid out pointer; the two totals are
    // not asked for.
    let ok = unsafe { GetDiskFreeSpaceExW(wide.as_ptr(), &mut available, std::ptr::null_mut(), std::ptr::null_mut()) };
    (ok != 0).then_some(available)
}

/// Creates `path` and holds it open for writing, shared for reading only and deleted when the
/// handle closes (also when the process dies), the way Chromium holds its singleton `lockfile`:
/// while it's held, opening it for writing fails, and nothing stays behind afterwards.
pub fn hold_lock_file(path: &std::path::Path) -> std::io::Result<std::fs::File> {
    use std::os::windows::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .access_mode(GENERIC_WRITE | DELETE)
        .share_mode(FILE_SHARE_READ)
        .custom_flags(FILE_FLAG_DELETE_ON_CLOSE)
        .open(path)
}

/// Modal error box without an owner (used before or instead of the main window).
pub fn show_error_box(title: &str, message: &str) {
    let (title, message) = (wide(title), wide(message));
    // SAFETY: valid NUL-terminated strings.
    unsafe {
        MessageBoxW(std::ptr::null_mut(), message.as_ptr(), title.as_ptr(), MB_OK | MB_ICONERROR | MB_SETFOREGROUND | MB_TOPMOST);
    }
}

/// OS preferred UI languages, most preferred first (`["en-US", "de-DE"]`).
pub fn os_ui_languages() -> Vec<String> {
    let (mut num, mut len) = (0u32, 0u32);
    // SAFETY: two-call pattern with a correctly sized buffer.
    unsafe {
        if GetUserPreferredUILanguages(MUI_LANGUAGE_NAME, &mut num, std::ptr::null_mut(), &mut len) == 0 || len == 0 {
            return Vec::new();
        }
        let mut buf = vec![0u16; len as usize];
        if GetUserPreferredUILanguages(MUI_LANGUAGE_NAME, &mut num, buf.as_mut_ptr(), &mut len) == 0 {
            return Vec::new();
        }
        buf.split(|&c| c == 0).filter(|s| !s.is_empty()).map(String::from_utf16_lossy).collect()
    }
}

/// The user's Downloads known folder.
pub fn downloads_dir() -> Option<PathBuf> {
    let mut out: *mut u16 = std::ptr::null_mut();
    // SAFETY: SHGetKnownFolderPath allocates `out`, which we free with CoTaskMemFree.
    let hr = unsafe { SHGetKnownFolderPath(&FOLDERID_Downloads, 0, std::ptr::null_mut(), &mut out) };
    let path = (hr >= 0).then(|| from_wide_ptr(out)).filter(|p| !p.is_empty()).map(PathBuf::from);
    if !out.is_null() {
        // SAFETY: allocated by the shell above.
        unsafe { CoTaskMemFree(out as *const c_void) };
    }
    path.or_else(|| std::env::var_os("USERPROFILE").map(|h| PathBuf::from(h).join("Downloads")))
}

/// SHA-256 of `data` (BCrypt's built-in algorithm handle; extension ids).
pub fn sha256(data: &[u8]) -> Option<[u8; 32]> {
    use windows_sys::Win32::Security::Cryptography::{BCRYPT_SHA256_ALG_HANDLE, BCryptHash};
    let mut out = [0u8; 32];
    // SAFETY: input and output buffers of the declared sizes; no secret (plain hash).
    let status = unsafe { BCryptHash(BCRYPT_SHA256_ALG_HANDLE, std::ptr::null(), 0, data.as_ptr(), data.len() as u32, out.as_mut_ptr(), 32) };
    (status == 0).then_some(out)
}

/// Re-attaches stdio to the parent console (release builds use the GUI subsystem).
pub fn attach_parent_console() {
    // SAFETY: plain Win32 call; failure (no parent console) is harmless.
    unsafe {
        AttachConsole(ATTACH_PARENT_PROCESS);
    }
}

/// The mouse cursor relative to our top-level window (sidebar hover reveal poll). `None` when the
/// cursor position or the window is unavailable (secure desktop, window gone).
///
/// `WindowFromPoint` may send `WM_NCHITTEST` synchronously to windows of this thread (Chromium's
/// hit test runs inside the call): callers must not hold `RefCell` borrows across it.
pub fn cursor_sample(hwnd: isize) -> Option<super::CursorSample> {
    use windows_sys::Win32::Foundation::POINT;
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_LBUTTON, VK_MBUTTON, VK_RBUTTON};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GA_ROOT, GW_OWNER, GetAncestor, GetCursorPos, GetWindow, GetWindowInfo, WINDOWINFO, WindowFromPoint,
    };
    if hwnd == 0 {
        return None;
    }
    let ours = hwnd as HWND;
    // SAFETY: plain Win32 calls with correctly sized out structs; handles are only compared.
    unsafe {
        let mut pt = POINT { x: 0, y: 0 };
        if GetCursorPos(&mut pt) == 0 {
            return None;
        }
        let mut info: WINDOWINFO = std::mem::zeroed();
        info.cbSize = std::mem::size_of::<WINDOWINFO>() as u32;
        if GetWindowInfo(ours, &mut info) == 0 {
            return None;
        }
        let under = WindowFromPoint(pt);
        let root = if under.is_null() { std::ptr::null_mut() } else { GetAncestor(under, GA_ROOT) };
        let over_window = root == ours;
        // A popup owned (directly or through its owners) by our window: select lists, menus.
        let mut owned_popup = false;
        let mut w = root;
        for _ in 0..8 {
            if over_window || w.is_null() {
                break;
            }
            w = GetWindow(w, GW_OWNER);
            if w == ours {
                owned_popup = true;
            }
        }
        let (mut buttons, mut clicked) = (false, false);
        for vk in [VK_LBUTTON, VK_RBUTTON, VK_MBUTTON] {
            let state = GetAsyncKeyState(vk as i32) as u16;
            buttons |= state & 0x8000 != 0;
            clicked |= state & 0x0001 != 0;
        }
        Some(super::CursorSample {
            x: pt.x,
            y: pt.y,
            client_left: info.rcClient.left,
            client_top: info.rcClient.top,
            over_window,
            owned_popup,
            buttons,
            clicked,
        })
    }
}

/// Real OS keyboard input for automated tests (debug builds only). Input is only ever injected
/// while our own window is the foreground window, checked immediately before every key.
#[cfg(debug_assertions)]
pub mod input {
    use windows_sys::Win32::Foundation::HWND;
    use windows_sys::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, MAPVK_VK_TO_VSC, MapVirtualKeyW,
        SendInput,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        BringWindowToTop, GetForegroundWindow, GetWindowThreadProcessId, IsIconic, SW_RESTORE, SetForegroundWindow,
        ShowWindow,
    };

    pub fn foreground_window() -> isize {
        // SAFETY: plain Win32 call.
        unsafe { GetForegroundWindow() as isize }
    }

    /// `pid class "title"` of the current foreground window (diagnostics for aborted input).
    pub fn describe_foreground() -> String {
        use windows_sys::Win32::UI::WindowsAndMessaging::{GetClassNameW, GetWindowTextW};
        // SAFETY: plain Win32 calls with correctly sized buffers.
        unsafe {
            let h = GetForegroundWindow();
            if h.is_null() {
                return "none".into();
            }
            let mut pid = 0u32;
            GetWindowThreadProcessId(h, &mut pid);
            let mut class = [0u16; 128];
            let n = GetClassNameW(h, class.as_mut_ptr(), class.len() as i32).max(0) as usize;
            let mut title = [0u16; 128];
            let t = GetWindowTextW(h, title.as_mut_ptr(), title.len() as i32).max(0) as usize;
            format!("pid {pid} class {} title {:?}", String::from_utf16_lossy(&class[..n]), String::from_utf16_lossy(&title[..t]))
        }
    }

    /// Brings `hwnd` to the foreground (restoring it if minimized). Attaches our input queue to
    /// the current foreground thread so `SetForegroundWindow` is allowed. Never injects input.
    pub fn bring_to_foreground(hwnd: isize) -> bool {
        if hwnd == 0 {
            return false;
        }
        let target = hwnd as HWND;
        // SAFETY: plain Win32 calls on window handles; the thread attachment is undone below.
        unsafe {
            if GetForegroundWindow() == target {
                return true;
            }
            if IsIconic(target) != 0 {
                ShowWindow(target, SW_RESTORE);
            }
            let fg = GetForegroundWindow();
            let fg_thread = if fg.is_null() { 0 } else { GetWindowThreadProcessId(fg, std::ptr::null_mut()) };
            let me = GetCurrentThreadId();
            let attached = fg_thread != 0 && fg_thread != me && AttachThreadInput(me, fg_thread, 1) != 0;
            BringWindowToTop(target);
            SetForegroundWindow(target);
            if attached {
                AttachThreadInput(me, fg_thread, 0);
            }
            GetForegroundWindow() == target
        }
    }

    /// Cursor position in physical screen pixels.
    pub fn cursor_pos() -> Option<(i32, i32)> {
        use windows_sys::Win32::Foundation::POINT;
        use windows_sys::Win32::UI::WindowsAndMessaging::GetCursorPos;
        let mut pt = POINT { x: 0, y: 0 };
        // SAFETY: plain Win32 call with a valid out pointer.
        (unsafe { GetCursorPos(&mut pt) } != 0).then_some((pt.x, pt.y))
    }

    /// Moves the real cursor, only while `hwnd` is the foreground window (checked right before).
    pub fn set_cursor_pos(hwnd: isize, x: i32, y: i32) -> bool {
        use windows_sys::Win32::UI::WindowsAndMessaging::SetCursorPos;
        if hwnd == 0 || foreground_window() != hwnd {
            return false;
        }
        // SAFETY: plain Win32 call.
        unsafe { SetCursorPos(x, y) != 0 }
    }

    /// Puts the cursor back where a guarded [`set_cursor_pos`] found it (no foreground check: only
    /// ever called to undo our own move while the cursor is still where that move left it).
    pub fn restore_cursor_pos(x: i32, y: i32) -> bool {
        use windows_sys::Win32::UI::WindowsAndMessaging::SetCursorPos;
        // SAFETY: plain Win32 call.
        unsafe { SetCursorPos(x, y) != 0 }
    }

    /// Client-area origin of `hwnd` in physical screen pixels.
    pub fn client_origin(hwnd: isize) -> Option<(i32, i32)> {
        use windows_sys::Win32::UI::WindowsAndMessaging::{GetWindowInfo, WINDOWINFO};
        if hwnd == 0 {
            return None;
        }
        // SAFETY: plain Win32 call with a correctly sized out struct.
        unsafe {
            let mut info: WINDOWINFO = std::mem::zeroed();
            info.cbSize = std::mem::size_of::<WINDOWINFO>() as u32;
            (GetWindowInfo(hwnd as HWND, &mut info) != 0).then_some((info.rcClient.left, info.rcClient.top))
        }
    }

    /// Posts a mouse message (`WM_MOUSEMOVE`, `WM_LBUTTONDOWN`, …) to our own window at client
    /// pixel coordinates. Nothing global: the OS cursor doesn't move and no other window sees it.
    pub fn post_mouse(hwnd: isize, msg: u32, wparam: usize, x: i32, y: i32) -> bool {
        use windows_sys::Win32::UI::WindowsAndMessaging::PostMessageW;
        if hwnd == 0 {
            return false;
        }
        let lparam = ((y as u16 as u32) << 16 | (x as u16 as u32)) as isize;
        // SAFETY: posting a message to our own window handle.
        unsafe { PostMessageW(hwnd as HWND, msg, wparam, lparam) != 0 }
    }

    fn is_extended(vk: u16) -> bool {
        matches!(vk, 0x21..=0x28 | 0x2D | 0x2E | 0x5B | 0x5C | 0x6F | 0x90 | 0xA3 | 0xA5)
    }

    /// One key transition via `SendInput`, only if `hwnd` is the foreground window right now.
    /// Returns `false` (nothing sent) otherwise.
    pub fn send_key(hwnd: isize, vk: u16, up: bool) -> bool {
        if hwnd == 0 || foreground_window() != hwnd {
            return false;
        }
        // SAFETY: a correctly sized INPUT array of one keyboard event.
        unsafe {
            let scan = MapVirtualKeyW(vk as u32, MAPVK_VK_TO_VSC) as u16;
            let mut flags = if up { KEYEVENTF_KEYUP } else { 0 };
            if is_extended(vk) {
                flags |= KEYEVENTF_EXTENDEDKEY;
            }
            let input = INPUT {
                r#type: INPUT_KEYBOARD,
                Anonymous: INPUT_0 { ki: KEYBDINPUT { wVk: vk, wScan: scan, dwFlags: flags, time: 0, dwExtraInfo: 0 } },
            };
            SendInput(1, &input, std::mem::size_of::<INPUT>() as i32) == 1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The registry read agrees with `reg query` (and a missing value means light).
    #[test]
    fn dark_mode_matches_reg_query() {
        use std::os::windows::process::CommandExt;
        let out = std::process::Command::new("reg")
            .args(["query", &format!(r"HKCU\{THEME_KEY}"), "/v", "AppsUseLightTheme"])
            .creation_flags(0x0800_0000) // CREATE_NO_WINDOW: `cargo test` must not flash a console
            .output()
            .expect("run reg.exe");
        let text = String::from_utf8_lossy(&out.stdout);
        let expected = text
            .lines()
            .find(|l| l.contains("AppsUseLightTheme"))
            .and_then(|l| l.split_whitespace().last())
            .and_then(|v| u32::from_str_radix(v.trim_start_matches("0x"), 16).ok());
        assert_eq!(apps_use_light_theme(), expected);
        assert_eq!(system_dark_mode(), expected == Some(0));
    }

    #[test]
    fn ui_languages_look_like_bcp47() {
        let langs = os_ui_languages();
        assert!(!langs.is_empty());
        for l in &langs {
            assert!(l.len() >= 2 && l.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'), "{l}");
        }
        assert_eq!(super::super::os_ui_locale(), langs[0]);
    }

    #[test]
    fn downloads_dir_is_absolute() {
        assert!(downloads_dir().is_some_and(|p| p.is_absolute()));
    }

    #[test]
    fn show_in_folder_rejects_missing_paths() {
        assert!(!show_in_folder(r"C:\definitely\not\here\sta-missing.txt"));
    }
}
