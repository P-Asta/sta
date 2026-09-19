//! macOS integration [owner: chrome] (docs/research/platform.md §10).
//!
//! The Cocoa half of the [`super`] facade: everything `win.rs` does with Win32, done with AppKit.
//! Every entry point here either runs on the main thread (the CEF UI thread) or says how it gets
//! there — AppKit is main-thread-only, and `MainThreadMarker::new()` returning `None` is what a
//! call from a worker thread looks like.
//!
//! Beyond the facade it owns the bootstrap CEF requires on this platform ([`init_application`]):
//! `NSApp` must be an `NSApplication` subclass that implements `CefAppProtocol`, or `cef_initialize`
//! aborts. [`StaApplication`] is that subclass; it tracks `handlingSendEvent` the way Chromium's own
//! `CrApplication` does, in a main-thread cell (the class is instantiated by AppKit itself, through
//! `+sharedApplication`, so it can own no Rust-initialized instance variables).
//!
//! Window handles are `NSView*` here (`super::handle_value`), not `HWND`: CEF hands out the view it
//! hosts its content in, so anything that wants the window goes through `[view window]`.
//!
//! What has no macOS counterpart yet, and stays inert (`super::hidden_windows`, the `input` test
//! hooks): the Chrome-created-window plumbing and the real-keyboard e2e helpers, both written
//! against Win32 messages. docs/STATUS.md tracks that.

use cef::application_mac::{CefAppProtocol, CrAppControlProtocol, CrAppProtocol};
use objc2::rc::{Retained, autoreleasepool};
use objc2::runtime::{AnyObject, Bool, NSObject, NSObjectProtocol, ProtocolObject, Sel};
use objc2::{AnyThread, ClassType, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{
    NSAlert, NSApplication, NSApplicationActivationPolicy, NSApplicationDelegate, NSApplicationTerminateReply, NSAppearance,
    NSAppearanceCustomization, NSAppearanceNameAqua, NSAppearanceNameDarkAqua, NSEvent, NSEventModifierFlags, NSMenu, NSMenuItem, NSModalResponse,
    NSOpenPanel, NSPasteboard, NSPasteboardTypeString, NSRequestUserAttentionType, NSScreen, NSView, NSWindow, NSWorkspace,
    NSWorkspaceAccessibilityDisplayOptionsDidChangeNotification,
};
use objc2_foundation::{
    MainThreadMarker, NSArray, NSDistributedNotificationCenter, NSLocale, NSPoint, NSString, NSURL, NSUserDefaults, ns_string,
};
use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};

// --------------------------------------------------------------------------- the NSApplication CEF needs

thread_local! {
    /// `CrAppProtocol`'s `handlingSendEvent` (main thread only, like `NSApp` itself).
    static HANDLING_SEND_EVENT: Cell<bool> = const { Cell::new(false) };
}

define_class!(
    // SAFETY:
    // - `NSApplication` has no subclassing requirements beyond being used on the main thread.
    // - No `Drop` impl, and no instance variables: `+sharedApplication` allocates this class
    //   itself, so there is no point where Rust could initialize any.
    #[unsafe(super(NSApplication))]
    #[thread_kind = MainThreadOnly]
    #[name = "StaApplication"]
    pub struct StaApplication;

    impl StaApplication {
        /// Chromium's `CrApplication` contract: code that runs *inside* `-sendEvent:` must be able
        /// to tell (CEF asks before it dispatches work of its own).
        #[unsafe(method(sendEvent:))]
        fn send_event(&self, event: &NSEvent) {
            let previous = HANDLING_SEND_EVENT.replace(true);
            // SAFETY: forwarding the selector to `NSApplication` with its own argument.
            unsafe { msg_send![super(self), sendEvent: event] }
            HANDLING_SEND_EVENT.set(previous);
        }
    }

    unsafe impl NSObjectProtocol for StaApplication {}

    unsafe impl CrAppProtocol for StaApplication {
        #[unsafe(method(isHandlingSendEvent))]
        unsafe fn is_handling_send_event(&self) -> Bool {
            Bool::new(HANDLING_SEND_EVENT.get())
        }
    }

    unsafe impl CrAppControlProtocol for StaApplication {
        #[unsafe(method(setHandlingSendEvent:))]
        unsafe fn set_handling_send_event(&self, handling_send_event: Bool) {
            HANDLING_SEND_EVENT.set(handling_send_event.as_bool());
        }
    }

    unsafe impl CefAppProtocol for StaApplication {}
);

