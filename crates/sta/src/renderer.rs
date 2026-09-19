//! Renderer-process side [owner: tabs] (ARCHITECTURE §4.1 "Boosts"/"Audio"/"Alt+click preview",
//! §5, docs/research/ipc.md §4.E).
//!
//! Runs in every renderer process (the same executable). Responsibility:
//! - the renderer half of the IPC router, **injected only** into trusted contexts: browsers
//!   created with `extra_info.sta_ui`, main frame, `sta://` URL. Everything else (web
//!   tabs, subframes, a UI browser that somehow shows a web page) never gets `__staQuery`;
//! - web tabs (`extra_info.sta_tab`), never UI pages:
//!   - **boosts**: the browser only ever sends the enabled boosts whose host matches the page
//!     (`[{host, css, js}]` as JSON + a version): `extra_info.boosts` holds those of the URL the
//!     browser was created for. On each main-frame `on_context_created` the boosts matching the
//!     frame URL (`sta_core::urls::host_matches`, re-checked here) are applied: CSS as
//!     `<style id="sta-boost">` (MutationObserver guard while `documentElement` is missing,
//!     moved last at `DOMContentLoaded`), JS after `DOMContentLoaded` in an IIFE with try/catch.
//!     `extra_info` is frozen at browser creation, so the renderer then sends
//!     [`MSG_BOOSTS_CHECK`]`{version}`; the browser answers with [`MSG_BOOSTS`]`{json, version}` for
//!     the frame's committed URL when that list differs (it also pushes the target host's list
//!     before same-site reloads/navigations), and the renderer re-applies to the current document
//!     (CSS replaced, JS of boosts not run yet in it);
//!   - **the Alt+click preview gesture** (every frame except error/DevTools pages and frames with
//!     an **opaque origin** — a sandboxed frame must not get a top-level page its embedder denied
//!     it; PROTOCOL §13): a capture-phase `click`/`auxclick` listener on `window`, installed before
//!     any page script runs, so nothing on the page can `stopImmediatePropagation` ahead of it. A
//!     **trusted** (`Event.isTrusted`, a `[LegacyUnforgeable]` own property no page can redefine —
//!     measured) left or middle **pointer** click (`UIEvent.detail >= 1`; Blink's keyboard
//!     activation of a link makes an equally trusted click with `detail === 0`) with Alt held on a
//!     node that resolves to a link is cancelled (`preventDefault` + `stopImmediatePropagation`,
//!     which is what stops Chromium's Alt+click *download*) and the URL is handed to the browser as
//!     [`MSG_PREVIEW`] through a native V8 function (a closure argument, never a global). The
//!     listener reads the event, the anchor and the document's own URL through accessors captured
//!     at install time, so a page that later redefines `altKey`, `composedPath`,
//!     `HTMLAnchorElement.prototype.href` or `Document.prototype.URL` cannot steer it. It resolves
//!     `<a>`/`<area>` (`href`), SVG `<a>` (`href`/`xlink:href` against `baseURI`) and links inside
//!     shadow trees (`composedPath`), and ignores a click that is not on a link, an anchor without
//!     an `href` attribute, and anything with another modifier held. A `javascript:` href is left
//!     **completely** alone (the page's own button: no `preventDefault`, so its handler runs, and
//!     Chromium saves no file for those — measured). A link with **nothing to preview** is cancelled
//!     but not hidden from the page (no `stopImmediatePropagation`, no report), because an Alt+click
//!     Chromium keeps *saves the page to disk*: a link into the same document (same URL apart from
//!     the fragment, `href="#"` included) and a `file:` URL from a non-`file:` document;
//!   - **media reporting** (every frame except error/DevTools pages): a capture-phase listener on
//!     `document` (+ per-element listeners and a `HTMLMediaElement.prototype.play` hook for
//!     detached `new Audio()`) for play/playing/pause/ended/emptied/volumechange computes
//!     `audible = any element playing && !muted && volume > 0` and, on change, calls a native V8
//!     function (a closure argument, never a global) that sends [`MSG_MEDIA`]`[audible]` to the
//!     browser. A released context that was audible reports `false`.
//!
//! Browser ids can be reused across cross-origin process swaps before the old browser is gone,
//! so trusted ids are reference-counted.
//!
//! - **docked DevTools frontends** (`extra_info.sta_devtools_shim`): the embedder shim
//!   (`devtools_shim.rs`) is installed in the main frame, and only while its URL starts with
//!   `devtools://devtools/bundled/`. It is the frontend's whole connection to sta: page bounds,
//!   dock state, links, forwarded debugger keys, the theme and the protocol itself
//!   ([`MSG_DEVTOOLS_EMBEDDER`] out, [`MSG_DEVTOOLS_IN`] in).
//!   A DevTools window **Chromium** owns (the undock path, `client.rs::on_before_dev_tools_popup`)
//!   carries `sta_devtools` but *not* `sta_devtools_shim`: it keeps Chromium's real
//!   `InspectorFrontendHost`, which is already wired to its own agent host. Installing the shim
//!   there swallowed its whole protocol — the shim posted `sendMessageToBackend` to a browser that
//!   has no frontend registered, and that window's client has no handler for the message, so the
//!   panels stayed empty with nothing logged.
//!
//! Public API:
//! - `pub fn create_handler() -> RenderProcessHandler` (App calls it once, in every process)
//! - `pub const MSG_MEDIA`, `MSG_BOOSTS`, `MSG_BOOSTS_CHECK`, `MSG_PREVIEW`,
//!   `MSG_DEVTOOLS_EMBEDDER`,
//!   `MSG_DEVTOOLS_IN`; extra-info keys `EXTRA_TAB`, `EXTRA_BOOSTS`, `EXTRA_BOOSTS_VERSION`,
//!   `EXTRA_DEVTOOLS`, `EXTRA_DEVTOOLS_SHIM`, `EXTRA_DEVTOOLS_TAB`, `EXTRA_DEVTOOLS_ORIGINS`
//! - `pub struct BoostData { host, css, js }` (serde; the JSON list in `extra_info`/messages)
//! - `pub struct DevToolsInfo { origins }` (what a frontend renderer needs at install time)

