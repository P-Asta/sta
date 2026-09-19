//! Keyboard shortcuts [owner: chrome] (ARCHITECTURE §4.2, arc_spec §3).
//!
//! Responsibility:
//! - the accelerator table (one `Window::set_accelerator` command id per key combo — registering
//!   an id twice replaces the first combo); `on_accelerator` maps id → `Command` and enqueues it;
//! - `KeyboardHandler::on_pre_key_event` (installed on every client):
//!   - Esc chain (unmodified Esc, in this order): exit page fullscreen → close the find bar if it
//!     has focus → **close the extension popup card** (UX8 calls it "first in the chain"; leaving
//!     page fullscreen still wins, because a fullscreen page is what Esc means there) → close Peek
//!     → close find bar → cancel the switcher → close the command bar →
//!     close a transient sidebar panel (downloads, app menu; not the space sheets, an inline
//!     rename or the edit pinned page panel) unless the sidebar has focus → hide the floating
//!     sidebar (hover reveal, not pinned by a panel; the key still reaches the page). The command
//!     bar and sidebar pages handle their own Esc;
//!   - Ctrl key-up after a `MruStep` (or while the switcher is requested) → `MruCommit`; also
//!     handled in `WindowDelegate::on_key_event` for keys no browser saw;
//!   - a debugger key the docked DevTools frontend asked for (`setWhitelistedShortcuts`) is handed
//!     to that frontend while the page has focus (F13/UX5, `devtools::forwarded_key`);
//! - three accelerators are resolved against what has focus before they are dispatched
//!   ([`on_accelerator`]), because only the shell knows it: **Ctrl+Shift+I** closes DevTools when
//!   the frontend itself has focus and otherwise opens/focuses them (UX17), plain **F11** is
//!   left unhandled while the frontend has focus, so the key reaches DevTools as "step out" instead
//!   of toggling fullscreen (D9), and **Ctrl+Shift+C** starts DevTools' element picker while the
//!   focused pane has a dock (Chrome's chord) instead of copying the URL;
//! - accelerator/fallback bookkeeping so every combo dispatches exactly once.
//!
//! - `KeyboardHandler::on_key_event` (every client): a key the page did not handle comes back here
//!   before Views matches it against the normal-priority accelerators. A key **injected through the
//!   DevTools protocol** (`Input.dispatchKeyEvent`, which is what an AI agent's `key` tool sends)
//!   carries no OS message, and is consumed here: otherwise an agent could press Ctrl+W, Ctrl+T or
//!   F12 and drive the browser itself, which is exactly what agent/keys.rs promises it cannot
//!   (R-SEC-5). Injected keys still reach the *page* — only sta's own shortcuts are out of reach.
//!
//! Public API:
//! - `pub struct Binding { key, shift, ctrl, alt, high_priority, command }`
//! - `pub fn bindings() -> &'static [Binding]` (index + `FIRST_COMMAND_ID` = command id)
//! - `pub fn install_accelerators(window: &Window)`
//! - `pub fn on_accelerator(command_id: i32) -> bool`
//! - `pub fn on_pre_key_event(browser_id: i32, event: &KeyEvent) -> bool` — `true` = consumed
//! - `pub fn on_key_event(browser_id: i32, event: &KeyEvent, native: bool) -> bool` — ditto, for keys
//!   the page left unhandled (`native == false` = injected through the protocol)
//! - `pub fn on_window_key_event(event: &KeyEvent) -> bool` — keys no view handled
//! - `pub fn is_devtools_toggle(event: &KeyEvent) -> bool` — F12 / Ctrl+Shift+I (DevTools window client)
//! - `pub fn find_binding(key: i32, shift: bool, ctrl: bool, alt: bool) -> Option<i32>` (tests/debug)
//! - `pub fn debug_snapshot() -> serde_json::Value`

use crate::browsers::{self, Role, Surface};
use crate::overlays::{self, Overlay};
use crate::{controller, sidebar_hover, tabs, window};
use sta_core::{Command, CommandBarMode, InternalPage, SidebarPanel, WindowAction, ZoomDirection};
use cef::*;
use std::cell::Cell;
use std::sync::OnceLock;

/// Command id of `bindings()[0]`.
pub const FIRST_COMMAND_ID: i32 = 1000;

