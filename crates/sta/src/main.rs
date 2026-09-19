//! sta browser executable [skeleton, frozen] (ARCHITECTURE §3, docs/research/platform.md §1.5).
//!
//! The same exe runs the browser process and every CEF subprocess:
//! 1. `api_hash` first, in every process;
//! 2. `execute_process` with the shared `StaApp` (scheme + renderer handler in all processes);
//!    `>= 0` → this was a subprocess, exit with that code. Nothing with side effects before it;
//! 3. browser process: resolve data dirs (migrating folders from before the rename, or an error
//!    box and exit 1 while an old build holds them), panic hook + log file, `initialize` (exit
//!    quietly when the command line was forwarded to a running instance), `run_message_loop`,
//!    drop every CEF handle (`app::teardown`), `shutdown`.
//!
//! Module map and stage-2 ownership:
//! - skeleton/frozen: `browsers`
//! - tabs owner: `tabs`, `client`, `renderer`, `downloads`, `permissions`, `external`
//! - chrome owner: `window`, `overlays`, `sidebar_hover`, `keyboard`, `platform`, `controller`, `ipc`, `suggest`, `app`,
//!   `debug`, `scheme`, `paths`, `task`, `log`, `main`
//!
//! Cross-owner rule: call the other owner's `pub fn`s, never reach into its thread_locals. If an API
//! is missing, add it in your own module or ask the owner; `controller::execute` already routes
//! every `Effect` to one function per owner.
//!
//! End-to-end checks (debug build; each suite takes `E2E_DATA_DIR` and `CDP_PORT`):
//! `node crates/sta/e2e/shell-e2e.mjs` (layout, IPC, security, window, tab lifecycle),
//! `tabs-e2e.mjs` (popups, errors, downloads, permissions, boosts, …) and `chrome-e2e.mjs`
//! (overlays, real keyboard input, window, shutdown, restart); `migration-e2e.mjs` (data from before
//! the rename: `paths::resolve`); shared helpers in `e2e/lib.mjs`.
//!
//! The debug-only MCP test surface (`test_hooks`, docs/TESTING.md) lets those suites drive the
//! browser through MCP instead: `cargo build -p sta -p sta-mcp --features test-hooks` and
//! `--sta-test-hooks` + `STA_E2E=1` + an explicit `--sta-data-dir`.
//!
//! Debug-only environment switches (tests): `STA_REMOTE_DEBUGGING_PORT`,
//! `STA_DEBUG_FAIL_STARTUP=1` (fatal startup error path), `STA_DEBUG_SHUTDOWN_TIMEOUT_MS`
//! (shutdown timeout path), `STA_TEST_CONTEXT_MENU=<label>` (scripted context menus),
//! `STA_TEST_EXTERNAL_PROTOCOL=1` (log external protocol launches instead of running them),
//! `STA_SUGGEST_URL=<template with {q}>` (search suggestion endpoint for tests).

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

// The debug-only MCP test surface (docs/TESTING.md) must never reach a release binary. The module
// itself is behind `debug_assertions` as well, which would silently *drop* it here instead — so
// this guard sits where a release build sees it and fails loudly.
#[cfg(all(feature = "test-hooks", not(debug_assertions)))]
compile_error!("the `test-hooks` feature is debug-only: it must never be built into a release binary");

#[macro_use]
mod log;

mod app;
mod automation;
mod browsers;
mod client;
mod controller;
#[cfg(debug_assertions)]
mod debug;
// Until docked DevTools lands (phase 2), everything but `clear()` is reached from `debug.rs` only,
// so a release build sees the module as dead code.
#[cfg_attr(not(debug_assertions), allow(dead_code))]
mod devtools;
mod devtools_cdp;
mod devtools_policy;
mod devtools_shim;
mod disk;
mod downloads;
mod ext_backend;
mod ext_popup;
mod ext_shim;
mod extension_files;
mod extensions;
mod external;
mod foreign;
mod ipc;
mod keyboard;
mod motion;
mod overlays;
mod paths;
mod permissions;
mod platform;
mod renderer;
mod rounded;
mod safe_mode;
mod scheme;
mod sidebar_hover;
mod suggest;
mod tabs;
mod task;
/// The debug-only MCP test surface (docs/TESTING.md): never in a release build, and armed only by
/// `--sta-test-hooks` + `STA_E2E=1` + an explicit test data directory.
#[cfg(all(debug_assertions, feature = "test-hooks"))]
mod test_hooks;
mod translate;
mod unzip;
mod update;
mod window;

use cef::*;
use std::io::Write;
use std::path::PathBuf;