use crate::{devtools_shim, ipc, scheme};
use cef::wrapper::message_router::{MessageRouterRendererSide, MessageRouterRendererSideHandlerCallbacks, RendererSideRouter};
use cef::*;
use serde::{Deserialize, Serialize};
use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex};

/// Renderer → browser: `[audible: bool]` for the sending frame.
pub const MSG_MEDIA: &str = "sta.media";
/// Browser → renderer: `[boosts_json: string, version: string, apply_now: bool]`. `apply_now`
/// re-applies to the current document (answer to a check); otherwise only the list is updated
/// because a navigation/reload that creates the next document follows.
pub const MSG_BOOSTS: &str = "sta.boosts";
/// Renderer → browser: `[version: string]` the renderer applied for a new main-frame document.
pub const MSG_BOOSTS_CHECK: &str = "sta.boosts.check";
/// Renderer → browser: `[url: string]` — the user Alt+clicked (or Alt+middle-clicked) a link in
/// the sending frame and wants it previewed in Peek. The renderer has already cancelled the click,
/// so Chromium neither downloads nor navigates. The browser re-checks the sender (a web tab, not
/// agent-controlled, no more than one preview per `PREVIEW_INTERVAL_MS`, and `file:` only from a
/// `file:` *frame*) and core re-checks the URL (`urls::web_content_may_open`) before anything opens.
pub const MSG_PREVIEW: &str = "sta.preview";
/// Renderer → browser: `[json: string]` — one wrapped `InspectorFrontendHost` call of a DevTools
/// frontend (`{m: method, a: [args]}`). The browser re-checks the sender before acting.
pub const MSG_DEVTOOLS_EMBEDDER: &str = "sta.devtools.embedder";
/// Browser → renderer: `[kind: string, payload: string, index: int, count: int]` for a DevTools
/// frontend (a protocol message, a forwarded key or the theme), chunked for large payloads.
pub const MSG_DEVTOOLS_IN: &str = "sta.devtools.in";

pub const EXTRA_UI: &str = "sta_ui";
pub const EXTRA_TAB: &str = "sta_tab";
pub const EXTRA_BOOSTS: &str = "boosts";
pub const EXTRA_BOOSTS_VERSION: &str = "boosts_version";
/// Set on DevTools browsers opened from a tab so they never count as a web tab.
pub const EXTRA_DEVTOOLS: &str = "sta_devtools";
/// Set **only** on the frontend of a dock sta builds itself (`devtools.rs`), which is the one
/// DevTools document whose embedder is sta. Chromium's own DevTools windows must not get the shim.
pub const EXTRA_DEVTOOLS_SHIM: &str = "sta_devtools_shim";
/// The tab a docked DevTools frontend belongs to (diagnostics; the browser side is authoritative).
pub const EXTRA_DEVTOOLS_TAB: &str = "sta_devtools_tab";
/// JSON array of sta's own origins, which DevTools extensions may never touch (SEC-3).
pub const EXTRA_DEVTOOLS_ORIGINS: &str = "sta_devtools_origins";

