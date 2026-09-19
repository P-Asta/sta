//! Chrome-created browsers [owner: tabs] (ARCHITECTURE §4.5; ext design FINAL PLAN §2).
//!
//! Some Chromium code paths create their own Chrome `Browser` (a Chrome window with tab strip and
//! toolbar) when the profile has none: the Web Store's post-install UI
//! (`ExtensionInstallUIDesktop::OnInstallSuccess` → new tab page + "added" dialog), an extension's
//! `tabs.create` / `windows.create` / `runtime.openOptionsPage`, `identity.launchWebAuthFlow`, the
//! DevTools protocol's `Target.createTarget`. CEF gives each tab of such a browser
//! `BrowserProcessHandler::GetDefaultClient` = [`client`]. This module keeps those windows from
//! ever being seen or taking the keyboard, turns what they wanted to show into sta tabs, and closes
//! them safely.
//!
//! **`on_after_created`** (every tab of a Chrome-created window):
//! - `has_view()` browsers are skipped (Chrome-style views sta creates itself);
//! - a request context that doesn't share the global one's storage (incognito) → hidden, closed,
//!   toast "sta has no private windows" (`ForeignBlocked`);
//! - **KeepNative**: a `_crx_<id>` window title (`windows.create({type:'popup'})`), a main frame
//!   that already navigated (`launchWebAuthFlow`), a `devtools://` page, or a tab in a window that
//!   is already native. These stay ordinary Chromium windows (extension popups and sign-in flows
//!   need their window, user decision D2) with a caption following sta's theme; never adopted;
//!   closed at shutdown;
//! - otherwise **Pending**: the root window is cloaked while it isn't visible yet
//!   (`platform::hidden_windows`: DWM cloak, activation refused by a `WH_CBT` hook, owned dialogs
//!   cloaked before they show, backstops), every key is consumed. Still Pending after 500 ms with no
//!   navigation → KeepNative (uncloaked) with a WARN.
//!
//! **Navigation** (`on_before_browse`, main frame, hidden browsers): always cancelled (the page
//! never loads, so it can't act in a hidden window), then:
//! - `chrome://newtab/`, `chrome://new-tab-page/` → **PostInstall**: the profile's `Extensions`
//!   directory is diffed against the last scan; each new extension → `ExtensionInstalled{id, name,
//!   external}` (toast);
//! - `http(s)` with a host → `ForeignTabRequested{url}`; `chrome-extension://<id>/<page>` →
//!   `ForeignTabRequested{url, extension}` with what that extension's own files say (core decides);
//!   `chrome://extensions/?options=<id>` is rewritten to the extension's options page first;
//! - anything else → WARN (scheme and browser id only).
//!
//! How many tabs a Chrome-created browser may actually open is **core's** budget (3 in 10 s, spent
//! on the verdict rather than on the request: `sta_core::store::foreign`). What this module limits
//! is its own work — see [`HANDLE_MAX`].
//!
//! **Closing**: a hidden window closes (`close_browser(1)` on each of its tabs) once it is at
//! least 3 s old and got no new tab for 1 s (an extension's welcome tab often comes a moment after
//! the post-install window), or at 10 s, and never while a download is in progress (closing
//! Chromium's last window with downloads running asks or cancels them). KeepNative windows close
//! at shutdown only. Every browser here is counted by `browsers::extra_*`, so the main window
//! waits for them; `window::begin_shutdown` cancels downloads, then calls [`close_all`].
//!
//! Public API:
//! - `pub fn client() -> Client` (for `BrowserProcessHandler::default_client`)
//! - `pub fn startup()` — scan the Extensions directory, install the window hooks
//! - `pub fn close_all()` — shutdown, `pub fn clear()` — teardown
//! - `pub fn debug_snapshot() -> serde_json::Value`

// `wrap_client!` generates a `new` taking one argument per handler field.
#![allow(clippy::too_many_arguments)]

use crate::extension_files::{self, ExtensionFiles, Source};
use crate::platform::hidden_windows as hw;
use crate::{browsers, controller, downloads, platform, task, window};
use cef::sys::MSG;
use cef::*;
use serde_json::{Value, json};
use sta_core::{Command, ForeignBlockReason, ForeignExtension};
use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::time::{Duration, Instant};

/// A hidden browser that never navigated becomes a native window after this long.
const PENDING_TIMEOUT_MS: i64 = 500;
/// A hidden window lives at least this long…
const MIN_AGE: Duration = Duration::from_millis(3000);
/// …and until it got no new tab for this long…
const QUIET: Duration = Duration::from_millis(1000);
/// …but closes at this age at the latest (only downloads keep it open then).
const MAX_AGE: Duration = Duration::from_millis(10_000);
const CLOSE_POLL_MS: i64 = 250;
/// Cancelling a navigation of a Chrome-created browser is cheap; deciding what it *meant* is not —
/// a `chrome-extension://` URL is answered from that extension's manifest and `_locales` on disk,
/// and a new tab page rescans the whole `Extensions` directory, both on the UI thread. So the shell
/// classifies at most `HANDLE_MAX` navigations within `HANDLE_WINDOW` and drops the rest with one
/// `ForeignBlocked{RateLimited}` toast.
///
/// This is a **resource** guard, not the user-visible rule: how many tabs those browsers may open
/// is core's budget, spent after `urls::foreign_tab_verdict` (`sta_core::store::foreign`), so a page
/// the user is only *asked* about doesn't spend it. `HANDLE_MAX` is far above what a real flow needs
/// (a post-install window, an options page or a sign-in navigate once or twice), so in practice only
/// a runaway extension ever meets it and the two limits never interact.
const HANDLE_MAX: usize = 24;
const HANDLE_WINDOW: Duration = Duration::from_millis(10_000);
/// An extension whose files were written this recently counts as just installed.
const RECENT_INSTALL_MS: i64 = 60_000;
/// Extensions other programs registered are installed during the first seconds of a run; the
/// silent re-scan after this long keeps them out of the install toasts (see [`startup`]).
const STARTUP_RESCAN_MS: i64 = 12_000;
/// At most this many install toasts per post-install window.
const MAX_INSTALL_TOASTS: usize = 3;
/// Debug events kept for `debug.info`.
const EVENTS_KEPT: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// Hidden, no navigation yet.
    Pending,
    /// Hidden new tab page of the Web Store's post-install window.
    PostInstall,
    /// Hidden; its navigation was handed to core (or refused).
    Handled,
    /// A Chromium window left visible (extension popup, sign-in, DevTools, unknown).
    KeepNative,
    /// Hidden and closed at once (incognito, shutdown).
    Blocked,
}

