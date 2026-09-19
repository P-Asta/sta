//! The dispatch loop [owner: chrome] (ARCHITECTURE §2.1, §7).
//!
//! Responsibility:
//! - own the single `sta_core::Store` (thread_local, UI thread);
//! - [`dispatch`] only **enqueues** a `Command` and posts one drain task; the drain applies
//!   commands in FIFO order, releases the store borrow, then executes the returned effects one by
//!   one; commands enqueued by synchronous CEF callbacks meanwhile are drained after them;
//! - route **every** `Effect` variant to the owning module ([`execute`]);
//! - after each drain: coalesced `state` push (≤ 30 Hz, snapshot taken when sending) and a
//!   debounced save (1 s, max 5 s under continuous changes) serialized on the UI thread and written
//!   by a background writer thread; `SaveNow` writes synchronously (ordered after pending writes);
//! - load the store at startup (quarantining corrupt files), run `Store::startup`, 60 s `Tick`.
//!
//! Contract guard: if core returns no effects for `WindowCloseRequested`/`Quit`/
//! `WindowControl{Close}` while the shell has not started closing, the controller runs
//! `[SaveNow, Quit]` itself so the window can always be closed (also when core already set its
//! shutting-down flag but the `Quit` effect never ran, e.g. after a panic).
//!
//! Draining never happens inline: [`dispatch`] and [`run_effects`] only post the drain task, so
//! commands never run inside `on_context_initialized`, `on_window_created`, an IPC handler or any
//! other CEF callback.
//!
//! Tick: a 5 s heartbeat dispatches `Tick` every 60 s of monotonic time, and immediately when the
//! wall clock jumped relative to the monotonic clock (clock change, resume from sleep). The same
//! heartbeat polls the OS app theme (`window::check_system_theme`) and the Windows
//! "Animation effects" setting (`window::check_system_animations`).
//!
//! Public API:
//! - `pub fn init(system_dark: bool, system_animations: bool)` — load store, start writer; applies
//!   `SystemThemeChanged` and `SystemAnimationsChanged` first
//! - `pub fn dispatch(cmd: Command)`
//! - `pub fn run_effects(effects: Vec<Effect>)` — execute the effects now, then post a drain for any
//!   commands they enqueued (never drains inline)
//! - `pub fn startup(urls: Vec<String>)` — `Store::startup` effects (called from `on_window_created`)
//! - `pub fn is_starting_up() -> bool` — inside those effects (session restore is not a user action)
//! - `pub fn start_tick()`
//! - `pub fn with_store<R>(f: impl FnOnce(&Store) -> R) -> Option<R>` — short read-only borrow
//! - `pub fn with_store_mut<R>(f: impl FnOnce(&mut Store) -> R) -> Option<R>` — startup switches only
//! - `pub fn alloc_id() -> Option<Id>`
//! - `pub fn execute(effect: Effect)`
//! - `pub fn request_state_push()` — schedule a coalesced push even if the revision is unchanged
//! - `pub fn save_now()`
//! - `pub fn shutdown()` — flush + join the writer, drop the store (after the message loop)
//! - `pub fn emergency_exit(code: i32) -> !` — save synchronously, stop the writer, exit the process
//!   without `cef::shutdown` (fatal startup errors, stuck shutdown)

use crate::{downloads, external, ipc, overlays, paths, permissions, platform, tabs, task, window};
use sta_core::persist::{self, HISTORY_FILE, STATE_FILE};
use sta_core::store::DirtyFlags;
use sta_core::{Command, Effect, Id, Store, now_ms};
use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// Minimum interval between two `state` pushes (≈30 Hz).
const PUSH_INTERVAL: Duration = Duration::from_millis(33);
/// Debounce delay for saves.
const SAVE_DEBOUNCE_MS: i64 = 1000;
/// Save anyway when changes kept coming for this long.
const SAVE_MAX_WAIT: Duration = Duration::from_secs(5);
/// Periodic `Tick` interval.
const TICK_INTERVAL: Duration = Duration::from_secs(60);
/// Heartbeat that checks the tick interval and wall-clock jumps.
const HEARTBEAT_MS: i64 = 5_000;
/// Wall-clock vs monotonic drift (per heartbeat) treated as a clock jump.
const CLOCK_JUMP_MS: i64 = 10_000;

