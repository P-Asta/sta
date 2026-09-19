//! Tab browsers and the content area [owner: tabs] (ARCHITECTURE §4 "Content layout", §4.1).
//!
//! Responsibility:
//! - registry: tab id → { wrapper Panel, BrowserView, browser id, internal, in Peek };
//!   browser id → tab; browsers whose close is in progress;
//! - `CreateBrowser` / `ReplaceBrowser` / `DestroyBrowser` with the Alloy close flow:
//!   `close_browser(1)` → `do_close` returns 1 and posts [`detach`] → views removed from whatever
//!   parent they have (wrapper or Peek) and every reference dropped → CEF destroys the browser →
//!   `on_before_close` → `TabBrowserClosed` (reported at most once per tab browser; never for the
//!   old browser of a `ReplaceBrowser`; a failed replace closes the old browser and reports the tab);
//! - popup adoption (Strategy B, handlers.md §5.1): `client.rs` `on_before_popup` →
//!   [`prepare_popup`] (id via `controller::alloc_id`, presentation + renderer `extra_info`);
//!   `TabDelegate::delegate_for_popup_browser_view` gives the popup its own `TabDelegate`;
//!   `on_popup_browser_view_created` adopts the view into a hidden wrapper and enqueues
//!   `PopupAdopted` (DevTools popups keep CEF's own window);
//! - `ShowContent` (Empty / Single / Split with wrapper flex + accent border), `FocusBrowser`, page
//!   fullscreen presentation ([`set_page_fullscreen_tab`]);
//! - page actions (load, history, reload, stop, zoom presets + `TabZoomChanged` reporting, mute,
//!   print, find, exit fullscreen, start download). DevTools live in `devtools.rs`: a docked
//!   frontend replaces the wrapper's child with a stack panel holding the frontend and, on top of
//!   it, this tab's page view, so every function here that (re)parents a page view asks
//!   [`page_parent`] where it belongs;
//! - boosts for renderers, host-matched: the creation URL's boosts in `extra_info`, [`sync_boosts`]
//!   before same-site navigations/reloads, [`on_boosts_check`] answers for the committed URL;
//! - the Peek view hand-off with overlays.rs.
//!
//! Public API:
//! - `pub fn create_browser(tab: Id, url: &str, internal: bool, muted: bool)`
//! - `pub fn replace_browser(tab: Id, url: &str, internal: bool)`, `pub fn destroy_browser(tab: Id)`
//! - `pub fn show_content(layout: &ContentLayout)`, `pub fn focus_browser(tab: Id)`,
//!   `pub fn view_for_tab(tab: Id) -> Option<BrowserView>`
//! - `pub fn set_page_fullscreen_tab(tab: Option<Id>)` — called by `window::set_page_fullscreen`
//! - `pub fn load_url(tab, url)`, `go_back(tab)`, `go_forward(tab)`, `reload(tab, ignore_cache)`,
//!   `stop_load(tab)`, `zoom(tab, direction)`, `set_audio_muted(tab, muted)`, `toggle_dev_tools(tab)`,
//!   `show_dev_tools(browser, at)`, `print(tab)`, `find(tab, text, forward, match_case, find_next)`, `stop_finding(tab)`,
//!   `exit_page_fullscreen(tab)`, `start_download(tab, url)`
//! - `pub fn report_zoom_later(tab: Id)` (LoadHandler::on_load_end)
//! - `pub fn prepare_popup(opener_browser: i32, popup_id: i32, url: &str, disposition: WindowOpenDisposition, features_popup: bool) -> Option<DictionaryValue>`,
//!   `pub fn skip_popup(opener_browser: i32, popup_id: i32)` (PiP), `pub fn popup_aborted(opener_browser: i32, popup_id: i32)`
//! - `pub fn boosts_payload_for(url: &str) -> (String, String)`, `pub fn sync_boosts(browser: &Browser, target_url: &str)`,
//!   `pub fn on_boosts_check(browser: &Browser, frame: &Frame, version: &str)`
//! - `pub fn tab_for_browser(browser_id: i32) -> Option<Id>`, `pub fn browser_for_tab(tab: Id) -> Option<Browser>`,
//!   `pub fn live_tab_urls() -> Vec<String>`
//! - `pub fn is_tab_visible(tab: Id) -> bool`, `pub fn is_closing_browser(browser_id: i32) -> bool`
//! - `pub fn tab_rect_in_window(tab: Id) -> Option<Rect>` — the **page** rect (pane-anchored
//!   overlays), which is the docked DevTools' page view when a tab has DevTools open;
//!   `pub fn wrapper_rect_in_window(tab: Id) -> Option<Rect>` — the whole card (rounded masks);
//!   `pub fn visible_pane_rects() -> Vec<(Rect, u32)>`
//! - `pub fn wrapper_of(tab: Id) -> Option<Panel>`, `pub fn is_in_peek(tab: Id) -> bool` (devtools.rs)
//! - `pub fn refresh_wrapper_color(tab: Id)` (automation/frame.rs: the agent frame changed)
//! - `pub fn take_view_for_peek(tab: Id) -> Option<BrowserView>`, `pub fn return_view_from_peek(tab: Id, view: BrowserView)`
//! - `pub fn on_do_close(browser_id: i32, tab: Id)`, `pub fn on_before_close(browser_id: i32, tab: Id)` (browsers.rs)
//! - `pub fn on_chrome_colors_changed()`, `pub fn clear()`, `pub fn debug_snapshot() -> serde_json::Value`

use crate::browsers::{self, Role, UiExtra};
use crate::renderer::{self, BoostData};
use crate::{client, controller, external, overlays, rounded, task, window};
use sta_core::{Command, ContentLayout, Id, Orientation, ZoomDirection};
use cef::*;
use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};

/// Chrome's preset zoom levels (percent).
const ZOOM_PRESETS: [f64; 17] =
    [25.0, 33.0, 50.0, 67.0, 75.0, 80.0, 90.0, 100.0, 110.0, 125.0, 150.0, 175.0, 200.0, 250.0, 300.0, 400.0, 500.0];
/// Wrapper inset: the accent border of the focused split pane.
const WRAPPER_BORDER: i32 = 2;
/// Gap between split panes.
const SPLIT_SPACING: i32 = 6;

struct TabEntry {
    wrapper: Panel,
    view: Option<BrowserView>,
    browser_id: Option<i32>,
    internal: bool,
    /// The view currently sits in the Peek overlay.
    in_peek: bool,
}

/// A browser whose close was requested (by us or the page) and is not yet gone.
struct Closing {
    tab: Id,
    /// `ReplaceBrowser`: don't report `TabBrowserClosed`, keep the tab entry.
    silent: bool,
    /// View kept alive until the posted detach releases it (replaced browsers only).
    view: Option<BrowserView>,
    detached: bool,
}

/// A popup allowed in `on_before_popup`, waiting for its BrowserView.
struct PendingPopup {
    popup_id: i32,
    /// `None`: not adopted (e.g. Picture-in-Picture keeps CEF's own window).
    tab: Option<Id>,
    url: String,
    popup: bool,
    foreground: bool,
    boosts_version: Option<String>,
}

#[derive(Clone, Copy)]
struct Fullscreen {
    tab: Id,
    /// The tab's view was taken out of the Peek overlay for the fullscreen presentation.
    from_peek: bool,
}

thread_local! {
    static TABS: RefCell<BTreeMap<Id, TabEntry>> = const { RefCell::new(BTreeMap::new()) };
    static BY_BROWSER: RefCell<HashMap<i32, Id>> = RefCell::new(HashMap::new());
    static CLOSING: RefCell<HashMap<i32, Closing>> = RefCell::new(HashMap::new());
    static SHOWN: RefCell<ContentLayout> = const { RefCell::new(ContentLayout::Empty) };
    static WRAPPER_COLORS: RefCell<HashMap<Id, u32>> = RefCell::new(HashMap::new());
    /// Opener browser id → popups allowed in `on_before_popup`, oldest first.
    static PENDING_POPUPS: RefCell<HashMap<i32, VecDeque<PendingPopup>>> = RefCell::new(HashMap::new());
    /// (tab, browser id or -1) whose `TabBrowserClosed` was already dispatched.
    static CLOSED_REPORTED: RefCell<HashSet<(Id, i32)>> = RefCell::new(HashSet::new());
    static PAGE_FULLSCREEN: RefCell<Option<Fullscreen>> = const { RefCell::new(None) };
    /// Last zoom level reported per tab.
    static LAST_ZOOM: RefCell<HashMap<Id, f64>> = RefCell::new(HashMap::new());
    /// Browser id → boosts version last shipped to its renderer.
    static BOOSTS_SENT: RefCell<HashMap<i32, String>> = RefCell::new(HashMap::new());
}

