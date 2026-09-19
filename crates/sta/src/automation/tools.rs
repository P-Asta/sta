//! MCP tool implementations [owner: automation] (docs/MCP.md "Tools").
//!
//! Every tool runs as a UI-thread task (see `exec`) with a deadline. Tab targeting, scope, URL and
//! site policy are checked on every call; page-derived strings (titles, URLs, element names, text)
//! are returned inside a per-call nonce boundary (`agent::text::untrusted`).
//!
//! Visibility (docs/research/automation.md): DOM, accessibility, focus and keyboard input work on
//! background tabs; screenshots and mouse input need the tab on screen (`tab_not_visible`).

use super::page::{self, Frame};
use super::{exec, guards, session};
use crate::{browsers, controller, tabs, window};
use sta_core::agent::channel::{Content, ToolError};
use sta_core::agent::policy::{self, UrlVerdict};
use sta_core::agent::tools::*;
use sta_core::agent::{ErrorCode, keys, snapshot, text};
use sta_core::{AgentAccess, Command, Id, NodeView, TabView};
use cef::{ImplBrowser, ImplFrame};
use cef::CefString;
use serde_json::{Value, json};
use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

use super::cdp::p;

/// Calls queued per tab before `busy`.
const MAX_QUEUED_PER_TAB: usize = 8;
/// Longest wait for a page load after an action.
const LOAD_WAIT_MS: u64 = 10_000;
/// Largest screenshot (base64 characters).
const MAX_IMAGE_BASE64: usize = 3_500_000;
/// Largest page width or height a full-page screenshot covers (CSS px).
const MAX_FULL_PAGE_CSS: f64 = 16_384.0;
/// Below this many image pixels per CSS pixel, a full-page screenshot's text can't be read.
const MIN_READABLE_SCALE: f64 = 0.3;

pub struct Ctx {
    pub conn: u64,
    pub session: u64,
    pub deadline: Instant,
}

#[derive(Default)]
pub struct Output {
    pub content: Vec<Content>,
    pub structured: Option<Value>,
    /// For the activity list and the log: the tab and site acted on.
    pub tab: Option<Id>,
    pub site: Option<String>,
}

pub(super) fn text_output(text: String) -> Output {
    Output { content: vec![Content::Text { text }], ..Default::default() }
}

pub(super) fn err(code: ErrorCode, message: impl Into<String>) -> ToolError {
    ToolError::new(code, message)
}

pub(super) fn invalid(message: impl Into<String>) -> ToolError {
    err(ErrorCode::InvalidArguments, message)
}

pub(super) fn nonce() -> String {
    super::win::random_hex(6).unwrap_or_else(|| format!("{:x}", Instant::now().elapsed().as_nanos()))
}

// ----------------------------------------------------------------------------------- tab queue

/// Per tab: busy, and the calls waiting for it (oldest first, with their waiter ids).
type Queue = (bool, VecDeque<(u64, exec::Sender<()>)>);

thread_local! {
    static QUEUES: RefCell<HashMap<Id, Queue>> = RefCell::new(HashMap::new());
    static NEXT_WAITER: std::cell::Cell<u64> = const { std::cell::Cell::new(1) };
}

/// One call at a time per tab (FIFO); `busy` when more than [`MAX_QUEUED_PER_TAB`] wait.
pub struct TabLock {
    tab: Id,
}

/// Hands the tab to the oldest waiter (the tab stays marked busy for it), or frees the tab.
fn release_tab(tab: Id) {
    let next = QUEUES.with(|q| {
        let mut q = q.borrow_mut();
        let entry = q.get_mut(&tab)?;
        match entry.1.pop_front() {
            Some((_, tx)) => Some(tx),
            None => {
                q.remove(&tab);
                None
            }
        }
    });
    if let Some(tx) = next {
        tx.send(());
    }
}

impl Drop for TabLock {
    fn drop(&mut self) {
        release_tab(self.tab);
    }
}

/// A call waiting in a tab's queue. When it is dropped before it got the lock (the MCP client
/// cancelled the call, or its deadline passed while it waited), it leaves the queue — or, if the
/// tab was already handed to it, passes the tab on — so a tab never stays busy without a holder.
struct Waiter {
    tab: Id,
    id: u64,
    armed: bool,
}

impl Drop for Waiter {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let still_queued = QUEUES.with(|q| {
            let mut q = q.borrow_mut();
            let Some(entry) = q.get_mut(&self.tab) else { return false };
            let before = entry.1.len();
            entry.1.retain(|(id, _)| *id != self.id);
            entry.1.len() != before
        });
        if !still_queued {
            release_tab(self.tab);
        }
    }
}

pub(super) async fn lock_tab(tab: Id) -> Result<TabLock, ToolError> {
    let rx = QUEUES.with(|q| {
        let mut q = q.borrow_mut();
        let entry = q.entry(tab).or_insert((false, VecDeque::new()));
        if !entry.0 {
            entry.0 = true;
            return Ok(None);
        }
        if entry.1.len() >= MAX_QUEUED_PER_TAB {
            return Err(err(ErrorCode::Busy, format!("{} calls are already waiting for tab {tab}", entry.1.len())));
        }
        let (tx, rx) = exec::oneshot();
        let id = NEXT_WAITER.replace(NEXT_WAITER.get() + 1);
        entry.1.push_back((id, tx));
        Ok(Some((id, rx)))
    })?;
    if let Some((id, rx)) = rx {
        let mut waiter = Waiter { tab, id, armed: true };
        // Woken by the previous lock's drop (which keeps the tab marked busy for us).
        let woken = rx.await;
        waiter.armed = false;
        woken.ok_or_else(|| err(ErrorCode::Busy, "The tab queue was reset"))?;
    }
    Ok(TabLock { tab })
}

// ----------------------------------------------------------------------------------- targeting

pub struct Target {
    pub tab: Id,
    pub browser: i32,
    pub url: String,
    pub site: Option<String>,
}

#[derive(Clone, Copy, Default)]
pub(super) struct Want {
    /// The call may run while a dialog is open (`handle_dialog`).
    pub dialog_ok: bool,
}

pub(super) fn settings() -> Option<sta_core::Settings> {
    controller::with_store(|s| s.settings().clone())
}

/// Picks the tab of a call: `tab`, else the tab a ref names, else the session's current tab.
pub(super) fn pick_tab(ctx: &Ctx, tab: Option<Id>, element: Option<&str>) -> Result<Id, ToolError> {
    if let Some(t) = tab {
        return Ok(t);
    }
    if let Some(t) = element.and_then(page::ref_tab) {
        return Ok(t);
    }
    session::current_tab(ctx.conn).ok_or_else(|| err(ErrorCode::NoSuchTab, "No current tab yet").with_hint("Call tabs_list, or open a tab with tab_open."))
}

/// Checks everything about a tab before a page tool touches it.
pub(super) async fn target(ctx: &Ctx, tab: Id, want: Want) -> Result<Target, ToolError> {
    let Some((exists, in_scope)) = controller::with_store(|s| {
        let exists = s.tab(tab).is_some();
        (exists, policy::in_scope(s.settings().agent_scope, s.agent_tabs(), tab))
    }) else {
        return Err(err(ErrorCode::BrowserNotRunning, "sta is shutting down"));
    };
    if !exists {
        return Err(err(ErrorCode::NoSuchTab, format!("There is no tab {tab}")));
    }
    if !in_scope {
        return Err(err(ErrorCode::NotInScope, format!("Tab {tab} isn't shared with agents")));
    }
    let Some(browser) = tabs::browser_for_tab(tab) else {
        return Err(err(ErrorCode::TabNotLoaded, format!("Tab {tab} is not loaded")));
    };
    let browser_id = browser.identifier();
    let url = browser.main_frame().map(|f| CefString::from(&f.url()).to_string()).unwrap_or_default();
    drop(browser);
    if browsers::is_ui_browser(browser_id) {
        return Err(err(ErrorCode::InternalPage, format!("Tab {tab} shows a sta page")));
    }
    if !want.dialog_ok
        && let Some((kind, message)) = guards::dialog(tab)
    {
        return Err(dialog_error(tab, kind, &message));
    }
    if url.to_ascii_lowercase().starts_with("chrome-error:") {
        let (_, error) = guards::load_info(tab);
        let text = error.map(|(code, t)| format!("{t} ({code})")).unwrap_or_else(|| "the page failed to load".into());
        return Err(err(ErrorCode::NavigationFailed, format!("Tab {tab}: {text}")));
    }
    let settings = settings().unwrap_or_default();
    let site = match policy::check_url(&settings, &url) {
        Ok(UrlVerdict::Blank) => None,
        Ok(UrlVerdict::Web { site }) => Some(site),
        Err(e) => return Err(e),
    };
    if let Some(site) = &site {
        session::ensure_site(ctx.conn, ctx.session, Some(tab), site, ctx.deadline).await?;
    }
    session::set_current_tab(ctx.conn, tab);
    Ok(Target { tab, browser: browser_id, url, site })
}

