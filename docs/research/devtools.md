# DevTools docked inside the window — research, decisions and measurements

Trimmed from the phase-2 design work (`C:/ast/tmp/ext-design/devtools.md`) and the gates that
implemented it (`gates-p2.md`). CEF 152.0.6 / Chromium 152.0.7977.83, Windows 11. What ships is
described in `ARCHITECTURE.md` §4.1 "DevTools"; this file keeps the *why*, so the next person does
not re-derive it.

## 1. The five approaches, and why B won

| | approach | verdict |
|---|---|---|
| **B** | the DevTools **frontend** (`devtools://devtools/bundled/devtools_app.html?can_dock=true`) in an Alloy BrowserView inside the tab's card, the page on top of it at the rect the frontend reports, and a bridge onto a child session of the tab's in-process DevTools session | **shipped** |
| A | CEF's own Chrome-style DevTools view adopted into a frameless *owned* window glued over a slot | works, but it is a second HWND: it covers every overlay (command bar, find bar, toasts, Peek, corner masks), steals activation, needs key forwarding and DWM de-decoration, and has no device mode. Kept as the **undock** path, which is what CEF gives us for free |
| C | `show_dev_tools` with a parent HWND | CEF logs *"Parent window handle not supported for this DevTools window"* and opens a normal top-level window |
| D0 | add CEF's Chrome-style DevTools view to the Alloy window | the browser process dies at once (`ChromeBrowserWidget` cast in `chrome_browser_host_impl.cc`) |
| D1 | `SetParent` on CEF's DevTools HWND | same overlay z-order problem as A plus undefined focus behaviour |

CEF facts behind that table (source-read):

- DevTools popups are always Chrome style (`browser_view_impl.cc` `ComputeAlloyStyle`), and a
  Chrome-style Window hosts at most one Chrome-style BrowserView — sta has many.
- Any WebContents that loads `devtools://devtools/…` gets Chromium's `DevToolsUIBindings`, but with
  the **`DefaultBindingsDelegate`**, whose `SetInspectedPageBounds`, `SetIsDocked`, `CloseWindow`,
  `OpenInNewTab`, `BringToFront`, `SetEyeDropperActive` and `ShowCertificateViewer` are no-ops
  (`chrome/browser/devtools/devtools_ui_bindings.cc`), and without an attached agent host it drops
  the frontend's protocol messages. Everything else (preferences, histograms, `loadNetworkResource`,
  file systems) works.
- So a docked frontend needs exactly two things from the embedder: **the no-op methods**, replaced by
  a renderer shim, and **a protocol channel**, which the shim routes to a session sta controls.

## 2. What the implementation measured (and had to change)

1. **A `CefPanel` always has a layout manager.** `GetLayout()` is non-null on a fresh panel and CEF
   exposes no way to clear it; its fill layout resets every child to the panel's bounds on each
   layout pass. Setting the page's bounds directly therefore invalidated the layout again and spun a
   **~560 Hz loop** (825 `Page.frameResized` and 1 662 `CSS.mediaQueryResultChanged` events per
   second reached the frontend, starving the UI thread). The page now sits in its own host panel
   whose **BoxLayout `inside_border_insets`** place it: the layout manager computes the rect we
   want, so nothing fights it, and the counters stop (`debug.info` `devtools.boundsWrites`).
2. **An empty panel above the frontend does not swallow mouse input.** Each BrowserView owns an aura
   window and aura targets the topmost *window* under the pointer; the host panel has none. Measured
   both ways: a click over the DevTools UI reaches the frontend, a click in the page reaches the
   page. (This is *not* true of CEF overlay views, which are widgets — see `rounded.rs`.)
3. **`CefV8Value::GetValue` needs the context entered.** The browser→renderer direction looks up one
   global function in the frontend's frame; from a process-message callback no V8 context is
   entered, the lookup silently finds nothing, and every protocol reply is dropped — DevTools' own
   requests go out fine, so the symptom is empty panels, not an error. `enter`/`exit` around the
   lookup fixes it.
4. **Process messages are fast enough to replace the socket.** `Runtime.evaluate` round trip:
   **0.41 ms** average over 20 calls (the WebSocket prototype measured 0.49 ms); a 10 MB answer
   arrives whole in 153 ms, chunked at 4 MB. The hardened-WebSocket fallback of the security review
   was therefore never built — there is no port, no token and no listener.
5. **DevTools 152 ignores the `uiTheme` preference an embedder writes.** Writing `"default"` /
   `"dark"` does land in the profile (`getPreferences` answers with it) and the UI stays dark: the
   theme follows `prefers-color-scheme`. sta emulates that media feature in the frontend's own page
   instead, which DevTools re-reads **live**, and leaves `uiTheme` alone so a theme the user picks
   inside DevTools keeps working.
6. **`DOM.getNodeForLocation` reports the frame the *node* is in**, not the frame under the point.
   To follow "Inspect" into a cross-origin iframe, read the child frame from the node itself
   (`DOM.Node.frameId` of the `<iframe>` element), which is also the target id of that frame's
   nested session.
7. **Keys injected through the protocol reached sta's accelerators.** `Input.dispatchKeyEvent` — what
   an AI agent's `key` tool sends — let a page press F12, Ctrl+Shift+I, Ctrl+T and **Ctrl+W**: a key
   the renderer leaves unhandled comes back to Views, which matches the accelerator table. Such
   events carry no OS message, so `keyboard::on_key_event` now consumes them (MCP.md `press_key`).

