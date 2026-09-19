//! The crash-loop guard [owner: chrome] (ext design FINAL PLAN §4 "Crash-loop guard"; R-SEC-11).
//!
//! An extension can crash the browser — `tabs.discard` on an Alloy tab does it today (user decision
//! D4a: accepted, reported upstream), and any extension with a bad service worker can make startup
//! fail. A browser that dies while restoring its tabs and then dies again restoring the same tabs is
//! unusable, and the user has no way in to turn the extension off.
//!
//! So sta writes a **launch marker** before it shows its window and clears it on a clean shutdown:
//! - the marker is there at startup → the previous run ended abnormally;
//! - it ended within [`CRASH_WINDOW_MS`] of its launch → it crashed *while starting*, which is what a
//!   loop looks like; the counter goes up (a later crash resets it: that is a normal crash, not a
//!   loop);
//! - two of those in a row → **safe mode**: the counter is cleared, core restores every tab unloaded
//!   (`Command::SafeModeStarted`) and Settings › Extensions shows a banner.
//!
//! The marker is one small JSON file next to the profile. A missing, unreadable or corrupt marker
//! means "first run": the guard never keeps sta from starting.
//!
//! Public API:
//! - `pub fn startup() -> bool` — write the marker; `true` = start in safe mode
//! - `pub fn on_clean_shutdown()` — the marker is cleared
//! - `pub fn debug_snapshot() -> serde_json::Value`

use serde_json::{Value, json};
use std::cell::Cell;
use std::path::PathBuf;

/// A run that ends within this long after launching counts as "crashed while starting".
const CRASH_WINDOW_MS: i64 = 60_000;
/// Startup crashes in a row before sta starts in safe mode.
const CRASHES_FOR_SAFE_MODE: u32 = 2;

thread_local! {
    static SAFE_MODE: Cell<bool> = const { Cell::new(false) };
    static CRASHES: Cell<u32> = const { Cell::new(0) };
    static CLEAN: Cell<bool> = const { Cell::new(false) };
}

fn marker_path() -> Option<PathBuf> {
    crate::paths::try_dirs().map(|d| d.user_data.join("launch-marker.json"))
}

/// Decides the new state from the marker of the previous run. Pure, so the rule is testable.
///
/// `previous`: `(launched_at, crashes)` of a marker that was still there, i.e. a run that did not
/// shut down cleanly. Returns `(safe_mode, crashes_to_record)`.
fn decide(previous: Option<(i64, u32)>, now: i64) -> (bool, u32) {
    let Some((launched_at, crashes)) = previous else { return (false, 0) };
    // A run that lived long enough was not a startup crash: whatever went wrong, restoring the
    // session is not what did it.
    if now.saturating_sub(launched_at) > CRASH_WINDOW_MS {
        return (false, 0);
    }
    let crashes = crashes.saturating_add(1);
    if crashes >= CRASHES_FOR_SAFE_MODE {
        // Safe mode now, and the count is cleared: the next launch starts normally again.
        (true, 0)
    } else {
        (false, crashes)
    }
}

/// Reads the previous run's marker, writes this run's, and answers whether to start in safe mode.
pub fn startup() -> bool {
    let Some(path) = marker_path() else { return false };
    let previous = std::fs::read_to_string(&path).ok().and_then(|text| {
        let value: Value = serde_json::from_str(&text).ok()?;
        Some((value.get("launchedAt")?.as_i64()?, value.get("crashes").and_then(Value::as_u64).unwrap_or(0) as u32))
    });
    let now = sta_core::now_ms();
    let (safe_mode, crashes) = decide(previous, now);
    SAFE_MODE.set(safe_mode);
    CRASHES.set(crashes);
    if safe_mode {
        log_warn!("safe mode: sta ended abnormally within {CRASH_WINDOW_MS} ms of launching twice in a row; tabs stay unloaded");
    } else if crashes > 0 {
        log_warn!("the previous run ended abnormally while starting ({crashes} in a row)");
    }
    let marker = json!({ "launchedAt": now, "crashes": crashes, "version": env!("CARGO_PKG_VERSION") });
    if let Err(e) = std::fs::write(&path, marker.to_string()) {
        log_warn!("could not write the launch marker: {e}");
    }
    safe_mode
}

/// A clean shutdown: the marker goes away, so the next launch sees no crash.
pub fn on_clean_shutdown() {
    if CLEAN.replace(true) {
        return;
    }
    let Some(path) = marker_path() else { return };
    match std::fs::remove_file(&path) {
        Ok(()) => log_debug!("launch marker cleared"),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => log_warn!("could not clear the launch marker: {e}"),
    }
}

#[cfg_attr(not(debug_assertions), allow(dead_code))] // debug.rs only
pub fn debug_snapshot() -> Value {
    json!({ "safeMode": SAFE_MODE.get(), "crashes": CRASHES.get(), "cleared": CLEAN.get() })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_startup_crashes_in_a_row_mean_safe_mode() {
        let now = 10_000_000;
        // First run ever: no marker.
        assert_eq!(decide(None, now), (false, 0));
        // A run that died 2 s after launching: one startup crash, not safe mode yet.
        assert_eq!(decide(Some((now - 2_000, 0)), now), (false, 1));
        // …and again: safe mode, with the count cleared so the next launch is normal.
        assert_eq!(decide(Some((now - 2_000, 1)), now), (true, 0));
        // A run that lived for an hour and then crashed is not a startup loop.
        assert_eq!(decide(Some((now - 3_600_000, 1)), now), (false, 0));
        // The window's edge: one millisecond past it the run "lived long enough", exactly on it the
        // run still counts as a startup crash.
        assert_eq!(decide(Some((now - CRASH_WINDOW_MS - 1, 1)), now), (false, 0));
        assert_eq!(decide(Some((now - CRASH_WINDOW_MS, 1)), now), (true, 0));
        // A marker from the future (a clock change) is not a crash loop by itself.
        assert_eq!(decide(Some((now + 5_000, 0)), now), (false, 1));
    }
}