// ----------------------------------------------------------------------------------- delegates

wrap_browser_view_delegate! {
    pub struct TabDelegate {
        tab: Id,
    }

    impl ViewDelegate {
        fn on_theme_changed(&self, view: Option<&mut View>) {
            // CEF resets the view background to the native theme's (#1f1f1f) here.
            if let Some(view) = view {
                view.set_background_color(view_background(self.tab));
            }
        }
    }

    impl BrowserViewDelegate {
        fn on_browser_created(&self, _browser_view: Option<&mut BrowserView>, browser: Option<&mut Browser>) {
            if let Some(browser) = browser {
                on_browser_created(self.tab, browser.identifier());
            }
        }

        fn delegate_for_popup_browser_view(
            &self,
            browser_view: Option<&mut BrowserView>,
            _settings: Option<&BrowserSettings>,
            _client: Option<&mut Client>,
            is_devtools: i32,
        ) -> Option<BrowserViewDelegate> {
            // DevTools is always Chrome style: no Alloy delegate for it.
            if is_devtools != 0 {
                return None;
            }
            let opener = browser_view.and_then(|v| v.browser()).map(|b| b.identifier())?;
            let tab = next_popup_tab(opener)?;
            Some(TabDelegate::new(tab))
        }

        fn on_popup_browser_view_created(
            &self,
            browser_view: Option<&mut BrowserView>,
            popup_browser_view: Option<&mut BrowserView>,
            is_devtools: i32,
        ) -> i32 {
            // DevTools: 0 = CEF opens its own (Chrome-style) window.
            if is_devtools != 0 {
                return 0;
            }
            let opener = browser_view.and_then(|v| v.browser()).map(|b| b.identifier());
            let Some(popup_view) = popup_browser_view else { return 0 };
            adopt_popup(opener, popup_view)
        }

        fn browser_runtime_style(&self) -> RuntimeStyle {
            RuntimeStyle::ALLOY
        }
    }
}

wrap_panel_delegate! {
    struct WrapperDelegate {
        tab: Id,
    }

    impl ViewDelegate {
        fn preferred_size(&self, _view: Option<&mut View>) -> Size {
            // Tiny preferred size so split fractions (flex) decide the pane sizes.
            Size { width: 1, height: 1 }
        }

        fn on_theme_changed(&self, view: Option<&mut View>) {
            let color = WRAPPER_COLORS.with(|c| c.borrow().get(&self.tab).copied()).unwrap_or_else(window::frame_color);
            let color = shown_wrapper_color(self.tab, color);
            if let Some(view) = view {
                view.set_background_color(color);
            }
        }
    }

    impl PanelDelegate {}
}

fn on_browser_created(tab: Id, browser_id: i32) {
    browsers::set_role(browser_id, Role::Tab(tab));
    BY_BROWSER.with(|b| b.borrow_mut().insert(browser_id, tab));
    TABS.with(|t| {
        if let Some(entry) = t.borrow_mut().get_mut(&tab) {
            entry.browser_id = Some(browser_id);
        }
    });
}

// ----------------------------------------------------------------------------------- lookups

pub fn tab_for_browser(browser_id: i32) -> Option<Id> {
    BY_BROWSER.with(|b| b.borrow().get(&browser_id).copied())
}

/// Main-frame URLs of every live tab browser (web and internal, Peek and popups included; closing
/// browsers excluded).
pub fn live_tab_urls() -> Vec<String> {
    let ids: Vec<i32> = BY_BROWSER.with(|b| b.borrow().keys().copied().collect());
    ids.into_iter()
        .filter(|id| !is_closing_browser(*id))
        .filter_map(browsers::browser)
        .filter_map(|b| b.main_frame())
        .map(|f| CefString::from(&f.url()).to_string())
        .collect()
}

pub fn browser_for_tab(tab: Id) -> Option<Browser> {
    let id = TABS.with(|t| t.borrow().get(&tab).and_then(|e| e.browser_id))?;
    browsers::browser(id)
}

fn host_for_tab(tab: Id) -> Option<BrowserHost> {
    browser_for_tab(tab)?.host()
}

/// Visible in the content area or in a visible Peek.
pub fn is_tab_visible(tab: Id) -> bool {
    let (shown, in_peek) = (SHOWN.with(|s| s.borrow().tabs().contains(&tab)), TABS.with(|t| t.borrow().get(&tab).is_some_and(|e| e.in_peek)));
    if in_peek {
        return overlays::is_visible(overlays::Overlay::Peek);
    }
    shown || PAGE_FULLSCREEN.with(|f| f.borrow().is_some_and(|f| f.tab == tab))
}

pub fn is_closing_browser(browser_id: i32) -> bool {
    CLOSING.with(|c| c.borrow().contains_key(&browser_id))
}

/// Bounds of a tab's **page** in window coordinates: where overlays anchored to a pane (find bar,
/// permission prompt) go. With DevTools docked that is the page view inside the stack, not the whole
/// card — the find bar belongs over the page, like in Chrome.
pub fn tab_rect_in_window(tab: Id) -> Option<Rect> {
    crate::devtools::page_rect_in_window(tab).or_else(|| wrapper_rect_in_window(tab))
}

/// Wrapper bounds of a tab in window coordinates: the whole content card, DevTools included (the
/// rounded corner masks round the card).
pub fn wrapper_rect_in_window(tab: Id) -> Option<Rect> {
    let wrapper = TABS.with(|t| t.borrow().get(&tab).map(|e| e.wrapper.clone()))?;
    if wrapper.is_drawn() == 0 {
        return None;
    }
    window::view_rect_in_window(&View::from(&wrapper))
}

/// The panes on screen whose corners rounded.rs masks, in layout order: `(wrapper rect in window
/// coordinates, ring color)`. The ring is the wrapper color as shown: the frame, the focused split
/// pane's accent, or the agent frame (automation/frame.rs). The empty-state view counts as one
/// unfocused pane (its page draws the same rounded card, so the masks stay up between Empty and
/// Single). Empty in page fullscreen (square presentation); a tab in Peek has its own card.
pub fn visible_pane_rects() -> Vec<(Rect, u32)> {
    if PAGE_FULLSCREEN.with(|f| f.borrow().is_some()) {
        return Vec::new();
    }
    let layout = SHOWN.with(|s| s.borrow().clone());
    let focused = match &layout {
        ContentLayout::Split { panes, focused, .. } => panes.get(*focused).map(|p| p.tab),
        _ => None,
    };
    let colors = rounded::colors();
    if matches!(layout, ContentLayout::Empty) {
        let empty = window::empty_view().filter(|v| v.is_drawn() != 0);
        return empty.and_then(|v| window::view_rect_in_window(&View::from(&v))).filter(|r| r.width > 0 && r.height > 0).map(|r| vec![(r, colors.frame)]).unwrap_or_default();
    }
    layout
        .tabs()
        .into_iter()
        .filter(|t| TABS.with(|m| m.borrow().get(t).is_some_and(|e| !e.in_peek)))
        .filter_map(|t| {
            let base = if Some(t) == focused { colors.accent } else { colors.frame };
            wrapper_rect_in_window(t).map(|r| (r, shown_wrapper_color(t, base)))
        })
        .filter(|(r, _)| r.width > 0 && r.height > 0)
        .collect()
}

// ----------------------------------------------------------------------------------- boosts

