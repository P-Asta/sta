//! Overlay hosts [owner: chrome] (ARCHITECTURE §4 "Overlay hosts" and positioning bullets).
//!
//! Responsibility:
//! - create the eight overlay hosts in fixed z-order at the end of `on_window_created` (each a
//!   transparent Panel in a CUSTOM-docked, hidden `OverlayController`, above the content corner
//!   masks of rounded.rs); their rounded cards (`rounded::Card`: corner images, border, shadow) and
//!   UI BrowserViews are built lazily on first show — except the command bar, which is pre-warmed;
//! - visibility = requested by an effect **and** the surface sent `ui.ready` at least once (no
//!   blank flash; a show that arrives earlier happens on ready). Peek alone falls back to showing
//!   after [`PEEK_READY_TIMEOUT_MS`] without a ready header: its content is the web page;
//! - hiding the **non-activatable** toast and switcher is acknowledged ([`hide_after_blank`],
//!   `motion.rs`): the page is asked to present a blank frame (`surface.exit {gen}`) and the widget
//!   **lingers** — visible, restacked and still a no-drag hole — until the page's
//!   `surface.exited {gen}` or the cap. A show during the linger cancels it. Everything else hides
//!   at once: an activatable overlay must not keep keyboard focus after the user dismissed it;
//! - activatable overlays (Peek, command bar, find bar, permission, agent) get keyboard focus when
//!   shown or re-targeted — the agent overlay not while the user types (`automation::ui`);
//! - positioning (window coordinates) relative to the content panel rect and the focused pane,
//!   recomputed on window layout, content/sidebar changes and `surface.setSize` (clamped). The
//!   rects below are the visible **card** (outer border edge): a page's `surface.setSize` is its
//!   content size, the card adds its inner chrome (`CardSpec::inner`), and the overlay host adds
//!   the shadow and is snapped to the device-pixel grid (`rounded::host_rect`):
//!   command bar centered, width `clamp(480, 56%, 680)`, top `max(72, 14%)`; find bar top-right
//!   and permission prompt top-left of the target pane (8 px inset); agent overlay top-right of
//!   the content (8 px inset), width `clamp(380, 300, 460)`; switcher centered (shown
//!   250 ms after `ShowSwitcher` unless hidden meanwhile); toast bottom-center 12 px above the
//!   content bottom; Peek `min(content_w − 96, 1200)` × `content_h − 56`, centered, top +28;
//! - Peek: vertical Panel [header BrowserView `sta://peek/` (40) | the tab's BrowserView moved
//!   in with `tabs::take_view_for_peek`]; `HidePeek` only hides; `ShowContent` including the tab
//!   takes it back (`take_back_peek_view` → `tabs::return_view_from_peek`). Page fullscreen of the
//!   Peek tab also takes the view back (tabs.rs shows it alone in its wrapper) and re-shows Peek
//!   when fullscreen ends;
//! - z-order: showing an overlay's widget raises it above every other overlay, whatever the
//!   creation order. So whenever an overlay becomes visible, the visible overlays that belong above
//!   it (`Overlay::Z_ORDER`) are re-shown in order ([`restack_overlays_above`]); e.g. a permission
//!   prompt or a command bar that was up before Peek appeared stays above Peek. Re-showing an
//!   activatable overlay moves keyboard focus through it (hiding a focused one hands focus to the
//!   main window), so those transient focus changes are ignored ([`FOCUS_GUARD`]) and the intended
//!   focus target is requested again; once the guard ends, the focus policy runs once for the
//!   browser that ends up focused. Peek never takes focus from a visible command bar, find bar,
//!   permission prompt or agent overlay above it (they work on Peek; e.g. Peek becoming ready late
//!   while the user types in the find bar);
//! - the floating sidebar host (`SidebarHover`, not activatable, above the focus-taking overlays):
//!   window.rs parks the hidden sidebar's BrowserView in it; sidebar_hover.rs shows and hides it;
//!   card `{8, 8, min(sidebar width + 8, client w - 16), client h - 16}` (the page keeps the
//!   sidebar width), its host snapped inwards so the shadow stays clear of the resize bands;
//! - the content corner masks (rounded.rs) are the lowest overlays: a mask that is shown rises
//!   above everything, so every visible overlay is re-shown ([`restack_all_visible`]);
//! - every visible overlay is a no-drag hole in the window drag regions
//!   (`visible_overlay_bounds`);
//! - focus policy ([`on_browser_got_focus`]): a browser other than the command bar gaining focus
//!   closes an open command bar (`CloseCommandBar`); a browser other than Peek's tab, Peek's header
//!   or another activatable overlay (command bar, find bar, permission prompt, agent overlay: they
//!   work *on* Peek) gaining focus closes Peek (`ClosePeek{focusLost: true}`; core ignores it for
//!   popups); a browser other than the agent overlay gaining focus closes the agent activity panel
//!   (`CloseAgentPanel{focusLost: true}`; core ignores it while a prompt is shown).
//!   Keyboard focus never stays in a hidden overlay: Views may hand focus to the pre-warmed (hidden)
//!   command bar at startup, and hiding an overlay that has focus leaves it there when core has no
//!   tab to refocus. A hidden overlay swallows key-downs (no accelerators fire), so focus is moved
//!   to the focused tab, else the empty-state view / sidebar ([`restore_main_focus`]).
//!
//! Public API:
//! - `pub enum Overlay { Peek, CommandBar, FindBar, ExtensionPopup, Permission, Agent, SidebarHover, Switcher, Toast }`
//! - `pub fn create_hosts(window: &Window)`, `pub fn clear()`
//! - `pub fn surface_ready(browser_id: i32)`, `pub fn set_surface_size(browser_id: i32, width: Option<i32>, height: i32)`
//! - `pub fn layout()`, `pub fn on_chrome_colors_changed()`
//! - `pub fn show_command_bar()`, `hide_command_bar()`, `show_find_bar(tab)`, `hide_find_bar()`,
//!   `show_switcher()`, `hide_switcher()`, `show_toast()`, `hide_toast()`,
//!   `show_permission_prompt(tab)`, `hide_permission_prompt()`, `show_agent_overlay()`,
//!   `hide_agent_overlay()` (AI agent prompts / activity panel, top-right of the content; an
//!   approval prompt doesn't take focus while the user types, `automation::ui`), `show_peek(tab)`,
//!   `hide_peek(tab)`
//! - `pub fn take_back_peek_view(tab: Id) -> Option<BrowserView>` — detach a tab view from Peek
//! - extension popup card (ext_popup.rs): `pub fn adopt_extension_popup(view, tab)`,
//!   `pub fn show_extension_popup(tab)`, `pub fn hide_extension_popup()`,
//!   `pub fn set_extension_popup_size(width, height)`, `pub fn extension_popup_failed()`
//! - floating sidebar: `pub fn adopt_sidebar(view)`, `pub fn release_sidebar(view)`,
//!   `pub fn show_sidebar_hover()`, `pub fn hide_sidebar_hover()`, `pub fn sidebar_hover_rect() -> Rect`
//! - `pub fn restack_all_visible()` (rounded.rs, after showing a corner mask),
//!   `Overlay::card_spec(self) -> CardSpec`
//! - `pub fn is_visible(o: Overlay) -> bool`, `pub fn is_switcher_requested() -> bool`,
//!   `pub fn peek_tab() -> Option<Id>`, `pub fn visible_overlay_bounds() -> Vec<Rect>`,
//!   `pub fn overlay_browser_id(o: Overlay) -> Option<i32>`
//! - `pub fn root_origin_in_window(view: &View) -> Option<Point>` — for `window::view_rect_in_window`
//! - `pub fn on_browser_got_focus(browser_id: i32)`, `pub fn focused_browser() -> Option<i32>`,
//!   `pub fn restore_main_focus()`, `pub fn focus_events_suppressed() -> bool` (client.rs skips
//!   `TabFocused` while a restack moves focus around)
//! - `pub fn release_surface(surface: Surface)`, `pub fn on_surface_closed(surface: Surface, browser_id: i32)`
//! - `pub fn debug_snapshot() -> serde_json::Value`

use crate::browsers::{self, Role, Surface};
use crate::rounded::{self, Card, CardSpec, Orientation, Palette};
use crate::{controller, ipc, motion, tabs, task, window};
use sta_core::{Command, Id};
use cef::*;
use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;

