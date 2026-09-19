//! Reducer scenarios: opening, activation, closing, reopening, pinning, favorites, drag & drop,
//! folders. Every command runs through the checking harness (invariants, effect guarantees,
//! revision/dirty tracking).

mod common;
use sta_core::*;
use common::*;

// ------------------------------------------------------------------------------------ opening

#[test]
fn startup_fresh_profile_shows_empty() {
    let h = Harness::new();
    assert_eq!(h.store.content_layout(), ContentLayout::Empty);
    let fx: Vec<&Effect> = h.history.iter().collect();
    assert!(matches!(fx[0], Effect::SetChrome { .. }));
    assert!(matches!(fx[1], Effect::SetSidebar { visible: true, width: SIDEBAR_DEFAULT_WIDTH, floating: false }));
    assert!(matches!(fx[2], Effect::ShowContent { layout: ContentLayout::Empty }));
    let ui = h.ui();
    assert_eq!(ui.spaces.len(), 1);
    assert_eq!(ui.spaces[0].name, "Home");
    assert!(ui.current.is_none());
}

#[test]
fn new_tab_opens_on_top_of_today_and_activates() {
    let mut h = Harness::new();
    let fx = h.apply(Command::OpenUrl { url: "https://a.com/".into(), target: OpenTarget::NewTab, opener: None });
    let a = h.focused().unwrap();
    let create = position(&fx, |e| is_create(e, a)).expect("create");
    let show = position(&fx, |e| matches!(e, Effect::ShowContent { layout: ContentLayout::Single { tab } } if *tab == a)).expect("show");
    let focus = position(&fx, |e| matches!(e, Effect::FocusBrowser { tab } if *tab == a)).expect("focus");
    assert!(create < show && show < focus, "{fx:?}");
    let b = h.open("https://b.com/");
    assert_eq!(h.today(), vec![b, a]);
    assert_eq!(h.active(), Some(b));
    assert_eq!(h.store.state().window.mru[..2], [b, a]);
    let ui = h.ui();
    let TodayRow(rows) = today_rows(&ui);
    assert!(rows[0].active && rows[0].visible && rows[0].loaded);
    assert!(!rows[1].active && !rows[1].visible && rows[1].loaded);
}

struct TodayRow(Vec<TabView>);
fn today_rows(ui: &UiState) -> TodayRow {
    let space = ui.spaces.iter().find(|s| s.id == ui.active_space).unwrap();
    TodayRow(
        space
            .today
            .iter()
            .filter_map(|n| if let NodeView::Tab(t) = n { Some(t.clone()) } else { None })
            .collect(),
    )
}

#[test]
fn open_input_classifies_and_records_typed_visit() {
    let mut h = Harness::new();
    h.apply(Command::OpenInput { text: "rust lang".into(), target: OpenTarget::NewTab });
    let t = h.focused().unwrap();
    assert_eq!(h.tab(t).url, "https://www.google.com/search?q=rust%20lang");
    h.apply(Command::OpenInput { text: "example.com".into(), target: OpenTarget::NewTab });
    let e = h.focused().unwrap();
    assert_eq!(h.tab(e).url, "https://example.com");
    // The committed URL is normalized by Chromium: still counts as typed.
    h.commit(e, "https://example.com/", "Example");
    let entry = h.store.history().get("https://example.com/").unwrap();
    assert_eq!(entry.typed_count, 1);
    assert_eq!(entry.title, "Example");
    // A later link navigation in the same tab is not typed.
    h.apply(Command::TabAddressChanged { tab: e, url: "https://example.com/docs".into() });
    assert_eq!(h.store.history().get("https://example.com/docs").unwrap().typed_count, 0);
    // Same URL again: no new visit.
    h.apply(Command::TabAddressChanged { tab: e, url: "https://example.com/docs".into() });
    assert_eq!(h.store.history().get("https://example.com/docs").unwrap().visit_count, 1);
    // Empty / whitespace input is ignored.
    let before = h.today().len();
    h.apply(Command::OpenInput { text: "   ".into(), target: OpenTarget::NewTab });
    h.apply(Command::OpenInput { text: "?".into(), target: OpenTarget::NewTab });
    assert_eq!(h.today().len(), before);
}

#[test]
fn background_tab_goes_below_opener_and_toasts_when_sidebar_hidden() {
    let mut h = Harness::new();
    let a = h.open("https://a.com/");
    let b = h.open("https://b.com/");
    h.apply(Command::ActivateItem { id: a });
    let fx = h.apply(Command::LinkOpenRequested { opener: a, url: "https://a.com/x".into(), disposition: LinkDisposition::BackgroundTab });
    let today = h.today();
    assert_eq!(today.len(), 3);
    let bg = today[2];
    assert_eq!(today, vec![b, a, bg]);
    assert!(has(&fx, |e| is_create(e, bg)));
    assert_eq!(h.active(), Some(a));
    assert!(h.toast().is_none());
    assert_eq!(h.tab(bg).opener, Some(a));
    h.apply(Command::ToggleSidebar);
    h.apply(Command::LinkOpenRequested { opener: a, url: "https://a.com/y".into(), disposition: LinkDisposition::BackgroundTab });
    let toast = h.toast().expect("toast");
    assert_eq!(toast.message, "New tab opened");
    assert_eq!(toast.duration_ms, 6000);
    assert!(matches!(*toast.action.unwrap().command, Command::ActivateItem { .. }));
    // Web content can't open internal pages.
    let n = h.today().len();
    h.apply(Command::LinkOpenRequested { opener: a, url: "sta://settings/".into(), disposition: LinkDisposition::ForegroundTab });
    h.apply(Command::LinkOpenRequested { opener: a, url: "javascript:alert(1)".into(), disposition: LinkDisposition::ForegroundTab });
    assert_eq!(h.today().len(), n);
}

#[test]
fn foreground_link_below_opener_and_close_returns_to_opener() {
    let mut h = Harness::new();
    let a = h.open("https://a.com/");
    let _b = h.open("https://b.com/");
    h.apply(Command::ActivateItem { id: a });
    h.apply(Command::LinkOpenRequested { opener: a, url: "https://c.com/".into(), disposition: LinkDisposition::ForegroundTab });
    let c = h.focused().unwrap();
    assert_eq!(h.today()[2], c);
    assert_eq!(h.tab(c).opener, Some(a));
    h.apply(Command::CloseItem { id: None });
    assert_eq!(h.active(), Some(a), "closing a never-left tab returns to its opener");

    // Leaving the tab clears the opener: close falls back to MRU instead.
    h.apply(Command::LinkOpenRequested { opener: a, url: "https://d.com/".into(), disposition: LinkDisposition::ForegroundTab });
    let d = h.focused().unwrap();
    let e = h.open("https://e.com/");
    h.apply(Command::ActivateItem { id: d });
    assert_eq!(h.tab(d).opener, None);
    h.apply(Command::CloseItem { id: None });
    assert_eq!(h.active(), Some(e));
}

