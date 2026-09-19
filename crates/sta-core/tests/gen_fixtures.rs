//! UI mock fixtures generated from the real serde types:
//! `cargo test -p sta-core -- --ignored gen_fixtures` writes `ui/common/fixtures/*.json`.
//!
//! The profile is built through the checking harness (invariants and effect guarantees hold at
//! every step). Timestamps are relative to `FIXTURE_NOW`, the instant `ui/common/mock.js` rebases
//! fixture times against. `appInfo.json` is shell data and is not generated here.

mod common;
use sta_core::*;
use common::*;
use std::path::PathBuf;

/// 2026-09-16T12:00:00Z (`FIXTURE_NOW` in `ui/common/mock.js`).
const FIXTURE_NOW: Millis = 1_789_560_000_000;

const DUSK: Theme = Theme { hue: 300.0, hue2: 340.0, chroma: 0.06 };
const LAGOON: Theme = Theme { hue: 190.0, hue2: 230.0, chroma: 0.06 };
const EMBER: Theme = Theme { hue: 50.0, hue2: 20.0, chroma: 0.07 };

fn visit(h: &mut Harness, url: &str, title: &str) -> Id {
    let id = h.open(url);
    h.commit(id, url, title);
    h.advance(7 * MIN);
    id
}

fn typed(h: &mut Harness, text: &str, url: &str, title: &str) -> Id {
    h.apply(Command::OpenInput { text: text.into(), target: OpenTarget::NewTab });
    let id = h.focused().unwrap();
    h.commit(id, url, title);
    h.advance(5 * MIN);
    id
}

#[allow(clippy::too_many_arguments)]
fn download(id: u32, tab: Option<Id>, name: &str, received: i64, total: Option<i64>, speed: i64, state: DownloadState, started_at: Millis) -> Download {
    Download {
        id,
        tab,
        url: format!("https://downloads.example.com/{name}"),
        file_name: name.into(),
        path: Some(format!("C:\\Users\\me\\Downloads\\{name}")),
        received_bytes: received,
        total_bytes: total,
        bytes_per_sec: speed,
        state,
        started_at,
    }
}

fn dismiss_toast(h: &mut Harness) {
    if let Some(t) = h.toast() {
        h.apply(Command::DismissToast { id: t.id });
    }
}