/// Height of the Peek header strip (DIP).
pub const PEEK_HEADER_HEIGHT: i32 = 40;
/// Delay before the Ctrl+Tab switcher becomes visible.
const SWITCHER_DELAY_MS: i64 = 250;
/// Peek shows without a ready header after this long.
pub const PEEK_READY_TIMEOUT_MS: i64 = 2000;
/// Inset of pane-anchored overlays (find bar, permission prompt, the extension popup card).
const PANE_INSET: i32 = 8;
/// The popup card drops below a visible find bar (its default height plus the inset).
const FIND_BAR_STACK: i32 = 52;
/// Chrome's action popup limits (S4): a popup is at least 25×25 and at most 800×600.
const EXT_POPUP_MIN: i32 = 25;
/// The card of a popup that never rendered: wide enough for its one honest sentence, high enough for
/// the header row above it (the extension's own page is gone, so the header fills the card).
const EXT_POPUP_FAILED: (i32, i32) = (320, 56);
pub const EXT_POPUP_MAX_W: i32 = 800;
pub const EXT_POPUP_MAX_H: i32 = 600;
/// Inset of the floating sidebar from the window edges (DIP).
const SIDEBAR_HOVER_INSET: i32 = 8;
/// How long focus changes caused by a restack are ignored (they are synchronous in practice; the
/// margin covers notifications posted by Chromium).
const FOCUS_GUARD_MS: i64 = 50;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Overlay {
    Peek,
    CommandBar,
    FindBar,
    /// An extension's action popup: sta's header strip above the extension's own page
    /// (ext_popup.rs). **Below** `Permission` (SEC-4): a card must never cover a prompt.
    ExtensionPopup,
    Permission,
    /// AI agent approval prompts and the agent activity panel (automation/ui.rs): top-right of
    /// the content. Takes keyboard focus when shown, except a prompt while the user is typing.
    Agent,
    /// The floating sidebar (hover reveal): the parked sidebar BrowserView of window.rs.
    SidebarHover,
    Switcher,
    Toast,
}

impl Overlay {
    /// Creation order = z-order, lowest first (the derived `Ord` follows it). The floating sidebar
    /// sits above the overlays that take keyboard focus, so revealing it never re-shows (and
    /// re-focuses) them.
    pub const Z_ORDER: [Overlay; 9] = [
        Overlay::Peek,
        Overlay::CommandBar,
        Overlay::FindBar,
        Overlay::ExtensionPopup,
        Overlay::Permission,
        Overlay::Agent,
        Overlay::SidebarHover,
        Overlay::Switcher,
        Overlay::Toast,
    ];

    pub fn surface(self) -> Surface {
        match self {
            Overlay::Peek => Surface::PeekHeader,
            Overlay::CommandBar => Surface::CommandBar,
            Overlay::FindBar => Surface::FindBar,
            Overlay::ExtensionPopup => Surface::ExtensionPopup,
            Overlay::Permission => Surface::Permission,
            Overlay::Agent => Surface::Agent,
            Overlay::SidebarHover => Surface::Sidebar,
            Overlay::Switcher => Surface::Switcher,
            Overlay::Toast => Surface::Toast,
        }
    }

    /// The overlay a surface's view lives in. Never `SidebarHover`: the sidebar is a docked
    /// surface that is only parked there (see `browser_in_overlay`).
    pub fn from_surface(surface: Surface) -> Option<Overlay> {
        Overlay::Z_ORDER.into_iter().filter(|o| *o != Overlay::SidebarHover).find(|o| o.surface() == surface)
    }

    /// Takes keyboard focus when shown.
    pub fn can_activate(self) -> bool {
        matches!(
            self,
            Overlay::Peek | Overlay::CommandBar | Overlay::FindBar | Overlay::ExtensionPopup | Overlay::Permission | Overlay::Agent
        )
    }

    /// Requests keyboard focus when shown or re-targeted (an agent prompt doesn't while the user
    /// is typing, so keys can't land on it).
    fn takes_focus(self) -> bool {
        self.can_activate() && (self != Overlay::Agent || crate::automation::ui::overlay_may_take_focus())
    }

    /// The rounded card around the overlay's page (rounded.rs tokens).
    pub fn card_spec(self) -> CardSpec {
        let base = CardSpec {
            radius: rounded::OVERLAY_RADIUS,
            shadow: rounded::OVERLAY_SHADOW,
            pad: rounded::OVERLAY_PAD,
            orientation: Orientation::Rows,
            vertical_content: false,
            palette: Palette::Surface,
        };
        match self {
            Overlay::Peek => CardSpec { pad: rounded::PEEK_PAD, vertical_content: true, ..base },
            // sta's header strip sits above the extension's page, like Peek's.
            Overlay::ExtensionPopup => CardSpec { vertical_content: true, ..base },
            Overlay::CommandBar | Overlay::Permission | Overlay::Agent | Overlay::Switcher => base,
            Overlay::FindBar => CardSpec { radius: rounded::FIND_RADIUS, orientation: Orientation::Columns, ..base },
            Overlay::Toast => CardSpec { radius: rounded::TOAST_RADIUS, orientation: Orientation::Columns, ..base },
            Overlay::SidebarHover => CardSpec { shadow: rounded::SIDEBAR_SHADOW, palette: Palette::Frame, ..base },
        }
    }
}

struct Host {
    /// What the overlay controller hosts. The card's root panel itself — except for the floating
    /// sidebar, where it is a bare clip panel one level above it ([`rounded::clip_root`]): that card
    /// keeps its full size while it slides and only the slice inside the clip is drawn.
    contents: Panel,
    /// A transparent panel the card is built into.
    panel: Panel,
    /// Built on first use ([`ensure_card`]); its inner panel parents the views.
    card: Option<Card>,
    controller: OverlayController,
    /// The surface's UI view (Peek: the header).
    view: Option<BrowserView>,
    browser_id: Option<i32>,
    ready: bool,
    wanted: bool,
    /// Peek only: shown without a ready header after the timeout.
    ready_timeout: bool,
    /// Size requested by the page via `surface.setSize` (DIP).
    page_width: Option<i32>,
    page_height: Option<i32>,
    /// Target tab (find bar, permission prompt, Peek, the extension popup card).
    tab: Option<Id>,
    /// Peek only: the tab view currently parented in this host.
    peek_view: Option<BrowserView>,
    /// ExtensionPopup only: the extension's own page, below sta's header strip (ext_popup.rs).
    popup_view: Option<BrowserView>,
    /// ExtensionPopup only: the popup never rendered, so sta's header *is* the card and its height
    /// is the whole card's ([`extension_popup_failed`]).
    popup_failed: bool,
}

thread_local! {
    static HOSTS: RefCell<BTreeMap<Overlay, Host>> = const { RefCell::new(BTreeMap::new()) };
    static SWITCHER_GEN: Cell<u64> = const { Cell::new(0) };
    static SWITCHER_REQUESTED: Cell<bool> = const { Cell::new(false) };
    static PEEK_GEN: Cell<u64> = const { Cell::new(0) };
    static FOCUSED_BROWSER: Cell<Option<i32>> = const { Cell::new(None) };
    /// Nesting depth of restacks whose transient focus changes must not trigger the focus policy.
    static FOCUS_GUARD: Cell<u32> = const { Cell::new(0) };
    /// How far left of its home the floating sidebar host is drawn while it slides (DIP, ≤ 0).
    static SIDEBAR_SLIDE: Cell<i32> = const { Cell::new(0) };
    /// How much of the floating sidebar's card is cut off at the window's left edge right now (DIP,
    /// ≥ 0): what its clip panel was last laid out for (`rounded::set_clip_cut`).
    static SIDEBAR_CUT: Cell<i32> = const { Cell::new(0) };
}

// ----------------------------------------------------------------------------------- creation

/// Creates every overlay host (call last in `on_window_created`) and pre-warms the command bar.
pub fn create_hosts(window: &Window) {
    for overlay in Overlay::Z_ORDER {
        let Some(panel) = rounded::card_root(&overlay.card_spec()) else { continue };
        let contents = if overlay == Overlay::SidebarHover { rounded::clip_root(&panel) } else { Some(panel.clone()) };
        let Some(contents) = contents else { continue };
        let Some(controller) =
            window.add_overlay_view(Some(&mut View::from(&contents)), DockingMode::CUSTOM, overlay.can_activate() as i32)
        else {
            log_error!("add_overlay_view({overlay:?}) failed");
            continue;
        };
        controller.set_visible(0);
        let host = Host {
            contents,
            panel,
            card: None,
            controller,
            view: None,
            browser_id: None,
            ready: false,
            wanted: false,
            ready_timeout: false,
            page_width: None,
            page_height: None,
            tab: None,
            peek_view: None,
            popup_view: None,
            popup_failed: false,
        };
        HOSTS.with(|h| h.borrow_mut().insert(overlay, host));
        if overlay == Overlay::Peek {
            rounded::create_peek_masks(window);
        }
    }
    ensure_view(Overlay::CommandBar);
}

