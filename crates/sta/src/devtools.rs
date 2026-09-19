//! Docked DevTools [owner: tabs] (ext design FINAL PLAN §3; docs/research/devtools.md approach B).
//!
//! A tab's DevTools frontend runs in an **Alloy BrowserView inside that tab's wrapper**, and the
//! page sits *on top of it* at the rect the frontend asks for — Chrome's own docked model, in one
//! window, so every sta overlay (command bar, find bar, toasts, Peek, the rounded corner masks)
//! keeps working above it:
//!
//! ```text
//! wrapper (BoxLayout, 2 px accent inset)
//!   stack     Panel, CEF's default fill layout: both children fill it
//!     frontend  BrowserView  devtools://devtools/bundled/devtools_app.html?can_dock=true
//!     page host Panel, a BoxLayout whose *insets* place the page inside it (on top)
//!       page    BrowserView  the tab's own browser, at `setInspectedPageBounds`
//! ```
//!
//! **Why a page host with insets and not absolute bounds** (deviation from the plan's "a stack
//! Panel without layout manager", `gates-p2.md` S11): a fresh `CefPanel` always *has* a layout
//! manager (`GetLayout()` is non-null and CEF exposes no way to clear it), and its fill layout
//! resets every child to the panel's bounds on each layout pass. Writing the page's bounds back
//! invalidated the layout again, which measured as a ~560 Hz layout loop: 825 `Page.frameResized`
//! and 1 662 `CSS.mediaQueryResultChanged` events per second through the bridge, enough to starve
//! the UI thread. A **BoxLayout's `inside_border_insets`** places a single child at an arbitrary
//! rect and is itself what the layout manager computes, so nothing fights it. The empty host above
//! the frontend does not swallow mouse input — each BrowserView owns an aura window, and aura
//! targets the topmost *window* under the pointer (measured: a click over the DevTools UI reaches
//! the frontend, a click in the page reaches the page).
//!
//! - **Transport** (S11): no socket. The renderer shim (`devtools_shim.rs`) routes the frontend's
//!   `sendMessageToBackend` to the browser as a process message; `devtools_cdp.rs` relays it onto a
//!   child session S of the inspected browser's in-process DevTools session and sends replies back
//!   as renderer messages, chunked, which the shim hands to `DevToolsAPI.dispatchMessage`.
//! - **Undocked** DevTools are CEF's own window (today's `show_dev_tools` path): the frontend's
//!   Undock button, and DevTools on a Peek page.
//! - **Keys**: F12 toggles, Ctrl+Shift+I opens/focuses/closes (keyboard.rs resolves the third step),
//!   the frontend's whitelisted debugger keys are forwarded from the focused page (F13/UX5), and
//!   plain F11 reaches a focused frontend instead of toggling fullscreen (D9).
//! - **Inspect**: `DOM.getNodeForLocation` on S (page zoom divided out), then
//!   `Overlay.inspectNodeRequested` injected into the frontend; an iframe owner is re-resolved in
//!   that frame's nested session.
//!
//! Nothing here is persisted: core owns *whether* DevTools are open (`sta-core/src/store/devtools.rs`)
//! and forgets it when the tab's browser goes away.
//!
//! Public API:
//! - `pub fn open(tab: Id, docked: bool)`, `pub fn close(tab: Id)`, `pub fn focus(tab: Id)`,
//!   `pub fn inspect_at(tab: Id, x: i32, y: i32)`
//! - `pub fn page_parent(tab: Id) -> Option<Panel>` — where tabs.rs parents the page view
//! - `pub fn page_rect_in_window(tab: Id) -> Option<Rect>` — pane-anchored overlays follow the page
//! - `pub fn relayout(tab: Id)`, `pub fn relayout_all()`
//! - `pub fn on_inspected_gone(tab: Id)` (tabs.rs: destroy / replace / crash),
//!   `pub fn on_inspected_browser_closed(browser_id: i32)` (browsers.rs: an unexpected death)
//! - `pub fn on_do_close(browser_id: i32, tab: Id)`, `pub fn on_frontend_closed(browser_id: i32, tab: Id)`
//! - `pub fn on_embedder_message(browser_id: i32, frame: &Frame, json: &str)` (client side of the shim)
//! - `pub fn deliver_to_frontend(inspected_browser: i32, message: String)` (devtools_cdp.rs)
//! - `pub fn on_session_ready(inspected_browser: i32)` (devtools_cdp.rs)
//! - `pub fn frontend_has_focus() -> bool`, `pub fn forwarded_key(browser_id: i32, event: &KeyEvent) -> bool`
//! - `pub fn on_dark_changed()`, `pub fn close_all()`, `pub fn clear()`, `pub fn debug_snapshot() -> Value`

use crate::browsers::{self, Role};
use crate::renderer::{self, MSG_DEVTOOLS_IN};
use crate::{controller, devtools_cdp, devtools_shim, overlays, rounded, tabs, task, window};
use cef::*;
use serde::Deserialize;
use serde_json::{Value, json};
use sta_core::{Command, Id};
use std::cell::RefCell;
use std::collections::HashMap;

/// The only URL a frontend browser ever loads.
const FRONTEND_URL: &str = "devtools://devtools/bundled/devtools_app.html?can_dock=true";
/// Protocol messages larger than this are split before crossing to the renderer.
const CHUNK_BYTES: usize = 4 * 1024 * 1024;
/// Timeout of the Inspect round trip.
const INSPECT_TIMEOUT_MS: i64 = 4000;
/// Timeout of the theme call on a frontend.
const THEME_TIMEOUT_MS: i64 = 4000;
/// DevTools' own zoom steps, in percent — Chrome's presets, the same list tabs.rs zooms a page
/// through. The frontend's `zoomIn`/`zoomOut`/`resetZoom` walk it (FID-4): Chromium's own
/// implementation needs a `ZoomController` on the DevTools WebContents, which only a Chrome-style
/// browser has, so a docked frontend zoomed by nothing at all until sta did it here.
const FRONTEND_ZOOM_PRESETS: [f64; 13] = [50.0, 67.0, 75.0, 80.0, 90.0, 100.0, 110.0, 125.0, 150.0, 175.0, 200.0, 250.0, 300.0];
/// The narrowest card (DIP) a frontend is docked into, and kept docked in.
///
/// DevTools' right dock never lets its own panel below ~355 DIP however little it is given, and
/// sta honours the rect the frontend reports — so inside a split pane of a 1280 DIP window (≈ 505
/// DIP) the inspected page was left 150 DIP wide and rendered one word per line, with the pane's
/// accent ring framing that strip: the split read as broken rather than cramped, and the frontend
/// offers no way to go narrower. Below this width a dock is not a dock, so sta opens (or moves)
/// DevTools into their own window instead and says why. Like every other undock this lasts for
/// that tab until its DevTools close (store/devtools.rs, D8), so widening the pane again does not
/// yank the window back.
const MIN_DOCK_WIDTH: i32 = 640;

struct Dock {
    /// The panel that replaces the page view in the wrapper.
    stack: Panel,
    frontend: BrowserView,
    frontend_browser: i32,
    /// The tab's own browser id, which owns the DevTools session the bridge attaches to.
    inspected: i32,
    /// Fills the stack above the frontend; its BoxLayout insets place the page view.
    page_host: Panel,
    /// Last `setInspectedPageBounds`, in stack coordinates.
    page_rect: Option<Rect>,
    /// The rect [`relayout`] last placed the page at (so it only touches the layout on a change).
    applied_rect: Option<Rect>,
    /// The frontend's own dock state (`setIsDocked`).
    docked: bool,
    /// Debugger keys the page must forward while the frontend is open: `keyCode | modifiers << 16`
    /// (Blink modifier bits), exactly like Chrome's `DevToolsEventForwarder`.
    forwarded: Vec<i32>,
    /// An `InspectAt` that arrived before session S existed.
    pending_inspect: Option<(i32, i32)>,
    /// [`undock_narrow`] was already asked for this dock, so a layout pass cannot ask twice while
    /// the round trip through core is in flight.
    narrow_asked: bool,
    embedder_messages: u64,
    /// Layout bookkeeping: how often the stack asked for a relayout, and how often that actually
    /// moved the page. The second number must stop growing once the dock settles; a layout loop is
    /// exactly what [`relayout`]'s "only on a change" rule prevents (see the module header).
    relayouts: u64,
    bounds_writes: u64,
}

