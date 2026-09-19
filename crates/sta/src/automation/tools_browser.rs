//! Browser-level tools [owner: automation] (docs/MCP.md "Tools"): `request_tab_access`,
//! `history_search`, `downloads_list`.
//!
//! - `request_tab_access` asks the user (a tab prompt in the agent overlay) to share a tab the
//!   agent can't see; nothing about that tab (title, URL) is returned before the user shares it.
//! - `history_search` needs the `agentHistory` setting and lists only pages agents could open under
//!   the current policy (no sta pages, files, blocked hosts or — unless allowed — local-network
//!   hosts).
//! - `downloads_list` needs the `agentDownloads` setting: file names and states, never folders,
//!   paths or download URLs.

use super::session;
use super::tools::{self, Ctx, Output, err, invalid};
use crate::controller;
use sta_core::agent::channel::{Content, ToolError};
use sta_core::agent::policy::{self, UrlVerdict};
use sta_core::agent::tools::*;
use sta_core::agent::{AgentPromptKind, ErrorCode, listing, text};
use sta_core::{Id, Millis};
use serde_json::json;

fn now_ms() -> Millis {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as Millis).unwrap_or(0)
}

// ----------------------------------------------------------------------------------- request_tab_access

pub async fn request_tab_access(ctx: &Ctx, args: RequestTabAccessArgs) -> Result<Output, ToolError> {
    let reason = args.reason.trim();
    if reason.is_empty() {
        return Err(invalid("reason can't be empty: tell the user what you need the tab for"));
    }
    let reason: String = reason.chars().take(300).collect();
    struct Facts {
        tab: Option<Id>,
        exists: bool,
        url: String,
        in_scope: bool,
        other_pending: bool,
    }
    let Some(facts) = controller::with_store(|s| {
        let tab = args.tab.or_else(|| s.focused_tab());
        let found = tab.and_then(|t| s.tab(t));
        Facts {
            tab,
            exists: found.is_some(),
            url: found.map(|t| t.url.clone()).unwrap_or_default(),
            in_scope: tab.is_some_and(|t| policy::in_scope(s.settings().agent_scope, s.agent_tabs(), t)),
            other_pending: s.agent_prompts().iter().any(|p| matches!(&p.kind, AgentPromptKind::Tab { session, tab: t, .. } if *session == ctx.session && Some(*t) != tab)),
        }
    }) else {
        return Err(err(ErrorCode::BrowserNotRunning, "sta is shutting down"));
    };
    let Some(tab) = facts.tab else {
        return Err(err(ErrorCode::NoSuchTab, "The user isn't looking at a tab right now").with_hint("Ask the user which tab to share, or pass its id."));
    };
    if !facts.exists {
        return Err(err(ErrorCode::NoSuchTab, format!("There is no tab {tab}")));
    }
    // Tabs agents could never use aren't worth a prompt (nothing about them is revealed).
    let settings = tools::settings().unwrap_or_default();
    if let Err(e) = policy::check_url(&settings, &facts.url) {
        return Err(match e.code {
            ErrorCode::InternalPage => err(ErrorCode::InternalPage, format!("Tab {tab} shows a sta page, which agents can't use")),
            ErrorCode::SiteBlocked => err(ErrorCode::SiteBlocked, format!("Tab {tab} is on a site the user blocked for agents")),
            code => err(code, format!("Tab {tab} shows a page agents can't use")),
        });
    }
    if !facts.in_scope {
        if facts.other_pending {
            return Err(err(ErrorCode::Busy, "Your earlier tab request is still waiting for the user").with_hint("Wait for the user to answer it (check tabs_list), then ask again."));
        }
        session::take_action_token(ctx.conn)?;
        session::ensure_tab_access(ctx.session, tab, &reason, ctx.deadline).await?;
    }
    session::set_current_tab(ctx.conn, tab);
    let (title, url) = controller::with_store(|s| s.tab(tab).map(|t| (t.title.clone(), t.url.clone()))).flatten().unwrap_or_default();
    let site = match policy::check_url(&settings, &url) {
        Ok(UrlVerdict::Web { site }) => Some(site),
        _ => None,
    };
    let n = tools::nonce();
    let head = if facts.in_scope { format!("You can already use tab {tab}. It is now your current tab.") } else { format!("The user shared tab {tab} with you. It is now your current tab.") };
    let body = format!("{head}\n{}", text::untrusted(&n, &format!("tab {tab} {} {}", text::quote(&title, 200), text::quote(&url, 400))));
    Ok(Output { content: vec![Content::Text { text: body }], structured: Some(json!({ "tab": tab, "shared": true, "alreadyShared": facts.in_scope })), tab: Some(tab), site })
}

