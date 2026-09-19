//! The extension rules of `store/extensions.rs` and the Ctrl+E picker of `store/omni.rs`
//! (ext design FINAL PLAN §4): what a row says, what Enter does, and the one rule that must never
//! bend — **Enter never turns an extension on**.

mod common;
use common::*;
use sta_core::extensions::{
    ExtensionGroup, ExtensionInfo, ExtensionInstall, ExtensionState, STATUS_NEEDS_OK, STATUS_NEEDS_OK_SHORT, STATUS_NO_ACTION,
    STATUS_NO_ACTION_HINT, STATUS_OFF,
};
use sta_core::*;

const POPUP: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const OPTIONS: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const BARE: &str = "cccccccccccccccccccccccccccccccc";
const EXTERNAL: &str = "dddddddddddddddddddddddddddddddd";
const LOCAL: &str = "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee";

fn info(id: &str, name: &str, state: ExtensionState, install: ExtensionInstall, popup: Option<&str>, options: Option<&str>) -> ExtensionInfo {
    ExtensionInfo {
        id: id.into(),
        name: name.into(),
        short_name: String::new(),
        version: "1.0".into(),
        description: String::new(),
        state,
        blocked: (state == ExtensionState::Blocked).then_some(sta_core::extensions::ExtensionBlock::Policy),
        install,
        source_label: "Chrome Web Store".into(),
        popup: popup.map(str::to_string),
        options: options.map(str::to_string),
        side_panel: None,
        needs_current_tab: false,
        commands: Vec::new(),
    }
}

/// The listing every test below starts from: one of each interesting kind.
fn listing() -> Vec<ExtensionInfo> {
    vec![
        info(POPUP, "Blocker", ExtensionState::Enabled, ExtensionInstall::WebStore, Some("popup.html"), Some("options.html")),
        info(OPTIONS, "Options Only", ExtensionState::Enabled, ExtensionInstall::WebStore, None, Some("opts/index.html")),
        info(BARE, "Toolbar Only", ExtensionState::Enabled, ExtensionInstall::Unpacked, None, None),
        info(EXTERNAL, "From A Program", ExtensionState::NeedsApproval, ExtensionInstall::ExternalStore, Some("popup.html"), None),
        info(LOCAL, "From A File", ExtensionState::NeedsApproval, ExtensionInstall::ExternalLocal, None, Some("options.html")),
    ]
}

fn with_extensions() -> Harness {
    let mut h = Harness::new();
    h.apply(Command::ExtensionsChanged { extensions: listing() });
    h
}

fn picker(h: &Harness, query: &str) -> Vec<OmniboxResult> {
    let req = OmniboxRequest {
        text: query.into(),
        mode: CommandBarMode::Extensions,
        split_side: None,
        prevent_inline_autocomplete: false,
        suggestions: Vec::new(),
        seq: 1,
    };
    h.store.omnibox(&req, h.now).results
}

fn run(id: &str, action: ExtensionAction) -> Command {
    Command::RunExtension { id: id.into(), action }
}

// ------------------------------------------------------------------------------------ the listing

#[test]
fn the_listing_is_sorted_by_name_and_deduplicated() {
    let mut h = Harness::new();
    let mut list = listing();
    list.reverse();
    list.push(info(POPUP, "Blocker (again)", ExtensionState::Off, ExtensionInstall::WebStore, None, None));
    // A bad id never reaches the UI.
    list.push(info("not-an-id", "Bogus", ExtensionState::Enabled, ExtensionInstall::WebStore, None, None));
    h.apply(Command::ExtensionsChanged { extensions: list });
    let names: Vec<&str> = h.store.extensions().iter().map(|e| e.display_name()).collect();
    assert_eq!(names, vec!["Blocker", "From A File", "From A Program", "Options Only", "Toolbar Only"]);
    assert_eq!(h.ui().extensions.items.len(), 5);
    assert_eq!(h.ui().extensions.needs_ok, 2);
}

/// The startup toast is a question, so it comes once and offers the place where the answer lives.
#[test]
fn extensions_other_programs_added_are_announced_once() {
    let mut h = with_extensions();
    let toast = h.toast().expect("the review toast");
    assert_eq!(toast.message, "2 extensions were added by other programs");
    let action = toast.action.expect("a Review button");
    assert_eq!(action.label, "Review");
    assert!(matches!(*action.command, Command::OpenUrl { ref url, .. } if url == "sta://settings/?section=extensions"));
    // A second listing (the watch, an operation) does not ask again.
    h.apply(Command::DismissToast { id: toast.id });
    let mut list = listing();
    list.pop();
    h.apply(Command::ExtensionsChanged { extensions: list });
    assert!(h.toast().is_none(), "asked twice: {:?}", h.toast());
}

