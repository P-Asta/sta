//! Command bar results assembled by `Store::omnibox` (arc_spec §6): groups and order, inline
//! completion, per-mode commands, top hit, suggestions, archive/history rows and the actions
//! registry.

mod common;
use sta_core::*;
use common::*;

struct Fixture {
    h: Harness,
    github: Id,
    docs: Id,
    news: Id,
    gitlab_closed: Id,
    article: Id,
    work: Id,
}

fn fixture() -> Fixture {
    let mut h = Harness::new();
    // Typed visits (inline-completion candidates), then the tab is closed.
    h.apply(Command::OpenInput { text: "gitlab.com".into(), target: OpenTarget::NewTab });
    let gitlab_closed = h.focused().unwrap();
    h.commit(gitlab_closed, "https://gitlab.com/", "GitLab");
    h.apply(Command::OpenInput { text: "gitlab.com/explore".into(), target: OpenTarget::CurrentTab });
    h.commit(gitlab_closed, "https://gitlab.com/explore", "Explore GitLab");
    h.apply(Command::CloseItem { id: Some(gitlab_closed) });
    let github = h.open("https://github.com/rust-lang/rust");
    h.commit(github, "https://github.com/rust-lang/rust", "rust-lang/rust: Empowering everyone");
    h.apply(Command::OpenInput { text: "docs.rs".into(), target: OpenTarget::NewTab });
    let docs = h.focused().unwrap();
    h.commit(docs, "https://docs.rs/", "Docs.rs");
    let article = h.open("https://blog.example.org/article");
    h.commit(article, "https://blog.example.org/article", "Archived Article");
    h.apply(Command::CloseItem { id: Some(article) });
    let home = h.space();
    let work = h.new_space("Work", "🚀", Theme { hue: 190.0, hue2: 230.0, chroma: 0.06 });
    h.apply(Command::SwitchSpace { id: home });
    let news = h.open("https://news.example.com/");
    h.commit(news, "https://news.example.com/", "Daily News");
    Fixture { h, github, docs, news, gitlab_closed, article, work }
}

fn req(text: &str, mode: CommandBarMode) -> OmniboxRequest {
    OmniboxRequest { text: text.into(), mode, split_side: None, prevent_inline_autocomplete: false, suggestions: Vec::new(), seq: 7 }
}

fn group_rank(g: ResultGroup) -> usize {
    match g {
        ResultGroup::TopHit => 0,
        ResultGroup::Go => 1,
        ResultGroup::RecentTabs => 2,
        ResultGroup::SuggestedActions => 3,
        ResultGroup::Tabs => 4,
        ResultGroup::Actions => 5,
        ResultGroup::Spaces => 6,
        ResultGroup::History => 7,
        ResultGroup::Suggestions => 8,
        ResultGroup::Archive => 9,
        // Ctrl+E only (its own mode, never mixed with the groups above).
        ResultGroup::Extensions => 10,
        ResultGroup::NeedsOk => 11,
        ResultGroup::ExtensionsOff => 12,
        ResultGroup::More => 13,
    }
}

fn check_shape(r: &OmniboxResponse) {
    assert!(r.results.len() <= 12, "{} results", r.results.len());
    let mut keys: Vec<&str> = r.results.iter().map(|x| x.key.as_str()).collect();
    keys.sort();
    keys.dedup();
    assert_eq!(keys.len(), r.results.len(), "duplicate keys");
    let ranks: Vec<usize> = r.results.iter().map(|x| group_rank(x.group)).collect();
    assert!(ranks.windows(2).all(|w| w[0] <= w[1]), "groups out of order: {:?}", r.results.iter().map(|x| x.group).collect::<Vec<_>>());
    for x in &r.results {
        assert!(x.command.allowed_from_ui(), "{x:?}");
        assert!(!matches!(x.command, Command::CommitOmnibox { .. }));
    }
}