// Windows virtual-key codes.
pub mod vk {
    pub const TAB: i32 = 0x09;
    pub const CONTROL: i32 = 0x11;
    pub const ESCAPE: i32 = 0x1B;
    pub const PRIOR: i32 = 0x21;
    pub const NEXT: i32 = 0x22;
    pub const LEFT: i32 = 0x25;
    pub const UP: i32 = 0x26;
    pub const RIGHT: i32 = 0x27;
    pub const DOWN: i32 = 0x28;
    pub const KEY_0: i32 = 0x30;
    pub const NUMPAD0: i32 = 0x60;
    pub const ADD: i32 = 0x6B;
    pub const SUBTRACT: i32 = 0x6D;
    pub const F3: i32 = 0x72;
    pub const F4: i32 = 0x73;
    pub const F5: i32 = 0x74;
    pub const F6: i32 = 0x75;
    pub const F11: i32 = 0x7A;
    pub const F12: i32 = 0x7B;
    pub const OEM_PLUS: i32 = 0xBB;
    pub const OEM_COMMA: i32 = 0xBC;
    pub const OEM_MINUS: i32 = 0xBD;
    pub const OEM_4: i32 = 0xDB; // [
    pub const OEM_6: i32 = 0xDD; // ]

    pub const fn letter(c: char) -> i32 {
        c as i32
    }
}

pub struct Binding {
    pub key: i32,
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
    /// Reserved: the page never sees the key first.
    pub high_priority: bool,
    pub command: Command,
}

fn b(key: i32, mods: &str, high_priority: bool, command: Command) -> Binding {
    Binding {
        key,
        shift: mods.contains('S'),
        ctrl: mods.contains('C'),
        alt: mods.contains('A'),
        high_priority,
        command,
    }
}

