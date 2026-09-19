//! Agent policy (docs/MCP.md "Safety"): which URLs an agent may open or act on, site approval,
//! private-network and blocked-host rules, tab scope and read-only access. Pure functions; the
//! shell calls them with store data before every tool call (and in `on_before_browse` for
//! agent-controlled tabs).

use super::channel::ToolError;
use super::errors::ErrorCode;
use crate::model::{AgentAccess, AgentScope, AgentSites, Settings};
use crate::{Id, urls};
use std::collections::BTreeSet;
use std::net::{Ipv4Addr, Ipv6Addr};
use url::{Host, Url};

/// Most blocked-host / allowed-site entries kept in settings.
pub const MAX_HOST_ENTRIES: usize = 500;

/// Where a host is on the network.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostKind {
    /// `localhost`, `127.0.0.0/8`, `::1`: always allowed (local development).
    Loopback,
    /// RFC 1918 / unique-local / CGNAT addresses, `.local` names, single-label names, `0.0.0.0`.
    Private,
    /// `169.254.0.0/16`, `fe80::/10`.
    LinkLocal,
    Public,
}

fn ipv4_kind(ip: Ipv4Addr) -> HostKind {
    let o = ip.octets();
    if ip.is_loopback() {
        HostKind::Loopback
    } else if ip.is_link_local() {
        HostKind::LinkLocal
    } else if ip.is_private() || ip.is_unspecified() || ip.is_broadcast() || (o[0] == 100 && (64..128).contains(&o[1])) {
        HostKind::Private
    } else {
        HostKind::Public
    }
}

fn ipv6_kind(ip: Ipv6Addr) -> HostKind {
    if let Some(v4) = ip.to_ipv4_mapped() {
        return ipv4_kind(v4);
    }
    let first = ip.segments()[0];
    if ip.is_loopback() {
        HostKind::Loopback
    } else if first & 0xffc0 == 0xfe80 {
        HostKind::LinkLocal
    } else if first & 0xfe00 == 0xfc00 || ip.is_unspecified() {
        HostKind::Private
    } else {
        HostKind::Public
    }
}

/// Network location of a parsed URL host. DNS names are classified by name only (DNS rebinding
/// can still point a public name at a private address; docs/MCP.md says so).
pub fn host_kind(host: &Host<&str>) -> HostKind {
    match host {
        Host::Ipv4(ip) => ipv4_kind(*ip),
        Host::Ipv6(ip) => ipv6_kind(*ip),
        Host::Domain(d) => {
            let d = d.trim_end_matches('.').to_ascii_lowercase();
            if d == "localhost" || d.ends_with(".localhost") {
                HostKind::Loopback
            } else if d.ends_with(".local") || d.ends_with(".home.arpa") || !d.contains('.') {
                HostKind::Private
            } else {
                HostKind::Public
            }
        }
    }
}

/// A blocked-host or allowed-site entry as stored: lowercase host without scheme, port, path,
/// leading `*.`/`.` or trailing dot. `None` for entries that aren't host names.
pub fn normalize_host_entry(entry: &str) -> Option<String> {
    let mut e = entry.trim().to_ascii_lowercase();
    if let Some((_, rest)) = e.split_once("://") {
        e = rest.to_string();
    }
    let e = e.split(['/', '?', '#']).next().unwrap_or_default();
    let e = e.rsplit_once('@').map_or(e, |(_, h)| h);
    // Strip a port, but keep bracketed IPv6 literals whole.
    let e = if e.starts_with('[') { e.split_once(']').map(|(h, _)| format!("{h}]")).unwrap_or_default() } else { e.split(':').next().unwrap_or_default().to_string() };
    let e = e.trim_start_matches("*.").trim_start_matches('.').trim_end_matches('.').to_string();
    let valid = !e.is_empty() && e.len() <= 253 && e.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '[' | ']' | ':' | '_'));
    valid.then_some(e)
}

/// Normalized, de-duplicated host list (settings patches).
pub fn normalize_host_list(list: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for entry in list {
        if let Some(h) = normalize_host_entry(entry)
            && !out.contains(&h)
        {
            out.push(h);
        }
        if out.len() >= MAX_HOST_ENTRIES {
            break;
        }
    }
    out
}

/// `host` is `entry` or a subdomain of it.
pub fn host_matches_entry(host: &str, entry: &str) -> bool {
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    host == entry || host.strip_suffix(entry).is_some_and(|rest| rest.ends_with('.'))
}

/// The site a URL belongs to for approvals: its registrable domain, or the host for IPs,
/// `localhost` and names without a public suffix. `None` for URLs without a host.
pub fn site_of(url: &str) -> Option<String> {
    let parsed = Url::parse(url.trim()).ok()?;
    let host = parsed.host_str()?.to_ascii_lowercase();
    if host.is_empty() {
        return None;
    }
    Some(urls::site_key(url).unwrap_or(host))
}