fn find<'a>(r: &'a OmniboxResponse, key: &str) -> &'a OmniboxResult {
    r.results.iter().find(|x| x.key == key).unwrap_or_else(|| panic!("no {key} in {:#?}", r.results.iter().map(|x| &x.key).collect::<Vec<_>>()))
}

#[test]
fn inline_completion_and_what_you_typed() {
    let f = fixture();
    let rev = f.h.store.revision();
    let r = f.h.store.omnibox(&req("git", CommandBarMode::NewTab), f.h.now);
    check_shape(&r);
    assert_eq!((r.text.as_str(), r.seq), ("git", 7));
    assert_eq!(r.inline_completion.as_deref(), Some("gitlab.com"), "typed host completes");
    let go = &r.results[0];
    assert_eq!((go.key.as_str(), go.group, go.title.as_str()), ("go", ResultGroup::Go, "gitlab.com"));
    assert_eq!(go.subtitle.as_deref(), Some("https://gitlab.com"));
    assert_eq!(go.command, Command::OpenInput { text: "gitlab.com".into(), target: OpenTarget::NewTab });
    assert_eq!(go.alt_command, Some(Command::OpenInput { text: "gitlab.com".into(), target: OpenTarget::BackgroundTab }));
    assert!(r.results.iter().all(|x| x.group != ResultGroup::TopHit), "inline completion wins over a top hit");
    // Open tab (deduped against history), history rows, archive rows.
    let tab = find(&r, &format!("tab:{}", f.github));
    assert_eq!(tab.group, ResultGroup::Tabs);
    assert_eq!(tab.command, Command::ActivateItem { id: f.github });
    assert_eq!(tab.hint.as_deref(), Some("Switch to Tab"));
    assert!(tab.subtitle.as_deref().unwrap().starts_with("github.com · "));
    assert!(r.results.iter().all(|x| x.key != "history:https://github.com/rust-lang/rust"), "open tabs hide their history row");
    let hist = find(&r, "history:https://gitlab.com/");
    assert_eq!(hist.command, Command::OpenUrl { url: "https://gitlab.com/".into(), target: OpenTarget::NewTab, opener: None });
    assert_eq!(hist.alt_command, Some(Command::OpenUrl { url: "https://gitlab.com/".into(), target: OpenTarget::BackgroundTab, opener: None }));
    assert_eq!(hist.hint.as_deref(), Some("just now"));
    let arch = find(&r, &format!("archive:{}", f.gitlab_closed));
    assert_eq!(arch.command, Command::RestoreArchived { id: f.gitlab_closed, whole_group: false });
    assert_eq!(f.h.store.revision(), rev, "queries are pure");

    // Path completion only once a '/' was typed.
    let r = f.h.store.omnibox(&req("gitlab.com/e", CommandBarMode::NewTab), f.h.now);
    assert_eq!(r.inline_completion.as_deref(), Some("gitlab.com/explore"));
    let r = f.h.store.omnibox(&req("gitl", CommandBarMode::NewTab), f.h.now);
    assert_eq!(r.inline_completion.as_deref(), Some("gitlab.com"));
    // Backspace, trailing space, '?', scheme typed: no completion.
    let mut no_inline = req("gitl", CommandBarMode::NewTab);
    no_inline.prevent_inline_autocomplete = true;
    for request in [no_inline, req("git ", CommandBarMode::NewTab), req("?git", CommandBarMode::NewTab), req("https://git", CommandBarMode::NewTab)] {
        let r = f.h.store.omnibox(&request, f.h.now);
        assert_eq!(r.inline_completion, None, "{:?}", request.text);
        check_shape(&r);
    }
    // Link-only (low frecency) hosts don't complete.
    assert_eq!(f.h.store.omnibox(&req("githu", CommandBarMode::NewTab), f.h.now).inline_completion, None);
    // Case of the typed prefix is kept.
    assert_eq!(f.h.store.omnibox(&req("GitL", CommandBarMode::NewTab), f.h.now).inline_completion.as_deref(), Some("GitLab.com"));
}

