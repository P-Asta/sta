//! Native context menus [owner: tabs] (ARCHITECTURE §4.1, arc_spec context menus).
//!
//! Declared as `client::context_menu` (`#[path]` in client.rs), so main.rs needs no module line.
//!
//! - **UI surfaces** (sidebar, top bar, overlays, internal pages — the UI client): Chromium's
//!   default menu is suppressed. In editable fields only the edit commands survive (undo/redo,
//!   cut, copy, paste, delete, select all, spelling suggestions); a plain text selection keeps
//!   Copy; everything else shows no menu (surfaces draw their own HTML menus).
//! - **Web tabs** (tab client): the default menu stays and gains, for links, "Open Link in New
//!   Tab" (`LinkOpenRequested{BackgroundTab}`), "Open Link in Peek (Alt+Click)" (`{NewWindow}`; the
//!   label names the gesture of PROTOCOL §13, because this menu is where a user looks for it),
//!   "Copy Link Address" (`CopyText`); for images "Open Image in New Tab" and "Copy Image Address"; and
//!   "Inspect" (DevTools at the click point). External-protocol links (`mailto:` …) go to the OS
//!   (external.rs); Chromium's "View page source" runs core's `ViewSource`.
//!
//! Debug builds only: with `STA_TEST_CONTEXT_MENU=<label>` the native menu is not shown; the
//! final items are logged (`context menu (ui|tab): a | b`) and the item with that label is executed
//! (anything else cancels). End-to-end tests use it because a native menu runs a modal loop.
//!
//! Public API:
//! - `pub fn ui_handler() -> ContextMenuHandler`, `pub fn tab_handler() -> ContextMenuHandler`
//! - `pub const CMD_*` command ids (MENU_ID_USER_FIRST based)

use crate::browsers::{self, Role};
use crate::{controller, external, task};
use std::sync::OnceLock;
use sta_core::{Command, Id, LinkDisposition};
use cef::sys::{cef_context_menu_type_flags_t, cef_menu_id_t};
use cef::*;

const USER_FIRST: i32 = cef_menu_id_t::MENU_ID_USER_FIRST as i32;
pub const CMD_OPEN_LINK_NEW_TAB: i32 = USER_FIRST + 1;
pub const CMD_OPEN_LINK_PEEK: i32 = USER_FIRST + 2;
pub const CMD_COPY_LINK: i32 = USER_FIRST + 3;
pub const CMD_OPEN_IMAGE_NEW_TAB: i32 = USER_FIRST + 4;
pub const CMD_COPY_IMAGE_ADDRESS: i32 = USER_FIRST + 5;
pub const CMD_INSPECT: i32 = USER_FIRST + 6;

pub fn ui_handler() -> ContextMenuHandler {
    UiContextMenu::new()
}

pub fn tab_handler() -> ContextMenuHandler {
    TabContextMenu::new()
}

fn user_string(s: CefStringUserfree) -> String {
    CefString::from(&s).to_string()
}

fn has_flag(params: &ContextMenuParams, flag: cef_context_menu_type_flags_t) -> bool {
    cef_context_menu_type_flags_t::from(params.type_flags()).0 & flag.0 != 0
}

fn tab_of(browser: &Browser) -> Option<Id> {
    match browsers::role_of(browser.identifier()) {
        Some(Role::Tab(tab)) => Some(tab),
        _ => None,
    }
}

/// Edit commands kept in editable fields of UI pages.
fn is_edit_command(id: i32) -> bool {
    use cef_menu_id_t::*;
    [MENU_ID_UNDO, MENU_ID_REDO, MENU_ID_CUT, MENU_ID_COPY, MENU_ID_PASTE, MENU_ID_PASTE_MATCH_STYLE, MENU_ID_DELETE, MENU_ID_SELECT_ALL]
        .iter()
        .any(|m| *m as i32 == id)
        || (MENU_ID_SPELLCHECK_SUGGESTION_0 as i32..=MENU_ID_ADD_TO_DICTIONARY as i32).contains(&id)
}