/// The enabled boosts whose host matches `url`, as `(json, version)` for a renderer. A renderer
/// only ever receives the boosts of the host it shows (or is about to show), never the whole list.
pub fn boosts_payload_for(url: &str) -> (String, String) {
    let list: Vec<BoostData> = controller::with_store(|s| {
        s.boosts_for_url(url)
            .into_iter()
            .filter(|b| {
                let empty = b.css.trim().is_empty() && b.js.trim().is_empty();
                !b.host.trim().is_empty() && !empty
            })
            .map(|b| BoostData { host: b.host, css: b.css, js: b.js })
            .collect()
    })
    .unwrap_or_default();
    let json = serde_json::to_string(&list).unwrap_or_else(|_| "[]".into());
    let version = renderer::boosts_version(&json);
    (json, version)
}

/// `extra_info` for a web-tab browser created for `url` (and its version of the boost list).
///
/// `extra_info` is frozen per browser and handed to every renderer process the browser later uses,
/// so a tab that navigates cross-site hands the creation host's boosts to the new site's process
/// once; the renderer applies nothing that does not match its URL and asks for its own list
/// (`sta.boosts.check`).
fn web_extra_info(tab: Id, url: &str) -> Option<(DictionaryValue, String)> {
    let info = dictionary_value_create()?;
    let (json, version) = boosts_payload_for(url);
    info.set_double(Some(&CefString::from(renderer::EXTRA_TAB)), tab as f64);
    info.set_string(Some(&CefString::from(renderer::EXTRA_BOOSTS)), Some(&CefString::from(json.as_str())));
    info.set_string(Some(&CefString::from(renderer::EXTRA_BOOSTS_VERSION)), Some(&CefString::from(version.as_str())));
    Some((info, version))
}

/// Both URLs belong to the same site (registrable domain, else host), i.e. the navigation between
/// them normally stays in the same renderer process.
fn same_site(a: &str, b: &str) -> bool {
    use sta_core::urls::site_key;
    matches!((site_key(a), site_key(b)), (Some(x), Some(y)) if x == y)
}

fn send_boosts(browser_id: i32, frame: &Frame, json: &str, version: &str, apply_now: bool) {
    let Some(mut msg) = process_message_create(Some(&CefString::from(renderer::MSG_BOOSTS))) else { return };
    if let Some(args) = msg.argument_list() {
        args.set_string(0, Some(&CefString::from(json)));
        args.set_string(1, Some(&CefString::from(version)));
        args.set_bool(2, apply_now as i32);
    }
    frame.send_process_message(ProcessId::RENDERER, Some(&mut msg));
    BOOSTS_SENT.with(|m| m.borrow_mut().insert(browser_id, version.to_string()));
}

/// Before a reload or main-frame navigation to `target_url`: pushes the boosts of the target host to
/// the web tab's renderer (unless it already has that list), so the next document sees them from
/// its first script. Only when the target is same-site with the current document: a cross-site
/// navigation gets a new renderer process, which must not learn another site's boosts; it asks
/// for its own list (`on_boosts_check`).
pub fn sync_boosts(browser: &Browser, target_url: &str) {
    let id = browser.identifier();
    if browsers::is_ui_browser(id) {
        return;
    }
    let Some(frame) = browser.main_frame() else { return };
    let current = CefString::from(&frame.url()).to_string();
    if !same_site(&current, target_url) {
        return;
    }
    let (json, version) = boosts_payload_for(target_url);
    if BOOSTS_SENT.with(|m| m.borrow().get(&id) == Some(&version)) {
        return;
    }
    // The document about to be replaced keeps its boosts; the next one gets the new list.
    send_boosts(id, &frame, &json, &version, false);
}

/// `sta.boosts.check` from a renderer that applied `version` to a new main-frame document
/// (its `extra_info` may be stale, e.g. in a new process). Answered with the boosts of the frame's
/// committed URL (browser-side, so a renderer can't ask for another host's list).
pub fn on_boosts_check(browser: &Browser, frame: &Frame, version: &str) {
    let url = CefString::from(&frame.url()).to_string();
    let (json, current) = boosts_payload_for(&url);
    if current != version {
        log_debug!("boosts: renderer of browser {} applied {version}; sending {current}", browser.identifier());
        send_boosts(browser.identifier(), frame, &json, &current, true);
    } else {
        BOOSTS_SENT.with(|m| m.borrow_mut().insert(browser.identifier(), current));
    }
}

// ----------------------------------------------------------------------------------- lifecycle

fn create_view(tab: Id, url: &str, internal: bool) -> Option<(BrowserView, Option<String>)> {
    let mut delegate = TabDelegate::new(tab);
    if internal {
        return browsers::create_ui_view(url, UiExtra { tab: Some(tab) }, &mut delegate).map(|v| (v, None));
    }
    let mut client = client::tab_client();
    let (mut info, version) = web_extra_info(tab, url)?;
    let settings = BrowserSettings { background_color: 0xFFFF_FFFF, ..Default::default() };
    browser_view_create(
        Some(&mut client),
        Some(&CefString::from(url)),
        Some(&settings),
        Some(&mut info),
        None,
        Some(&mut delegate),
    )
    .map(|v| (v, Some(version)))
}

/// Sets a wrapper's base color (frame, or the focused split pane's accent) and paints it, as the
/// agent color while an agent acts on the tab ([`shown_wrapper_color`]); the tab's view background
/// follows.
fn set_wrapper_color(tab: Id, wrapper: &Panel, color: u32) {
    WRAPPER_COLORS.with(|c| c.borrow_mut().insert(tab, color));
    wrapper.set_background_color(shown_wrapper_color(tab, color));
    let view = TABS.with(|t| t.try_borrow().ok().and_then(|t| t.get(&tab).and_then(|e| e.view.clone())));
    if let Some(view) = view {
        view.set_background_color(view_background(tab));
    }
}

/// The color a wrapper shows for its base color: the agent frame (automation/frame.rs) while an
/// agent acts on the tab, except in page fullscreen (no inset, so no frame).
fn shown_wrapper_color(tab: Id, base: u32) -> u32 {
    let fullscreen = PAGE_FULLSCREEN.with(|f| f.try_borrow().ok().is_some_and(|f| f.is_some_and(|f| f.tab == tab)));
    if fullscreen { base } else { crate::automation::frame::wrapper_color(tab, base) }
}

/// Re-applies a wrapper's color after the agent frame of the tab changed (automation/frame.rs):
/// the wrapper, its view background and, when the tab is on screen, the ring of its corner masks.
pub fn refresh_wrapper_color(tab: Id) {
    let wrapper = TABS.with(|t| t.borrow().get(&tab).map(|e| e.wrapper.clone()));
    let color = WRAPPER_COLORS.with(|c| c.borrow().get(&tab).copied());
    if let (Some(wrapper), Some(color)) = (wrapper, color) {
        set_wrapper_color(tab, &wrapper, color);
        if SHOWN.with(|s| s.borrow().tabs().contains(&tab)) {
            rounded::layout_masks();
        }
    }
}

/// The background of a tab's BrowserView: its wrapper color as shown (frame, the focused split
/// pane's accent ring, or the agent frame), the card surface while it sits in Peek. At fractional
/// scales the page layer can leave a device-pixel column or row of the view uncovered along its
/// left or top edge; with the theme's default background that showed as a dark `#1f1f1f` line
/// between the rounded corners. It is also the web view's resize background.
fn view_background(tab: Id) -> u32 {
    let in_peek = TABS.with(|t| t.try_borrow().ok().is_some_and(|t| t.get(&tab).is_some_and(|e| e.in_peek)));
    if in_peek {
        return rounded::colors().surface;
    }
    let base = WRAPPER_COLORS.with(|c| c.try_borrow().ok().and_then(|c| c.get(&tab).copied())).unwrap_or_else(window::frame_color);
    shown_wrapper_color(tab, base)
}

fn set_wrapper_insets(wrapper: &Panel, inset: i32) {
    wrapper.set_to_box_layout(Some(&BoxLayoutSettings {
        horizontal: 1,
        inside_border_insets: Insets { top: inset, left: inset, bottom: inset, right: inset },
        cross_axis_alignment: AxisAlignment::STRETCH,
        default_flex: 1,
        ..Default::default()
    }));
    // A new layout manager does not re-lay out children whose parent bounds stay the same.
    wrapper.invalidate_layout();
    wrapper.layout();
}

