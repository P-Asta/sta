//! Browser registry, roles and shared lifecycle glue [skeleton, frozen].
//!
//! Responsibility:
//! - [`Surface`]: every HTML UI surface of the shell (host name, URL, overlay or docked).
//! - The registry of **live** browsers (from `on_after_created` to `on_before_close`, both
//!   clients) and their [`Role`] (a UI surface or a tab). Roles are set by BrowserView delegates
//!   (`on_browser_created`, or `on_popup_browser_view_created` for adopted popups).
//! - The common `on_after_created` / `do_close` / `on_before_close` logic both clients call,
//!   including IPC trust bookkeeping, router notification, per-role routing and shutdown progress.
//! - Creating trusted UI BrowserViews ([`create_ui_view`]) and a generic delegate for simple
//!   surfaces ([`SurfaceViewDelegate`]).
//!
//! UI-thread only (thread_local state). Pattern everywhere: copy handles out of a borrow, end the
//! borrow, then call CEF (ARCHITECTURE §2.1).
//!
//! Public API:
//! - `pub enum Surface { Sidebar, Topbar, Empty, CommandBar, FindBar, Permission, Switcher, Toast, PeekHeader, Agent, ExtensionPopup }`
//!   with `host()`, `url()`, `from_host()`, `is_overlay()`
//! - `pub enum Role { Surface(Surface), Tab(Id), DevTools { tab: Id }, ExtensionPopup }`
//! - `pub fn set_role(browser_id: i32, role: Role)`, `pub fn role_of(browser_id: i32) -> Option<Role>`
//! - `pub fn browser(browser_id: i32) -> Option<Browser>`, `pub fn surface_browser(s: Surface) -> Option<Browser>`
//! - `pub fn live_browsers() -> Vec<Browser>`, `pub fn live_count() -> usize`, `pub fn is_ui_browser(id) -> bool`
//! - `pub fn extra_register(browser_id: i32)`, `pub fn extra_unregister(browser_id: i32)`,
//!   `pub fn extra_count() -> usize` — browsers sta doesn't host but must see closed before the
//!   window closes (Chrome-created browsers, foreign.rs). Added to the frozen API for the
//!   extensions work (FINAL PLAN §1); their owners call `window::on_browser_closed` themselves.
//! - `pub fn on_after_created(browser: &Browser, ui: bool)` — called by both clients' LifeSpanHandler
//! - `pub fn on_do_close(browser: &Browser) -> i32` — ditto (return value for `do_close`)
//! - `pub fn on_before_close(browser: &Browser)` — ditto
//! - `pub fn create_ui_view(url: &str, extra: UiExtra, delegate: &mut BrowserViewDelegate) -> Option<BrowserView>`
//! - `pub fn surface_view(surface: Surface, width: i32, height: i32) -> Option<BrowserView>`
//! - `pub fn clear()` — drop every handle before `cef::shutdown()`
//! - `pub fn debug_snapshot() -> serde_json::Value`

use crate::{ipc, overlays, tabs, task, window};
use sta_core::Id;
use cef::wrapper::message_router::MessageRouterBrowserSideHandlerCallbacks;
use cef::*;
use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};

/// An HTML UI surface hosted by the shell (not a tab).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Surface {
    Sidebar,
    Topbar,
    Empty,
    CommandBar,
    FindBar,
    Permission,
    Switcher,
    Toast,
    /// Header strip of the Peek overlay.
    PeekHeader,
    /// AI agent approval prompts and activity panel (automation/ui.rs).
    Agent,
    /// Header strip sta draws above an extension's action popup (ext_popup.rs): icon, name, Options,
    /// ×. The extension's own page is a separate, untrusted browser in the same card.
    ExtensionPopup,
}

#[allow(dead_code)] // part of the stage-2 API
impl Surface {
    pub const ALL: [Surface; 11] = [
        Surface::Sidebar,
        Surface::Topbar,
        Surface::Empty,
        Surface::CommandBar,
        Surface::FindBar,
        Surface::Permission,
        Surface::Switcher,
        Surface::Toast,
        Surface::PeekHeader,
        Surface::Agent,
        Surface::ExtensionPopup,
    ];

    /// Host of the `sta://` URL serving this surface.
    pub fn host(self) -> &'static str {
        match self {
            Surface::Sidebar => "sidebar",
            Surface::Topbar => "topbar",
            Surface::Empty => "empty",
            Surface::CommandBar => "command",
            Surface::FindBar => "find",
            Surface::Permission => "permission",
            Surface::Switcher => "switcher",
            Surface::Toast => "toast",
            Surface::PeekHeader => "peek",
            Surface::Agent => "agent",
            Surface::ExtensionPopup => "extension",
        }
    }

    pub fn url(self) -> String {
        format!("sta://{}/", self.host())
    }

    pub fn from_host(host: &str) -> Option<Surface> {
        Surface::ALL.into_iter().find(|s| s.host() == host)
    }

    /// Lives in an overlay host (overlays.rs) rather than the docked view tree (window.rs).
    pub fn is_overlay(self) -> bool {
        !matches!(self, Surface::Sidebar | Surface::Topbar | Surface::Empty)
    }
}

