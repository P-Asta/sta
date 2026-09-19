//! AI agents (MCP): sessions, approval prompts, agent tabs, activity and held downloads
//! (docs/MCP.md). Runtime only: nothing here is saved except settings changes ("Always" answers).

use super::*;
use crate::agent::policy;
use crate::agent::{AgentActivityView, AgentClientInfo, AgentHeldDownloadView, AgentPromptKind, AgentPromptView, AgentSessionView, AgentView};
use std::collections::VecDeque;

/// Activity entries kept.
const MAX_ACTIVITY: usize = 5;
/// Prompts kept (older ones are denied).
const MAX_PROMPTS: usize = 20;
/// Held downloads kept (older ones are discarded).
const MAX_HELD_DOWNLOADS: usize = 20;
/// Trusted clients kept.
const MAX_TRUSTED_CLIENTS: usize = 50;
/// A chip click right after the panel closed on focus loss is the click that closed it.
const PANEL_TOGGLE_GUARD_MS: Millis = 400;

#[derive(Debug, Default)]
pub(super) struct AgentRuntime {
    paused: bool,
    sessions: Vec<AgentSessionView>,
    prompts: Vec<AgentPromptView>,
    activity: VecDeque<AgentActivityView>,
    held_downloads: Vec<AgentHeldDownloadView>,
    /// Tabs in agent scope: opened by agents (`opened`) or shared by the user.
    pub(super) tabs: BTreeSet<Id>,
    /// Tabs agents opened themselves (or popups of them): the only tabs they may close.
    opened: BTreeSet<Id>,
    /// "Allow for this session" site answers.
    session_sites: BTreeMap<u64, BTreeSet<String>>,
    emitted_endpoint: Option<bool>,
    /// The activity panel is open (topbar chip).
    panel_open: bool,
    /// When the panel last closed because another browser took focus.
    panel_closed_on_focus_loss: Option<Millis>,
    /// Last agent overlay presentation sent to the shell: `(prompt, first prompt id)`.
    emitted_overlay: Option<(bool, Option<u64>)>,
}

fn same_client(c: &AgentTrustedClient, client: &AgentClientInfo) -> bool {
    match (client.exe.as_deref(), client.signer.as_deref()) {
        (Some(exe), Some(signer)) => client.verified && c.exe.eq_ignore_ascii_case(exe) && c.signer == signer,
        _ => false,
    }
}

impl Store {
    // ------------------------------------------------------------------------------ queries

    /// Tabs in agent scope (opened by agents or shared with them).
    pub fn agent_tabs(&self) -> &BTreeSet<Id> {
        &self.rt.agent.tabs
    }

    /// The tab was opened by an agent (it may close it).
    pub fn agent_opened_tab(&self, tab: Id) -> bool {
        self.rt.agent.opened.contains(&tab)
    }

    /// Waiting approval prompts, oldest first (the agent overlay shows the first).
    pub fn agent_prompts(&self) -> &[AgentPromptView] {
        &self.rt.agent.prompts
    }

    /// An approval prompt with this request id is waiting for the user.
    pub fn agent_prompt_pending(&self, id: u64) -> bool {
        self.rt.agent.prompts.iter().any(|p| p.id == id)
    }

    /// The user pressed Stop and hasn't resumed.
    pub fn agents_paused(&self) -> bool {
        self.rt.agent.paused
    }

    /// `site` is allowed for `session` (all sites, "Always", or "Allow for this session").
    pub fn agent_site_approved(&self, session: u64, site: &str) -> bool {
        let empty = BTreeSet::new();
        let sites = self.rt.agent.session_sites.get(&session).unwrap_or(&empty);
        policy::site_approved(&self.state.settings, sites, site)
    }

    pub(super) fn agent_view(&self) -> AgentView {
        let a = &self.rt.agent;
        AgentView {
            paused: a.paused,
            sessions: a.sessions.clone(),
            prompts: a.prompts.clone(),
            activity: a.activity.iter().cloned().collect(),
            held_downloads: a.held_downloads.clone(),
            panel_open: a.panel_open,
            opened_tabs: self.archivable_agent_tabs().len(),
        }
    }

    /// Tabs agents opened that are still open in a Today list (directly or in a split).
    fn archivable_agent_tabs(&self) -> Vec<Id> {
        self.rt
            .agent
            .opened
            .iter()
            .copied()
            .filter(|t| self.tab(*t).is_some() && matches!(self.parent_of(*t), Some((super::tree::Parent::Today(_) | super::tree::Parent::Split(_), _))))
            .collect()
    }

