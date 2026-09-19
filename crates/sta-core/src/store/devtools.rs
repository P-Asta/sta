//! Docked DevTools rules (FINAL PLAN §3 "Behaviour", "Core").
//!
//! Core owns *whether* DevTools are open for a tab and *how* they are presented; the shell owns the
//! views, the frontend browser and the protocol session (`crates/sta/src/devtools.rs`).
//!
//! - Nothing is persisted: [`crate::Store`]'s two sets live in the runtime half only, like Chrome,
//!   which never restores an open DevTools window.
//! - `ToggleDevTools` opens (docked) or closes. `FocusDevTools` focuses an open one — the shell
//!   resolves Ctrl+Shift+I's third step (close when the frontend has focus) itself, because only it
//!   knows what has focus (`keyboard.rs`).
//! - Undock (`UndockDevTools`, or the frontend's own button as `DevToolsUndockRequested`) closes the
//!   docked frontend and reopens DevTools in CEF's own window. It lasts for that tab until DevTools
//!   close again (D8: per tab, per session, never persisted).
//! - A Peek page opens undocked: the Peek card shows the page alone.
//! - DevTools close with the tab's browser: closed, unloaded, archived, crashed (all of which end in
//!   `TabBrowserClosed`) or replaced (`Effect::ReplaceBrowser`, where core emits `CloseDevTools`
//!   *before* the replace so the page view is back in its wrapper first).
//! - `sta://` pages are refused in every build (SEC-3, D7) by [`Store::devtools_allowed`]
//!   (`store/foreign.rs`), with the debug-only `STA_DEVTOOLS_INTERNAL=1` override.
//! - `DevToolsLinkRequested` is the frontend's "open in new tab" (and its search-results variant):
//!   checked with [`urls::web_content_may_open`] like any link web content offers, external schemes
//!   go to the OS.

use super::*;
use crate::urls;

impl Store {
    /// Tabs whose DevTools are open (docked or undocked).
    pub fn devtools_open(&self) -> &BTreeSet<Id> {
        &self.rt.devtools_open
    }

    /// Whether `tab`'s DevTools are undocked (this session only).
    pub fn devtools_undocked(&self, tab: Id) -> bool {
        self.rt.devtools_undocked.contains(&tab)
    }

    /// How DevTools open for `tab`: docked unless undocked this session or the tab is in Peek
    /// (whose card shows the page alone).
    fn devtools_docked_for(&self, tab: Id) -> bool {
        !self.rt.devtools_undocked.contains(&tab) && self.rt.peek.as_ref().is_none_or(|p| p.tab.id != tab)
    }

    /// The tab's DevTools are gone (its browser closed, was replaced, or the shell reported the
    /// frontend closed): forget both bits of state.
    pub(super) fn devtools_gone(&mut self, tab: Id) {
        self.rt.devtools_open.remove(&tab);
        self.rt.devtools_undocked.remove(&tab);
    }

    /// `Effect::CloseDevTools` for a tab whose browser core is about to replace or destroy, so the
    /// shell puts the page view back before the new browser arrives. Emits nothing when DevTools
    /// are not open.
    pub(super) fn close_devtools_before(&mut self, tab: Id, fx: &mut Vec<Effect>) {
        if self.rt.devtools_open.contains(&tab) {
            fx.push(Effect::CloseDevTools { tab });
        }
        self.devtools_gone(tab);
    }

    fn open_devtools(&mut self, tab: Id, fx: &mut Vec<Effect>) {
        self.rt.devtools_open.insert(tab);
        fx.push(Effect::OpenDevTools { tab, docked: self.devtools_docked_for(tab) });
    }

    pub(super) fn handle_devtools(&mut self, cmd: Command, now: Millis, fx: &mut Vec<Effect>) {
        match cmd {
            Command::ToggleDevTools => {
                let Some(tab) = self.live_target(None) else { return };
                if self.rt.devtools_open.contains(&tab) {
                    fx.push(Effect::CloseDevTools { tab });
                    self.devtools_gone(tab);
                } else if self.devtools_allowed(tab) {
                    self.open_devtools(tab, fx);
                }
            }
            Command::FocusDevTools => {
                let Some(tab) = self.live_target(None) else { return };
                if self.rt.devtools_open.contains(&tab) {
                    fx.push(Effect::FocusDevTools { tab });
                } else if self.devtools_allowed(tab) {
                    self.open_devtools(tab, fx);
                }
            }
            // The command-bar action and the frontend's own Undock button. Undocking an unopened
            // tab's DevTools opens them undocked (that is what the action promises).
            Command::UndockDevTools | Command::DevToolsUndockRequested { .. } => {
                let (asked, narrow) = match cmd {
                    Command::DevToolsUndockRequested { tab, narrow } => (Some(tab), narrow),
                    _ => (None, false),
                };
                let Some(tab) = self.live_target(asked) else { return };
                if !self.devtools_allowed(tab) {
                    return;
                }
                if self.rt.devtools_open.contains(&tab) {
                    fx.push(Effect::CloseDevTools { tab });
                }
                self.rt.devtools_undocked.insert(tab);
                self.rt.devtools_open.insert(tab);
                fx.push(Effect::OpenDevTools { tab, docked: false });
                // The shell could not dock: say why, or the DevTools window looks like a bug.
                if narrow {
                    self.toast("DevTools opened in their own window: this pane is too narrow to dock them", None);
                }
            }
            Command::DevToolsClosed { tab } => self.devtools_gone(tab),
            Command::InspectElement { tab, x, y } => {
                let Some(tab) = self.live_target(Some(tab)) else { return };
                if !self.devtools_allowed(tab) {
                    return;
                }
                if !self.rt.devtools_open.contains(&tab) {
                    self.open_devtools(tab, fx);
                }
                fx.push(Effect::InspectAt { tab, x, y });
            }
            // The frontend offered a link (openInNewTab) or a search (openSearchResultsInNewTab).
            // Same rule as any link web content offers.
            Command::DevToolsLinkRequested { tab, url, search } => {
                let url = url.trim().to_string();
                if url.is_empty() {
                    return;
                }
                if search {
                    let url = crate::omnibox::search_url(self.state.settings.search_engine, &self.state.settings.custom_search_url, &url);
                    self.open_url(url, OpenTarget::NewTab, self.state.items.contains_key(&tab).then_some(tab), false, now, fx);
                    return;
                }
                // External protocols go to the OS (`open_url` does that); everything else must be
                // something web content may open.
                if !urls::is_external_scheme(&url) && !urls::web_content_may_open(&url, None) {
                    return;
                }
                self.open_url(url, OpenTarget::NewTab, self.state.items.contains_key(&tab).then_some(tab), false, now, fx);
            }
            _ => {}
        }
    }
}
