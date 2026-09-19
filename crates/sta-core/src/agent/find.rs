//! `page_find`: plain-text or regular-expression search over a page's readable text (the same
//! text `page_text` pages through, so a match offset is a valid `page_text` `offset`).
//!
//! Patterns run in Rust's `regex` (finite automata, size-limited): a hostile pattern can't hang
//! the page or the browser the way a backtracking JavaScript regex could.

use regex::{Regex, RegexBuilder};

/// Characters of context shown before and after a match.
pub const CONTEXT_CHARS: usize = 60;
/// Matches counted at most (the total says "at least" beyond this).
pub const MAX_COUNT: usize = 10_000;
/// Default and largest `maxResults`.
pub const DEFAULT_RESULTS: usize = 20;
pub const MAX_RESULTS: usize = 100;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FindMatch {
    /// Character offset of the match in the searched text.
    pub offset: usize,
    /// Length of the match in characters.
    pub len: usize,
    /// The match with up to [`CONTEXT_CHARS`] characters on each side, whitespace collapsed.
    pub context: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FindResult {
    pub matches: Vec<FindMatch>,
    /// Matches counted (at most [`MAX_COUNT`]).
    pub total: usize,
    /// Counting stopped at [`MAX_COUNT`].
    pub capped: bool,
}

/// The search pattern: `text` (escaped) or `regex`, exactly one; case-insensitive unless
/// `case_sensitive`.
pub fn pattern(text: Option<&str>, regex: Option<&str>, case_sensitive: bool) -> Result<Regex, String> {
    let source = match (text, regex) {
        (Some(t), None) if !t.is_empty() => regex::escape(t),
        (None, Some(r)) if !r.is_empty() => r.to_string(),
        (Some(_), Some(_)) => return Err("give either text or regex, not both".into()),
        _ => return Err("give text or regex".into()),
    };
    if source.len() > 4000 {
        return Err("the pattern is too long".into());
    }
    RegexBuilder::new(&source)
        .case_insensitive(!case_sensitive)
        .size_limit(1 << 20)
        .dfa_size_limit(1 << 20)
        .build()
        .map_err(|e| {
            let text = e.to_string();
            // The regex crate's messages span several lines with a caret diagram.
            text.lines().map(str::trim).rfind(|l| !l.is_empty()).unwrap_or("invalid regular expression").to_string()
        })
}

fn collapse(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut space = false;
    for c in s.chars() {
        if c.is_whitespace() || c.is_control() {
            if !space {
                out.push(' ');
            }
            space = true;
        } else {
            out.push(c);
            space = false;
        }
    }
    out
}

/// Finds the matches of `re` in `haystack`: the first `max_results` with context, all of them (up
/// to [`MAX_COUNT`]) counted. Empty matches are skipped.
pub fn find(haystack: &str, re: &Regex, max_results: usize) -> FindResult {
    let mut matches = Vec::new();
    let (mut total, mut capped) = (0usize, false);
    // Running byte → char offset conversion (matches come in ascending order).
    let (mut last_byte, mut last_char) = (0usize, 0usize);
    for m in re.find_iter(haystack) {
        if m.start() == m.end() {
            continue;
        }
        if total >= MAX_COUNT {
            capped = true;
            break;
        }
        total += 1;
        if matches.len() >= max_results {
            continue;
        }
        last_char += haystack[last_byte..m.start()].chars().count();
        last_byte = m.start();
        let len = m.as_str().chars().count();
        let before: String = {
            let head = &haystack[..m.start()];
            let chars: Vec<char> = head.chars().rev().take(CONTEXT_CHARS).collect();
            chars.into_iter().rev().collect()
        };
        let after: String = haystack[m.end()..].chars().take(CONTEXT_CHARS).collect();
        let lead = if last_char > CONTEXT_CHARS { "…" } else { "" };
        let tail = if haystack[m.end()..].chars().nth(CONTEXT_CHARS).is_some() { "…" } else { "" };
        let context = format!("{lead}{}{}{}{tail}", collapse(&before).trim_start(), collapse(m.as_str()), collapse(&after).trim_end());
        matches.push(FindMatch { offset: last_char, len, context });
    }
    FindResult { matches, total, capped }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_text_is_escaped_and_case_insensitive() {
        let re = pattern(Some("a.b (c)"), None, false).unwrap();
        let r = find("xx A.B (C) yy a-b (c)", &re, 10);
        assert_eq!(r.total, 1);
        assert_eq!(r.matches[0].offset, 3);
        assert_eq!(r.matches[0].len, 7);
        let exact = pattern(Some("A.B"), None, true).unwrap();
        assert_eq!(find("a.b A.B", &exact, 10).matches[0].offset, 4);
    }

    #[test]
    fn offsets_are_characters_and_context_is_trimmed() {
        let text = format!("{}\n\n찾는 단어 here, and 단어 again{}", "가".repeat(100), " tail".repeat(30));
        let re = pattern(Some("단어"), None, false).unwrap();
        let r = find(&text, &re, 1);
        assert_eq!(r.total, 2, "counted beyond max_results");
        assert_eq!(r.matches.len(), 1);
        let m = &r.matches[0];
        assert_eq!(m.offset, 105);
        assert_eq!(text.chars().skip(m.offset).take(m.len).collect::<String>(), "단어");
        assert!(m.context.starts_with('…') && m.context.contains("가 찾는 단어 here") && !m.context.contains('\n'), "{}", m.context);
        assert!(m.context.chars().count() <= 2 * CONTEXT_CHARS + 2 + m.len);
    }

    #[test]
    fn regexes_and_errors() {
        let re = pattern(None, Some(r"order #\d{4}"), false).unwrap();
        let r = find("Order #1234 and order #99 and ORDER #5678", &re, 10);
        assert_eq!(r.matches.iter().map(|m| m.offset).collect::<Vec<_>>(), vec![0, 30]);
        assert!(pattern(None, Some("("), false).is_err());
        assert!(pattern(None, Some(r"(a)\1"), false).is_err(), "no backreferences");
        assert!(pattern(Some("a"), Some("b"), false).is_err());
        assert!(pattern(None, None, false).is_err());
        assert!(pattern(Some(""), None, false).is_err());
        // Empty matches are skipped.
        let empty = pattern(None, Some("x*"), false).unwrap();
        assert_eq!(find("abc", &empty, 10).total, 0);
    }

    #[test]
    fn counting_is_capped() {
        let text = "a ".repeat(MAX_COUNT + 50);
        let re = pattern(Some("a"), None, false).unwrap();
        let r = find(&text, &re, 3);
        assert_eq!((r.total, r.capped, r.matches.len()), (MAX_COUNT, true, 3));
    }
}