// ------------------------------------------------------------------------------------ the picker

#[test]
fn an_empty_query_lists_every_group_in_order_plus_the_more_rows() {
    let h = with_extensions();
    let rows = picker(&h, "");
    let groups: Vec<ResultGroup> = rows.iter().map(|r| r.group).collect();
    assert_eq!(
        groups,
        vec![
            ResultGroup::Extensions,
            ResultGroup::Extensions,
            ResultGroup::Extensions,
            ResultGroup::NeedsOk,
            ResultGroup::NeedsOk,
            ResultGroup::More,
            ResultGroup::More,
        ]
    );
    let keys: Vec<&str> = rows.iter().map(|r| r.key.as_str()).collect();
    assert_eq!(keys[0], format!("ext:{POPUP}"), "A–Z inside the group: {keys:?}");
    assert_eq!(keys[5..], ["ext.manage", "ext.get"]);
    // The status line explains only what needs explaining — and, in the picker, exactly once: the row
    // whose Enter leaves sta says so (UXV-5), and the rows under the "Needs your OK" heading do not
    // repeat that heading (UXV-6).
    let status = |key: &str| rows.iter().find(|r| r.key == key).and_then(|r| r.subtitle.clone());
    assert_eq!(status(&format!("ext:{POPUP}")), None);
    assert_eq!(status(&format!("ext:{BARE}")).as_deref(), Some(STATUS_NO_ACTION_HINT));
    assert_eq!(status(&format!("ext:{EXTERNAL}")).as_deref(), Some(STATUS_NEEDS_OK_SHORT));
    // Settings has no such heading, so the row there still carries the whole sentence.
    assert_eq!(h.store.extension(EXTERNAL).unwrap().status(), Some(STATUS_NEEDS_OK));
    // The icon is same-origin, so the command bar's CSP can load it.
    let icon = rows.iter().find(|r| r.key == format!("ext:{POPUP}")).map(|r| r.icon.clone()).unwrap();
    assert!(matches!(icon, ResultIcon::Favicon { url: Some(ref u), .. } if u == &format!("sta://command/__ext-icon/{POPUP}/32")), "{icon:?}");
}

#[test]
fn an_extension_that_is_off_is_listed_under_off() {
    let mut h = with_extensions();
    let mut list = listing();
    list[1].state = ExtensionState::Off;
    list[2].state = ExtensionState::Blocked;
    h.apply(Command::ExtensionsChanged { extensions: list });
    let rows = picker(&h, "");
    let group = |key: &str| rows.iter().find(|r| r.key == key).map(|r| r.group);
    assert_eq!(group(&format!("ext:{OPTIONS}")), Some(ResultGroup::ExtensionsOff));
    assert_eq!(group(&format!("ext:{BARE}")), Some(ResultGroup::ExtensionsOff));
    let status = |key: &str| rows.iter().find(|r| r.key == key).and_then(|r| r.subtitle.clone());
    assert_eq!(status(&format!("ext:{OPTIONS}")).as_deref(), Some(STATUS_OFF));
}

#[test]
fn a_query_ranks_by_name_and_keeps_the_more_rows_reachable() {
    let h = with_extensions();
    let rows = picker(&h, "block");
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0].key, format!("ext:{POPUP}"));
    // The "More" rows answer their own words, and their aliases.
    assert!(picker(&h, "manage").iter().any(|r| r.key == "ext.manage"));
    assert!(picker(&h, "web store").iter().any(|r| r.key == "ext.get"));
    assert!(picker(&h, "zzzz").is_empty());
}

/// UX15: a query typed with a Hangul IME on still finds its extension — the letters are recovered
/// from the 2-set keyboard layout.
#[test]
fn a_query_typed_with_the_hangul_ime_on_still_finds_the_extension() {
    let h = with_extensions();
    // "Blocker" typed with the IME on becomes "뷸ㅐㅊdistrict"… in practice the first syllables are
    // what the user sees; `ㅠ` is the key `b`, `ㅣ` is `l`.
    assert_eq!(sta_core::omnibox::jamo_to_qwerty("ㅠㅣ").as_deref(), Some("bl"));
    let rows = picker(&h, "ㅠㅣ");
    assert_eq!(rows.first().map(|r| r.key.as_str()), Some(&*format!("ext:{POPUP}")), "{rows:?}");
    // Latin input is unaffected, and nothing to translate means no retry.
    assert_eq!(sta_core::omnibox::jamo_to_qwerty("blocker"), None);
    // A full syllable decomposes into all of its keys.
    assert_eq!(sta_core::omnibox::jamo_to_qwerty("한글").as_deref(), Some("gksrmf"));
}