/// Builds the overlay's rounded card on first use; returns the panel that parents its views.
fn ensure_card(overlay: Overlay) -> Option<Panel> {
    let (root, inner) = HOSTS.with(|h| h.borrow().get(&overlay).map(|host| (host.panel.clone(), host.card.as_ref().map(|c| c.inner.clone()))))?;
    if inner.is_some() {
        return inner;
    }
    let card = rounded::build_card(&root, overlay.card_spec())?;
    let inner = card.inner.clone();
    HOSTS.with(|h| {
        if let Some(host) = h.borrow_mut().get_mut(&overlay) {
            host.card = Some(card);
        }
    });
    Some(inner)
}

/// The panel that parents an overlay's views, once its card is built.
fn view_parent(overlay: Overlay) -> Option<Panel> {
    HOSTS.with(|h| h.borrow().get(&overlay).and_then(|host| host.card.as_ref().map(|c| c.inner.clone())))
}

/// Adds `view` to a card's inner panel (at `index`, else last); `flex`: it fills the card (not the
/// Peek header, which keeps its preferred height).
fn add_to_card(parent: &Panel, view: &BrowserView, index: Option<i32>, flex: bool) {
    match index {
        Some(i) => parent.add_child_view_at(Some(&mut View::from(view)), i),
        None => parent.add_child_view(Some(&mut View::from(view))),
    }
    if flex && let Some(layout) = parent.get_layout().and_then(|l| l.as_box_layout()) {
        layout.set_flex_for_view(Some(&mut View::from(view)), 1);
    }
}

/// Removes `view` from a card's inner panel. Its box-layout flex is cleared first: Views stores it
/// on the view itself, so it would follow the view into its next parent (a docked sidebar with
/// flex 1 took half the window).
fn remove_from_card(parent: &Panel, view: &BrowserView) {
    if let Some(layout) = parent.get_layout().and_then(|l| l.as_box_layout()) {
        layout.clear_flex_for_view(Some(&mut View::from(view)));
    }
    parent.remove_child_view(Some(&mut View::from(view)));
}

/// Creates the overlay's UI view on first use.
fn ensure_view(overlay: Overlay) {
    if HOSTS.with(|h| h.borrow().get(&overlay).is_none_or(|host| host.view.is_some())) {
        return;
    }
    let Some(panel) = ensure_card(overlay) else { return };
    // Peek and the extension popup card put sta's own header strip above a guest page, so their
    // surface keeps its preferred height instead of filling the card.
    let header = matches!(overlay, Overlay::Peek | Overlay::ExtensionPopup);
    let height = if header { PEEK_HEADER_HEIGHT } else { 1 };
    let Some(view) = browsers::surface_view(overlay.surface(), 1, height) else {
        log_error!("cannot create the {overlay:?} view");
        return;
    };
    HOSTS.with(|h| {
        if let Some(host) = h.borrow_mut().get_mut(&overlay) {
            host.view = Some(view.clone());
        }
    });
    // The header is always the first child of such a card. The browser is created here.
    add_to_card(&panel, &view, Some(0), !header);
    if let Some(browser) = view.browser() {
        let id = browser.identifier();
        HOSTS.with(|h| {
            if let Some(host) = h.borrow_mut().get_mut(&overlay) {
                host.browser_id = Some(id);
            }
        });
    }
}

// ----------------------------------------------------------------------------------- visibility

fn overlay_of_browser(browser_id: i32) -> Option<Overlay> {
    match browsers::role_of(browser_id)? {
        Role::Surface(s) => Overlay::from_surface(s),
        Role::Tab(_) | Role::DevTools { .. } | Role::ExtensionPopup => None,
    }
}

/// `ui.ready` from a surface (the calling browser identifies it).
pub fn surface_ready(browser_id: i32) {
    if browsers::role_of(browser_id) == Some(Role::Surface(Surface::Sidebar)) {
        crate::sidebar_hover::on_sidebar_ready();
        return;
    }
    let Some(overlay) = overlay_of_browser(browser_id) else { return };
    let first = HOSTS.with(|h| {
        let mut hosts = h.borrow_mut();
        let host = hosts.get_mut(&overlay)?;
        host.browser_id = Some(browser_id);
        Some(!std::mem::replace(&mut host.ready, true))
    });
    if first == Some(true) {
        log_debug!("{overlay:?} surface ready (browser {browser_id})");
    }
    apply_visibility(overlay, false);
}

fn request(overlay: Overlay, wanted: bool, tab: Option<Id>) {
    if wanted {
        ensure_view(overlay);
        // Shown again while it was lingering out ([`hide_after_blank`]): the exit is cancelled by
        // generation, so the widget simply stays up and nothing hides it a moment later.
        if let Some(id) = overlay_browser_id(overlay) {
            motion::cancel(id);
        }
    }
    let known = HOSTS.with(|h| {
        let mut hosts = h.borrow_mut();
        let Some(host) = hosts.get_mut(&overlay) else { return false };
        host.wanted = wanted;
        if tab.is_some() {
            host.tab = tab;
        }
        true
    });
    if known {
        apply_visibility(overlay, wanted);
    }
}

fn apply_visibility(overlay: Overlay, refocus: bool) {
    let Some((controller, view, peek_view, should)) = HOSTS.with(|h| {
        h.borrow().get(&overlay).map(|host| {
            let ready = host.ready || (overlay == Overlay::Peek && host.ready_timeout);
            // The card focuses its guest page (Peek's tab, the extension's popup), not the header.
            let guest = host.peek_view.clone().or_else(|| host.popup_view.clone());
            (host.controller.clone(), host.view.clone(), guest, host.wanted && ready)
        })
    }) else {
        return;
    };
    let is = controller.is_visible() != 0;
    if should {
        let focused_before = FOCUSED_BROWSER.get();
        // Peek doesn't take focus from an overlay above it that has it (they work on Peek).
        let keep_focus = (overlay == Overlay::Peek)
            .then(|| focused_before.and_then(browser_in_overlay))
            .flatten()
            .filter(|o| *o > overlay && o.can_activate() && is_visible(*o));
        let mut guarded = false;
        if !is {
            layout_overlay(overlay);
            controller.set_visible(1);
            if overlay == Overlay::Peek {
                // The page's corner masks go right above Peek, below the overlays restacked next.
                rounded::layout_peek_masks(peek_page_rect().as_ref(), true);
            }
            guarded = restack_overlays_above(overlay);
        }
        let own_view = peek_view.or(view);
        let target: Option<BrowserView> = if (!is || refocus) && overlay.takes_focus() {
            match keep_focus {
                Some(o) => overlay_focus_view(o),
                // Peek focuses the page, not its header.
                None => own_view.clone(),
            }
        } else if guarded {
            // The restack moved focus through re-shown overlays: give it back.
            focused_before.and_then(focus_view_of_browser)
        } else {
            None
        };
        if let Some(v) = target {
            // An overlay that had focus and was re-shown: Views still counts its view as focused
            // while its page lost focus with the hidden widget, so a plain request would be a
            // no-op. Pass focus through the overlay just shown first (all within the guard).
            let refocus_restacked = guarded && focused_before.and_then(browser_in_overlay).is_some_and(|o| o > overlay);
            if refocus_restacked
                && overlay.can_activate()
                && let Some(bounce) = own_view
            {
                bounce.request_focus();
            }
            v.request_focus();
        }
        if guarded {
            end_focus_guard(focused_before);
        }
    } else if is {
        controller.set_visible(0);
        if overlay == Overlay::Peek {
            rounded::layout_peek_masks(None, false);
        }
        let focused_here = FOCUSED_BROWSER.get().is_some_and(|id| browser_in_overlay(id) == Some(overlay));
        if focused_here {
            task::post_ui(restore_main_focus);
        }
    }
    if should != is {
        log_debug!("overlay {overlay:?} {}", if should { "shown" } else { "hidden" });
        window::schedule_draggable_regions();
    }
}

/// Showing an overlay stacks its widget above every other overlay, whatever the creation order
/// (e.g. Peek opened while a toast, a permission prompt or the command bar is up would cover
/// them). Re-show the visible overlays that belong above it, lowest first: hiding and showing
/// again raises a widget. Non-activatable ones (switcher, toast) never move keyboard focus;
/// activatable ones do, so the focus guard starts before the first of them. Returns whether the
/// guard was started (the caller requests the intended focus and ends it).
fn restack_overlays_above(shown: Overlay) -> bool {
    let above: Vec<(Overlay, OverlayController)> = HOSTS.with(|h| {
        let hosts = h.borrow();
        Overlay::Z_ORDER
            .iter()
            .skip_while(|o| **o != shown)
            .skip(1)
            .filter_map(|o| hosts.get(o).map(|host| (*o, host.controller.clone())))
            .collect()
    });
    let mut guarded = false;
    for (overlay, controller) in above.into_iter().filter(|(_, c)| c.is_visible() != 0) {
        if overlay.can_activate() && !guarded {
            FOCUS_GUARD.set(FOCUS_GUARD.get() + 1);
            guarded = true;
        }
        layout_overlay(overlay);
        controller.set_visible(0);
        controller.set_visible(1);
        log_debug!("overlay {overlay:?} restacked above {shown:?}");
    }
    guarded
}

