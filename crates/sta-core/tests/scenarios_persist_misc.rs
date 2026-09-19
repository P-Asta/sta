//! Persistence (round trip, session restore, tolerant loading and repair) and the remaining
//! commands: sidebar, window, settings, boosts, duplicate/view source, mute/unload, page events.

mod common;
use sta_core::*;
use common::*;
use sta_core::store::LoadReport;
use serde_json::json;

// ------------------------------------------------------------------------------------ persistence

/// A populated profile exercising every persisted structure.
fn rich_harness() -> Harness {
    let mut h = Harness::new();
    let home = h.space();
    let fav = h.open_favorite("https://fav.com/");
    h.apply(Command::NewFolder { space: None, parent: None, name: Some("Work".into()) });
    let folder = h.pinned()[0];
    let p = h.open_pinned("https://pinned.com/");
    h.apply(Command::MoveItem { id: p, to: DropTarget { container: Container::Folder { id: folder }, before: None } });
    h.apply(Command::ToggleFolder { id: folder });
    let a = h.open("https://a.com/");
    let b = h.open("https://b.com/");
    h.apply(Command::SplitWith { tab: b, with: a, side: SplitSide::Right });
    h.apply(Command::SetSplitFractions { id: b, fractions: vec![0.4, 0.6] });
    let c = h.open("https://c.com/");
    h.commit(c, "https://c.com/page", "C page");
    h.apply(Command::RenameItem { id: c, title: Some("My C".into()) });
    h.apply(Command::ToggleMute { id: Some(c) });
    let gone = h.open("https://gone.com/");
    h.apply(Command::CloseItem { id: Some(gone) });
    h.apply(Command::NewSpace { name: "Play".into(), icon: "🎮".into(), theme: Theme { hue: 50.0, hue2: 20.0, chroma: 0.07 } });
    h.open("https://play.com/");
    h.apply(Command::SwitchSpace { id: home });
    h.apply(Command::ActivateItem { id: fav });
    h.apply(Command::UpsertBoost { boost: Boost { id: 0, name: "B".into(), host: "a.com".into(), css: "body{}".into(), ..Default::default() } });
    h.apply(Command::PermissionRequested { id: 1, tab: fav, origin: "https://fav.com".into(), kinds: vec![PermissionKind::Camera] });
    h.apply(Command::ResolvePermission { id: 1, allow: true, remember: true });
    h.apply(Command::UpdateSettings { patch: SettingsPatch { search_engine: Some(SearchEngineId::Kagi), archive_after_hours: Some(168), ..Default::default() } });
    h.apply(Command::SetSidebarWidth { width: 300 });
    h.apply(Command::WindowStateChanged { maximized: false, fullscreen: false, focused: true, bounds: Some(Rect { x: 10, y: 20, width: 1200, height: 800 }) });
    h
}

#[test]
fn save_load_round_trip_and_session_restore() {
    let h = rich_harness();
    let state = h.store.state_json();
    let history = h.store.history_json();
    let (mut loaded, report) = Store::load(Some(&state), Some(&history), h.now);
    assert_eq!(report, LoadReport::default());
    assert!(!loaded.take_dirty().any(), "a clean load is not dirty");
    assert_eq!(loaded.state_json(), state);
    assert_eq!(loaded.history_json(), history);
    loaded.check_invariants().unwrap();
    // Session restore: only the active item of the active space loads (a favorite here).
    let fav = h.favorites()[0];
    let h2 = Harness::start(loaded, Vec::new());
    let creates: Vec<(Id, String)> =
        h2.history.iter().filter_map(|e| if let Effect::CreateBrowser { tab, url, .. } = e { Some((*tab, url.clone())) } else { None }).collect();
    assert_eq!(creates, vec![(fav, "https://fav.com/".to_string())]);
    assert_eq!(h2.store.window_state().sidebar_width, 300);
    assert!(h2.history.contains(&Effect::SetSidebar { visible: true, width: 300, floating: false }));
    let ui = h2.ui();
    assert!(ui.spaces[0].today.iter().all(|n| match n {
        NodeView::Tab(t) => !t.loaded,
        NodeView::Split(s) => s.panes.iter().all(|p| !p.loaded),
        NodeView::Folder(_) => false,
    }));
    assert!(ui.can_reopen);
    // Muted state is applied on creation.
    let c = h2.today().into_iter().find(|t| h2.store.tab(*t).is_some_and(|t| t.muted)).unwrap();
    let mut h2 = h2;
    let fx = h2.apply(Command::ActivateItem { id: c });
    assert!(has(&fx, |e| matches!(e, Effect::CreateBrowser { tab, muted: true, url, .. } if *tab == c && url == "https://c.com/page")));
    // Missing files = fresh profile.
    let (mut fresh, report) = Store::load(None, None, T0);
    assert_eq!(report, LoadReport::default());
    assert_eq!(fresh.state().spaces.len(), 1);
    assert!(fresh.take_dirty().state);
}

#[test]
fn navigated_pinned_tabs_reopen_at_pinned_url() {
    let mut h = Harness::new();
    let p = h.open_pinned("https://p.com/home");
    h.commit(p, "https://p.com/elsewhere", "Elsewhere");
    assert_eq!(h.tab(p).url, "https://p.com/elsewhere");
    let (store, report) = Store::load(Some(&h.store.state_json()), None, h.now);
    assert!(!report.state_corrupt);
    let h2 = Harness::start(store, Vec::new());
    assert_eq!(h2.tab(p).url, "https://p.com/home");
    assert!(h2.history.iter().any(|e| matches!(e, Effect::CreateBrowser { tab, url, .. } if *tab == p && url == "https://p.com/home")));
}

#[test]
fn startup_opens_command_line_urls() {
    let mut h = Harness::new();
    let a = h.open("https://a.com/");
    let (store, _) = Store::load(Some(&h.store.state_json()), None, h.now);
    let h2 = Harness::start(store, vec!["https://x.com/".into(), "https://y.com/".into()]);
    let today = h2.today();
    assert_eq!(today.len(), 3);
    let (y, x) = (today[0], today[1]);
    assert_eq!((h2.tab(y).url.as_str(), h2.tab(x).url.as_str()), ("https://y.com/", "https://x.com/"));
    assert_eq!(h2.active(), Some(y));
    assert!(h2.store.is_loaded(x) && h2.store.is_loaded(y), "every command-line URL loads");
    assert!(!h2.store.is_loaded(a));
    let chrome = position(&h2.history, |e| matches!(e, Effect::SetChrome { .. })).unwrap();
    let sidebar = position(&h2.history, |e| matches!(e, Effect::SetSidebar { .. })).unwrap();
    let show = position(&h2.history, |e| matches!(e, Effect::ShowContent { .. })).unwrap();
    assert!(chrome < sidebar && sidebar < show);
}

