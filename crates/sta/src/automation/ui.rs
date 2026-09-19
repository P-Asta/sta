//! What the user sees of agents, shell side [owner: automation] (docs/MCP.md "Approval",
//! "Seeing what agents do"; PROTOCOL §9).
//!
//! - The agent overlay (`sta://agent/`, `overlays::Overlay::Agent`): core's
//!   `ShowAgentOverlay{prompt}` / `HideAgentOverlay`. An approval prompt takes keyboard focus like
//!   the permission prompt, **except while the user is typing** (a key in the last 2 s): then it
//!   appears without focus, so keys meant for a page can't land on it (the page also keeps its
//!   buttons inert for 1 s after it appears and after every key). A connection prompt flashes the
//!   taskbar button while the window isn't in the foreground.
//! - IPC requests of the Settings page (403 from anywhere else, the agent overlay included only for
//!   `agent.info`):
//!   - `agent.info` → `{bridgePath, bridgeFound, dataDir, dataDirIsDefault, endpointOpen, logPath, build}`;
//!   - `agent.testConnection` → `{ok, steps: [{id, ok, detail}]}`: access on, endpoint open, bridge
//!     found, and `sta-mcp.exe --check` (reads the endpoint file, opens the pipe with the
//!     owner and session checks, closes it without a hello: no prompt). One test at a time (409).

use super::{endpoint, session};
use crate::ipc::Reply;
use crate::{controller, overlays, paths, task, window};
use sta_core::{AgentAccess, Id};
use cef::wrapper::message_router::BrowserSideCallback;
use serde_json::{Value, json};
use std::cell::Cell;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

type Cb = Arc<Mutex<dyn BrowserSideCallback>>;

/// A key this recent means the user is typing: an approval prompt doesn't take focus.
const TYPING_WINDOW: Duration = Duration::from_secs(2);
/// `sta-mcp --check` gets this long.
const CHECK_TIMEOUT: Duration = Duration::from_secs(10);
/// How long after `tab_show` restoring keyboard focus keeps out of the shown tab.
const AGENT_SHOW_FOCUS_WINDOW: Duration = Duration::from_secs(2);
/// An approval answer this soon after a key in the prompt's page came from the keyboard.
const KEY_ANSWER_WINDOW: Duration = Duration::from_millis(500);

thread_local! {
    static SHOWING_PROMPT: Cell<bool> = const { Cell::new(false) };
    static LAST_KEY: Cell<Option<Instant>> = const { Cell::new(None) };
    static TESTING: Cell<bool> = const { Cell::new(false) };
    static SHOWN: Cell<u64> = const { Cell::new(0) };
    static SHOWN_WITHOUT_FOCUS: Cell<u64> = const { Cell::new(0) };
    static FLASHES: Cell<u64> = const { Cell::new(0) };
    /// The tab `tab_show` just showed, and when.
    static AGENT_SHOWN: Cell<Option<(Id, Instant)>> = const { Cell::new(None) };
    /// The browser that saw the last real key, and when (for the approval log line).
    static LAST_KEY_IN: Cell<Option<(i32, Instant)>> = const { Cell::new(None) };
}

// ----------------------------------------------------------------------------------- overlay

/// Any key a browser saw (`KeyboardHandler::on_pre_key_event`).
pub fn note_user_key() {
    LAST_KEY.set(Some(Instant::now()));
}

/// A real key (with a native message) in `browser_id`.
pub fn note_native_key(browser_id: i32) {
    LAST_KEY_IN.set(Some((browser_id, Instant::now())));
}

/// How an approval answer from `browser_id` was most likely given, for the log: `keyboard` when a
/// real key reached that page just before, else `pointer` (a click, or a DevTools click in tests).
pub fn answer_input(browser_id: i32) -> &'static str {
    match LAST_KEY_IN.get() {
        Some((b, at)) if b == browser_id && at.elapsed() < KEY_ANSWER_WINDOW => "keyboard",
        _ => "pointer",
    }
}

/// `tab_show` is about to show `tab`. Showing a tab for an agent never moves keyboard focus into
/// it: when the layout change hides the tab that had focus, the focus rescue
/// (`overlays::restore_main_focus`) must not pick the shown tab for a moment.
pub fn note_agent_show(tab: Id) {
    AGENT_SHOWN.set(Some((tab, Instant::now())));
}

/// Whether restoring keyboard focus may move it into `tab` (overlays.rs).
pub fn may_restore_focus_to(tab: Id) -> bool {
    !matches!(AGENT_SHOWN.get(), Some((t, at)) if t == tab && at.elapsed() < AGENT_SHOW_FOCUS_WINDOW)
}

fn user_typing() -> bool {
    LAST_KEY.get().is_some_and(|t| t.elapsed() < TYPING_WINDOW)
}