/// One enabled boost as shipped to renderers.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct BoostData {
    pub host: String,
    #[serde(default)]
    pub css: String,
    #[serde(default)]
    pub js: String,
}

/// Stable version string of a serialized boost list.
pub fn boosts_version(json: &str) -> String {
    let mut h = DefaultHasher::new();
    json.hash(&mut h);
    format!("{:016x}", h.finish())
}

struct WebTab {
    refs: u32,
    boosts: Vec<BoostData>,
    version: String,
    /// JS boosts (hash of host+js) already run in the current main-frame document.
    applied_js: HashSet<u64>,
}

/// What a DevTools frontend renderer needs when its shim is installed.
pub struct DevToolsInfo {
    /// JSON array of sta's `sta://<host>` origins, which DevTools extensions may never touch.
    pub origins: String,
}

#[derive(Default)]
struct RendererState {
    /// Trusted UI browser ids → refcount.
    ui_ids: Mutex<HashMap<i32, u32>>,
    /// DevTools frontend browser ids → what their shim is installed with, and a refcount.
    devtools: Mutex<HashMap<i32, (u32, Arc<DevToolsInfo>)>>,
    /// Web tab browser ids.
    tabs: Mutex<HashMap<i32, WebTab>>,
    /// (browser id, frame id) → last audible value reported by that frame's document.
    audible_frames: Mutex<HashMap<(i32, String), bool>>,
}

/// Creates the render process handler (cheap; no CEF calls).
pub fn create_handler() -> RenderProcessHandler {
    StaRenderProcessHandler::new(RendererSideRouter::new(ipc::router_config()), Arc::new(RendererState::default()))
}

fn increment<K: std::hash::Hash + Eq>(map: &Mutex<HashMap<K, u32>>, key: K) {
    if let Ok(mut m) = map.lock() {
        *m.entry(key).or_insert(0) += 1;
    }
}

fn decrement<K: std::hash::Hash + Eq + Copy>(map: &Mutex<HashMap<K, u32>>, key: K) {
    if let Ok(mut m) = map.lock()
        && let Some(n) = m.get_mut(&key)
    {
        *n = n.saturating_sub(1);
        if *n == 0 {
            m.remove(&key);
        }
    }
}

fn key(name: &str) -> CefString {
    CefString::from(name)
}

fn parse_boosts(json: &str) -> Vec<BoostData> {
    serde_json::from_str(json).unwrap_or_default()
}

/// Web documents that get renderer features (not Chromium error pages or DevTools).
fn is_page_url(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    !(lower.starts_with("chrome-error:") || lower.starts_with("devtools:") || lower.starts_with("chrome:") || scheme::is_sta_url(url))
}

