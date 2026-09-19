//! Chrome-created browsers (shell `foreign.rs`) and DevTools on sta pages:
//! `ForeignTabRequested` (web pages, declared and undeclared extension pages, refusals),
//! `ExtensionInstalled` (toasts, the 60 s grace for a fresh extension's pages),
//! `ForeignBlocked`, and `ToggleDevTools` on `sta://` pages.

mod common;
use common::*;
use sta_core::*;

const EXT: &str = "abcdefghijklmnopabcdefghijklmnop";
const OTHER: &str = "ponmlkjihgfedcbaponmlkjihgfedcba";

fn extension(id: &str) -> ForeignExtension {
    ForeignExtension {
        id: id.into(),
        name: "Blocker".into(),
        pages: vec!["options.html".into(), "popup/index.html".into()],
        web_accessible: vec!["welcome/*.html".into()],
        recently_installed: false,
    }
}

fn foreign(url: &str, extension: Option<ForeignExtension>) -> Command {
    Command::ForeignTabRequested { url: url.into(), extension }
}

#[test]
fn web_pages_open_as_a_foreground_tab_after_the_active_one() {
    let mut h = Harness::new();
    let first = h.open("https://store.example/item");
    let second = h.open("https://other.example/");
    h.apply(Command::ActivateItem { id: first });
    let fx = h.apply(foreign("https://getadblock.com/installed/?u=1", None));
    let tab = h.focused().expect("the adopted tab is focused");
    assert!(tab != first && tab != second);
    assert_eq!(h.tab(tab).url, "https://getadblock.com/installed/?u=1");
    assert!(has(&fx, |e| is_create(e, tab)), "the tab is created: {fx:?}");
    // Directly below the active item, like a link opened from it.
    let today = h.today();
    assert_eq!(today.iter().position(|t| *t == tab), today.iter().position(|t| *t == first).map(|i| i + 1));
    assert!(h.toast().is_none(), "no toast for a plain page: {:?}", h.toast());
}

#[test]
fn extension_pages_follow_the_manifest_and_a_fresh_install() {
    let mut h = Harness::new();
    // A page the extension declares: opened.
    h.apply(foreign(&format!("chrome-extension://{EXT}/options.html"), Some(extension(EXT))));
    let tab = h.focused().expect("options tab");
    assert_eq!(h.tab(tab).url, format!("chrome-extension://{EXT}/options.html"));
    // Web-accessible to every site: opened.
    h.apply(foreign(&format!("chrome-extension://{EXT}/welcome/first.html"), Some(extension(EXT))));
    assert_eq!(h.tab(h.focused().unwrap()).url, format!("chrome-extension://{EXT}/welcome/first.html"));
    let before = h.today().len();

    // Anything else asks first.
    h.apply(foreign(&format!("chrome-extension://{EXT}/app/app.html"), Some(extension(EXT))));
    assert_eq!(h.today().len(), before, "nothing opened");
    let toast = h.toast().expect("toast");
    assert_eq!(toast.message, "An extension wants to open a page of Blocker");
    let action = toast.action.expect("Open action");
    assert_eq!(action.label, "Open");
    assert!(action.command.allowed_from_ui(), "the toast's action comes from the UI");
    h.apply(*action.command);
    assert_eq!(h.tab(h.focused().unwrap()).url, format!("chrome-extension://{EXT}/app/app.html"), "Open opens it");

    // An extension installed less than a minute ago may open any of its pages (welcome flows).
    let mut fresh = Harness::new();
    fresh.apply(Command::ExtensionInstalled { id: EXT.into(), name: "Blocker".into(), external: false });
    fresh.apply(foreign(&format!("chrome-extension://{EXT}/app/welcome.html"), None));
    assert_eq!(fresh.tab(fresh.focused().unwrap()).url, format!("chrome-extension://{EXT}/app/welcome.html"));
    // …but not for 61 s, and not for another extension.
    fresh.advance(61_000);
    fresh.apply(Command::Tick);
    let count = fresh.today().len();
    fresh.apply(foreign(&format!("chrome-extension://{EXT}/app/later.html"), None));
    assert_eq!(fresh.today().len(), count, "the grace period is over");
    fresh.apply(foreign(&format!("chrome-extension://{OTHER}/options.html"), Some(extension(EXT))));
    assert_eq!(fresh.today().len(), count, "another extension's data never vouches for a page");
}

