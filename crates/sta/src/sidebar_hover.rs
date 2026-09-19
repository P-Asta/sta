//! Sidebar hover reveal [owner: chrome] (ARCHITECTURE §4 "Floating sidebar", arc_spec §2.15).
//!
//! While the sidebar is hidden (`SetSidebar{visible:false}`), window.rs keeps its one BrowserView
//! *parked* in the non-activatable `SidebarHover` overlay host (overlays.rs). This module decides
//! when that overlay is shown:
//! - **reveal**: the pointer rests in the left edge zone for [`DWELL_MS`] (restored window:
//!   `x < 12` DIP, the 4 DIP resize band included; maximized/fullscreen: `x < 8`), full client
//!   height, reaching [`OUTSIDE_SLOP`] DIP past the window's left edge — a pointer thrown at the
//!   edge of a window that is not at the screen's edge overshoots it, and that is the same gesture. A button pressed inside the zone or in the resize band left of it (resizing,
//!   clicking the gap) blocks the reveal until it is released; a drag that started elsewhere and
//!   enters the zone counts. After a park, an Esc hide, or a window move/resize with the pointer in
//!   the zone, the pointer must leave the zone first (`needs_exit`). When a dock for a panel that
//!   holds input (e.g. a rename started in the floating sidebar) ends with the pointer in the keep
//!   zone, it floats again at once;
//! - **hide**: at the first sample outside the keep zone ([`HIDE_MS`] is 0; the card takes
//!   `motion::SLIDE_OUT_MS` to leave) — the keep zone is the overlay bounds grown by 8 DIP, plus the strip
//!   `x < 16`), unless a button is held, the pointer is over a popup owned by our window, core
//!   pins it (`SetSidebar.floating`: a transient panel is open) or the page locks it
//!   (`sidebar.hoverLock`: an HTML menu, popover or drag is open in it). Un-pinning or unlocking
//!   hides it at once when the pointer is outside the keep zone. Esc in a page hides an overlay
//!   core doesn't pin (the key still reaches the page). The page is told first
//!   (`sidebar.hover {visible:false, dismiss:true, gen}`: it closes its menus and hides its
//!   contents, so the next reveal never shows a stale frame), and the overlay hides when the page
//!   acknowledges that blank frame (`surface.exited {gen}`) or at the cap — never sooner than
//!   `motion::HIDE_FLOOR_MS`, whatever the animation settings say (`motion.rs`);
//! - **dismiss**: a button press outside the overlay while it is shown closes the page's menus
//!   (`sidebar.hover {visible:true, dismiss:true}`) and a transient panel (`CloseSidebarPanel`);
//! - **refocus**: a click inside the overlay moves aura focus to the window itself (the overlay
//!   can't take it): the page blurs and keys and accelerators go nowhere until something is
//!   focused again. After the release, `overlays::refocus_after_floating_click` gives focus back;
//! - **armed** (pointer reveal possible): the sidebar is parked and its page sent `ui.ready`, no
//!   page fullscreen, the window is active and not minimized or closing. The cursor poll runs
//!   while armed or while the overlay is visible (never while minimized): every [`FAST_POLL_MS`]
//!   while the pointer is over the window, within 96 DIP of its left edge or the overlay is shown,
//!   else every [`SLOW_POLL_MS`], and at the dwell / hide deadlines (a reveal ≤ dwell + one poll).
//!
//! Detection is a UI-thread poll of `GetCursorPos` (`platform::cursor_sample`), not a window
//! subclass: Chromium forwards mouse messages over web content to its own handler directly, the
//! left band is non-client, a resting cursor sends nothing, and OLE drag loops deliver no
//! `WM_MOUSEMOVE`. Positions are converted with the *window's* client rect and scale factor.
//!
//! The overlay can't take keyboard focus (`can_activate` 0), so the page keeps focus and sees no
//! blur. Panels that need typing dock the sidebar instead (core).
//!
//! Debug builds: `STA_DEBUG_HOVER_REVEAL=0` starts with pointer reveal disabled (the e2e
//! suites do, so a resting cursor can't float the sidebar), and `debug.hoverInput` turns it on,
//! injects a virtual pointer or runs a guarded real-cursor check.
//!
//! Public API:
//! - `pub struct Machine` (pure, unit-tested), `pub struct Sample`, `pub struct Area`, `pub enum Action`
//! - `pub fn refresh()` — re-evaluate after sidebar, fullscreen, activation or ready changes
//! - `pub fn set_pinned(pinned: bool)`, `pub fn set_locked(locked: bool)`, `pub fn escape() -> bool`
//! - `pub fn on_parked(after_panel: bool)`, `pub fn on_docked()`, `pub fn on_bounds_changed()`,
//!   `pub fn pointer_in_keep_zone() -> bool`
//! - `pub fn on_sidebar_ready()`, `pub fn on_sidebar_gone()`, `pub fn on_sidebar_load_start()`,
//!   `pub fn is_ready() -> bool`
//! - `pub fn emit_contents(visible: bool, dismiss: bool)` — `sidebar.hover` event to the sidebar page
//! - `pub fn begin_exit(floor: i64, done: impl FnOnce())`, `pub fn cancel_exit()` — the
//!   acknowledged contents exit (window.rs parks through them)
//! - `pub fn debug_snapshot() -> serde_json::Value`, `pub fn debug_input(..)` (debug builds)

use crate::browsers::{self, Surface};
use crate::overlays::{self, Overlay};
use crate::{controller, ipc, motion, task, window};
use sta_core::Command;
use cef::{ImplBrowser, ImplDisplay, ImplWindow};
use std::cell::{Cell, RefCell};
use std::time::Instant;

/// The pointer rests in the edge zone this long before the sidebar appears.
pub const DWELL_MS: i64 = 120;
/// Grace period before a hide once the pointer has left the keep zone: none. Leaving starts the
/// card on its way out at the next sample, and the leaving itself is what takes time
/// ([`motion::SLIDE_OUT_MS`]). There is nothing to forgive here — the keep zone already does that
/// (the card's bounds grown by [`KEEP_MARGIN`], the strip left of [`KEEP_STRIP`] and the slop
/// outside the window), and a button held, an owned popup, a pin or the page's own lock still hold
/// it open however far the pointer goes. Raise it to put a delay back.
pub const HIDE_MS: i64 = 0;
/// Width of the reveal zone (DIP), measured from the window's left edge inwards.
pub const EDGE_ZONE: f64 = 8.0;
/// How far *outside* the window's left edge the reveal zone (and the keep zone) reach (DIP). A
/// pointer thrown at the edge usually overshoots it — the window is not at the screen's edge, so
/// nothing stops the cursor there — and landing just outside the window is the same gesture as
/// landing on it. Beyond this the pointer is simply somewhere else.
pub const OUTSIDE_SLOP: f64 = 64.0;
/// Resize band of a restored frameless window (DIP, views.md §6d).
pub const RESIZE_BAND: f64 = 4.0;
/// Keep zone: the overlay bounds grown by this much (DIP)…
pub const KEEP_MARGIN: f64 = 8.0;
/// …plus the strip left of this x (DIP).
pub const KEEP_STRIP: f64 = 16.0;
/// Poll fast while the pointer is this close to the left edge (DIP).
pub const NEAR_EDGE: f64 = 96.0;
/// Poll interval while the pointer is over the window, near its left edge or the overlay is shown.
pub const FAST_POLL_MS: i64 = 33;
/// Poll interval otherwise (the pointer over another window): a reveal still starts within
/// dwell + this after the pointer arrives.
pub const SLOW_POLL_MS: i64 = 50;
/// Focus is given back this long after a click in the overlay (its own commands run first).
pub const REFOCUS_DELAY_MS: i64 = 60;

// ----------------------------------------------------------------------------------- machine