wrap_render_process_handler! {
    struct StaRenderProcessHandler {
        router: Arc<RendererSideRouter>,
        state: Arc<RendererState>,
    }

    impl RenderProcessHandler {
        fn on_browser_created(&self, browser: Option<&mut Browser>, extra_info: Option<&mut DictionaryValue>) {
            let (Some(browser), Some(info)) = (browser, extra_info) else { return };
            let id = browser.identifier();
            if info.bool(Some(&key(EXTRA_UI))) != 0 {
                increment(&self.state.ui_ids, id);
            } else if info.bool(Some(&key(EXTRA_DEVTOOLS))) != 0 {
                // Only sta's own dock frontend gets the shim; a Chromium-owned DevTools window
                // (undock) carries `sta_devtools` alone and keeps its real embedder.
                if info.bool(Some(&key(EXTRA_DEVTOOLS_SHIM))) != 0 {
                    let origins = CefString::from(&info.string(Some(&key(EXTRA_DEVTOOLS_ORIGINS)))).to_string();
                    if let Ok(mut m) = self.state.devtools.lock() {
                        let entry = m.entry(id).or_insert_with(|| (0, Arc::new(DevToolsInfo { origins })));
                        entry.0 += 1;
                    }
                }
            } else if info.has_key(Some(&key(EXTRA_TAB))) != 0 && info.has_key(Some(&key(EXTRA_DEVTOOLS))) == 0 {
                let json = CefString::from(&info.string(Some(&key(EXTRA_BOOSTS)))).to_string();
                let version = CefString::from(&info.string(Some(&key(EXTRA_BOOSTS_VERSION)))).to_string();
                if let Ok(mut m) = self.state.tabs.lock() {
                    let entry = m.entry(id).or_insert_with(|| WebTab {
                        refs: 0,
                        boosts: parse_boosts(&json),
                        version: version.clone(),
                        applied_js: HashSet::new(),
                    });
                    entry.refs += 1;
                }
            }
        }

        fn on_browser_destroyed(&self, browser: Option<&mut Browser>) {
            let Some(browser) = browser else { return };
            let id = browser.identifier();
            decrement(&self.state.ui_ids, id);
            if let Ok(mut m) = self.state.devtools.lock()
                && let Some(entry) = m.get_mut(&id)
            {
                entry.0 = entry.0.saturating_sub(1);
                if entry.0 == 0 {
                    m.remove(&id);
                }
            }
            if let Ok(mut m) = self.state.tabs.lock()
                && let Some(entry) = m.get_mut(&id)
            {
                entry.refs = entry.refs.saturating_sub(1);
                if entry.refs == 0 {
                    m.remove(&id);
                }
            }
            if let Ok(mut m) = self.state.audible_frames.lock() {
                m.retain(|(b, _), _| *b != id);
            }
        }

        fn on_context_created(&self, browser: Option<&mut Browser>, frame: Option<&mut Frame>, context: Option<&mut V8Context>) {
            let (Some(browser), Some(frame)) = (browser, frame) else { return };
            let id = browser.identifier();
            let url = CefString::from(&frame.url()).to_string();
            let is_ui = self.state.ui_ids.lock().is_ok_and(|m| m.contains_key(&id));
            if is_ui {
                if frame.is_main() != 0 && scheme::is_sta_url(&url) {
                    self.router.on_context_created(Some(browser.clone()), Some(frame.clone()), context.cloned());
                }
                return;
            }
            // A DevTools frontend: the shim, and only in its own main document.
            let devtools = self.state.devtools.lock().ok().and_then(|m| m.get(&id).map(|(_, info)| info.clone()));
            if let Some(info) = devtools {
                if frame.is_main() != 0
                    && devtools_shim::is_frontend_url(&url)
                    && let Some(context) = context
                {
                    devtools_shim::install(&info, context);
                }
                return;
            }
            let is_tab = self.state.tabs.lock().is_ok_and(|m| m.contains_key(&id));
            let Some(context) = context else { return };
            if !is_tab || !is_page_url(&url) {
                return;
            }
            if frame.is_main() != 0 {
                let version = {
                    let Ok(mut m) = self.state.tabs.lock() else { return };
                    let Some(tab) = m.get_mut(&id) else { return };
                    tab.applied_js.clear();
                    tab.version.clone()
                };
                apply_boosts(&self.state, id, &url, context, true);
                if let Some(mut msg) = process_message_create(Some(&key(MSG_BOOSTS_CHECK))) {
                    if let Some(args) = msg.argument_list() {
                        args.set_string(0, Some(&CefString::from(version.as_str())));
                    }
                    frame.send_process_message(ProcessId::BROWSER, Some(&mut msg));
                }
            }
            install_media_reporting(&self.state, context);
            install_preview_gesture(context);
        }

        fn on_context_released(&self, browser: Option<&mut Browser>, frame: Option<&mut Frame>, context: Option<&mut V8Context>) {
            if let (Some(b), Some(f)) = (browser.as_deref(), frame.as_deref()) {
                let frame_key = (b.identifier(), CefString::from(&f.identifier()).to_string());
                let was_audible = self.state.audible_frames.lock().ok().and_then(|mut m| m.remove(&frame_key)).unwrap_or(false);
                if was_audible {
                    send_media(f, false);
                }
            }
            // Forwarding a superset of contexts is harmless (unknown contexts are ignored).
            self.router.on_context_released(browser.cloned(), frame.cloned(), context.cloned());
        }

        fn on_process_message_received(
            &self,
            browser: Option<&mut Browser>,
            frame: Option<&mut Frame>,
            source_process: ProcessId,
            message: Option<&mut ProcessMessage>,
        ) -> i32 {
            if let (Some(b), Some(m)) = (browser.as_deref(), message.as_deref()) {
                let name = CefString::from(&m.name()).to_string();
                if name == MSG_BOOSTS {
                    on_boosts_message(&self.state, b, m);
                    return 1;
                }
                if name == MSG_DEVTOOLS_IN {
                    // Only a frontend renderer has the receiver; the dispatch is a no-op otherwise.
                    if let Some(frame) = frame.as_deref()
                        && let Some(args) = m.argument_list()
                    {
                        let kind = CefString::from(&args.string(0)).to_string();
                        let payload = CefString::from(&args.string(1)).to_string();
                        devtools_shim::dispatch_in(frame, &kind, &payload, args.int(2), args.int(3));
                    }
                    return 1;
                }
            }
            self.router.on_process_message_received(browser.cloned(), frame.cloned(), Some(source_process), message.cloned()) as i32
        }
    }
}