#[test]
fn modes_change_commands() {
    let f = fixture();
    let s = &f.h.store;
    // Search row.
    let r = s.omnibox(&req("rust lang", CommandBarMode::NewTab), f.h.now);
    check_shape(&r);
    let search = find(&r, "search");
    assert_eq!((search.title.as_str(), search.subtitle.as_deref()), ("rust lang", Some("Search Google")));
    assert_eq!(search.command, Command::OpenInput { text: "rust lang".into(), target: OpenTarget::NewTab });
    // Edit URL mode navigates the current tab.
    let r = s.omnibox(&req("example.com", CommandBarMode::EditUrl), f.h.now);
    check_shape(&r);
    let go = find(&r, "go");
    assert_eq!(go.command, Command::OpenInput { text: "example.com".into(), target: OpenTarget::CurrentTab });
    assert_eq!(go.alt_command, Some(Command::OpenInput { text: "example.com".into(), target: OpenTarget::BackgroundTab }));
    let r = s.omnibox(&req("gitlab", CommandBarMode::EditUrl), f.h.now);
    assert_eq!(find(&r, "history:https://gitlab.com/").command, Command::OpenUrl { url: "https://gitlab.com/".into(), target: OpenTarget::CurrentTab, opener: None });
    // Split mode opens panes; tabs join the split; no actions/spaces/archive.
    let mut split = req("git", CommandBarMode::Split);
    split.split_side = Some(SplitSide::Left);
    let r = s.omnibox(&split, f.h.now);
    check_shape(&r);
    assert_eq!(find(&r, "go").command, Command::SplitOpenInput { text: "gitlab.com".into(), side: SplitSide::Left });
    assert_eq!(find(&r, &format!("tab:{}", f.github)).command, Command::SplitWith { tab: f.github, with: f.news, side: SplitSide::Left });
    assert_eq!(find(&r, "history:https://gitlab.com/").command, Command::SplitOpenInput { text: "https://gitlab.com/".into(), side: SplitSide::Left });
    assert!(r.results.iter().all(|x| !matches!(x.group, ResultGroup::Actions | ResultGroup::Spaces | ResultGroup::Archive)));
    // DuckDuckGo naming.
    let mut h = f.h;
    h.apply(Command::UpdateSettings { patch: SettingsPatch { search_engine: Some(SearchEngineId::DuckDuckGo), ..Default::default() } });
    let r = h.store.omnibox(&req("?docs.rs", CommandBarMode::NewTab), h.now);
    let search = find(&r, "search");
    assert_eq!((search.title.as_str(), search.subtitle.as_deref()), ("docs.rs", Some("Search DuckDuckGo")));
    assert_eq!(search.command, Command::OpenInput { text: "?docs.rs".into(), target: OpenTarget::NewTab });
    // Committing the row really searches.
    h.apply(Command::CommitOmnibox { command: Box::new(search.command.clone()), alt: false });
    assert_eq!(h.tab(h.focused().unwrap()).url, "https://duckduckgo.com/?q=docs.rs");
}

