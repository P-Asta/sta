//! Reducer scenarios: spaces, split view and Peek. Every command runs through the checking
//! harness (invariants, effect guarantees, revision/dirty tracking).

mod common;
use sta_core::*;
use common::*;

const LAGOON: Theme = Theme { hue: 190.0, hue2: 230.0, chroma: 0.06 };
const EMBER: Theme = Theme { hue: 50.0, hue2: 20.0, chroma: 0.07 };

fn chrome_fx(fx: &[Effect]) -> Option<(u32, bool)> {
    fx.iter().find_map(|e| if let Effect::SetChrome { frame_argb, dark, .. } = e { Some((*frame_argb, *dark)) } else { None })
}

// ------------------------------------------------------------------------------------ spaces

#[test]
fn new_space_switches_and_sets_chrome() {
    let mut h = Harness::new();
    let home = h.space();
    let a = h.open("https://a.com/");
    let fx = h.apply(Command::NewSpace { name: " Work ".into(), icon: "🚀".into(), theme: LAGOON });
    let work = h.space();
    assert_ne!(work, home);
    assert_eq!(chrome_fx(&fx), Some((theme::frame_argb(&LAGOON, false), false)));
    assert_eq!(shows(&fx), Some(ContentLayout::Empty));
    let ui = h.ui();
    assert_eq!(ui.spaces.iter().map(|s| (s.name.as_str(), s.icon.as_str())).collect::<Vec<_>>(), [("Home", "🏠"), ("Work", "🚀")]);
    assert_eq!(ui.active_space, work);
    assert!(ui.current.is_none());
    assert_eq!(ui.spaces[1].colors, theme::colors(&LAGOON, false));
    // Empty name/icon get defaults; the theme is sanitized.
    h.apply(Command::NewSpace { name: "  ".into(), icon: "".into(), theme: Theme { hue: 400.0, hue2: -10.0, chroma: 9.0 } });
    let third = h.space_data(h.space());
    assert_eq!((third.name.as_str(), third.icon.as_str()), ("Space 3", "✨"));
    assert_eq!(third.theme, Theme { hue: 40.0, hue2: 350.0, chroma: 0.08 });
    // Switching back shows the remembered active item without re-creating it.
    let fx = h.apply(Command::SwitchSpace { id: home });
    assert!(!has(&fx, |e| matches!(e, Effect::CreateBrowser { .. })), "{fx:?}");
    assert_eq!(shows(&fx), Some(ContentLayout::Single { tab: a }));
    assert!(has(&fx, |e| matches!(e, Effect::FocusBrowser { tab } if *tab == a)));
    assert_eq!(chrome_fx(&fx), Some((theme::frame_argb(&Theme::default(), false), false)));
    // Same space: no effects.
    assert!(h.apply(Command::SwitchSpace { id: home }).is_empty());
    assert!(h.apply(Command::SwitchSpace { id: 987_654 }).is_empty());
}

#[test]
fn switch_space_loads_unloaded_active_item() {
    let mut h = Harness::new();
    let home = h.space();
    let a = h.open("https://a.com/");
    let work = h.new_space("Work", "🚀", LAGOON);
    // Unload Home's (now hidden) active tab.
    let fx = h.apply(Command::UnloadTab { id: a });
    assert!(has(&fx, |e| is_destroy(e, a)));
    assert_eq!(h.space_data(home).active_item, Some(a), "row and active item stay");
    let fx = h.apply(Command::SwitchSpace { id: home });
    let create = position(&fx, |e| matches!(e, Effect::CreateBrowser { tab, url, .. } if *tab == a && url == "https://a.com/")).expect("create");
    let show = position(&fx, |e| matches!(e, Effect::ShowContent { layout: ContentLayout::Single { tab } } if *tab == a)).expect("show");
    assert!(create < show);
    assert_eq!(h.space_data(work).active_item, None);
}

#[test]
fn switch_space_nth_and_adjacent() {
    let mut h = Harness::new();
    let home = h.space();
    let work = h.new_space("Work", "🚀", LAGOON);
    let play = h.new_space("Play", "🎮", EMBER);
    h.apply(Command::SwitchSpaceNth { n: 1 });
    assert_eq!(h.space(), home);
    h.apply(Command::SwitchSpaceNth { n: 3 });
    assert_eq!(h.space(), play);
    assert!(h.apply(Command::SwitchSpaceNth { n: 0 }).is_empty());
    assert!(h.apply(Command::SwitchSpaceNth { n: 4 }).is_empty());
    h.apply(Command::SwitchSpaceAdjacent { delta: -1 });
    assert_eq!(h.space(), work);
    h.apply(Command::SwitchSpaceAdjacent { delta: 5 });
    assert_eq!(h.space(), play, "delta is a direction");
    assert!(h.apply(Command::SwitchSpaceAdjacent { delta: 1 }).is_empty(), "no wrap");
    assert!(h.apply(Command::SwitchSpaceAdjacent { delta: 0 }).is_empty());
    // Reorder
    h.apply(Command::MoveSpace { id: play, index: 0 });
    assert_eq!(h.store.state().spaces.iter().map(|s| s.id).collect::<Vec<_>>(), [play, home, work]);
    h.apply(Command::MoveSpace { id: play, index: 99 });
    assert_eq!(h.store.state().spaces.iter().map(|s| s.id).collect::<Vec<_>>(), [home, work, play]);
    assert!(h.apply(Command::MoveSpace { id: 12345, index: 0 }).is_empty());
}

#[test]
fn update_space_and_appearance_drive_set_chrome() {
    let mut h = Harness::new();
    let home = h.space();
    let work = h.new_space("Work", "🚀", LAGOON);
    // Editing an inactive space: no chrome change, but visible.
    let rev = h.store.revision();
    let fx = h.apply(Command::UpdateSpace { id: home, name: Some("House".into()), icon: Some(" 🏡 ".into()), theme: Some(EMBER) });
    assert!(chrome_fx(&fx).is_none());
    assert!(h.store.revision() > rev);
    let s = h.space_data(home);
    assert_eq!((s.name.as_str(), s.icon.as_str(), s.theme.clone()), ("House", "🏡", EMBER));
    // Blank name/icon are ignored.
    h.apply(Command::UpdateSpace { id: home, name: Some(" ".into()), icon: Some("".into()), theme: None });
    assert_eq!(h.space_data(home).name, "House");
    // Active space theme → SetChrome.
    let fx = h.apply(Command::UpdateSpace { id: work, name: None, icon: None, theme: Some(EMBER) });
    assert_eq!(chrome_fx(&fx), Some((theme::frame_argb(&EMBER, false), false)));
    // Appearance override.
    let fx = h.apply(Command::UpdateSettings { patch: SettingsPatch { appearance: Some(Appearance::Dark), ..Default::default() } });
    assert_eq!(chrome_fx(&fx), Some((theme::frame_argb(&EMBER, true), true)));
    // The card and ring colors travel with it (`theme::chrome_argb` of the active space).
    let full = fx.iter().find_map(|e| match e {
        Effect::SetChrome { accent_argb, surface_argb, border_argb, frame_border_argb, .. } => {
            Some((*accent_argb, *surface_argb, *border_argb, *frame_border_argb))
        }
        _ => None,
    });
    let c = theme::chrome_argb(&EMBER, true);
    assert_eq!(full, Some((c.accent, c.surface, c.border, c.frame_border)));
    assert_eq!(h.store.chrome_argb(), c);
    assert!(h.ui().dark);
    assert_eq!(h.ui().theme_presets[0].colors, theme::colors(&Theme::default(), true));
    // System theme is ignored while the appearance is forced…
    let fx = h.apply(Command::SystemThemeChanged { dark: true });
    assert!(chrome_fx(&fx).is_none());
    // …and followed with Appearance::System.
    let fx = h.apply(Command::UpdateSettings { patch: SettingsPatch { appearance: Some(Appearance::System), ..Default::default() } });
    assert!(chrome_fx(&fx).is_none(), "system is dark too: nothing changes");
    let fx = h.apply(Command::SystemThemeChanged { dark: false });
    assert_eq!(chrome_fx(&fx), Some((theme::frame_argb(&EMBER, false), false)));
    assert!(h.apply(Command::SystemThemeChanged { dark: false }).is_empty());
    assert_eq!(h.store.frame_argb(), theme::hex_to_argb(&h.ui().spaces[1].colors.frame).unwrap());
    assert_eq!(h.store.theme_colors(&LAGOON), theme::colors(&LAGOON, false));
}