// ----------------------------------------------------------------------------------- boosts

fn js_hash(b: &BoostData) -> u64 {
    let mut h = DefaultHasher::new();
    b.host.hash(&mut h);
    b.js.hash(&mut h);
    h.finish()
}

/// Applies the matching boosts of browser `id` to `context` (the current main-frame document).
/// `fresh`: a new document (CSS may need the documentElement guard).
fn apply_boosts(state: &RendererState, id: i32, url: &str, context: &V8Context, fresh: bool) {
    let (css, scripts) = {
        let Ok(mut m) = state.tabs.lock() else { return };
        let Some(tab) = m.get_mut(&id) else { return };
        let matching: Vec<BoostData> =
            tab.boosts.iter().filter(|b| sta_core::urls::host_matches(&b.host, url)).cloned().collect();
        let css: Vec<&str> = matching.iter().map(|b| b.css.trim()).filter(|c| !c.is_empty()).collect();
        let css = css.join("\n");
        let mut scripts = Vec::new();
        for b in &matching {
            if b.js.trim().is_empty() {
                continue;
            }
            if tab.applied_js.insert(js_hash(b)) {
                scripts.push(b.js.clone());
            }
        }
        (css, scripts)
    };
    // A fresh document without boosts needs nothing; an update may have to remove old CSS.
    if !css.is_empty() || !fresh {
        eval(context, &boost_css_script(&css));
    }
    for js in scripts {
        eval(context, &boost_js_script(&js));
    }
}

fn on_boosts_message(state: &RendererState, browser: &Browser, message: &ProcessMessage) {
    let Some(args) = message.argument_list() else { return };
    let json = CefString::from(&args.string(0)).to_string();
    let version = CefString::from(&args.string(1)).to_string();
    let apply_now = args.bool(2) != 0;
    let id = browser.identifier();
    {
        let Ok(mut m) = state.tabs.lock() else { return };
        let Some(tab) = m.get_mut(&id) else { return };
        if tab.version == version {
            return;
        }
        tab.boosts = parse_boosts(&json);
        tab.version = version;
    }
    if !apply_now {
        return;
    }
    let Some(frame) = browser.main_frame() else { return };
    let url = CefString::from(&frame.url()).to_string();
    if !is_page_url(&url) {
        return;
    }
    if let Some(context) = frame.v8_context() {
        apply_boosts(state, id, &url, &context, false);
    }
}

fn js_string(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| "\"\"".into())
}

fn boost_css_script(css: &str) -> String {
    format!(
        r#"(function (css) {{
  try {{
    var ID = 'sta-boost';
    var apply = function (last) {{
      var d = document, el = d.getElementById(ID);
      if (!css) {{ if (el) el.remove(); return; }}
      if (!el) {{ el = d.createElement('style'); el.id = ID; }}
      if (el.textContent !== css) el.textContent = css;
      var root = d.documentElement;
      if (root && (last || !el.isConnected)) root.appendChild(el);
    }};
    if (document.documentElement) {{
      apply(false);
    }} else {{
      var mo = new MutationObserver(function () {{
        if (document.documentElement) {{ mo.disconnect(); apply(false); }}
      }});
      mo.observe(document, {{ childList: true }});
    }}
    if (document.readyState === 'loading') {{
      document.addEventListener('DOMContentLoaded', function () {{ apply(true); }}, {{ once: true }});
    }}
  }} catch (e) {{}}
}})({css});"#,
        css = js_string(css)
    )
}

fn boost_js_script(js: &str) -> String {
    format!(
        "(function () {{\n  var run = function () {{\n    try {{\n{js}\n    }} catch (e) {{ console.error('sta boost:', e); }}\n  }};\n  \
         if (document.readyState === 'loading') document.addEventListener('DOMContentLoaded', run, {{ once: true }});\n  else run();\n}})();"
    )
}

fn eval(context: &V8Context, code: &str) {
    let mut retval: Option<V8Value> = None;
    let mut exception: Option<V8Exception> = None;
    let ok = context.eval(Some(&CefString::from(code)), Some(&CefString::from("")), 1, Some(&mut retval), Some(&mut exception));
    if ok == 0 {
        let msg = exception.map(|e| CefString::from(&e.message()).to_string()).unwrap_or_default();
        log_warn!("renderer: script failed: {msg}");
    }
}

