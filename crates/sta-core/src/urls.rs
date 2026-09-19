//! URL helpers used for display, tracker stripping, peek decisions and boosts.

use url::{Host, Url};

/// Prefix of internal page URLs (scheme case-insensitive).
pub const INTERNAL_PREFIX: &str = "sta://";

/// `true` for `sta://` URLs (internal pages served by the shell).
pub fn is_internal(url: &str) -> bool {
    url.get(..INTERNAL_PREFIX.len()).is_some_and(|s| s.eq_ignore_ascii_case(INTERNAL_PREFIX))
}

/// `true` for `http:`/`https:` URLs.
pub fn is_web(url: &str) -> bool {
    scheme(url).is_some_and(|s| s == "http" || s == "https")
}

/// Lowercase scheme of `url` (text before the first `:`), if it is syntactically a scheme.
pub fn scheme(url: &str) -> Option<String> {
    let t = url.trim_start();
    let idx = t.find(':')?;
    let s = &t[..idx];
    let mut chars = s.chars();
    let first = chars.next()?;
    if !first.is_ascii_alphabetic() || !chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.')) {
        return None;
    }
    Some(s.to_ascii_lowercase())
}

/// Schemes Chromium loads itself (everything else is an external protocol handed to the OS).
pub const BROWSER_SCHEMES: &[&str] = &[
    "http",
    "https",
    "file",
    "about",
    "data",
    "blob",
    "filesystem",
    "view-source",
    "javascript",
    "sta",
    "chrome",
    "chrome-error",
    "chrome-untrusted",
    "chrome-devtools",
    "chrome-extension",
    "devtools",
    "ws",
    "wss",
];

/// `true` for a URL whose scheme Chromium does not load itself (`mailto:`, `tel:`,
/// `ms-settings:`, `zoommtg://…`): such URLs are handed to the OS, never loaded in a tab.
/// One-letter "schemes" are drive letters (`C:\…`, `c:/…`), not protocols, and don't count.
pub fn is_external_scheme(url: &str) -> bool {
    scheme(url.trim()).is_some_and(|s| s.len() >= 2 && !BROWSER_SCHEMES.contains(&s.as_str()))
}

/// Internal pages, browser-internal and script URLs.
pub fn is_privileged_or_script(url: &str) -> bool {
    matches!(scheme(url).as_deref(), Some("sta" | "javascript" | "chrome" | "devtools" | "chrome-devtools"))
}

/// Dangerous schemes (a denylist): local files and data (`file:`, `data:`, `filesystem:`),
/// `view-source:`, `javascript:`, internal pages (`sta:`) and browser-internal schemes
/// (`chrome:`, `devtools:`, `chrome-devtools:`). Core decides what web content may open with the
/// allowlist [`web_content_may_open`], which refuses all of these too except `file:` links
/// opened from `file:` pages.
///
/// The scheme is read the way a URL parser would: leading/trailing C0 controls and spaces are
/// ignored and ASCII tabs/newlines inside are removed, so `" java\tscript:"` is caught too.
pub fn is_blocked_for_web_content(url: &str) -> bool {
    matches!(
        scheme(&parser_cleaned(url)).as_deref(),
        Some("file" | "data" | "filesystem" | "view-source" | "javascript" | "sta" | "chrome" | "devtools" | "chrome-devtools")
    )
}

/// `url` as a URL parser reads it: leading/trailing C0 controls and spaces removed, ASCII tabs and
/// newlines inside removed.
fn parser_cleaned(url: &str) -> String {
    url.trim_matches(|c: char| c <= ' ').chars().filter(|c| !matches!(c, '\t' | '\n' | '\r')).collect()
}

/// The scheme a URL *parser* reads (leading controls/spaces and inner tabs/newlines removed first),
/// for callers that must decide on a scheme before [`web_content_may_open`] sees the URL — so that
/// `" fi\tle:///C:/…"` is recognised as `file` rather than as no scheme at all.
pub fn parsed_scheme(url: &str) -> Option<String> {
    scheme(&parser_cleaned(url))
}

/// Allowlist for URLs that web content opens in a tab by itself (link clicks, popups, cross-site
/// links from pinned tabs, links dragged onto the sidebar):
/// - `http:` / `https:` URLs with a host;
/// - `about:blank` (optionally with a query or fragment);
/// - `blob:` URLs whose origin is http(s) (`blob:https://site/uuid`);
/// - `file:` URLs, but only when the opening page (`opener_url`) is itself a `file:` page.
///
/// Everything else is refused: `data:`, `javascript:`, `view-source:`, `filesystem:`,
/// `sta:`, `chrome:`, `devtools:`, other `about:` pages, `mailto:` and other external
/// protocols, opaque `blob:` URLs. The URL is read the way a URL parser would (see
/// [`is_blocked_for_web_content`]), so `" java\tscript:"` is refused too.
pub fn web_content_may_open(url: &str, opener_url: Option<&str>) -> bool {
    let cleaned = parser_cleaned(url);
    let Some(scheme) = scheme(&cleaned) else { return false };
    match scheme.as_str() {
        "http" | "https" => Url::parse(&cleaned).is_ok_and(|u| u.host_str().is_some_and(|h| !h.is_empty())),
        "about" => is_about_blank(&cleaned),
        "blob" => Url::parse(&cleaned)
            .ok()
            .and_then(|u| Url::parse(u.path()).ok())
            .is_some_and(|inner| matches!(inner.scheme(), "http" | "https") && inner.host_str().is_some_and(|h| !h.is_empty())),
        "file" => opener_url.and_then(|o| self::scheme(&parser_cleaned(o))).is_some_and(|s| s == "file"),
        _ => false,
    }
}

