//! Browser automation for AI agents over MCP [owner: automation] (docs/MCP.md, ARCHITECTURE §5.2,
//! docs/research/automation.md).
//!
//! ```text
//! MCP client ─stdio─► sta-mcp ─NDJSON over a pipe (Windows) / socket (Unix)─► pipe.rs (threads)
//!                                                                                      └► session.rs (UI thread)
//!                                                                                          ├ policy + approval (core)
//!                                                                                          └ tools.rs ─► page.rs ─► cdp.rs (in-process DevTools, allowlist)
//! ```
//!
//! Modules: `exec` (UI-thread futures), `cdp` (DevTools client + method allowlist), `page` (frames,
//! isolated world, refs, page functions), `tools` (the MCP tools), `session` (connections, approval,
//! calls, rate limits), `tools_input` / `tools_page` / `tools_browser` (the tools beyond the MVP set:
//! hover, select_option, scroll, fill_form / page_find, evaluate, console_messages /
//! request_tab_access, history_search, downloads_list), `console` (console messages of web tabs
//! while access is on), `pipe` (the channel server: a named pipe in `pipe.rs` on Windows, a Unix
//! domain socket in `socket.rs` elsewhere), `win` / `unix` (the OS half of the channel: CSPRNG,
//! client identity, and on Windows the SID/SDDL the pipe is created with),
//! `guards` (agent-controlled tabs in the shell's handlers), `endpoint` (endpoint file, agent.log),
//! `ui` (the agent overlay, typing guard, taskbar flash, Settings' `agent.*` requests), `frame` (the
//! agent-colored frame around agent-controlled tabs), `spike` (debug-only `debug.cdp`, `debug.tabKey`).
//!
//! Public API (for the rest of the shell):
//! - effects: `set_endpoint(enabled)`, `answer(id, allow)`, `disconnect_all()`, `resolve_download(id, keep)`
//! - hooks: `on_pre_key_event(browser_id, native)`, `on_load_start(browser_id)`,
//!   `on_console_message(browser_id, level, message, source, line)`,
//!   `on_load_error(browser_id, code, text)`, `on_browser_closed(browser_id)`,
//!   `on_agent_detached(browser_id)` (from `cdp`), `guards::*` (handlers), `may_answer_prompts`,
//!   `occlusion_flag_needed()` (before `cef::initialize`)
//! - UI: `ui::show_overlay(prompt)` / `ui::hide_overlay()` (effects), `ui::handle_request` (ipc.rs
//!   `agent.*`), `ui::overlay_may_take_focus()` (overlays.rs), `frame::wrapper_color(tab, base)` (tabs.rs)
//! - `clear()` — teardown, before `cef::shutdown`; `debug_snapshot()`

pub mod cdp;
pub mod console;
pub mod endpoint;
pub mod exec;
pub mod frame;
pub mod guards;
pub mod page;
/// The agent channel's transport. Both files serve the same module API (`endpoint_name`, `start`,
/// `send`, `close`, `close_all`, `connection_count`, `PipeEvent`, `ClientIdentity`).
#[cfg(windows)]
#[path = "pipe.rs"]
pub mod pipe;
#[cfg(unix)]
#[path = "socket.rs"]
pub mod pipe;
pub mod session;
#[cfg(debug_assertions)]
pub mod spike;
pub mod tools;
pub mod tools_browser;
pub mod tools_input;
pub mod tools_page;
pub mod ui;
#[cfg(unix)]
pub mod unix;
#[cfg(windows)]
pub mod win;

/// `2n` hex digits from the OS CSPRNG (channel names, ref ids).
pub fn random_hex(n: usize) -> Option<String> {
    #[cfg(windows)]
    {
        win::random_hex(n)
    }
    #[cfg(unix)]
    {
        unix::random_hex(n)
    }
}

use crate::browsers::{self, Role};
use sta_core::Command;

/// `Effect::AgentEndpoint`.
pub fn set_endpoint(enabled: bool) {
    session::set_endpoint(enabled);
}

/// `Effect::AgentAnswer`.
pub fn answer(id: u64, allow: bool) {
    session::answer(id, allow);
}

/// `Effect::AgentDisconnect` (Stop).
pub fn disconnect_all() {
    session::disconnect_all();
}

/// `Effect::AgentDownload`.
pub fn resolve_download(id: u32, keep: bool) {
    guards::resolve_download(id, keep);
}

/// The DevTools agent of a browser detached: its isolated worlds (and refs of that generation) are
/// stale.
pub fn on_agent_detached(browser_id: i32) {
    page::on_agent_detached(browser_id);
}