#[test]
fn corrupt_state_is_repaired() {
    let raw = json!({
        "version": 1,
        "nextId": 3,
        "settings": { "searchEngine": "nope", "archiveAfterHours": 13, "peekEnabled": false },
        "window": { "activeSpace": 99, "sidebarWidth": 1000, "mru": [10, 10, 11, 555], "bounds": { "x": 0, "y": 0, "width": -5, "height": 10 } },
        "spaces": [
            { "id": 1, "name": "Home", "icon": "🏠", "theme": { "hue": 300, "hue2": 340, "chroma": 5 }, "pinned": [20, 21, 30], "today": [10, 11, 40, 22, 777], "activeItem": 12 },
            { "id": 1, "name": "", "icon": "", "pinned": [], "today": [11], "activeItem": 11 },
            "garbage"
        ],
        "favorites": [13, 13, "x", 20],
        "items": {
            "10": { "kind": "tab", "id": 10, "url": "https://a.com/", "title": 5, "pinnedUrl": "https://a.com/", "opener": 9999 },
            "11": { "kind": "tab", "id": 11, "url": "https://b.com/" },
            "12": { "kind": "tab", "id": 12, "url": "https://pane.com/" },
            "13": { "kind": "tab", "id": 13, "url": "https://fav.com/x" },
            "20": { "kind": "folder", "id": 20, "name": "F", "children": [21, 23] },
            "21": { "kind": "tab", "id": 21, "url": "https://p.com/nav", "pinnedUrl": "https://p.com/" },
            "22": { "kind": "folder", "id": 22, "name": "InToday", "children": [] },
            "23": { "kind": "tab", "url": "https://q.com/" },
            "30": { "kind": "split", "id": 30, "panes": [12, 31], "fractions": [0.5, 0.5], "focused": 7 },
            "40": { "kind": "split", "id": 40, "panes": [41, 42], "fractions": [0.3, 0.9], "focused": 1 },
            "41": { "kind": "tab", "id": 41, "url": "https://s1.com/" },
            "42": { "kind": "tab", "id": 42, "url": "https://s2.com/" },
            "50": { "kind": "widget", "id": 50 },
            "60": { "kind": "tab", "id": 60, "url": "https://orphan.com/" },
            "61": "not an object"
        },
        "archive": [ { "id": 70, "url": "https://old.com/", "archivedAt": 5 }, { "id": 70, "url": "dup" }, 3 ],
        "boosts": [ { "id": 0, "name": "b", "host": "x.com" } ],
        "reopen": [ { "type": "archived", "archiveId": 70 }, { "type": "archived", "archiveId": 71 }, { "type": "bogus" } ],
        "sitePermissions": [
            { "origin": "https://Meet.com/", "kind": "camera", "allow": true },
            { "origin": "https://meet.com", "kind": "camera", "allow": false }
        ]
    });
    let (mut store, report) = Store::load(Some(&raw.to_string()), None, T0);
    assert!(report.state_corrupt, "{report:?}");
    assert!(!report.history_corrupt);
    assert!(!report.warnings.is_empty());
    store.check_invariants().unwrap_or_else(|e| panic!("{e:?}\n{:#?}", report.warnings));
    assert!(store.take_dirty().state, "repairs must be saved");
    let st = store.state();
    // Settings / window
    assert_eq!(st.settings.search_engine, SearchEngineId::Google);
    assert_eq!(st.settings.archive_after_hours, 12);
    assert!(!st.settings.peek_enabled);
    assert_eq!(st.window.sidebar_width, SIDEBAR_MAX_WIDTH);
    assert_eq!(st.window.bounds, None);
    assert_eq!(st.window.mru, vec![10, 11]);
    // Spaces
    assert_eq!(st.spaces.len(), 2);
    assert_eq!(st.window.active_space, 1);
    assert_eq!(st.spaces[0].theme.chroma, 0.08);
    let second = &st.spaces[1];
    assert!(second.id != 1 && second.id >= 71);
    assert_eq!((second.name.as_str(), second.icon.as_str()), ("Space", "✨"));
    assert!(second.today.is_empty(), "tab 11 is already listed in Home");
    assert_eq!(second.active_item, None);
    // Containers
    assert_eq!(st.favorites, vec![13]);
    assert_eq!(st.spaces[0].pinned, vec![20, 22]);
    assert_eq!(st.spaces[0].today, vec![12, 10, 11, 40], "dissolved split first, then Today");
    assert_eq!(st.spaces[0].active_item, Some(12));
    let Some(Item::Folder(f)) = st.items.get(&20) else { panic!() };
    assert_eq!(f.children, vec![21, 23]);
    let Some(Item::Split(s)) = st.items.get(&40) else { panic!() };
    assert_eq!(s.focused, 1);
    assert!((s.fractions[0] - 0.25).abs() < 1e-5 && (s.fractions[1] - 0.75).abs() < 1e-5, "{:?}", s.fractions);
    assert!(!st.items.contains_key(&30) && !st.items.contains_key(&60) && !st.items.contains_key(&50));
    // Tabs
    let tab = |id: Id| match st.items.get(&id) {
        Some(Item::Tab(t)) => t.clone(),
        other => panic!("{id}: {other:?}"),
    };
    assert_eq!((tab(10).title.as_str(), tab(10).pinned_url.clone(), tab(10).opener), ("", None, None));
    assert_eq!(tab(13).pinned_url.as_deref(), Some("https://fav.com/x"));
    assert_eq!((tab(21).url.as_str(), tab(21).pinned_url.as_deref()), ("https://p.com/", Some("https://p.com/")));
    assert_eq!(tab(23).id, 23, "id taken from the key");
    assert_eq!(tab(23).pinned_url.as_deref(), Some("https://q.com/"));
    // Archive, boosts, reopen, permissions
    assert_eq!(st.archive.len(), 1);
    assert_eq!(st.archive[0].url, "https://old.com/");
    assert!(st.boosts[0].id > 70);
    assert_eq!(st.reopen, vec![ReopenEntry::Archived { archive_id: 70 }]);
    assert_eq!(st.site_permissions, vec![SitePermission { origin: "https://meet.com".into(), kind: PermissionKind::Camera, allow: false }]);
    assert!(st.next_id > st.boosts[0].id);
    // It starts and works.
    let mut h = Harness::start(store, Vec::new());
    assert_eq!(h.store.content_layout(), ContentLayout::Single { tab: 12 });
    let t = h.open("https://new.com/");
    assert!(t > 72);
}

#[test]
fn unparsable_files_start_fresh() {
    for text in ["{not json", "[1,2,3]", "null", ""] {
        let (mut store, report) = Store::load(Some(text), Some("{\"urls\": 5}"), T0);
        assert!(report.state_corrupt, "{text}");
        assert!(report.history_corrupt);
        assert_eq!(store.state().spaces.len(), 1);
        assert!(store.history().urls.is_empty());
        let dirty = store.take_dirty();
        assert!(dirty.state && dirty.history);
        store.check_invariants().unwrap();
    }
    // Empty object: valid, repaired (a space is created) but not corrupt.
    let (store, report) = Store::load(Some("{}"), None, T0);
    assert!(!report.state_corrupt);
    assert_eq!(store.state().spaces.len(), 1);
    // No spaces but items: orphans dropped.
    let raw = json!({"items": {"5": {"kind": "tab", "id": 5, "url": "https://x.com"}}, "nextId": 1});
    let (store, report) = Store::load(Some(&raw.to_string()), None, T0);
    assert!(store.state().items.is_empty());
    assert!(store.state().next_id > 5);
    assert!(report.warnings.iter().any(|w| w.contains("orphan")));
}

#[test]
fn favorites_overflow_and_nesting_repair() {
    let mut items = serde_json::Map::new();
    let mut favorites = Vec::new();
    for id in 100..114u64 {
        items.insert(id.to_string(), json!({"kind": "tab", "id": id, "url": format!("https://f{id}.com/"), "pinnedUrl": format!("https://f{id}.com/")}));
        favorites.push(id);
    }
    // Folders nested 4 deep: the deepest is flattened.
    for (id, child) in [(200u64, 201u64), (201, 202), (202, 203), (203, 204)] {
        items.insert(id.to_string(), json!({"kind": "folder", "id": id, "children": [child]}));
    }
    items.insert("204".into(), json!({"kind": "tab", "id": 204, "url": "https://deep.com/", "pinnedUrl": "https://deep.com/"}));
    let raw = json!({"nextId": 300, "favorites": favorites, "items": items, "spaces": [{"id": 1, "pinned": [200], "today": [], "activeItem": 113}], "window": {"activeSpace": 1}});
    let (store, report) = Store::load(Some(&raw.to_string()), None, T0);
    store.check_invariants().unwrap();
    let st = store.state();
    assert_eq!(st.favorites.len(), MAX_FAVORITES);
    assert_eq!(st.spaces[0].today, vec![112, 113], "overflow favorites go to Today");
    let Some(Item::Tab(t)) = st.items.get(&112) else { panic!() };
    assert_eq!(t.pinned_url, None);
    let Some(Item::Folder(f)) = st.items.get(&202) else { panic!() };
    assert_eq!(f.children, vec![204], "folder at depth 4 flattened into its parent");
    assert!(!st.items.contains_key(&203));
    assert_eq!(st.spaces[0].active_item, Some(113));
    assert!(!report.state_corrupt);
}