struct Entry {
    browser: Browser,
    root: isize,
    kind: Kind,
    created: Instant,
    closing: bool,
    /// Why it stayed native, or what it was (debug).
    note: String,
}

#[derive(Default, Clone, Copy)]
struct Stats {
    created: u64,
    kept_native: u64,
    post_install: u64,
    tab_requests: u64,
    throttled: u64,
    incognito: u64,
    warned: u64,
    closed: u64,
    installs: u64,
}

thread_local! {
    static CLIENT: RefCell<Option<Client>> = const { RefCell::new(None) };
    static ENTRIES: RefCell<BTreeMap<i32, Entry>> = const { RefCell::new(BTreeMap::new()) };
    /// Root window → when it last got a new tab.
    static ROOT_ACTIVITY: RefCell<HashMap<isize, Instant>> = RefCell::new(HashMap::new());
    /// When the last `HANDLE_MAX` navigations were classified (see `HANDLE_MAX`).
    static HANDLED: RefCell<VecDeque<Instant>> = const { RefCell::new(VecDeque::new()) };
    static THROTTLE_TOAST: Cell<Option<Instant>> = const { Cell::new(None) };
    /// Extensions directory contents at the last scan (`id → version dirs`).
    static KNOWN: RefCell<Option<BTreeMap<String, Vec<String>>>> = const { RefCell::new(None) };
    /// When `KNOWN` was taken (ms since the Unix epoch).
    static LAST_SCAN_MS: Cell<i64> = const { Cell::new(0) };
    static POLL_SCHEDULED: Cell<bool> = const { Cell::new(false) };
    static STATS: Cell<Stats> = Cell::new(Stats::default());
    static EVENTS: RefCell<VecDeque<Value>> = const { RefCell::new(VecDeque::new()) };
    static START: Instant = Instant::now();
}

fn stats(f: impl FnOnce(&mut Stats)) {
    let mut s = STATS.get();
    f(&mut s);
    STATS.set(s);
}

fn event(kind: &str, data: Value) {
    let t = START.with(|s| s.elapsed().as_millis() as u64);
    EVENTS.with(|e| {
        let mut e = e.borrow_mut();
        e.push_back(json!({ "t": t, "kind": kind, "data": data }));
        while e.len() > EVENTS_KEPT {
            e.pop_front();
        }
    });
}

/// Scheme and host of a URL for logs (never paths or queries).
fn log_url(url: &str) -> String {
    match url.split_once("://") {
        Some((scheme, rest)) => format!("{scheme}://{}", rest.split(['/', '?', '#']).next().unwrap_or_default()),
        None => url.split(':').next().unwrap_or_default().to_string() + ":",
    }
}

// ----------------------------------------------------------------------------------- lifecycle

/// The client CEF gives every tab of a Chrome-created window.
pub fn client() -> Client {
    if let Some(c) = CLIENT.with(|c| c.borrow().clone()) {
        return c;
    }
    let client = ForeignClient::new(ForeignLifeSpan::new(), ForeignRequest::new(), ForeignKeyboard::new(), ForeignDisplay::new(), ForeignDownload::new());
    CLIENT.with(|c| *c.borrow_mut() = Some(client.clone()));
    client
}

/// `on_context_initialized`: remember which extensions are installed; install the window hooks
/// once the main window exists.
pub fn startup() {
    let scan = extension_files::extensions_dir().map(|d| extension_files::scan(&d)).unwrap_or_default();
    log_debug!("foreign: {} installed extension(s) at startup", scan.len());
    KNOWN.with(|k| *k.borrow_mut() = Some(scan));
    LAST_SCAN_MS.set(sta_core::now_ms());
    hw::set_sta_owned_listener(on_sta_owned_window_shown);
    install_hooks_when_window_exists(0);
    // Chromium installs the extensions other programs registered (the Windows registry, external
    // preferences) during the first seconds of a run. They are not this session's installs: a
    // silent re-scan takes them into the "known" set instead of announcing them later (phase 3's
    // Settings › Extensions has the "N extensions were added by other programs" review).
    task::post_ui_delayed(STARTUP_RESCAN_MS, refresh_known);
}

fn install_hooks_when_window_exists(attempt: u32) {
    let hwnd = window::hwnd_value();
    if hwnd != 0 {
        hw::install(hwnd);
        return;
    }
    if attempt < 100 {
        task::post_ui_delayed(100, move || install_hooks_when_window_exists(attempt + 1));
    }
}

fn is_ntp(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    lower.starts_with("chrome://newtab") || lower.starts_with("chrome://new-tab-page")
}