/// What an agent may do with a URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UrlVerdict {
    /// `about:blank`: no site involved.
    Blank,
    /// An http(s) URL of `site`; the caller still checks the site approval.
    Web { site: String },
}

fn refuse(code: ErrorCode, message: impl Into<String>) -> ToolError {
    ToolError::new(code, message)
}

/// Checks a URL an agent wants to open (`tab_open`, `tab_navigate`) or a page's committed URL
/// before acting on it: only `http(s)` with a host (and `about:blank`), never a blocked host,
/// private-network hosts only when allowed. `internal_page` for `sta://` URLs.
pub fn check_url(settings: &Settings, url: &str) -> Result<UrlVerdict, ToolError> {
    let url = url.trim();
    if urls::is_about_blank(url) {
        return Ok(UrlVerdict::Blank);
    }
    if urls::is_internal(url) {
        return Err(refuse(ErrorCode::InternalPage, "sta's own pages are not available to agents"));
    }
    let scheme = urls::scheme(url).unwrap_or_default();
    if scheme != "http" && scheme != "https" {
        let shown = if scheme.is_empty() { "this URL".to_string() } else { format!("{scheme}: URLs") };
        return Err(refuse(ErrorCode::UrlNotAllowed, format!("Agents can't open {shown}")));
    }
    let Ok(parsed) = Url::parse(url) else {
        return Err(refuse(ErrorCode::UrlNotAllowed, "Not a valid URL"));
    };
    let Some(host) = parsed.host() else {
        return Err(refuse(ErrorCode::UrlNotAllowed, "The URL has no host"));
    };
    let host_str = parsed.host_str().unwrap_or_default().to_ascii_lowercase();
    // Installing extensions changes the whole browser (and what agents can't see): never for agents.
    if urls::is_web_store_url(url) {
        return Err(refuse(ErrorCode::UrlNotAllowed, "Agents can't open the Chrome Web Store"));
    }
    if settings.agent_blocked_hosts.iter().any(|b| host_matches_entry(&host_str, b)) {
        return Err(refuse(ErrorCode::SiteBlocked, format!("{host_str} is blocked for agents")));
    }
    match host_kind(&host) {
        HostKind::Private | HostKind::LinkLocal if !settings.agent_allow_private_network => {
            return Err(refuse(ErrorCode::UrlNotAllowed, format!("{host_str} is on the local network, which agents may not open")).with_hint(
                "Private-network hosts are off for agents in sta Settings (loopback addresses are allowed).",
            ));
        }
        _ => {}
    }
    let site = site_of(url).unwrap_or(host_str);
    Ok(UrlVerdict::Web { site })
}

/// The site is approved for agents: all sites allowed, "Always" answered, or allowed for this
/// session.
pub fn site_approved(settings: &Settings, session_sites: &BTreeSet<String>, site: &str) -> bool {
    settings.agent_sites == AgentSites::All
        || settings.agent_allowed_sites.iter().any(|s| s == site)
        || session_sites.contains(site)
}

/// The tab is visible to agents under the scope setting.
pub fn in_scope(scope: AgentScope, agent_tabs: &BTreeSet<Id>, tab: Id) -> bool {
    scope == AgentScope::AllTabs || agent_tabs.contains(&tab)
}