/// What a browser is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Surface(Surface),
    /// A tab (web or internal page); the id is the core tab id.
    Tab(Id),
    /// The docked DevTools frontend of a tab (devtools.rs). Not a tab: it has no core item, reports
    /// nothing about its document, and is never counted as web content.
    DevTools { tab: Id },
    /// An extension's action popup page, hosted in the popup card (ext_popup.rs). Untrusted web
    /// content with no core item: never a tab, and never trusted by the IPC surface.
    ExtensionPopup,
}

struct Live {
    browser: Browser,
    /// Created with the trusted UI client.
    ui: bool,
}

thread_local! {
    static LIVE: RefCell<BTreeMap<i32, Live>> = const { RefCell::new(BTreeMap::new()) };
    static ROLES: RefCell<HashMap<i32, Role>> = RefCell::new(HashMap::new());
    /// Browsers outside `LIVE` that shutdown waits for (see `extra_register`).
    static EXTRA: RefCell<std::collections::BTreeSet<i32>> = const { RefCell::new(std::collections::BTreeSet::new()) };
}

/// A browser sta doesn't host (no role, not in `LIVE`) whose close the window must wait for.
pub fn extra_register(browser_id: i32) {
    EXTRA.with(|e| e.borrow_mut().insert(browser_id));
}

pub fn extra_unregister(browser_id: i32) {
    EXTRA.with(|e| e.borrow_mut().remove(&browser_id));
}

pub fn extra_count() -> usize {
    EXTRA.with(|e| e.borrow().len())
}

pub fn set_role(browser_id: i32, role: Role) {
    ROLES.with(|r| r.borrow_mut().insert(browser_id, role));
}

pub fn role_of(browser_id: i32) -> Option<Role> {
    ROLES.with(|r| r.borrow().get(&browser_id).copied())
}

pub fn browser(browser_id: i32) -> Option<Browser> {
    LIVE.with(|l| l.borrow().get(&browser_id).map(|l| l.browser.clone()))
}

pub fn is_ui_browser(browser_id: i32) -> bool {
    LIVE.with(|l| l.borrow().get(&browser_id).is_some_and(|l| l.ui))
}

/// The live browser currently showing `surface`, if any.
#[allow(dead_code)] // stage-2 API
pub fn surface_browser(surface: Surface) -> Option<Browser> {
    let id = ROLES.with(|r| r.borrow().iter().find(|(_, role)| **role == Role::Surface(surface)).map(|(id, _)| *id))?;
    browser(id)
}

pub fn live_browsers() -> Vec<Browser> {
    LIVE.with(|l| l.borrow().values().map(|l| l.browser.clone()).collect())
}

pub fn live_count() -> usize {
    LIVE.with(|l| l.borrow().len())
}

/// `LifeSpanHandler::on_after_created` of both clients.
pub fn on_after_created(browser: &Browser, ui: bool) {
    let id = browser.identifier();
    LIVE.with(|l| l.borrow_mut().insert(id, Live { browser: browser.clone(), ui }));
    if ui {
        ipc::trust_browser(id);
    }
    log_debug!("browser {id} created (ui={ui})");
}

/// `LifeSpanHandler::do_close` of both clients. Never releases views synchronously.
///
/// Returning 0 for a Views-hosted browser would close the whole top-level Window
/// (docs/research/views.md §5), so every browser we host returns 1 and its owner posts the
/// detach + release of its BrowserView.
pub fn on_do_close(browser: &Browser) -> i32 {
    let id = browser.identifier();
    match role_of(id) {
        Some(Role::Tab(tab)) => {
            tabs::on_do_close(id, tab);
            1
        }
        Some(Role::DevTools { tab }) => {
            crate::devtools::on_do_close(id, tab);
            1
        }
        // A popup page that closes itself (`window.close()`): CEF tears the browser down and
        // `on_before_close` tells core the card is gone.
        Some(Role::ExtensionPopup) => 1,
        Some(Role::Surface(surface)) => {
            // UI surfaces only close during shutdown; a stray window.close() keeps them alive.
            if window::is_closing() {
                task::post_ui(move || {
                    if surface.is_overlay() {
                        overlays::release_surface(surface);
                    } else {
                        window::release_surface(surface);
                    }
                });
            }
            1
        }
        // Unknown UI browser: keep the main window alive. Unknown web browser (e.g. a popup in a
        // CEF-created window): default handling closes its own window.
        None => i32::from(is_ui_browser(id)),
    }
}