fn on_after_created(browser: &Browser) {
    let id = browser.identifier();
    let Some(host) = browser.host() else { return };
    if host.has_view() != 0 {
        log_debug!("foreign: browser {id} is a Views-hosted Chrome browser; not handled");
        return;
    }
    let hwnd = host.window_handle().0 as isize;
    let root = hw::root_of(hwnd);
    browsers::extra_register(id);
    stats(|s| s.created += 1);
    hw::install(window::hwnd_value());

    let title = hw::window_title(root);
    let url = browser.main_frame().map(|f| CefString::from(&f.url()).to_string()).unwrap_or_default();
    let incognito = match (host.request_context(), request_context_get_global_context()) {
        (Some(ctx), Some(mut global)) => ctx.is_sharing_with(Some(&mut global)) == 0,
        _ => false,
    };
    let root_native = ENTRIES.with(|e| e.borrow().values().any(|x| x.root == root && x.kind == Kind::KeepNative));
    let (kind, note) = if incognito {
        (Kind::Blocked, "incognito".to_string())
    } else if window::is_closing() {
        (Kind::Blocked, "shutdown".to_string())
    } else if root == 0 || root == window::hwnd_value() {
        // `launchWebAuthFlow` browsers arrive before their window exists; sta's own window is
        // never hidden. Both keep Chromium's own presentation.
        (Kind::KeepNative, "no window of its own (yet)".to_string())
    } else if title.starts_with("_crx_") {
        (Kind::KeepNative, "extension popup window".to_string())
    } else if url.to_ascii_lowercase().starts_with("devtools:") {
        (Kind::KeepNative, "devtools".to_string())
    } else if !url.is_empty() && !url.eq_ignore_ascii_case("about:blank") {
        (Kind::KeepNative, "already navigated".to_string())
    } else if root_native {
        (Kind::KeepNative, "tab of a native window".to_string())
    } else {
        (Kind::Pending, String::new())
    };
    let visible = hw::is_visible(root);
    if kind != Kind::KeepNative {
        hw::hide_root(root);
    } else {
        stats(|s| s.kept_native += 1);
        hw::set_native_caption(root, window::is_dark());
    }
    if root != 0 {
        ROOT_ACTIVITY.with(|r| r.borrow_mut().insert(root, Instant::now()));
    }
    event(
        "created",
        json!({ "id": id, "root": root, "kind": format!("{kind:?}"), "note": note, "visibleAtCreate": visible, "incognito": incognito, "popup": browser.is_popup() != 0 }),
    );
    log_info!("foreign: browser {id} created ({kind:?}{}{})", if note.is_empty() { "" } else { ", " }, note);
    ENTRIES.with(|e| e.borrow_mut().insert(id, Entry { browser: browser.clone(), root, kind, created: Instant::now(), closing: false, note }));
    match kind {
        Kind::Pending => task::post_ui_delayed(PENDING_TIMEOUT_MS, move || pending_timeout(id)),
        Kind::KeepNative => task::post_ui_delayed(300, move || refresh_native_root(id, 0)),
        Kind::Blocked => {
            if incognito {
                stats(|s| s.incognito += 1);
                controller::dispatch(Command::ForeignBlocked { reason: ForeignBlockReason::Incognito });
            }
            task::post_ui(move || close_now(id));
        }
        _ => {}
    }
    schedule_poll();
}

/// A window that stays native gets sta's caption and icon once it has one (a `launchWebAuthFlow`
/// browser has no root window when it is created).
fn refresh_native_root(id: i32, attempt: u32) {
    let browser = ENTRIES.with(|e| e.borrow().get(&id).filter(|x| x.kind == Kind::KeepNative).map(|x| x.browser.clone()));
    let Some(browser) = browser else { return };
    let root = browser.host().map(|h| hw::root_of(h.window_handle().0 as isize)).unwrap_or(0);
    if root == 0 {
        if attempt < 12 {
            task::post_ui_delayed(300, move || refresh_native_root(id, attempt + 1));
        }
        return;
    }
    ENTRIES.with(|e| {
        if let Some(entry) = e.borrow_mut().get_mut(&id) {
            entry.root = root;
        }
    });
    hw::set_native_caption(root, window::is_dark());
}

fn pending_timeout(id: i32) {
    let root = ENTRIES.with(|e| {
        let mut e = e.borrow_mut();
        let entry = e.get_mut(&id).filter(|x| x.kind == Kind::Pending && !x.closing)?;
        entry.kind = Kind::KeepNative;
        entry.note = "no navigation within 500 ms".into();
        Some(entry.root)
    });
    let Some(root) = root else { return };
    log_warn!("foreign: browser {id} did not navigate within {PENDING_TIMEOUT_MS} ms; showing its Chromium window");
    stats(|s| {
        s.kept_native += 1;
        s.warned += 1;
    });
    event("pending-timeout", json!({ "id": id, "root": root }));
    // Other hidden tabs of that window keep their own rules; the window itself becomes visible.
    hw::show_root(root);
    hw::set_native_caption(root, window::is_dark());
}

fn on_before_close(browser: &Browser) {
    let id = browser.identifier();
    let entry = ENTRIES.with(|e| e.borrow_mut().remove(&id));
    browsers::extra_unregister(id);
    if let Some(entry) = entry {
        stats(|s| s.closed += 1);
        let root = entry.root;
        let shared = ENTRIES.with(|e| e.borrow().values().any(|x| x.root == root));
        if !shared {
            hw::forget_root(root);
            ROOT_ACTIVITY.with(|r| r.borrow_mut().remove(&root));
        }
        event("closed", json!({ "id": id, "kind": format!("{:?}", entry.kind) }));
        log_debug!("foreign: browser {id} closed ({:?})", entry.kind);
        drop(entry);
    }
    window::on_browser_closed();
}

