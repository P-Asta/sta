//! The `sta://` scheme [owner: chrome] (ARCHITECTURE §6, docs/research/ipc.md §1, §2.4, §6).
//!
//! Responsibility:
//! - register the custom scheme in **every** process (`register_custom_scheme`, called from
//!   `App::on_register_custom_schemes`);
//! - serve the embedded `ui/` directory from one `SchemeHandlerFactory` with an in-memory
//!   `BytesHandler` (rust-embed; debug builds read `ui/` from disk, so UI edits need no rebuild);
//! - map URLs: `sta://<host>/<path>?q#f` → `common/<rest>` → `ui/common/<rest>`; empty path
//!   or no extension → `ui/<host>/index.html`; otherwise `ui/<host>/<path>`; unknown host → 404;
//! - add CSP / `Cache-Control: no-store` / `nosniff` headers to every response;
//! - overlay surfaces whose page is missing from `ui/` (`find`, `permission`, `switcher`, `toast`,
//!   `peek`) get a small built-in placeholder page (`res/overlay-placeholder.{html,js}`) so the
//!   shell's overlays stay usable (they are only shown after `ui.ready`). The real page always wins.
//!
//! Public API:
//! - `pub const SCHEME: &str`, `pub const ORIGIN_PREFIX: &str` (`"sta://"`)
//! - `pub const CSP: &str`
//! - `pub const HOSTS: &[&str]` — hosts that are served
//! - `pub fn register_custom_scheme(registrar: &mut SchemeRegistrar)`
//! - `pub fn register_factory()` — browser process, after `initialize` (UI thread)
//! - `pub fn is_sta_url(url: &str) -> bool`, `pub fn host_of(url: &str) -> Option<&str>`
//! - `pub fn resolve_asset(url: &str) -> Result<String, u16>` — pure URL → asset path mapping
//! - `pub fn placeholder_asset(path: &str) -> Option<(&'static str, &'static [u8])>` — fallback pages

use cef::*;
use std::borrow::Cow;
use std::sync::{Arc, Mutex};

pub const SCHEME: &str = "sta";
pub const ORIGIN_PREFIX: &str = "sta://";

/// Content-Security-Policy for every UI response (pages also carry it as a `<meta>`).
pub const CSP: &str = "default-src 'none'; script-src 'self'; style-src 'self' 'unsafe-inline'; \
img-src 'self' data: https:; font-src 'self' data:; connect-src 'self'; base-uri 'none'; \
form-action 'none'; frame-ancestors 'none'";

/// Hosts served by the factory. Anything else is a 404.
pub const HOSTS: &[&str] = &[
    "sidebar", "topbar", "command", "empty", "peek", "find", "permission", "switcher", "toast", "settings",
    "archive", "history", "boosts", "agent", "extension",
];

/// Extra hosts served only by debug builds. **Empty today**: it used to carry `_shelltest` (IPC
/// test pages under `ui/_shelltest/`), a directory that no longer exists — so the host was served
/// but every request for it 404'd, and ARCHITECTURE §6 documented a page that was not there. The
/// mechanism stays for the next debug-only surface; `sta_origins()` (devtools.rs) and
/// [`is_served_host`] read it.
#[cfg(debug_assertions)]
pub const DEBUG_HOSTS: &[&str] = &[];

#[derive(rust_embed::Embed)]
#[folder = "../../ui/"]
struct UiAssets;

/// Overlay hosts that get the built-in placeholder page when `ui/<host>/index.html` is missing.
pub const PLACEHOLDER_HOSTS: &[&str] = &["find", "permission", "switcher", "toast", "peek", "extension"];
static PLACEHOLDER_HTML: &[u8] = include_bytes!("../res/overlay-placeholder.html");
static PLACEHOLDER_JS: &[u8] = include_bytes!("../res/overlay-placeholder.js");

/// Built-in fallback for a missing overlay page: `(mime, body)` for `<host>/index.html` and
/// `<host>/__shell_placeholder.js` of the [`PLACEHOLDER_HOSTS`].
pub fn placeholder_asset(path: &str) -> Option<(&'static str, &'static [u8])> {
    let (host, file) = path.split_once('/')?;
    if !PLACEHOLDER_HOSTS.contains(&host) {
        return None;
    }
    match file {
        "index.html" => Some(("text/html", PLACEHOLDER_HTML)),
        "__shell_placeholder.js" => Some(("text/javascript", PLACEHOLDER_JS)),
        _ => None,
    }
}