// ----------------------------------------------------------------------------------- media

const MEDIA_SCRIPT: &str = r#"(function (report) {
  'use strict';
  var EVENTS = ['play', 'playing', 'pause', 'ended', 'emptied', 'volumechange'];
  var tracked = new Set();
  var hooked = new WeakSet();
  var last = false;
  var pending = false;
  function audible(m) { return !m.paused && !m.ended && !m.muted && m.volume > 0; }
  function update() {
    pending = false;
    var any = false;
    tracked.forEach(function (m) {
      if (audible(m)) any = true;
      else if (!m.isConnected) tracked.delete(m);
    });
    if (any !== last) { last = any; try { report(any); } catch (e) {} }
  }
  function schedule() { if (!pending) { pending = true; Promise.resolve().then(update); } }
  function track(m) {
    if (!(m instanceof HTMLMediaElement)) return;
    tracked.add(m);
    if (!hooked.has(m)) {
      hooked.add(m);
      EVENTS.forEach(function (t) { m.addEventListener(t, schedule); });
    }
    schedule();
  }
  EVENTS.forEach(function (t) { document.addEventListener(t, function (e) { track(e.target); }, true); });
  var proto = HTMLMediaElement.prototype;
  var desc = Object.getOwnPropertyDescriptor(proto, 'play');
  if (desc && typeof desc.value === 'function') {
    var originalPlay = desc.value;
    Object.defineProperty(proto, 'play', {
      configurable: desc.configurable, enumerable: desc.enumerable, writable: desc.writable,
      value: function play() { track(this); return originalPlay.apply(this, arguments); }
    });
  }
  addEventListener('pagehide', function () { if (last) { last = false; try { report(false); } catch (e) {} } }, true);
})"#;

wrap_v8_handler! {
    struct MediaReportHandler {
        state: Arc<RendererState>,
    }

    impl V8Handler {
        fn execute(
            &self,
            _name: Option<&CefString>,
            _object: Option<&mut V8Value>,
            arguments: Option<&[Option<V8Value>]>,
            _retval: Option<&mut Option<V8Value>>,
            _exception: Option<&mut CefString>,
        ) -> i32 {
            let audible = arguments.and_then(|a| a.first()).and_then(|v| v.as_ref()).is_some_and(|v| v.is_bool() != 0 && v.bool_value() != 0);
            let Some(frame) = v8_context_get_current_context().and_then(|c| c.frame()) else { return 1 };
            if let Some(browser) = frame.browser() {
                let frame_key = (browser.identifier(), CefString::from(&frame.identifier()).to_string());
                if let Ok(mut m) = self.state.audible_frames.lock() {
                    if audible {
                        m.insert(frame_key, true);
                    } else {
                        m.remove(&frame_key);
                    }
                }
            }
            send_media(&frame, audible);
            1
        }
    }
}

fn send_media(frame: &Frame, audible: bool) {
    if let Some(mut msg) = process_message_create(Some(&key(MSG_MEDIA))) {
        if let Some(args) = msg.argument_list() {
            args.set_bool(0, audible as i32);
        }
        frame.send_process_message(ProcessId::BROWSER, Some(&mut msg));
    }
}

// ------------------------------------------------------------------------- Alt+click preview

/// A URL longer than this is dropped (with a log line) rather than turned into a process message —
/// nothing that passes `urls::web_content_may_open` is anywhere near it. The click stays cancelled:
/// letting Chromium have such a link back means it *saves the page to disk*, which is the one thing
/// the gesture exists to stop (measured — an Alt+click it is given back downloads).
const MAX_PREVIEW_URL: usize = 64 * 1024;