#[test]
fn corrupt_history_entries() {
    let raw = json!({"urls": [
        {"url": "https://a.com/", "title": "A", "visitCount": "x", "visits": [{"at": 1, "transition": "typed"}]},
        5,
        {"url": ""},
        {"url": "https://a.com/", "title": "dup"},
        {"url": "https://b.com/", "title": "B", "visitCount": 2, "typedCount": 0, "lastVisitAt": T0 - 10, "visits": [{"at": T0 - 10, "transition": "link"}], "frecency": 99999}
    ]});
    let (store, report) = Store::load(None, Some(&raw.to_string()), T0);
    assert!(report.history_corrupt);
    assert!(!report.state_corrupt);
    let urls = &store.history().urls;
    assert_eq!(urls.iter().map(|u| u.url.as_str()).collect::<Vec<_>>(), ["https://a.com/", "https://b.com/"]);
    assert_eq!(urls[0].visit_count, 1, "at least the recorded visits");
    assert_eq!(urls[1].frecency, 100, "frecency is recomputed (2 / min(2, 10) × 100)");
    let rows = store.history_list("", 10, T0);
    assert_eq!(rows[0].url, "https://b.com/");
    assert_eq!(store.history_list("a.com", 10, T0)[0].title, "A");
}

#[test]
fn history_commands_and_titles() {
    let mut h = Harness::new();
    let a = h.open("https://a.com/");
    h.commit(a, "https://a.com/1", "One");
    h.commit(a, "https://a.com/1#section", "One");
    assert_eq!(h.store.history().get("https://a.com/1#section").unwrap().title, "One", "same-document keeps the title");
    h.commit(a, "https://a.com/2", "Two");
    h.apply(Command::TabAddressChanged { tab: a, url: "sta://settings/".into() });
    assert!(h.store.history().get("sta://settings/").is_none());
    let rev = h.ui().history_revision;
    assert_eq!(h.store.history_list("", 10, h.now).len(), 3);
    h.apply(Command::DeleteHistoryEntry { url: "https://a.com/2".into() });
    assert!(h.ui().history_revision > rev);
    assert!(h.store.history().get("https://a.com/2").is_none());
    assert!(h.apply(Command::DeleteHistoryEntry { url: "https://nope/".into() }).is_empty());
    h.apply(Command::ClearHistory);
    assert!(h.store.history().urls.is_empty());
    let rows = h.store.history_list("", 10, h.now);
    assert!(rows.is_empty());
}

#[test]
fn history_visits_follow_browser_commits() {
    let mut h = Harness::new();
    // The first commit of a new tab is a visit even though Tab.url already equals it.
    let a = h.open("https://a.com/");
    h.apply(Command::TabAddressChanged { tab: a, url: "https://a.com/".into() });
    let visit = |h: &Harness, url: &str| h.store.history().get(url).map(|u| (u.visit_count, u.typed_count, u.visits.last().map(|v| v.transition)));
    assert_eq!(visit(&h, "https://a.com/"), Some((1, 0, Some(Transition::Link))));
    // A reload commits the same URL again: no visit.
    h.apply(Command::TabAddressChanged { tab: a, url: "https://a.com/".into() });
    assert_eq!(visit(&h, "https://a.com/").unwrap().0, 1);
    // Typing the current URL again is a (typed) visit.
    h.apply(Command::CommitOmnibox { command: Box::new(Command::OpenInput { text: "https://a.com/".into(), target: OpenTarget::CurrentTab }), alt: false });
    h.apply(Command::TabAddressChanged { tab: a, url: "https://a.com/".into() });
    assert_eq!(visit(&h, "https://a.com/"), Some((2, 1, Some(Transition::Typed))));
    // Lazy reload of an unloaded tab: the new browser's first commit is not a visit, later
    // navigations in it are.
    h.apply(Command::TabTitleChanged { tab: a, title: "A".into() });
    let b = h.open("https://b.com/");
    h.apply(Command::UnloadTab { id: a });
    h.apply(Command::ActivateItem { id: a });
    h.apply(Command::TabAddressChanged { tab: a, url: "https://a.com/".into() });
    assert_eq!(visit(&h, "https://a.com/").unwrap().0, 2);
    assert_eq!(h.store.history().get("https://a.com/").unwrap().title, "A");
    h.apply(Command::TabAddressChanged { tab: a, url: "https://a.com/next".into() });
    assert_eq!(visit(&h, "https://a.com/next").unwrap().0, 1);
    // Pinned tabs committing their pinned URL count as bookmark visits.
    let p = h.open_pinned("https://p.com/home");
    h.apply(Command::TabAddressChanged { tab: p, url: "https://p.com/home".into() });
    assert_eq!(visit(&h, "https://p.com/home").unwrap().2, Some(Transition::Bookmark));
    h.apply(Command::TabAddressChanged { tab: p, url: "https://p.com/other".into() });
    assert_eq!(visit(&h, "https://p.com/other").unwrap().2, Some(Transition::Link));
    // A failed load has no visit; a successful retry of the same URL clears the error.
    h.apply(Command::ActivateItem { id: b });
    h.apply(Command::Navigate { tab: None, url: "https://down.example.com/".into() });
    h.apply(Command::TabLoadFailed { tab: b, url: "https://down.example.com/".into(), error_code: -105, error_text: "".into() });
    assert_eq!(h.ui().current.unwrap().load_error.as_deref(), Some("Error -105"));
    assert!(visit(&h, "https://down.example.com/").is_none());
    h.apply(Command::TabAddressChanged { tab: b, url: "https://down.example.com/".into() });
    assert!(h.ui().current.unwrap().load_error.is_none());
    assert_eq!(visit(&h, "https://down.example.com/").unwrap().0, 1);
}

// ------------------------------------------------------------------------------------ misc commands

#[test]
fn sidebar_toggle_width_and_panels() {
    let mut h = Harness::new();
    let set = |visible: bool, floating: bool| vec![Effect::SetSidebar { visible, width: SIDEBAR_DEFAULT_WIDTH, floating }];
    assert_eq!(h.apply(Command::ToggleSidebar), set(false, false));
    assert!(!h.ui().window.sidebar_visible);
    // Opening a transient panel floats the hidden sidebar for the panel only (see
    // `sidebar_panel_reveals_hidden_sidebar_transiently`).
    let fx = h.apply(Command::ToggleSidebarPanel { panel: SidebarPanel::Downloads });
    assert_eq!(fx, set(false, true));
    let panel = h.ui().sidebar_panel.unwrap();
    assert_eq!(panel.panel, SidebarPanel::Downloads);
    let fx = h.apply(Command::ToggleSidebarPanel { panel: SidebarPanel::Downloads });
    assert_eq!(fx, set(false, false));
    assert!(h.ui().sidebar_panel.is_none());
    h.apply(Command::ToggleSidebar);
    h.apply(Command::OpenSidebarPanel { panel: SidebarPanel::AppMenu });
    let seq = h.ui().sidebar_panel.unwrap().seq;
    h.apply(Command::OpenSidebarPanel { panel: SidebarPanel::AppMenu });
    assert!(h.ui().sidebar_panel.unwrap().seq > seq, "re-issued intent bumps seq");
    // Hiding the sidebar closes its panel.
    h.apply(Command::ToggleSidebar);
    assert!(h.ui().sidebar_panel.is_none());
    h.apply(Command::ToggleSidebar);
    h.apply(Command::OpenSidebarPanel { panel: SidebarPanel::NewSpace });
    assert!(h.apply(Command::CloseSidebarPanel).is_empty());
    assert!(h.ui().sidebar_panel.is_none());
    assert!(h.apply(Command::OpenSidebarPanel { panel: SidebarPanel::RenameItem { id: 999 } }).is_empty());
    assert!(h.ui().sidebar_panel.is_none());
    // Width is clamped; an out-of-range live width is corrected.
    let docked = |width: u32| vec![Effect::SetSidebar { visible: true, width, floating: false }];
    assert_eq!(h.apply(Command::SetSidebarWidth { width: 320 }), docked(320));
    assert_eq!(h.apply(Command::SetSidebarWidth { width: 9999 }), docked(SIDEBAR_MAX_WIDTH));
    assert_eq!(h.apply(Command::SetSidebarWidth { width: 5000 }), docked(SIDEBAR_MAX_WIDTH));
    assert!(h.apply(Command::SetSidebarWidth { width: SIDEBAR_MAX_WIDTH }).is_empty());
    assert_eq!(h.ui().window.sidebar_width, SIDEBAR_MAX_WIDTH);
    // The width of a hidden sidebar changes too (the floating sidebar uses it).
    h.apply(Command::ToggleSidebar);
    assert_eq!(h.apply(Command::SetSidebarWidth { width: 300 }), vec![Effect::SetSidebar { visible: false, width: 300, floating: false }]);
}

