//! AI agents (MCP): settings fallbacks, the endpoint effect, approval prompts, trusted clients,
//! site approval, agent tabs, Stop/Resume and held downloads (store/agent.rs, docs/MCP.md).

mod common;
use sta_core::agent::{AgentClientInfo, AgentPromptKind};
use sta_core::*;
use common::*;
use serde_json::json;

fn full_access(h: &mut Harness) -> Vec<Effect> {
    h.apply(Command::UpdateSettings { patch: SettingsPatch { agent_access: Some(AgentAccess::Full), ..Default::default() } })
}

fn client(verified: bool) -> AgentClientInfo {
    AgentClientInfo {
        name: "claude-code".into(),
        title: Some("Claude Code".into()),
        version: Some("2.1.268".into()),
        exe: Some(r"C:\Users\me\AppData\Local\Programs\claude\claude.exe".into()),
        signer: verified.then(|| "Anthropic, PBC".into()),
        verified,
    }
}

fn answers(fx: &[Effect]) -> Vec<(u64, bool)> {
    fx.iter().filter_map(|e| if let Effect::AgentAnswer { id, allow } = e { Some((*id, *allow)) } else { None }).collect()
}

#[test]
fn settings_default_off_and_unknown_values_fall_back() {
    let s = Settings::default();
    assert_eq!((s.agent_access, s.agent_scope, s.agent_sites, s.agent_scripts), (AgentAccess::Off, AgentScope::AgentTabs, AgentSites::Ask, AgentScripts::Off));
    assert!(!s.agent_history && !s.agent_downloads && !s.agent_allow_private_network);
    let parsed: Settings = serde_json::from_value(json!({
        "agentAccess": "everything", "agentScope": "galaxy", "agentSites": "maybe", "agentScripts": "root"
    }))
    .unwrap();
    assert_eq!((parsed.agent_access, parsed.agent_scope, parsed.agent_sites, parsed.agent_scripts), (AgentAccess::Off, AgentScope::AgentTabs, AgentSites::Ask, AgentScripts::Off));
    let parsed: Settings = serde_json::from_value(json!({ "agentAccess": "readOnly", "agentScope": "allTabs", "agentSites": "all", "agentScripts": "isolated" })).unwrap();
    assert_eq!((parsed.agent_access, parsed.agent_scope, parsed.agent_sites, parsed.agent_scripts), (AgentAccess::ReadOnly, AgentScope::AllTabs, AgentSites::All, AgentScripts::Isolated));
    assert_eq!(serde_json::to_value(AgentAccess::ReadOnly).unwrap(), json!("readOnly"));
    assert_eq!(serde_json::to_value(AgentAccess::Off).unwrap(), json!("off"));

    // An old profile (no agent fields) and a profile with a bad value load cleanly, access off.
    let old = r#"{"version":2,"nextId":2,"settings":{"searchEngine":"bing"},"spaces":[{"id":1,"name":"Home"}]}"#;
    let (store, report) = Store::load(Some(old), None, T0);
    assert!(!report.state_corrupt, "{report:?}");
    assert_eq!(store.settings().agent_access, AgentAccess::Off);
    assert_eq!(store.state().version, 2, "STATE_VERSION stays 2");
    let bad = r#"{"version":2,"nextId":2,"settings":{"agentAccess":"superuser","agentBlockedHosts":["bank.example"]},"spaces":[{"id":1,"name":"Home"}]}"#;
    let (store, report) = Store::load(Some(bad), None, T0);
    assert!(!report.state_corrupt, "{report:?}");
    assert_eq!(store.settings().agent_access, AgentAccess::Off);
    assert_eq!(store.settings().agent_blocked_hosts, vec!["bank.example".to_string()]);
}

