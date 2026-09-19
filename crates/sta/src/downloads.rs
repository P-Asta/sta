//! Downloads [owner: tabs] (ARCHITECTURE §4.1 "Downloads", docs/research/handlers.md §9).
//!
//! Responsibility: `DownloadHandler` logic for web tabs and `Effect::DownloadControl`.
//!
//! - `can_download` allows everything (the Rust trait default 0 would cancel every download);
//! - `on_before_download` saves to a de-duplicated path (`name (n).ext`) in the download dir:
//!   `Settings.download_dir` when absolute and creatable, else the OS Downloads folder, always
//!   with native separators (Chromium interrupts a `C:/dir\file` target); honours
//!   `ask_download_location` (native Save As);
//! - `on_download_updated` → `DownloadUpdated{id, tab, url, fileName, path, receivedBytes,
//!   totalBytes, bytesPerSec, state, startedAt}` and keeps the `DownloadItemCallback` per id for
//!   pause/resume/cancel (also for interrupted downloads, which can resume). When the browser that
//!   started it has **no document at all** — a Peek opened for a link that turned out to be a
//!   `Content-Disposition` attachment — it also dispatches `DownloadInBlankTab{tab}` once, after
//!   the first `DownloadUpdated` (so core already defers destroying that browser), and core throws
//!   the empty overlay away;
//! - Mark-of-the-Web: when a download completes (before `DownloadUpdated{complete}` is
//!   dispatched, so "Open" can never run on an unmarked file) the file gets a `Zone.Identifier`
//!   alternate data stream (`ZoneId=3`, `ReferrerUrl` = the page that started the download when it
//!   is http(s), `HostUrl` = the download URL, or `about:internet` for `data:`/`blob:`/`file:`
//!   sources; credentials stripped), like Chrome's quarantine. SmartScreen and Office Protected
//!   View use it when the user opens the file. CEF's Alloy runtime doesn't annotate downloads;
//! - `DownloadControl` Open / ShowInFolder via `platform::{shell_open, show_in_folder}` on a worker
//!   thread (both can block while Explorer or the handler starts). Retry never
//!   reaches the shell: core turns it into `Effect::StartDownload{tab, url}` (tabs.rs).
//!
//! Chromium asks before a page starts a second automatic download: that arrives as a permission
//! prompt (permissions.rs, kind `Other`).
//!
//! Public API:
//! - `pub fn can_download(browser_id: i32, url: &str, method: &str) -> bool`
//! - `pub fn on_before_download(browser_id: i32, item: &DownloadItem, suggested_name: &str, callback: &BeforeDownloadCallback) -> bool`
//! - `pub fn on_download_updated(browser_id: i32, item: &DownloadItem, callback: Option<DownloadItemCallback>)`
//! - `pub fn control(id: u32, action: DownloadAction)`
//! - `pub fn in_progress_count() -> usize` — downloads not finished yet (running or paused)
//! - `pub fn cancel_all_in_progress()` — shutdown: cancel them before closing browsers
//! - `pub fn notify_when_idle(f: impl FnOnce() + 'static)` — runs `f` (posted) once the count is 0
//! - `pub fn clear()`

use crate::{browsers, controller, platform, tabs};
use sta_core::{Command, Download, DownloadAction, DownloadState, now_ms};
use cef::*;
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;

struct Tracked {
    callback: Option<DownloadItemCallback>,
    path: Option<String>,
    started_at: i64,
    /// URL of the page that started the download (Mark-of-the-Web `ReferrerUrl`).
    referrer: Option<String>,
    /// The `Zone.Identifier` stream was written for the completed file.
    marked: bool,
    /// Not complete, cancelled or interrupted (Chromium would ask before closing its last window).
    in_progress: bool,
    /// `DownloadInBlankTab` was already dispatched for this download (once per download).
    reported_blank: bool,
}