// ----------------------------------------------------------------------------------- navigation

/// `on_before_browse` of a main frame: `true` = cancel.
fn on_main_frame_navigation(browser: &Browser, url: &str) -> bool {
    let id = browser.identifier();
    let kind = ENTRIES.with(|e| e.borrow().get(&id).map(|x| x.kind));
    let Some(kind) = kind else { return false };
    if kind == Kind::KeepNative {
        return false;
    }
    if kind == Kind::Blocked {
        return true;
    }
    let new_kind = if is_ntp(url) { Kind::PostInstall } else { Kind::Handled };
    let root = ENTRIES.with(|e| {
        let mut e = e.borrow_mut();
        let entry = e.get_mut(&id)?;
        entry.kind = new_kind;
        Some(entry.root)
    });
    if let Some(root) = root {
        ROOT_ACTIVITY.with(|r| r.borrow_mut().insert(root, Instant::now()));
    }
    let url = url.to_string();
    // File reads and core commands happen outside the CEF callback.
    task::post_ui(move || handle_navigation(id, &url, new_kind));
    true
}

fn handle_navigation(id: i32, url: &str, kind: Kind) {
    if !may_classify(id, url) {
        return;
    }
    if kind == Kind::PostInstall {
        stats(|s| s.post_install += 1);
        event("post-install", json!({ "id": id }));
        detect_installs();
        schedule_poll();
        return;
    }
    let lower = url.to_ascii_lowercase();
    let target = if lower.starts_with("http://") || lower.starts_with("https://") {
        Some((url.to_string(), None))
    } else if lower.starts_with("chrome-extension://") {
        extension_request(url)
    } else if lower.starts_with("chrome://extensions") {
        options_rewrite(url)
    } else {
        None
    };
    match target {
        Some((url, extension)) => request_tab(id, url, extension),
        None => {
            stats(|s| s.warned += 1);
            event("ignored", json!({ "id": id, "url": log_url(url) }));
            log_warn!("foreign: browser {id} wanted to load a {} page; cancelled", log_url(url));
        }
    }
    schedule_poll();
}

/// `chrome-extension://<id>/…` with what that extension's files say about it.
fn extension_request(url: &str) -> Option<(String, Option<ForeignExtension>)> {
    let (ext_id, _) = sta_core::urls::extension_url_parts(url)?;
    let files = extension_files::find(&ext_id);
    Some((url.to_string(), files.map(|f| foreign_extension(&f))))
}

/// `chrome://extensions/?options=<id>` (embedded options) → the extension's options page.
fn options_rewrite(url: &str) -> Option<(String, Option<ForeignExtension>)> {
    let query = url.split_once('?')?.1.split('#').next().unwrap_or_default();
    let ext_id = query.split('&').find_map(|kv| kv.strip_prefix("options="))?;
    let files = extension_files::find(ext_id)?;
    let page = files.options_page()?;
    Some((format!("chrome-extension://{ext_id}/{page}"), Some(foreign_extension(&files))))
}

fn foreign_extension(files: &ExtensionFiles) -> ForeignExtension {
    let now = sta_core::now_ms();
    let recently_installed = files.source == Source::Installed && files.modified_ms.is_some_and(|m| now.saturating_sub(m) <= RECENT_INSTALL_MS);
    ForeignExtension {
        id: files.id.clone(),
        name: files.name(&platform::os_ui_locale()),
        pages: [files.options_page(), files.popup_page(), files.side_panel_page()].into_iter().flatten().collect(),
        web_accessible: files.web_accessible_to_all(),
        recently_installed,
    }
}

/// One slot of `HANDLE_MAX` (a runaway extension is stopped before the manifest reads, not after).
/// Over budget: one toast per `HANDLE_WINDOW`, and the navigation is dropped — it was cancelled by
/// `on_main_frame_navigation` either way.
fn may_classify(id: i32, url: &str) -> bool {
    let now = Instant::now();
    let room = HANDLED.with(|h| {
        let mut h = h.borrow_mut();
        while h.front().is_some_and(|t| now.duration_since(*t) > HANDLE_WINDOW) {
            h.pop_front();
        }
        if h.len() >= HANDLE_MAX {
            return false;
        }
        h.push_back(now);
        true
    });
    if !room {
        stats(|s| s.throttled += 1);
        event("throttled", json!({ "id": id, "url": log_url(url) }));
        log_warn!("foreign: browser {id}: more than {HANDLE_MAX} navigations in {HANDLE_WINDOW:?}; dropped {}", log_url(url));
        if THROTTLE_TOAST.get().is_none_or(|t| now.duration_since(t) > HANDLE_WINDOW) {
            THROTTLE_TOAST.set(Some(now));
            controller::dispatch(Command::ForeignBlocked { reason: ForeignBlockReason::RateLimited });
        }
    }
    room
}

fn request_tab(id: i32, url: String, extension: Option<ForeignExtension>) {
    stats(|s| s.tab_requests += 1);
    event("tab-requested", json!({ "id": id, "url": log_url(&url), "extension": extension.as_ref().map(|e| e.id.clone()) }));
    log_info!("foreign: browser {id} → tab for {}", log_url(&url));
    controller::dispatch(Command::ForeignTabRequested { url, extension });
}

