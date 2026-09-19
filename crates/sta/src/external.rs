//! External protocols (`mailto:`, `tel:`, `ms-settings:`, `zoommtg:`, …) [owner: shell].
//!
//! Chromium (Alloy) cannot load URLs of schemes it does not handle itself: without this module a
//! link to `mailto:` commits an error page. The shell instead hands such URLs to Windows
//! (`ShellExecuteW`, on a worker thread), like Chrome's external protocol handler, but only for
//! navigations with a **user gesture**:
//! - web tabs: main-frame `on_before_browse`, `on_before_popup` (`target=_blank` / `window.open`),
//!   `on_open_urlfrom_tab` (middle/Ctrl+click) and the link items of the tab context menu;
//! - UI pages (trusted): blocked navigations and popups;
//! - core's `Effect::OpenExternal` (a typed `mailto:` / `tel:` / app URL in the command bar,
//!   `OpenInput`, `OpenUrl`, `Navigate`): a user action, launched without creating a tab.
//!
//! Every navigation to an external scheme is cancelled, with or without a gesture (the current
//! page stays; no error page, no blank tab). Schemes known to be abused through their Windows
//! handlers (`ms-msdt:`, `search-ms:`, `ms-officecmd:`, …, plus Chromium's own blocklist) are never
//! launched, and neither are one-letter "schemes" (`c:/windows/…` is a drive path, which
//! `ShellExecuteW` would open or run). The URL is percent-escaped before launch so it cannot
//! inject arguments into the handler's command line.
//!
//! Debug builds only: `STA_TEST_EXTERNAL_PROTOCOL=1` logs `external protocol (test): <url>`
//! instead of launching anything (end-to-end tests).
//!
//! Public API:
//! - `pub fn is_external(url: &str) -> bool` — a scheme Chromium does not load itself
//! - `pub fn open(url: &str, user_gesture: bool, source: &str)` — launch (gesture) or drop, logged

use crate::platform;
use sta_core::urls::BROWSER_SCHEMES as INTERNAL_SCHEMES;
use std::sync::OnceLock;

/// External schemes that are never launched: Chromium's `kDeniedSchemes` plus Windows handlers
/// with a history of remote code execution or phishing abuse.
const DENIED_SCHEMES: &[&str] = &[
    "afp",
    "disk",
    "disks",
    "hcp",
    "ie.http",
    "its",
    "mk",
    "ms-appinstaller",
    "ms-cxh",
    "ms-cxh-full",
    "ms-help",
    "ms-its",
    "ms-itss",
    "ms-msdt",
    "ms-officecmd",
    "nntp",
    "res",
    "search",
    "search-ms",
    "shell",
    "vbscript",
    "vnd.ms.radio",
];

/// Longest URL handed to the OS.
const MAX_URL_LEN: usize = 8192;

fn scheme(url: &str) -> Option<String> {
    sta_core::urls::scheme(url.trim())
}

/// `true` for a syntactically valid URL whose scheme Chromium does not load itself.
pub fn is_external(url: &str) -> bool {
    scheme(url).is_some_and(|s| !INTERNAL_SCHEMES.contains(&s.as_str()))
}

/// The escaped URL to launch, or why it must not be launched.
fn launch_url(url: &str) -> Result<String, &'static str> {
    let url = url.trim();
    let Some(scheme) = scheme(url) else { return Err("not a URL") };
    if INTERNAL_SCHEMES.contains(&scheme.as_str()) {
        return Err("not an external scheme");
    }
    if DENIED_SCHEMES.contains(&scheme.as_str()) {
        return Err("denied scheme");
    }
    if scheme.len() < 2 {
        return Err("drive letter, not a scheme");
    }
    if url.len() > MAX_URL_LEN {
        return Err("too long");
    }
    Ok(escape(url))
}

