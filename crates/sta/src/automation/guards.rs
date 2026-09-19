//! `agent_controlled` tabs and what that changes in the shell's handlers [owner: automation]
//! (docs/MCP.md "Guards", critique S6).
//!
//! DevTools input counts as a user gesture, so a tab an agent acts on is *agent-controlled* until
//! the user presses Stop, access is turned off, or the user types into that tab (real keyboard
//! input: DevTools key events never reach `on_pre_key_event`). Popups of such tabs inherit it.
//! While a tab is agent-controlled:
//! - external protocols (`mailto:`, app links) are never launched;
//! - downloads are held until the user keeps or discards them (`UiState.agent.heldDownloads`);
//! - page fullscreen is exited at once;
//! - file choosers are cancelled (the click reports `file_chooser_blocked`);
//! - permission prompts are dismissed;
//! - JavaScript dialogs are held for `handle_dialog` (no native dialog), `beforeunload` is accepted;
//! - Peek interception is bypassed: popups become background tabs, reported as `openedTabs`;
//! - main-frame navigations to blocked hosts, private-network hosts (unless allowed) and sites the
//!   session hasn't approved are cancelled; blocked hosts are cancelled in every frame.
//!
//! UI-thread only. Every entry point takes a short borrow and never calls CEF inside it.

use crate::{controller, tabs, task};
use sta_core::agent::policy::{self, UrlVerdict};
use sta_core::{Command, Id};
use cef::*;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// Real key input in a tab this recent makes input tools return `user_active`.
pub const USER_ACTIVE_WINDOW: Duration = Duration::from_secs(2);

pub struct Dialog {
    pub kind: &'static str,
    pub message: String,
    pub default_prompt: String,
    callback: JsdialogCallback,
}

struct HeldDownload {
    browser_id: i32,
    suggested_name: String,
    callback: BeforeDownloadCallback,
}

/// What happened in a tab since the last `take_events` (reported by input tools).
#[derive(Debug, Default, Clone)]
pub struct TabEvents {
    pub opened_tabs: Vec<Id>,
    pub file_chooser_blocked: bool,
    pub navigation_blocked: Option<String>,
    pub external_blocked: bool,
    pub download_held: Option<String>,
    pub dialog_opened: bool,
    pub fullscreen_blocked: bool,
}

#[derive(Default)]
struct LoadInfo {
    starts: u64,
    error: Option<(i32, String)>,
}

thread_local! {
    /// Agent-controlled tabs → controlling session.
    static CONTROLLED: RefCell<HashMap<Id, u64>> = RefCell::new(HashMap::new());
    static USER_KEYS: RefCell<HashMap<Id, Instant>> = RefCell::new(HashMap::new());
    static DIALOGS: RefCell<HashMap<Id, Dialog>> = RefCell::new(HashMap::new());
    static HELD: RefCell<HashMap<u32, HeldDownload>> = RefCell::new(HashMap::new());
    static EVENTS: RefCell<HashMap<Id, TabEvents>> = RefCell::new(HashMap::new());
    static LOADS: RefCell<HashMap<Id, LoadInfo>> = RefCell::new(HashMap::new());
    static BLOCKED_COUNT: Cell<u64> = const { Cell::new(0) };
}

fn tab_of(browser_id: i32) -> Option<Id> {
    tabs::tab_for_browser(browser_id)
}

fn note(tab: Id, f: impl FnOnce(&mut TabEvents)) {
    EVENTS.with(|e| f(e.borrow_mut().entry(tab).or_default()));
    BLOCKED_COUNT.set(BLOCKED_COUNT.get() + 1);
}

// ----------------------------------------------------------------------------------- control

pub fn mark_controlled(tab: Id, session: u64) {
    if CONTROLLED.with(|c| c.borrow_mut().insert(tab, session)) != Some(session) {
        super::frame::schedule_refresh();
    }
}

/// Every agent-controlled tab with its controlling session.
pub fn controlled_tabs() -> Vec<(Id, u64)> {
    CONTROLLED.with(|c| c.borrow().iter().map(|(t, s)| (*t, *s)).collect())
}

/// A session ended: its tabs go back to the user (their guards would otherwise keep checking
/// navigations against a session that no longer exists).
pub fn release_session(session: u64) {
    let before = CONTROLLED.with(|c| c.borrow().len());
    CONTROLLED.with(|c| c.borrow_mut().retain(|_, s| *s != session));
    if CONTROLLED.with(|c| c.borrow().len()) != before {
        super::frame::schedule_refresh();
    }
}