fn dialog_error(tab: Id, kind: &str, message: &str) -> ToolError {
    let n = nonce();
    err(ErrorCode::DialogOpen, format!("A {kind} dialog is open in tab {tab}. Its message: {}", text::untrusted(&n, &message.chars().take(300).collect::<String>())))
}

pub(super) fn full_access_required(tool: &str) -> Result<(), ToolError> {
    // `session::access()`, not the store: the dispatcher's `check_access` uses it too, and an armed
    // test build reports full access in memory for the run (docs/TESTING.md §2). Reading the store
    // here made the two disagree — a call passed the gate and was then refused `read_only`.
    match session::access() {
        AgentAccess::Full => Ok(()),
        _ => Err(err(ErrorCode::ReadOnly, format!("{tool} needs full agent access"))),
    }
}

pub(super) fn check_input_allowed(tab: Id) -> Result<(), ToolError> {
    if guards::user_active(tab) {
        return Err(err(ErrorCode::UserActive, format!("The user is typing in tab {tab}")));
    }
    Ok(())
}

pub(super) fn check_visible(tab: Id) -> Result<(), ToolError> {
    if window::is_minimized() {
        return Err(err(ErrorCode::TabNotVisible, "The sta window is minimized").with_hint("Ask the user to restore the sta window."));
    }
    if !tabs::is_tab_visible(tab) {
        return Err(err(ErrorCode::TabNotVisible, format!("Tab {tab} is in the background")));
    }
    Ok(())
}

pub(super) async fn page_state(target: &Target, frame: &Frame, deadline: Instant) -> Result<Value, ToolError> {
    page::eval_document(target.browser, frame, page::PAGE_STATE, vec![], deadline, page::CALL_MS).await
}

pub(super) async fn wait_ms(ms: u64) {
    exec::sleep(ms as i64).await;
}

/// After an action: wait briefly for a navigation to start, then for it to load. Returns whether
/// the tab navigated.
pub(super) async fn settle(target: &Target, starts_before: u64, deadline: Instant) -> bool {
    let start = Instant::now();
    let mut navigated = false;
    while start.elapsed() < Duration::from_millis(400) && Instant::now() < deadline {
        if guards::load_info(target.tab).0 != starts_before {
            navigated = true;
            break;
        }
        wait_ms(50).await;
    }
    if navigated {
        wait_loaded(target.tab, LoadState::Load, deadline.min(Instant::now() + Duration::from_millis(LOAD_WAIT_MS))).await;
    }
    navigated
}

/// Waits until the tab's browser exists and has finished loading (`Load`) or parsing
/// (`Domcontentloaded`). `false` on timeout.
async fn wait_loaded(tab: Id, state: LoadState, until: Instant) -> bool {
    if state == LoadState::None {
        return true;
    }
    let mut seen_browser_at: Option<Instant> = None;
    loop {
        if let Some(b) = tabs::browser_for_tab(tab) {
            let loading = b.is_loading() != 0;
            let id = b.identifier();
            drop(b);
            let since = *seen_browser_at.get_or_insert_with(Instant::now);
            // A fresh browser reports "not loading" before its first navigation starts.
            let started = guards::load_info(tab).0 > 0 || since.elapsed() > Duration::from_millis(1500);
            if !loading && started {
                return true;
            }
            if state == LoadState::Domcontentloaded && started {
                let deadline = Instant::now() + Duration::from_millis(1000);
                if let Ok(frame) = page::main_frame(id, deadline).await
                    && let Ok(v) = page::eval_document(id, &frame, "function(){ return this.readyState; }", vec![], deadline, 1000).await
                    && v.as_str().is_some_and(|s| s != "loading")
                {
                    return true;
                }
            }
        }
        if Instant::now() >= until {
            return false;
        }
        wait_ms(100).await;
    }
}

/// The committed URL of a tab's main frame (`None` when the tab isn't loaded).
pub(super) fn committed_url(tab: Id) -> Option<String> {
    tabs::browser_for_tab(tab).and_then(|b| b.main_frame()).map(|f| CefString::from(&f.url()).to_string())
}

/// What happened during an action, for the result: lines for the model and the structured flags.
/// `url_before`: the tab's committed URL before the action (`urlChanged` compares it with now).
pub(super) fn describe_events(tab: Id, events: &guards::TabEvents, navigated: bool, url_before: Option<&str>) -> (Vec<String>, Value) {
    let mut lines = Vec::new();
    let url_changed = url_before.is_some_and(|before| committed_url(tab).is_some_and(|now| now != before));
    if navigated {
        lines.push(format!("Tab {tab} navigated; refs from before are stale (call page_snapshot)."));
    } else if url_changed {
        lines.push(format!("The URL of tab {tab} changed within the page (tabs_list shows it)."));
    }
    if !events.opened_tabs.is_empty() {
        let list: Vec<String> = events.opened_tabs.iter().map(|t| t.to_string()).collect();
        lines.push(format!("Opened background tab(s): {}.", list.join(", ")));
    }
    if events.dialog_opened || guards::dialog(tab).is_some() {
        lines.push("A JavaScript dialog is open: answer it with handle_dialog.".into());
    }
    if let Some(site) = &events.navigation_blocked {
        lines.push(format!("A navigation to {site} was cancelled (not approved for agents, blocked, or on the local network); use tab_navigate to ask for approval."));
    }
    if events.external_blocked {
        lines.push("An external app link (mailto:, app protocol) was not opened: agents can't launch other apps.".into());
    }
    if let Some(name) = &events.download_held {
        lines.push(format!("A download ({}) waits for the user to keep or discard it.", text::quote(name, 80)));
    }
    if events.fullscreen_blocked {
        lines.push("The page asked for fullscreen, which is refused for agents.".into());
    }
    let structured = json!({
        "tab": tab,
        "navigated": navigated,
        "openedTabs": events.opened_tabs,
        "dialogOpen": guards::dialog(tab).is_some(),
        "urlChanged": url_changed,
    });
    (lines, structured)
}

// ----------------------------------------------------------------------------------- dispatch