/// A hidden wrapper panel for `tab`, already added to the content panel.
fn new_wrapper(tab: Id) -> Option<Panel> {
    let content = window::content_panel()?;
    let mut delegate = WrapperDelegate::new(tab);
    let wrapper = panel_create(Some(&mut delegate))?;
    set_wrapper_insets(&wrapper, WRAPPER_BORDER);
    set_wrapper_color(tab, &wrapper, window::frame_color());
    wrapper.set_visible(0);
    content.add_child_view(Some(&mut View::from(&wrapper)));
    Some(wrapper)
}

/// Where a tab's page view belongs: the docked DevTools stack if there is one, else the wrapper.
fn page_parent(tab: Id) -> Option<Panel> {
    crate::devtools::page_parent(tab).or_else(|| TABS.with(|t| t.borrow().get(&tab).map(|e| e.wrapper.clone())))
}

/// The tab's wrapper panel (devtools.rs builds its stack inside it).
pub fn wrapper_of(tab: Id) -> Option<Panel> {
    TABS.with(|t| t.borrow().get(&tab).map(|e| e.wrapper.clone()))
}

/// The tab's page view currently sits in the Peek overlay.
pub fn is_in_peek(tab: Id) -> bool {
    TABS.with(|t| t.borrow().get(&tab).is_some_and(|e| e.in_peek))
}

/// Removes a view from whatever panel currently parents it (tab wrapper, DevTools stack or the
/// Peek host).
fn detach_from_parent(view: &BrowserView) {
    let parent = View::from(view).parent_view().and_then(|p| p.as_panel());
    if let Some(parent) = parent {
        parent.remove_child_view(Some(&mut View::from(view)));
    }
}

/// Dispatches `TabBrowserClosed` once per (tab, browser).
fn report_closed(tab: Id, browser_id: Option<i32>) {
    let first = CLOSED_REPORTED.with(|c| c.borrow_mut().insert((tab, browser_id.unwrap_or(-1))));
    if first {
        controller::dispatch(Command::TabBrowserClosed { tab });
    } else {
        log_warn!("duplicate TabBrowserClosed for tab {tab} (browser {browser_id:?}) suppressed");
    }
}

/// A new browser generation for `tab` starts: forget its old close reports and zoom.
fn begin_generation(tab: Id) {
    CLOSED_REPORTED.with(|c| c.borrow_mut().retain(|(t, _)| *t != tab));
    LAST_ZOOM.with(|z| z.borrow_mut().remove(&tab));
}

/// `Effect::CreateBrowser`.
pub fn create_browser(tab: Id, url: &str, internal: bool, muted: bool) {
    if TABS.with(|t| t.borrow().contains_key(&tab)) {
        log_warn!("CreateBrowser for tab {tab} which already has a browser");
        return;
    }
    // A tab whose URL is an external protocol (core opens typed ones with `OpenExternal` and never
    // creates a tab for them, but a pinned URL edited to `mailto:…` or an older profile can still
    // hold one) could never load it: hand the URL to the OS (a user action; not when restoring the
    // session) and let the tab show about:blank, which also replaces the URL in core.
    let external_url = !internal && external::is_external(url);
    if external_url {
        if controller::is_starting_up() {
            log_warn!("session restore: not launching external protocol of tab {tab}: {url}");
        } else {
            external::open(url, true, "new tab");
        }
    }
    let url = if external_url { "about:blank" } else { url };
    begin_generation(tab);
    let Some(wrapper) = new_wrapper(tab) else {
        report_closed(tab, None);
        return;
    };
    TABS.with(|t| {
        t.borrow_mut().insert(tab, TabEntry { wrapper: wrapper.clone(), view: None, browser_id: None, internal, in_peek: false })
    });

    let Some((view, version)) = create_view(tab, url, internal) else {
        log_error!("browser_view_create failed for tab {tab}");
        remove_entry(tab);
        report_closed(tab, None);
        return;
    };
    TABS.with(|t| {
        if let Some(e) = t.borrow_mut().get_mut(&tab) {
            e.view = Some(view.clone());
        }
    });
    // A brand-new tab has no dock yet, so this is its wrapper; the browser is created
    // synchronously while the view joins the Window hierarchy.
    wrapper.add_child_view(Some(&mut View::from(&view)));
    let Some(browser) = view.browser() else {
        log_error!("tab {tab}: browser was not created");
        wrapper.remove_child_view(Some(&mut View::from(&view)));
        drop(view);
        remove_entry(tab);
        report_closed(tab, None);
        return;
    };
    if let Some(version) = version {
        BOOSTS_SENT.with(|m| m.borrow_mut().insert(browser.identifier(), version));
    }
    controller::dispatch(Command::TabBrowserCreated { tab });
    if muted && let Some(host) = browser.host() {
        host.set_audio_muted(1);
    }
}

/// Removes a tab entry whose browser never came to life (no close flow needed).
fn remove_entry(tab: Id) {
    let entry = TABS.with(|t| t.borrow_mut().remove(&tab));
    WRAPPER_COLORS.with(|c| c.borrow_mut().remove(&tab));
    LAST_ZOOM.with(|z| z.borrow_mut().remove(&tab));
    end_fullscreen_for_removed(tab);
    let Some(entry) = entry else { return };
    if let Some(view) = &entry.view {
        detach_from_parent(view);
    }
    if let Some(content) = window::content_panel() {
        content.remove_child_view(Some(&mut View::from(&entry.wrapper)));
    }
    drop(entry);
}

/// `Effect::ReplaceBrowser`: new view in the same wrapper; the old one closes silently. If the
/// new browser cannot be created, the old one is closed normally and the tab reported closed.
pub fn replace_browser(tab: Id, url: &str, internal: bool) {
    // DevTools belong to the browser being replaced: core emits `CloseDevTools` first, and this is
    // the backstop for any other path into a replace.
    crate::devtools::on_inspected_gone(tab);
    // The new view lives in the wrapper: bring a Peek-parented view home first.
    if TABS.with(|t| t.borrow().get(&tab).is_some_and(|e| e.in_peek))
        && let Some(view) = overlays::take_back_peek_view(tab)
    {
        return_view_from_peek(tab, view);
    }
    let Some((wrapper, old_view, old_id, old_internal)) = TABS.with(|t| {
        let mut tabs = t.borrow_mut();
        let e = tabs.get_mut(&tab)?;
        let old_internal = e.internal;
        e.internal = internal;
        Some((e.wrapper.clone(), e.view.take(), e.browser_id.take(), old_internal))
    }) else {
        log_warn!("ReplaceBrowser for unknown tab {tab}");
        return;
    };
    let old_alive = old_id.and_then(browsers::browser).is_some();
    if let (Some(old_id), Some(old_view)) = (old_id, &old_view) {
        BY_BROWSER.with(|b| b.borrow_mut().remove(&old_id));
        if old_alive {
            CLOSING.with(|c| c.borrow_mut().insert(old_id, Closing { tab, silent: true, view: Some(old_view.clone()), detached: false }));
        }
        old_view.set_visible(0);
    }

    let created = create_view(tab, url, internal).and_then(|(view, version)| {
        TABS.with(|t| {
            if let Some(e) = t.borrow_mut().get_mut(&tab) {
                e.view = Some(view.clone());
            }
        });
        wrapper.add_child_view(Some(&mut View::from(&view)));
        match view.browser() {
            Some(browser) => Some((browser, version)),
            None => {
                wrapper.remove_child_view(Some(&mut View::from(&view)));
                TABS.with(|t| {
                    if let Some(e) = t.borrow_mut().get_mut(&tab) {
                        e.view = None;
                    }
                });
                None
            }
        }
    });

    let Some((browser, version)) = created else {
        log_error!("ReplaceBrowser: could not create the new browser for tab {tab}; closing the old one");
        // Roll back to the old browser and close it through the normal (reported) path.
        if let Some(old_id) = old_id {
            CLOSING.with(|c| c.borrow_mut().remove(&old_id));
            BY_BROWSER.with(|b| b.borrow_mut().insert(old_id, tab));
        }
        if let Some(v) = &old_view {
            v.set_visible(1);
        }
        TABS.with(|t| {
            if let Some(e) = t.borrow_mut().get_mut(&tab) {
                e.view = old_view.clone();
                e.browser_id = old_id.filter(|_| old_alive);
                e.internal = old_internal;
            }
        });
        drop(old_view);
        destroy_browser(tab);
        return;
    };

    // The old browser must never be reported: mark (tab, old) as already reported.
    begin_generation(tab);
    if let Some(old_id) = old_id {
        CLOSED_REPORTED.with(|c| c.borrow_mut().insert((tab, old_id)));
    }
    if let Some(version) = version {
        BOOSTS_SENT.with(|m| m.borrow_mut().insert(browser.identifier(), version));
    }
    controller::dispatch(Command::TabBrowserCreated { tab });
    match (old_alive, old_id.and_then(browsers::browser).and_then(|b| b.host())) {
        (true, Some(host)) => {
            drop(old_view);
            host.close_browser(1); // do_close → detach (silent)
        }
        _ => {
            // Nothing to close: release the dead view now.
            if let Some(v) = &old_view {
                detach_from_parent(v);
            }
            if let Some(old_id) = old_id {
                CLOSING.with(|c| c.borrow_mut().remove(&old_id));
            }
            drop(old_view);
        }
    }
}