/// Ends a focus guard after the focus notifications of the restack were delivered, then applies
/// the focus policy once to the browser that has focus now (if it changed).
fn end_focus_guard(focused_before: Option<i32>) {
    task::post_ui_delayed(FOCUS_GUARD_MS, move || {
        FOCUS_GUARD.set(FOCUS_GUARD.get().saturating_sub(1));
        if FOCUS_GUARD.get() > 0 || window::is_closing() {
            return;
        }
        let now = FOCUSED_BROWSER.get();
        if now != focused_before
            && let Some(id) = now
        {
            if let Some(Role::Tab(tab)) = browsers::role_of(id) {
                controller::dispatch(Command::TabFocused { tab });
            }
            on_browser_got_focus(id);
        }
    });
}

/// Focus changes are being caused by an overlay restack (client.rs doesn't report `TabFocused`).
pub fn focus_events_suppressed() -> bool {
    FOCUS_GUARD.get() > 0
}

/// Re-shows every visible overlay, lowest first: a content corner mask (rounded.rs, below every
/// overlay) was just shown and rose above them. Keyboard focus stays where it was: re-showing an
/// activatable overlay moves focus through it, so the focus guard runs and the target is focused
/// again (through another view first when its own overlay was re-shown: Views still counts that
/// view as focused while its page lost focus with the hidden widget).
pub fn restack_all_visible() {
    if window::is_closing() {
        return;
    }
    let hosts: Vec<(Overlay, OverlayController)> = HOSTS.with(|h| {
        let hosts = h.borrow();
        Overlay::Z_ORDER.iter().filter_map(|o| hosts.get(o).map(|host| (*o, host.controller.clone()))).collect()
    });
    let visible: Vec<(Overlay, OverlayController)> = hosts.into_iter().filter(|(_, c)| c.is_visible() != 0).collect();
    if visible.is_empty() {
        return;
    }
    let focused_before = FOCUSED_BROWSER.get();
    let guarded = visible.iter().any(|(o, _)| o.can_activate());
    if guarded {
        FOCUS_GUARD.set(FOCUS_GUARD.get() + 1);
    }
    for (overlay, controller) in &visible {
        controller.set_visible(0);
        controller.set_visible(1);
        if *overlay == Overlay::Peek {
            rounded::reshow_peek_masks();
        }
        log_debug!("overlay {overlay:?} restacked above the corner masks");
    }
    if guarded {
        if let Some(target) = focused_before.filter(|id| is_shown_browser(*id)).and_then(focus_view_of_browser) {
            let owner_restacked = focused_before.and_then(browser_in_overlay).is_some_and(|o| visible.iter().any(|(v, _)| *v == o));
            if owner_restacked && let Some(bounce) = window::focus_fallback_view() {
                bounce.request_focus();
            }
            target.request_focus();
        }
        end_focus_guard(focused_before);
    }
}

/// The view that takes keyboard focus for an overlay (Peek: its page; the popup card: the popup).
fn overlay_focus_view(overlay: Overlay) -> Option<BrowserView> {
    HOSTS.with(|h| h.borrow().get(&overlay).and_then(|host| host.peek_view.clone().or_else(|| host.popup_view.clone()).or_else(|| host.view.clone())))
}

/// `browser_id` is a live browser on screen: a visible overlay's surface, a visible tab (content or
/// Peek) or a drawn docked surface (sidebar, top bar, empty state).
fn is_shown_browser(browser_id: i32) -> bool {
    if tabs::is_closing_browser(browser_id) {
        return false;
    }
    match browsers::role_of(browser_id) {
        // A docked DevTools frontend is on screen exactly when its tab is.
        Some(Role::Tab(tab)) | Some(Role::DevTools { tab }) => tabs::is_tab_visible(tab),
        Some(Role::ExtensionPopup) => is_visible(Overlay::ExtensionPopup),
        Some(Role::Surface(Surface::Sidebar)) => window::sidebar_on_screen(),
        Some(Role::Surface(surface)) => match Overlay::from_surface(surface) {
            Some(overlay) => is_visible(overlay),
            None => window::docked_view(surface).is_some_and(|v| v.is_drawn() != 0),
        },
        None => false,
    }
}

/// The BrowserView showing `browser_id` (an overlay surface, a Peek or content tab, or a docked
/// UI surface).
fn focus_view_of_browser(browser_id: i32) -> Option<BrowserView> {
    match browsers::role_of(browser_id)? {
        Role::Tab(tab) => tabs::view_for_tab(tab),
        Role::DevTools { tab } => crate::devtools::frontend_view(tab),
        Role::ExtensionPopup => HOSTS.with(|h| h.borrow().get(&Overlay::ExtensionPopup).and_then(|host| host.popup_view.clone())),
        Role::Surface(surface) => match Overlay::from_surface(surface) {
            Some(overlay) => HOSTS.with(|h| h.borrow().get(&overlay).and_then(|host| host.view.clone())),
            None => window::docked_view(surface),
        },
    }
}

pub fn is_visible(overlay: Overlay) -> bool {
    let controller = HOSTS.with(|h| h.borrow().get(&overlay).map(|host| host.controller.clone()));
    controller.is_some_and(|c| c.is_visible() != 0)
}

fn is_wanted(overlay: Overlay) -> bool {
    HOSTS.with(|h| h.borrow().get(&overlay).is_some_and(|host| host.wanted))
}

/// `ShowSwitcher` was received and not yet cancelled (the overlay may still be delayed).
pub fn is_switcher_requested() -> bool {
    SWITCHER_REQUESTED.get()
}

/// The tab whose view is in the Peek overlay while Peek is requested.
pub fn peek_tab() -> Option<Id> {
    HOSTS.with(|h| h.borrow().get(&Overlay::Peek).filter(|host| host.wanted).and_then(|host| host.tab))
}

pub fn overlay_browser_id(overlay: Overlay) -> Option<i32> {
    HOSTS.with(|h| h.borrow().get(&overlay).and_then(|host| host.browser_id))
}

/// Bounds (window coordinates) of visible overlays, for draggable-region holes.
pub fn visible_overlay_bounds() -> Vec<Rect> {
    let controllers: Vec<OverlayController> = HOSTS.with(|h| h.borrow().values().map(|host| host.controller.clone()).collect());
    controllers.into_iter().filter(|c| c.is_visible() != 0).map(|c| c.bounds()).collect()
}

/// Origin (window coordinates) of `root` when it is an overlay host's contents panel.
pub fn root_origin_in_window(root: &View) -> Option<Point> {
    let hosts: Vec<(Panel, OverlayController)> =
        HOSTS.with(|h| h.borrow().values().map(|host| (host.contents.clone(), host.controller.clone())).collect());
    hosts.into_iter().find(|(panel, _)| View::from(panel).is_same(Some(&mut root.clone())) != 0).map(|(_, c)| {
        let b = c.bounds();
        Point { x: b.x, y: b.y }
    })
}

// ----------------------------------------------------------------------------------- layout

/// Recomputes the bounds of every overlay (window `on_layout_changed`, content/sidebar changes).
pub fn layout() {
    for overlay in Overlay::Z_ORDER {
        layout_overlay(overlay);
    }
    if !visible_overlay_bounds().is_empty() {
        window::schedule_draggable_regions();
    }
}

/// Rect of the pane an overlay is anchored to: the tab's view in Peek, else its wrapper in the
/// content area, else the whole content rect.
fn pane_rect(tab: Option<Id>, content: &Rect) -> Rect {
    let Some(tab) = tab else { return content.clone() };
    let peek_view = HOSTS.with(|h| {
        h.borrow().get(&Overlay::Peek).filter(|host| host.wanted && host.tab == Some(tab)).and_then(|host| host.peek_view.clone())
    });
    if let Some(view) = peek_view
        && is_visible(Overlay::Peek)
        && let Some(r) = window::view_rect_in_window(&View::from(&view)).filter(|r| r.width > 0 && r.height > 0)
    {
        return r;
    }
    tabs::tab_rect_in_window(tab).filter(|r| r.width > 0 && r.height > 0).unwrap_or_else(|| content.clone())
}

fn clamp(v: i32, lo: i32, hi: i32) -> i32 {
    v.max(lo).min(hi.max(lo))
}