fn build_bindings() -> Vec<Binding> {
    use vk::*;
    let open_bar = |mode| Command::OpenCommandBar { mode, split_side: None };
    let mut t = vec![
        // ---- high priority (reserved)
        b(letter('T'), "C", true, open_bar(CommandBarMode::NewTab)),
        b(letter('W'), "C", true, Command::CloseItem { id: None }),
        b(F4, "C", true, Command::CloseItem { id: None }),
        b(letter('T'), "CS", true, Command::ReopenClosed),
        b(TAB, "C", true, Command::MruStep { forward: true }),
        b(TAB, "CS", true, Command::MruStep { forward: false }),
        b(UP, "CA", true, Command::ActivateAdjacent { delta: -1 }),
        b(DOWN, "CA", true, Command::ActivateAdjacent { delta: 1 }),
        b(PRIOR, "C", true, Command::ActivateAdjacent { delta: -1 }),
        b(NEXT, "C", true, Command::ActivateAdjacent { delta: 1 }),
        b(LEFT, "CA", true, Command::SwitchSpaceAdjacent { delta: -1 }),
        b(RIGHT, "CA", true, Command::SwitchSpaceAdjacent { delta: 1 }),
        b(letter('K'), "CS", true, Command::ClearToday { space: None }),
        b(OEM_PLUS, "CS", true, open_bar(CommandBarMode::Split)),
        b(ADD, "CS", true, open_bar(CommandBarMode::Split)),
        b(OEM_MINUS, "CS", true, Command::SeparatePane { tab: None }),
        b(SUBTRACT, "CS", true, Command::SeparatePane { tab: None }),
        b(OEM_4, "CS", true, Command::FocusPaneAdjacent { delta: -1 }),
        b(OEM_6, "CS", true, Command::FocusPaneAdjacent { delta: 1 }),
        b(F12, "", true, Command::ToggleDevTools),
        b(letter('I'), "CS", true, Command::FocusDevTools),
        b(F11, "", true, Command::WindowControl { action: WindowAction::ToggleFullscreen }),
        b(letter('F'), "AS", true, Command::WindowControl { action: WindowAction::ToggleFullscreen }),
        b(letter('W'), "CS", true, Command::WindowCloseRequested),
        // ---- normal priority (page first)
        b(letter('L'), "C", false, open_bar(CommandBarMode::EditUrl)),
        b(letter('D'), "A", false, open_bar(CommandBarMode::EditUrl)),
        b(F6, "", false, open_bar(CommandBarMode::EditUrl)),
        b(letter('S'), "C", false, Command::ToggleSidebar),
        b(letter('D'), "C", false, Command::TogglePin { id: None }),
        b(letter('C'), "CS", false, Command::CopyUrl { id: None, markdown: false }),
        b(letter('C'), "CSA", false, Command::CopyUrl { id: None, markdown: true }),
        b(LEFT, "A", false, Command::GoBack { tab: None }),
        b(RIGHT, "A", false, Command::GoForward { tab: None }),
        b(letter('R'), "C", false, Command::Reload { tab: None, ignore_cache: false }),
        b(F5, "", false, Command::Reload { tab: None, ignore_cache: false }),
        b(letter('R'), "CS", false, Command::Reload { tab: None, ignore_cache: true }),
        b(F5, "C", false, Command::Reload { tab: None, ignore_cache: true }),
        b(F5, "S", false, Command::Reload { tab: None, ignore_cache: true }),
        b(letter('F'), "C", false, Command::OpenFind),
        b(F3, "", false, Command::FindNext { forward: true }),
        b(F3, "S", false, Command::FindNext { forward: false }),
        b(OEM_PLUS, "C", false, Command::Zoom { direction: ZoomDirection::In }),
        b(ADD, "C", false, Command::Zoom { direction: ZoomDirection::In }),
        b(OEM_MINUS, "C", false, Command::Zoom { direction: ZoomDirection::Out }),
        b(SUBTRACT, "C", false, Command::Zoom { direction: ZoomDirection::Out }),
        b(KEY_0, "C", false, Command::Zoom { direction: ZoomDirection::Reset }),
        b(NUMPAD0, "C", false, Command::Zoom { direction: ZoomDirection::Reset }),
        b(letter('P'), "C", false, Command::Print),
        b(letter('U'), "C", false, Command::ViewSource),
        b(letter('J'), "C", false, Command::ToggleSidebarPanel { panel: SidebarPanel::Downloads }),
        b(OEM_COMMA, "C", false, Command::OpenInternalPage { page: InternalPage::Settings }),
        b(letter('H'), "C", false, Command::OpenInternalPage { page: InternalPage::History }),
        b(letter('O'), "C", false, Command::ExpandPeek { split: false }),
        b(letter('F'), "A", false, Command::ToggleSidebarPanel { panel: SidebarPanel::AppMenu }),
        // Ctrl+E is page first (D5a): Notion, vscode.dev and DevTools keep their own Ctrl+E, and the
        // app menu or `>` opens the picker everywhere. Ignored while the Ctrl+Tab switcher is up
        // ([`on_accelerator`]), where Ctrl is still held.
        b(letter('E'), "C", false, open_bar(CommandBarMode::Extensions)),
    ];
    for n in 1..=9u32 {
        let key = KEY_0 + n as i32;
        t.push(b(key, "C", true, Command::ActivateNth { n }));
        t.push(b(key, "A", false, Command::SwitchSpaceNth { n }));
        if n <= 4 {
            t.push(b(key, "CS", true, Command::FocusPane { index: n as usize - 1 }));
        }
    }
    t
}

/// The accelerator table.
pub fn bindings() -> &'static [Binding] {
    static TABLE: OnceLock<Vec<Binding>> = OnceLock::new();
    TABLE.get_or_init(build_bindings)
}

/// Command id of the binding for a combo (debug/tests).
#[cfg_attr(not(debug_assertions), allow(dead_code))]
pub fn find_binding(key: i32, shift: bool, ctrl: bool, alt: bool) -> Option<i32> {
    bindings()
        .iter()
        .position(|b| b.key == key && b.shift == shift && b.ctrl == ctrl && b.alt == alt)
        .map(|i| FIRST_COMMAND_ID + i as i32)
}

/// Registers every binding on the window (only effective from `on_window_created` on).
pub fn install_accelerators(window: &Window) {
    for (i, binding) in bindings().iter().enumerate() {
        window.set_accelerator(
            FIRST_COMMAND_ID + i as i32,
            binding.key,
            binding.shift as i32,
            binding.ctrl as i32,
            binding.alt as i32,
            binding.high_priority as i32,
        );
    }
}

thread_local! {
    /// A `MruStep` was dispatched and Ctrl has not been released since.
    static MRU_ACTIVE: Cell<bool> = const { Cell::new(false) };
    static ACCELERATOR_COUNT: Cell<u64> = const { Cell::new(0) };
    /// Injected keys that matched one of sta's own bindings and were refused.
    static INJECTED_BLOCKED: Cell<u64> = const { Cell::new(0) };
    static LAST_ACCELERATOR: Cell<Option<i32>> = const { Cell::new(None) };
}

