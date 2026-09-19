//! Docked DevTools rules (FINAL PLAN §3, §7 "Core unit"): open/close/focus, the close triggers
//! (tab closed, unloaded, archived, crashed, replaced), undock, Peek, nothing persisted, and the
//! frontend's own link/search requests.
//!
//! The refusal on `sta://` pages and the debug override live in `scenarios_foreign.rs`
//! (`devtools_never_open_on_sta_pages`), next to the rest of phase 1.

mod common;
use common::*;
use sta_core::*;

fn opened(fx: &[Effect], tab: Id, docked: bool) -> bool {
    has(fx, |e| matches!(e, Effect::OpenDevTools { tab: t, docked: d } if *t == tab && *d == docked))
}

fn closed(fx: &[Effect], tab: Id) -> bool {
    has(fx, |e| matches!(e, Effect::CloseDevTools { tab: t } if *t == tab))
}

#[test]
fn f12_toggles_a_docked_frontend() {
    let mut h = Harness::new();
    let tab = h.open("https://a.com/");
    let fx = h.apply(Command::ToggleDevTools);
    assert!(opened(&fx, tab, true), "{fx:?}");
    assert!(h.store.devtools_open().contains(&tab));

    let fx = h.apply(Command::ToggleDevTools);
    assert!(closed(&fx, tab), "{fx:?}");
    assert!(!h.store.devtools_open().contains(&tab));
}

#[test]
fn ctrl_shift_i_opens_then_focuses() {
    let mut h = Harness::new();
    let tab = h.open("https://a.com/");
    // Closed: the shortcut opens (its third step — close while the frontend has focus — is the
    // shell's, which dispatches ToggleDevTools then; see keyboard.rs).
    let fx = h.apply(Command::FocusDevTools);
    assert!(opened(&fx, tab, true), "{fx:?}");
    // Open: it only moves focus.
    let fx = h.apply(Command::FocusDevTools);
    assert!(has(&fx, |e| matches!(e, Effect::FocusDevTools { tab: t } if *t == tab)), "{fx:?}");
    assert!(!closed(&fx, tab), "{fx:?}");
    assert!(h.store.devtools_open().contains(&tab));
}

#[test]
fn devtools_close_with_the_tab_they_inspect() {
    let mut h = Harness::new();
    let a = h.open("https://a.com/");
    let b = h.open("https://b.com/");
    h.apply(Command::ActivateItem { id: a });
    h.apply(Command::ToggleDevTools);
    assert!(h.store.devtools_open().contains(&a));

    // Closing the tab.
    h.apply(Command::CloseItem { id: Some(a) });
    assert!(!h.store.devtools_open().contains(&a), "the tab's browser is gone");

    // Unloading another tab (its browser is destroyed too).
    h.apply(Command::ActivateItem { id: b });
    h.apply(Command::ToggleDevTools);
    assert!(h.store.devtools_open().contains(&b));
    h.apply(Command::UnloadTab { id: b });
    assert!(!h.store.devtools_open().contains(&b), "an unloaded tab has no DevTools");
}

#[test]
fn a_replaced_browser_closes_devtools_before_the_replace() {
    let mut h = Harness::new();
    let tab = h.open("https://a.com/");
    h.apply(Command::ToggleDevTools);
    // web -> sta:// replaces the browser: the page view must be back in its wrapper first.
    let fx = h.apply(Command::Navigate { tab: Some(tab), url: "sta://settings/".into() });
    let close = position(&fx, |e| matches!(e, Effect::CloseDevTools { tab: t } if *t == tab));
    let replace = position(&fx, |e| matches!(e, Effect::ReplaceBrowser { tab: t, .. } if *t == tab));
    assert!(close.is_some() && replace.is_some() && close < replace, "{fx:?}");
    assert!(!h.store.devtools_open().contains(&tab));
}

#[test]
fn a_crash_closes_devtools() {
    let mut h = Harness::new();
    let tab = h.open("https://a.com/");
    h.apply(Command::ToggleDevTools);
    h.apply(Command::TabCrashed { tab });
    // The crashed browser is destroyed and reported closed by the shell.
    h.apply(Command::TabBrowserClosed { tab });
    assert!(!h.store.devtools_open().contains(&tab));
}

#[test]
fn undock_reopens_in_cefs_own_window_until_devtools_close() {
    let mut h = Harness::new();
    let tab = h.open("https://a.com/");
    h.apply(Command::ToggleDevTools);
    let fx = h.apply(Command::DevToolsUndockRequested { tab, narrow: false });
    let close = position(&fx, |e| matches!(e, Effect::CloseDevTools { tab: t } if *t == tab));
    let open = position(&fx, |e| matches!(e, Effect::OpenDevTools { tab: t, docked: false } if *t == tab));
    assert!(close.is_some() && open.is_some() && close < open, "{fx:?}");
    assert!(h.store.devtools_undocked(tab));

    // While undocked, F12 closes and F12 again re-opens **docked**: the undock lasts for that
    // DevTools, not for the rest of the session (FINAL PLAN §7 "Undock -> close -> F12 docks again").
    let fx = h.apply(Command::ToggleDevTools);
    assert!(closed(&fx, tab), "{fx:?}");
    assert!(!h.store.devtools_undocked(tab));
    let fx = h.apply(Command::ToggleDevTools);
    assert!(opened(&fx, tab, true), "{fx:?}");
}

