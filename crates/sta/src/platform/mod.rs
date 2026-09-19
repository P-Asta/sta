//! OS integration facade [owner: chrome].
//!
//! Responsibility: everything that talks to the operating system directly (not through CEF).
//! Windows is the primary target (`win.rs`); other targets get inert fallbacks so the crate still
//! type-checks.
//!
//! Window handles are passed as `isize` (raw HWND) so callers need no platform types and values
//! can cross threads.
//!
//! Public API:
//! - `pub fn system_dark_mode() -> bool` — OS app theme (registry `AppsUseLightTheme == 0`)
//! - `pub fn system_animations() -> bool` — Windows "Animation effects"
//!   (`SPI_GETCLIENTAREAANIMATION`; `true` when it cannot be read)
//! - `pub fn watch_setting_change(f: fn())` / `pub fn unwatch_setting_change()` — a thread-local
//!   `WH_CALLWNDPROC` observer that calls `f` on `WM_SETTINGCHANGE` (motion, `window.rs`)
//! - `pub fn apply_window_chrome(hwnd: isize, dark: bool)` — DWM immersive dark mode + round corners
//! - `pub fn set_clipboard_text(text: &str) -> bool` — `CF_UNICODETEXT`
//! - `pub fn pick_folder(owner_hwnd: isize, title: &str, initial_dir: Option<&str>) -> Option<String>` —
//!   modern folder picker (`IFileOpenDialog`, `FOS_PICKFOLDERS`); **blocking**, call it on a
//!   dedicated thread
//! - `pub fn shell_open(path: &str) -> bool` (`ShellExecuteW`), `pub fn show_in_folder(path: &str) -> bool`
//!   (`SHOpenFolderAndSelectItems`)
//! - `pub fn show_error_box(title: &str, message: &str)` — fatal errors
//! - `pub fn move_dir(from: &Path, to: &Path) -> io::Result<()>` — same-volume directory rename
//!   that never replaces an existing target (`MoveFileExW` without flags)
//! - `pub fn hold_lock_file(path: &Path) -> io::Result<File>` — a file held open (write, shared for
//!   reading only, deleted on close) that shows a live process uses its folder
//! - `pub fn os_ui_languages() -> Vec<String>`, `pub fn os_ui_locale() -> String`
//! - `pub fn unique_path(dir: &Path, name: &str) -> PathBuf` — `name (n).ext` de-duplication
//! - `pub fn downloads_dir() -> Option<PathBuf>` — `FOLDERID_Downloads`
//! - `pub fn attach_parent_console()` — `--console` in release builds
//! - `pub fn cursor_sample(hwnd: isize) -> Option<CursorSample>` — cursor position, our client
//!   origin, window / owned popup under the cursor, buttons held (sidebar hover reveal)
//! - `pub mod input` (Windows, debug builds): `foreground_window`, `bring_to_foreground`, `send_key`
//!   for real-keyboard tests (`debug.realKeys`); `cursor_pos`, `set_cursor_pos`,
//!   `restore_cursor_pos`, `post_mouse` for the hover reveal checks (`debug.hoverInput`,
//!   `debug.postMouse`)
//! - `pub fn sha256(data: &[u8]) -> Option<[u8; 32]>` (BCrypt; extension ids)
//! - `pub mod hidden_windows` — cloaked Chrome-created windows, activation hooks (foreign.rs)

use std::path::PathBuf;

#[cfg(windows)]
mod win;

#[cfg(windows)]
pub use win::*;

#[cfg(windows)]
pub mod hidden_windows;

/// Inert stand-ins on other platforms (Chrome-created windows are a Windows-only concern today).
#[cfg(not(windows))]
pub mod hidden_windows {
    #[derive(Debug, Clone, serde::Serialize)]
    pub struct WindowDescription {
        pub hwnd: isize,
        pub title: String,
    }
    pub fn install(_main_hwnd: isize) {}
    pub fn uninstall() {}
    pub fn hide_root(_root: isize) -> bool {
        false
    }
    pub fn show_root(_root: isize) {}
    pub fn forget_root(_root: isize) {}
    pub fn is_hidden(_hwnd: isize) -> bool {
        false
    }
    pub fn root_of(hwnd: isize) -> isize {
        hwnd
    }
    pub fn window_title(_hwnd: isize) -> String {
        String::new()
    }
    pub fn is_visible(_hwnd: isize) -> bool {
        false
    }
    pub fn is_window(_hwnd: isize) -> bool {
        false
    }
    pub fn describe(hwnd: isize) -> WindowDescription {
        WindowDescription { hwnd, title: String::new() }
    }
    pub fn styles(_hwnd: isize) -> (u32, u32) {
        (0, 0)
    }
    pub fn set_native_caption(_hwnd: isize, _dark: bool) {}
    pub fn set_sta_owned_listener(_f: fn(isize)) {}
    pub fn allow_dialogs(_root: isize, _allow: bool) {}
    pub fn set_dialog_listener(_f: fn(isize)) {}
    pub fn move_window(_hwnd: isize, _x: i32, _y: i32) -> bool {
        false
    }
    pub fn window_rect(_hwnd: isize) -> Option<[i32; 4]> {
        None
    }
    pub fn client_origin(_hwnd: isize) -> Option<(i32, i32)> {
        None
    }
    pub fn client_rect(_hwnd: isize) -> Option<[i32; 4]> {
        None
    }
    pub fn work_area(_hwnd: isize) -> Option<[i32; 4]> {
        None
    }
    pub fn snapshot() -> serde_json::Value {
        serde_json::Value::Null
    }
}