#[test]
fn refused_urls_open_nothing() {
    let mut h = Harness::new();
    let before = h.today();
    for url in [
        "javascript:alert(1)",
        "sta://settings/",
        "chrome://settings",
        "chrome://extensions/?options=abcdefghijklmnopabcdefghijklmnop",
        "file:///C:/Windows/win.ini",
        "data:text/html,x",
        "about:blank",
        &format!("chrome-extension://{EXT}/../{OTHER}/options.html"),
        &format!("chrome-extension://{EXT}/%2e%2e/options.html"),
        "chrome-extension://short/options.html",
        "",
    ] {
        h.apply(foreign(url, Some(ForeignExtension { recently_installed: true, ..extension(EXT) })));
        assert_eq!(h.today(), before, "{url} opened a tab");
        assert!(h.toast().is_none(), "{url} toasted: {:?}", h.toast());
    }
}

/// FINAL PLAN §2: *"more than 3 adoptions in 10 s"*. The budget is spent on the **verdict**, so the
/// three checks below are one rule seen from three sides.
#[test]
fn three_adoptions_per_ten_seconds_then_one_toast() {
    let mut h = Harness::new();
    for i in 0..3 {
        h.apply(foreign(&format!("https://a.example/{i}"), None));
    }
    let today = h.today().len();
    assert_eq!(today, 3, "three adoptions open: {:?}", h.today());
    assert!(h.toast().is_none(), "{:?}", h.toast());

    h.apply(foreign("https://a.example/4", None));
    assert_eq!(h.today().len(), today, "the fourth is blocked");
    assert_eq!(h.toast().map(|t| t.message), Some("An extension keeps opening windows; sta blocked them".into()));
    // A window that keeps trying can't keep replacing that toast (one per 10 s).
    let first = h.toast().unwrap().id;
    h.advance(2000);
    h.apply(foreign("https://a.example/5", None));
    assert_eq!(h.toast().map(|t| t.id), Some(first), "a second toast: {:?}", h.toast());
    // Blocked requests don't extend the window either: 10 s after the third adoption it is free.
    h.advance(8000);
    h.apply(foreign("https://a.example/6", None));
    assert_eq!(h.today().len(), today + 1, "the window passed");
}

/// `take_slot`'s own invariant: *"an over-budget use is **not** recorded: a flood must not keep the
/// window alive"*. A window that keeps trying for the whole 10 s must still find the budget free
/// once the adoptions that filled it have aged out — otherwise the budget is wedged for as long as
/// the flood lasts, which is the failure phase 1 set out to fix.
#[test]
fn a_flood_does_not_keep_the_adoption_window_alive() {
    let mut h = Harness::new();
    for i in 0..3 {
        h.apply(foreign(&format!("https://a.example/{i}"), None));
    }
    assert_eq!(h.today().len(), 3);
    // Nine more attempts, one per second, each of them over budget.
    let mut blocked = Vec::new();
    for s in 1..=9 {
        h.advance(1000);
        let before = h.today().len();
        h.apply(foreign(&format!("https://a.example/flood{s}"), None));
        if h.today().len() == before {
            blocked.push(s);
        }
    }
    assert_eq!(blocked, (1..=9).collect::<Vec<_>>(), "every attempt inside the window is blocked");
    // 10.001 s after the three adoptions: their slots are gone and nothing the flood did replaced
    // them.
    h.advance(1001);
    let before = h.today().len();
    h.apply(foreign("https://a.example/after", None));
    assert_eq!(h.today().len(), before + 1, "the flood kept the window alive: {blocked:?}");
}

/// The verdict is about the page, not about who asked (`urls::foreign_tab_verdict`, ARCHITECTURE
/// §4.5 "Who asked"): an installed extension can have sta open another extension's declared page.
/// Accepted and recorded — with the wording that does *not* claim the named extension asked.
#[test]
fn the_question_names_the_pages_owner_not_the_asker() {
    let mut h = Harness::new();
    // The shell answers with the **target** extension's own files, whoever caused the window.
    h.apply(foreign(&format!("chrome-extension://{OTHER}/options.html"), Some(extension(OTHER))));
    let tab = h.focused().expect("the declared page of the other extension opens");
    assert_eq!(h.tab(tab).url, format!("chrome-extension://{OTHER}/options.html"));
    // An undeclared page of that extension asks, and the question names it as the page's owner.
    h.apply(foreign(&format!("chrome-extension://{OTHER}/app/secret.html"), Some(extension(OTHER))));
    let message = h.toast().expect("toast").message;
    assert_eq!(message, "An extension wants to open a page of Blocker");
    assert!(!message.starts_with("Blocker"), "the toast must not say the named extension asked: {message}");
}