/// The startup re-scan ([`STARTUP_RESCAN_MS`]): takes what **other programs** registered into the
/// known set without announcing it, and leaves everything else alone.
///
/// Absorbing the whole directory would swallow this session's own installs: an extension whose files
/// land in the first 12 s (a Web Store install started right after launch) would afterwards look like
/// it had always been there, and the post-install window would report nothing — no toast, no
/// `recent_installs` entry. So only ids with an external install location are absorbed, which is
/// exactly what this scan is for.
fn refresh_known() {
    let Some(dir) = extension_files::extensions_dir() else { return };
    let scan = extension_files::scan(&dir);
    let found = scan.len();
    let merged = KNOWN.with(|k| {
        let previous = k.borrow_mut().take().unwrap_or_default();
        let merged = merge_known(previous, scan, |id| extension_files::install_location(id).is_some_and(extension_files::is_external_location));
        let n = merged.len();
        *k.borrow_mut() = Some(merged);
        n
    });
    log_debug!("foreign: {found} installed extension(s) after startup, {merged} known ({} this session's)", found.saturating_sub(merged));
    LAST_SCAN_MS.set(sta_core::now_ms());
}

/// The new known set: everything that was already known (with its current version dirs), plus ids
/// another program registered. A new id that is not external stays unknown, so the next post-install
/// window still reports it as an install.
fn merge_known(previous: BTreeMap<String, Vec<String>>, scan: BTreeMap<String, Vec<String>>, external: impl Fn(&str) -> bool) -> BTreeMap<String, Vec<String>> {
    scan.into_iter().filter(|(id, _)| previous.contains_key(id) || external(id)).collect()
}

/// Post-install: extensions that appeared (or were rewritten) since the last scan.
fn detect_installs() {
    let Some(dir) = extension_files::extensions_dir() else { return };
    let now_scan = extension_files::scan(&dir);
    let previous = KNOWN.with(|k| k.borrow_mut().replace(now_scan.clone())).unwrap_or_default();
    let now = sta_core::now_ms();
    let last_scan = LAST_SCAN_MS.replace(now);
    let mut reported = 0;
    for (ext_id, versions) in &now_scan {
        if reported >= MAX_INSTALL_TOASTS {
            log_warn!("foreign: more than {MAX_INSTALL_TOASTS} new extensions at once; the rest are not announced");
            break;
        }
        let known = previous.get(ext_id);
        let Some(files) = versions.iter().filter_map(|v| extension_files::load_version_dir(ext_id, &dir.join(ext_id).join(v))).max_by_key(|f| f.modified_ms.unwrap_or(0))
        else {
            continue;
        };
        let fresh = files.modified_ms.is_some_and(|m| now.saturating_sub(m) <= RECENT_INSTALL_MS);
        // New since the last scan, a version set that changed just now, or a version directory
        // written after the last scan (removed and installed again within one session).
        let written_since_scan = last_scan > 0 && files.modified_ms.is_some_and(|m| m > last_scan);
        let installed = known.is_none() || (known != Some(versions) && fresh) || written_since_scan;
        if !installed {
            continue;
        }
        let external = extension_files::install_location(ext_id).is_some_and(extension_files::is_external_location);
        let name = files.name(&platform::os_ui_locale());
        reported += 1;
        stats(|s| s.installs += 1);
        event("installed", json!({ "extension": ext_id, "external": external }));
        log_info!("foreign: extension {ext_id} installed (external: {external})");
        crate::extensions::note_installed(ext_id, external);
        controller::dispatch(Command::ExtensionInstalled { id: ext_id.clone(), name, external });
    }
    if reported > 0 {
        // The listing changed. Chromium writes the preferences with a delay, so the state and
        // location of a fresh install are only there at the second read (FINAL PLAN §4: "+11 s").
        crate::extensions::refresh_soon(0);
        crate::extensions::refresh_later(crate::extensions::AFTER_INSTALL_MS);
    }
}

// --------------------------------------------------------------------------- install dialog (UX9)

const WS_CHILD: u32 = 0x4000_0000;
const WS_EX_DLGMODALFRAME: u32 = 0x0000_0001;
const WS_EX_TOOLWINDOW: u32 = 0x0000_0080;
/// The install dialog sits this far below the top of the Web Store pane (or 14 % of its height).
const DIALOG_TOP_MIN: i32 = 72;
const DIALOG_TOP_FRACTION: f32 = 0.14;

/// Chromium's "Add <extension>?" dialog is a window owned by sta's main window with a dialog frame
/// and no tool-window style (gate S17: menus, `<select>` popups, tooltips and bubbles are
/// `TYPE_MENU`/`TYPE_POPUP`/`TYPE_TOOLTIP`, which always carry `WS_EX_TOOLWINDOW` and never
/// `WS_EX_DLGMODALFRAME`, `ui/views/widget/widget_hwnd_utils.cc`). Kept as a pure function of the
/// two style words so the signature S17 measured is unit-testable (`dialog_signature_excludes_menus`).
fn is_install_dialog_style(style: u32, ex_style: u32) -> bool {
    style & WS_CHILD == 0 && ex_style & WS_EX_DLGMODALFRAME != 0 && ex_style & WS_EX_TOOLWINDOW == 0
}

fn looks_like_install_dialog(hwnd: isize) -> bool {
    let (style, ex_style) = hw::styles(hwnd);
    is_install_dialog_style(style, ex_style)
}

/// A window owned by sta's main window was shown (from the WinEvent hook: posts everything).
fn on_sta_owned_window_shown(hwnd: isize) {
    let dialog = looks_like_install_dialog(hwnd);
    log_debug!("foreign: sta-owned window {hwnd} shown (install dialog: {dialog})");
    if dialog {
        task::post_ui(move || place_install_dialog(hwnd));
    }
}

