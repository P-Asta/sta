//! Native window, capture, clipboard and file tools of the test surface (bucket D of the design).
//!
//! Together with [`super::capture`] these replace every PowerShell call the suites made
//! (`win-probe.ps1`, `capture-window.ps1`, `png-pixel.ps1`, `Get-Clipboard`, `Get-Content
//! -Stream`), which is what removes the console-window flashes the user asked about. The scripts
//! stay in the repo as human and agent debugging tools.
//!
//! Everything here is scoped to **this process's own windows**: messages are only ever sent to a
//! window that belongs to us, and a capture draws one such window, never the screen.

use super::{bool_arg, err, invalid, opt_str, out, points_arg, str_arg};
use crate::automation::tools::Output;
use crate::test_hooks::{capture, console_watch};
use cef::{ImplDisplay, ImplWindow};
use serde_json::{Value, json};
use sta_core::agent::ErrorCode;
use sta_core::agent::channel::ToolError;
use sta_core::agent::test_tools::MAX_INLINE_BASE64;

fn internal(message: impl Into<String>) -> ToolError {
    err(ErrorCode::Internal, message)
}

fn main_hwnd() -> Result<isize, ToolError> {
    let hwnd = crate::window::hwnd_value();
    if hwnd == 0 {
        return Err(err(ErrorCode::NoSuchTarget, "sta has no window (starting up or shutting down)"));
    }
    Ok(hwnd)
}

/// The window a tool acts on: `hwnd` when given (and it belongs to this process), else the main
/// window.
fn target_hwnd(args: &Value) -> Result<isize, ToolError> {
    match args.get("hwnd").and_then(Value::as_i64) {
        Some(h) => {
            let hwnd = h as isize;
            if !win::belongs_to_us(hwnd) {
                return Err(invalid(format!("window {hwnd} does not belong to this process")));
            }
            Ok(hwnd)
        }
        None => main_hwnd(),
    }
}

/// Device pixels per DIP, the way Views counts them: the **display's** device scale factor, which
/// `--force-device-scale-factor` sets and `GetDpiForWindow` knows nothing about (`debug.rs::scale`
/// reads the same number, so `test_post_mouse` and `test_pixels` agree). A forced factor otherwise
/// made every `space: "dip"` point sample the wrong pixel, silently (LAY-4). Falls back to the
/// window's DPI before the window exists.
fn scale_of(hwnd: isize) -> f64 {
    crate::window::main_window()
        .and_then(|w| w.display())
        .map(|d| d.device_scale_factor() as f64)
        .filter(|s| *s > 0.0)
        .unwrap_or_else(|| capture::dpi_of(hwnd) as f64 / 96.0)
}

/// `test_window`: what `win-probe.ps1 info` (and `modifiers`, and `dialogs`) reported.
pub fn window(args: &Value) -> Result<Output, ToolError> {
    let hwnd = main_hwnd()?;
    let mut info = win::info(hwnd);
    info["modifiers"] = win::modifiers();
    if bool_arg(args, "all", false) {
        let windows = win::top_level_windows();
        return Ok(out(format!("{} top-level window(s)", windows.len()), json!({ "main": info, "windows": windows })));
    }
    Ok(out("window info", info))
}

/// `test_hit_test`: `WM_NCHITTEST` at window-relative points (device pixels by default, like
/// `win-probe.ps1 hittest`; `space: "dip"` scales by the window's DPI first).
pub fn hit_test(args: &Value) -> Result<Output, ToolError> {
    let hwnd = main_hwnd()?;
    let points = points_arg(args, "points")?;
    let scale = if opt_str(args, "space").as_deref() == Some("dip") { scale_of(hwnd) } else { 1.0 };
    let codes: Vec<i32> = points.iter().map(|(x, y)| win::hit_test(hwnd, (x * scale).round() as i32, (y * scale).round() as i32)).collect();
    Ok(out(format!("{} hit test(s)", codes.len()), json!({ "codes": codes })))
}

/// `test_window_message`: `WM_CLOSE`, `SC_RESTORE` or `SC_MINIMIZE`, posted to one of our windows.
pub fn window_message(args: &Value) -> Result<Output, ToolError> {
    let hwnd = target_hwnd(args)?;
    let message = str_arg(args, "message")?;
    let posted = win::post(hwnd, &message).ok_or_else(|| invalid(format!("unknown message {message:?} (close, restore or minimize)")))?;
    Ok(out(format!("posted {posted}"), json!({ "posted": posted, "hwnd": hwnd })))
}