#[test]
fn endpoint_follows_access() {
    let mut h = Harness::new();
    assert!(h.history.contains(&Effect::AgentEndpoint { enabled: false }), "startup reports the endpoint state");
    let fx = full_access(&mut h);
    assert!(fx.contains(&Effect::AgentEndpoint { enabled: true }));
    let fx = h.apply(Command::UpdateSettings { patch: SettingsPatch { agent_access: Some(AgentAccess::ReadOnly), ..Default::default() } });
    assert!(!has(&fx, |e| matches!(e, Effect::AgentEndpoint { .. })), "read-only keeps the endpoint");
    h.apply(Command::AgentConnectionRequested { id: 1, client: client(false) });
    assert_eq!(h.ui().agent.prompts.len(), 1);
    let fx = h.apply(Command::UpdateSettings { patch: SettingsPatch { agent_access: Some(AgentAccess::Off), ..Default::default() } });
    assert!(fx.contains(&Effect::AgentEndpoint { enabled: false }));
    assert_eq!(answers(&fx), vec![(1, false)], "turning access off denies waiting prompts");
    assert!(h.ui().agent.prompts.is_empty());
}

#[test]
fn connection_prompt_session_and_always() {
    let mut h = Harness::new();
    // Access off: denied at once.
    let fx = h.apply(Command::AgentConnectionRequested { id: 1, client: client(true) });
    assert_eq!(answers(&fx), vec![(1, false)]);
    full_access(&mut h);
    let fx = h.apply(Command::AgentConnectionRequested { id: 2, client: client(true) });
    assert!(answers(&fx).is_empty());
    let prompts = h.ui().agent.prompts;
    assert_eq!(prompts.len(), 1);
    assert!(matches!(&prompts[0].kind, AgentPromptKind::Connection { client } if client.display_name() == "Claude Code"));
    // Answering an unknown id does nothing.
    assert!(answers(&h.apply(Command::AnswerAgentConnection { id: 99, allow: true, remember: true })).is_empty());
    let fx = h.apply(Command::AnswerAgentConnection { id: 2, allow: true, remember: true });
    assert_eq!(answers(&fx), vec![(2, true)]);
    assert_eq!(h.store.settings().agent_trusted_clients.len(), 1, "Always trusts a signed host");
    // The same signed host connects again: allowed without a prompt.
    let fx = h.apply(Command::AgentConnectionRequested { id: 3, client: client(true) });
    assert_eq!(answers(&fx), vec![(3, true)]);
    // An unsigned host with the same path isn't trusted, and Always doesn't stick for it.
    let fx = h.apply(Command::AgentConnectionRequested { id: 4, client: client(false) });
    assert!(answers(&fx).is_empty());
    h.apply(Command::AnswerAgentConnection { id: 4, allow: true, remember: true });
    assert_eq!(h.store.settings().agent_trusted_clients.len(), 1);
    // Sessions.
    h.apply(Command::AgentSessionStarted { session: 7, client: client(true), access: AgentAccess::Full });
    assert_eq!(h.ui().agent.sessions.len(), 1);
    h.apply(Command::AgentSessionEnded { session: 7 });
    assert!(h.ui().agent.sessions.is_empty());
    // Revoke through a settings patch; a patch can't add trust.
    let mut forged = h.store.settings().agent_trusted_clients.clone();
    forged.push(AgentTrustedClient { name: "evil".into(), exe: r"C:\evil.exe".into(), signer: "Evil".into(), added_at: 0 });
    h.apply(Command::UpdateSettings { patch: SettingsPatch { agent_trusted_clients: Some(forged), ..Default::default() } });
    assert_eq!(h.store.settings().agent_trusted_clients.len(), 1);
    h.apply(Command::UpdateSettings { patch: SettingsPatch { agent_trusted_clients: Some(vec![]), ..Default::default() } });
    assert!(h.store.settings().agent_trusted_clients.is_empty());
}