/// A frontend browser being closed: its views stay alive here until the browser is gone. Taking the
/// entry out of the map *is* the "already handled" guard, so `do_close` and `on_before_close` can
/// both call [`detach_frontend`].
struct Closing {
    stack: Panel,
    frontend: BrowserView,
}

thread_local! {
    static DOCKS: RefCell<HashMap<Id, Dock>> = RefCell::new(HashMap::new());
    /// Frontend browser id → tab, and inspected browser id → tab.
    static BY_FRONTEND: RefCell<HashMap<i32, Id>> = RefCell::new(HashMap::new());
    static BY_INSPECTED: RefCell<HashMap<i32, Id>> = RefCell::new(HashMap::new());
    static CLOSING: RefCell<HashMap<i32, Closing>> = RefCell::new(HashMap::new());
    /// Tabs whose DevTools are CEF's own window (undock, Peek pages).
    static UNDOCKED: RefCell<std::collections::BTreeSet<Id>> = const { RefCell::new(std::collections::BTreeSet::new()) };
}

// ----------------------------------------------------------------------------------- open / close

/// `Effect::OpenDevTools`.
pub fn open(tab: Id, docked: bool) {
    if !docked {
        open_undocked(tab);
        return;
    }
    if DOCKS.with(|d| d.borrow().contains_key(&tab)) {
        focus(tab);
        return;
    }
    // Too narrow a card to dock into at all (see `MIN_DOCK_WIDTH`): never build the dock, so the
    // user never sees the squeezed frame. A card that has not been laid out yet (width 0) is
    // decided by `relayout` instead, once it has a real width.
    if let Some(width) = tabs::wrapper_of(tab).map(|w| w.bounds().width)
        && width > 1
        && width < MIN_DOCK_WIDTH
    {
        undock_narrow(tab, width);
        return;
    }
    close_undocked(tab);
    if !build(tab) {
        log_warn!("DevTools: could not dock for tab {tab}");
        controller::dispatch(Command::DevToolsClosed { tab });
    }
}

/// Asks core to move this tab's DevTools into their own window because the card is too narrow to
/// dock in. Core owns the transition (close the dock, remember the undock for the session, reopen
/// undocked) and raises the toast that explains it; the dispatch is posted because this can run
/// inside effect execution and inside a layout pass.
fn undock_narrow(tab: Id, width: i32) {
    log_info!("DevTools: the card of tab {tab} is {width} DIP, under the {MIN_DOCK_WIDTH} DIP a dock needs; opening undocked");
    task::post_ui(move || controller::dispatch(Command::DevToolsUndockRequested { tab, narrow: true }));
}

fn open_undocked(tab: Id) {
    close_docked(tab);
    let Some(browser) = tabs::browser_for_tab(tab) else {
        controller::dispatch(Command::DevToolsClosed { tab });
        return;
    };
    UNDOCKED.with(|u| u.borrow_mut().insert(tab));
    tabs::show_dev_tools(&browser, None);
}

fn close_undocked(tab: Id) {
    if !UNDOCKED.with(|u| u.borrow_mut().remove(&tab)) {
        return;
    }
    if let Some(host) = tabs::browser_for_tab(tab).and_then(|b| b.host())
        && host.has_dev_tools() != 0
    {
        host.close_dev_tools();
    }
}

/// `Effect::CloseDevTools`.
pub fn close(tab: Id) {
    close_undocked(tab);
    close_docked(tab);
}

fn build(tab: Id) -> bool {
    let Some(wrapper) = tabs::wrapper_of(tab) else { return false };
    let Some(inspected) = tabs::browser_for_tab(tab).map(|b| b.identifier()) else { return false };
    let page = tabs::view_for_tab(tab);
    let mut stack_delegate = StackDelegate::new(tab);
    let Some(stack) = panel_create(Some(&mut stack_delegate)) else { return false };
    stack.set_background_color(window::frame_color());

    // The page view leaves the wrapper first, so the wrapper's BoxLayout only ever sizes the stack.
    // A page view that currently sits in the Peek overlay stays there (Peek shows the page alone).
    let in_peek = tabs::is_in_peek(tab);
    if let Some(page) = &page
        && !in_peek
    {
        wrapper.remove_child_view(Some(&mut View::from(page)));
    }
    wrapper.add_child_view(Some(&mut View::from(&stack)));

    // Any failure from here on puts the card back the way it was: the page view returns to the
    // wrapper and nothing half-built is left in the view tree.
    let give_up = |frontend: Option<&BrowserView>| {
        if let Some(frontend) = frontend {
            stack.remove_child_view(Some(&mut View::from(frontend)));
        }
        wrapper.remove_child_view(Some(&mut View::from(&stack)));
        if let Some(page) = &page
            && !in_peek
        {
            wrapper.add_child_view(Some(&mut View::from(page)));
            wrapper.invalidate_layout();
            wrapper.layout();
        }
        false
    };

    let mut delegate = FrontendDelegate::new(tab);
    let Some(frontend) = create_frontend_view(tab, &mut delegate) else {
        return give_up(None);
    };
    // The browser is created synchronously while the view joins a Window hierarchy.
    stack.add_child_view(Some(&mut View::from(&frontend)));
    let Some(frontend_browser) = frontend.browser().map(|b| b.identifier()) else {
        log_error!("DevTools: the frontend browser was not created for tab {tab}");
        return give_up(Some(&frontend));
    };
    // Added after the frontend, so Views paints it (and the page inside it) above.
    let Some(page_host) = panel_create(None) else {
        return give_up(Some(&frontend));
    };
    stack.add_child_view(Some(&mut View::from(&page_host)));
    if let Some(page) = &page
        && !in_peek
    {
        page_host.add_child_view(Some(&mut View::from(page)));
    }
    DOCKS.with(|d| {
        d.borrow_mut().insert(tab, Dock {
            page_host,
            stack,
            frontend,
            frontend_browser,
            inspected,
            page_rect: None,
            applied_rect: None,
            docked: true,
            forwarded: Vec::new(),
            pending_inspect: None,
            narrow_asked: false,
            embedder_messages: 0,
            relayouts: 0,
            bounds_writes: 0,
        })
    });
    BY_FRONTEND.with(|m| m.borrow_mut().insert(frontend_browser, tab));
    apply_theme(frontend_browser);
    BY_INSPECTED.with(|m| m.borrow_mut().insert(inspected, tab));
    devtools_cdp::bridge_attach(inspected);
    log_info!("DevTools docked for tab {tab} (frontend browser {frontend_browser}, inspected {inspected})");
    relayout(tab);
    wrapper.invalidate_layout();
    wrapper.layout();
    focus(tab);
    true
}