/// `Effect::DestroyBrowser`: force close; `TabBrowserClosed` follows from `on_before_close`.
pub fn destroy_browser(tab: Id) {
    crate::devtools::on_inspected_gone(tab);
    let Some(browser_id) = TABS.with(|t| t.borrow().get(&tab).map(|e| e.browser_id)) else {
        log_warn!("DestroyBrowser for unknown tab {tab}");
        return;
    };
    let Some(browser_id) = browser_id else {
        remove_entry(tab);
        report_closed(tab, None);
        return;
    };
    let already = CLOSING.with(|c| {
        let mut c = c.borrow_mut();
        if c.contains_key(&browser_id) {
            return true;
        }
        c.insert(browser_id, Closing { tab, silent: false, view: None, detached: false });
        false
    });
    if already {
        return;
    }
    match browsers::browser(browser_id).and_then(|b| b.host()) {
        Some(host) => host.close_browser(1), // beforeunload auto-accepted (client.rs); do_close follows
        None => {
            CLOSING.with(|c| c.borrow_mut().remove(&browser_id));
            BY_BROWSER.with(|b| b.borrow_mut().remove(&browser_id));
            let in_peek = TABS.with(|t| t.borrow().get(&tab).is_some_and(|e| e.in_peek));
            let peek_view = if in_peek { overlays::take_back_peek_view(tab) } else { None };
            drop(peek_view);
            remove_entry(tab);
            report_closed(tab, Some(browser_id));
        }
    }
}

/// `do_close` for a tab browser (from browsers.rs). Never releases synchronously.
pub fn on_do_close(browser_id: i32, tab: Id) {
    CLOSING.with(|c| {
        c.borrow_mut().entry(browser_id).or_insert(Closing { tab, silent: false, view: None, detached: false });
    });
    task::post_ui(move || detach(browser_id));
}

/// Removes the closing browser's view from the hierarchy and drops our last references, which
/// makes CEF destroy the browser (`on_before_close`, possibly synchronously inside this call).
fn detach(browser_id: i32) {
    let Some((tab, silent, replaced_view)) = CLOSING.with(|c| {
        let mut c = c.borrow_mut();
        let closing = c.get_mut(&browser_id)?;
        if closing.detached {
            return None;
        }
        closing.detached = true;
        Some((closing.tab, closing.silent, closing.view.take()))
    }) else {
        return;
    };
    BY_BROWSER.with(|b| b.borrow_mut().remove(&browser_id));

    if silent {
        if let Some(view) = &replaced_view {
            detach_from_parent(view);
        }
        drop(replaced_view);
        return;
    }

    // The browser this tab's DevTools inspect is going away: take the dock apart while the wrapper
    // and the page view are still here, rather than leaving it to `browsers::on_before_close`.
    crate::devtools::on_inspected_gone(tab);

    // Only remove the tab entry if it still belongs to this browser.
    let entry = TABS.with(|t| {
        let mut tabs = t.borrow_mut();
        match tabs.get(&tab) {
            Some(e) if e.browser_id == Some(browser_id) => tabs.remove(&tab),
            _ => None,
        }
    });
    let Some(entry) = entry else { return };
    WRAPPER_COLORS.with(|c| c.borrow_mut().remove(&tab));
    LAST_ZOOM.with(|z| z.borrow_mut().remove(&tab));
    end_fullscreen_for_removed(tab);
    let TabEntry { wrapper, view, in_peek, .. } = entry;
    // Peek keeps a reference to a view it parents: take it back so ours is the last one.
    let peek_view = if in_peek { overlays::take_back_peek_view(tab) } else { None };
    if let Some(view) = &view {
        detach_from_parent(view);
    }
    if let Some(content) = window::content_panel() {
        content.remove_child_view(Some(&mut View::from(&wrapper)));
    }
    drop(peek_view);
    drop(wrapper);
    drop(view); // last reference → browser destruction → on_before_close
}

/// `on_before_close` for a tab browser (from browsers.rs).
pub fn on_before_close(browser_id: i32, tab: Id) {
    let closing = CLOSING.with(|c| c.borrow_mut().remove(&browser_id));
    BY_BROWSER.with(|b| b.borrow_mut().remove(&browser_id));
    BOOSTS_SENT.with(|m| m.borrow_mut().remove(&browser_id));
    PENDING_POPUPS.with(|p| p.borrow_mut().remove(&browser_id));
    match closing {
        Some(c) if c.silent => {}
        Some(_) => report_closed(tab, Some(browser_id)),
        None => {
            // Destroyed without do_close (window teardown). Drop a stale entry if it is ours.
            let stale = TABS.with(|t| {
                let mut tabs = t.borrow_mut();
                match tabs.get(&tab) {
                    Some(e) if e.browser_id == Some(browser_id) => tabs.remove(&tab),
                    _ => None,
                }
            });
            if stale.is_some() && !window::is_closing() {
                log_warn!("tab {tab} browser {browser_id} closed unexpectedly");
                report_closed(tab, Some(browser_id));
            }
            drop(stale);
        }
    }
}

// ----------------------------------------------------------------------------------- popups

/// `on_before_popup` of a web tab that allows the popup: allocates its tab id, remembers how core
/// should present it and returns its renderer `extra_info`. `None` = cancel the popup.
pub fn prepare_popup(
    opener_browser: i32,
    popup_id: i32,
    url: &str,
    disposition: WindowOpenDisposition,
    features_popup: bool,
) -> Option<DictionaryValue> {
    let tab = controller::alloc_id()?;
    let (info, version) = web_extra_info(tab, url)?;
    let url = if url.trim().is_empty() { "about:blank".to_string() } else { url.to_string() };
    let popup = features_popup || disposition == WindowOpenDisposition::NEW_POPUP;
    let foreground = disposition != WindowOpenDisposition::NEW_BACKGROUND_TAB;
    // Popups of agent-controlled tabs open as background tabs (no Peek, no activation).
    let (popup, foreground) = crate::automation::guards::popup_presentation(opener_browser).unwrap_or((popup, foreground));
    let pending = PendingPopup { popup_id, tab: Some(tab), url, popup, foreground, boosts_version: Some(version) };
    PENDING_POPUPS.with(|p| p.borrow_mut().entry(opener_browser).or_default().push_back(pending));
    Some(info)
}

/// `on_before_popup` allowed a popup that must keep CEF's default presentation
/// (Picture-in-Picture): keeps the pending queue in step without adopting it.
pub fn skip_popup(opener_browser: i32, popup_id: i32) {
    let pending = PendingPopup { popup_id, tab: None, url: String::new(), popup: false, foreground: false, boosts_version: None };
    PENDING_POPUPS.with(|p| p.borrow_mut().entry(opener_browser).or_default().push_back(pending));
}

/// `on_before_popup_aborted`.
pub fn popup_aborted(opener_browser: i32, popup_id: i32) {
    PENDING_POPUPS.with(|p| {
        if let Some(queue) = p.borrow_mut().get_mut(&opener_browser) {
            queue.retain(|x| x.popup_id != popup_id);
        }
    });
}