impl Tracked {
    fn new() -> Self {
        Tracked { callback: None, path: None, started_at: now_ms(), referrer: None, marked: false, in_progress: true, reported_blank: false }
    }
}

thread_local! {
    static DOWNLOADS: RefCell<HashMap<u32, Tracked>> = RefCell::new(HashMap::new());
    static IDLE_WAITERS: RefCell<Vec<Box<dyn FnOnce()>>> = RefCell::new(Vec::new());
}

/// Downloads that are still running or paused.
pub fn in_progress_count() -> usize {
    DOWNLOADS.with(|d| d.borrow().values().filter(|t| t.in_progress).count())
}

/// Cancels every running or paused download (shutdown, before any browser closes).
pub fn cancel_all_in_progress() {
    let callbacks: Vec<(u32, DownloadItemCallback)> =
        DOWNLOADS.with(|d| d.borrow().iter().filter(|(_, t)| t.in_progress).filter_map(|(id, t)| t.callback.clone().map(|c| (*id, c))).collect());
    if !callbacks.is_empty() {
        log_info!("shutdown: cancelling {} download(s)", callbacks.len());
    }
    for (id, callback) in callbacks {
        callback.cancel();
        DOWNLOADS.with(|d| {
            if let Some(t) = d.borrow_mut().get_mut(&id) {
                t.in_progress = false;
            }
        });
    }
    wake_idle_waiters();
}

/// Runs `f` once no download is in progress (posted; at once when none is).
pub fn notify_when_idle(f: impl FnOnce() + 'static) {
    IDLE_WAITERS.with(|w| w.borrow_mut().push(Box::new(f)));
    wake_idle_waiters();
}

fn wake_idle_waiters() {
    if in_progress_count() > 0 {
        return;
    }
    let waiters = IDLE_WAITERS.with(|w| std::mem::take(&mut *w.borrow_mut()));
    for f in waiters {
        crate::task::post_ui(f);
    }
}

pub fn can_download(browser_id: i32, url: &str, method: &str) -> bool {
    let _ = (browser_id, url, method);
    true
}

/// The download directory (created if needed): `Settings.download_dir` when it is a usable
/// absolute path, else the OS Downloads folder. Always with native separators: Chromium interrupts
/// a download whose target path mixes `/` and `\` (`C:/dir\file`).
fn download_dir() -> PathBuf {
    let native = |p: PathBuf| std::path::absolute(&p).unwrap_or(p);
    let configured = controller::with_store(|s| s.settings().download_dir.clone())
        .flatten()
        .filter(|d| !d.trim().is_empty())
        .map(|d| PathBuf::from(d.trim()))
        .filter(|p| p.is_absolute())
        .map(native)
        .filter(|p| std::fs::create_dir_all(p).is_ok());
    configured.unwrap_or_else(|| {
        let dir = native(platform::downloads_dir().unwrap_or_else(std::env::temp_dir));
        let _ = std::fs::create_dir_all(&dir);
        dir
    })
}

fn sanitize_file_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| if matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*') || c.is_control() { '_' } else { c })
        .collect();
    let cleaned = cleaned.trim().trim_end_matches(|c: char| c == '.' || c.is_whitespace()).to_string();
    if cleaned.is_empty() {
        return "download".into();
    }
    // DOS device names (`CON`, `nul.txt`, `COM1.log`, …) must not reach the file system as-is.
    let stem = cleaned.split('.').next().unwrap_or_default().trim_end().to_ascii_uppercase();
    let device = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$")
        || (stem.len() == 4
            && (stem.starts_with("COM") || stem.starts_with("LPT"))
            && stem.as_bytes()[3].is_ascii_digit()
            && stem.as_bytes()[3] != b'0');
    if device { format!("_{cleaned}") } else { cleaned }
}