/// The Chrome Web Store: `chromewebstore.google.com` (and subdomains) or the old
/// `chrome.google.com/webstore` pages, http(s) only.
pub fn is_web_store_url(url: &str) -> bool {
    let Ok(parsed) = Url::parse(&parser_cleaned(url)) else { return false };
    if !matches!(parsed.scheme(), "http" | "https") {
        return false;
    }
    let host = parsed.host_str().unwrap_or_default().trim_end_matches('.').to_ascii_lowercase();
    host == "chromewebstore.google.com"
        || host.ends_with(".chromewebstore.google.com")
        || (host == "chrome.google.com" && (parsed.path() == "/webstore" || parsed.path().starts_with("/webstore/")))
}

/// `[a-p]{32}`: a Chromium extension id.
pub fn is_extension_id(id: &str) -> bool {
    id.len() == 32 && id.bytes().all(|b| (b'a'..=b'p').contains(&b))
}

/// `chrome-extension://<id>/<page>` → `(id, page)`, the page a relative path without query or
/// fragment. `None` for anything else, a bad id, an empty page, and paths with `..`, `.`, `\`,
/// `:`, percent-encoded dots or separators (no traversal tricks, even ones a URL parser would
/// resolve inside the extension's origin).
pub fn extension_url_parts(url: &str) -> Option<(String, String)> {
    let cleaned = parser_cleaned(url);
    let rest = cleaned.get(..19).filter(|s| s.eq_ignore_ascii_case("chrome-extension://")).map(|_| &cleaned[19..])?;
    let (id, path) = rest.split_once('/')?;
    if !is_extension_id(id) {
        return None;
    }
    let page = path.split(['?', '#']).next().unwrap_or_default();
    let lower = page.to_ascii_lowercase();
    if page.is_empty()
        || page.contains('\\')
        || page.contains(':')
        || lower.contains("%2e")
        || lower.contains("%2f")
        || lower.contains("%5c")
        || page.split('/').any(|s| s.is_empty() || s == "." || s == "..")
    {
        return None;
    }
    Some((id.to_string(), page.to_string()))
}

/// `chrome-extension://<id>/<page>` for a page sta itself opens (the picker's options tab, the popup
/// card). The inverse of [`extension_url_parts`], and just as strict: the id must be `[a-p]{32}`,
/// the page a relative path with no scheme, query, fragment, backslash, `..`, `.` or empty segment
/// — a manifest is just a file on disk, and a hostile one must not be able to name
/// `../../../other/page.html`, `javascript:…` or a Windows path.
pub fn extension_page_url(id: &str, page: &str) -> Option<String> {
    let page = page.trim().trim_start_matches('/');
    let url = format!("chrome-extension://{id}/{page}");
    // Round-tripping through the reader is the guarantee: whatever it would refuse to *read* is
    // never built here either.
    let (parsed_id, parsed_page) = extension_url_parts(&url)?;
    (parsed_id == id && parsed_page == page).then_some(url)
}

/// `*` in `pattern` matches any run of characters (manifest `web_accessible_resources`).
fn glob_matches(pattern: &str, text: &str) -> bool {
    let pattern = pattern.trim_start_matches('/');
    let parts: Vec<&str> = pattern.split('*').collect();
    if parts.len() == 1 {
        return pattern == text;
    }
    let (first, last) = (parts[0], parts[parts.len() - 1]);
    if !text.starts_with(first) || !text[first.len()..].ends_with(last) || text.len() < first.len() + last.len() {
        return false;
    }
    let mut rest = &text[first.len()..text.len() - last.len()];
    for part in &parts[1..parts.len() - 1] {
        match rest.find(part) {
            Some(i) => rest = &rest[i + part.len()..],
            None => return false,
        }
    }
    true
}

/// What core does with a URL a Chrome-created browser wanted to load (`ForeignTabRequested`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ForeignTabVerdict {
    /// Open it as a tab.
    Open,
    /// An extension page that isn't one of its declared pages: ask with a toast.
    Ask { id: String },
    /// Never opened.
    Refuse,
}

