//! Regression scenarios: transient sidebar panels close when a page takes focus, typed external
//! protocols go to the OS (no tab), inline completion keeps the port and the history entry's
//! scheme, history titles of same-titled pages, separating the focused split pane keeps it active.

mod common;
use sta_core::*;
use common::*;

fn req(text: &str) -> OmniboxRequest {
    OmniboxRequest { text: text.into(), mode: CommandBarMode::NewTab, split_side: None, prevent_inline_autocomplete: false, suggestions: Vec::new(), seq: 1 }
}

// ------------------------------------------------------------------------------------ sidebar panels

#[test]
fn page_focus_closes_transient_sidebar_panels_only() {
    let mut h = Harness::new();
    let a = h.open("https://a.com/");
    for panel in [SidebarPanel::Downloads, SidebarPanel::AppMenu] {
        h.apply(Command::ToggleSidebarPanel { panel: panel.clone() });
        assert_eq!(h.ui().sidebar_panel.map(|p| p.panel), Some(panel.clone()));
        let fx = h.apply(Command::TabFocused { tab: a });
        assert!(h.ui().sidebar_panel.is_none(), "{panel:?} closes when the page takes focus");
        assert!(!has(&fx, |e| matches!(e, Effect::FocusBrowser { .. })), "the page already has focus: {fx:?}");
    }
    assert!(SidebarPanel::Downloads.is_transient() && SidebarPanel::AppMenu.is_transient());
    // Sheets with user input stay open (and Esc in a page leaves them alone: not transient).
    let space = h.space();
    let p = h.open_pinned("https://p.com/");
    h.apply(Command::ActivateItem { id: a });
    for panel in [SidebarPanel::NewSpace, SidebarPanel::EditSpace { id: space }, SidebarPanel::RenameItem { id: a }, SidebarPanel::EditPinned { id: p }] {
        assert!(!panel.is_transient(), "{panel:?}");
        h.apply(Command::OpenSidebarPanel { panel: panel.clone() });
        h.apply(Command::TabFocused { tab: a });
        assert_eq!(h.ui().sidebar_panel.map(|p| p.panel), Some(panel.clone()), "{panel:?} stays open");
        h.apply(Command::CloseSidebarPanel);
    }
    // A transient panel floating on a hidden sidebar un-pins it again.
    h.apply(Command::ToggleSidebar);
    let fx = h.apply(Command::ToggleSidebarPanel { panel: SidebarPanel::Downloads });
    assert!(has(&fx, |e| matches!(e, Effect::SetSidebar { visible: false, floating: true, .. })), "{fx:?}");
    assert!(!h.ui().window.sidebar_visible, "floating, not docked");
    let fx = h.apply(Command::TabFocused { tab: a });
    assert!(has(&fx, |e| matches!(e, Effect::SetSidebar { visible: false, floating: false, .. })), "{fx:?}");
    assert!(!h.ui().window.sidebar_visible);
    assert_eq!(h.store.sidebar_panel(), None);
}

// ------------------------------------------------------------------------------------ external protocols

