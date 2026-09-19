//! Rust-side logging [skeleton, frozen].
//!
//! Responsibility: tiny, dependency-free logger used by every module through the `log_info!`,
//! `log_warn!`, `log_error!` and `log_debug!` macros (declared first in `main.rs` with
//! `#[macro_use]`, so they are in scope everywhere without imports).
//!
//! - Lines go to stderr (visible in debug builds, which use the console subsystem) and, in the
//!   browser process once [`init`] ran, are appended to `<data>/Logs/sta.log`.
//! - Subprocesses (renderer, GPU, ...) never call [`init`], so they only write to stderr.
//! - `log_debug!` compiles to nothing in release builds.
//!
//! Public API:
//! - `pub fn init(path: &Path)`
//! - `pub fn write(level: &str, args: std::fmt::Arguments)` (used by the macros)

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

static FILE: Mutex<Option<File>> = Mutex::new(None);
static START: OnceLock<Instant> = OnceLock::new();

/// Opens (append) the browser-process log file. Safe to call once; later calls replace the file.
pub fn init(path: &Path) {
    START.get_or_init(Instant::now);
    match OpenOptions::new().create(true).append(true).open(path) {
        Ok(f) => {
            if let Ok(mut guard) = FILE.lock() {
                *guard = Some(f);
            }
        }
        Err(e) => eprintln!("[sta] cannot open log file {}: {e}", path.display()),
    }
}

/// Writes one log line. Never panics.
pub fn write(level: &str, args: std::fmt::Arguments) {
    let t = START.get_or_init(Instant::now).elapsed();
    let line = format!("[{:>8.3}] {level:<5} {args}\n", t.as_secs_f64());
    let _ = std::io::stderr().write_all(line.as_bytes());
    if let Ok(mut guard) = FILE.lock()
        && let Some(f) = guard.as_mut()
    {
        let _ = f.write_all(line.as_bytes());
    }
}

macro_rules! log_info {
    ($($t:tt)*) => { $crate::log::write("INFO", format_args!($($t)*)) };
}

macro_rules! log_warn {
    ($($t:tt)*) => { $crate::log::write("WARN", format_args!($($t)*)) };
}

macro_rules! log_error {
    ($($t:tt)*) => { $crate::log::write("ERROR", format_args!($($t)*)) };
}

/// Debug-build-only logging (arguments are still type-checked in release builds).
macro_rules! log_debug {
    ($($t:tt)*) => {
        if cfg!(debug_assertions) {
            $crate::log::write("DEBUG", format_args!($($t)*))
        }
    };
}