#[test]
fn delete_space_archives_and_falls_back() {
    let mut h = Harness::new();
    let home = h.space();
    let a = h.open("https://a.com/");
    let work = h.new_space("Work", "🚀", LAGOON);
    let w1 = h.open("https://w1.com/");
    let wp = h.open_pinned("https://wp.com/");
    h.apply(Command::NewFolder { space: None, parent: None, name: Some("F".into()) });
    let folder = h.pinned()[0];
    let wf = h.open_pinned("https://wf.com/");
    h.apply(Command::MoveItem { id: wf, to: DropTarget { container: Container::Folder { id: folder }, before: None } });
    h.apply(Command::OpenSidebarPanel { panel: SidebarPanel::EditSpace { id: work } });
    let fx = h.apply(Command::DeleteSpace { id: work });
    assert!(has(&fx, |e| is_destroy(e, w1)) && has(&fx, |e| is_destroy(e, wf)));
    assert_eq!(h.space(), home);
    assert_eq!(shows(&fx), Some(ContentLayout::Single { tab: a }));
    assert_eq!(h.store.state().spaces.len(), 1);
    assert!(h.ui().sidebar_panel.is_none(), "edit sheet of the deleted space closes");
    let archive = &h.store.state().archive;
    assert_eq!(archive.len(), 3);
    assert!(archive.iter().all(|e| e.reason == ArchiveReason::SpaceDeleted && e.space == Some(work)));
    let pinned_entry = archive.iter().find(|e| e.id == wp).unwrap();
    assert_eq!((pinned_entry.section, pinned_entry.pinned_url.as_deref()), (Section::Pinned, Some("https://wp.com/")));
    assert_eq!(archive.iter().find(|e| e.id == wf).unwrap().folder, Some(folder));
    assert_eq!(h.toast().unwrap().message, "Deleted “Work” · 3 tabs archived");
    // Restoring into a deleted space lands at the top of the active space's Today.
    h.apply(Command::RestoreArchived { id: wp, whole_group: false });
    assert_eq!(h.today()[0], wp);
    assert_eq!(h.tab(wp).pinned_url, None);
    // The last space can't be deleted.
    let fx = h.apply(Command::DeleteSpace { id: home });
    assert!(!has(&fx, |e| matches!(e, Effect::DestroyBrowser { .. })));
    assert_eq!(h.toast().unwrap().message, "The last space can't be deleted");
    assert_eq!(h.store.state().spaces.len(), 1);
    // Deleting an inactive space keeps the current one.
    let other = h.new_space("Other", "🎮", EMBER);
    h.apply(Command::SwitchSpace { id: home });
    h.apply(Command::DeleteSpace { id: other });
    assert_eq!(h.space(), home);
    assert_eq!(h.toast().unwrap().message, "Deleted “Other”");
}

// ------------------------------------------------------------------------------------ split view

#[test]
fn split_create_add_focus_and_fractions() {
    let mut h = Harness::new();
    let a = h.open("https://a.com/");
    let b = h.open("https://b.com/");
    let x = h.open("https://x.com/");
    let fx = h.apply(Command::SplitWith { tab: b, with: a, side: SplitSide::Right });
    let sid = h.active().unwrap();
    let s = h.split(sid);
    assert_eq!((s.panes.clone(), s.orientation, s.focused, s.fractions.clone()), (vec![a, b], Orientation::Horizontal, 1, vec![0.5, 0.5]));
    assert_eq!(h.today(), vec![x, sid], "the split replaces `with` at its position");
    assert_eq!(
        shows(&fx),
        Some(ContentLayout::Split {
            orientation: Orientation::Horizontal,
            panes: vec![Pane { tab: a, fraction: 0.5 }, Pane { tab: b, fraction: 0.5 }],
            focused: 1
        })
    );
    assert!(has(&fx, |e| matches!(e, Effect::FocusBrowser { tab } if *tab == b)));
    let ui = h.ui();
    assert_eq!(ui.active_item, Some(sid));
    assert_eq!(ui.focused_tab, Some(b));
    let current = ui.current.clone().unwrap();
    assert_eq!((current.tab, current.split_panes), (b, 2));
    let NodeView::Split(row) = &ui.spaces[0].today[1] else { panic!("split row") };
    assert!(row.active && row.panes.iter().all(|p| p.visible));
    assert_eq!(row.panes.iter().map(|p| p.active).collect::<Vec<_>>(), [false, true]);

    // Split mode commit: new pane left of the focused pane; fractions re-equalized.
    let fx = h.apply(Command::SplitOpenInput { text: "c.com".into(), side: SplitSide::Left });
    let c = h.focused().unwrap();
    assert!(has(&fx, |e| matches!(e, Effect::CreateBrowser { tab, url, .. } if *tab == c && url == "https://c.com")));
    let s = h.split(sid);
    assert_eq!(s.panes, vec![a, c, b]);
    assert_eq!(s.focused, 1);
    assert!(s.fractions.iter().all(|f| (f - 1.0 / 3.0).abs() < 1e-6));
    // An existing split keeps its orientation.
    h.apply(Command::SplitOpenInput { text: "d.com".into(), side: SplitSide::Bottom });
    let d = h.focused().unwrap();
    assert_eq!(h.split(sid).panes, vec![a, c, d, b]);
    assert_eq!(h.split(sid).orientation, Orientation::Horizontal);
    // Full at 4 panes.
    let today_len = h.today().len();
    assert!(!has(&h.apply(Command::SplitOpenInput { text: "e.com".into(), side: SplitSide::Right }), |e| matches!(e, Effect::CreateBrowser { .. })));
    assert_eq!(h.toast().unwrap().message, "Split view is full (4 panes)");
    h.apply(Command::SplitWith { tab: x, with: sid, side: SplitSide::Right });
    assert_eq!(h.split(sid).panes.len(), 4);
    assert_eq!(h.today().len(), today_len);

    // Pane focus.
    let fx = h.apply(Command::FocusPane { index: 0 });
    assert_eq!(h.focused(), Some(a));
    assert!(has(&fx, |e| matches!(e, Effect::FocusBrowser { tab } if *tab == a)));
    assert!(matches!(shows(&fx), Some(ContentLayout::Split { focused: 0, .. })));
    assert!(h.apply(Command::FocusPane { index: 4 }).is_empty());
    assert!(h.apply(Command::FocusPaneAdjacent { delta: -1 }).is_empty(), "no wrap");
    h.apply(Command::FocusPaneAdjacent { delta: 1 });
    assert_eq!(h.focused(), Some(c));
    // The user clicking into a pane (shell event) updates focus without FocusBrowser.
    let fx = h.apply(Command::TabFocused { tab: b });
    assert_eq!(h.focused(), Some(b));
    assert!(!has(&fx, |e| matches!(e, Effect::FocusBrowser { .. })));
    assert!(matches!(shows(&fx), Some(ContentLayout::Split { focused: 3, .. })));
    // Activating a pane id activates its split with that pane focused.
    h.apply(Command::ActivateItem { id: x });
    let fx = h.apply(Command::ActivateItem { id: d });
    assert_eq!((h.active(), h.focused()), (Some(sid), Some(d)));
    assert!(matches!(shows(&fx), Some(ContentLayout::Split { focused: 2, .. })));

    // Fractions: clamped to the minimum share, renormalized; invalid input ignored.
    h.apply(Command::SetSplitFractions { id: sid, fractions: vec![0.05, 0.25, 0.3, 0.4] });
    let f = h.split(sid).fractions;
    assert!((f[0] - MIN_PANE_FRACTION).abs() < 1e-4, "{f:?}");
    assert!((f.iter().sum::<f32>() - 1.0).abs() < 1e-4);
    assert!(h.apply(Command::SetSplitFractions { id: sid, fractions: vec![0.5, 0.5] }).is_empty());
    assert!(h.apply(Command::SetSplitFractions { id: sid, fractions: vec![f32::NAN, 0.1, 0.1, 0.1] }).is_empty());
    assert!(h.apply(Command::SetSplitFractions { id: a, fractions: vec![0.25; 4] }).len() <= 1, "a pane id addresses its split");
    assert_eq!(h.split(sid).fractions, vec![0.25; 4]);
}

