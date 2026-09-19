//! Themed network error page for web tabs [owner: tabs] (ARCHITECTURE §4.1 "Load errors").
//!
//! Declared as `client::error_page` (`#[path]` in client.rs), so main.rs needs no module line.
//!
//! On a main-frame load error (not `ERR_ABORTED`) Chromium commits its `chrome-error://chromewebdata/`
//! document into the *failed* navigation entry. Instead of navigating to a `data:` URL (which would
//! add a history entry after the failed one — Back would re-run the failed load and push the error
//! page again, trapping the user), the shell replaces that error document **in place** with a
//! script executed in it. The result behaves like Chrome's own error page:
//! - the address stays the failed URL; history has exactly one entry for it; Back/Forward work;
//! - Retry (button, Enter) is `location.reload()`, which re-requests the failed URL (verified);
//! - Chromium's auto-reload of error pages keeps working and a successful reload replaces it.
//!
//! The script only acts in a `chrome-error:` document, so an error that did not commit an error
//! page (the previous page is still shown) leaves that page untouched. Every page-controlled string
//! (URL, host, error text) is inserted with `textContent` from JSON string literals.
//!
//! Public API:
//! - `pub fn script(failed_url: &str, error_code: i32, error_text: &str) -> String`

use crate::controller;
use serde_json::json;

struct Message {
    title: &'static str,
    body: String,
}

fn copy_for(code: i32, name: &str, host: &str) -> Message {
    let site = if host.is_empty() { "The site".to_string() } else { host.to_string() };
    match code {
        -105 | -137 => Message { title: "This site can’t be reached", body: format!("{site}’s server IP address could not be found.") },
        -102 => Message { title: "This site can’t be reached", body: format!("{site} refused to connect.") },
        -106 => Message { title: "No internet", body: "You’re offline. Check your network connection.".into() },
        -7 | -118 => Message { title: "This site can’t be reached", body: format!("{site} took too long to respond.") },
        -100 | -101 => Message { title: "This site can’t be reached", body: "The connection was reset.".into() },
        -109 => Message { title: "This site can’t be reached", body: format!("{site} is unreachable.") },
        -21 => Message { title: "Your connection was interrupted", body: "A network change was detected.".into() },
        -324 => Message { title: "This page isn’t working", body: format!("{site} didn’t send any data.") },
        -312 => Message { title: "This address is blocked", body: "The port of this address is restricted for safety.".into() },
        -300 => Message { title: "Invalid address", body: "The address of this page is not valid.".into() },
        -301 | -302 => Message { title: "This page can’t be opened", body: "The address uses a scheme sta can’t open.".into() },
        -20 | -27 => Message { title: "This page has been blocked", body: format!("{site} can’t be displayed here.") },
        -299..=-200 => Message { title: "Your connection isn’t private", body: format!("The certificate of {site} is not trusted.") },
        _ if name.starts_with("ERR_CERT") || name.starts_with("ERR_SSL") => {
            Message { title: "Your connection isn’t private", body: format!("A secure connection to {site} could not be established.") }
        }
        _ => Message { title: "This page isn’t working", body: format!("{site} could not be loaded.") },
    }
}

/// Theme colors of the active space in the effective mode: (dark, surface, text, muted, accent, border, frame).
fn palette() -> (bool, String, String, String, String, String, String) {
    controller::with_store(|s| {
        let st = s.state();
        let theme = st.spaces.iter().find(|sp| sp.id == st.window.active_space).map(|sp| sp.theme.clone()).unwrap_or_default();
        let c = s.theme_colors(&theme);
        (s.is_dark(), c.surface, c.text, c.text_muted, c.accent, c.border, c.frame)
    })
    .unwrap_or_else(|| {
        (false, "#ffffff".into(), "#1f1f1f".into(), "rgba(0,0,0,.6)".into(), "#6b5bd6".into(), "rgba(0,0,0,.12)".into(), "#f3f1f7".into())
    })
}

/// JavaScript that replaces the committed Chromium error document with the themed page.
pub fn script(failed_url: &str, error_code: i32, error_text: &str) -> String {
    let host = sta_core::urls::host(failed_url).unwrap_or_default();
    let host = host.strip_prefix("www.").map(str::to_string).unwrap_or(host);
    let name = if error_text.trim().is_empty() { format!("ERROR {error_code}") } else { error_text.to_string() };
    let copy = copy_for(error_code, &name, &host);
    let (dark, surface, text, muted, accent, border, frame) = palette();
    let data = json!({
        "url": failed_url,
        "host": host,
        "title": copy.title,
        "body": copy.body,
        "code": name,
        "dark": dark,
        "css": css(&surface, &text, &muted, &accent, &border, &frame),
    });
    format!("({SCRIPT})({data});")
}