/// The populated profile: 3 spaces, 6 favorites, pinned tabs with nested folders and a navigated
/// tab, 9 Today tabs (one split) in loading/audible/muted/unloaded/failed/crashed variations,
/// downloads in every state (one of unknown size), archive (auto, closed, split group), history
/// with typed visits, a boost, and the command bar
/// open in New Tab mode.
fn build() -> (Harness, Id) {
    let start = FIXTURE_NOW - 5 * DAY;
    let mut h = Harness::start_at(Store::new(start), Vec::new(), start);
    h.apply(Command::UpdateSettings { patch: SettingsPatch { archive_after_hours: Some(24), ..Default::default() } });

    // ------------------------------------------------------------------ Personal (Dusk)
    let personal = h.space();
    h.apply(Command::UpdateSpace { id: personal, name: Some("Personal".into()), icon: Some("🏠".into()), theme: Some(DUSK) });
    let bank = typed(&mut h, "bank.example.com/login", "https://bank.example.com/login", "Online Banking");
    h.apply(Command::TogglePin { id: Some(bank) });
    visit(&mut h, "https://longreads.example.com/story/the-quiet-web", "The Quiet Web — A Very Long Read");
    h.advance(20 * HOUR);
    visit(&mut h, "https://www.seriouseats.com/the-food-lab", "The Food Lab | Serious Eats");
    visit(&mut h, "https://www.amazon.com/gp/your-account/order-history", "Your Orders");
    h.apply(Command::UnloadTab { id: bank });
    h.advance(5 * HOUR);
    h.apply(Command::Tick); // archives the long read (idle > 24 h)
    let rebase = typed(&mut h, "git-scm.com/docs/git-rebase", "https://git-scm.com/docs/git-rebase", "Git - git-rebase Documentation");
    h.commit(rebase, "https://git-scm.com/docs/git-rebase#_interactive_mode", "Git - git-rebase Documentation");
    let gitlab = visit(&mut h, "https://gitlab.com/explore/projects", "Explore projects · GitLab");
    let food = h.today().into_iter().find(|t| h.tab(*t).url.contains("seriouseats")).unwrap();
    for t in [rebase, gitlab] {
        h.apply(Command::CloseItem { id: Some(t) });
    }
    h.apply(Command::UnloadTab { id: food });

    // ------------------------------------------------------------------ Play (Ember)
    h.advance(20 * HOUR);
    h.apply(Command::NewSpace { name: "Play".into(), icon: "🎮".into(), theme: EMBER });
    let steam = visit(&mut h, "https://store.steampowered.com/", "Welcome to Steam");
    let reddit = visit(&mut h, "https://www.reddit.com/r/Games/", "r/Games");
    visit(&mut h, "https://www.twitch.tv/directory", "Browse - Twitch");
    h.apply(Command::CloseItem { id: Some(reddit) });
    let wiki = visit(&mut h, "https://minecraft.wiki/w/Redstone_circuits", "Redstone circuits – Minecraft Wiki");
    let chess = visit(&mut h, "https://www.chess.com/play/online", "Play Chess Online - Chess.com");
    h.apply(Command::SplitWith { tab: chess, with: wiki, side: SplitSide::Right });
    let play_split = h.active().unwrap();
    h.apply(Command::SetSplitFractions { id: play_split, fractions: vec![0.6, 0.4] });
    h.advance(HOUR);
    h.apply(Command::CloseItem { id: Some(play_split) });
    h.apply(Command::UnloadTab { id: steam });

    // ------------------------------------------------------------------ Work (Lagoon)
    h.advance(10 * HOUR);
    h.apply(Command::NewSpace { name: "Work".into(), icon: "🚀".into(), theme: LAGOON });

    // Favorites (shared by all spaces).
    let gmail = typed(&mut h, "mail.google.com", "https://mail.google.com/mail/u/0/#inbox", "Inbox (3) - me@example.com - Gmail");
    let calendar = visit(&mut h, "https://calendar.google.com/calendar/u/0/r/week", "Google Calendar - Week of September 14, 2026");
    let github = typed(&mut h, "github.com", "https://github.com/", "GitHub");
    let youtube = visit(&mut h, "https://www.youtube.com/", "YouTube");
    let figma = visit(&mut h, "https://www.figma.com/files/recents", "Recents – Figma");
    let notion = visit(&mut h, "https://www.notion.so/sta/Roadmap", "Roadmap");
    for f in [gmail, calendar, github, youtube, figma, notion] {
        h.apply(Command::AddFavorite { id: Some(f) });
    }
    for f in [calendar, figma, notion] {
        h.apply(Command::UnloadTab { id: f });
    }
    h.apply(Command::TabFaviconChanged { tab: gmail, url: Some("https://ssl.gstatic.com/ui/v1/icons/mail/rfr/gmail.ico".into()) });
    h.apply(Command::TabFaviconChanged { tab: github, url: Some("https://github.githubassets.com/favicons/favicon.svg".into()) });

    // Pinned: Projects/{Specs/{spec doc}, Linear}, a navigated GitHub tab, unloaded Slack.
    h.apply(Command::NewFolder { space: None, parent: None, name: Some("Projects".into()) });
    let projects = h.pinned()[0];
    let linear = visit(&mut h, "https://linear.app/sta/team/AST/active", "Active issues › sta › Linear");
    h.apply(Command::MoveItem { id: linear, to: DropTarget { container: Container::Folder { id: projects }, before: None } });
    h.apply(Command::NewFolder { space: None, parent: Some(projects), name: Some("Specs".into()) });
    let specs = h.folder(projects).children[0];
    let spec = visit(&mut h, "https://docs.google.com/document/d/1StaSpec/edit", "sta – Product spec - Google Docs");
    h.apply(Command::MoveItem { id: spec, to: DropTarget { container: Container::Folder { id: specs }, before: None } });
    h.apply(Command::UnloadTab { id: spec });
    let pulls = visit(&mut h, "https://github.com/pulls", "Pull requests");
    h.apply(Command::TogglePin { id: Some(pulls) });
    h.commit(pulls, "https://github.com/sta/sta/pull/42", "Keep split fractions on restore by p-asta · Pull Request #42");
    let slack = visit(&mut h, "https://app.slack.com/client/T0STA/C0GENERAL", "general (Channel) - sta - Slack");
    h.apply(Command::TogglePin { id: Some(slack) });
    h.apply(Command::UnloadTab { id: slack });
    h.apply(Command::CloseSidebarPanel); // NewFolder opened the inline rename

    // Today, oldest first (newest ends up on top).
    let wikipedia = visit(&mut h, "https://en.wikipedia.org/wiki/Chromium_Embedded_Framework", "Chromium Embedded Framework - Wikipedia");
    let hn = visit(&mut h, "https://news.ycombinator.com/", "Hacker News");
    let mdn = visit(&mut h, "https://developer.mozilla.org/en-US/docs/Web/CSS/color_value/oklch", "oklch() - CSS: Cascading Style Sheets | MDN");
    let search = typed(&mut h, "cef views overlay", "https://www.google.com/search?q=cef+views+overlay", "cef views overlay - Google Search");
    let lofi = visit(&mut h, "https://www.youtube.com/watch?v=jfKfPfyJRdk", "lofi hip hop radio 📚 beats to relax/study to - YouTube");
    let so = visit(&mut h, "https://stackoverflow.com/questions/47437376/refcell-already-borrowed", "rust - RefCell<T> already borrowed panic - Stack Overflow");
    let design = visit(&mut h, "https://www.figma.com/design/k3ySta/sta-UI", "sta UI – Figma");
    let serde_docs = visit(&mut h, "https://docs.rs/serde/latest/serde/", "serde - Rust");
    let refcell = visit(&mut h, "https://doc.rust-lang.org/std/cell/struct.RefCell.html", "RefCell in std::cell - Rust");
    for t in [wikipedia, hn] {
        h.apply(Command::UnloadTab { id: t });
    }
    h.apply(Command::TabFaviconChanged { tab: mdn, url: Some("https://developer.mozilla.org/favicon-48x48.png".into()) });
    h.apply(Command::TabLoadFailed {
        tab: mdn,
        url: "https://developer.mozilla.org/en-US/docs/Web/CSS/color_value/oklch".into(),
        error_code: -106,
        error_text: "net::ERR_INTERNET_DISCONNECTED".into(),
    });
    h.apply(Command::TabCrashed { tab: search });
    h.apply(Command::TabAudioChanged { tab: lofi, audible: true });
    h.apply(Command::ToggleMute { id: Some(design) });
    h.apply(Command::SplitWith { tab: refcell, with: serde_docs, side: SplitSide::Right });
    let work_split = h.active().unwrap();
    h.apply(Command::SetSplitFractions { id: work_split, fractions: vec![0.55, 0.45] });
    h.apply(Command::ActivateItem { id: search });
    h.apply(Command::ActivateItem { id: so });
    h.apply(Command::ActivateItem { id: work_split });
    h.apply(Command::FocusPane { index: 1 });
    // The Stack Overflow tab reloads in the background.
    h.apply(Command::TabLoadingStateChanged { tab: so, loading: true, can_go_back: true, can_go_forward: false });
    h.apply(Command::TabLoadProgress { tab: so, progress: 0.6 });
    h.apply(Command::TabLoadingStateChanged { tab: refcell, loading: false, can_go_back: true, can_go_forward: false });

    // Downloads (newest first in the UI): every state, one of unknown size.
    let downloads = [
        download(1, Some(github), "rustup-init.exe", 7_874_560, Some(7_874_560), 0, DownloadState::Complete, h.now - 2 * HOUR),
        download(2, None, "cef_binary_152_windows64.tar.bz2", 96_432_128, Some(412_876_800), 0, DownloadState::Interrupted, h.now - HOUR),
        download(3, None, "old-build.zip", 1_048_576, Some(73_741_824), 0, DownloadState::Cancelled, h.now - 50 * MIN),
        download(4, Some(serde_docs), "fonts-bundle.zip", 22_020_096, Some(52_428_800), 0, DownloadState::Paused, h.now - 20 * MIN),
        download(5, Some(refcell), "node-v22-x64.msi", 18_350_080, None, 2_202_009, DownloadState::InProgress, h.now - 90_000),
        download(6, Some(refcell), "sta-spec.pdf", 12_897_485, Some(41_943_040), 1_258_291, DownloadState::InProgress, h.now - 30_000),
    ];
    for d in downloads {
        h.apply(Command::DownloadUpdated { download: d });
    }

    // A boost for GitHub.
    h.apply(Command::UpsertBoost {
        boost: Boost {
            id: 0,
            name: "GitHub wide".into(),
            host: "github.com".into(),
            enabled: true,
            css: "/* Use the full window width for code and diffs */\n.container-xl, .container-lg {\n  max-width: none !important;\n}\n".into(),
            js: "// Collapse the file tree by default on pull request pages\ndocument.querySelector('[data-hotkey=\"Control+b\"]')?.click();\n".into(),
            ..Default::default()
        },
    });
    let boost = h.store.state().boosts[0].id;

    // Final moment: the command bar is open in New Tab mode.
    h.now = FIXTURE_NOW;
    dismiss_toast(&mut h);
    h.apply(Command::OpenCommandBar { mode: CommandBarMode::NewTab, split_side: None });
    (h, boost)
}