use sta_core::store::MIN_PANE_FRACTION;

#[test]
fn split_vertical_and_pinned_duplication() {
    let mut h = Harness::new();
    let space = h.space();
    let p = h.open_pinned("https://pinned.com/");
    let t = h.open("https://t.com/");
    // Top → vertical, `tab` before `with`.
    h.apply(Command::SplitWith { tab: p, with: t, side: SplitSide::Top });
    let sid = h.active().unwrap();
    let s = h.split(sid);
    assert_eq!(s.orientation, Orientation::Vertical);
    assert_ne!(s.panes[0], p, "a pinned tab is duplicated into Today");
    assert_eq!(s.panes[1], t);
    assert_eq!(h.pinned(), vec![p]);
    assert_eq!(h.tab(s.panes[0]).url, "https://pinned.com/");
    assert_eq!(h.tab(s.panes[0]).pinned_url, None);
    assert_eq!(h.store.tab_section(s.panes[0]), Some(Section::Today));
    // Split mode command bar defaults to the right side.
    h.apply(Command::OpenCommandBar { mode: CommandBarMode::Split, split_side: None });
    assert_eq!(h.ui().command_bar.unwrap().split_side, Some(SplitSide::Right));
    // `with` pinned: duplicated, split created at the top of Today.
    let u = h.open("https://u.com/");
    h.apply(Command::SplitWith { tab: u, with: p, side: SplitSide::Right });
    let sid2 = h.active().unwrap();
    assert_ne!(sid2, sid);
    assert_eq!(h.today()[0], sid2);
    assert_eq!(h.split(sid2).panes[1], u);
    assert_eq!(h.split(sid2).orientation, Orientation::Horizontal);
    // Moving a pane between splits dissolves the source when it drops to one pane.
    let first_pane = h.split(sid).panes[0];
    h.apply(Command::SplitWith { tab: first_pane, with: u, side: SplitSide::Right });
    assert!(!h.store.state().items.contains_key(&sid));
    assert_eq!(h.split(sid2).panes.len(), 3);
    assert!(h.today().contains(&t));
    // Invalid: same tab, unknown tabs.
    assert!(h.apply(Command::SplitWith { tab: u, with: u, side: SplitSide::Left }).is_empty());
    assert!(h.apply(Command::SplitWith { tab: 5555, with: u, side: SplitSide::Left }).is_empty());
    // Reorder within the same split.
    let panes = h.split(sid2).panes;
    h.apply(Command::SplitWith { tab: panes[2], with: panes[0], side: SplitSide::Left });
    assert_eq!(h.split(sid2).panes, vec![panes[2], panes[0], panes[1]]);
    let _ = space;
}

#[test]
fn split_separate_close_and_reopen() {
    let mut h = Harness::new();
    let x = h.open("https://x.com/");
    let a = h.open("https://a.com/");
    let b = h.open("https://b.com/");
    let c = h.open("https://c.com/");
    let d = h.open("https://d.com/");
    // today: [d, c, b, a, x]
    h.apply(Command::SplitWith { tab: b, with: a, side: SplitSide::Right });
    let sid = h.active().unwrap();
    h.apply(Command::SplitWith { tab: c, with: b, side: SplitSide::Right });
    h.apply(Command::SplitWith { tab: d, with: c, side: SplitSide::Right });
    assert_eq!(h.split(sid).panes, vec![a, b, c, d]);
    assert_eq!(h.today(), vec![sid, x]);
    // Separate the focused pane (d): below the split, and it stays the active tab.
    h.apply(Command::SeparatePane { tab: None });
    assert_eq!(h.today(), vec![sid, d, x]);
    assert_eq!(h.split(sid).panes, vec![a, b, c]);
    assert_eq!(h.focused(), Some(d));
    h.apply(Command::ActivateItem { id: sid });
    assert_eq!(h.focused(), Some(c));
    // Ctrl+W in a split closes only the focused pane.
    let fx = h.apply(Command::CloseItem { id: None });
    assert!(has(&fx, |e| is_destroy(e, c)));
    let entry = h.store.state().archive[0].clone();
    assert_eq!(entry.id, c);
    assert_eq!(entry.split.as_ref().map(|s| (s.group, s.pane_index)), Some((sid, 2)));
    assert_eq!(h.split(sid).panes, vec![a, b]);
    assert_eq!(h.active(), Some(sid));
    // Closing another pane dissolves the split into a normal tab in its place.
    h.apply(Command::CloseItem { id: Some(a) });
    assert!(!h.store.state().items.contains_key(&sid));
    assert_eq!(h.today(), vec![b, d, x]);
    assert_eq!(h.active(), Some(b));
    assert_eq!(h.store.content_layout(), ContentLayout::Single { tab: b });

    // Whole-split close archives every pane and Ctrl+Shift+T rebuilds it.
    h.apply(Command::SplitWith { tab: d, with: b, side: SplitSide::Bottom });
    let sid = h.active().unwrap();
    h.apply(Command::SetSplitFractions { id: sid, fractions: vec![0.3, 0.7] });
    h.apply(Command::FocusPane { index: 0 });
    let fx = h.apply(Command::CloseItem { id: Some(sid) });
    assert!(has(&fx, |e| is_destroy(e, b)) && has(&fx, |e| is_destroy(e, d)));
    assert_eq!(h.today(), vec![x]);
    assert_eq!(h.active(), Some(x));
    assert!(matches!(h.store.state().reopen.last(), Some(ReopenEntry::Split { archive_ids }) if archive_ids == &vec![b, d]));
    let list = h.store.archive_list();
    assert!(list.iter().filter(|e| e.group == Some(sid)).count() == 2);
    let fx = h.apply(Command::ReopenClosed);
    assert_eq!(h.active(), Some(sid), "split id reused");
    let s = h.split(sid);
    assert_eq!((s.panes.clone(), s.orientation, s.focused), (vec![b, d], Orientation::Vertical, 0));
    assert!((s.fractions[0] - 0.3).abs() < 1e-4);
    assert!(has(&fx, |e| is_create(e, b)) && has(&fx, |e| is_create(e, d)));
    assert!(matches!(shows(&fx), Some(ContentLayout::Split { orientation: Orientation::Vertical, focused: 0, .. })));
    assert!(h.store.state().archive.iter().all(|e| e.id != b && e.id != d));

    // Archive page: restore the whole group from one entry.
    h.apply(Command::CloseItem { id: Some(sid) });
    h.apply(Command::RestoreArchived { id: d, whole_group: true });
    assert_eq!(h.split(sid).panes, vec![b, d]);
    assert_eq!(h.active(), Some(sid));
    // Restoring a single pane into its existing split.
    h.apply(Command::SplitOpenInput { text: "e.com".into(), side: SplitSide::Right });
    let e = h.focused().unwrap();
    h.apply(Command::CloseItem { id: Some(e) });
    h.apply(Command::RestoreArchived { id: e, whole_group: false });
    assert_eq!(h.split(sid).panes.len(), 3);
    assert!(h.split(sid).panes.contains(&e));

    // Separate all: panes become Today tabs where the split was, focused pane stays active.
    h.apply(Command::FocusPane { index: 1 });
    let panes = h.split(sid).panes;
    h.apply(Command::SeparateAll { id: sid });
    assert_eq!(h.today()[..3], panes[..]);
    assert_eq!(h.active(), Some(panes[1]));
    assert!(h.apply(Command::SeparateAll { id: sid }).is_empty());
    assert!(h.apply(Command::SeparatePane { tab: Some(x) }).is_empty(), "not a pane");
}