/// Tab id the next popup of `opener` will be adopted under; `None` = not ours to adopt.
fn next_popup_tab(opener: i32) -> Option<Id> {
    PENDING_POPUPS.with(|p| p.borrow().get(&opener).and_then(|q| q.front()).and_then(|x| x.tab))
}

/// `on_popup_browser_view_created`: adopt the popup view as a hidden tab and enqueue
/// `PopupAdopted`. Returns 1 when adopted (0 lets CEF create its default window).
fn adopt_popup(opener: Option<i32>, popup_view: &BrowserView) -> i32 {
    let pending = opener.and_then(|o| PENDING_POPUPS.with(|p| p.borrow_mut().get_mut(&o).and_then(VecDeque::pop_front)));
    let Some(pending) = pending else {
        log_warn!("popup view from browser {opener:?} without a pending on_before_popup: default window");
        return 0;
    };
    let Some(tab) = pending.tab else { return 0 };
    let Some(browser) = popup_view.browser() else {
        log_error!("popup view without a browser");
        return 0;
    };
    let browser_id = browser.identifier();
    let url = if pending.url == "about:blank" {
        browser
            .main_frame()
            .map(|f| CefString::from(&f.url()).to_string())
            .filter(|u| !u.is_empty())
            .unwrap_or(pending.url)
    } else {
        pending.url
    };
    begin_generation(tab);
    let Some(wrapper) = new_wrapper(tab) else { return 0 };
    TABS.with(|t| {
        t.borrow_mut().insert(
            tab,
            TabEntry { wrapper: wrapper.clone(), view: Some(popup_view.clone()), browser_id: Some(browser_id), internal: false, in_peek: false },
        )
    });
    on_browser_created(tab, browser_id);
    if let Some(version) = pending.boosts_version {
        BOOSTS_SENT.with(|m| m.borrow_mut().insert(browser_id, version));
    }
    wrapper.add_child_view(Some(&mut View::from(popup_view)));
    let opener_tab = opener.and_then(tab_for_browser);
    log_info!("adopted popup browser {browser_id} as tab {tab} (opener {opener_tab:?}, popup {}, foreground {})", pending.popup, pending.foreground);
    controller::dispatch(Command::PopupAdopted { tab, opener: opener_tab, url, popup: pending.popup, foreground: pending.foreground });
    crate::automation::guards::on_popup_adopted(opener_tab, tab);
    1
}

// ----------------------------------------------------------------------------------- content

/// `Effect::ShowContent`.
pub fn show_content(layout: &ContentLayout) {
    SHOWN.with(|s| *s.borrow_mut() = layout.clone());
    if let Some(fs) = PAGE_FULLSCREEN.with(|f| *f.borrow()) {
        let exists = TABS.with(|t| t.borrow().contains_key(&fs.tab));
        if exists && (fs.from_peek || layout.tabs().contains(&fs.tab)) {
            apply_fullscreen(fs.tab);
            return;
        }
        end_fullscreen_state_for(fs.tab);
    }
    apply_layout(layout);
}

fn apply_layout(layout: &ContentLayout) {
    let Some(content) = window::content_panel() else { return };
    let empty = window::empty_view();

    // Views that sit in Peek go back into their wrappers first.
    for tab in layout.tabs() {
        if TABS.with(|t| t.borrow().get(&tab).is_some_and(|e| e.in_peek))
            && let Some(view) = overlays::take_back_peek_view(tab)
        {
            return_view_from_peek(tab, view);
        }
    }

    let (horizontal, spacing) = match layout {
        ContentLayout::Split { orientation, .. } => (*orientation == Orientation::Horizontal, SPLIT_SPACING),
        _ => (true, 0),
    };
    let Some(box_layout) = content.set_to_box_layout(Some(&BoxLayoutSettings {
        horizontal: horizontal as i32,
        between_child_spacing: spacing,
        cross_axis_alignment: AxisAlignment::STRETCH,
        ..Default::default()
    })) else {
        return;
    };

    let wrappers: Vec<(Id, Panel)> = TABS.with(|t| t.borrow().iter().map(|(id, e)| (*id, e.wrapper.clone())).collect());
    let frame = window::frame_color();
    let accent = window::accent_color();

    // (tab, flex, color) of the panes to show, in order.
    let panes: Vec<(Id, i32, u32)> = match layout {
        ContentLayout::Empty => Vec::new(),
        ContentLayout::Single { tab } => vec![(*tab, 1, frame)],
        ContentLayout::Split { panes, focused, .. } => panes
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let flex = if p.fraction.is_finite() { (p.fraction * 1000.0).round() as i32 } else { 500 };
                (p.tab, flex.max(1), if i == *focused { accent } else { frame })
            })
            .collect(),
    };

    if let Some(empty) = &empty {
        let show_empty = matches!(layout, ContentLayout::Empty);
        empty.set_visible(show_empty as i32);
        if show_empty {
            box_layout.set_flex_for_view(Some(&mut View::from(empty)), 1);
        }
    }
    for (id, wrapper) in &wrappers {
        if !panes.iter().any(|(t, _, _)| t == id) {
            wrapper.set_visible(0);
            if WRAPPER_COLORS.with(|c| c.borrow().get(id).copied()) != Some(frame) {
                set_wrapper_color(*id, wrapper, frame);
            }
        }
    }
    for (index, (tab, flex, color)) in panes.iter().enumerate() {
        let Some((_, wrapper)) = wrappers.iter().find(|(id, _)| id == tab) else {
            log_warn!("ShowContent: tab {tab} has no browser");
            continue;
        };
        content.reorder_child_view(Some(&mut View::from(wrapper)), index as i32);
        box_layout.set_flex_for_view(Some(&mut View::from(wrapper)), *flex);
        set_wrapper_color(*tab, wrapper, *color);
        wrapper.set_visible(1);
    }
    content.layout();
    crate::devtools::relayout_all();
    rounded::layout_masks();
    overlays::layout();
    rescue_focus_from_hidden_tab();
}

/// Keyboard focus must not stay in a tab view that the layout just hid (e.g. switching to a space
/// without tabs shows `Empty` and nothing else takes focus): a hidden page never hands key events
/// back, so page-first accelerators (Alt+1..9, Ctrl+S, Ctrl+L, Ctrl+D, Ctrl+J) would stop working.
/// Posted, after the rest of the effect batch (a `FocusBrowser` for the new layout wins): if focus
/// is still in a hidden tab, it moves to core's focused tab when visible, else to the empty-state
/// view or the sidebar (`overlays::restore_main_focus`).
fn rescue_focus_from_hidden_tab() {
    let in_hidden_tab = || {
        overlays::focused_browser()
            .and_then(|id| match browsers::role_of(id) {
                Some(Role::Tab(tab)) => Some(tab),
                _ => None,
            })
            .filter(|tab| !is_tab_visible(*tab))
    };
    if in_hidden_tab().is_none() {
        return;
    }
    task::post_ui(move || {
        if window::is_closing() {
            return;
        }
        if let Some(tab) = in_hidden_tab() {
            log_debug!("focus was in hidden tab {tab}; moving it");
            overlays::restore_main_focus();
        }
    });
}

/// Page (HTML5) fullscreen presentation of the content area, called by
/// `window::set_page_fullscreen` (which hides the sidebar/topbar/insets and fullscreens the
/// window). `Some(tab)`: only that tab's wrapper is visible, without its 2 px border inset (a Peek
/// tab is moved into its wrapper for the duration). `None`: restore the last `ShowContent` layout
/// (and the Peek overlay if the tab came from there).
pub fn set_page_fullscreen_tab(tab: Option<Id>) {
    match tab {
        Some(tab) => {
            if !TABS.with(|t| t.borrow().contains_key(&tab)) {
                log_warn!("page fullscreen for unknown tab {tab}");
                return;
            }
            if let Some(previous) = PAGE_FULLSCREEN.with(|f| *f.borrow()).filter(|f| f.tab != tab) {
                end_fullscreen_state_for(previous.tab);
            }
            let from_peek = TABS.with(|t| t.borrow().get(&tab).is_some_and(|e| e.in_peek))
                || PAGE_FULLSCREEN.with(|f| f.borrow().is_some_and(|f| f.tab == tab && f.from_peek));
            PAGE_FULLSCREEN.with(|f| *f.borrow_mut() = Some(Fullscreen { tab, from_peek }));
            apply_fullscreen(tab);
            focus_browser(tab);
        }
        None => {
            let Some(fs) = PAGE_FULLSCREEN.with(|f| *f.borrow()) else {
                // Already ended (the fullscreen tab was destroyed): make sure the layout is back.
                let layout = SHOWN.with(|s| s.borrow().clone());
                apply_layout(&layout);
                return;
            };
            end_fullscreen_state_for(fs.tab);
            let layout = SHOWN.with(|s| s.borrow().clone());
            apply_layout(&layout);
            let still_exists = TABS.with(|t| t.borrow().contains_key(&fs.tab));
            if fs.from_peek && still_exists && !layout.tabs().contains(&fs.tab) {
                overlays::show_peek(fs.tab);
            } else if still_exists {
                focus_browser(fs.tab);
            }
        }
    }
}

