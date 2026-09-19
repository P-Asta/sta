//! Command bar ("omnibox"): input classification, fuzzy matching, result ranking and the action
//! registry. Spec: `docs/research/arc_spec.md` §6 (groups, scoring, frecency, URL-vs-search rule,
//! search engines, actions registry).
//!
//! Request/response over IPC: `invoke('omnibox.query', OmniboxRequest) -> OmniboxResponse`.
//! Each result carries the exact [`Command`] to run; the UI commits with
//! `dispatch({type: 'commitOmnibox', command: result.command, alt})`, so the UI never interprets
//! result semantics. Core is pure: remote search suggestions are fetched by the shell
//! (`omnibox.suggest`, from [`suggest_url`], read with [`parse_suggestions`]) and passed back in
//! via `OmniboxRequest::suggestions`.
//!
//! The result assembly itself lives in `store/omni.rs` (it needs the store); this module holds the
//! pure building blocks.

use crate::command::{CommandBarMode, OpenTarget, SplitSide};
use crate::model::SearchEngineId;
use crate::view::SearchEngineInfo;
use crate::Command;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OmniboxRequest {
    pub text: String,
    pub mode: CommandBarMode,
    #[serde(default)]
    pub split_side: Option<SplitSide>,
    /// The user just deleted text (Backspace/Delete): don't offer inline completion.
    #[serde(default)]
    pub prevent_inline_autocomplete: bool,
    /// Remote suggestions for `text` (from `omnibox.suggest`). Empty = none. They become
    /// Suggestions rows, and without a history completion the first one extending `text`
    /// completes inline.
    #[serde(default)]
    pub suggestions: Vec<String>,
    /// Client sequence number, echoed in the response (drop stale responses).
    #[serde(default)]
    pub seq: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OmniboxResponse {
    pub text: String,
    pub seq: u64,
    /// Inline autocompletion: full text to display (the part after `text` is shown selected).
    /// When present, `results[0]` already commits the completed text.
    pub inline_completion: Option<String>,
    /// Ordered for display (grouped); index 0 is the default (Enter) result. Max 12.
    pub results: Vec<OmniboxResult>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OmniboxResult {
    /// Stable key for list rendering (e.g. "tab:12", "action:tab.pin_toggle", "go", "search").
    pub key: String,
    pub group: ResultGroup,
    pub title: String,
    pub subtitle: Option<String>,
    pub icon: ResultIcon,
    /// Right-aligned hint chip: "Switch to Tab", "↵", "Ctrl+Shift+K", ...
    pub hint: Option<String>,
    /// Command to commit on Enter/click.
    pub command: Command,
    /// Command to commit on Alt+Enter (e.g. background tab); `None` = same as `command`.
    pub alt_command: Option<Command>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ResultGroup {
    TopHit,
    Go,
    /// Empty query: MRU open tabs.
    RecentTabs,
    /// Empty query: New Space, View Archive, Toggle Sidebar, Settings…
    SuggestedActions,
    Tabs,
    Actions,
    Spaces,
    History,
    Suggestions,
    Archive,
    // ---------------------------------------------------------------- Ctrl+E (extensions mode)
    /// Installed and enabled.
    Extensions,
    /// Added by another program, waiting for the user's OK.
    NeedsOk,
    /// Turned off (or blocked).
    ExtensionsOff,
    /// Get extensions / Manage extensions.
    More,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum ResultIcon {
    /// Page favicon; UI falls back to a letter tile from `host`.
    Favicon { url: Option<String>, host: String },
    Emoji { emoji: String },
    /// Named UI glyph (see `ui/common/icons.js`): "search", "globe", "archive", "settings",
    /// "sidebar", "split", "pin", "copy", "reload", "folder", "space", "download", "boost",
    /// "zoom", "code", "print", "find", "history", "close", "plus", "moon", "sun", "quit",
    /// "star", "restore", plus "back", "forward", "zoom-out", "edit", "palette", "emoji",
    /// "folder-plus", "trash", "maximize", "pause", "speaker", "speaker-muted".
    Glyph { name: String },
}

/// Classification of typed text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Classified {
    /// Navigate to this absolute URL.
    Url(String),
    /// Search for this query (not yet turned into a URL).
    Search(String),
}

/// Schemes that are always navigated as typed.
const KNOWN_SCHEMES: &[&str] =
    &["http", "https", "file", "sta", "about", "chrome", "view-source", "data", "mailto", "tel", "sms", "ftp"];

/// URL-vs-search heuristic (arc_spec §6.4): `?` prefix forces search; known schemes go
/// (http, https, file, sta, about, chrome, view-source, data, mailto, tel, sms, ftp), and so
/// does any other `scheme://…` without whitespace (`zoommtg://zoom.us/join`); whitespace →
/// search; localhost/IPv4/`[IPv6]` → http; `host.tld[:port][/path]` with a public suffix (psl crate)
/// or explicit port/path → https; emails → search; Windows paths (`C:\...`) → file URL; else search.
///
/// Windows paths are checked before the whitespace rule (paths often contain spaces), and UNC
/// paths (`\\server\share`) also become file URLs. A host whose last label is all digits
/// (`3.14`) never counts as a domain. Opening a URL of a scheme Chromium doesn't load hands it
/// to the OS instead of a tab (`Effect::OpenExternal`).
pub fn classify(text: &str) -> Classified {
    let s = text.trim();
    // Internal page URLs from before the rename open their `sta://` page (`legacy::upgrade_url`).
    if let Some(url) = crate::legacy::upgrade_url(s) {
        return Classified::Url(url);
    }
    if let Some(rest) = s.strip_prefix('?') {
        return Classified::Search(rest.trim().to_string());
    }
    if s.is_empty() {
        return Classified::Search(String::new());
    }
    if let Some(scheme) = crate::urls::scheme(s)
        && KNOWN_SCHEMES.contains(&scheme.as_str())
    {
        return Classified::Url(s.to_string());
    }
    if let Some(url) = windows_path_to_file_url(s) {
        return Classified::Url(url);
    }
    if s.chars().any(char::is_whitespace) {
        return Classified::Search(s.to_string());
    }
    // Any other `scheme://…` (an app protocol such as `zoommtg://`). One-letter schemes are drive
    // letters (handled above); `localhost` is a host, not a scheme.
    if let Some(scheme) = crate::urls::scheme(s)
        && scheme.len() >= 2
        && !scheme.contains('.')
        && scheme != "localhost"
        && s[scheme.len() + 1..].starts_with("//")
        && s.len() > scheme.len() + 3
        && scheme != "javascript"
    {
        return Classified::Url(s.to_string());
    }
    let (authority, rest) = match s.find(['/', '?', '#']) {
        Some(i) => (&s[..i], &s[i..]),
        None => (s, ""),
    };
    // localhost[:port]
    let (host_part, port) = split_port(authority);
    if port != PortSplit::Invalid && host_part.eq_ignore_ascii_case("localhost") {
        return Classified::Url(format!("http://{s}"));
    }
    // [IPv6][:port]
    if let Some(inner) = authority.strip_prefix('[')
        && let Some(end) = inner.find(']')
    {
        let after = &inner[end + 1..];
        let port_ok = after.is_empty() || after.strip_prefix(':').is_some_and(valid_port);
        if port_ok && inner[..end].parse::<std::net::Ipv6Addr>().is_ok() {
            return Classified::Url(format!("http://{s}"));
        }
    }
    // IPv4[:port]
    if port != PortSplit::Invalid && is_ipv4(host_part) {
        return Classified::Url(format!("http://{s}"));
    }
    // host.tld[:port][/path]
    if port != PortSplit::Invalid && !authority.contains('@') && host_part.contains('.') && valid_host(host_part) {
        let ascii = url::quirks::domain_to_ascii(host_part);
        if let Some(tld) = ascii.trim_end_matches('.').rsplit('.').next().filter(|t| !t.is_empty()) {
            let numeric = tld.chars().all(|c| c.is_ascii_digit());
            let known = psl::suffix(tld.as_bytes()).is_some_and(|sfx| sfx.is_known());
            if !numeric && (known || port == PortSplit::Port || !rest.is_empty()) {
                return Classified::Url(format!("https://{s}"));
            }
        }
    }
    Classified::Search(s.to_string())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PortSplit {
    None,
    Port,
    Invalid,
}

fn valid_port(p: &str) -> bool {
    (1..=5).contains(&p.len()) && p.chars().all(|c| c.is_ascii_digit())
}

fn split_port(authority: &str) -> (&str, PortSplit) {
    match authority.rfind(':') {
        Some(i) => {
            if valid_port(&authority[i + 1..]) {
                (&authority[..i], PortSplit::Port)
            } else {
                (authority, PortSplit::Invalid)
            }
        }
        None => (authority, PortSplit::None),
    }
}

fn is_ipv4(host: &str) -> bool {
    let parts: Vec<&str> = host.split('.').collect();
    parts.len() == 4 && parts.iter().all(|p| (1..=3).contains(&p.len()) && p.chars().all(|c| c.is_ascii_digit()) && p.parse::<u16>().is_ok_and(|v| v <= 255))
}

fn valid_host(host: &str) -> bool {
    !host.starts_with('.')
        && !host.ends_with("..")
        && host.split('.').filter(|l| !l.is_empty()).count() >= 2
        && !host.split('.').rev().skip(1).any(str::is_empty)
        && host.chars().all(|c| c.is_alphanumeric() || c == '-' || c == '.' || c == '_')
}

fn windows_path_to_file_url(s: &str) -> Option<String> {
    let b = s.as_bytes();
    let drive = b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && (b[2] == b'\\' || b[2] == b'/');
    let unc = s.starts_with("\\\\") && s.len() > 2;
    if !drive && !unc {
        return None;
    }
    let escaped = s.replace('%', "%25").replace('#', "%23").replace('?', "%3F").replace('\\', "/");
    let candidate = if drive { format!("file:///{escaped}") } else { format!("file:{escaped}") };
    url::Url::parse(&candidate).ok().map(|u| u.to_string())
}

/// Search engine display name.
pub fn engine_name(engine: SearchEngineId) -> &'static str {
    match engine {
        SearchEngineId::Google => "Google",
        SearchEngineId::Bing => "Bing",
        SearchEngineId::DuckDuckGo => "DuckDuckGo",
        SearchEngineId::Ecosia => "Ecosia",
        SearchEngineId::Brave => "Brave Search",
        SearchEngineId::Kagi => "Kagi",
        SearchEngineId::Perplexity => "Perplexity",
        SearchEngineId::Custom => "Custom",
    }
}

/// Built-in search URL template (`{q}` placeholder); `Custom` has none.
pub fn engine_template(engine: SearchEngineId) -> &'static str {
    match engine {
        SearchEngineId::Google => "https://www.google.com/search?q={q}",
        SearchEngineId::Bing => "https://www.bing.com/search?q={q}",
        SearchEngineId::DuckDuckGo => "https://duckduckgo.com/?q={q}",
        SearchEngineId::Ecosia => "https://www.ecosia.org/search?q={q}",
        SearchEngineId::Brave => "https://search.brave.com/search?q={q}",
        SearchEngineId::Kagi => "https://kagi.com/search?q={q}",
        SearchEngineId::Perplexity => "https://www.perplexity.ai/search?q={q}",
        SearchEngineId::Custom => "",
    }
}

/// All engines for the settings page (`Custom` carries the user's template).
pub fn search_engines(custom_template: &str) -> Vec<SearchEngineInfo> {
    [
        SearchEngineId::Google,
        SearchEngineId::Bing,
        SearchEngineId::DuckDuckGo,
        SearchEngineId::Ecosia,
        SearchEngineId::Brave,
        SearchEngineId::Kagi,
        SearchEngineId::Perplexity,
        SearchEngineId::Custom,
    ]
    .into_iter()
    .map(|id| SearchEngineInfo {
        id,
        name: engine_name(id).into(),
        url: if id == SearchEngineId::Custom { custom_template.trim().to_string() } else { engine_template(id).into() },
    })
    .collect()
}

/// Name shown in "Search X" rows: the custom engine shows its host when it has a usable template.
pub fn effective_engine_name(engine: SearchEngineId, custom_template: &str) -> String {
    if engine == SearchEngineId::Custom {
        if custom_usable(custom_template) {
            let probe = custom_template.replace("{q}", "x").replace("%s", "x");
            return crate::urls::host(&probe).map(|h| crate::urls::display_host(&format!("https://{h}/"))).unwrap_or_else(|| "Custom".into());
        }
        return "Google".into();
    }
    engine_name(engine).into()
}

fn custom_usable(template: &str) -> bool {
    let t = template.trim();
    (t.contains("{q}") || t.contains("%s")) && crate::urls::scheme(t).is_some()
}

/// `encodeURIComponent` semantics: everything but `A-Z a-z 0-9 - _ . ! ~ * ' ( )` is
/// percent-encoded as UTF-8.
pub fn encode_uri_component(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'!' | b'~' | b'*' | b'\'' | b'(' | b')') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Search URL for `query` with the given engine (`custom_template` used for `Custom`, falling back
/// to Google when it lacks `{q}`); `{q}` replaced by the percent-encoded query
/// (encodeURIComponent semantics). Chromium-style `%s` is accepted in custom templates too.
pub fn search_url(engine: SearchEngineId, custom_template: &str, query: &str) -> String {
    let q = encode_uri_component(query);
    let template = if engine == SearchEngineId::Custom {
        if custom_usable(custom_template) { custom_template.trim() } else { engine_template(SearchEngineId::Google) }
    } else {
        engine_template(engine)
    };
    template.replace("{q}", &q).replace("%s", &q)
}

/// Resolve typed text to the URL to load.
pub fn resolve_input(text: &str, engine: SearchEngineId, custom_template: &str) -> String {
    match classify(text) {
        Classified::Url(u) => u,
        Classified::Search(q) => search_url(engine, custom_template, &q),
    }
}

// ------------------------------------------------------------------------------ suggestions

/// Most remote suggestions kept per query ([`parse_suggestions`]).
pub const MAX_SUGGESTIONS: usize = 8;

/// Remote suggestion endpoint of `engine` for `query`, answering OpenSearch suggestion JSON
/// (`[query, [s1, s2, …], …]`); `None` for engines without one (Kagi, Perplexity, Custom).
///
/// `query` is percent-encoded with `encodeURIComponent` semantics (UTF-8). `lang` is the primary
/// OS UI language (`ko`, `en-US`): Google gets it as `hl` (omitted when empty); the other engines
/// localize by region themselves. Every endpoint was checked to answer OpenSearch JSON.
pub fn suggest_url(engine: SearchEngineId, query: &str, lang: &str) -> Option<String> {
    let q = encode_uri_component(query);
    Some(match engine {
        SearchEngineId::Google => {
            let lang = lang.trim();
            let hl = if lang.is_empty() { String::new() } else { format!("&hl={}", encode_uri_component(lang)) };
            format!("https://suggestqueries.google.com/complete/search?client=firefox&ie=utf-8&oe=utf-8{hl}&q={q}")
        }
        SearchEngineId::Bing => format!("https://www.bing.com/osjson.aspx?query={q}"),
        SearchEngineId::DuckDuckGo => format!("https://duckduckgo.com/ac/?q={q}&type=list"),
        SearchEngineId::Brave => format!("https://search.brave.com/api/suggest?q={q}"),
        SearchEngineId::Ecosia => format!("https://ac.ecosia.org/autocomplete?q={q}&type=list"),
        SearchEngineId::Kagi | SearchEngineId::Perplexity | SearchEngineId::Custom => return None,
    })
}

/// Suggestions from an OpenSearch suggestion response (`[query, [s1, s2, …], …]`; anything after
/// the list is ignored). The body is decoded as UTF-8 (lossy); entries are trimmed; empty entries,
/// non-strings, case-insensitive duplicates and the query itself are dropped; at most
/// [`MAX_SUGGESTIONS`]. Anything else (malformed JSON, another shape) yields no suggestions.
pub fn parse_suggestions(body: &[u8], query: &str) -> Vec<String> {
    let text = String::from_utf8_lossy(body);
    let Ok(serde_json::Value::Array(top)) = serde_json::from_str::<serde_json::Value>(text.trim_start_matches('\u{feff}')) else {
        return Vec::new();
    };
    let Some(serde_json::Value::Array(list)) = top.get(1) else { return Vec::new() };
    let mut seen: Vec<String> = vec![query.trim().to_lowercase()];
    let mut out = Vec::new();
    for s in list.iter().filter_map(serde_json::Value::as_str).map(str::trim).filter(|s| !s.is_empty()) {
        let key = s.to_lowercase();
        if seen.contains(&key) {
            continue;
        }
        seen.push(key);
        out.push(s.to_string());
        if out.len() == MAX_SUGGESTIONS {
            break;
        }
    }
    out
}

/// Byte length of the prefix of `text` that matches all of `typed`, compared char by char
/// case-insensitively (Unicode lowercase); `None` when `text` doesn't start with `typed`. The
/// result is always a char boundary of `text`.
pub fn case_insensitive_prefix(text: &str, typed: &str) -> Option<usize> {
    let mut rest = text.char_indices();
    for t in typed.chars() {
        let (_, c) = rest.next()?;
        if c != t && !c.to_lowercase().eq(t.to_lowercase()) {
            return None;
        }
    }
    Some(rest.next().map_or(text.len(), |(i, _)| i))
}

// ------------------------------------------------------------------------------------ fuzzy

/// Minimum score for a fuzzy match.
pub const FUZZY_THRESHOLD: f32 = 0.35;

const BASE: f32 = 0.25;
const WORD: f32 = 0.35;
const CONSECUTIVE: f32 = 0.08;
const GAP: f32 = 0.02;
const GAP_MAX: f32 = 0.3;
const PREFIX: f32 = 0.6;
const EXACT: f32 = 1.0;
/// Texts are truncated to this many characters for matching.
const MAX_TEXT: usize = 512;

/// Lowercase and fold common Latin diacritics (a cheap stand-in for NFKD folding).
pub fn fold(s: &str) -> Vec<char> {
    let mut out = Vec::with_capacity(s.len());
    for c in s.chars() {
        for l in c.to_lowercase() {
            out.push(fold_char(l));
        }
    }
    out
}

fn fold_char(c: char) -> char {
    if c.is_ascii() {
        return c;
    }
    match c {
        'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' | 'ā' | 'ă' | 'ą' => 'a',
        'ç' | 'ć' | 'ĉ' | 'ċ' | 'č' => 'c',
        'ď' | 'đ' => 'd',
        'è' | 'é' | 'ê' | 'ë' | 'ē' | 'ĕ' | 'ė' | 'ę' | 'ě' => 'e',
        'ĝ' | 'ğ' | 'ġ' | 'ģ' => 'g',
        'ĥ' | 'ħ' => 'h',
        'ì' | 'í' | 'î' | 'ï' | 'ĩ' | 'ī' | 'ĭ' | 'į' | 'ı' => 'i',
        'ĵ' => 'j',
        'ķ' => 'k',
        'ĺ' | 'ļ' | 'ľ' | 'ŀ' | 'ł' => 'l',
        'ñ' | 'ń' | 'ņ' | 'ň' => 'n',
        'ò' | 'ó' | 'ô' | 'õ' | 'ö' | 'ø' | 'ō' | 'ŏ' | 'ő' => 'o',
        'ŕ' | 'ŗ' | 'ř' => 'r',
        'ś' | 'ŝ' | 'ş' | 'š' | 'ß' => 's',
        'ţ' | 'ť' | 'ŧ' => 't',
        'ù' | 'ú' | 'û' | 'ü' | 'ũ' | 'ū' | 'ŭ' | 'ů' | 'ű' | 'ų' => 'u',
        'ŵ' => 'w',
        'ý' | 'ÿ' | 'ŷ' => 'y',
        'ź' | 'ż' | 'ž' => 'z',
        other => other,
    }
}

fn is_boundary(prev: char) -> bool {
    matches!(prev, ' ' | '.' | '/' | '-' | '_' | ':' | '?' | '&' | '=' | '#' | '(' | '[' | '|' | ',' | '+' | '@')
}

/// Best-alignment score of `q` (folded) as a subsequence of `t` (folded), normalized to `[0, 1]`.
fn score_chars(q: &[char], t: &[char]) -> Option<f32> {
    let m = q.len();
    let t = &t[..t.len().min(MAX_TEXT)];
    let n = t.len();
    if m == 0 || m > n {
        return None;
    }
    // Cheap rejection: greedy subsequence test.
    let mut qi = 0;
    for &c in t {
        if qi < m && c == q[qi] {
            qi += 1;
        }
    }
    if qi < m {
        return None;
    }
    if q == t {
        return Some(1.0);
    }
    const NEG: f32 = f32::NEG_INFINITY;
    let word = |j: usize| if j == 0 || is_boundary(t[j - 1]) { WORD } else { 0.0 };
    // prev[j]: best raw score with q[i-1] matched at t[j].
    let mut prev: Vec<f32> = (0..n).map(|j| if t[j] == q[0] { BASE + word(j) } else { NEG }).collect();
    let mut cur = vec![NEG; n];
    let far = (GAP_MAX / GAP).round() as usize; // gap length from which the penalty is capped
    for &qc in &q[1..] {
        // run_lin = max_{k <= j-2} prev[k] + GAP*k ; run_far = max_{k <= j-1-far} prev[k]
        let mut run_lin = NEG;
        let mut run_far = NEG;
        for j in 0..n {
            cur[j] = NEG;
            if j >= 2 && prev[j - 2] > NEG {
                run_lin = run_lin.max(prev[j - 2] + GAP * (j - 2) as f32);
            }
            if j > far && prev[j - 1 - far] > NEG {
                run_far = run_far.max(prev[j - 1 - far]);
            }
            if t[j] != qc {
                continue;
            }
            let mut best = NEG;
            if j >= 1 && prev[j - 1] > NEG {
                best = prev[j - 1] + CONSECUTIVE;
            }
            if run_lin > NEG {
                best = best.max(run_lin - GAP * (j - 1) as f32);
            }
            if run_far > NEG {
                best = best.max(run_far - GAP_MAX);
            }
            if best > NEG {
                cur[j] = best + BASE + word(j);
            }
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    let best = prev.iter().copied().fold(NEG, f32::max);
    if best == NEG {
        return None;
    }
    let mut raw = best;
    if t.starts_with(q) {
        raw += PREFIX;
    }
    let ideal = m as f32 * BASE + PREFIX + WORD + CONSECUTIVE * (m as f32 - 1.0) + EXACT * 0.5;
    Some((raw / ideal).clamp(0.0, 1.0))
}

/// Fuzzy subsequence score in [0, 1] (arc_spec §6.3); `None` when below the 0.35 threshold.
///
/// Scoring: each matched char 0.25, +0.35 at a word start (after space . / - _ …), +0.08 when
/// consecutive, −0.02 per skipped char between matches (capped at −0.3 per gap), +0.6 when the
/// text starts with the query; normalized by the ideal score for the query length (exact match =
/// 1.0). Multi-word queries that don't match as a whole match when every word matches (mean × 0.9).
pub fn fuzzy_score(query: &str, text: &str) -> Option<f32> {
    let q = fold(query.trim());
    if q.is_empty() {
        return None;
    }
    let t = fold(text);
    fuzzy_folded(&q, &t)
}

/// [`fuzzy_score`] over pre-folded inputs.
pub fn fuzzy_folded(q: &[char], t: &[char]) -> Option<f32> {
    if let Some(s) = score_chars(q, t).filter(|s| *s >= FUZZY_THRESHOLD) {
        return Some(s);
    }
    let tokens: Vec<&[char]> = q.split(|c| c.is_whitespace()).filter(|t| !t.is_empty()).collect();
    if tokens.len() < 2 {
        return None;
    }
    let mut sum = 0.0;
    for tok in &tokens {
        sum += score_chars(tok, t).filter(|s| *s >= FUZZY_THRESHOLD)?;
    }
    let s = sum / tokens.len() as f32 * 0.9;
    (s >= FUZZY_THRESHOLD).then_some(s)
}

// ------------------------------------------------------------------------- Hangul IME fallback

/// The 2-set Korean keyboard, by jamo index: what QWERTY key produces each initial, vowel and final.
/// Compound vowels and finals are two keys, exactly as they are typed.
const HANGUL_INITIALS: [&str; 19] =
    ["r", "r", "s", "e", "e", "f", "a", "q", "q", "t", "t", "d", "w", "w", "c", "z", "x", "v", "g"];
const HANGUL_VOWELS: [&str; 21] =
    ["k", "o", "i", "o", "j", "p", "u", "p", "h", "hk", "ho", "hl", "y", "n", "nj", "np", "nl", "b", "m", "ml", "l"];
const HANGUL_FINALS: [&str; 28] = [
    "", "r", "r", "rt", "s", "sw", "sg", "e", "f", "fr", "fa", "fq", "ft", "fx", "fv", "fg", "a", "q", "qt", "t", "t",
    "d", "w", "c", "z", "x", "v", "g",
];
/// Compatibility jamo (U+3131..U+3163), which an IME leaves behind for an unfinished syllable.
const HANGUL_COMPAT: [&str; 51] = [
    "r", "R", "rt", "s", "sw", "sg", "e", "E", "f", "fr", "fa", "fq", "ft", "fx", "fv", "fg", "a", "q", "Q", "qt", "t",
    "T", "d", "w", "W", "c", "z", "x", "v", "g", "k", "o", "i", "O", "j", "p", "u", "P", "h", "hk", "ho", "hl", "y",
    "n", "nj", "np", "nl", "b", "m", "ml", "l",
];

/// The letters a Hangul string was typed with (2-set keyboard), so a query the user entered with the
/// IME on still finds "AdBlock" (UX15). `None` when there is no Hangul to translate — the caller
/// then has nothing to retry.
///
/// This is a *fallback*, not a transliteration: it undoes the IME, key for key.
pub fn jamo_to_qwerty(text: &str) -> Option<String> {
    let mut out = String::with_capacity(text.len());
    let mut found = false;
    for c in text.chars() {
        let code = c as u32;
        match code {
            0xAC00..=0xD7A3 => {
                let index = code - 0xAC00;
                out.push_str(HANGUL_INITIALS[(index / 588) as usize]);
                out.push_str(HANGUL_VOWELS[((index % 588) / 28) as usize]);
                out.push_str(HANGUL_FINALS[(index % 28) as usize]);
                found = true;
            }
            0x3131..=0x3163 => {
                out.push_str(HANGUL_COMPAT[(code - 0x3131) as usize]);
                found = true;
            }
            _ => out.push(c),
        }
    }
    found.then_some(out)
}

/// Weighted match over a page's fields: `max(title × 1.0, host × 1.1, path × 0.6)`.
pub fn match_page(query: &str, title: &str, url: &str) -> Option<f32> {
    let q = fold(query.trim());
    if q.is_empty() {
        return None;
    }
    match_page_folded(&q, title, url)
}

/// [`match_page`] with a pre-folded query.
pub fn match_page_folded(q: &[char], title: &str, url: &str) -> Option<f32> {
    let mut best: Option<f32> = None;
    let mut consider = |s: Option<f32>, w: f32| {
        if let Some(s) = s {
            let v = s * w;
            best = Some(best.map_or(v, |b: f32| b.max(v)));
        }
    };
    consider(fuzzy_folded(q, &fold(title)), 1.0);
    let host = crate::urls::display_host(url);
    consider(fuzzy_folded(q, &fold(&host)), 1.1);
    let path = crate::urls::path_part(url);
    if path.len() > 1 {
        consider(fuzzy_folded(q, &fold(path)), 0.6);
    }
    best
}

/// Default `OpenTarget` for committing typed text in a mode.
pub fn target_for_mode(mode: CommandBarMode) -> OpenTarget {
    match mode {
        CommandBarMode::EditUrl => OpenTarget::CurrentTab,
        _ => OpenTarget::NewTab,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(s: &str) -> Classified {
        Classified::Url(s.to_string())
    }
    fn search(s: &str) -> Classified {
        Classified::Search(s.to_string())
    }

    #[test]
    fn classify_cases() {
        let cases: &[(&str, Classified)] = &[
            ("example.com", url("https://example.com")),
            ("  example.com  ", url("https://example.com")),
            ("github.com/rust-lang/rust", url("https://github.com/rust-lang/rust")),
            ("www.google.co.uk", url("https://www.google.co.uk")),
            ("sub.domain.example.org/path?q=1#x", url("https://sub.domain.example.org/path?q=1#x")),
            ("http://example.com", url("http://example.com")),
            ("HTTPS://Example.com/A", url("HTTPS://Example.com/A")),
            ("https://example.com/a b", url("https://example.com/a b")),
            ("file:///C:/x.txt", url("file:///C:/x.txt")),
            ("sta://settings", url("sta://settings")),
            ("about:blank", url("about:blank")),
            ("chrome://version", url("chrome://version")),
            ("view-source:https://x.com", url("view-source:https://x.com")),
            ("data:text/html,hi", url("data:text/html,hi")),
            ("mailto:me@example.com", url("mailto:me@example.com")),
            ("tel:+15551234", url("tel:+15551234")),
            ("sms:+15551234", url("sms:+15551234")),
            ("zoommtg://zoom.us/join?confno=1", url("zoommtg://zoom.us/join?confno=1")),
            ("ms-settings:display", search("ms-settings:display")),
            ("zoommtg://", search("zoommtg://")),
            ("localhost://x", search("localhost://x")),
            ("ftp://ftp.example.com", url("ftp://ftp.example.com")),
            ("localhost", url("http://localhost")),
            ("localhost:3000", url("http://localhost:3000")),
            ("localhost:3000/api/x", url("http://localhost:3000/api/x")),
            ("LOCALHOST:8080", url("http://LOCALHOST:8080")),
            ("192.168.1.1", url("http://192.168.1.1")),
            ("10.0.0.1:8080/admin", url("http://10.0.0.1:8080/admin")),
            ("[::1]", url("http://[::1]")),
            ("[::1]:8080/x", url("http://[::1]:8080/x")),
            ("myhost.internal:8443", url("https://myhost.internal:8443")),
            ("intranet.corp/wiki", url("https://intranet.corp/wiki")),
            ("bücher.de", url("https://bücher.de")),
            ("пример.рф", url("https://пример.рф")),
            ("rust lang", search("rust lang")),
            ("rust", search("rust")),
            ("node.js", search("node.js")),
            ("file.txt", search("file.txt")),
            ("3.14", search("3.14")),
            ("999.1.1.1", search("999.1.1.1")),
            ("me@example.com", search("me@example.com")),
            ("?example.com", search("example.com")),
            ("? what is rust", search("what is rust")),
            ("", search("")),
            ("localhost:abc", search("localhost:abc")),
            ("javascript:alert(1)", search("javascript:alert(1)")),
            ("C:\\Users\\me\\My Doc.pdf", url("file:///C:/Users/me/My%20Doc.pdf")),
            ("c:/temp/a#b.txt", url("file:///c:/temp/a%23b.txt")),
            ("\\\\server\\share\\x.txt", url("file://server/share/x.txt")),
            ("a..com", search("a..com")),
            (".com", search(".com")),
            ("what is 2+2?", search("what is 2+2?")),
        ];
        for (input, expected) in cases {
            assert_eq!(&classify(input), expected, "input {input:?}");
        }
    }

    #[test]
    fn search_url_encoding() {
        assert_eq!(search_url(SearchEngineId::Google, "", "rust lang"), "https://www.google.com/search?q=rust%20lang");
        assert_eq!(search_url(SearchEngineId::Bing, "", "a&b=c"), "https://www.bing.com/search?q=a%26b%3Dc");
        assert_eq!(search_url(SearchEngineId::DuckDuckGo, "", "ünï/?#"), "https://duckduckgo.com/?q=%C3%BCn%C3%AF%2F%3F%23");
        assert_eq!(search_url(SearchEngineId::Ecosia, "", "x"), "https://www.ecosia.org/search?q=x");
        assert_eq!(search_url(SearchEngineId::Brave, "", "x"), "https://search.brave.com/search?q=x");
        assert_eq!(search_url(SearchEngineId::Kagi, "", "x"), "https://kagi.com/search?q=x");
        assert_eq!(search_url(SearchEngineId::Perplexity, "", "x"), "https://www.perplexity.ai/search?q=x");
        assert_eq!(search_url(SearchEngineId::Google, "", "-_.!~*'()"), "https://www.google.com/search?q=-_.!~*'()");
        assert_eq!(search_url(SearchEngineId::Custom, "https://s.example/?k={q}&x={q}", "a b"), "https://s.example/?k=a%20b&x=a%20b");
        assert_eq!(search_url(SearchEngineId::Custom, "https://s.example/?k=%s", "a"), "https://s.example/?k=a");
        assert_eq!(search_url(SearchEngineId::Custom, "https://s.example/", "a"), "https://www.google.com/search?q=a");
        assert_eq!(search_url(SearchEngineId::Custom, "", "a"), "https://www.google.com/search?q=a");
        assert_eq!(resolve_input("rust", SearchEngineId::Google, ""), "https://www.google.com/search?q=rust");
        assert_eq!(resolve_input("rust-lang.org", SearchEngineId::Google, ""), "https://rust-lang.org");
        assert_eq!(effective_engine_name(SearchEngineId::Custom, "https://www.startpage.com/do?q={q}"), "startpage.com");
        assert_eq!(effective_engine_name(SearchEngineId::Custom, "nope"), "Google");
        assert_eq!(search_engines("t{q}").len(), 8);
    }

    #[test]
    fn fuzzy_basics() {
        assert_eq!(fuzzy_score("github", "github"), Some(1.0));
        assert_eq!(fuzzy_score("GitHub", "github"), Some(1.0));
        let prefix = fuzzy_score("git", "github").unwrap();
        let mid = fuzzy_score("hub", "github").unwrap();
        assert!(prefix > mid, "{prefix} {mid}");
        assert!(prefix < 1.0);
        assert!(fuzzy_score("xyz", "github").is_none());
        assert!(fuzzy_score("", "github").is_none());
        assert!(fuzzy_score("githubx", "github").is_none());
        // word starts beat scattered matches
        let ws = fuzzy_score("gh", "git hub").unwrap();
        assert!(fuzzy_score("gb", "gxxxxxxxxxxxxxxb").is_none());
        assert!(ws >= FUZZY_THRESHOLD);
        // single chars: prefix / word start only
        assert!(fuzzy_score("d", "Google Docs").is_some());
        assert!(fuzzy_score("o", "Google Docs").is_none());
        // diacritics fold
        assert!(fuzzy_score("cafe", "Café de Flore").is_some());
        assert!(fuzzy_score("café", "cafe").is_some());
        // multi-word in any order
        assert!(fuzzy_score("rust lang", "The Rust Programming Language").is_some());
        assert!(fuzzy_score("lang rust", "The Rust Programming Language").is_some());
        assert!(fuzzy_score("lang zzz", "The Rust Programming Language").is_none());
        // ranking: exact > prefix > word-prefix > substring
        let a = fuzzy_score("doc", "doc").unwrap();
        let b = fuzzy_score("doc", "docs.rs").unwrap();
        let c = fuzzy_score("doc", "google docs").unwrap();
        let d = fuzzy_score("doc", "xdocx").unwrap_or(0.0);
        assert!(a > b && b > c && c > d, "{a} {b} {c} {d}");
        // long texts don't blow up
        let long = "a".repeat(5000) + "zq";
        assert!(fuzzy_score("aq", &long).is_none() || fuzzy_score("aq", &long).unwrap() <= 1.0);
    }

    #[test]
    fn fuzzy_gap_penalty_monotonic() {
        let near = fuzzy_score("ab", "a-b").unwrap();
        let far = fuzzy_score("ab", "a------b").unwrap_or(0.0);
        assert!(near >= far);
        // Gap penalty is capped: a very long gap scores the same as a 15-char gap.
        let g15 = score_chars(&fold("ab"), &fold(&format!("a{}b", "x".repeat(15)))).unwrap();
        let g40 = score_chars(&fold("ab"), &fold(&format!("a{}b", "x".repeat(40)))).unwrap();
        assert!((g15 - g40).abs() < 1e-6);
    }

    #[test]
    fn match_page_weights() {
        let host = match_page("github", "Some title", "https://github.com/x").unwrap();
        let title = match_page("github", "github", "https://example.com/").unwrap();
        assert!(host > 0.9 && title >= 0.99, "{host} {title}");
        let exact_host = match_page("github.com", "Some title", "https://www.github.com/x").unwrap();
        assert!(exact_host > 1.0, "{exact_host}");
        let path = match_page("issues", "Title", "https://example.com/org/issues").unwrap();
        assert!(path <= 0.6);
        assert!(match_page("zzz", "Title", "https://example.com/").is_none());
    }

    #[test]
    fn suggest_url_per_engine_and_encoding() {
        let google = |q: &str, lang: &str| suggest_url(SearchEngineId::Google, q, lang).unwrap();
        assert_eq!(google("rust", "ko"), "https://suggestqueries.google.com/complete/search?client=firefox&ie=utf-8&oe=utf-8&hl=ko&q=rust");
        assert_eq!(google("rust", ""), "https://suggestqueries.google.com/complete/search?client=firefox&ie=utf-8&oe=utf-8&q=rust");
        assert_eq!(google("rust", "  "), "https://suggestqueries.google.com/complete/search?client=firefox&ie=utf-8&oe=utf-8&q=rust");
        assert_eq!(google("x", "en-US"), "https://suggestqueries.google.com/complete/search?client=firefox&ie=utf-8&oe=utf-8&hl=en-US&q=x");
        // Korean (UTF-8 percent-encoding), spaces, & = # ? + / and a trailing space.
        assert!(google("러스트", "ko").ends_with("&hl=ko&q=%EB%9F%AC%EC%8A%A4%ED%8A%B8"));
        assert!(google("rust pro", "").ends_with("&q=rust%20pro"));
        assert!(google("a&b=c#d?e+f/g", "").ends_with("&q=a%26b%3Dc%23d%3Fe%2Bf%2Fg"));
        assert!(google("rust ", "").ends_with("&q=rust%20"));
        assert!(google("x", "a&b").contains("&hl=a%26b&q=x"), "lang is encoded too");
        assert_eq!(suggest_url(SearchEngineId::Bing, "a b&c", "ko").as_deref(), Some("https://www.bing.com/osjson.aspx?query=a%20b%26c"));
        assert_eq!(suggest_url(SearchEngineId::DuckDuckGo, "러스트", "ko").as_deref(), Some("https://duckduckgo.com/ac/?q=%EB%9F%AC%EC%8A%A4%ED%8A%B8&type=list"));
        assert_eq!(suggest_url(SearchEngineId::Brave, "rust", "").as_deref(), Some("https://search.brave.com/api/suggest?q=rust"));
        assert_eq!(suggest_url(SearchEngineId::Ecosia, "a&b", "").as_deref(), Some("https://ac.ecosia.org/autocomplete?q=a%26b&type=list"));
        for engine in [SearchEngineId::Kagi, SearchEngineId::Perplexity, SearchEngineId::Custom] {
            assert_eq!(suggest_url(engine, "rust", "en"), None, "{engine:?}");
        }
    }

    #[test]
    fn parse_opensearch_suggestions() {
        let parse = |body: &str, q: &str| parse_suggestions(body.as_bytes(), q);
        // Google's shape: extra arrays and an object after the list are ignored.
        let google = r#"["rust",["rustdesk","rust","rust 뜻","rust 언어"],[],{"google:suggestsubtypes":[[512],[512]]}]"#;
        assert_eq!(parse(google, "rust"), ["rustdesk", "rust 뜻", "rust 언어"]);
        assert_eq!(parse(r#"["러스트",["러스트","러스트데스크","러스트 게임"]]"#, "러스트"), ["러스트데스크", "러스트 게임"]);
        // Trim, empty, case-insensitive duplicates, the query itself (any case), non-strings.
        let messy = r#"["Rust",[" rust lang ","RUST LANG","","   ","RUST",null,42,{"x":1},["nested"],"rust book"]]"#;
        assert_eq!(parse(messy, "rust "), ["rust lang", "rust book"]);
        // At most 8.
        let many = format!("[\"q\",[{}]]", (0..20).map(|i| format!("\"q{i}\"")).collect::<Vec<_>>().join(","));
        assert_eq!(parse(&many, "q"), (0..8).map(|i| format!("q{i}")).collect::<Vec<_>>());
        // Malformed or other shapes: nothing.
        for bad in ["", "not json", "[\"rust\",", "{\"a\":[1]}", "\"rust\"", "[\"rust\"]", "[\"rust\",\"rustdesk\"]", "[\"rust\",{\"0\":\"x\"}]", "null", "[]"] {
            assert!(parse(bad, "rust").is_empty(), "{bad:?}");
        }
        // Invalid UTF-8 is decoded lossily; a BOM is tolerated.
        let mut bytes = b"[\"q\",[\"ok\",\"bad \xff byte\"]]".to_vec();
        assert_eq!(parse_suggestions(&bytes, "q"), ["ok", "bad \u{fffd} byte"]);
        bytes.splice(0..0, "\u{feff}".bytes());
        assert_eq!(parse_suggestions(&bytes, "q").len(), 2);
    }

    #[test]
    fn case_insensitive_prefixes() {
        assert_eq!(case_insensitive_prefix("Rust Programming", "rust p"), Some(6));
        assert_eq!(case_insensitive_prefix("rust", "RUST"), Some(4));
        assert_eq!(case_insensitive_prefix("rust", "rusty"), None);
        assert_eq!(case_insensitive_prefix("rust", ""), Some(0));
        assert_eq!(case_insensitive_prefix("러스트 게임", "러스"), Some("러스".len()));
        assert_eq!(case_insensitive_prefix("ÉCOLE normale", "éco"), Some("ÉCO".len()));
        assert_eq!(case_insensitive_prefix("Straße", "STRASSE"), None, "no multi-char folding");
        assert_eq!(case_insensitive_prefix("rust", "rsut"), None);
    }

    #[test]
    fn targets() {
        assert_eq!(target_for_mode(CommandBarMode::EditUrl), OpenTarget::CurrentTab);
        assert_eq!(target_for_mode(CommandBarMode::NewTab), OpenTarget::NewTab);
        assert_eq!(target_for_mode(CommandBarMode::Split), OpenTarget::NewTab);
    }
}