/// UXV-7: the same fallback the other way round. A user who forgot to switch the IME **on** types the
/// letters of a Korean name, and the picker has to find it — the mirror image of the case above, and
/// just as likely for extensions with Korean names.
#[test]
fn a_korean_name_is_found_by_the_letters_it_is_typed_with() {
    let mut h = Harness::new();
    let mut list = listing();
    list.push(info(
        "ffffffffffffffffffffffffffffffff",
        "한글 확장",
        ExtensionState::Enabled,
        ExtensionInstall::WebStore,
        Some("popup.html"),
        None,
    ));
    h.apply(Command::ExtensionsChanged { extensions: list });
    let names = |query: &str| picker(&h, query).into_iter().map(|r| r.title).collect::<Vec<_>>();
    assert_eq!(names("한글"), vec!["한글 확장".to_string()], "the documented direction still works");
    assert_eq!(names("gksrmf"), vec!["한글 확장".to_string()], "the IME was off: {:?}", names("gksrmf"));
    // A Latin name still wins its own query (the retries rank below a direct hit).
    assert_eq!(names("blocker").first().map(String::as_str), Some("Blocker"));
}

// ------------------------------------------------------------------------------------ Enter

#[test]
fn enter_opens_the_popup_card_for_an_extension_that_has_one() {
    let mut h = with_extensions();
    let fx = h.apply(run(POPUP, ExtensionAction::Primary));
    let url = format!("chrome-extension://{POPUP}/popup.html");
    assert!(has(&fx, |e| matches!(e, Effect::OpenExtensionPopup { id, url: u, .. } if id == POPUP && *u == url)), "{fx:?}");
    let popup = h.ui().extensions.popup.expect("the card");
    assert_eq!(popup.id, POPUP);
    assert_eq!(popup.name, "Blocker");
    assert_eq!(popup.icon, format!("sta://command/__ext-icon/{POPUP}/32"));
    assert!(popup.has_options && !popup.failed);
    // Esc / blur / the × button.
    let fx = h.apply(Command::CloseExtensionPopup);
    assert!(has(&fx, |e| matches!(e, Effect::HideExtensionPopup)), "{fx:?}");
    assert!(h.ui().extensions.popup.is_none());
}

#[test]
fn a_popup_that_never_rendered_says_so_instead_of_closing() {
    let mut h = with_extensions();
    h.apply(run(POPUP, ExtensionAction::Primary));
    let fx = h.apply(Command::ExtensionPopupClosed { failed: true });
    assert!(!has(&fx, |e| matches!(e, Effect::HideExtensionPopup)), "the card stays up: {fx:?}");
    assert!(h.ui().extensions.popup.expect("the card").failed);
    // The page closing itself does close it.
    let fx = h.apply(Command::ExtensionPopupClosed { failed: false });
    assert!(has(&fx, |e| matches!(e, Effect::HideExtensionPopup)), "{fx:?}");
    assert!(h.ui().extensions.popup.is_none());
}

#[test]
fn enter_without_a_popup_opens_the_options_tab_once() {
    let mut h = with_extensions();
    h.apply(run(OPTIONS, ExtensionAction::Primary));
    let tab = h.focused().expect("the options tab");
    assert_eq!(h.tab(tab).url, format!("chrome-extension://{OPTIONS}/opts/index.html"));
    let before = h.today().len();
    // A second Enter switches to the tab that is already open instead of piling them up.
    let other = h.open("https://example.test/");
    h.apply(Command::ActivateItem { id: other });
    h.apply(run(OPTIONS, ExtensionAction::Primary));
    assert_eq!(h.focused(), Some(tab));
    assert_eq!(h.today().len(), before + 1, "only the unrelated tab was added");
}

#[test]
fn enter_on_an_extension_sta_cannot_open_says_so_and_shows_its_store_page() {
    let mut h = with_extensions();
    // The row said it before the press ("… · ↵ opens its Web Store page"), so the press itself opens
    // the page and says nothing more: a toast repeating the row's own subtitle told the user nothing
    // (UXV-5). Asking for the popup of an extension that has none still explains itself, because
    // nothing there announced it.
    let announcement = h.toast().expect("the startup announcement").id;
    h.apply(Command::DismissToast { id: announcement });
    h.apply(run(BARE, ExtensionAction::Primary));
    // The only toast here is sta answering the Web Store's "Switch to Chrome" banner, once per run
    // (P9) — not the row's own subtitle read back at the user.
    let store_note = h.toast().expect("the Web Store note").clone();
    assert!(store_note.message.contains("Switch to Chrome"), "{}", store_note.message);
    h.apply(Command::DismissToast { id: store_note.id });
    assert!(h.toast().is_none(), "{:?}", h.toast());
    let tab = h.focused().expect("the store page");
    assert_eq!(h.tab(tab).url, format!("https://chromewebstore.google.com/detail/{BARE}"));
    h.apply(run(BARE, ExtensionAction::Popup));
    assert_eq!(h.toast().expect("a toast").message, STATUS_NO_ACTION);
}