#[test]
fn window_state_bounds_and_revision() {
    let mut h = Harness::new();
    let rev = h.store.revision();
    h.store.take_dirty();
    let bounds = Rect { x: 5, y: 6, width: 1000, height: 700 };
    assert!(h.apply(Command::WindowStateChanged { maximized: false, fullscreen: false, focused: true, bounds: Some(bounds) }).is_empty());
    assert_eq!(h.store.revision(), rev, "bounds-only changes don't bump revision");
    assert_eq!(h.store.window_state().bounds, Some(bounds));
    h.apply(Command::WindowStateChanged { maximized: true, fullscreen: false, focused: true, bounds: Some(Rect { x: 0, y: 0, width: 1920, height: 1080 }) });
    assert!(h.store.revision() > rev);
    assert!(h.ui().window.maximized && h.store.window_state().maximized);
    assert_eq!(h.store.window_state().bounds, Some(bounds), "maximized bounds are not the restore bounds");
    h.apply(Command::WindowStateChanged { maximized: false, fullscreen: true, focused: true, bounds: None });
    assert!(h.ui().window.fullscreen);
    assert_eq!(h.apply(Command::WindowControl { action: WindowAction::ToggleMaximize }), vec![Effect::Window { action: WindowAction::ToggleMaximize }]);
}

#[test]
fn settings_patch() {
    let mut h = Harness::new();
    h.apply(Command::UpdateSettings {
        patch: SettingsPatch {
            search_engine: Some(SearchEngineId::Custom),
            custom_search_url: Some("  https://s.example/?q={q}  ".into()),
            download_dir: Some("D:\\dl".into()),
            ask_download_location: Some(true),
            search_suggestions: Some(false),
            ..Default::default()
        },
    });
    let s = h.store.settings().clone();
    assert_eq!((s.search_engine, s.custom_search_url.as_str(), s.download_dir.as_deref()), (SearchEngineId::Custom, "https://s.example/?q={q}", Some("D:\\dl")));
    assert!(s.ask_download_location && !s.search_suggestions, "search suggestions default to on and can be turned off");
    let ui = h.ui();
    assert_eq!(ui.search_engines.len(), 8);
    assert_eq!(ui.search_engines[7].url, "https://s.example/?q={q}");
    h.apply(Command::OpenInput { text: "hello world".into(), target: OpenTarget::NewTab });
    assert_eq!(h.tab(h.focused().unwrap()).url, "https://s.example/?q=hello%20world");
    h.apply(Command::UpdateSettings { patch: SettingsPatch { download_dir: Some("   ".into()), ..Default::default() } });
    assert_eq!(h.store.settings().download_dir, None);
}

#[test]
fn boosts_upsert_toggle_delete_reload_matching_tabs() {
    let mut h = Harness::new();
    let docs = h.open("https://docs.example.com/page");
    let other = h.open("https://other.com/");
    let fx = h.apply(Command::UpsertBoost { boost: Boost { id: 0, name: "Wide".into(), host: " https://www.Example.com/path ".into(), css: "body{}".into(), ..Default::default() } });
    let id = h.store.state().boosts[0].id;
    assert_eq!(h.store.boost(id).unwrap().host, "example.com");
    assert_eq!(fx, vec![Effect::Reload { tab: docs, ignore_cache: false }]);
    assert_eq!(h.store.boosts_for_url("https://docs.example.com/x").len(), 1);
    assert!(h.store.boosts_for_url("https://other.com/").is_empty());
    h.apply(Command::ActivateItem { id: docs });
    assert_eq!(h.ui().current.unwrap().boosts[0].id, id);
    // Toggle: reload, disabled boosts still listed for the pill.
    let fx = h.apply(Command::ToggleBoost { id });
    assert_eq!(fx, vec![Effect::Reload { tab: docs, ignore_cache: false }]);
    assert!(h.store.boosts_for_url("https://docs.example.com/").is_empty());
    assert!(!h.ui().current.unwrap().boosts[0].enabled);
    // Changing the host reloads tabs matching the old and the new host.
    let mut b = h.store.boost(id).unwrap();
    b.host = "other.com".into();
    b.enabled = true;
    let fx = h.apply(Command::UpsertBoost { boost: b });
    assert!(has(&fx, |e| matches!(e, Effect::Reload { tab, .. } if *tab == docs)) && has(&fx, |e| matches!(e, Effect::Reload { tab, .. } if *tab == other)));
    assert_eq!(h.store.state().boosts.len(), 1);
    let fx = h.apply(Command::DeleteBoost { id });
    assert_eq!(fx, vec![Effect::Reload { tab: other, ignore_cache: false }]);
    assert!(h.store.state().boosts.is_empty());
    assert!(h.apply(Command::DeleteBoost { id }).is_empty());
    // New boost for the focused site opens the boosts page (internal browser).
    h.apply(Command::ActivateItem { id: other });
    let fx = h.apply(Command::NewBoostForSite { tab: None });
    let boost = h.store.state().boosts[0].clone();
    assert_eq!((boost.host.as_str(), boost.name.as_str()), ("other.com", "other.com"));
    let page = h.focused().unwrap();
    assert!(has(&fx, |e| matches!(e, Effect::CreateBrowser { tab, url, internal: true, .. } if *tab == page && *url == format!("sta://boosts/?id={}", boost.id))));
    assert_eq!(h.ui().current.unwrap().pill, "Boosts");
    assert!(h.ui().current.unwrap().internal);
    // Internal pages have no boosts.
    assert!(h.apply(Command::NewBoostForSite { tab: None }).is_empty());
}

