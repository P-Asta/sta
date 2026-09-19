# In-process DevTools automation in CEF 152 (Phase 0 spike)

Verified 2026-09-17 against the debug build (CEF 152.0.6, Chromium 152, Alloy style, Views),
Windows 11, 100 % display scale. Measured with the debug-only `debug.cdp` / `debug.cdpEvents`
requests (`crates/sta/src/automation/spike.rs`), which send any DevTools method through the
same in-process client the MCP tools use (`automation/cdp.rs`). Spike scripts:
`C:/ast/tmp/mcp/spike/spike*.mjs` (not part of the repo). This note records **what the tools rely on**;
the design is in `docs/MCP.md` and `crates/sta/src/automation/`.

## 1. API surface (cef 152.3.0)

- `BrowserHost::send_dev_tools_message(&self, message: Option<&[u8]>) -> c_int` — raw JSON
  `{"id", "method", "params", "sessionId"?}`; returns 0 off the UI thread.
- `BrowserHost::execute_dev_tools_method(&self, message_id: c_int, method: Option<&CefString>,
  params: Option<&mut DictionaryValue>) -> c_int` — not used (the structured callbacks drop
  `sessionId` and need `DictionaryValue` params).
- `BrowserHost::add_dev_tools_message_observer(&self, observer: Option<&mut DevToolsMessageObserver>)
  -> Option<Registration>` — the observer stays registered while the `Registration` lives.
- `wrap_dev_tools_message_observer!` → `on_dev_tools_message(browser, message: Option<&[u8]>) -> c_int`
  (return 1 = handled, the structured callbacks are skipped), `on_dev_tools_method_result`,
  `on_dev_tools_event`, `on_dev_tools_agent_attached/detached(browser)`.
- Buffers are valid only during the callback (copy them). No remote-debugging port or DevTools
  front-end is needed; the session coexists with F12 DevTools and with a remote-debugging client.

## 2. Findings

| # | Question | Result |
|---|---|---|
| 1 | Synchronous callbacks | `on_dev_tools_agent_attached` runs **inside** the first `send_dev_tools_message` of a browser (1 synchronous callback counted). Replies arrived asynchronously in every case measured, but the client must assume they can be synchronous: the observer only copies and posts; no borrow is held across a send. First call ≈ 1 ms. |
| 2 | Cost | `Accessibility.getFullAXTree` of a small form page: 36–42 nodes, 1–8 ms. `Page.createIsolatedWorld` ≈ 0–1 ms. `Page.captureScreenshot` (JPEG 60, 1020×768) 23–33 ms, 15–17 KB. |
| 3 | `Accessibility.getFullAXTree` without `Accessibility.enable` | Works. Roles are ARIA names or internal names (`RootWebArea`, `StaticText`, `InlineTextBox`, `Iframe`, `LabelText`, …). Password field values come back masked (`••••••`). Nodes carry `backendDOMNodeId`. |
| 4 | Isolated world | `Page.createIsolatedWorld{frameId, worldName, grantUniveralAccess:false}` → context id; the page's globals (`window.pageSecret`) are invisible there, the DOM is shared. Creating the same world name twice in one document returns the **same** context id. `DOM.resolveNode{backendNodeId, executionContextId}` works **without** `DOM.getDocument` first. |
| 5 | Stale nodes | After a navigation, `DOM.resolveNode` of an old backend id in the old context fails ("Node with given id does not belong to the document"), but **backend ids are reused**: the same id resolved in the new document's world succeeds and `DOM.getContentQuads` returns quads — for a *different* element. Refs must carry the document (loader id) and be checked before use (`stale_ref`). |
| 6 | Visible tab | Quads → center → `DOM.getNodeForLocation` hits the element; `Input.dispatchMouseEvent` (moved/pressed/released) produces `isTrusted` mousedown/focus/click; `DOM.focus` + `Input.insertText` types Korean text with trusted `input` events; `Input.dispatchKeyEvent` Enter submits a form; `rawKeyDown` Ctrl+A + `insertText` replaces the content. Same in **Peek** and in both panes of a **split**. |
| 7 | Background tab, never shown (created hidden) | `visibilityState` reports **"visible"** but `innerWidth/innerHeight` are **0**; `document.hasFocus()` false; rAF and timers run. `Page.captureScreenshot` **stalls** (5 s timeout). Quads are meaningless and hit tests fail; mouse events hit `<html>`. `DOM.focus` + `Input.insertText` **work** (value set, trusted `input` events). |
| 8 | Background tab after it was shown | `visibilityState` "hidden", size kept (1020×768), rAF stopped (0 in 2 s), timers throttled to 1/s. Screenshot **stalls**. Quads and `DOM.getNodeForLocation` still match the element, but `Input.dispatchMouseEvent` **times out** (no acknowledgement from a hidden widget). `DOM.focus`, `Input.insertText` and `Input.dispatchKeyEvent` (Enter submits the form) **work**. |
| 9 | Minimized window | `visibilityState` "hidden"; screenshot stalls (4 s timeout); restore → visible again. |
| 10 | Focus emulation | `Emulation.setFocusEmulationEnabled` makes `document.hasFocus()` true in a background tab but doesn't make hit testing work there. Not needed for typing (finding 7/8); not used by the MVP. |
| 11 | Page zoom 175 % | `innerWidth` shrinks by the zoom (583 CSS px), `devicePixelRatio` 1.75. `DOM.getContentQuads` and `Input.dispatchMouseEvent` use the **same** CSS-pixel viewport coordinates: the hit test matched and the click submitted the form. (Display scaling other than 100 % was not available to test.) |
| 12 | JavaScript dialogs | With `Page.enable`, `Page.javascriptDialogOpening` arrives (`hasBrowserHandler: true`) in visible **and** hidden tabs; no native `#32770` window appeared. While the dialog is open the renderer is blocked: `DOM.getDocument` / `Runtime.callFunctionOn` and even the `mouseReleased` that opened it time out. `Page.handleJavaScriptDialog` closes it. The shell uses CEF's `JsdialogHandler::on_jsdialog` for agent-controlled tabs instead (no `Page.enable` needed) and fails page calls fast with `dialog_open`. |
| 13 | DevTools key events vs the shell | `Input.dispatchKeyEvent` never reaches CEF's `KeyboardHandler::on_pre_key_event` (0 calls for letters, Escape, Enter) — so any `on_pre_key_event` in a tab is the user's own typing (agent takeover), and agents can't trigger accelerators (Ctrl+W, …). A DevTools Escape does **not** exit page fullscreen. |
| 14 | Gestures (critique S6 confirmed) | A DevTools click counts as a user gesture: a `mailto:` link went to `ShellExecuteW` (**the first spike run launched the default mail handler once**; later runs set `STA_TEST_EXTERNAL_PROTOCOL=1`), `<input type=file>` opened the native "Open" dialog (owned by our window), a download link downloaded, `requestFullscreen()` entered page fullscreen, `navigator.clipboard.writeText` succeeded in the visible tab. |
| 15 | Out-of-process iframes | `Target.setAutoAttach{autoAttach, flatten:true}` delivers `Target.attachedToTarget` with a `sessionId` for a cross-site iframe (`localhost` in a `127.0.0.1` page). The main AX tree shows the `Iframe` node without children. |
| 16 | Agent detach | A cross-site navigation (new renderer process) did **not** detach the session (same generation, frame tree readable right after). |