#[test]
fn top_hit_spaces_suggestions_archive() {
    let f = fixture();
    let s = &f.h.store;
    // A strong tab match beats the URL row.
    let r = s.omnibox(&req("docs.rs", CommandBarMode::NewTab), f.h.now);
    check_shape(&r);
    assert_eq!((r.results[0].group, r.results[0].key.clone()), (ResultGroup::TopHit, format!("tab:{}", f.docs)));
    assert_eq!(r.results[0].command, Command::ActivateItem { id: f.docs });
    assert_eq!(r.results[1].key, "go");
    // A weak match does not.
    let r = s.omnibox(&req("zzqx.com", CommandBarMode::NewTab), f.h.now);
    assert_eq!(r.results[0].key, "go");
    assert_eq!(r.results.len(), 1);
    // Spaces (other than the active one).
    let r = s.omnibox(&req("work", CommandBarMode::NewTab), f.h.now);
    check_shape(&r);
    let space = find(&r, &format!("space:{}", f.work));
    assert_eq!(space.command, Command::SwitchSpace { id: f.work });
    assert_eq!(space.icon, ResultIcon::Emoji { emoji: "🚀".into() });
    assert!(r.results.iter().any(|x| x.command == Command::MoveToSpace { id: Some(f.news), space: f.work }));
    assert!(r.results.iter().all(|x| !x.title.starts_with("Go to Space")), "per-space goto rows are actions-mode only");
    assert!(s.omnibox(&req("home", CommandBarMode::NewTab), f.h.now).results.iter().all(|x| x.group != ResultGroup::Spaces));
    // Remote suggestions: de-duplicated, never repeating the query (no inline completion here:
    // the last edit was a deletion).
    let mut with_suggestions = req("rust", CommandBarMode::NewTab);
    with_suggestions.suggestions = vec!["rust lang".into(), "Rust Lang".into(), "rust".into(), " ".into(), "rust book".into()];
    with_suggestions.prevent_inline_autocomplete = true;
    let r = s.omnibox(&with_suggestions, f.h.now);
    check_shape(&r);
    let sugg: Vec<&OmniboxResult> = r.results.iter().filter(|x| x.group == ResultGroup::Suggestions).collect();
    assert_eq!(sugg.iter().map(|x| x.title.as_str()).collect::<Vec<_>>(), ["rust lang", "rust book"]);
    assert_eq!(sugg[0].command, Command::OpenUrl { url: "https://www.google.com/search?q=rust%20lang".into(), target: OpenTarget::NewTab, opener: None });
    // Otherwise the first one completes inline and becomes the Enter row.
    with_suggestions.prevent_inline_autocomplete = false;
    let r = s.omnibox(&with_suggestions, f.h.now);
    check_shape(&r);
    assert_eq!(r.inline_completion.as_deref(), Some("rust lang"));
    assert_eq!(r.results[0].command, sugg[0].command);
    assert_eq!(r.results.iter().filter(|x| x.group == ResultGroup::Suggestions).map(|x| x.title.as_str()).collect::<Vec<_>>(), ["rust book"]);
    // Archive rows restore.
    let r = s.omnibox(&req("archived article", CommandBarMode::NewTab), f.h.now);
    check_shape(&r);
    let arch = find(&r, &format!("archive:{}", f.article));
    assert_eq!(arch.command, Command::RestoreArchived { id: f.article, whole_group: false });
    assert!(arch.subtitle.as_deref().unwrap().starts_with("blog.example.org · archived "));
    // Unloaded tabs say "Open".
    let mut h = f.h;
    h.apply(Command::UnloadTab { id: f.github });
    let r = h.store.omnibox(&req("github", CommandBarMode::NewTab), h.now);
    assert_eq!(find(&r, &format!("tab:{}", f.github)).hint.as_deref(), Some("Open"));
    // The focused tab is never offered.
    let r = h.store.omnibox(&req("daily news", CommandBarMode::NewTab), h.now);
    assert!(r.results.iter().all(|x| x.key != format!("tab:{}", f.news)));
}

#[test]
fn empty_query_lists() {
    let f = fixture();
    let s = &f.h.store;
    let r = s.omnibox(&req("", CommandBarMode::NewTab), f.h.now);
    check_shape(&r);
    assert_eq!(r.inline_completion, None);
    let recent: Vec<&OmniboxResult> = r.results.iter().filter(|x| x.group == ResultGroup::RecentTabs).collect();
    assert_eq!(recent.iter().map(|x| x.command.clone()).collect::<Vec<_>>(), [Command::ActivateItem { id: f.docs }, Command::ActivateItem { id: f.github }]);
    let suggested: Vec<&str> = r.results.iter().filter(|x| x.group == ResultGroup::SuggestedActions).map(|x| x.key.as_str()).collect();
    assert_eq!(suggested, ["action:space.new", "action:view.archive", "action:sidebar.toggle", "action:view.settings"]);
    // Edit URL: Copy URL first.
    let r = s.omnibox(&req("  ", CommandBarMode::EditUrl), f.h.now);
    assert_eq!((r.results[0].key.as_str(), r.results[0].group), ("action:tab.copy_url", ResultGroup::SuggestedActions));
    assert_eq!(r.results[0].command, Command::CopyUrl { id: None, markdown: false });
    // Split: recent tabs join the split, no suggested actions.
    let r = s.omnibox(&req("", CommandBarMode::Split), f.h.now);
    assert!(r.results.iter().all(|x| x.group == ResultGroup::RecentTabs));
    assert_eq!(r.results[0].command, Command::SplitWith { tab: f.docs, with: f.news, side: SplitSide::Right });
    // Fresh profile.
    let h = Harness::new();
    let r = h.store.omnibox(&req("", CommandBarMode::NewTab), h.now);
    assert_eq!(r.results.len(), 4);
}