fn labels(model: &MenuModel) -> Vec<String> {
    (0..model.count())
        .map(|i| if model.type_at(i) == MenuItemType::SEPARATOR { "-".to_string() } else { user_string(model.label_at(i)) })
        .collect()
}

/// `STA_TEST_CONTEXT_MENU` (debug builds only).
fn test_menu_choice() -> Option<&'static str> {
    static CHOICE: OnceLock<Option<String>> = OnceLock::new();
    CHOICE
        .get_or_init(|| if cfg!(debug_assertions) { std::env::var("STA_TEST_CONTEXT_MENU").ok().filter(|v| !v.is_empty()) } else { None })
        .as_deref()
}

/// Test replacement for the native menu: log the items, run the chosen one (posted) or cancel.
fn run_test_menu(kind: &str, model: &MenuModel, callback: &RunContextMenuCallback) -> i32 {
    let Some(choice) = test_menu_choice() else { return 0 };
    log_info!("context menu ({kind}): {}", labels(model).join(" | "));
    let id = (0..model.count()).find(|i| user_string(model.label_at(*i)) == choice).map(|i| model.command_id_at(i));
    let callback = callback.clone();
    task::post_ui(move || match id {
        Some(id) => callback.cont(id, EventFlags::default()),
        None => callback.cancel(),
    });
    1
}

/// Removes separators at the start, the end and in runs.
fn tidy_separators(model: &MenuModel) {
    let mut i = 0usize;
    let mut previous_separator = true; // drops leading separators
    while i < model.count() {
        let separator = model.type_at(i) == MenuItemType::SEPARATOR;
        if separator && previous_separator {
            model.remove_at(i);
            continue;
        }
        previous_separator = separator;
        i += 1;
    }
    while model.count() > 0 && model.type_at(model.count() - 1) == MenuItemType::SEPARATOR {
        model.remove_at(model.count() - 1);
    }
}

wrap_context_menu_handler! {
    struct UiContextMenu;

    impl ContextMenuHandler {
        fn on_before_context_menu(
            &self,
            _browser: Option<&mut Browser>,
            _frame: Option<&mut Frame>,
            params: Option<&mut ContextMenuParams>,
            model: Option<&mut MenuModel>,
        ) {
            let (Some(params), Some(model)) = (params, model) else { return };
            let editable = params.is_editable() != 0;
            let selection = !user_string(params.selection_text()).trim().is_empty();
            let copy = cef_menu_id_t::MENU_ID_COPY as i32;
            let mut i = model.count();
            while i > 0 {
                i -= 1;
                if model.type_at(i) == MenuItemType::SEPARATOR {
                    continue;
                }
                let id = model.command_id_at(i);
                let keep = if editable { is_edit_command(id) } else { selection && id == copy };
                if !keep {
                    model.remove_at(i);
                }
            }
            if !editable && selection && model.index_of(copy) < 0 {
                model.add_item(copy, Some(&CefString::from("Copy")));
            }
            tidy_separators(model);
            log_debug!("context menu (ui): {}", labels(model).join(" | "));
        }

        fn run_context_menu(
            &self,
            _browser: Option<&mut Browser>,
            _frame: Option<&mut Frame>,
            _params: Option<&mut ContextMenuParams>,
            model: Option<&mut MenuModel>,
            callback: Option<&mut RunContextMenuCallback>,
        ) -> i32 {
            let (Some(model), Some(callback)) = (model, callback) else { return 0 };
            run_test_menu("ui", model, callback)
        }
    }
}