#[test]
fn restart_restores_split_with_all_panes_loaded() {
    let mut h = Harness::new();
    let a = h.open("https://a.com/");
    let b = h.open("https://b.com/");
    let _c = h.open("https://c.com/");
    h.apply(Command::SplitWith { tab: b, with: a, side: SplitSide::Right });
    let sid = h.active().unwrap();
    let (store, report) = Store::load(Some(&h.store.state_json()), Some(&h.store.history_json()), h.now);
    assert!(!report.state_corrupt && report.warnings.is_empty(), "{report:?}");
    let h2 = Harness::start(store, Vec::new());
    let creates: Vec<Id> = h2.history.iter().filter_map(|e| if let Effect::CreateBrowser { tab, .. } = e { Some(*tab) } else { None }).collect();
    assert_eq!(creates, vec![a, b], "only the active item's panes load");
    assert!(matches!(h2.store.content_layout(), ContentLayout::Split { .. }));
    assert_eq!(h2.active(), Some(sid));
}

// ------------------------------------------------------------------------------------ peek

#[test]
fn pinned_cross_site_link_opens_peek_then_expand() {
    let mut h = Harness::new();
    let t = h.open("https://today.com/");
    let p = h.open_pinned("https://mail.example.com/inbox");
    // on_before_browse interception.
    assert_eq!(h.store.intercept_navigation(p, "https://other.org/x", true, false), Some(LinkDisposition::PinnedCrossSite));
    assert_eq!(h.store.intercept_navigation(p, "https://www.example.com/x", true, false), None, "same site");
    assert_eq!(h.store.intercept_navigation(p, "https://other.org/x", false, false), None, "no gesture");
    assert_eq!(h.store.intercept_navigation(p, "https://other.org/x", true, true), None, "redirect");
    assert_eq!(h.store.intercept_navigation(t, "https://other.org/x", true, false), None, "Today tab");
    assert_eq!(h.store.intercept_navigation(p, "sta://settings/", true, false), None);

    let fx = h.apply(Command::LinkOpenRequested { opener: p, url: "https://other.org/x".into(), disposition: LinkDisposition::PinnedCrossSite });
    let peek = h.store.peek_tab().expect("peek");
    let create = position(&fx, |e| is_create(e, peek)).expect("create");
    let show = position(&fx, |e| matches!(e, Effect::ShowPeek { tab } if *tab == peek)).expect("show");
    assert!(create < show);
    assert!(!has(&fx, |e| matches!(e, Effect::ShowContent { .. })), "content stays: {fx:?}");
    assert!(!h.store.state().items.contains_key(&peek), "peek is runtime only");
    let ui = h.ui();
    let pv = ui.peek.clone().unwrap();
    assert_eq!((pv.tab.id, pv.tab.section, pv.popup, pv.tab.host.as_str()), (peek, None, false, "other.org"));
    assert_eq!(ui.focused_tab, Some(peek));
    assert_eq!(ui.current.as_ref().unwrap().section, None);
    assert_eq!(h.store.tab(peek).unwrap().opener, Some(p));
    assert_eq!(h.store.tab_section(peek), None);
    // Commands target Peek while it is open.
    assert_eq!(h.apply(Command::Reload { tab: None, ignore_cache: false }), vec![Effect::Reload { tab: peek, ignore_cache: false }]);
    h.apply(Command::TabTitleChanged { tab: peek, title: "Other".into() });
    assert_eq!(h.ui().peek.unwrap().tab.title, "Other");

    // Expand: top of Today, same browser, activated.
    let fx = h.apply(Command::ExpandPeek { split: false });
    assert!(!has(&fx, |e| matches!(e, Effect::CreateBrowser { .. } | Effect::DestroyBrowser { .. })), "{fx:?}");
    let hide = position(&fx, |e| matches!(e, Effect::HidePeek { tab } if *tab == peek)).expect("hide");
    let show = position(&fx, |e| matches!(e, Effect::ShowContent { layout: ContentLayout::Single { tab } } if *tab == peek)).expect("show");
    assert!(hide < show);
    assert_eq!(h.today()[0], peek);
    assert_eq!(h.active(), Some(peek));
    assert_eq!(h.tab(peek).title, "Other");
    assert!(h.store.peek_tab().is_none());
    assert!(h.apply(Command::ExpandPeek { split: false }).is_empty());
}

#[test]
fn peek_close_paths() {
    let mut h = Harness::new();
    let t = h.open("https://today.com/");
    let p = h.open_pinned("https://mail.example.com/");
    let peek_link = |h: &mut Harness| {
        h.apply(Command::LinkOpenRequested { opener: p, url: "https://other.org/".into(), disposition: LinkDisposition::PinnedCrossSite });
        h.store.peek_tab().unwrap()
    };
    // Click outside (another browser got focus).
    let k = peek_link(&mut h);
    let fx = h.apply(Command::ClosePeek { focus_lost: true });
    assert!(has(&fx, |e| is_destroy(e, k)) && has(&fx, |e| matches!(e, Effect::HidePeek { .. })));
    assert!(!has(&fx, |e| matches!(e, Effect::FocusBrowser { .. })), "the user focused something else");
    // × button refocuses the content.
    peek_link(&mut h);
    let fx = h.apply(Command::ClosePeek { focus_lost: false });
    assert!(has(&fx, |e| matches!(e, Effect::FocusBrowser { tab } if *tab == p)));
    // Ctrl+W closes Peek only.
    peek_link(&mut h);
    h.apply(Command::CloseItem { id: None });
    assert!(h.store.peek_tab().is_none());
    assert_eq!(h.active(), Some(p));
    assert!(h.store.is_loaded(p));
    // The underlying tab got focus.
    let k = peek_link(&mut h);
    let fx = h.apply(Command::TabFocused { tab: p });
    assert!(has(&fx, |e| is_destroy(e, k)));
    // A second link replaces the first Peek.
    let k1 = peek_link(&mut h);
    let fx = h.apply(Command::LinkOpenRequested { opener: p, url: "https://third.net/".into(), disposition: LinkDisposition::PinnedCrossSite });
    let k2 = h.store.peek_tab().unwrap();
    assert_ne!(k1, k2);
    assert!(has(&fx, |e| is_destroy(e, k1)) && has(&fx, |e| is_create(e, k2)));
    // Activating an item or switching space closes Peek.
    h.apply(Command::ActivateItem { id: t });
    assert!(h.store.peek_tab().is_none());
    peek_link(&mut h);
    h.new_space("Work", "🚀", LAGOON);
    assert!(h.store.peek_tab().is_none());
    // Command bar open: click-outside is ignored (the bar took focus).
    h.apply(Command::SwitchSpaceNth { n: 1 });
    h.apply(Command::ActivateItem { id: p });
    peek_link(&mut h);
    h.apply(Command::OpenCommandBar { mode: CommandBarMode::NewTab, split_side: None });
    h.apply(Command::ClosePeek { focus_lost: true });
    assert!(h.store.peek_tab().is_some());
    // Page closed itself.
    let k = h.store.peek_tab().unwrap();
    let fx = h.page_closed(k);
    assert!(has(&fx, |e| matches!(e, Effect::HidePeek { tab } if *tab == k)));
    assert!(h.store.peek_tab().is_none());
    assert!(h.store.state().archive.iter().all(|e| e.id != k), "Peek never archives");
    assert!(h.apply(Command::ClosePeek { focus_lost: false }).is_empty());
}