pub fn controlling_session(tab: Id) -> Option<u64> {
    CONTROLLED.with(|c| c.borrow().get(&tab).copied())
}

pub fn is_controlled_tab(tab: Id) -> bool {
    controlling_session(tab).is_some()
}

/// The browser shows an agent-controlled tab.
pub fn is_controlled(browser_id: i32) -> bool {
    tab_of(browser_id).is_some_and(is_controlled_tab)
}

/// Stop / access off: every tab goes back to the user.
pub fn release_all() {
    CONTROLLED.with(|c| c.borrow_mut().clear());
    super::frame::schedule_refresh();
}

/// Real keyboard input in a tab browser: the user takes the tab back.
pub fn on_user_key(browser_id: i32) {
    let Some(tab) = tab_of(browser_id) else { return };
    USER_KEYS.with(|k| k.borrow_mut().insert(tab, Instant::now()));
    if CONTROLLED.with(|c| c.borrow_mut().remove(&tab)).is_some() {
        log_info!("agent: the user took tab {tab} back (keyboard input)");
        super::frame::schedule_refresh();
    }
}

pub fn user_active(tab: Id) -> bool {
    USER_KEYS.with(|k| k.borrow().get(&tab).is_some_and(|t| t.elapsed() < USER_ACTIVE_WINDOW))
}

/// Events of a tab since the last call (and resets them).
pub fn take_events(tab: Id) -> TabEvents {
    EVENTS.with(|e| e.borrow_mut().remove(&tab)).unwrap_or_default()
}

// ----------------------------------------------------------------------------------- loads

pub fn on_load_start(browser_id: i32) {
    if let Some(tab) = tab_of(browser_id) {
        LOADS.with(|l| {
            let mut l = l.borrow_mut();
            let info = l.entry(tab).or_default();
            info.starts += 1;
            info.error = None;
        });
    }
}

pub fn on_load_error(browser_id: i32, code: i32, text: &str) {
    if let Some(tab) = tab_of(browser_id) {
        LOADS.with(|l| l.borrow_mut().entry(tab).or_default().error = Some((code, text.to_string())));
    }
}

/// Main-frame load starts seen for the tab (a counter), and the error of the last load.
pub fn load_info(tab: Id) -> (u64, Option<(i32, String)>) {
    LOADS.with(|l| l.borrow().get(&tab).map(|i| (i.starts, i.error.clone())).unwrap_or_default())
}

// ----------------------------------------------------------------------------------- dialogs

/// `JsdialogHandler::on_jsdialog`: held for `handle_dialog` in agent-controlled tabs.
pub fn on_jsdialog(browser_id: i32, dialog_type: JsdialogType, message: &str, default_prompt: &str, callback: &JsdialogCallback) -> bool {
    let Some(tab) = tab_of(browser_id).filter(|t| is_controlled_tab(*t)) else { return false };
    let kind = if dialog_type == JsdialogType::CONFIRM {
        "confirm"
    } else if dialog_type == JsdialogType::PROMPT {
        "prompt"
    } else {
        "alert"
    };
    log_info!("agent: {kind} dialog held for tab {tab}");
    let dialog = Dialog { kind, message: message.chars().take(2000).collect(), default_prompt: default_prompt.chars().take(2000).collect(), callback: callback.clone() };
    let old = DIALOGS.with(|d| d.borrow_mut().insert(tab, dialog));
    if let Some(old) = old {
        old.callback.cont(0, None);
    }
    note(tab, |e| e.dialog_opened = true);
    true
}

/// `beforeunload` of an agent-controlled tab: leave (the agent navigates on purpose).
pub fn on_before_unload(browser_id: i32, callback: &JsdialogCallback) -> bool {
    if !is_controlled(browser_id) {
        return false;
    }
    callback.cont(1, None);
    true
}

/// The open dialog of a tab: `(kind, message)`.
pub fn dialog(tab: Id) -> Option<(&'static str, String)> {
    DIALOGS.with(|d| d.borrow().get(&tab).map(|x| (x.kind, x.message.clone())))
}

/// Answers the held dialog. `Some(kind)` when there was one.
pub fn answer_dialog(tab: Id, accept: bool, prompt_text: Option<&str>) -> Option<&'static str> {
    let dialog = DIALOGS.with(|d| d.borrow_mut().remove(&tab))?;
    let text = prompt_text.map(CefString::from).or_else(|| (dialog.kind == "prompt" && accept).then(|| CefString::from(dialog.default_prompt.as_str())));
    let kind = dialog.kind;
    // Posted: continuing a dialog runs page script synchronously.
    task::post_ui(move || dialog.callback.cont(accept as i32, text.as_ref()));
    Some(kind)
}