fn css(surface: &str, text: &str, muted: &str, accent: &str, border: &str, frame: &str) -> String {
    // Colors come from core's ThemeColors (hex / rgba strings), never from the page.
    format!(
        ":root{{--bg:color-mix(in srgb,{frame} 93%,{text});--card:{surface};--text:{text};--muted:{muted};--accent:{accent};--border:{border}}}\
*{{box-sizing:border-box}}\
html,body{{margin:0;height:100%;background:var(--bg);color:var(--text);\
font:14px/1.5 'Segoe UI Variable Text','Segoe UI',system-ui,sans-serif}}\
body{{display:flex;align-items:center;justify-content:center;padding:24px}}\
main{{max-width:520px;width:100%;background:var(--card);border:1px solid var(--border);border-radius:16px;padding:32px 32px 28px}}\
.mark{{width:40px;height:40px;border-radius:12px;background:color-mix(in srgb,var(--accent) 16%,transparent);\
color:var(--accent);display:flex;align-items:center;justify-content:center;margin-bottom:18px}}\
h1{{font-size:20px;font-weight:600;margin:0 0 8px}}\
p{{margin:0 0 6px;color:var(--muted)}}\
.url{{font:12px/1.4 'Cascadia Mono',Consolas,monospace;color:var(--muted);word-break:break-all;margin:14px 0 4px}}\
.code{{font:11px/1.4 'Cascadia Mono',Consolas,monospace;color:var(--muted);opacity:.8}}\
.row{{display:flex;gap:8px;margin-top:22px}}\
button{{font:inherit;font-weight:600;border:0;border-radius:8px;padding:8px 16px;cursor:pointer;\
background:var(--accent);color:#fff}}\
button:hover{{filter:brightness(1.08)}}\
button:focus-visible{{outline:2px solid var(--accent);outline-offset:2px}}"
    )
}

const SCRIPT: &str = r#"function (d) {
  if (location.protocol !== 'chrome-error:') return;
  var build = function () {
    document.open();
    document.write('<!doctype html><html><head><meta charset="utf-8"></head><body></body></html>');
    document.close();
    var html = document.documentElement;
    html.lang = 'en';
    html.style.colorScheme = d.dark ? 'dark' : 'light';
    var meta = document.createElement('meta');
    meta.name = 'color-scheme';
    meta.content = d.dark ? 'dark' : 'light';
    document.head.appendChild(meta);
    var title = document.createElement('title');
    title.textContent = d.host || d.url;
    document.head.appendChild(title);
    var style = document.createElement('style');
    style.textContent = d.css;
    document.head.appendChild(style);
    var el = function (tag, cls, text) {
      var e = document.createElement(tag);
      if (cls) e.className = cls;
      if (text != null) e.textContent = text;
      return e;
    };
    var main = el('main');
    main.setAttribute('data-sta-error', d.code);
    var mark = el('div', 'mark');
    mark.innerHTML = '<svg width="22" height="22" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" ' +
      'stroke-linecap="round" stroke-linejoin="round"><path d="M12 9v4"/><path d="M12 17h.01"/>' +
      '<path d="M10.3 3.9 1.8 18a2 2 0 0 0 1.7 3h17a2 2 0 0 0 1.7-3L13.7 3.9a2 2 0 0 0-3.4 0z"/></svg>';
    main.appendChild(mark);
    main.appendChild(el('h1', '', d.title));
    main.appendChild(el('p', 'body', d.body));
    main.appendChild(el('div', 'url', d.url));
    main.appendChild(el('div', 'code', d.code));
    var row = el('div', 'row');
    var retry = el('button', '', 'Retry');
    retry.id = 'sta-retry';
    retry.type = 'button';
    retry.addEventListener('click', function () { location.reload(); });
    row.appendChild(retry);
    main.appendChild(row);
    document.body.appendChild(main);
  };
  if (document.readyState === 'loading') document.addEventListener('DOMContentLoaded', build, { once: true });
  else build();
}"#;