fn write(name: &str, value: &impl serde::Serialize) {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../ui/common/fixtures");
    std::fs::create_dir_all(&dir).expect("fixtures dir");
    let mut json = serde_json::to_string_pretty(value).expect("serialize");
    json.push('\n');
    let path = dir.join(format!("{name}.json"));
    std::fs::write(&path, json).expect("write fixture");
    println!("wrote {}", path.display());
}

fn request(text: &str, mode: CommandBarMode) -> OmniboxRequest {
    OmniboxRequest { text: text.into(), mode, split_side: None, prevent_inline_autocomplete: false, suggestions: Vec::new(), seq: 1 }
}

fn group_count(r: &OmniboxResponse, group: ResultGroup) -> usize {
    r.results.iter().filter(|x| x.group == group).count()
}

#[test]
#[ignore = "writes ui/common/fixtures; run with --ignored gen_fixtures"]
fn gen_fixtures() {
    let (mut h, boost) = build();
    let now = FIXTURE_NOW;
    let ui = h.ui();
    // Shape checks so the fixture stays representative.
    assert_eq!(ui.spaces.len(), 3);
    assert_eq!(ui.favorites.len(), 6);
    assert_eq!(ui.downloads.len(), 6);
    for state in [DownloadState::InProgress, DownloadState::Paused, DownloadState::Complete, DownloadState::Cancelled, DownloadState::Interrupted] {
        assert!(ui.downloads.iter().any(|d| d.state == state), "{state:?}");
    }
    assert!(ui.downloads.iter().any(|d| d.total_bytes.is_none()));
    assert_eq!(ui.command_bar.as_ref().map(|c| c.mode), Some(CommandBarMode::NewTab));
    assert!(ui.archive_count >= 4);
    let work = ui.spaces.iter().find(|s| s.id == ui.active_space).unwrap();
    assert_eq!(work.today.len(), 8);
    assert!(work.today.iter().any(|n| matches!(n, NodeView::Split(s) if s.active)));
    assert!(work.pinned.iter().any(|n| matches!(n, NodeView::Folder(f) if f.children.iter().any(|c| matches!(c, NodeView::Folder(_))))));
    assert!(work.pinned.iter().any(|n| matches!(n, NodeView::Tab(t) if t.navigated && t.loaded)));
    let today_tabs: Vec<&TabView> = work.today.iter().filter_map(|n| if let NodeView::Tab(t) = n { Some(t) } else { None }).collect();
    assert!(today_tabs.iter().any(|t| t.failed) && today_tabs.iter().any(|t| t.crashed));
    assert!(today_tabs.iter().any(|t| t.audible) && today_tabs.iter().any(|t| t.muted) && today_tabs.iter().any(|t| t.loading));
    assert!(today_tabs.iter().any(|t| !t.loaded));

    // "git" with remote suggestions: Go (inline completion), tabs, history, suggestions, archive.
    let mut git_request = request("git", CommandBarMode::NewTab);
    git_request.suggestions = vec!["github copilot".into(), "git rebase interactive".into(), "gitignore template".into()];
    let git = h.store.omnibox(&git_request, now);
    assert_eq!(git.inline_completion.as_deref(), Some("github.com"));
    for group in [ResultGroup::Go, ResultGroup::Tabs, ResultGroup::History, ResultGroup::Suggestions, ResultGroup::Archive] {
        assert!(group_count(&git, group) > 0, "{group:?} missing from omnibox.json");
    }
    let empty = h.store.omnibox(&request("", CommandBarMode::NewTab), now);
    let actions = OmniboxResponse { text: String::new(), seq: 1, inline_completion: None, results: h.store.omnibox_actions() };

    write("uiState", &ui);
    write("omnibox", &git);
    write("omniboxEmpty", &empty);
    write("omniboxActions", &actions);
    write("archive", &h.store.archive_list());
    write("history", &h.store.history_list("", 100, now));
    write("boost", &h.store.boost(boost).expect("boost"));
    h.apply(Command::SystemThemeChanged { dark: true });
    write("uiStateDark", &h.ui());
}