    // ------------------------------------------------------------------------------ commands

    pub(super) fn handle_agent(&mut self, cmd: Command, now: Millis, fx: &mut Vec<Effect>) {
        match cmd {
            Command::AnswerAgentConnection { id, allow, remember } => {
                let Some(pos) = self.rt.agent.prompts.iter().position(|p| p.id == id && matches!(p.kind, AgentPromptKind::Connection { .. })) else {
                    return;
                };
                let prompt = self.rt.agent.prompts.remove(pos);
                if let AgentPromptKind::Connection { client } = prompt.kind
                    && allow
                    && remember
                    && client.verified
                    && let (Some(exe), Some(signer)) = (client.exe.clone(), client.signer.clone())
                {
                    let trusted = &mut self.state.settings.agent_trusted_clients;
                    if !trusted.iter().any(|c| same_client(c, &client)) {
                        trusted.push(AgentTrustedClient { name: client.display_name(), exe, signer, added_at: now });
                        if trusted.len() > MAX_TRUSTED_CLIENTS {
                            trusted.remove(0);
                        }
                        self.touch();
                    }
                }
                fx.push(Effect::AgentAnswer { id, allow });
                self.bump();
            }
            Command::AnswerSitePermission { id, allow, remember } => {
                let Some((session, site)) = self.rt.agent.prompts.iter().find(|p| p.id == id).and_then(|p| match &p.kind {
                    AgentPromptKind::Site { session, site, .. } => Some((*session, site.clone())),
                    _ => None,
                }) else {
                    return;
                };
                if allow {
                    self.rt.agent.session_sites.entry(session).or_default().insert(site.clone());
                    let sites = &mut self.state.settings.agent_allowed_sites;
                    if remember && !sites.contains(&site) && sites.len() < policy::MAX_HOST_ENTRIES {
                        sites.push(site.clone());
                        self.touch();
                    }
                }
                // Every waiting request of this session for the same site gets the same answer.
                let answered: Vec<u64> = self
                    .rt
                    .agent
                    .prompts
                    .iter()
                    .filter(|p| matches!(&p.kind, AgentPromptKind::Site { session: s, site: x, .. } if *s == session && *x == site))
                    .map(|p| p.id)
                    .collect();
                self.rt.agent.prompts.retain(|p| !answered.contains(&p.id));
                for pid in answered {
                    fx.push(Effect::AgentAnswer { id: pid, allow });
                }
                self.bump();
            }
            Command::StopAgents => {
                self.rt.agent.paused = true;
                self.deny_all_prompts(fx);
                fx.push(Effect::AgentDisconnect);
                self.bump();
            }
            Command::ResumeAgents if std::mem::take(&mut self.rt.agent.paused) => self.bump(),
            Command::ShareTabWithAgent { tab, shared } => {
                if self.tab(tab).is_none() {
                    return;
                }
                let changed = if shared { self.rt.agent.tabs.insert(tab) } else { self.rt.agent.tabs.remove(&tab) };
                if !shared {
                    self.rt.agent.opened.remove(&tab);
                }
                if changed {
                    self.bump();
                }
            }
            Command::ResolveAgentDownload { id, keep } => {
                let before = self.rt.agent.held_downloads.len();
                self.rt.agent.held_downloads.retain(|d| d.id != id);
                if self.rt.agent.held_downloads.len() != before {
                    fx.push(Effect::AgentDownload { id, keep });
                    self.bump();
                }
            }
            Command::AgentConnectionRequested { id, client } => {
                let a = &self.rt.agent;
                if self.state.settings.agent_access == AgentAccess::Off || a.paused {
                    fx.push(Effect::AgentAnswer { id, allow: false });
                    return;
                }
                if self.state.settings.agent_trusted_clients.iter().any(|c| same_client(c, &client)) {
                    fx.push(Effect::AgentAnswer { id, allow: true });
                    return;
                }
                if self.rt.agent.prompts.iter().any(|p| p.id == id) {
                    return;
                }
                self.push_prompt(AgentPromptView { id, kind: AgentPromptKind::Connection { client }, requested_at: now }, fx);
            }
            Command::AgentSessionStarted { session, client, access } => {
                self.rt.agent.sessions.retain(|s| s.session != session);
                self.rt.agent.sessions.push(AgentSessionView { session, client, access, started_at: now });
                self.bump();
            }
            Command::AgentSessionEnded { session } => {
                let before = self.rt.agent.sessions.len();
                self.rt.agent.sessions.retain(|s| s.session != session);
                self.rt.agent.session_sites.remove(&session);
                let prompts = self.rt.agent.prompts.len();
                self.rt.agent.prompts.retain(|p| !matches!(p.kind, AgentPromptKind::Site { session: s, .. } | AgentPromptKind::Tab { session: s, .. } if s == session));
                if self.rt.agent.sessions.len() != before || self.rt.agent.prompts.len() != prompts {
                    self.bump();
                }
                // The last session ended: offer to archive the tabs agents left open.
                let n = self.archivable_agent_tabs().len();
                if self.rt.agent.sessions.len() != before && self.rt.agent.sessions.is_empty() && n > 0 {
                    let label = if n == 1 { "Archive 1 agent tab".to_string() } else { format!("Archive {n} agent tabs") };
                    self.toast("Agent session ended", Some(ToastAction { label, command: Box::new(Command::ArchiveAgentTabs) }));
                }
            }
            Command::ToggleAgentPanel => {
                let just_closed = self.rt.agent.panel_closed_on_focus_loss.take().is_some_and(|t| now - t >= 0 && now - t < PANEL_TOGGLE_GUARD_MS);
                if self.rt.agent.panel_open || !just_closed {
                    self.rt.agent.panel_open = !self.rt.agent.panel_open;
                    self.bump();
                }
            }
            // While a prompt covers the panel, focus loss isn't about the panel.
            Command::CloseAgentPanel { focus_lost } if self.rt.agent.panel_open && (!focus_lost || self.rt.agent.prompts.is_empty()) => {
                self.rt.agent.panel_open = false;
                if focus_lost {
                    self.rt.agent.panel_closed_on_focus_loss = Some(now);
                }
                self.bump();
            }
            Command::ArchiveAgentTabs => self.archive_agent_tabs(now, fx),
            Command::AgentTabAccessRequested { id, session, tab, reason } => {
                let a = &self.rt.agent;
                if a.paused || self.state.settings.agent_access == AgentAccess::Off || self.tab(tab).is_none() || !a.sessions.iter().any(|s| s.session == session) {
                    fx.push(Effect::AgentAnswer { id, allow: false });
                    return;
                }
                if policy::in_scope(self.state.settings.agent_scope, &a.tabs, tab) {
                    fx.push(Effect::AgentAnswer { id, allow: true });
                    return;
                }
                if a.prompts.iter().any(|p| p.id == id) {
                    return;
                }
                let reason: String = reason.trim().chars().take(300).collect();
                self.push_prompt(AgentPromptView { id, kind: AgentPromptKind::Tab { session, tab, reason }, requested_at: now }, fx);
            }
            Command::AnswerTabAccess { id, allow } => {
                let Some((session, tab)) = self.rt.agent.prompts.iter().find(|p| p.id == id).and_then(|p| match &p.kind {
                    AgentPromptKind::Tab { session, tab, .. } => Some((*session, *tab)),
                    _ => None,
                }) else {
                    return;
                };
                if allow && let Some(url) = self.tab(tab).map(|t| t.url.clone()) {
                    self.rt.agent.tabs.insert(tab);
                    // Sharing a tab with this agent also allows the site it shows, for the session.
                    if let Ok(policy::UrlVerdict::Web { site }) = policy::check_url(&self.state.settings, &url) {
                        self.rt.agent.session_sites.entry(session).or_default().insert(site);
                    }
                }
                // Every waiting request of this session for the same tab gets the same answer.
                let answered: Vec<u64> = self
                    .rt
                    .agent
                    .prompts
                    .iter()
                    .filter(|p| matches!(&p.kind, AgentPromptKind::Tab { session: s, tab: t, .. } if *s == session && *t == tab))
                    .map(|p| p.id)
                    .collect();
                self.rt.agent.prompts.retain(|p| !answered.contains(&p.id));
                for pid in answered {
                    fx.push(Effect::AgentAnswer { id: pid, allow });
                }
                self.bump();
            }
            Command::AgentActivity { session, tool, tab, site, error } => {
                let tool: String = tool.chars().take(64).collect();
                let site = site.map(|s| s.chars().take(253).collect());
                let error = error.map(|e| e.chars().take(64).collect());
                self.rt.agent.activity.push_front(AgentActivityView { session, tool, tab, site, at: now, error });
                self.rt.agent.activity.truncate(MAX_ACTIVITY);
                self.bump();
            }
            Command::AgentSiteRequested { id, session, tab, site } => {
                if self.rt.agent.paused || self.state.settings.agent_access == AgentAccess::Off {
                    fx.push(Effect::AgentAnswer { id, allow: false });
                    return;
                }
                if self.agent_site_approved(session, &site) {
                    fx.push(Effect::AgentAnswer { id, allow: true });
                    return;
                }
                if self.rt.agent.prompts.iter().any(|p| p.id == id) {
                    return;
                }
                let site: String = site.chars().take(253).collect();
                self.push_prompt(AgentPromptView { id, kind: AgentPromptKind::Site { session, site, tab }, requested_at: now }, fx);
            }
            Command::OpenAgentTab { tab, url } => self.open_agent_tab(tab, url, now, fx),
            Command::LoadTab { tab } if self.tab(tab).is_some() && !self.is_loaded(tab) => {
                let url = self.url_to_load(tab);
                self.load_tab(tab, url, fx);
            }
            // Showing a tab for an agent never moves keyboard focus into it.
            Command::ShowAgentTab { tab } if self.activate(tab, now, fx) => self.rt.focus_request = None,
            Command::AgentDownloadHeld { id, tab, file_name } => {
                if self.rt.agent.held_downloads.iter().any(|d| d.id == id) {
                    return;
                }
                let file_name = file_name.chars().take(255).collect();
                self.rt.agent.held_downloads.push(AgentHeldDownloadView { id, tab, file_name });
                if self.rt.agent.held_downloads.len() > MAX_HELD_DOWNLOADS {
                    let old = self.rt.agent.held_downloads.remove(0);
                    fx.push(Effect::AgentDownload { id: old.id, keep: false });
                }
                self.bump();
            }
            Command::AgentTabAdopted { tab } if self.tab(tab).is_some() => {
                self.rt.agent.tabs.insert(tab);
                self.rt.agent.opened.insert(tab);
                self.bump();
            }
            _ => {}
        }
    }