fn create_frontend_view(tab: Id, delegate: &mut BrowserViewDelegate) -> Option<BrowserView> {
    let mut client = frontend_client();
    let mut info = dictionary_value_create()?;
    info.set_bool(Some(&CefString::from(renderer::EXTRA_DEVTOOLS)), 1);
    // This frontend's embedder is sta, so its renderer gets the shim. A DevTools window Chromium
    // owns (undock) deliberately does not (renderer.rs `EXTRA_DEVTOOLS_SHIM`).
    info.set_bool(Some(&CefString::from(renderer::EXTRA_DEVTOOLS_SHIM)), 1);
    info.set_double(Some(&CefString::from(renderer::EXTRA_DEVTOOLS_TAB)), tab as f64);
    info.set_string(Some(&CefString::from(renderer::EXTRA_DEVTOOLS_ORIGINS)), Some(&CefString::from(sta_origins().as_str())));
    let settings = BrowserSettings { background_color: window::frame_color(), ..Default::default() };
    browser_view_create(Some(&mut client), Some(&CefString::from(FRONTEND_URL)), Some(&settings), Some(&mut info), None, Some(delegate))
}

/// Every origin sta serves its own UI from, for `setOriginsForbiddenForExtensions` (SEC-3).
fn sta_origins() -> String {
    #[cfg(debug_assertions)]
    let hosts = crate::scheme::HOSTS.iter().chain(crate::scheme::DEBUG_HOSTS.iter());
    #[cfg(not(debug_assertions))]
    let hosts = crate::scheme::HOSTS.iter();
    let origins: Vec<String> = hosts.map(|h| format!("{}{h}", crate::scheme::ORIGIN_PREFIX)).collect();
    serde_json::to_string(&origins).unwrap_or_else(|_| "[]".into())
}

fn close_docked(tab: Id) {
    tear_down(tab, true);
}

/// Takes the dock apart: the page view goes back into its wrapper and the frontend browser is
/// closed (`ask_close`) or is already closing (then only its views are released).
fn tear_down(tab: Id, ask_close: bool) {
    let Some(dock) = DOCKS.with(|d| d.borrow_mut().remove(&tab)) else { return };
    let Dock { stack, page_host, frontend, frontend_browser, inspected, .. } = dock;
    // Who has the keyboard decides who gets it back: `build` gave the frontend focus, and the view
    // that had it is about to be destroyed, so the page would be left with no focus at all — typed
    // characters went nowhere until the user clicked (LAY-1, the F12 → F12 flow).
    let had_focus = overlays::focused_browser() == Some(frontend_browser);
    BY_INSPECTED.with(|m| m.borrow_mut().remove(&inspected));
    devtools_cdp::bridge_detach(inspected);
    // The page view goes home before anything else, so the tab keeps working even if the frontend
    // takes a moment to die.
    let wrapper = tabs::wrapper_of(tab);
    if let (Some(page), Some(wrapper)) = (tabs::view_for_tab(tab), &wrapper)
        && !tabs::is_in_peek(tab)
    {
        page_host.remove_child_view(Some(&mut View::from(&page)));
        wrapper.add_child_view(Some(&mut View::from(&page)));
        page.set_visible(1);
    }
    // The stack leaves the card **now**, not when its browser finally dies: the wrapper's box layout
    // would otherwise split the card between the page and a stack that is only waiting to be
    // dropped (measured: the page came back at x = 280 instead of the 2 px border inset).
    if let Some(wrapper) = &wrapper {
        wrapper.remove_child_view(Some(&mut View::from(&stack)));
    }
    match browsers::browser(frontend_browser) {
        Some(browser) => {
            CLOSING.with(|c| c.borrow_mut().insert(frontend_browser, Closing { stack, frontend }));
            if ask_close && let Some(host) = browser.host() {
                host.close_browser(1); // do_close -> on_do_close -> detach
            }
        }
        None => release(&stack, &frontend),
    }
    drop(page_host); // owned by the stack now; it goes when the stack does
    if let Some(wrapper) = &wrapper {
        // A layout manager does not re-lay out children whose parent bounds stay the same, and the
        // page view comes back carrying the bounds it had inside the stack (tabs.rs
        // `set_wrapper_insets` has the same note).
        wrapper.invalidate_layout();
        wrapper.layout();
    }
    rounded::layout_masks();
    overlays::layout();
    // Posted: the frontend's view is still being taken apart, and a `request_focus` inside that
    // gets undone by the widget's own focus bookkeeping when the old view finally goes.
    if had_focus && tabs::is_tab_visible(tab) && !tabs::is_in_peek(tab) {
        task::post_ui(move || {
            if DOCKS.with(|d| d.borrow().contains_key(&tab)) {
                return; // a new dock was built in the meantime; it owns the focus
            }
            tabs::focus_browser(tab);
        });
    }
    log_info!("DevTools closed for tab {tab}");
}

/// Removes the stack and the frontend view from the hierarchy and drops them.
fn release(stack: &Panel, frontend: &BrowserView) {
    stack.remove_child_view(Some(&mut View::from(frontend)));
    if let Some(parent) = View::from(stack).parent_view().and_then(|p| p.as_panel()) {
        parent.remove_child_view(Some(&mut View::from(stack)));
    }
}

/// Whether the tab's *current* dock is the one `browser_id` belongs to. Closing DevTools and
/// opening them again takes two frames, so an old frontend's `do_close`/`on_before_close` can arrive
/// **after** a new dock was built for the same tab: without this check it would tear the new one
/// down (and leave its browser open until shutdown).
fn is_current_frontend(tab: Id, browser_id: i32) -> bool {
    DOCKS.with(|d| d.borrow().get(&tab).is_some_and(|dock| dock.frontend_browser == browser_id))
}

/// `LifeSpanHandler::do_close` of a frontend browser (through browsers.rs). Never releases views
/// synchronously (views.md §5).
pub fn on_do_close(browser_id: i32, tab: Id) {
    let closing = CLOSING.with(|c| c.borrow().contains_key(&browser_id));
    task::post_ui(move || {
        if !closing && is_current_frontend(tab, browser_id) {
            // The frontend closed itself (`window.close()` from a DevTools action): take the dock
            // apart without asking the dying browser to close again.
            tear_down(tab, false);
            controller::dispatch(Command::DevToolsClosed { tab });
        }
        detach_frontend(browser_id);
    });
}

/// Releases a closing frontend's views. Dropping the last reference is what makes CEF destroy the
/// browser, so this never runs inside `do_close` itself (views.md §5).
fn detach_frontend(browser_id: i32) {
    let closing = CLOSING.with(|c| c.borrow_mut().remove(&browser_id));
    if let Some(closing) = closing {
        release(&closing.stack, &closing.frontend);
        drop(closing); // last reference -> the browser is destroyed
    }
}

/// `LifeSpanHandler::on_before_close` of a frontend browser (through browsers.rs).
pub fn on_frontend_closed(browser_id: i32, tab: Id) {
    BY_FRONTEND.with(|m| m.borrow_mut().remove(&browser_id));
    let closing = CLOSING.with(|c| c.borrow_mut().remove(&browser_id));
    let expected = closing.is_some();
    if let Some(closing) = closing {
        release(&closing.stack, &closing.frontend);
        drop(closing);
    }
    if is_current_frontend(tab, browser_id) {
        // The frontend died on its own (crash, or a close we did not start).
        if !expected {
            log_warn!("DevTools frontend of tab {tab} closed unexpectedly");
        }
        tear_down(tab, false);
        controller::dispatch(Command::DevToolsClosed { tab });
    }
}

/// A tab browser died without sta asking (`browsers::on_before_close`): if it was the one a dock
/// inspects, that dock is finished. Matched by browser id, so the old browser of a `ReplaceBrowser`
/// cannot close a dock that was opened for the new one.
pub fn on_inspected_browser_closed(browser_id: i32) {
    let tab = BY_INSPECTED.with(|m| m.borrow().get(&browser_id).copied());
    if let Some(tab) = tab {
        log_warn!("DevTools: the browser of tab {tab} closed while it was being inspected");
        on_inspected_gone(tab);
    }
}