pub async fn run(ctx: &Ctx, tool: &str, args: &Value) -> Result<Output, ToolError> {
    macro_rules! parsed {
        ($t:ty) => {
            parse_args::<$t>(args).map_err(|e| invalid(format!("Invalid arguments for {tool}: {e}")))?
        };
    }
    match tool {
        "tabs_list" => {
            let _: Empty = parsed!(Empty);
            tabs_list(ctx)
        }
        "tab_open" => tab_open(ctx, parsed!(TabOpenArgs)).await,
        "tab_navigate" => tab_navigate(ctx, parsed!(TabNavigateArgs)).await,
        "tab_show" => tab_show(ctx, parsed!(TabArgs)).await,
        "tab_close" => tab_close(ctx, parsed!(TabArgs)).await,
        "page_snapshot" => page_snapshot(ctx, parsed!(PageSnapshotArgs)).await,
        "page_text" => page_text(ctx, parsed!(PageTextArgs)).await,
        "page_screenshot" => page_screenshot(ctx, parsed!(PageScreenshotArgs)).await,
        "click" => click(ctx, parsed!(ClickArgs)).await,
        "type" => type_text(ctx, parsed!(TypeArgs)).await,
        "press_key" => press_key(ctx, parsed!(PressKeyArgs)).await,
        "wait_for" => wait_for(ctx, parsed!(WaitForArgs)).await,
        "handle_dialog" => handle_dialog(ctx, parsed!(HandleDialogArgs)).await,
        "request_tab_access" => super::tools_browser::request_tab_access(ctx, parsed!(RequestTabAccessArgs)).await,
        "page_find" => super::tools_page::page_find(ctx, parsed!(PageFindArgs)).await,
        "hover" => super::tools_input::hover(ctx, parsed!(HoverArgs)).await,
        "select_option" => super::tools_input::select_option(ctx, parsed!(SelectOptionArgs)).await,
        "scroll" => super::tools_input::scroll(ctx, parsed!(ScrollArgs)).await,
        "fill_form" => super::tools_input::fill_form(ctx, parsed!(FillFormArgs)).await,
        "evaluate" => super::tools_page::evaluate(ctx, parsed!(EvaluateArgs)).await,
        "console_messages" => super::tools_page::console_messages(ctx, parsed!(ConsoleMessagesArgs)).await,
        "history_search" => super::tools_browser::history_search(ctx, parsed!(HistorySearchArgs)),
        "downloads_list" => super::tools_browser::downloads_list(ctx, parsed!(DownloadsListArgs)),
        other => Err(err(ErrorCode::UnknownTool, format!("Unknown tool {other:?}"))),
    }
}

// ----------------------------------------------------------------------------------- tabs

fn collect_tabs(nodes: &[NodeView], out: &mut Vec<TabView>) {
    for n in nodes {
        match n {
            NodeView::Tab(t) => out.push(t.clone()),
            NodeView::Folder(f) => collect_tabs(&f.children, out),
            NodeView::Split(s) => out.extend(s.panes.iter().cloned()),
        }
    }
}

fn tabs_list(ctx: &Ctx) -> Result<Output, ToolError> {
    let Some((ui, scope, agent_tabs, settings)) = controller::with_store(|s| (s.ui_state(), s.settings().agent_scope, s.agent_tabs().clone(), s.settings().clone())) else {
        return Err(err(ErrorCode::BrowserNotRunning, "sta is shutting down"));
    };
    let current = session::current_tab(ctx.conn);
    let mut all: Vec<(TabView, String)> = Vec::new();
    for t in &ui.favorites {
        all.push((t.clone(), "favorites".into()));
    }
    for space in &ui.spaces {
        let mut pinned = Vec::new();
        collect_tabs(&space.pinned, &mut pinned);
        let mut today = Vec::new();
        collect_tabs(&space.today, &mut today);
        all.extend(pinned.into_iter().map(|t| (t, format!("space \"{}\" pinned", space.name))));
        all.extend(today.into_iter().map(|t| (t, format!("space \"{}\" today", space.name))));
    }
    if let Some(peek) = &ui.peek {
        all.push((peek.tab.clone(), "peek".into()));
    }
    let n = nonce();
    let mut lines = Vec::new();
    let mut structured = Vec::new();
    for (t, place) in all.into_iter().filter(|(t, _)| policy::in_scope(scope, &agent_tabs, t.id)) {
        let internal = sta_core::urls::is_internal(&t.url);
        let blocked = policy::check_url(&settings, &t.url).err().is_some_and(|e| e.code == ErrorCode::SiteBlocked);
        let mut flags = Vec::new();
        if Some(t.id) == current {
            flags.push("current");
        }
        if t.visible {
            flags.push("on screen");
        }
        if !t.loaded {
            flags.push("unloaded");
        }
        if t.loading {
            flags.push("loading");
        }
        if internal {
            flags.push("sta page");
        }
        let flags = if flags.is_empty() { String::new() } else { format!(" [{}]", flags.join(", ")) };
        if blocked {
            lines.push(format!("- tab {}{flags} (hidden by the user's settings)", t.id));
        } else {
            lines.push(format!("- tab {}{flags} {} {} ({place})", t.id, text::quote(&t.title, 120), text::quote(&t.url, 300)));
        }
        structured.push(json!({ "tab": t.id, "current": Some(t.id) == current, "visible": t.visible, "loaded": t.loaded, "loading": t.loading }));
    }
    let header = if lines.is_empty() {
        "No tabs are available to you yet. Open one with tab_open (or ask the user to share a tab).".to_string()
    } else {
        format!("{} tab(s) available{}:", lines.len(), current.map(|c| format!(" (current tab: {c})")).unwrap_or_default())
    };
    let body = if lines.is_empty() { header } else { format!("{header}\n{}", text::untrusted(&n, &lines.join("\n"))) };
    Ok(Output { content: vec![Content::Text { text: body }], structured: Some(json!({ "tabs": structured, "currentTab": current })), ..Default::default() })
}

async fn tab_open(ctx: &Ctx, args: TabOpenArgs) -> Result<Output, ToolError> {
    full_access_required("tab_open")?;
    let settings = settings().unwrap_or_default();
    let url = args.url.trim().to_string();
    let site = match policy::check_url(&settings, &url)? {
        UrlVerdict::Blank => None,
        UrlVerdict::Web { site } => Some(site),
    };
    // Parse again so the tab gets a normalized URL.
    let url = if site.is_some() { url::Url::parse(&url).map(|u| u.to_string()).unwrap_or(url) } else { "about:blank".to_string() };
    if let Some(site) = &site {
        session::ensure_site(ctx.conn, ctx.session, None, site, ctx.deadline).await?;
    }
    session::take_tab_token(ctx.conn)?;
    let Some(tab) = controller::alloc_id() else { return Err(err(ErrorCode::BrowserNotRunning, "sta is shutting down")) };
    guards::mark_controlled(tab, ctx.session);
    controller::dispatch(Command::OpenAgentTab { tab, url: url.clone() });
    session::set_current_tab(ctx.conn, tab);
    let wait = args.wait_until.unwrap_or_default();
    let until = ctx.deadline.min(Instant::now() + Duration::from_millis(args.timeout_ms.unwrap_or(15_000).min(120_000)));
    // The tab exists once core applied the command.
    let created = {
        let start = Instant::now();
        loop {
            if controller::with_store(|s| s.tab(tab).is_some()).unwrap_or(false) {
                break true;
            }
            if start.elapsed() > Duration::from_secs(3) {
                break false;
            }
            wait_ms(20).await;
        }
    };
    if !created {
        return Err(err(ErrorCode::Internal, "The tab could not be opened (agents paused or access changed?)"));
    }
    let loaded = wait_loaded(tab, wait, until).await;
    let (_, error) = guards::load_info(tab);
    let status = match (&error, loaded) {
        (Some(_), _) => "failed",
        (None, true) if wait == LoadState::None => "opened",
        (None, true) => "loaded",
        (None, false) => "still loading",
    };
    let mut text = format!("Opened tab {tab} in the background ({status}). It is now your current tab.");
    if let Some((code, t)) = &error {
        text.push_str(&format!(" The load failed: {t} ({code})."));
    }
    let title = tabs::browser_for_tab(tab).map(|_| controller::with_store(|s| s.tab(tab).map(|t| t.title.clone())).flatten().unwrap_or_default()).unwrap_or_default();
    if !title.is_empty() {
        let n = nonce();
        text.push('\n');
        text.push_str(&sta_core::agent::text::untrusted(&n, &format!("title {}", text::quote(&title, 200))));
    }
    Ok(Output { content: vec![Content::Text { text }], structured: Some(json!({ "tab": tab, "status": status })), tab: Some(tab), site })
}