fn layout_overlay(overlay: Overlay) {
    let Some((controller, contents, root, page_w, page_h, tab, failed)) = HOSTS.with(|h| {
        h.borrow().get(&overlay).map(|host| {
            (host.controller.clone(), host.contents.clone(), host.panel.clone(), host.page_width, host.page_height, host.tab, host.popup_failed)
        })
    }) else {
        return;
    };
    let Some(card) = card_rect(overlay, page_w, page_h, tab, failed) else { return };
    // The floating sidebar's host is snapped inwards: its shadow stays clear of the resize bands.
    let mut host = rounded::host_rect(&card, &overlay.card_spec(), rounded::window_snap_unit(), overlay != Overlay::SidebarHover);
    if overlay == Overlay::SidebarHover {
        // It is drawn `SIDEBAR_SLIDE` DIP further left while it slides in or out (motion.rs). An
        // overlay cannot hang outside the window (CEF fits its bounds to it), so the *host* is cut
        // down to the slice that is inside the window — and the card in it is not: it keeps its
        // settled size and is placed `cut` DIP left of the host, where Views clips it. The page is
        // therefore never resized during a slide, only moved. (Resizing it per step is what this
        // replaced: every step was a new viewport the renderer had to lay out and raster before the
        // compositor could show it, and the slide visibly dropped frames.) Never 0 wide: a widget
        // with no width has no page to paint the next frame from.
        let home_width = host.width;
        host.x += SIDEBAR_SLIDE.get().min(0);
        if host.x < 0 {
            host.width = (host.width + host.x).max(1);
            host.x = 0;
        }
        // `home_width - host.width`, not `-host.x`: the two differ once the width is held at 1.
        let cut = home_width - host.width;
        if SIDEBAR_CUT.replace(cut) != cut {
            // The cut first, the width right after it: one layout pass sees both (`set_clip_cut`).
            rounded::set_clip_cut(&contents, &root, cut);
            if !set_bounds_if_changed(&controller, host) {
                contents.layout();
            }
            return;
        }
    }
    set_bounds_if_changed(&controller, host);
    if overlay == Overlay::Peek && controller.is_visible() != 0 {
        // Follow the page; a hidden mask is only shown together with Peek (z-order).
        rounded::layout_peek_masks(peek_page_rect().as_ref(), false);
    }
}

/// The peeked page's rect (window coordinates) inside the visible Peek card: the host without its
/// chrome and the header.
fn peek_page_rect() -> Option<Rect> {
    let controller = HOSTS.with(|h| h.borrow().get(&Overlay::Peek).filter(|host| host.peek_view.is_some()).map(|host| host.controller.clone()))?;
    let (cx, cy) = Overlay::Peek.card_spec().chrome();
    let inner = rounded::inset(&controller.bounds(), cx, cy);
    (inner.height > PEEK_HEADER_HEIGHT).then(|| Rect { y: inner.y + PEEK_HEADER_HEIGHT, height: inner.height - PEEK_HEADER_HEIGHT, ..inner })
}

/// The visible card of an overlay (outer border edge, window coordinates, before snapping) for its
/// page's requested content size.
fn card_rect(overlay: Overlay, page_w: Option<i32>, page_h: Option<i32>, tab: Option<Id>, failed_popup: bool) -> Option<Rect> {
    if overlay == Overlay::SidebarHover {
        return Some(sidebar_hover_rect());
    }
    let c = window::content_rect()?;
    if c.width <= 0 || c.height <= 0 {
        return None;
    }
    // Page content size → card size.
    let (ix, iy) = overlay.card_spec().inner();
    let card_w = |default: i32| page_w.map(|w| w + 2 * ix).unwrap_or(default);
    let card_h = |default: i32| page_h.map(|h| h + 2 * iy).unwrap_or(default);
    Some(match overlay {
        Overlay::SidebarHover => return None,
        Overlay::CommandBar => {
            let width = clamp((c.width as f32 * 0.56) as i32, 480, 680).min(c.width - 16).max(1);
            let top = c.y + 72.max((c.height as f32 * 0.14) as i32);
            let max_h = c.y + c.height - top - 16;
            let height = clamp(card_h(56), 56, max_h);
            Rect { x: c.x + (c.width - width) / 2, y: top, width, height }
        }
        Overlay::FindBar => {
            let p = pane_rect(tab, &c);
            let width = clamp(card_w(360), 240, 480).min(p.width - 2 * PANE_INSET).max(1);
            let height = clamp(card_h(44), 32, 120).min(p.height - 2 * PANE_INSET).max(1);
            Rect { x: p.x + p.width - width - PANE_INSET, y: p.y + PANE_INSET, width, height }
        }
        // Top-right of the pane, below the find bar (UX8): the card is anchored where the toolbar
        // button it stands in for would be. Its size is the popup's own (`page_w`/`page_h` plus the
        // header), clamped to Chrome's popup limits and to the pane.
        Overlay::ExtensionPopup => {
            let p = pane_rect(tab, &c);
            // A failed card has no page: sta's header fills it, so its height is the card's.
            let header = if failed_popup { 0 } else { PEEK_HEADER_HEIGHT };
            let width = clamp(card_w(EXT_POPUP_MIN), EXT_POPUP_MIN, EXT_POPUP_MAX_W).min(p.width - 2 * PANE_INSET).max(1);
            let height = clamp(card_h(EXT_POPUP_MIN) + header, EXT_POPUP_MIN, EXT_POPUP_MAX_H + PEEK_HEADER_HEIGHT)
                .min(p.height - 2 * PANE_INSET)
                .max(1);
            let top = p.y + PANE_INSET + if is_visible(Overlay::FindBar) { FIND_BAR_STACK } else { 0 };
            Rect { x: p.x + p.width - width - PANE_INSET, y: top, width, height }
        }
        Overlay::Permission => {
            let p = pane_rect(tab, &c);
            let width = clamp(card_w(340), 280, 480).min(p.width - 2 * PANE_INSET).max(1);
            let height = clamp(card_h(120), 40, p.height - 2 * PANE_INSET);
            Rect { x: p.x + PANE_INSET, y: p.y + PANE_INSET, width, height }
        }
        Overlay::Switcher => {
            let width = clamp(card_w(720), 120, c.width - 32);
            let height = clamp(card_h(200), 60, c.height - 32);
            Rect { x: c.x + (c.width - width) / 2, y: c.y + (c.height - height) / 2, width, height }
        }
        // Top-right of the content (not a pane: prompts and the panel concern the whole window).
        Overlay::Agent => {
            let width = clamp(card_w(380), 300, 460).min(c.width - 2 * PANE_INSET).max(1);
            let height = clamp(card_h(160), 48, c.height - 2 * PANE_INSET);
            Rect { x: c.x + c.width - width - PANE_INSET, y: c.y + PANE_INSET, width, height }
        }
        Overlay::Toast => {
            let width = clamp(card_w(320), 120, 480).min(c.width - 16).max(1);
            // At least as high as its two corner arcs.
            let height = clamp(card_h(36), 2 * rounded::TOAST_RADIUS, 120);
            Rect { x: c.x + (c.width - width) / 2, y: c.y + c.height - 12 - height, width, height }
        }
        // A Peek tab in page fullscreen leaves the overlay for its wrapper (tabs.rs), so Peek never
        // has to cover the window itself.
        Overlay::Peek => {
            let width = (c.width - 96).min(1200).max(c.width.min(320));
            let height = (c.height - 56).max(c.height.min(200));
            let top = if height == c.height - 56 { c.y + 28 } else { c.y + (c.height - height) / 2 };
            Rect { x: c.x + (c.width - width) / 2, y: top, width, height }
        }
    })
}

/// `true` when the bounds were actually set (which also lays the overlay's contents out again).
fn set_bounds_if_changed(controller: &OverlayController, rect: Rect) -> bool {
    if rect.width > 0 && rect.height > 0 {
        let current = controller.bounds();
        if (current.x, current.y, current.width, current.height) != (rect.x, rect.y, rect.width, rect.height) {
            controller.set_bounds(Some(&rect));
            return true;
        }
    }
    false
}

/// The floating sidebar's card (window coordinates): `{8, 8, min(width + 8, client_w - 16),
/// client_h - 16}`, inside the content inset; the page inside keeps the sidebar width. Its host
/// adds the 4 DIP shadow, which stays clear of the 4 DIP resize bands.
pub fn sidebar_hover_rect() -> Rect {
    let (cw, ch) = window::client_size().unwrap_or((0, 0));
    let inset = SIDEBAR_HOVER_INSET;
    let (ix, _) = Overlay::SidebarHover.card_spec().inner();
    Rect { x: inset, y: inset, width: (window::sidebar_width() + 2 * ix).min(cw - 2 * inset).max(1), height: (ch - 2 * inset).max(1) }
}

/// The visible card of an overlay host (its bounds without the shadow).
fn card_of_host(overlay: Overlay, host: &Rect) -> Rect {
    let shadow = overlay.card_spec().shadow;
    rounded::inset(host, shadow, shadow)
}