/// The rule of SEC-7: the picker is a way in, never a way to grant anything.
#[test]
fn enter_never_turns_an_extension_on() {
    for id in [EXTERNAL, LOCAL] {
        let mut h = with_extensions();
        let fx = h.apply(run(id, ExtensionAction::Primary));
        assert!(!has(&fx, |e| matches!(e, Effect::ExtensionOp { .. })), "{id} ran an operation: {fx:?}");
        assert!(!has(&fx, |e| matches!(e, Effect::OpenExtensionPopup { .. })), "{id} opened a card: {fx:?}");
        // It opens Settings › Extensions at that row instead.
        let tab = h.focused().expect("settings");
        assert_eq!(h.tab(tab).url, format!("sta://settings/?section=extensions&ext={id}"));
        assert_eq!(h.store.extension(id).unwrap().state, ExtensionState::NeedsApproval);
        // Alt+Enter (options) and the popup action are just as harmless.
        h.apply(run(id, ExtensionAction::Options));
        h.apply(run(id, ExtensionAction::Popup));
        assert_eq!(h.store.extension(id).unwrap().state, ExtensionState::NeedsApproval);
        assert!(h.ui().extensions.popup.is_none());
    }
}

#[test]
fn alt_enter_opens_the_options_page_of_an_extension_that_has_one() {
    let mut h = with_extensions();
    let rows = picker(&h, "");
    let row = rows.iter().find(|r| r.key == format!("ext:{POPUP}")).unwrap();
    assert_eq!(row.alt_command, Some(run(POPUP, ExtensionAction::Options)));
    // …and there is nothing to open for one without options.
    let bare = rows.iter().find(|r| r.key == format!("ext:{BARE}")).unwrap();
    assert_eq!(bare.alt_command, None);
    h.apply(run(POPUP, ExtensionAction::Options));
    assert_eq!(h.tab(h.focused().unwrap()).url, format!("chrome-extension://{POPUP}/options.html"));
}

/// P3-E2E-4: an options page is free to route itself the moment it loads
/// (`location.replace(pathname + '#general')` is what most of them do). "One options tab per
/// extension" has to survive that, or every Enter piles up another copy.
#[test]
fn an_options_page_that_routes_itself_still_opens_only_one_tab() {
    let mut h = with_extensions();
    h.apply(run(OPTIONS, ExtensionAction::Primary));
    let tab = h.focused().expect("the options tab");
    let routed = format!("chrome-extension://{OPTIONS}/opts/index.html#general");
    h.commit(tab, &routed, "Options");
    let before = h.today().len();
    // Somewhere else first, so re-activating the existing tab is observable.
    let other = h.open("https://example.test/");
    h.apply(Command::ActivateItem { id: other });
    h.apply(run(OPTIONS, ExtensionAction::Primary));
    assert_eq!(h.focused(), Some(tab), "a second Enter opened another tab");
    h.apply(Command::ActivateItem { id: other });
    h.apply(run(OPTIONS, ExtensionAction::Options));
    assert_eq!(h.focused(), Some(tab), "Alt+Enter opened another tab");
    assert_eq!(h.today().len(), before + 1, "only the unrelated tab was added: {:?}", h.today());
    // A page that redirects to a different page of the same extension is still that extension's tab.
    h.apply(Command::ActivateItem { id: other });
    h.commit(tab, &format!("chrome-extension://{OPTIONS}/opts/general.html"), "Options");
    h.apply(run(OPTIONS, ExtensionAction::Primary));
    assert_eq!(h.focused(), Some(tab));
    assert_eq!(h.today().len(), before + 1, "{:?}", h.today());
}

// ------------------------------------------------------------------------------------ operations