async fn tab_navigate(ctx: &Ctx, args: TabNavigateArgs) -> Result<Output, ToolError> {
    full_access_required("tab_navigate")?;
    let tab = pick_tab(ctx, args.tab, None)?;
    let _lock = lock_tab(tab).await?;
    let action = args.action.unwrap_or(if args.url.is_some() { NavigateAction::Url } else { NavigateAction::Reload });
    let in_scope = controller::with_store(|s| s.tab(tab).is_some().then(|| policy::in_scope(s.settings().agent_scope, s.agent_tabs(), tab))).flatten();
    match in_scope {
        None => return Err(err(ErrorCode::NoSuchTab, format!("There is no tab {tab}"))),
        Some(false) => return Err(err(ErrorCode::NotInScope, format!("Tab {tab} isn't shared with agents"))),
        Some(true) => {}
    }
    if let Some(b) = tabs::browser_for_tab(tab)
        && browsers::is_ui_browser(b.identifier())
    {
        return Err(err(ErrorCode::InternalPage, format!("Tab {tab} shows a sta page")));
    }
    if let Some((kind, message)) = guards::dialog(tab) {
        return Err(dialog_error(tab, kind, &message));
    }
    check_input_allowed(tab)?;
    let (starts_before, _) = guards::load_info(tab);
    let url_before = committed_url(tab);
    let mut site = None;
    let command = match action {
        NavigateAction::Url => {
            let url = args.url.as_deref().map(str::trim).filter(|u| !u.is_empty()).ok_or_else(|| invalid("action url needs `url`"))?;
            let settings = settings().unwrap_or_default();
            if let UrlVerdict::Web { site: s } = policy::check_url(&settings, url)? {
                session::ensure_site(ctx.conn, ctx.session, Some(tab), &s, ctx.deadline).await?;
                site = Some(s);
            }
            Command::Navigate { tab: Some(tab), url: url.to_string() }
        }
        NavigateAction::Back => Command::GoBack { tab: Some(tab) },
        NavigateAction::Forward => Command::GoForward { tab: Some(tab) },
        NavigateAction::Reload => Command::Reload { tab: Some(tab), ignore_cache: false },
    };
    guards::mark_controlled(tab, ctx.session);
    session::set_current_tab(ctx.conn, tab);
    let unloaded = tabs::browser_for_tab(tab).is_none();
    controller::dispatch(command);
    if unloaded {
        controller::dispatch(Command::LoadTab { tab });
    }
    let wait = args.wait_until.unwrap_or_default();
    let until = ctx.deadline.min(Instant::now() + Duration::from_millis(args.timeout_ms.unwrap_or(15_000).min(120_000)));
    // Wait for the navigation to start (back/forward at the end of history never start one).
    let start = Instant::now();
    while guards::load_info(tab).0 == starts_before && start.elapsed() < Duration::from_millis(2000) && Instant::now() < until {
        wait_ms(50).await;
    }
    let started = guards::load_info(tab).0 != starts_before;
    let loaded = started && wait_loaded(tab, wait, until).await;
    let (_, error) = guards::load_info(tab);
    let events = guards::take_events(tab);
    let mut lines = vec![match (started, loaded, &error) {
        (false, _, _) if !unloaded => format!("Tab {tab}: nothing happened (no {} entry, or the navigation was cancelled).", match action { NavigateAction::Back => "back", NavigateAction::Forward => "forward", _ => "history" }),
        (_, _, Some((code, t))) => format!("Tab {tab}: the load failed: {t} ({code})."),
        (_, true, None) => format!("Tab {tab} navigated and loaded."),
        _ => format!("Tab {tab} is still loading."),
    }];
    let (extra, structured) = describe_events(tab, &events, started, url_before.as_deref());
    lines.extend(extra);
    Ok(Output { content: vec![Content::Text { text: lines.join("\n") }], structured: Some(structured), tab: Some(tab), site })
}

async fn tab_show(ctx: &Ctx, args: TabArgs) -> Result<Output, ToolError> {
    full_access_required("tab_show")?;
    let tab = pick_tab(ctx, args.tab, None)?;
    let in_scope = controller::with_store(|s| s.tab(tab).is_some().then(|| policy::in_scope(s.settings().agent_scope, s.agent_tabs(), tab))).flatten();
    match in_scope {
        None => return Err(err(ErrorCode::NoSuchTab, format!("There is no tab {tab}"))),
        Some(false) => return Err(err(ErrorCode::NotInScope, format!("Tab {tab} isn't shared with agents"))),
        Some(true) => {}
    }
    super::ui::note_agent_show(tab);
    controller::dispatch(Command::ShowAgentTab { tab });
    session::set_current_tab(ctx.conn, tab);
    let start = Instant::now();
    while !tabs::is_tab_visible(tab) && start.elapsed() < Duration::from_secs(3) && Instant::now() < ctx.deadline {
        wait_ms(50).await;
    }
    if !tabs::is_tab_visible(tab) {
        return Err(err(ErrorCode::Timeout, format!("Tab {tab} did not come on screen")));
    }
    let mut text = format!("Tab {tab} is on screen.");
    if window::is_minimized() {
        text.push_str(" The sta window is minimized, so screenshots and clicks still won't work until the user restores it.");
    }
    Ok(Output { content: vec![Content::Text { text }], structured: Some(json!({ "tab": tab, "visible": true })), tab: Some(tab), site: None })
}

async fn tab_close(ctx: &Ctx, args: TabArgs) -> Result<Output, ToolError> {
    full_access_required("tab_close")?;
    let tab = pick_tab(ctx, args.tab, None)?;
    let Some((exists, opened)) = controller::with_store(|s| (s.tab(tab).is_some(), s.agent_opened_tab(tab))) else {
        return Err(err(ErrorCode::BrowserNotRunning, "sta is shutting down"));
    };
    if !exists {
        return Err(err(ErrorCode::NoSuchTab, format!("There is no tab {tab}")));
    }
    if !opened {
        return Err(err(ErrorCode::NotInScope, format!("Tab {tab} wasn't opened by an agent")).with_hint("Agents can only close tabs they opened."));
    }
    controller::dispatch(Command::CloseItem { id: Some(tab) });
    if session::current_tab(ctx.conn) == Some(tab) {
        session::clear_current_tab(ctx.conn);
    }
    guards::forget_tab(tab);
    page::forget_tab(tab);
    super::console::forget_tab(tab);
    Ok(Output { content: vec![Content::Text { text: format!("Closed tab {tab} (it is in sta's archive).") }], structured: Some(json!({ "tab": tab, "closed": true })), tab: Some(tab), site: None })
}

// ----------------------------------------------------------------------------------- page reads

async fn page_snapshot(ctx: &Ctx, args: PageSnapshotArgs) -> Result<Output, ToolError> {
    let tab = pick_tab(ctx, args.tab, args.root.as_deref())?;
    let _lock = lock_tab(tab).await?;
    let t = target(ctx, tab, Want::default()).await?;
    let frame = page::main_frame(t.browser, ctx.deadline).await?;
    let root = match &args.root {
        Some(r) => Some(page::resolve_ref(tab, t.browser, &frame, r)?),
        None => None,
    };
    page::note_ax_use(t.browser);
    let ax = super::cdp::call(t.browser, p::GetFullAxTree { depth: Some(100) }, page::left(ctx.deadline, 15_000)).await.map_err(page::cdp_error)?;
    let nodes = snapshot::parse_nodes(&ax);
    let options = snapshot::Options { interactive_only: args.interactive_only.unwrap_or(true), max_tokens: text::budget(args.max_tokens), root_backend_node_id: root };
    let snap = page::with_refs(tab, t.browser, &frame, |table| snapshot::build(&nodes, &options, &mut |b| Some(table.ref_for(b, ()).to_string())));
    if snap.root_missing {
        return Err(err(ErrorCode::StaleRef, "The root element isn't in the page any more"));
    }
    let title = page::eval_document(t.browser, &frame, "function(){ return this.title; }", vec![], ctx.deadline, 2_000).await.ok().and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default();
    let n = nonce();
    let body = format!("page {} {}\n{}", text::quote(&title, 200), text::quote(&t.url, 300), if snap.text.is_empty() { "(no interactive elements; try interactiveOnly: false or page_text)".to_string() } else { snap.text.clone() });
    let mut out = format!("Snapshot of tab {tab} ({} entries, {} refs). Use the refs with click, type and press_key.\n{}", snap.entries, snap.refs, text::untrusted(&n, &body));
    if snap.truncated {
        out.push_str("\n[Truncated at maxTokens: pass root=<ref> of a section, interactiveOnly: true, or a larger maxTokens.]");
    }
    Ok(Output { content: vec![Content::Text { text: out }], structured: Some(json!({ "tab": tab, "entries": snap.entries, "refs": snap.refs, "truncated": snap.truncated })), tab: Some(tab), site: t.site })
}

