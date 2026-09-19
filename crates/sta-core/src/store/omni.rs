//! Command bar results (arc_spec §6): what-you-typed row with inline completion, open tabs,
//! actions registry, spaces, history, remote suggestions and archive, ranked and grouped.

use super::*;
use crate::extensions::ExtensionAction;
use crate::omnibox::{
    classify, effective_engine_name, fold, fuzzy_folded, match_page_folded, search_url, Classified, OmniboxResult, ResultGroup, ResultIcon,
};
use crate::urls;

const MAX_RESULTS: usize = 12;
/// The extensions picker lists every extension (its list scrolls), up to a sane ceiling.
const MAX_EXTENSION_RESULTS: usize = 50;

struct Cand {
    score: f32,
    result: OmniboxResult,
}

/// One command bar action.
pub(super) struct Action {
    pub id: String,
    pub title: String,
    pub aliases: Vec<&'static str>,
    pub shortcut: Option<&'static str>,
    pub icon: ResultIcon,
    pub command: Command,
    /// Only listed in actions mode (per-space "Go to Space: X").
    pub actions_mode_only: bool,
}

fn glyph(name: &str) -> ResultIcon {
    ResultIcon::Glyph { name: name.to_string() }
}

pub(super) fn query(store: &Store, req: &OmniboxRequest, now: Millis) -> OmniboxResponse {
    let trimmed = req.text.trim();
    let side = req.split_side.unwrap_or(SplitSide::Right);
    let mut response = OmniboxResponse { text: req.text.clone(), seq: req.seq, inline_completion: None, results: Vec::new() };

    // ---------------------------------------------------------------- extensions mode (Ctrl+E)
    // `>` still switches to the actions list, like every other mode.
    if req.mode == CommandBarMode::Extensions && !trimmed.starts_with('>') {
        response.results = extensions_query(store, trimmed);
        return response;
    }

    // ---------------------------------------------------------------- actions mode
    if req.mode == CommandBarMode::Actions || trimmed.starts_with('>') {
        let q = trimmed.trim_start_matches('>').trim();
        let actions = registry(store);
        let cands: Vec<Cand> = if q.is_empty() {
            let mut all: Vec<Action> = actions;
            all.sort_by_key(|a| a.title.to_lowercase());
            all.into_iter().map(|a| Cand { score: 0.0, result: action_result(a, ResultGroup::Actions) }).collect()
        } else {
            let qf = fold(q);
            let mut c: Vec<Cand> = actions.into_iter().filter_map(|a| score_action(&a, q, &qf, 1.10).map(|s| Cand { score: s, result: action_result(a, ResultGroup::Actions) })).collect();
            sort_cands(&mut c);
            c
        };
        response.results = cands.into_iter().take(MAX_RESULTS).map(|c| c.result).collect();
        return response;
    }

    let focused = store.content_focused_tab();
    let peek = store.peek_tab();

    // ---------------------------------------------------------------- empty query
    if trimmed.is_empty() {
        let mut actions = registry(store);
        let mut take = |id: &str| actions.iter().position(|a| a.id == id).map(|pos| actions.remove(pos));
        if req.mode == CommandBarMode::EditUrl
            && let Some(a) = take("tab.copy_url")
        {
            response.results.push(action_result(a, ResultGroup::SuggestedActions));
        }
        let recent: Vec<Id> = store
            .state
            .window
            .mru
            .iter()
            .copied()
            .filter(|t| Some(*t) != focused && Some(*t) != peek && store.tab_item(*t).is_some() && store.section_of(*t).is_some())
            .take(6)
            .collect();
        for t in recent {
            if let Some(r) = tab_result(store, t, req.mode, focused, side, ResultGroup::RecentTabs) {
                response.results.push(r);
            }
        }
        if req.mode != CommandBarMode::Split {
            for id in ["space.new", "view.archive", "sidebar.toggle", "view.settings"] {
                if let Some(a) = take(id) {
                    response.results.push(action_result(a, ResultGroup::SuggestedActions));
                }
            }
        }
        response.results.truncate(MAX_RESULTS);
        return response;
    }

    // ---------------------------------------------------------------- what you typed
    let settings = &store.state.settings;
    let engine_label = || format!("Search {}", effective_engine_name(settings.search_engine, &settings.custom_search_url));
    let inline = if !req.prevent_inline_autocomplete && req.text == trimmed && !trimmed.contains(char::is_whitespace) && !trimmed.starts_with('?') {
        inline_completion(store, trimmed)
    } else {
        None
    };
    // Without a history completion, the first remote suggestion extending the typed text.
    let suggestion_inline = if inline.is_none() && !req.prevent_inline_autocomplete && !trimmed.starts_with('?') && matches!(classify(trimmed), Classified::Search(_)) {
        suggestion_completion(&req.text, &req.suggestions)
    } else {
        None
    };
    response.inline_completion = inline.as_ref().map(|i| i.text.clone()).or_else(|| suggestion_inline.clone());
    let go = if let Some(completed) = &suggestion_inline {
        // Enter searches the completed suggestion, like its Suggestions row would.
        let url = search_url(settings.search_engine, &settings.custom_search_url, completed);
        OmniboxResult {
            key: "search".into(),
            group: ResultGroup::Go,
            title: completed.clone(),
            subtitle: Some(engine_label()),
            icon: glyph("search"),
            hint: Some("↵".into()),
            command: search_command(req.mode, completed, &url, side),
            alt_command: Some(Command::OpenUrl { url, target: OpenTarget::BackgroundTab, opener: None }),
        }
    } else {
        let wyt_text = inline.as_ref().map_or_else(|| trimmed.to_string(), |i| i.text.clone());
        // What Enter opens: the typed text, or the completion with its history entry's scheme.
        let commit_text = inline.as_ref().map_or_else(|| wyt_text.clone(), InlineCompletion::commit_text);
        let (go_key, go_title, go_subtitle, go_icon) = match classify(&commit_text) {
            Classified::Url(u) => ("go", wyt_text.clone(), (u != wyt_text).then_some(u), glyph("globe")),
            Classified::Search(q) => ("search", q, Some(engine_label()), glyph("search")),
        };
        let go_command = match req.mode {
            CommandBarMode::EditUrl => Command::OpenInput { text: commit_text.clone(), target: OpenTarget::CurrentTab },
            CommandBarMode::Split => Command::SplitOpenInput { text: commit_text.clone(), side },
            _ => Command::OpenInput { text: commit_text.clone(), target: OpenTarget::NewTab },
        };
        OmniboxResult {
            key: go_key.into(),
            group: ResultGroup::Go,
            title: go_title,
            subtitle: go_subtitle,
            icon: go_icon,
            hint: Some("↵".into()),
            command: go_command,
            alt_command: Some(Command::OpenInput { text: commit_text, target: OpenTarget::BackgroundTab }),
        }
    };
    let wyt_score = match (&response.inline_completion, &classify(trimmed)) {
        (Some(_), _) => f32::INFINITY,
        (None, Classified::Url(_)) if urls::scheme(trimmed).is_some() && trimmed.contains("://") => 1.3,
        (None, Classified::Url(_)) => 1.0,
        (None, Classified::Search(_)) => 0.0,
    };

    let qf = fold(trimmed);
    let active_space = store.active_space_id();

    // ---------------------------------------------------------------- tabs
    let mut open_urls: BTreeSet<String> = BTreeSet::new();
    let mut tabs: Vec<Cand> = Vec::new();
    let mut all_tabs: Vec<Id> = store.state.favorites.clone();
    for s in &store.state.spaces {
        all_tabs.extend(s.pinned.iter().flat_map(|i| store.tabs_under(*i)));
        all_tabs.extend(s.today.iter().flat_map(|i| store.tabs_under(*i)));
    }
    for t in all_tabs {
        let Some(tab) = store.tab_item(t) else { continue };
        open_urls.insert(urls::dedupe_key(&tab.url));
        // A pinned tab stands for its home URL too.
        open_urls.extend(tab.pinned_url.as_deref().map(urls::dedupe_key));
        if Some(t) == focused {
            continue;
        }
        let title = urls::display_title(tab.custom_title.as_deref(), &tab.title, &tab.url);
        let Some(fz) = match_page_folded(&qf, &title, &tab.url) else { continue };
        let favorite = store.state.favorites.contains(&t);
        let space = store.space_of(t);
        let mut score = fz;
        if favorite || space == Some(active_space) {
            score += 0.15;
        }
        if let Some(i) = store.state.window.mru.iter().position(|m| *m == t).filter(|i| *i >= 1) {
            score += 0.10 * 0.8f32.powi(i as i32 - 1);
        }
        if favorite {
            score += 0.05;
        }
        if let Some(r) = tab_result(store, t, req.mode, focused, side, ResultGroup::Tabs) {
            tabs.push(Cand { score, result: r });
        }
    }

    // ---------------------------------------------------------------- actions
    let mut actions: Vec<Cand> = registry(store)
        .into_iter()
        .filter(|a| !a.actions_mode_only)
        .filter_map(|a| score_action(&a, trimmed, &qf, 0.95).map(|s| Cand { score: s, result: action_result(a, ResultGroup::Actions) }))
        .collect();
    if req.mode == CommandBarMode::Split {
        actions.clear();
    }

    // ---------------------------------------------------------------- spaces
    let mut spaces: Vec<Cand> = Vec::new();
    if req.mode != CommandBarMode::Split {
        for s in &store.state.spaces {
            if s.id == active_space {
                continue;
            }
            if let Some(fz) = fuzzy_folded(&qf, &fold(&s.name)) {
                spaces.push(Cand {
                    score: fz * 0.90,
                    result: OmniboxResult {
                        key: format!("space:{}", s.id),
                        group: ResultGroup::Spaces,
                        title: s.name.clone(),
                        subtitle: Some("Space".into()),
                        icon: ResultIcon::Emoji { emoji: s.icon.clone() },
                        hint: Some("Go to Space".into()),
                        command: Command::SwitchSpace { id: s.id },
                        alt_command: None,
                    },
                });
            }
        }
    }

    // ---------------------------------------------------------------- history
    let mut history: Vec<Cand> = Vec::new();
    {
        let matched: Vec<(f32, i32, &crate::history::HistoryUrl)> = store
            .history
            .urls
            .iter()
            .filter(|u| !open_urls.contains(&urls::dedupe_key(&u.url)))
            .filter_map(|u| match_page_folded(&qf, &u.title, &u.url).map(|fz| (fz, crate::history::frecency(u, now), u)))
            .collect();
        let max_f = matched.iter().map(|m| m.1).max().unwrap_or(0).max(1) as f32;
        let mut scored: Vec<(f32, &crate::history::HistoryUrl)> =
            matched.into_iter().map(|(fz, f, u)| (fz * 0.85 + 0.25 * (f as f32 / max_f), u)).collect();
        scored.sort_by(|a, b| {
            b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal).then_with(|| b.1.last_visit_at.cmp(&a.1.last_visit_at))
        });
        // One row per page: URLs differing only by fragment / trailing slash collapse.
        let mut seen_pages: BTreeSet<String> = BTreeSet::new();
        for (score, u) in scored {
            if !seen_pages.insert(urls::dedupe_key(&u.url)) {
                continue;
            }
            if seen_pages.len() > 64 {
                break;
            }
            history.push(Cand {
                score,
                result: OmniboxResult {
                    key: format!("history:{}", u.url),
                    group: ResultGroup::History,
                    title: urls::display_title(None, &u.title, &u.url),
                    subtitle: Some(urls::display_host(&u.url)),
                    icon: ResultIcon::Favicon { url: None, host: urls::display_host(&u.url) },
                    hint: Some(relative_time(u.last_visit_at, now)),
                    command: url_command(req.mode, &u.url, side, settings),
                    alt_command: Some(Command::OpenUrl { url: u.url.clone(), target: OpenTarget::BackgroundTab, opener: None }),
                },
            });
        }
    }

    // ---------------------------------------------------------------- suggestions
    let mut suggestions: Vec<Cand> = Vec::new();
    // The completed suggestion is already the Enter row.
    let mut seen: Vec<String> = std::iter::once(trimmed).chain(suggestion_inline.as_deref()).map(str::to_lowercase).collect();
    for s in req.suggestions.iter().map(|s| s.trim()).filter(|s| !s.is_empty()) {
        if seen.contains(&s.to_lowercase()) {
            continue;
        }
        seen.push(s.to_lowercase());
        let i = suggestions.len();
        let url = search_url(settings.search_engine, &settings.custom_search_url, s);
        let command = search_command(req.mode, s, &url, side);
        suggestions.push(Cand {
            score: 0.60 * (1.0 - 0.05 * i as f32),
            result: OmniboxResult {
                key: format!("suggest:{s}"),
                group: ResultGroup::Suggestions,
                title: s.to_string(),
                subtitle: None,
                icon: glyph("search"),
                hint: Some("Search".into()),
                command,
                alt_command: Some(Command::OpenUrl { url, target: OpenTarget::BackgroundTab, opener: None }),
            },
        });
    }

    // ---------------------------------------------------------------- archive
    let mut archive: Vec<Cand> = Vec::new();
    if req.mode != CommandBarMode::Split {
        for e in &store.state.archive {
            let title = urls::display_title(e.custom_title.as_deref(), &e.title, &e.url);
            if let Some(fz) = match_page_folded(&qf, &title, &e.url) {
                archive.push(Cand {
                    score: fz * 0.55,
                    result: OmniboxResult {
                        key: format!("archive:{}", e.id),
                        group: ResultGroup::Archive,
                        title,
                        subtitle: Some(format!("{} · archived {}", urls::display_host(&e.url), relative_time(e.archived_at, now))),
                        icon: ResultIcon::Favicon { url: e.favicon.clone(), host: urls::display_host(&e.url) },
                        hint: Some("Restore".into()),
                        command: Command::RestoreArchived { id: e.id, whole_group: false },
                        alt_command: None,
                    },
                });
            }
        }
    }

    let mut groups = [(tabs, 4usize), (actions, 3), (spaces, 2), (history, 4), (suggestions, 4), (archive, 2)];
    for (g, cap) in groups.iter_mut() {
        sort_cands(g);
        g.truncate(*cap);
    }
    // Top hit: the best candidate overall, if it clearly beats what-you-typed.
    let mut top: Option<OmniboxResult> = None;
    if response.inline_completion.is_none() {
        let best = groups
            .iter()
            .enumerate()
            .filter_map(|(gi, (g, _))| g.first().map(|c| (gi, c.score)))
            .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
        if let Some((gi, score)) = best
            && score >= 0.85 * wyt_score.max(1.0)
        {
            let mut c = groups[gi].0.remove(0);
            c.result.group = ResultGroup::TopHit;
            top = Some(c.result);
        }
    }
    response.results.extend(top);
    response.results.push(go);
    for (g, _) in groups {
        response.results.extend(g.into_iter().map(|c| c.result));
    }
    response.results.truncate(MAX_RESULTS);
    response
}