/// `test_dialog`: presses a key in one of sta's own **modal dialogs** — the one window a suite
/// otherwise cannot answer.
///
/// Chromium's "Add <extension>?" dialog (and its siblings) are views widgets: their buttons are not
/// child windows, so there is no `WM_COMMAND` to post, and the dialog takes the foreground itself, so
/// `test_real_keys` — which insists on sta's *main* window being foreground — refuses and would steal
/// the focus away. This sends one real key transition pair to the dialog instead.
///
/// Scope: a visible window of this process owned by **one of sta's own top-level windows** — sta's
/// main window, or the hidden `chrome://extensions` window of an extension operation, whose "Remove
/// …?" confirmation is the only window a person could otherwise have to answer by hand
/// (`ext_backend.rs`). Never the main window itself (that is `test_real_keys`), never another
/// process's window, and never a window with no owner at all.
pub fn dialog(args: &Value) -> Result<Output, ToolError> {
    let main = main_hwnd()?;
    let hwnd = match args.get("hwnd").and_then(Value::as_i64) {
        Some(h) => h as isize,
        None => win::foreground(),
    };
    if hwnd == 0 || !win::belongs_to_us(hwnd) {
        return Err(err(ErrorCode::NoSuchTarget, format!("window {hwnd} does not belong to this process")));
    }
    if hwnd == main {
        return Err(invalid("that is sta's main window; use test_real_keys"));
    }
    let owner = win::owner(hwnd);
    if owner == 0 || !win::belongs_to_us(owner) {
        return Err(invalid(format!("window {hwnd} is not owned by a window of sta's")));
    }
    if !win::visible(hwnd) {
        return Err(err(ErrorCode::NoSuchTarget, format!("window {hwnd} is not visible")));
    }
    let press = opt_str(args, "press").unwrap_or_else(|| "enter".to_string());
    // The keys a dialog answers: its default button, cancel, the focused button, and the way to it.
    // (Only the Windows branch below sends it; the argument is still validated everywhere.)
    #[cfg_attr(not(windows), allow(unused_variables))]
    let vk: u16 = match press.as_str() {
        "enter" => 0x0D,
        "escape" => 0x1B,
        "space" => 0x20,
        "tab" => 0x09,
        other => return Err(invalid(format!("unknown press {other:?} (enter, escape, space or tab)"))),
    };
    let info = win::info(hwnd);
    if win::foreground() != hwnd {
        return Err(err(ErrorCode::WindowBusy, format!("window {hwnd} is not the foreground window; nothing was sent")));
    }
    #[cfg(windows)]
    let sent = crate::platform::input::send_key(hwnd, vk, false) && crate::platform::input::send_key(hwnd, vk, true);
    #[cfg(not(windows))]
    let sent = false;
    if !sent {
        return Err(err(ErrorCode::WindowBusy, format!("the foreground changed while sending {press} to {hwnd}")));
    }
    Ok(out(format!("pressed {press} in window {hwnd}"), json!({ "hwnd": hwnd, "press": press, "sent": sent, "window": info })))
}