/// A rectangle in window client DIP.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Area {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl Area {
    fn contains(&self, x: f64, y: f64, margin: f64) -> bool {
        x >= self.x - margin && x < self.x + self.w + margin && y >= self.y - margin && y < self.y + self.h + margin
    }
}

/// One pointer sample.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Sample {
    /// Pointer position relative to the window's client area (DIP).
    pub x: f64,
    pub y: f64,
    /// Client area height (DIP).
    pub height: f64,
    /// The top-level window under the pointer is ours.
    pub over_window: bool,
    /// The pointer is over a popup owned by our window (select list, menu).
    pub owned_popup: bool,
    /// A mouse button is held.
    pub buttons: bool,
    /// A button was pressed since the previous sample (catches clicks shorter than the poll).
    pub clicked: bool,
    /// Restored window: the outer [`RESIZE_BAND`] DIP resize the window.
    pub resize_band: bool,
    /// Monotonic milliseconds.
    pub now: i64,
}

impl Sample {
    /// A pointer that is nowhere near the window (pointer reveal disabled, no cursor).
    pub fn nowhere(now: i64) -> Sample {
        Sample { x: -1e6, y: -1e6, height: 0.0, over_window: false, owned_popup: false, buttons: false, clicked: false, resize_band: false, now }
    }

    /// In the reveal zone at the left window edge: anywhere left of it, from [`EDGE_ZONE`] DIP
    /// inside the window (plus the resize band of a restored one) out to [`OUTSIDE_SLOP`] DIP
    /// beyond its edge. Over the window the zone belongs to us only while no other window covers
    /// it; outside it there is nothing of ours to cover, so `over_window` says nothing there and is
    /// not asked — the window still has to be the active one for any of this to run (`armed`).
    pub fn in_edge_zone(&self) -> bool {
        if self.y < 0.0 || self.y >= self.height {
            return false; // full client height only
        }
        let inner = if self.resize_band { RESIZE_BAND + EDGE_ZONE } else { EDGE_ZONE };
        if self.x >= inner {
            return false;
        }
        if self.x >= 0.0 { self.over_window } else { self.x >= -OUTSIDE_SLOP }
    }

    /// In the left resize band of a restored window (left of the edge zone).
    pub fn in_resize_band(&self) -> bool {
        self.resize_band && self.over_window && self.x < RESIZE_BAND && self.y >= 0.0 && self.y < self.height
    }

    /// Where the shown overlay stays open — including the slop outside the window's left edge, so
    /// the pointer that revealed it by overshooting does not close it again at once.
    pub fn in_keep_zone(&self, overlay: &Area) -> bool {
        if self.y < 0.0 || self.y >= self.height {
            return false;
        }
        if self.x < 0.0 {
            return self.x >= -OUTSIDE_SLOP;
        }
        self.over_window && (overlay.contains(self.x, self.y, KEEP_MARGIN) || self.x < KEEP_STRIP)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    None,
    Show,
    /// Hide (the page closes its menus first).
    Hide,
    /// A press outside the shown overlay: close the page's menus and a transient panel.
    Dismiss,
    /// A click inside the shown overlay ended: give keyboard focus back.
    Refocus,
}

/// Hover reveal state machine (no CEF, unit-tested).
#[derive(Debug, Default, Clone)]
pub struct Machine {
    /// The overlay is (being) shown.
    pub visible: bool,
    /// Core keeps it open (a transient panel is open).
    pub pinned: bool,
    /// The sidebar page keeps it open (`sidebar.hoverLock`: a menu, popover or drag is open).
    pub locked: bool,
    /// The pointer must leave the edge zone before the next reveal.
    pub needs_exit: bool,
    /// A click that started inside the overlay was released as it hid: the focus hand-back
    /// ([`Action::Refocus`]) is still owed, because only one action can be returned per sample.
    refocus_owed: bool,
    dwell_since: Option<i64>,
    outside_since: Option<i64>,
    /// The held button was pressed inside the edge zone or the resize band left of it.
    press_in_zone: bool,
    /// The held button was pressed inside the shown overlay.
    press_in_overlay: bool,
    prev_buttons: Option<bool>,
    last_in_zone: bool,
}

impl Machine {
    /// Feeds one sample. `armed`: pointer reveal is possible now; `overlay`: the overlay bounds.
    /// `Show`/`Hide` already updated [`Machine::visible`].
    pub fn step(&mut self, s: &Sample, overlay: &Area, armed: bool) -> Action {
        let in_zone = s.in_edge_zone();
        self.last_in_zone = in_zone;
        // A whole click between two samples counts as a press and a release here.
        let missed_click = s.clicked && !s.buttons && self.prev_buttons != Some(true);
        let pressed = (s.buttons && self.prev_buttons != Some(true)) || missed_click;
        let released = (!s.buttons && self.prev_buttons == Some(true)) || missed_click;
        self.prev_buttons = Some(s.buttons);
        if !in_zone {
            self.needs_exit = false;
        }
        if !s.buttons {
            self.press_in_zone = false;
        } else if pressed && (in_zone || s.in_resize_band()) {
            // Resizing from the left edge (the pointer may drift into the zone once the window
            // hits its minimum width) or a click in the gap.
            self.press_in_zone = true;
        }

        if self.visible {
            self.dwell_since = None;
            let inside = s.over_window && overlay.contains(s.x, s.y, 0.0);
            if pressed {
                self.press_in_overlay = inside;
            }
            let refocus = released && std::mem::take(&mut self.press_in_overlay);
            let locked = self.pinned || self.locked || s.buttons || s.owned_popup;
            if locked || s.in_keep_zone(overlay) {
                self.outside_since = None;
            } else {
                let since = *self.outside_since.get_or_insert(s.now);
                if s.now - since >= HIDE_MS {
                    self.refocus_owed = refocus; // the release that hid it still gives focus back
                    self.hide();
                    return Action::Hide;
                }
            }
            return if pressed && !inside && !s.owned_popup {
                Action::Dismiss
            } else if refocus {
                Action::Refocus
            } else {
                Action::None
            };
        }

        self.outside_since = None;
        if !armed || !in_zone || self.needs_exit || self.press_in_zone {
            self.dwell_since = None;
            return Action::None;
        }
        let since = *self.dwell_since.get_or_insert(s.now);
        if s.now - since >= DWELL_MS {
            self.show();
            return Action::Show;
        }
        Action::None
    }

    /// Takes the focus hand-back a hide swallowed (see [`Machine::refocus_owed`]).
    pub fn take_refocus(&mut self) -> bool {
        std::mem::take(&mut self.refocus_owed)
    }

    fn show(&mut self) {
        self.visible = true;
        self.dwell_since = None;
        self.outside_since = None;
    }

    fn hide(&mut self) {
        self.visible = false;
        self.dwell_since = None;
        self.outside_since = None;
        self.press_in_overlay = false;
    }

    /// Core pins or un-pins the overlay. Pinning shows it at once (`can_show`); un-pinning hides it
    /// at once when nothing else keeps it (`released`).
    pub fn set_pinned(&mut self, pinned: bool, can_show: bool, last: Option<&Sample>, overlay: &Area) -> Action {
        if std::mem::replace(&mut self.pinned, pinned) == pinned {
            return Action::None;
        }
        if pinned {
            if !self.visible && can_show {
                self.show();
                return Action::Show;
            }
            return Action::None;
        }
        self.released(last, overlay)
    }

    /// The sidebar page locks or unlocks the overlay (an HTML menu, popover or drag is open in it).
    /// A lock keeps a shown overlay open but never shows it; unlocking hides it at once when
    /// nothing else keeps it (`released`), e.g. a menu closed by a press outside.
    pub fn set_locked(&mut self, locked: bool, last: Option<&Sample>, overlay: &Area) -> Action {
        if std::mem::replace(&mut self.locked, locked) == locked || locked {
            return Action::None;
        }
        self.released(last, overlay)
    }