// --------------------------------------------------------------------------- extensions (Ctrl+E)

/// One picker row for an installed extension. The icon is served same-origin out of the
/// extension's own directory (UX7), so the card page never loads `chrome-extension://`.
fn extension_result(e: &crate::extensions::ExtensionInfo, group: ResultGroup) -> OmniboxResult {
    use crate::extensions::{STATUS_NEEDS_OK, STATUS_NEEDS_OK_SHORT, STATUS_NO_ACTION, STATUS_NO_ACTION_HINT};
    let name = e.display_name().to_string();
    // The picker has group headings; Settings does not. So a row under "Needs your OK" drops the
    // "· needs your OK" tail the heading already carries (UXV-6), and the one row whose Enter *leaves
    // sta* says so before it is pressed (UXV-5).
    let subtitle = match e.status() {
        Some(STATUS_NEEDS_OK) if group == ResultGroup::NeedsOk => Some(STATUS_NEEDS_OK_SHORT),
        Some(STATUS_NO_ACTION) => Some(STATUS_NO_ACTION_HINT),
        other => other,
    };
    OmniboxResult {
        key: format!("ext:{}", e.id),
        group,
        title: name.clone(),
        subtitle: subtitle.map(str::to_string),
        icon: ResultIcon::Favicon { url: Some(crate::extensions::icon_url("command", &e.id, 32)), host: name },
        // What Enter does depends on the extension, and core decides it (`store/extensions.rs`);
        // the hint only says that Enter does *something*.
        hint: Some("↵".into()),
        command: Command::RunExtension { id: e.id.clone(), action: ExtensionAction::Primary },
        // Alt+Enter is always "open its options", when it has any.
        alt_command: e.options.is_some().then(|| Command::RunExtension { id: e.id.clone(), action: ExtensionAction::Options }),
    }
}