/// Top-left of a dialog of `dialog` size `[w, h]` centered over the screen-pixel rect `area`
/// (`[x, y, w, h]`), `DIALOG_TOP_MIN` DIP (or 14 % of the area) below its top, clamped to `work`.
fn install_dialog_position(area: [i32; 4], dialog: [i32; 2], work: Option<[i32; 4]>, scale: f64) -> (i32, i32) {
    let [area_x, area_y, area_w, area_h] = area;
    let top_offset = ((DIALOG_TOP_MIN as f64 * scale).round() as i32).max((area_h as f32 * DIALOG_TOP_FRACTION) as i32);
    let mut x = area_x + (area_w - dialog[0]) / 2;
    let mut y = area_y + top_offset;
    if let Some([wx, wy, ww, wh]) = work {
        x = x.clamp(wx, (wx + ww - dialog[0]).max(wx));
        y = y.clamp(wy, (wy + wh - dialog[1]).max(wy));
    }
    (x, y)
}

/// Centers the install dialog over the pane that shows the Web Store, clamped to the work area.
///
/// The dialog belongs to whichever tab started the install, which sta cannot ask Chromium about. So:
/// the focused pane when it shows the Web Store; otherwise — an install whose tab was switched away
/// from — the focused pane, or sta's content area, as long as *some* tab still shows the Web Store.
/// With no Web Store tab at all the window is left alone: the same style signature also fits sta's
/// own native dialogs (a file picker), which must never be moved.
fn place_install_dialog(hwnd: isize) {
    if !hw::is_window(hwnd) || !hw::is_visible(hwnd) {
        return;
    }
    let is_store = |url: &str| sta_core::urls::is_web_store_url(url) || is_test_gallery(url);
    let focused = controller::with_store(|s| s.focused_tab().map(|t| (t, s.tab(t).is_some_and(|tab| is_store(&tab.url))))).flatten();
    let tab = match focused {
        Some((tab, true)) => Some(tab),
        other => {
            if !crate::tabs::live_tab_urls().iter().any(|u| is_store(u)) {
                log_debug!("foreign: dialog {hwnd} left where Chromium put it (no Web Store tab open)");
                return;
            }
            other.map(|(tab, _)| tab)
        }
    };
    let Some(dialog) = hw::window_rect(hwnd) else { return };
    let main = window::hwnd_value();
    let scale = window::main_window().and_then(|w| w.display()).map(|d| d.device_scale_factor() as f64).filter(|s| *s > 0.0).unwrap_or(1.0);
    let px = |dip: i32| (dip as f64 * scale).round() as i32;
    // The pane in screen pixels, or sta's whole content area when no pane is drawn for it.
    let pane = tab.and_then(crate::tabs::tab_rect_in_window);
    let over_pane = pane.is_some();
    let area = match (pane, hw::client_origin(main)) {
        (Some(pane), Some((cx, cy))) => [cx + px(pane.x), cy + px(pane.y), px(pane.width), px(pane.height)],
        _ => match hw::client_rect(main) {
            Some(rect) => rect,
            None => return,
        },
    };
    let (x, y) = install_dialog_position(area, [dialog[2], dialog[3]], hw::work_area(main), scale);
    if hw::move_window(hwnd, x, y) {
        event("install-dialog-placed", json!({ "hwnd": hwnd, "from": dialog, "to": [x, y], "area": area, "pane": over_pane }));
        log_debug!("foreign: install dialog moved to {x},{y} (area {area:?})");
    }
}

/// Debug builds: the local gallery an e2e run passes with `--apps-gallery-url`.
fn is_test_gallery(url: &str) -> bool {
    if !cfg!(debug_assertions) {
        return false;
    }
    let Some(cl) = command_line_get_global() else { return false };
    let gallery = CefString::from(&cl.switch_value(Some(&CefString::from("apps-gallery-url")))).to_string();
    !gallery.is_empty() && url.starts_with(gallery.trim_end_matches('/'))
}

// ----------------------------------------------------------------------------------- closing

fn schedule_poll() {
    if POLL_SCHEDULED.replace(true) {
        return;
    }
    task::post_ui_delayed(CLOSE_POLL_MS, poll);
}

fn poll() {
    POLL_SCHEDULED.set(false);
    let now = Instant::now();
    let busy = downloads::in_progress_count() > 0;
    // Roots whose every browser is hidden and classified.
    let mut roots: HashMap<isize, (Instant, bool, Vec<i32>)> = HashMap::new();
    let mut waiting = false;
    ENTRIES.with(|e| {
        for (id, x) in e.borrow().iter() {
            if x.closing || x.kind == Kind::KeepNative {
                continue;
            }
            let slot = roots.entry(x.root).or_insert((x.created, true, Vec::new()));
            slot.0 = slot.0.min(x.created);
            slot.1 &= x.kind != Kind::Pending;
            slot.2.push(*id);
        }
    });
    let mut to_close = Vec::new();
    for (root, (oldest, classified, ids)) in roots {
        waiting = true;
        if !classified {
            continue;
        }
        let last_tab = ROOT_ACTIVITY.with(|r| r.borrow().get(&root).copied()).unwrap_or(oldest);
        let age = now.duration_since(oldest);
        let ready = age >= MAX_AGE || (age >= MIN_AGE && now.duration_since(last_tab) >= QUIET);
        if !ready {
            continue;
        }
        if busy {
            event("close-deferred", json!({ "root": root, "downloads": downloads::in_progress_count() }));
            continue;
        }
        to_close.extend(ids);
    }
    for id in to_close {
        close_now(id);
    }
    if waiting {
        if busy {
            downloads::notify_when_idle(schedule_poll);
        }
        schedule_poll();
    }
}