fn main() {
    // 0a) macOS: nothing works outside an app bundle, and libcef is not linked into the binary —
    // it is loaded from the framework inside that bundle (platform/mac_bundle.rs). Both come
    // before every CEF call, in every process.
    #[cfg(target_os = "macos")]
    {
        platform::mac_bundle::ensure_bundled();
        if let Err(e) = platform::mac_bundle::load_framework() {
            eprintln!("[sta] {e}");
            std::process::exit(1);
        }
    }
    // 0b) The apply helper: a staged build started to replace the installed one (update.rs). It
    // copies files and starts the new browser — no CEF, no profile, nothing else in this file.
    if update::apply_from_command_line() {
        return;
    }
    // 1) First CEF call in every process.
    let _ = api_hash(sys::CEF_API_VERSION_LAST, 0);
    let args = args::Args::new();

    // 2) Same App in every process. Subprocesses block here and exit.
    let mut app = app::create_app();
    let code = execute_process(Some(args.as_main_args()), Some(&mut app), std::ptr::null_mut());
    if code >= 0 {
        std::process::exit(code);
    }

    // 3) Browser process.
    // `NSApp` has to be sta's own NSApplication subclass before `initialize`: CEF checks that it
    // implements `CefAppProtocol` (platform/mac.rs). Subprocesses need no application object.
    #[cfg(target_os = "macos")]
    platform::init_application();
    if std::env::args().any(|a| a == "--console") {
        platform::attach_parent_console();
    }
    // Before any profile work: the test surface reads its two tokens and the data directory once,
    // and exits with code 2 rather than running a suite against a real profile un-armed.
    #[cfg(all(debug_assertions, feature = "test-hooks"))]
    test_hooks::arm_from_process();
    let (dirs, notes) = match paths::resolve() {
        Ok((d, notes)) => (paths::init(d), notes),
        Err(paths::ResolveError::LegacyInUse { legacy, new }) => {
            // Before CEF: nothing else runs yet, and no log file exists.
            let old = sta_core::legacy::PRODUCT_NAME;
            eprintln!("[sta] {} is in use by a running {old}; not starting", legacy.display());
            platform::show_error_box(
                "sta",
                &format!(
                    "sta can't start while {old} is running.\n\nsta moves {old}'s data from\n{}\nto\n{}\nwhen it starts, but {old} is still using it. Close {old}, then start sta again.",
                    legacy.display(),
                    new.display()
                ),
            );
            std::process::exit(1);
        }
        Err(paths::ResolveError::Io(e)) => {
            eprintln!("[sta] cannot create data directories: {e}");
            std::process::exit(1);
        }
    };
    install_panic_hook(dirs.logs.join("panic.log"));
    log::init(&dirs.logs.join("sta.log"));
    log_info!("sta {} starting (data dir {})", env!("CARGO_PKG_VERSION"), dirs.base.display());
    for note in notes {
        match note {
            paths::Note::Moved { from, to } => log_info!("data from before the rename moved: {} -> {}", from.display(), to.display()),
            paths::Note::UsingLegacyInPlace { path, error } => {
                log_warn!("could not move {} to its new name ({error}); using it in place for this run, retrying next launch", path.display())
            }
            paths::Note::SharingLegacyInPlace { path } => {
                log_info!("{} is used in place by a running sta; using it too (the command line goes to that instance)", path.display())
            }
            paths::Note::LegacyLeftAlone { path } => log_info!("{} from before the rename left alone (the new folder exists)", path.display()),
        }
    }

    let settings = app::settings(dirs);
    if initialize(Some(args.as_main_args()), Some(&settings), Some(&mut app), std::ptr::null_mut()) != 1 {
        let rc = get_exit_code();
        if rc == Resultcode::NORMAL_EXIT_PROCESS_NOTIFIED.get_raw() as i32 {
            log_info!("command line forwarded to the running instance");
            std::process::exit(0);
        }
        log_error!("CEF initialize failed (exit code {rc})");
        std::process::exit(if rc > 0 { rc } else { 1 });
    }
    drop(settings);

    run_message_loop();

    app::teardown();
    drop(app);
    shutdown();
    log_info!("exited cleanly");
}

/// Panics inside CEF callbacks abort the process; at least leave a trace on disk.
fn install_panic_hook(path: PathBuf) {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let backtrace = std::backtrace::Backtrace::force_capture();
        let secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
            let _ = writeln!(f, "[unix {secs}] {info}\n{backtrace}\n");
        }
        log::write("PANIC", format_args!("{info}"));
        default_hook(info);
    }));
}