pub fn on_before_download(browser_id: i32, item: &DownloadItem, suggested_name: &str, callback: &BeforeDownloadCallback) -> bool {
    // Downloads started in agent-controlled tabs wait for the user (automation/guards.rs).
    if crate::automation::guards::hold_download(browser_id, item.id(), suggested_name, callback) {
        return true;
    }
    continue_download(browser_id, item.id(), suggested_name, callback);
    true
}

/// Saves download `id` into the download directory (also a held agent download the user kept).
pub fn continue_download(browser_id: i32, id: u32, suggested_name: &str, callback: &BeforeDownloadCallback) {
    let ask = controller::with_store(|s| s.settings().ask_download_location).unwrap_or(false);
    let dir = download_dir();
    let path = platform::unique_path(&dir, &sanitize_file_name(suggested_name));
    let path_str = path.to_string_lossy().into_owned();
    let page_url = browsers::browser(browser_id)
        .and_then(|b| b.main_frame())
        .map(|f| CefString::from(&f.url()).to_string())
        .filter(|u| !u.is_empty());
    DOWNLOADS.with(|d| {
        let mut d = d.borrow_mut();
        let t = d.entry(id).or_insert_with(Tracked::new);
        t.path = Some(path_str.clone());
        if t.referrer.is_none() {
            t.referrer = page_url;
        }
    });
    callback.cont(Some(&CefString::from(path_str.as_str())), ask as i32);
}

pub fn on_download_updated(browser_id: i32, item: &DownloadItem, callback: Option<DownloadItemCallback>) {
    let id = item.id();
    let full_path = CefString::from(&item.full_path()).to_string();
    let started_at = DOWNLOADS.with(|d| {
        let mut d = d.borrow_mut();
        let t = d.entry(id).or_insert_with(Tracked::new);
        if callback.is_some() {
            t.callback = callback;
        }
        if !full_path.is_empty() {
            t.path = Some(full_path.clone());
        }
        t.started_at
    });
    let state = if item.is_complete() != 0 {
        DownloadState::Complete
    } else if item.is_canceled() != 0 {
        DownloadState::Cancelled
    } else if item.is_interrupted() != 0 {
        DownloadState::Interrupted
    } else if item.is_paused() != 0 {
        DownloadState::Paused
    } else {
        DownloadState::InProgress
    };
    if state == DownloadState::Interrupted {
        let reason = cef::sys::cef_download_interrupt_reason_t::from(item.interrupt_reason()) as i32;
        log_warn!("download {id} interrupted (reason {reason})");
    }
    let path = DOWNLOADS.with(|d| d.borrow().get(&id).and_then(|t| t.path.clone()));
    let file_name = path
        .as_deref()
        .and_then(|p| std::path::Path::new(p).file_name())
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| CefString::from(&item.suggested_file_name()).to_string());
    let total = item.total_bytes();
    let download = Download {
        id,
        tab: tabs::tab_for_browser(browser_id),
        url: CefString::from(&item.url()).to_string(),
        file_name,
        path,
        received_bytes: item.received_bytes(),
        total_bytes: (total > 0).then_some(total),
        bytes_per_sec: item.current_speed(),
        state,
        started_at,
    };
    if matches!(state, DownloadState::Complete | DownloadState::Cancelled) {
        DOWNLOADS.with(|d| {
            if let Some(t) = d.borrow_mut().get_mut(&id) {
                t.callback = None;
            }
        });
    }
    let in_progress = matches!(state, DownloadState::InProgress | DownloadState::Paused);
    let changed = DOWNLOADS.with(|d| d.borrow_mut().get_mut(&id).is_some_and(|t| std::mem::replace(&mut t.in_progress, in_progress) != in_progress));
    if changed && !in_progress {
        wake_idle_waiters();
    }
    if state == DownloadState::Complete {
        mark_downloaded_file(id, &download.url);
    }
    let blank_tab = download.tab.filter(|_| !committed(browser_id));
    controller::dispatch(Command::DownloadUpdated { download });
    // After `DownloadUpdated`, so core already keeps the browser alive for this download.
    if let Some(tab) = blank_tab
        && DOWNLOADS.with(|d| d.borrow_mut().get_mut(&id).is_some_and(|t| !std::mem::replace(&mut t.reported_blank, true)))
    {
        controller::dispatch(Command::DownloadInBlankTab { tab });
    }
}

