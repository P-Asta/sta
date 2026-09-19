//! The DevTools frontend's embedder shim (renderer process) [owner: tabs]
//! (ext design FINAL PLAN §3 "Renderer shim"; devtools.md §6.4).
//!
//! A `devtools://` document loaded in an Alloy BrowserView gets Chromium's `DevToolsUIBindings`, but
//! with the **`DefaultBindingsDelegate`**, whose docking, page-bounds, close, open-in-tab and
//! bring-to-front methods are no-ops and which has no agent host attached, so protocol messages are
//! dropped (`devtools.md` §2). This shim replaces exactly those methods on the frontend's
//! `InspectorFrontendHost` object with calls into sta's browser process, and relays the protocol
//! both ways:
//!
//! | frontend calls | sta does |
//! |---|---|
//! | `setInspectedPageBounds` | places the page BrowserView on top of the frontend (devtools.rs) |
//! | `setIsDocked` | `false` → `DevToolsUndockRequested` (the frontend's own Undock) |
//! | `closeWindow` | closes the dock (`DevToolsClosed`) |
//! | `bringToFront` | focuses the frontend view |
//! | `openInNewTab` / `openSearchResultsInNewTab` | `DevToolsLinkRequested` (UX6, R-SEC-6) |
//! | `setWhitelistedShortcuts` | remembers the debugger keys the page must forward (F13/UX5) |
//! | `sendMessageToBackend` | the bridge's session S (`devtools_cdp.rs`) |
//! | `inspectElementCompleted` | nothing (the element picker ended) |
//! | `zoomIn` / `zoomOut` / `resetZoom` | `CefBrowserHost::SetZoomLevel` on the frontend browser |
//!
//! Chromium's own `ZoomIn`/`ZoomOut`/`ResetZoom` call `zoom::PageZoom::Zoom` on the DevTools
//! WebContents, which needs a `ZoomController` that only a Chrome-style browser creates: in an Alloy
//! BrowserView they are silent no-ops, so DevTools' own Ctrl+= / Ctrl+- / Ctrl+0 and its ⋮ menu did
//! nothing at all (FID-4).
//!
//! The **theme** is not set here: DevTools 152 decides it from `prefers-color-scheme` (its
//! `uiTheme` preference is "auto" unless the user picks a theme in DevTools' own settings, and
//! writing `default`/`dark` there does not move it — measured, `gates-p2.md` S12). `devtools.rs`
//! emulates that media feature on the frontend browser instead, which DevTools follows **live** and
//! which a user's explicit choice in DevTools still overrides.
//!
//! `getPreferences` / `getPreference` keep Chromium's implementation but their **answer** is
//! filtered: a saved `currentDockState` of `undocked` becomes `right` (S1), because sta's DevTools
//! live inside the window and a profile that once undocked them must not silently reopen an extra
//! top-level window forever. The user's own Undock during a session still works (it goes through
//! `setIsDocked`), and `bottom` / `left` / `right` are left alone.
//!
//! Everything else — preferences, histograms, `loadNetworkResource`, `registerExtensionsAPI`, the
//! file-system and survey methods — keeps going to Chromium's own bindings, which handle it.
//!
//! Two rules make this safe (SEC-1, SEC-3):
//! - the shim is installed **only** in the main frame of a browser sta created with
//!   `extra_info.sta_devtools_shim` — the frontend of a dock sta builds itself, never a DevTools
//!   window Chromium owns (`renderer.rs`) — whose URL starts with `devtools://devtools/bundled/`. The native
//!   send function is a closure argument, never a property of anything, so no other script in the
//!   process can reach it; the one global the reverse direction needs
//!   (`__staDevToolsDispatch`) is non-enumerable, non-writable and non-configurable, and only ever
//!   *delivers* messages to the frontend;
//! - `DevToolsAPI.setOriginsForbiddenForExtensions` is wrapped so sta's own `sta://` origins are
//!   always in the list a DevTools extension may not touch, whatever the embedder later sets.
//!
//! The frontend assigns `window.DevToolsAPI` and `window.InspectorFrontendHost` from
//! `devtools_compatibility.js`, which runs after `on_context_created`: both names are therefore
//! installed as property traps that wrap the object at assignment time and then become plain values.

use crate::renderer::{DevToolsInfo, MSG_DEVTOOLS_EMBEDDER};
use cef::*;
use std::sync::Arc;

/// Frontend documents that may carry the shim.
pub const FRONTEND_PREFIX: &str = "devtools://devtools/bundled/";

