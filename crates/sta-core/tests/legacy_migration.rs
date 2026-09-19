//! Profiles and input from before the rename from Astatine to sta (`sta_core::legacy`).
//!
//! rename:keep-file — every "astatine" here is a legacy name on purpose.

mod common;

use common::*;
use serde_json::json;
use sta_core::omnibox::{Classified, classify, resolve_input};
use sta_core::*;

/// A profile with internal pages everywhere they can be persisted: a pinned page, a favorite, a
/// Today tab, an archived tab (also on the reopen stack) and a web tab.
fn profile_with_internal_pages() -> Harness {
    let mut h = Harness::new();
    h.open_pinned("sta://settings/");
    h.open_favorite("sta://history/");
    h.open("https://example.com/");
    h.open("sta://boosts/?id=5");
    let archived = h.open("sta://archive/");
    h.apply(Command::CloseItem { id: Some(archived) });
    h
}

#[test]
fn legacy_state_json_loads_with_sta_urls_and_is_saved_again() {
    let h = profile_with_internal_pages();
    let current = h.store.state_json();
    assert!(current.matches("sta://").count() >= 5, "{current}");
    assert!(!current.contains("astatine"));
    assert!(h.store.state().archive.iter().any(|e| e.url == "sta://archive/"), "archived internal page");
    assert!(!h.store.state().reopen.is_empty(), "the close is on the reopen stack");

    // The same profile as Astatine saved it (mixed case, as a hand-edited file could have it).
    let legacy = current.replace("sta://settings/", "ASTATINE://settings/").replace("sta://", "astatine://");
    assert!(!legacy.contains("sta://"));

    let (mut expected, _) = Store::load(Some(&current), None, h.now);
    let (mut upgraded, report) = Store::load(Some(&legacy), None, h.now);
    assert!(!report.state_corrupt, "{report:?}");
    assert!(report.warnings.iter().any(|w| w.contains("internal URL(s) from before the rename")), "{report:?}");
    upgraded.check_invariants().unwrap_or_else(|e| panic!("{e:?}"));
    assert_eq!(upgraded.state_json(), expected.state_json(), "identical to the profile saved by sta");
    assert!(upgraded.take_dirty().state, "the upgraded state is saved");
    assert!(!expected.take_dirty().state);

    // Restoring the session opens the sta:// pages as internal browsers.
    let restored = Harness::start(upgraded, Vec::new());
    let pinned = restored.pinned();
    assert_eq!(restored.tab(pinned[0]).url, "sta://settings/");
    let favorites = restored.favorites();
    assert_eq!(restored.tab(favorites[0]).url, "sta://history/");
    assert!(restored.today().iter().any(|t| restored.tab(*t).url == "sta://boosts/?id=5"));
    assert!(!restored.store.state_json().contains("astatine"));
}

#[test]
fn legacy_history_json_is_upgraded() {
    let history = json!({
        "urls": [
            { "url": "astatine://settings/", "title": "Settings", "visitCount": 1, "typedCount": 1, "lastVisitAt": T0, "visits": [{ "at": T0, "transition": "typed" }] },
            { "url": "https://example.com/", "title": "Example about astatine", "visitCount": 1, "typedCount": 0, "lastVisitAt": T0, "visits": [{ "at": T0, "transition": "link" }] }
        ]
    });
    let (mut store, report) = Store::load(None, Some(&history.to_string()), T0);
    assert!(!report.history_corrupt, "{report:?}");
    assert!(store.history().get("sta://settings/").is_some());
    assert!(store.history().get("astatine://settings/").is_none());
    assert_eq!(store.history().get("https://example.com/").map(|u| u.title.as_str()), Some("Example about astatine"), "only URLs change");
    assert!(!store.history_json().contains("astatine:"));
    assert!(store.take_dirty().history, "the upgraded history is saved");

    let (mut clean, report) = Store::load(None, Some(&store.history_json()), T0);
    assert!(report.warnings.is_empty(), "{report:?}");
    assert!(!clean.take_dirty().history);
}

#[test]
fn typed_legacy_internal_urls_open_sta_pages() {
    assert_eq!(classify("astatine://settings"), Classified::Url("sta://settings".into()));
    assert_eq!(classify("  ASTATINE://boosts/?id=5  "), Classified::Url("sta://boosts/?id=5".into()));
    assert_eq!(classify("view-source:astatine://history/"), Classified::Url("view-source:sta://history/".into()));
    assert_eq!(classify("?astatine://settings"), Classified::Search("astatine://settings".into()));
    assert_eq!(classify("astatine settings"), Classified::Search("astatine settings".into()));
    // Command-line arguments and relaunch URLs go through resolve_input.
    assert_eq!(resolve_input("astatine://history/", SearchEngineId::Google, ""), "sta://history/");

    // The command bar commit opens the page as an internal browser.
    let mut h = Harness::new();
    let fx = h.apply(Command::OpenInput { text: "astatine://settings/".into(), target: OpenTarget::NewTab });
    let tab = h.focused().expect("opened");
    assert_eq!(h.tab(tab).url, "sta://settings/");
    assert!(has(&fx, |e| matches!(e, Effect::CreateBrowser { tab: t, url, internal: true, .. } if *t == tab && url == "sta://settings/")), "{fx:?}");
}