/// Every phase-1 toast stays on the toast's one line at the name budget core allows
/// (`ui/toast/toast.css`: 448 px inner width, `nowrap`), the question with less room because it is
/// the only one with an action button.
#[test]
fn toast_messages_fit_one_line() {
    let long = "Privacy Guard Pro for Chrome — Ads, Trackers & Cookies";
    let mut h = Harness::new();
    let mut seen: Vec<(String, usize)> = Vec::new();
    let mut record = |h: &mut Harness, action: bool| {
        let toast = h.toast().expect("toast");
        let n = toast.message.chars().count();
        let max = if action { 56 } else { 70 };
        assert_eq!(toast.action.is_some(), action, "{}", toast.message);
        assert!(n <= max, "{n} characters (max {max}): {}", toast.message);
        seen.push((toast.message.clone(), n));
        h.apply(Command::DismissToast { id: toast.id });
    };
    // The question first: an extension installed a moment ago may open any of its own pages, so an
    // `ExtensionInstalled` for the same id would turn the ask into an Open.
    h.apply(foreign(&format!("chrome-extension://{OTHER}/app/x.html"), Some(ForeignExtension { name: long.into(), ..extension(OTHER) })));
    record(&mut h, true);
    h.apply(Command::ExtensionInstalled { id: EXT.into(), name: long.into(), external: false });
    record(&mut h, false);
    h.apply(Command::ExtensionInstalled { id: OTHER.into(), name: long.into(), external: true });
    record(&mut h, false);
    h.apply(Command::ForeignBlocked { reason: ForeignBlockReason::RateLimited });
    record(&mut h, false);
    h.apply(Command::ForeignBlocked { reason: ForeignBlockReason::Incognito });
    record(&mut h, false);
    // The question still reads as a whole sentence at the budget: only the name is ellipsized.
    let ask = &seen[0].0;
    assert!(ask.starts_with("An extension wants to open a page of ") && ask.ends_with('…'), "{ask}");
}

#[test]
fn an_ask_the_user_never_answers_leaves_the_adoption_budget_alone() {
    let mut h = Harness::new();
    let ext = extension(EXT);
    let ask = |n: char| format!("chrome-extension://{EXT}/app/{n}.html");
    // Two undeclared pages ask…
    for n in ['a', 'b'] {
        h.apply(foreign(&ask(n), Some(ext.clone())));
        assert_eq!(h.toast().map(|t| t.message), Some("An extension wants to open a page of Blocker".into()), "{n}");
    }
    // …a third ask is dropped: the toast path has a budget of its own, so it can't be spammed.
    h.apply(foreign(&ask('c'), Some(ext.clone())));
    assert_eq!(h.toast().map(|t| t.message), Some("An extension keeps opening windows; sta blocked them".into()));
    assert!(h.today().is_empty(), "an ask never opens anything: {:?}", h.today());

    // None of them spent an adoption slot: the extension's welcome tab, and two more pages, open.
    h.apply(Command::DismissToast { id: h.toast().unwrap().id });
    for i in 0..3 {
        h.apply(foreign(&format!("https://a.example/{i}"), None));
    }
    assert_eq!(h.today().len(), 3, "the asks left the adoption budget alone: {:?}", h.today());
}

#[test]
fn refused_urls_never_spend_the_budget() {
    let mut h = Harness::new();
    for i in 0..30 {
        h.apply(foreign(&format!("chrome://settings/{i}"), None));
        h.apply(foreign("javascript:alert(1)", None));
    }
    assert!(h.toast().is_none(), "a refusal is silent: {:?}", h.toast());
    for i in 0..3 {
        h.apply(foreign(&format!("https://a.example/{i}"), None));
    }
    assert_eq!(h.today().len(), 3, "{:?}", h.today());
}