/// Enqueues a binding's command (accelerators and fallback share this path).
fn dispatch_binding(binding: &Binding) {
    if matches!(binding.command, Command::MruStep { .. }) {
        MRU_ACTIVE.set(true);
    }
    controller::dispatch(binding.command.clone());
}

/// `WindowDelegate::on_accelerator`. Returning `false` leaves the key to the focused view, which is
/// how plain F11 reaches a focused DevTools frontend (D9).
pub fn on_accelerator(command_id: i32) -> bool {
    let index = command_id - FIRST_COMMAND_ID;
    let Some(binding) = usize::try_from(index).ok().and_then(|i| bindings().get(i)) else {
        return false;
    };
    let devtools_focused = crate::devtools::frontend_has_focus();
    // D9: F11 belongs to DevTools while its frontend has focus (step out); fullscreen stays on F11
    // from the page and on Alt+Shift+F.
    if devtools_focused && binding.key == vk::F11 && !binding.shift && !binding.ctrl && !binding.alt {
        log_debug!("accelerator {command_id}: F11 left to the focused DevTools frontend");
        return false;
    }
    // FID-6: Ctrl+Shift+C is Chrome's element picker, and sta's Copy URL. With DevTools docked on the
    // focused pane the picker wins (that is the state in which a developer presses it, and DevTools
    // itself takes the chord as soon as the frontend has focus); with no dock it stays Copy URL,
    // which is what README's key table promises.
    if binding.key == vk::letter('C')
        && binding.ctrl
        && binding.shift
        && !binding.alt
        && let Some(tab) = crate::devtools::picker_tab()
    {
        ACCELERATOR_COUNT.set(ACCELERATOR_COUNT.get() + 1);
        LAST_ACCELERATOR.set(Some(command_id));
        log_debug!("accelerator {command_id}: DevTools is docked on tab {tab}, starting the element picker");
        crate::devtools::enter_inspect_mode(tab);
        return true;
    }
    // Ctrl+E while the Ctrl+Tab switcher is up: Ctrl is held for the switcher, and E would open the
    // picker over it. The key is consumed and nothing happens (FINAL PLAN §4 "Binding").
    if binding.key == vk::letter('E')
        && binding.ctrl
        && !binding.shift
        && !binding.alt
        && (MRU_ACTIVE.get() || overlays::is_switcher_requested())
    {
        log_debug!("accelerator {command_id}: Ctrl+E ignored while the switcher is up");
        return true;
    }
    ACCELERATOR_COUNT.set(ACCELERATOR_COUNT.get() + 1);
    LAST_ACCELERATOR.set(Some(command_id));
    // UX17: Ctrl+Shift+I opens, then focuses, then closes. Only the shell knows whether the frontend
    // has focus, so the third step becomes a `ToggleDevTools` here (core would close nothing else:
    // it is the same tab either way).
    if devtools_focused && binding.command == Command::FocusDevTools {
        log_debug!("accelerator {command_id}: DevTools has focus, closing");
        controller::dispatch(Command::ToggleDevTools);
        return true;
    }
    log_debug!("accelerator {command_id}: {:?}", binding.command);
    dispatch_binding(binding);
    true
}

const EVENTFLAG_SHIFT_DOWN: u32 = 1 << 1;
const EVENTFLAG_CONTROL_DOWN: u32 = 1 << 2;
const EVENTFLAG_ALT_DOWN: u32 = 1 << 3;

fn modifiers(event: &KeyEvent) -> u32 {
    event.modifiers & (EVENTFLAG_SHIFT_DOWN | EVENTFLAG_CONTROL_DOWN | EVENTFLAG_ALT_DOWN)
}

/// Ctrl released after Ctrl+Tab: commit the switcher selection (once). `from_browser`: the
/// browser's pre-key handler, which sees the key-up first; the window's unhandled-key callback
/// gets the same key-up again after the renderer round trip, so it only acts on `MRU_ACTIVE`.
fn on_ctrl_released(from_browser: bool) -> bool {
    if MRU_ACTIVE.replace(false) || (from_browser && overlays::is_switcher_requested()) {
        controller::dispatch(Command::MruCommit);
        return true;
    }
    false
}