/// Kinds of message the browser sends to the shim (`__staDevToolsDispatch(kind, payload, i, n)`).
pub const IN_PROTOCOL: &str = "msg";
pub const IN_KEY: &str = "key";
/// One `DevToolsAPI` action by name, from sta's own keys (see [`ACTION_INSPECT`]).
pub const IN_ACTION: &str = "action";
/// Chrome's Ctrl+Shift+C: the frontend arms its element picker itself.
pub const ACTION_INSPECT: &str = "enterInspectElementMode";
/// The one global the shim installs: it only *delivers* messages into the frontend.
const DISPATCH: &str = "__staDevToolsDispatch";

pub fn is_frontend_url(url: &str) -> bool {
    url.starts_with(FRONTEND_PREFIX)
}

wrap_v8_handler! {
    struct EmbedderSend;

    impl V8Handler {
        fn execute(
            &self,
            _name: Option<&CefString>,
            _object: Option<&mut V8Value>,
            arguments: Option<&[Option<V8Value>]>,
            _retval: Option<&mut Option<V8Value>>,
            _exception: Option<&mut CefString>,
        ) -> i32 {
            let json = arguments
                .and_then(|a| a.first())
                .and_then(|v| v.as_ref())
                .filter(|v| v.is_string() != 0)
                .map(|v| CefString::from(&v.string_value()).to_string())
                .unwrap_or_default();
            if json.is_empty() {
                return 1;
            }
            // The browser re-checks the sender (registry id, main frame, URL) before acting.
            let Some(frame) = v8_context_get_current_context().and_then(|c| c.frame()) else { return 1 };
            if let Some(mut msg) = process_message_create(Some(&CefString::from(MSG_DEVTOOLS_EMBEDDER))) {
                if let Some(args) = msg.argument_list() {
                    args.set_string(0, Some(&CefString::from(json.as_str())));
                }
                frame.send_process_message(ProcessId::BROWSER, Some(&mut msg));
            }
            1
        }
    }
}

/// Installs the shim in `context` (a validated frontend main frame).
pub fn install(info: &Arc<DevToolsInfo>, context: &V8Context) {
    let mut handler = EmbedderSend::new();
    let Some(send) = v8_value_create_function(Some(&CefString::from("send")), Some(&mut handler)) else { return };
    let mut retval: Option<V8Value> = None;
    let mut exception: Option<V8Exception> = None;
    let ok = context.eval(Some(&CefString::from(SHIM)), Some(&CefString::from("")), 1, Some(&mut retval), Some(&mut exception));
    let Some(install) = retval.filter(|v| ok != 0 && v.is_function() != 0) else {
        let msg = exception.map(|e| CefString::from(&e.message()).to_string()).unwrap_or_default();
        log_warn!("renderer: DevTools shim failed: {msg}");
        return;
    };
    let origins = v8_value_create_string(Some(&CefString::from(info.origins.as_str())));
    let mut ctx = context.clone();
    let _ = install.execute_function_with_context(Some(&mut ctx), None, Some(&[Some(send), origins]));
}

/// Calls `__staDevToolsDispatch(kind, payload, index, count)` in the frame (no `eval`, so a
/// multi-megabyte protocol message costs one V8 string and no parse).
///
/// The context must be **entered** around the lookup: `CefV8Value::GetValue` reads the *current*
/// V8 context, and this runs from a process-message callback where none is entered, so without the
/// `enter`/`exit` pair the global's properties are simply not found (measured: every protocol reply
/// was dropped and DevTools' panels stayed empty).
pub fn dispatch_in(frame: &Frame, kind: &str, payload: &str, index: i32, count: i32) {
    let Some(context) = frame.v8_context() else { return };
    if context.enter() == 0 {
        log_warn!("renderer: could not enter the DevTools frontend context");
        return;
    }
    deliver(&context, kind, payload, index, count);
    context.exit();
}

fn deliver(context: &V8Context, kind: &str, payload: &str, index: i32, count: i32) {
    let Some(global) = context.global() else { return };
    let Some(f) = global.value_bykey(Some(&CefString::from(DISPATCH))) else { return };
    if f.is_function() == 0 {
        return;
    }
    let args = [
        v8_value_create_string(Some(&CefString::from(kind))),
        v8_value_create_string(Some(&CefString::from(payload))),
        v8_value_create_int(index),
        v8_value_create_int(count),
    ];
    let (mut ctx, mut this) = (context.clone(), global);
    let _ = f.execute_function_with_context(Some(&mut ctx), Some(&mut this), Some(&args));
}