#[test]
fn peek_expand_into_split_with_opener() {
    let mut h = Harness::new();
    let t = h.open("https://today.com/");
    h.apply(Command::LinkOpenRequested { opener: t, url: "https://other.org/".into(), disposition: LinkDisposition::NewWindow });
    let k = h.store.peek_tab().expect("shift+click opens Peek from any tab");
    h.apply(Command::ExpandPeek { split: true });
    let sid = h.active().unwrap();
    assert_eq!(h.split(sid).panes, vec![t, k]);
    assert_eq!(h.focused(), Some(k));
    // Opener is pinned → it is duplicated.
    let p = h.open_pinned("https://mail.example.com/");
    h.apply(Command::LinkOpenRequested { opener: p, url: "https://other.org/2".into(), disposition: LinkDisposition::PinnedCrossSite });
    let k2 = h.store.peek_tab().unwrap();
    h.apply(Command::ExpandPeek { split: true });
    let sid2 = h.active().unwrap();
    let panes = h.split(sid2).panes;
    assert_eq!(panes[1], k2);
    assert_ne!(panes[0], p);
    assert_eq!(h.pinned(), vec![p]);
}

/// Alt+click / Alt+middle-click on a link (`LinkDisposition::Preview`, PROTOCOL §13): the gesture
/// previews the URL from *any* tab, replaces an open Peek instead of nesting, and keeps every rule
/// the other Peek paths have.
#[test]
fn alt_click_previews_a_link_from_any_tab() {
    let mut h = Harness::new();
    let t = h.open("https://today.com/");
    let preview = |h: &mut Harness, opener: Id, url: &str| {
        let fx = h.apply(Command::LinkOpenRequested { opener, url: url.into(), disposition: LinkDisposition::Preview });
        (h.store.peek_tab(), fx)
    };

    // A plain Today tab, same site: still a Peek (unlike `PinnedCrossSite`, which needs both).
    let (peek, fx) = preview(&mut h, t, "https://today.com/deep");
    let k = peek.expect("Alt+click peeks from a Today tab");
    let create = position(&fx, |e| is_create(e, k)).expect("create");
    let show = position(&fx, |e| matches!(e, Effect::ShowPeek { tab } if *tab == k)).expect("show");
    assert!(create < show);
    assert!(!has(&fx, |e| matches!(e, Effect::ShowContent { .. })), "the page underneath stays: {fx:?}");
    assert_eq!(h.store.tab(k).unwrap().opener, Some(t));
    assert_eq!(h.today(), vec![t], "no tab is opened");
    let pv = h.ui().peek.clone().unwrap();
    assert!(!pv.popup, "Split/Expand are offered");
    assert_eq!(h.ui().focused_tab, Some(k));

    // Alt+click *inside* Peek replaces the page Peek shows and inherits the Peek's opener, so
    // Expand-to-split still splits against the tab the preview came from.
    let (peek2, fx) = preview(&mut h, k, "https://other.org/x");
    let k2 = peek2.expect("still one Peek");
    assert_ne!(k2, k);
    assert!(has(&fx, |e| is_destroy(e, k)), "the previous Peek browser is destroyed: {fx:?}");
    assert_eq!(h.store.tab(k2).unwrap().opener, Some(t), "the Peek's own opener is inherited");
    h.apply(Command::ExpandPeek { split: true });
    let sid = h.active().unwrap();
    assert_eq!(h.split(sid).panes, vec![t, k2]);

    // From a split pane (the opener is a pane, and the split stays exactly as it was).
    let panes = h.split(sid).panes.clone();
    let (peek_pane, fx) = preview(&mut h, panes[0], "https://pane.example/");
    assert_eq!(h.store.tab(peek_pane.unwrap()).unwrap().opener, Some(panes[0]));
    assert_eq!(h.split(sid).panes, panes, "the split is untouched");
    assert!(!has(&fx, |e| matches!(e, Effect::ShowContent { .. })), "the split stays on screen: {fx:?}");
    h.apply(Command::ClosePeek { focus_lost: false });

    // From a pinned tab, same site — `PinnedCrossSite` would not peek this, Preview does.
    let p = h.open_pinned("https://mail.example.com/inbox");
    let (peek3, _) = preview(&mut h, p, "https://mail.example.com/sent");
    assert_eq!(h.store.tab(peek3.unwrap()).unwrap().opener, Some(p));
    h.apply(Command::ClosePeek { focus_lost: false });

    // A URL web content may not open is refused with the same toast, and nothing opens.
    let today_before = h.today();
    for (url, message) in [
        ("javascript:alert(1)", "Blocked a javascript: link"),
        ("sta://settings/", "Blocked a sta: link"),
        ("chrome-extension://aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa/p.html", "Blocked a chrome-extension: link"),
        ("file:///C:/Windows/win.ini", "Blocked a file: link"),
        ("mailto:a@b.c", "Blocked a mailto: link"),
    ] {
        let (peek, fx) = preview(&mut h, t, url);
        assert!(peek.is_none(), "{url} must not open a Peek");
        assert!(!has(&fx, |e| matches!(e, Effect::CreateBrowser { .. })), "{url}: {fx:?}");
        assert_eq!(h.toast().unwrap().message, message);
        assert_eq!(h.today(), today_before, "{url} opened a tab");
    }
    // An empty URL (a link the renderer could not resolve) is dropped silently.
    assert!(h.apply(Command::LinkOpenRequested { opener: t, url: "   ".into(), disposition: LinkDisposition::Preview }).is_empty());
    // An unknown opener still previews (the tab may have gone while the message travelled).
    let (peek, _) = preview(&mut h, 9999, "https://orphan.org/");
    assert_eq!(h.store.tab(peek.unwrap()).unwrap().opener, None);
    h.apply(Command::ClosePeek { focus_lost: false });

    // A popup (sign-in) Peek is never thrown away by a gesture made in another tab…
    h.apply(Command::ActivateItem { id: t });
    let (login, _) = h.popup(Some(t), "https://accounts.example.com/auth", true, true);
    let (peek, fx) = preview(&mut h, t, "https://news.com/");
    assert_eq!(peek, Some(login), "the login flow keeps Peek");
    let news = *h.today().last().unwrap();
    assert!(has(&fx, |e| is_create(e, news)), "…the link opens in a tab: {fx:?}");
    assert_ne!(h.active(), Some(news), "…in the background, so the login flow keeps the screen");
    // …but an Alt+click inside it is the user asking for that Peek to move on.
    let (peek, _) = preview(&mut h, login, "https://accounts.example.com/next");
    let moved = peek.expect("peek");
    assert_ne!(moved, login);
    assert!(!h.ui().peek.unwrap().popup);
    h.apply(Command::ClosePeek { focus_lost: false });

    // Peek off in settings → a foreground Today tab, like Shift+click with Peek off.
    h.apply(Command::UpdateSettings { patch: SettingsPatch { peek_enabled: Some(false), ..Default::default() } });
    let (peek, _) = preview(&mut h, t, "https://nopeek.org/");
    assert!(peek.is_none());
    assert_eq!(h.tab(h.active().unwrap()).url, "https://nopeek.org/");
}