/// Percent-escapes everything outside the RFC 3986 unreserved/reserved characters (and `%`), so
/// quotes, spaces, controls and non-ASCII text can't break out of `"%1"` in a handler command.
fn escape(url: &str) -> String {
    const KEEP: &[u8] = b"-._~:/?#[]@!$&'()*+,;=%";
    let mut out = String::with_capacity(url.len());
    for b in url.bytes() {
        if b.is_ascii_alphanumeric() || KEEP.contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// `STA_TEST_EXTERNAL_PROTOCOL=1` (debug builds only).
fn test_mode() -> bool {
    static TEST: OnceLock<bool> = OnceLock::new();
    *TEST.get_or_init(|| cfg!(debug_assertions) && std::env::var("STA_TEST_EXTERNAL_PROTOCOL").is_ok_and(|v| v == "1"))
}

/// Hands `url` to Windows if the navigation had a user gesture; otherwise (or for denied schemes)
/// only logs. Never blocks the calling (UI) thread.
pub fn open(url: &str, user_gesture: bool, source: &str) {
    if !user_gesture {
        log_warn!("external protocol without a user gesture ignored ({source}): {}", preview(url));
        return;
    }
    let target = match launch_url(url) {
        Ok(t) => t,
        Err(why) => {
            log_warn!("external protocol not launched ({source}, {why}): {}", preview(url));
            return;
        }
    };
    if test_mode() {
        log_info!("external protocol (test): {target}");
        return;
    }
    log_info!("external protocol ({source}): {}", preview(&target));
    let spawned = std::thread::Builder::new().name("sta-shell-open".into()).spawn(move || {
        if !platform::shell_open(&target) {
            log_warn!("no application opened {}", preview(&target));
        }
    });
    if let Err(e) = spawned {
        log_error!("cannot start the external protocol thread: {e}");
    }
}

fn preview(url: &str) -> String {
    let mut s: String = url.chars().take(200).collect();
    if s.len() < url.len() {
        s.push('…');
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_schemes() {
        for url in ["mailto:a@b.c", "tel:+123", "ms-settings:display", "zoommtg://zoom.us/join", "MAILTO:x", "ftp://host/"] {
            assert!(is_external(url), "{url}");
        }
        for url in [
            "https://example.com",
            "http://x",
            "file:///C:/",
            "about:blank",
            "data:text/html,x",
            "blob:https://a/b",
            "view-source:https://a",
            "sta://settings/",
            "chrome://version",
            "devtools://devtools/x",
            "javascript:alert(1)",
            "chrome-error://chromewebdata/",
            "",
            "no scheme here",
            "1abc:foo",
        ] {
            assert!(!is_external(url), "{url}");
        }
    }

    #[test]
    fn denies_dangerous_handlers() {
        assert_eq!(launch_url("ms-msdt:/id PCWDiagnostic"), Err("denied scheme"));
        assert_eq!(launch_url("search-ms:query=x&crumb=location:\\\\evil\\share"), Err("denied scheme"));
        assert_eq!(launch_url("https://example.com"), Err("not an external scheme"));
        assert_eq!(launch_url("c:/windows/system32/calc.exe"), Err("drive letter, not a scheme"));
        assert_eq!(launch_url("C:\\Windows\\notepad.exe"), Err("drive letter, not a scheme"));
        assert!(is_external("c:/windows/system32/calc.exe"), "still cancelled as a navigation");
        assert!(launch_url(&format!("mailto:{}", "a".repeat(MAX_URL_LEN))).is_err());
        assert_eq!(launch_url("mailto:someone@example.com?subject=Hi").as_deref(), Ok("mailto:someone@example.com?subject=Hi"));
    }

    #[test]
    fn escapes_command_line_breakers() {
        assert_eq!(escape("x-app:a\" --evil \"b"), "x-app:a%22%20--evil%20%22b");
        assert_eq!(escape("tel:+1 (555)"), "tel:+1%20(555)");
        assert_eq!(escape("mailto:ä@x"), "mailto:%C3%A4@x");
        assert_eq!(escape("x:\r\n^|<>`{}\\"), "x:%0D%0A%5E%7C%3C%3E%60%7B%7D%5C");
    }
}