#[test]
fn actions_registry() {
    let f = fixture();
    let s = &f.h.store;
    let all = s.omnibox_actions();
    assert!(all.len() >= 40, "{} actions", all.len());
    let titles: Vec<String> = all.iter().map(|a| a.title.to_lowercase()).collect();
    let mut sorted = titles.clone();
    sorted.sort();
    assert_eq!(titles, sorted, "A–Z");
    for a in &all {
        assert_eq!(a.group, ResultGroup::Actions);
        assert!(a.key.starts_with("action:"));
        assert!(a.command.allowed_from_ui());
    }
    let by_title = |t: &str| all.iter().find(|a| a.title == t).unwrap_or_else(|| panic!("action {t}"));
    assert_eq!(by_title("Go to Space: Work").command, Command::SwitchSpace { id: f.work });
    assert_eq!(by_title("Go to Space: Work").hint.as_deref(), Some("Alt+2"));
    assert_eq!(by_title("Move Tab to Space: Work").command, Command::MoveToSpace { id: Some(f.news), space: f.work });
    assert_eq!(by_title("Pin Tab").command, Command::TogglePin { id: Some(f.news) });
    assert_eq!(by_title("Clear Today Tabs").hint.as_deref(), Some("Ctrl+Shift+K"));
    assert_eq!(by_title("Reopen Closed Tab").command, Command::ReopenClosed);
    assert_eq!(by_title("Clear Archive").command, Command::ClearArchive);
    assert_eq!(by_title("Add Right Split").command, Command::OpenCommandBar { mode: CommandBarMode::Split, split_side: Some(SplitSide::Right) });
    assert!(all.iter().all(|a| a.title != "Appearance: System"), "current appearance is not offered");
    assert!(all.iter().all(|a| a.title != "Separate All Tabs"), "no split active");
    // Actions mode (empty) = everything A–Z.
    let r = s.omnibox(&req("", CommandBarMode::Actions), f.h.now);
    assert_eq!(r.results.len(), 12);
    assert_eq!(r.results[0].title, all[0].title);
    // ">" prefix from any mode, fuzzy + alias bonus.
    let r = s.omnibox(&req(">pin", CommandBarMode::NewTab), f.h.now);
    check_shape(&r);
    assert_eq!(r.results[0].title, "Pin Tab");
    let r = s.omnibox(&req("> dark mode", CommandBarMode::NewTab), f.h.now);
    assert_eq!(r.results[0].title, "Appearance: Dark");
    // Actions mix into normal results.
    let r = s.omnibox(&req("toggle sidebar", CommandBarMode::NewTab), f.h.now);
    check_shape(&r);
    let toggle = r.results.iter().find(|x| x.command == Command::ToggleSidebar).expect("toggle sidebar action");
    assert!(matches!(toggle.group, ResultGroup::Actions | ResultGroup::TopHit));
    assert_eq!(toggle.hint.as_deref(), Some("Ctrl+S"));
    // Committing an action closes the bar and runs it.
    let mut h = f.h;
    h.apply(Command::OpenCommandBar { mode: CommandBarMode::Actions, split_side: None });
    h.apply(Command::CommitOmnibox { command: Box::new(toggle.command.clone()), alt: false });
    assert!(!h.ui().window.sidebar_visible);
    assert!(h.ui().command_bar.is_none());
    // Split and pinned-state availability.
    let t = h.open("https://t.com/");
    h.apply(Command::SplitOpenInput { text: "u.com".into(), side: SplitSide::Right });
    let all = h.store.omnibox_actions();
    assert!(all.iter().any(|a| a.title == "Separate All Tabs"));
    assert!(all.iter().any(|a| a.title == "Remove Pane from Split"));
    assert!(all.iter().all(|a| a.title != "Pin Tab"), "panes can't be pinned");
    h.apply(Command::SeparateAll { id: h.active().unwrap() });
    h.apply(Command::ActivateItem { id: t });
    h.apply(Command::TogglePin { id: Some(t) });
    h.commit(t, "https://t.com/elsewhere", "Elsewhere");
    let all = h.store.omnibox_actions();
    for title in ["Unpin Tab", "Reset Tab to Pinned URL", "Replace Pinned URL with Current", "Add to Favorites"] {
        assert!(all.iter().any(|a| a.title == title), "{title}");
    }
    let _ = f.gitlab_closed;
}