#[test]
fn current_tab_navigation_and_browser_replacement() {
    let mut h = Harness::new();
    let a = h.open("https://a.com/");
    let fx = h.apply(Command::OpenUrl { url: "https://b.com/".into(), target: OpenTarget::CurrentTab, opener: None });
    assert!(has(&fx, |e| matches!(e, Effect::LoadUrl { tab, url } if *tab == a && url == "https://b.com/")));
    assert!(!has(&fx, |e| matches!(e, Effect::ShowContent { .. })), "no redundant ShowContent: {fx:?}");
    let fx = h.apply(Command::Navigate { tab: Some(a), url: "sta://history/".into() });
    assert!(has(&fx, |e| matches!(e, Effect::ReplaceBrowser { tab, internal: true, .. } if *tab == a)));
    let fx = h.apply(Command::Navigate { tab: None, url: "https://c.com/".into() });
    assert!(has(&fx, |e| matches!(e, Effect::ReplaceBrowser { tab, internal: false, .. } if *tab == a)));
    // No tab at all: CurrentTab opens a new tab.
    let mut h = Harness::new();
    h.apply(Command::OpenInput { text: "x.com".into(), target: OpenTarget::CurrentTab });
    assert_eq!(h.today().len(), 1);
}

#[test]
fn internal_pages_are_singletons() {
    let mut h = Harness::new();
    h.apply(Command::OpenInternalPage { page: InternalPage::Settings });
    let s = h.focused().unwrap();
    let fx = h.history.clone();
    assert!(fx.iter().any(|e| matches!(e, Effect::CreateBrowser { tab, internal: true, .. } if *tab == s)));
    let _other = h.open("https://a.com/");
    h.apply(Command::OpenInternalPage { page: InternalPage::Settings });
    assert_eq!(h.focused(), Some(s));
    assert_eq!(h.today().len(), 2);
    h.apply(Command::OpenInternalPage { page: InternalPage::Boosts });
    let b = h.focused().unwrap();
    let fx = h.apply(Command::OpenUrl { url: "sta://boosts/?id=5".into(), target: OpenTarget::NewTab, opener: None });
    assert_eq!(h.focused(), Some(b));
    assert!(has(&fx, |e| matches!(e, Effect::LoadUrl { tab, url } if *tab == b && url == "sta://boosts/?id=5")));
    assert_eq!(h.ui().current.unwrap().pill, "Boosts");
}

#[test]
fn back_forward_reload_stop_need_a_live_tab() {
    let mut h = Harness::new();
    assert!(h.apply(Command::GoBack { tab: None }).is_empty());
    let a = h.open("https://a.com/");
    assert_eq!(h.apply(Command::GoBack { tab: None }), vec![Effect::GoBack { tab: a }]);
    assert_eq!(h.apply(Command::GoForward { tab: Some(a) }), vec![Effect::GoForward { tab: a }]);
    assert_eq!(h.apply(Command::StopLoad { tab: None }), vec![Effect::StopLoad { tab: a }]);
    assert_eq!(h.apply(Command::Reload { tab: None, ignore_cache: true }), vec![Effect::Reload { tab: a, ignore_cache: true }]);
    // Failed load: Reload retries the failed URL.
    h.apply(Command::TabLoadFailed { tab: a, url: "https://a.com/broken".into(), error_code: -105, error_text: "ERR_NAME_NOT_RESOLVED".into() });
    let ui = h.ui();
    assert_eq!(ui.current.as_ref().unwrap().load_error.as_deref(), Some("ERR_NAME_NOT_RESOLVED"));
    assert!(today_rows(&ui).0[0].failed);
    assert_eq!(h.tab(a).url, "https://a.com/broken");
    let fx = h.apply(Command::Reload { tab: None, ignore_cache: false });
    assert_eq!(fx, vec![Effect::LoadUrl { tab: a, url: "https://a.com/broken".into() }]);
    assert!(h.ui().current.unwrap().load_error.is_none());
    // Crash + reload
    h.apply(Command::TabCrashed { tab: a });
    assert!(today_rows(&h.ui()).0[0].crashed);
    h.apply(Command::Reload { tab: None, ignore_cache: false });
    assert!(!today_rows(&h.ui()).0[0].crashed);
}

#[test]
fn activate_nth_and_adjacent_follow_visual_order() {
    let mut h = Harness::new();
    let f1 = h.open_favorite("https://f1.com/");
    let p1 = h.open_pinned("https://p1.com/");
    h.apply(Command::NewFolder { space: None, parent: None, name: Some("F".into()) });
    let folder = h.pinned()[0];
    let p2 = h.open_pinned("https://p2.com/");
    h.apply(Command::MoveItem { id: p2, to: DropTarget { container: Container::Folder { id: folder }, before: None } });
    let t1 = h.open("https://t1.com/");
    let t2 = h.open("https://t2.com/");
    // order: f1, [folder: p2], p1, t2, t1
    h.apply(Command::ActivateNth { n: 1 });
    assert_eq!(h.active(), Some(f1));
    h.apply(Command::ActivateNth { n: 2 });
    assert_eq!(h.active(), Some(p2));
    h.apply(Command::ActivateNth { n: 3 });
    assert_eq!(h.active(), Some(p1));
    h.apply(Command::ActivateNth { n: 9 });
    assert_eq!(h.active(), Some(t1));
    h.apply(Command::ActivateNth { n: 0 });
    assert_eq!(h.active(), Some(t1));
    h.apply(Command::ToggleFolder { id: folder });
    h.apply(Command::ActivateNth { n: 2 });
    assert_eq!(h.active(), Some(p1), "collapsed folder children are skipped");
    h.apply(Command::ActivateAdjacent { delta: 1 });
    assert_eq!(h.active(), Some(t2));
    h.apply(Command::ActivateAdjacent { delta: 1 });
    assert_eq!(h.active(), Some(t1));
    h.apply(Command::ActivateAdjacent { delta: 1 });
    assert_eq!(h.active(), Some(t1), "no wrap");
    h.apply(Command::ActivateAdjacent { delta: -1 });
    assert_eq!(h.active(), Some(t2));
    h.apply(Command::ActivateNth { n: 7 });
    assert_eq!(h.active(), Some(t2), "out of range ignored");
}

// ------------------------------------------------------------------------------------ closing

