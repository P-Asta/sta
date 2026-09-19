//! Persistence helpers. Files live in the shell-provided profile directory:
//! - `state.json`   — [`crate::model::State`]
//! - `history.json` — [`crate::history::History`]
//!
//! Loading is tolerant: missing or corrupt files yield defaults (a corrupt file is renamed to
//! `*.corrupt-<ms>.json` so it isn't overwritten). Saving is atomic (write `*.tmp`, fsync, rename).

use std::io;
use std::path::Path;

pub const STATE_FILE: &str = "state.json";
pub const HISTORY_FILE: &str = "history.json";

/// Read a UTF-8 file; `Ok(None)` if it doesn't exist.
pub fn read_optional(path: &Path) -> io::Result<Option<String>> {
    match std::fs::read_to_string(path) {
        Ok(s) => Ok(Some(s)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// Atomically replace `path` with `contents`.
pub fn write_atomic(path: &Path, contents: &str) -> io::Result<()> {
    use std::io::Write;
    let tmp = path.with_extension("json.tmp");
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(contents.as_bytes())?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)
}

/// Move an unreadable file aside so defaults can be written without losing it.
pub fn quarantine(path: &Path, now_ms: i64) {
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("file");
    let target = path.with_file_name(format!("{stem}.corrupt-{now_ms}.json"));
    let _ = std::fs::rename(path, target);
}