/// Access check for a tool: `read_only` tools run with any access that isn't Off.
pub fn check_access(access: AgentAccess, tool_is_read_only: bool) -> Result<(), ToolError> {
    match access {
        AgentAccess::Off => Err(ToolError::new(ErrorCode::AccessOff, "AI agent access is off in sta")),
        AgentAccess::ReadOnly if !tool_is_read_only => Err(ToolError::new(ErrorCode::ReadOnly, "Agent access is read-only")),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings() -> Settings {
        Settings::default()
    }

    fn code(r: Result<UrlVerdict, ToolError>) -> Option<ErrorCode> {
        r.err().map(|e| e.code)
    }

    #[test]
    fn schemes_and_internal_pages() {
        let s = settings();
        assert_eq!(check_url(&s, "about:blank"), Ok(UrlVerdict::Blank));
        assert_eq!(check_url(&s, "https://www.example.com/a?b"), Ok(UrlVerdict::Web { site: "example.com".into() }));
        assert_eq!(code(check_url(&s, "sta://settings/")), Some(ErrorCode::InternalPage));
        for url in ["file:///C:/Windows/win.ini", "javascript:alert(1)", "data:text/html,x", "chrome://version", "view-source:https://a.com", "mailto:a@b.c", "ftp://x.com/", "about:settings", "not a url", "https://"] {
            assert_eq!(code(check_url(&s, url)), Some(ErrorCode::UrlNotAllowed), "{url}");
        }
        // The Chrome Web Store: agents never install extensions (docs/MCP.md "Threat model").
        for url in ["https://chromewebstore.google.com/detail/x/abcdefghijklmnopabcdefghijklmnop", "https://chrome.google.com/webstore/category/extensions"] {
            assert_eq!(code(check_url(&s, url)), Some(ErrorCode::UrlNotAllowed), "{url}");
        }
        assert!(check_url(&s, "https://chrome.google.com/").is_ok());
    }

    #[test]
    fn private_network_rules() {
        let mut s = settings();
        for url in ["http://127.0.0.1:8080/", "http://localhost:3000/x", "http://[::1]/", "http://app.localhost/", "http://2130706433/"] {
            assert!(check_url(&s, url).is_ok(), "loopback {url}");
        }
        for url in [
            "http://192.168.0.1/",
            "http://10.0.0.8/admin",
            "http://172.16.5.4/",
            "http://169.254.169.254/latest/meta-data",
            "http://printer.local/",
            "http://router/",
            "http://[fe80::1]/",
            "http://[fd00::5]/",
            "http://0.0.0.0:8080/",
            "http://100.64.1.1/",
            "http://[::ffff:192.168.1.1]/",
            "http://0xC0A80001/",
            "http://nas.home.arpa/",
        ] {
            assert_eq!(code(check_url(&s, url)), Some(ErrorCode::UrlNotAllowed), "{url}");
        }
        s.agent_allow_private_network = true;
        assert!(check_url(&s, "http://192.168.0.1/").is_ok());
        assert!(check_url(&s, "http://169.254.169.254/").is_ok());
        assert_eq!(check_url(&s, "http://printer.local/"), Ok(UrlVerdict::Web { site: "printer.local".into() }));
        // Public addresses are public.
        assert!(check_url(&settings(), "http://8.8.8.8/").is_ok());
        assert!(check_url(&settings(), "http://172.32.0.1/").is_ok());
    }

    #[test]
    fn blocked_hosts_match_subdomains_only() {
        let mut s = settings();
        s.agent_blocked_hosts = normalize_host_list(&["https://Bank.example/login".into(), "*.mail.test".into(), "bad entry!".into(), "bank.example".into()]);
        assert_eq!(s.agent_blocked_hosts, vec!["bank.example".to_string(), "mail.test".to_string()]);
        assert_eq!(code(check_url(&s, "https://bank.example/")), Some(ErrorCode::SiteBlocked));
        assert_eq!(code(check_url(&s, "https://www.bank.example/")), Some(ErrorCode::SiteBlocked));
        assert_eq!(code(check_url(&s, "https://inbox.mail.test/")), Some(ErrorCode::SiteBlocked));
        assert!(check_url(&s, "https://notbank.example/").is_ok());
        assert!(check_url(&s, "https://bank.example.org/").is_ok());
    }

    #[test]
    fn host_entry_normalization() {
        assert_eq!(normalize_host_entry("  HTTPS://user:pw@Example.COM:8443/path?q#f "), Some("example.com".into()));
        assert_eq!(normalize_host_entry(".example.com."), Some("example.com".into()));
        assert_eq!(normalize_host_entry("[::1]:80"), Some("[::1]".into()));
        assert_eq!(normalize_host_entry(""), None);
        assert_eq!(normalize_host_entry("a b"), None);
    }

    #[test]
    fn site_approval_and_scope() {
        let mut s = settings();
        let mut session = BTreeSet::new();
        assert_eq!(site_of("https://docs.rust-lang.org/std"), Some("rust-lang.org".into()));
        assert_eq!(site_of("http://127.0.0.1:9000/"), Some("127.0.0.1".into()));
        assert_eq!(site_of("about:blank"), None);
        assert!(!site_approved(&s, &session, "example.com"));
        session.insert("example.com".to_string());
        assert!(site_approved(&s, &session, "example.com"));
        assert!(!site_approved(&s, &BTreeSet::new(), "example.com"));
        s.agent_allowed_sites.push("example.com".into());
        assert!(site_approved(&s, &BTreeSet::new(), "example.com"));
        s.agent_sites = AgentSites::All;
        assert!(site_approved(&s, &BTreeSet::new(), "anything.org"));

        let tabs: BTreeSet<Id> = [3, 5].into_iter().collect();
        assert!(in_scope(AgentScope::AgentTabs, &tabs, 3));
        assert!(!in_scope(AgentScope::AgentTabs, &tabs, 4));
        assert!(in_scope(AgentScope::AllTabs, &tabs, 4));
    }

    #[test]
    fn access_levels() {
        assert_eq!(check_access(AgentAccess::Off, true).unwrap_err().code, ErrorCode::AccessOff);
        assert!(check_access(AgentAccess::ReadOnly, true).is_ok());
        assert_eq!(check_access(AgentAccess::ReadOnly, false).unwrap_err().code, ErrorCode::ReadOnly);
        assert!(check_access(AgentAccess::Full, false).is_ok());
    }
}