    /// A pin or lock ended: hide at once unless the other one still holds it or the last sample is
    /// in the keep zone (then the normal hide delay applies).
    fn released(&mut self, last: Option<&Sample>, overlay: &Area) -> Action {
        if self.visible && !self.pinned && !self.locked && !last.is_some_and(|s| s.in_keep_zone(overlay)) {
            self.hide();
            return Action::Hide;
        }
        Action::None
    }

    /// Esc in a page: hides an overlay core doesn't pin (a page lock doesn't count: the hide closes
    /// the page's menus); the pointer must leave the edge zone first.
    pub fn escape(&mut self) -> Action {
        if !self.visible || self.pinned {
            return Action::None;
        }
        self.hide();
        self.needs_exit = true;
        Action::Hide
    }

    /// The overlay must go away now (docked, page fullscreen, page gone, closing).
    pub fn force_hide(&mut self) -> bool {
        let was = self.visible;
        self.hide();
        was
    }

    /// The sidebar was just parked (Ctrl+S): no reveal until the pointer left the edge zone.
    pub fn on_parked(&mut self) {
        self.hide();
        self.needs_exit = true;
        self.press_in_zone = false;
    }

    /// The window moved or resized: a pointer that ends up in the edge zone (e.g. Aero Snap to the
    /// left) must leave it first.
    pub fn on_bounds_changed(&mut self) {
        if self.last_in_zone {
            self.needs_exit = true;
            self.dwell_since = None;
        }
    }