    /// Archives the Today tabs agents opened, as one Ctrl+Shift+T batch.
    fn archive_agent_tabs(&mut self, now: Millis, fx: &mut Vec<Effect>) {
        let mut archived = Vec::new();
        for tab in self.archivable_agent_tabs() {
            let last = self.state.reopen.last().cloned();
            self.close_item(Some(tab), now, fx);
            if self.state.reopen.last() != last.as_ref()
                && let Some(ReopenEntry::Archived { archive_id }) = self.state.reopen.last().cloned()
            {
                self.state.reopen.pop();
                archived.push(archive_id);
            }
        }
        if archived.is_empty() {
            return;
        }
        let n = archived.len();
        self.push_reopen(ReopenEntry::Batch { archive_ids: archived });
        self.toast(
            format!("Archived {n} agent {}", if n == 1 { "tab" } else { "tabs" }),
            Some(ToastAction { label: "Undo".into(), command: Box::new(Command::ReopenClosed) }),
        );
        self.bump();
    }

    fn push_prompt(&mut self, prompt: AgentPromptView, fx: &mut Vec<Effect>) {
        self.rt.agent.prompts.push(prompt);
        while self.rt.agent.prompts.len() > MAX_PROMPTS {
            let old = self.rt.agent.prompts.remove(0);
            fx.push(Effect::AgentAnswer { id: old.id, allow: false });
        }
        self.bump();
    }