#[test]
fn site_prompts_session_and_always() {
    let mut h = Harness::new();
    full_access(&mut h);
    h.apply(Command::AgentSessionStarted { session: 1, client: client(false), access: AgentAccess::Full });
    let fx = h.apply(Command::AgentSiteRequested { id: 10, session: 1, tab: None, site: "example.com".into() });
    assert!(answers(&fx).is_empty());
    let fx = h.apply(Command::AgentSiteRequested { id: 11, session: 1, tab: None, site: "example.com".into() });
    assert!(answers(&fx).is_empty());
    assert_eq!(h.ui().agent.prompts.len(), 2);
    let fx = h.apply(Command::AnswerSitePermission { id: 10, allow: true, remember: false });
    assert_eq!(answers(&fx), vec![(10, true), (11, true)], "same site, same answer");
    assert!(h.store.agent_site_approved(1, "example.com"));
    assert!(!h.store.agent_site_approved(2, "example.com"), "only for that session");
    assert!(h.store.settings().agent_allowed_sites.is_empty());
    // Already allowed: answered at once.
    assert_eq!(answers(&h.apply(Command::AgentSiteRequested { id: 12, session: 1, tab: None, site: "example.com".into() })), vec![(12, true)]);
    // Always.
    h.apply(Command::AgentSiteRequested { id: 13, session: 1, tab: None, site: "rust-lang.org".into() });
    h.apply(Command::AnswerSitePermission { id: 13, allow: true, remember: true });
    assert_eq!(h.store.settings().agent_allowed_sites, vec!["rust-lang.org".to_string()]);
    assert!(h.store.agent_site_approved(99, "rust-lang.org"));
    // Deny.
    h.apply(Command::AgentSiteRequested { id: 14, session: 1, tab: None, site: "evil.test".into() });
    assert_eq!(answers(&h.apply(Command::AnswerSitePermission { id: 14, allow: false, remember: true })), vec![(14, false)]);
    assert!(!h.store.agent_site_approved(1, "evil.test"));
    // Session end forgets session grants.
    h.apply(Command::AgentSessionEnded { session: 1 });
    assert!(!h.store.agent_site_approved(1, "example.com"));
}

#[test]
fn agent_tabs_open_in_background_and_leave_scope_when_closed() {
    let mut h = Harness::new();
    let user_tab = h.open("https://user.example/");
    // Access off: ignored.
    let id = h.store.alloc_id();
    let fx = h.apply(Command::OpenAgentTab { tab: id, url: "https://example.com/".into() });
    assert!(fx.is_empty() && h.store.tab(id).is_none());
    full_access(&mut h);
    let id = h.store.alloc_id();
    let fx = h.apply(Command::OpenAgentTab { tab: id, url: "https://example.com/".into() });
    assert!(has(&fx, |e| is_create(e, id)), "{fx:?}");
    assert_eq!(h.today()[0], id, "top of Today");
    assert_eq!(h.focused(), Some(user_tab), "background: the user's tab stays active");
    assert!(!has(&fx, |e| matches!(e, Effect::FocusBrowser { .. })));
    assert!(h.store.agent_tabs().contains(&id) && h.store.agent_opened_tab(id));
    assert!(h.ui().spaces[0].today.iter().any(|n| matches!(n, NodeView::Tab(t) if t.id == id && t.agent)));
    // The UI JSON omits `agent` for other tabs.
    let v = serde_json::to_value(h.ui()).unwrap();
    let today = v["spaces"][0]["today"].as_array().unwrap();
    assert!(today.iter().any(|t| t["id"] == json!(user_tab) && t.get("agent").is_none()));
    assert!(today.iter().any(|t| t["id"] == json!(id) && t["agent"] == json!(true)));
    // Ids in use and non-web URLs are refused.
    assert!(h.apply(Command::OpenAgentTab { tab: user_tab, url: "https://x.com/".into() }).is_empty());
    let other = h.store.alloc_id();
    assert!(h.apply(Command::OpenAgentTab { tab: other, url: "file:///C:/".into() }).is_empty());
    assert!(h.apply(Command::OpenAgentTab { tab: other, url: "sta://settings/".into() }).is_empty());
    // Show without focus.
    let fx = h.apply(Command::ShowAgentTab { tab: id });
    assert_eq!(h.focused(), Some(id));
    assert!(!has(&fx, |e| matches!(e, Effect::FocusBrowser { .. })), "{fx:?}");
    // Share a user tab, then close the agent tab: it leaves the scope.
    h.apply(Command::ShareTabWithAgent { tab: user_tab, shared: true });
    assert!(h.store.agent_tabs().contains(&user_tab) && !h.store.agent_opened_tab(user_tab));
    h.apply(Command::CloseItem { id: Some(id) });
    assert!(!h.store.agent_tabs().contains(&id));
    h.apply(Command::ShareTabWithAgent { tab: user_tab, shared: false });
    assert!(h.store.agent_tabs().is_empty());
}