define_class!(
    // SAFETY: plain `NSObject` subclass, no ivars, no `Drop`.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "StaAppDelegate"]
    struct AppDelegate;

    unsafe impl NSObjectProtocol for AppDelegate {}

    unsafe impl NSApplicationDelegate for AppDelegate {
        /// ⌘Q, the Dock's Quit and a logout all end here. sta closes the way its own close button
        /// does — saving the session and shutting CEF down — instead of letting AppKit stop the
        /// process where it stands, so the answer is always "not yet".
        #[unsafe(method(applicationShouldTerminate:))]
        fn should_terminate(&self, _sender: &NSApplication) -> NSApplicationTerminateReply {
            match QUIT_REQUESTED.with(Cell::get) {
                Some(quit) => {
                    quit();
                    NSApplicationTerminateReply::TerminateCancel
                }
                // Before the browser is up there is nothing to save.
                None => NSApplicationTerminateReply::TerminateNow,
            }
        }
    }
);

thread_local! {
    static QUIT_REQUESTED: Cell<Option<fn()>> = const { Cell::new(None) };
    static APP_DELEGATE: RefCell<Option<Retained<AppDelegate>>> = const { RefCell::new(None) };
}

/// What the app does when macOS asks it to quit (`app.rs` points this at the shell's close
/// sequence once there is a window to close).
pub fn set_quit_handler(quit: fn()) {
    QUIT_REQUESTED.with(|slot| slot.set(Some(quit)));
}

/// Creates `NSApp` as [`StaApplication`], makes this a regular, activatable app, and gives it the
/// menu bar every macOS app is expected to have.
///
/// **Must run on the main thread before `cef::initialize`** (and before anything else touches
/// `NSApp`): `+sharedApplication` creates the singleton from the class it is sent to, so whoever
/// asks first decides what `NSApp` is. `cef_initialize` then checks that it implements
/// `CefAppProtocol`.
pub fn init_application() {
    let Some(mtm) = MainThreadMarker::new() else {
        eprintln!("[sta] init_application must run on the main thread");
        return;
    };
    // SAFETY: `+sharedApplication` on our subclass, on the main thread. The instance AppKit
    // creates (and keeps as `NSApp`) is a `StaApplication`.
    let app: Retained<StaApplication> = unsafe { msg_send![StaApplication::class(), sharedApplication] };
    app.setActivationPolicy(NSApplicationActivationPolicy::Regular);

    let delegate: Retained<AppDelegate> = unsafe { msg_send![AppDelegate::alloc(mtm), init] };
    app.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
    // `setDelegate:` does not retain: the delegate has to outlive this call.
    APP_DELEGATE.with(|slot| *slot.borrow_mut() = Some(delegate));
    install_main_menu(&app, mtm);
}