fn close_now(id: i32) {
    // The browser handle is copied out of the borrow before any CEF call (ARCHITECTURE §2.1).
    let browser = ENTRIES.with(|e| {
        let mut e = e.borrow_mut();
        let entry = e.get_mut(&id).filter(|x| !x.closing)?;
        entry.closing = true;
        Some(entry.browser.clone())
    });
    if let Some(host) = browser.and_then(|b| b.host()) {
        event("close", json!({ "id": id }));
        host.close_browser(1);
    }
}

/// Debug builds (`debug.foreign.close`): close a Chrome-created browser now, ignoring the rules
/// (gate S15: what Chromium does when its last window closes during a download).
#[cfg(debug_assertions)]
pub fn debug_close(id: i32) -> bool {
    let known = ENTRIES.with(|e| e.borrow().contains_key(&id));
    if known {
        close_now(id);
    }
    known
}

/// Shutdown: close every Chrome-created browser (native ones too).
pub fn close_all() {
    let ids: Vec<i32> = ENTRIES.with(|e| e.borrow().keys().copied().collect());
    if !ids.is_empty() {
        log_info!("shutdown: closing {} Chrome-created browser(s)", ids.len());
    }
    for id in ids {
        close_now(id);
    }
}

/// Teardown: drop every handle and the hooks.
pub fn clear() {
    let entries = ENTRIES.with(|e| std::mem::take(&mut *e.borrow_mut()));
    let client = CLIENT.with(|c| c.borrow_mut().take());
    ROOT_ACTIVITY.with(|r| r.borrow_mut().clear());
    hw::uninstall();
    drop((entries, client));
}

#[cfg_attr(not(debug_assertions), allow(dead_code))] // debug.rs only
pub fn debug_snapshot() -> Value {
    let s = STATS.get();
    let entries: Vec<Value> = ENTRIES.with(|e| {
        e.borrow()
            .iter()
            .map(|(id, x)| {
                json!({
                    "id": id,
                    "root": x.root,
                    "kind": format!("{:?}", x.kind),
                    "ageMs": x.created.elapsed().as_millis() as u64,
                    "closing": x.closing,
                    "note": x.note,
                    "window": hw::describe(x.root),
                })
            })
            .collect()
    });
    let events: Vec<Value> = EVENTS.with(|e| e.borrow().iter().cloned().collect());
    json!({
        "browsers": entries,
        "stats": {
            "created": s.created, "keptNative": s.kept_native, "postInstall": s.post_install,
            "tabRequests": s.tab_requests, "throttled": s.throttled, "incognito": s.incognito,
            "warned": s.warned, "closed": s.closed, "installs": s.installs,
        },
        "knownExtensions": KNOWN.with(|k| k.borrow().as_ref().map(|k| k.len())),
        "events": events,
        "windows": hw::snapshot(),
    })
}

// ----------------------------------------------------------------------------------- client

wrap_client! {
    struct ForeignClient {
        life_span: LifeSpanHandler,
        request: RequestHandler,
        keyboard: KeyboardHandler,
        display: DisplayHandler,
        download: DownloadHandler,
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

        fn display_handler(&self) -> Option<DisplayHandler> {
            Some(self.display.clone())
        }

        fn download_handler(&self) -> Option<DownloadHandler> {
            Some(self.download.clone())
        }
    }
}

fn is_hidden_browser(id: i32) -> bool {
    ENTRIES.with(|e| e.borrow().get(&id).is_some_and(|x| x.kind != Kind::KeepNative))
}

wrap_life_span_handler! {
    struct ForeignLifeSpan;

    impl LifeSpanHandler {
        fn on_before_popup(
            &self,
            browser: Option<&mut Browser>,
            _frame: Option<&mut Frame>,
            _popup_id: i32,
            _target_url: Option<&CefString>,
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
            // Hidden browsers never load a page; native windows (sign-in flows) keep Chromium's popups.
            browser.is_some_and(|b| is_hidden_browser(b.identifier())) as i32
        }

        fn on_after_created(&self, browser: Option<&mut Browser>) {
            if let Some(browser) = browser {
                on_after_created(browser);
            }
        }

        fn do_close(&self, _browser: Option<&mut Browser>) -> i32 {
            0 // Chromium closes its own tab / window
        }

        fn on_before_close(&self, browser: Option<&mut Browser>) {
            if let Some(browser) = browser {
                on_before_close(browser);
            }
        }
    }
}

wrap_request_handler! {
    struct ForeignRequest;

    impl RequestHandler {
        fn on_before_browse(
            &self,
            browser: Option<&mut Browser>,
            frame: Option<&mut Frame>,
            request: Option<&mut Request>,
            _user_gesture: i32,
            _is_redirect: i32,
        ) -> i32 {
            let (Some(browser), Some(frame)) = (browser, frame) else { return 0 };
            let url = request.map(|r| CefString::from(&r.url()).to_string()).unwrap_or_default();
            if frame.is_main() == 0 {
                // Subframes of hidden browsers never load either (their main frame never commits).
                return is_hidden_browser(browser.identifier()) as i32;
            }
            on_main_frame_navigation(browser, &url) as i32
        }
    }
}

wrap_keyboard_handler! {
    struct ForeignKeyboard;

    impl KeyboardHandler {
        fn on_pre_key_event(
            &self,
            browser: Option<&mut Browser>,
            _event: Option<&KeyEvent>,
            _os_event: Option<&mut MSG>,
            _is_keyboard_shortcut: Option<&mut i32>,
        ) -> i32 {
            // A hidden window never takes typing (the hooks already refuse its activation).
            browser.is_some_and(|b| is_hidden_browser(b.identifier())) as i32
        }
    }
}