#[test]
fn popups_of_agent_tabs_and_load_tab() {
    let mut h = Harness::new();
    full_access(&mut h);
    let id = h.store.alloc_id();
    h.apply(Command::OpenAgentTab { tab: id, url: "https://example.com/".into() });
    let (popup, _) = h.popup(Some(id), "https://example.com/next", false, false);
    h.apply(Command::AgentTabAdopted { tab: popup });
    assert!(h.store.agent_opened_tab(popup));
    // LoadTab loads an unloaded tab without showing it.
    h.apply(Command::UnloadTab { id: popup });
    assert!(!h.store.is_loaded(popup));
    let fx = h.apply(Command::LoadTab { tab: popup });
    assert!(has(&fx, |e| is_create(e, popup)));
    assert!(h.store.is_loaded(popup));
    assert!(!h.ui().spaces[0].today.iter().any(|n| matches!(n, NodeView::Tab(t) if t.id == popup && t.visible)));
}

#[test]
fn stop_resume_activity_and_held_downloads() {
    let mut h = Harness::new();
    full_access(&mut h);
    h.apply(Command::AgentConnectionRequested { id: 5, client: client(false) });
    for i in 0..7 {
        h.apply(Command::AgentActivity { session: 1, tool: format!("tool{i}"), tab: Some(3), site: Some("example.com".into()), error: (i == 6).then(|| "site_not_approved".into()) });
    }
    let activity = h.ui().agent.activity;
    assert_eq!(activity.len(), 5);
    assert_eq!(activity[0].tool, "tool6", "newest first");
    assert_eq!((activity[0].error.as_deref(), activity[1].error.as_deref()), (Some("site_not_approved"), None));
    h.apply(Command::AgentDownloadHeld { id: 40, tab: None, file_name: "setup.exe".into() });
    assert_eq!(h.ui().agent.held_downloads.len(), 1);
    assert_eq!(h.apply(Command::ResolveAgentDownload { id: 40, keep: false }), vec![Effect::AgentDownload { id: 40, keep: false }]);
    assert!(h.ui().agent.held_downloads.is_empty());

    let fx = h.apply(Command::StopAgents);
    assert!(fx.contains(&Effect::AgentDisconnect));
    assert_eq!(answers(&fx), vec![(5, false)]);
    assert!(h.ui().agent.paused && h.store.agents_paused());
    // Paused: new connections and sites are denied at once.
    assert_eq!(answers(&h.apply(Command::AgentConnectionRequested { id: 6, client: client(false) })), vec![(6, false)]);
    assert_eq!(answers(&h.apply(Command::AgentSiteRequested { id: 7, session: 1, tab: None, site: "a.com".into() })), vec![(7, false)]);
    let tab = h.store.alloc_id();
    assert!(h.apply(Command::OpenAgentTab { tab, url: "https://example.com/".into() }).is_empty());
    // Paused is not saved.
    assert!(!h.store.state_json().contains("paused"));
    h.apply(Command::ResumeAgents);
    assert!(!h.ui().agent.paused);
}