#[test]
fn duplicate_view_source_mute_unload() {
    let mut h = Harness::new();
    let a = h.open("https://a.com/");
    let b = h.open("https://b.com/");
    h.commit(b, "https://b.com/x", "B");
    h.apply(Command::TabFaviconChanged { tab: b, url: Some("https://b.com/favicon.ico".into()) });
    // Duplicate below a Today tab.
    h.apply(Command::DuplicateTab { id: None });
    let dup = h.focused().unwrap();
    assert_eq!(h.today(), vec![b, dup, a]);
    let t = h.tab(dup);
    assert_eq!((t.url.as_str(), t.title.as_str(), t.favicon.as_deref()), ("https://b.com/x", "B", Some("https://b.com/favicon.ico")));
    // Duplicate a pinned tab: top of Today.
    let p = h.open_pinned("https://p.com/");
    h.apply(Command::DuplicateTab { id: Some(p) });
    let pd = h.focused().unwrap();
    assert_eq!(h.today()[0], pd);
    assert_eq!(h.tab(pd).pinned_url, None);
    // View source below the focused tab.
    h.apply(Command::ActivateItem { id: b });
    h.apply(Command::ViewSource);
    let vs = h.focused().unwrap();
    assert_eq!(h.tab(vs).url, "view-source:https://b.com/x");
    assert_eq!(h.today()[h.today().iter().position(|x| *x == b).unwrap() + 1], vs);
    assert_eq!(h.ui().current.unwrap().host, "b.com");
    assert!(h.apply(Command::ViewSource).is_empty(), "no view-source of view-source");
    // Mute.
    h.apply(Command::ActivateItem { id: b });
    assert_eq!(h.apply(Command::ToggleMute { id: None }), vec![Effect::SetAudioMuted { tab: b, muted: true }]);
    assert!(h.tab(b).muted && h.ui().current.unwrap().muted);
    // Muting an unloaded tab just flips the flag.
    h.apply(Command::UnloadTab { id: a });
    assert!(h.apply(Command::ToggleMute { id: Some(a) }).is_empty());
    assert!(h.tab(a).muted);
    // Unloading the visible Today tab falls back and keeps the row.
    h.apply(Command::ActivateItem { id: dup });
    let fx = h.apply(Command::UnloadTab { id: dup });
    assert!(has(&fx, |e| is_destroy(e, dup)));
    assert!(h.today().contains(&dup));
    assert_eq!(h.active(), Some(b));
    assert!(h.apply(Command::UnloadTab { id: dup }).is_empty(), "already unloaded");
    let fx = h.apply(Command::ActivateItem { id: dup });
    assert!(has(&fx, |e| matches!(e, Effect::CreateBrowser { tab, url, .. } if *tab == dup && url == "https://b.com/x")));
}

#[test]
fn page_state_events_feed_the_view() {
    let mut h = Harness::new();
    let a = h.open("http://plain.example.com/");
    let ui = h.ui();
    let cur = ui.current.unwrap();
    assert!(!cur.secure);
    assert_eq!((cur.pill.as_str(), cur.host.as_str(), cur.title.as_str()), ("plain.example.com", "plain.example.com", "plain.example.com"));
    h.apply(Command::TabLoadingStateChanged { tab: a, loading: true, can_go_back: true, can_go_forward: false });
    h.apply(Command::TabLoadProgress { tab: a, progress: 0.4 });
    let cur = h.ui().current.unwrap();
    assert!(cur.loading && cur.can_go_back && !cur.can_go_forward);
    assert!((cur.progress - 0.4).abs() < 1e-6);
    assert!(h.ui().spaces[0].today.iter().any(|n| matches!(n, NodeView::Tab(t) if t.loading)));
    h.apply(Command::TabLoadProgress { tab: a, progress: f32::NAN });
    assert_eq!(h.ui().current.unwrap().progress, 0.0);
    h.apply(Command::TabAudioChanged { tab: a, audible: true });
    assert!(h.ui().current.unwrap().audible);
    // Cross-document navigation while loading clears audible.
    h.apply(Command::TabAddressChanged { tab: a, url: "https://secure.example.com/".into() });
    let cur = h.ui().current.unwrap();
    assert!(!cur.audible && cur.secure);
    h.apply(Command::TabLoadingStateChanged { tab: a, loading: false, can_go_back: true, can_go_forward: false });
    assert_eq!(h.ui().current.unwrap().progress, 1.0);
    h.apply(Command::TabFaviconChanged { tab: a, url: Some("  ".into()) });
    assert_eq!(h.tab(a).favicon, None);
    // Unknown tabs are ignored.
    for cmd in [
        Command::TabTitleChanged { tab: 777, title: "x".into() },
        Command::TabAddressChanged { tab: 777, url: "https://x.com/".into() },
        Command::TabLoadingStateChanged { tab: 777, loading: true, can_go_back: false, can_go_forward: false },
        Command::TabCrashed { tab: 777 },
        Command::TabFocused { tab: 777 },
        Command::TabBrowserCreated { tab: 777 },
    ] {
        assert!(h.apply(cmd).is_empty());
    }
    // file: and internal pills.
    h.apply(Command::Navigate { tab: None, url: "file:///C:/Users/me/Report%202026.pdf".into() });
    h.apply(Command::TabAddressChanged { tab: a, url: "file:///C:/Users/me/Report%202026.pdf".into() });
    let cur = h.ui().current.unwrap();
    assert_eq!(cur.pill, "Report 2026.pdf");
    assert!(cur.secure && !cur.internal);
    // alloc_id never collides.
    let id = h.store.alloc_id();
    assert!(id > a && h.store.tab(id).is_none());
}

#[test]
fn out_of_range_ids_and_timestamps_are_repaired_without_overflow() {
    const HUGE: u64 = MAX_ID + 2;
    let state = json!({
        "version": 1,
        "nextId": u64::MAX,
        "spaces": [{ "id": u64::MAX, "name": "Huge", "icon": "🧪", "pinned": [], "today": [3, HUGE], "activeItem": 3, "createdAt": i64::MIN }],
        "items": {
            "3": { "kind": "tab", "id": 3, "url": "https://a.com/", "createdAt": i64::MIN, "lastActiveAt": i64::MAX },
            HUGE.to_string(): { "kind": "tab", "id": HUGE, "url": "https://huge.com/" }
        },
        "archive": [
            { "id": 4, "url": "https://b.com/", "archivedAt": i64::MAX, "section": "today", "space": u64::MAX, "index": u64::MAX,
              "split": { "group": u64::MAX, "orientation": "vertical", "fractions": [0.5, 0.5], "focused": u64::MAX, "paneIndex": 0, "panes": [4, u64::MAX] } },
            { "id": 5, "url": "https://c.com/", "archivedAt": i64::MAX - 1, "section": "today",
              "split": { "group": u64::MAX, "orientation": "vertical", "fractions": [0.5, 0.5], "focused": 0, "paneIndex": 1 } },
            { "id": u64::MAX, "url": "https://d.com/", "archivedAt": 1 }
        ],
        "boosts": [{ "id": u64::MAX, "name": "B", "host": "a.com", "createdAt": i64::MIN, "updatedAt": i64::MAX }],
        "window": { "activeSpace": u64::MAX, "mru": [3, u64::MAX] },
        "reopen": [{ "type": "archived", "archiveId": u64::MAX }, { "type": "split", "archiveIds": [4, 5] }]
    });
    let history = json!({ "urls": [{
        "url": "https://a.com/", "title": "A", "visitCount": 2, "typedCount": 1, "lastVisitAt": i64::MAX,
        "visits": [{ "at": i64::MIN, "transition": "link" }, { "at": i64::MAX, "transition": "typed" }]
    }]});
    let (mut store, report) = Store::load(Some(&state.to_string()), Some(&history.to_string()), T0);
    assert!(store.check_invariants().is_ok(), "{:?}", store.check_invariants());
    assert!(report.warnings.iter().any(|w| w.contains("nextId")), "{report:?}");
    assert!(report.warnings.iter().any(|w| w.contains("renumbered")), "{report:?}");
    // Every id that appears anywhere is renumbered in order and nothing is dropped for its id:
    // 3 → 1, 4 → 2, 5 → 3, MAX_ID + 2 → 4, u64::MAX → 5.
    let st = store.state().clone();
    assert!(st.next_id > 5 && st.next_id <= 16, "nextId {}", st.next_id);
    assert_eq!(st.spaces[0].id, 5);
    assert_eq!(st.window.active_space, st.spaces[0].id);
    assert!(st.boosts.iter().all(|b| b.id <= 16 && b.id != 5), "the boost that shared the space's id gets its own");
    assert_eq!(st.items.keys().copied().collect::<Vec<_>>(), vec![1, 4]);
    assert_eq!(st.spaces[0].today, vec![1, 4]);
    assert_eq!(st.spaces[0].active_item, Some(1));
    assert_eq!(st.window.mru, vec![1]);
    assert_eq!(st.archive.iter().map(|e| e.id).collect::<Vec<_>>(), vec![2, 3, 5]);
    let groups: Vec<Id> = st.archive.iter().filter_map(|e| e.split.as_ref().map(|s| s.group)).collect();
    assert_eq!(groups, vec![5, 5]);
    assert_eq!(st.archive[0].split.as_ref().unwrap().panes, vec![2, 5]);
    assert_eq!(st.reopen, vec![ReopenEntry::Archived { archive_id: 5 }, ReopenEntry::Split { archive_ids: vec![2, 3] }]);
    let tab = store.tab(1).unwrap();
    assert_eq!((tab.created_at, tab.last_active_at), (0, MAX_MILLIS));
    assert_eq!((st.spaces[0].created_at, st.boosts[0].created_at, st.boosts[0].updated_at), (0, 0, MAX_MILLIS));
    assert!(st.archive.iter().all(|e| (0..=MAX_MILLIS).contains(&e.archived_at)));
    let h_url = store.history().get("https://a.com/").unwrap();
    assert_eq!(h_url.last_visit_at, MAX_MILLIS);
    assert_eq!(h_url.visits.iter().map(|v| v.at).collect::<Vec<_>>(), vec![0, MAX_MILLIS]);
    let dirty = store.take_dirty();
    assert!(dirty.state && dirty.history, "repairs are saved");
    let id = store.alloc_id();
    assert!(id > 5 && id <= 16, "{id}");

    // Everything doing id or time arithmetic runs on the repaired data without overflowing.
    let mut h = Harness::start(store, Vec::new());
    let r = h.store.omnibox(&OmniboxRequest { text: "a.com".into(), mode: CommandBarMode::NewTab, split_side: None, prevent_inline_autocomplete: false, suggestions: Vec::new(), seq: 1 }, h.now);
    assert!(!r.results.is_empty());
    assert!(!h.store.history_list("", 10, h.now).is_empty());
    // The tab kept from the out-of-range id was idle since 1970: startup auto-archives it.
    assert_eq!(h.store.archive_list().len(), 4);
    h.apply(Command::RestoreArchived { id: 3, whole_group: true });
    let sid = h.active().unwrap();
    assert_eq!(h.split(sid).panes, vec![2, 3]);
    h.apply(Command::TabAddressChanged { tab: 2, url: "https://b.com/x".into() });
    h.apply(Command::CloseItem { id: Some(sid) });
    h.advance(40 * DAY);
    h.apply(Command::Tick);
    h.apply(Command::OpenUrl { url: "https://new.com/".into(), target: OpenTarget::NewTab, opener: None });
    let (_, report) = Store::load(Some(&h.store.state_json()), Some(&h.store.history_json()), h.now);
    assert!(!report.state_corrupt && !report.history_corrupt, "{report:?}");
    assert!(report.warnings.iter().all(|w| !w.contains("range") && !w.contains("reassigned") && !w.contains("nextId")), "{report:?}");
}

