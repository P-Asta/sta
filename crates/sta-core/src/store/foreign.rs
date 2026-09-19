//! Chrome-created browsers (shell `foreign.rs`) and DevTools on sta pages.
//!
//! - `ForeignTabRequested`: a URL a Chrome-created browser wanted to load becomes a foreground
//!   Today tab after the active item, when [`urls::foreign_tab_verdict`] allows it; an undeclared
//!   extension page asks first (toast with Open). Both are rate limited — see [`FOREIGN_OPEN_MAX`].
//!   The verdict is about the **page**, never about who asked for it (which the shell cannot see:
//!   `urls::foreign_tab_verdict`), so the question names the page's owner as its owner.
//! - `ExtensionInstalled`: toast, and the id is remembered for 60 s.
//! - `ForeignBlocked`: toast.
//! - `ToggleDevTools` on a `sta://` page: refused with a toast (DevTools extensions could otherwise
//!   reach sta's internal pages), unless the shell allowed it (`STA_DEVTOOLS_INTERNAL=1`, debug
//!   builds only).

use super::*;
use crate::urls::{self, ForeignTabVerdict};
use std::collections::VecDeque;

/// An `ExtensionInstalled` id vouches for its pages this long.
pub const RECENT_INSTALL_MS: Millis = 60_000;

/// The window of the rate budget below.
pub const FOREIGN_RATE_WINDOW_MS: Millis = 10_000;
/// Tabs Chrome-created browsers may open within [`FOREIGN_RATE_WINDOW_MS`] (FINAL PLAN §2,
/// "Navigation": *"more than 3 adoptions in 10 s"*).
///
/// The budget is spent **after** [`urls::foreign_tab_verdict`], by what core actually does, not by
/// every URL a hidden window asked about:
/// - `Open` spends one slot — those are the adoptions the rule is about;
/// - `Ask` spends one slot of its own, [`FOREIGN_ASK_MAX`], so a window flood can't spam toasts
///   either, but leaves the tab budget alone: a page the user is only *asked* about, and never
///   approves, must not block the next legitimate window (an extension that offers a page sta
///   won't open on its own commonly also opens its welcome tab a moment later);
/// - `Refuse` spends nothing at all: an ignored `chrome://` or `javascript:` URL costs a cancelled
///   navigation and a WARN, and nothing the user can see.
///
/// Over budget, both paths do nothing beyond `foreign_flood_toast` below.
pub const FOREIGN_OPEN_MAX: usize = 3;
/// "An extension wants to open …" toasts within [`FOREIGN_RATE_WINDOW_MS`]; see
/// [`FOREIGN_OPEN_MAX`]. Lower than the tab budget: each of these is a question, and two unanswered
/// ones are already as much as a toast row can usefully say.
pub const FOREIGN_ASK_MAX: usize = 2;

/// Shown when a Chrome-created browser runs out of either budget, and by `ForeignBlocked`.
pub const RATE_LIMITED_TOAST: &str = "An extension keeps opening windows; sta blocked them";
pub const INCOGNITO_TOAST: &str = "sta has no private windows";
/// Longest extension name shown in a toast. The toast is one line of at most ~70 characters
/// (`ui/toast/toast.css`: 448 px inner width, no wrapping), so the longer message about extensions
/// another program added leaves less room for the name.
///
/// At these budgets every message below stays inside the ~70 characters one line holds (~56 with an
/// action button), which `toast_messages_fit_one_line` (`tests/scenarios_foreign.rs`) checks with a
/// name of exactly this length: a verbose — or hostile — manifest name must not be able to push the
/// sentence out of the pill.
const NAME_MAX_CHARS: usize = 48;
const NAME_MAX_CHARS_EXTERNAL: usize = 20;
/// The question is the only phase-1 toast with an action button, so ~60 px less room, and the only
/// one whose own chrome is 37 characters long. Its name comes **last**, so at the budget the name
/// ellipsizes and the sentence still reads whole (the earlier wording ended in " page", which a long
/// name cut off).
const NAME_MAX_CHARS_ASK: usize = 18;

pub const DEVTOOLS_REFUSED_TOAST: &str = "DevTools isn't available on sta pages";

/// Drops the timestamps that fell out of the window, records `now`, and answers whether it fits
/// inside `max` (an over-budget use is *not* recorded: a flood must not keep the window alive).
fn take_slot(slots: &mut VecDeque<Millis>, now: Millis, max: usize) -> bool {
    while slots.front().is_some_and(|t| now.saturating_sub(*t) >= FOREIGN_RATE_WINDOW_MS) {
        slots.pop_front();
    }
    if slots.len() >= max {
        return false;
    }
    slots.push_back(now);
    true
}

fn short_name(name: &str, max: usize) -> String {
    let name = name.trim();
    if name.chars().count() <= max {
        return name.to_string();
    }
    let mut s: String = name.chars().take(max.saturating_sub(1)).collect();
    s.push('…');
    s
}