/// The tab's own browser is going away (`ReplaceBrowser`, `DestroyBrowser`, a crash): the dock must
/// go first, because the page view has to be back in its wrapper before the new browser arrives.
pub fn on_inspected_gone(tab: Id) {
    if DOCKS.with(|d| d.borrow().contains_key(&tab)) || UNDOCKED.with(|u| u.borrow().contains(&tab)) {
        close(tab);
        controller::dispatch(Command::DevToolsClosed { tab });
    }
}

/// Shutdown: close every frontend (`window::begin_shutdown`, before the browser sweep).
pub fn close_all() {
    let tabs: Vec<Id> = DOCKS.with(|d| d.borrow().keys().copied().collect());
    for tab in tabs {
        close_docked(tab);
    }
    let undocked: Vec<Id> = UNDOCKED.with(|u| u.borrow().iter().copied().collect());
    for tab in undocked {
        close_undocked(tab);
    }
}

pub fn clear() {
    let docks = DOCKS.with(|d| std::mem::take(&mut *d.borrow_mut()));
    let closing = CLOSING.with(|c| std::mem::take(&mut *c.borrow_mut()));
    BY_FRONTEND.with(|m| m.borrow_mut().clear());
    BY_INSPECTED.with(|m| m.borrow_mut().clear());
    UNDOCKED.with(|u| u.borrow_mut().clear());
    drop(docks);
    drop(closing);
}

// ----------------------------------------------------------------------------------- layout

wrap_panel_delegate! {
    struct StackDelegate {
        tab: Id,
    }

    impl ViewDelegate {
        fn preferred_size(&self, _view: Option<&mut View>) -> Size {
            Size { width: 1, height: 1 }
        }

        fn on_layout_changed(&self, _view: Option<&mut View>, _new_bounds: Option<&Rect>) {
            let tab = self.tab;
            // Setting child bounds inside the parent's own layout pass: post it.
            task::post_ui(move || relayout(tab));
        }
    }

    impl PanelDelegate {}
}

fn same(a: &Rect, b: &Rect) -> bool {
    a.x == b.x && a.y == b.y && a.width == b.width && a.height == b.height
}

/// Places the page at the rect the frontend reported (clamped to the stack) by giving the page host
/// the matching BoxLayout insets. The frontend needs no placement of its own: the stack's fill
/// layout already gives it the whole card. In page fullscreen the frontend is hidden and the page
/// fills the stack.
///
/// Idempotent by construction — it only touches the layout when the target rect changed — because a
/// layout write invalidates the chain up to the widget, whose next pass calls the stack's
/// `on_layout_changed` again (see the module header).
pub fn relayout(tab: Id) {
    let Some((stack, frontend, rect, page_host, applied)) = DOCKS.with(|d| {
        let d = d.borrow();
        let dock = d.get(&tab)?;
        Some((dock.stack.clone(), dock.frontend.clone(), dock.page_rect.clone(), dock.page_host.clone(), dock.applied_rect.clone()))
    }) else {
        return;
    };
    let bounds = stack.bounds();
    let (w, h) = (bounds.width.max(0), bounds.height.max(0));
    // A collapsed stack (the window is minimized: Views lays its contents out at 1×1) would compute
    // insets of 0 on every side, and the *next* layout pass — the one that restores the window —
    // would hand the page the whole card for a frame before the real rect came back (LAY-3: two page
    // reflows and a flash of the page over DevTools). The last good insets survive instead.
    if w <= 1 || h <= 1 {
        DOCKS.with(|d| {
            if let Some(dock) = d.borrow_mut().get_mut(&tab) {
                dock.relayouts += 1;
            }
        });
        return;
    }
    // The card shrank below what a dock can use — the tab was put into a split pane, or the window
    // was dragged narrow. Ask core to move DevTools into their own window (once per dock).
    if w < MIN_DOCK_WIDTH {
        let ask = DOCKS.with(|d| {
            d.borrow_mut().get_mut(&tab).is_some_and(|dock| {
                let first = !dock.narrow_asked;
                dock.narrow_asked = true;
                first
            })
        });
        if ask {
            undock_narrow(tab, w);
        }
    }
    let fullscreen = window::page_fullscreen_tab() == Some(tab);
    let full = Rect { x: 0, y: 0, width: w, height: h };
    if (frontend.is_visible() != 0) == fullscreen {
        frontend.set_visible((!fullscreen) as i32);
    }
    let _ = &stack;
    let mut writes = 0u64;
    let page = tabs::view_for_tab(tab).filter(|_| !tabs::is_in_peek(tab));
    if let Some(page) = page {
        let target = match rect {
            Some(r) if !fullscreen => clamp(r, w, h),
            _ => full,
        };
        // Self-healing: re-apply when the page is not where it should be, even if `applied_rect`
        // says it is (a stray layout elsewhere), but never when both already agree.
        let placed = View::from(&page).bounds();
        if applied.as_ref().is_none_or(|a| !same(a, &target)) || !same(&placed, &target) {
            writes += 1;
            page_host.set_to_box_layout(Some(&BoxLayoutSettings {
                horizontal: 1,
                inside_border_insets: Insets {
                    top: target.y,
                    left: target.x,
                    bottom: h - target.y - target.height,
                    right: w - target.x - target.width,
                },
                cross_axis_alignment: AxisAlignment::STRETCH,
                default_flex: 1,
                ..Default::default()
            }));
            page_host.invalidate_layout();
            page_host.layout();
            DOCKS.with(|d| {
                if let Some(dock) = d.borrow_mut().get_mut(&tab) {
                    dock.applied_rect = Some(target);
                }
            });
        }
        if page.is_visible() == 0 {
            page.set_visible(1);
        }
    }
    DOCKS.with(|d| {
        if let Some(dock) = d.borrow_mut().get_mut(&tab) {
            dock.relayouts += 1;
            dock.bounds_writes += writes;
        }
    });
}

pub fn relayout_all() {
    let tabs: Vec<Id> = DOCKS.with(|d| d.borrow().keys().copied().collect());
    for tab in tabs {
        relayout(tab);
    }
}

/// The reported rect, kept inside the stack (a frontend that asks for more than it has must not
/// push the page out of the card).
fn clamp(r: Rect, w: i32, h: i32) -> Rect {
    let x = r.x.clamp(0, w);
    let y = r.y.clamp(0, h);
    Rect { x, y, width: r.width.clamp(0, w - x), height: r.height.clamp(0, h - y) }
}

/// The page host, so tabs.rs parents the tab's page view inside it while DevTools are docked.
pub fn page_parent(tab: Id) -> Option<Panel> {
    DOCKS.with(|d| d.borrow().get(&tab).map(|dock| dock.page_host.clone()))
}

/// The page view's bounds in window coordinates, for overlays anchored to the page (find bar,
/// permission prompt). `None` when the tab has no dock — tabs.rs uses the wrapper then.
pub fn page_rect_in_window(tab: Id) -> Option<Rect> {
    let page = DOCKS.with(|d| d.borrow().get(&tab).map(|_| ()))
        .and_then(|_| tabs::view_for_tab(tab))
        .filter(|_| !tabs::is_in_peek(tab))?;
    if View::from(&page).is_drawn() == 0 {
        return None;
    }
    window::view_rect_in_window(&View::from(&page))
}

// ----------------------------------------------------------------------------------- focus, keys

/// The frontend BrowserView of a tab's dock (overlays.rs focus bookkeeping).
pub fn frontend_view(tab: Id) -> Option<BrowserView> {
    DOCKS.with(|d| d.borrow().get(&tab).map(|dock| dock.frontend.clone()))
}

/// `Effect::FocusDevTools`.
pub fn focus(tab: Id) {
    if let Some(frontend) = DOCKS.with(|d| d.borrow().get(&tab).map(|dock| dock.frontend.clone())) {
        frontend.request_focus();
    }
}