/// Clears the fullscreen state (and the tab's inset override) if `tab` is the fullscreen tab.
/// Returns whether it was.
fn end_fullscreen_state_for(tab: Id) -> bool {
    let was = PAGE_FULLSCREEN.with(|f| {
        let mut f = f.borrow_mut();
        if f.is_some_and(|x| x.tab == tab) { f.take() } else { None }
    });
    if was.is_some()
        && let Some(wrapper) = TABS.with(|t| t.borrow().get(&tab).map(|e| e.wrapper.clone()))
    {
        set_wrapper_insets(&wrapper, WRAPPER_BORDER);
    }
    was.is_some()
}

/// The fullscreen tab's browser is gone: show the last layout again (the other wrappers were hidden).
fn end_fullscreen_for_removed(tab: Id) {
    if end_fullscreen_state_for(tab) {
        let layout = SHOWN.with(|s| s.borrow().clone());
        apply_layout(&layout);
    }
}

fn apply_fullscreen(tab: Id) {
    let Some(content) = window::content_panel() else { return };
    if TABS.with(|t| t.borrow().get(&tab).is_some_and(|e| e.in_peek))
        && let Some(view) = overlays::take_back_peek_view(tab)
    {
        return_view_from_peek(tab, view);
    }
    let Some(box_layout) = content.set_to_box_layout(Some(&BoxLayoutSettings {
        horizontal: 1,
        cross_axis_alignment: AxisAlignment::STRETCH,
        ..Default::default()
    })) else {
        return;
    };
    if let Some(empty) = window::empty_view() {
        empty.set_visible(0);
    }
    let wrappers: Vec<(Id, Panel)> = TABS.with(|t| t.borrow().iter().map(|(id, e)| (*id, e.wrapper.clone())).collect());
    let frame = window::frame_color();
    for (id, wrapper) in &wrappers {
        if *id == tab {
            set_wrapper_insets(wrapper, 0);
            set_wrapper_color(*id, wrapper, frame);
            box_layout.set_flex_for_view(Some(&mut View::from(wrapper)), 1);
            wrapper.set_visible(1);
        } else {
            wrapper.set_visible(0);
        }
    }
    content.layout();
    crate::devtools::relayout_all();
    rounded::layout_masks();
    overlays::layout();
}

/// `Effect::FocusBrowser` (asynchronous in CEF).
pub fn focus_browser(tab: Id) {
    if let Some(view) = view_for_tab(tab) {
        view.request_focus();
    }
}

/// The tab's BrowserView (in its wrapper or in the Peek overlay).
pub fn view_for_tab(tab: Id) -> Option<BrowserView> {
    TABS.with(|t| t.borrow().get(&tab).and_then(|e| e.view.clone()))
}

/// Recolors wrappers after `SetChrome`.
pub fn on_chrome_colors_changed() {
    let layout = SHOWN.with(|s| s.borrow().clone());
    let fullscreen = PAGE_FULLSCREEN.with(|f| f.borrow().map(|f| f.tab));
    let focused = match &layout {
        ContentLayout::Split { panes, focused, .. } if fullscreen.is_none() => panes.get(*focused).map(|p| p.tab),
        _ => None,
    };
    let wrappers: Vec<(Id, Panel)> = TABS.with(|t| t.borrow().iter().map(|(id, e)| (*id, e.wrapper.clone())).collect());
    for (id, wrapper) in wrappers {
        let color = if Some(id) == focused { window::accent_color() } else { window::frame_color() };
        set_wrapper_color(id, &wrapper, color);
    }
    rounded::layout_masks();
}

// ----------------------------------------------------------------------------------- Peek hand-off

/// Detaches the tab's view from its wrapper for the Peek overlay (the tab keeps ownership in
/// the registry). The caller parents the returned view.
pub fn take_view_for_peek(tab: Id) -> Option<BrowserView> {
    let (wrapper, view) = TABS.with(|t| {
        let mut tabs = t.borrow_mut();
        let e = tabs.get_mut(&tab)?;
        if e.in_peek {
            return None;
        }
        e.in_peek = true;
        Some((e.wrapper.clone(), e.view.clone()?))
    })?;
    detach_from_parent(&view);
    wrapper.set_visible(0);
    view.set_background_color(view_background(tab));
    crate::devtools::relayout(tab);
    Some(view)
}

/// Puts a view taken with [`take_view_for_peek`] (and already removed from the Peek host) back
/// into its wrapper.
pub fn return_view_from_peek(tab: Id, view: BrowserView) {
    let wrapper = TABS.with(|t| {
        let mut tabs = t.borrow_mut();
        let e = tabs.get_mut(&tab)?;
        e.in_peek = false;
        Some(e.wrapper.clone())
    });
    if let Some(wrapper) = wrapper {
        detach_from_parent(&view);
        // Added last, so it stays on top of a docked DevTools frontend.
        let parent = page_parent(tab).unwrap_or(wrapper);
        parent.add_child_view(Some(&mut View::from(&view)));
        view.set_background_color(view_background(tab));
        crate::devtools::relayout(tab);
    }
}

// ----------------------------------------------------------------------------------- page actions

pub fn load_url(tab: Id, url: &str) {
    // Core only loads URLs for user actions (typed URL, reset to pinned, retry): an external
    // protocol goes to the OS and the current page stays.
    if external::is_external(url) {
        external::open(url, true, "load");
        return;
    }
    if let Some(browser) = browser_for_tab(tab) {
        sync_boosts(&browser, url);
        if let Some(frame) = browser.main_frame() {
            frame.load_url(Some(&CefString::from(url)));
        }
    }
}

pub fn go_back(tab: Id) {
    if let Some(b) = browser_for_tab(tab) {
        b.go_back();
    }
}

pub fn go_forward(tab: Id) {
    if let Some(b) = browser_for_tab(tab) {
        b.go_forward();
    }
}

pub fn reload(tab: Id, ignore_cache: bool) {
    if let Some(b) = browser_for_tab(tab) {
        // Boost changes reload affected tabs: make sure the renderer has the new list first.
        if let Some(url) = b.main_frame().map(|f| CefString::from(&f.url()).to_string()) {
            sync_boosts(&b, &url);
        }
        if ignore_cache {
            b.reload_ignore_cache();
        } else {
            b.reload();
        }
    }
}

pub fn stop_load(tab: Id) {
    if let Some(b) = browser_for_tab(tab) {
        b.stop_load();
    }
}

/// Steps through Chrome's preset zoom levels and reports the result.
pub fn zoom(tab: Id, direction: ZoomDirection) {
    let Some(host) = host_for_tab(tab) else { return };
    let percent = 100.0 * 1.2f64.powf(host.zoom_level());
    let next = match direction {
        ZoomDirection::Reset => 100.0,
        ZoomDirection::In => ZOOM_PRESETS.iter().copied().find(|p| *p > percent + 0.5).unwrap_or(500.0),
        ZoomDirection::Out => ZOOM_PRESETS.iter().rev().copied().find(|p| *p < percent - 0.5).unwrap_or(25.0),
    };
    host.set_zoom_level((next / 100.0).ln() / 1.2f64.ln());
    // Read the applied level back in a later task (always reported: core shows a zoom toast).
    task::post_ui(move || report_zoom(tab, true));
}