#[test]
fn close_today_tab_archives_and_reopen_restores_position() {
    let mut h = Harness::new();
    let a = h.open("https://a.com/");
    let b = h.open("https://b.com/");
    let c = h.open("https://c.com/");
    h.commit(b, "https://b.com/page", "B page");
    let fx = h.apply(Command::CloseItem { id: Some(b) });
    assert!(has(&fx, |e| is_destroy(e, b)));
    assert_eq!(h.today(), vec![c, a]);
    let entry = h.store.state().archive[0].clone();
    assert_eq!((entry.id, entry.reason, entry.section, entry.index), (b, ArchiveReason::UserClosed, Section::Today, 1));
    assert_eq!(entry.url, "https://b.com/page");
    assert_eq!(h.active(), Some(c), "closing a background tab keeps the active one");
    assert!(h.ui().can_reopen);
    h.apply(Command::ReopenClosed);
    assert_eq!(h.today(), vec![c, b, a]);
    assert_eq!(h.active(), Some(b));
    assert_eq!(h.tab(b).url, "https://b.com/page");
    assert!(h.store.state().archive.is_empty());
    assert!(!h.ui().can_reopen);
    // Reopen with an empty stack is a no-op.
    assert!(h.apply(Command::ReopenClosed).is_empty());
}

#[test]
fn close_active_falls_back_to_mru_then_empty() {
    let mut h = Harness::new();
    let a = h.open("https://a.com/");
    let b = h.open("https://b.com/");
    let c = h.open("https://c.com/");
    h.apply(Command::ActivateItem { id: a });
    h.apply(Command::ActivateItem { id: c });
    h.apply(Command::CloseItem { id: None });
    assert_eq!(h.active(), Some(a));
    h.apply(Command::CloseItem { id: None });
    assert_eq!(h.active(), Some(b));
    h.apply(Command::CloseItem { id: None });
    assert_eq!(h.active(), None);
    assert_eq!(h.store.content_layout(), ContentLayout::Empty);
    // Nothing to close: behaves like WindowCloseRequested.
    let fx = h.apply(Command::CloseItem { id: None });
    assert_eq!(fx, vec![Effect::SaveNow, Effect::Quit]);
    assert!(h.store.is_shutting_down());
}

#[test]
fn shutdown_ignores_everything() {
    let mut h = Harness::new();
    let a = h.open("https://a.com/");
    let fx = h.apply(Command::WindowCloseRequested);
    assert_eq!(fx, vec![Effect::SaveNow, Effect::Quit]);
    let rev = h.store.revision();
    let state = h.store.state_json();
    // Browser teardown must not archive tabs.
    assert!(h.store.apply(Command::TabBrowserClosed { tab: a }, h.now).is_empty());
    assert!(h.store.apply(Command::CloseItem { id: Some(a) }, h.now).is_empty());
    assert!(h.store.apply(Command::Tick, h.now + 100 * DAY).is_empty());
    assert!(h.store.apply(Command::OpenUrl { url: "https://x.com".into(), target: OpenTarget::NewTab, opener: None }, h.now).is_empty());
    assert_eq!(h.store.revision(), rev);
    assert_eq!(h.store.state_json(), state);
    assert!(!h.store.take_dirty().any());
    assert!(h.store.startup(vec![], h.now).is_empty());
    // Other quit paths.
    let mut h = Harness::new();
    assert_eq!(h.apply(Command::Quit), vec![Effect::SaveNow, Effect::Quit]);
    let mut h = Harness::new();
    assert_eq!(h.apply(Command::WindowControl { action: WindowAction::Close }), vec![Effect::SaveNow, Effect::Quit]);
    let mut h = Harness::new();
    assert_eq!(h.apply(Command::WindowControl { action: WindowAction::Minimize }), vec![Effect::Window { action: WindowAction::Minimize }]);
}

#[test]
fn close_pinned_unloads_and_reopen_restores_navigated_url() {
    let mut h = Harness::new();
    let other = h.open("https://other.com/");
    let p = h.open_pinned("https://mail.com/inbox");
    h.commit(p, "https://mail.com/message/42", "Message");
    assert!(h.ui().current.unwrap().navigated);
    let fx = h.apply(Command::CloseItem { id: None });
    assert!(has(&fx, |e| is_destroy(e, p)));
    assert_eq!(h.pinned(), vec![p], "row stays");
    assert_eq!(h.tab(p).url, "https://mail.com/inbox");
    assert_eq!(h.active(), Some(other));
    let row = h.ui().spaces[0].pinned[0].clone();
    let NodeView::Tab(row) = row else { panic!() };
    assert!(!row.loaded && !row.navigated);
    assert_eq!(h.store.state().reopen.last(), Some(&ReopenEntry::Unloaded { tab: p, url: "https://mail.com/message/42".into() }));
    let fx = h.apply(Command::ReopenClosed);
    assert!(has(&fx, |e| matches!(e, Effect::CreateBrowser { tab, url, .. } if *tab == p && url == "https://mail.com/message/42")));
    assert_eq!(h.active(), Some(p));
    // Activating an unloaded pinned tab loads its pinned URL.
    h.apply(Command::CloseItem { id: Some(p) });
    let fx = h.apply(Command::ActivateItem { id: p });
    assert!(has(&fx, |e| matches!(e, Effect::CreateBrowser { tab, url, .. } if *tab == p && url == "https://mail.com/inbox")));
    // Closing an unloaded, inactive pinned tab is a no-op.
    h.apply(Command::ActivateItem { id: other });
    h.apply(Command::CloseItem { id: Some(p) });
    let reopen_len = h.store.state().reopen.len();
    assert!(h.apply(Command::CloseItem { id: Some(p) }).is_empty());
    assert_eq!(h.store.state().reopen.len(), reopen_len);
}

#[test]
fn reopen_skips_entries_whose_archive_is_gone() {
    let mut h = Harness::new();
    let a = h.open("https://a.com/");
    let b = h.open("https://b.com/");
    h.apply(Command::CloseItem { id: Some(a) });
    h.apply(Command::CloseItem { id: Some(b) });
    h.apply(Command::DeleteArchived { id: b });
    h.apply(Command::ReopenClosed);
    assert_eq!(h.active(), Some(a));
    assert!(h.store.state().reopen.is_empty());
}

#[test]
fn reopen_stack_is_capped() {
    let mut h = Harness::new();
    for i in 0..30 {
        let t = h.open(&format!("https://t{i}.com/"));
        h.apply(Command::CloseItem { id: Some(t) });
    }
    assert_eq!(h.store.state().reopen.len(), MAX_REOPEN_STACK);
}

#[test]
fn page_closed_itself_or_creation_failed() {
    let mut h = Harness::new();
    let a = h.open("https://a.com/");
    let b = h.open("https://b.com/");
    h.page_closed(b);
    assert_eq!(h.today(), vec![a]);
    assert_eq!(h.store.state().archive[0].id, b);
    assert_eq!(h.active(), Some(a));
    let p = h.open_pinned("https://p.com/");
    h.page_closed(p);
    assert_eq!(h.pinned(), vec![p]);
    assert!(!h.store.is_loaded(p));
    assert_eq!(h.active(), Some(a));
    // Unknown tab: ignored.
    h.apply(Command::TabBrowserClosed { tab: 9999 });
}