thread_local! {
    static STORE: RefCell<Option<Store>> = const { RefCell::new(None) };
    static QUEUE: RefCell<VecDeque<Command>> = const { RefCell::new(VecDeque::new()) };
    /// A drain task is posted or running.
    static DRAINING: Cell<bool> = const { Cell::new(false) };
    static PUSH: RefCell<PushState> = const { RefCell::new(PushState { scheduled: false, last_revision: 0, last_push: None }) };
    static SAVE: RefCell<SaveState> = const { RefCell::new(SaveState { generation: 0, pending: DirtyFlags { state: false, history: false }, first_dirty: None }) };
    static WRITER: RefCell<Option<Writer>> = const { RefCell::new(None) };
    static TICK_STARTED: Cell<bool> = const { Cell::new(false) };
    static STARTING_UP: Cell<bool> = const { Cell::new(false) };
}

struct PushState {
    scheduled: bool,
    last_revision: u64,
    last_push: Option<Instant>,
}

struct SaveState {
    generation: u64,
    pending: DirtyFlags,
    first_dirty: Option<Instant>,
}

// ----------------------------------------------------------------------------------- lifecycle

/// Loads the store from the profile directory and starts the writer thread. Then applies
/// `SystemThemeChanged{dark}` and `SystemAnimationsChanged{enabled}` synchronously so
/// `Store::startup` sees the right theme and the first `state` push carries the right motion level.
pub fn init(system_dark: bool, system_animations: bool) {
    let now = now_ms();
    let dir = paths::dirs().profile.clone();
    let (state_path, history_path) = (dir.join(STATE_FILE), dir.join(HISTORY_FILE));
    let read = |p: &PathBuf| match persist::read_optional(p) {
        Ok(s) => s,
        Err(e) => {
            log_error!("cannot read {}: {e}", p.display());
            None
        }
    };
    let (state_json, history_json) = (read(&state_path), read(&history_path));
    let (store, report) = Store::load(state_json.as_deref(), history_json.as_deref(), now);
    if report.state_corrupt {
        log_warn!("state.json is corrupt; quarantined");
        persist::quarantine(&state_path, now);
    }
    if report.history_corrupt {
        log_warn!("history.json is corrupt; quarantined");
        persist::quarantine(&history_path, now);
    }
    for w in &report.warnings {
        log_warn!("store load: {w}");
    }
    STORE.with(|s| *s.borrow_mut() = Some(store));
    // Without a writer thread saves are written synchronously (flush_save falls back).
    WRITER.with(|w| *w.borrow_mut() = Writer::start());
    log_info!("store loaded from {}", dir.display());

    let effects = apply(Command::SystemThemeChanged { dark: system_dark });
    run_effects(effects);
    let effects = apply(Command::SystemAnimationsChanged { enabled: system_animations });
    run_effects(effects);
    // The crash-loop guard, before any tab is restored: safe mode tells core to leave every tab
    // unloaded (`Store::startup`) and shows the banner in Settings › Extensions.
    if crate::safe_mode::startup() {
        let effects = apply(Command::SafeModeStarted);
        run_effects(effects);
    }
}

/// `Store::startup(urls)` effects; call once the view tree exists.
pub fn startup(urls: Vec<String>) {
    let effects = STORE.with(|s| s.borrow_mut().as_mut().map(|st| st.startup(urls, now_ms()))).unwrap_or_default();
    STARTING_UP.set(true);
    run_effects(effects);
    STARTING_UP.set(false);
    // An armed test surface opens the agent endpoint for this run without touching the user's
    // settings (docs/TESTING.md), so a suite can reach MCP without bootstrapping over CDP first.
    #[cfg(all(debug_assertions, feature = "test-hooks"))]
    crate::test_hooks::on_startup();
}