/// `DownloadInBlankTab` (PROTOCOL §13): the link a Peek was opened for turned out to be a file, so
/// the overlay would sit there empty over the page. It closes itself, the download runs on, and a
/// Peek that *has* a page is never touched by a download started inside it.
#[test]
fn a_download_closes_a_peek_that_never_showed_a_page() {
    let mut h = Harness::new();
    let t = h.open("https://today.com/");
    let dl = |id: u32, tab: Id, state: DownloadState| Download {
        id,
        tab: Some(tab),
        url: "https://files.example.com/report.pdf".into(),
        file_name: "report.pdf".into(),
        path: Some("C:\\Users\\me\\Downloads\\report.pdf".into()),
        received_bytes: 10,
        total_bytes: Some(1000),
        bytes_per_sec: 100,
        state,
        started_at: 0,
    };

    // Alt+click an attachment link: the Peek opens, the response is a download, the card is empty.
    h.apply(Command::LinkOpenRequested { opener: t, url: "https://files.example.com/report.pdf".into(), disposition: LinkDisposition::Preview });
    let k = h.store.peek_tab().expect("a Peek opened for the link");
    h.apply(Command::DownloadUpdated { download: dl(1, k, DownloadState::InProgress) });
    let fx = h.apply(Command::DownloadInBlankTab { tab: k });
    assert_eq!(h.store.peek_tab(), None, "the empty preview closes itself");
    assert!(has(&fx, |e| matches!(e, Effect::HidePeek { tab } if *tab == k)), "the overlay is hidden: {fx:?}");
    assert!(!has(&fx, |e| is_destroy(e, k)), "…but its browser stays while the download runs: {fx:?}");
    assert_eq!(h.today(), vec![t], "no tab is left behind either");
    // The file still arrives, and only then is the browser destroyed.
    let fx = h.apply(Command::DownloadUpdated { download: dl(1, k, DownloadState::Complete) });
    assert!(has(&fx, |e| is_destroy(e, k)), "{fx:?}");
    assert_eq!(h.toast().unwrap().message, "Downloaded report.pdf", "the toast is the whole outcome");

    // The same rule for a **tab** (P5). The shell reports `DownloadInBlankTab` only for a browser
    // that never committed a document, so this tab is the blank white rectangle that opening a
    // download URL as a page used to leave behind for good — its URL pill naming a file that was
    // never on screen. It is closed, without going on the reopen stack (Ctrl+Shift+T must not offer
    // to start the download again), and the overlay is not its business either way.
    h.apply(Command::LinkOpenRequested { opener: t, url: "https://news.com/".into(), disposition: LinkDisposition::Preview });
    let k2 = h.store.peek_tab().expect("peek");
    h.apply(Command::DownloadUpdated { download: dl(2, k2, DownloadState::InProgress) });
    h.apply(Command::DownloadInBlankTab { tab: t });
    assert!(!h.today().contains(&t), "the blank tab is gone: {:?}", h.today());
    assert!(h.store.state().reopen.is_empty(), "and it is not offered back: {:?}", h.store.state().reopen);
    assert_eq!(h.store.peek_tab(), Some(k2), "the overlay is untouched");
    assert!(h.apply(Command::DownloadInBlankTab { tab: 9999 }).is_empty(), "an unknown tab is ignored");
    assert_eq!(h.store.peek_tab(), Some(k2));
}

#[test]
fn popups_adopted_as_peek_or_tabs() {
    let mut h = Harness::new();
    let t = h.open("https://app.com/");
    // OAuth popup from a Today tab → popup Peek (browser already exists: no CreateBrowser).
    let (k, fx) = h.popup(Some(t), "https://accounts.example.com/auth", true, true);
    assert!(!has(&fx, |e| is_create(e, k)));
    assert!(has(&fx, |e| matches!(e, Effect::ShowPeek { tab } if *tab == k)));
    assert!(h.ui().peek.unwrap().popup);
    // Popup Peeks ignore click-outside and focus changes.
    h.apply(Command::ClosePeek { focus_lost: true });
    h.apply(Command::TabFocused { tab: t });
    assert_eq!(h.store.peek_tab(), Some(k));
    // A Peek-candidate link while a popup Peek is open becomes a background Today tab (activating
    // it would close the login flow).
    let fx = h.apply(Command::LinkOpenRequested { opener: t, url: "https://news.com/".into(), disposition: LinkDisposition::NewWindow });
    assert_eq!(h.store.peek_tab(), Some(k));
    assert_eq!(h.today().len(), 2);
    let news = h.today()[1];
    assert!(has(&fx, |e| is_create(e, news)));
    assert_eq!(h.active(), Some(t));
    assert_eq!(h.store.peek_tab(), h.focused());
    // Same for a nested feature popup.
    let (nested, _) = h.popup(Some(k), "https://accounts.example.com/2fa", true, true);
    assert_eq!(h.store.peek_tab(), Some(k));
    assert_eq!(h.store.tab_section(nested), Some(Section::Today));
    assert_eq!(h.active(), Some(t));
    // The popup calls window.close().
    h.page_closed(k);
    assert!(h.store.peek_tab().is_none());

    // target=_blank from a Today tab: foreground tab below the opener.
    h.apply(Command::ActivateItem { id: t });
    let (n, fx) = h.popup(Some(t), "https://blank.com/", false, true);
    assert!(!has(&fx, |e| is_create(e, n)));
    assert!(has(&fx, |e| matches!(e, Effect::ShowContent { layout: ContentLayout::Single { tab } } if *tab == n)));
    let today = h.today();
    assert_eq!(today[today.iter().position(|x| *x == t).unwrap() + 1], n);
    assert_eq!(h.active(), Some(n));
    // Background popup: not activated.
    let (bg, _) = h.popup(Some(n), "https://bg.com/", false, false);
    assert_eq!(h.active(), Some(n));
    assert!(h.store.is_loaded(bg));
    // Non-popup from a pinned opener to another site → Peek.
    let p = h.open_pinned("https://mail.example.com/");
    let (pk, _) = h.popup(Some(p), "https://elsewhere.org/", false, true);
    assert_eq!(h.store.peek_tab(), Some(pk));
    h.apply(Command::ClosePeek { focus_lost: false });
    // Same site from a pinned opener → Today tab at the top.
    let (same, _) = h.popup(Some(p), "https://docs.example.com/", false, true);
    assert_eq!(h.today()[0], same);
    // Internal URLs from web content are loaded as about:blank.
    let (evil, fx) = h.popup(Some(same), "sta://settings/", false, true);
    assert!(has(&fx, |e| matches!(e, Effect::LoadUrl { tab, url } if *tab == evil && url == "about:blank")));
    assert_eq!(h.tab(evil).url, "about:blank");
    // Peek disabled: feature popups become foreground tabs.
    h.apply(Command::UpdateSettings { patch: SettingsPatch { peek_enabled: Some(false), ..Default::default() } });
    let (np, _) = h.popup(Some(same), "https://accounts.example.com/", true, false);
    assert!(h.store.peek_tab().is_none());
    assert_eq!(h.active(), Some(np));
    assert_eq!(h.store.intercept_navigation(p, "https://other.org/", true, false), None);
    h.apply(Command::LinkOpenRequested { opener: p, url: "https://other.org/".into(), disposition: LinkDisposition::PinnedCrossSite });
    assert!(h.store.peek_tab().is_none());
    assert_eq!(h.tab(h.focused().unwrap()).url, "https://other.org/");
    // Duplicate adoption of an existing id is ignored.
    assert!(h.apply(Command::PopupAdopted { tab: np, opener: None, url: "https://x.com/".into(), popup: false, foreground: true }).is_empty());
}