#[test]
fn turning_on_an_external_extension_needs_its_details_first() {
    let mut h = with_extensions();
    // The first attempt loads the disclosure instead of enabling anything.
    let fx = h.apply(Command::SetExtensionEnabled { id: EXTERNAL.into(), enabled: true });
    assert!(
        has(&fx, |e| matches!(e, Effect::ExtensionOp { id, op: ExtensionOp::GetInfo } if id == EXTERNAL)),
        "the warnings are loaded: {fx:?}"
    );
    assert!(!has(&fx, |e| matches!(e, Effect::ExtensionOp { op: ExtensionOp::SetEnabled { .. }, .. })), "{fx:?}");
    assert_eq!(h.ui().extensions.busy.as_deref(), Some(EXTERNAL));
    // With the details in, the confirm goes through.
    let details = ExtensionDetails {
        id: EXTERNAL.into(),
        warnings: vec!["Read and change all your data on all websites".into()],
        host_access: "On all sites".into(),
        source: "Added by another program, not from the Chrome Web Store".into(),
    };
    h.apply(Command::ExtensionDetailsLoaded { details: details.clone() });
    assert_eq!(h.ui().extensions.busy, None, "the row is usable again");
    assert_eq!(h.store.extension_details(EXTERNAL), Some(&details));
    let fx = h.apply(Command::SetExtensionEnabled { id: EXTERNAL.into(), enabled: true });
    assert!(
        has(&fx, |e| matches!(e, Effect::ExtensionOp { id, op: ExtensionOp::SetEnabled { enabled: true } } if id == EXTERNAL)),
        "{fx:?}"
    );
}

/// SEC-P3-5: the disclosure is the user's answer to **one** Turn on. It is consumed by the press it
/// belongs to, and a panel left open and forgotten stops counting — so a Turn on the user cancelled
/// can never pre-authorise a later one.
#[test]
fn a_disclosure_authorises_exactly_one_turn_on() {
    let mut h = with_extensions();
    let details = ExtensionDetails {
        id: EXTERNAL.into(),
        warnings: vec!["Read your browsing history".into()],
        host_access: "On all sites".into(),
        source: "Added by another program, not from the Chrome Web Store".into(),
    };
    h.apply(Command::ExtensionDetailsLoaded { details: details.clone() });
    let fx = h.apply(Command::SetExtensionEnabled { id: EXTERNAL.into(), enabled: true });
    assert!(has(&fx, |e| matches!(e, Effect::ExtensionOp { op: ExtensionOp::SetEnabled { enabled: true }, .. })), "{fx:?}");
    // The operation ends and the extension is still waiting for an answer (Chromium refused it, say).
    // The next Turn on must load and show the warnings again.
    h.apply(Command::ExtensionOpFailed { id: EXTERNAL.into(), op: ExtensionOp::SetEnabled { enabled: true }, message: String::new() });
    let fx = h.apply(Command::SetExtensionEnabled { id: EXTERNAL.into(), enabled: true });
    assert!(has(&fx, |e| matches!(e, Effect::ExtensionOp { op: ExtensionOp::GetInfo, .. })), "the disclosure was reused: {fx:?}");
    // Loading them again authorises the next press — but only for a while.
    h.apply(Command::ExtensionDetailsLoaded { details });
    h.advance(61_000);
    let fx = h.apply(Command::SetExtensionEnabled { id: EXTERNAL.into(), enabled: true });
    assert!(has(&fx, |e| matches!(e, Effect::ExtensionOp { op: ExtensionOp::GetInfo, .. })), "a stale disclosure was used: {fx:?}");
}

#[test]
fn a_local_crx_another_program_added_can_only_be_removed() {
    let mut h = with_extensions();
    let fx = h.apply(Command::SetExtensionEnabled { id: LOCAL.into(), enabled: true });
    assert!(!has(&fx, |e| matches!(e, Effect::ExtensionOp { .. })), "{fx:?}");
    assert_eq!(h.toast().expect("a toast").message, "sta can't turn this on: another program installed it from a file");
    // Remove is offered, and runs.
    let fx = h.apply(Command::RemoveExtension { id: LOCAL.into() });
    assert!(has(&fx, |e| matches!(e, Effect::ExtensionOp { id, op: ExtensionOp::Uninstall } if id == LOCAL)), "{fx:?}");
}

#[test]
fn a_managed_extension_is_left_alone() {
    let mut h = Harness::new();
    let mut list = listing();
    list[0].install = ExtensionInstall::Managed;
    list[0].state = ExtensionState::Blocked;
    h.apply(Command::ExtensionsChanged { extensions: list });
    let fx = h.apply(Command::SetExtensionEnabled { id: POPUP.into(), enabled: true });
    assert!(!has(&fx, |e| matches!(e, Effect::ExtensionOp { .. })), "{fx:?}");
    assert_eq!(h.toast().expect("a toast").message, "Your organization manages this extension");
    let fx = h.apply(Command::RemoveExtension { id: POPUP.into() });
    assert!(!has(&fx, |e| matches!(e, Effect::ExtensionOp { .. })), "{fx:?}");
}