/// `surface.setSize` from an overlay page.
pub fn set_surface_size(browser_id: i32, width: Option<i32>, height: i32) {
    let Some(overlay) = overlay_of_browser(browser_id) else { return };
    let changed = HOSTS.with(|h| {
        let mut hosts = h.borrow_mut();
        let host = hosts.get_mut(&overlay)?;
        let (w, ht) = (width.filter(|w| *w > 0), (height > 0).then_some(height));
        let changed = host.page_width != w || host.page_height != ht;
        host.page_width = w;
        host.page_height = ht;
        Some(changed)
    });
    if changed == Some(true) && is_visible(overlay) {
        layout_overlay(overlay);
        window::schedule_draggable_regions();
    }
}

/// `SetChrome` → recolor the built cards (fills, border, shadow strips, corner images).
pub fn on_chrome_colors_changed() {
    HOSTS.with(|h| {
        // Colors and images call no delegate back: no re-entrant borrow.
        for card in h.borrow().values().filter_map(|host| host.card.as_ref()) {
            card.recolor();
        }
    });
    if is_visible(Overlay::Peek) {
        rounded::layout_peek_masks(peek_page_rect().as_ref(), false);
    }
}

// ----------------------------------------------------------------------------------- effects

pub fn show_command_bar() {
    request(Overlay::CommandBar, true, None);
}

pub fn hide_command_bar() {
    request(Overlay::CommandBar, false, None);
}

pub fn show_find_bar(tab: Id) {
    request(Overlay::FindBar, true, Some(tab));
    layout_overlay(Overlay::FindBar);
}

pub fn hide_find_bar() {
    request(Overlay::FindBar, false, None);
}

pub fn show_switcher() {
    let generation = SWITCHER_GEN.get() + 1;
    SWITCHER_GEN.set(generation);
    SWITCHER_REQUESTED.set(true);
    ensure_view(Overlay::Switcher);
    task::post_ui_delayed(SWITCHER_DELAY_MS, move || {
        if SWITCHER_GEN.get() == generation {
            request(Overlay::Switcher, true, None);
        }
    });
}

pub fn hide_switcher() {
    SWITCHER_GEN.set(SWITCHER_GEN.get() + 1);
    SWITCHER_REQUESTED.set(false);
    hide_after_blank(Overlay::Switcher, motion::SWITCHER_KEY);
}

pub fn show_toast() {
    request(Overlay::Toast, true, None);
}

pub fn hide_toast() {
    hide_after_blank(Overlay::Toast, motion::TOAST_KEY);
}

/// Hides a **non-activatable** overlay (toast, switcher) once its page has presented a blank frame
/// (FINAL PLAN §4, `motion.rs`): the page is asked to blank (`surface.exit {gen}`), the widget
/// **lingers** meanwhile — still restacked above a corner mask, still a no-drag hole, which is
/// accepted — and it is hidden on the page's `surface.exited {gen}` (never sooner than
/// `motion::HIDE_FLOOR_MS`) or at the cap. A show during the linger cancels the exit ([`request`]).
///
/// Activatable overlays never come through here: they close instantly, because keeping keyboard
/// focus in a bar the user just dismissed is worse than a stale frame (critique issues 3-4).
fn hide_after_blank(overlay: Overlay, key: &'static str) {
    // Not on screen: nothing to blank (a switcher cancelled inside its 250 ms delay, a toast whose
    // page never became ready). Clear the intent the ordinary way.
    if !is_visible(overlay) {
        request(overlay, false, None);
        return;
    }
    // The intent is gone from here on: `is_wanted` is false while it lingers, so a show that arrives
    // meanwhile is a fresh one and the exit's own hide can tell that it was not overtaken.
    HOSTS.with(|h| {
        if let Some(host) = h.borrow_mut().get_mut(&overlay) {
            host.wanted = false;
        }
    });
    let Some(id) = overlay_browser_id(overlay) else {
        motion::note_early_hide();
        apply_visibility(overlay, false);
        return;
    };
    motion::begin(
        id,
        key,
        motion::HIDE_FLOOR_MS,
        |generation| ipc::emit_raw_to(id, "surface.exit", &format!("{{\"gen\":{generation}}}")),
        move || {
            if !is_wanted(overlay) {
                apply_visibility(overlay, false);
            }
        },
    );
}

pub fn show_permission_prompt(tab: Id) {
    request(Overlay::Permission, true, Some(tab));
    layout_overlay(Overlay::Permission);
}

pub fn hide_permission_prompt() {
    request(Overlay::Permission, false, None);
}

/// `Effect::ShowAgentOverlay` (via automation/ui.rs).
pub fn show_agent_overlay() {
    request(Overlay::Agent, true, None);
}

pub fn hide_agent_overlay() {
    request(Overlay::Agent, false, None);
}

// ----------------------------------------------------------------------------------- extension popup

/// Parents the extension's popup page under sta's header strip (hidden until it has a size).
/// ext_popup.rs owns the view and closes its browser.
pub fn adopt_extension_popup(view: &BrowserView, tab: Option<Id>) {
    ensure_view(Overlay::ExtensionPopup);
    let Some(parent) = view_parent(Overlay::ExtensionPopup) else {
        log_error!("no extension popup host");
        return;
    };
    release_extension_popup_view();
    // The header goes back to its fixed height (a failed card before it let the header fill).
    if let (Some(layout), Some(header)) = (parent.get_layout().and_then(|l| l.as_box_layout()), header_view()) {
        layout.clear_flex_for_view(Some(&mut View::from(&header)));
    }
    add_to_card(&parent, view, None, true);
    view.set_visible(1);
    HOSTS.with(|h| {
        if let Some(host) = h.borrow_mut().get_mut(&Overlay::ExtensionPopup) {
            host.popup_view = Some(view.clone());
            host.tab = tab;
            host.popup_failed = false;
            // A new popup starts at the minimum until it reports its own size.
            host.page_width = None;
            host.page_height = None;
        }
    });
    layout_overlay(Overlay::ExtensionPopup);
    parent.layout();
}

/// Removes whatever popup page is parented in the card (the caller drops the view).
fn release_extension_popup_view() {
    let previous = HOSTS.with(|h| h.borrow_mut().get_mut(&Overlay::ExtensionPopup).and_then(|host| host.popup_view.take()));
    if let (Some(view), Some(parent)) = (previous, view_parent(Overlay::ExtensionPopup)) {
        remove_from_card(&parent, &view);
    }
}

/// `Effect::OpenExtensionPopup` → the card is shown once the popup reported a size (S4), or with its
/// header alone when it never does (the honest-failure line).
pub fn show_extension_popup(tab: Option<Id>) {
    request(Overlay::ExtensionPopup, true, tab);
    layout_overlay(Overlay::ExtensionPopup);
}

pub fn hide_extension_popup() {
    request(Overlay::ExtensionPopup, false, None);
    release_extension_popup_view();
}

/// The popup never rendered (`ext_popup.rs`, [`FAIL_MS`]): the blank page is taken out of the card
/// and sta's header fills it, so the honest line has room to be read.
pub fn extension_popup_failed() {
    release_extension_popup_view();
    HOSTS.with(|h| {
        if let Some(host) = h.borrow_mut().get_mut(&Overlay::ExtensionPopup) {
            host.popup_failed = true;
        }
    });
    if let (Some(parent), Some(header)) = (view_parent(Overlay::ExtensionPopup), header_view())
        && let Some(layout) = parent.get_layout().and_then(|l| l.as_box_layout())
    {
        layout.set_flex_for_view(Some(&mut View::from(&header)), 1);
        parent.layout();
    }
    set_extension_popup_size(EXT_POPUP_FAILED.0, EXT_POPUP_FAILED.1);
}

fn header_view() -> Option<BrowserView> {
    HOSTS.with(|h| h.borrow().get(&Overlay::ExtensionPopup).and_then(|host| host.view.clone()))
}

/// The popup page's content size in DIP (S4's measurement). The card adds the header strip.
pub fn set_extension_popup_size(width: i32, height: i32) {
    let changed = HOSTS.with(|h| {
        let mut hosts = h.borrow_mut();
        let Some(host) = hosts.get_mut(&Overlay::ExtensionPopup) else { return false };
        let (w, ht) = (Some(width.clamp(EXT_POPUP_MIN, EXT_POPUP_MAX_W)), Some(height.clamp(EXT_POPUP_MIN, EXT_POPUP_MAX_H)));
        let changed = host.page_width != w || host.page_height != ht;
        host.page_width = w;
        host.page_height = ht;
        changed
    });
    if changed {
        layout_overlay(Overlay::ExtensionPopup);
        if is_visible(Overlay::ExtensionPopup) {
            window::schedule_draggable_regions();
        }
    }
}