/// Scheme options: STANDARD | SECURE | CORS_ENABLED | FETCH_ENABLED | DISPLAY_ISOLATED.
pub fn scheme_options() -> i32 {
    [
        SchemeOptions::STANDARD,
        SchemeOptions::SECURE,
        SchemeOptions::CORS_ENABLED,
        SchemeOptions::FETCH_ENABLED,
        SchemeOptions::DISPLAY_ISOLATED,
    ]
    .iter()
    .fold(0, |acc, o| acc | o.get_raw())
}

/// Registers the scheme. Must run in every process with identical options.
pub fn register_custom_scheme(registrar: &mut SchemeRegistrar) {
    let ok = registrar.add_custom_scheme(Some(&CefString::from(SCHEME)), scheme_options());
    if ok != 1 {
        log_error!("add_custom_scheme({SCHEME}) failed");
    }
}

/// Registers the global scheme handler factory (browser process).
pub fn register_factory() {
    let mut factory = StaSchemeFactory::new();
    let ok = register_scheme_handler_factory(Some(&CefString::from(SCHEME)), None, Some(&mut factory));
    if ok != 1 {
        log_error!("register_scheme_handler_factory({SCHEME}) failed");
    }
}

/// `true` for `sta://…` (case-insensitive scheme).
pub fn is_sta_url(url: &str) -> bool {
    sta_core::urls::is_internal(url)
}

/// Host of a `sta://` URL (`sta://sidebar/x` → `sidebar`).
pub fn host_of(url: &str) -> Option<&str> {
    if !is_sta_url(url) {
        return None;
    }
    let rest = &url[ORIGIN_PREFIX.len()..];
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    Some(&rest[..end])
}

fn is_served_host(host: &str) -> bool {
    #[cfg(debug_assertions)]
    if DEBUG_HOSTS.contains(&host) {
        return true;
    }
    HOSTS.contains(&host)
}

/// Maps a request URL to a path inside `ui/`. `Err(status)` for rejected requests.
pub fn resolve_asset(url: &str) -> Result<String, u16> {
    let host = host_of(url).ok_or(400u16)?;
    if !is_served_host(host) {
        return Err(404);
    }
    let rest = &url[ORIGIN_PREFIX.len() + host.len()..];
    let rest = rest.split(['?', '#']).next().unwrap_or("");
    let path = rest.trim_start_matches('/');
    let path = percent_decode(path).ok_or(400u16)?;
    if path.split('/').any(|seg| seg == ".." || seg == "." || seg.contains('\\') || seg.contains(':')) {
        return Err(400);
    }
    if let Some(common) = path.strip_prefix("common/") {
        return if common.is_empty() { Err(404) } else { Ok(format!("common/{common}")) };
    }
    // An extension icon (`__ext-icon/<id>/<px>`) keeps its path: its last segment is a number, which
    // the "no extension means index.html" rule below would otherwise swallow.
    if path.starts_with(sta_core::extensions::ICON_PATH_PREFIX) {
        return if sta_core::extensions::ICON_HOSTS.contains(&host) { Ok(format!("{host}/{path}")) } else { Err(404) };
    }
    let last = path.rsplit('/').next().unwrap_or("");
    if path.is_empty() || !last.contains('.') {
        return Ok(format!("{host}/index.html"));
    }
    Ok(format!("{host}/{path}"))
}

/// `<host>/__ext-icon/<id>/<px>` for a host that serves extension icons → the part after the host.
/// `resolve_asset` has already rejected traversal and `\`, and a request with no dot in its last
/// segment never reaches here (it resolves to `index.html`), which is why the size is required.
pub fn extension_icon_path(path: &str) -> Option<&str> {
    let (host, rest) = path.split_once('/')?;
    if !sta_core::extensions::ICON_HOSTS.contains(&host) || !rest.starts_with(sta_core::extensions::ICON_PATH_PREFIX) {
        return None;
    }
    Some(rest)
}