/// A tab browser is gone (`on_before_close`).
pub fn on_browser_closed(browser_id: i32) {
    guards::on_browser_closed(browser_id);
    page::on_browser_closed(browser_id);
    cdp::on_browser_closed(browser_id);
}

/// `DisplayHandler::on_console_message` of a tab browser (`console_messages`).
pub fn on_console_message(browser_id: i32, level: cef::LogSeverity, message: &str, source: &str, line: i32) {
    console::on_message(browser_id, level, message, source, line);
}

/// Main-frame load start of a web tab.
pub fn on_load_start(browser_id: i32) {
    guards::on_load_start(browser_id);
}

/// Main-frame load error of a tab.
pub fn on_load_error(browser_id: i32, code: i32, text: &str) {
    guards::on_load_error(browser_id, code, text);
}

thread_local! {
    static KEY_EVENTS: std::cell::Cell<(u64, u64)> = const { std::cell::Cell::new((0, 0)) };
}

/// Every key event a browser sees before the page (`KeyboardHandler::on_pre_key_event`). DevTools
/// key events never get here, so in a tab this is the user's own typing: the tab goes back to the
/// user (see `guards`). `native` = it carries an OS message.
pub fn on_pre_key_event(browser_id: i32, native: bool) {
    let (n, s) = KEY_EVENTS.get();
    KEY_EVENTS.set(if native { (n + 1, s) } else { (n, s + 1) });
    ui::note_user_key();
    if native {
        ui::note_native_key(browser_id);
    }
    if matches!(browsers::role_of(browser_id), Some(Role::Tab(_))) {
        guards::on_user_key(browser_id);
    }
}

/// Approval answers are accepted only from the agent overlay (`sta://agent/`) and the Settings
/// page: a compromised other surface can't approve an agent. Checks commands wrapped in
/// `commitOmnibox` too.
pub fn may_answer_prompts(browser_id: i32, command: &Command) -> bool {
    let answers = |c: &Command| matches!(c, Command::AnswerAgentConnection { .. } | Command::AnswerSitePermission { .. } | Command::AnswerTabAccess { .. });
    let wrapped = match command {
        Command::CommitOmnibox { command, .. } => answers(command),
        other => answers(other),
    };
    if !wrapped {
        return true;
    }
    let allowed = is_agent_surface(browser_id) || is_settings_page(browser_id);
    log_info!("agent: approval answer from browser {browser_id} ({:?}, {}): {}", browsers::role_of(browser_id), ui::answer_input(browser_id), if allowed { "accepted" } else { "refused" });
    allowed
}

/// The browser is the agent overlay surface (a trusted UI browser in the overlay host).
pub fn is_agent_surface(browser_id: i32) -> bool {
    browsers::is_ui_browser(browser_id) && browsers::role_of(browser_id) == Some(Role::Surface(browsers::Surface::Agent))
}

/// The browser is an internal-page tab showing `sta://settings/`.
pub fn is_settings_page(browser_id: i32) -> bool {
    let Some(Role::Tab(_)) = browsers::role_of(browser_id) else { return false };
    let url = browsers::browser(browser_id)
        .and_then(|b| cef::ImplBrowser::main_frame(&b))
        .map(|f| cef::CefString::from(&cef::ImplFrame::url(&f)).to_string())
        .unwrap_or_default();
    browsers::is_ui_browser(browser_id) && crate::scheme::host_of(&url) == Some("settings")
}

/// `--disable-backgrounding-occluded-windows` while agents may connect (a covered window keeps
/// rendering, so screenshots and clicks work). Read from `state.json` before CEF initializes.
pub fn occlusion_flag_needed() -> bool {
    endpoint::access_enabled_on_disk()
}

/// Drops every CEF handle the automation module holds (says `bye` to agents first).
pub fn clear() {
    #[cfg(all(debug_assertions, feature = "test-hooks"))]
    crate::test_hooks::clear();
    session::shutdown();
    session::clear();
    frame::clear();
    ui::clear();
    guards::clear();
    console::clear();
    page::clear();
    cdp::clear();
    exec::clear();
}

#[cfg_attr(not(debug_assertions), allow(dead_code))] // used by debug.rs only
pub fn debug_snapshot() -> serde_json::Value {
    let (native, synthetic) = KEY_EVENTS.get();
    serde_json::json!({
        "cdp": cdp::debug_snapshot(),
        "tasks": exec::task_count(),
        "keyEvents": { "native": native, "synthetic": synthetic },
        "session": session::debug_snapshot(),
        "guards": guards::debug_snapshot(),
        "frames": frame::debug_snapshot(),
        "console": console::debug_snapshot(),
        "ui": ui::debug_snapshot(),
    })
}