    /// Delay until the next sample. Fast whenever the pointer could reach the edge zone within one
    /// interval: over our window (it may jump to the edge), near its left edge, or while shown.
    pub fn next_delay(&self, s: &Sample) -> i64 {
        let near = s.x.abs() < NEAR_EDGE && s.y > -NEAR_EDGE && s.y < s.height + NEAR_EDGE;
        let mut delay = if self.visible || s.over_window || near { FAST_POLL_MS } else { SLOW_POLL_MS };
        if let Some(since) = self.dwell_since {
            delay = delay.min(since + DWELL_MS - s.now);
        }
        if let Some(since) = self.outside_since {
            delay = delay.min(since + HIDE_MS - s.now);
        }
        delay.max(1)
    }
}

// ----------------------------------------------------------------------------------- runtime

/// Debug-injected pointer (window client DIP).
#[derive(Debug, Clone, Copy)]
#[cfg_attr(not(debug_assertions), allow(dead_code))] // only `debug.hoverInput` creates one
struct VirtualPointer {
    x: f64,
    y: f64,
    buttons: bool,
    over_window: bool,
    owned_popup: bool,
}

thread_local! {
    static MACHINE: RefCell<Machine> = RefCell::new(Machine::default());
    static EPOCH: Instant = Instant::now();
    /// The sidebar page sent `ui.ready` since its last (re)load.
    static READY: Cell<bool> = const { Cell::new(false) };
    /// Pointer reveal enabled (debug builds: `STA_DEBUG_HOVER_REVEAL=0` / `debug.hoverInput`).
    static ENABLED: Cell<Option<bool>> = const { Cell::new(None) };
    static POLL_GEN: Cell<u64> = const { Cell::new(0) };
    static POLLING: Cell<bool> = const { Cell::new(false) };
    static HIDE_GEN: Cell<u64> = const { Cell::new(0) };
    static LAST: Cell<Option<Sample>> = const { Cell::new(None) };
    static VIRTUAL: Cell<Option<VirtualPointer>> = const { Cell::new(None) };
    /// When the virtual pointer last moved, and the time from that move to the latest reveal
    /// (`debug.info.sidebarHover.revealAfterMs`: reveal latency without CDP round trips).
    static VIRTUAL_MOVED_AT: Cell<Option<i64>> = const { Cell::new(None) };
    static REVEAL_AFTER_MS: Cell<Option<i64>> = const { Cell::new(None) };
    /// A virtual button press+release happened since the last sample (`debug.postMouse`).
    static VIRTUAL_CLICKED: Cell<bool> = const { Cell::new(false) };
    static COUNTS: Cell<(u64, u64, u64)> = const { Cell::new((0, 0, 0)) };
}

fn now_ms() -> i64 {
    EPOCH.with(|e| e.elapsed().as_millis() as i64)
}

fn enabled() -> bool {
    if let Some(e) = ENABLED.get() {
        return e;
    }
    #[cfg(debug_assertions)]
    let e = std::env::var("STA_DEBUG_HOVER_REVEAL").map(|v| v != "0").unwrap_or(true);
    #[cfg(not(debug_assertions))]
    let e = true;
    ENABLED.set(Some(e));
    e
}

pub fn is_ready() -> bool {
    READY.get()
}

/// The overlay may be shown now (pinned or by the pointer).
fn can_show() -> bool {
    READY.get() && window::sidebar_parked() && window::page_fullscreen_tab().is_none() && !window::is_closing()
}

/// Pointer reveal is possible now.
fn armed() -> bool {
    enabled() && can_show() && window::is_active() && !window::is_minimized()
}

/// The poll runs while pointer reveal is armed or the overlay is shown, never while minimized (a
/// pinned overlay stays "shown" meanwhile; restoring the window refreshes and restarts the poll).
fn should_poll() -> bool {
    let visible = MACHINE.with(|m| m.borrow().visible);
    (visible && can_show() && !window::is_minimized()) || armed()
}

/// Re-evaluates after sidebar placement, page fullscreen, activation, bounds or ready changes:
/// hides an overlay that can't stay, shows a pinned one, starts or stops the poll.
pub fn refresh() {
    if !can_show() {
        if MACHINE.with(|m| m.borrow_mut().force_hide()) {
            hide_now();
        }
    } else {
        let show = MACHINE.with(|m| {
            let mut m = m.borrow_mut();
            (m.pinned && !m.visible).then(|| m.show()).is_some()
        });
        if show {
            perform(Action::Show);
        }
    }
    if should_poll() && !POLLING.get() {
        POLLING.set(true);
        let generation = POLL_GEN.get() + 1;
        POLL_GEN.set(generation);
        task::post_ui(move || poll(generation));
    }
}

fn poll(generation: u64) {
    if POLL_GEN.get() != generation {
        return;
    }
    if !should_poll() {
        POLLING.set(false);
        return;
    }
    // No borrows across the sample: WindowFromPoint can run Chromium's hit test synchronously.
    let sample = sample_now();
    let overlay = overlay_area();
    let armed = armed();
    let action = MACHINE.with(|m| m.borrow_mut().step(&sample, &overlay, armed));
    LAST.set(Some(sample));
    perform(action);
    let delay = MACHINE.with(|m| m.borrow().next_delay(&sample));
    task::post_ui_delayed(delay, move || poll(generation));
}

/// The floating overlay's bounds in window client DIP.
fn overlay_area() -> Area {
    let r = overlays::sidebar_hover_rect();
    Area { x: r.x as f64, y: r.y as f64, w: r.width as f64, h: r.height as f64 }
}

fn sample_now() -> Sample {
    let now = now_ms();
    let Some(window) = window::main_window() else { return Sample::nowhere(now) };
    let client = window.client_area_bounds_in_screen();
    let resize_band = window.is_maximized() == 0 && window.is_fullscreen() == 0;
    let height = client.height as f64;
    if let Some(v) = VIRTUAL.get() {
        let clicked = VIRTUAL_CLICKED.replace(false);
        return Sample { x: v.x, y: v.y, height, over_window: v.over_window, owned_popup: v.owned_popup, buttons: v.buttons, clicked, resize_band, now };
    }
    if !enabled() {
        return Sample::nowhere(now);
    }
    let scale = window.display().map(|d| d.device_scale_factor() as f64).filter(|s| *s > 0.0).unwrap_or(1.0);
    match crate::platform::cursor_sample(window::hwnd_value()) {
        Some(c) => Sample {
            x: (c.x - c.client_left) as f64 / scale,
            y: (c.y - c.client_top) as f64 / scale,
            height,
            over_window: c.over_window,
            owned_popup: c.owned_popup,
            buttons: c.buttons,
            clicked: c.clicked,
            resize_band,
            now,
        },
        None => Sample::nowhere(now),
    }
}

fn perform(action: Action) {
    match action {
        Action::None => {}
        Action::Show => {
            let generation = HIDE_GEN.get() + 1;
            HIDE_GEN.set(generation); // cancels a hide still sliding (or fading) out
            cancel_exit(); // …and stops the shell waiting for that fade's acknowledgement
            // The card comes in from outside the window's left edge: it is placed there *before* it
            // is shown, so the reveal starts from nothing, and the page has the whole slide to
            // render what is on its way in. A reveal that interrupts a leave starts where that left
            // the card, never from the far side again.
            let travel = overlays::sidebar_hover_travel();
            let sliding = motion::slides();
            let start = match (sliding, overlays::is_visible(Overlay::SidebarHover)) {
                (false, _) => 0,
                (true, true) => overlays::sidebar_hover_slide(),
                (true, false) => -travel,
            };
            motion::cancel_slide();
            overlays::set_sidebar_hover_slide(start);
            overlays::show_sidebar_hover();
            emit_contents(true, false);
            if sliding {
                // The lead keeps the card outside the window while the page paints the contents it
                // was just told to show; a hide in that gap (or a dock) leaves it where it is.
                task::post_ui_delayed(motion::SLIDE_LEAD_MS, move || {
                    if HIDE_GEN.get() == generation && MACHINE.with(|m| m.borrow().visible) {
                        motion::slide_sidebar(overlays::sidebar_hover_slide(), 0, motion::SLIDE_IN_MS, || {});
                    }
                });
            }
            if VIRTUAL.get().is_some() {
                REVEAL_AFTER_MS.set(VIRTUAL_MOVED_AT.get().map(|at| now_ms() - at));
            }
            let (r, h, d) = COUNTS.get();
            COUNTS.set((r + 1, h, d));
            log_debug!("sidebar hover: shown");
        }
        Action::Hide => {
            let generation = HIDE_GEN.get() + 1;
            HIDE_GEN.set(generation);
            if motion::slides() && overlays::is_visible(Overlay::SidebarHover) {
                // The card leaves the way it came: it carries its contents out of the window and is
                // hidden once it is outside it. The page only closes its menus (`dismiss`) — blanking
                // it here would slide an empty card away — and blanks when nothing of it is on screen,
                // which is also why this hide needs no acknowledged exit: the frame it leaves behind
                // is outside the window, and the next reveal starts there.
                emit_contents(true, true);
                let travel = overlays::sidebar_hover_travel();
                motion::slide_sidebar(overlays::sidebar_hover_slide(), -travel, motion::SLIDE_OUT_MS, move || {
                    if HIDE_GEN.get() == generation && !MACHINE.with(|m| m.borrow().visible) {
                        overlays::hide_sidebar_hover();
                        emit_contents(false, true);
                        overlays::set_sidebar_hover_slide(0);
                    }
                });
            } else {
                // Motion off (or `reduced`): the card is where it belongs and only the page fades.
                // It blanks first and says when that frame is out; the overlay hides then, or at the
                // cap. A reveal meanwhile bumps `HIDE_GEN`, so the hide below never runs late.
                begin_exit(motion::HIDE_FLOOR_MS, move || {
                    if HIDE_GEN.get() == generation && !MACHINE.with(|m| m.borrow().visible) {
                        overlays::hide_sidebar_hover();
                    }
                });
            }
            if MACHINE.with(|m| m.borrow_mut().take_refocus()) {
                task::post_ui_delayed(REFOCUS_DELAY_MS, overlays::refocus_after_floating_click);
            }
            let (r, h, d) = COUNTS.get();
            COUNTS.set((r, h + 1, d));
            log_debug!("sidebar hover: hidden");
        }
        Action::Refocus => {
            task::post_ui_delayed(REFOCUS_DELAY_MS, overlays::refocus_after_floating_click);
        }
        Action::Dismiss => {
            emit_contents(true, true);
            if controller::with_store(|s| s.sidebar_panel().is_some_and(|p| p.is_transient())).unwrap_or(false) {
                controller::dispatch(Command::CloseSidebarPanel);
            }
            let (r, h, d) = COUNTS.get();
            COUNTS.set((r, h, d + 1));
        }
    }
}

/// Hides the overlay right away (it can't stay: docked, page fullscreen, page gone). Not part of the
/// exit protocol: the surface is going away with its host, so there is no frame left to blank —
/// docking re-parents the same view into the window, and page fullscreen hides everything.
fn hide_now() {
    HIDE_GEN.set(HIDE_GEN.get() + 1);
    cancel_exit();
    motion::cancel_slide();
    emit_contents(false, true);
    overlays::hide_sidebar_hover();
    overlays::set_sidebar_hover_slide(0);
    let (r, h, d) = COUNTS.get();
    COUNTS.set((r, h + 1, d));
}

/// The sidebar page's browser, while it has one.
fn sidebar_browser() -> Option<i32> {
    browsers::surface_browser(Surface::Sidebar).map(|b| b.identifier())
}

/// Starts an acknowledged exit of the sidebar contents ([`motion::begin`]): the page is told to hide
/// them and to report the blank frame, and `done` runs on that acknowledgement or at the cap — never
/// sooner than `floor` ([`motion::HIDE_FLOOR_MS`] for a hide, [`motion::PARK_FLOOR_MS`] for a park,
/// which window.rs uses through here).
pub fn begin_exit(floor: i64, done: impl FnOnce() + 'static) {
    let Some(id) = sidebar_browser() else {
        // No page to ask: hide after the floor all the same, so a page that appears meanwhile still
        // gets its frame (and the counter says a blank frame was not waited for).
        motion::note_early_hide();
        task::post_ui_delayed(floor, done);
        return;
    };
    motion::begin(id, motion::SIDEBAR_KEY, floor, |generation| emit_contents_gen(false, true, Some(generation)), done);
}

/// Drops a pending contents exit: the sidebar is being shown (or docked) again.
pub fn cancel_exit() {
    if let Some(id) = sidebar_browser() {
        motion::cancel(id);
    }
}

/// `sidebar.hover {visible, dismiss}` to the sidebar page: `visible` = the floating sidebar is
/// shown (or docked) from now on; `dismiss` = close menus and popovers.
pub fn emit_contents(visible: bool, dismiss: bool) {
    emit_contents_gen(visible, dismiss, None);
}

/// Like [`emit_contents`], plus the generation of an acknowledged exit: `gen` asks the page to
/// answer `surface.exited {gen}` once it has rendered the frame that shows nothing (PROTOCOL §14).
/// Only ever sent with `visible: false` — there is nothing to acknowledge about a reveal.
fn emit_contents_gen(visible: bool, dismiss: bool, generation: Option<u64>) {
    let Some(id) = sidebar_browser() else { return };
    // `gen` is a reserved word in Rust 2024; the wire field keeps the name.
    let tail = generation.map(|g| format!(",\"gen\":{g}")).unwrap_or_default();
    ipc::emit_raw_to(id, "sidebar.hover", &format!("{{\"visible\":{visible},\"dismiss\":{dismiss}{tail}}}"));
}

/// `SetSidebar.floating`.
pub fn set_pinned(pinned: bool) {
    let last = LAST.get();
    let overlay = overlay_area();
    let can = can_show();
    let action = MACHINE.with(|m| m.borrow_mut().set_pinned(pinned, can, last.as_ref(), &overlay));
    perform(action);
    refresh();
}

/// `sidebar.hoverLock {locked}` from the sidebar page (posted): an HTML menu, popover or drag is
/// open in it. Released when the page reloads, navigates or its renderer goes away.
pub fn set_locked(locked: bool) {
    let last = LAST.get();
    let overlay = overlay_area();
    let action = MACHINE.with(|m| m.borrow_mut().set_locked(locked, last.as_ref(), &overlay));
    perform(action);
    refresh();
}

/// Esc chain (keyboard.rs), at its end: hides a shown overlay that core doesn't pin. The caller
/// doesn't consume the key either way, so the page still gets its Escape. `true` = hidden.
pub fn escape() -> bool {
    let action = MACHINE.with(|m| m.borrow_mut().escape());
    let hidden = action == Action::Hide;
    perform(action);
    hidden
}

/// Pointer reveal is armed (or would be once parked) and the pointer is in the keep zone now.
pub fn pointer_in_keep_zone() -> bool {
    enabled() && READY.get() && window::is_active() && sample_now().in_keep_zone(&overlay_area())
}

/// window.rs parked the sidebar in the overlay host. `after_panel`: it was docked only for a panel
/// that holds input; with the pointer still over the sidebar it floats on at once.
pub fn on_parked(after_panel: bool) {
    let keep = after_panel && armed() && {
        let sample = sample_now();
        LAST.set(Some(sample));
        sample.in_keep_zone(&overlay_area())
    };
    let show = MACHINE.with(|m| {
        let mut m = m.borrow_mut();
        m.on_parked();
        if keep {
            m.needs_exit = false;
            m.show();
        }
        keep
    });
    if show {
        perform(Action::Show);
    }
    refresh();
}

/// window.rs docked the sidebar again (the overlay host was hidden and emptied).
pub fn on_docked() {
    HIDE_GEN.set(HIDE_GEN.get() + 1);
    cancel_exit(); // the view is in the window again; there is no floating frame left to blank
    motion::cancel_slide();
    overlays::set_sidebar_hover_slide(0);
    MACHINE.with(|m| m.borrow_mut().force_hide());
    refresh();
}

/// `WindowDelegate::on_window_bounds_changed` (inside a CEF callback: the refresh is posted).
pub fn on_bounds_changed() {
    MACHINE.with(|m| m.borrow_mut().on_bounds_changed());
    task::post_ui(refresh);
}

/// `ui.ready` from the sidebar page.
pub fn on_sidebar_ready() {
    READY.set(true);
    if MACHINE.with(|m| m.borrow().visible) {
        emit_contents(true, false); // a reloaded page starts with its contents shown anyway
    }
    refresh();
}

/// The sidebar's renderer is gone (it reloads): no reveal until it is ready again, and its lock
/// (if any) is gone with it.
pub fn on_sidebar_gone() {
    READY.set(false);
    set_locked(false);
}

/// A main-frame load started in the sidebar (reload or navigation, client.rs; posted): the old
/// document's menus are gone, so is its lock.
pub fn on_sidebar_load_start() {
    set_locked(false);
}

#[cfg_attr(not(debug_assertions), allow(dead_code))] // used by debug.rs only
pub fn debug_snapshot() -> serde_json::Value {
    let m = MACHINE.with(|m| m.borrow().clone());
    let (reveals, hides, dismisses) = COUNTS.get();
    let last = LAST.get().map(|s| {
        serde_json::json!({ "x": s.x, "y": s.y, "overWindow": s.over_window, "ownedPopup": s.owned_popup, "buttons": s.buttons, "inEdgeZone": s.in_edge_zone() })
    });
    let r = overlays::sidebar_hover_rect();
    serde_json::json!({
        "enabled": enabled(),
        "ready": READY.get(),
        "armed": armed(),
        "polling": POLLING.get(),
        "placement": if window::sidebar_parked() { "parked" } else { "docked" },
        "visible": m.visible,
        "overlayVisible": overlays::is_visible(Overlay::SidebarHover),
        "bounds": [r.x, r.y, r.width, r.height],
        "slideDx": overlays::sidebar_hover_slide(),
        "sliding": !motion::slide_settled(),
        "pinned": m.pinned,
        "locked": m.locked,
        "needsExit": m.needs_exit,
        "revealAfterMs": REVEAL_AFTER_MS.get(),
        "reveals": reveals,
        "hides": hides,
        "dismisses": dismisses,
        "virtualPointer": VIRTUAL.get().is_some(),
        "lastSample": last,
    })
}

/// `debug.hoverInput`: `{enabled?}` turns pointer reveal on/off; `{pointer: {x, y, buttons?,
/// overWindow?=true, ownedPopup?}}` injects a virtual pointer (window client DIP) that the poll
/// uses instead of the cursor; `{pointer: null}` goes back to the real cursor. Replies with the
/// snapshot.
#[cfg(debug_assertions)]
pub fn debug_input(payload: &serde_json::Value) -> Result<serde_json::Value, (i32, String)> {
    if let Some(e) = payload.get("enabled") {
        let e = e.as_bool().ok_or((400, "enabled must be a boolean".to_string()))?;
        ENABLED.set(Some(e));
        log_info!("sidebar hover: pointer reveal {}", if e { "enabled" } else { "disabled" });
    }
    if let Some(p) = payload.get("pointer") {
        if p.is_null() {
            VIRTUAL.set(None);
        } else {
            let num = |k: &str| p.get(k).and_then(|v| v.as_f64()).ok_or((400, format!("pointer.{k} must be a number")));
            let flag = |k: &str, d: bool| p.get(k).and_then(|v| v.as_bool()).unwrap_or(d);
            let next = VirtualPointer {
                x: num("x")?,
                y: num("y")?,
                buttons: flag("buttons", false),
                over_window: flag("overWindow", true),
                owned_popup: flag("ownedPopup", false),
            };
            if VIRTUAL.get().is_none_or(|v| (v.x, v.y) != (next.x, next.y)) {
                VIRTUAL_MOVED_AT.set(Some(now_ms()));
            }
            VIRTUAL.set(Some(next));
        }
    }
    refresh();
    Ok(debug_snapshot())
}

/// `debug.postMouse` posted a mouse message at window client DIP `(x, y)`: while a virtual pointer
/// is set it follows, so the poll sees the same pointer and buttons as Chromium. (`debug.postMouse`
/// posts Win32 messages, so this has no caller on other platforms.)
#[cfg(all(debug_assertions, windows))]
pub fn debug_note_mouse(x: f64, y: f64, buttons: Option<bool>) {
    let Some(mut v) = VIRTUAL.get() else { return };
    if (v.x, v.y) != (x, y) {
        VIRTUAL_MOVED_AT.set(Some(now_ms()));
    }
    v.x = x;
    v.y = y;
    if let Some(down) = buttons {
        if down && !v.buttons {
            VIRTUAL_CLICKED.set(true);
        }
        v.buttons = down;
    }
    VIRTUAL.set(Some(v));
}

#[cfg(test)]
mod tests {
    use super::*;