/// Rewrite every id of a saved `state.json` with `f` (test-side list of the model's id fields).
fn map_state_ids(state: &mut serde_json::Value, f: &mut dyn FnMut(u64) -> u64) {
    use serde_json::Value;
    type F<'a> = &'a mut dyn FnMut(u64) -> u64;
    // `get_mut`, never `IndexMut`: indexing a missing key would insert a `null` field.
    fn id(v: &mut Value, f: F) {
        if let Some(n) = v.as_u64() {
            *v = json!(f(n));
        }
    }
    fn one(obj: &mut Value, key: &str, f: F) {
        if let Some(v) = obj.get_mut(key) {
            id(v, f);
        }
    }
    fn all(obj: &mut Value, key: &str, f: F) {
        if let Some(a) = obj.get_mut(key).and_then(Value::as_array_mut) {
            a.iter_mut().for_each(|x| id(x, &mut *f));
        }
    }
    fn each<'v>(obj: &'v mut Value, key: &str) -> Vec<&'v mut Value> {
        obj.get_mut(key).and_then(Value::as_array_mut).map(|a| a.iter_mut().collect()).unwrap_or_default()
    }
    all(state, "favorites", f);
    for s in each(state, "spaces") {
        one(s, "id", f);
        all(s, "pinned", f);
        all(s, "today", f);
        one(s, "activeItem", f);
    }
    let items = std::mem::take(state.get_mut("items").and_then(Value::as_object_mut).unwrap());
    let mut renamed = serde_json::Map::new();
    for (key, mut item) in items {
        one(&mut item, "id", f);
        one(&mut item, "opener", f);
        all(&mut item, "children", f);
        all(&mut item, "panes", f);
        let key = key.parse::<u64>().map(|k| f(k).to_string()).unwrap_or(key);
        renamed.insert(key, item);
    }
    *state.get_mut("items").unwrap() = Value::Object(renamed);
    for e in each(state, "archive") {
        one(e, "id", f);
        one(e, "space", f);
        one(e, "folder", f);
        if let Some(split) = e.get_mut("split").filter(|s| s.is_object()) {
            one(split, "group", f);
            all(split, "panes", f);
        }
    }
    for b in each(state, "boosts") {
        one(b, "id", f);
    }
    if let Some(w) = state.get_mut("window") {
        one(w, "activeSpace", f);
        all(w, "mru", f);
    }
    for r in each(state, "reopen") {
        one(r, "archiveId", f);
        one(r, "tab", f);
        all(r, "archiveIds", f);
    }
}

#[test]
fn ids_near_the_limit_are_renumbered_compactly_on_load() {
    use std::collections::{BTreeMap, BTreeSet};
    let h = rich_harness();
    let original: serde_json::Value = serde_json::from_str(&h.store.state_json()).unwrap();
    // Expected result: the same profile with its ids renumbered 1..=n, keeping their order.
    let mut ids = BTreeSet::new();
    map_state_ids(&mut original.clone(), &mut |id| {
        ids.insert(id);
        id
    });
    let rank: BTreeMap<u64, u64> = ids.iter().copied().zip(1..).collect();
    let mut expected = original.clone();
    map_state_ids(&mut expected, &mut |id| rank[&id]);
    expected["nextId"] = json!(rank.len() as u64 + 1);
    assert!(!original["reopen"].as_array().unwrap().is_empty() && !original["archive"].as_array().unwrap().is_empty());

    for offset in [MAX_ID / 2, MAX_ID - 10_000, u64::MAX - 10_000] {
        let mut huge = original.clone();
        map_state_ids(&mut huge, &mut |id| id + offset);
        huge["nextId"] = json!(original["nextId"].as_u64().unwrap() + offset);
        let (store, report) = Store::load(Some(&huge.to_string()), Some(&h.store.history_json()), h.now);
        assert!(!report.state_corrupt, "{offset}: {report:?}");
        assert!(report.warnings.iter().any(|w| w.contains("renumbered")), "{offset}: {report:?}");
        store.check_invariants().unwrap();
        let loaded: serde_json::Value = serde_json::from_str(&store.state_json()).unwrap();
        assert_eq!(loaded, expected, "offset {offset}: every reference follows the renumbering");
        // The renumbered profile works and saves clean.
        let mut h2 = Harness::start(store, Vec::new());
        assert_eq!(h2.store.alloc_id(), rank.len() as u64 + 1);
        let fav = h2.favorites()[0];
        h2.apply(Command::ReopenClosed);
        h2.apply(Command::ActivateItem { id: fav });
        let (_, report) = Store::load(Some(&h2.store.state_json()), None, h2.now);
        assert!(report.warnings.is_empty(), "{offset}: {report:?}");
    }

    // Small ids but a counter close to the limit: the counter restarts above the largest id.
    let mut high_next = original.clone();
    high_next["nextId"] = json!(MAX_ID - 5);
    let (mut store, report) = Store::load(Some(&high_next.to_string()), None, h.now);
    assert!(report.warnings.iter().any(|w| w.contains("nextId")), "{report:?}");
    assert_eq!(store.state().next_id, ids.last().unwrap() + 1);
    assert_eq!(serde_json::from_str::<serde_json::Value>(&store.state_json()).unwrap()["items"], original["items"]);
    assert!(store.alloc_id() <= original["nextId"].as_u64().unwrap());
}