/// The `Store::startup` effects (session restore) are being executed right now.
pub fn is_starting_up() -> bool {
    STARTING_UP.get()
}

/// Starts the periodic `Tick` heartbeat (idempotent).
pub fn start_tick() {
    if TICK_STARTED.replace(true) {
        return;
    }
    #[derive(Clone, Copy)]
    struct Beat {
        wall: i64,
        mono: Instant,
        last_tick: Instant,
    }
    fn schedule(prev: Beat) {
        task::post_ui_delayed(HEARTBEAT_MS, move || {
            let shutting_down = with_store(|s| s.is_shutting_down()).unwrap_or(true);
            if shutting_down {
                return;
            }
            window::check_system_theme();
            window::check_system_animations();
            let (wall, mono) = (now_ms(), Instant::now());
            let wall_delta = wall - prev.wall;
            let mono_delta = mono.duration_since(prev.mono).as_millis() as i64;
            let jumped = is_clock_jump(wall_delta, mono_delta);
            let mut last_tick = prev.last_tick;
            if jumped || mono.duration_since(last_tick) >= TICK_INTERVAL {
                if jumped {
                    log_info!("wall clock jumped by {} ms; ticking now", wall_delta - mono_delta);
                }
                dispatch(Command::Tick);
                last_tick = mono;
            }
            schedule(Beat { wall, mono, last_tick });
        });
    }
    let now = Instant::now();
    schedule(Beat { wall: now_ms(), mono: now, last_tick: now });
}

/// Saves synchronously (if a store is loaded), stops the writer and exits the process without
/// `cef::shutdown` (which must never run with live browsers). For fatal startup errors and a
/// stuck shutdown.
pub fn emergency_exit(code: i32) -> ! {
    if with_store(|_| ()).is_some() {
        save_now();
    }
    let writer = WRITER.with(|w| w.borrow_mut().take());
    if let Some(w) = writer {
        w.stop();
    }
    log_warn!("exiting the process now (code {code})");
    std::process::exit(code)
}

/// The wall clock moved differently from the monotonic clock by at least [`CLOCK_JUMP_MS`]
/// between two heartbeats (clock set, time sync, sleep not counted by the monotonic clock).
fn is_clock_jump(wall_delta_ms: i64, mono_delta_ms: i64) -> bool {
    (wall_delta_ms - mono_delta_ms).abs() >= CLOCK_JUMP_MS
}

/// Flushes and joins the writer, drops the store. Call after `run_message_loop` returns.
pub fn shutdown() {
    // A shutdown that reaches this point was clean: the crash-loop guard forgets this run
    // (`emergency_exit` deliberately does not, which is what makes it count as a crash).
    crate::safe_mode::on_clean_shutdown();
    let writer = WRITER.with(|w| w.borrow_mut().take());
    if let Some(w) = writer {
        w.stop();
    }
    QUEUE.with(|q| q.borrow_mut().clear());
    let store = STORE.with(|s| s.borrow_mut().take());
    drop(store);
}

// ----------------------------------------------------------------------------------- dispatch

/// Enqueues a command. Never runs it synchronously.
pub fn dispatch(cmd: Command) {
    QUEUE.with(|q| q.borrow_mut().push_back(cmd));
    if !DRAINING.replace(true) {
        task::post_ui(drain);
    }
}

/// Short read-only access to the store (`None` before `init` / after `shutdown`). Never call CEF
/// inside `f`.
pub fn with_store<R>(f: impl FnOnce(&Store) -> R) -> Option<R> {
    STORE.with(|s| s.borrow().as_ref().map(f))
}

/// Short mutable access for startup configuration that isn't a command (e.g. debug-only store
/// switches). Never call CEF inside `f`; never use it for state changes the UI must see.
pub fn with_store_mut<R>(f: impl FnOnce(&mut Store) -> R) -> Option<R> {
    STORE.with(|s| s.borrow_mut().as_mut().map(f))
}