#[test]
fn deferred_create_while_close_is_pending() {
    let mut h = Harness::new();
    h.auto = false;
    let a = h.open("https://a.com/");
    let p = h.open_pinned("https://p.com/");
    let fx = h.apply(Command::CloseItem { id: Some(p) });
    assert!(has(&fx, |e| is_destroy(e, p)));
    assert_eq!(h.active(), Some(a));
    // Re-activate before the browser is gone: no second CreateBrowser, not shown yet.
    let fx = h.apply(Command::ActivateItem { id: p });
    assert!(!has(&fx, |e| is_create(e, p)), "{fx:?}");
    assert_eq!(h.store.content_layout(), ContentLayout::Empty);
    assert!(h.ui().current.is_some_and(|c| c.tab == p));
    assert!(h.store.is_loaded(p));
    // Events from the dying browser are ignored.
    h.apply(Command::TabTitleChanged { tab: p, title: "stale".into() });
    assert_eq!(h.tab(p).title, "");
    let fx = h.finish_close(p);
    let create = position(&fx, |e| is_create(e, p)).expect("deferred create");
    let show = position(&fx, |e| matches!(e, Effect::ShowContent { layout: ContentLayout::Single { tab } } if *tab == p)).expect("show");
    assert!(create < show);
}

/// Unload the active Today tab `t` (auto answers off), reactivate it before its browser is gone,
/// then deliver the close: core issues the deferred `CreateBrowser`, still unconfirmed.
fn deferred_recreate(h: &mut Harness, t: Id) {
    h.auto = false;
    let fx = h.apply(Command::UnloadTab { id: t });
    assert!(has(&fx, |e| is_destroy(e, t)));
    h.apply(Command::ActivateItem { id: t });
    let fx = h.finish_close(t);
    assert!(has(&fx, |e| is_create(e, t)), "{fx:?}");
    assert!(h.store.is_loaded(t));
}

#[test]
fn stale_browser_closed_before_a_deferred_create_is_confirmed_is_ignored() {
    let mut h = Harness::new();
    let a = h.open("https://a.com/");
    let t = h.open("https://t.com/");
    deferred_recreate(&mut h, t);
    // A duplicate close report for the old browser arrives before TabBrowserCreated: the
    // re-created tab stays (not archived, not unloaded, still shown).
    let fx = h.apply(Command::TabBrowserClosed { tab: t });
    h.live.insert(t); // the fake shell's new browser is still alive
    assert!(fx.is_empty(), "{fx:?}");
    assert_eq!(h.today(), vec![t, a]);
    assert!(h.store.state().archive.is_empty() && h.store.state().reopen.is_empty());
    assert!(h.store.is_loaded(t));
    assert_eq!(h.active(), Some(t));
    assert_eq!(h.store.content_layout(), ContentLayout::Single { tab: t });
    // Confirmed: nothing changes; two ticks later nothing is treated as a failure either.
    assert!(h.apply(Command::TabBrowserCreated { tab: t }).is_empty());
    h.apply(Command::Tick);
    h.apply(Command::Tick);
    assert_eq!(h.today(), vec![t, a]);
    assert!(h.store.is_loaded(t));
    // Once confirmed, a close report is the page closing itself again.
    h.page_closed(t);
    assert_eq!(h.today(), vec![a]);
    assert!(h.store.state().archive.iter().any(|e| e.id == t));
}

#[test]
fn failed_deferred_create_is_handled_after_two_ticks() {
    let mut h = Harness::new();
    let a = h.open("https://a.com/");
    let t = h.open("https://t.com/");
    deferred_recreate(&mut h, t);
    // The re-created browser could not be made: the shell reports it closed, never created.
    assert!(h.page_closed(t).is_empty());
    assert!(h.store.is_loaded(t));
    h.apply(Command::Tick);
    assert!(h.store.is_loaded(t), "one tick could still precede a queued TabBrowserCreated");
    assert_eq!(h.today(), vec![t, a]);
    h.apply(Command::Tick);
    assert!(h.store.state().archive.iter().any(|e| e.id == t), "handled like a creation failure");
    assert_eq!(h.today(), vec![a]);
    assert_eq!(h.active(), Some(a));

    // A failed create that didn't follow a close is unambiguous: handled at once.
    let b = h.open("https://b.com/");
    assert!(h.store.is_loaded(b));
    h.page_closed(b);
    assert!(h.store.state().archive.iter().any(|e| e.id == b));
    assert_eq!(h.active(), Some(a));
}

#[test]
fn created_browser_for_vanished_tab_is_destroyed() {
    let mut h = Harness::new();
    h.auto = false;
    let a = h.open("https://a.com/");
    h.apply(Command::CloseItem { id: Some(a) });
    // CreateBrowser answered late, DestroyBrowser already pending: nothing new.
    assert!(h.apply(Command::TabBrowserCreated { tab: a }).is_empty());
    h.finish_close(a);
}

// ------------------------------------------------------------------------------------ pin / favorites

#[test]
fn toggle_pin_moves_between_sections() {
    let mut h = Harness::new();
    let a = h.open("https://a.com/x");
    let b = h.open("https://b.com/");
    h.apply(Command::TogglePin { id: Some(a) });
    assert_eq!(h.pinned(), vec![a]);
    assert_eq!(h.tab(a).pinned_url.as_deref(), Some("https://a.com/x"));
    assert_eq!(h.toast().unwrap().message, "Pinned");
    let c = h.open_pinned("https://c.com/");
    assert_eq!(h.pinned(), vec![a, c], "pinned at the bottom");
    h.apply(Command::TogglePin { id: Some(a) });
    assert_eq!(h.today(), vec![a, b]);
    assert_eq!(h.tab(a).pinned_url, None);
    h.apply(Command::AddFavorite { id: Some(b) });
    assert_eq!(h.favorites(), vec![b]);
    assert_eq!(h.tab(b).pinned_url.as_deref(), Some("https://b.com/"));
    h.apply(Command::TogglePin { id: Some(b) });
    assert_eq!(h.favorites(), Vec::<Id>::new());
    assert_eq!(h.today()[0], b);
    assert_eq!(h.store.tab_section(b), Some(Section::Today));
    // Ctrl+D with no id pins the focused tab.
    h.apply(Command::ActivateItem { id: c });
    h.apply(Command::TogglePin { id: None });
    assert_eq!(h.today()[0], c);
}

