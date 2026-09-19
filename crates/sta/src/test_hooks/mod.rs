//! The **debug-only MCP test surface** [owner: automation] (docs/TESTING.md, the harness design
//! §4). Nothing here ships: the module is compiled only under
//! `all(debug_assertions, feature = "test-hooks")`, and a release build that tries to enable the
//! feature fails to compile (below) instead of quietly producing an un-armed binary.
//!
//! It exists so the end-to-end suites can drive a running browser **through MCP** instead of
//! through the DevTools port and PowerShell helpers: the `test_*` tools re-expose what
//! `crates/sta/src/debug.rs` exposes to a trusted `sta://` page today, plus the native window,
//! capture, clipboard and console-window probes that used to be `.ps1` scripts.
//!
//! ```text
//! e2e (node) ─stdio─► sta-mcp.exe ─pipe─► session.rs::start_call
//!                                            ├ name starts with `test_` and armed → test_hooks::run   (no policy)
//!                                            └ otherwise → the 23 shipped tools                       (full policy)
//! ```
//!
//! **Four locks** (design §4.1), all required:
//! 1. *compile* — `#[cfg(all(debug_assertions, feature = "test-hooks"))]` on every line, plus the
//!    `compile_error!` below;
//! 2. *runtime* — the command line must contain `--sta-test-hooks` (or
//!    `--sta-test-hooks-no-approve`) **and** the environment `STA_E2E=1`; read once, at startup;
//! 3. *profile* — an explicit `--sta-data-dir=<path>` (or `STA_DATA_DIR`) that is not, and is not
//!    inside, one of the real profiles (`%LOCALAPPDATA%\sta`, `sta Dev`, `Astatine`, <!-- rename:keep -->
//!    `Astatine Dev`). Otherwise the browser **exits with code 2** rather than starting un-armed; <!-- rename:keep -->
//! 4. *invisibility* — un-armed, `tools/list` has no test tool and every `test_*` name answers
//!    `unknown_tool`, the same error a typo gets, so probing leaks nothing.
//!
//! Arming also opens the agent endpoint and pre-sets full agent access **in memory for this run**
//! (never written to `state.json`), which removes the bootstrap deadlock that made the suites
//! reach for the DevTools port before MCP worked. `--sta-test-hooks-no-approve` arms the tools but
//! leaves approvals and access — and therefore the endpoint — exactly as the profile has them, for
//! the consent checks.
//!
//! Modules: `js` (JavaScript, DevTools and targets), `native` (window, hit test, capture, pixels,
//! clipboard, Mark-of-the-Web), `capture` (GDI window capture + a small PNG writer/reader),
//! `console_watch` (console windows on the desktop, including ones that only flash).

#[cfg(all(feature = "test-hooks", not(debug_assertions)))]
compile_error!("the `test-hooks` feature is debug-only: it must never be built into a release binary");

pub mod capture;
pub mod console_watch;
pub mod js;
pub mod native;

use crate::automation::exec;
use crate::automation::tools::{Ctx, Output};
use crate::ipc::Reply;
use crate::{browsers, controller, task};
use cef::wrapper::message_router::BrowserSideCallback;
use serde_json::{Value, json};
use sta_core::agent::ErrorCode;
use sta_core::agent::channel::{Content, ToolError};
use sta_core::agent::test_tools;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

/// Command-line switch that arms the surface (with `STA_E2E=1`).
pub const SWITCH: &str = "--sta-test-hooks";
/// Same, but without the auto-approval and the in-memory full access (`agent-e2e`'s consent
/// sections drive the real prompts).
pub const SWITCH_NO_APPROVE: &str = "--sta-test-hooks-no-approve";
/// Environment variable that must be `1` as well.
pub const ENV: &str = "STA_E2E";

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Arm {
    on: bool,
    /// Connections are auto-approved and agent access is Full for this run (in memory only).
    approve: bool,
}

static ARM: OnceLock<Arm> = OnceLock::new();

thread_local! {
    /// Set by `clear()` during teardown: calls then fail with `test_hooks_off` instead of racing
    /// a shutting-down browser.
    static REVOKED: Cell<bool> = const { Cell::new(false) };
    static REPLIES: RefCell<HashMap<u64, exec::Sender<DebugResult>>> = RefCell::new(HashMap::new());
    static NEXT_REPLY: Cell<u64> = const { Cell::new(1) };
}

// ----------------------------------------------------------------------------------- arming