#[test]
fn restore_whole_group_with_more_entries_than_panes_keeps_every_tab() {
    let mut h = Harness::new();
    let x = h.open("https://x.com/");
    let a = h.open("https://a.com/");
    let b = h.open("https://b.com/");
    let c = h.open("https://c.com/");
    let d = h.open("https://d.com/");
    h.apply(Command::SplitWith { tab: b, with: a, side: SplitSide::Right });
    let sid = h.active().unwrap();
    h.apply(Command::SplitWith { tab: c, with: b, side: SplitSide::Right });
    h.apply(Command::SplitWith { tab: d, with: c, side: SplitSide::Right });
    assert_eq!(h.split(sid).panes, vec![a, b, c, d]);
    // Close one pane, fill the split up again, then close the whole split: five archive entries
    // share the group, two of them recorded at pane index 1.
    h.apply(Command::CloseItem { id: Some(b) });
    h.advance(MIN);
    let e = h.open("https://e.com/");
    h.apply(Command::SplitWith { tab: e, with: c, side: SplitSide::Right });
    assert_eq!(h.split(sid).panes, vec![a, c, e, d]);
    h.advance(MIN);
    h.apply(Command::CloseItem { id: Some(sid) });
    let group: Vec<(Id, usize)> =
        h.store.state().archive.iter().filter_map(|en| en.split.as_ref().filter(|s| s.group == sid).map(|s| (en.id, s.pane_index))).collect();
    assert_eq!(group.len(), 5, "{group:?}");
    assert_eq!(h.today(), vec![x]);

    let fx = h.apply(Command::RestoreArchived { id: a, whole_group: true });
    // The split as it was last closed comes back; the earlier-closed pane is a Today tab below it.
    assert_eq!(h.active(), Some(sid));
    assert_eq!(h.split(sid).panes, vec![a, c, e, d]);
    assert_eq!(h.focused(), Some(e), "focus as it was when the split was closed");
    assert_eq!(h.today(), vec![sid, b, x]);
    for t in [a, b, c, d, e] {
        assert!(h.store.tab(t).is_some(), "tab {t} lost");
        assert!(h.store.state().archive.iter().all(|en| en.id != t), "tab {t} still archived");
    }
    for t in [a, c, e, d] {
        assert!(has(&fx, |ef| is_create(ef, t)));
    }
    assert!(!h.store.is_loaded(b), "the extra tab loads when activated");
}

#[test]
fn reopening_a_pane_rejoins_its_dissolved_split() {
    let mut h = Harness::new();
    let x = h.open("https://x.com/");
    let a = h.open("https://a.com/");
    let b = h.open("https://b.com/");
    h.apply(Command::SplitWith { tab: b, with: a, side: SplitSide::Bottom });
    let sid = h.active().unwrap();
    h.apply(Command::SetSplitFractions { id: sid, fractions: vec![0.3, 0.7] });
    assert_eq!(h.split(sid).panes, vec![a, b]);
    // Closing the focused pane dissolves the 2-pane split into `a`.
    h.apply(Command::CloseItem { id: None });
    assert_eq!(h.today(), vec![a, x]);
    assert!(!h.store.state().items.contains_key(&sid));
    // Ctrl+Shift+T: `a` still sits where the split was, so the split is rebuilt around it.
    let fx = h.apply(Command::ReopenClosed);
    assert_eq!(h.today(), vec![sid, x]);
    assert_eq!(h.active(), Some(sid));
    let s = h.split(sid);
    assert_eq!((s.panes.clone(), s.orientation), (vec![a, b], Orientation::Vertical));
    assert!((s.fractions[0] - 0.3).abs() < 1e-4, "{:?}", s.fractions);
    assert_eq!(h.focused(), Some(b));
    assert!(has(&fx, |e| is_create(e, b)));
    assert!(matches!(shows(&fx), Some(ContentLayout::Split { orientation: Orientation::Vertical, focused: 1, .. })));

    // First pane: the order is kept.
    h.apply(Command::CloseItem { id: Some(a) });
    assert_eq!(h.today(), vec![b, x]);
    h.apply(Command::RestoreArchived { id: a, whole_group: false });
    assert_eq!(h.split(sid).panes, vec![a, b]);
    assert_eq!(h.focused(), Some(a));

    // The surviving pane moved within Today: it is found by id, wherever it is now.
    h.apply(Command::CloseItem { id: Some(a) });
    h.apply(Command::MoveItem { id: b, to: DropTarget { container: Container::Today { space: h.space() }, before: None } });
    assert_eq!(h.today(), vec![x, b]);
    h.apply(Command::ReopenClosed);
    assert_eq!(h.today(), vec![x, sid]);
    assert_eq!(h.split(sid).panes, vec![a, b]);
    assert_eq!(h.active(), Some(sid));

    // The surviving pane is no longer a plain Today tab (pinned): the pane comes back on its own.
    h.apply(Command::CloseItem { id: Some(a) });
    assert_eq!(h.today(), vec![x, b]);
    h.apply(Command::TogglePin { id: Some(b) });
    h.apply(Command::ReopenClosed);
    assert_eq!(h.today(), vec![x, a]);
    assert_eq!(h.active(), Some(a));
    assert_eq!(h.pinned(), vec![b]);
    assert!(!h.store.state().items.contains_key(&sid));
}

#[test]
fn rejoin_finds_the_surviving_pane_by_id_after_today_shifted() {
    let mut h = Harness::new();
    let x = h.open("https://x.com/");
    let a = h.open("https://a.com/");
    let b = h.open("https://b.com/");
    h.apply(Command::SplitWith { tab: b, with: a, side: SplitSide::Right });
    let sid = h.active().unwrap();
    assert_eq!(h.today(), vec![sid, x]);
    // Closing `b` dissolves the split into `a` at index 0 (recorded in b's archive entry).
    h.apply(Command::CloseItem { id: Some(b) });
    assert_eq!(h.today(), vec![a, x]);
    // New tabs above push `a` down; an unrelated tab now sits at the recorded index.
    let n1 = h.open("https://n1.com/");
    let n2 = h.open("https://n2.com/");
    assert_eq!(h.today(), vec![n2, n1, a, x]);
    h.apply(Command::RestoreArchived { id: b, whole_group: false });
    assert_eq!(h.today(), vec![n2, n1, sid, x], "rejoined where `a` is, not merged with the tab at index 0");
    assert_eq!(h.split(sid).panes, vec![a, b]);
    assert_eq!(h.active(), Some(sid));
    assert_eq!(h.focused(), Some(b));
    assert!(h.store.tab_section(n2) == Some(Section::Today) && h.store.tab_section(n1) == Some(Section::Today));
}