    const OVERLAY: Area = Area { x: 8.0, y: 8.0, w: 248.0, h: 684.0 };

    fn at(x: f64, y: f64, now: i64) -> Sample {
        Sample { x, y, height: 700.0, over_window: true, owned_popup: false, buttons: false, clicked: false, resize_band: true, now }
    }

    fn pressed(mut s: Sample) -> Sample {
        s.buttons = true;
        s
    }

    /// Feeds samples every `step` ms from `from` to `to` (inclusive), built by `make`.
    fn hold_sample(m: &mut Machine, make: impl Fn(i64) -> Sample, from: i64, to: i64, step: i64) -> Vec<(i64, Action)> {
        let mut out = Vec::new();
        let mut t = from;
        while t <= to {
            let a = m.step(&make(t), &OVERLAY, true);
            if a != Action::None {
                out.push((t, a));
            }
            t += step;
        }
        out
    }

    /// Feeds samples every `step` ms from `from` to `to` (inclusive) at one position.
    fn hold(m: &mut Machine, x: f64, from: i64, to: i64, step: i64) -> Vec<(i64, Action)> {
        let mut out = Vec::new();
        let mut t = from;
        while t <= to {
            let a = m.step(&at(x, 300.0, t), &OVERLAY, true);
            if a != Action::None {
                out.push((t, a));
            }
            t += step;
        }
        out
    }

