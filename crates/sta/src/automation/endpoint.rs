//! `<data>/sta/agent-endpoint.json` and `<data>/Logs/agent.log` [owner: automation].
//!
//! The endpoint file tells the bridge which pipe to open; it exists only while agent access is on.
//! The log records connections and tool calls (tool, tab, site, duration, result) and never typed
//! text, scripts, page content or URLs.

use crate::paths;
use sta_core::agent::channel::{ENDPOINT_FILE, Endpoint, PROTOCOL_VERSION};
use std::io::Write;
use std::path::PathBuf;

/// Log rotation size.
const LOG_MAX_BYTES: u64 = 5 * 1024 * 1024;

fn endpoint_path() -> Option<PathBuf> {
    paths::try_dirs().map(|d| d.profile.join(ENDPOINT_FILE))
}

pub fn write(pipe: &str) -> bool {
    let Some(path) = endpoint_path() else { return false };
    let endpoint = Endpoint { pipe: pipe.to_string(), pid: std::process::id(), protocol: PROTOCOL_VERSION, build: env!("CARGO_PKG_VERSION").to_string() };
    let Ok(json) = serde_json::to_string_pretty(&endpoint) else { return false };
    match sta_core::persist::write_atomic(&path, &json) {
        Ok(()) => true,
        Err(e) => {
            log_error!("agent: cannot write {}: {e}", path.display());
            false
        }
    }
}

pub fn remove() {
    if let Some(path) = endpoint_path()
        && path.exists()
        && let Err(e) = std::fs::remove_file(&path)
    {
        log_warn!("agent: cannot remove {}: {e}", path.display());
    }
}

/// Appends one line to `agent.log` (rotated to `agent.log.1` at 5 MB).
pub fn log(line: &str) {
    let Some(dir) = paths::try_dirs().map(|d| d.logs.clone()) else { return };
    let path = dir.join("agent.log");
    if std::fs::metadata(&path).is_ok_and(|m| m.len() > LOG_MAX_BYTES) {
        let _ = std::fs::rename(&path, dir.join("agent.log.1"));
    }
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let clean: String = line.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = writeln!(f, "[unix {secs}] {clean}");
    }
}

/// Reads `settings.agentAccess` from `state.json` before CEF initializes (the browser process adds
/// `--disable-backgrounding-occluded-windows` while agents may connect).
pub fn access_enabled_on_disk() -> bool {
    let Some(path) = paths::try_dirs().map(|d| d.profile.join(sta_core::persist::STATE_FILE)) else { return false };
    let Ok(text) = std::fs::read_to_string(path) else { return false };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else { return false };
    matches!(value.pointer("/settings/agentAccess").and_then(|v| v.as_str()), Some("readOnly" | "full"))
}