#[test]
fn a_failed_operation_says_what_it_was_and_frees_the_row() {
    let mut h = with_extensions();
    h.apply(Command::RequestExtensionDetails { id: EXTERNAL.into() });
    assert_eq!(h.ui().extensions.busy.as_deref(), Some(EXTERNAL));
    h.apply(Command::ExtensionOpFailed { id: EXTERNAL.into(), op: ExtensionOp::SetEnabled { enabled: true }, message: "timed out".into() });
    assert_eq!(h.toast().expect("a toast").message, "Couldn't turn on From A Program (timed out)");
    assert_eq!(h.ui().extensions.busy, None);
}

#[test]
fn removing_an_extension_takes_its_card_and_its_details_with_it() {
    let mut h = with_extensions();
    h.apply(Command::RunExtension { id: POPUP.into(), action: ExtensionAction::Primary });
    assert!(h.ui().extensions.popup.is_some());
    let fx = h.apply(Command::RemoveExtension { id: POPUP.into() });
    assert!(has(&fx, |e| matches!(e, Effect::HideExtensionPopup)), "{fx:?}");
    // The shell answers with the new listing; the row is gone and so is anything about it.
    let remaining: Vec<ExtensionInfo> = listing().into_iter().filter(|e| e.id != POPUP).collect();
    h.apply(Command::ExtensionsChanged { extensions: remaining });
    assert!(h.store.extension(POPUP).is_none());
    assert!(h.ui().extensions.popup.is_none());
}

/// An extension that is turned off while its popup is open cannot keep showing it.
#[test]
fn turning_an_extension_off_closes_its_card() {
    let mut h = with_extensions();
    h.apply(Command::RunExtension { id: POPUP.into(), action: ExtensionAction::Primary });
    let mut list = listing();
    list[0].state = ExtensionState::Off;
    let fx = h.apply(Command::ExtensionsChanged { extensions: list });
    assert!(has(&fx, |e| matches!(e, Effect::HideExtensionPopup)), "{fx:?}");
}

// ------------------------------------------------------------------------------------ the card's life

#[test]
fn the_card_belongs_to_its_pane_and_never_sits_over_a_prompt() {
    let mut h = with_extensions();
    let first = h.open("https://one.test/");
    let second = h.open("https://two.test/");
    h.apply(Command::ActivateItem { id: first });
    h.apply(Command::RunExtension { id: POPUP.into(), action: ExtensionAction::Primary });
    assert_eq!(h.ui().extensions.popup.expect("card").tab, Some(first));
    // Switching tabs takes the card away.
    let fx = h.apply(Command::ActivateItem { id: second });
    assert!(has(&fx, |e| matches!(e, Effect::HideExtensionPopup)), "{fx:?}");
    assert!(h.ui().extensions.popup.is_none());

    // A permission prompt on the visible pane closes it too (SEC-4).
    h.apply(Command::RunExtension { id: POPUP.into(), action: ExtensionAction::Primary });
    assert!(h.ui().extensions.popup.is_some());
    let fx = h.apply(Command::PermissionRequested {
        id: 1,
        tab: second,
        origin: "https://two.test".into(),
        kinds: vec![PermissionKind::Camera],
    });
    assert!(has(&fx, |e| matches!(e, Effect::HideExtensionPopup)), "{fx:?}");
    assert!(has(&fx, |e| matches!(e, Effect::ShowPermissionPrompt { .. })), "{fx:?}");
}

// ------------------------------------------------------------------------------------ safe mode

#[test]
fn safe_mode_restores_every_tab_unloaded_and_says_so() {
    let mut h = Harness::new();
    let tab = h.open("https://one.test/");
    h.apply(Command::ActivateItem { id: tab });
    let json = h.store.state_json();
    // A new run of the same profile, started in safe mode by the crash-loop guard.
    let (mut store, _) = Store::load(Some(&json), None, T0);
    store.apply(Command::SystemThemeChanged { dark: false }, T0);
    store.apply(Command::SafeModeStarted, T0);
    let mut h2 = Harness { store, ..Harness::new() };
    let fx = h2.store.startup(Vec::new(), T0);
    assert!(h2.store.safe_mode());
    assert!(!has(&fx, |e| matches!(e, Effect::CreateBrowser { .. })), "no tab is loaded: {fx:?}");
    assert!(matches!(shows(&fx), Some(ContentLayout::Empty)), "{fx:?}");
    assert!(h2.store.ui_state().extensions.safe_mode);
    // The tab itself is still there, just not loaded.
    assert!(h2.store.state().items.contains_key(&tab));
}

// ------------------------------------------------------------------------------------ the `>` list