/// Whether a docked DevTools frontend currently has keyboard focus (keyboard.rs: Ctrl+Shift+I's
/// third step, and plain F11 belonging to DevTools — D9).
pub fn frontend_has_focus() -> bool {
    overlays::focused_browser().is_some_and(|id| matches!(browsers::role_of(id), Some(Role::DevTools { .. })))
}

/// CEF event flags → Blink's modifier bits, which are what the frontend's whitelist counts in.
fn blink_modifiers(event: &KeyEvent) -> i32 {
    const EVENTFLAG_SHIFT_DOWN: u32 = 1 << 1;
    const EVENTFLAG_CONTROL_DOWN: u32 = 1 << 2;
    const EVENTFLAG_ALT_DOWN: u32 = 1 << 3;
    const EVENTFLAG_COMMAND_DOWN: u32 = 1 << 7;
    let mut m = 0;
    if event.modifiers & EVENTFLAG_SHIFT_DOWN != 0 {
        m |= 1;
    }
    if event.modifiers & EVENTFLAG_CONTROL_DOWN != 0 {
        m |= 2;
    }
    if event.modifiers & EVENTFLAG_ALT_DOWN != 0 {
        m |= 4;
    }
    if event.modifiers & EVENTFLAG_COMMAND_DOWN != 0 {
        m |= 8;
    }
    m
}

/// A key the focused page must hand to its docked DevTools (the frontend's
/// `setWhitelistedShortcuts`: F8, F10, Shift+F11, Ctrl+\, Ctrl+' while paused). Returns whether the
/// key was forwarded and must be consumed.
///
/// Plain F11 is **not** forwarded from the page: D9 keeps it as sta's fullscreen key there, and
/// gives it to DevTools only while the frontend itself has focus (keyboard.rs).
pub fn forwarded_key(browser_id: i32, event: &KeyEvent) -> bool {
    let Some(Role::Tab(tab)) = browsers::role_of(browser_id) else { return false };
    let kind = match event.type_ {
        KeyEventType::RAWKEYDOWN | KeyEventType::KEYDOWN => "keydown",
        KeyEventType::KEYUP => "keyup",
        _ => return false,
    };
    let modifiers = blink_modifiers(event);
    let key = event.windows_key_code | (modifiers << 16);
    let wanted = DOCKS.with(|d| d.borrow().get(&tab).is_some_and(|dock| dock.forwarded.contains(&key)));
    if !wanted {
        return false;
    }
    if event.windows_key_code == crate::keyboard::vk::F11 && modifiers == 0 {
        return false; // D9: fullscreen from the page
    }
    let payload = json!({
        "type": kind,
        "keyCode": event.windows_key_code,
        "modifiers": modifiers,
        "key": "",
        "code": "",
    });
    send_in(tab, devtools_shim::IN_KEY, &payload.to_string());
    true
}

/// The tab whose docked DevTools should take Ctrl+Shift+C: the focused pane's, when it has a dock
/// (keyboard.rs; Chrome's element-picker chord). `None` leaves the key to sta's own binding.
pub fn picker_tab() -> Option<Id> {
    let focused = overlays::focused_browser()?;
    let tab = match browsers::role_of(focused) {
        Some(Role::Tab(tab)) | Some(Role::DevTools { tab }) => tab,
        _ => return None,
    };
    DOCKS.with(|d| d.borrow().contains_key(&tab)).then_some(tab)
}

/// Starts the element picker in `tab`'s docked frontend, exactly as Chrome's Ctrl+Shift+C does
/// (`DevToolsAPI.enterInspectElementMode`, which is also what a `DevToolsToggleAction::Inspect`
/// turns into): the frontend arms `Overlay.setInspectMode` itself, so its toolbar button lights up
/// and the node the user clicks is revealed in Elements.
pub fn enter_inspect_mode(tab: Id) {
    if !DOCKS.with(|d| d.borrow().contains_key(&tab)) {
        return;
    }
    focus(tab);
    send_in(tab, devtools_shim::IN_ACTION, devtools_shim::ACTION_INSPECT);
}

/// One step through [`FRONTEND_ZOOM_PRESETS`] on the frontend browser (`direction`: 1 in, -1 out,
/// 0 reset).
fn zoom_frontend(tab: Id, direction: i32) {
    let Some(host) = DOCKS
        .with(|d| d.borrow().get(&tab).map(|dock| dock.frontend_browser))
        .and_then(browsers::browser)
        .and_then(|b| b.host())
    else {
        return;
    };
    let percent = 100.0 * 1.2f64.powf(host.zoom_level());
    let next = match direction {
        0 => 100.0,
        1 => FRONTEND_ZOOM_PRESETS.iter().copied().find(|p| *p > percent + 0.5).unwrap_or(300.0),
        _ => FRONTEND_ZOOM_PRESETS.iter().rev().copied().find(|p| *p < percent - 0.5).unwrap_or(50.0),
    };
    host.set_zoom_level((next / 100.0).ln() / 1.2f64.ln());
}

/// sta's dark mode changed: let every open frontend follow it (UX16).
pub fn on_dark_changed() {
    let frontends: Vec<i32> = DOCKS.with(|d| d.borrow().values().map(|dock| dock.frontend_browser).collect());
    for id in frontends {
        apply_theme(id);
    }
}

/// Makes a frontend follow sta's dark mode by emulating `prefers-color-scheme` in its own page
/// (UX16). DevTools reads that media feature live, so a theme change while DevTools are open is
/// picked up at once; its `uiTheme` preference is left alone, so a user who picks a theme inside
/// DevTools' settings keeps it.
fn apply_theme(frontend_browser: i32) {
    let value = if window::is_dark() { "dark" } else { "light" };
    let params = json!({ "media": "", "features": [{ "name": "prefers-color-scheme", "value": value }] });
    devtools_cdp::call(frontend_browser, devtools_cdp::User::DevTools, "Emulation.setEmulatedMedia", params, THEME_TIMEOUT_MS, move |r| {
        if let Err(e) = r {
            log_warn!("DevTools: could not set the frontend theme: {e}");
        }
    });
}

// ----------------------------------------------------------------------------------- transport

/// Sends one message to a tab's frontend renderer, chunked (`__staDevToolsDispatch`).
fn send_in(tab: Id, kind: &str, payload: &str) {
    let Some(frame) = DOCKS
        .with(|d| d.borrow().get(&tab).map(|dock| dock.frontend_browser))
        .and_then(browsers::browser)
        .and_then(|b| b.main_frame())
    else {
        return;
    };
    let chunks = split(payload);
    let count = chunks.len() as i32;
    for (index, chunk) in chunks.iter().enumerate() {
        let Some(mut msg) = process_message_create(Some(&CefString::from(MSG_DEVTOOLS_IN))) else { return };
        if let Some(args) = msg.argument_list() {
            args.set_string(0, Some(&CefString::from(kind)));
            args.set_string(1, Some(&CefString::from(*chunk)));
            args.set_int(2, index as i32);
            args.set_int(3, count);
        }
        frame.send_process_message(ProcessId::RENDERER, Some(&mut msg));
    }
}

/// `payload` split at char boundaries into pieces of at most [`CHUNK_BYTES`].
fn split(payload: &str) -> Vec<&str> {
    if payload.len() <= CHUNK_BYTES {
        return vec![payload];
    }
    let mut out = Vec::new();
    let mut rest = payload;
    while rest.len() > CHUNK_BYTES {
        let mut at = CHUNK_BYTES;
        while at > 0 && !rest.is_char_boundary(at) {
            at -= 1;
        }
        if at == 0 {
            break;
        }
        let (head, tail) = rest.split_at(at);
        out.push(head);
        rest = tail;
    }
    if !rest.is_empty() {
        out.push(rest);
    }
    out
}

/// One protocol message for the frontend of `inspected_browser` (devtools_cdp.rs).
pub fn deliver_to_frontend(inspected_browser: i32, message: String) {
    let Some(tab) = BY_INSPECTED.with(|m| m.borrow().get(&inspected_browser).copied()) else { return };
    send_in(tab, devtools_shim::IN_PROTOCOL, &message);
}