#[test]
fn typed_external_protocols_open_in_the_os_without_a_tab() {
    let mut h = Harness::new();
    let a = h.open("https://a.com/");
    h.commit(a, "https://a.com/", "A");
    let today = h.today();
    let external = |fx: &[Effect], url: &str| fx.iter().any(|e| matches!(e, Effect::OpenExternal { url: u } if u == url));
    let no_tab_effects = |fx: &[Effect]| {
        !fx.iter().any(|e| {
            matches!(e, Effect::CreateBrowser { .. } | Effect::LoadUrl { .. } | Effect::ReplaceBrowser { .. } | Effect::ShowContent { .. })
        })
    };
    for target in [OpenTarget::NewTab, OpenTarget::BackgroundTab, OpenTarget::CurrentTab] {
        let fx = h.apply(Command::OpenInput { text: " mailto:me@example.com ".into(), target });
        assert!(external(&fx, "mailto:me@example.com") && no_tab_effects(&fx), "{target:?}: {fx:?}");
        let fx = h.apply(Command::OpenInput { text: "tel:+15551234".into(), target });
        assert!(external(&fx, "tel:+15551234") && no_tab_effects(&fx), "{target:?}: {fx:?}");
        let fx = h.apply(Command::OpenUrl { url: "zoommtg://zoom.us/join?confno=1".into(), target, opener: None });
        assert!(external(&fx, "zoommtg://zoom.us/join?confno=1") && no_tab_effects(&fx), "{target:?}: {fx:?}");
        let fx = h.apply(Command::OpenUrl { url: "ms-settings:display".into(), target, opener: None });
        assert!(external(&fx, "ms-settings:display") && no_tab_effects(&fx), "{target:?}: {fx:?}");
    }
    let fx = h.apply(Command::Navigate { tab: Some(a), url: "mailto:x@example.com".into() });
    assert!(external(&fx, "mailto:x@example.com") && no_tab_effects(&fx), "{fx:?}");
    assert_eq!(h.today(), today, "no tab was created");
    assert_eq!(h.tab(a).url, "https://a.com/", "the current tab was not navigated");
    assert_eq!(h.focused(), Some(a));

    // Committed from the command bar: the bar closes and the page gets focus back.
    h.apply(Command::OpenCommandBar { mode: CommandBarMode::EditUrl, split_side: None });
    let r = h.store.omnibox(&OmniboxRequest { mode: CommandBarMode::EditUrl, ..req("mailto:team@example.com") }, h.now);
    assert_eq!(r.results[0].command, Command::OpenInput { text: "mailto:team@example.com".into(), target: OpenTarget::CurrentTab });
    let fx = h.apply(Command::CommitOmnibox { command: Box::new(r.results[0].command.clone()), alt: false });
    assert!(external(&fx, "mailto:team@example.com"), "{fx:?}");
    assert!(has(&fx, |e| matches!(e, Effect::HideCommandBar)) && has(&fx, |e| matches!(e, Effect::FocusBrowser { tab } if *tab == a)), "{fx:?}");
    assert_eq!(h.tab(a).url, "https://a.com/");
    // Split mode: no pane.
    let fx = h.apply(Command::SplitOpenInput { text: "mailto:split@example.com".into(), side: SplitSide::Right });
    assert!(external(&fx, "mailto:split@example.com") && no_tab_effects(&fx), "{fx:?}");
    assert_eq!(h.today(), today);
    // Without any tab either.
    let mut h = Harness::new();
    let fx = h.apply(Command::OpenInput { text: "mailto:me@example.com".into(), target: OpenTarget::CurrentTab });
    assert!(external(&fx, "mailto:me@example.com") && no_tab_effects(&fx), "{fx:?}");
    assert!(h.today().is_empty());
    assert_eq!(h.store.content_layout(), ContentLayout::Empty);
    // Browser schemes and Windows paths still open tabs.
    let fx = h.apply(Command::OpenInput { text: "C:\\temp\\a.txt".into(), target: OpenTarget::NewTab });
    assert!(!has(&fx, |e| matches!(e, Effect::OpenExternal { .. })), "{fx:?}");
    assert_eq!(h.today().len(), 1);
}

// ------------------------------------------------------------------------------------ omnibox

#[test]
fn inline_completion_keeps_the_port() {
    let mut h = Harness::new();
    h.apply(Command::OpenInput { text: "127.0.0.1:8931/media".into(), target: OpenTarget::NewTab });
    let t = h.focused().unwrap();
    h.commit(t, "http://127.0.0.1:8931/media", "Media");
    assert_eq!(h.store.history().get("http://127.0.0.1:8931/media").unwrap().typed_count, 1);
    h.apply(Command::CloseItem { id: Some(t) });

    let r = h.store.omnibox(&req("127"), h.now);
    assert_eq!(r.inline_completion.as_deref(), Some("127.0.0.1:8931"));
    assert_eq!(r.results[0].command, Command::OpenInput { text: "127.0.0.1:8931".into(), target: OpenTarget::NewTab });
    assert_eq!(r.results[0].subtitle.as_deref(), Some("http://127.0.0.1:8931"));
    let r = h.store.omnibox(&req("127.0.0.1"), h.now);
    assert_eq!(r.inline_completion.as_deref(), Some("127.0.0.1:8931"), "the typed host completes to host:port");
    let r = h.store.omnibox(&req("127.0.0.1:8931/m"), h.now);
    assert_eq!(r.inline_completion.as_deref(), Some("127.0.0.1:8931/media"), "path completion after the port");
    // Committing the completion opens the same origin, port included.
    let r = h.store.omnibox(&req("127"), h.now);
    h.apply(Command::CommitOmnibox { command: Box::new(r.results[0].command.clone()), alt: false });
    let t = h.focused().unwrap();
    assert_eq!(h.tab(t).url, "http://127.0.0.1:8931");
    // Default ports are not shown; www. is still optional.
    h.apply(Command::OpenInput { text: "https://www.example.com:443/".into(), target: OpenTarget::NewTab });
    let e = h.focused().unwrap();
    h.commit(e, "https://www.example.com/", "Example");
    assert_eq!(h.store.omnibox(&req("exa"), h.now).inline_completion.as_deref(), Some("example.com"));
    // localhost with a port.
    h.apply(Command::OpenInput { text: "localhost:8931/media".into(), target: OpenTarget::NewTab });
    let l = h.focused().unwrap();
    h.commit(l, "http://localhost:8931/media", "Media");
    assert_eq!(h.store.omnibox(&req("localh"), h.now).inline_completion.as_deref(), Some("localhost:8931"));
}