    fn deny_all_prompts(&mut self, fx: &mut Vec<Effect>) {
        for p in std::mem::take(&mut self.rt.agent.prompts) {
            fx.push(Effect::AgentAnswer { id: p.id, allow: false });
            self.bump();
        }
    }

    fn open_agent_tab(&mut self, tab: Id, url: String, now: Millis, fx: &mut Vec<Effect>) {
        let url = url.trim().to_string();
        let free = tab != 0 && tab <= crate::MAX_ID && !self.state.items.contains_key(&tab) && self.peek_tab() != Some(tab) && !self.rt.tabs.contains_key(&tab);
        let allowed = crate::urls::is_about_blank(&url) || crate::urls::host(&url).is_some();
        if !free || !allowed || self.state.settings.agent_access != AgentAccess::Full || self.rt.agent.paused {
            return;
        }
        if self.state.next_id <= tab {
            self.state.next_id = tab + 1;
        }
        let item = Tab { id: tab, url: url.clone(), created_at: now, last_active_at: now, ..Tab::default() };
        self.state.items.insert(tab, Item::Tab(item));
        self.place_in_today(tab, None);
        self.rt.visit_first_commit.insert(tab);
        self.load_tab(tab, url, fx);
        self.rt.agent.tabs.insert(tab);
        self.rt.agent.opened.insert(tab);
        self.touch();
    }

