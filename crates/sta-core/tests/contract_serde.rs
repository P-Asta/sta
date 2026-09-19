//! Locks the JSON wire format shared with the HTML UI (docs/PROTOCOL.md).

use sta_core::*;
use serde_json::json;

fn cmd(v: serde_json::Value) -> Command {
    serde_json::from_value(v.clone()).unwrap_or_else(|e| panic!("{v} -> {e}"))
}

#[test]
fn commands_parse_from_protocol_examples() {
    assert_eq!(cmd(json!({"type":"activateItem","id":12})), Command::ActivateItem { id: 12 });
    assert_eq!(cmd(json!({"type":"closeItem"})), Command::CloseItem { id: None });
    assert_eq!(cmd(json!({"type":"reload"})), Command::Reload { tab: None, ignore_cache: false });
    assert_eq!(
        cmd(json!({"type":"openInput","text":"rust lang","target":"newTab"})),
        Command::OpenInput { text: "rust lang".into(), target: OpenTarget::NewTab }
    );
    assert_eq!(
        cmd(json!({"type":"moveItem","id":12,"to":{"container":{"type":"pinned","space":3},"before":7}})),
        Command::MoveItem { id: 12, to: DropTarget { container: Container::Pinned { space: 3 }, before: Some(7) } }
    );
    assert_eq!(
        cmd(json!({"type":"openCommandBar","mode":"editUrl"})),
        Command::OpenCommandBar { mode: CommandBarMode::EditUrl, split_side: None }
    );
    assert_eq!(
        cmd(json!({"type":"commitOmnibox","command":{"type":"activateItem","id":3},"alt":true})),
        Command::CommitOmnibox { command: Box::new(Command::ActivateItem { id: 3 }), alt: true }
    );
    assert_eq!(
        cmd(json!({"type":"openSidebarPanel","panel":{"type":"editSpace","id":4}})),
        Command::OpenSidebarPanel { panel: SidebarPanel::EditSpace { id: 4 } }
    );
    assert_eq!(
        cmd(json!({"type":"openSidebarPanel","panel":{"type":"editPinned","id":5}})),
        Command::OpenSidebarPanel { panel: SidebarPanel::EditPinned { id: 5 } }
    );
    assert_eq!(
        cmd(json!({"type":"findInPage","text":"x"})),
        Command::FindInPage { tab: None, text: "x".into(), forward: true, match_case: false, find_next: false }
    );
    assert_eq!(
        cmd(json!({"type":"newSpace","name":"Work","icon":"🚀"})),
        Command::NewSpace { name: "Work".into(), icon: "🚀".into(), theme: Theme::default() }
    );
    assert_eq!(
        cmd(json!({"type":"updateSettings","patch":{"searchEngine":"duckDuckGo"}})),
        Command::UpdateSettings { patch: SettingsPatch { search_engine: Some(SearchEngineId::DuckDuckGo), ..Default::default() } }
    );
    assert_eq!(cmd(json!({"type":"closeCommandBar"})), Command::CloseCommandBar { seq: None });
    assert_eq!(cmd(json!({"type":"closeCommandBar","seq":7})), Command::CloseCommandBar { seq: Some(7) });
    assert_eq!(
        cmd(json!({"type":"updateSettings","patch":{"animations":{"enabled":false,"set":{"overlays.toast":null}}}})),
        Command::UpdateSettings {
            patch: SettingsPatch {
                animations: Some(AnimationsPatch {
                    enabled: Some(false),
                    set: [("overlays.toast".to_string(), None)].into(),
                    ..Default::default()
                }),
                ..Default::default()
            }
        }
    );
    assert_eq!(cmd(json!({"type":"systemAnimationsChanged","enabled":false})), Command::SystemAnimationsChanged { enabled: false });
}

#[test]
fn motion_serializes_camel_case() {
    // `UiState.motion` (PROTOCOL §3) and the stored `settings.animations`.
    let view = MotionView { level: MotionLevel::Reduced, off: vec!["menus.popIn".into()], system_animations: false };
    let v = serde_json::to_value(&view).unwrap();
    assert_eq!(v, json!({"level":"reduced","off":["menus.popIn"],"systemAnimations":false}));
    assert_eq!(serde_json::from_value::<MotionView>(v).unwrap(), view);
    let stored = serde_json::to_value(AnimationSettings::default()).unwrap();
    assert_eq!(stored, json!({"enabled":true,"followSystem":true,"groups":{},"choices":{}}));
    // Every registry key is a legal JSON object key and appears in `UiState.motion.off` verbatim.
    let mut all_off = AnimationSettings::default();
    for g in sta_core::motion::GROUPS {
        all_off.groups.insert(g.id.to_string(), false);
    }
    assert_eq!(all_off.off_keys().len(), sta_core::motion::ANIMATIONS.len());
}

#[test]
fn shell_events_are_not_allowed_from_ui() {
    assert!(!Command::Tick.allowed_from_ui());
    assert!(!Command::TabBrowserClosed { tab: 1 }.allowed_from_ui());
    assert!(Command::ToggleSidebar.allowed_from_ui());
}