/// `LifeSpanHandler::on_before_close` of both clients.
pub fn on_before_close(browser: &Browser) {
    let id = browser.identifier();
    let live = LIVE.with(|l| l.borrow_mut().remove(&id));
    let role = ROLES.with(|r| r.borrow_mut().remove(&id));
    if live.as_ref().is_some_and(|l| l.ui) {
        ipc::untrust_browser(id);
        ipc::router().on_before_close(Some(browser.clone()));
    }
    drop(live);
    log_debug!("browser {id} closed (role={role:?})");
    match role {
        Some(Role::Tab(tab)) => {
            // A tab browser that dies while a docked DevTools inspects it takes the dock with it.
            crate::devtools::on_inspected_browser_closed(id);
            // …and one that dies mid-translation drops that job rather than answering into nothing.
            crate::translate::on_browser_closed(id);
            tabs::on_before_close(id, tab);
        }
        Some(Role::DevTools { tab }) => crate::devtools::on_frontend_closed(id, tab),
        Some(Role::ExtensionPopup) => crate::ext_popup::on_browser_closed(id),
        Some(Role::Surface(s)) if s.is_overlay() => overlays::on_surface_closed(s, id),
        Some(Role::Surface(s)) => window::on_surface_closed(s, id),
        None => {}
    }
    window::on_browser_closed();
}

/// Extra info passed to the renderer for trusted UI browsers.
#[derive(Debug, Clone, Copy, Default)]
pub struct UiExtra {
    /// Set for internal-page tabs (sta://settings etc.).
    pub tab: Option<Id>,
}

/// Creates a BrowserView with the trusted UI client and `extra_info {sta_ui: true}`.
/// The browser itself is created when the view is added to a Window hierarchy.
pub fn create_ui_view(url: &str, extra: UiExtra, delegate: &mut BrowserViewDelegate) -> Option<BrowserView> {
    let mut client = crate::client::ui_client();
    let mut info = dictionary_value_create()?;
    info.set_bool(Some(&CefString::from("sta_ui")), 1);
    if let Some(tab) = extra.tab {
        info.set_double(Some(&CefString::from("sta_tab")), tab as f64);
    }
    let settings = BrowserSettings { background_color: window::frame_color(), ..Default::default() };
    browser_view_create(
        Some(&mut client),
        Some(&CefString::from(url)),
        Some(&settings),
        Some(&mut info),
        None,
        Some(delegate),
    )
}

/// A UI view for `surface` with a fixed preferred size (use `1` for "let the layout decide").
pub fn surface_view(surface: Surface, width: i32, height: i32) -> Option<BrowserView> {
    let mut delegate = SurfaceViewDelegate::new(surface, width.max(1), height.max(1));
    create_ui_view(&surface.url(), UiExtra::default(), &mut delegate)
}

// Delegate for simple UI surfaces: Alloy style, fixed preferred size, registers the role.
wrap_browser_view_delegate! {
    pub struct SurfaceViewDelegate {
        surface: Surface,
        width: i32,
        height: i32,
    }

    impl ViewDelegate {
        fn preferred_size(&self, _view: Option<&mut View>) -> Size {
            // Both dimensions must be > 0 or CEF ignores the value.
            Size { width: self.width, height: self.height }
        }
    }

    impl BrowserViewDelegate {
        fn on_browser_created(&self, _browser_view: Option<&mut BrowserView>, browser: Option<&mut Browser>) {
            if let Some(browser) = browser {
                set_role(browser.identifier(), Role::Surface(self.surface));
            }
        }

        fn browser_runtime_style(&self) -> RuntimeStyle {
            RuntimeStyle::ALLOY
        }
    }
}

/// Drops every browser handle (call before `cef::shutdown()`).
pub fn clear() {
    let live = LIVE.with(|l| std::mem::take(&mut *l.borrow_mut()));
    ROLES.with(|r| r.borrow_mut().clear());
    EXTRA.with(|e| e.borrow_mut().clear());
    drop(live);
}

/// Registry contents for `debug.info`.
#[cfg_attr(not(debug_assertions), allow(dead_code))] // used by debug.rs only
pub fn debug_snapshot() -> serde_json::Value {
    let live: Vec<serde_json::Value> = LIVE.with(|l| {
        l.borrow()
            .iter()
            .map(|(id, live)| {
                let role = role_of(*id).map(|r| format!("{r:?}"));
                serde_json::json!({ "id": id, "ui": live.ui, "role": role })
            })
            .collect()
    });
    let extra: Vec<i32> = EXTRA.with(|e| e.borrow().iter().copied().collect());
    serde_json::json!({ "live": live, "count": live.len(), "extra": extra })
}