/// The "More" rows, which are always reachable — also with no extensions installed at all.
fn extension_more(store: &Store) -> Vec<(OmniboxResult, &'static str)> {
    let manage = Command::OpenUrl { url: super::extensions::settings_extensions_url(None), target: OpenTarget::NewTab, opener: None };
    let store_url = Command::OpenUrl { url: "https://chromewebstore.google.com/".into(), target: OpenTarget::NewTab, opener: None };
    let _ = store;
    vec![
        (
            OmniboxResult {
                key: "ext.manage".into(),
                group: ResultGroup::More,
                title: "Manage Extensions".into(),
                subtitle: None,
                icon: glyph("settings"),
                hint: Some("↵".into()),
                command: manage,
                alt_command: None,
            },
            "settings extensions manage",
        ),
        (
            OmniboxResult {
                key: "ext.get".into(),
                group: ResultGroup::More,
                title: "Get Extensions".into(),
                subtitle: Some("Chrome Web Store".into()),
                icon: glyph("plus"),
                hint: Some("↵".into()),
                command: store_url,
                alt_command: None,
            },
            "web store install add",
        ),
    ]
}

/// The Ctrl+E picker. An empty query lists everything by group; a query ranks by name, with one
/// retry through the Hangul keyboard so a query typed with the IME on still finds its extension.
pub(super) fn extensions_query(store: &Store, query: &str) -> Vec<OmniboxResult> {
    let groups = [
        (crate::extensions::ExtensionGroup::Extensions, ResultGroup::Extensions),
        (crate::extensions::ExtensionGroup::NeedsOk, ResultGroup::NeedsOk),
        (crate::extensions::ExtensionGroup::Off, ResultGroup::ExtensionsOff),
    ];
    let mut out: Vec<OmniboxResult> = Vec::new();
    if query.is_empty() {
        for (group, result_group) in groups {
            for e in store.extensions().iter().filter(|e| crate::extensions::ExtensionGroup::of(e) == group) {
                out.push(extension_result(e, result_group));
            }
        }
        out.truncate(MAX_EXTENSION_RESULTS);
        out.extend(extension_more(store).into_iter().map(|(r, _)| r));
        return out;
    }
    let retry = crate::omnibox::jamo_to_qwerty(query);
    let queries: Vec<Vec<char>> = std::iter::once(fold(query)).chain(retry.as_deref().map(fold)).collect();
    let score = |text: &str| -> Option<f32> {
        let folded = fold(text);
        // The same fallback in the other direction: the letters a Korean name is *typed* with, so
        // `gksrmf` finds "한글 확장" for a user who forgot to switch the IME on (UXV-7). Both retries
        // rank below a direct hit, so a real match always wins.
        let typed = crate::omnibox::jamo_to_qwerty(text).map(|t| fold(&t));
        let texts: Vec<(&[char], f32)> = std::iter::once((folded.as_slice(), 1.0)).chain(typed.as_deref().map(|t| (t, 0.9))).collect();
        queries
            .iter()
            .enumerate()
            .flat_map(|(i, q)| texts.iter().filter_map(move |(t, tw)| fuzzy_folded(q, t).map(|s| if i == 0 { s * tw } else { s * 0.9 * tw })))
            .fold(None, |best: Option<f32>, s| Some(best.map_or(s, |b| b.max(s))))
    };
    for (group, result_group) in groups {
        let mut cands: Vec<Cand> = store
            .extensions()
            .iter()
            .filter(|e| crate::extensions::ExtensionGroup::of(e) == group)
            .filter_map(|e| score(e.display_name()).map(|s| Cand { score: s, result: extension_result(e, result_group) }))
            .collect();
        sort_cands(&mut cands);
        out.extend(cands.into_iter().map(|c| c.result));
    }
    out.truncate(MAX_EXTENSION_RESULTS);
    for (result, aliases) in extension_more(store) {
        if score(&result.title).is_some() || score(aliases).is_some() {
            out.push(result);
        }
    }
    out
}