/// The mouse cursor for the sidebar hover poll (`cursor_sample`). Positions in physical screen
/// pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CursorSample {
    pub x: i32,
    pub y: i32,
    /// Top-left of the window's client area.
    pub client_left: i32,
    pub client_top: i32,
    /// The top-level window under the cursor is ours.
    pub over_window: bool,
    /// The cursor is over a top-level window owned by ours (select list, menu).
    pub owned_popup: bool,
    /// Left, right or middle button held.
    pub buttons: bool,
    /// A button was pressed since the previous sample (best effort: the async key state's
    /// "pressed since the last query" bit).
    pub clicked: bool,
}

#[cfg(not(windows))]
mod fallback {
    use super::PathBuf;

    pub fn cursor_sample(_hwnd: isize) -> Option<super::CursorSample> {
        None
    }

    pub fn system_dark_mode() -> bool {
        false
    }
    pub fn system_animations() -> bool {
        true
    }
    pub fn watch_setting_change(_f: fn()) {}
    pub fn unwatch_setting_change() {}
    pub fn apply_window_chrome(_hwnd: isize, _dark: bool) {}
    pub fn set_clipboard_text(_text: &str) -> bool {
        false
    }
    pub fn pick_folder(_owner_hwnd: isize, _title: &str, _initial_dir: Option<&str>) -> Option<String> {
        None
    }
    pub fn shell_open(_path: &str) -> bool {
        false
    }
    pub fn show_in_folder(_path: &str) -> bool {
        false
    }
    pub fn show_error_box(title: &str, message: &str) {
        eprintln!("[{title}] {message}");
    }
    pub fn move_dir(from: &std::path::Path, to: &std::path::Path) -> std::io::Result<()> {
        if to.exists() {
            return Err(std::io::Error::from(std::io::ErrorKind::AlreadyExists));
        }
        std::fs::rename(from, to)
    }
    pub fn hold_lock_file(path: &std::path::Path) -> std::io::Result<std::fs::File> {
        std::fs::OpenOptions::new().write(true).create(true).truncate(true).open(path)
    }
    pub fn os_ui_languages() -> Vec<String> {
        Vec::new()
    }
    pub fn downloads_dir() -> Option<PathBuf> {
        std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Downloads"))
    }
    pub fn attach_parent_console() {}
    pub fn sha256(_data: &[u8]) -> Option<[u8; 32]> {
        None
    }
}

#[cfg(not(windows))]
pub use fallback::*;

/// Most preferred OS UI language (`en-US`), or empty (CEF then uses en-US).
pub fn os_ui_locale() -> String {
    os_ui_languages().into_iter().next().unwrap_or_default()
}

/// Multi-part extensions kept together when numbering (`archive (1).tar.gz`).
const DOUBLE_EXTENSIONS: &[&str] = &[".tar.gz", ".tar.bz2", ".tar.xz", ".tar.zst", ".user.js"];

/// A path that does not exist yet: `dir/name`, else `dir/stem (n).ext` (n = 1, 2, …).
pub fn unique_path(dir: &std::path::Path, name: &str) -> PathBuf {
    let candidate = dir.join(name);
    if !candidate.exists() {
        return candidate;
    }
    let lower = name.to_ascii_lowercase();
    let split = DOUBLE_EXTENSIONS
        .iter()
        .find(|ext| lower.ends_with(*ext) && lower.len() > ext.len())
        .map(|ext| name.len() - ext.len())
        .or_else(|| name.rfind('.').filter(|&i| i > 0));
    let (stem, ext) = match split {
        Some(i) => (&name[..i], &name[i..]),
        None => (name, ""),
    };
    (1..10_000)
        .map(|n| dir.join(format!("{stem} ({n}){ext}")))
        .find(|p| !p.exists())
        .unwrap_or(candidate)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unique_path_numbers_like_chrome() {
        let dir = std::env::temp_dir().join(format!("sta-unique-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let touch = |n: &str| std::fs::write(dir.join(n), b"x").unwrap();

        assert_eq!(unique_path(&dir, "report.pdf"), dir.join("report.pdf"));
        touch("report.pdf");
        assert_eq!(unique_path(&dir, "report.pdf"), dir.join("report (1).pdf"));
        touch("report (1).pdf");
        assert_eq!(unique_path(&dir, "report.pdf"), dir.join("report (2).pdf"));

        touch("archive.tar.gz");
        assert_eq!(unique_path(&dir, "archive.tar.gz"), dir.join("archive (1).tar.gz"));
        touch("README");
        assert_eq!(unique_path(&dir, "README"), dir.join("README (1)"));
        touch(".env");
        assert_eq!(unique_path(&dir, ".env"), dir.join(".env (1)"));
        touch("my.file.name.txt");
        assert_eq!(unique_path(&dir, "my.file.name.txt"), dir.join("my.file.name (1).txt"));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