/// The menu bar. It is short on purpose: sta's own shortcuts are not menu items (they are matched
/// in `keyboard.rs`), and a menu item's key equivalent would swallow the key before the page or the
/// shell ever saw it. What is here is what macOS itself expects to find — the application menu, and
/// the editing commands whose `cut:`/`copy:`/`paste:` actions Chromium's views implement.
fn install_main_menu(app: &NSApplication, mtm: MainThreadMarker) {
    let menubar = NSMenu::new(mtm);
    let item = |title: &str, action: Sel, key: &str, modifiers: Option<NSEventModifierFlags>| {
        // SAFETY: a standard responder-chain selector with no target, and a key equivalent string.
        let item = unsafe {
            NSMenuItem::initWithTitle_action_keyEquivalent(NSMenuItem::alloc(mtm), &NSString::from_str(title), Some(action), &NSString::from_str(key))
        };
        if let Some(modifiers) = modifiers {
            item.setKeyEquivalentModifierMask(modifiers);
        }
        item
    };
    let submenu = |title: &str, items: Vec<Retained<NSMenuItem>>| {
        let menu = NSMenu::initWithTitle(NSMenu::alloc(mtm), &NSString::from_str(title));
        for entry in items {
            menu.addItem(&entry);
        }
        let holder = NSMenuItem::new(mtm);
        holder.setSubmenu(Some(&menu));
        menubar.addItem(&holder);
        menu
    };
    let command = NSEventModifierFlags::Command;

    // The first submenu is the application menu, whatever it is called; macOS titles it after the
    // bundle name.
    submenu(
        "sta",
        vec![
            item("About sta", objc2::sel!(orderFrontStandardAboutPanel:), "", None),
            NSMenuItem::separatorItem(mtm),
            item("Hide sta", objc2::sel!(hide:), "h", None),
            item("Hide Others", objc2::sel!(hideOtherApplications:), "h", Some(command | NSEventModifierFlags::Option)),
            item("Show All", objc2::sel!(unhideAllApplications:), "", None),
            NSMenuItem::separatorItem(mtm),
            // Through `applicationShouldTerminate:` above, not straight out of the process.
            item("Quit sta", objc2::sel!(terminate:), "q", None),
        ],
    );
    submenu(
        "Edit",
        vec![
            item("Undo", objc2::sel!(undo:), "z", None),
            item("Redo", objc2::sel!(redo:), "z", Some(command | NSEventModifierFlags::Shift)),
            NSMenuItem::separatorItem(mtm),
            item("Cut", objc2::sel!(cut:), "x", None),
            item("Copy", objc2::sel!(copy:), "c", None),
            item("Paste", objc2::sel!(paste:), "v", None),
            item("Select All", objc2::sel!(selectAll:), "a", None),
        ],
    );
    // No ⌘W here: closing is sta's own shortcut (a tab, not the window), and a menu item would take
    // the key first.
    let windows = submenu("Window", vec![item("Minimize", objc2::sel!(performMiniaturize:), "m", None), item("Zoom", objc2::sel!(performZoom:), "", None)]);
    app.setWindowsMenu(Some(&windows));
    app.setMainMenu(Some(&menubar));
}

// --------------------------------------------------------------------------- system settings

/// The OS app appearance. `AppleInterfaceStyle` exists (as "Dark") only in dark mode.
pub fn system_dark_mode() -> bool {
    autoreleasepool(|_| {
        NSUserDefaults::standardUserDefaults()
            .stringForKey(ns_string!("AppleInterfaceStyle"))
            .is_some_and(|style| style.to_string().eq_ignore_ascii_case("dark"))
    })
}

/// Accessibility → Display → "Reduce motion" (the macOS counterpart of Windows' animation
/// effects). `true` = animate.
pub fn system_animations() -> bool {
    !NSWorkspace::sharedWorkspace().accessibilityDisplayShouldReduceMotion()
}

define_class!(
    // SAFETY: plain `NSObject` subclass, no ivars, no `Drop`.
    #[unsafe(super(NSObject))]
    #[name = "StaSettingObserver"]
    struct SettingObserver;

    impl SettingObserver {
        #[unsafe(method(staSettingChanged:))]
        fn setting_changed(&self, _notification: *mut AnyObject) {
            if let Some(f) = SETTING_CHANGED.with(|f| f.get()) {
                f();
            }
        }
    }

    unsafe impl NSObjectProtocol for SettingObserver {}
);

thread_local! {
    static SETTING_CHANGED: Cell<Option<fn()>> = const { Cell::new(None) };
    static SETTING_OBSERVER: RefCell<Option<Retained<SettingObserver>>> = const { RefCell::new(None) };
}

/// Calls `f` on this thread when the OS theme or the reduce-motion setting changes (`window.rs`
/// re-reads both). Theme changes arrive as a distributed notification, reduce-motion through the
/// workspace notification center; both are delivered on the thread that registered, so this must
/// run on the UI thread and `f` runs there too.
pub fn watch_setting_change(f: fn()) {
    let Some(_mtm) = MainThreadMarker::new() else { return };
    SETTING_CHANGED.with(|slot| slot.set(Some(f)));
    SETTING_OBSERVER.with(|slot| {
        let mut slot = slot.borrow_mut();
        if slot.is_some() {
            return;
        }
        let observer: Retained<SettingObserver> = unsafe { msg_send![SettingObserver::alloc(), init] };
        let selector = objc2::sel!(staSettingChanged:);
        // SAFETY: the observer outlives the registration (it is released in
        // `unwatch_setting_change`, which removes it first), and the selector takes the
        // notification AppKit passes.
        unsafe {
            NSDistributedNotificationCenter::defaultCenter().addObserver_selector_name_object(
                &observer,
                selector,
                Some(ns_string!("AppleInterfaceThemeChangedNotification")),
                None,
            );
            NSWorkspace::sharedWorkspace().notificationCenter().addObserver_selector_name_object(
                &observer,
                selector,
                Some(NSWorkspaceAccessibilityDisplayOptionsDidChangeNotification),
                None,
            );
        }
        *slot = Some(observer);
    });
}