/// The Alt+click gesture (PROTOCOL §13). Everything it needs is captured before any page script
/// runs: the event accessors, `composedPath`, the anchor `href` getters (which double as brand
/// checks — calling one on a node that is not that kind of element throws), the document's own URL
/// getter and `URL`.
///
/// `report` is a native function passed in as an argument, so the page has no way to reach it.
const PREVIEW_SCRIPT: &str = r#"(function (report) {
  'use strict';
  // An opaque origin means the document is contained: a sandboxed frame (`sandbox` without
  // `allow-same-origin`), a `data:` frame, a CSP-sandboxed page. Such a frame must not be able to
  // turn one user Alt+click into a top-level page of its choosing — its embedder took even
  // `target=_blank` away from it — so the gesture does not exist there at all.
  try { if (self.origin === 'null') return; } catch (e) { return; }
  var own = Object.getOwnPropertyDescriptor;
  var proto = function (c, name) { try { var d = own(c.prototype, name); return d && d.get; } catch (e) { return null; } };
  var altKey = proto(MouseEvent, 'altKey');
  var ctrlKey = proto(MouseEvent, 'ctrlKey');
  var shiftKey = proto(MouseEvent, 'shiftKey');
  var metaKey = proto(MouseEvent, 'metaKey');
  var button = proto(MouseEvent, 'button');
  var detail = proto(UIEvent, 'detail');
  var composedPath = Event.prototype.composedPath;
  var preventDefault = Event.prototype.preventDefault;
  var stopImmediate = Event.prototype.stopImmediatePropagation;
  var documentUrl = proto(Document, 'URL');
  var indexOf = String.prototype.indexOf;
  var slice = String.prototype.slice;
  var lower = String.prototype.toLowerCase;
  var baseURI = proto(Node, 'baseURI');
  var anchorHref = proto(HTMLAnchorElement, 'href');
  var areaHref = proto(HTMLAreaElement, 'href');
  var svgHref = typeof SVGAElement === 'function' ? proto(SVGAElement, 'href') : null;
  var animatedBase = typeof SVGAnimatedString === 'function' ? proto(SVGAnimatedString, 'baseVal') : null;
  var getAttribute = Element.prototype.getAttribute;
  var getAttributeNS = Element.prototype.getAttributeNS;
  var XLINK = 'http://www.w3.org/1999/xlink';
  var Url = URL;
  if (!altKey || !button || !detail || !composedPath || !preventDefault || !stopImmediate) return;

  function attr(el, name) { try { return getAttribute.call(el, name); } catch (e) { return null; } }

  function starts(s, prefix) { return indexOf.call(lower.call(s), prefix) === 0; }

  /** `url` without its fragment: two URLs equal here are the same document. */
  function bare(url) {
    var i = indexOf.call(url, '#');
    return i < 0 ? url : slice.call(url, 0, i);
  }

  /**
   * A link there is nothing to preview in, so the gesture stops after cancelling the click: the page
   * still sees it and its own handler decides, but Chromium does not get to *save the page to disk*
   * (which is what an uncancelled Alt+click does here — measured):
   * - a link into the document the click happened in (same URL apart from the fragment) — an
   *   in-page anchor, and an href of just '#', the button idiom of web apps;
   * - a `file:` URL from a document that is not itself a `file:` page. The tab may be one (a saved
   *   page framing a remote ad), but this *frame* is not, and Blink lets no such document reach
   *   `file:` either.
   */
  function nothingToPreview(url, here) {
    if (here && bare(url) === bare(here)) return true;
    return starts(url, 'file:') && !(here && starts(here, 'file:'));
  }

  /** The absolute URL of `el` when it is a link, else null. */
  function linkUrl(el) {
    if (!el) return null;
    for (var i = 0; i < 2; i++) {
      var get = i === 0 ? anchorHref : areaHref;
      if (!get) continue;
      try {
        // Brand check: this throws unless `el` really is that element.
        var href = get.call(el);
        // An <a> with no href attribute is not a link (its `href` is "").
        return attr(el, 'href') === null ? null : href || null;
      } catch (e) {
        // not this kind of element
      }
    }
    if (svgHref) {
      try {
        var animated = svgHref.call(el);
        var raw = animatedBase ? animatedBase.call(animated) : null;
        if (raw === null || raw === undefined || raw === '') raw = attr(el, 'href');
        if (raw === null) {
          try { raw = getAttributeNS.call(el, XLINK, 'href'); } catch (e2) { raw = null; }
        }
        if (raw === null || raw === '') return null;
        var base = baseURI ? baseURI.call(el) : undefined;
        return new Url(raw, base).href;
      } catch (e) {
        // not an SVG <a>
      }
    }
    return null;
  }

  function onClick(e) {
    try {
      // `isTrusted` is an unforgeable own property: a script-made event can never claim it.
      if (!e.isTrusted) return;
      if (!altKey.call(e) || ctrlKey.call(e) || shiftKey.call(e) || metaKey.call(e)) return;
      var b = button.call(e);
      if (e.type === 'click' ? b !== 0 : b !== 1) return;
      // A real pointer click carries its click count; Blink's *keyboard* activation of a link
      // (Alt+Enter on a focused link, a screen reader's activation) dispatches an equally trusted
      // click with `detail === 0`. This is an Alt+**click** gesture: keys are not it, and an
      // automation tool that can press keys in a page must not be able to raise a preview.
      if (detail.call(e) < 1) return;
      var path = composedPath.call(e);
      var url = null;
      for (var i = 0; i < path.length && !url; i++) url = linkUrl(path[i]);
      if (!url) return;
      // A `javascript:` href is the page's own button (`javascript:void(0)` with a click handler, or
      // a legacy `javascript:doThing()` link): there is nothing to preview, Chromium does not save
      // those to disk either (measured), and cancelling the click would break the app. It is the one
      // shape the gesture leaves completely untouched.
      if (starts(url, 'javascript:')) return;
      // From here on the click is cancelled whatever happens next, because an Alt+click Chromium
      // keeps is an Alt+click that writes a file.
      preventDefault.call(e);
      var here = null;
      try { here = documentUrl ? documentUrl.call(document) : null; } catch (x2) { here = null; }
      if (nothingToPreview(url, here)) return;
      stopImmediate.call(e);
      report(url);
    } catch (x) {}
  }

  addEventListener('click', onClick, true);
  addEventListener('auxclick', onClick, true);
})"#;

