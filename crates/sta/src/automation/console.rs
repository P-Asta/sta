//! Console messages of web tabs for `console_messages` [owner: automation].
//!
//! `DisplayHandler::on_console_message` of the tab client forwards every message here; while
//! agent access is on (the endpoint is open) the last [`console::MAX_MESSAGES`] per tab are kept in
//! memory (never on disk or in `agent.log`). Turning access off drops them. Trusted UI pages are
//! never recorded. UI-thread only.

use crate::{browsers, tabs};
use sta_core::Id;
use sta_core::agent::console::{self, ConsoleBuffer};
use sta_core::agent::tools::ConsoleLevel;
use cef::LogSeverity;
use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};

/// Tabs with a buffer (the oldest buffer goes first beyond this).
const MAX_TABS: usize = 200;

#[derive(Default)]
struct Buffers {
    by_tab: HashMap<Id, ConsoleBuffer>,
    order: VecDeque<Id>,
}

thread_local! {
    static BUFFERS: RefCell<Buffers> = RefCell::new(Buffers::default());
}

fn level_of(level: LogSeverity) -> ConsoleLevel {
    if level == LogSeverity::ERROR || level == LogSeverity::FATAL {
        ConsoleLevel::Error
    } else if level == LogSeverity::WARNING {
        ConsoleLevel::Warning
    } else if level == LogSeverity::VERBOSE {
        ConsoleLevel::Debug
    } else {
        ConsoleLevel::Info
    }
}

/// `DisplayHandler::on_console_message` of a tab browser.
pub fn on_message(browser_id: i32, level: LogSeverity, message: &str, source: &str, line: i32) {
    if !super::session::endpoint_open() || browsers::is_ui_browser(browser_id) {
        return;
    }
    let Some(tab) = tabs::tab_for_browser(browser_id) else { return };
    BUFFERS.with(|b| {
        let mut b = b.borrow_mut();
        if !b.by_tab.contains_key(&tab) {
            b.order.push_back(tab);
            while b.order.len() > MAX_TABS {
                if let Some(old) = b.order.pop_front() {
                    b.by_tab.remove(&old);
                }
            }
        }
        b.by_tab.entry(tab).or_default().push(level_of(level), message, source, line.max(0) as u32);
    });
}

/// The tool's view of a tab's messages: `(listing, kept, dropped)`.
pub fn listing(tab: Id, level: ConsoleLevel, limit: usize, max_tokens: usize) -> (console::ConsoleListing, usize, u64) {
    BUFFERS.with(|b| {
        let b = b.borrow();
        match b.by_tab.get(&tab) {
            Some(buffer) => (console::listing(buffer, level, limit, max_tokens), buffer.len(), buffer.dropped),
            None => (console::listing(&ConsoleBuffer::default(), level, limit, max_tokens), 0, 0),
        }
    })
}

pub fn forget_tab(tab: Id) {
    BUFFERS.with(|b| {
        let mut b = b.borrow_mut();
        b.by_tab.remove(&tab);
        b.order.retain(|t| *t != tab);
    });
}

/// Access off / shutdown.
pub fn clear() {
    BUFFERS.with(|b| *b.borrow_mut() = Buffers::default());
}

pub fn debug_snapshot() -> serde_json::Value {
    BUFFERS.with(|b| {
        let b = b.borrow();
        serde_json::json!({ "tabs": b.by_tab.len(), "messages": b.by_tab.values().map(|x| x.len()).sum::<usize>() })
    })
}