pub fn unwatch_setting_change() {
    SETTING_CHANGED.with(|slot| slot.set(None));
    SETTING_OBSERVER.with(|slot| {
        if let Some(observer) = slot.borrow_mut().take() {
            // SAFETY: removing the registrations made above, on the same thread.
            unsafe {
                NSDistributedNotificationCenter::defaultCenter().removeObserver(&observer);
                NSWorkspace::sharedWorkspace().notificationCenter().removeObserver(&observer);
            }
        }
    });
}

// --------------------------------------------------------------------------- windows

/// The `NSWindow` behind a handle the shell passes around (`NSView*`).
fn window_of(handle: isize) -> Option<Retained<NSWindow>> {
    if handle == 0 {
        return None;
    }
    // SAFETY: the handle comes from `Window::window_handle` (CEF's hosting `NSView`) and is only
    // used while that window lives (UI thread).
    let view: &NSView = unsafe { &*(handle as *const NSView) };
    view.window()
}

/// Gives the window the OS appearance matching sta's theme, so the pieces AppKit draws itself
/// (scrollbars, menus, the traffic lights' hover art, native dialogs) match the chrome. The
/// Windows counterpart sets the DWM dark title bar and round corners; macOS rounds windows itself.
pub fn apply_window_chrome(handle: isize, dark: bool) {
    if MainThreadMarker::new().is_none() {
        return;
    }
    let Some(window) = window_of(handle) else { return };
    let name = if dark { unsafe { NSAppearanceNameDarkAqua } } else { unsafe { NSAppearanceNameAqua } };
    // `None` means "inherit", which is the right answer for an appearance macOS doesn't know.
    window.setAppearance(NSAppearance::appearanceNamed(name).as_deref());
}

/// Bounces the Dock icon until sta is activated — what an agent's approval prompt does to ask for
/// the user's attention (Windows flashes the taskbar button). `false` = sta is already active, so
/// nothing was asked.
pub fn request_attention() -> bool {
    let Some(mtm) = MainThreadMarker::new() else { return false };
    let app = NSApplication::sharedApplication(mtm);
    if app.isActive() {
        return false;
    }
    app.requestUserAttention(NSRequestUserAttentionType::CriticalRequest);
    true
}

// --------------------------------------------------------------------------- clipboard, shell, dialogs

pub fn set_clipboard_text(text: &str) -> bool {
    autoreleasepool(|_| {
        let pasteboard = NSPasteboard::generalPasteboard();
        pasteboard.clearContents();
        unsafe { pasteboard.setString_forType(&NSString::from_str(text), NSPasteboardTypeString) }
    })
}

/// A file path or a URL, opened by whatever the user set as its handler (`external.rs` sends
/// non-web protocols here, `downloads.rs` finished files).
pub fn shell_open(path: &str) -> bool {
    autoreleasepool(|_| {
        match url_for(path) {
            Some(url) => NSWorkspace::sharedWorkspace().openURL(&url),
            None => false,
        }
    })
}

/// Reveals the file in Finder with it selected.
pub fn show_in_folder(path: &str) -> bool {
    autoreleasepool(|_| {
        let url = NSURL::fileURLWithPath(&NSString::from_str(path));
        let urls: Retained<NSArray<NSURL>> = NSArray::from_retained_slice(&[url]);
        NSWorkspace::sharedWorkspace().activateFileViewerSelectingURLs(&urls);
        true
    })
}

/// `file:` URL for an existing path, else the text parsed as a URL.
fn url_for(path: &str) -> Option<Retained<NSURL>> {
    let text = NSString::from_str(path);
    if Path::new(path).exists() {
        return Some(NSURL::fileURLWithPath(&text));
    }
    NSURL::URLWithString(&text)
}