/// `KeyboardHandler::on_pre_key_event` for every browser. Returns `true` to consume the event.
pub fn on_pre_key_event(browser_id: i32, event: &KeyEvent) -> bool {
    if event.type_ == KeyEventType::KEYUP && event.windows_key_code == vk::CONTROL {
        on_ctrl_released(true);
        return false;
    }
    if event.type_ != KeyEventType::RAWKEYDOWN {
        return false;
    }
    if event.windows_key_code == vk::ESCAPE {
        match modifiers(event) {
            0 => return escape_chain(browser_id),
            // Esc while still holding Ctrl after Ctrl+Tab cancels the switcher (when the OS does
            // not take Ctrl+Esc for the Start menu).
            EVENTFLAG_CONTROL_DOWN if MRU_ACTIVE.get() || overlays::is_switcher_requested() => {
                MRU_ACTIVE.set(false);
                controller::dispatch(Command::MruCancel);
                return true;
            }
            _ => {}
        }
    }
    // A debugger key the frontend asked the page to forward (F8, F10, Shift+F11, Ctrl+\, Ctrl+').
    if crate::devtools::forwarded_key(browser_id, event) {
        return true;
    }
    if let Some(index) = match_binding(event) {
        log_debug!(
            "pre-key browser {browser_id} ({:?}): binding {} {:?}",
            browsers::role_of(browser_id),
            FIRST_COMMAND_ID + index as i32,
            bindings()[index].command
        );
    }
    false
}

/// `KeyboardHandler::on_key_event`: the page did not handle this key, so Views is about to match it
/// against the normal-priority accelerators. `native` is whether the event carries an OS message.
///
/// Returning `true` (handled) stops that match. Only injected keys are stopped: a real key must
/// still reach the page-first bindings (Ctrl+L, Ctrl+R, Alt+←, …), which is the whole point of this
/// callback.
pub fn on_key_event(browser_id: i32, event: &KeyEvent, native: bool) -> bool {
    if native || event.type_ == KeyEventType::CHAR {
        return false;
    }
    if match_binding(event).is_some() {
        INJECTED_BLOCKED.set(INJECTED_BLOCKED.get() + 1);
        log_debug!("injected key {:#x} from browser {browser_id} kept away from sta's shortcuts", event.windows_key_code);
    }
    // Consumed either way: an injected key that matches nothing would be dropped by Views anyway,
    // and one that does must never reach the accelerator table.
    true
}

/// F12 or Ctrl+Shift+I key-down (DevTools toggle; used by the DevTools window's own client).
pub fn is_devtools_toggle(event: &KeyEvent) -> bool {
    if event.type_ != KeyEventType::RAWKEYDOWN {
        return false;
    }
    let m = modifiers(event);
    (event.windows_key_code == vk::F12 && m == 0)
        || (event.windows_key_code == vk::letter('I') && m == EVENTFLAG_CONTROL_DOWN | EVENTFLAG_SHIFT_DOWN)
}

/// Index of the binding for a key event's combo.
fn match_binding(event: &KeyEvent) -> Option<usize> {
    let m = modifiers(event);
    let (shift, ctrl, alt) = (m & EVENTFLAG_SHIFT_DOWN != 0, m & EVENTFLAG_CONTROL_DOWN != 0, m & EVENTFLAG_ALT_DOWN != 0);
    bindings()
        .iter()
        .position(|b| b.key == event.windows_key_code && b.shift == shift && b.ctrl == ctrl && b.alt == alt)
}