fn percent_decode(s: &str) -> Option<String> {
    if !s.contains('%') {
        return Some(s.to_string());
    }
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = s.get(i + 1..i + 3)?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// MIME type by file extension.
pub fn mime_for(path: &str) -> &'static str {
    let ext = path.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default();
    match ext.as_str() {
        "html" | "htm" => "text/html",
        "js" | "mjs" => "text/javascript",
        "css" => "text/css",
        "json" | "map" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "ico" => "image/x-icon",
        "woff2" => "font/woff2",
        "woff" => "font/woff",
        "ttf" => "font/ttf",
        "txt" => "text/plain",
        _ => "application/octet-stream",
    }
}

// ----------------------------------------------------------------------------------- handler

/// An in-memory response (ipc.md §2.4).
struct Resp {
    status: i32,
    mime: &'static str,
    headers: Vec<(&'static str, &'static str)>,
    body: Cow<'static, [u8]>,
    pos: usize,
}

fn security_headers() -> Vec<(&'static str, &'static str)> {
    vec![
        ("Content-Security-Policy", CSP),
        ("Cache-Control", "no-store"),
        ("X-Content-Type-Options", "nosniff"),
        ("Referrer-Policy", "no-referrer"),
    ]
}

fn respond(status: i32, mime: &'static str, body: Cow<'static, [u8]>) -> ResourceHandler {
    BytesHandler::new(Arc::new(Mutex::new(Resp { status, mime, headers: security_headers(), body, pos: 0 })))
}

fn error_response(status: u16) -> ResourceHandler {
    let msg: &'static [u8] = match status {
        400 => b"bad request",
        404 => b"not found",
        405 => b"method not allowed",
        _ => b"error",
    };
    respond(status as i32, "text/plain", Cow::Borrowed(msg))
}

wrap_resource_handler! {
    struct BytesHandler {
        resp: Arc<Mutex<Resp>>,
    }

    impl ResourceHandler {
        fn open(
            &self,
            _request: Option<&mut Request>,
            handle_request: Option<&mut i32>,
            _callback: Option<&mut Callback>,
        ) -> i32 {
            if let Some(h) = handle_request {
                *h = 1;
            }
            1
        }

        fn response_headers(
            &self,
            response: Option<&mut Response>,
            response_length: Option<&mut i64>,
            _redirect_url: Option<&mut CefString>,
        ) {
            let Ok(r) = self.resp.lock() else { return };
            let Some(response) = response else { return };
            response.set_status(r.status);
            response.set_mime_type(Some(&CefString::from(r.mime)));
            if r.mime.starts_with("text/") || r.mime.ends_with("json") || r.mime == "image/svg+xml" {
                response.set_charset(Some(&CefString::from("utf-8")));
            }
            for (k, v) in &r.headers {
                response.set_header_by_name(Some(&CefString::from(*k)), Some(&CefString::from(*v)), 1);
            }
            if let Some(len) = response_length {
                *len = r.body.len() as i64;
            }
        }

        fn skip(
            &self,
            bytes_to_skip: i64,
            bytes_skipped: Option<&mut i64>,
            _callback: Option<&mut ResourceSkipCallback>,
        ) -> i32 {
            let Some(out) = bytes_skipped else { return 0 };
            let Ok(mut r) = self.resp.lock() else {
                *out = -2;
                return 0;
            };
            let n = (r.body.len() - r.pos).min(bytes_to_skip.max(0) as usize);
            if n == 0 {
                *out = -2;
                return 0;
            }
            r.pos += n;
            *out = n as i64;
            1
        }

        fn read(
            &self,
            data_out: *mut u8,
            bytes_to_read: i32,
            bytes_read: Option<&mut i32>,
            _callback: Option<&mut ResourceReadCallback>,
        ) -> i32 {
            let Some(out) = bytes_read else { return 0 };
            *out = 0;
            let Ok(mut r) = self.resp.lock() else {
                *out = -2;
                return 0;
            };
            let remaining = r.body.len() - r.pos;
            if remaining == 0 || bytes_to_read <= 0 || data_out.is_null() {
                return 0; // bytes_read = 0 + false => complete
            }
            let n = remaining.min(bytes_to_read as usize);
            // SAFETY: CEF guarantees `data_out` points to at least `bytes_to_read` writable bytes.
            unsafe { std::ptr::copy_nonoverlapping(r.body.as_ptr().add(r.pos), data_out, n) };
            r.pos += n;
            *out = n as i32;
            1
        }

        fn cancel(&self) {
            if let Ok(mut r) = self.resp.lock() {
                r.pos = r.body.len();
            }
        }
    }
}