#[test]
fn alloc_id_never_exceeds_max_id() {
    let mut h = Harness::new();
    let t = h.open("https://a.com/");
    // A popup adopted under the largest id exhausts the counter.
    h.live.insert(MAX_ID);
    h.apply(Command::PopupAdopted { tab: MAX_ID, opener: Some(t), url: "https://popup.com/".into(), popup: false, foreground: false });
    assert!(h.store.tab(MAX_ID).is_some());
    assert_eq!(h.store.state().next_id, MAX_ID + 1);
    let a = h.store.alloc_id();
    let b = h.store.alloc_id();
    for id in [a, b] {
        assert!(id > t && id < MAX_ID, "{id}");
        assert!(h.store.tab(id).is_none());
    }
    assert_ne!(a, b);
    let n = h.open("https://new.com/");
    assert!(n < MAX_ID && n != a && n != b && n != t, "{n}");
    h.apply(Command::SplitWith { tab: n, with: t, side: SplitSide::Right });
    let sid = h.active().unwrap();
    assert!(sid < MAX_ID && ![a, b, n, t].contains(&sid), "{sid}");
    // Ids beyond MAX_ID are never adopted.
    assert!(h.apply(Command::PopupAdopted { tab: MAX_ID + 1, opener: Some(t), url: "https://x.com/".into(), popup: false, foreground: true }).is_empty());
    assert!(h.store.tab(MAX_ID + 1).is_none());
    // The next load renumbers everything compactly.
    let (store, report) = Store::load(Some(&h.store.state_json()), None, h.now);
    assert!(report.warnings.iter().any(|w| w.contains("renumbered")), "{report:?}");
    assert!(store.state().next_id < 16, "{}", store.state().next_id);
    store.check_invariants().unwrap();
}

#[test]
fn docked_sidebar_panels_change_no_placement() {
    let mut h = Harness::new();
    let t = h.open("https://t.com/");
    let p = h.open_pinned("https://p.com/");
    let space = h.space();
    for panel in [
        SidebarPanel::Downloads,
        SidebarPanel::AppMenu,
        SidebarPanel::NewSpace,
        SidebarPanel::EditSpace { id: space },
        SidebarPanel::RenameItem { id: t },
        SidebarPanel::EditPinned { id: p },
    ] {
        let fx = h.apply(Command::OpenSidebarPanel { panel: panel.clone() });
        assert!(!has(&fx, |e| matches!(e, Effect::SetSidebar { .. })), "{panel:?}: {fx:?}");
        assert!(h.ui().window.sidebar_visible);
        let fx = h.apply(Command::CloseSidebarPanel);
        assert!(!has(&fx, |e| matches!(e, Effect::SetSidebar { .. })), "{panel:?}: {fx:?}");
    }
}

#[test]
fn sidebar_panel_reveals_hidden_sidebar_transiently() {
    let mut h = Harness::new();
    h.apply(Command::ToggleSidebar);
    let saved = h.store.state_json();
    let set = |visible: bool, floating: bool| vec![Effect::SetSidebar { visible, width: SIDEBAR_DEFAULT_WIDTH, floating }];
    // Ctrl+J while hidden: the sidebar floats for the transient panel (the page keeps keyboard
    // focus); not docked, nothing to save.
    assert_eq!(h.apply(Command::ToggleSidebarPanel { panel: SidebarPanel::Downloads }), set(false, true));
    assert!(!h.ui().window.sidebar_visible);
    assert!(!h.store.window_state().sidebar_visible);
    assert_eq!(h.store.state_json(), saved);
    // Switching between transient panels keeps it floating; closing the panel un-pins it.
    assert!(h.apply(Command::OpenSidebarPanel { panel: SidebarPanel::AppMenu }).is_empty());
    assert_eq!(h.apply(Command::CloseSidebarPanel), set(false, false));
    assert!(!h.ui().window.sidebar_visible);
    assert_eq!(h.store.state_json(), saved);
    // A panel that holds input docks it while open (it needs keyboard focus); switching to a
    // transient panel floats it again, and back.
    let t = h.open("https://t.com/");
    assert_eq!(h.apply(Command::OpenSidebarPanel { panel: SidebarPanel::RenameItem { id: t } }), set(true, false));
    assert!(h.ui().window.sidebar_visible && !h.store.window_state().sidebar_visible);
    assert_eq!(h.apply(Command::OpenSidebarPanel { panel: SidebarPanel::AppMenu }), set(false, true));
    assert_eq!(h.apply(Command::OpenSidebarPanel { panel: SidebarPanel::NewSpace }), set(true, false));
    assert_eq!(h.apply(Command::CloseSidebarPanel), set(false, false));
    // Panels that close themselves (a committed rename) hide it too.
    h.apply(Command::OpenSidebarPanel { panel: SidebarPanel::RenameItem { id: t } });
    let fx = h.apply(Command::RenameItem { id: t, title: Some("Mine".into()) });
    assert!(has(&fx, |e| *e == Effect::SetSidebar { visible: false, width: SIDEBAR_DEFAULT_WIDTH, floating: false }), "{fx:?}");
    // "Edit Pinned Page" holds input (title and URL fields): it docks, and saving closes it.
    h.apply(Command::TogglePin { id: Some(t) });
    assert_eq!(h.apply(Command::OpenSidebarPanel { panel: SidebarPanel::EditPinned { id: t } }), set(true, false));
    assert!(h.ui().window.sidebar_visible && !h.store.window_state().sidebar_visible);
    let fx = h.apply(Command::EditPinned { id: t, title: Some("Pinned".into()), url: None });
    assert!(has(&fx, |e| *e == Effect::SetSidebar { visible: false, width: SIDEBAR_DEFAULT_WIDTH, floating: false }), "{fx:?}");
    assert!(h.ui().sidebar_panel.is_none());
    h.apply(Command::TogglePin { id: Some(t) });
    if let Some(toast) = h.toast() {
        h.apply(Command::DismissToast { id: toast.id }); // "Pinned"
    }
    // Ctrl+S while it floats for a panel docks it for real; the panel stays open.
    h.apply(Command::OpenSidebarPanel { panel: SidebarPanel::Downloads });
    assert_eq!(h.apply(Command::ToggleSidebar), set(true, false));
    assert_eq!(h.ui().sidebar_panel.map(|p| p.panel), Some(SidebarPanel::Downloads));
    assert!(h.store.window_state().sidebar_visible && h.ui().window.sidebar_visible);
    assert!(h.apply(Command::CloseSidebarPanel).is_empty(), "closing the panel keeps it docked");
    // Ctrl+S while docked only for a panel that holds input docks it for real too.
    assert_eq!(h.apply(Command::ToggleSidebar), set(false, false));
    assert_eq!(h.apply(Command::OpenSidebarPanel { panel: SidebarPanel::NewSpace }), set(true, false));
    assert!(h.apply(Command::ToggleSidebar).is_empty(), "already docked: no native change");
    assert!(h.store.window_state().sidebar_visible);
    assert_eq!(h.ui().sidebar_panel.map(|p| p.panel), Some(SidebarPanel::NewSpace));
    assert!(h.apply(Command::CloseSidebarPanel).is_empty());
    // Ctrl+S with nothing open (e.g. while the shell shows the hover reveal) toggles the setting.
    assert_eq!(h.apply(Command::ToggleSidebar), set(false, false));
    assert_eq!(h.apply(Command::ToggleSidebar), set(true, false));
    // A background tab opened while the sidebar floats for a panel shows up there: no toast.
    h.apply(Command::ToggleSidebar);
    h.apply(Command::OpenSidebarPanel { panel: SidebarPanel::Downloads });
    h.apply(Command::OpenUrl { url: "https://bg.com/".into(), target: OpenTarget::BackgroundTab, opener: None });
    assert!(h.toast().is_none());
    // Hidden without a panel, it toasts.
    h.apply(Command::CloseSidebarPanel);
    h.apply(Command::OpenUrl { url: "https://bg2.com/".into(), target: OpenTarget::BackgroundTab, opener: None });
    assert!(h.toast().is_some());

    // Dirty flags on a bare store: revealing and hiding never mark state dirty; Ctrl+S from a
    // reveal (docking for real) does.
    let mut s = Store::new(T0);
    s.startup(Vec::new(), T0);
    s.apply(Command::ToggleSidebar, T0);
    s.take_dirty();
    for panel in [SidebarPanel::Downloads, SidebarPanel::NewSpace] {
        s.apply(Command::OpenSidebarPanel { panel }, T0);
        assert!(!s.take_dirty().state);
        s.apply(Command::CloseSidebarPanel, T0);
        assert!(!s.take_dirty().state);
    }
    s.apply(Command::OpenSidebarPanel { panel: SidebarPanel::AppMenu }, T0);
    let (restarted, _) = Store::load(Some(&s.state_json()), None, T0);
    assert!(!restarted.window_state().sidebar_visible && !restarted.ui_state().window.sidebar_visible);
    let rev = s.revision();
    let fx = s.apply(Command::ToggleSidebar, T0);
    assert!(fx.contains(&Effect::SetSidebar { visible: true, width: SIDEBAR_DEFAULT_WIDTH, floating: false }), "{fx:?}");
    assert!(s.take_dirty().state && s.revision() > rev);
    assert_eq!(s.sidebar_panel(), Some(&SidebarPanel::AppMenu));
}

