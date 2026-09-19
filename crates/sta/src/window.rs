//! Main window and docked view tree [owner: chrome] (ARCHITECTURE §3.1, §4; docs/research/views.md).
//!
//! Responsibility:
//! - the frameless Alloy `Window` and its delegate (bounds, show state, can_* overrides, titlebar
//!   height, accelerators → keyboard.rs, OS theme changes);
//! - the docked tree: `[Sidebar BrowserView | Right column [Topbar BrowserView | Content frame
//!   (insets) [Content Panel [Empty BrowserView, tab wrappers (tabs.rs)]]]]`; overlay hosts are
//!   created by overlays.rs at the end of `on_window_created`;
//! - sidebar placement: docked in the window, or (while hidden) *parked* in the floating sidebar
//!   overlay host that sidebar_hover.rs shows on hover. [`apply_sidebar_placement`] is the only
//!   writer of the sidebar view's parent and visibility. Parking tells the page to hide its contents
//!   (`sidebar.hover {gen}`) and waits for its acknowledgement of that blank frame — at least
//!   `motion::PARK_FLOOR_MS`, at most the cap (`motion.rs`) — so the first reveal never shows a
//!   stale frame; a hidden sidebar at startup is created in the host directly;
//! - `SetSidebar` / live sidebar width, `SetChrome` colors (+ `on_theme_changed` re-apply, DWM),
//!   draggable regions (UI DragHandler → [`on_draggable_regions_changed`]), `WindowStateChanged`
//!   reporting, window actions, and the shutdown sequence (§3.1): `can_close` →
//!   `WindowCloseRequested`; `Effect::Quit` → [`begin_shutdown`] closes every browser; the last
//!   `on_before_close` closes the window; `on_window_destroyed` drops handles and quits the loop.
//!
//! Public API:
//! - `pub fn create_main_window(urls: Vec<String>)`
//! - `pub fn main_window() -> Option<Window>`, `pub fn hwnd_value() -> isize`
//! - `pub fn content_panel() -> Option<Panel>`, `pub fn empty_view() -> Option<BrowserView>`,
//!   `pub fn docked_view(surface: Surface) -> Option<BrowserView>`
//! - `pub const TOPBAR_HEIGHT: i32`
//! - `pub fn content_rect() -> Option<Rect>` — content panel bounds in window coordinates
//! - `pub fn view_rect_in_window(view: &View) -> Option<Rect>` — use instead of
//!   `View::convert_point_to_window`, which does not write its result back in cef 152.3.0
//! - `pub fn frame_color() -> u32`, `pub fn accent_color() -> u32`, `pub fn is_dark() -> bool`
//! - `pub fn set_sidebar(visible: bool, width: u32, floating: bool)`, `pub fn set_sidebar_width_live(width: u32)`,
//!   `pub fn note_sidebar_toggled()`,
//!   `pub fn sidebar_state() -> (bool, u32)`, `pub fn sidebar_width() -> i32`
//! - `pub fn sidebar_parked() -> bool`, `pub fn sidebar_on_screen() -> bool`,
//!   `pub fn client_size() -> Option<(i32, i32)>`, `pub fn is_active() -> bool`, `pub fn is_minimized() -> bool`
//! - `pub fn init_system_animations(enabled: bool)`, `pub fn check_system_animations()`,
//!   `pub fn motion_snapshot() -> serde_json::Value` (debug builds) — the Windows "Animation
//!   effects" setting (`docs/ARCHITECTURE.md` "Motion")
//! - `pub fn set_chrome(frame_argb: u32, dark: bool, card: [u32; 4])`, `pub fn init_system_dark(dark: bool)`,
//!   `pub fn check_system_theme()`
//! - `pub fn set_page_fullscreen(tab: Option<Id>)`, `pub fn page_fullscreen_tab() -> Option<Id>`
//! - `pub fn window_action(action: WindowAction)`, `pub fn activate()`, `pub fn focus_fallback()`
//! - `pub fn on_draggable_regions_changed(browser_id: i32, regions: Vec<DraggableRegion>)`,
//!   `pub fn schedule_draggable_regions()`, `pub fn apply_draggable_regions()`
//! - `pub fn begin_shutdown()`, `pub fn is_closing() -> bool`, `pub fn on_browser_closed()`,
//!   `pub fn release_surface(surface: Surface)`, `pub fn on_surface_closed(surface: Surface, browser_id: i32)`
//! - `pub fn debug_snapshot() -> serde_json::Value`
//!
//! Failure policy: if the view tree cannot be built at startup, an error box is shown and the
//! process exits (it must not linger invisibly holding the process singleton). If browsers do not
//! close within 8 s during shutdown, state is saved and the process exits without
//! `cef::shutdown` (which must never run with live browsers).

use crate::browsers::{self, Role, Surface, UiExtra};
use crate::overlays::Overlay;
use crate::{controller, keyboard, overlays, platform, rounded, sidebar_hover, tabs, task};
use sta_core::model::{SIDEBAR_MAX_WIDTH, SIDEBAR_MIN_WIDTH};
use sta_core::{Command, Id, WindowAction};
use cef::*;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;

/// Height of the HTML top bar (DIP).
pub const TOPBAR_HEIGHT: i32 = 40;
/// Content inset from the window edges (DIP).
const CONTENT_INSET: i32 = 8;
/// Minimum window size (DIP).
const MIN_WIDTH: i32 = 640;
const MIN_HEIGHT: i32 = 420;
/// Give up waiting for browsers to close after this long.
const SHUTDOWN_TIMEOUT_MS: i64 = 8000;

static ICON_16: &[u8] = include_bytes!("../res/icon-16.png");
static ICON_32: &[u8] = include_bytes!("../res/icon-32.png");
static ICON_48: &[u8] = include_bytes!("../res/icon-48.png");
static ICON_64: &[u8] = include_bytes!("../res/icon-64.png");

#[derive(Default)]
struct Views {
    window: Option<Window>,
    sidebar: Option<BrowserView>,
    right: Option<Panel>,
    topbar: Option<BrowserView>,
    content_frame: Option<Panel>,
    content: Option<Panel>,
    empty: Option<BrowserView>,
}

#[derive(Clone, Copy, PartialEq)]
struct Report {
    maximized: bool,
    fullscreen: bool,
    focused: bool,
    bounds: Option<sta_core::Rect>,
}