pub(super) fn all_actions(store: &Store) -> Vec<OmniboxResult> {
    let mut all = registry(store);
    all.sort_by_key(|a| a.title.to_lowercase());
    all.into_iter().map(|a| action_result(a, ResultGroup::Actions)).collect()
}

fn sort_cands(c: &mut [Cand]) {
    c.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal).then_with(|| a.result.key.cmp(&b.result.key)));
}

/// Command that searches `query` (whose search URL is `url`) in the bar's mode: split panes get a
/// forced search (`?query`), Edit URL navigates the current tab, anything else opens a tab.
fn search_command(mode: CommandBarMode, query: &str, url: &str, side: SplitSide) -> Command {
    match mode {
        CommandBarMode::Split => Command::SplitOpenInput { text: format!("?{query}"), side },
        CommandBarMode::EditUrl => Command::OpenUrl { url: url.to_string(), target: OpenTarget::CurrentTab, opener: None },
        _ => Command::OpenUrl { url: url.to_string(), target: OpenTarget::NewTab, opener: None },
    }
}

/// Inline completion from remote suggestions: the first suggestion that starts with the typed text
/// (case-insensitively, Unicode-aware) and is longer, as the typed text (the user's case) plus the
/// suggestion's remainder. Whitespace inside the typed text is fine (`rust pro` →
/// `rust programming`). URL-like suggestions (`naver.com`) are skipped: the completed text would
/// read as an address while Enter searches it.
fn suggestion_completion(typed: &str, suggestions: &[String]) -> Option<String> {
    if typed.trim().is_empty() {
        return None;
    }
    suggestions.iter().map(|s| s.trim()).find_map(|s| {
        let end = crate::omnibox::case_insensitive_prefix(s, typed)?;
        (end < s.len() && matches!(classify(s), Classified::Search(_))).then(|| format!("{typed}{}", &s[end..]))
    })
}

