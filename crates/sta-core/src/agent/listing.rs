//! `history_search` and `downloads_list` output: which entries agents may see and how they read.
//! Pure; the shell passes store data.

use super::policy::{self, UrlVerdict};
use super::text::quote;
use crate::history::HistoryUrl;
use crate::model::{Download, DownloadState, Settings};
use crate::Millis;

/// Default and largest `limit` of both tools.
pub const DEFAULT_LIMIT: usize = 20;
pub const MAX_LIMIT: usize = 100;

/// `limit` clamped to `1..=MAX_LIMIT`.
pub fn limit(requested: Option<u64>) -> usize {
    requested.map_or(DEFAULT_LIMIT, |l| (l.min(MAX_LIMIT as u64) as usize).max(1))
}

/// A history entry agents may see: an `http(s)` page they could open under the current policy
/// (not a sta page, a file, a blocked host or — unless allowed — a private-network host).
pub fn history_visible(settings: &Settings, url: &str) -> bool {
    matches!(policy::check_url(settings, url), Ok(UrlVerdict::Web { .. }))
}

/// `2026-09-17 08:05 UTC`.
pub fn format_utc(ms: Millis) -> String {
    let secs = ms.div_euclid(1000);
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02} {:02}:{:02} UTC", rem / 3600, (rem % 3600) / 60)
}

/// `just now`, `5 min ago`, `3 h ago`, `2 days ago`.
pub fn ago(ms: Millis, now: Millis) -> String {
    let s = ((now - ms) / 1000).max(0);
    match s {
        0..=59 => "just now".into(),
        60..=3599 => format!("{} min ago", s / 60),
        3600..=86_399 => format!("{} h ago", s / 3600),
        _ => {
            let d = s / 86_400;
            format!("{d} day{} ago", if d == 1 { "" } else { "s" })
        }
    }
}

/// `1.5 MB`, `820 KB`, `12 B`.
pub fn size(bytes: i64) -> String {
    let b = bytes.max(0) as f64;
    if b >= 1024.0 * 1024.0 * 1024.0 {
        format!("{:.1} GB", b / (1024.0 * 1024.0 * 1024.0))
    } else if b >= 1024.0 * 1024.0 {
        format!("{:.1} MB", b / (1024.0 * 1024.0))
    } else if b >= 1024.0 {
        format!("{:.0} KB", b / 1024.0)
    } else {
        format!("{bytes} B")
    }
}

/// One `history_search` line (page strings quoted; the caller frames them as untrusted).
pub fn history_line(entry: &HistoryUrl, now: Millis) -> String {
    format!(
        "- {} {} (last visit {}, {}; {} visit{})",
        quote(&entry.title, 160),
        quote(&entry.url, 400),
        format_utc(entry.last_visit_at),
        ago(entry.last_visit_at, now),
        entry.visit_count,
        if entry.visit_count == 1 { "" } else { "s" }
    )
}

/// State of a download as agents see it.
pub fn download_state(d: &Download, held: bool) -> String {
    if held {
        return "waiting for the user to keep or discard it".into();
    }
    match d.state {
        DownloadState::InProgress | DownloadState::Paused => {
            let verb = if d.state == DownloadState::Paused { "paused" } else { "in progress" };
            match d.total_bytes.filter(|t| *t > 0) {
                Some(total) => format!("{verb}, {}% of {}", (d.received_bytes.max(0) * 100 / total).min(100), size(total)),
                None => format!("{verb}, {} so far", size(d.received_bytes)),
            }
        }
        DownloadState::Complete => format!("complete, {}", size(d.total_bytes.unwrap_or(d.received_bytes))),
        DownloadState::Cancelled => "cancelled".into(),
        DownloadState::Interrupted => "interrupted".into(),
    }
}

/// One `downloads_list` line: file name and state only (never the folder, path or URL).
pub fn download_line(d: &Download, held: bool, now: Millis) -> String {
    format!("- download {}: {} {} (started {})", d.id, quote(&d.file_name, 200), download_state(d, held), ago(d.started_at, now))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_and_ages() {
        assert_eq!(format_utc(0), "1970-01-01 00:00 UTC");
        assert_eq!(format_utc(951_782_400_000), "2000-02-29 00:00 UTC");
        assert_eq!(format_utc(1_789_634_700_000), "2026-09-17 08:45 UTC");
        assert_eq!(ago(1000, 1000), "just now");
        assert_eq!(ago(0, 5 * 60_000), "5 min ago");
        assert_eq!(ago(0, 3 * 3_600_000), "3 h ago");
        assert_eq!(ago(0, 86_400_000), "1 day ago");
        assert_eq!(ago(0, 3 * 86_400_000), "3 days ago");
        assert_eq!(limit(None), DEFAULT_LIMIT);
        assert_eq!(limit(Some(0)), 1);
        assert_eq!(limit(Some(10_000)), MAX_LIMIT);
    }

    #[test]
    fn history_visibility_follows_policy() {
        let mut s = Settings { agent_blocked_hosts: vec!["bank.example".into()], ..Default::default() };
        assert!(history_visible(&s, "https://docs.rs/regex"));
        assert!(history_visible(&s, "http://localhost:3000/"));
        assert!(!history_visible(&s, "https://www.bank.example/login"));
        assert!(!history_visible(&s, "file:///C:/Users/me/notes.html"));
        assert!(!history_visible(&s, "sta://settings/"));
        assert!(!history_visible(&s, "http://192.168.0.1/"));
        s.agent_allow_private_network = true;
        assert!(history_visible(&s, "http://192.168.0.1/"));
        let e = HistoryUrl { url: "https://docs.rs/regex".into(), title: "regex - Rust".into(), visit_count: 3, last_visit_at: 1_789_634_700_000, ..Default::default() };
        assert_eq!(history_line(&e, 1_789_634_700_000 + 7_200_000), "- \"regex - Rust\" \"https://docs.rs/regex\" (last visit 2026-09-17 08:45 UTC, 2 h ago; 3 visits)");
    }

    #[test]
    fn download_lines_have_no_paths() {
        let d = Download {
            id: 7,
            tab: Some(3),
            url: "https://example.com/files/report.pdf?token=secret".into(),
            file_name: "report.pdf".into(),
            path: Some(r"C:\Users\me\Downloads\report.pdf".into()),
            received_bytes: 512 * 1024,
            total_bytes: Some(1024 * 1024),
            bytes_per_sec: 0,
            state: DownloadState::InProgress,
            started_at: 0,
        };
        let line = download_line(&d, false, 60_000);
        assert_eq!(line, "- download 7: \"report.pdf\" in progress, 50% of 1.0 MB (started 1 min ago)");
        assert!(!line.contains("Users") && !line.contains("token"));
        assert_eq!(download_state(&d, true), "waiting for the user to keep or discard it");
        let done = Download { state: DownloadState::Complete, received_bytes: 2048, total_bytes: None, ..d.clone() };
        assert_eq!(download_state(&done, false), "complete, 2 KB");
        assert_eq!(download_state(&Download { state: DownloadState::Interrupted, ..d }, false), "interrupted");
    }
}