/// The DevTools actions (FINAL PLAN §3): "Developer Tools" is always there for a live page, and
/// "Undock DevTools" only while a **docked** DevTools is there to undock.
#[test]
fn devtools_actions_follow_the_dock() {
    let mut h = Harness::new();
    let tab = h.open("https://a.com/");
    let titles = |h: &Harness| h.store.omnibox_actions().into_iter().map(|a| a.title).collect::<Vec<_>>();
    assert!(titles(&h).contains(&"Developer Tools".to_string()));
    assert!(!titles(&h).contains(&"Undock DevTools".to_string()), "nothing to undock yet");

    h.apply(Command::ToggleDevTools);
    let open = titles(&h);
    assert!(open.contains(&"Close Developer Tools".to_string()), "{open:?}");
    assert!(open.contains(&"Undock DevTools".to_string()), "{open:?}");

    h.apply(Command::UndockDevTools);
    let undocked = titles(&h);
    assert!(undocked.contains(&"Close Developer Tools".to_string()), "{undocked:?}");
    assert!(!undocked.contains(&"Undock DevTools".to_string()), "already undocked: {undocked:?}");

    h.apply(Command::ToggleDevTools);
    assert!(titles(&h).contains(&"Developer Tools".to_string()));
    let _ = tab;
}

#[test]
fn history_rows_collapse_fragments_and_pinned_homes() {
    let mut h = Harness::new();
    let a = h.open("https://guide.example.com/intro");
    h.commit(a, "https://guide.example.com/intro", "Guide intro");
    h.commit(a, "https://guide.example.com/intro#setup", "Guide intro");
    h.commit(a, "https://guide.example.com/intro#usage", "Guide intro");
    h.apply(Command::CloseItem { id: Some(a) });
    let p = h.open_pinned("https://mail.example.com/inbox");
    h.commit(p, "https://mail.example.com/inbox", "Mail inbox");
    h.commit(p, "https://mail.example.com/message/1", "Mail message");
    h.open("https://other.com/");
    let r = h.store.omnibox(&req("guide intro", CommandBarMode::NewTab), h.now);
    check_shape(&r);
    assert_eq!(r.results.iter().filter(|x| x.group == ResultGroup::History || (x.group == ResultGroup::TopHit && x.key.starts_with("history:"))).count(), 1);
    let r = h.store.omnibox(&req("mail", CommandBarMode::NewTab), h.now);
    check_shape(&r);
    assert!(r.results.iter().all(|x| !x.key.starts_with("history:https://mail.example.com/inbox")), "the pinned tab stands for its home URL");
    assert!(r.results.iter().any(|x| x.key == format!("tab:{p}") || x.command == Command::ActivateItem { id: p }));
}