#[test]
fn settings_patch_normalizes_hosts() {
    let mut h = Harness::new();
    h.apply(Command::UpdateSettings {
        patch: SettingsPatch {
            agent_blocked_hosts: Some(vec!["https://Bank.Example/login".into(), "bank.example".into(), " ".into()]),
            agent_allowed_sites: Some(vec!["docs.rs".into()]),
            agent_scope: Some(AgentScope::AllTabs),
            agent_allow_private_network: Some(true),
            ..Default::default()
        },
    });
    let s = h.store.settings();
    assert_eq!(s.agent_blocked_hosts, vec!["bank.example".to_string()]);
    assert_eq!(s.agent_allowed_sites, vec!["docs.rs".to_string()]);
    assert_eq!(s.agent_scope, AgentScope::AllTabs);
    assert!(s.agent_allow_private_network);
}

#[test]
fn agent_commands_wire_format() {
    let cmd = |v: serde_json::Value| serde_json::from_value::<Command>(v.clone()).unwrap_or_else(|e| panic!("{v} -> {e}"));
    assert_eq!(cmd(json!({"type":"answerAgentConnection","id":3,"allow":true})), Command::AnswerAgentConnection { id: 3, allow: true, remember: false });
    assert_eq!(cmd(json!({"type":"shareTabWithAgent","tab":4})), Command::ShareTabWithAgent { tab: 4, shared: true });
    assert_eq!(cmd(json!({"type":"stopAgents"})), Command::StopAgents);
    assert!(Command::StopAgents.allowed_from_ui() && Command::ResumeAgents.allowed_from_ui());
    assert!(Command::AnswerSitePermission { id: 1, allow: true, remember: false }.allowed_from_ui());
    for shell in [
        Command::AgentConnectionRequested { id: 1, client: AgentClientInfo::default() },
        Command::AgentSessionStarted { session: 1, client: AgentClientInfo::default(), access: AgentAccess::Full },
        Command::AgentSessionEnded { session: 1 },
        Command::AgentActivity { session: 1, tool: "x".into(), tab: None, site: None, error: None },
        Command::AgentSiteRequested { id: 1, session: 1, tab: None, site: "a".into() },
        Command::OpenAgentTab { tab: 1, url: "https://a.com/".into() },
        Command::LoadTab { tab: 1 },
        Command::ShowAgentTab { tab: 1 },
        Command::AgentDownloadHeld { id: 1, tab: None, file_name: "a".into() },
        Command::AgentTabAdopted { tab: 1 },
    ] {
        assert!(!shell.allowed_from_ui(), "{shell:?}");
    }
    assert_eq!(serde_json::to_value(Effect::AgentEndpoint { enabled: true }).unwrap(), json!({"type":"agentEndpoint","enabled":true}));
    assert_eq!(serde_json::to_value(Effect::AgentAnswer { id: 2, allow: false }).unwrap(), json!({"type":"agentAnswer","id":2,"allow":false}));
    assert_eq!(serde_json::to_value(Effect::AgentDisconnect).unwrap(), json!({"type":"agentDisconnect"}));
    let prompt = sta_core::agent::AgentPromptView { id: 9, kind: AgentPromptKind::Site { session: 1, site: "example.com".into(), tab: Some(4) }, requested_at: 5 };
    assert_eq!(serde_json::to_value(&prompt).unwrap(), json!({"id":9,"kind":"site","session":1,"site":"example.com","tab":4,"requestedAt":5}));
}

fn overlay(fx: &[Effect]) -> Vec<Effect> {
    fx.iter().filter(|e| matches!(e, Effect::ShowAgentOverlay { .. } | Effect::HideAgentOverlay)).cloned().collect()
}