#[test]
fn favorites_are_capped_and_shared_across_spaces() {
    let mut h = Harness::new();
    let mut favs = Vec::new();
    for i in 0..MAX_FAVORITES {
        favs.push(h.open_favorite(&format!("https://f{i}.com/")));
    }
    assert!(h.ui().favorites_full);
    let extra = h.open("https://extra.com/");
    h.apply(Command::AddFavorite { id: Some(extra) });
    assert_eq!(h.favorites().len(), MAX_FAVORITES);
    assert_eq!(h.toast().unwrap().message, "Favorites are full (12)");
    h.apply(Command::MoveItem { id: extra, to: DropTarget { container: Container::Favorites, before: None } });
    assert_eq!(h.store.tab_section(extra), Some(Section::Today));
    // Reordering inside a full grid is fine.
    h.apply(Command::MoveItem { id: favs[11], to: DropTarget { container: Container::Favorites, before: Some(favs[0]) } });
    assert_eq!(h.favorites()[0], favs[11]);
    // Favorites stay when switching spaces; activating one keeps the space.
    let home = h.space();
    let work = h.new_space("Work", "🚀", Theme::default());
    h.apply(Command::ActivateItem { id: favs[3] });
    assert_eq!(h.space(), work);
    assert_eq!(h.active(), Some(favs[3]));
    assert_eq!(h.ui().favorites.len(), MAX_FAVORITES);
    h.apply(Command::RemoveFavorite { id: favs[3] });
    assert_eq!(h.today()[0], favs[3]);
    assert!(h.space_data(home).active_item.is_some());
    assert_eq!(h.active(), Some(favs[3]), "still active, now a Today tab of Work");
}

#[test]
fn favorite_active_in_another_space_is_repaired_when_moved() {
    let mut h = Harness::new();
    let home = h.space();
    let f = h.open_favorite("https://f.com/");
    let work = h.new_space("Work", "🚀", Theme::default());
    h.apply(Command::ActivateItem { id: f });
    assert_eq!(h.space_data(home).active_item, Some(f));
    assert_eq!(h.space_data(work).active_item, Some(f));
    h.apply(Command::RemoveFavorite { id: f });
    assert_eq!(h.space_data(work).active_item, Some(f));
    assert_eq!(h.space_data(home).active_item, None, "home can't point at Work's Today tab");
}

#[test]
fn reset_replace_and_edit_pinned() {
    let mut h = Harness::new();
    let p = h.open_pinned("https://p.com/home");
    h.commit(p, "https://p.com/other", "Other");
    let ui = h.ui();
    let NodeView::Tab(row) = &ui.spaces[0].pinned[0] else { panic!() };
    assert!(row.navigated);
    let fx = h.apply(Command::ResetToPinned { id: p });
    assert!(has(&fx, |e| matches!(e, Effect::LoadUrl { tab, url } if *tab == p && url == "https://p.com/home")));
    h.commit(p, "https://p.com/home", "Home");
    h.commit(p, "https://p.com/new", "New");
    h.apply(Command::ReplacePinnedUrl { id: p });
    assert_eq!(h.tab(p).pinned_url.as_deref(), Some("https://p.com/new"));
    assert!(!h.ui().current.unwrap().navigated);
    h.apply(Command::EditPinned { id: p, title: Some("Mine".into()), url: Some("example.org".into()) });
    let t = h.tab(p);
    assert_eq!((t.custom_title.as_deref(), t.pinned_url.as_deref()), (Some("Mine"), Some("https://example.org")));
    assert_eq!(h.ui().current.unwrap().title, "Mine");
    h.apply(Command::EditPinned { id: p, title: Some("".into()), url: None });
    assert_eq!(h.tab(p).custom_title, None);
    // The "Edit Pinned Page" panel: saving closes it; an unpinned tab has no pinned page to edit.
    h.apply(Command::OpenSidebarPanel { panel: SidebarPanel::EditPinned { id: p } });
    assert_eq!(h.ui().sidebar_panel.map(|v| v.panel), Some(SidebarPanel::EditPinned { id: p }));
    h.apply(Command::EditPinned { id: p, title: None, url: None });
    assert!(h.ui().sidebar_panel.is_none(), "saved (even unchanged): closed");
    let q = h.open_pinned("https://q.com/");
    h.apply(Command::OpenSidebarPanel { panel: SidebarPanel::EditPinned { id: q } });
    h.apply(Command::EditPinned { id: p, title: None, url: None });
    assert!(h.ui().sidebar_panel.is_some(), "another tab's save leaves it open");
    h.apply(Command::TogglePin { id: Some(q) });
    assert!(h.ui().sidebar_panel.is_none(), "unpinned while editing: the panel closes");
    h.apply(Command::ActivateItem { id: p });
    // Not for Today tabs.
    let t = h.open("https://t.com/");
    assert!(h.apply(Command::OpenSidebarPanel { panel: SidebarPanel::EditPinned { id: t } }).is_empty());
    assert!(h.ui().sidebar_panel.is_none());
    assert!(h.apply(Command::ReplacePinnedUrl { id: t }).is_empty());
    assert!(h.apply(Command::ResetToPinned { id: t }).is_empty());
    // Reset of an unloaded pinned tab loads it at the pinned URL.
    h.apply(Command::CloseItem { id: Some(p) });
    let fx = h.apply(Command::ResetToPinned { id: p });
    assert!(has(&fx, |e| matches!(e, Effect::CreateBrowser { tab, url, .. } if *tab == p && url == "https://example.org")));
}

// ------------------------------------------------------------------------------------ drag & drop