#[test]
fn unload_and_mute_actions_follow_availability_rules() {
    let mut h = Harness::new();
    let a = h.open("https://a.com/");
    let titles = |h: &Harness| h.store.omnibox_actions().into_iter().map(|x| x.title).collect::<Vec<_>>();
    let has_title = |h: &Harness, t: &str| titles(h).iter().any(|x| x == t);
    // Actions target the focused tab, which is active: never offered for unloading. A silent,
    // unmuted tab has no mute toggle.
    assert!(!has_title(&h, "Unload Tab"));
    assert!(!has_title(&h, "Mute Tab") && !has_title(&h, "Unmute Tab"));
    h.apply(Command::TabAudioChanged { tab: a, audible: true });
    assert!(has_title(&h, "Mute Tab"));
    let r = h.store.omnibox(&req(">mute", CommandBarMode::NewTab), h.now);
    assert_eq!(r.results[0].command, Command::ToggleMute { id: None });
    h.apply(Command::ToggleMute { id: None });
    h.apply(Command::TabAudioChanged { tab: a, audible: false });
    assert!(has_title(&h, "Unmute Tab"), "a muted tab can always be unmuted");
    h.apply(Command::ToggleMute { id: None });
    assert!(!has_title(&h, "Mute Tab") && !has_title(&h, "Unmute Tab"));
    assert!(!has_title(&h, "Unload Tab"));
}

fn with_suggestions(text: &str, mode: CommandBarMode, suggestions: &[&str]) -> OmniboxRequest {
    OmniboxRequest { suggestions: suggestions.iter().map(|s| s.to_string()).collect(), ..req(text, mode) }
}