/// Allocates an id (adopted popups, debug requests).
#[cfg_attr(not(debug_assertions), allow(dead_code))] // stage-2 API: popup adoption (tabs owner)
pub fn alloc_id() -> Option<Id> {
    STORE.with(|s| s.borrow_mut().as_mut().map(|st| st.alloc_id()))
}

/// Executes `effects` now, as if they were returned by a command. Commands they enqueue are
/// drained by a posted task (never inline), which also schedules the state push and save.
pub fn run_effects(effects: Vec<Effect>) {
    let was_draining = DRAINING.replace(true);
    for e in effects {
        execute_guarded(e);
    }
    if !was_draining {
        // No drain was posted or running: post one. An outer drain (running or posted) otherwise
        // picks up anything enqueued meanwhile.
        task::post_ui(drain);
    }
}

fn drain() {
    // The queue borrow ends before `apply`: effects may enqueue more commands.
    while let Some(cmd) = QUEUE.with(|q| q.borrow_mut().pop_front()) {
        let effects = apply(cmd);
        for e in effects {
            execute_guarded(e);
        }
    }
    DRAINING.set(false);
    after_drain();
}

fn is_noisy(cmd: &Command) -> bool {
    matches!(cmd, Command::TabLoadProgress { .. } | Command::WindowStateChanged { .. } | Command::Tick)
}

/// Applies one command with the store borrowed only for the duration of `Store::apply`.
fn apply(cmd: Command) -> Vec<Effect> {
    let now = now_ms();
    if !is_noisy(&cmd) {
        log_debug!("command {cmd:?}");
    }
    #[cfg(debug_assertions)]
    count_command(&cmd);
    // Ctrl+S docks for real (or hides): a later park is never the end of a dock for a panel.
    let toggles_sidebar = matches!(cmd, Command::ToggleSidebar);
    let guard_close = matches!(
        cmd,
        Command::WindowCloseRequested
            | Command::Quit
            | Command::WindowControl { action: sta_core::WindowAction::Close }
    );
    let result = STORE.with(|s| {
        let mut s = s.borrow_mut();
        let st = s.as_mut()?;
        Some(catch_unwind(AssertUnwindSafe(|| st.apply(cmd, now))))
    });
    let Some(effects) = result else { return Vec::new() };
    let effects = match effects {
        Ok(e) => e,
        Err(_) => {
            log_error!("Store::apply panicked; command dropped");
            Vec::new()
        }
    };
    if toggles_sidebar {
        window::note_sidebar_toggled();
    }
    let quits = effects.iter().any(|e| matches!(e, Effect::Quit));
    if guard_close && !quits && !window::is_closing() {
        // Core ignores commands once it is shutting down; if its `Quit` never reached the shell
        // (or it returned nothing), the window must still close.
        log_warn!("close request produced no Quit (effects: {effects:?}); forcing [SaveNow, Quit]");
        let mut effects = effects;
        effects.extend([Effect::SaveNow, Effect::Quit]);
        return effects;
    }
    effects
}

fn after_drain() {
    let Some((revision, dirty, shutting_down)) = STORE.with(|s| {
        let mut s = s.borrow_mut();
        let st = s.as_mut()?;
        Some((st.revision(), st.take_dirty(), st.is_shutting_down()))
    }) else {
        return;
    };
    let last = PUSH.with(|p| p.borrow().last_revision);
    if revision != last {
        request_state_push();
    }
    if dirty.any() && !shutting_down {
        schedule_save(dirty);
    }
}

// ----------------------------------------------------------------------------------- effects

fn execute_guarded(effect: Effect) {
    if catch_unwind(AssertUnwindSafe(|| execute(effect))).is_err() {
        log_error!("effect execution panicked");
    }
}