#[test]
fn legacy_snapshots_without_pane_ids_never_merge_unrelated_tabs() {
    // A profile saved before snapshots listed their panes: a 2-pane snapshot and an unrelated
    // Today tab sitting at the split's recorded index.
    let state = serde_json::json!({
        "version": 1,
        "nextId": 10,
        "spaces": [{ "id": 1, "name": "Home", "icon": "🏠", "pinned": [], "today": [2], "activeItem": 2 }],
        "items": { "2": { "kind": "tab", "id": 2, "url": "https://unrelated.com/", "title": "Unrelated" } },
        "window": { "activeSpace": 1 },
        "archive": [{
            "id": 3, "url": "https://b.com/", "title": "B", "archivedAt": T0, "reason": "userClosed",
            "space": 1, "section": "today", "index": 0,
            "split": { "group": 9, "orientation": "horizontal", "fractions": [0.5, 0.5], "focused": 1, "paneIndex": 1 }
        }],
        "reopen": [{ "type": "archived", "archiveId": 3 }]
    });
    let (store, report) = Store::load(Some(&state.to_string()), None, T0);
    assert!(!report.state_corrupt, "{report:?}");
    let mut h = Harness::start(store, Vec::new());
    h.apply(Command::ReopenClosed);
    assert_eq!(h.today(), vec![3, 2]);
    assert!(h.store.state().items.values().all(|i| !matches!(i, Item::Split(_))), "no split rebuilt around an unrelated tab");
    assert_eq!(h.active(), Some(3));
}

#[test]
fn whole_group_and_single_pane_restores_keep_the_original_pane_order() {
    let mut h = Harness::new();
    let x = h.open("https://x.com/");
    let a = h.open("https://a.com/");
    let b = h.open("https://b.com/");
    let c = h.open("https://c.com/");
    h.apply(Command::SplitWith { tab: b, with: a, side: SplitSide::Right });
    let sid = h.active().unwrap();
    h.apply(Command::SplitWith { tab: c, with: b, side: SplitSide::Right });
    assert_eq!(h.split(sid).panes, vec![a, b, c]);
    h.apply(Command::FocusPane { index: 2 });
    // Close the middle pane, then the rest of the split: `b` and `c` were both at pane index 1
    // at their close, so positions alone can't order them.
    h.apply(Command::CloseItem { id: Some(b) });
    h.advance(MIN);
    assert_eq!(h.split(sid).panes, vec![a, c]);
    h.apply(Command::CloseItem { id: Some(sid) });
    assert_eq!(h.today(), vec![x]);
    h.apply(Command::RestoreArchived { id: c, whole_group: true });
    assert_eq!(h.active(), Some(sid));
    assert_eq!(h.split(sid).panes, vec![a, b, c]);
    assert_eq!(h.focused(), Some(c), "the pane focused at the last close");
    let fractions = h.split(sid).fractions;
    assert!(fractions.iter().all(|f| (f - 1.0 / 3.0).abs() < 1e-4), "{fractions:?}");

    // Single panes restored into the still-existing split go back between their old neighbors.
    let d = h.open("https://d.com/");
    h.apply(Command::SplitWith { tab: d, with: c, side: SplitSide::Right });
    assert_eq!(h.split(sid).panes, vec![a, b, c, d]);
    h.apply(Command::CloseItem { id: Some(a) });
    h.apply(Command::CloseItem { id: Some(c) });
    assert_eq!(h.split(sid).panes, vec![b, d]);
    h.apply(Command::RestoreArchived { id: c, whole_group: false });
    assert_eq!(h.split(sid).panes, vec![b, c, d]);
    h.apply(Command::RestoreArchived { id: a, whole_group: false });
    assert_eq!(h.split(sid).panes, vec![a, b, c, d]);
    assert_eq!(h.focused(), Some(a));
}

#[test]
fn peek_is_hidden_before_its_browser_is_destroyed() {
    let mut h = Harness::new();
    let t = h.open("https://today.com/");
    let p = h.open_pinned("https://mail.example.com/");
    let peek_link = |h: &mut Harness| {
        h.apply(Command::LinkOpenRequested { opener: p, url: "https://other.org/".into(), disposition: LinkDisposition::PinnedCrossSite });
        h.store.peek_tab().expect("peek")
    };
    let hidden_first = |fx: &[Effect], k: Id| {
        let hide = position(fx, |e| matches!(e, Effect::HidePeek { tab } if *tab == k)).unwrap_or_else(|| panic!("no HidePeek: {fx:?}"));
        let destroy = position(fx, |e| is_destroy(e, k)).unwrap_or_else(|| panic!("no DestroyBrowser: {fx:?}"));
        assert!(hide < destroy, "HidePeek must precede DestroyBrowser: {fx:?}");
    };
    let closers: [fn(Id, Id, Id) -> Command; 6] = [
        |_, _, _| Command::ClosePeek { focus_lost: false },
        |_, _, _| Command::ClosePeek { focus_lost: true },
        |_, _, _| Command::CloseItem { id: None },
        |k, _, _| Command::CloseItem { id: Some(k) },
        |_, p, _| Command::TabFocused { tab: p },
        |_, _, t| Command::ActivateItem { id: t },
    ];
    for close in closers {
        h.apply(Command::ActivateItem { id: p });
        let k = peek_link(&mut h);
        let cmd = close(k, p, t);
        let fx = h.apply(cmd.clone());
        assert!(h.store.peek_tab().is_none(), "{cmd:?}");
        hidden_first(&fx, k);
    }
    // Replacing a Peek hides the old one before destroying it.
    let k1 = peek_link(&mut h);
    let fx = h.apply(Command::LinkOpenRequested { opener: p, url: "https://third.net/".into(), disposition: LinkDisposition::PinnedCrossSite });
    hidden_first(&fx, k1);
    assert!(has(&fx, |e| matches!(e, Effect::ShowPeek { tab } if Some(*tab) == h.store.peek_tab())));
    // Switching space.
    let k2 = h.store.peek_tab().unwrap();
    let fx = h.apply(Command::NewSpace { name: "Work".into(), icon: "🚀".into(), theme: LAGOON });
    hidden_first(&fx, k2);
}

#[test]
fn click_outside_peek_is_ignored_only_for_prompts_it_would_cover() {
    let mut h = Harness::new();
    let bg = h.open("https://background.com/");
    let p = h.open_pinned("https://mail.example.com/");
    let peek_link = |h: &mut Harness| {
        h.apply(Command::LinkOpenRequested { opener: p, url: "https://other.org/".into(), disposition: LinkDisposition::PinnedCrossSite });
        h.store.peek_tab().expect("peek")
    };
    let prompt = |id: u64, tab: Id, origin: &str| Command::PermissionRequested { id, tab, origin: origin.into(), kinds: vec![PermissionKind::Camera] };
    // A prompt for a background tab isn't showing: click outside still closes Peek.
    h.apply(prompt(1, bg, "https://background.com"));
    assert!(h.ui().permission_prompts.iter().any(|x| x.id == 1));
    peek_link(&mut h);
    h.apply(Command::ClosePeek { focus_lost: true });
    assert!(h.store.peek_tab().is_none());
    // A prompt for the visible tab took focus: click outside is ignored.
    h.apply(prompt(2, p, "https://mail.example.com"));
    peek_link(&mut h);
    h.apply(Command::ClosePeek { focus_lost: true });
    assert!(h.store.peek_tab().is_some());
    h.apply(Command::ResolvePermission { id: 2, allow: false, remember: false });
    h.apply(Command::ClosePeek { focus_lost: true });
    assert!(h.store.peek_tab().is_none());
    // A prompt for the Peek tab itself too.
    let k = peek_link(&mut h);
    h.apply(prompt(3, k, "https://other.org"));
    h.apply(Command::ClosePeek { focus_lost: true });
    assert_eq!(h.store.peek_tab(), Some(k));
    h.apply(Command::ClosePeek { focus_lost: false });
    assert!(h.store.peek_tab().is_none());
}