// ----------------------------------------------------------------------------------- floating sidebar

/// Parks the sidebar BrowserView in the (hidden) floating sidebar host. window.rs owns the view
/// and has removed it from the window first.
pub fn adopt_sidebar(sidebar: &BrowserView) {
    let Some(parent) = ensure_card(Overlay::SidebarHover) else {
        log_error!("no floating sidebar host");
        return;
    };
    layout_overlay(Overlay::SidebarHover);
    add_to_card(&parent, sidebar, None, true);
    sidebar.set_visible(1);
    parent.layout();
}

/// Hides the floating sidebar host and removes the parked sidebar view from it (docking,
/// shutdown). The caller re-parents or drops the view.
pub fn release_sidebar(sidebar: &BrowserView) {
    let Some(controller) = HOSTS.with(|h| h.borrow().get(&Overlay::SidebarHover).map(|host| host.controller.clone())) else {
        return;
    };
    if controller.is_visible() != 0 {
        controller.set_visible(0);
        window::schedule_draggable_regions();
    }
    if let Some(parent) = view_parent(Overlay::SidebarHover) {
        remove_from_card(&parent, sidebar);
    }
}

/// Shows the floating sidebar (sidebar_hover.rs decides when). Not activatable: keyboard focus
/// stays where it is. The overlays above it (switcher, toast) are re-shown to stay on top.
/// How far the floating sidebar travels to be completely outside the window: its home rect's right
/// edge, so a card at this offset is clipped away entirely.
pub fn sidebar_hover_travel() -> i32 {
    let r = sidebar_hover_rect();
    let shadow = Overlay::SidebarHover.card_spec().shadow;
    r.x + r.width + shadow
}

/// The floating sidebar's slide offset (DIP, ≤ 0; 0 = its home rect).
pub fn sidebar_hover_slide() -> i32 {
    SIDEBAR_SLIDE.get()
}

/// Draws the floating sidebar host `dx` DIP left of its home rect (`dx ≤ 0`), for the slide in
/// `motion::slide_sidebar`. The home rect is what `sidebar_hover_rect` keeps reporting: the hover
/// machine's keep zone must not travel with the card.
///
/// Moving the host is all this does: the draggable regions are *not* recomputed per step (that asks
/// every page for its rects over IPC, twenty times in one slide). The card is a no-drag hole where
/// it lands, and `slide_sidebar` schedules the recomputation once, when it gets there.
pub fn set_sidebar_hover_slide(dx: i32) {
    if SIDEBAR_SLIDE.replace(dx.min(0)) == dx.min(0) {
        return;
    }
    if is_visible(Overlay::SidebarHover) {
        layout_overlay(Overlay::SidebarHover);
    }
}

pub fn show_sidebar_hover() {
    let Some(controller) = HOSTS.with(|h| h.borrow().get(&Overlay::SidebarHover).map(|host| host.controller.clone())) else {
        return;
    };
    if controller.is_visible() != 0 {
        return;
    }
    layout_overlay(Overlay::SidebarHover);
    controller.set_visible(1);
    if restack_overlays_above(Overlay::SidebarHover) {
        // Only non-activatable overlays are above it; kept in case the order ever changes.
        end_focus_guard(FOCUSED_BROWSER.get());
    }
    log_debug!("overlay SidebarHover shown");
    window::schedule_draggable_regions();
}

pub fn hide_sidebar_hover() {
    let Some(controller) = HOSTS.with(|h| h.borrow().get(&Overlay::SidebarHover).map(|host| host.controller.clone())) else {
        return;
    };
    if controller.is_visible() == 0 {
        return;
    }
    controller.set_visible(0);
    log_debug!("overlay SidebarHover hidden");
    window::schedule_draggable_regions();
}

/// `Effect::ShowPeek`: move the tab's view into the Peek host (under the header) and show it.
pub fn show_peek(tab: Id) {
    ensure_view(Overlay::Peek);
    let Some((root, current, has_view, ready)) = HOSTS.with(|h| {
        h.borrow().get(&Overlay::Peek).map(|host| (host.panel.clone(), host.tab, host.peek_view.is_some(), host.ready))
    }) else {
        return;
    };
    let Some(parent) = view_parent(Overlay::Peek) else { return };
    if current != Some(tab) || !has_view {
        if let Some(previous) = current.filter(|t| *t != tab)
            && let Some(view) = take_back_peek_view(previous)
        {
            tabs::return_view_from_peek(previous, view);
        }
        let Some(view) = tabs::take_view_for_peek(tab) else {
            log_warn!("ShowPeek: tab {tab} has no view");
            return;
        };
        add_to_card(&parent, &view, None, true);
        view.set_visible(1);
        HOSTS.with(|h| {
            if let Some(host) = h.borrow_mut().get_mut(&Overlay::Peek) {
                host.peek_view = Some(view);
                host.tab = Some(tab);
            }
        });
    }
    if !ready {
        let generation = PEEK_GEN.get() + 1;
        PEEK_GEN.set(generation);
        task::post_ui_delayed(PEEK_READY_TIMEOUT_MS, move || {
            if PEEK_GEN.get() != generation || !is_wanted(Overlay::Peek) {
                return;
            }
            let late = HOSTS.with(|h| {
                let mut hosts = h.borrow_mut();
                let host = hosts.get_mut(&Overlay::Peek)?;
                (!host.ready).then(|| host.ready_timeout = true)
            });
            if late.is_some() {
                log_warn!("Peek header not ready after {PEEK_READY_TIMEOUT_MS} ms; showing Peek anyway");
                apply_visibility(Overlay::Peek, false);
            }
        });
    }
    request(Overlay::Peek, true, Some(tab));
    layout_overlay(Overlay::Peek);
    root.layout();
    // Pane-anchored overlays over the Peek tab follow its view.
    layout_overlay(Overlay::FindBar);
    layout_overlay(Overlay::Permission);
}

/// `Effect::HidePeek`: hide only; the view stays parented until destroyed or taken back.
pub fn hide_peek(tab: Id) {
    let current = HOSTS.with(|h| h.borrow().get(&Overlay::Peek).and_then(|host| host.tab));
    if current.is_some() && current != Some(tab) {
        log_debug!("HidePeek({tab}) while Peek holds {current:?}");
    }
    PEEK_GEN.set(PEEK_GEN.get() + 1);
    request(Overlay::Peek, false, None);
}

/// Detaches `tab`'s view from the Peek host (hiding Peek) and returns it to the caller, which
/// must re-parent or drop it. `None` if the tab is not in Peek.
pub fn take_back_peek_view(tab: Id) -> Option<BrowserView> {
    let view = HOSTS.with(|h| {
        let mut hosts = h.borrow_mut();
        let host = hosts.get_mut(&Overlay::Peek)?;
        if host.tab != Some(tab) {
            return None;
        }
        host.tab = None;
        host.wanted = false;
        host.peek_view.take()
    })?;
    apply_visibility(Overlay::Peek, false);
    if let Some(parent) = view_parent(Overlay::Peek) {
        remove_from_card(&parent, &view);
    }
    Some(view)
}

// ----------------------------------------------------------------------------------- focus

/// Browser that most recently gained keyboard focus.
pub fn focused_browser() -> Option<i32> {
    FOCUSED_BROWSER.get()
}

/// The overlay whose host contains `browser_id` (its surface view, or the tab view in Peek).
fn browser_in_overlay(browser_id: i32) -> Option<Overlay> {
    match browsers::role_of(browser_id)? {
        Role::Surface(Surface::Sidebar) => window::sidebar_parked().then_some(Overlay::SidebarHover),
        Role::Surface(s) => Overlay::from_surface(s),
        Role::Tab(t) => {
            let in_peek = HOSTS.with(|h| h.borrow().get(&Overlay::Peek).is_some_and(|host| host.tab == Some(t) && host.peek_view.is_some()));
            in_peek.then_some(Overlay::Peek)
        }
        // DevTools on a Peek page open undocked (CEF's own window), so a docked frontend is never
        // inside an overlay host.
        Role::DevTools { .. } => None,
        Role::ExtensionPopup => Some(Overlay::ExtensionPopup),
    }
}

/// Moves keyboard focus out of a hidden overlay: to core's focused tab when it is visible, else
/// to the empty-state view or the sidebar (`window::focus_fallback`). Posted, never inline.
pub fn restore_main_focus() {
    if window::is_closing() {
        return;
    }
    let focused = FOCUSED_BROWSER.get();
    if let Some(overlay) = focused.and_then(browser_in_overlay)
        && overlay != Overlay::SidebarHover // the floating sidebar never holds focus
        && is_visible(overlay)
    {
        return; // focus went back into a visible overlay meanwhile
    }
    let tab = controller::with_store(|s| s.focused_tab()).flatten().filter(|t| tabs::is_tab_visible(*t) && crate::automation::ui::may_restore_focus_to(*t));
    log_debug!("focus was in a hidden overlay (browser {focused:?}); moving it to {tab:?}");
    match tab {
        Some(tab) => tabs::focus_browser(tab),
        None => window::focus_fallback(),
    }
}