/// Esc: page fullscreen → focused find bar → Peek → find bar → switcher → command bar → sidebar
/// panel → floating sidebar (hidden, not consumed). `true` = consumed.
fn escape_chain(browser_id: i32) -> bool {
    let role = browsers::role_of(browser_id);
    // The command bar page handles its own Esc (closeCommandBar), and so does the agent overlay.
    // A docked DevTools frontend keeps Esc too (its drawer and the element picker use it).
    if matches!(role, Some(Role::Surface(Surface::CommandBar | Surface::Agent)) | Some(Role::DevTools { .. })) {
        return false;
    }
    let fullscreen_tab = window::page_fullscreen_tab().or_else(|| match role {
        Some(Role::Tab(tab)) => browsers::browser(browser_id)
            .and_then(|b| b.host())
            .filter(|h| h.is_fullscreen() != 0)
            .map(|_| tab),
        _ => None,
    });
    if let Some(tab) = fullscreen_tab {
        tabs::exit_page_fullscreen(tab);
        return true;
    }
    // Esc inside the find bar closes the find bar even over Peek.
    if role == Some(Role::Surface(Surface::FindBar)) && overlays::is_visible(Overlay::FindBar) {
        controller::dispatch(Command::CloseFind);
        return true;
    }
    // The extension popup card is first in the chain (UX8): it is the most recent thing the user
    // opened, and Esc is how Chrome's own action popup closes.
    if overlays::is_visible(Overlay::ExtensionPopup) {
        controller::dispatch(Command::CloseExtensionPopup);
        return true;
    }
    if overlays::is_visible(Overlay::Peek) {
        controller::dispatch(Command::ClosePeek { focus_lost: false });
        return true;
    }
    if overlays::is_visible(Overlay::FindBar) {
        controller::dispatch(Command::CloseFind);
        return true;
    }
    if overlays::is_switcher_requested() {
        MRU_ACTIVE.set(false);
        controller::dispatch(Command::MruCancel);
        return true;
    }
    if overlays::is_visible(Overlay::CommandBar) {
        controller::dispatch(Command::CloseCommandBar { seq: None });
        return true;
    }
    // A transient sidebar panel opened by a shortcut while focus stays elsewhere (Ctrl+J downloads,
    // Alt+F app menu). Space sheets and an inline rename survive a page taking focus, so Esc in
    // the page leaves them alone too. The sidebar page handles its own Esc.
    if role != Some(Role::Surface(Surface::Sidebar)) && controller::with_store(|s| esc_closes_panel(s.sidebar_panel())).unwrap_or(false) {
        controller::dispatch(Command::CloseSidebarPanel);
        return true;
    }
    // The floating sidebar revealed by hovering the window edge (it can't have focus itself) hides,
    // but the key isn't consumed: the page gets its Escape too (web apps close their own dialogs).
    sidebar_hover::escape();
    false
}

/// Does Esc outside the sidebar close this sidebar panel? Only transient ones (downloads, app menu).
fn esc_closes_panel(panel: Option<&SidebarPanel>) -> bool {
    panel.is_some_and(SidebarPanel::is_transient)
}

/// `WindowDelegate::on_key_event`: keys that no view handled.
pub fn on_window_key_event(event: &KeyEvent) -> bool {
    if event.type_ == KeyEventType::KEYUP && event.windows_key_code == vk::CONTROL {
        on_ctrl_released(false);
    }
    false
}

/// Keyboard state for `debug.info`.
#[cfg_attr(not(debug_assertions), allow(dead_code))] // used by debug.rs only
pub fn debug_snapshot() -> serde_json::Value {
    serde_json::json!({
        "accelerators": ACCELERATOR_COUNT.get(),
        "lastAccelerator": LAST_ACCELERATOR.get(),
        "mruActive": MRU_ACTIVE.get(),
        "injectedBlocked": INJECTED_BLOCKED.get(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn combos_are_unique() {
        let t = bindings();
        for (i, a) in t.iter().enumerate() {
            for b in &t[i + 1..] {
                assert!(
                    !(a.key == b.key && a.shift == b.shift && a.ctrl == b.ctrl && a.alt == b.alt),
                    "duplicate combo {:#x} {:?}",
                    a.key,
                    a.command
                );
            }
        }
        assert!(find_binding(vk::letter('T'), false, true, false).is_some());
    }

    #[test]
    fn esc_in_a_page_closes_only_transient_sidebar_panels() {
        assert!(esc_closes_panel(Some(&SidebarPanel::Downloads)));
        assert!(esc_closes_panel(Some(&SidebarPanel::AppMenu)));
        assert!(!esc_closes_panel(Some(&SidebarPanel::NewSpace)));
        assert!(!esc_closes_panel(Some(&SidebarPanel::EditSpace { id: 3 })));
        assert!(!esc_closes_panel(Some(&SidebarPanel::RenameItem { id: 7 })));
        assert!(!esc_closes_panel(Some(&SidebarPanel::EditPinned { id: 7 })));
        assert!(!esc_closes_panel(None));
    }
}