/// Routes one effect to its owner module. Runs with no store borrow held.
pub fn execute(effect: Effect) {
    log_debug!("effect {effect:?}");
    match effect {
        // browsers
        Effect::CheckForUpdate => crate::update::start_check("settings"),
        Effect::DownloadUpdate => crate::update::start_download(),
        // The staged build takes over from here: it waits for this process to exit before it
        // touches a file, so the quit below is what lets the update happen at all.
        Effect::InstallUpdate => {
            if crate::update::install_now() {
                dispatch(sta_core::Command::Quit);
            }
        }
        Effect::CreateBrowser { tab, url, internal, muted } => tabs::create_browser(tab, &url, internal, muted),
        Effect::ReplaceBrowser { tab, url, internal } => tabs::replace_browser(tab, &url, internal),
        Effect::DestroyBrowser { tab } => tabs::destroy_browser(tab),
        // content area
        Effect::ShowContent { layout } => tabs::show_content(&layout),
        Effect::FocusBrowser { tab } => tabs::focus_browser(tab),
        Effect::ShowPeek { tab } => overlays::show_peek(tab),
        Effect::HidePeek { tab } => overlays::hide_peek(tab),
        // page actions
        Effect::LoadUrl { tab, url } => tabs::load_url(tab, &url),
        Effect::GoBack { tab } => tabs::go_back(tab),
        Effect::GoForward { tab } => tabs::go_forward(tab),
        Effect::Reload { tab, ignore_cache } => tabs::reload(tab, ignore_cache),
        Effect::StopLoad { tab } => tabs::stop_load(tab),
        Effect::Zoom { tab, direction } => tabs::zoom(tab, direction),
        Effect::SetAudioMuted { tab, muted } => tabs::set_audio_muted(tab, muted),
        Effect::OpenDevTools { tab, docked } => crate::devtools::open(tab, docked),
        Effect::CloseDevTools { tab } => crate::devtools::close(tab),
        Effect::FocusDevTools { tab } => crate::devtools::focus(tab),
        Effect::InspectAt { tab, x, y } => crate::devtools::inspect_at(tab, x, y),
        Effect::Print { tab } => tabs::print(tab),
        Effect::Find { tab, text, forward, match_case, find_next } => tabs::find(tab, &text, forward, match_case, find_next),
        Effect::StopFinding { tab } => tabs::stop_finding(tab),
        Effect::ExitPageFullscreen { tab } => tabs::exit_page_fullscreen(tab),
        Effect::StartDownload { tab, url } => tabs::start_download(tab, &url),
        // chrome & overlays
        Effect::ShowCommandBar => overlays::show_command_bar(),
        Effect::HideCommandBar => overlays::hide_command_bar(),
        Effect::ShowFindBar { tab } => overlays::show_find_bar(tab),
        Effect::HideFindBar => overlays::hide_find_bar(),
        Effect::ShowSwitcher => overlays::show_switcher(),
        Effect::HideSwitcher => overlays::hide_switcher(),
        Effect::ShowToast => overlays::show_toast(),
        Effect::HideToast => overlays::hide_toast(),
        Effect::ShowPermissionPrompt { tab } => overlays::show_permission_prompt(tab),
        Effect::HidePermissionPrompt => overlays::hide_permission_prompt(),
        Effect::AnswerPermission { id, allow, remember } => permissions::answer(id, allow, remember),
        Effect::SetSidebar { visible, width, floating } => window::set_sidebar(visible, width, floating),
        Effect::SetChrome { frame_argb, dark, accent_argb, surface_argb, border_argb, frame_border_argb } => {
            window::set_chrome(frame_argb, dark, [accent_argb, surface_argb, border_argb, frame_border_argb])
        }
        Effect::SetPageFullscreen { tab } => window::set_page_fullscreen(tab),
        // OS / app
        Effect::CopyToClipboard { text } => {
            if !platform::set_clipboard_text(&text) {
                log_warn!("clipboard write failed");
            }
        }
        Effect::OpenExternal { url } => external::open(&url, true, "typed URL"),
        Effect::Window { action } => window::window_action(action),
        Effect::DownloadControl { id, action } => downloads::control(id, action),
        Effect::SaveNow => save_now(),
        Effect::Quit => window::begin_shutdown(),
        // AI agents (MCP)
        Effect::OpenExtensionPopup { id, url, tab } => crate::ext_popup::open(id, url, tab),
        Effect::HideExtensionPopup => crate::ext_popup::close(),
        Effect::RefreshExtensions => crate::extensions::refresh_soon(0),
        Effect::ExtensionOp { id, op } => crate::ext_backend::run(id, op),

        Effect::AgentEndpoint { enabled } => crate::automation::set_endpoint(enabled),
        Effect::AgentAnswer { id, allow } => crate::automation::answer(id, allow),
        Effect::AgentDisconnect => crate::automation::disconnect_all(),
        Effect::AgentDownload { id, keep } => crate::automation::resolve_download(id, keep),
        Effect::ShowAgentOverlay { prompt } => crate::automation::ui::show_overlay(prompt),
        Effect::HideAgentOverlay => crate::automation::ui::hide_overlay(),
    }
}