8. **The shim must be installed only in sta's *own* frontend.** `on_before_dev_tools_popup` marks
   a Chromium-owned DevTools window's `extra_info` so it cannot inherit the tab's (or the UI's)
   renderer features — and that marker was the same one the shim looked for. So the **undocked**
   window (CEF's own `ShowDevTools`, which is Chrome's `DevToolsWindow`, already wired to an agent
   host) got sta's shim on top: `sendMessageToBackend` became a process message to a browser with no
   frontend registered, and that window's client has no handler for it. The window rendered its
   chrome, both protocol directions counted **zero** messages, the Elements tree stayed empty, and
   nothing was logged. The shim now needs its own key (`sta_devtools_shim`, set only by
   `devtools.rs`), which is why `renderer.rs` has two DevTools markers rather than one.
9. **DevTools' own zoom needs a `ZoomController`.** `DevToolsUIBindings::ZoomIn` calls
   `zoom::PageZoom::Zoom` on the DevTools WebContents; Chrome's `DevToolsWindow` and CEF's
   chrome-style views delegate both create a `ZoomController` for it, and an Alloy BrowserView gets
   none — so Ctrl+= / Ctrl+- / Ctrl+0 and the ⋮ menu were silent no-ops while docked (they work in
   the undocked window). The shim wraps `zoomIn`/`zoomOut`/`resetZoom` and sta steps Chrome's preset
   factors with `CefBrowserHost::SetZoomLevel` on the frontend browser.
10. **An inventory measured by *opening* panels has holes that *using* one finds.** The S14 sweep
   opened 27 panels; typing an incomplete expression into the Console (`Runtime.compileScript`,
   which is how `ConsolePrompt` decides whether to continue the line) and leaving device mode
   (`Emulation.resetPageScaleFactor`) were both refused, and the refusal is invisible inside the
   frontend — the Console printed `Uncaught SyntaxError` instead of waiting for the rest. Fixture
   entries are therefore added by *driving* panels, and `debug.info`'s `devtools.refused` is checked
   after each pass.
11. **A method's parameters are part of the policy.** `Network.getCookies` honours its `urls` array
   verbatim in Chromium, so the frontend could read any other host's cookies — `HttpOnly` included —
   while `Network.getAllCookies` next door was refused with "cookies of other sites are not
   available"; `Storage.clearDataForOrigin` was the destructive twin (it emptied another origin's
   jar). `devtools_policy` now judges those parameters against the origins of the inspected
   browser's own frames, read on demand for that handful of methods.
12. **The `sessionId` rewrite must only touch the top-level field.** Removing "the last `sessionId`
   in the text" corrupted every frontend message whose `params` carry one — `Target.detachFromTarget`
   became `"params":{,}`, which Chromium rejects with a parse error that carries no `sessionId` and
   is therefore dropped at the observer: the one nested-session method the plan grants the frontend
   could never work, invisibly. A message for a nested session already carries the id to send, so the
   bridge now only ever **appends** S's id to a message that has none.

## 3. Still open

- Mixed-DPI monitors and 125–175 % *display* scaling were not measured (one display on the machine
  used); page zoom is (Inspect at 150 %).
- The eyedropper and the certificate viewer stay no-ops; Workspaces ("Add folder") and "Save as"
  keep Chromium's own native pickers and were not exercised by an automated run.
- **`loadNetworkResource` answers 409 while docked.** `DevToolsUIBindings::LoadNetworkResource` asks
  `delegate_->GetInspectedWebContents()`, and `DefaultBindingsDelegate` returns `nullptr`, so the
  non-file branch fails without loading anything (measured: 409 docked, 200 in the undocked window).
  The user-visible damage is limited to fetches DevTools makes *for itself* — the Developer resources
  panel, doc and insight fetches, source maps of a target that is already gone; the page's own source
  maps go through `Network.loadNetworkResource` on its session and resolve. Wrapping it means serving
  the load from the browser process and streaming it back through `DevToolsAPI.streamWrite`; until
  then it is named in README's DevTools gaps.
- **`ServiceWorker.inspectWorker` is not usable from a page session** (Chromium answers
  "'ServiceWorker.inspectWorker' wasn't found"), so the Application panel's "inspect" link for a
  worker does nothing. It was dropped from the fixture so the allowlist stops implying support;
  routing it means attaching the worker target as a nested session and showing it in the dock.
- **`Page.navigate` may open `file:` URLs** (FINAL PLAN §3 lists the scheme), which is the one path
  by which a compromised frontend renderer could read local files — `Runtime.evaluate` on the same
  session then reads the page. The frontend's *own* `openInNewTab('file://…')` is refused, because it
  goes through `urls::web_content_may_open` like any link web content offers, so the two paths
  deliberately disagree: a developer inspecting a local page is the reason the first one stays. sta's
  omnibox opens `file:` URLs as well, so the marginal reach is a tab the user could open anyway.
- **No rate budget on the frontend → browser direction.** A single message over 64 MiB is refused in
  both directions, but the plan's "S detaches on overflow" is not implemented as a per-second budget
  (`gates-p2.md` deviation 8, with the measurement that made it acceptable).
- The frontend connects to a **page** session, not Chrome's `targetType=tab` target, so
  tab-target-only features (prerender and fenced-frame roots) behave like pre-M114 DevTools.
- DevTools extensions (`chrome.devtools.*` panels) in an Alloy frontend are untested; the shim
  already adds every `sta://` origin to the origins such an extension may not touch.
