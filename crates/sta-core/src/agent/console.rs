//! `console_messages`: a ring buffer of a tab's console messages (filled by the shell's
//! `DisplayHandler::on_console_message` while agent access is on) and the tool's output lines.

use super::text::{MAX_CHARS, estimate_tokens, quote};
use super::tools::ConsoleLevel;
use std::collections::VecDeque;

/// Messages kept per tab.
pub const MAX_MESSAGES: usize = 500;
/// Longest message kept (chars).
pub const MAX_MESSAGE_CHARS: usize = 2_000;
/// Default `limit`.
pub const DEFAULT_LIMIT: usize = 100;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConsoleEntry {
    pub level: ConsoleLevel,
    pub text: String,
    /// Script URL (may be empty).
    pub source: String,
    pub line: u32,
}

#[derive(Debug, Clone, Default)]
pub struct ConsoleBuffer {
    entries: VecDeque<ConsoleEntry>,
    /// Messages pushed out of the buffer so far.
    pub dropped: u64,
}

impl ConsoleBuffer {
    pub fn push(&mut self, level: ConsoleLevel, text: &str, source: &str, line: u32) {
        let text: String = text.chars().take(MAX_MESSAGE_CHARS).collect();
        let source: String = source.chars().take(500).collect();
        self.entries.push_back(ConsoleEntry { level, text, source, line });
        while self.entries.len() > MAX_MESSAGES {
            self.entries.pop_front();
            self.dropped += 1;
        }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn entries(&self) -> impl Iterator<Item = &ConsoleEntry> {
        self.entries.iter()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConsoleListing {
    /// One line per message, oldest first.
    pub lines: Vec<String>,
    /// Messages at or above the level.
    pub matching: usize,
    /// Lines left out by `limit` or the token budget (the oldest ones).
    pub omitted: usize,
}

/// The newest `limit` messages at or above `level`, oldest first, within `max_tokens`.
pub fn listing(buffer: &ConsoleBuffer, level: ConsoleLevel, limit: usize, max_tokens: usize) -> ConsoleListing {
    let matching: Vec<&ConsoleEntry> = buffer.entries().filter(|e| e.level >= level).collect();
    let mut lines: Vec<String> = Vec::new();
    let (mut tokens, mut chars) = (0usize, 0usize);
    for e in matching.iter().rev().take(limit.max(1)) {
        let mut line = format!("- [{}] {}", e.level.as_str(), quote(&e.text, MAX_MESSAGE_CHARS));
        if !e.source.is_empty() {
            line.push_str(&format!(" ({}:{})", quote(&e.source, 200), e.line));
        }
        let cost = estimate_tokens(&line) + 1;
        let length = line.chars().count() + 1;
        if tokens + cost > max_tokens || chars + length > MAX_CHARS {
            break;
        }
        tokens += cost;
        chars += length;
        lines.push(line);
    }
    lines.reverse();
    ConsoleListing { omitted: matching.len() - lines.len(), matching: matching.len(), lines }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ring_buffer_keeps_the_newest() {
        let mut b = ConsoleBuffer::default();
        for i in 0..(MAX_MESSAGES + 7) {
            b.push(ConsoleLevel::Info, &format!("m{i}"), "", 0);
        }
        assert_eq!((b.len(), b.dropped), (MAX_MESSAGES, 7));
        assert_eq!(b.entries().next().unwrap().text, "m7");
        b.push(ConsoleLevel::Error, &"x".repeat(5000), "https://a.example/app.js", 12);
        assert_eq!(b.entries().last().unwrap().text.chars().count(), MAX_MESSAGE_CHARS);
    }

    #[test]
    fn listing_filters_limits_and_budgets() {
        let mut b = ConsoleBuffer::default();
        b.push(ConsoleLevel::Debug, "debug one", "", 0);
        b.push(ConsoleLevel::Warning, "careful \"now\"\nline two", "https://a.example/app.js", 3);
        b.push(ConsoleLevel::Error, "Uncaught TypeError: x is undefined", "https://a.example/app.js", 9);
        b.push(ConsoleLevel::Info, "hello", "", 0);
        let all = listing(&b, ConsoleLevel::Debug, 100, 8000);
        assert_eq!((all.matching, all.omitted, all.lines.len()), (4, 0, 4));
        assert_eq!(all.lines[0], "- [debug] \"debug one\"");
        assert_eq!(all.lines[1], "- [warning] \"careful \\\"now\\\" line two\" (\"https://a.example/app.js\":3)");
        let warnings = listing(&b, ConsoleLevel::Warning, 100, 8000);
        assert_eq!(warnings.lines.len(), 2);
        assert!(warnings.lines[1].starts_with("- [error] \"Uncaught TypeError"));
        let last = listing(&b, ConsoleLevel::Debug, 1, 8000);
        assert_eq!((last.lines.len(), last.omitted), (1, 3));
        assert_eq!(last.lines[0], "- [info] \"hello\"");
        let tiny = listing(&b, ConsoleLevel::Debug, 100, 12);
        assert!(tiny.lines.len() < 4 && tiny.omitted == 4 - tiny.lines.len());
        assert_eq!(tiny.lines.last().map(String::as_str), Some("- [info] \"hello\""), "the newest survive the budget");
    }
}