/// `on_reset_dialog_state` / browser gone: forget held dialogs.
pub fn on_dialog_state_reset(browser_id: i32) {
    if let Some(tab) = tab_of(browser_id) {
        DIALOGS.with(|d| d.borrow_mut().remove(&tab));
    }
}

// ----------------------------------------------------------------------------------- other handlers

wrap_dialog_handler! {
    struct TabDialog;

    impl DialogHandler {
        fn on_file_dialog(
            &self,
            browser: Option<&mut Browser>,
            _mode: FileDialogMode,
            _title: Option<&CefString>,
            _default_file_path: Option<&CefString>,
            _accept_filters: Option<&mut CefStringList>,
            _accept_extensions: Option<&mut CefStringList>,
            _accept_descriptions: Option<&mut CefStringList>,
            callback: Option<&mut FileDialogCallback>,
        ) -> i32 {
            let (Some(browser), Some(callback)) = (browser, callback) else { return 0 };
            on_file_dialog(browser.identifier(), callback) as i32
        }
    }
}

/// The tab client's `DialogHandler` (default file choosers, except in agent-controlled tabs).
pub fn dialog_handler() -> DialogHandler {
    TabDialog::new()
}

/// `DialogHandler::on_file_dialog`: cancelled in agent-controlled tabs.
pub fn on_file_dialog(browser_id: i32, callback: &FileDialogCallback) -> bool {
    let Some(tab) = tab_of(browser_id).filter(|t| is_controlled_tab(*t)) else { return false };
    log_info!("agent guard: file chooser cancelled in tab {tab}");
    callback.cancel();
    note(tab, |e| e.file_chooser_blocked = true);
    true
}

/// Downloads of agent-controlled tabs wait for the user (`ResolveAgentDownload`).
pub fn hold_download(browser_id: i32, id: u32, suggested_name: &str, callback: &BeforeDownloadCallback) -> bool {
    let Some(tab) = tab_of(browser_id).filter(|t| is_controlled_tab(*t)) else { return false };
    if HELD.with(|h| h.borrow().contains_key(&id)) {
        return true;
    }
    log_info!("agent guard: download {id} from tab {tab} held for the user");
    HELD.with(|h| h.borrow_mut().insert(id, HeldDownload { browser_id, suggested_name: suggested_name.to_string(), callback: callback.clone() }));
    note(tab, |e| e.download_held = Some(suggested_name.chars().take(120).collect()));
    controller::dispatch(Command::AgentDownloadHeld { id, tab: Some(tab), file_name: suggested_name.to_string() });
    true
}

/// `Effect::AgentDownload`.
pub fn resolve_download(id: u32, keep: bool) {
    let Some(held) = HELD.with(|h| h.borrow_mut().remove(&id)) else { return };
    if keep {
        crate::downloads::continue_download(held.browser_id, id, &held.suggested_name, &held.callback);
    } else {
        log_info!("agent: held download {id} discarded");
        // Releasing the callback without `cont` leaves the download pending: cancel it explicitly.
        crate::downloads::control(id, sta_core::DownloadAction::Cancel);
        drop(held);
    }
}

/// `on_fullscreen_mode_change`: agent-controlled tabs never go fullscreen. `true` = suppressed.
pub fn on_fullscreen(browser_id: i32, fullscreen: bool) -> bool {
    let Some(tab) = tab_of(browser_id).filter(|t| is_controlled_tab(*t)) else { return false };
    if !fullscreen {
        return false;
    }
    log_info!("agent guard: page fullscreen refused in tab {tab}");
    note(tab, |e| e.fullscreen_blocked = true);
    task::post_ui(move || tabs::exit_page_fullscreen(tab));
    true
}

/// Permission requests of agent-controlled tabs are dismissed. `true` = dismiss.
pub fn dismiss_permission(browser_id: i32) -> bool {
    let controlled = is_controlled(browser_id);
    if controlled {
        log_info!("agent guard: permission request dismissed (browser {browser_id})");
    }
    controlled
}

/// The user gesture an external-protocol launch gets: none in agent-controlled tabs.
pub fn external_gesture(browser_id: i32, user_gesture: bool) -> bool {
    match tab_of(browser_id).filter(|t| is_controlled_tab(*t)) {
        Some(tab) => {
            if user_gesture {
                log_info!("agent guard: external protocol blocked in tab {tab}");
                note(tab, |e| e.external_blocked = true);
            }
            false
        }
        None => user_gesture,
    }
}