/// The rule of `ForeignTabRequested` (see `Command::ForeignTabRequested`): `http(s)` with a host
/// opens; `chrome-extension://<id>/<page>` opens when `extension` is that id's data and the page is
/// declared (options, popup, side panel), web-accessible to every site, or the extension is
/// `recently_installed`; other extension pages ask; everything else is refused.
///
/// **The verdict is about the page, not about who asked for it** (FINAL PLAN §2; ARCHITECTURE §4.5
/// "Who asked"). Chromium creates the window and sta cancels the navigation before it commits, so
/// with prebuilt CEF (user decision D1a) nothing in `on_before_browse` carries an initiator: a
/// `_crx_<id>` title only exists for popup windows, which are never adopted, and a request for
/// `chrome-extension://X/<page>` looks the same whether X or another installed extension made it.
/// So an installed extension can have sta open another extension's declared page — a navigation
/// Chromium itself refuses for resources that are not web-accessible. Accepted, because every
/// alternative either breaks `runtime.openOptionsPage` (the flow that motivated the rule) or asks
/// the user a question they cannot answer. Consequences that are dealt with instead: the toast never
/// claims the named extension asked (`store::foreign`), and the page opens in a foreground tab the
/// user sees rather than in a hidden window. Phase 3's picker owns the popup that starts these
/// flows, so it can attribute them; this rule is then worth tightening.
pub fn foreign_tab_verdict(url: &str, extension: Option<&crate::command::ForeignExtension>, recently_installed: bool) -> ForeignTabVerdict {
    let cleaned = parser_cleaned(url);
    match scheme(&cleaned).as_deref() {
        Some("http" | "https") if web_content_may_open(&cleaned, None) => ForeignTabVerdict::Open,
        Some("chrome-extension") => {
            let Some((id, page)) = extension_url_parts(&cleaned) else { return ForeignTabVerdict::Refuse };
            let Some(ext) = extension.filter(|e| e.id == id) else {
                return if recently_installed { ForeignTabVerdict::Open } else { ForeignTabVerdict::Ask { id } };
            };
            let declared = ext.pages.iter().any(|p| p.trim_start_matches('/') == page);
            let accessible = ext.web_accessible.iter().any(|p| glob_matches(p, &page));
            if declared || accessible || ext.recently_installed || recently_installed {
                ForeignTabVerdict::Open
            } else {
                ForeignTabVerdict::Ask { id }
            }
        }
        _ => ForeignTabVerdict::Refuse,
    }
}

/// `about:blank`, optionally followed by a query or fragment (case-insensitive).
pub fn is_about_blank(url: &str) -> bool {
    let t = url.trim();
    t.get(..11).is_some_and(|head| head.eq_ignore_ascii_case("about:blank")) && t[11..].chars().next().is_none_or(|c| c == '?' || c == '#')
}

/// Display name of an internal page host ("settings" → "Settings").
pub fn internal_page_name(host: &str) -> String {
    match host {
        "settings" => "Settings".into(),
        "archive" => "Archive".into(),
        "history" => "History".into(),
        "boosts" => "Boosts".into(),
        "" => "sta".into(),
        other => {
            let mut c = other.chars();
            match c.next() {
                Some(f) => f.to_uppercase().chain(c).collect(),
                None => String::new(),
            }
        }
    }
}

/// Host without leading `www.`; internal pages → their page name ("Settings"); `file:` → file
/// name; unparsable → the input.
///
/// Details: IDN hosts are shown in Unicode unless a label mixes Latin, Cyrillic or Greek letters
/// (spoofing guard → punycode); an explicit non-default port is appended (`localhost:3000`);
/// `view-source:` URLs show the inner URL's host; `about:` URLs show themselves (`about:blank`),
/// `data:` URLs show `data:`.
pub fn display_host(url: &str) -> String {
    let trimmed = url.trim();
    if let Some(inner) = strip_prefix_ci(trimmed, "view-source:") {
        return display_host(inner);
    }
    let Ok(u) = Url::parse(trimmed) else { return url.to_string() };
    match u.scheme() {
        "sta" => internal_page_name(u.host_str().unwrap_or("")),
        "file" => file_name(&u).unwrap_or_else(|| "File".into()),
        "about" => trimmed.split(['?', '#']).next().unwrap_or(trimmed).to_ascii_lowercase(),
        "data" => "data:".into(),
        _ => match u.host() {
            Some(host) => {
                let mut h = match host {
                    Host::Domain(d) => display_domain(d),
                    Host::Ipv4(ip) => ip.to_string(),
                    Host::Ipv6(ip) => format!("[{ip}]"),
                };
                if let Some(port) = u.port() {
                    h.push_str(&format!(":{port}"));
                }
                h
            }
            None => url.to_string(),
        },
    }
}

fn strip_prefix_ci<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    let head = s.get(..prefix.len())?;
    head.eq_ignore_ascii_case(prefix).then(|| &s[prefix.len()..])
}

fn file_name(u: &Url) -> Option<String> {
    let seg = u.path_segments()?.rfind(|s| !s.is_empty())?;
    let decoded = percent_decode(seg);
    (!decoded.is_empty()).then_some(decoded)
}