/// A click inside the floating sidebar (not activatable) moves aura focus to the window itself:
/// the page blurs, no browser has focus, keys and accelerators go nowhere, and Views still counts
/// the old view as focused, so a plain `request_focus` on it is a no-op. Pass focus through the
/// parked sidebar view (which can't keep it) back to the browser that had it (posted by
/// sidebar_hover.rs after the button release).
pub fn refocus_after_floating_click() {
    if window::is_closing() || !window::sidebar_parked() {
        return;
    }
    let focused = FOCUSED_BROWSER.get();
    let target = focused
        .filter(|id| browser_in_overlay(*id) != Some(Overlay::SidebarHover) && is_shown_browser(*id))
        .and_then(focus_view_of_browser)
        .or_else(|| controller::with_store(|s| s.focused_tab()).flatten().filter(|t| tabs::is_tab_visible(*t)).and_then(tabs::view_for_tab))
        .or_else(window::focus_fallback_view);
    let (Some(target), Some(bounce)) = (target, window::docked_view(Surface::Sidebar)) else { return };
    log_debug!("refocusing {focused:?} after a click in the floating sidebar");
    FOCUS_GUARD.set(FOCUS_GUARD.get() + 1);
    bounce.request_focus();
    target.request_focus();
    end_focus_guard(focused);
}

/// Focus-loss rules (FocusHandler::on_got_focus of every browser). Only enqueues commands.
pub fn on_browser_got_focus(browser_id: i32) {
    let previous = FOCUSED_BROWSER.replace(Some(browser_id));
    if focus_events_suppressed() {
        return; // an overlay restack; `end_focus_guard` applies the policy to the final focus
    }
    if let Some(overlay) = browser_in_overlay(browser_id)
        && !is_visible(overlay)
    {
        // E.g. an overlay view created lazily (Peek's header) grabbed focus while the user types
        // in the command bar: give focus back to that overlay while it is still shown. (Not to a
        // tab: refocusing it would report `TabFocused` and close the Peek being opened.)
        let previous = previous.filter(|id| *id != browser_id && browser_in_overlay(*id).is_some_and(|o| o != Overlay::Peek));
        task::post_ui(move || match previous.filter(|id| is_shown_browser(*id)).and_then(focus_view_of_browser) {
            Some(view) if FOCUSED_BROWSER.get() == Some(browser_id) => {
                log_debug!("focus went to hidden overlay browser {browser_id}; back to {previous:?}");
                view.request_focus();
            }
            _ => restore_main_focus(),
        });
        return;
    }
    let role = browsers::role_of(browser_id);
    if is_visible(Overlay::CommandBar) && role != Some(Role::Surface(Surface::CommandBar)) {
        controller::dispatch(Command::CloseCommandBar { seq: None });
    }
    if is_wanted(Overlay::Peek) && is_visible(Overlay::Peek) {
        let peek_tab = peek_tab();
        let works_on_peek = match role {
            Some(Role::Tab(t)) => Some(t) == peek_tab,
            Some(Role::DevTools { .. }) => false,
            // A popup card opened over a Peek page works on it, like the other overlays.
            Some(Role::ExtensionPopup) => true,
            Some(Role::Surface(s)) => matches!(
                s,
                Surface::PeekHeader | Surface::CommandBar | Surface::FindBar | Surface::Permission | Surface::Agent | Surface::ExtensionPopup
            ),
            None => false,
        };
        if !works_on_peek {
            controller::dispatch(Command::ClosePeek { focus_lost: true });
        }
    }
    // The agent activity panel closes when something else takes focus (core ignores it while a
    // prompt is shown).
    if is_visible(Overlay::Agent) && role != Some(Role::Surface(Surface::Agent)) {
        controller::dispatch(Command::CloseAgentPanel { focus_lost: true });
    }
    // The popup card closes on blur, like Chrome's own action popup: anything but its own two
    // browsers (the extension's page and sta's header strip) taking focus closes it.
    if is_visible(Overlay::ExtensionPopup)
        && !matches!(role, Some(Role::ExtensionPopup) | Some(Role::Surface(Surface::ExtensionPopup)))
    {
        controller::dispatch(Command::CloseExtensionPopup);
    }
}

// ----------------------------------------------------------------------------------- teardown

/// Releases an overlay surface view during shutdown (posted from `do_close`).
pub fn release_surface(surface: Surface) {
    let Some(overlay) = Overlay::from_surface(surface) else { return };
    let taken = HOSTS.with(|h| h.borrow_mut().get_mut(&overlay).and_then(|host| host.view.take()));
    if let Some(view) = taken {
        if let Some(parent) = view_parent(overlay) {
            remove_from_card(&parent, &view);
        }
        drop(view);
    }
}

pub fn on_surface_closed(surface: Surface, browser_id: i32) {
    let Some(overlay) = Overlay::from_surface(surface) else { return };
    HOSTS.with(|h| {
        if let Some(host) = h.borrow_mut().get_mut(&overlay)
            && host.browser_id == Some(browser_id)
        {
            host.browser_id = None;
            host.ready = false;
        }
    });
    // A page that is gone can never acknowledge the exit it was asked for: end that wait here
    // instead of at the cap, and hide the widget it was lingering in.
    if motion::cancel(browser_id) && !is_wanted(overlay) {
        apply_visibility(overlay, false);
    }
    if FOCUSED_BROWSER.get() == Some(browser_id) {
        FOCUSED_BROWSER.set(None);
    }
}

/// Drops every overlay handle (window destroyed).
pub fn clear() {
    motion::clear(); // pending exit waits would touch widgets that are going away
    let hosts = HOSTS.with(|h| std::mem::take(&mut *h.borrow_mut()));
    drop(hosts);
}

#[cfg_attr(not(debug_assertions), allow(dead_code))] // used by debug.rs only
pub fn debug_snapshot() -> serde_json::Value {
    #[allow(clippy::type_complexity)]
    let hosts: Vec<(Overlay, OverlayController, bool, bool, Option<BrowserView>, Option<i32>, Option<i32>, Option<i32>, Option<Id>, Option<BrowserView>, bool)> =
        HOSTS.with(|h| {
            h.borrow()
                .iter()
                .map(|(o, host)| {
                    (
                        *o,
                        host.controller.clone(),
                        host.ready,
                        host.wanted,
                        host.view.clone(),
                        host.browser_id,
                        host.page_width,
                        host.page_height,
                        host.tab,
                        host.peek_view.clone().or_else(|| host.popup_view.clone()),
                        host.card.is_some(),
                    )
                })
                .collect()
        });
    let list: Vec<serde_json::Value> = hosts
        .into_iter()
        .map(|(o, c, ready, wanted, view, browser_id, page_w, page_h, tab, peek_view, built)| {
            let b = c.bounds();
            let rect = |v: &BrowserView| window::view_rect_in_window(&View::from(v)).map(|r| [r.x, r.y, r.width, r.height]);
            // The floating sidebar host has no view of its own: report the parked sidebar.
            let (ready, wanted, view) = if o == Overlay::SidebarHover {
                let parked = window::docked_view(Surface::Sidebar).filter(|_| window::sidebar_parked());
                (crate::sidebar_hover::is_ready(), c.is_visible() != 0, parked)
            } else {
                (ready, wanted, view)
            };
            let card = card_of_host(o, &b);
            let spec = o.card_spec();
            serde_json::json!({
                "overlay": format!("{o:?}"),
                "visible": c.is_visible() != 0,
                "ready": ready,
                "wanted": wanted,
                "hasView": view.is_some(),
                "viewVisible": view.as_ref().map(|v| v.is_visible() != 0),
                "viewRect": view.as_ref().and_then(rect),
                "browserId": browser_id,
                // The visible card; the host adds the shadow (drag-region holes are host bounds).
                "bounds": [card.x, card.y, card.width, card.height],
                "hostBounds": [b.x, b.y, b.width, b.height],
                "card": {
                    "built": built,
                    "radius": spec.radius,
                    "shadow": spec.shadow,
                    "inner": [spec.inner().0, spec.inner().1],
                    "orientation": format!("{:?}", spec.orientation),
                },
                "pageSize": [page_w, page_h],
                "tab": tab,
                "peekViewRect": peek_view.as_ref().and_then(rect),
            })
        })
        .collect();
    serde_json::json!({ "hosts": list, "switcherRequested": SWITCHER_REQUESTED.get(), "peekTab": peek_tab(), "focusGuard": FOCUS_GUARD.get() })
}