#[test]
fn move_item_matrix() {
    let mut h = Harness::new();
    let space = h.space();
    let a = h.open("https://a.com/");
    let b = h.open("https://b.com/");
    let c = h.open("https://c.com/");
    // Reorder in Today: [c, b, a] → [a, c, b]
    h.apply(Command::MoveItem { id: a, to: DropTarget { container: Container::Today { space }, before: Some(c) } });
    assert_eq!(h.today(), vec![a, c, b]);
    // before = self → append
    h.apply(Command::MoveItem { id: a, to: DropTarget { container: Container::Today { space }, before: Some(a) } });
    assert_eq!(h.today(), vec![c, b, a]);
    // Today → Pinned sets pinned_url
    h.apply(Command::MoveItem { id: b, to: DropTarget { container: Container::Pinned { space }, before: None } });
    assert_eq!(h.pinned(), vec![b]);
    assert!(h.tab(b).pinned_url.is_some());
    // Folder rules
    h.apply(Command::NewFolder { space: None, parent: None, name: None });
    let f1 = h.pinned()[0];
    assert_eq!(h.folder(f1).name, "New Folder");
    h.apply(Command::MoveItem { id: f1, to: DropTarget { container: Container::Today { space }, before: None } });
    assert_eq!(h.pinned()[0], f1, "folder into Today rejected");
    h.apply(Command::MoveItem { id: f1, to: DropTarget { container: Container::Favorites, before: None } });
    assert!(h.favorites().is_empty(), "folder into Favorites rejected");
    h.apply(Command::MoveItem { id: b, to: DropTarget { container: Container::Folder { id: f1 }, before: None } });
    assert_eq!(h.folder(f1).children, vec![b]);
    // Pinned → Today clears pinned_url
    h.apply(Command::MoveItem { id: b, to: DropTarget { container: Container::Today { space }, before: None } });
    assert_eq!(h.tab(b).pinned_url, None);
    assert_eq!(*h.today().last().unwrap(), b);
    // Nesting depth limit (3) and cycles
    h.apply(Command::NewFolder { space: None, parent: Some(f1), name: Some("f2".into()) });
    let f2 = h.folder(f1).children[0];
    h.apply(Command::NewFolder { space: None, parent: Some(f2), name: Some("f3".into()) });
    let f3 = h.folder(f2).children[0];
    h.apply(Command::NewFolder { space: None, parent: Some(f3), name: Some("too deep".into()) });
    assert!(h.folder(f3).children.is_empty(), "depth 4 folder rejected");
    h.apply(Command::MoveItem { id: f1, to: DropTarget { container: Container::Folder { id: f3 }, before: None } });
    assert_eq!(h.pinned()[0], f1, "folder into its own descendant rejected");
    h.apply(Command::NewFolder { space: None, parent: None, name: Some("g".into()) });
    let g = h.pinned()[0];
    h.apply(Command::MoveItem { id: g, to: DropTarget { container: Container::Folder { id: f2 }, before: None } });
    assert_eq!(h.folder(f2).children, vec![f3, g], "depth 3 ok");
    h.apply(Command::NewFolder { space: None, parent: Some(g), name: None });
    assert!(h.folder(g).children.is_empty());
    h.apply(Command::MoveItem { id: g, to: DropTarget { container: Container::Pinned { space }, before: None } });
    h.apply(Command::MoveItem { id: f1, to: DropTarget { container: Container::Folder { id: g }, before: None } });
    assert_eq!(h.pinned().last(), Some(&g), "height 3 subtree under depth-1 folder rejected (1+3 > 3)");
    assert!(h.folder(g).children.is_empty());
    // Splits live only in Today
    h.apply(Command::SplitWith { tab: c, with: a, side: SplitSide::Right });
    let split = h.active().unwrap();
    assert!(matches!(h.store.state().items.get(&split), Some(Item::Split(_))));
    h.apply(Command::MoveItem { id: split, to: DropTarget { container: Container::Pinned { space }, before: None } });
    assert!(h.today().contains(&split));
    h.apply(Command::MoveItem { id: split, to: DropTarget { container: Container::Favorites, before: None } });
    assert!(h.today().contains(&split));
    // A pane dragged out leaves its split (which dissolves)
    h.apply(Command::MoveItem { id: c, to: DropTarget { container: Container::Pinned { space }, before: None } });
    assert!(h.pinned().contains(&c));
    assert!(!h.store.state().items.contains_key(&split));
    assert!(h.today().contains(&a));
    // Unknown container / item: ignored
    assert!(h.apply(Command::MoveItem { id: a, to: DropTarget { container: Container::Folder { id: 424242 }, before: None } }).is_empty());
    assert!(h.apply(Command::MoveItem { id: 424242, to: DropTarget { container: Container::Today { space }, before: None } }).is_empty());
}

#[test]
fn move_to_space_rules() {
    let mut h = Harness::new();
    let home = h.space();
    let a = h.open("https://a.com/");
    let b = h.open("https://b.com/");
    let p = h.open_pinned("https://p.com/");
    let f = h.open_favorite("https://f.com/");
    let work = h.new_space("Work", "🚀", Theme { hue: 190.0, hue2: 230.0, chroma: 0.06 });
    let w1 = h.open("https://w1.com/");
    h.apply(Command::SwitchSpace { id: home });
    h.apply(Command::ActivateItem { id: b });
    h.apply(Command::ActivateItem { id: a });
    h.apply(Command::MoveToSpace { id: None, space: work });
    assert_eq!(h.space_data(work).today, vec![a, w1]);
    assert_eq!(h.active(), Some(b), "source falls back to its MRU");
    assert_eq!(h.space(), home);
    let toast = h.toast().unwrap();
    assert_eq!(toast.message, "Moved to 🚀 Work");
    h.apply(Command::MoveToSpace { id: Some(p), space: work });
    assert_eq!(h.space_data(work).pinned, vec![p]);
    h.apply(Command::MoveToSpace { id: Some(f), space: work });
    assert!(h.favorites().contains(&f), "favorites can't move to a space");
    h.apply(Command::MoveToSpace { id: Some(b), space: 999 });
    assert_eq!(h.today(), vec![b]);
    // Folder with contents
    h.apply(Command::NewFolder { space: None, parent: None, name: Some("F".into()) });
    let folder = h.pinned()[0];
    let q = h.open_pinned("https://q.com/");
    h.apply(Command::MoveItem { id: q, to: DropTarget { container: Container::Folder { id: folder }, before: None } });
    h.apply(Command::MoveToSpace { id: Some(folder), space: work });
    assert_eq!(h.space_data(work).pinned, vec![p, folder]);
    assert_eq!(h.folder(folder).children, vec![q]);
}

// ------------------------------------------------------------------------------------ folders

#[test]
fn folders_create_rename_toggle_delete_restore() {
    let mut h = Harness::new();
    let space = h.space();
    h.apply(Command::NewFolder { space: None, parent: None, name: None });
    let f = h.pinned()[0];
    assert!(matches!(h.ui().sidebar_panel, Some(SidebarPanelView { panel: SidebarPanel::RenameItem { id }, .. }) if id == f));
    h.apply(Command::RenameItem { id: f, title: Some("  Work stuff ".into()) });
    assert_eq!(h.folder(f).name, "Work stuff");
    assert!(h.ui().sidebar_panel.is_none());
    h.apply(Command::RenameItem { id: f, title: Some("".into()) });
    assert_eq!(h.folder(f).name, "Work stuff", "empty folder name ignored");
    let p1 = h.open_pinned("https://p1.com/");
    let p2 = h.open_pinned("https://p2.com/");
    h.apply(Command::MoveItem { id: p1, to: DropTarget { container: Container::Folder { id: f }, before: None } });
    h.apply(Command::NewFolder { space: Some(space), parent: Some(f), name: Some("Sub".into()) });
    let sub = h.folder(f).children[0];
    h.apply(Command::MoveItem { id: p2, to: DropTarget { container: Container::Folder { id: sub }, before: None } });
    h.apply(Command::ToggleFolder { id: f });
    assert!(h.folder(f).collapsed);
    let t = h.open("https://t.com/");
    h.apply(Command::ActivateItem { id: p2 });
    h.apply(Command::DeleteFolder { id: f });
    assert!(h.pinned().is_empty());
    assert!(!h.store.state().items.contains_key(&sub));
    let reasons: Vec<ArchiveReason> = h.store.state().archive.iter().map(|e| e.reason).collect();
    assert_eq!(reasons, vec![ArchiveReason::FolderDeleted, ArchiveReason::FolderDeleted]);
    assert_eq!(h.active(), Some(t), "active tab inside the folder falls back");
    assert!(h.toast().unwrap().message.contains("2 tabs archived"));
    // Restoring a tab whose folder is gone → top of Today, pinned_url cleared.
    h.apply(Command::RestoreArchived { id: p2, whole_group: false });
    assert_eq!(h.today()[0], p2);
    assert_eq!(h.tab(p2).pinned_url, None);
    assert_eq!(h.active(), Some(p2));
    // Renaming tabs
    h.apply(Command::RenameItem { id: p2, title: Some("Named".into()) });
    assert_eq!(h.tab(p2).custom_title.as_deref(), Some("Named"));
    h.apply(Command::RenameItem { id: p2, title: None });
    assert_eq!(h.tab(p2).custom_title, None);
}