#[test]
fn tagged_views_serialize_camel_case() {
    let node = NodeView::Folder(FolderView { id: 1, name: "F".into(), collapsed: false, children: vec![] });
    assert_eq!(serde_json::to_value(&node).unwrap(), json!({"kind":"folder","id":1,"name":"F","collapsed":false,"children":[]}));
    let layout = ContentLayout::Single { tab: 5 };
    assert_eq!(serde_json::to_value(&layout).unwrap(), json!({"type":"single","tab":5}));
    let item = Item::Tab(Tab { id: 2, ..Default::default() });
    let v = serde_json::to_value(&item).unwrap();
    assert_eq!(v["kind"], "tab");
    assert_eq!(serde_json::from_value::<Item>(v).unwrap(), item);
}

#[test]
fn effects_serialize_camel_case() {
    // `debug.execute` takes effects as JSON (debug builds).
    let answer = Effect::AnswerPermission { id: 4, allow: false, remember: true };
    let v = serde_json::to_value(&answer).unwrap();
    assert_eq!(v, json!({"type":"answerPermission","id":4,"allow":false,"remember":true}));
    assert_eq!(serde_json::from_value::<Effect>(v).unwrap(), answer);
    assert!(serde_json::from_value::<Effect>(json!({"type":"answerPermission","id":4,"allow":false})).is_err(), "remember is required");
    let external = Effect::OpenExternal { url: "mailto:a@b.c".into() };
    let v = serde_json::to_value(&external).unwrap();
    assert_eq!(v, json!({"type":"openExternal","url":"mailto:a@b.c"}));
    assert_eq!(serde_json::from_value::<Effect>(v).unwrap(), external);
    assert_eq!(
        serde_json::to_value(Effect::SetPageFullscreen { tab: Some(3) }).unwrap(),
        json!({"type":"setPageFullscreen","tab":3})
    );
    let sidebar = Effect::SetSidebar { visible: false, width: 248, floating: true };
    assert_eq!(serde_json::to_value(&sidebar).unwrap(), json!({"type":"setSidebar","visible":false,"width":248,"floating":true}));
    assert_eq!(
        serde_json::from_value::<Effect>(json!({"type":"setSidebar","visible":true,"width":300})).unwrap(),
        Effect::SetSidebar { visible: true, width: 300, floating: false },
        "floating defaults to false"
    );
    let chrome = Effect::SetChrome {
        frame_argb: 0xFF26_222E,
        dark: true,
        accent_argb: 0xFFB8_97F0,
        surface_argb: 0xFF23_2228,
        border_argb: 0xFF39_383E,
        frame_border_argb: 0xFF3B_3843,
    };
    let v = serde_json::to_value(&chrome).unwrap();
    assert_eq!(
        v,
        json!({"type":"setChrome","frameArgb":0xFF26_222Eu32,"dark":true,"accentArgb":0xFFB8_97F0u32,"surfaceArgb":0xFF23_2228u32,"borderArgb":0xFF39_383Eu32,"frameBorderArgb":0xFF3B_3843u32})
    );
    assert_eq!(serde_json::from_value::<Effect>(v).unwrap(), chrome);
    assert_eq!(
        serde_json::from_value::<Effect>(json!({"type":"setChrome","frameArgb":1,"dark":false})).unwrap(),
        Effect::SetChrome { frame_argb: 1, dark: false, accent_argb: 0, surface_argb: 0, border_argb: 0, frame_border_argb: 0 },
        "older JSON without the card colors still parses (0 = derived by the shell)"
    );
}

#[test]
fn commit_omnibox_is_allowed_only_with_an_allowed_inner_command() {
    let commit = |c: Command| Command::CommitOmnibox { command: Box::new(c), alt: false };
    assert!(commit(Command::ActivateItem { id: 1 }).allowed_from_ui());
    assert!(commit(Command::ToggleSidebar).allowed_from_ui());
    assert!(!commit(Command::Tick).allowed_from_ui());
    assert!(!commit(Command::TabBrowserClosed { tab: 1 }).allowed_from_ui());
    assert!(!commit(Command::WindowCloseRequested).allowed_from_ui());
    // Nested commits are never valid (core ignores them), whatever they wrap.
    assert!(!commit(commit(Command::ToggleSidebar)).allowed_from_ui());
    assert!(!commit(commit(Command::Tick)).allowed_from_ui());
    // As parsed from IPC JSON.
    let smuggled = cmd(json!({"type":"commitOmnibox","command":{"type":"popupAdopted","tab":9,"url":"https://x.com/","popup":true,"foreground":true}}));
    assert!(!smuggled.allowed_from_ui());
    let ok = cmd(json!({"type":"commitOmnibox","command":{"type":"openInput","text":"x","target":"newTab"},"alt":true}));
    assert!(ok.allowed_from_ui());
}