fn report_zoom(tab: Id, force: bool) {
    let Some(host) = host_for_tab(tab) else { return };
    let level = host.zoom_level();
    let changed = LAST_ZOOM.with(|z| z.borrow_mut().insert(tab, level)) != Some(level);
    if force || changed {
        controller::dispatch(Command::TabZoomChanged { tab, level });
    }
}

/// `LoadHandler::on_load_end` (main frame): Chromium restores per-host zoom on navigation.
pub fn report_zoom_later(tab: Id) {
    task::post_ui(move || report_zoom(tab, false));
}

pub fn set_audio_muted(tab: Id, muted: bool) {
    if let Some(host) = host_for_tab(tab) {
        host.set_audio_muted(muted as i32);
    }
}

/// `STA_DEVTOOLS_INTERNAL=1` (debug builds only): DevTools may open on `sta://` pages.
pub const DEVTOOLS_INTERNAL_ENV: &str = "STA_DEVTOOLS_INTERNAL";

fn devtools_on_internal_pages() -> bool {
    cfg!(debug_assertions) && std::env::var(DEVTOOLS_INTERNAL_ENV).is_ok_and(|v| v == "1")
}

/// Startup: core refuses `ToggleDevTools` on `sta://` pages unless the debug override is set.
pub fn init_devtools_policy() {
    let allowed = devtools_on_internal_pages();
    if allowed {
        log_warn!("{DEVTOOLS_INTERNAL_ENV}=1: DevTools may open on sta:// pages (debug build)");
    }
    controller::with_store_mut(|s| s.set_devtools_on_internal_pages(allowed));
}

/// Opens (or focuses) **undocked** DevTools for `browser` — CEF's own Chrome-style window —
/// inspecting the element at `at` if given. Called by `devtools.rs` for the undock path (the
/// frontend's Undock button, and DevTools on a Peek page); the docked path builds a BrowserView
/// inside the tab's wrapper instead.
///
/// The DevTools browser gets a client of its own whose only handler closes it again on
/// F12 / Ctrl+Shift+I (the window's accelerators don't reach that separate top-level window).
///
/// Never for trusted UI browsers (`sta://` pages: DevTools extensions could reach sta's internal
/// commands through them), in any build, unless `STA_DEVTOOLS_INTERNAL=1` in a debug build.
///
/// Of the two callers, `Effect::ToggleDevTools` is already refused by core with the toast
/// "DevTools isn't available on sta pages", and the context menu's Inspect never reaches an
/// internal page at all: those run on the trusted UI client, whose `ContextMenuHandler` keeps only
/// the edit commands, so no Inspect item exists to click (`context_menu.rs`). This check is the
/// backstop for both, which is why it only WARNs — no gesture can get here.
pub fn show_dev_tools(browser: &Browser, at: Option<Point>) {
    if browsers::is_ui_browser(browser.identifier()) && !devtools_on_internal_pages() {
        log_warn!("DevTools refused for sta:// browser {}", browser.identifier());
        return;
    }
    let Some(host) = browser.host() else { return };
    let mut devtools_client = client::devtools_client(browser.identifier());
    host.show_dev_tools(None, Some(&mut devtools_client), Some(&BrowserSettings::default()), at.as_ref());
}

pub fn print(tab: Id) {
    if let Some(host) = host_for_tab(tab) {
        host.print();
    }
}

/// `Effect::Find`. CEF 152 only counts matches for `findNext = false` (no active match, and an
/// identical repeat is dropped without a result), while `findNext = true` on a new text starts a
/// session *and* selects the first match. So a new query (`find_next == false`) restarts the
/// session and asks with `findNext = true`: the find bar gets Chrome's "1/3" right away.
pub fn find(tab: Id, text: &str, forward: bool, match_case: bool, find_next: bool) {
    let Some(host) = host_for_tab(tab) else { return };
    if !find_next {
        // Clearing the selection makes the restarted search begin at the top of the page.
        host.stop_finding(1);
    }
    host.find(Some(&CefString::from(text)), forward as i32, match_case as i32, 1);
}

pub fn stop_finding(tab: Id) {
    if let Some(host) = host_for_tab(tab) {
        host.stop_finding(1);
    }
}

pub fn exit_page_fullscreen(tab: Id) {
    if let Some(host) = host_for_tab(tab) {
        host.exit_fullscreen(1);
    }
}

pub fn start_download(tab: Id, url: &str) {
    if let Some(host) = host_for_tab(tab) {
        host.start_download(Some(&CefString::from(url)));
    }
}

// ----------------------------------------------------------------------------------- teardown

/// Drops every tab handle (window destroyed).
pub fn clear() {
    let tabs = TABS.with(|t| std::mem::take(&mut *t.borrow_mut()));
    let closing = CLOSING.with(|c| std::mem::take(&mut *c.borrow_mut()));
    BY_BROWSER.with(|b| b.borrow_mut().clear());
    WRAPPER_COLORS.with(|c| c.borrow_mut().clear());
    PENDING_POPUPS.with(|p| p.borrow_mut().clear());
    BOOSTS_SENT.with(|m| m.borrow_mut().clear());
    LAST_ZOOM.with(|z| z.borrow_mut().clear());
    PAGE_FULLSCREEN.with(|f| *f.borrow_mut() = None);
    SHOWN.with(|s| *s.borrow_mut() = ContentLayout::Empty);
    drop(tabs);
    drop(closing);
}

#[cfg_attr(not(debug_assertions), allow(dead_code))] // used by debug.rs only
pub fn debug_snapshot() -> serde_json::Value {
    // Handles are copied out first: the Views getters below must not run inside the borrow.
    let entries: Vec<_> = TABS.with(|t| {
        t.borrow()
            .iter()
            .map(|(id, e)| (*id, e.wrapper.clone(), e.view.clone(), e.browser_id, e.internal, e.in_peek))
            .collect()
    });
    let tabs: Vec<serde_json::Value> = entries
        .into_iter()
        .map(|(id, wrapper, view, browser_id, internal, in_peek)| {
            let b = wrapper.bounds();
            let insets = view.as_ref().map(|v| {
                let vb = View::from(v).bounds();
                [vb.x, vb.y]
            });
            let docked = crate::devtools::page_parent(id).is_some();
            let url = browser_id
                .and_then(browsers::browser)
                .and_then(|b| b.main_frame())
                .map(|f| CefString::from(&f.url()).to_string());
            // Web tabs: the boost list version last shipped to the renderer vs the list for the
            // current URL (and how many boosts that list holds).
            let boosts = (!internal).then_some(()).and(browser_id).zip(url.as_deref()).map(|(bid, u)| {
                let (json, expected) = boosts_payload_for(u);
                let count = serde_json::from_str::<Vec<BoostData>>(&json).map(|l| l.len()).unwrap_or(0);
                serde_json::json!({ "sent": BOOSTS_SENT.with(|m| m.borrow().get(&bid).cloned()), "expected": expected, "count": count })
            });
            serde_json::json!({
                "boosts": boosts,
                "tab": id,
                "browserId": browser_id,
                "internal": internal,
                "inPeek": in_peek,
                "visible": wrapper.is_visible() != 0,
                "hasView": view.is_some(),
                "viewOrigin": insets,
                "devtoolsDocked": docked,
                "wrapperBounds": [b.x, b.y, b.width, b.height],
                "wrapperColor": WRAPPER_COLORS.with(|c| c.borrow().get(&id).map(|c| format!("#{c:08x}"))),
                "url": url,
            })
        })
        .collect();
    let closing: Vec<i32> = CLOSING.with(|c| c.borrow().keys().copied().collect());
    let shown = SHOWN.with(|s| serde_json::to_value(&*s.borrow()).ok());
    let fullscreen = PAGE_FULLSCREEN.with(|f| f.borrow().map(|f| serde_json::json!({ "tab": f.tab, "fromPeek": f.from_peek })));
    let pending_popups: usize = PENDING_POPUPS.with(|p| p.borrow().values().map(VecDeque::len).sum());
    serde_json::json!({ "tabs": tabs, "closing": closing, "shown": shown, "pageFullscreen": fullscreen, "pendingPopups": pending_popups })
}