fn url_command(mode: CommandBarMode, url: &str, side: SplitSide, _settings: &Settings) -> Command {
    match mode {
        CommandBarMode::EditUrl => Command::OpenUrl { url: url.to_string(), target: OpenTarget::CurrentTab, opener: None },
        CommandBarMode::Split => Command::SplitOpenInput { text: url.to_string(), side },
        _ => Command::OpenUrl { url: url.to_string(), target: OpenTarget::NewTab, opener: None },
    }
}

fn tab_result(store: &Store, t: Id, mode: CommandBarMode, focused: Option<Id>, side: SplitSide, group: ResultGroup) -> Option<OmniboxResult> {
    let tab = store.tab_item(t)?;
    let host = urls::display_host(&tab.url);
    let place = if store.state.favorites.contains(&t) {
        "Favorites".to_string()
    } else {
        store.space_of(t).and_then(|s| store.space(s)).map(|s| format!("{} {}", s.icon, s.name)).unwrap_or_default()
    };
    let command = match (mode, focused) {
        (CommandBarMode::Split, Some(f)) if f != t => Command::SplitWith { tab: t, with: f, side },
        _ => Command::ActivateItem { id: t },
    };
    Some(OmniboxResult {
        key: format!("tab:{t}"),
        group,
        title: urls::display_title(tab.custom_title.as_deref(), &tab.title, &tab.url),
        subtitle: Some(if place.is_empty() { host.clone() } else { format!("{host} · {place}") }),
        icon: ResultIcon::Favicon { url: tab.favicon.clone(), host },
        hint: Some(if mode == CommandBarMode::Split { "Split".into() } else if store.is_loaded(t) { "Switch to Tab".into() } else { "Open".into() }),
        command,
        alt_command: None,
    })
}

fn action_result(a: Action, group: ResultGroup) -> OmniboxResult {
    OmniboxResult {
        key: format!("action:{}", a.id),
        group,
        title: a.title,
        subtitle: None,
        icon: a.icon,
        hint: a.shortcut.map(str::to_string),
        command: a.command,
        alt_command: None,
    }
}

fn score_action(a: &Action, query: &str, qf: &[char], weight: f32) -> Option<f32> {
    let mut best = fuzzy_folded(qf, &fold(&a.title));
    for alias in &a.aliases {
        if let Some(s) = fuzzy_folded(qf, &fold(alias)) {
            best = Some(best.map_or(s, |b| b.max(s)));
        }
    }
    let mut score = best? * weight;
    let tokens: Vec<String> = query.split_whitespace().map(|t| t.to_lowercase()).collect();
    if a.aliases.iter().any(|al| tokens.iter().any(|t| al.eq_ignore_ascii_case(t))) {
        score += 0.10;
    }
    Some(score)
}

/// An inline completion: the completed text and the scheme of the history entry it came from.
struct InlineCompletion {
    text: String,
    scheme: String,
}

impl InlineCompletion {
    /// The text to open. The completed host resolves like typed input (a dotted host becomes
    /// `https://`, `localhost` and IPs `http://`, an unknown TLD a search), so when that is not a
    /// URL of the entry's own scheme, the entry's scheme is spelled out (`http://intranet.test:8080`).
    fn commit_text(&self) -> String {
        match classify(&self.text) {
            Classified::Url(u) if urls::scheme(&u).as_deref() == Some(self.scheme.as_str()) => self.text.clone(),
            _ => format!("{}://{}", self.scheme, self.text),
        }
    }
}

/// Host (or host + path when `/` was typed) completion from http(s) history entries that were
/// typed or have frecency ≥ 200 (arc_spec §6.3). A non-default port is part of the host
/// (`127` → `127.0.0.1:8931`), and the entry's scheme is kept for committing, so the completed
/// text opens the same origin.
fn inline_completion(store: &Store, typed: &str) -> Option<InlineCompletion> {
    let ql = typed.to_lowercase();
    if ql.contains("://") || ql.is_empty() {
        return None;
    }
    let with_path = ql.contains('/');
    let mut best: Option<(i32, usize, String, String)> = None;
    for u in &store.history.urls {
        if u.typed_count < 1 && u.frecency < 200 {
            continue;
        }
        let Some(scheme) = urls::scheme(&u.url).filter(|s| s == "http" || s == "https") else { continue };
        let Some(host) = urls::host(&u.url) else { continue };
        let host = match url::Url::parse(&u.url).ok().and_then(|p| p.port()) {
            Some(port) => format!("{host}:{port}"),
            None => host,
        };
        let bare = host.strip_prefix("www.").unwrap_or(&host).to_string();
        let path = urls::path_part(&u.url);
        let candidates: Vec<String> = if with_path {
            vec![format!("{bare}{path}"), format!("{host}{path}")]
        } else {
            vec![bare.clone(), host.clone()]
        };
        for c in candidates {
            if c.len() > ql.len() && c.starts_with(&ql) {
                let key = (u.frecency, usize::MAX - c.len());
                if best.as_ref().is_none_or(|b| (b.0, b.1) < key) {
                    best = Some((key.0, key.1, c, scheme.clone()));
                }
            }
        }
    }
    best.map(|(_, _, c, scheme)| InlineCompletion { text: format!("{typed}{}", &c[ql.len()..]), scheme })
}