#[test]
fn the_actions_list_offers_the_picker_and_the_settings_page() {
    let h = with_extensions();
    let actions = h.store.omnibox_actions();
    let find = |title: &str| actions.iter().find(|a| a.title == title).cloned();
    let show = find("Show Extensions").expect("Show Extensions");
    assert_eq!(show.hint.as_deref(), Some("Ctrl+E"));
    assert_eq!(show.command, Command::OpenCommandBar { mode: CommandBarMode::Extensions, split_side: None });
    let manage = find("Manage Extensions").expect("Manage Extensions");
    assert!(matches!(manage.command, Command::OpenUrl { ref url, .. } if url == "sta://settings/?section=extensions"));
    // `>` inside the picker still switches to the actions list.
    let mut req = OmniboxRequest {
        text: ">dev".into(),
        mode: CommandBarMode::Extensions,
        split_side: None,
        prevent_inline_autocomplete: false,
        suggestions: Vec::new(),
        seq: 1,
    };
    let rows = h.store.omnibox(&req, h.now).results;
    assert!(rows.iter().all(|r| r.group == ResultGroup::Actions), "{rows:?}");
    req.text = ">extensions".into();
    assert!(h.store.omnibox(&req, h.now).results.iter().any(|r| r.title == "Show Extensions"));
}

#[test]
fn opening_the_picker_asks_the_shell_for_a_fresh_listing() {
    let mut h = with_extensions();
    let fx = h.apply(Command::OpenCommandBar { mode: CommandBarMode::Extensions, split_side: None });
    assert!(has(&fx, |e| matches!(e, Effect::RefreshExtensions)), "{fx:?}");
    assert_eq!(h.ui().command_bar.expect("the bar").mode, CommandBarMode::Extensions);
    // Other modes don't (the listing is only interesting where it is shown).
    let fx = h.apply(Command::OpenCommandBar { mode: CommandBarMode::NewTab, split_side: None });
    assert!(!has(&fx, |e| matches!(e, Effect::RefreshExtensions)), "{fx:?}");
}

// ------------------------------------------------------------------------------------ the UI surface

#[test]
fn shell_only_extension_commands_are_refused_from_the_ui() {
    for cmd in [
        Command::ExtensionsChanged { extensions: Vec::new() },
        Command::ExtensionDetailsLoaded { details: ExtensionDetails { id: POPUP.into(), warnings: Vec::new(), host_access: String::new(), source: String::new() } },
        Command::ExtensionOpFailed { id: POPUP.into(), op: ExtensionOp::GetInfo, message: String::new() },
        Command::ExtensionPopupClosed { failed: false },
        Command::SafeModeStarted,
    ] {
        assert!(!cmd.allowed_from_ui(), "{cmd:?}");
    }
    // What the UI does send is allowed.
    for cmd in [
        run(POPUP, ExtensionAction::Primary),
        Command::RequestExtensionDetails { id: POPUP.into() },
        Command::SetExtensionEnabled { id: POPUP.into(), enabled: false },
        Command::RemoveExtension { id: POPUP.into() },
        Command::CloseExtensionPopup,
    ] {
        assert!(cmd.allowed_from_ui(), "{cmd:?}");
    }
}

#[test]
fn groups_match_the_state() {
    let list = listing();
    assert_eq!(ExtensionGroup::of(&list[0]), ExtensionGroup::Extensions);
    assert_eq!(ExtensionGroup::of(&list[3]), ExtensionGroup::NeedsOk);
    assert_eq!(ExtensionGroup::Extensions.title(), "Extensions");
    assert_eq!(ExtensionGroup::NeedsOk.title(), "Needs your OK");
    assert_eq!(ExtensionGroup::Off.title(), "Off");
    assert_eq!(ExtensionGroup::More.title(), "More");
}

/// A write ends when the profile has been re-read: that fresh listing is its answer. Without this the
/// row stayed busy forever and the *next* operation was silently dropped (found by the e2e's
/// off-then-on-again check).
#[test]
fn a_fresh_listing_ends_a_pending_write_but_not_a_pending_read() {
    let mut h = with_extensions();
    // A write: the listing that follows frees the row.
    h.apply(Command::SetExtensionEnabled { id: POPUP.into(), enabled: false });
    assert_eq!(h.ui().extensions.busy.as_deref(), Some(POPUP));
    let mut off = listing();
    off[0].state = ExtensionState::Off;
    h.apply(Command::ExtensionsChanged { extensions: off });
    assert_eq!(h.ui().extensions.busy, None, "the write is over");
    // …so the opposite operation can run at once.
    let fx = h.apply(Command::SetExtensionEnabled { id: POPUP.into(), enabled: true });
    assert!(
        has(&fx, |e| matches!(e, Effect::ExtensionOp { id, op: ExtensionOp::SetEnabled { enabled: true } } if id == POPUP)),
        "{fx:?}"
    );
    // A read is answered by its details, not by a listing (the watch fires all the time).
    let mut h = with_extensions();
    h.apply(Command::RequestExtensionDetails { id: EXTERNAL.into() });
    h.apply(Command::ExtensionsChanged { extensions: listing().into_iter().rev().collect() });
    assert_eq!(h.ui().extensions.busy.as_deref(), Some(EXTERNAL), "a read waits for its answer");
}

