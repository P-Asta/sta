//! Animation settings through the store (`sta_core::motion`, FINAL PLAN §3/§7): the patch order,
//! the view, the runtime-only Windows setting, the `closeCommandBar` `seq` guard, lenient loading
//! and the omnibox action.

mod common;
use common::*;
use serde_json::json;
use sta_core::*;

fn patch(a: AnimationsPatch) -> Command {
    Command::UpdateSettings { patch: SettingsPatch { animations: Some(a), ..SettingsPatch::default() } }
}

#[test]
fn the_patch_reaches_the_store_and_the_view() {
    let mut h = Harness::new();
    assert_eq!(h.ui().motion, MotionView { level: MotionLevel::Full, off: vec![], system_animations: true });
    assert!(h.store.settings().animations.is_default());

    // A group switch and a per-key choice.
    h.apply(patch(AnimationsPatch {
        groups: [("overlays".to_string(), Some(false))].into(),
        set: [("sidebar.reorder".to_string(), Some(false))].into(),
        ..Default::default()
    }));
    let m = h.ui().motion;
    assert_eq!(m.level, MotionLevel::Full);
    assert_eq!(
        m.off,
        vec![
            "sidebar.reorder".to_string(),
            "overlays.toast".to_string(),
            "overlays.switcher".to_string(),
            "overlays.find".to_string(),
            "overlays.permission".to_string(),
            "overlays.peek".to_string(),
        ],
        "off is in registry order"
    );

    // The master switch drops the level for everything, and the choices stay stored.
    h.apply(patch(AnimationsPatch { enabled: Some(false), ..Default::default() }));
    assert_eq!(h.ui().motion.level, MotionLevel::Off);
    assert_eq!(h.ui().motion.off.len(), 6, "per-key state is unchanged by the master switch");

    // Follow Windows: the level is `reduced` only while Windows has animations off.
    h.apply(patch(AnimationsPatch { enabled: Some(true), ..Default::default() }));
    h.apply(Command::SystemAnimationsChanged { enabled: false });
    assert_eq!(h.ui().motion.level, MotionLevel::Reduced);
    assert!(!h.ui().motion.system_animations);
    h.apply(patch(AnimationsPatch { follow_system: Some(false), ..Default::default() }));
    assert_eq!(h.ui().motion.level, MotionLevel::Full, "not following Windows any more");

    // Reset clears every group, choice and scalar.
    h.apply(patch(AnimationsPatch { reset: true, ..Default::default() }));
    assert!(h.store.settings().animations.is_default());
    assert_eq!(h.ui().motion, MotionView { level: MotionLevel::Reduced, off: vec![], system_animations: false });
}

#[test]
fn the_patch_parses_from_ipc_json_and_clears_with_null() {
    let mut h = Harness::new();
    let set_off: Command = serde_json::from_value(json!({
        "type": "updateSettings",
        "patch": {"animations": {"set": {"theme.crossFade": false}, "groups": {"menus": false}}}
    }))
    .expect("patch parses");
    h.apply(set_off);
    assert_eq!(h.store.settings().animations.choices.get("theme.crossFade"), Some(&false));
    assert_eq!(h.store.settings().animations.groups.get("menus"), Some(&false));
    assert!(h.ui().motion.off.contains(&"menus.popIn".to_string()));

    let clear: Command = serde_json::from_value(json!({
        "type": "updateSettings",
        "patch": {"animations": {"set": {"theme.crossFade": null}, "groups": {"menus": null}}}
    }))
    .expect("null clears");
    h.apply(clear);
    assert!(h.store.settings().animations.is_default());
    assert!(h.ui().motion.off.is_empty());

    // Settings survive a save/load round trip.
    h.apply(patch(AnimationsPatch { enabled: Some(false), set: [("pages.enter".to_string(), Some(false))].into(), ..Default::default() }));
    let (store, report) = Store::load(Some(&h.store.state_json()), None, h.now);
    assert!(!report.state_corrupt, "{report:?}");
    assert!(!store.settings().animations.enabled);
    assert_eq!(store.settings().animations.choices.get("pages.enter"), Some(&false));
    assert_eq!(store.state().version, STATE_VERSION, "no version bump");
}

