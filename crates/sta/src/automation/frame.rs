//! The agent frame [owner: automation] (docs/MCP.md "Seeing what agents do").
//!
//! A tab an agent is acting on — agent-controlled (`guards`) by a session that is still
//! connected — gets a 2 px agent-colored frame: its wrapper panel's background, which shows
//! through the wrapper's 2 px inset (tabs.rs asks [`wrapper_color`] whenever it colors a wrapper,
//! its tab view's background, or the ring of the pane's rounded corner masks in rounded.rs, so
//! the frame follows the rounded content card).
//! The frame goes away when the user takes the tab back (real key input), presses Stop, turns
//! access off, or the session ends. A tab in page fullscreen or in Peek has no inset, so no frame.
//!
//! Self-contained on purpose: the only coupling to the content layout is the wrapper color hook.

use super::{guards, session};
use crate::{controller, tabs, task, window};
use sta_core::Id;
use std::cell::{Cell, RefCell};
use std::collections::BTreeSet;

/// Agent color (ARGB): orange, a little lighter on dark frames (`--agent` in ui/common/agent.css).
pub const AGENT_ARGB_LIGHT: u32 = 0xFFE8641B;
pub const AGENT_ARGB_DARK: u32 = 0xFFFF9150;

thread_local! {
    static FRAMED: RefCell<BTreeSet<Id>> = const { RefCell::new(BTreeSet::new()) };
    static REFRESH_POSTED: Cell<bool> = const { Cell::new(false) };
}

pub fn agent_color() -> u32 {
    if window::is_dark() { AGENT_ARGB_DARK } else { AGENT_ARGB_LIGHT }
}

/// The color a tab's wrapper shows: the agent color while the tab is framed, else `base`.
pub fn wrapper_color(tab: Id, base: u32) -> u32 {
    if FRAMED.with(|f| f.borrow().contains(&tab)) { agent_color() } else { base }
}

pub fn is_framed(tab: Id) -> bool {
    FRAMED.with(|f| f.borrow().contains(&tab))
}

/// Something that decides the frames changed (control, sessions, Stop): recompute once, posted.
pub fn schedule_refresh() {
    if REFRESH_POSTED.replace(true) {
        return;
    }
    task::post_ui(|| {
        REFRESH_POSTED.set(false);
        refresh();
    });
}

fn refresh() {
    let paused = controller::with_store(|s| s.agents_paused()).unwrap_or(true);
    let wanted: BTreeSet<Id> = if paused {
        BTreeSet::new()
    } else {
        guards::controlled_tabs().into_iter().filter(|(_, s)| session::is_active_session(*s)).map(|(t, _)| t).collect()
    };
    let changed: Vec<Id> = FRAMED.with(|f| {
        let mut f = f.borrow_mut();
        let changed = f.symmetric_difference(&wanted).copied().collect();
        *f = wanted;
        changed
    });
    for tab in changed {
        log_debug!("agent frame of tab {tab}: {}", if is_framed(tab) { "on" } else { "off" });
        tabs::refresh_wrapper_color(tab);
    }
}

pub fn clear() {
    FRAMED.with(|f| f.borrow_mut().clear());
}

pub fn debug_snapshot() -> serde_json::Value {
    serde_json::json!(FRAMED.with(|f| f.borrow().iter().copied().collect::<Vec<_>>()))
}