/// Session S exists: run an Inspect that was waiting for it.
pub fn on_session_ready(inspected_browser: i32) {
    let Some(tab) = BY_INSPECTED.with(|m| m.borrow().get(&inspected_browser).copied()) else { return };
    let pending = DOCKS.with(|d| d.borrow_mut().get_mut(&tab).and_then(|dock| dock.pending_inspect.take()));
    if let Some((x, y)) = pending {
        inspect_at(tab, x, y);
    }
}

// ----------------------------------------------------------------------------------- embedder

/// What the shim sends: the wrapped `InspectorFrontendHost` method and its arguments.
#[derive(Deserialize)]
struct Embedder {
    m: String,
    #[serde(default)]
    a: Vec<Value>,
}

#[derive(Deserialize)]
struct Bounds {
    #[serde(default)]
    x: i32,
    #[serde(default)]
    y: i32,
    #[serde(default)]
    width: i32,
    #[serde(default)]
    height: i32,
}

/// `renderer::MSG_DEVTOOLS_EMBEDDER` from a frontend renderer. The sender is re-checked here
/// (registered frontend browser, main frame, frontend URL) — a renderer must not be taken at its
/// word (FINAL PLAN §3 "Transport").
pub fn on_embedder_message(browser_id: i32, frame: &Frame, json: &str) {
    let Some(tab) = BY_FRONTEND.with(|m| m.borrow().get(&browser_id).copied()) else {
        log_warn!("DevTools: embedder message from browser {browser_id}, which is not a frontend");
        return;
    };
    if frame.is_main() == 0 || !devtools_shim::is_frontend_url(&CefString::from(&frame.url()).to_string()) {
        log_warn!("DevTools: embedder message from a frame that is not the frontend of tab {tab}");
        return;
    }
    let Ok(msg) = serde_json::from_str::<Embedder>(json) else { return };
    DOCKS.with(|d| {
        if let Some(dock) = d.borrow_mut().get_mut(&tab) {
            dock.embedder_messages += 1;
        }
    });
    let inspected = DOCKS.with(|d| d.borrow().get(&tab).map(|dock| dock.inspected));
    let arg_str = |i: usize| msg.a.get(i).and_then(Value::as_str).unwrap_or_default().to_string();
    match msg.m.as_str() {
        "sendMessageToBackend" => {
            if let Some(inspected) = inspected {
                devtools_cdp::bridge_from_frontend(inspected, &arg_str(0));
            }
        }
        "setInspectedPageBounds" => {
            let Some(bounds) = msg.a.first().and_then(|v| serde_json::from_value::<Bounds>(v.clone()).ok()) else { return };
            let rect = Rect { x: bounds.x, y: bounds.y, width: bounds.width, height: bounds.height };
            DOCKS.with(|d| {
                if let Some(dock) = d.borrow_mut().get_mut(&tab) {
                    dock.page_rect = Some(rect);
                }
            });
            relayout(tab);
        }
        "setIsDocked" => {
            let docked = msg.a.first().and_then(Value::as_bool).unwrap_or(true);
            DOCKS.with(|d| {
                if let Some(dock) = d.borrow_mut().get_mut(&tab) {
                    dock.docked = docked;
                }
            });
            if !docked {
                controller::dispatch(Command::DevToolsUndockRequested { tab, narrow: false });
            }
        }
        // This frontend closes *itself*, so it names its own tab: an untargeted `ToggleDevTools`
        // resolves against the focused pane instead, which from a split pane that does not have
        // focus closed nothing and opened a second dock on the other pane (LAY-2).
        // Posted, like every other teardown here: this runs inside a process message from the very
        // renderer whose browser is about to be closed.
        "closeWindow" => task::post_ui(move || {
            close(tab);
            controller::dispatch(Command::DevToolsClosed { tab });
        }),
        "bringToFront" => {
            window::activate();
            focus(tab);
        }
        // DevTools' own zoom (Ctrl+= / Ctrl+- / Ctrl+0 and the ⋮ menu).
        "zoomIn" => zoom_frontend(tab, 1),
        "zoomOut" => zoom_frontend(tab, -1),
        "resetZoom" => zoom_frontend(tab, 0),
        "openInNewTab" => {
            let url = arg_str(0);
            controller::dispatch(Command::DevToolsLinkRequested { tab, url, search: false });
        }
        "openSearchResultsInNewTab" => {
            let url = arg_str(0);
            controller::dispatch(Command::DevToolsLinkRequested { tab, url, search: true });
        }
        "setWhitelistedShortcuts" => {
            let keys = parse_shortcuts(&arg_str(0));
            DOCKS.with(|d| {
                if let Some(dock) = d.borrow_mut().get_mut(&tab) {
                    dock.forwarded = keys;
                }
            });
        }
        "inspectElementCompleted" => {}
        other => log_debug!("DevTools: unhandled embedder message {other} from tab {tab}"),
    }
}

/// `setWhitelistedShortcuts`: `[{keyCode, modifiers}]` → `keyCode | modifiers << 16`, with Chrome's
/// own rule that only function keys may be forwarded unmodified.
fn parse_shortcuts(json: &str) -> Vec<i32> {
    let Ok(list) = serde_json::from_str::<Vec<Value>>(json) else { return Vec::new() };
    list.iter()
        .filter_map(|item| {
            let key = item.get("keyCode").and_then(Value::as_i64)? as i32;
            let modifiers = item.get("modifiers").and_then(Value::as_i64).unwrap_or(0) as i32;
            let function_key = (0x70..=0x7B).contains(&key); // VK_F1..VK_F12
            if key == 0 || !(function_key || modifiers != 0) {
                return None;
            }
            Some(key | (modifiers << 16))
        })
        .collect()
}

// ----------------------------------------------------------------------------------- Inspect

/// `Effect::InspectAt`: `x`/`y` are the context menu's point in the page view's own coordinates.
pub fn inspect_at(tab: Id, x: i32, y: i32) {
    let Some(inspected) = DOCKS.with(|d| d.borrow().get(&tab).map(|dock| dock.inspected)) else {
        // Undocked DevTools: CEF's own InspectElement path.
        if let Some(browser) = tabs::browser_for_tab(tab) {
            tabs::show_dev_tools(&browser, Some(Point { x, y }));
        }
        return;
    };
    if devtools_cdp::bridge_session(inspected).is_none() {
        DOCKS.with(|d| {
            if let Some(dock) = d.borrow_mut().get_mut(&tab) {
                dock.pending_inspect = Some((x, y));
            }
        });
        return;
    }
    // View pixels → CSS pixels of the page (Chromium scales the view by the page zoom).
    let zoom = tabs::browser_for_tab(tab).and_then(|b| b.host()).map(|h| h.zoom_level()).unwrap_or(0.0);
    let scale = 1.2f64.powf(zoom);
    let (cx, cy) = ((x as f64 / scale).round() as i32, (y as f64 / scale).round() as i32);
    node_at(inspected, None, cx, cy, 0);
}

/// `DOM.getNodeForLocation` on `session`; an iframe owner is followed into its own session once
/// (`depth` guards a cycle).
fn node_at(inspected: i32, session: Option<String>, x: i32, y: i32, depth: u32) {
    let params = json!({ "x": x, "y": y, "includeUserAgentShadowDOM": false });
    let owned = session.clone();
    devtools_cdp::session_call(inspected, session.as_deref(), "DOM.getNodeForLocation", params, INSPECT_TIMEOUT_MS, move |result| {
        let Ok(value) = result else { return };
        let Some(backend_node_id) = value.get("backendNodeId").and_then(Value::as_i64) else { return };
        let frame_id = value.get("frameId").and_then(Value::as_str).map(str::to_string);
        if depth == 0 && let Some(frame_id) = frame_id {
            // An OOPIF's owner element: the node the user pointed at lives in the nested session.
            descend(inspected, owned, backend_node_id, frame_id, x, y, depth);
            return;
        }
        select(inspected, owned, backend_node_id);
    });
}