/// Whether the agent overlay requests keyboard focus now (overlays.rs).
pub fn overlay_may_take_focus() -> bool {
    let take = !SHOWING_PROMPT.get() || !user_typing();
    if !take {
        SHOWN_WITHOUT_FOCUS.set(SHOWN_WITHOUT_FOCUS.get() + 1);
    }
    take
}

/// `Effect::ShowAgentOverlay`.
pub fn show_overlay(prompt: bool) {
    SHOWING_PROMPT.set(prompt);
    SHOWN.set(SHOWN.get() + 1);
    if prompt {
        let connection = controller::with_store(|s| s.agent_prompts().first().is_some_and(|p| matches!(p.kind, sta_core::agent::AgentPromptKind::Connection { .. })))
            .unwrap_or(false);
        if connection {
            flash_taskbar();
        }
    }
    overlays::show_agent_overlay();
}

/// `Effect::HideAgentOverlay`.
pub fn hide_overlay() {
    SHOWING_PROMPT.set(false);
    overlays::hide_agent_overlay();
}

/// Flashes the taskbar button until the window comes to the foreground (only when it isn't).
#[cfg(windows)]
fn flash_taskbar() {
    use windows_sys::Win32::UI::WindowsAndMessaging::{FLASHW_TIMERNOFG, FLASHW_TRAY, FLASHWINFO, FlashWindowEx, GetForegroundWindow};
    let hwnd = window::hwnd_value();
    if hwnd == 0 {
        return;
    }
    // SAFETY: plain Win32 calls with our own top-level window handle.
    unsafe {
        if GetForegroundWindow() as isize == hwnd {
            return;
        }
        let info = FLASHWINFO { cbSize: std::mem::size_of::<FLASHWINFO>() as u32, hwnd: hwnd as _, dwFlags: FLASHW_TRAY | FLASHW_TIMERNOFG, uCount: 0, dwTimeout: 0 };
        FlashWindowEx(&info);
    }
    FLASHES.set(FLASHES.get() + 1);
}

#[cfg(not(windows))]
fn flash_taskbar() {}

// ----------------------------------------------------------------------------------- IPC

fn reply(cb: &Cb, value: Value) {
    if let Ok(cb) = cb.lock() {
        cb.success_str(&value.to_string());
    }
}

/// `agent.*` requests (ipc.rs).
pub fn handle_request(browser_id: i32, cmd: &str, _payload: Value, callback: &Cb) -> Reply {
    let settings = super::is_settings_page(browser_id);
    match cmd {
        "agent.info" if settings || super::is_agent_surface(browser_id) => Reply::Json(info().to_string()),
        "agent.testConnection" if settings => {
            let cb = callback.clone();
            task::post_ui(move || test_connection(cb));
            Reply::Deferred
        }
        "agent.info" | "agent.testConnection" => Reply::Err(403, format!("{cmd} is only allowed from the Settings page")),
        _ => Reply::Err(404, format!("unknown request: {cmd}")),
    }
}

fn bridge_path() -> Option<PathBuf> {
    std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.join("sta-mcp.exe")))
}

/// The data directory the bridge uses without `--data-dir` (same rule as `sta-mcp`).
fn bridge_default_data_dir() -> Option<PathBuf> {
    let local = std::env::var_os("LOCALAPPDATA")?;
    Some(PathBuf::from(local).join(if cfg!(debug_assertions) { "sta Dev" } else { "sta" }))
}

fn same_path(a: &std::path::Path, b: &std::path::Path) -> bool {
    let norm = |p: &std::path::Path| p.to_string_lossy().trim_end_matches(['\\', '/']).replace('/', "\\").to_lowercase();
    norm(a) == norm(b)
}

/// The legacy default data folder (from before the rename) that sta may run in place when it
/// couldn't move it; the bridge's default data directory finds that run too.
fn legacy_default_data_dir() -> Option<PathBuf> {
    let local = std::env::var_os("LOCALAPPDATA")?;
    Some(PathBuf::from(local).join(sta_core::legacy::data_dir_name(cfg!(debug_assertions))))
}

fn info() -> Value {
    let bridge = bridge_path();
    let data_dir = paths::try_dirs().map(|d| d.base.clone());
    // A legacy default folder used in place counts as the default: the setup snippets must not
    // name a folder the next start moves away.
    let is_default = data_dir.as_ref().is_some_and(|d| [bridge_default_data_dir(), legacy_default_data_dir()].iter().flatten().any(|def| same_path(d, def)));
    json!({
        "bridgePath": bridge.as_ref().map(|p| p.to_string_lossy().into_owned()),
        "bridgeFound": bridge.as_ref().is_some_and(|p| p.is_file()),
        "dataDir": data_dir.as_ref().map(|p| p.to_string_lossy().into_owned()),
        "dataDirIsDefault": is_default,
        "endpointOpen": session::endpoint_open(),
        "logPath": paths::try_dirs().map(|d| d.logs.join("agent.log").to_string_lossy().into_owned()),
        "build": env!("CARGO_PKG_VERSION"),
    })
}