    #[test]
    fn the_edge_zone_covers_the_resize_band_and_the_slop_outside_the_window() {
        let s = |x: f64, band: bool| Sample { resize_band: band, ..at(x, 300.0, 0) };
        // Restored: everything left of the band's far side, maximized: left of the zone itself.
        assert!(s(0.0, true).in_edge_zone() && s(3.9, true).in_edge_zone(), "the resize band reveals too");
        assert!(s(4.0, true).in_edge_zone() && s(11.9, true).in_edge_zone() && !s(12.0, true).in_edge_zone());
        assert!(s(0.0, false).in_edge_zone() && s(7.9, false).in_edge_zone() && !s(8.0, false).in_edge_zone());
        // Outside the window's left edge: the overshoot of a pointer thrown at it still counts…
        let outside = |x: f64| Sample { over_window: false, ..s(x, true) };
        assert!(outside(-0.1).in_edge_zone() && outside(-OUTSIDE_SLOP).in_edge_zone());
        assert!(!outside(-OUTSIDE_SLOP - 0.1).in_edge_zone(), "…but the pointer is somewhere else by then");
        assert!(!Sample { over_window: false, ..s(6.0, true) }.in_edge_zone(), "another window covers the edge");
        assert!(!at(6.0, -1.0, 0).in_edge_zone() && !at(6.0, 700.0, 0).in_edge_zone(), "full client height only");
        assert!(outside(-10.0).in_keep_zone(&OVERLAY), "the keep zone reaches out just as far");
        assert!(!Sample { y: 700.0, ..outside(-10.0) }.in_keep_zone(&OVERLAY));
        assert!(!Sample { x: -OUTSIDE_SLOP - 1.0, ..outside(-10.0) }.in_keep_zone(&OVERLAY));
    }

    #[test]
    fn a_flick_past_the_window_edge_reveals() {
        // The pointer leaves the window on its way to the edge and rests just outside it.
        let mut m = Machine::default();
        let out = |x: f64, t: i64| Sample { over_window: false, ..at(x, 300.0, t) };
        m.step(&at(300.0, 300.0, 0), &OVERLAY, true);
        assert_eq!(m.step(&out(-20.0, 33), &OVERLAY, true), Action::None);
        assert_eq!(m.step(&out(-20.0, 100), &OVERLAY, true), Action::None);
        assert_eq!(m.step(&out(-20.0, 153), &OVERLAY, true), Action::Show);
        // …and it stays open there.
        assert_eq!(m.step(&out(-20.0, 1000), &OVERLAY, true), Action::None);
        assert!(m.visible);
        // Far outside: no reveal, and a shown overlay hides.
        let mut m = Machine::default();
        assert_eq!(hold_sample(&mut m, |t| out(-200.0, t), 0, 600, 30), vec![]);
    }

    #[test]
    fn reveals_after_the_dwell_not_before() {
        let mut m = Machine::default();
        assert_eq!(hold(&mut m, 6.0, 0, 110, 10), vec![]);
        assert!(!m.visible);
        assert_eq!(m.step(&at(6.0, 300.0, 120), &OVERLAY, true), Action::Show);
        assert!(m.visible);

        // Leaving the zone in between restarts the dwell.
        let mut m = Machine::default();
        hold(&mut m, 6.0, 0, 60, 30);
        assert_eq!(m.step(&at(40.0, 300.0, 90), &OVERLAY, true), Action::None);
        assert_eq!(hold(&mut m, 6.0, 100, 210, 10), vec![]);
        assert_eq!(m.step(&at(6.0, 300.0, 220), &OVERLAY, true), Action::Show);

        // Not armed (inactive window, disabled): no reveal.
        let mut m = Machine::default();
        for t in (0..500).step_by(33) {
            assert_eq!(m.step(&at(6.0, 300.0, t), &OVERLAY, false), Action::None);
        }
    }

    #[test]
    fn next_delay_hits_the_deadlines() {
        let mut m = Machine::default();
        let elsewhere = |x: f64| Sample { over_window: false, ..at(x, 300.0, 0) };
        assert_eq!(m.next_delay(&elsewhere(500.0)), SLOW_POLL_MS, "over another window, far from our left edge");
        assert_eq!(m.next_delay(&elsewhere(60.0)), FAST_POLL_MS, "near our left edge");
        assert_eq!(m.next_delay(&elsewhere(-60.0)), FAST_POLL_MS, "just left of our window");
        assert_eq!(m.next_delay(&Sample::nowhere(0)), SLOW_POLL_MS);
        m.step(&at(6.0, 300.0, 1000), &OVERLAY, true);
        assert_eq!(m.next_delay(&at(6.0, 300.0, 1100)), 20, "wakes up at the dwell deadline");
        m.step(&at(6.0, 300.0, 1120), &OVERLAY, true);
        // Shown, and the pointer inside it: no deadline to wake for (leaving hides it at the next
        // sample), so the interval is the plain fast poll.
        assert!(m.visible);
        assert_eq!(m.next_delay(&at(100.0, 300.0, 1200)), FAST_POLL_MS);
    }

    #[test]
    fn hides_at_the_first_sample_outside_the_keep_zone() {
        let mut m = Machine { visible: true, ..Machine::default() };
        // Inside the overlay and its 8 DIP margin, and in the strip left of x = 16: stays.
        assert_eq!(hold(&mut m, 200.0, 0, 1000, 33), vec![]);
        assert_eq!(hold(&mut m, 8.0 + 248.0 + 7.9, 1000, 2000, 33), vec![]);
        assert!(at(10.0, 699.0, 0).in_keep_zone(&OVERLAY) && at(15.9, 699.0, 0).in_keep_zone(&OVERLAY));
        assert!(!at(8.0 + 248.0 + 8.0, 300.0, 0).in_keep_zone(&OVERLAY));
        assert!(!Sample { over_window: false, ..at(200.0, 300.0, 0) }.in_keep_zone(&OVERLAY));
        // Outside it: gone at once — what takes time is the card's slide out, which is the shell's.
        assert_eq!(m.step(&at(600.0, 300.0, 2033), &OVERLAY, true), Action::Hide);
        assert!(!m.visible);
        // Every sample of the way out counts, not just the first one after a dwell.
        let mut m = Machine { visible: true, ..Machine::default() };
        assert_eq!(m.step(&at(100.0, 300.0, 0), &OVERLAY, true), Action::None);
        assert_eq!(m.step(&at(263.9, 300.0, 33), &OVERLAY, true), Action::None, "still in the 8 DIP margin");
        assert_eq!(m.step(&at(600.0, 300.0, 66), &OVERLAY, true), Action::Hide);
    }

    #[test]
    fn a_held_button_an_owned_popup_or_a_pin_hold_it_open() {
        // A press that started inside and is dragged out: the held button holds it open.
        let mut m = Machine { visible: true, ..Machine::default() };
        m.step(&at(100.0, 300.0, 0), &OVERLAY, true);
        assert_eq!(m.step(&pressed(at(100.0, 300.0, 33)), &OVERLAY, true), Action::None);
        assert_eq!(m.step(&pressed(at(600.0, 300.0, 300)), &OVERLAY, true), Action::None, "held outside: stays");
        assert_eq!(m.step(&pressed(at(600.0, 300.0, 900)), &OVERLAY, true), Action::None);
        // Released out there it hides at once — and still hands focus back for that click.
        assert_eq!(m.step(&at(600.0, 300.0, 1000), &OVERLAY, true), Action::Hide);
        assert!(m.take_refocus() && !m.take_refocus(), "the owed refocus is taken exactly once");

        // The pointer over a popup of ours (a select list): not in the window, still ours.
        let mut m = Machine { visible: true, ..Machine::default() };
        let popup = Sample { over_window: false, owned_popup: true, ..at(300.0, 300.0, 0) };
        assert_eq!(m.step(&popup, &OVERLAY, true), Action::None);
        assert_eq!(m.step(&Sample { now: 3000, ..popup }, &OVERLAY, true), Action::None);
        assert!(m.visible);

        // A pin (a transient panel) holds it wherever the pointer is.
        let mut m = Machine { visible: true, pinned: true, ..Machine::default() };
        assert_eq!(hold(&mut m, 600.0, 0, 9000, 300), vec![]);
        assert!(m.visible);
    }