/// Follows an `<iframe>` owner into the nested session of its frame, offsetting the point by the
/// iframe's content box (F8 step 3). Falls back to selecting the owner itself.
///
/// `DOM.getNodeForLocation` answers with the frame the *node* belongs to (the parent), so the child
/// frame is read from the node itself: `DOM.Node.frameId` of a frame owner element is its own
/// frame's id, which is also the target id of that frame's session when it is an OOPIF.
fn descend(inspected: i32, session: Option<String>, backend_node_id: i64, _frame_id: String, x: i32, y: i32, depth: u32) {
    let params = json!({ "backendNodeId": backend_node_id });
    let owned = session.clone();
    devtools_cdp::session_call(inspected, session.as_deref(), "DOM.describeNode", params, INSPECT_TIMEOUT_MS, move |result| {
        let node = result.ok().and_then(|v| v.get("node").cloned());
        let name = node.as_ref().and_then(|n| n.get("nodeName")).and_then(Value::as_str).unwrap_or_default().to_string();
        let child_frame = node.as_ref().and_then(|n| n.get("frameId")).and_then(Value::as_str).map(str::to_string);
        let Some(child_frame) = child_frame.filter(|_| name.eq_ignore_ascii_case("IFRAME")) else {
            select(inspected, owned, backend_node_id);
            return;
        };
        let nested = devtools_cdp::nested_session_for_target(inspected, &child_frame);
        let Some(nested) = nested else {
            select(inspected, owned, backend_node_id);
            return;
        };
        let box_params = json!({ "backendNodeId": backend_node_id });
        let owner = owned.clone();
        devtools_cdp::session_call(inspected, owner.as_deref(), "DOM.getBoxModel", box_params, INSPECT_TIMEOUT_MS, move |result| {
            // content quad: x1,y1, x2,y2, x3,y3, x4,y4 — its top-left is the frame's origin.
            let (ox, oy) = result
                .ok()
                .and_then(|v| v.get("model").and_then(|m| m.get("content")).cloned())
                .and_then(|q| {
                    let q = q.as_array()?;
                    Some((q.first()?.as_f64()?.round() as i32, q.get(1)?.as_f64()?.round() as i32))
                })
                .unwrap_or((0, 0));
            node_at(inspected, Some(nested), x - ox, y - oy, depth + 1);
        });
    });
}

/// Tells the frontend to select the node, exactly as Chromium's own "Inspect" does
/// (`Overlay.inspectNodeRequested`).
fn select(inspected: i32, session: Option<String>, backend_node_id: i64) {
    let mut event = json!({ "method": "Overlay.inspectNodeRequested", "params": { "backendNodeId": backend_node_id } });
    if let Some(session) = session {
        event["sessionId"] = Value::String(session);
    }
    deliver_to_frontend(inspected, event.to_string());
}

// ----------------------------------------------------------------------------------- the client

thread_local! {
    static CLIENT: RefCell<Option<Client>> = const { RefCell::new(None) };
}

/// The frontend browser's client: it loads exactly one URL, opens no popups of its own, says nothing
/// to the console and takes part in sta's focus and key handling like any view in the window.
fn frontend_client() -> Client {
    if let Some(c) = CLIENT.with(|c| c.borrow().clone()) {
        return c;
    }
    let client = FrontendClient::new(FrontendLifeSpan::new(), FrontendRequest::new(), FrontendKeyboard::new(), FrontendFocus::new(), FrontendDisplay::new());
    CLIENT.with(|c| *c.borrow_mut() = Some(client.clone()));
    client
}

wrap_browser_view_delegate! {
    pub struct FrontendDelegate {
        tab: Id,
    }

    impl ViewDelegate {}

    impl BrowserViewDelegate {
        fn on_browser_created(&self, _browser_view: Option<&mut BrowserView>, browser: Option<&mut Browser>) {
            if let Some(browser) = browser {
                let id = browser.identifier();
                browsers::set_role(id, Role::DevTools { tab: self.tab });
                BY_FRONTEND.with(|m| m.borrow_mut().insert(id, self.tab));
            }
        }

        fn browser_runtime_style(&self) -> RuntimeStyle {
            RuntimeStyle::ALLOY
        }
    }
}

wrap_client! {
    struct FrontendClient {
        life_span: LifeSpanHandler,
        request: RequestHandler,
        keyboard: KeyboardHandler,
        focus: FocusHandler,
        display: DisplayHandler,
    }

    impl Client {
        fn life_span_handler(&self) -> Option<LifeSpanHandler> {
            Some(self.life_span.clone())
        }

        fn request_handler(&self) -> Option<RequestHandler> {
            Some(self.request.clone())
        }

        fn keyboard_handler(&self) -> Option<KeyboardHandler> {
            Some(self.keyboard.clone())
        }

        fn focus_handler(&self) -> Option<FocusHandler> {
            Some(self.focus.clone())
        }

        fn display_handler(&self) -> Option<DisplayHandler> {
            Some(self.display.clone())
        }

        fn on_process_message_received(
            &self,
            browser: Option<&mut Browser>,
            frame: Option<&mut Frame>,
            _source_process: ProcessId,
            message: Option<&mut ProcessMessage>,
        ) -> i32 {
            let (Some(browser), Some(frame), Some(message)) = (browser, frame, message) else { return 0 };
            if CefString::from(&message.name()).to_string() != renderer::MSG_DEVTOOLS_EMBEDDER {
                return 0;
            }
            let json = message.argument_list().map(|a| CefString::from(&a.string(0)).to_string()).unwrap_or_default();
            on_embedder_message(browser.identifier(), frame, &json);
            1
        }
    }
}

wrap_life_span_handler! {
    struct FrontendLifeSpan;

    impl LifeSpanHandler {
        /// The frontend's own links and popups become `DevToolsLinkRequested` (R-SEC-6): core decides
        /// with the web-content rules, and nothing opens a window of its own.
        fn on_before_popup(
            &self,
            browser: Option<&mut Browser>,
            _frame: Option<&mut Frame>,
            _popup_id: i32,
            target_url: Option<&CefString>,
            _target_frame_name: Option<&CefString>,
            _target_disposition: WindowOpenDisposition,
            _user_gesture: i32,
            _popup_features: Option<&PopupFeatures>,
            _window_info: Option<&mut WindowInfo>,
            _client: Option<&mut Option<Client>>,
            _settings: Option<&mut BrowserSettings>,
            _extra_info: Option<&mut Option<DictionaryValue>>,
            _no_javascript_access: Option<&mut i32>,
        ) -> i32 {
            let url = target_url.map(|u| u.to_string()).unwrap_or_default();
            if let Some(tab) = browser.and_then(|b| BY_FRONTEND.with(|m| m.borrow().get(&b.identifier()).copied()))
                && !url.is_empty()
            {
                controller::dispatch(Command::DevToolsLinkRequested { tab, url, search: false });
            }
            1 // cancel
        }

        fn on_after_created(&self, browser: Option<&mut Browser>) {
            if let Some(browser) = browser {
                browsers::on_after_created(browser, false);
            }
        }

        fn do_close(&self, browser: Option<&mut Browser>) -> i32 {
            browser.map(|b| browsers::on_do_close(b)).unwrap_or(1)
        }

        fn on_before_close(&self, browser: Option<&mut Browser>) {
            if let Some(browser) = browser {
                devtools_cdp::on_browser_closed(browser.identifier());
                browsers::on_before_close(browser);
            }
        }
    }
}