/// Reads the two runtime tokens and the data directory **once**, before any profile work
/// (`main.rs`). Exits with code 2 when the switch and the environment agree but the data
/// directory is missing or is a real profile — a silent downgrade would run a suite against the
/// user's own browser data and pass.
pub fn arm_from_process() {
    let args: Vec<String> = std::env::args().collect();
    let switch = args.iter().any(|a| a == SWITCH);
    let no_approve = args.iter().any(|a| a == SWITCH_NO_APPROVE);
    let env_on = std::env::var(ENV).is_ok_and(|v| v == "1");
    if !(switch || no_approve) || !env_on {
        let _ = ARM.set(Arm::default());
        return;
    }
    let dir = data_dir_argument(&args);
    let local = std::env::var_os("LOCALAPPDATA").filter(|v| !v.is_empty()).map(PathBuf::from);
    if let Err(why) = data_dir_ok(dir.as_deref(), local.as_deref()) {
        eprintln!("[sta] {SWITCH} needs an explicit test data directory: {why}");
        std::process::exit(2);
    }
    let _ = ARM.set(Arm { on: true, approve: !no_approve });
}

/// The `--sta-data-dir=` switch, else `STA_DATA_DIR` — the same order `paths::resolve` uses.
fn data_dir_argument(args: &[String]) -> Option<PathBuf> {
    args.iter()
        .find_map(|a| a.strip_prefix(crate::paths::DATA_DIR_SWITCH).filter(|s| !s.is_empty()).map(PathBuf::from))
        .or_else(|| std::env::var_os(crate::paths::DATA_DIR_ENV).filter(|v| !v.is_empty()).map(PathBuf::from))
}

/// Lock 3, as a pure function: an explicit directory that is not, and is not inside, one of the
/// four real profile folders under `%LOCALAPPDATA%` (this build's and the ones from before the
/// rename).
pub fn data_dir_ok(dir: Option<&Path>, local_app_data: Option<&Path>) -> Result<(), String> {
    let Some(dir) = dir else {
        return Err(format!("pass {}<path> (or {})", crate::paths::DATA_DIR_SWITCH, crate::paths::DATA_DIR_ENV));
    };
    let Some(local) = local_app_data else { return Ok(()) };
    let normalized = |p: &Path| {
        let p = std::path::absolute(p).unwrap_or_else(|_| p.to_path_buf());
        p.to_string_lossy().replace('/', "\\").trim_end_matches('\\').to_lowercase()
    };
    let wanted = normalized(dir);
    for name in [
        crate::paths::DEFAULT_DIR_RELEASE,
        crate::paths::DEFAULT_DIR_DEBUG,
        sta_core::legacy::DATA_DIR_RELEASE,
        sta_core::legacy::DATA_DIR_DEBUG,
    ] {
        let real = normalized(&local.join(name));
        if wanted == real || wanted.starts_with(&format!("{real}\\")) {
            return Err(format!("{} is the real {name} profile", dir.display()));
        }
    }
    Ok(())
}

/// The test surface is armed for this run.
pub fn armed() -> bool {
    ARM.get().copied().unwrap_or_default().on && !REVOKED.get()
}

/// Arming implies auto-approval and full agent access for this run (`session.rs`), unless the
/// browser was started with `--sta-test-hooks-no-approve`.
pub fn auto_approve() -> bool {
    let arm = ARM.get().copied().unwrap_or_default();
    arm.on && arm.approve && !REVOKED.get()
}

/// `session.rs`: the name belongs to the test surface.
pub fn is_test_tool(name: &str) -> bool {
    test_tools::is_test_tool(name)
}

/// Called once the store and the view tree exist (`controller::startup`): opens the agent endpoint
/// for this run without changing the user's settings, and starts watching for console windows.
pub fn on_startup() {
    if !armed() {
        return;
    }
    log_warn!("TEST HOOKS ARMED: the debug-only MCP test surface is serving {} test_* tools (docs/TESTING.md)", test_tools::TOOLS.len());
    console_watch::install();
    // Opening the endpoint *is* an access change, so it belongs to the approving mode only:
    // `--sta-test-hooks-no-approve` must leave the agent path exactly as the profile has it, which is
    // what `agent-e2e` asserts on (access off ⇒ no endpoint file ⇒ `browser_not_running`).
    if auto_approve() {
        crate::automation::set_endpoint(true);
    }
}