/// Peek interception (`intercept_navigation`) applies: not in agent-controlled tabs.
pub fn allow_peek(browser_id: i32) -> bool {
    !is_controlled(browser_id)
}

/// A popup of an agent-controlled tab opens as a background tab (not Peek, not activated).
/// Returns `Some((popup, foreground))` overrides.
pub fn popup_presentation(opener_browser: i32) -> Option<(bool, bool)> {
    is_controlled(opener_browser).then_some((false, false))
}

/// A popup of an agent-controlled tab was adopted as `tab`.
pub fn on_popup_adopted(opener_tab: Option<Id>, tab: Id) {
    let Some(session) = opener_tab.and_then(controlling_session) else { return };
    mark_controlled(tab, session);
    if let Some(opener) = opener_tab {
        note(opener, |e| e.opened_tabs.push(tab));
    }
    controller::dispatch(Command::AgentTabAdopted { tab });
}

/// `on_before_browse` of a web tab: `true` = cancel. In any frame of an agent-controlled tab:
/// blocked hosts and private-network hosts (unless allowed), so a page an agent opened can't frame
/// a router or an intranet page either; for its main frame also sites its session hasn't approved.
/// (Other schemes have their own guards; subresource requests a page makes itself are not
/// filtered, as for any page.)
pub fn cancel_navigation(browser_id: i32, url: &str, main_frame: bool) -> bool {
    let Some(tab) = tab_of(browser_id) else { return false };
    let Some(session) = controlling_session(tab) else { return false };
    if sta_core::urls::is_about_blank(url) || !sta_core::urls::is_web(url) {
        return false; // other schemes have their own guards (external protocols, sta://)
    }
    let Some(verdict) = controller::with_store(|s| {
        let settings = s.settings();
        match policy::check_url(settings, url) {
            Ok(UrlVerdict::Web { site }) if main_frame && !s.agent_site_approved(session, &site) => Err(Some(site)),
            Ok(_) => Ok(()),
            Err(_) => Err(None),
        }
    }) else {
        return false;
    };
    match verdict {
        Ok(()) => false,
        Err(site) => {
            let shown = site.clone().unwrap_or_else(|| sta_core::urls::host(url).unwrap_or_default());
            log_info!("agent guard: navigation of tab {tab} to {shown} cancelled");
            note(tab, |e| e.navigation_blocked = Some(shown));
            true
        }
    }
}

pub fn on_browser_closed(browser_id: i32) {
    let held: Vec<u32> = HELD.with(|h| h.borrow().iter().filter(|(_, d)| d.browser_id == browser_id).map(|(id, _)| *id).collect());
    // Held downloads of a closed tab can't continue: discard them (and drop them from the list).
    for id in held {
        let callback = HELD.with(|h| h.borrow_mut().remove(&id));
        drop(callback);
        controller::dispatch(Command::ResolveAgentDownload { id, keep: false });
    }
    on_dialog_state_reset(browser_id);
}

/// A tab id is gone for good (called lazily by tools).
pub fn forget_tab(tab: Id) {
    if CONTROLLED.with(|c| c.borrow_mut().remove(&tab)).is_some() {
        super::frame::schedule_refresh();
    }
    USER_KEYS.with(|k| k.borrow_mut().remove(&tab));
    EVENTS.with(|e| e.borrow_mut().remove(&tab));
    LOADS.with(|l| l.borrow_mut().remove(&tab));
}

pub fn clear() {
    CONTROLLED.with(|c| c.borrow_mut().clear());
    let dialogs = DIALOGS.with(|d| std::mem::take(&mut *d.borrow_mut()));
    drop(dialogs);
    let held = HELD.with(|h| std::mem::take(&mut *h.borrow_mut()));
    drop(held);
    EVENTS.with(|e| e.borrow_mut().clear());
    LOADS.with(|l| l.borrow_mut().clear());
}

pub fn debug_snapshot() -> serde_json::Value {
    serde_json::json!({
        "controlled": CONTROLLED.with(|c| c.borrow().keys().copied().collect::<Vec<_>>()),
        "dialogs": DIALOGS.with(|d| d.borrow().iter().map(|(t, x)| serde_json::json!({ "tab": t, "kind": x.kind })).collect::<Vec<_>>()),
        "heldDownloads": HELD.with(|h| h.borrow().keys().copied().collect::<Vec<_>>()),
        "guardEvents": BLOCKED_COUNT.get(),
    })
}