    pub(super) fn patch_agent_settings(&mut self, patch: &SettingsPatch) {
        let s = &mut self.state.settings;
        if let Some(v) = patch.agent_access {
            s.agent_access = v;
        }
        if let Some(v) = patch.agent_scope {
            s.agent_scope = v;
        }
        if let Some(v) = patch.agent_sites {
            s.agent_sites = v;
        }
        if let Some(v) = patch.agent_scripts {
            s.agent_scripts = v;
        }
        if let Some(v) = patch.agent_history {
            s.agent_history = v;
        }
        if let Some(v) = patch.agent_downloads {
            s.agent_downloads = v;
        }
        if let Some(v) = patch.agent_allow_private_network {
            s.agent_allow_private_network = v;
        }
        if let Some(list) = &patch.agent_blocked_hosts {
            s.agent_blocked_hosts = policy::normalize_host_list(list);
        }
        if let Some(list) = &patch.agent_allowed_sites {
            s.agent_allowed_sites = policy::normalize_host_list(list);
        }
        if let Some(list) = &patch.agent_trusted_clients {
            // Revoke only: a patch can't add trust.
            s.agent_trusted_clients.retain(|c| list.iter().any(|x| x.exe.eq_ignore_ascii_case(&c.exe) && x.signer == c.signer));
        }
    }

    /// After every command: the endpoint follows `agentAccess`; closed tabs leave the agent scope.
    pub(super) fn reconcile_agent(&mut self, fx: &mut Vec<Effect>) {
        let enabled = self.state.settings.agent_access != AgentAccess::Off;
        if self.rt.agent.emitted_endpoint != Some(enabled) {
            if !enabled && self.rt.agent.emitted_endpoint.is_some() {
                self.deny_all_prompts(fx);
                if !self.rt.agent.sessions.is_empty() {
                    self.rt.agent.sessions.clear();
                    self.rt.agent.session_sites.clear();
                    self.bump();
                }
            }
            fx.push(Effect::AgentEndpoint { enabled });
            self.rt.agent.emitted_endpoint = Some(enabled);
        }
        // A tab access prompt for a tab that closed is denied.
        let orphaned: Vec<u64> = self
            .rt
            .agent
            .prompts
            .iter()
            .filter(|p| matches!(p.kind, AgentPromptKind::Tab { tab, .. } if self.tab(tab).is_none()))
            .map(|p| p.id)
            .collect();
        if !orphaned.is_empty() {
            self.rt.agent.prompts.retain(|p| !orphaned.contains(&p.id));
            for id in orphaned {
                fx.push(Effect::AgentAnswer { id, allow: false });
            }
            self.bump();
        }
        // The agent overlay: the first prompt, else the activity panel.
        let overlay = match self.rt.agent.prompts.first() {
            Some(p) => Some((true, Some(p.id))),
            None => self.rt.agent.panel_open.then_some((false, None)),
        };
        if overlay != self.rt.agent.emitted_overlay {
            match overlay {
                Some((prompt, _)) => fx.push(Effect::ShowAgentOverlay { prompt }),
                None => fx.push(Effect::HideAgentOverlay),
            }
            self.rt.agent.emitted_overlay = overlay;
        }
        if self.rt.agent.tabs.is_empty() && self.rt.agent.opened.is_empty() {
            return;
        }
        let gone: Vec<Id> = self.rt.agent.tabs.iter().chain(self.rt.agent.opened.iter()).copied().filter(|t| self.tab(*t).is_none()).collect();
        if !gone.is_empty() {
            for t in gone {
                self.rt.agent.tabs.remove(&t);
                self.rt.agent.opened.remove(&t);
            }
            self.bump();
        }
    }
}