#[test]
fn the_windows_setting_is_runtime_only_and_refused_from_the_ui() {
    assert!(!Command::SystemAnimationsChanged { enabled: false }.allowed_from_ui());
    assert!(
        !Command::CommitOmnibox { command: Box::new(Command::SystemAnimationsChanged { enabled: false }), alt: false }.allowed_from_ui(),
        "and not smuggled inside a commit"
    );

    let mut h = Harness::new();
    let before = h.store.state_json();
    let rev = h.store.revision();
    h.apply(Command::SystemAnimationsChanged { enabled: false });
    assert!(h.store.revision() > rev, "a change is visible");
    assert_eq!(h.store.state_json(), before, "nothing is persisted");
    assert!(!h.store.state_json().contains("systemAnimations"));

    let rev = h.store.revision();
    h.apply(Command::SystemAnimationsChanged { enabled: false });
    assert_eq!(h.store.revision(), rev, "the same value bumps nothing");
    h.apply(Command::SystemAnimationsChanged { enabled: true });
    assert!(h.store.revision() > rev);
    assert!(h.store.motion_view().system_animations);
}

#[test]
fn a_stale_close_command_bar_seq_is_ignored() {
    let mut h = Harness::new();
    let a = h.open("https://a.com/");
    h.apply(Command::OpenCommandBar { mode: CommandBarMode::NewTab, split_side: None });
    let first = h.ui().command_bar.expect("bar open").seq;

    // The page closes the bar it is showing.
    assert!(!h.apply(Command::CloseCommandBar { seq: Some(first) }).is_empty());
    assert!(h.ui().command_bar.is_none());

    // Esc, then Ctrl+T: the close for the *old* seq arrives after the new bar opened.
    h.apply(Command::OpenCommandBar { mode: CommandBarMode::NewTab, split_side: None });
    let second = h.ui().command_bar.expect("bar open").seq;
    assert_ne!(first, second);
    assert!(h.apply(Command::CloseCommandBar { seq: Some(first) }).is_empty(), "stale close does nothing");
    assert!(h.ui().command_bar.is_some(), "the new bar is still open");
    // No `seq` still closes whatever is open (the shell's Esc chain and focus rules).
    assert_eq!(h.apply(Command::CloseCommandBar { seq: None }), vec![Effect::HideCommandBar, Effect::FocusBrowser { tab: a }]);
    // A close for a bar that is not open at all is a no-op either way.
    assert!(h.apply(Command::CloseCommandBar { seq: Some(second) }).is_empty());
    assert_eq!(
        serde_json::from_value::<Command>(json!({"type": "closeCommandBar"})).unwrap(),
        Command::CloseCommandBar { seq: None },
        "seq is optional on the wire"
    );
}

#[test]
fn one_bad_animation_entry_keeps_the_rest_of_the_profile() {
    // A hand-edited profile: a junk choice, a junk group, a junk master switch.
    let bad = r#"{"version":2,"nextId":2,"settings":{"searchEngine":"bing","animations":{"enabled":"sure","groups":{"menus":false,"nope":1},"choices":{"pages.enter":false,"theme.crossFade":"no"}}},"spaces":[{"id":1,"name":"Home"}]}"#;
    let (store, report) = Store::load(Some(bad), None, T0);
    assert!(!report.state_corrupt, "{report:?}");
    let a = &store.settings().animations;
    assert!(a.enabled, "a non-boolean master switch falls back to on");
    assert!(a.follow_system);
    assert_eq!(a.groups.get("menus"), Some(&false), "the good group entry survived");
    assert_eq!(a.choices.get("pages.enter"), Some(&false), "the good choice survived");
    assert!(!a.choices.contains_key("theme.crossFade"));
    assert_eq!(store.settings().search_engine, SearchEngineId::Bing, "the other settings are untouched");

    // A profile from before the field, and one whose `animations` is not an object at all.
    for json_text in [
        r#"{"version":2,"nextId":2,"settings":{"searchEngine":"bing"},"spaces":[{"id":1,"name":"Home"}]}"#,
        r#"{"version":2,"nextId":2,"settings":{"searchEngine":"bing","animations":[1,2]},"spaces":[{"id":1,"name":"Home"}]}"#,
    ] {
        let (store, _) = Store::load(Some(json_text), None, T0);
        assert!(store.settings().animations.is_default(), "{json_text}");
        assert_eq!(store.settings().search_engine, SearchEngineId::Bing);
    }
}

#[test]
fn the_omnibox_offers_turning_animations_off_and_on() {
    let mut h = Harness::new();
    h.open("https://a.com/");
    let row = |h: &Harness| h.store.omnibox_actions().into_iter().find(|r| r.key == "action:motion.toggle").expect("action");
    let off = row(&h);
    assert_eq!(off.title, "Turn Animations Off");
    assert!(matches!(off.command, Command::UpdateSettings { .. }));
    h.apply(off.command.clone());
    assert!(!h.store.settings().animations.enabled);
    assert_eq!(h.ui().motion.level, MotionLevel::Off);

    let on = row(&h);
    assert_eq!(on.title, "Turn Animations On");
    h.apply(on.command.clone());
    assert!(h.store.settings().animations.enabled);
    assert_eq!(h.ui().motion.level, MotionLevel::Full);
}