// ----------------------------------------------------------------------------------- state push

/// Schedules one coalesced `state` push (≤ 30 Hz). The snapshot is taken when sending.
pub fn request_state_push() {
    let delay = PUSH.with(|p| {
        let mut p = p.borrow_mut();
        if p.scheduled {
            return None;
        }
        p.scheduled = true;
        let wait = p.last_push.map(|t| PUSH_INTERVAL.saturating_sub(t.elapsed())).unwrap_or_default();
        Some(wait.as_millis() as i64)
    });
    if let Some(ms) = delay {
        task::post_ui_delayed(ms, send_state_push);
    }
}

fn send_state_push() {
    let revision = with_store(|s| s.revision()).unwrap_or(0);
    PUSH.with(|p| {
        let mut p = p.borrow_mut();
        p.scheduled = false;
        p.last_revision = revision;
        p.last_push = Some(Instant::now());
    });
    ipc::push_state();
}

// ----------------------------------------------------------------------------------- saving

fn schedule_save(dirty: DirtyFlags) {
    let (generation, delay) = SAVE.with(|s| {
        let mut s = s.borrow_mut();
        s.pending.state |= dirty.state;
        s.pending.history |= dirty.history;
        s.generation += 1;
        let first = *s.first_dirty.get_or_insert_with(Instant::now);
        let delay = if first.elapsed() >= SAVE_MAX_WAIT { 0 } else { SAVE_DEBOUNCE_MS };
        (s.generation, delay)
    });
    task::post_ui_delayed(delay, move || {
        if SAVE.with(|s| s.borrow().generation) == generation {
            flush_save(false);
        }
    });
}

/// Writes `state.json` and `history.json` synchronously (after any queued background writes).
pub fn save_now() {
    SAVE.with(|s| {
        let mut s = s.borrow_mut();
        s.pending = DirtyFlags { state: true, history: true };
    });
    flush_save(true);
}

fn flush_save(sync: bool) {
    let pending = SAVE.with(|s| {
        let mut s = s.borrow_mut();
        s.generation += 1; // invalidates scheduled debounced saves
        s.first_dirty = None;
        std::mem::take(&mut s.pending)
    });
    if !pending.any() {
        return;
    }
    let Some((state, history)) = with_store(|s| {
        (pending.state.then(|| s.state_json()), pending.history.then(|| s.history_json()))
    }) else {
        return;
    };
    let dir = paths::dirs().profile.clone();
    let mut jobs = Vec::new();
    if let Some(json) = state {
        jobs.push((dir.join(STATE_FILE), json));
    }
    if let Some(json) = history {
        jobs.push((dir.join(HISTORY_FILE), json));
    }
    WRITER.with(|w| match w.borrow().as_ref() {
        Some(writer) => writer.write(jobs, sync),
        None => write_files(jobs),
    });
}