#[test]
fn agent_overlay_follows_prompts_and_the_panel() {
    let mut h = Harness::new();
    full_access(&mut h);
    let fx = h.apply(Command::AgentConnectionRequested { id: 1, client: client(true) });
    assert_eq!(overlay(&fx), vec![Effect::ShowAgentOverlay { prompt: true }]);
    assert!(overlay(&h.apply(Command::AgentConnectionRequested { id: 2, client: client(false) })).is_empty(), "the first prompt is unchanged");
    assert_eq!(overlay(&h.apply(Command::AnswerAgentConnection { id: 1, allow: false, remember: false })), vec![Effect::ShowAgentOverlay { prompt: true }], "the next prompt");
    assert_eq!(overlay(&h.apply(Command::AnswerAgentConnection { id: 2, allow: true, remember: false })), vec![Effect::HideAgentOverlay]);

    // The activity panel (topbar chip).
    assert_eq!(overlay(&h.apply(Command::ToggleAgentPanel)), vec![Effect::ShowAgentOverlay { prompt: false }]);
    assert!(h.ui().agent.panel_open);
    h.apply(Command::AgentSessionStarted { session: 3, client: client(false), access: AgentAccess::Full });
    // A prompt takes over the overlay while the panel is open, and gives it back.
    assert_eq!(overlay(&h.apply(Command::AgentSiteRequested { id: 4, session: 3, tab: None, site: "example.com".into() })), vec![Effect::ShowAgentOverlay { prompt: true }]);
    assert!(overlay(&h.apply(Command::CloseAgentPanel { focus_lost: true })).is_empty(), "a prompt stays up on focus loss");
    assert!(h.ui().agent.panel_open, "and the panel behind it stays open");
    assert_eq!(overlay(&h.apply(Command::AnswerSitePermission { id: 4, allow: true, remember: false })), vec![Effect::ShowAgentOverlay { prompt: false }]);

    // Focus loss closes the panel; the chip click that caused it doesn't reopen it.
    assert_eq!(overlay(&h.apply(Command::CloseAgentPanel { focus_lost: true })), vec![Effect::HideAgentOverlay]);
    h.advance(100);
    assert!(overlay(&h.apply(Command::ToggleAgentPanel)).is_empty());
    assert!(!h.ui().agent.panel_open);
    h.advance(100);
    assert_eq!(overlay(&h.apply(Command::ToggleAgentPanel)), vec![Effect::ShowAgentOverlay { prompt: false }]);
    assert_eq!(overlay(&h.apply(Command::ToggleAgentPanel)), vec![Effect::HideAgentOverlay]);
    // Esc / a button: a toggle right after reopens it.
    h.apply(Command::ToggleAgentPanel);
    h.apply(Command::CloseAgentPanel { focus_lost: false });
    assert!(!h.ui().agent.panel_open);
    h.apply(Command::ToggleAgentPanel);
    assert!(h.ui().agent.panel_open);
    // Stop keeps the panel (Resume is there); turning access off denies prompts and hides them.
    h.apply(Command::StopAgents);
    assert!(h.ui().agent.panel_open && h.ui().agent.paused);
    h.apply(Command::CloseAgentPanel { focus_lost: false });
    h.apply(Command::ResumeAgents);
    h.apply(Command::AgentConnectionRequested { id: 5, client: client(false) });
    let fx = h.apply(Command::UpdateSettings { patch: SettingsPatch { agent_access: Some(AgentAccess::Off), ..Default::default() } });
    assert_eq!(overlay(&fx), vec![Effect::HideAgentOverlay]);
}