/// `false` for a browser that has never committed a document — the shape a Peek (or a fresh tab)
/// has when the link it was opened for turned out to be a file: its main frame has no URL at all.
/// A page that starts a download of its own has committed and is never touched.
fn committed(browser_id: i32) -> bool {
    browsers::browser(browser_id).and_then(|b| b.main_frame()).is_some_and(|f| !CefString::from(&f.url()).to_string().is_empty())
}

/// Writes the Mark-of-the-Web of a completed download once (see the module docs).
fn mark_downloaded_file(id: u32, download_url: &str) {
    let Some((path, referrer)) = DOWNLOADS.with(|d| {
        let mut d = d.borrow_mut();
        let t = d.get_mut(&id).filter(|t| !t.marked)?;
        t.marked = true;
        Some((t.path.clone()?, t.referrer.clone()))
    }) else {
        return;
    };
    let content = zone_identifier(download_url, referrer.as_deref());
    let stream = format!("{path}:Zone.Identifier");
    match std::fs::write(&stream, content.as_bytes()) {
        Ok(()) => log_debug!("download {id}: Mark-of-the-Web written ({path})"),
        // FAT/exFAT volumes and some network shares have no alternate data streams.
        Err(e) => log_warn!("download {id}: cannot write the Mark-of-the-Web for {path}: {e}"),
    }
}

/// Longest URL written into a `Zone.Identifier` stream (`INTERNET_MAX_URL_LENGTH`).
const MAX_ZONE_URL_LEN: usize = 2083;

/// An http(s) URL without credentials, as written into `Zone.Identifier`; `None` for other
/// schemes (`data:`, `blob:`, `file:`, …), unparsable or overlong URLs.
fn zone_url(raw: &str) -> Option<String> {
    let mut url = url::Url::parse(raw.trim()).ok()?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none_or(str::is_empty) {
        return None;
    }
    let _ = url.set_username("");
    let _ = url.set_password(None);
    let spec = url.to_string();
    (spec.len() <= MAX_ZONE_URL_LEN).then_some(spec)
}

/// `Zone.Identifier` contents for a file downloaded from `download_url` on page `referrer`
/// (the format Windows Attachment Services and Chrome write).
fn zone_identifier(download_url: &str, referrer: Option<&str>) -> String {
    let mut out = String::from("[ZoneTransfer]\r\nZoneId=3\r\n");
    if let Some(r) = referrer.and_then(zone_url) {
        out.push_str(&format!("ReferrerUrl={r}\r\n"));
    }
    let host = zone_url(download_url).unwrap_or_else(|| "about:internet".into());
    out.push_str(&format!("HostUrl={host}\r\n"));
    out
}

/// `Effect::DownloadControl`.
pub fn control(id: u32, action: DownloadAction) {
    let (callback, path) = DOWNLOADS.with(|d| d.borrow().get(&id).map(|t| (t.callback.clone(), t.path.clone()))).unwrap_or_default();
    match action {
        DownloadAction::Pause | DownloadAction::Resume | DownloadAction::Cancel => {
            let Some(callback) = callback else {
                log_warn!("DownloadControl {action:?}({id}): no callback");
                return;
            };
            match action {
                DownloadAction::Pause => callback.pause(),
                DownloadAction::Resume => callback.resume(),
                _ => callback.cancel(),
            }
        }
        // Both can block on Explorer / the handler starting: never on the UI thread.
        DownloadAction::Open => {
            if let Some(p) = path {
                run_detached("sta-download-open", move || {
                    if !platform::shell_open(&p) {
                        log_warn!("download: no application opened {p}");
                    }
                });
            }
        }
        DownloadAction::ShowInFolder => {
            if let Some(p) = path {
                run_detached("sta-show-in-folder", move || {
                    platform::show_in_folder(&p);
                });
            }
        }
        // Core turns Retry into Effect::StartDownload; it is never emitted as DownloadControl.
        DownloadAction::Retry => log_debug!("DownloadControl Retry({id}) ignored (core emits StartDownload)"),
    }
}