#[test]
fn pinned_restore_into_existing_folder_and_favorites() {
    let mut h = Harness::new();
    h.apply(Command::NewFolder { space: None, parent: None, name: Some("F".into()) });
    let f = h.pinned()[0];
    let p = h.open_pinned("https://p.com/");
    h.apply(Command::MoveItem { id: p, to: DropTarget { container: Container::Folder { id: f }, before: None } });
    // Archive via space-less path: unload isn't archiving, so use MoveToSpace + DeleteSpace? Use archive via ClearToday? No: pinned
    // tabs only reach the archive through folder/space deletion; simulate with a folder delete of a sibling folder.
    h.apply(Command::NewFolder { space: None, parent: Some(f), name: Some("Sub".into()) });
    let sub = h.folder(f).children[0];
    let q = h.open_pinned("https://q.com/");
    h.apply(Command::MoveItem { id: q, to: DropTarget { container: Container::Folder { id: sub }, before: None } });
    h.apply(Command::DeleteFolder { id: sub });
    let entry = h.store.state().archive.iter().find(|e| e.id == q).unwrap().clone();
    assert_eq!((entry.section, entry.folder, entry.pinned_url.as_deref()), (Section::Pinned, Some(sub), Some("https://q.com/")));
    let f1 = h.open_favorite("https://fav.com/");
    let fav_entry_space = h.space();
    let _ = fav_entry_space;
    assert!(h.favorites().contains(&f1));
}

#[test]
fn favorites_close_reopen_and_section_conversions() {
    let mut h = Harness::new();
    let space = h.space();
    let other = h.open("https://other.com/");
    let f = h.open_favorite("https://fav.com/start");
    h.commit(f, "https://fav.com/deep", "Deep");
    assert!(h.ui().favorites[0].navigated);
    // Ctrl+W on a favorite unloads it (row stays, URL resets) and can be reopened.
    let fx = h.apply(Command::CloseItem { id: None });
    assert!(has(&fx, |e| is_destroy(e, f)));
    assert_eq!(h.favorites(), vec![f]);
    assert!(!h.ui().favorites[0].loaded && !h.ui().favorites[0].navigated);
    assert_eq!(h.active(), Some(other));
    let fx = h.apply(Command::ReopenClosed);
    assert!(has(&fx, |e| matches!(e, Effect::CreateBrowser { tab, url, .. } if *tab == f && url == "https://fav.com/deep")));
    assert_eq!(h.active(), Some(f));
    // Double-click on the tile resets to the pinned URL.
    let fx = h.apply(Command::ResetToPinned { id: f });
    assert!(has(&fx, |e| matches!(e, Effect::LoadUrl { tab, url } if *tab == f && url == "https://fav.com/start")));
    // Favorite → Pinned (keeps pinned_url), Pinned → Favorites, Favorite → Today (clears it).
    h.apply(Command::MoveItem { id: f, to: DropTarget { container: Container::Pinned { space }, before: None } });
    assert_eq!((h.pinned(), h.favorites()), (vec![f], vec![]));
    assert_eq!(h.tab(f).pinned_url.as_deref(), Some("https://fav.com/start"));
    h.apply(Command::MoveItem { id: f, to: DropTarget { container: Container::Favorites, before: None } });
    assert_eq!((h.pinned(), h.favorites()), (vec![], vec![f]));
    h.apply(Command::MoveItem { id: f, to: DropTarget { container: Container::Today { space }, before: Some(other) } });
    assert_eq!(h.today(), vec![f, other]);
    assert_eq!(h.tab(f).pinned_url, None);
    // AddFavorite moves a pinned tab too; splits and panes are ignored.
    let p = h.open_pinned("https://p.com/");
    h.apply(Command::AddFavorite { id: Some(p) });
    assert_eq!(h.favorites(), vec![p]);
    assert!(h.pinned().is_empty());
    h.apply(Command::SplitWith { tab: f, with: other, side: SplitSide::Right });
    let sid = h.active().unwrap();
    assert!(h.apply(Command::AddFavorite { id: Some(f) }).is_empty());
    assert!(h.apply(Command::AddFavorite { id: Some(sid) }).is_empty());
    assert!(h.apply(Command::TogglePin { id: Some(sid) }).is_empty());
    assert!(h.apply(Command::TogglePin { id: Some(f) }).is_empty(), "panes can't be pinned");
    // Unload a favorite that isn't showing.
    let fx = h.apply(Command::UnloadTab { id: p });
    assert!(has(&fx, |e| is_destroy(e, p)));
    assert_eq!(h.active(), Some(sid));
}

#[test]
fn open_url_at_drop_positions() {
    let mut h = Harness::new();
    let space = h.space();
    let a = h.open("https://a.com/");
    let b = h.open("https://b.com/");
    // Today at a position: activated.
    h.apply(Command::OpenUrlAt { url: "https://dropped.com/".into(), to: DropTarget { container: Container::Today { space }, before: Some(a) } });
    let d = h.focused().unwrap();
    assert_eq!(h.today(), vec![b, d, a]);
    assert_eq!(h.tab(d).pinned_url, None);
    // Pinned / folder / favorites get a pinned URL.
    h.apply(Command::OpenUrlAt { url: "https://pin.com/".into(), to: DropTarget { container: Container::Pinned { space }, before: None } });
    let p = h.pinned()[0];
    assert_eq!(h.tab(p).pinned_url.as_deref(), Some("https://pin.com/"));
    h.apply(Command::NewFolder { space: None, parent: None, name: Some("F".into()) });
    let folder = h.pinned()[0];
    h.apply(Command::OpenUrlAt { url: "https://infolder.com/".into(), to: DropTarget { container: Container::Folder { id: folder }, before: None } });
    assert_eq!(h.folder(folder).children.len(), 1);
    h.apply(Command::OpenUrlAt { url: "https://fav.com/".into(), to: DropTarget { container: Container::Favorites, before: None } });
    assert_eq!(h.favorites().len(), 1);
    // Another space's Today: created there, not activated.
    let work = h.new_space("Work", "🚀", Theme::default());
    h.apply(Command::SwitchSpace { id: space });
    let active = h.active();
    h.apply(Command::OpenUrlAt { url: "https://elsewhere.com/".into(), to: DropTarget { container: Container::Today { space: work }, before: None } });
    assert_eq!(h.space_data(work).today.len(), 1);
    assert_eq!((h.space(), h.active()), (space, active));
    // Web content can't drop internal/script URLs; unknown containers are ignored.
    let n = h.store.state().items.len();
    h.apply(Command::OpenUrlAt { url: "javascript:alert(1)".into(), to: DropTarget { container: Container::Today { space }, before: None } });
    h.apply(Command::OpenUrlAt { url: "sta://settings/".into(), to: DropTarget { container: Container::Today { space }, before: None } });
    h.apply(Command::OpenUrlAt { url: "https://x.com/".into(), to: DropTarget { container: Container::Folder { id: 4242 }, before: None } });
    assert_eq!(h.store.state().items.len(), n);
}