/// `test_capture`: a PNG of one of our windows.
pub fn capture_window(args: &Value) -> Result<Output, ToolError> {
    let hwnd = target_hwnd(args)?;
    let client_only = opt_str(args, "region").as_deref() == Some("client");
    let image = capture::window(hwnd, client_only).map_err(internal)?;
    let png = capture::write_png(&image).map_err(internal)?;
    let scale = scale_of(hwnd);
    if bool_arg(args, "inline", false) {
        let data = base64(&png);
        if data.len() > MAX_INLINE_BASE64 {
            return Err(err(ErrorCode::TooLarge, format!("the capture is {} base64 characters; write it to a file instead", data.len())));
        }
        return Ok(out(
            format!("{}x{} inline capture", image.width, image.height),
            json!({ "data": data, "width": image.width, "height": image.height, "scale": scale }),
        ));
    }
    let path = match opt_str(args, "out") {
        Some(p) => std::path::PathBuf::from(p),
        None => default_capture_path()?,
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    std::fs::write(&path, &png).map_err(|e| internal(format!("cannot write {}: {e}", path.display())))?;
    Ok(out(
        format!("saved {} ({}x{})", path.display(), image.width, image.height),
        json!({ "path": path.to_string_lossy(), "width": image.width, "height": image.height, "scale": scale, "bytes": png.len() }),
    ))
}

fn default_capture_path() -> Result<std::path::PathBuf, ToolError> {
    let dirs = crate::paths::try_dirs().ok_or_else(|| internal("no data directory yet"))?;
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
    Ok(dirs.logs.join(format!("capture-{stamp}.png")))
}

fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(ALPHABET[(n >> 18) as usize & 63] as char);
        out.push(ALPHABET[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 { ALPHABET[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { ALPHABET[n as usize & 63] as char } else { '=' });
    }
    out
}

/// `test_pixels`: colors of a capture, in DIP by default (`png-pixel.ps1` took image pixels, which
/// is `space: "device"`).
pub fn pixels(args: &Value) -> Result<Output, ToolError> {
    let path = str_arg(args, "path")?;
    let points = points_arg(args, "points")?;
    let bytes = std::fs::read(&path).map_err(|e| err(ErrorCode::NoSuchTarget, format!("cannot read {path}: {e}")))?;
    let image = capture::read_png(&bytes).map_err(|e| invalid(format!("{path}: {e}")))?;
    let scale = if opt_str(args, "space").as_deref() == Some("device") { 1.0 } else { scale_of(crate::window::hwnd_value()) };
    let colors: Vec<String> = points
        .iter()
        .map(|(x, y)| {
            let px = ((x * scale).round() as i64).clamp(0, image.width as i64 - 1) as u32;
            let py = ((y * scale).round() as i64).clamp(0, image.height as i64 - 1) as u32;
            let [r, g, b, _] = image.pixel(px, py).unwrap_or([0, 0, 0, 0]);
            format!("#{r:02x}{g:02x}{b:02x}")
        })
        .collect();
    Ok(out(format!("{} pixel(s)", colors.len()), json!({ "colors": colors, "width": image.width, "height": image.height, "scale": scale })))
}

pub fn clipboard_get() -> Result<Output, ToolError> {
    let text = win::clipboard_text();
    Ok(out(match &text {
        Some(t) => format!("{} clipboard characters", t.chars().count()),
        None => "the clipboard holds no text".to_string(),
    }, json!({ "text": text })))
}

pub fn clipboard_set(args: &Value) -> Result<Output, ToolError> {
    let text = str_arg(args, "text")?;
    if !crate::platform::set_clipboard_text(&text) {
        return Err(internal("the clipboard could not be opened"));
    }
    Ok(out("clipboard set", json!({ "text": text })))
}

/// `test_zone_identifier`: the Mark-of-the-Web alternate data stream of a downloaded file.
pub fn zone_identifier(args: &Value) -> Result<Output, ToolError> {
    let path = str_arg(args, "path")?;
    if !std::path::Path::new(&path).exists() {
        return Err(err(ErrorCode::NoSuchTarget, format!("{path} does not exist")));
    }
    let zone = std::fs::read_to_string(format!("{path}:Zone.Identifier")).ok();
    Ok(out(
        match &zone {
            Some(_) => "the file carries a Zone.Identifier".to_string(),
            None => "no Zone.Identifier".to_string(),
        },
        json!({ "zone": zone }),
    ))
}

/// `test_console_windows`: the user's "no cmd window" requirement, asserted at runtime.
pub fn console_windows(args: &Value) -> Result<Output, ToolError> {
    if bool_arg(args, "reset", false) {
        console_watch::reset();
    }
    // The browser's own pid is always a root: a console window opened by anything sta started is
    // ours, whether or not the caller thought of it.
    let mut roots: Vec<u32> = vec![console_watch::our_pid()];
    if let Some(list) = args.get("roots").and_then(Value::as_array) {
        roots.extend(list.iter().filter_map(Value::as_u64).map(|p| p as u32));
    }
    let snapshot = console_watch::snapshot(&roots);
    let (current, seen, ours) = (
        snapshot["current"].as_array().map_or(0, Vec::len),
        snapshot["seen"].as_array().map_or(0, Vec::len),
        snapshot["ours"].as_array().map_or(0, Vec::len),
    );
    let visible = snapshot["seen"].as_array().map_or(0, |list| list.iter().filter(|e| e["userVisible"] == json!(true)).count());
    Ok(out(format!("{current} console window(s) now, {seen} seen ({visible} user-visible), {ours} ours"), snapshot))
}

// ----------------------------------------------------------------------------------- Win32

#[cfg(windows)]
mod win {
    use serde_json::{Value, json};
    use windows_sys::Win32::Foundation::{HWND, LPARAM, WPARAM};
    use windows_sys::Win32::System::DataExchange::{CloseClipboard, GetClipboardData, OpenClipboard};
    use windows_sys::Win32::System::Memory::{GlobalLock, GlobalUnlock};
    use windows_sys::Win32::System::Ole::CF_UNICODETEXT;
    use windows_sys::Win32::System::Threading::GetCurrentProcessId;
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, IsWindowEnabled, VK_CONTROL, VK_MENU, VK_SHIFT};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GWL_EXSTYLE, GWL_STYLE, GetClassNameW, GetForegroundWindow, GetWindow, GetWindowLongW, GetWindowRect, GetWindowThreadProcessId,
        GW_OWNER, InternalGetWindowText, IsIconic, IsWindowVisible, IsZoomed, PostMessageW, SC_MINIMIZE, SC_RESTORE, SendMessageW, WM_CLOSE,
        WM_NCHITTEST, WM_SYSCOMMAND, WS_THICKFRAME,
    };

    pub fn belongs_to_us(hwnd: isize) -> bool {
        if hwnd == 0 {
            return false;
        }
        let mut pid = 0u32;
        // SAFETY: plain Win32 calls on a window handle.
        unsafe {
            GetWindowThreadProcessId(hwnd as HWND, &mut pid);
            pid != 0 && pid == GetCurrentProcessId()
        }
    }

    pub fn owner(hwnd: isize) -> isize {
        // SAFETY: plain Win32 call on a window handle.
        unsafe { GetWindow(hwnd as HWND, GW_OWNER) as isize }
    }

    pub fn visible(hwnd: isize) -> bool {
        // SAFETY: plain Win32 call on a window handle.
        hwnd != 0 && unsafe { IsWindowVisible(hwnd as HWND) } != 0
    }

    pub fn foreground() -> isize {
        // SAFETY: plain Win32 call.
        unsafe { GetForegroundWindow() as isize }
    }

    fn class_name(hwnd: HWND) -> String {
        let mut buf = [0u16; 128];
        // SAFETY: correctly sized buffer.
        let n = unsafe { GetClassNameW(hwnd, buf.as_mut_ptr(), buf.len() as i32) }.max(0) as usize;
        String::from_utf16_lossy(&buf[..n])
    }

    fn title(hwnd: HWND) -> String {
        let mut buf = [0u16; 256];
        // SAFETY: correctly sized buffer; no WM_GETTEXT is sent.
        let n = unsafe { InternalGetWindowText(hwnd, buf.as_mut_ptr(), buf.len() as i32) }.max(0) as usize;
        String::from_utf16_lossy(&buf[..n])
    }

    fn rect(hwnd: HWND) -> [i32; 4] {
        // SAFETY: plain Win32 call with a zeroed out struct.
        unsafe {
            let mut r = std::mem::zeroed();
            if GetWindowRect(hwnd, &mut r) == 0 {
                return [0, 0, 0, 0];
            }
            [r.left, r.top, r.right - r.left, r.bottom - r.top]
        }
    }

    pub fn info(hwnd: isize) -> Value {
        let h = hwnd as HWND;
        let [left, top, width, height] = rect(h);
        // SAFETY: plain Win32 calls on our own window handle.
        unsafe {
            let style = GetWindowLongW(h, GWL_STYLE) as u32;
            json!({
                "hwnd": hwnd,
                "left": left,
                "top": top,
                "width": width,
                "height": height,
                "dpi": super::capture::dpi_of(hwnd),
                "zoomed": IsZoomed(h) != 0,
                "iconic": IsIconic(h) != 0,
                "thickFrame": style & WS_THICKFRAME != 0,
                "enabled": IsWindowEnabled(h) != 0,
                "foreground": GetForegroundWindow() == h,
                "className": class_name(h),
                "title": title(h),
            })
        }
    }

    pub fn modifiers() -> Value {
        // SAFETY: plain Win32 calls.
        let down = |vk: i32| unsafe { GetAsyncKeyState(vk) as u16 & 0x8000 != 0 };
        json!({ "ctrl": down(VK_CONTROL as i32), "shift": down(VK_SHIFT as i32), "alt": down(VK_MENU as i32) })
    }

    unsafe extern "system" fn collect(hwnd: HWND, lparam: LPARAM) -> i32 {
        // SAFETY: `lparam` is the &mut Vec `EnumWindows` was called with, valid for the call.
        let out = unsafe { &mut *(lparam as *mut Vec<Value>) };
        let mut pid = 0u32;
        // SAFETY: plain Win32 calls on a window handle.
        unsafe {
            GetWindowThreadProcessId(hwnd, &mut pid);
            if pid != GetCurrentProcessId() {
                return 1;
            }
            let [left, top, width, height] = rect(hwnd);
            out.push(json!({
                "hwnd": hwnd as isize,
                "className": class_name(hwnd),
                "title": title(hwnd),
                "owner": GetWindow(hwnd, GW_OWNER) as isize,
                "visible": IsWindowVisible(hwnd) != 0,
                "hidden": crate::platform::hidden_windows::is_hidden(hwnd as isize),
                // The two style words: what `foreign.rs::is_install_dialog_style` reads, so an e2e can
                // assert the signature gate S17 measured instead of guessing from the class name
                // (Chromium's dialogs are `Chrome_WidgetWin_1` views widgets, not `#32770`).
                "style": GetWindowLongW(hwnd, GWL_STYLE) as u32,
                "exStyle": GetWindowLongW(hwnd, GWL_EXSTYLE) as u32,
                "enabled": IsWindowEnabled(hwnd) != 0,
                "left": left,
                "top": top,
                "width": width,
                "height": height,
            }));
        }
        1
    }

    /// Every top-level window of this process, cloaked and Chrome-created ones included.
    pub fn top_level_windows() -> Vec<Value> {
        let mut out: Vec<Value> = Vec::new();
        // SAFETY: `collect` only writes through the pointer we pass, which outlives the call.
        unsafe { EnumWindows(Some(collect), (&mut out as *mut Vec<Value>) as LPARAM) };
        out
    }

    pub fn hit_test(hwnd: isize, x: i32, y: i32) -> i32 {
        let [left, top, _, _] = rect(hwnd as HWND);
        let (sx, sy) = (left + x, top + y);
        let lparam = (((sy as u16 as u32) << 16) | (sx as u16 as u32)) as LPARAM;
        // SAFETY: a message to our own window; WM_NCHITTEST has no pointer parameters.
        unsafe { SendMessageW(hwnd as HWND, WM_NCHITTEST, 0 as WPARAM, lparam) as i32 }
    }

    pub fn post(hwnd: isize, message: &str) -> Option<&'static str> {
        let h = hwnd as HWND;
        // SAFETY: posting a message to a window of this process.
        unsafe {
            match message {
                "close" => {
                    PostMessageW(h, WM_CLOSE, 0, 0);
                    Some("WM_CLOSE")
                }
                "restore" => {
                    PostMessageW(h, WM_SYSCOMMAND, SC_RESTORE as WPARAM, 0);
                    Some("SC_RESTORE")
                }
                "minimize" => {
                    PostMessageW(h, WM_SYSCOMMAND, SC_MINIMIZE as WPARAM, 0);
                    Some("SC_MINIMIZE")
                }
                _ => None,
            }
        }
    }

    pub fn clipboard_text() -> Option<String> {
        // SAFETY: the standard clipboard read protocol; the handle stays owned by the clipboard
        // and is only locked for the copy.
        unsafe {
            if OpenClipboard(std::ptr::null_mut()) == 0 {
                return None;
            }
            let handle = GetClipboardData(CF_UNICODETEXT as u32);
            let mut text = None;
            if !handle.is_null() {
                let ptr = GlobalLock(handle).cast::<u16>();
                if !ptr.is_null() {
                    let len = (0..).take_while(|&i| *ptr.add(i) != 0).take(1 << 22).count();
                    text = Some(String::from_utf16_lossy(std::slice::from_raw_parts(ptr, len)));
                    GlobalUnlock(handle);
                }
            }
            CloseClipboard();
            text
        }
    }
}