fn run_detached(name: &str, f: impl FnOnce() + Send + 'static) {
    if let Err(e) = std::thread::Builder::new().name(name.into()).spawn(f) {
        log_error!("cannot start thread {name}: {e}");
    }
}

/// Drops stored callbacks (before `cef::shutdown()`).
pub fn clear() {
    let taken = DOWNLOADS.with(|d| std::mem::take(&mut *d.borrow_mut()));
    let waiters = IDLE_WAITERS.with(|w| std::mem::take(&mut *w.borrow_mut()));
    drop((taken, waiters));
}

#[cfg(test)]
mod tests {
    use super::{sanitize_file_name, zone_identifier};

    #[test]
    fn zone_identifier_contents() {
        assert_eq!(
            zone_identifier("https://files.example.com/a.exe", Some("https://example.com/page?q=1")),
            "[ZoneTransfer]\r\nZoneId=3\r\nReferrerUrl=https://example.com/page?q=1\r\nHostUrl=https://files.example.com/a.exe\r\n"
        );
        // Unknown referrer: omitted. Credentials are stripped.
        assert_eq!(
            zone_identifier("https://user:pw@files.example.com/a.exe", None),
            "[ZoneTransfer]\r\nZoneId=3\r\nHostUrl=https://files.example.com/a.exe\r\n"
        );
        // Local / opaque sources and referrers are never written out.
        for source in ["data:application/octet-stream;base64,AAAA", "blob:https://example.com/7c1f", "file:///C:/Users/me/secret.txt"] {
            assert_eq!(
                zone_identifier(source, Some("file:///C:/Users/me/page.html")),
                "[ZoneTransfer]\r\nZoneId=3\r\nHostUrl=about:internet\r\n",
                "{source}"
            );
        }
        // Control characters can't break the INI format.
        let text = zone_identifier("https://x.com/a\r\nZoneId=0", Some("https://x.com/\nHostUrl=evil"));
        assert_eq!(text.matches("\r\n").count(), 4, "{text:?}");
        let keys: Vec<&str> = text.split("\r\n").filter(|l| !l.is_empty()).map(|l| l.split('=').next().unwrap_or(l)).collect();
        assert_eq!(keys, ["[ZoneTransfer]", "ZoneId", "ReferrerUrl", "HostUrl"], "{text:?}");
        assert!(text.contains("ZoneId=3\r\n") && !text.contains("\nZoneId=0"), "{text:?}");
        let long = format!("https://x.com/{}", "a".repeat(3000));
        assert!(zone_identifier(&long, Some(&long)).ends_with("HostUrl=about:internet\r\n"));
    }

    #[test]
    fn file_names_are_safe() {
        assert_eq!(sanitize_file_name("report.pdf"), "report.pdf");
        assert_eq!(sanitize_file_name("..\\..\\evil.exe"), ".._.._evil.exe");
        assert_eq!(sanitize_file_name("a/b:c*?.txt. . "), "a_b_c__.txt");
        assert_eq!(sanitize_file_name("  ..  "), "download");
        assert_eq!(sanitize_file_name("CON"), "_CON");
        assert_eq!(sanitize_file_name("nul.txt"), "_nul.txt");
        assert_eq!(sanitize_file_name("com1.tar.gz"), "_com1.tar.gz");
        assert_eq!(sanitize_file_name("COM0.txt"), "COM0.txt");
        assert_eq!(sanitize_file_name("console.txt"), "console.txt");
    }
}