fn write_files(jobs: Vec<(PathBuf, String)>) {
    for (path, contents) in jobs {
        match persist::write_atomic(&path, &contents) {
            Ok(()) => log_debug!("saved {}", path.display()),
            Err(e) => log_error!("save {} failed: {e}", path.display()),
        }
    }
}

enum Job {
    Write(Vec<(PathBuf, String)>),
    Sync(mpsc::Sender<()>),
    Stop,
}

/// Background writer: jobs are written in order, so a synchronous save is never overwritten by
/// an older debounced one.
struct Writer {
    tx: mpsc::Sender<Job>,
    handle: std::thread::JoinHandle<()>,
}

impl Writer {
    /// `None` if the thread cannot be spawned (this runs inside `on_context_initialized`, where a
    /// panic would abort the process).
    fn start() -> Option<Writer> {
        let (tx, rx) = mpsc::channel::<Job>();
        let spawned = std::thread::Builder::new().name("sta-writer".into()).spawn(move || {
            while let Ok(job) = rx.recv() {
                match job {
                    Job::Write(jobs) => write_files(jobs),
                    Job::Sync(ack) => {
                        let _ = ack.send(());
                    }
                    Job::Stop => break,
                }
            }
        });
        match spawned {
            Ok(handle) => Some(Writer { tx, handle }),
            Err(e) => {
                log_error!("cannot start the writer thread ({e}); saving synchronously");
                None
            }
        }
    }

    fn write(&self, jobs: Vec<(PathBuf, String)>, sync: bool) {
        if self.tx.send(Job::Write(jobs)).is_err() {
            log_error!("writer thread is gone");
            return;
        }
        if sync {
            let (ack_tx, ack_rx) = mpsc::channel();
            if self.tx.send(Job::Sync(ack_tx)).is_ok() && ack_rx.recv_timeout(Duration::from_secs(10)).is_err() {
                log_error!("synchronous save timed out");
            }
        }
    }

    fn stop(self) {
        let _ = self.tx.send(Job::Stop);
        let _ = self.handle.join();
    }
}

#[cfg(debug_assertions)]
thread_local! {
    static COMMAND_COUNTS: RefCell<std::collections::BTreeMap<String, u64>> = const { RefCell::new(std::collections::BTreeMap::new()) };
}

/// Debug builds: counts applied commands by their JSON `type` (see `debug.info`).
#[cfg(debug_assertions)]
fn count_command(cmd: &Command) {
    let name = serde_json::to_value(cmd)
        .ok()
        .and_then(|v| v.get("type").and_then(|t| t.as_str()).map(str::to_owned))
        .unwrap_or_else(|| "?".into());
    COMMAND_COUNTS.with(|c| *c.borrow_mut().entry(name).or_insert(0) += 1);
}

/// Queue/writer status for `debug.info`.
#[cfg_attr(not(debug_assertions), allow(dead_code))] // used by debug.rs only
pub fn debug_snapshot() -> serde_json::Value {
    #[cfg(debug_assertions)]
    let counts = COMMAND_COUNTS.with(|c| serde_json::to_value(&*c.borrow()).unwrap_or_default());
    #[cfg(not(debug_assertions))]
    let counts = serde_json::Value::Null;
    serde_json::json!({
        "queued": QUEUE.with(|q| q.borrow().len()),
        "draining": DRAINING.with(|d| d.get()),
        "revision": with_store(|s| s.revision()),
        "shuttingDown": with_store(|s| s.is_shutting_down()),
        "lastPushedRevision": PUSH.with(|p| p.borrow().last_revision),
        "commandCounts": counts,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clock_jumps() {
        assert!(!is_clock_jump(5_000, 5_000));
        assert!(!is_clock_jump(5_900, 5_000)); // scheduling jitter
        assert!(is_clock_jump(3_605_000, 5_000)); // clock set forward an hour
        assert!(is_clock_jump(-60_000, 5_000)); // clock set back
        assert!(is_clock_jump(20_000, 5_000)); // resumed after sleep
    }
}
