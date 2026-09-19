//! Browsing history (persisted separately in `history.json`), with Firefox-style frecency used by
//! the command bar (arc_spec §6.3). Capped to the most relevant [`MAX_HISTORY_URLS`] URLs.

use crate::model::Transition;
use crate::Millis;
use serde::{Deserialize, Serialize};

pub const MAX_HISTORY_URLS: usize = 10_000;
/// Visits remembered per URL for frecency.
pub const MAX_VISITS_PER_URL: usize = 10;

const DAY: Millis = 24 * 3600 * 1000;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct History {
    /// One entry per distinct URL.
    pub urls: Vec<HistoryUrl>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct HistoryUrl {
    pub url: String,
    pub title: String,
    pub visit_count: u32,
    pub typed_count: u32,
    pub last_visit_at: Millis,
    /// Most recent visits (newest last), max [`MAX_VISITS_PER_URL`].
    pub visits: Vec<Visit>,
    pub frecency: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Visit {
    pub at: Millis,
    pub transition: Transition,
}

/// Whether a URL is recorded in history (http, https, file).
pub fn is_history_url(url: &str) -> bool {
    matches!(crate::urls::scheme(url).as_deref(), Some("http" | "https" | "file"))
}

/// Age bucket points (arc_spec §6.3).
fn bucket(age: Millis) -> f64 {
    match age {
        a if a <= 4 * DAY => 100.0,
        a if a <= 14 * DAY => 70.0,
        a if a <= 31 * DAY => 50.0,
        a if a <= 90 * DAY => 30.0,
        _ => 10.0,
    }
}

/// Transition bonus (arc_spec §6.3; `Bookmark` plays the role of the spec's `Pinned`). Transitions
/// the spec doesn't list (form submit, back/forward, other) count like links.
fn transition_bonus(t: Transition) -> f64 {
    match t {
        Transition::Typed => 2.0,
        Transition::Bookmark => 1.2,
        Transition::Redirect | Transition::Reload => 0.0,
        Transition::Link | Transition::FormSubmit | Transition::BackForward | Transition::Other => 1.0,
    }
}

/// `visit_count / min(visit_count, 10) × Σ(last 10 visits) bucket(age) × transition_bonus`.
pub fn frecency(entry: &HistoryUrl, now: Millis) -> i32 {
    let start = entry.visits.len().saturating_sub(MAX_VISITS_PER_URL);
    let sampled = &entry.visits[start..];
    if sampled.is_empty() {
        return 0;
    }
    let sum: f64 = sampled.iter().map(|v| bucket(now.saturating_sub(v.at)) * transition_bonus(v.transition)).sum();
    let count = entry.visit_count.max(sampled.len() as u32) as f64;
    let factor = count / count.min(MAX_VISITS_PER_URL as f64);
    (sum * factor).round().clamp(0.0, i32::MAX as f64) as i32
}

impl History {
    /// Record a main-frame visit. Ignores non-web URLs (anything but http/https/file). `title`
    /// may be empty (kept from an earlier visit). Recomputes frecency; evicts the lowest-frecency
    /// URLs beyond [`MAX_HISTORY_URLS`].
    pub fn record_visit(&mut self, url: &str, title: &str, transition: Transition, now: Millis) {
        if !is_history_url(url) {
            return;
        }
        let idx = match self.urls.iter().position(|u| u.url == url) {
            Some(i) => i,
            None => {
                self.urls.push(HistoryUrl { url: url.to_string(), ..HistoryUrl::default() });
                self.urls.len() - 1
            }
        };
        let e = &mut self.urls[idx];
        e.visit_count = e.visit_count.saturating_add(1);
        if transition == Transition::Typed {
            e.typed_count = e.typed_count.saturating_add(1);
        }
        if !title.trim().is_empty() {
            e.title = title.to_string();
        }
        e.last_visit_at = e.last_visit_at.max(now);
        e.visits.push(Visit { at: now, transition });
        if e.visits.len() > MAX_VISITS_PER_URL {
            let excess = e.visits.len() - MAX_VISITS_PER_URL;
            e.visits.drain(..excess);
        }
        e.frecency = frecency(e, now);
        self.evict();
    }

    /// Drop the least relevant entries (lowest frecency, then oldest) beyond the cap, keeping the
    /// order of the others.
    pub fn evict(&mut self) {
        let excess = self.urls.len().saturating_sub(MAX_HISTORY_URLS);
        if excess == 0 {
            return;
        }
        let mut order: Vec<usize> = (0..self.urls.len()).collect();
        order.sort_by_key(|i| (self.urls[*i].frecency, self.urls[*i].last_visit_at, *i));
        let mut drop = vec![false; self.urls.len()];
        for i in &order[..excess] {
            drop[*i] = true;
        }
        let mut index = 0;
        self.urls.retain(|_| {
            index += 1;
            !drop[index - 1]
        });
    }

    /// Update the title of the entry for `url` (no-op if absent or `title` empty).
    pub fn set_title(&mut self, url: &str, title: &str) {
        self.update_title(url, title);
    }

    /// Like [`History::set_title`], returning whether anything changed.
    pub fn update_title(&mut self, url: &str, title: &str) -> bool {
        if title.trim().is_empty() {
            return false;
        }
        match self.urls.iter_mut().find(|u| u.url == url) {
            Some(e) if e.title != title => {
                e.title = title.to_string();
                true
            }
            _ => false,
        }
    }

    pub fn remove(&mut self, url: &str) {
        self.urls.retain(|u| u.url != url);
    }

    pub fn clear(&mut self) {
        self.urls.clear();
    }

    /// Entry for an exact URL.
    pub fn get(&self, url: &str) -> Option<&HistoryUrl> {
        self.urls.iter().find(|u| u.url == url)
    }

    /// Entries matching `query` (fuzzy over title/url; empty query = most recent), best first.
    /// Score = weighted fuzzy match + 0.25 × frecency (at `now`) normalized by the best frecency.
    pub fn search(&self, query: &str, limit: usize, now: Millis) -> Vec<&HistoryUrl> {
        let q = crate::omnibox::fold(query.trim());
        if q.is_empty() {
            let mut all: Vec<&HistoryUrl> = self.urls.iter().collect();
            all.sort_by(|a, b| b.last_visit_at.cmp(&a.last_visit_at).then_with(|| a.url.cmp(&b.url)));
            all.truncate(limit);
            return all;
        }
        let mut matched: Vec<(f32, i32, &HistoryUrl)> = self
            .urls
            .iter()
            .filter_map(|u| crate::omnibox::match_page_folded(&q, &u.title, &u.url).map(|s| (s, frecency(u, now), u)))
            .collect();
        let max_f = matched.iter().map(|m| m.1).max().unwrap_or(0).max(1) as f32;
        let mut scored: Vec<(f32, &HistoryUrl)> =
            matched.drain(..).map(|(s, f, u)| (s + 0.25 * (f as f32 / max_f), u)).collect();
        scored.sort_by(|a, b| {
            b.0.partial_cmp(&a.0)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| b.1.last_visit_at.cmp(&a.1.last_visit_at))
                .then_with(|| a.1.url.cmp(&b.1.url))
        });
        scored.into_iter().take(limit).map(|(_, u)| u).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_visits_and_counts() {
        let mut h = History::default();
        h.record_visit("https://a.com/", "A", Transition::Typed, 1000);
        h.record_visit("https://a.com/", "", Transition::Link, 2000);
        h.record_visit("sta://settings/", "S", Transition::Typed, 2000);
        h.record_visit("about:blank", "", Transition::Link, 2000);
        assert_eq!(h.urls.len(), 1);
        let e = &h.urls[0];
        assert_eq!((e.visit_count, e.typed_count, e.last_visit_at), (2, 1, 2000));
        assert_eq!(e.title, "A");
        assert_eq!(e.frecency, 300); // 100×2 + 100×1
        assert!(h.update_title("https://a.com/", "A2"));
        assert!(!h.update_title("https://a.com/", "A2"));
        assert!(!h.update_title("https://missing/", "x"));
        h.set_title("https://a.com/", "");
        assert_eq!(h.get("https://a.com/").unwrap().title, "A2");
    }

    #[test]
    fn frecency_buckets_and_factor() {
        let mut e = HistoryUrl { url: "https://x/".into(), visit_count: 1, ..Default::default() };
        e.visits.push(Visit { at: 0, transition: Transition::Link });
        assert_eq!(frecency(&e, 3 * DAY), 100);
        assert_eq!(frecency(&e, 10 * DAY), 70);
        assert_eq!(frecency(&e, 20 * DAY), 50);
        assert_eq!(frecency(&e, 60 * DAY), 30);
        assert_eq!(frecency(&e, 365 * DAY), 10);
        e.visits[0].transition = Transition::Redirect;
        assert_eq!(frecency(&e, 0), 0);
        e.visits[0].transition = Transition::Bookmark;
        assert_eq!(frecency(&e, 0), 120);
        // 20 visits, 10 sampled → factor 2
        let mut h = History::default();
        for i in 0..20 {
            h.record_visit("https://y/", "", Transition::Link, i);
        }
        let y = h.get("https://y/").unwrap();
        assert_eq!(y.visits.len(), MAX_VISITS_PER_URL);
        assert_eq!(y.visit_count, 20);
        assert_eq!(y.frecency, 2000);
    }

    #[test]
    fn eviction_drops_lowest_frecency() {
        let mut h = History::default();
        for i in 0..MAX_HISTORY_URLS {
            h.urls.push(HistoryUrl { url: format!("https://e{i}.com/"), frecency: 100, last_visit_at: 5, visit_count: 1, ..Default::default() });
        }
        h.urls[7].frecency = 1;
        h.record_visit("https://new.com/", "", Transition::Typed, 10);
        assert_eq!(h.urls.len(), MAX_HISTORY_URLS);
        assert!(h.get("https://e7.com/").is_none());
        assert!(h.get("https://new.com/").is_some());
    }

    #[test]
    fn search_ranks() {
        let mut h = History::default();
        h.record_visit("https://github.com/", "GitHub", Transition::Typed, 0);
        h.record_visit("https://github.com/", "GitHub", Transition::Typed, 1);
        h.record_visit("https://gitlab.com/", "GitLab", Transition::Link, 2);
        h.record_visit("https://example.com/digit", "Example", Transition::Link, 3);
        let r = h.search("git", 10, 10);
        assert_eq!(r[0].url, "https://github.com/");
        assert!(r.iter().any(|u| u.url == "https://gitlab.com/"));
        let recent = h.search("", 2, 10);
        assert_eq!(recent.len(), 2);
        assert_eq!(recent[0].url, "https://example.com/digit");
        assert!(h.search("zzzz", 10, 10).is_empty());
        h.remove("https://github.com/");
        assert!(h.get("https://github.com/").is_none());
        h.clear();
        assert!(h.urls.is_empty());
    }
}