// ----------------------------------------------------------------------------------- history_search

pub fn history_search(_ctx: &Ctx, args: HistorySearchArgs) -> Result<Output, ToolError> {
    let settings = tools::settings().unwrap_or_default();
    if !settings.agent_history {
        return Err(err(ErrorCode::HistoryDisabled, "Browsing history is off for agents in sta Settings"));
    }
    let query: String = args.query.as_deref().unwrap_or_default().trim().chars().take(200).collect();
    let limit = listing::limit(args.limit);
    let now = now_ms();
    let Some(entries) = controller::with_store(|s| {
        s.history().search(&query, 2_000, now).into_iter().filter(|u| listing::history_visible(&settings, &u.url)).take(limit).cloned().collect::<Vec<_>>()
    }) else {
        return Err(err(ErrorCode::BrowserNotRunning, "sta is shutting down"));
    };
    let what = if query.is_empty() { "most recent pages".to_string() } else { format!("pages matching {}", text::quote(&query, 200)) };
    let out = if entries.is_empty() {
        format!("No history entries for {what} (pages agents can't open are never listed).")
    } else {
        let n = tools::nonce();
        let lines: Vec<String> = entries.iter().map(|e| listing::history_line(e, now)).collect();
        format!("{} history entr{} for {what}, best match first:\n{}", entries.len(), if entries.len() == 1 { "y" } else { "ies" }, text::untrusted(&n, &lines.join("\n")))
    };
    Ok(Output { content: vec![Content::Text { text: out }], structured: Some(json!({ "count": entries.len() })), tab: None, site: None })
}

// ----------------------------------------------------------------------------------- downloads_list

pub fn downloads_list(_ctx: &Ctx, args: DownloadsListArgs) -> Result<Output, ToolError> {
    let settings = tools::settings().unwrap_or_default();
    if !settings.agent_downloads {
        return Err(err(ErrorCode::DownloadsDisabled, "The downloads list is off for agents in sta Settings"));
    }
    let limit = listing::limit(args.limit);
    let now = now_ms();
    let Some((mut downloads, held)) = controller::with_store(|s| {
        let ui = s.ui_state();
        (ui.downloads, ui.agent.held_downloads)
    }) else {
        return Err(err(ErrorCode::BrowserNotRunning, "sta is shutting down"));
    };
    downloads.sort_by(|a, b| b.started_at.cmp(&a.started_at).then(b.id.cmp(&a.id)));
    let mut lines: Vec<String> = Vec::new();
    // Held downloads haven't started yet: they may not be in the list.
    for h in held.iter().filter(|h| !downloads.iter().any(|d| d.id == h.id)) {
        lines.push(format!("- download {}: {} waiting for the user to keep or discard it", h.id, text::quote(&h.file_name, 200)));
    }
    for d in &downloads {
        let is_held = held.iter().any(|h| h.id == d.id);
        lines.push(listing::download_line(d, is_held, now));
    }
    let total = lines.len();
    lines.truncate(limit);
    let out = if lines.is_empty() {
        "No downloads.".to_string()
    } else {
        let n = tools::nonce();
        let more = if total > lines.len() { format!(" ({} older not shown)", total - lines.len()) } else { String::new() };
        format!("{} download{}, newest first{more}:\n{}", lines.len(), if lines.len() == 1 { "" } else { "s" }, text::untrusted(&n, &lines.join("\n")))
    };
    Ok(Output { content: vec![Content::Text { text: out }], structured: Some(json!({ "count": lines.len(), "total": total })), tab: None, site: None })
}