/// Teardown (`automation::clear`): later calls fail with `test_hooks_off` instead of touching a
/// shutting-down browser.
pub fn clear() {
    REVOKED.set(true);
    console_watch::uninstall();
    js::raw::clear();
    REPLIES.with(|r| r.borrow_mut().clear());
}

// ----------------------------------------------------------------------------------- errors

pub(crate) fn err(code: ErrorCode, message: impl Into<String>) -> ToolError {
    ToolError::new(code, message)
}

pub(crate) fn invalid(message: impl Into<String>) -> ToolError {
    err(ErrorCode::InvalidArguments, message)
}

/// One line for the model plus the machine data in `_meta["sta/structured"]` (the shipped
/// convention, `sta-mcp/src/server.rs`).
pub(crate) fn out(summary: impl Into<String>, structured: Value) -> Output {
    Output { content: vec![Content::Text { text: summary.into() }], structured: Some(structured), tab: None, site: None }
}

pub(crate) fn ok_null(summary: impl Into<String>) -> Result<Output, ToolError> {
    Ok(out(summary, Value::Null))
}

// ----------------------------------------------------------------------------------- arguments

pub(crate) fn str_arg(args: &Value, key: &str) -> Result<String, ToolError> {
    args.get(key).and_then(Value::as_str).map(str::to_string).ok_or_else(|| invalid(format!("{key} (a string) is required")))
}

pub(crate) fn opt_str(args: &Value, key: &str) -> Option<String> {
    args.get(key).and_then(Value::as_str).map(str::to_string)
}

pub(crate) fn bool_arg(args: &Value, key: &str, default: bool) -> bool {
    args.get(key).and_then(Value::as_bool).unwrap_or(default)
}

pub(crate) fn timeout_arg(args: &Value) -> i64 {
    args.get("timeoutMs").and_then(Value::as_i64).unwrap_or(10_000).clamp(1, 120_000)
}

/// `[[x, y], …]`.
pub(crate) fn points_arg(args: &Value, key: &str) -> Result<Vec<(f64, f64)>, ToolError> {
    let list = args.get(key).and_then(Value::as_array).ok_or_else(|| invalid(format!("{key} must be an array of [x, y] pairs")))?;
    list.iter()
        .map(|p| {
            let pair = p.as_array().filter(|a| a.len() == 2).ok_or_else(|| invalid(format!("{key}: every entry is a [x, y] pair")))?;
            let (x, y) = (pair[0].as_f64(), pair[1].as_f64());
            match (x, y) {
                (Some(x), Some(y)) => Ok((x, y)),
                _ => Err(invalid(format!("{key}: every entry is a [x, y] pair of numbers"))),
            }
        })
        .collect()
}

// --------------------------------------------------------------------- the debug.* request bridge

type DebugResult = Result<Value, (i32, String)>;

/// A `BrowserSideCallback` that answers into an `exec` oneshot instead of a JavaScript promise, so
/// the test tools can run the **existing** `debug.*` handlers unchanged (same code path the
/// suites' `window.sta.invoke("debug.…")` calls take today).
struct Callback {
    id: u64,
}

impl BrowserSideCallback for Callback {
    fn success_str(&self, response: &str) {
        deliver(self.id, Ok(serde_json::from_str(response).unwrap_or(Value::Null)));
    }

    fn success_binary(&self, data: &[u8]) {
        deliver(self.id, Ok(json!({ "bytes": data.len() })));
    }

    fn failure(&self, error_code: i32, error_message: &str) {
        deliver(self.id, Err((error_code, error_message.to_string())));
    }
}

fn deliver(id: u64, result: DebugResult) {
    // Always through the UI thread: `REPLIES` is thread-local and the debug handlers may answer
    // from a posted task.
    task::post_ui_from_any_thread(move || {
        let tx = REPLIES.with(|r| r.borrow_mut().remove(&id));
        if let Some(tx) = tx {
            tx.send(result);
        }
    });
}

/// The `debug.*` request failure codes, mapped onto MCP error codes.
fn from_debug_code(code: i32, message: String) -> ToolError {
    let mapped = match code {
        400 => ErrorCode::InvalidArguments,
        404 => ErrorCode::NoSuchTarget,
        409 => ErrorCode::WindowBusy,
        503 => ErrorCode::BrowserNotRunning,
        _ => ErrorCode::Internal,
    };
    err(mapped, message)
}