/// "just now", "5m ago", "3h ago", "2d ago", "3w ago", "4mo ago", "2y ago".
pub(super) fn relative_time(then: Millis, now: Millis) -> String {
    let secs = (now.saturating_sub(then) / 1000).max(0);
    let (m, h, d) = (60, 3600, 86_400);
    match secs {
        s if s < m => "just now".into(),
        s if s < h => format!("{}m ago", s / m),
        s if s < d => format!("{}h ago", s / h),
        s if s < 7 * d => format!("{}d ago", s / d),
        s if s < 30 * d => format!("{}w ago", s / (7 * d)),
        s if s < 365 * d => format!("{}mo ago", s / (30 * d)),
        s => format!("{}y ago", s / (365 * d)),
    }
}

/// The action registry (arc_spec §6.6), filtered by availability for the current state.
pub(super) fn registry(store: &Store) -> Vec<Action> {
    let mut out: Vec<Action> = Vec::new();
    let mut add = |id: &str, title: &str, aliases: &[&'static str], shortcut: Option<&'static str>, icon: ResultIcon, command: Command| {
        out.push(Action { id: id.into(), title: title.into(), aliases: aliases.to_vec(), shortcut, icon, command, actions_mode_only: false });
    };
    let focused = store.focused_tab();
    let item_tab = store.content_focused_tab().filter(|_| store.peek_tab().is_none());
    let live = focused.is_some_and(|t| store.is_live(t));
    let section = item_tab.and_then(|t| store.tab_section(t));
    let in_pane = item_tab.is_some_and(|t| matches!(store.parent_of(t), Some((Parent::Split(_), _))));
    let tab = focused.and_then(|t| store.tab(t));
    let url = tab.map(|t| t.url.clone()).unwrap_or_default();
    let navigated = tab.is_some_and(|t| t.pinned_url.as_deref().is_some_and(|p| urls::differs_from_pinned(&t.url, p)));
    let active_item = store.active_item();
    let active_split = active_item.and_then(|a| store.split_item(a));
    let spaces = &store.state.spaces;
    let active_space = store.active_space_id();
    let space_index = spaces.iter().position(|s| s.id == active_space).unwrap_or(0);
    let today_nonempty = store.space(active_space).is_some_and(|s| !s.today.is_empty());
    let r = focused.and_then(|t| store.rt.tabs.get(&t)).filter(|_| live);

    add("tab.new", "New Tab", &["open tab", "new"], Some("Ctrl+T"), glyph("plus"), Command::OpenCommandBar { mode: CommandBarMode::NewTab, split_side: None });
    if focused.is_some() {
        add("tab.close", "Close Tab", &["archive tab", "close"], Some("Ctrl+W"), glyph("close"), Command::CloseItem { id: None });
        add("tab.copy_url", "Copy URL", &["copy link", "share"], Some("Ctrl+Shift+C"), glyph("copy"), Command::CopyUrl { id: None, markdown: false });
        add("tab.copy_url_md", "Copy URL as Markdown", &["markdown link"], Some("Ctrl+Shift+Alt+C"), glyph("copy"), Command::CopyUrl { id: None, markdown: true });
        add("tab.duplicate", "Duplicate Tab", &["clone", "copy tab"], None, glyph("copy"), Command::DuplicateTab { id: None });
    }
    if store.can_reopen() {
        add("tab.reopen", "Reopen Closed Tab", &["undo close", "restore"], Some("Ctrl+Shift+T"), glyph("restore"), Command::ReopenClosed);
    }
    if let (Some(t), Some(sec)) = (item_tab, section) {
        if !in_pane {
            match sec {
                Section::Today => add("tab.pin_toggle", "Pin Tab", &["pin"], Some("Ctrl+D"), glyph("pin"), Command::TogglePin { id: Some(t) }),
                Section::Pinned => add("tab.pin_toggle", "Unpin Tab", &["unpin"], Some("Ctrl+D"), glyph("pin"), Command::TogglePin { id: Some(t) }),
                Section::Favorites => {}
            }
            match sec {
                Section::Favorites => add("tab.favorite_toggle", "Remove from Favorites", &["unfavorite"], None, glyph("star"), Command::RemoveFavorite { id: t }),
                _ if store.state.favorites.len() < MAX_FAVORITES => {
                    add("tab.favorite_toggle", "Add to Favorites", &["favorite", "star"], None, glyph("star"), Command::AddFavorite { id: Some(t) })
                }
                _ => {}
            }
        }
        if matches!(sec, Section::Pinned | Section::Favorites) && navigated {
            add("tab.reset_pinned", "Reset Tab to Pinned URL", &["back to pinned url", "reset"], None, glyph("restore"), Command::ResetToPinned { id: t });
            add("tab.replace_pinned_url", "Replace Pinned URL with Current", &["update pinned"], None, glyph("pin"), Command::ReplacePinnedUrl { id: t });
        }
        // F2 is not an accelerator: the docked sidebar handles it in its own keydown path, so it
        // only reaches the rename while that sidebar has keyboard focus. `ui/sidebar/menus.js`
        // already hides the hint when the sidebar is hidden or floating; this row used to print it
        // unconditionally, which promised a key that goes to the page instead (DOC-11).
        let rename_hint = store.state.window.sidebar_visible.then_some("F2");
        add("tab.rename", "Rename Tab", &["title"], rename_hint, glyph("edit"), Command::OpenSidebarPanel { panel: SidebarPanel::RenameItem { id: t } });
        // arc_spec §6.6: only loaded tabs that aren't active. The registry targets the focused
        // tab, which is always active, so this stays hidden until actions can target other tabs.
        if store.is_loaded(t) && store.content_focused_tab() != Some(t) {
            add("tab.unload", "Unload Tab", &["free memory", "discard"], None, glyph("pause"), Command::UnloadTab { id: t });
        }
    }
    // arc_spec §6.6: only when the tab plays audio or is muted.
    let muted = tab.is_some_and(|t| t.muted);
    if focused.is_some() && (muted || r.is_some_and(|r| r.audible)) {
        add("tab.mute_toggle", if muted { "Unmute Tab" } else { "Mute Tab" }, &["sound", "audio", "silence"], None, glyph(if muted { "speaker" } else { "speaker-muted" }), Command::ToggleMute { id: None });
    }
    if live {
        add("page.find", "Find in Page", &["search page"], Some("Ctrl+F"), glyph("find"), Command::OpenFind);
        add("page.zoom_in", "Zoom In", &["bigger", "larger"], Some("Ctrl+="), glyph("zoom"), Command::Zoom { direction: ZoomDirection::In });
        add("page.zoom_out", "Zoom Out", &["smaller"], Some("Ctrl+-"), glyph("zoom-out"), Command::Zoom { direction: ZoomDirection::Out });
        add("page.zoom_reset", "Actual Size", &["reset zoom", "100%"], Some("Ctrl+0"), glyph("zoom"), Command::Zoom { direction: ZoomDirection::Reset });
        add("page.reload", "Reload Page", &["refresh"], Some("Ctrl+R"), glyph("reload"), Command::Reload { tab: None, ignore_cache: false });
        add("page.reload_hard", "Hard Reload", &["clear cache reload", "force refresh"], Some("Ctrl+Shift+R"), glyph("reload"), Command::Reload { tab: None, ignore_cache: true });
        let devtools = focused.is_some_and(|t| store.devtools_open().contains(&t));
        let label = if devtools { "Close Developer Tools" } else { "Developer Tools" };
        add("page.devtools", label, &["inspect", "devtools", "console"], Some("F12"), glyph("code"), Command::ToggleDevTools);
        // Undock is only offered while a docked frontend is there to undock.
        if devtools && focused.is_some_and(|t| !store.devtools_undocked(t)) {
            add("page.devtools_undock", "Undock DevTools", &["devtools window", "detach devtools"], None, glyph("code"), Command::UndockDevTools);
        }
        add("page.print", "Print…", &["pdf"], Some("Ctrl+P"), glyph("print"), Command::Print);
        if r.is_some_and(|r| r.can_go_back) {
            add("page.back", "Go Back", &["previous page"], Some("Alt+←"), glyph("back"), Command::GoBack { tab: None });
        }
        if r.is_some_and(|r| r.can_go_forward) {
            add("page.forward", "Go Forward", &["next page"], Some("Alt+→"), glyph("forward"), Command::GoForward { tab: None });
        }
        if !urls::is_internal(&url) && urls::scheme(&url).is_some_and(|s| s != "view-source" && s != "about" && s != "data") {
            add("page.view_source", "View Page Source", &["source", "html"], Some("Ctrl+U"), glyph("code"), Command::ViewSource);
        }
    }
    if urls::is_web(&url) {
        add("boost.new", "New Boost for this Site", &["custom css", "custom js", "boost"], None, glyph("boost"), Command::NewBoostForSite { tab: None });
        for b in store.state.boosts.iter().filter(|b| urls::host_matches(&b.host, &url)) {
            let title = if b.enabled { format!("Disable Boost: {}", b.name) } else { format!("Enable Boost: {}", b.name) };
            add(&format!("boost.toggle:{}", b.id), &title, &["boost"], None, glyph("boost"), Command::ToggleBoost { id: b.id });
        }
    }
    if today_nonempty {
        add("tabs.clear_today", "Clear Today Tabs", &["archive unpinned", "clean up"], Some("Ctrl+Shift+K"), glyph("archive"), Command::ClearToday { space: None });
    }
    add("folder.new", "New Folder", &["group"], None, glyph("folder-plus"), Command::NewFolder { space: None, parent: None, name: None });
    add("space.new", "New Space", &["create space", "workspace"], None, glyph("space"), Command::OpenSidebarPanel { panel: SidebarPanel::NewSpace });
    if space_index + 1 < spaces.len() {
        add("space.next", "Next Space", &["switch space"], Some("Ctrl+Alt+→"), glyph("space"), Command::SwitchSpaceAdjacent { delta: 1 });
    }
    if space_index > 0 {
        add("space.prev", "Previous Space", &["switch space"], Some("Ctrl+Alt+←"), glyph("space"), Command::SwitchSpaceAdjacent { delta: -1 });
    }
    let edit = Command::OpenSidebarPanel { panel: SidebarPanel::EditSpace { id: active_space } };
    add("space.rename", "Rename Space", &["space name"], None, glyph("edit"), edit.clone());
    add("space.theme", "Change Space Theme", &["color", "colour"], None, glyph("palette"), edit.clone());
    add("space.icon", "Change Space Icon", &["emoji"], None, glyph("emoji"), edit.clone());
    if spaces.len() >= 2 {
        add("space.delete", "Delete Space", &["remove space"], None, glyph("trash"), edit);
    }
    if item_tab.is_some() && active_split.is_none_or(|s| s.panes.len() < MAX_SPLIT_PANES) {
        for (side, name, key) in [
            (SplitSide::Right, "Add Right Split", "split.add_right"),
            (SplitSide::Left, "Add Left Split", "split.add_left"),
            (SplitSide::Top, "Add Top Split", "split.add_top"),
            (SplitSide::Bottom, "Add Bottom Split", "split.add_bottom"),
        ] {
            let shortcut = (side == SplitSide::Right).then_some("Ctrl+Shift+=");
            add(key, name, &["split view", "side by side"], shortcut, glyph("split"), Command::OpenCommandBar { mode: CommandBarMode::Split, split_side: Some(side) });
        }
    }
    if let Some(s) = active_split {
        add("split.separate_all", "Separate All Tabs", &["unsplit", "exit split"], None, glyph("split"), Command::SeparateAll { id: s.id });
        add("split.close_pane", "Remove Pane from Split", &["close split"], Some("Ctrl+Shift+-"), glyph("split"), Command::SeparatePane { tab: None });
    }
    add("view.extensions", "Show Extensions", &["extensions", "addons", "add-ons", "plugins"], Some("Ctrl+E"), glyph("puzzle"), Command::OpenCommandBar { mode: CommandBarMode::Extensions, split_side: None });
    add("view.manage_extensions", "Manage Extensions", &["extensions settings", "remove extension", "turn on extension"], None, glyph("puzzle"), Command::OpenUrl { url: super::extensions::settings_extensions_url(None), target: OpenTarget::NewTab, opener: None });
    add("sidebar.toggle", "Toggle Sidebar", &["hide sidebar", "show sidebar"], Some("Ctrl+S"), glyph("sidebar"), Command::ToggleSidebar);
    add("view.archive", "View Archive", &["archived tabs", "closed tabs"], None, glyph("archive"), Command::OpenInternalPage { page: InternalPage::Archive });
    if !store.state.archive.is_empty() {
        add("archive.clear", "Clear Archive", &["delete archive"], None, glyph("trash"), Command::ClearArchive);
    }
    add("view.downloads", "Show Downloads", &["downloads"], Some("Ctrl+J"), glyph("download"), Command::OpenSidebarPanel { panel: SidebarPanel::Downloads });
    add("view.history", "Show History", &["history", "visited"], Some("Ctrl+H"), glyph("history"), Command::OpenInternalPage { page: InternalPage::History });
    add("view.settings", "Open Settings", &["preferences", "options"], Some("Ctrl+,"), glyph("settings"), Command::OpenInternalPage { page: InternalPage::Settings });
    add("view.boosts", "Manage Boosts", &["boosts", "custom css"], None, glyph("boost"), Command::OpenInternalPage { page: InternalPage::Boosts });
    let appearance = store.state.settings.appearance;
    for (value, title, icon, key) in [
        (Appearance::Light, "Appearance: Light", "sun", "theme.appearance_light"),
        (Appearance::Dark, "Appearance: Dark", "moon", "theme.appearance_dark"),
        (Appearance::System, "Appearance: System", "settings", "theme.appearance_system"),
    ] {
        if appearance != value {
            add(key, title, &["theme", "dark mode", "light mode"], None, glyph(icon), Command::UpdateSettings { patch: SettingsPatch { appearance: Some(value), ..SettingsPatch::default() } });
        }
    }
    let animations_on = store.state.settings.animations.enabled;
    add(
        "motion.toggle",
        if animations_on { "Turn Animations Off" } else { "Turn Animations On" },
        &["animation", "motion", "reduce motion", "no animation"],
        None,
        glyph("settings"),
        Command::UpdateSettings {
            patch: SettingsPatch {
                animations: Some(crate::motion::AnimationsPatch { enabled: Some(!animations_on), ..Default::default() }),
                ..SettingsPatch::default()
            },
        },
    );
    add("window.fullscreen", "Toggle Full Screen", &["fullscreen"], Some("F11"), glyph("maximize"), Command::WindowControl { action: WindowAction::ToggleFullscreen });
    add("app.quit", "Quit sta", &["exit", "close window"], None, glyph("quit"), Command::Quit);

    for s in spaces.iter().filter(|s| s.id != active_space) {
        let pos = spaces.iter().position(|x| x.id == s.id).unwrap_or(0);
        let shortcut: Option<&'static str> = ["Alt+1", "Alt+2", "Alt+3", "Alt+4", "Alt+5", "Alt+6", "Alt+7", "Alt+8", "Alt+9"].get(pos).copied();
        out.push(Action {
            id: format!("space.goto:{}", s.id),
            title: format!("Go to Space: {}", s.name),
            aliases: vec!["switch space"],
            shortcut,
            icon: ResultIcon::Emoji { emoji: s.icon.clone() },
            command: Command::SwitchSpace { id: s.id },
            actions_mode_only: true,
        });
        if let Some(a) = active_item.filter(|a| !store.state.favorites.contains(a)) {
            out.push(Action {
                id: format!("tab.move_to_space:{}", s.id),
                title: format!("Move Tab to Space: {}", s.name),
                aliases: vec!["move to space"],
                shortcut: None,
                icon: ResultIcon::Emoji { emoji: s.icon.clone() },
                command: Command::MoveToSpace { id: Some(a), space: s.id },
                actions_mode_only: false,
            });
        }
    }
    out
}