async fn page_text(ctx: &Ctx, args: PageTextArgs) -> Result<Output, ToolError> {
    let tab = pick_tab(ctx, args.tab, args.element.as_deref())?;
    let _lock = lock_tab(tab).await?;
    let t = target(ctx, tab, Want::default()).await?;
    let frame = page::main_frame(t.browser, ctx.deadline).await?;
    let format = if args.format == Some(TextFormat::Markdown) { "markdown" } else { "text" };
    let value = match &args.element {
        Some(r) => {
            let backend = page::resolve_ref(tab, t.browser, &frame, r)?;
            page::eval_node(t.browser, &frame, backend, page::EXTRACT_TEXT, vec![json!(format)], ctx.deadline).await?
        }
        None => page::eval_document(t.browser, &frame, page::EXTRACT_TEXT, vec![json!(format)], ctx.deadline, 10_000).await?,
    };
    let full = value.as_str().unwrap_or_default();
    let offset = args.offset.unwrap_or(0) as usize;
    let (slice, next, total) = text::page(full, offset, text::budget(args.max_tokens));
    let n = nonce();
    let end = offset.min(total) + slice.chars().count();
    let mut out = format!("Text of tab {tab} ({format}), characters {}–{end} of {total}.\n{}", offset.min(total), text::untrusted(&n, &slice));
    if let Some(next) = next {
        out.push_str(&format!("\nMore text follows: call page_text with offset: {next}."));
    }
    Ok(Output { content: vec![Content::Text { text: out }], structured: Some(json!({ "tab": tab, "offset": offset.min(total), "nextOffset": next, "totalChars": total })), tab: Some(tab), site: t.site })
}

/// Width and height of a base64 PNG or JPEG.
fn image_size(b64: &str) -> Option<(u32, u32)> {
    use base64_decode as dec;
    let head = dec(&b64[..b64.len().min(64_000)])?;
    if head.starts_with(&[0x89, b'P', b'N', b'G']) && head.len() >= 24 {
        return Some((u32::from_be_bytes(head[16..20].try_into().ok()?), u32::from_be_bytes(head[20..24].try_into().ok()?)));
    }
    let mut i = 2;
    while i + 9 < head.len() {
        if head[i] != 0xFF {
            i += 1;
            continue;
        }
        let marker = head[i + 1];
        let len = u16::from_be_bytes([head[i + 2], head[i + 3]]) as usize;
        if matches!(marker, 0xC0..=0xC2) {
            let h = u16::from_be_bytes([head[i + 5], head[i + 6]]) as u32;
            let w = u16::from_be_bytes([head[i + 7], head[i + 8]]) as u32;
            return Some((w, h));
        }
        i += 2 + len;
    }
    None
}

/// Minimal base64 decoder for image headers.
fn base64_decode(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let mut buf = 0u32;
    let mut bits = 0;
    for c in s.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => break,
            _ => return None,
        } as u32;
        buf = (buf << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
        }
    }
    Some(out)
}

async fn page_screenshot(ctx: &Ctx, args: PageScreenshotArgs) -> Result<Output, ToolError> {
    let full_page = args.full_page.unwrap_or(false);
    if full_page && args.element.is_some() {
        return Err(invalid("fullPage and ref can't be combined"));
    }
    let tab = pick_tab(ctx, args.tab, args.element.as_deref())?;
    let _lock = lock_tab(tab).await?;
    let t = target(ctx, tab, Want::default()).await?;
    // A full-page capture briefly resizes the page's view: only in tabs agents opened.
    if full_page && !controller::with_store(|s| s.agent_opened_tab(tab)).unwrap_or(false) {
        return Err(err(ErrorCode::NotInScope, format!("fullPage is only available in tabs agents opened, not in tab {tab}")).with_hint("Omit fullPage (the visible part), or scroll and take several screenshots."));
    }
    check_visible(tab)?;
    session::take_screenshot_token(ctx.conn)?;
    let frame = page::main_frame(t.browser, ctx.deadline).await?;
    let state = page_state(&t, &frame, ctx.deadline).await?;
    let (vw, vh, dpr) = (state["width"].as_f64().unwrap_or(0.0), state["height"].as_f64().unwrap_or(0.0), state["dpr"].as_f64().unwrap_or(1.0).max(0.1));
    if state["visibility"].as_str() != Some("visible") || vw < 1.0 || vh < 1.0 {
        return Err(err(ErrorCode::TabNotVisible, format!("Tab {tab} isn't rendering (hidden or minimized)")));
    }
    let clip = match &args.element {
        Some(r) => {
            let backend = page::resolve_ref(tab, t.browser, &frame, r)?;
            let _ = page::call(t.browser, p::ScrollIntoViewIfNeeded { backend_node_id: backend }, ctx.deadline).await;
            let b = page::eval_node(t.browser, &frame, backend, page::BOUNDS, vec![], ctx.deadline).await?;
            let (x, y, w, h) = (b["x"].as_f64().unwrap_or(0.0), b["y"].as_f64().unwrap_or(0.0), b["width"].as_f64().unwrap_or(0.0), b["height"].as_f64().unwrap_or(0.0));
            let (x0, y0) = (x.max(0.0), y.max(0.0));
            let (x1, y1) = ((x + w).min(vw), (y + h).min(vh));
            if x1 - x0 < 1.0 || y1 - y0 < 1.0 {
                return Err(err(ErrorCode::ElementNotFound, "The element has no visible area"));
            }
            (x0, y0, x1 - x0, y1 - y0)
        }
        None if full_page => {
            let size = page::eval_document(t.browser, &frame, page::PAGE_SIZE, vec![], ctx.deadline, page::CALL_MS).await?;
            let (w, h) = (size["width"].as_f64().unwrap_or(vw).max(1.0), size["height"].as_f64().unwrap_or(vh).max(1.0));
            (0.0, 0.0, w.min(MAX_FULL_PAGE_CSS), h.min(MAX_FULL_PAGE_CSS))
        }
        None => (0.0, 0.0, vw, vh),
    };
    let cut = full_page && clip.3 >= MAX_FULL_PAGE_CSS;
    let max_dim = args.max_dimension.unwrap_or(1568).clamp(64, 1568) as f64;
    let scale = (max_dim / (clip.2.max(clip.3) * dpr)).min(1.0);
    let format = args.format.unwrap_or_default();
    let (fmt, mime) = match format {
        ImageFormat::Jpeg => ("jpeg", "image/jpeg"),
        ImageFormat::Png => ("png", "image/png"),
    };
    let mut quality = (format == ImageFormat::Jpeg).then_some(75);
    let data = loop {
        let v = super::cdp::call(
            t.browser,
            p::CaptureScreenshot { format: fmt, quality, clip: Some(p::Clip { x: clip.0, y: clip.1, width: clip.2, height: clip.3, scale }), from_surface: true, capture_beyond_viewport: full_page },
            page::left(ctx.deadline, if full_page { 15_000 } else { 6_000 }),
        )
        .await
        .map_err(|e| match e {
            super::cdp::CdpError::Timeout => err(ErrorCode::TabNotVisible, format!("Tab {tab} produced no frame (it isn't rendering)")),
            other => page::cdp_error(other),
        })?;
        let data = v["data"].as_str().unwrap_or_default().to_string();
        if data.len() <= MAX_IMAGE_BASE64 {
            break data;
        }
        match quality {
            Some(q) if q > 40 => quality = Some(40),
            _ => return Err(err(ErrorCode::TooLarge, "The screenshot is too large").with_hint("Use format jpeg, a ref, or a smaller maxDimension.")),
        }
    };
    let (w, h) = image_size(&data).unwrap_or(((clip.2 * dpr * scale) as u32, (clip.3 * dpr * scale) as u32));
    // A long page shrinks to a strip whose text can't be read.
    let unreadable = full_page && scale * dpr < MIN_READABLE_SCALE;
    let mut text = if full_page {
        format!("Full-page screenshot of tab {tab} ({w}×{h} px for {:.0}×{:.0} CSS px{}).", clip.2, clip.3, if cut { ", cut at the maximum height" } else { "" })
    } else {
        format!("Screenshot of tab {tab} ({w}×{h} px).")
    };
    if unreadable {
        text.push_str(&format!(" The page is scaled to {:.2} of its size, so its text is too small to read: use page_text, or scroll and take viewport screenshots.", scale * dpr));
    }
    Ok(Output {
        content: vec![Content::Text { text }, Content::Image { mime_type: mime.into(), data }],
        structured: Some(json!({ "tab": tab, "width": w, "height": h, "cssScale": scale * dpr, "fullPage": full_page, "cut": cut, "readable": !unreadable })),
        tab: Some(tab),
        site: t.site,
    })
}