    #[test]
    fn press_in_the_zone_blocks_the_reveal_but_a_drag_entering_it_counts() {
        // Pressed at the edge (resize, a click in the gap): nothing while held.
        let mut m = Machine::default();
        m.step(&at(6.0, 300.0, 0), &OVERLAY, true);
        for t in (10..600).step_by(30) {
            assert_eq!(m.step(&pressed(at(6.0, 300.0, t)), &OVERLAY, true), Action::None);
        }
        // Released: the dwell starts again.
        assert_eq!(m.step(&at(6.0, 300.0, 600), &OVERLAY, true), Action::None);
        assert_eq!(m.step(&at(6.0, 300.0, 700), &OVERLAY, true), Action::None);
        assert_eq!(m.step(&at(6.0, 300.0, 720), &OVERLAY, true), Action::Show);

        // A drag that started in the page and moves into the zone reveals.
        let mut m = Machine::default();
        m.step(&at(300.0, 300.0, 0), &OVERLAY, true);
        m.step(&pressed(at(300.0, 300.0, 30)), &OVERLAY, true);
        assert_eq!(m.step(&pressed(at(6.0, 300.0, 60)), &OVERLAY, true), Action::None);
        assert_eq!(m.step(&pressed(at(6.0, 300.0, 180)), &OVERLAY, true), Action::Show);
        // …and the held drag keeps it open outside the keep zone.
        assert_eq!(m.step(&pressed(at(900.0, 300.0, 300)), &OVERLAY, true), Action::None);
        assert_eq!(m.step(&pressed(at(900.0, 300.0, 2000)), &OVERLAY, true), Action::None);
        assert!(m.visible);
    }

    #[test]
    fn a_press_outside_the_shown_overlay_dismisses_once() {
        let mut m = Machine { visible: true, ..Machine::default() };
        m.step(&at(100.0, 300.0, 0), &OVERLAY, true);
        assert_eq!(m.step(&pressed(at(100.0, 300.0, 30)), &OVERLAY, true), Action::None, "inside the overlay");
        m.step(&at(100.0, 300.0, 60), &OVERLAY, true);
        assert_eq!(m.step(&pressed(at(262.0, 300.0, 90)), &OVERLAY, true), Action::Dismiss, "in the margin, outside the overlay");
        assert_eq!(m.step(&pressed(at(262.0, 300.0, 120)), &OVERLAY, true), Action::None, "only on the press edge");
        m.step(&at(262.0, 300.0, 150), &OVERLAY, true);
        let popup = Sample { over_window: false, owned_popup: true, ..pressed(at(400.0, 300.0, 180)) };
        assert_eq!(m.step(&popup, &OVERLAY, true), Action::None, "a click in our own popup");
    }

    #[test]
    fn a_click_inside_the_shown_overlay_refocuses_after_the_release() {
        let mut m = Machine { visible: true, ..Machine::default() };
        m.step(&at(100.0, 300.0, 0), &OVERLAY, true);
        assert_eq!(m.step(&pressed(at(100.0, 300.0, 33)), &OVERLAY, true), Action::None);
        assert_eq!(m.step(&pressed(at(120.0, 300.0, 66)), &OVERLAY, true), Action::None, "dragging a row");
        // Released outside the keep zone: it hides there and then, and the focus hand-back the
        // click earned rides along with the hide (`take_refocus`) instead of being dropped.
        assert_eq!(m.step(&at(400.0, 300.0, 99), &OVERLAY, true), Action::Hide);
        assert!(m.take_refocus() && !m.take_refocus());
        // Released inside it: the plain refocus, once.
        let mut m = Machine { visible: true, ..Machine::default() };
        m.step(&at(100.0, 300.0, 0), &OVERLAY, true);
        m.step(&pressed(at(100.0, 300.0, 33)), &OVERLAY, true);
        assert_eq!(m.step(&at(120.0, 300.0, 99), &OVERLAY, true), Action::Refocus, "released inside");
        assert_eq!(m.step(&at(100.0, 300.0, 132), &OVERLAY, true), Action::None, "once");
        assert!(!m.take_refocus(), "nothing owed: the action was returned");
        // A click shorter than the poll interval.
        let quick = Sample { clicked: true, ..at(100.0, 300.0, 165) };
        assert_eq!(m.step(&quick, &OVERLAY, true), Action::Refocus);
        // A press that started outside never refocuses (here it dismisses: a lock holds the card
        // open, which is the only way a press lands outside it without hiding it).
        let mut m = Machine { visible: true, locked: true, ..Machine::default() };
        m.step(&at(600.0, 300.0, 167), &OVERLAY, true);
        assert_eq!(m.step(&pressed(at(600.0, 300.0, 200)), &OVERLAY, true), Action::Dismiss);
        assert_eq!(m.step(&at(100.0, 300.0, 233), &OVERLAY, true), Action::None);
        assert!(!m.take_refocus());
        // Hidden: clicks elsewhere are not ours.
        let mut m = Machine::default();
        m.step(&pressed(at(100.0, 300.0, 0)), &OVERLAY, true);
        assert_eq!(m.step(&at(100.0, 300.0, 33), &OVERLAY, true), Action::None);
    }

    #[test]
    fn needs_exit_after_park_escape_and_snap() {
        let mut m = Machine::default();
        m.on_parked();
        assert_eq!(hold(&mut m, 6.0, 0, 1000, 33), vec![], "Ctrl+S with the pointer at the edge");
        assert!(m.needs_exit);
        m.step(&at(50.0, 300.0, 1000), &OVERLAY, true);
        assert!(!m.needs_exit);
        assert_eq!(hold(&mut m, 6.0, 1100, 1300, 10), vec![(1220, Action::Show)]);
        // Esc hides; resting in the zone doesn't bring it back.
        assert_eq!(m.escape(), Action::Hide);
        assert_eq!(hold(&mut m, 6.0, 1300, 2000, 33), vec![]);
        m.step(&at(50.0, 300.0, 2000), &OVERLAY, true);
        assert_eq!(hold(&mut m, 6.0, 2100, 2300, 10).first(), Some(&(2220, Action::Show)));
        m.escape();
        // The window moved under a resting pointer (Aero Snap).
        let mut m = Machine::default();
        m.step(&at(6.0, 300.0, 0), &OVERLAY, true);
        m.on_bounds_changed();
        assert_eq!(hold(&mut m, 6.0, 30, 600, 30), vec![]);
        // Bounds changes with the pointer elsewhere change nothing.
        let mut m = Machine::default();
        m.step(&at(300.0, 300.0, 0), &OVERLAY, true);
        m.on_bounds_changed();
        assert!(!m.needs_exit);
    }