wrap_scheme_handler_factory! {
    struct StaSchemeFactory;

    impl SchemeHandlerFactory {
        fn create(
            &self,
            _browser: Option<&mut Browser>,
            _frame: Option<&mut Frame>,
            _scheme_name: Option<&CefString>,
            request: Option<&mut Request>,
        ) -> Option<ResourceHandler> {
            // IO thread. Never return None for our scheme: always answer (404 etc.). Assets are
            // public and immutable; web tabs are kept away from sta:// by on_before_browse and
            // DISPLAY_ISOLATED, and the IPC surface re-checks trust itself.
            let Some(request) = request else { return Some(error_response(400)) };
            let method = CefString::from(&request.method()).to_string();
            if method != "GET" && method != "HEAD" {
                return Some(error_response(405));
            }
            let url = CefString::from(&request.url()).to_string();
            let path = match resolve_asset(&url) {
                Ok(p) => p,
                Err(status) => return Some(error_response(status)),
            };
            // An extension icon, read out of that extension's own directory (UX7): a `sta://` page
            // may not load `chrome-extension://` images, so the picker and the settings page get
            // them same-origin. `extensions::icon_response` does the containment check.
            if let Some(icon) = extension_icon_path(&path) {
                return match crate::extensions::icon_response(icon) {
                    Some((mime, bytes)) => Some(respond(200, mime, Cow::Owned(bytes))),
                    None => Some(error_response(404)),
                };
            }
            match UiAssets::get(&path) {
                Some(file) => Some(respond(200, mime_for(&path), file.data)),
                None => match placeholder_asset(&path) {
                    Some((mime, body)) => Some(respond(200, mime, Cow::Borrowed(body))),
                    None => Some(error_response(404)),
                },
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_urls() {
        assert_eq!(resolve_asset("sta://sidebar/"), Ok("sidebar/index.html".into()));
        assert_eq!(resolve_asset("sta://sidebar"), Ok("sidebar/index.html".into()));
        assert_eq!(resolve_asset("sta://boosts/?id=5#x"), Ok("boosts/index.html".into()));
        assert_eq!(resolve_asset("sta://settings/general"), Ok("settings/index.html".into()));
        assert_eq!(resolve_asset("sta://sidebar/sidebar.js?v=1"), Ok("sidebar/sidebar.js".into()));
        assert_eq!(resolve_asset("sta://topbar/common/ipc.js"), Ok("common/ipc.js".into()));
        assert_eq!(resolve_asset("sta://nope/"), Err(404));
        assert_eq!(resolve_asset("https://sidebar/"), Err(400));
        assert_eq!(resolve_asset("sta://sidebar/a/../../x.js"), Err(400));
        assert_eq!(resolve_asset("sta://sidebar/a%5Cb.js"), Err(400));
        assert_eq!(host_of("sta://command/?x"), Some("command"));
    }

    #[test]
    fn extension_icons_keep_their_path_on_the_two_hosts_that_serve_them() {
        let id = "a".repeat(32);
        assert_eq!(resolve_asset(&format!("sta://command/__ext-icon/{id}/32")), Ok(format!("command/__ext-icon/{id}/32")));
        assert_eq!(resolve_asset(&format!("sta://settings/__ext-icon/{id}/48")), Ok(format!("settings/__ext-icon/{id}/48")));
        // Any other host is a 404, and traversal is refused before this rule ever runs.
        assert_eq!(resolve_asset(&format!("sta://sidebar/__ext-icon/{id}/32")), Err(404));
        assert_eq!(resolve_asset("sta://command/__ext-icon/../../x/32"), Err(400));
        assert_eq!(extension_icon_path(&format!("command/__ext-icon/{id}/32")), Some(&*format!("__ext-icon/{id}/32")));
        assert_eq!(extension_icon_path("command/index.html"), None);
        assert_eq!(extension_icon_path(&format!("sidebar/__ext-icon/{id}/32")), None);
    }

    #[test]
    fn placeholders_only_for_overlay_hosts() {
        assert_eq!(placeholder_asset("find/index.html").map(|a| a.0), Some("text/html"));
        assert_eq!(placeholder_asset("peek/__shell_placeholder.js").map(|a| a.0), Some("text/javascript"));
        assert!(placeholder_asset("sidebar/index.html").is_none());
        assert!(placeholder_asset("toast/other.js").is_none());
        assert!(placeholder_asset("common/ipc.js").is_none());
    }
}