impl Store {
    /// Debug builds only (`STA_DEVTOOLS_INTERNAL=1`): DevTools may open on `sta://` pages.
    pub fn set_devtools_on_internal_pages(&mut self, allowed: bool) {
        self.rt.devtools_internal = allowed;
    }

    /// `ToggleDevTools` for `tab`: `false` (with a toast) when it shows a `sta://` page.
    pub(super) fn devtools_allowed(&mut self, tab: Id) -> bool {
        let internal = self.tab(tab).is_some_and(|t| urls::is_internal(&t.url)) || self.rt.tabs.get(&tab).is_some_and(|r| r.internal);
        if internal && !self.rt.devtools_internal {
            self.toast(DEVTOOLS_REFUSED_TOAST, None);
            return false;
        }
        true
    }

    /// The flood toast, at most once per [`FOREIGN_RATE_WINDOW_MS`]: a window that keeps asking
    /// must not be able to keep replacing the toast its predecessor caused.
    fn foreign_flood_toast(&mut self, now: Millis) {
        if self.rt.foreign_blocked_at.is_some_and(|at| now.saturating_sub(at) < FOREIGN_RATE_WINDOW_MS) {
            return;
        }
        self.rt.foreign_blocked_at = Some(now);
        self.toast(RATE_LIMITED_TOAST, None);
    }

    pub(super) fn handle_foreign(&mut self, cmd: Command, now: Millis, fx: &mut Vec<Effect>) {
        self.rt.recent_installs.retain(|_, at| now.saturating_sub(*at) <= RECENT_INSTALL_MS);
        match cmd {
            Command::ForeignTabRequested { url, extension } => {
                let url = url.trim().to_string();
                let recent_id = urls::extension_url_parts(&url).is_some_and(|(id, _)| self.rt.recent_installs.contains_key(&id));
                match urls::foreign_tab_verdict(&url, extension.as_ref(), recent_id) {
                    // The budget is spent here, on the verdict, not on the request: see
                    // `FOREIGN_OPEN_MAX`.
                    ForeignTabVerdict::Open => {
                        if !take_slot(&mut self.rt.foreign_opens, now, FOREIGN_OPEN_MAX) {
                            self.foreign_flood_toast(now);
                            return;
                        }
                        let anchor = self.active_item();
                        let id = self.new_tab_item(&url, None, now);
                        self.place_in_today(id, anchor);
                        self.activate(id, now, fx);
                    }
                    ForeignTabVerdict::Ask { id } => {
                        if !take_slot(&mut self.rt.foreign_asks, now, FOREIGN_ASK_MAX) {
                            self.foreign_flood_toast(now);
                            return;
                        }
                        let name = extension.as_ref().filter(|e| e.id == id).map(|e| short_name(&e.name, NAME_MAX_CHARS_ASK)).filter(|n| !n.is_empty());
                        // The name is the page's **owner**, never the window that asked: sta cannot
                        // see who asked (see `urls::foreign_tab_verdict`), so the wording must not
                        // imply it ("a page of X", not "X wants …").
                        let message = match name {
                            Some(name) => format!("An extension wants to open a page of {name}"),
                            None => "An extension wants to open an extension page".to_string(),
                        };
                        let open = Command::OpenUrl { url, target: OpenTarget::NewTab, opener: None };
                        self.toast(message, Some(ToastAction { label: "Open".into(), command: Box::new(open) }));
                    }
                    ForeignTabVerdict::Refuse => {}
                }
            }
            Command::ExtensionInstalled { id, name, external } => {
                if !urls::is_extension_id(&id) {
                    return;
                }
                self.rt.recent_installs.insert(id, now);
                let name = short_name(&name, if external { NAME_MAX_CHARS_EXTERNAL } else { NAME_MAX_CHARS });
                let name = if name.is_empty() { "An extension".to_string() } else { name };
                if external {
                    self.toast(format!("{name} added by another program, off until you allow it"), None);
                } else {
                    // sta has no extension toolbar, so the one thing a fresh install has to say is
                    // *how to use it* (FINAL PLAN §2: "phase 3 appends · Ctrl+E").
                    self.toast(format!("{name} added · Ctrl+E"), None);
                }
            }
            // The shell sends `Incognito` (only it can see a private request context). `RateLimited`
            // is the same toast core raises from its own budget above; the shell keeps it for a
            // flood it stops before core ever hears about it (`foreign.rs`, `HANDLE_MAX`).
            Command::ForeignBlocked { reason } => match reason {
                ForeignBlockReason::RateLimited => self.foreign_flood_toast(now),
                ForeignBlockReason::Incognito => self.toast(INCOGNITO_TOAST, None),
            },
            _ => {}
        }
    }
}