// ------------------------------------------------------------- what the card refuses and warns about

/// SEC-4 says the popup card never sits over a permission prompt, and `validate_runtime` enforces
/// that by closing one that is up. Opening it anyway meant the picker's primary action created a
/// browser and killed it 7 ms later: no card, no toast, nothing at all from the user's side. It is
/// refused up front now, with the reason (P7).
#[test]
fn a_popup_is_refused_while_a_permission_prompt_is_up() {
    let mut h = with_extensions();
    let tab = h.open("https://site.example/");
    h.apply(Command::PermissionRequested { id: 1, tab, origin: "https://site.example/".into(), kinds: vec![PermissionKind::Geolocation] });
    assert!(!h.ui().permission_prompts.is_empty(), "the prompt is up");
    let fx = h.apply(run(POPUP, ExtensionAction::Primary));
    assert!(h.ui().extensions.popup.is_none(), "no card was opened");
    assert!(!has(&fx, |e| matches!(e, Effect::OpenExtensionPopup { .. })), "and no browser was created: {fx:?}");
    assert_eq!(h.toast().expect("a toast").message, "Answer the site's permission request first");
    // Answered: the same press works.
    h.apply(Command::ResolvePermission { id: 1, allow: false, remember: false });
    h.apply(Command::PermissionDismissed { id: 1 });
    assert!(h.ui().permission_prompts.is_empty(), "the prompt is answered and dismissed");
    h.apply(run(POPUP, ExtensionAction::Primary));
    assert!(h.ui().extensions.popup.is_some(), "the card opens once the prompt is gone");
}

/// D1a: prebuilt CEF cannot give an action popup the current tab, so a popup of an extension that
/// asks for `tabs`/`activeTab` may render nothing but the extension's *own* error page — which is
/// what AdBlock's card showed, with not a word from sta on it. The card carries the warning itself
/// now, whatever the page paints (P8).
#[test]
fn the_card_says_when_a_popup_needs_the_current_tab() {
    let mut h = Harness::new();
    let mut list = listing();
    list[0].needs_current_tab = true;
    h.apply(Command::ExtensionsChanged { extensions: list });
    h.apply(run(POPUP, ExtensionAction::Primary));
    assert!(h.ui().extensions.popup.expect("the card").needs_current_tab);
    h.apply(Command::CloseExtensionPopup);
    // An extension that asks for nothing of the kind gets no warning.
    h.apply(run(EXTERNAL, ExtensionAction::Primary));
    assert!(h.ui().extensions.popup.is_none(), "that one is not even on");
}

/// D11a: the "N extensions were added by other programs" toast used to be once per *run*, with the
/// flag in the runtime half — so a machine whose three registry extensions the user will never
/// allow was interrupted on every single launch, forever. The set they have been told about is
/// persisted; only a *new* id is news (P4).
#[test]
fn the_external_announcement_is_once_per_set_not_once_per_run() {
    let mut h = with_extensions();
    let first = h.toast().expect("the announcement");
    assert_eq!(first.message, "2 extensions were added by other programs");
    let saved = h.store.state().announced_external.clone();
    assert_eq!(saved.len(), 2, "both waiting ids are remembered: {saved:?}");
    h.apply(Command::DismissToast { id: first.id });
    // What the next launch loads.
    let json = serde_json::to_string(h.store.state()).expect("state json");
    assert!(json.contains("announcedExternal"), "the set is saved");
    let restart = || Harness::start(Store::load(Some(&json), None, 0).0, Vec::new());

    // A second run of the same profile with the same listing says nothing.
    let mut again = restart();
    again.apply(Command::ExtensionsChanged { extensions: listing() });
    assert!(again.toast().is_none(), "{:?}", again.toast());

    // A fourth extension another program added is news again.
    let mut grown = listing();
    grown.push(info("ffffffffffffffffffffffffffffffff", "New One", ExtensionState::NeedsApproval, ExtensionInstall::ExternalStore, None, None));
    let mut third = restart();
    third.apply(Command::ExtensionsChanged { extensions: grown });
    assert_eq!(third.toast().expect("a toast").message, "1 extension was added by another program");
}