#[test]
fn session_end_offers_to_archive_agent_tabs() {
    let mut h = Harness::new();
    let user_tab = h.open("https://user.example/");
    full_access(&mut h);
    h.apply(Command::AgentSessionStarted { session: 1, client: client(false), access: AgentAccess::Full });
    h.apply(Command::AgentSessionStarted { session: 2, client: client(true), access: AgentAccess::Full });
    let a = h.store.alloc_id();
    h.apply(Command::OpenAgentTab { tab: a, url: "https://a.example/".into() });
    let b = h.store.alloc_id();
    h.apply(Command::OpenAgentTab { tab: b, url: "https://b.example/".into() });
    // A shared user tab and a pinned agent tab are never archived by it.
    h.apply(Command::ShareTabWithAgent { tab: user_tab, shared: true });
    let pinned = h.store.alloc_id();
    h.apply(Command::OpenAgentTab { tab: pinned, url: "https://p.example/".into() });
    h.apply(Command::TogglePin { id: Some(pinned) });
    assert_eq!(h.ui().agent.opened_tabs, 2);

    let before = h.toast().map(|t| t.id);
    h.apply(Command::AgentSessionEnded { session: 1 });
    assert_eq!(h.toast().map(|t| t.id), before, "another session is still connected");
    h.apply(Command::AgentSessionEnded { session: 2 });
    let toast = h.toast().expect("session-end toast");
    let action = toast.action.expect("toast action");
    assert_eq!((toast.message.as_str(), action.label.as_str()), ("Agent session ended", "Archive 2 agent tabs"));
    assert_eq!(*action.command, Command::ArchiveAgentTabs);

    h.apply(Command::ArchiveAgentTabs);
    assert!(h.store.tab(a).is_none() && h.store.tab(b).is_none());
    assert!(h.store.tab(user_tab).is_some() && h.store.tab(pinned).is_some());
    assert_eq!(h.toast().map(|t| t.message), Some("Archived 2 agent tabs".to_string()));
    assert_eq!(h.ui().agent.opened_tabs, 0);
    // One Ctrl+Shift+T brings both back.
    h.apply(Command::ReopenClosed);
    assert!(h.store.tab(a).is_some() && h.store.tab(b).is_some());

    // No agent tabs left open: no toast.
    for t in [a, b] {
        h.apply(Command::CloseItem { id: Some(t) });
    }
    h.apply(Command::DismissToast { id: h.toast().map(|t| t.id).unwrap_or(0) });
    h.apply(Command::AgentSessionStarted { session: 3, client: client(false), access: AgentAccess::Full });
    h.apply(Command::AgentSessionEnded { session: 3 });
    assert!(h.toast().is_none_or(|t| t.message != "Agent session ended"));
}

#[test]
fn agent_ui_commands_wire_format() {
    let cmd = |v: serde_json::Value| serde_json::from_value::<Command>(v.clone()).unwrap_or_else(|e| panic!("{v} -> {e}"));
    assert_eq!(cmd(json!({"type":"toggleAgentPanel"})), Command::ToggleAgentPanel);
    assert_eq!(cmd(json!({"type":"closeAgentPanel"})), Command::CloseAgentPanel { focus_lost: false });
    assert_eq!(cmd(json!({"type":"closeAgentPanel","focusLost":true})), Command::CloseAgentPanel { focus_lost: true });
    assert_eq!(cmd(json!({"type":"archiveAgentTabs"})), Command::ArchiveAgentTabs);
    for c in [Command::ToggleAgentPanel, Command::CloseAgentPanel { focus_lost: true }, Command::ArchiveAgentTabs] {
        assert!(c.allowed_from_ui(), "{c:?}");
    }
    assert_eq!(serde_json::to_value(Effect::ShowAgentOverlay { prompt: true }).unwrap(), json!({"type":"showAgentOverlay","prompt":true}));
    assert_eq!(serde_json::to_value(Effect::HideAgentOverlay).unwrap(), json!({"type":"hideAgentOverlay"}));
    let v = serde_json::to_value(Harness::new().ui().agent).unwrap();
    assert_eq!((v["panelOpen"].clone(), v["openedTabs"].clone()), (json!(false), json!(0)));
}