#[test]
fn install_and_block_toasts() {
    let mut h = Harness::new();
    h.apply(Command::ExtensionInstalled { id: EXT.into(), name: "AdBlock".into(), external: false });
    // sta has no extension toolbar, so a fresh install is told how to use it (FINAL PLAN §2),
    // with the shortcut spelled the way this platform spells it.
    let picker = if cfg!(target_os = "macos") { "⌘E" } else { "Ctrl+E" };
    assert_eq!(h.toast().map(|t| t.message), Some(format!("AdBlock added · {picker}")));
    h.apply(Command::ExtensionInstalled { id: OTHER.into(), name: "NordPass".into(), external: true });
    assert_eq!(h.toast().map(|t| t.message), Some("NordPass added by another program, off until you allow it".into()));
    // One line of the toast holds about 70 characters (ui/toast/toast.css).
    h.apply(Command::ExtensionInstalled { id: OTHER.into(), name: "A very long extension name indeed".into(), external: true });
    assert!(h.toast().unwrap().message.chars().count() <= 70, "{:?}", h.toast().unwrap().message);
    // A bad id is ignored; a long name is shortened.
    h.apply(Command::DismissToast { id: h.toast().unwrap().id });
    h.apply(Command::ExtensionInstalled { id: "nope".into(), name: "x".into(), external: false });
    assert!(h.toast().is_none());
    h.apply(Command::ExtensionInstalled { id: EXT.into(), name: "N".repeat(100), external: false });
    let message = h.toast().unwrap().message;
    assert!(message.chars().count() <= 48 + format!(" added · {picker}").chars().count(), "{message}");
    assert!(message.contains('…') && message.ends_with(&format!(" added · {picker}")), "{message}");

    h.apply(Command::ForeignBlocked { reason: ForeignBlockReason::RateLimited });
    assert_eq!(h.toast().map(|t| t.message), Some("An extension keeps opening windows; sta blocked them".into()));
    h.apply(Command::ForeignBlocked { reason: ForeignBlockReason::Incognito });
    assert_eq!(h.toast().map(|t| t.message), Some("sta has no private windows".into()));
}

#[test]
fn shell_events_are_refused_from_the_ui() {
    for cmd in [
        foreign("https://a.com/", None),
        Command::ExtensionInstalled { id: EXT.into(), name: "x".into(), external: false },
        Command::ForeignBlocked { reason: ForeignBlockReason::Incognito },
        Command::LowDiskSpace { free_mb: 342 },
    ] {
        assert!(!cmd.allowed_from_ui(), "{cmd:?}");
        let wrapped = Command::CommitOmnibox { command: Box::new(cmd.clone()), alt: false };
        assert!(!wrapped.allowed_from_ui(), "{cmd:?} inside commitOmnibox");
    }
}

/// A full disk makes Chromium fail an install with "Could not unzip extension"; the shell reports
/// the disk when a store page opens, and core says it once — every later store page would only
/// repeat a toast the user has already read.
#[test]
fn a_nearly_full_disk_is_said_once_per_run() {
    let mut h = Harness::new();
    h.apply(Command::LowDiskSpace { free_mb: 342 });
    let toast = h.toast().expect("a toast");
    assert!(toast.message.contains("342 MB") && toast.message.contains("Could not unzip extension"), "{}", toast.message);
    assert!(toast.duration_ms > 2500, "it explains an error, so it stays up longer than a plain toast");
    h.apply(Command::DismissToast { id: toast.id });
    h.apply(Command::LowDiskSpace { free_mb: 120 });
    assert!(h.toast().is_none(), "the second report in one run is silent");
}

#[test]
fn devtools_never_open_on_sta_pages() {
    let mut h = Harness::new();
    let web = h.open("https://a.com/");
    let fx = h.apply(Command::ToggleDevTools);
    assert!(has(&fx, |e| matches!(e, Effect::OpenDevTools { tab, docked: true } if *tab == web)), "{fx:?}");
    assert!(h.toast().is_none());

    h.apply(Command::OpenInternalPage { page: InternalPage::Settings });
    let internal = h.focused().expect("settings tab");
    assert!(h.tab(internal).url.starts_with("sta://"));
    let fx = h.apply(Command::ToggleDevTools);
    assert!(!has(&fx, |e| matches!(e, Effect::OpenDevTools { .. })), "{fx:?}");
    assert_eq!(h.toast().map(|t| t.message), Some("DevTools isn't available on sta pages".into()));

    // The debug override (`STA_DEVTOOLS_INTERNAL=1`, debug builds) allows it again.
    h.store.set_devtools_on_internal_pages(true);
    let fx = h.apply(Command::ToggleDevTools);
    assert!(has(&fx, |e| matches!(e, Effect::OpenDevTools { tab, .. } if *tab == internal)), "{fx:?}");
}