## 3. Consequences for the tools

- **Keyboard and DOM tools work on background tabs**: `page_snapshot`, `page_text`, `type`,
  `press_key`, `wait_for` don't need the tab on screen.
- **Screenshots and mouse input need the tab on screen** (`tab_not_visible` otherwise; the shell
  checks `tabs::is_tab_visible` + not minimized, then the page's `visibilityState` and viewport
  size, because a never-shown tab claims "visible"). `tab_show` brings a tab on screen. No
  synthetic `element.click()` fallback.
- **Refs carry the document**: every ref is checked against the main frame's current loader id
  before use; a new document starts a new ref generation.
- **Dialogs**: held via `JsdialogHandler` for agent-controlled tabs; page calls fail with
  `dialog_open` until `handle_dialog`.
- **Guards** for agent-controlled tabs (external protocols, file choosers, downloads, fullscreen,
  permission prompts, Peek) are required, not optional (finding 14).
- `--disable-backgrounding-occluded-windows` is added while agent access is on, so a covered (not
  minimized) window keeps rendering.
- v1 tools (verified by `agent-e2e.mjs`): `select_option`, `fill_form` (focus + Space toggles
  checkboxes and radios with trusted key events) and `evaluate` work in never-shown background
  tabs; `scroll` doesn't (a 0×0 viewport, finding 7) but does in a hidden tab that was shown
  before; `DOM.resolveNode` without `executionContextId` resolves in the page's main world, so
  `evaluate {world: "main"}` needs no extra method; `Page.captureScreenshot` with
  `captureBeyondViewport` captures the whole page of a visible tab; a cross-site iframe appears in
  `Accessibility.getFullAXTree` of the main frame as an `Iframe` node without children.

## 4. Not verified here

- Display scaling other than 100 % (only page zoom was tested), multiple monitors.
- Clipboard writes in background tabs under focus emulation (the click itself doesn't hit there).
- MSIX-packaged clients (Claude Desktop) launching the bridge: the bridge refuses to launch the
  browser when it has package identity; connecting to an already running browser is expected to
  work but was not tested.
- A real MCP client session (Claude Code) — the e2e suite speaks MCP JSON-RPC itself.