wrap_context_menu_handler! {
    struct TabContextMenu;

    impl ContextMenuHandler {
        fn on_before_context_menu(
            &self,
            _browser: Option<&mut Browser>,
            _frame: Option<&mut Frame>,
            params: Option<&mut ContextMenuParams>,
            model: Option<&mut MenuModel>,
        ) {
            let (Some(params), Some(model)) = (params, model) else { return };
            let mut index = 0usize;
            let mut insert = |id: i32, label: &str| {
                model.insert_item_at(index, id, Some(&CefString::from(label)));
                index += 1;
            };
            let link = user_string(params.link_url());
            if has_flag(params, cef_context_menu_type_flags_t::CM_TYPEFLAG_LINK) && !link.is_empty() {
                insert(CMD_OPEN_LINK_NEW_TAB, "Open Link in New Tab");
                // The gesture is spelled out here because this is the one place a user hovers a link
                // and looks for what they can do with it (PROTOCOL §13).
                insert(CMD_OPEN_LINK_PEEK, "Open Link in Peek (Alt+Click)");
                insert(CMD_COPY_LINK, "Copy Link Address");
            }
            let source = user_string(params.source_url());
            if params.media_type() == ContextMenuMediaType::IMAGE && !source.is_empty() {
                insert(CMD_OPEN_IMAGE_NEW_TAB, "Open Image in New Tab");
                insert(CMD_COPY_IMAGE_ADDRESS, "Copy Image Address");
            }
            if index > 0 && model.count() > index {
                model.insert_separator_at(index);
            }
            if model.count() > 0 {
                model.add_separator();
            }
            model.add_item(CMD_INSPECT, Some(&CefString::from("Inspect")));
            tidy_separators(model);
            log_debug!("context menu (tab): {}", labels(model).join(" | "));
        }

        fn run_context_menu(
            &self,
            _browser: Option<&mut Browser>,
            _frame: Option<&mut Frame>,
            _params: Option<&mut ContextMenuParams>,
            model: Option<&mut MenuModel>,
            callback: Option<&mut RunContextMenuCallback>,
        ) -> i32 {
            let (Some(model), Some(callback)) = (model, callback) else { return 0 };
            run_test_menu("tab", model, callback)
        }

        fn on_context_menu_command(
            &self,
            browser: Option<&mut Browser>,
            _frame: Option<&mut Frame>,
            params: Option<&mut ContextMenuParams>,
            command_id: i32,
            _event_flags: EventFlags,
        ) -> i32 {
            let (Some(browser), Some(params)) = (browser, params) else { return 0 };
            let open = |url: String, disposition: LinkDisposition| {
                if external::is_external(&url) {
                    external::open(&url, true, "context menu");
                } else if let Some(opener) = tab_of(browser)
                    && !url.is_empty()
                {
                    controller::dispatch(Command::LinkOpenRequested { opener, url, disposition });
                }
            };
            match command_id {
                CMD_OPEN_LINK_NEW_TAB => open(user_string(params.link_url()), LinkDisposition::BackgroundTab),
                CMD_OPEN_LINK_PEEK => open(user_string(params.link_url()), LinkDisposition::NewWindow),
                CMD_OPEN_IMAGE_NEW_TAB => open(user_string(params.source_url()), LinkDisposition::BackgroundTab),
                CMD_COPY_LINK => controller::dispatch(Command::CopyText { text: user_string(params.unfiltered_link_url()) }),
                CMD_COPY_IMAGE_ADDRESS => controller::dispatch(Command::CopyText { text: user_string(params.source_url()) }),
                // Inspect goes through core (which refuses `sta://` pages and opens the dock
                // first); `devtools.rs` turns the point into a node on session S.
                CMD_INSPECT => {
                    if let Some(tab) = tab_of(browser) {
                        controller::dispatch(Command::InspectElement { tab, x: params.xcoord(), y: params.ycoord() });
                    }
                }
                // Chromium would open `view-source:` through on_open_urlfrom_tab, which core refuses
                // for web content: use the app's View Source (the clicked tab has focus).
                id if id == cef_menu_id_t::MENU_ID_VIEW_SOURCE as i32 => controller::dispatch(Command::ViewSource),
                _ => return 0,
            }
            1
        }
    }
}