#[cfg(not(windows))]
mod win {
    use serde_json::{Value, json};

    pub fn belongs_to_us(_hwnd: isize) -> bool {
        false
    }
    pub fn owner(_hwnd: isize) -> isize {
        0
    }
    pub fn visible(_hwnd: isize) -> bool {
        false
    }
    pub fn foreground() -> isize {
        0
    }
    pub fn info(hwnd: isize) -> Value {
        json!({ "hwnd": hwnd })
    }
    pub fn modifiers() -> Value {
        json!({ "ctrl": false, "shift": false, "alt": false })
    }
    pub fn top_level_windows() -> Vec<Value> {
        Vec::new()
    }
    pub fn hit_test(_hwnd: isize, _x: i32, _y: i32) -> i32 {
        0
    }
    pub fn post(_hwnd: isize, _message: &str) -> Option<&'static str> {
        None
    }
    pub fn clipboard_text() -> Option<String> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_the_standard_alphabet_and_padding() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
        assert_eq!(base64(&[0xff, 0xef, 0xbf]), "/++/");
    }

    #[test]
    fn zone_identifier_reports_a_missing_file() {
        // `Output` has no `Debug`, so no `unwrap_err` here.
        let Err(e) = zone_identifier(&json!({ "path": r"C:\definitely\not\here\sta-test.bin" })) else { panic!("a missing file must fail") };
        assert_eq!(e.code, ErrorCode::NoSuchTarget);
    }
}