    #[test]
    fn pinning_shows_at_once_and_unpinning_hides_outside_the_keep_zone() {
        let mut m = Machine::default();
        assert_eq!(m.set_pinned(true, false, None, &OVERLAY), Action::None, "not showable yet (page not ready)");
        assert!(m.pinned && !m.visible);
        m.pinned = false;
        assert_eq!(m.set_pinned(true, true, None, &OVERLAY), Action::Show);
        assert_eq!(m.set_pinned(true, true, None, &OVERLAY), Action::None, "no change");
        assert_eq!(m.escape(), Action::None, "Esc leaves a pinned overlay alone");
        assert_eq!(m.set_pinned(false, true, Some(&at(900.0, 300.0, 0)), &OVERLAY), Action::Hide);
        assert!(!m.visible);
        // Un-pinned with the pointer over it: it stays, and goes when the pointer leaves.
        m.set_pinned(true, true, None, &OVERLAY);
        assert_eq!(m.set_pinned(false, true, Some(&at(100.0, 300.0, 0)), &OVERLAY), Action::None);
        assert!(m.visible);
        assert_eq!(m.step(&at(100.0, 300.0, 100), &OVERLAY, true), Action::None);
        assert_eq!(m.step(&at(900.0, 300.0, 500), &OVERLAY, true), Action::Hide);
        // Nowhere (pointer reveal disabled) is never in a zone.
        assert!(!Sample::nowhere(0).in_edge_zone() && !Sample::nowhere(0).in_keep_zone(&OVERLAY));
    }

    #[test]
    fn a_page_lock_keeps_it_open_and_unlocking_outside_hides_at_once() {
        // A lock (HTML context menu open) never shows a hidden overlay.
        let mut m = Machine::default();
        assert_eq!(m.set_locked(true, None, &OVERLAY), Action::None);
        assert!(m.locked && !m.visible);
        assert_eq!(m.set_locked(false, None, &OVERLAY), Action::None);

        // Shown and locked: the pointer away for a long time doesn't hide it.
        let mut m = Machine { visible: true, ..Machine::default() };
        m.step(&at(100.0, 300.0, 0), &OVERLAY, true);
        assert_eq!(m.set_locked(true, None, &OVERLAY), Action::None);
        assert_eq!(m.set_locked(true, None, &OVERLAY), Action::None, "no change");
        assert_eq!(hold(&mut m, 700.0, 100, 3000, 33), vec![], "menu open, pointer over the page");
        assert!(m.visible);
        // A press outside dismisses (the page closes its menu), then the unlock hides it at once.
        assert_eq!(m.step(&pressed(at(700.0, 300.0, 3033)), &OVERLAY, true), Action::Dismiss);
        let last = pressed(at(700.0, 300.0, 3033));
        assert_eq!(m.set_locked(false, Some(&last), &OVERLAY), Action::Hide);
        assert!(!m.visible && !m.locked);

        // Unlocked with the pointer in the keep zone (a menu item picked): it stays until the
        // pointer leaves, and hides on the first sample outside.
        let mut m = Machine { visible: true, locked: true, ..Machine::default() };
        assert_eq!(m.set_locked(false, Some(&at(100.0, 300.0, 0)), &OVERLAY), Action::None);
        assert_eq!(m.step(&at(100.0, 300.0, 33), &OVERLAY, true), Action::None);
        assert_eq!(m.step(&at(700.0, 300.0, 66), &OVERLAY, true), Action::Hide);

        // Pin and lock hold it independently: releasing one keeps it while the other holds.
        let mut m = Machine { visible: true, pinned: true, locked: true, ..Machine::default() };
        let away = at(700.0, 300.0, 0);
        assert_eq!(m.set_pinned(false, true, Some(&away), &OVERLAY), Action::None, "still locked (the page closes its popover a moment later)");
        assert_eq!(m.set_locked(false, Some(&away), &OVERLAY), Action::Hide);
        let mut m = Machine { visible: true, pinned: true, locked: true, ..Machine::default() };
        assert_eq!(m.set_locked(false, Some(&away), &OVERLAY), Action::None, "still pinned");
        assert_eq!(m.set_pinned(false, true, Some(&away), &OVERLAY), Action::Hide);

        // Esc hides a locked overlay (the hide closes the page's menus); a pin still blocks it.
        let mut m = Machine { visible: true, locked: true, ..Machine::default() };
        assert_eq!(m.escape(), Action::Hide);
        let mut m = Machine { visible: true, pinned: true, locked: true, ..Machine::default() };
        assert_eq!(m.escape(), Action::None);
    }

    #[test]
    fn a_press_in_the_resize_band_that_drifts_into_the_zone_does_not_reveal() {
        let band = |x: f64| Sample { resize_band: true, ..at(x, 300.0, 0) };
        assert!(band(0.0).in_resize_band() && band(3.9).in_resize_band() && !band(4.0).in_resize_band());
        assert!(band(2.0).in_edge_zone(), "the band is part of the zone: hovering it reveals, pressing it does not");
        assert!(!Sample { resize_band: false, ..band(2.0) }.in_resize_band(), "maximized: no band");
        assert!(!Sample { over_window: false, ..band(2.0) }.in_resize_band());

        // Pressed at x = 2 (resizing from the left edge), moved to x = 7 while held: nothing.
        let mut m = Machine::default();
        m.step(&at(2.0, 300.0, 0), &OVERLAY, true);
        m.step(&pressed(at(2.0, 300.0, 33)), &OVERLAY, true);
        for t in (66..900).step_by(33) {
            assert_eq!(m.step(&pressed(at(7.0, 300.0, t)), &OVERLAY, true), Action::None, "t = {t}");
        }
        // Released in the zone: the dwell starts then.
        assert_eq!(m.step(&at(7.0, 300.0, 900), &OVERLAY, true), Action::None);
        assert_eq!(m.step(&at(7.0, 300.0, 1020), &OVERLAY, true), Action::Show);

        // A maximized window has no band: x = 2 is the zone itself (still blocked while held).
        let mut m = Machine::default();
        let max = |x: f64, t: i64| Sample { resize_band: false, ..at(x, 300.0, t) };
        m.step(&max(2.0, 0), &OVERLAY, true);
        m.step(&Sample { buttons: true, ..max(2.0, 33) }, &OVERLAY, true);
        assert_eq!(m.step(&Sample { buttons: true, ..max(5.0, 400) }, &OVERLAY, true), Action::None);

        // A drag that starts in the page and crosses the band into the zone still reveals.
        let mut m = Machine::default();
        m.step(&at(300.0, 300.0, 0), &OVERLAY, true);
        m.step(&pressed(at(300.0, 300.0, 33)), &OVERLAY, true);
        m.step(&pressed(at(2.0, 300.0, 66)), &OVERLAY, true);
        m.step(&pressed(at(6.0, 300.0, 99)), &OVERLAY, true);
        assert_eq!(m.step(&pressed(at(6.0, 300.0, 219)), &OVERLAY, true), Action::Show);
    }

    /// Runs the poll loop (`next_delay` scheduling) with the pointer at `before` until `arrive`,
    /// then at the edge; returns the reveal time after the arrival.
    fn reveal_latency(before: Sample, arrive: i64) -> i64 {
        let mut m = Machine::default();
        let mut t = 0;
        loop {
            let s = if t < arrive { Sample { now: t, ..before } } else { at(6.0, 300.0, t) };
            if m.step(&s, &OVERLAY, true) == Action::Show {
                return t - arrive;
            }
            t += m.next_delay(&s);
            assert!(t < arrive + 5000, "never revealed");
        }
    }

    #[test]
    fn reveal_latency_is_the_dwell_plus_at_most_one_poll() {
        // The pointer jumps to the edge from far inside the window, at every phase of the poll.
        let far_inside = at(900.0, 300.0, 0);
        let worst = (1000..1000 + FAST_POLL_MS).map(|arrive| reveal_latency(far_inside, arrive)).max().unwrap();
        assert!((DWELL_MS..=DWELL_MS + FAST_POLL_MS).contains(&worst), "from inside the window: {worst} ms");
        // From another window (e.g. a second monitor) far from our left edge.
        let elsewhere = Sample { over_window: false, ..at(3000.0, 300.0, 0) };
        let worst = (1000..1000 + SLOW_POLL_MS).map(|arrive| reveal_latency(elsewhere, arrive)).max().unwrap();
        assert!((DWELL_MS..=DWELL_MS + SLOW_POLL_MS).contains(&worst) && worst <= 170, "from another window: {worst} ms");
    }
}