/// A browser id to attribute the request to (`debug.foreign.trigger` sends on its DevTools
/// session): the topbar surface, which always exists while the window does.
fn shell_browser() -> Result<i32, ToolError> {
    use cef::ImplBrowser;
    browsers::surface_browser(browsers::Surface::Topbar)
        .map(|b| b.identifier())
        .ok_or_else(|| err(ErrorCode::BrowserNotRunning, "the shell has no topbar browser (starting up or shutting down)"))
}

/// Runs one `debug.*` request through `debug::handle` and waits for its answer.
pub(crate) async fn debug_request(cmd: &str, payload: Value, timeout_ms: i64) -> Result<Value, ToolError> {
    let browser_id = shell_browser()?;
    let id = NEXT_REPLY.replace(NEXT_REPLY.get() + 1);
    let (tx, rx) = exec::oneshot();
    REPLIES.with(|r| r.borrow_mut().insert(id, tx));
    let callback: Arc<Mutex<dyn BrowserSideCallback>> = Arc::new(Mutex::new(Callback { id }));
    let forget = || REPLIES.with(|r| r.borrow_mut().remove(&id));
    match crate::debug::handle(browser_id, cmd, payload, &callback) {
        Reply::Json(text) => {
            forget();
            Ok(serde_json::from_str(&text).unwrap_or(Value::Null))
        }
        Reply::Err(code, message) => {
            forget();
            Err(from_debug_code(code, message))
        }
        Reply::Deferred => match exec::timeout(timeout_ms, rx).await {
            Some(Some(Ok(value))) => Ok(value),
            Some(Some(Err((code, message)))) => Err(from_debug_code(code, message)),
            Some(None) => Err(err(ErrorCode::Internal, format!("{cmd} was dropped without an answer"))),
            None => {
                forget();
                Err(err(ErrorCode::Timeout, format!("{cmd} did not answer within {timeout_ms} ms")))
            }
        },
    }
}

// ----------------------------------------------------------------------------------- dispatch

/// Runs one test tool. `session.rs` routes here **before** every access, scope, URL, site and
/// rate-limit check — and only for names that start with `test_`.
pub async fn run(ctx: &Ctx, tool: &str, args: &Value) -> Result<Output, ToolError> {
    if REVOKED.get() {
        return Err(err(ErrorCode::TestHooksOff, "sta is shutting down: the test surface is closed"));
    }
    if !armed() {
        // Lock 4: the same answer a typo gets, so probing can't confirm the surface exists.
        return Err(err(ErrorCode::UnknownTool, format!("sta has no tool {tool:?}")));
    }
    if test_tools::find(tool).is_none() {
        return Err(err(ErrorCode::UnknownTool, format!("sta has no tool {tool:?}")));
    }
    let args = if args.is_null() { json!({}) } else { args.clone() };
    dispatch(ctx, tool, &args).await
}