#[test]
fn inline_completion_commits_with_the_history_entry_scheme() {
    let mut h = Harness::new();
    let visit = |h: &mut Harness, url: &str| {
        h.apply(Command::OpenInput { text: url.into(), target: OpenTarget::NewTab });
        let t = h.focused().unwrap();
        h.commit(t, url, "Page");
        assert_eq!(h.store.history().get(url).unwrap().typed_count, 1, "{url}");
        h.apply(Command::CloseItem { id: Some(t) });
    };
    // An http: host that is neither localhost nor an IP (typed text would resolve to https://).
    visit(&mut h, "http://intranet.example.com:8080/wiki");
    let r = h.store.omnibox(&req("intra"), h.now);
    assert_eq!(r.inline_completion.as_deref(), Some("intranet.example.com:8080"));
    let go = &r.results[0];
    assert_eq!(go.title, "intranet.example.com:8080", "the row shows the completed text");
    assert_eq!(go.subtitle.as_deref(), Some("http://intranet.example.com:8080"));
    assert_eq!(go.command, Command::OpenInput { text: "http://intranet.example.com:8080".into(), target: OpenTarget::NewTab });
    assert_eq!(go.alt_command, Some(Command::OpenInput { text: "http://intranet.example.com:8080".into(), target: OpenTarget::BackgroundTab }));
    let edit = h.store.omnibox(&OmniboxRequest { mode: CommandBarMode::EditUrl, ..req("intra") }, h.now);
    assert_eq!(edit.results[0].command, Command::OpenInput { text: "http://intranet.example.com:8080".into(), target: OpenTarget::CurrentTab });
    let split = h.store.omnibox(&OmniboxRequest { mode: CommandBarMode::Split, ..req("intra") }, h.now);
    assert_eq!(split.results[0].command, Command::SplitOpenInput { text: "http://intranet.example.com:8080".into(), side: SplitSide::Right });
    // Path completion keeps it too.
    let r = h.store.omnibox(&req("intranet.example.com:8080/w"), h.now);
    assert_eq!(r.inline_completion.as_deref(), Some("intranet.example.com:8080/wiki"));
    assert_eq!(r.results[0].command, Command::OpenInput { text: "http://intranet.example.com:8080/wiki".into(), target: OpenTarget::NewTab });
    // Committing opens the http: origin.
    let r = h.store.omnibox(&req("intra"), h.now);
    h.apply(Command::CommitOmnibox { command: Box::new(r.results[0].command.clone()), alt: false });
    let t = h.focused().unwrap();
    assert_eq!(h.tab(t).url, "http://intranet.example.com:8080");
    h.apply(Command::CloseItem { id: Some(t) });

    // A host that alone would be a search (unknown TLD, no port) still opens as a URL.
    visit(&mut h, "http://nas.lan/");
    let r = h.store.omnibox(&req("nas"), h.now);
    assert_eq!(r.inline_completion.as_deref(), Some("nas.lan"));
    assert_eq!(r.results[0].command, Command::OpenInput { text: "http://nas.lan".into(), target: OpenTarget::NewTab });

    // Where typed text already resolves to the entry's scheme, the command keeps the bare text.
    visit(&mut h, "https://secure.example.org/");
    let r = h.store.omnibox(&req("secu"), h.now);
    assert_eq!(r.results[0].command, Command::OpenInput { text: "secure.example.org".into(), target: OpenTarget::NewTab });
    visit(&mut h, "http://10.1.2.3:8000/");
    let r = h.store.omnibox(&req("10.1"), h.now);
    assert_eq!(r.results[0].command, Command::OpenInput { text: "10.1.2.3:8000".into(), target: OpenTarget::NewTab });
    // An https: entry on localhost (typed text would resolve to http://) keeps https.
    visit(&mut h, "https://localhost:8443/");
    let r = h.store.omnibox(&req("localh"), h.now);
    assert_eq!(r.inline_completion.as_deref(), Some("localhost:8443"));
    assert_eq!(r.results[0].command, Command::OpenInput { text: "https://localhost:8443".into(), target: OpenTarget::NewTab });
}