#[test]
fn lazy_loads_and_session_restore_are_not_history_visits() {
    let mut h = Harness::new();
    let a = h.open("https://a.com/");
    h.commit(a, "https://a.com/", "A");
    let b = h.open("https://b.com/");
    h.commit(b, "https://b.com/", "B");
    let count = |h: &Harness, url: &str| h.store.history().get(url).map_or(0, |u| u.visit_count);
    assert_eq!((count(&h, "https://a.com/"), count(&h, "https://b.com/")), (1, 1), "first loads of new tabs are visits");

    // Session restore: the restored tab's first commit is not a visit.
    let (store, _) = Store::load(Some(&h.store.state_json()), Some(&h.store.history_json()), h.now);
    let mut h = Harness::start(store, Vec::new());
    assert_eq!(h.active(), Some(b));
    h.apply(Command::TabAddressChanged { tab: b, url: "https://b.com/".into() });
    assert_eq!(count(&h, "https://b.com/"), 1);
    // Lazy load on activation: not a visit either; later navigations in that browser are.
    h.apply(Command::ActivateItem { id: a });
    h.apply(Command::TabAddressChanged { tab: a, url: "https://a.com/".into() });
    assert_eq!(count(&h, "https://a.com/"), 1);
    h.apply(Command::TabAddressChanged { tab: a, url: "https://a.com/page".into() });
    assert_eq!(count(&h, "https://a.com/page"), 1);
    // Reopening a closed tab from the archive is not a visit.
    h.apply(Command::CloseItem { id: Some(a) });
    h.apply(Command::ReopenClosed);
    assert_eq!(h.active(), Some(a));
    h.apply(Command::TabAddressChanged { tab: a, url: "https://a.com/page".into() });
    assert_eq!(count(&h, "https://a.com/page"), 1);
    // A pinned tab reloading its home after being unloaded is not a visit.
    h.apply(Command::TogglePin { id: Some(a) });
    h.apply(Command::ActivateItem { id: b });
    h.apply(Command::UnloadTab { id: a });
    h.apply(Command::ActivateItem { id: a });
    h.apply(Command::TabAddressChanged { tab: a, url: "https://a.com/page".into() });
    assert_eq!(count(&h, "https://a.com/page"), 1);
    // A user navigation of an unloaded tab is a visit once the browser loads it.
    h.apply(Command::UnloadTab { id: b });
    h.apply(Command::Navigate { tab: Some(b), url: "https://b.com/news".into() });
    assert!(!h.store.is_loaded(b));
    h.apply(Command::ActivateItem { id: b });
    h.apply(Command::TabAddressChanged { tab: b, url: "https://b.com/news".into() });
    assert_eq!(count(&h, "https://b.com/news"), 1);
    // Newly opened tabs, even in the background, and Peek: the first load is a visit.
    h.apply(Command::OpenUrl { url: "https://c.com/".into(), target: OpenTarget::BackgroundTab, opener: None });
    let c = h.today()[0];
    h.apply(Command::TabAddressChanged { tab: c, url: "https://c.com/".into() });
    assert_eq!(count(&h, "https://c.com/"), 1);
    h.apply(Command::LinkOpenRequested { opener: b, url: "https://peek.com/".into(), disposition: LinkDisposition::NewWindow });
    let k = h.store.peek_tab().unwrap();
    h.apply(Command::TabAddressChanged { tab: k, url: "https://peek.com/".into() });
    assert_eq!(count(&h, "https://peek.com/"), 1);
}

#[test]
fn version_1_profiles_migrate_to_search_suggestions_on() {
    assert!(Settings::default().search_suggestions, "on by default");
    assert_eq!(State::default().version, 2);
    let mut h = Harness::new();
    h.apply(Command::UpdateSettings { patch: SettingsPatch { search_suggestions: Some(false), search_engine: Some(SearchEngineId::Bing), ..Default::default() } });
    let saved: serde_json::Value = serde_json::from_str(&h.store.state_json()).unwrap();
    assert_eq!((saved["version"].as_u64(), saved["settings"]["searchSuggestions"].as_bool()), (Some(2), Some(false)));

    // A current profile keeps "off" and loads clean.
    let (mut store, report) = Store::load(Some(&saved.to_string()), None, h.now);
    assert_eq!(report, LoadReport::default());
    assert!(!store.settings().search_suggestions);
    assert!(!store.take_dirty().state);

    // Version 1 (the setting was hidden and did nothing): turned on, other settings kept, saved.
    for version in [json!(1), json!(0), json!(null), json!("1")] {
        let mut old = saved.clone();
        old["version"] = version.clone();
        let (mut store, report) = Store::load(Some(&old.to_string()), None, h.now);
        assert!(!report.state_corrupt, "{version}: {report:?}");
        assert!(report.warnings.iter().any(|w| w.contains("migrated to 2")), "{version}: {report:?}");
        assert!(store.settings().search_suggestions, "{version}");
        assert_eq!(store.settings().search_engine, SearchEngineId::Bing);
        assert!(store.take_dirty().state, "{version}: the migration is saved");
        let resaved: serde_json::Value = serde_json::from_str(&store.state_json()).unwrap();
        assert_eq!((resaved["version"].as_u64(), resaved["settings"]["searchSuggestions"].as_bool()), (Some(2), Some(true)));
        // The migrated file loads clean.
        let (_, report) = Store::load(Some(&store.state_json()), None, h.now);
        assert_eq!(report, LoadReport::default(), "{version}");
    }
    let mut no_version = saved.clone();
    no_version.as_object_mut().unwrap().remove("version");
    let (store, _) = Store::load(Some(&no_version.to_string()), None, h.now);
    assert!(store.settings().search_suggestions, "a profile without a version counts as old");
    // An old profile without settings: defaults (on).
    let (store, _) = Store::load(Some(r#"{"version":1,"spaces":[]}"#), None, h.now);
    assert!(store.settings().search_suggestions);
    // A newer profile is left alone.
    let mut newer = saved.clone();
    newer["version"] = json!(3);
    let (store, report) = Store::load(Some(&newer.to_string()), None, h.now);
    assert!(!store.settings().search_suggestions && !report.warnings.iter().any(|w| w.contains("migrated")), "{report:?}");
}