/// Lenient percent-decoding (invalid escapes are kept verbatim, invalid UTF-8 replaced).
pub(crate) fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok().and_then(|h| u8::from_str_radix(h, 16).ok());
            if let Some(b) = hex {
                out.push(b);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// ASCII (punycode) domain → display form: `www.` stripped, Unicode unless mixed-script.
fn display_domain(ascii: &str) -> String {
    let d = ascii.trim_end_matches('.').to_ascii_lowercase();
    let d = match d.strip_prefix("www.") {
        Some(rest) if rest.contains('.') => rest.to_string(),
        _ => d,
    };
    if !d.split('.').any(|l| l.starts_with("xn--")) {
        return d;
    }
    let unicode = url::quirks::domain_to_unicode(&d);
    if unicode.is_empty() || unicode.split('.').any(is_mixed_script) {
        d
    } else {
        unicode
    }
}

/// Rough mixed-script check used to decide whether an IDN label is safe to show in Unicode:
/// letters from more than one of Latin / Cyrillic / Greek in the same label.
fn is_mixed_script(label: &str) -> bool {
    let mut seen = [false; 3];
    for c in label.chars() {
        let cp = c as u32;
        let script = if c.is_ascii_alphabetic() || (0x00C0..=0x024F).contains(&cp) || (0x1E00..=0x1EFF).contains(&cp) {
            0
        } else if (0x0400..=0x052F).contains(&cp) {
            1
        } else if (0x0370..=0x03FF).contains(&cp) || (0x1F00..=0x1FFF).contains(&cp) {
            2
        } else {
            continue;
        };
        seen[script] = true;
    }
    seen.iter().filter(|s| **s).count() > 1
}

const TRACKER_PARAMS: &[&str] = &[
    "fbclid", "gclid", "dclid", "gbraid", "wbraid", "msclkid", "mc_cid", "mc_eid", "igshid", "igsh", "ref_src", "_hsenc",
    "_hsmi", "yclid",
];

/// Remove tracking query parameters (arc_spec §2.16 list: utm_*, fbclid, gclid, dclid, gbraid,
/// wbraid, msclkid, mc_cid, mc_eid, igshid, igsh, ref_src, _hsenc, _hsmi, yclid; `si` only on
/// youtube.com/youtu.be/spotify.com). Keeps everything else byte-identical where possible.
/// Only http(s) URLs are cleaned; parameter names compare case-insensitively.
pub fn clean_url(url: &str) -> String {
    if !is_web(url) {
        return url.to_string();
    }
    let Some(q_start) = url.find('?') else { return url.to_string() };
    let frag_start = url.find('#');
    if frag_start.is_some_and(|f| f < q_start) {
        return url.to_string();
    }
    let q_end = frag_start.unwrap_or(url.len());
    let host = host(url).unwrap_or_default();
    let si_host = ["youtube.com", "youtu.be", "spotify.com"].iter().any(|d| host == *d || host.ends_with(&format!(".{d}")));
    let query = &url[q_start + 1..q_end];
    let mut removed = false;
    let kept: Vec<&str> = query
        .split('&')
        .filter(|seg| {
            let key = seg.split('=').next().unwrap_or("");
            let key = percent_decode(&key.replace('+', " ")).to_ascii_lowercase();
            let tracker = key.starts_with("utm_") || TRACKER_PARAMS.contains(&key.as_str()) || (si_host && key == "si");
            removed |= tracker;
            !tracker
        })
        .collect();
    if !removed {
        return url.to_string();
    }
    let mut out = String::with_capacity(url.len());
    out.push_str(&url[..q_start]);
    if kept.iter().any(|s| !s.is_empty()) {
        out.push('?');
        out.push_str(&kept.join("&"));
    }
    out.push_str(&url[q_end..]);
    out
}

/// Registrable domain (eTLD+1) via the public suffix list; `None` for IPs/localhost/non-http.
pub fn registrable_domain(url: &str) -> Option<String> {
    let u = Url::parse(url.trim()).ok()?;
    if !matches!(u.scheme(), "http" | "https") {
        return None;
    }
    match u.host()? {
        Host::Domain(d) => {
            let d = d.trim_end_matches('.').to_ascii_lowercase();
            if d == "localhost" || !d.contains('.') {
                return None;
            }
            psl::domain_str(&d).map(str::to_string)
        }
        _ => None,
    }
}

/// `true` when both URLs are http(s) with the same registrable domain.
pub fn same_site(a: &str, b: &str) -> bool {
    match (registrable_domain(a), registrable_domain(b)) {
        (Some(x), Some(y)) => x == y,
        _ => false,
    }
}

/// Site identity for "is this another site?" decisions: registrable domain, else the host
/// (IPs, localhost, single-label hosts).
pub fn site_key(url: &str) -> Option<String> {
    registrable_domain(url).or_else(|| host(url))
}

/// Host (lowercase) of an http(s) URL.
pub fn host(url: &str) -> Option<String> {
    let u = Url::parse(url.trim()).ok()?;
    if !matches!(u.scheme(), "http" | "https") {
        return None;
    }
    u.host_str().map(|h| h.trim_end_matches('.').to_ascii_lowercase()).filter(|h| !h.is_empty())
}

/// Serialized origin (`scheme://host[:port]`) of a URL with a tuple origin; `None` otherwise.
pub fn origin(url: &str) -> Option<String> {
    let u = Url::parse(url.trim()).ok()?;
    let o = u.origin();
    o.is_tuple().then(|| o.ascii_serialization())
}

/// Normalize an origin string reported by CEF (`https://Example.com/` → `https://example.com`).
pub fn normalize_origin(origin_str: &str) -> String {
    origin(origin_str).unwrap_or_else(|| origin_str.trim().trim_end_matches('/').to_ascii_lowercase())
}

/// URL without fragment and trailing slash, lowercased: used to de-duplicate tabs and history.
pub fn dedupe_key(url: &str) -> String {
    let u = url.split('#').next().unwrap_or(url);
    let u = u.strip_suffix('/').unwrap_or(u);
    u.to_ascii_lowercase()
}

/// Does a boost `boost_host` apply to `url` (exact host or any subdomain)?
pub fn host_matches(boost_host: &str, url: &str) -> bool {
    let Some(h) = host(url) else { return false };
    let b = boost_host.trim().trim_start_matches("www.").to_ascii_lowercase();
    !b.is_empty() && (h == b || h.ends_with(&format!(".{b}")) || h.trim_start_matches("www.") == b)
}

/// URL without its query and fragment. "The same page" when sta asks whether a page it opened is
/// already in a tab: an extension's options page routinely routes itself on load
/// (`options.html#general`), and a second Enter must not pile up a duplicate tab.
pub fn without_query_or_fragment(url: &str) -> &str {
    url.split(['?', '#']).next().unwrap_or(url)
}

/// "Pinned tab navigated away": compares URLs ignoring fragment and a trailing slash.
pub fn differs_from_pinned(url: &str, pinned_url: &str) -> bool {
    fn norm(u: &str) -> &str {
        let u = u.split('#').next().unwrap_or(u);
        u.strip_suffix('/').unwrap_or(u)
    }
    norm(url) != norm(pinned_url)
}

/// Path + query of a URL (everything after the authority), used for fuzzy matching.
pub fn path_part(url: &str) -> &str {
    let rest = match url.find("://") {
        Some(i) => &url[i + 3..],
        None => return url,
    };
    match rest.find('/') {
        Some(i) => &rest[i..],
        None => "",
    }
}

/// Whether the URL is shown with a lock (secure) glyph: https, sta, file, about, data,
/// chrome, and `view-source:` of those.
pub fn is_secure(url: &str) -> bool {
    let t = url.trim();
    if t.is_empty() {
        return true;
    }
    if let Some(inner) = strip_prefix_ci(t, "view-source:") {
        return is_secure(inner);
    }
    matches!(scheme(t).as_deref(), Some("https" | "wss" | "sta" | "file" | "about" | "data" | "chrome" | "devtools"))
}

/// Display title: custom title → page title → display host → URL.
pub fn display_title(custom: Option<&str>, title: &str, url: &str) -> String {
    if let Some(c) = custom.map(str::trim).filter(|c| !c.is_empty()) {
        return c.to_string();
    }
    let t = title.trim();
    if !t.is_empty() {
        return t.to_string();
    }
    let h = display_host(url);
    if !h.is_empty() {
        return h;
    }
    url.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The picker builds these URLs from manifest strings, so everything a hostile manifest could
    /// put in `options_ui.page` has to come back `None`.
    #[test]
    fn extension_page_urls_are_built_as_strictly_as_they_are_read() {
        let id = "abcdefghijklmnopabcdefghijklmnop";
        assert_eq!(extension_page_url(id, "options.html").as_deref(), Some("chrome-extension://abcdefghijklmnopabcdefghijklmnop/options.html"));
        assert_eq!(extension_page_url(id, "/popup/index.html").as_deref(), Some("chrome-extension://abcdefghijklmnopabcdefghijklmnop/popup/index.html"));
        for bad in [
            "",
            " ",
            "../other.html",
            "a/../b.html",
            "./a.html",
            "a//b.html",
            "a\\b.html",
            "javascript:alert(1)",
            "https://evil.test/x.html",
            "a.html?x=1",
            "a.html#f",
            "%2e%2e/x.html",
            "a%2fb.html",
        ] {
            assert_eq!(extension_page_url(id, bad), None, "{bad}");
        }
        assert_eq!(extension_page_url("ABCDEFGHIJKLMNOPABCDEFGHIJKLMNOP", "a.html"), None);
        assert_eq!(extension_page_url("short", "a.html"), None);
    }

    #[test]
    fn foreign_tab_rules() {
        use crate::command::ForeignExtension;
        let id = "abcdefghijklmnopabcdefghijklmnop";
        let ext = ForeignExtension {
            id: id.into(),
            name: "Blocker".into(),
            pages: vec!["options.html".into(), "popup/index.html".into()],
            web_accessible: vec!["welcome/*.html".into(), "/installed.html".into()],
            recently_installed: false,
        };
        let v = |url: &str, e: Option<&ForeignExtension>, recent: bool| foreign_tab_verdict(url, e, recent);
        assert_eq!(v("https://getadblock.com/installed/?x=1", None, false), ForeignTabVerdict::Open);
        assert_eq!(v("http://127.0.0.1:8841/page", None, false), ForeignTabVerdict::Open);
        assert_eq!(v(&format!("chrome-extension://{id}/options.html"), Some(&ext), false), ForeignTabVerdict::Open);
        assert_eq!(v(&format!("chrome-extension://{id}/popup/index.html?tab=1#x"), Some(&ext), false), ForeignTabVerdict::Open);
        assert_eq!(v(&format!("chrome-extension://{id}/welcome/a.html"), Some(&ext), false), ForeignTabVerdict::Open);
        assert_eq!(v(&format!("chrome-extension://{id}/installed.html"), Some(&ext), false), ForeignTabVerdict::Open);
        // Undeclared pages ask; a fresh install opens them.
        assert_eq!(v(&format!("chrome-extension://{id}/app/app.html"), Some(&ext), false), ForeignTabVerdict::Ask { id: id.into() });
        assert_eq!(v(&format!("chrome-extension://{id}/app/app.html"), Some(&ext), true), ForeignTabVerdict::Open);
        let fresh = ForeignExtension { recently_installed: true, ..ext.clone() };
        assert_eq!(v(&format!("chrome-extension://{id}/app/app.html"), Some(&fresh), false), ForeignTabVerdict::Open);
        assert_eq!(v(&format!("chrome-extension://{id}/app.html"), None, false), ForeignTabVerdict::Ask { id: id.into() });
        // Another extension's data never vouches for a page.
        let other = "pppppppppppppppppppppppppppppppp";
        assert_eq!(v(&format!("chrome-extension://{other}/options.html"), Some(&ext), false), ForeignTabVerdict::Ask { id: other.into() });
        // Traversal, bad ids, other schemes.
        for refused in [
            format!("chrome-extension://{id}/../{other}/options.html"),
            format!("chrome-extension://{id}/a/../options.html"),
            format!("chrome-extension://{id}/%2e%2e/options.html"),
            format!("chrome-extension://{id}/a%2Foptions.html"),
            format!("chrome-extension://{id}/a\\options.html"),
            format!("chrome-extension://{id}//options.html"),
            format!("chrome-extension://{id}/"),
            "chrome-extension://ABCDEFGHIJKLMNOPABCDEFGHIJKLMNOP/options.html".to_string(),
            "chrome-extension://abcdefghijklmnopabcdefghijklmnoz/options.html".to_string(),
            "chrome-extension://short/options.html".to_string(),
            "javascript:alert(1)".to_string(),
            " java\tscript:alert(1)".to_string(),
            "chrome://settings".to_string(),
            "chrome://extensions/?options=abcdefghijklmnopabcdefghijklmnop".to_string(),
            "sta://settings/".to_string(),
            "file:///C:/Windows/win.ini".to_string(),
            "data:text/html,x".to_string(),
            "about:blank".to_string(),
            "https://".to_string(),
            "mailto:a@b.c".to_string(),
        ] {
            assert_eq!(v(&refused, Some(&fresh), true), ForeignTabVerdict::Refuse, "{refused}");
        }
        assert_eq!(extension_url_parts(&format!("CHROME-EXTENSION://{id}/p.html")), Some((id.to_string(), "p.html".to_string())));
        assert!(glob_matches("*", "a/b.html") && glob_matches("a*b*c", "aXbYc") && !glob_matches("a*b", "ba"));
    }

    #[test]
    fn web_store_urls() {
        for store in [
            "https://chromewebstore.google.com/detail/adblock/gighmmpiobklfepjocnamgkkbiglidom",
            "HTTPS://ChromeWebStore.Google.Com./",
            "http://chromewebstore.google.com",
            "https://x.chromewebstore.google.com/",
            "https://chrome.google.com/webstore/detail/x",
            "https://chrome.google.com/webstore",
        ] {
            assert!(is_web_store_url(store), "{store}");
        }
        for other in [
            "https://chrome.google.com/",
            "https://chrome.google.com/webstorefront",
            "https://google.com/",
            "https://chromewebstore.google.com.evil.example/",
            "chrome://extensions",
            "file:///chromewebstore.google.com",
        ] {
            assert!(!is_web_store_url(other), "{other}");
        }
    }

    #[test]
    fn display_host_cases() {
        assert_eq!(display_host("https://www.github.com/rust-lang"), "github.com");
        assert_eq!(display_host("https://docs.rs/serde"), "docs.rs");
        assert_eq!(display_host("http://localhost:3000/api"), "localhost:3000");
        assert_eq!(display_host("https://example.com:443/"), "example.com");
        assert_eq!(display_host("http://192.168.0.1/admin"), "192.168.0.1");
        assert_eq!(display_host("http://[::1]:8080/"), "[::1]:8080");
        assert_eq!(display_host("sta://settings/"), "Settings");
        assert_eq!(display_host("sta://archive"), "Archive");
        assert_eq!(display_host("sta://history/"), "History");
        assert_eq!(display_host("sta://boosts/?id=5"), "Boosts");
        assert_eq!(display_host("file:///C:/Users/me/My%20Doc.pdf"), "My Doc.pdf");
        assert_eq!(display_host("about:blank"), "about:blank");
        assert_eq!(display_host("view-source:https://www.example.org/x"), "example.org");
        assert_eq!(display_host("not a url"), "not a url");
        assert_eq!(display_host("https://www.com/"), "www.com");
    }

    #[test]
    fn display_host_idn() {
        assert_eq!(display_host("https://xn--bcher-kva.de/"), "bücher.de");
        assert_eq!(display_host("https://bücher.de/"), "bücher.de");
        // Latin "a" followed by Cyrillic letters in one label → keep punycode.
        let mixed = format!("https://{}/", url::quirks::domain_to_ascii("aрр.com"));
        let shown = display_host(&mixed);
        assert!(shown.starts_with("xn--"), "{shown}");
        // Pure Cyrillic is fine.
        assert_eq!(display_host("https://xn--e1afmkfd.xn--p1ai/"), "пример.рф");
    }

    #[test]
    fn clean_url_strips_trackers() {
        assert_eq!(clean_url("https://example.com/p?utm_source=x&id=5&fbclid=abc#frag"), "https://example.com/p?id=5#frag");
        assert_eq!(clean_url("https://example.com/p?utm_source=x&utm_medium=y"), "https://example.com/p");
        assert_eq!(clean_url("https://example.com/p?a=1&b=%20x"), "https://example.com/p?a=1&b=%20x");
        assert_eq!(clean_url("https://www.youtube.com/watch?v=abc&si=track"), "https://www.youtube.com/watch?v=abc");
        assert_eq!(clean_url("https://open.spotify.com/track/1?si=zzz"), "https://open.spotify.com/track/1");
        assert_eq!(clean_url("https://example.com/?si=keep"), "https://example.com/?si=keep");
        assert_eq!(clean_url("https://example.com/#a?utm_source=x"), "https://example.com/#a?utm_source=x");
        assert_eq!(clean_url("sta://settings/?utm_source=x"), "sta://settings/?utm_source=x");
        assert_eq!(clean_url("https://x.com/?GCLID=1&q=2"), "https://x.com/?q=2");
        assert_eq!(clean_url("https://x.com/?igsh=1&_hsenc=2&mc_eid=3&yclid=4"), "https://x.com/");
    }

    #[test]
    fn registrable_domains() {
        assert_eq!(registrable_domain("https://www.google.co.uk/search").as_deref(), Some("google.co.uk"));
        assert_eq!(registrable_domain("https://mail.google.com/").as_deref(), Some("google.com"));
        assert_eq!(registrable_domain("https://user.github.io/").as_deref(), Some("user.github.io"));
        assert_eq!(registrable_domain("http://localhost:3000/"), None);
        assert_eq!(registrable_domain("http://127.0.0.1/"), None);
        assert_eq!(registrable_domain("file:///C:/x"), None);
        assert!(same_site("https://a.example.com/", "http://b.example.com/x"));
        assert!(!same_site("https://a.github.io/", "https://b.github.io/"));
        assert_eq!(site_key("http://localhost:8080/").as_deref(), Some("localhost"));
    }

    #[test]
    fn host_and_origin() {
        assert_eq!(host("https://WWW.Example.COM/a").as_deref(), Some("www.example.com"));
        assert_eq!(host("sta://settings/"), None);
        assert_eq!(origin("https://meet.example.com/room?x").as_deref(), Some("https://meet.example.com"));
        assert_eq!(normalize_origin("https://Meet.Example.com/"), "https://meet.example.com");
        assert!(host_matches("example.com", "https://www.example.com/"));
        assert!(host_matches("example.com", "https://sub.example.com/"));
        assert!(!host_matches("example.com", "https://notexample.com/"));
        assert!(!differs_from_pinned("https://a.com/", "https://a.com"));
        assert!(!differs_from_pinned("https://a.com/x#y", "https://a.com/x"));
        assert!(differs_from_pinned("https://a.com/x", "https://a.com/y"));
    }

    #[test]
    fn misc_helpers() {
        assert!(is_internal("STA://settings"));
        assert!(is_web("HTTPS://x.com"));
        assert!(!is_web("file:///x"));
        assert!(is_secure("https://x.com"));
        assert!(!is_secure("http://x.com"));
        assert!(is_secure("view-source:https://x.com"));
        assert!(!is_secure("view-source:http://x.com"));
        assert!(is_secure("sta://settings"));
        assert_eq!(path_part("https://x.com/a/b?c"), "/a/b?c");
        assert_eq!(path_part("https://x.com"), "");
        assert!(is_privileged_or_script("javascript:alert(1)"));
        assert!(is_privileged_or_script("sta://settings"));
        assert!(!is_privileged_or_script("https://x.com"));
        for blocked in [
            "file:///C:/Windows/win.ini",
            "FILE://server/share",
            "data:text/html,<script>alert(1)</script>",
            "view-source:https://x.com/",
            "filesystem:https://x.com/temporary/a",
            "javascript:alert(1)",
            " java\tscript:alert(1)",
            "\u{1}javascript:alert(1)",
            "sta://settings/",
            "chrome://settings",
            "devtools://devtools/bundled/inspector.html",
            "chrome-devtools://devtools/",
        ] {
            assert!(is_blocked_for_web_content(blocked), "{blocked:?}");
        }
        for allowed in ["https://x.com/", "http://localhost:3000/", "about:blank", "blob:https://x.com/uuid", "mailto:a@b.c"] {
            assert!(!is_blocked_for_web_content(allowed), "{allowed:?}");
        }
        assert_eq!(display_title(Some("  "), "", "https://www.x.com/"), "x.com");
    }

    #[test]
    fn external_schemes() {
        for external in ["mailto:a@b.c", " tel:+123", "MS-SETTINGS:display", "zoommtg://zoom.us/join", "ftp://host/", "sms:+1"] {
            assert!(is_external_scheme(external), "{external:?}");
        }
        for browser in [
            "https://x.com/",
            "http://localhost:3000/",
            "file:///C:/x",
            "about:blank",
            "data:text/html,x",
            "blob:https://a/b",
            "view-source:https://a",
            "sta://settings/",
            "chrome://version",
            "chrome-error://chromewebdata/",
            "devtools://devtools/x",
            "javascript:alert(1)",
            "wss://x.com/socket",
            "C:\\Users\\me\\a.txt",
            "c:/temp/x",
            "",
            "no scheme",
            "1abc:foo",
        ] {
            assert!(!is_external_scheme(browser), "{browser:?}");
        }
    }

    #[test]
    fn web_content_allowlist() {
        for allowed in [
            "https://x.com/",
            "HTTP://localhost:3000/a?b#c",
            "  https://x.com/\n",
            "about:blank",
            "ABOUT:BLANK#frag",
            "about:blank?x",
            "blob:https://x.com/7c1f-uuid",
            "blob:http://127.0.0.1:8080/uuid",
        ] {
            assert!(web_content_may_open(allowed, None), "{allowed:?}");
            assert!(web_content_may_open(allowed, Some("https://opener.com/")), "{allowed:?}");
        }
        for refused in [
            "",
            "   ",
            "file:///C:/Windows/win.ini",
            "data:text/html,<h1>x</h1>",
            "javascript:alert(1)",
            " java\tscript:alert(1)",
            "view-source:https://x.com/",
            "filesystem:https://x.com/temporary/a",
            "sta://settings/",
            "chrome://settings",
            "devtools://devtools/bundled/inspector.html",
            "about:version",
            "about:blankx",
            "about:srcdoc",
            "mailto:a@b.c",
            "ms-settings:privacy",
            "blob:null/uuid",
            "blob:file:///C:/x",
            "blob:ws://x.com/uuid",
            "http://",
            "relative/path",
        ] {
            assert!(!web_content_may_open(refused, None), "{refused:?}");
            assert!(!web_content_may_open(refused, Some("https://opener.com/")), "{refused:?}");
        }
        // file: only from a file: page.
        assert!(web_content_may_open("file:///C:/docs/b.html", Some("file:///C:/docs/a.html")));
        assert!(web_content_may_open("FILE:///C:/docs/b.html", Some(" FILE:///C:/a.html")));
        assert!(!web_content_may_open("file:///C:/docs/b.html", Some("https://x.com/")));
        assert!(!web_content_may_open("file:///C:/docs/b.html", Some("about:blank")));
        assert!(!web_content_may_open("data:text/html,x", Some("file:///C:/a.html")));
        assert!(is_about_blank("about:blank") && !is_about_blank("about:blank-ish"));
        assert_eq!(display_title(Some("Mine"), "Page", "https://x.com/"), "Mine");
        assert_eq!(percent_decode("a%20b%zz%"), "a b%zz%");
        assert_eq!(scheme("view-source:https://x"), Some("view-source".into()));
        assert_eq!(scheme("localhost:3000"), Some("localhost".into()));
        assert_eq!(scheme("1http:x"), None);
    }
}