// ------------------------------------------------------------------------------------ history

#[test]
fn history_title_of_a_same_titled_next_page() {
    let mut h = Harness::new();
    h.apply(Command::OpenInput { text: "127.0.0.1:8931/media".into(), target: OpenTarget::NewTab });
    let t = h.focused().unwrap();
    h.commit(t, "http://127.0.0.1:8931/media", "Media");
    assert_eq!(h.store.history().get("http://127.0.0.1:8931/media").unwrap().title, "Media");
    // Ctrl+L localhost:8931/media: a different URL with the same page title.
    h.apply(Command::OpenInput { text: "localhost:8931/media".into(), target: OpenTarget::CurrentTab });
    let fx = h.apply(Command::TabAddressChanged { tab: t, url: "http://localhost:8931/media".into() });
    assert!(fx.is_empty(), "{fx:?}");
    assert_eq!(h.store.history().get("http://localhost:8931/media").unwrap().title, "", "the visit is recorded before its title");
    let rev = h.store.revision();
    h.apply(Command::TabTitleChanged { tab: t, title: "Media".into() });
    let entry = h.store.history().get("http://localhost:8931/media").unwrap();
    assert_eq!(entry.title, "Media", "the unchanged tab title still names the new history entry");
    assert!(h.store.revision() > rev, "history revision bumped");
    let rows = h.store.history_list("", 10, h.now);
    assert!(rows.iter().all(|r| r.title == "Media"), "{rows:?}");
    // The same title again changes nothing.
    let rev = h.store.revision();
    h.apply(Command::TabTitleChanged { tab: t, title: "Media".into() });
    assert_eq!(h.store.revision(), rev);
}

// ------------------------------------------------------------------------------------ split

#[test]
fn separating_the_focused_pane_keeps_it_active() {
    let mut h = Harness::new();
    let a = h.open("https://a.com/");
    let b = h.open("https://b.com/");
    let c = h.open("https://c.com/");
    h.apply(Command::SplitWith { tab: b, with: a, side: SplitSide::Right });
    let sid = h.active().unwrap();
    h.apply(Command::SplitWith { tab: c, with: b, side: SplitSide::Right });
    assert_eq!(h.split(sid).panes, vec![a, b, c]);
    h.apply(Command::FocusPane { index: 1 });
    assert_eq!(h.focused(), Some(b));
    // Ctrl+Shift+- on the focused pane: b leaves the split, stays active and focused.
    let fx = h.apply(Command::SeparatePane { tab: None });
    assert_eq!(h.active(), Some(b));
    assert_eq!(h.focused(), Some(b));
    assert_eq!(shows(&fx), Some(ContentLayout::Single { tab: b }));
    assert!(has(&fx, |e| matches!(e, Effect::FocusBrowser { tab } if *tab == b)), "{fx:?}");
    assert_eq!(h.today(), vec![sid, b]);
    assert_eq!(h.split(sid).panes, vec![a, c]);
    assert_eq!(h.store.state().window.mru[0], b);
    // Separating the focused pane of a 2-pane split: the split dissolves, the separated tab stays.
    h.apply(Command::ActivateItem { id: c });
    assert_eq!(h.focused(), Some(c));
    h.apply(Command::SeparatePane { tab: Some(c) });
    assert!(!h.store.state().items.contains_key(&sid));
    assert_eq!(h.today(), vec![a, c, b]);
    assert_eq!(h.active(), Some(c));
    assert_eq!(h.store.content_layout(), ContentLayout::Single { tab: c });
    // Separating another (unfocused) pane keeps the split active.
    h.apply(Command::SplitWith { tab: b, with: a, side: SplitSide::Right });
    let sid = h.active().unwrap();
    h.apply(Command::SplitWith { tab: c, with: b, side: SplitSide::Right });
    h.apply(Command::FocusPane { index: 0 });
    assert_eq!(h.focused(), Some(a));
    h.apply(Command::SeparatePane { tab: Some(c) });
    assert_eq!(h.active(), Some(sid));
    assert_eq!(h.focused(), Some(a));
    // Separating the focused pane of a split in the background (not active) activates nothing.
    let x = h.open("https://x.com/");
    h.apply(Command::SeparatePane { tab: Some(b) });
    assert_eq!(h.active(), Some(x));
}