#[test]
fn suggestions_complete_inline_and_search_on_enter() {
    let f = fixture();
    let s = &f.h.store;
    let now = f.h.now;
    let google = |q: &str| format!("https://www.google.com/search?q={}", omnibox::encode_uri_component(q));
    let sugg_titles = |r: &OmniboxResponse| r.results.iter().filter(|x| x.group == ResultGroup::Suggestions).map(|x| x.title.clone()).collect::<Vec<_>>();

    // Whitespace inside the typed text; the first suggestion extending it wins.
    let r = s.omnibox(&with_suggestions("rust pro", CommandBarMode::NewTab, &["rust", "rust programming language", "rust proof"]), now);
    check_shape(&r);
    assert_eq!(r.inline_completion.as_deref(), Some("rust programming language"));
    let go = &r.results[0];
    assert_eq!((go.key.as_str(), go.group, go.title.as_str(), go.subtitle.as_deref()), ("search", ResultGroup::Go, "rust programming language", Some("Search Google")));
    assert_eq!(go.hint.as_deref(), Some("↵"));
    assert_eq!(go.command, Command::OpenUrl { url: google("rust programming language"), target: OpenTarget::NewTab, opener: None });
    assert_eq!(go.alt_command, Some(Command::OpenUrl { url: google("rust programming language"), target: OpenTarget::BackgroundTab, opener: None }));
    assert!(r.results.iter().all(|x| x.group != ResultGroup::TopHit), "the completion is the Enter row");
    assert_eq!(sugg_titles(&r), ["rust", "rust proof"], "the completed suggestion is not repeated; others still show");

    // Per mode.
    let r = s.omnibox(&with_suggestions("rust pro", CommandBarMode::EditUrl, &["rust programming language"]), now);
    assert_eq!(r.results[0].command, Command::OpenUrl { url: google("rust programming language"), target: OpenTarget::CurrentTab, opener: None });
    let mut split = with_suggestions("rust pro", CommandBarMode::Split, &["rust programming language"]);
    split.split_side = Some(SplitSide::Left);
    let r = s.omnibox(&split, now);
    check_shape(&r);
    assert_eq!(r.inline_completion.as_deref(), Some("rust programming language"));
    assert_eq!(r.results[0].command, Command::SplitOpenInput { text: "?rust programming language".into(), side: SplitSide::Left });
    assert_eq!(r.results[0].alt_command, Some(Command::OpenUrl { url: google("rust programming language"), target: OpenTarget::BackgroundTab, opener: None }));
    // Committing the Enter row really searches the completion.
    let mut h = Harness::new();
    let r = h.store.omnibox(&with_suggestions("rust tu", CommandBarMode::NewTab, &["rust tutorial"]), h.now);
    h.apply(Command::CommitOmnibox { command: Box::new(r.results[0].command.clone()), alt: false });
    assert_eq!(h.tab(h.focused().unwrap()).url, "https://www.google.com/search?q=rust%20tutorial");

    // Typed characters keep the user's case (Unicode-aware); multi-byte text; a trailing space.
    let completion = |text: &str, suggestions: &[&str]| s.omnibox(&with_suggestions(text, CommandBarMode::NewTab, suggestions), now).inline_completion;
    assert_eq!(completion("RUST Pro", &["rust programming"]).as_deref(), Some("RUST Programming"));
    let r = s.omnibox(&with_suggestions("RUST Pro", CommandBarMode::NewTab, &["rust programming"]), now);
    assert_eq!(r.results[0].command, Command::OpenUrl { url: google("RUST Programming"), target: OpenTarget::NewTab, opener: None });
    assert_eq!(completion("러스", &["러스트 게임", "러스트"]).as_deref(), Some("러스트 게임"));
    assert_eq!(completion("ÉCO", &["école normale"]).as_deref(), Some("ÉCOle normale"));
    assert_eq!(completion("straße", &["STRASSE"]), None, "no multi-char case folding");
    assert_eq!(completion("rust ", &["rust book"]).as_deref(), Some("rust book"));
    assert_eq!(completion("rust", &["  rust book  "]).as_deref(), Some("rust book"), "suggestions are trimmed");
    // Only longer suggestions that start with the text; URL-like suggestions are skipped.
    assert_eq!(completion("rust", &["Rust", "the rust book"]), None);
    assert_eq!(completion("nave", &["naver.com", "naver map"]).as_deref(), Some("naver map"));
    assert_eq!(completion("rust", &[]), None);

    // No completion: a history (host) completion exists, the user deleted, actions mode, '?',
    // URL-like text, leading whitespace.
    let r = s.omnibox(&with_suggestions("git", CommandBarMode::NewTab, &["github copilot"]), now);
    assert_eq!(r.inline_completion.as_deref(), Some("gitlab.com"), "history completion wins");
    assert_eq!(r.results[0].command, Command::OpenInput { text: "gitlab.com".into(), target: OpenTarget::NewTab });
    assert_eq!(sugg_titles(&r), ["github copilot"]);
    let mut deleted = with_suggestions("rust pro", CommandBarMode::NewTab, &["rust programming language"]);
    deleted.prevent_inline_autocomplete = true;
    let r = s.omnibox(&deleted, now);
    check_shape(&r);
    assert_eq!(r.inline_completion, None);
    assert_eq!(find(&r, "search").title, "rust pro");
    assert_eq!(find(&r, "search").command, Command::OpenInput { text: "rust pro".into(), target: OpenTarget::NewTab });
    assert_eq!(sugg_titles(&r), ["rust programming language"], "suggestion rows still show after a deletion");
    for request in [
        with_suggestions("rust", CommandBarMode::Actions, &["rust book"]),
        with_suggestions(">rust", CommandBarMode::NewTab, &[">rust book"]),
        with_suggestions("?rust", CommandBarMode::NewTab, &["rust book", "?rust book"]),
        with_suggestions("example.com/ru", CommandBarMode::NewTab, &["example.com/rust", "example.com/ru docs"]),
        with_suggestions("https://ru", CommandBarMode::NewTab, &["https://rust-lang.org"]),
        with_suggestions(" rust", CommandBarMode::NewTab, &["rust book", " rust book"]),
    ] {
        let r = s.omnibox(&request, now);
        assert_eq!(r.inline_completion, None, "{:?}", request.text);
        check_shape(&r);
    }
}