wrap_v8_handler! {
    struct PreviewHandler;

    impl V8Handler {
        fn execute(
            &self,
            _name: Option<&CefString>,
            _object: Option<&mut V8Value>,
            arguments: Option<&[Option<V8Value>]>,
            _retval: Option<&mut Option<V8Value>>,
            _exception: Option<&mut CefString>,
        ) -> i32 {
            let url = arguments
                .and_then(|a| a.first())
                .and_then(|v| v.as_ref())
                .filter(|v| v.is_string() != 0)
                .map(|v| CefString::from(&v.string_value()).to_string())
                .unwrap_or_default();
            if url.is_empty() {
                return 1;
            }
            if url.len() > MAX_PREVIEW_URL {
                // The click is already cancelled (nothing is downloaded), so the link simply does
                // nothing. A link this long is not a link anyone means to follow.
                log_warn!("renderer: preview URL of {} bytes dropped (over {MAX_PREVIEW_URL})", url.len());
                return 1;
            }
            // The browser re-checks that the sender is a web tab, and core the URL itself.
            let Some(frame) = v8_context_get_current_context().and_then(|c| c.frame()) else { return 1 };
            if let Some(mut msg) = process_message_create(Some(&key(MSG_PREVIEW))) {
                if let Some(args) = msg.argument_list() {
                    args.set_string(0, Some(&CefString::from(url.as_str())));
                }
                frame.send_process_message(ProcessId::BROWSER, Some(&mut msg));
            }
            1
        }
    }
}

fn install_preview_gesture(context: &V8Context) {
    let mut handler = PreviewHandler::new();
    let Some(report) = v8_value_create_function(Some(&key("report")), Some(&mut handler)) else { return };
    let mut retval: Option<V8Value> = None;
    let mut exception: Option<V8Exception> = None;
    let ok = context.eval(Some(&CefString::from(PREVIEW_SCRIPT)), Some(&CefString::from("")), 1, Some(&mut retval), Some(&mut exception));
    let Some(install) = retval.filter(|v| ok != 0 && v.is_function() != 0) else {
        let msg = exception.map(|e| CefString::from(&e.message()).to_string()).unwrap_or_default();
        log_warn!("renderer: preview script failed: {msg}");
        return;
    };
    let mut ctx = context.clone();
    let _ = install.execute_function_with_context(Some(&mut ctx), None, Some(&[Some(report)]));
}

fn install_media_reporting(state: &Arc<RendererState>, context: &V8Context) {
    let mut handler = MediaReportHandler::new(state.clone());
    let Some(report) = v8_value_create_function(Some(&key("report")), Some(&mut handler)) else { return };
    let mut retval: Option<V8Value> = None;
    let mut exception: Option<V8Exception> = None;
    let ok = context.eval(Some(&CefString::from(MEDIA_SCRIPT)), Some(&CefString::from("")), 1, Some(&mut retval), Some(&mut exception));
    let Some(install) = retval.filter(|v| ok != 0 && v.is_function() != 0) else {
        let msg = exception.map(|e| CefString::from(&e.message()).to_string()).unwrap_or_default();
        log_warn!("renderer: media script failed: {msg}");
        return;
    };
    let mut ctx = context.clone();
    let _ = install.execute_function_with_context(Some(&mut ctx), None, Some(&[Some(report)]));
}