thread_local! {
    // Cells read by delegate callbacks (which can fire re-entrantly inside any CEF call).
    static FRAME: Cell<u32> = const { Cell::new(0xFF26_222E) };
    static ACCENT: Cell<u32> = const { Cell::new(0xFF7B_5CD6) };
    static DARK: Cell<bool> = const { Cell::new(false) };
    static SYSTEM_DARK: Cell<Option<bool>> = const { Cell::new(None) };
    /// Windows "Animation effects" (`SPI_GETCLIENTAREAANIMATION`) as last read.
    static SYSTEM_ANIMATIONS: Cell<Option<bool>> = const { Cell::new(None) };
    /// `(reads, changes, WM_SETTINGCHANGE messages)` of the motion setting, for `debug.info.motion`.
    static MOTION_COUNTERS: Cell<(u64, u64, u64)> = const { Cell::new((0, 0, 0)) };
    /// A `SetChrome` has been applied (the first one never waits for a theme cross-fade).
    static CHROME_APPLIED: Cell<bool> = const { Cell::new(false) };
    /// Generation of the delayed `SetChrome` (`set_chrome`): the newest colours win.
    static CHROME_GEN: Cell<u64> = const { Cell::new(0) };
    /// `(applied, delayed)` `SetChrome` calls, for `debug.info.motion`.
    static CHROME_COUNTERS: Cell<(u64, u64)> = const { Cell::new((0, 0)) };
    static SIDEBAR_WIDTH: Cell<i32> = const { Cell::new(sta_core::model::SIDEBAR_DEFAULT_WIDTH as i32) };
    /// Core's `SetSidebar.visible`: docked.
    static SIDEBAR_VISIBLE: Cell<bool> = const { Cell::new(true) };
    /// Core's `SetSidebar.floating`: pinned open in the floating sidebar.
    static SIDEBAR_FLOATING: Cell<bool> = const { Cell::new(false) };
    /// The sidebar view is parked in the floating sidebar host (else it is a child of the window).
    static SIDEBAR_PARKED: Cell<bool> = const { Cell::new(false) };
    /// A park waits for the page to hide its contents.
    static PARK_PENDING: Cell<bool> = const { Cell::new(false) };
    /// Docked only for a panel that holds input (the setting says hidden).
    static PANEL_DOCK: Cell<bool> = const { Cell::new(false) };
    static PARK_GEN: Cell<u64> = const { Cell::new(0) };
    static ACTIVE: Cell<bool> = const { Cell::new(false) };
    static CLOSING: Cell<bool> = const { Cell::new(false) };
    /// Tab in HTML5 page fullscreen (sidebar, topbar and insets hidden, window fullscreen).
    static PAGE_FULLSCREEN: Cell<Option<Id>> = const { Cell::new(None) };
    /// The window was already fullscreen (F11) when page fullscreen started.
    static WINDOW_FULLSCREEN_BEFORE_PAGE: Cell<bool> = const { Cell::new(false) };
    static REGIONS_PENDING: Cell<bool> = const { Cell::new(false) };

    static VIEWS: RefCell<Views> = RefCell::new(Views::default());
    static REGIONS: RefCell<HashMap<Surface, Vec<DraggableRegion>>> = RefCell::new(HashMap::new());
    static APPLIED_REGIONS: RefCell<Vec<DraggableRegion>> = const { RefCell::new(Vec::new()) };
    static LAST_REPORT: Cell<Option<Report>> = const { Cell::new(None) };
    static STARTUP_URLS: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

// ----------------------------------------------------------------------------------- creation

/// Creates the main window. The view tree is built inside `on_window_created`, which runs
/// synchronously inside `window_create_top_level`.
pub fn create_main_window(urls: Vec<String>) {
    STARTUP_URLS.with(|u| *u.borrow_mut() = urls);
    if let Some((visible, width)) = controller::with_store(|s| (s.window_state().sidebar_visible, s.window_state().sidebar_width)) {
        SIDEBAR_VISIBLE.set(visible);
        SIDEBAR_PARKED.set(!visible);
        SIDEBAR_WIDTH.set(clamp_sidebar_width(width));
    }
    let mut delegate = MainWindowDelegate::new();
    if window_create_top_level(Some(&mut delegate)).is_none() {
        fatal_startup_error("the main window could not be created");
    }
}

/// Startup cannot continue: tell the user, then exit so no invisible instance keeps the process
/// singleton (a second launch would otherwise just forward its command line to it).
fn fatal_startup_error(what: &str) -> ! {
    log_error!("fatal startup error: {what}");
    let logs = crate::paths::try_dirs().map(|d| d.logs.display().to_string()).unwrap_or_default();
    platform::show_error_box("sta", &format!("sta could not start: {what}.\n\nLogs: {logs}"));
    controller::emergency_exit(1)
}

fn clamp_sidebar_width(width: u32) -> i32 {
    width.clamp(SIDEBAR_MIN_WIDTH, SIDEBAR_MAX_WIDTH) as i32
}

fn box_settings(horizontal: bool, insets: Insets) -> BoxLayoutSettings {
    BoxLayoutSettings {
        horizontal: horizontal as i32,
        inside_border_insets: insets,
        cross_axis_alignment: AxisAlignment::STRETCH,
        ..Default::default()
    }
}

fn no_insets() -> Insets {
    Insets { top: 0, left: 0, bottom: 0, right: 0 }
}

fn content_insets() -> Insets {
    if PAGE_FULLSCREEN.get().is_some() {
        return no_insets();
    }
    let left = if SIDEBAR_PARKED.get() { CONTENT_INSET } else { 0 };
    Insets { top: 0, left, bottom: CONTENT_INSET, right: CONTENT_INSET }
}

/// The docked sidebar is shown (not parked, no page fullscreen).
fn sidebar_shown() -> bool {
    !SIDEBAR_PARKED.get() && PAGE_FULLSCREEN.get().is_none()
}

fn build(window: Window) {
    #[cfg(debug_assertions)]
    if std::env::var("STA_DEBUG_FAIL_STARTUP").is_ok_and(|v| v == "1") {
        fatal_startup_error("startup failure forced by STA_DEBUG_FAIL_STARTUP");
    }
    let frame = frame_color();
    platform::apply_window_chrome(platform::handle_value(window.window_handle()), is_dark());
    window.set_background_color(frame);
    let root_layout = window.set_to_box_layout(Some(&box_settings(true, no_insets())));

    // Sidebar (flex 0, width from SIDEBAR_WIDTH). A sidebar hidden at startup is created parked in
    // the floating sidebar host below instead (never attached and detached right away).
    let mut sidebar_delegate = SidebarDelegate::new();
    let Some(sidebar) = browsers::create_ui_view(&Surface::Sidebar.url(), UiExtra::default(), &mut sidebar_delegate) else {
        fatal_startup_error("the sidebar view could not be created");
    };
    sidebar.set_visible(1);
    if !SIDEBAR_PARKED.get() {
        window.add_child_view(Some(&mut View::from(&sidebar)));
    }

    // Right column: topbar over the content frame.
    let mut right_delegate = FramePanelDelegate::new();
    let Some(right) = panel_create(Some(&mut right_delegate)) else { fatal_startup_error("panel_create failed") };
    right.set_background_color(frame);
    let right_layout = right.set_to_box_layout(Some(&box_settings(false, no_insets())));
    window.add_child_view(Some(&mut View::from(&right)));
    if let Some(l) = &root_layout {
        l.set_flex_for_view(Some(&mut View::from(&right)), 1);
    }

    let Some(topbar) = browsers::surface_view(Surface::Topbar, 1, TOPBAR_HEIGHT) else {
        fatal_startup_error("the top bar view could not be created");
    };
    right.add_child_view(Some(&mut View::from(&topbar)));

    let mut frame_delegate = FramePanelDelegate::new();
    let Some(content_frame) = panel_create(Some(&mut frame_delegate)) else { fatal_startup_error("panel_create failed") };
    content_frame.set_background_color(frame);
    right.add_child_view(Some(&mut View::from(&content_frame)));
    if let Some(l) = &right_layout {
        l.set_flex_for_view(Some(&mut View::from(&content_frame)), 1);
    }

    let mut content_delegate = FramePanelDelegate::new();
    let Some(content) = panel_create(Some(&mut content_delegate)) else { fatal_startup_error("panel_create failed") };
    content.set_background_color(frame);
    content.set_to_box_layout(Some(&box_settings(true, no_insets())));
    content_frame.add_child_view(Some(&mut View::from(&content)));

    VIEWS.with(|v| {
        *v.borrow_mut() = Views {
            window: Some(window.clone()),
            sidebar: Some(sidebar.clone()),
            right: Some(right.clone()),
            topbar: Some(topbar.clone()),
            content_frame: Some(content_frame.clone()),
            content: Some(content.clone()),
            empty: None,
        }
    });
    relayout_content_frame();

    // Empty state view: first child of the content panel; tabs.rs toggles it for ShowContent.
    let Some(empty) = browsers::surface_view(Surface::Empty, 1, 1) else {
        fatal_startup_error("the empty-state view could not be created");
    };
    VIEWS.with(|v| v.borrow_mut().empty = Some(empty.clone()));
    content.add_child_view(Some(&mut View::from(&empty)));
    if let Some(layout) = content.get_layout().and_then(|l| l.as_box_layout()) {
        layout.set_flex_for_view(Some(&mut View::from(&empty)), 1);
    }
    // Overlays last (top-most): the content corner masks lowest, then the hosts; then accelerators
    // (need the widget), icons and title.
    rounded::create_masks(&window);
    overlays::create_hosts(&window);
    if SIDEBAR_PARKED.get() {
        overlays::adopt_sidebar(&sidebar); // the sidebar browser is created here
    }
    if [&sidebar, &topbar, &empty].iter().any(|v| v.browser().is_none()) {
        fatal_startup_error("a UI browser could not be created");
    }
    keyboard::install_accelerators(&window);
    set_icons(&window);
    window.set_title(Some(&CefString::from("sta")));

    let urls = STARTUP_URLS.with(|u| std::mem::take(&mut *u.borrow_mut()));
    controller::startup(urls);

    window.show();
    controller::start_tick();
    log_info!("main window created");
}

fn set_icons(window: &Window) {
    if let Some(mut small) = image_create() {
        small.add_png(1.0, Some(ICON_16));
        small.add_png(2.0, Some(ICON_32));
        window.set_window_icon(Some(&mut small));
    }
    if let Some(mut big) = image_create() {
        big.add_png(1.0, Some(ICON_32));
        big.add_png(1.5, Some(ICON_48));
        big.add_png(2.0, Some(ICON_64));
        window.set_window_app_icon(Some(&mut big));
    }
}

/// Re-applies the content frame layout (insets depend on sidebar visibility).
fn relayout_content_frame() {
    let (frame, content) = VIEWS.with(|v| {
        let v = v.borrow();
        (v.content_frame.clone(), v.content.clone())
    });
    let (Some(frame), Some(content)) = (frame, content) else { return };
    if let Some(layout) = frame.set_to_box_layout(Some(&box_settings(true, content_insets()))) {
        layout.set_flex_for_view(Some(&mut View::from(&content)), 1);
    }
    frame.layout();
}

fn initial_bounds() -> Rect {
    let saved = controller::with_store(|s| s.window_state().bounds).flatten();
    if let Some(b) = saved.filter(|b| b.width > 0 && b.height > 0) {
        let r = Rect { x: b.x, y: b.y, width: b.width, height: b.height };
        // The display with the largest intersection (or the nearest one when the saved bounds are
        // off-screen, e.g. a monitor was unplugged): clamp the bounds into its work area.
        if let Some(display) = display_get_matching_bounds(Some(&r), 0) {
            return clamp_to_work_area(r, display.work_area());
        }
    }
    let wa = display_get_primary().map(|d| d.work_area()).unwrap_or(Rect { x: 0, y: 0, width: 1920, height: 1040 });
    let width = 1280.min(wa.width - 80).max(MIN_WIDTH);
    let height = 820.min(wa.height - 60).max(MIN_HEIGHT);
    Rect { x: wa.x + (wa.width - width) / 2, y: wa.y + (wa.height - height) / 2, width, height }
}

/// Shrinks `r` to fit `work_area` (but not below the minimum size) and moves it inside.
fn clamp_to_work_area(r: Rect, wa: Rect) -> Rect {
    let width = r.width.max(MIN_WIDTH).min(wa.width.max(1));
    let height = r.height.max(MIN_HEIGHT).min(wa.height.max(1));
    let x = r.x.min(wa.x + wa.width - width).max(wa.x);
    let y = r.y.min(wa.y + wa.height - height).max(wa.y);
    Rect { x, y, width, height }
}

// ----------------------------------------------------------------------------------- delegates

wrap_window_delegate! {
    struct MainWindowDelegate {}

    impl ViewDelegate {
        fn minimum_size(&self, _view: Option<&mut View>) -> Size {
            Size { width: MIN_WIDTH, height: MIN_HEIGHT }
        }

        fn on_layout_changed(&self, _view: Option<&mut View>, _new_bounds: Option<&Rect>) {
            rounded::layout_masks();
            overlays::layout();
            schedule_draggable_regions();
        }

        fn on_theme_changed(&self, view: Option<&mut View>) {
            // CEF resets background colors whenever the theme is (re)applied.
            if let Some(view) = view {
                view.set_background_color(frame_color());
            }
        }
    }

    impl PanelDelegate {}

    impl WindowDelegate {
        fn on_window_created(&self, window: Option<&mut Window>) {
            if let Some(window) = window {
                build(window.clone());
            }
        }

        fn on_window_closing(&self, _window: Option<&mut Window>) {
            log_info!("window closing");
        }

        fn on_window_destroyed(&self, _window: Option<&mut Window>) {
            teardown();
        }

        fn on_window_activation_changed(&self, window: Option<&mut Window>, active: i32) {
            ACTIVE.set(active != 0);
            if let Some(window) = window {
                report_state(window);
            }
            task::post_ui(sidebar_hover::refresh);
        }

        fn on_window_bounds_changed(&self, window: Option<&mut Window>, _new_bounds: Option<&Rect>) {
            if let Some(window) = window {
                report_state(window);
            }
            sidebar_hover::on_bounds_changed();
        }

        fn on_window_fullscreen_transition(&self, window: Option<&mut Window>, is_completed: i32) {
            if let (Some(window), true) = (window, is_completed != 0) {
                report_state(window);
            }
        }

        fn initial_bounds(&self, _window: Option<&mut Window>) -> Rect {
            initial_bounds()
        }

        fn initial_show_state(&self, _window: Option<&mut Window>) -> ShowState {
            if controller::with_store(|s| s.window_state().maximized).unwrap_or(false) {
                ShowState::MAXIMIZED
            } else {
                ShowState::NORMAL
            }
        }

        fn is_frameless(&self, _window: Option<&mut Window>) -> i32 {
            1
        }

        fn titlebar_height(&self, _window: Option<&mut Window>, titlebar_height: Option<&mut f32>) -> i32 {
            // Keeps CEF-positioned dialogs below the HTML top bar.
            if let Some(h) = titlebar_height {
                *h = TOPBAR_HEIGHT as f32;
            }
            1
        }

        // The Rust trait defaults for these are 0 (C++ defaults are true).
        fn can_resize(&self, _window: Option<&mut Window>) -> i32 {
            1
        }

        fn can_maximize(&self, _window: Option<&mut Window>) -> i32 {
            1
        }

        fn can_minimize(&self, _window: Option<&mut Window>) -> i32 {
            1
        }

        fn can_close(&self, _window: Option<&mut Window>) -> i32 {
            can_close()
        }

        fn on_accelerator(&self, _window: Option<&mut Window>, command_id: i32) -> i32 {
            keyboard::on_accelerator(command_id) as i32
        }

        fn on_key_event(&self, _window: Option<&mut Window>, event: Option<&KeyEvent>) -> i32 {
            event.is_some_and(keyboard::on_window_key_event) as i32
        }

        fn on_theme_colors_changed(&self, _window: Option<&mut Window>, _chrome_theme: i32) {
            on_os_theme_colors_changed();
        }

        fn window_runtime_style(&self) -> RuntimeStyle {
            RuntimeStyle::ALLOY
        }
    }
}

wrap_browser_view_delegate! {
    struct SidebarDelegate {}

    impl ViewDelegate {
        fn preferred_size(&self, _view: Option<&mut View>) -> Size {
            // Height must be > 0 or CEF ignores the size; the box layout stretches it.
            Size { width: SIDEBAR_WIDTH.get(), height: 1 }
        }
    }

    impl BrowserViewDelegate {
        fn on_browser_created(&self, _browser_view: Option<&mut BrowserView>, browser: Option<&mut Browser>) {
            if let Some(browser) = browser {
                browsers::set_role(browser.identifier(), Role::Surface(Surface::Sidebar));
            }
        }

        fn browser_runtime_style(&self) -> RuntimeStyle {
            RuntimeStyle::ALLOY
        }
    }
}

wrap_panel_delegate! {
    struct FramePanelDelegate {}

    impl ViewDelegate {
        fn on_theme_changed(&self, view: Option<&mut View>) {
            if let Some(view) = view {
                view.set_background_color(frame_color());
            }
        }
    }

    impl PanelDelegate {}
}

// ----------------------------------------------------------------------------------- queries

pub fn main_window() -> Option<Window> {
    VIEWS.with(|v| v.borrow().window.clone())
}

/// The main window's OS handle as an integer (`HWND` on Windows, `NSView*` on macOS; 0 if none).
pub fn hwnd_value() -> isize {
    main_window().map(|w| platform::handle_value(w.window_handle())).unwrap_or(0)
}

pub fn content_panel() -> Option<Panel> {
    VIEWS.with(|v| v.borrow().content.clone())
}

pub fn empty_view() -> Option<BrowserView> {
    VIEWS.with(|v| v.borrow().empty.clone())
}

/// The docked BrowserView of a UI surface (sidebar, top bar, empty state); `None` for overlays.
pub fn docked_view(surface: Surface) -> Option<BrowserView> {
    VIEWS.with(|v| {
        let v = v.borrow();
        match surface {
            Surface::Sidebar => v.sidebar.clone(),
            Surface::Topbar => v.topbar.clone(),
            Surface::Empty => v.empty.clone(),
            _ => None,
        }
    })
}

/// Content panel bounds in window coordinates.
pub fn content_rect() -> Option<Rect> {
    view_rect_in_window(&View::from(&content_panel()?))
}

/// Bounds of an attached view in window coordinates, also for views inside overlay hosts.
///
/// Do not use `View::convert_point_to_window` & co.: in cef 152.3.0 the binding copies the
/// `Point` into a temporary and never writes the converted value back. Instead, sum the
/// parent-relative `bounds()` up to (excluding) the Window, whose own bounds are screen DIP. An
/// overlay host's contents view is the root of its own widget (no parent): its origin in the
/// window is the overlay controller's bounds.
pub fn view_rect_in_window(view: &View) -> Option<Rect> {
    if view.is_attached() == 0 {
        return None;
    }
    let b = view.bounds();
    let is_window = |v: &View| v.as_panel().and_then(|panel| panel.as_window()).is_some();
    if is_window(view) {
        return Some(Rect { x: 0, y: 0, width: b.width, height: b.height });
    }
    let (mut x, mut y) = (b.x, b.y);
    let mut root = view.clone();
    while let Some(parent) = root.parent_view() {
        if is_window(&parent) {
            return Some(Rect { x, y, width: b.width, height: b.height });
        }
        if parent.parent_view().is_some() {
            let pb = parent.bounds();
            x += pb.x;
            y += pb.y;
        }
        root = parent;
    }
    // `root` has no parent and is not the Window: an overlay host's contents view. Its own
    // bounds inside the overlay widget are not added; the controller bounds place it instead.
    if let Some(origin) = overlays::root_origin_in_window(&root) {
        if root.is_same(Some(&mut view.clone())) != 0 {
            return Some(Rect { x: origin.x, y: origin.y, width: b.width, height: b.height });
        }
        x += origin.x;
        y += origin.y;
    }
    Some(Rect { x, y, width: b.width, height: b.height })
}

/// Opaque frame color (ARGB) of the active space.
pub fn frame_color() -> u32 {
    FRAME.get()
}

/// Accent color (ARGB) of the active space (focused split pane border).
pub fn accent_color() -> u32 {
    ACCENT.get()
}

pub fn is_dark() -> bool {
    DARK.get()
}

pub fn is_closing() -> bool {
    CLOSING.get()
}

/// The main window is the active window.
pub fn is_active() -> bool {
    ACTIVE.get()
}

pub fn is_minimized() -> bool {
    main_window().is_some_and(|w| w.is_minimized() != 0)
}

/// Client area size of the main window (DIP): the extent of window coordinates.
pub fn client_size() -> Option<(i32, i32)> {
    main_window().map(|w| {
        let r = w.client_area_bounds_in_screen();
        (r.width, r.height)
    })
}

/// Current sidebar width (DIP), docked or floating.
pub fn sidebar_width() -> i32 {
    SIDEBAR_WIDTH.get()
}

/// The sidebar view is parked in the floating sidebar host (the sidebar is hidden).
pub fn sidebar_parked() -> bool {
    SIDEBAR_PARKED.get()
}

/// The sidebar page is on screen: docked and shown, or floating and revealed.
pub fn sidebar_on_screen() -> bool {
    if SIDEBAR_PARKED.get() {
        overlays::is_visible(Overlay::SidebarHover)
    } else {
        sidebar_shown()
    }
}

/// `(visible, width)` of the docked sidebar as requested by core (hidden anyway in page fullscreen).
#[allow(dead_code)] // stage-2 API
pub fn sidebar_state() -> (bool, u32) {
    (SIDEBAR_VISIBLE.get(), SIDEBAR_WIDTH.get() as u32)
}

// ----------------------------------------------------------------------------------- effects

/// `Effect::SetSidebar`: `visible` = docked; `floating` = pinned open in the floating sidebar.
pub fn set_sidebar(visible: bool, width: u32, floating: bool) {
    SIDEBAR_VISIBLE.set(visible);
    SIDEBAR_FLOATING.set(floating);
    SIDEBAR_WIDTH.set(clamp_sidebar_width(width));
    apply_sidebar_placement(true);
    sidebar_hover::set_pinned(floating);
}

/// `ToggleSidebar` was applied (controller.rs): the sidebar is no longer docked only for a panel.
pub fn note_sidebar_toggled() {
    PANEL_DOCK.set(false);
}

/// Places the sidebar view (docked / parked) and sets its visibility, then lays out the window.
/// The only writer of both: a parked view always stays visible inside its host (the host's
/// overlay controller decides what is on screen).
fn apply_sidebar_placement(allow_fade: bool) {
    let Some((window, sidebar)) = place_sidebar(allow_fade) else { return };
    sidebar.invalidate_layout();
    relayout_content_frame();
    window.layout();
    overlays::layout();
    schedule_draggable_regions();
    sidebar_hover::refresh();
}

/// Placement and visibility only (see [`apply_sidebar_placement`]).
fn place_sidebar(allow_fade: bool) -> Option<(Window, BrowserView)> {
    let (window, sidebar) = VIEWS.with(|v| {
        let v = v.borrow();
        (v.window.clone(), v.sidebar.clone())
    });
    let (window, sidebar) = (window?, sidebar?);
    let want_parked = !SIDEBAR_VISIBLE.get();
    let parked = SIDEBAR_PARKED.get();
    if want_parked && !parked {
        if PARK_PENDING.get() {
            // The fade already running parks it.
        } else if allow_fade
            && sidebar_shown()
            && sidebar_hover::is_ready()
            && !CLOSING.get()
            && window.is_minimized() == 0
            && !(PANEL_DOCK.get() && sidebar_hover::pointer_in_keep_zone())
        {
            // The page hides its contents first: the parked page renders no frames, and the
            // first reveal would otherwise show the docked frame for a moment. The page says when
            // that blank frame is out (`surface.exited`), and the park happens then — never sooner
            // than `motion::PARK_FLOOR_MS`, never later than the cap (motion.rs).
            PARK_PENDING.set(true);
            let generation = PARK_GEN.get() + 1;
            PARK_GEN.set(generation);
            sidebar_hover::begin_exit(crate::motion::PARK_FLOOR_MS, move || {
                if PARK_GEN.get() == generation && PARK_PENDING.replace(false) {
                    apply_sidebar_placement(false);
                }
            });
        } else {
            park(&window, &sidebar);
        }
    } else if !want_parked && parked {
        dock(&window, &sidebar);
    } else if !want_parked && PARK_PENDING.replace(false) {
        // Docked again before the fade ended.
        PARK_GEN.set(PARK_GEN.get() + 1);
        sidebar_hover::cancel_exit();
        sidebar_hover::emit_contents(true, false);
    }
    sidebar.set_visible(if SIDEBAR_PARKED.get() { 1 } else { sidebar_shown() as i32 });
    Some((window, sidebar))
}

/// Moves the sidebar view from the window into the floating sidebar host (hidden).
fn park(window: &Window, sidebar: &BrowserView) {
    let after_panel = PANEL_DOCK.replace(false);
    let had_focus = sidebar.browser().is_some_and(|b| overlays::focused_browser() == Some(b.identifier()));
    window.remove_child_view(Some(&mut View::from(sidebar)));
    SIDEBAR_PARKED.set(true);
    overlays::adopt_sidebar(sidebar);
    log_debug!("sidebar parked");
    if had_focus {
        // The floating sidebar never holds keyboard focus.
        task::post_ui(overlays::restore_main_focus);
    }
    sidebar_hover::on_parked(after_panel);
}

/// Moves the parked sidebar view back into the window (first child), in one task: one frame.
fn dock(window: &Window, sidebar: &BrowserView) {
    let was_floating = overlays::is_visible(Overlay::SidebarHover);
    overlays::release_sidebar(sidebar);
    window.add_child_view_at(Some(&mut View::from(sidebar)), 0);
    SIDEBAR_PARKED.set(false);
    PANEL_DOCK.set(controller::with_store(|s| !s.window_state().sidebar_visible).unwrap_or(false));
    log_debug!("sidebar docked");
    sidebar_hover::on_docked();
    sidebar_hover::emit_contents(true, false);
    // Docked for a panel that holds input (e.g. "Rename" picked in the floating sidebar): the
    // user is working in the sidebar, so it takes keyboard focus.
    let wants_keys = controller::with_store(|s| s.sidebar_panel().is_some_and(|p| !p.is_transient())).unwrap_or(false);
    if was_floating && wants_keys {
        let view = sidebar.clone();
        task::post_ui(move || view.request_focus());
    }
}

/// Live drag-resize from `sidebar.setWidth` (not persisted).
pub fn set_sidebar_width_live(width: u32) {
    SIDEBAR_WIDTH.set(clamp_sidebar_width(width));
    let (window, sidebar) = VIEWS.with(|v| {
        let v = v.borrow();
        (v.window.clone(), v.sidebar.clone())
    });
    if let Some(sidebar) = sidebar {
        sidebar.invalidate_layout();
    }
    if let Some(window) = window {
        window.layout();
    }
    if SIDEBAR_PARKED.get() {
        overlays::layout();
    }
    schedule_draggable_regions();
}

/// Remembers the OS theme at startup (for change detection in `on_theme_colors_changed`).
pub fn init_system_dark(dark: bool) {
    SYSTEM_DARK.set(Some(dark));
}

/// Reads the OS app theme and dispatches `SystemThemeChanged` if it differs from the last value.
/// Called from `on_theme_colors_changed` and polled by the controller heartbeat (in case the
/// Chromium theme notification does not fire for a registry-only change).
pub fn check_system_theme() {
    let dark = platform::system_dark_mode();
    if SYSTEM_DARK.replace(Some(dark)) != Some(dark) {
        log_info!("system theme changed: dark={dark}");
        controller::dispatch(Command::SystemThemeChanged { dark });
    }
}

/// Remembers the Windows "Animation effects" setting at startup and starts watching
/// `WM_SETTINGCHANGE` for it (`docs/ARCHITECTURE.md` "Motion").
pub fn init_system_animations(enabled: bool) {
    SYSTEM_ANIMATIONS.set(Some(enabled));
    MOTION_COUNTERS.set((1, 0, 0));
    platform::watch_setting_change(on_setting_change);
}

/// `WM_SETTINGCHANGE` observer. It runs inside Windows' hook, so it does nothing but count the
/// message and defer the work: reading the setting and dispatching a command must never happen
/// re-entrantly inside a sent message.
fn on_setting_change() {
    let (reads, changes, messages) = MOTION_COUNTERS.get();
    MOTION_COUNTERS.set((reads, changes, messages + 1));
    task::post_ui(check_system_animations);
}

/// Reads `SPI_GETCLIENTAREAANIMATION` and dispatches `SystemAnimationsChanged` when it differs
/// from the last value. Called at init, on `WM_SETTINGCHANGE` and by the controller heartbeat.
pub fn check_system_animations() {
    let enabled = platform::system_animations();
    let (reads, changes, messages) = MOTION_COUNTERS.get();
    let changed = SYSTEM_ANIMATIONS.replace(Some(enabled)) != Some(enabled);
    MOTION_COUNTERS.set((reads + 1, changes + u64::from(changed), messages));
    if changed {
        log_info!("windows animation effects changed: enabled={enabled}");
        controller::dispatch(Command::SystemAnimationsChanged { enabled });
    }
}

/// `debug.info.motion` (debug builds): what core resolved, the Windows setting, the correctness
/// delays the shell keeps whatever the settings say, and the counters of the acknowledged exits
/// (`motion.rs`).
#[cfg_attr(not(debug_assertions), allow(dead_code))] // used by debug.rs only
pub fn motion_snapshot() -> serde_json::Value {
    use crate::motion;
    let view = controller::with_store(|s| s.motion_view()).unwrap_or_default();
    let (reads, changes, messages) = MOTION_COUNTERS.get();
    let (chrome_calls, chrome_delayed) = CHROME_COUNTERS.get();
    let mut out = serde_json::json!({
        "level": view.level.as_str(),
        "off": view.off,
        "systemAnimations": view.system_animations,
        "systemAnimationsRead": SYSTEM_ANIMATIONS.get(),
        // The waits around a surface's blank frame: never below the floors, never above the cap.
        "hideDelayMs": motion::hide_delay_ms(motion::SIDEBAR_KEY),
        "parkDelayMs": motion::park_delay_ms(motion::SIDEBAR_KEY),
        "toastDelayMs": motion::hide_delay_ms(motion::TOAST_KEY),
        "switcherDelayMs": motion::hide_delay_ms(motion::SWITCHER_KEY),
        "hideFloorMs": motion::HIDE_FLOOR_MS,
        "parkFloorMs": motion::PARK_FLOOR_MS,
        "waitCapMs": motion::WAIT_CAP_MS,
        // The fade the page plays inside that wait (0 when the animation is switched off).
        "fadeMs": motion::exit_fade_ms(motion::SIDEBAR_KEY),
        "themeFadeMs": motion::THEME_FADE_MS,
        "chromeDelayMs": motion::chrome_delay_ms(),
        "chromeCalls": chrome_calls,
        "chromeDelayed": chrome_delayed,
        "reads": reads,
        "changes": changes,
        "settingMessages": messages,
    });
    if let (Some(dst), Some(src)) = (out.as_object_mut(), motion::debug_counters().as_object()) {
        for (k, v) in src {
            dst.insert(k.clone(), v.clone());
        }
    }
    out
}

fn on_os_theme_colors_changed() {
    check_system_theme();
    // Chromium may reset DWM attributes on theme changes; re-apply ours.
    if let Some(window) = main_window() {
        platform::apply_window_chrome(platform::handle_value(window.window_handle()), is_dark());
    }
}

/// `Effect::SetChrome`: `card` = `[accent, surface, border, frame_border]` ARGB (`0` = not sent by
/// an older producer: derived from the active space theme here).
///
/// While the pages cross-fade their own colours (`theme.crossFade`), the native card fill, border
/// and corner tiles can only **snap** — so they snap at the *midpoint* of that fade instead of at
/// its start, where a 3-4 DIP ring of mismatched colour would stand for the whole 300 ms (critique
/// issue 12). The newest call wins (a fast run of space switches applies the last colours once), and
/// the delay is skipped where there is no fade to meet: the first call of the session (nothing has
/// been painted yet), a minimized or closing window, and motion or the key switched off.
pub fn set_chrome(frame_argb: u32, dark: bool, card: [u32; 4]) {
    let delay = crate::motion::chrome_delay_ms();
    let first = !CHROME_APPLIED.replace(true);
    let hidden = main_window().is_none_or(|w| w.is_minimized() != 0) || CLOSING.get();
    let (applied, delayed) = CHROME_COUNTERS.get();
    if delay == 0 || first || hidden {
        CHROME_COUNTERS.set((applied + 1, delayed));
        apply_chrome(frame_argb, dark, card);
        return;
    }
    CHROME_COUNTERS.set((applied + 1, delayed + 1));
    let generation = CHROME_GEN.get() + 1;
    CHROME_GEN.set(generation);
    task::post_ui_delayed(delay, move || {
        if CHROME_GEN.get() == generation && !CLOSING.get() {
            apply_chrome(frame_argb, dark, card);
        }
    });
}

/// Applies the chrome colours to the window, the panels, the overlay cards and the tab wrappers.
fn apply_chrome(frame_argb: u32, dark: bool, card: [u32; 4]) {
    let frame = frame_argb | 0xFF00_0000;
    FRAME.set(frame);
    if DARK.replace(dark) != dark {
        crate::devtools::on_dark_changed(); // UX16: an open frontend follows sta's dark mode
    }
    let derived = card.contains(&0).then(|| {
        controller::with_store(|s| {
            let st = s.state();
            let theme = st.spaces.iter().find(|sp| sp.id == st.window.active_space).map(|sp| sp.theme.clone()).unwrap_or_default();
            sta_core::theme::chrome_argb(&theme, dark)
        })
        .unwrap_or_else(|| sta_core::theme::chrome_argb(&sta_core::Theme::default(), dark))
    });
    let pick = |sent: u32, derive: fn(&sta_core::theme::ChromeArgb) -> u32| {
        if sent != 0 { sent | 0xFF00_0000 } else { derived.as_ref().map(derive).unwrap_or(0xFF80_8080) }
    };
    let [accent, surface, border, frame_border] = card;
    let colors = rounded::ChromeColors {
        frame,
        accent: pick(accent, |c| c.accent),
        surface: pick(surface, |c| c.surface),
        border: pick(border, |c| c.border),
        frame_border: pick(frame_border, |c| c.frame_border),
        dark,
    };
    ACCENT.set(colors.accent);
    rounded::set_colors(colors);
    let (window, right, content_frame, content) = VIEWS.with(|v| {
        let v = v.borrow();
        (v.window.clone(), v.right.clone(), v.content_frame.clone(), v.content.clone())
    });
    if let Some(w) = &window {
        w.set_background_color(frame);
        platform::apply_window_chrome(platform::handle_value(w.window_handle()), dark);
    }
    for panel in [right, content_frame, content].into_iter().flatten() {
        panel.set_background_color(frame);
    }
    overlays::on_chrome_colors_changed();
    tabs::on_chrome_colors_changed();
}

/// Tab currently in HTML5 page fullscreen.
pub fn page_fullscreen_tab() -> Option<Id> {
    PAGE_FULLSCREEN.get()
}

/// `Effect::SetPageFullscreen`: hide sidebar, topbar and content insets and fullscreen the window
/// for `Some(tab)`; restore exactly the previous chrome and window state for `None`.
pub fn set_page_fullscreen(tab: Option<Id>) {
    let Some(window) = main_window() else { return };
    let previous = PAGE_FULLSCREEN.get();
    if previous == tab {
        return;
    }
    if previous.is_none() {
        WINDOW_FULLSCREEN_BEFORE_PAGE.set(window.is_fullscreen() != 0);
    }
    PAGE_FULLSCREEN.set(tab);
    log_info!("page fullscreen: {previous:?} -> {tab:?}");
    if let Some((_, sidebar)) = place_sidebar(true) {
        sidebar.invalidate_layout();
    }
    let topbar = VIEWS.with(|v| v.borrow().topbar.clone());
    if let Some(topbar) = &topbar {
        topbar.set_visible(tab.is_none() as i32);
    }
    tabs::set_page_fullscreen_tab(tab);
    relayout_content_frame();
    match tab {
        Some(_) if window.is_fullscreen() == 0 => window.set_fullscreen(1),
        None if !WINDOW_FULLSCREEN_BEFORE_PAGE.get() && window.is_fullscreen() != 0 => window.set_fullscreen(0),
        _ => {}
    }
    window.layout();
    rounded::layout_masks();
    overlays::layout();
    schedule_draggable_regions();
    sidebar_hover::refresh();
}

/// `Effect::Window`.
pub fn window_action(action: WindowAction) {
    let Some(window) = main_window() else { return };
    match action {
        WindowAction::Minimize => window.minimize(),
        WindowAction::ToggleMaximize => {
            if window.is_maximized() != 0 {
                window.restore();
            } else {
                window.maximize();
            }
        }
        WindowAction::ToggleFullscreen => match PAGE_FULLSCREEN.get() {
            // Leaving window fullscreen ends page fullscreen too (core then sends
            // `SetPageFullscreen{None}`, which restores the pre-page window state).
            Some(tab) => tabs::exit_page_fullscreen(tab),
            None => window.set_fullscreen((window.is_fullscreen() == 0) as i32),
        },
        WindowAction::Close => controller::dispatch(Command::WindowCloseRequested),
    }
}

/// Keyboard focus target when no tab can take it: the empty-state view if shown, else the
/// sidebar, else the top bar.
pub fn focus_fallback() {
    if let Some(view) = focus_fallback_view() {
        view.request_focus();
    }
}

/// The view [`focus_fallback`] focuses.
pub fn focus_fallback_view() -> Option<BrowserView> {
    let (empty, sidebar, topbar) = VIEWS.with(|v| {
        let v = v.borrow();
        (v.empty.clone(), v.sidebar.clone(), v.topbar.clone())
    });
    // A parked sidebar may count as drawn while its host is hidden, and the floating sidebar can't
    // take keyboard focus anyway.
    let sidebar = sidebar.filter(|_| !SIDEBAR_PARKED.get());
    [empty, sidebar, topbar].into_iter().flatten().find(|v| v.is_drawn() != 0)
}

/// Brings the window to the front (second launch, notifications). Call from a posted task, not
/// inside a CEF callback (activation fires delegate callbacks synchronously).
pub fn activate() {
    let Some(window) = main_window() else { return };
    if window.is_minimized() != 0 {
        window.restore();
    }
    window.activate();
    window.bring_to_top();
}

fn report_state(window: &Window) {
    if CLOSING.get() {
        return;
    }
    let minimized = window.is_minimized() != 0;
    let maximized = window.is_maximized() != 0;
    let fullscreen = window.is_fullscreen() != 0;
    let bounds = (!minimized && !maximized && !fullscreen).then(|| {
        let b = window.bounds();
        sta_core::Rect { x: b.x, y: b.y, width: b.width, height: b.height }
    });
    let report = Report { maximized, fullscreen, focused: ACTIVE.get(), bounds };
    if LAST_REPORT.replace(Some(report)) == Some(report) {
        return;
    }
    controller::dispatch(Command::WindowStateChanged { maximized, fullscreen, focused: report.focused, bounds });
}

// ----------------------------------------------------------------------------------- drag regions

/// UI DragHandler input: regions of a docked surface in view coordinates.
pub fn on_draggable_regions_changed(browser_id: i32, regions: Vec<DraggableRegion>) {
    let Some(Role::Surface(surface @ (Surface::Sidebar | Surface::Topbar))) = browsers::role_of(browser_id) else {
        return;
    };
    REGIONS.with(|r| r.borrow_mut().insert(surface, regions));
    schedule_draggable_regions();
}

/// Coalesces region updates into one posted [`apply_draggable_regions`].
pub fn schedule_draggable_regions() {
    if !REGIONS_PENDING.replace(true) {
        task::post_ui(|| {
            REGIONS_PENDING.set(false);
            apply_draggable_regions();
        });
    }
}

/// Converts the docked surfaces' regions to window coordinates, punches holes for visible
/// overlays and applies them to the window.
pub fn apply_draggable_regions() {
    let (window, sidebar, topbar) = VIEWS.with(|v| {
        let v = v.borrow();
        (v.window.clone(), v.sidebar.clone(), v.topbar.clone())
    });
    let Some(window) = window else { return };
    let regions = REGIONS.with(|r| r.borrow().clone());
    let mut out: Vec<DraggableRegion> = Vec::new();
    // The floating sidebar is never a drag area (its overlay is a no-drag hole below).
    let sidebar = sidebar.filter(|_| !SIDEBAR_PARKED.get());
    for (surface, view) in [(Surface::Sidebar, sidebar), (Surface::Topbar, topbar)] {
        let (Some(view), Some(list)) = (view, regions.get(&surface)) else { continue };
        if view.is_drawn() == 0 {
            continue;
        }
        let Some(origin) = view_rect_in_window(&View::from(&view)) else { continue };
        for r in list {
            // Clip to the view, then offset by the view's origin in the window.
            let (x0, y0) = (r.bounds.x.max(0), r.bounds.y.max(0));
            let x1 = (r.bounds.x + r.bounds.width).min(origin.width);
            let y1 = (r.bounds.y + r.bounds.height).min(origin.height);
            if x1 <= x0 || y1 <= y0 {
                continue;
            }
            out.push(DraggableRegion {
                bounds: Rect { x: origin.x + x0, y: origin.y + y0, width: x1 - x0, height: y1 - y0 },
                draggable: r.draggable,
            });
        }
    }
    for bounds in overlays::visible_overlay_bounds() {
        out.push(DraggableRegion { bounds, draggable: 0 });
    }
    window.set_draggable_regions(Some(&out));
    APPLIED_REGIONS.with(|a| *a.borrow_mut() = out);
}

// ----------------------------------------------------------------------------------- shutdown

/// Browsers the window still waits for: sta's own and Chrome-created ones (foreign.rs).
fn open_browsers() -> usize {
    browsers::live_count() + browsers::extra_count()
}

fn can_close() -> i32 {
    if CLOSING.get() {
        return (open_browsers() == 0) as i32;
    }
    // No latch: every OS close asks again until the shutdown sequence has started. Core ignores
    // commands once it is shutting down, so in that state start the sequence directly.
    if controller::with_store(|s| s.is_shutting_down()).unwrap_or(true) {
        task::post_ui(|| {
            controller::save_now();
            begin_shutdown();
        });
    } else {
        controller::dispatch(Command::WindowCloseRequested);
    }
    0
}

/// The OS asked sta to quit rather than to close a window (macOS ⌘Q, Dock → Quit, logout): the
/// same answer as the window's own close button, so the session is saved and CEF shuts down.
#[cfg(target_os = "macos")]
pub fn request_close() {
    let _ = can_close();
}

/// `Effect::Quit`: force-close every browser; the window closes when the last one is gone.
pub fn begin_shutdown() {
    if CLOSING.replace(true) {
        return;
    }
    sidebar_hover::refresh(); // hides the floating sidebar and stops the poll
    // Downloads first: closing the last Chrome-created browser must never wait on (or ask about)
    // downloads that end with the process anyway.
    crate::downloads::cancel_all_in_progress();
    crate::devtools::close_all();
    // The popup card and the hidden extensions backend before the Chrome-created browsers: both own
    // browsers shutdown waits for (`browsers::extra_count`).
    crate::ext_popup::close();
    crate::ext_backend::close_all();
    crate::foreign::close_all();
    let browsers = browsers::live_browsers();
    log_info!("shutdown: closing {} browsers ({} Chrome-created)", browsers.len(), browsers::extra_count());
    // Debug builds: STA_DEBUG_SHUTDOWN_TIMEOUT_MS shortens the timeout (tests).
    #[cfg(debug_assertions)]
    let timeout = std::env::var("STA_DEBUG_SHUTDOWN_TIMEOUT_MS").ok().and_then(|v| v.parse().ok()).unwrap_or(SHUTDOWN_TIMEOUT_MS);
    #[cfg(not(debug_assertions))]
    let timeout = SHUTDOWN_TIMEOUT_MS;
    task::post_ui_delayed(timeout, || {
        if main_window().is_some() {
            // `cef::shutdown` with live browsers crashes or hangs: save and leave instead.
            log_warn!("shutdown timed out with {} live browsers; saving and exiting", open_browsers());
            controller::emergency_exit(0);
        }
    });
    for browser in browsers {
        if let Some(host) = browser.host() {
            if host.has_dev_tools() != 0 {
                host.close_dev_tools();
            }
            host.close_browser(1);
        }
    }
    if open_browsers() == 0 {
        task::post_ui(close_window);
    }
}

fn close_window() {
    if let Some(window) = main_window()
        && window.is_closed() == 0
    {
        window.close();
    }
}

/// Called after every `on_before_close` (browsers.rs).
pub fn on_browser_closed() {
    if CLOSING.get() && open_browsers() == 0 {
        log_info!("shutdown: all browsers closed");
        task::post_ui(close_window);
    }
}

/// Detaches and releases a docked UI surface during shutdown (posted from `do_close`).
pub fn release_surface(surface: Surface) {
    let (parent, view): (Option<Panel>, Option<BrowserView>) = VIEWS.with(|v| {
        let mut v = v.borrow_mut();
        match surface {
            Surface::Sidebar => (v.window.as_ref().map(Panel::from), v.sidebar.take()),
            Surface::Topbar => (v.right.clone(), v.topbar.take()),
            Surface::Empty => (v.content.clone(), v.empty.take()),
            _ => (None, None),
        }
    });
    match (&parent, &view) {
        // A parked sidebar lives in the floating sidebar host, not in the window.
        (_, Some(view)) if surface == Surface::Sidebar && SIDEBAR_PARKED.get() => overlays::release_sidebar(view),
        (Some(parent), Some(view)) => parent.remove_child_view(Some(&mut View::from(view))),
        _ => {}
    }
    drop(view); // last reference → browser destroyed → on_before_close
}

/// A docked surface's browser is gone.
pub fn on_surface_closed(surface: Surface, browser_id: i32) {
    REGIONS.with(|r| r.borrow_mut().remove(&surface));
    if !CLOSING.get() {
        log_warn!("{surface:?} browser {browser_id} closed unexpectedly");
    }
}

fn teardown() {
    log_info!("window destroyed");
    let views = VIEWS.with(|v| std::mem::take(&mut *v.borrow_mut()));
    drop(views);
    REGIONS.with(|r| r.borrow_mut().clear());
    APPLIED_REGIONS.with(|a| a.borrow_mut().clear());
    overlays::clear();
    rounded::clear();
    crate::devtools::clear();
    tabs::clear();
    platform::unwatch_setting_change();
    quit_message_loop();
}

/// Window state for `debug.info`.
#[cfg_attr(not(debug_assertions), allow(dead_code))] // used by debug.rs only
pub fn debug_snapshot() -> serde_json::Value {
    let window = main_window();
    let regions: Vec<serde_json::Value> = APPLIED_REGIONS.with(|a| {
        a.borrow()
            .iter()
            .map(|r| serde_json::json!([r.bounds.x, r.bounds.y, r.bounds.width, r.bounds.height, r.draggable]))
            .collect()
    });
    let bounds = window.as_ref().map(|w| {
        let b = w.bounds();
        serde_json::json!([b.x, b.y, b.width, b.height])
    });
    let work_area = window.as_ref().and_then(|w| w.display()).map(|d| {
        let a = d.work_area();
        serde_json::json!([a.x, a.y, a.width, a.height])
    });
    serde_json::json!({
        "exists": window.is_some(),
        "bounds": bounds,
        "workArea": work_area,
        "maximized": window.as_ref().map(|w| w.is_maximized() != 0),
        "minimized": window.as_ref().map(|w| w.is_minimized() != 0),
        "fullscreen": window.as_ref().map(|w| w.is_fullscreen() != 0),
        "active": ACTIVE.get(),
        "closing": CLOSING.get(),
        "sidebar": {
            "visible": SIDEBAR_VISIBLE.get(),
            "shown": sidebar_shown(),
            "width": SIDEBAR_WIDTH.get(),
            "floating": SIDEBAR_FLOATING.get(),
            "parked": SIDEBAR_PARKED.get(),
            "parkPending": PARK_PENDING.get(),
            "drawn": VIEWS.with(|v| v.borrow().sidebar.clone()).map(|v| v.is_drawn() != 0),
        },
        "topbarVisible": VIEWS.with(|v| v.borrow().topbar.clone()).map(|t| t.is_visible() != 0),
        "pageFullscreen": PAGE_FULLSCREEN.get(),
        "frame": format!("#{:08x}", FRAME.get()),
        "dark": DARK.get(),
        "contentRect": content_rect().map(|r| serde_json::json!([r.x, r.y, r.width, r.height])),
        "emptyVisible": empty_view().map(|v| v.is_visible() != 0),
        "draggableRegions": regions,
    })
}