fn step(id: &str, ok: bool, detail: impl Into<String>) -> Value {
    json!({ "id": id, "ok": ok, "detail": detail.into() })
}

/// `agent.testConnection` (UI task): the checks that don't need the bridge, then the bridge's own
/// `--check` on a worker thread.
fn test_connection(cb: Cb) {
    if TESTING.replace(true) {
        if let Ok(cb) = cb.lock() {
            cb.failure(409, "a connection test is already running");
        }
        return;
    }
    let started = Instant::now();
    let access = controller::with_store(|s| s.settings().agent_access).unwrap_or_default();
    let mut steps = vec![match access {
        AgentAccess::Off => step("access", false, "AI agent access is off. Choose Read only or Full access above."),
        AgentAccess::ReadOnly => step("access", true, "Read only access is on"),
        AgentAccess::Full => step("access", true, "Full access is on"),
    }];
    let open = session::endpoint_open();
    steps.push(if open {
        step("endpoint", true, "sta is listening for agents on a private pipe")
    } else {
        step("endpoint", false, "sta isn't listening for agents (turn access on)")
    });
    let bridge = bridge_path().filter(|p| p.is_file());
    steps.push(match &bridge {
        Some(p) => step("bridge", true, p.to_string_lossy()),
        None => step("bridge", false, "sta-mcp.exe was not found next to sta.exe"),
    });
    let (Some(bridge), true, Some(data_dir)) = (bridge, open, paths::try_dirs().map(|d| d.base.clone())) else {
        finish_test(&cb, steps, started);
        return;
    };
    let spawned = std::thread::Builder::new().name("sta-agent-check".into()).spawn(move || {
        let result = run_check(&bridge, &data_dir);
        task::post_ui_from_any_thread(move || {
            steps.push(match result {
                Ok(detail) => step("channel", true, detail),
                Err(detail) => step("channel", false, detail),
            });
            finish_test(&cb, steps, started);
        });
    });
    if let Err(e) = spawned {
        TESTING.set(false);
        log_error!("agent: cannot start the connection test thread: {e}");
    }
}

fn finish_test(cb: &Cb, steps: Vec<Value>, started: Instant) {
    TESTING.set(false);
    let ok = steps.iter().all(|s| s["ok"] == json!(true));
    endpoint::log(&format!("connection test from Settings: {}", if ok { "ok" } else { "failed" }));
    reply(cb, json!({ "ok": ok, "steps": steps, "ms": started.elapsed().as_millis() as u64 }));
}

/// Runs `sta-mcp.exe --check --data-dir <dir>` (no console window, 10 s at most) and reads its
/// one-line JSON answer.
fn run_check(bridge: &std::path::Path, data_dir: &std::path::Path) -> Result<String, String> {
    use std::io::Read;
    use std::process::{Command, Stdio};
    let mut command = Command::new(bridge);
    command.arg("--check").arg("--data-dir").arg(data_dir).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = command.spawn().map_err(|e| format!("Couldn't start sta-mcp.exe ({e})"))?;
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() < CHECK_TIMEOUT => std::thread::sleep(Duration::from_millis(50)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("sta-mcp.exe didn't answer within 10 s".into());
            }
            Err(e) => return Err(format!("sta-mcp.exe failed ({e})")),
        }
    };
    let mut out = String::new();
    if let Some(stdout) = child.stdout.take() {
        let _ = stdout.take(64 * 1024).read_to_string(&mut out);
    }
    let answer: Value = out.lines().rev().find_map(|l| serde_json::from_str(l).ok()).unwrap_or(Value::Null);
    let message = answer["message"].as_str().unwrap_or_default().to_string();
    match (answer["ok"].as_bool(), status.success()) {
        (Some(true), true) => Ok(if message.is_empty() { "The MCP server reached sta".into() } else { message }),
        (Some(false), _) => Err(format!("{} ({})", message, answer["code"].as_str().unwrap_or("error"))),
        _ => Err(format!("sta-mcp.exe exited with {status} without an answer")),
    }
}

pub fn clear() {
    SHOWING_PROMPT.set(false);
}

pub fn debug_snapshot() -> Value {
    json!({
        "showingPrompt": SHOWING_PROMPT.get(),
        "userTyping": user_typing(),
        "overlayShown": SHOWN.get(),
        "shownWithoutFocus": SHOWN_WITHOUT_FOCUS.get(),
        "taskbarFlashes": FLASHES.get(),
        "overlayVisible": overlays::is_visible(overlays::Overlay::Agent),
    })
}