/// The folder picker behind `dialog.pickFolder`. **Blocking**, and `NSOpenPanel` is main-thread
/// only, so the call `ipc.rs` makes from its picker thread hops to the main thread and waits for
/// the panel there (the panel's own modal loop keeps the app responsive, as it does for every
/// other Cocoa app; Chromium's message pump runs in the modal run loop mode too).
pub fn pick_folder(_owner_handle: isize, title: &str, initial_dir: Option<&str>) -> Option<String> {
    if MainThreadMarker::new().is_some() {
        return run_folder_panel(title, initial_dir);
    }
    let (title, initial_dir) = (title.to_string(), initial_dir.map(str::to_string));
    let (tx, rx) = std::sync::mpsc::channel();
    dispatch2::DispatchQueue::main().exec_async(move || {
        let _ = tx.send(run_folder_panel(&title, initial_dir.as_deref()));
    });
    rx.recv().ok().flatten()
}

fn run_folder_panel(title: &str, initial_dir: Option<&str>) -> Option<String> {
    const NS_MODAL_RESPONSE_OK: NSModalResponse = 1;
    let mtm = MainThreadMarker::new()?;
    autoreleasepool(|_| {
        let panel = NSOpenPanel::openPanel(mtm);
        panel.setCanChooseDirectories(true);
        panel.setCanChooseFiles(false);
        panel.setAllowsMultipleSelection(false);
        panel.setMessage(Some(&NSString::from_str(title)));
        if let Some(dir) = initial_dir.filter(|d| !d.is_empty()) {
            panel.setDirectoryURL(Some(&NSURL::fileURLWithPath(&NSString::from_str(dir))));
        }
        if panel.runModal() != NS_MODAL_RESPONSE_OK {
            return None;
        }
        let url = panel.URLs().iter().next()?;
        url.path().map(|p| p.to_string())
    })
}

/// Fatal startup errors, before there is a window or a log file.
pub fn show_error_box(title: &str, message: &str) {
    eprintln!("[{title}] {message}");
    let Some(mtm) = MainThreadMarker::new() else { return };
    autoreleasepool(|_| {
        let alert = NSAlert::new(mtm);
        alert.setMessageText(&NSString::from_str(title));
        alert.setInformativeText(&NSString::from_str(message));
        alert.runModal();
    });
}

// --------------------------------------------------------------------------- files

/// There is no .msi on macOS: the app bundle is what is installed, and it is replaced whole.
pub fn msi_install_dir() -> Option<PathBuf> {
    None
}

/// Not asked on macOS (no `statvfs` without a new dependency): the low-disk notice is Windows-only.
pub fn free_disk_bytes(_dir: &Path) -> Option<u64> {
    None
}

/// A directory rename that never replaces an existing target (`rename(2)` would replace an empty
/// one, and merge nothing into a non-empty one).
pub fn move_dir(from: &Path, to: &Path) -> std::io::Result<()> {
    if to.exists() {
        return Err(std::io::Error::from(std::io::ErrorKind::AlreadyExists));
    }
    std::fs::rename(from, to)
}

/// A file held open for as long as the returned handle lives, which another process can detect:
/// the Windows version relies on share modes, this one on an exclusive `flock`. A second caller
/// (this process or another) gets `WouldBlock` while it is held.
pub fn hold_lock_file(path: &Path) -> std::io::Result<std::fs::File> {
    const LOCK_EX: i32 = 2;
    const LOCK_NB: i32 = 4;
    unsafe extern "C" {
        fn flock(fd: i32, operation: i32) -> i32;
    }
    use std::os::fd::AsRawFd;

    let file = std::fs::OpenOptions::new().write(true).create(true).truncate(true).open(path)?;
    // SAFETY: a live descriptor of the file just opened; the lock is released when it closes.
    if unsafe { flock(file.as_raw_fd(), LOCK_EX | LOCK_NB) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(file)
}

/// The user's preferred languages, most preferred first (`en-US`, …), for `Settings.locale`.
pub fn os_ui_languages() -> Vec<String> {
    autoreleasepool(|_| NSLocale::preferredLanguages().iter().map(|l| l.to_string()).collect())
}

pub fn downloads_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|home| PathBuf::from(home).join("Downloads"))
}