// ----------------------------------------------------------------------------------- input

fn center_of_quads(quads: &Value) -> Option<(f64, f64)> {
    for q in quads.as_array()? {
        let v: Vec<f64> = q.as_array()?.iter().filter_map(Value::as_f64).collect();
        if v.len() != 8 {
            continue;
        }
        let (xs, ys) = ([v[0], v[2], v[4], v[6]], [v[1], v[3], v[5], v[7]]);
        let w = xs.iter().cloned().fold(f64::MIN, f64::max) - xs.iter().cloned().fold(f64::MAX, f64::min);
        let h = ys.iter().cloned().fold(f64::MIN, f64::max) - ys.iter().cloned().fold(f64::MAX, f64::min);
        if w >= 1.0 && h >= 1.0 {
            return Some((xs.iter().sum::<f64>() / 4.0, ys.iter().sum::<f64>() / 4.0));
        }
    }
    None
}

pub(super) async fn ensure_same_document(t: &Target, frame: &Frame, deadline: Instant) -> Result<(), ToolError> {
    let now = page::main_frame(t.browser, deadline).await?;
    if now.loader_id != frame.loader_id {
        return Err(err(ErrorCode::StaleRef, "The page navigated before the input could be sent"));
    }
    Ok(())
}

pub(super) async fn mouse(t: &Target, kind: &'static str, x: f64, y: f64, button: &'static str, click_count: i32, deadline: Instant) -> Result<bool, ToolError> {
    let buttons = match (kind, button) {
        ("mousePressed", "left") => 1,
        ("mousePressed", "right") => 2,
        ("mousePressed", "middle") => 4,
        _ => 0,
    };
    let b = if kind == "mouseMoved" { "none" } else { button };
    match super::cdp::call(t.browser, p::DispatchMouseEvent { kind, x, y, button: b, buttons, click_count, modifiers: 0 }, page::left(deadline, 3_000)).await {
        Ok(_) => Ok(true),
        // A dialog opened by the click blocks the event's acknowledgement.
        Err(super::cdp::CdpError::Timeout) if guards::dialog(t.tab).is_some() => Ok(false),
        Err(e) => Err(page::cdp_error(e)),
    }
}

/// The viewport point for mouse input on ref `r`: scrolled into view, the center of its first
/// content quad, inside the viewport, and not covered by another element (`element_obscured`).
pub(super) async fn element_point(t: &Target, frame: &Frame, r: &str, deadline: Instant) -> Result<(f64, f64), ToolError> {
    let backend = page::resolve_ref(t.tab, t.browser, frame, r)?;
    page::call(t.browser, p::ScrollIntoViewIfNeeded { backend_node_id: backend }, deadline).await.map_err(|_| err(ErrorCode::ElementNotFound, format!("Ref {r} can't be scrolled into view (not rendered?)")))?;
    let quads = page::call(t.browser, p::GetContentQuads { backend_node_id: backend }, deadline).await.map_err(|_| err(ErrorCode::ElementNotFound, format!("Ref {r} has no layout (hidden?)")))?;
    let (x, y) = center_of_quads(&quads["quads"]).ok_or_else(|| err(ErrorCode::ElementNotFound, format!("Ref {r} has no visible area")))?;
    let state = page_state(t, frame, deadline).await?;
    let (vw, vh) = (state["width"].as_f64().unwrap_or(0.0), state["height"].as_f64().unwrap_or(0.0));
    if x < 0.0 || y < 0.0 || x >= vw || y >= vh {
        return Err(err(ErrorCode::ElementObscured, format!("Ref {r} is outside the visible part of the page")));
    }
    let hit = page::call(t.browser, p::GetNodeForLocation { x: x.round() as i64, y: y.round() as i64, include_user_agent_shadow_dom: false, ignore_pointer_events_none: true }, deadline).await;
    let hit_backend = hit.ok().and_then(|v| v["backendNodeId"].as_i64());
    let inside = match hit_backend {
        Some(h) if h == backend => true,
        Some(h) => page::eval_pair(t.browser, frame, backend, h, page::CONTAINS, deadline).await.map(|v| v.as_bool().unwrap_or(false)).unwrap_or(false),
        None => false,
    };
    if !inside {
        let cover = match hit_backend {
            Some(h) => page::eval_node(t.browser, frame, h, "function(){ return this.tagName ? this.tagName.toLowerCase() : 'node'; }", vec![], deadline).await.ok().and_then(|v| v.as_str().map(str::to_string)),
            None => None,
        };
        let cover: String = cover.unwrap_or_else(|| "something".into()).chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-').take(30).collect();
        return Err(err(ErrorCode::ElementObscured, format!("Ref {r} is covered by a <{cover}> element at its center")));
    }
    Ok((x, y))
}

async fn click(ctx: &Ctx, args: ClickArgs) -> Result<Output, ToolError> {
    full_access_required("click")?;
    let tab = pick_tab(ctx, args.tab, args.element.as_deref())?;
    let _lock = lock_tab(tab).await?;
    let t = target(ctx, tab, Want::default()).await?;
    check_visible(tab)?;
    check_input_allowed(tab)?;
    session::take_action_token(ctx.conn)?;
    let frame = page::main_frame(t.browser, ctx.deadline).await?;
    let (x, y, what) = match (&args.element, args.x, args.y) {
        (Some(r), None, None) => {
            let (x, y) = element_point(&t, &frame, r, ctx.deadline).await?;
            (x, y, format!("ref {r}"))
        }
        (None, Some(x), Some(y)) => {
            if !x.is_finite() || !y.is_finite() {
                return Err(invalid("x and y must be numbers"));
            }
            (x, y, format!("point ({x:.0}, {y:.0})"))
        }
        _ => return Err(invalid("Give either `ref` or both `x` and `y`")),
    };
    ensure_same_document(&t, &frame, ctx.deadline).await?;
    guards::mark_controlled(tab, ctx.session);
    let _ = guards::take_events(tab);
    let (starts_before, _) = guards::load_info(tab);
    let button = match args.button.unwrap_or_default() {
        MouseButton::Left => "left",
        MouseButton::Right => "right",
        MouseButton::Middle => "middle",
    };
    let clicks = if args.double_click.unwrap_or(false) { 2 } else { 1 };
    let mut delivered = mouse(&t, "mouseMoved", x, y, button, 0, ctx.deadline).await?;
    for count in 1..=clicks {
        if !delivered {
            break;
        }
        delivered = mouse(&t, "mousePressed", x, y, button, count, ctx.deadline).await? && mouse(&t, "mouseReleased", x, y, button, count, ctx.deadline).await?;
    }
    let navigated = settle(&t, starts_before, ctx.deadline).await;
    let events = guards::take_events(tab);
    if events.file_chooser_blocked {
        return Err(err(ErrorCode::FileChooserBlocked, format!("Clicking {what} opened a file chooser, which was cancelled")));
    }
    let mut lines = vec![format!("Clicked {what} in tab {tab}.")];
    let (extra, structured) = describe_events(tab, &events, navigated, Some(&t.url));
    lines.extend(extra);
    Ok(Output { content: vec![Content::Text { text: lines.join("\n") }], structured: Some(structured), tab: Some(tab), site: t.site })
}