async fn dispatch(ctx: &Ctx, tool: &str, args: &Value) -> Result<Output, ToolError> {
    match tool {
        // ---------------------------------------------------------------- A. core and shell
        "test_info" => info(args).await,
        "test_state" => {
            let state = controller::with_store(|s| s.ui_state()).ok_or_else(|| err(ErrorCode::BrowserNotRunning, "the store is not ready"))?;
            let value = serde_json::to_value(state).map_err(|e| err(ErrorCode::Internal, e.to_string()))?;
            Ok(out("UiState", value))
        }
        "test_dispatch" => {
            let command = args.get("command").cloned().ok_or_else(|| invalid("command is required"))?;
            debug_request("debug.dispatch", command, timeout_arg(args)).await?;
            ok_null("dispatched")
        }
        "test_execute" => {
            let effects = args.get("effects").cloned().ok_or_else(|| invalid("effects is required"))?;
            debug_request("debug.execute", effects, timeout_arg(args)).await?;
            ok_null("effects run")
        }
        "test_push_state" => {
            debug_request("debug.pushState", Value::Null, timeout_arg(args)).await?;
            ok_null("state pushed")
        }
        "test_open_tab" => {
            let value = debug_request("debug.openTab", args.clone(), timeout_arg(args)).await?;
            Ok(out(format!("opened tab {}", value.get("tab").unwrap_or(&Value::Null)), value))
        }
        "test_focus" => {
            debug_request("debug.focus", args.clone(), timeout_arg(args)).await?;
            ok_null("focused")
        }
        "test_accelerator" => {
            let value = debug_request("debug.accelerator", args.clone(), timeout_arg(args)).await?;
            Ok(out(format!("accelerator ran command {}", value.get("commandId").unwrap_or(&Value::Null)), value))
        }
        "test_send_key" => {
            debug_request("debug.sendKey", args.clone(), timeout_arg(args)).await?;
            ok_null("key sent to the window")
        }
        "test_reset_permissions" => {
            let value = debug_request("debug.resetPermissions", args.clone(), timeout_arg(args)).await?;
            Ok(out(format!("{value} content settings reset"), json!({ "reset": value })))
        }
        "test_counts" => {
            let info = debug_request("debug.info", Value::Null, timeout_arg(args)).await?;
            let counts = info.pointer("/controller/commandCounts").cloned().unwrap_or(Value::Null);
            let accelerators = info.pointer("/keyboard/accelerators").cloned().unwrap_or(Value::Null);
            Ok(out("command counters", json!({ "commandCounts": counts, "accelerators": accelerators })))
        }
        // ---------------------------------------------------------------- B/E. JavaScript, DevTools
        "test_eval" => js::eval(args).await,
        "test_invoke" => js::invoke(args).await,
        "test_targets" => js::targets(args).await,
        "test_cdp" => js::cdp(args).await,
        "test_cdp_events" => js::cdp_events(args).await,
        "test_attach" => js::attach(args).await,
        // ---------------------------------------------------------------- C. input
        "test_real_keys" => {
            let value = debug_request("debug.realKeys", args.clone(), 120_000).await?;
            Ok(out(format!("sent {} key transitions", value.get("sent").unwrap_or(&Value::Null)), value))
        }
        "test_post_mouse" => {
            let value = debug_request("debug.postMouse", args.clone(), 120_000).await?;
            Ok(out(format!("posted {} mouse messages", value.get("posted").unwrap_or(&Value::Null)), value))
        }
        "test_hover_input" => {
            let value = debug_request("debug.hoverInput", args.clone(), 30_000).await?;
            Ok(out("hover snapshot", value))
        }
        "test_tab_key" => {
            let value = debug_request("debug.tabKey", args.clone(), timeout_arg(args)).await?;
            Ok(out("key sent to the tab", value))
        }
        // ---------------------------------------------------------------- D. native
        "test_window" => native::window(args),
        "test_hit_test" => native::hit_test(args),
        "test_window_message" => native::window_message(args),
        "test_dialog" => native::dialog(args),
        "test_capture" => native::capture_window(args),
        "test_pixels" => native::pixels(args),
        "test_clipboard_get" => native::clipboard_get(),
        "test_clipboard_set" => native::clipboard_set(args),
        "test_zone_identifier" => native::zone_identifier(args),
        "test_console_windows" => native::console_windows(args),
        // ---------------------------------------------------------------- F. Chrome-created browsers
        "test_foreign" => {
            let value = debug_request("debug.foreign", Value::Null, timeout_arg(args)).await?;
            Ok(out("foreign snapshot", value))
        }
        "test_foreign_close" => {
            let value = debug_request("debug.foreign.close", args.clone(), timeout_arg(args)).await?;
            Ok(out(format!("closed: {value}"), json!({ "closed": value })))
        }
        "test_foreign_trigger" => {
            let value = debug_request("debug.foreign.trigger", args.clone(), timeout_arg(args)).await?;
            Ok(out("Target.createTarget", value))
        }
        // ---------------------------------------------------------------- G. batching
        "test_batch" => batch(ctx, args).await,
        other => Err(err(ErrorCode::UnknownTool, format!("sta has no tool {other:?}"))),
    }
}

async fn info(args: &Value) -> Result<Output, ToolError> {
    let mut value = debug_request("debug.info", Value::Null, timeout_arg(args)).await?;
    if let Some(wanted) = args.get("sections").and_then(Value::as_array) {
        let keep: Vec<&str> = wanted.iter().filter_map(Value::as_str).collect();
        if let Some(map) = value.as_object_mut() {
            map.retain(|k, _| keep.contains(&k.as_str()));
        }
    }
    if let Some(map) = value.as_object_mut() {
        map.insert("at".into(), json!(monotonic_ms()));
        map.insert("testHooks".into(), json!(true));
    }
    Ok(out("shell snapshot", value))
}

/// Milliseconds since the process started: timing assertions must read browser-side stamps, never
/// wall clock around an MCP call (design §10.5).
fn monotonic_ms() -> u64 {
    static START: OnceLock<std::time::Instant> = OnceLock::new();
    START.get_or_init(std::time::Instant::now).elapsed().as_millis() as u64
}