#[test]
fn the_undock_action_works_without_devtools_open() {
    let mut h = Harness::new();
    let tab = h.open("https://a.com/");
    let fx = h.apply(Command::UndockDevTools);
    assert!(opened(&fx, tab, false), "{fx:?}");
    assert!(!closed(&fx, tab), "nothing was open to close: {fx:?}");
    assert!(h.store.devtools_undocked(tab));
}

#[test]
fn a_peek_page_opens_undocked() {
    let mut h = Harness::new();
    let opener = h.open("https://a.com/");
    let (peek, _) = h.popup(Some(opener), "https://popup.example/", true, true);
    assert_eq!(h.focused(), Some(peek), "the Peek tab is the focused target");
    let fx = h.apply(Command::ToggleDevTools);
    assert!(opened(&fx, peek, false), "{fx:?}");
}

#[test]
fn inspect_opens_devtools_and_selects_the_point() {
    let mut h = Harness::new();
    let tab = h.open("https://a.com/");
    let fx = h.apply(Command::InspectElement { tab, x: 40, y: 12 });
    let open = position(&fx, |e| matches!(e, Effect::OpenDevTools { tab: t, docked: true } if *t == tab));
    let at = position(&fx, |e| matches!(e, Effect::InspectAt { tab: t, x: 40, y: 12 } if *t == tab));
    assert!(open.is_some() && at.is_some() && open < at, "{fx:?}");

    // Already open: no second open.
    let fx = h.apply(Command::InspectElement { tab, x: 1, y: 2 });
    assert!(!has(&fx, |e| matches!(e, Effect::OpenDevTools { .. })), "{fx:?}");
    assert!(has(&fx, |e| matches!(e, Effect::InspectAt { tab: t, x: 1, y: 2 } if *t == tab)), "{fx:?}");
}

#[test]
fn inspect_is_refused_on_sta_pages() {
    let mut h = Harness::new();
    h.apply(Command::OpenInternalPage { page: InternalPage::Settings });
    let internal = h.focused().expect("settings tab");
    let fx = h.apply(Command::InspectElement { tab: internal, x: 5, y: 5 });
    assert!(!has(&fx, |e| matches!(e, Effect::OpenDevTools { .. } | Effect::InspectAt { .. })), "{fx:?}");
    assert_eq!(h.toast().map(|t| t.message), Some("DevTools isn't available on sta pages".into()));
}

#[test]
fn open_devtools_is_not_persisted() {
    let mut h = Harness::new();
    let tab = h.open("https://a.com/");
    h.apply(Command::ToggleDevTools);
    h.apply(Command::DevToolsUndockRequested { tab, narrow: false });
    let state = h.store.state_json();
    assert!(!state.contains("devtools"), "no DevTools state is saved: {state}");
}

#[test]
fn the_frontend_can_open_links_web_content_may_open() {
    let mut h = Harness::new();
    let tab = h.open("https://a.com/");
    let before = h.today().len();
    let fx = h.apply(Command::DevToolsLinkRequested { tab, url: "https://web.dev/x".into(), search: false });
    let opened = h.focused().expect("the link's tab");
    assert_eq!(h.tab(opened).url, "https://web.dev/x");
    assert!(has(&fx, |e| is_create(e, opened)), "{fx:?}");

    // Refused schemes open nothing at all.
    let count = h.today().len();
    for url in ["sta://settings/", "chrome://version", "file:///C:/x.txt", "javascript:alert(1)", "devtools://devtools/x"] {
        h.apply(Command::DevToolsLinkRequested { tab, url: url.into(), search: false });
        assert_eq!(h.today().len(), count, "{url} opened a tab");
    }
    assert!(h.today().len() > before);
}

#[test]
fn the_frontend_search_uses_the_configured_engine() {
    let mut h = Harness::new();
    let tab = h.open("https://a.com/");
    h.apply(Command::DevToolsLinkRequested { tab, url: "flex gap".into(), search: true });
    let opened = h.focused().expect("the search tab");
    let url = h.tab(opened).url;
    assert!(url.contains("flex") && url.contains("gap") && url.starts_with("https://"), "{url}");
}

#[test]
fn an_external_protocol_from_the_frontend_goes_to_the_os() {
    let mut h = Harness::new();
    let tab = h.open("https://a.com/");
    let count = h.today().len();
    let fx = h.apply(Command::DevToolsLinkRequested { tab, url: "mailto:a@b.c".into(), search: false });
    assert!(has(&fx, |e| matches!(e, Effect::OpenExternal { url } if url == "mailto:a@b.c")), "{fx:?}");
    assert_eq!(h.today().len(), count, "no tab for an external protocol");
}

#[test]
fn shell_only_devtools_commands_are_refused_from_the_ui() {
    for cmd in [
        Command::DevToolsClosed { tab: 3 },
        Command::DevToolsUndockRequested { tab: 3, narrow: false },
        Command::DevToolsLinkRequested { tab: 3, url: "https://a.com/".into(), search: false },
        Command::InspectElement { tab: 3, x: 1, y: 1 },
    ] {
        assert!(!cmd.allowed_from_ui(), "{cmd:?} must be shell-only");
    }
    for cmd in [Command::ToggleDevTools, Command::FocusDevTools, Command::UndockDevTools] {
        assert!(cmd.allowed_from_ui(), "{cmd:?} is a user action");
    }
}