/// Waits until key events reach the page (`page::KEY_READY_IN`: at most about 750 ms after a
/// navigation of a tab that hasn't painted). Call before the first `key_press` of a tool.
pub(super) async fn wait_keyboard_ready(t: &Target, frame: &Frame, deadline: Instant) -> Result<(), ToolError> {
    let wait = page::eval_document(t.browser, frame, page::KEY_READY_IN, vec![], deadline, page::CALL_MS).await?;
    let ms = wait.as_f64().unwrap_or(0.0).clamp(0.0, 1_000.0) as u64;
    if ms > 0 {
        wait_ms(ms.min(page::left(deadline, 1_000).max(0) as u64)).await;
    }
    Ok(())
}

pub(super) async fn key_press(t: &Target, key: &keys::KeyPress, deadline: Instant) -> Result<(), ToolError> {
    let down = if key.text.is_some() { "keyDown" } else { "rawKeyDown" };
    for kind in [down, "keyUp"] {
        let text = if kind == "keyDown" { key.text.clone() } else { None };
        let r = super::cdp::call(
            t.browser,
            p::DispatchKeyEvent { kind, modifiers: key.modifiers, key: key.key.clone(), code: key.code.clone(), windows_virtual_key_code: key.windows_virtual_key_code, unmodified_text: text.clone(), text },
            page::left(deadline, 3_000),
        )
        .await;
        match r {
            Ok(_) => {}
            Err(super::cdp::CdpError::Timeout) if guards::dialog(t.tab).is_some() => return Ok(()),
            Err(e) => return Err(page::cdp_error(e)),
        }
    }
    Ok(())
}

pub(super) async fn focus_ref(t: &Target, frame: &Frame, backend: i64, deadline: Instant) -> Result<(), ToolError> {
    page::call(t.browser, p::Focus { backend_node_id: backend }, deadline).await.map_err(|_| err(ErrorCode::ElementNotFound, "The element can't take focus"))?;
    let focused = page::eval_node(t.browser, frame, backend, page::IS_FOCUSED, vec![], deadline).await?;
    if focused.as_bool() != Some(true) {
        return Err(err(ErrorCode::FocusLost, "The element didn't keep the focus"));
    }
    Ok(())
}

/// Types `text` into the editable element `backend` (ref `r`): checks it is a text field, marks the
/// tab agent-controlled, focuses it (`focus_lost` if the page moves focus), replaces its content
/// with `clear`, inserts the text (key by key with `slowly`) and checks it arrived. The text is
/// never echoed.
#[allow(clippy::too_many_arguments)]
pub(super) async fn fill_text(ctx: &Ctx, t: &Target, frame: &Frame, r: &str, backend: i64, text: &str, clear: bool, slowly: bool) -> Result<(), ToolError> {
    let info = page::eval_node(t.browser, frame, backend, page::EDITABLE, vec![], ctx.deadline).await?;
    let kind = info["kind"].as_str().unwrap_or_default().to_string();
    if kind == "input-file" {
        return Err(err(ErrorCode::FileChooserBlocked, "That is a file input; agents can't choose files"));
    }
    if info["ok"].as_bool() != Some(true) {
        return Err(invalid(format!("Ref {r} is not a text field (it is a {kind})")).with_hint("Use click for buttons, select_option for selects, fill_form for checkboxes; type needs a textbox."));
    }
    if info["disabled"].as_bool() == Some(true) {
        return Err(invalid(format!("Ref {r} is disabled or read-only")));
    }
    let before = info["value"].as_str().unwrap_or_default().to_string();
    guards::mark_controlled(t.tab, ctx.session);
    focus_ref(t, frame, backend, ctx.deadline).await?;
    if clear && !before.is_empty() {
        page::eval_node(t.browser, frame, backend, page::SELECT_ALL, vec![], ctx.deadline).await?;
        if text.is_empty() {
            let delete = keys::parse("Delete").map_err(invalid)?;
            wait_keyboard_ready(t, frame, ctx.deadline).await?;
            key_press(t, &delete, ctx.deadline).await?;
        }
    }
    ensure_same_document(t, frame, ctx.deadline).await?;
    if slowly {
        wait_keyboard_ready(t, frame, ctx.deadline).await?;
        for c in text.chars().take(2_000) {
            let press = keys::KeyPress { key: c.to_string(), code: String::new(), windows_virtual_key_code: 0, text: Some(if c == '\n' { "\r".into() } else { c.to_string() }), modifiers: 0 };
            key_press(t, &press, ctx.deadline).await?;
        }
    } else if !text.is_empty() {
        page::call(t.browser, p::InsertText { text: text.to_string() }, ctx.deadline).await?;
    }
    let after = page::eval_node(t.browser, frame, backend, page::VALUE_OF, vec![], ctx.deadline).await.ok().and_then(|v| v.as_str().map(str::to_string));
    if !text.is_empty() && after.as_deref() == Some(before.as_str()) && !clear {
        return Err(err(ErrorCode::FocusLost, "The text didn't reach the element (the page moved focus or rejected the input)"));
    }
    Ok(())
}

async fn type_text(ctx: &Ctx, args: TypeArgs) -> Result<Output, ToolError> {
    full_access_required("type")?;
    if args.text.chars().count() > 10_000 {
        return Err(invalid("text is longer than 10000 characters"));
    }
    let tab = pick_tab(ctx, args.tab, Some(&args.element))?;
    let _lock = lock_tab(tab).await?;
    let t = target(ctx, tab, Want::default()).await?;
    check_input_allowed(tab)?;
    session::take_action_token(ctx.conn)?;
    let frame = page::main_frame(t.browser, ctx.deadline).await?;
    let backend = page::resolve_ref(tab, t.browser, &frame, &args.element)?;
    let _ = guards::take_events(tab);
    fill_text(ctx, &t, &frame, &args.element, backend, &args.text, args.clear.unwrap_or(false), args.slowly.unwrap_or(false)).await?;
    let (starts_before, _) = guards::load_info(tab);
    let mut navigated = false;
    if args.submit.unwrap_or(false) {
        let enter = keys::parse("Enter").map_err(invalid)?;
        wait_keyboard_ready(&t, &frame, ctx.deadline).await?;
        key_press(&t, &enter, ctx.deadline).await?;
        navigated = settle(&t, starts_before, ctx.deadline).await;
    }
    let events = guards::take_events(tab);
    let count = args.text.chars().count();
    let mut lines = vec![format!(
        "Typed {count} character{} into ref {} in tab {tab}{}{}.",
        if count == 1 { "" } else { "s" },
        args.element,
        if args.clear.unwrap_or(false) { " (replacing its content)" } else { "" },
        if args.submit.unwrap_or(false) { " and pressed Enter" } else { "" }
    )];
    let (extra, structured) = describe_events(tab, &events, navigated, Some(&t.url));
    lines.extend(extra);
    Ok(Output { content: vec![Content::Text { text: lines.join("\n") }], structured: Some(structured), tab: Some(tab), site: t.site })
}