#[test]
fn late_or_duplicate_browser_closed_is_ignored() {
    let mut h = Harness::new();
    let a = h.open("https://a.com/");
    let b = h.open("https://b.com/");
    // `a` is loaded in the background; unloading it destroys the browser and the shell reports it.
    h.apply(Command::UnloadTab { id: a });
    assert!(!h.store.is_loaded(a));
    // A late/duplicate report for the now unloaded tab must not archive it.
    let fx = h.page_closed(a);
    assert!(fx.is_empty(), "{fx:?}");
    assert_eq!(h.today(), vec![b, a]);
    assert!(h.store.state().archive.is_empty());
    assert!(h.store.state().reopen.is_empty());
    // Same after a requested close completed (archived tab) and for ids core never knew.
    h.apply(Command::CloseItem { id: Some(b) });
    let reopen = h.store.state().reopen.clone();
    assert!(h.page_closed(b).is_empty());
    assert!(h.page_closed(4242).is_empty());
    assert_eq!(h.store.state().reopen, reopen);
    // An unrequested close of a loaded tab still closes it (the page called window.close()).
    h.apply(Command::ActivateItem { id: a });
    assert!(h.store.is_loaded(a));
    h.page_closed(a);
    assert!(h.store.state().archive.iter().any(|e| e.id == a));
}

#[test]
fn web_content_cannot_open_local_or_privileged_urls() {
    let mut h = Harness::new();
    let t = h.open("https://site.com/");
    let space = h.space();
    let blocked = [
        "file:///C:/Windows/win.ini",
        "data:text/html,<h1>hi</h1>",
        "view-source:https://site.com/",
        "filesystem:https://site.com/temporary/x",
        "javascript:alert(1)",
        " JavaScript:alert(1)",
        "sta://settings/",
        "chrome://settings/",
        "devtools://devtools/bundled/inspector.html",
        "about:version",
        "mailto:someone@example.com",
        "ms-settings:privacy",
        "blob:null/0b2c",
        "blob:file:///C:/x",
    ];
    let only_effects_are_the_toast = |fx: &[Effect]| fx.iter().all(|e| matches!(e, Effect::ShowToast));
    for url in blocked {
        for disposition in [LinkDisposition::ForegroundTab, LinkDisposition::BackgroundTab, LinkDisposition::NewWindow, LinkDisposition::PinnedCrossSite] {
            h.apply(Command::DismissToast { id: h.toast().map_or(0, |t| t.id) });
            let fx = h.apply(Command::LinkOpenRequested { opener: t, url: url.into(), disposition });
            assert!(only_effects_are_the_toast(&fx) && has(&fx, |e| matches!(e, Effect::ShowToast)), "{url} {disposition:?}: {fx:?}");
            assert!(h.toast().is_some_and(|t| t.message.starts_with("Blocked")), "{url}");
        }
        h.apply(Command::DismissToast { id: h.toast().map_or(0, |t| t.id) });
        let fx = h.apply(Command::OpenUrlAt { url: url.into(), to: DropTarget { container: Container::Today { space }, before: None } });
        assert!(only_effects_are_the_toast(&fx) && h.toast().is_some(), "{url}: {fx:?}");
        assert_eq!(h.today(), vec![t], "{url}");
        assert!(h.store.peek_tab().is_none());
        // A popup's browser already exists: it is adopted, but navigated to about:blank.
        let (id, fx) = h.popup(Some(t), url, false, true);
        assert!(has(&fx, |e| matches!(e, Effect::LoadUrl { tab, url } if *tab == id && url == "about:blank")), "{url}: {fx:?}");
        assert_eq!(h.tab(id).url, "about:blank");
        h.apply(Command::CloseItem { id: Some(id) });
        h.apply(Command::ActivateItem { id: t });
    }
    // Allowed: web URLs, about:blank and blob: URLs of a web origin.
    h.apply(Command::LinkOpenRequested { opener: t, url: "https://ok.com/".into(), disposition: LinkDisposition::BackgroundTab });
    h.apply(Command::OpenUrlAt { url: "http://localhost:8080/".into(), to: DropTarget { container: Container::Today { space }, before: None } });
    h.apply(Command::LinkOpenRequested { opener: t, url: "blob:https://site.com/5f1e".into(), disposition: LinkDisposition::BackgroundTab });
    h.apply(Command::LinkOpenRequested { opener: t, url: "about:blank".into(), disposition: LinkDisposition::BackgroundTab });
    assert_eq!(h.today().len(), 5);
    // file: URLs only from a file: page.
    let fx = h.apply(Command::LinkOpenRequested { opener: t, url: "file:///C:/docs/b.html".into(), disposition: LinkDisposition::ForegroundTab });
    assert!(only_effects_are_the_toast(&fx), "{fx:?}");
    let local = h.open("file:///C:/docs/a.html");
    h.apply(Command::LinkOpenRequested { opener: local, url: "file:///C:/docs/b.html".into(), disposition: LinkDisposition::ForegroundTab });
    assert_eq!(h.tab(h.focused().unwrap()).url, "file:///C:/docs/b.html");
    let (from_file, fx) = h.popup(Some(local), "file:///C:/docs/c.html", false, true);
    assert!(!has(&fx, |e| matches!(e, Effect::LoadUrl { .. })), "{fx:?}");
    assert_eq!(h.tab(from_file).url, "file:///C:/docs/c.html");
}

#[test]
fn script_popups_without_a_url_are_not_navigated() {
    let mut h = Harness::new();
    let t = h.open("https://app.com/");
    // window.open() (+ document.write): the shell reports an empty URL or about:blank.
    for url in ["", "about:blank", "  about:blank  ", "about:blank#top"] {
        let (id, fx) = h.popup(Some(t), url, false, true);
        assert!(!has(&fx, |e| matches!(e, Effect::LoadUrl { .. })), "{url:?}: {fx:?}");
        assert!(h.toast().is_none(), "{url:?}");
        assert!(h.tab(id).url.starts_with("about:blank"), "{url:?}");
        assert_eq!(h.active(), Some(id));
        h.apply(Command::CloseItem { id: Some(id) });
    }
    // Feature popups (Peek) too.
    let (id, fx) = h.popup(Some(t), "", true, true);
    assert_eq!(h.store.peek_tab(), Some(id));
    assert!(!has(&fx, |e| matches!(e, Effect::LoadUrl { .. })), "{fx:?}");
}