wrap_display_handler! {
    struct ForeignDisplay;

    impl DisplayHandler {
        fn on_console_message(&self, browser: Option<&mut Browser>, _level: LogSeverity, _message: Option<&CefString>, _source: Option<&CefString>, _line: i32) -> i32 {
            // Pages of hidden windows never run; native windows keep Chromium's logging.
            browser.is_some_and(|b| is_hidden_browser(b.identifier())) as i32
        }
    }
}

wrap_download_handler! {
    struct ForeignDownload;

    impl DownloadHandler {
        fn can_download(&self, _browser: Option<&mut Browser>, _url: Option<&CefString>, _request_method: Option<&CefString>) -> i32 {
            1
        }

        fn on_before_download(
            &self,
            browser: Option<&mut Browser>,
            download_item: Option<&mut DownloadItem>,
            suggested_name: Option<&CefString>,
            callback: Option<&mut BeforeDownloadCallback>,
        ) -> i32 {
            let (Some(item), Some(callback)) = (download_item, callback) else { return 0 };
            let id = browser.map(|b| b.identifier()).unwrap_or(0);
            let name = suggested_name.map(CefString::to_string).unwrap_or_default();
            downloads::on_before_download(id, item, &name, callback) as i32
        }

        fn on_download_updated(&self, browser: Option<&mut Browser>, download_item: Option<&mut DownloadItem>, callback: Option<&mut DownloadItemCallback>) {
            let Some(item) = download_item else { return };
            let id = browser.map(|b| b.identifier()).unwrap_or(0);
            downloads::on_download_updated(id, item, callback.cloned());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_for_logs_and_ntp() {
        assert_eq!(log_url("https://getadblock.com/installed/?u=secret"), "https://getadblock.com");
        assert_eq!(log_url("chrome-extension://abcdefghijklmnopabcdefghijklmnop/opts.html"), "chrome-extension://abcdefghijklmnopabcdefghijklmnop");
        assert_eq!(log_url("data:text/html,secret"), "data:");
        assert!(is_ntp("chrome://new-tab-page/") && is_ntp("CHROME://NEWTAB/") && !is_ntp("chrome://settings/"));
    }

    /// The startup re-scan absorbs what other programs registered and nothing else: an install that
    /// lands in the first seconds of a run must still be reported by the post-install window.
    #[test]
    fn the_startup_rescan_only_absorbs_extensions_other_programs_added() {
        let map = |pairs: &[(&str, &str)]| -> BTreeMap<String, Vec<String>> { pairs.iter().map(|(id, v)| ((*id).to_string(), vec![(*v).to_string()])).collect() };
        let previous = map(&[("known", "1.0_0")]);
        let scan = map(&[("known", "1.1_0"), ("installed-now", "2.0_0"), ("from-the-registry", "3.0_0")]);
        let merged = merge_known(previous, scan, |id| id == "from-the-registry");
        assert_eq!(merged, map(&[("known", "1.1_0"), ("from-the-registry", "3.0_0")]));
        assert!(!merged.contains_key("installed-now"), "this session's install must stay unknown: {merged:?}");
        // Nothing new: the set is just the current directory.
        let same = map(&[("known", "1.0_0")]);
        assert_eq!(merge_known(same.clone(), same.clone(), |_| false), same);
    }

    /// Gate S17's window words: the install dialog is accepted, menus, tooltips and child windows are
    /// not (`is_install_dialog_style` is the only thing that decides whether sta moves a window).
    #[test]
    fn dialog_signature_excludes_menus() {
        // As recorded in gates-p1.md for the real "Add extension?" dialog.
        assert!(is_install_dialog_style(0x8600_0080, 0x0020_0101));
        // A views menu / tooltip / bubble: WS_EX_TOOLWINDOW, no dialog frame.
        assert!(!is_install_dialog_style(0x9600_0000, 0x0800_0088));
        assert!(!is_install_dialog_style(0x9600_0000, WS_EX_TOOLWINDOW | WS_EX_DLGMODALFRAME));
        // A child window is never a dialog, whatever its extended styles say.
        assert!(!is_install_dialog_style(WS_CHILD | 0x0600_0000, WS_EX_DLGMODALFRAME));
        // No dialog frame: an ordinary top-level window (a kept-native extension popup).
        assert!(!is_install_dialog_style(0x8600_0080, 0x0000_0100));
    }

    #[test]
    fn the_install_dialog_is_centered_below_the_top_of_the_pane() {
        let work = Some([0, 0, 2560, 1440]);
        // The pane of gate S5's run: 14 % of 820 px wins over the 72 DIP minimum.
        assert_eq!(install_dialog_position([640, 310, 1280, 820], [462, 240], work, 1.0), (1049, 424));
        // A short pane: the 72 DIP minimum wins, and it scales with the display.
        assert_eq!(install_dialog_position([0, 0, 400, 300], [462, 240], work, 1.0), (0, 72));
        assert_eq!(install_dialog_position([0, 0, 400, 300], [462, 240], work, 1.5), (0, 108));
        // Clamped to the work area on both sides.
        assert_eq!(install_dialog_position([-500, -200, 1280, 820], [462, 240], work, 1.0), (0, 0));
        assert_eq!(install_dialog_position([2400, 1300, 1280, 820], [462, 240], work, 1.0), (2098, 1200));
        // A work area smaller than the dialog still gives its top-left corner.
        assert_eq!(install_dialog_position([100, 100, 400, 300], [462, 240], Some([0, 0, 300, 100]), 1.0), (0, 0));
        // No work area: nothing is clamped.
        assert_eq!(install_dialog_position([-500, -200, 1280, 820], [462, 240], None, 1.0), (-91, -86));
    }
}