async fn press_key(ctx: &Ctx, args: PressKeyArgs) -> Result<Output, ToolError> {
    full_access_required("press_key")?;
    let key = keys::parse(&args.key).map_err(|e| invalid(format!("key: {e}")))?;
    let tab = pick_tab(ctx, args.tab, args.element.as_deref())?;
    let _lock = lock_tab(tab).await?;
    let t = target(ctx, tab, Want::default()).await?;
    check_input_allowed(tab)?;
    session::take_action_token(ctx.conn)?;
    let frame = page::main_frame(t.browser, ctx.deadline).await?;
    guards::mark_controlled(tab, ctx.session);
    if let Some(r) = &args.element {
        let backend = page::resolve_ref(tab, t.browser, &frame, r)?;
        focus_ref(&t, &frame, backend, ctx.deadline).await?;
    }
    ensure_same_document(&t, &frame, ctx.deadline).await?;
    let _ = guards::take_events(tab);
    let (starts_before, _) = guards::load_info(tab);
    wait_keyboard_ready(&t, &frame, ctx.deadline).await?;
    key_press(&t, &key, ctx.deadline).await?;
    let navigated = settle(&t, starts_before, ctx.deadline).await;
    let events = guards::take_events(tab);
    let mut lines = vec![format!("Pressed {} in tab {tab}.", args.key.trim())];
    let (extra, structured) = describe_events(tab, &events, navigated, Some(&t.url));
    lines.extend(extra);
    Ok(Output { content: vec![Content::Text { text: lines.join("\n") }], structured: Some(structured), tab: Some(tab), site: t.site })
}

async fn wait_for(ctx: &Ctx, args: WaitForArgs) -> Result<Output, ToolError> {
    let conditions = [args.text.is_some(), args.text_gone.is_some(), args.selector.is_some(), args.url_matches.is_some(), args.load_state.is_some(), args.time_ms.is_some()].iter().filter(|x| **x).count();
    if conditions != 1 {
        return Err(invalid("Give exactly one of text, textGone, selector, urlMatches, loadState, timeMs"));
    }
    let start = Instant::now();
    if let Some(ms) = args.time_ms {
        let ms = ms.min(60_000);
        let until = ctx.deadline.min(start + Duration::from_millis(ms));
        while Instant::now() < until {
            wait_ms(until.saturating_duration_since(Instant::now()).as_millis().min(250) as u64).await;
        }
        return Ok(text_output(format!("Waited {} ms.", start.elapsed().as_millis())));
    }
    let regex = match &args.url_matches {
        Some(p) => Some(sta_core::agent::text::url_regex(p).map_err(|e| invalid(format!("urlMatches: {e}")))?),
        None => None,
    };
    let tab = pick_tab(ctx, args.tab, None)?;
    let until = ctx.deadline.min(start + Duration::from_millis(args.timeout_ms.unwrap_or(10_000).min(120_000)));
    loop {
        let t = target(ctx, tab, Want::default()).await?;
        let met = if let Some(re) = &regex {
            re.is_match(&t.url)
        } else if let Some(state) = args.load_state {
            wait_loaded(tab, state, Instant::now() + Duration::from_millis(50)).await && {
                let frame = page::main_frame(t.browser, ctx.deadline).await?;
                let ready = page::eval_document(t.browser, &frame, "function(){ return this.readyState; }", vec![], ctx.deadline, 2_000).await?;
                match state {
                    LoadState::Load => ready.as_str() == Some("complete"),
                    _ => ready.as_str() != Some("loading"),
                }
            }
        } else {
            let frame = page::main_frame(t.browser, ctx.deadline).await?;
            if let Some(s) = &args.selector {
                let v = page::eval_document(t.browser, &frame, page::HAS_SELECTOR, vec![json!(s)], ctx.deadline, 2_000).await?;
                if v.as_str() == Some("invalid") {
                    return Err(invalid("selector is not a valid CSS selector"));
                }
                v.as_bool() == Some(true)
            } else {
                let (needle, gone) = match (&args.text, &args.text_gone) {
                    (Some(t), _) => (t.clone(), false),
                    (_, Some(t)) => (t.clone(), true),
                    _ => unreachable!("one condition"),
                };
                let v = page::eval_document(t.browser, &frame, page::HAS_TEXT, vec![json!(needle)], ctx.deadline, 3_000).await?;
                (v.as_bool() == Some(true)) != gone
            }
        };
        if met {
            return Ok(Output {
                content: vec![Content::Text { text: format!("Condition met in tab {tab} after {} ms.", start.elapsed().as_millis()) }],
                structured: Some(json!({ "tab": tab, "met": true, "elapsedMs": start.elapsed().as_millis() as u64 })),
                tab: Some(tab),
                site: t.site,
            });
        }
        if Instant::now() >= until {
            return Err(err(ErrorCode::Timeout, format!("The condition wasn't met in tab {tab} within {} ms", start.elapsed().as_millis())));
        }
        wait_ms(250).await;
    }
}

async fn handle_dialog(ctx: &Ctx, args: HandleDialogArgs) -> Result<Output, ToolError> {
    full_access_required("handle_dialog")?;
    let tab = pick_tab(ctx, args.tab, None)?;
    let t = target(ctx, tab, Want { dialog_ok: true }).await?;
    match guards::answer_dialog(tab, args.accept, args.prompt_text.as_deref()) {
        Some(kind) => {
            // Let the page continue before the next call.
            wait_ms(100).await;
            Ok(Output {
                content: vec![Content::Text { text: format!("{} the {kind} dialog in tab {tab}.", if args.accept { "Accepted" } else { "Dismissed" }) }],
                structured: Some(json!({ "tab": tab, "dialog": kind, "accepted": args.accept })),
                tab: Some(tab),
                site: t.site,
            })
        }
        None => Err(err(ErrorCode::ElementNotFound, format!("No JavaScript dialog is open in tab {tab}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quad_centers() {
        assert_eq!(center_of_quads(&json!([[10, 10, 30, 10, 30, 20, 10, 20]])), Some((20.0, 15.0)));
        assert_eq!(center_of_quads(&json!([[0, 0, 0, 0, 0, 0, 0, 0], [0, 0, 4, 0, 4, 4, 0, 4]])), Some((2.0, 2.0)));
        assert_eq!(center_of_quads(&json!([])), None);
    }

    #[test]
    fn image_sizes() {
        // 1×1 PNG.
        let png = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg==";
        assert_eq!(image_size(png), Some((1, 1)));
        assert_eq!(base64_decode("aGk="), Some(b"hi".to_vec()));
        assert_eq!(image_size("!!!"), None);
    }

    fn poll_once<F: std::future::Future>(f: std::pin::Pin<&mut F>) -> std::task::Poll<F::Output> {
        std::future::Future::poll(f, &mut std::task::Context::from_waker(std::task::Waker::noop()))
    }

    #[test]
    fn dropped_waiters_never_wedge_a_tab() {
        use std::task::Poll;
        let tab = 90_001;
        let Poll::Ready(Ok(holder)) = poll_once(Box::pin(lock_tab(tab)).as_mut()) else { panic!("a free tab locks at once") };
        let mut cancelled = Box::pin(lock_tab(tab));
        assert!(poll_once(cancelled.as_mut()).is_pending());
        let mut next = Box::pin(lock_tab(tab));
        assert!(poll_once(next.as_mut()).is_pending());
        // A queued call is cancelled (its future dropped) before the holder finishes.
        drop(cancelled);
        drop(holder);
        let Poll::Ready(Ok(second)) = poll_once(next.as_mut()) else { panic!("the next waiter gets the tab") };
        // A waiter that was handed the tab but dropped before it ran passes the tab on.
        let mut woken_then_dropped = Box::pin(lock_tab(tab));
        assert!(poll_once(woken_then_dropped.as_mut()).is_pending());
        let mut last = Box::pin(lock_tab(tab));
        assert!(poll_once(last.as_mut()).is_pending());
        drop(second);
        drop(woken_then_dropped);
        let Poll::Ready(Ok(third)) = poll_once(last.as_mut()) else { panic!("the tab was passed on") };
        drop(third);
        assert!(QUEUES.with(|q| q.borrow().get(&tab).is_none()), "the tab is free and nothing waits");
        assert!(matches!(poll_once(Box::pin(lock_tab(tab)).as_mut()), Poll::Ready(Ok(_))));
        assert!(QUEUES.with(|q| q.borrow().get(&tab).is_none()));
    }
}