wrap_request_handler! {
    struct FrontendRequest;

    impl RequestHandler {
        /// Only the one frontend document ever loads here.
        fn on_before_browse(
            &self,
            browser: Option<&mut Browser>,
            _frame: Option<&mut Frame>,
            request: Option<&mut Request>,
            _user_gesture: i32,
            _is_redirect: i32,
        ) -> i32 {
            let url = request.map(|r| CefString::from(&r.url()).to_string()).unwrap_or_default();
            if url == FRONTEND_URL || devtools_shim::is_frontend_url(&url) {
                return 0;
            }
            let id = browser.map(|b| b.identifier());
            log_warn!("DevTools frontend (browser {id:?}) tried to load {url}");
            1
        }

        fn on_render_process_terminated(&self, browser: Option<&mut Browser>, status: TerminationStatus, _error_code: i32, _error_string: Option<&CefString>) {
            let Some(browser) = browser else { return };
            let id = browser.identifier();
            log_warn!("DevTools frontend of browser {id} crashed ({status:?})");
            let tab = BY_FRONTEND.with(|m| m.borrow().get(&id).copied());
            if let Some(tab) = tab {
                task::post_ui(move || {
                    close(tab);
                    controller::dispatch(Command::DevToolsClosed { tab });
                });
            }
        }
    }
}

wrap_keyboard_handler! {
    struct FrontendKeyboard;

    impl KeyboardHandler {
        fn on_pre_key_event(
            &self,
            browser: Option<&mut Browser>,
            event: Option<&KeyEvent>,
            _os_event: Option<&mut cef::sys::MSG>,
            _is_keyboard_shortcut: Option<&mut i32>,
        ) -> i32 {
            let (Some(browser), Some(event)) = (browser, event) else { return 0 };
            crate::keyboard::on_pre_key_event(browser.identifier(), event) as i32
        }

        fn on_key_event(&self, browser: Option<&mut Browser>, event: Option<&KeyEvent>, os_event: Option<&mut cef::sys::MSG>) -> i32 {
            let (Some(browser), Some(event)) = (browser, event) else { return 0 };
            crate::keyboard::on_key_event(browser.identifier(), event, os_event.is_some()) as i32
        }
    }
}

wrap_focus_handler! {
    struct FrontendFocus;

    impl FocusHandler {
        fn on_got_focus(&self, browser: Option<&mut Browser>) {
            let Some(browser) = browser else { return };
            let id = browser.identifier();
            // The tab the frontend belongs to stays the focused tab: Ctrl+L, Ctrl+R and the like
            // keep acting on the page the user is inspecting.
            if let Some(tab) = BY_FRONTEND.with(|m| m.borrow().get(&id).copied())
                && !overlays::focus_events_suppressed()
            {
                controller::dispatch(Command::TabFocused { tab });
            }
            overlays::on_browser_got_focus(id);
        }
    }
}

wrap_display_handler! {
    struct FrontendDisplay;

    impl DisplayHandler {
        /// The frontend's console output never reaches sta's log or the agent console watcher.
        fn on_console_message(&self, _browser: Option<&mut Browser>, _level: LogSeverity, _message: Option<&CefString>, _source: Option<&CefString>, _line: i32) -> i32 {
            1
        }
    }
}

// ----------------------------------------------------------------------------------- debug

#[cfg_attr(not(debug_assertions), allow(dead_code))] // used by debug.rs only
pub fn debug_snapshot() -> Value {
    let docks: Vec<Value> = DOCKS.with(|d| {
        d.borrow()
            .iter()
            .map(|(tab, dock)| {
                let b = dock.stack.bounds();
                let page = tabs::view_for_tab(*tab).map(|v| View::from(&v).bounds());
                let f = View::from(&dock.frontend).bounds();
                let host = View::from(&dock.page_host).bounds();
                json!({
                    "tab": tab,
                    "frontendBrowser": dock.frontend_browser,
                    "inspected": dock.inspected,
                    "docked": dock.docked,
                    "session": devtools_cdp::bridge_session(dock.inspected),
                    "stackBounds": [b.x, b.y, b.width, b.height],
                    "reportedPageRect": dock.page_rect.as_ref().map(|r| [r.x, r.y, r.width, r.height]),
                    "pageBounds": page.map(|r| [r.x, r.y, r.width, r.height]),
                    "frontendBounds": [f.x, f.y, f.width, f.height],
                    "pageHostBounds": [host.x, host.y, host.width, host.height],
                    "frontendVisible": dock.frontend.is_visible() != 0,
                    "forwardedKeys": dock.forwarded,
                    "embedderMessages": dock.embedder_messages,
                    "relayouts": dock.relayouts,
                    "boundsWrites": dock.bounds_writes,
                })
            })
            .collect()
    });
    let undocked: Vec<Id> = UNDOCKED.with(|u| u.borrow().iter().copied().collect());
    let closing: Vec<i32> = CLOSING.with(|c| c.borrow().keys().copied().collect());
    json!({ "docks": docks, "undocked": undocked, "closing": closing, "dark": window::is_dark() })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reported_rect_stays_inside_the_stack() {
        assert_eq!(rect_tuple(clamp(Rect { x: 0, y: 0, width: 400, height: 300 }, 800, 600)), (0, 0, 400, 300));
        // A frontend asking for more than the card has cannot push the page out of it.
        assert_eq!(rect_tuple(clamp(Rect { x: 0, y: 0, width: 4000, height: 3000 }, 800, 600)), (0, 0, 800, 600));
        assert_eq!(rect_tuple(clamp(Rect { x: 700, y: 500, width: 400, height: 300 }, 800, 600)), (700, 500, 100, 100));
        assert_eq!(rect_tuple(clamp(Rect { x: -50, y: -10, width: 400, height: 300 }, 800, 600)), (0, 0, 400, 300));
        assert_eq!(rect_tuple(clamp(Rect { x: 900, y: 700, width: 10, height: 10 }, 800, 600)), (800, 600, 0, 0));
    }

    fn rect_tuple(r: Rect) -> (i32, i32, i32, i32) {
        (r.x, r.y, r.width, r.height)
    }

    #[test]
    fn only_function_keys_may_be_forwarded_unmodified() {
        // F8 (0x77) unmodified, Shift+F11 (0x7A, shift = 1), Ctrl+backslash (0xDC, ctrl = 2).
        let keys = parse_shortcuts(r#"[{"keyCode":119,"modifiers":0},{"keyCode":122,"modifiers":1},{"keyCode":220,"modifiers":2}]"#);
        assert_eq!(keys, vec![119, 122 | (1 << 16), 220 | (2 << 16)]);
        // A letter without modifiers is Chrome's own forbidden case, and so is keyCode 0.
        assert!(parse_shortcuts(r#"[{"keyCode":65,"modifiers":0},{"keyCode":0,"modifiers":2}]"#).is_empty());
        assert!(parse_shortcuts("not json").is_empty());
        assert!(parse_shortcuts("[]").is_empty());
    }

    #[test]
    fn messages_are_split_at_char_boundaries() {
        assert_eq!(split("abc"), vec!["abc"]);
        let big = "é".repeat(CHUNK_BYTES); // 2 bytes each
        let parts = split(&big);
        assert!(parts.len() > 1);
        assert_eq!(parts.concat(), big, "the frontend reassembles exactly what was sent");
        for p in &parts {
            assert!(p.len() <= CHUNK_BYTES);
        }
    }

    #[test]
    fn sta_origins_cover_every_served_host() {
        let json = sta_origins();
        let list: Vec<String> = serde_json::from_str(&json).unwrap();
        for host in crate::scheme::HOSTS {
            assert!(list.contains(&format!("sta://{host}")), "{host} is missing from {json}");
        }
        assert!(list.iter().all(|o| o.starts_with("sta://")));
    }
}