#[test]
fn tab_access_prompts_share_the_tab() {
    let mut h = Harness::new();
    let user_tab = h.open("https://user.example/");
    let other_tab = h.open("https://other.example/");
    full_access(&mut h);
    // No such session: denied at once.
    assert_eq!(answers(&h.apply(Command::AgentTabAccessRequested { id: 1, session: 9, tab: user_tab, reason: "x".into() })), vec![(1, false)]);
    h.apply(Command::AgentSessionStarted { session: 1, client: client(true), access: AgentAccess::Full });
    // A missing tab is denied at once too.
    assert_eq!(answers(&h.apply(Command::AgentTabAccessRequested { id: 2, session: 1, tab: 99_999, reason: "x".into() })), vec![(2, false)]);

    let long = "Summarize this page for the user. ".repeat(20);
    let fx = h.apply(Command::AgentTabAccessRequested { id: 3, session: 1, tab: user_tab, reason: long.clone() });
    assert!(answers(&fx).is_empty());
    assert_eq!(overlay(&fx), vec![Effect::ShowAgentOverlay { prompt: true }]);
    let prompt = h.ui().agent.prompts[0].clone();
    match &prompt.kind {
        AgentPromptKind::Tab { session, tab, reason } => {
            assert_eq!((*session, *tab), (1, user_tab));
            assert_eq!(reason.chars().count(), 300, "the reason is capped");
        }
        other => panic!("{other:?}"),
    }
    let v = serde_json::to_value(&prompt).unwrap();
    assert_eq!((v["kind"].clone(), v["tab"].clone()), (json!("tab"), json!(user_tab)));
    // A second request for the same tab gets the same answer.
    h.apply(Command::AgentTabAccessRequested { id: 4, session: 1, tab: user_tab, reason: "again".into() });
    let fx = h.apply(Command::AnswerTabAccess { id: 3, allow: true });
    assert_eq!(answers(&fx), vec![(3, true), (4, true)]);
    assert!(h.store.agent_tabs().contains(&user_tab) && !h.store.agent_opened_tab(user_tab), "shared, not opened");
    assert!(h.ui().agent.prompts.is_empty());
    assert!(h.store.agent_site_approved(1, "user.example"), "sharing allows the tab's site for that session");
    assert!(!h.store.agent_site_approved(2, "user.example") && !h.store.agent_site_approved(1, "other.example"));
    // Already in scope: allowed at once.
    assert_eq!(answers(&h.apply(Command::AgentTabAccessRequested { id: 5, session: 1, tab: user_tab, reason: "x".into() })), vec![(5, true)]);

    // Deny keeps the tab out of scope.
    h.apply(Command::AgentTabAccessRequested { id: 6, session: 1, tab: other_tab, reason: "x".into() });
    assert_eq!(answers(&h.apply(Command::AnswerTabAccess { id: 6, allow: false })), vec![(6, false)]);
    assert!(!h.store.agent_tabs().contains(&other_tab));
    // Closing the tab denies its prompt; so does the end of the session.
    h.apply(Command::AgentTabAccessRequested { id: 7, session: 1, tab: other_tab, reason: "x".into() });
    assert_eq!(answers(&h.apply(Command::CloseItem { id: Some(other_tab) })), vec![(7, false)]);
    assert!(h.ui().agent.prompts.is_empty());
    let third = h.open("https://third.example/");
    h.apply(Command::AgentTabAccessRequested { id: 8, session: 1, tab: third, reason: "x".into() });
    h.apply(Command::AgentSessionEnded { session: 1 });
    assert!(h.ui().agent.prompts.is_empty());
    // All tabs in scope: nothing to ask.
    h.apply(Command::AgentSessionStarted { session: 2, client: client(true), access: AgentAccess::Full });
    h.apply(Command::UpdateSettings { patch: SettingsPatch { agent_scope: Some(AgentScope::AllTabs), ..Default::default() } });
    assert_eq!(answers(&h.apply(Command::AgentTabAccessRequested { id: 9, session: 2, tab: third, reason: "x".into() })), vec![(9, true)]);
    // Wire format and caller rules.
    assert_eq!(serde_json::from_value::<Command>(json!({"type":"answerTabAccess","id":3,"allow":true})).unwrap(), Command::AnswerTabAccess { id: 3, allow: true });
    assert!(Command::AnswerTabAccess { id: 1, allow: true }.allowed_from_ui());
    assert!(!Command::AgentTabAccessRequested { id: 1, session: 1, tab: 1, reason: String::new() }.allowed_from_ui());
}