/// `(send, staOrigins) => void`: wraps the frontend's embedder host.
const SHIM: &str = r#"(function (send, staOriginsJson) {
  'use strict';
  var staOrigins = [];
  try { staOrigins = JSON.parse(staOriginsJson) || []; } catch (e) {}

  // ---------------------------------------------------------------- browser -> frontend
  var buffer = [];
  function dispatch(kind, payload, index, count) {
    try {
      if (count > 1) {
        buffer.push(payload);
        if (index + 1 < count) return;
        payload = buffer.join('');
        buffer = [];
      }
      var api = window.DevToolsAPI;
      if (!api) return;
      if (kind === 'msg') api.dispatchMessage(payload);
      else if (kind === 'key') api.keyEventUnhandled(JSON.parse(payload));
      else if (kind === 'action' && typeof api[payload] === 'function') api[payload]();
    } catch (e) {
      buffer = [];
    }
  }
  Object.defineProperty(window, '__staDevToolsDispatch', {
    value: dispatch, writable: false, enumerable: false, configurable: false
  });

  // ---------------------------------------------------------------- frontend -> browser
  function post(method, args) {
    try { send(JSON.stringify({ m: method, a: args })); } catch (e) {}
  }

  // Methods DefaultBindingsDelegate leaves as no-ops, plus the protocol transport. Everything not
  // listed here keeps Chromium's own implementation (preferences, histograms, network loads, ...).
  var ACK = 1, PLAIN = 0;
  var WRAPPED = {
    setInspectedPageBounds: PLAIN,
    setIsDocked: ACK,
    closeWindow: PLAIN,
    bringToFront: PLAIN,
    openInNewTab: PLAIN,
    openSearchResultsInNewTab: PLAIN,
    setWhitelistedShortcuts: PLAIN,
    sendMessageToBackend: PLAIN,
    inspectElementCompleted: PLAIN,
    zoomIn: PLAIN,
    zoomOut: PLAIN,
    resetZoom: PLAIN
  };

  // A saved `undocked` dock state maps to `right` on the way *in* (S1): no race with the
  // frontend's own startup read, and nothing is written to the profile.
  function mapDockState(value) {
    try {
      return typeof value === 'string' && value.indexOf('undocked') >= 0 ? '"right"' : value;
    } catch (e) {
      return value;
    }
  }

  function wrapPreferences(host) {
    var all = host.getPreferences;
    if (typeof all === 'function') {
      host.getPreferences = function (callback) {
        all.call(host, function (prefs) {
          try {
            if (prefs && prefs.currentDockState !== undefined) prefs.currentDockState = mapDockState(prefs.currentDockState);
          } catch (e) {}
          callback(prefs);
        });
      };
    }
    var one = host.getPreference;
    if (typeof one === 'function') {
      host.getPreference = function (name, callback) {
        one.call(host, name, function (value) {
          callback(name === 'currentDockState' ? mapDockState(value) : value);
        });
      };
    }
  }

  function wrapHost(host) {
    if (!host || host.__sta) return host;
    wrapPreferences(host);
    Object.keys(WRAPPED).forEach(function (name) {
      var ack = WRAPPED[name] === ACK;
      host[name] = function () {
        var args = Array.prototype.slice.call(arguments);
        var callback = ack && typeof args[args.length - 1] === 'function' ? args.pop() : null;
        post(name, args);
        if (callback) Promise.resolve().then(function () { try { callback(); } catch (e) {} });
      };
    });
    try { Object.defineProperty(host, '__sta', { value: true, enumerable: false }); } catch (e) {}
    return host;
  }

  // sta's own pages must stay out of reach of DevTools extensions (SEC-3): whatever list the
  // embedder sets, ours is appended.
  function wrapApi(api) {
    if (!api || api.__sta) return api;
    var original = api.setOriginsForbiddenForExtensions;
    api.setOriginsForbiddenForExtensions = function (origins) {
      var list = (origins || []).slice();
      staOrigins.forEach(function (o) { if (list.indexOf(o) < 0) list.push(o); });
      return original.call(api, list);
    };
    try { Object.defineProperty(api, '__sta', { value: true, enumerable: false }); } catch (e) {}
    api.setOriginsForbiddenForExtensions(api.getOriginsForbiddenForExtensions ? api.getOriginsForbiddenForExtensions() : []);
    return api;
  }

  // devtools_compatibility.js assigns both names after this script runs: trap the assignment, wrap,
  // then become a plain property so the frontend's own capture sees the wrapped object.
  function trap(name, wrap) {
    var stored;
    try {
      Object.defineProperty(window, name, {
        configurable: true,
        get: function () { return stored; },
        set: function (value) {
          stored = wrap(value);
          try {
            Object.defineProperty(window, name, { value: stored, writable: true, enumerable: true, configurable: true });
          } catch (e) {}
        }
      });
    } catch (e) {}
  }
  trap('DevToolsAPI', wrapApi);
  trap('InspectorFrontendHost', wrapHost);
})"#;