/// Windows-only (`--console` re-attaches a detached GUI process to its console). A macOS binary
/// started from a terminal already has its stdio.
pub fn attach_parent_console() {}

/// SHA-256 through CommonCrypto (in libSystem, always linked) — extension ids.
pub fn sha256(data: &[u8]) -> Option<[u8; 32]> {
    unsafe extern "C" {
        fn CC_SHA256(data: *const std::ffi::c_void, len: u32, md: *mut u8) -> *mut u8;
    }
    let len = u32::try_from(data.len()).ok()?;
    let mut out = [0u8; 32];
    // SAFETY: `data` is `len` bytes and `out` is the 32 bytes CC_SHA256 writes.
    let ok = unsafe { CC_SHA256(data.as_ptr().cast(), len, out.as_mut_ptr()) };
    ok.is_null().then_some(()).map_or(Some(out), |()| None)
}

// --------------------------------------------------------------------------- the cursor poll

thread_local! {
    /// Buttons held at the previous sample, for `clicked` (AppKit has no "pressed since you last
    /// asked" bit like `GetAsyncKeyState`).
    static PREVIOUS_BUTTONS: Cell<bool> = const { Cell::new(false) };
}

/// Where the pointer is, in physical pixels with the origin at the top left of the primary
/// display — `sidebar_hover.rs` subtracts `client_*` and divides by the window's scale factor, so
/// this must be in the same units as CEF's device scale (points × backing scale).
pub fn cursor_sample(handle: isize) -> Option<super::CursorSample> {
    let mtm = MainThreadMarker::new()?;
    let window = window_of(handle)?;
    let scale = window.backingScaleFactor();
    // Cocoa screen coordinates start at the bottom left of the primary screen (the first one).
    let flip_height = NSScreen::screens(mtm).iter().next().map(|s| s.frame().size.height)?;
    let to_px = |value: f64| (value * scale).round() as i32;

    let point = NSEvent::mouseLocation();
    let content = window.contentRectForFrameRect(window.frame());
    let buttons = NSEvent::pressedMouseButtons() != 0;
    let clicked = buttons && !PREVIOUS_BUTTONS.replace(buttons);

    let (over_window, owned_popup) = window_under(point, &window, mtm);
    Some(super::CursorSample {
        x: to_px(point.x),
        y: to_px(flip_height - point.y),
        client_left: to_px(content.origin.x),
        client_top: to_px(flip_height - (content.origin.y + content.size.height)),
        over_window,
        owned_popup,
        buttons,
        clicked,
    })
}

/// `(the window under the pointer is ours, it is a window ours owns)` — a select list, a menu or
/// any other panel attached to the main window counts as "still over us" for the hover reveal.
fn window_under(point: NSPoint, ours: &NSWindow, mtm: MainThreadMarker) -> (bool, bool) {
    let number = NSWindow::windowNumberAtPoint_belowWindowWithWindowNumber(point, 0, mtm);
    if number == ours.windowNumber() {
        return (true, false);
    }
    let app = NSApplication::sharedApplication(mtm);
    let Some(under) = app.windowWithWindowNumber(number) else { return (false, false) };
    let owned = std::iter::successors(under.parentWindow(), |w| w.parentWindow()).any(|w| w.windowNumber() == ours.windowNumber());
    (owned, owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_matches_a_known_vector() {
        // SHA-256("abc")
        let want = [
            0xbau8, 0x78, 0x16, 0xbf, 0x8f, 0x01, 0xcf, 0xea, 0x41, 0x41, 0x40, 0xde, 0x5d, 0xae, 0x22, 0x23, 0xb0, 0x03, 0x61, 0xa3, 0x96, 0x17,
            0x7a, 0x9c, 0xb4, 0x10, 0xff, 0x61, 0xf2, 0x00, 0x15, 0xad,
        ];
        assert_eq!(sha256(b"abc"), Some(want));
    }

    #[test]
    fn a_held_lock_file_is_held_once() {
        let dir = std::env::temp_dir().join(format!("sta-lock-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("held.lock");
        let held = hold_lock_file(&path).unwrap();
        assert!(hold_lock_file(&path).is_err(), "held by this process");
        drop(held);
        assert!(hold_lock_file(&path).is_ok(), "free again");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