async fn batch(ctx: &Ctx, args: &Value) -> Result<Output, ToolError> {
    let calls = args.get("calls").and_then(Value::as_array).ok_or_else(|| invalid("calls is required"))?;
    if calls.is_empty() || calls.len() > test_tools::MAX_BATCH {
        return Err(invalid(format!("calls takes 1 to {} entries (got {})", test_tools::MAX_BATCH, calls.len())));
    }
    let stop_on_error = bool_arg(args, "stopOnError", true);
    let mut results = Vec::new();
    for call in calls {
        let name = call.get("name").and_then(Value::as_str).ok_or_else(|| invalid("every call needs a name"))?;
        if matches!(name, "test_batch" | "test_real_keys") {
            return Err(invalid(format!("{name} cannot run inside test_batch")));
        }
        if test_tools::find(name).is_none() {
            return Err(err(ErrorCode::UnknownTool, format!("sta has no tool {name:?}")));
        }
        let call_args = call.get("args").cloned().unwrap_or_else(|| json!({}));
        // Boxed: `dispatch` is recursive through this arm.
        let result = Box::pin(dispatch(ctx, name, &call_args)).await;
        match result {
            Ok(output) => results.push(json!({ "name": name, "ok": true, "structured": output.structured })),
            Err(e) => {
                results.push(json!({ "name": name, "ok": false, "error": { "code": e.code.as_str(), "message": e.message } }));
                if stop_on_error {
                    break;
                }
            }
        }
    }
    Ok(out(format!("{} call(s)", results.len()), json!({ "results": results })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_real_profile_is_never_a_test_data_directory() {
        let local = Path::new(r"C:\Users\someone\AppData\Local");
        for bad in [r"C:\Users\someone\AppData\Local\sta", r"C:\Users\someone\AppData\Local\sta Dev", r"C:/Users/someone/AppData/Local/Astatine", r"C:\Users\someone\AppData\Local\Astatine Dev\", r"C:\Users\someone\AppData\Local\sta\deeper"] { // rename:keep (legacy profile names, refused on purpose)
            assert!(data_dir_ok(Some(Path::new(bad)), Some(local)).is_err(), "{bad}");
        }
        for good in [r"C:\ast\tmp\s6\tools\smoke", r"C:\Users\someone\AppData\Local\sta-e2e", r"D:\profiles\work"] {
            assert!(data_dir_ok(Some(Path::new(good)), Some(local)).is_ok(), "{good}");
        }
        assert!(data_dir_ok(None, Some(local)).is_err(), "no data directory at all");
    }

    /// Lock 4: an un-armed build answers `unknown_tool`, never `test_hooks_off` (probing a name
    /// must not confirm that the surface exists).
    #[test]
    fn unarmed_answers_unknown_tool() {
        assert!(!armed(), "the test binary never arms itself");
        let ctx = Ctx { conn: 1, session: 1, deadline: std::time::Instant::now() };
        let Err(e) = futures_lite_block_on(run(&ctx, "test_info", &json!({}))) else { panic!("an un-armed build has no test tools") };
        assert_eq!(e.code, ErrorCode::UnknownTool, "{e:?}");
        assert!(!e.message.contains("test hook"), "{e:?}");
    }

    /// Polls a future that never yields (the un-armed path answers on the first poll).
    fn futures_lite_block_on<T>(fut: impl std::future::Future<Output = T>) -> T {
        let mut fut = std::pin::pin!(fut);
        let waker = std::task::Waker::noop();
        let mut cx = std::task::Context::from_waker(waker);
        match fut.as_mut().poll(&mut cx) {
            std::task::Poll::Ready(v) => v,
            std::task::Poll::Pending => panic!("the un-armed path must answer without waiting"),
        }
    }

    /// Every catalog entry has a dispatch arm (and the other way round).
    #[test]
    fn the_catalog_and_the_dispatcher_agree() {
        let source = include_str!("mod.rs");
        for t in test_tools::TOOLS {
            assert!(source.contains(&format!("\"{}\" =>", t.name)), "{} has no dispatch arm", t.name);
        }
        let arms = source.matches("        \"test_").count();
        assert!(arms >= test_tools::TOOLS.len(), "{arms} arms for {} tools", test_tools::TOOLS.len());
    }
}
