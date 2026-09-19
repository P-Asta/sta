# sta UI ⇄ shell protocol

The HTML UI talks to the Rust browser process through `cef::wrapper::message_router`
(`window.__staQuery`), which is injected **only** into trusted `sta://` pages. JSON field
names are camelCase and match the Rust serde types in
`crates/sta-core/src/{command,effect,view,omnibox,model}.rs`. **Those Rust files are the
source of truth**; `crates/sta-core/tests/contract_serde.rs` locks the wire format. This
document summarizes them and adds the IPC wrapper.

---

## 1. JS client — `ui/common/ipc.js`

```js
import { invoke, dispatch, on, getState, isMock } from '/common/ipc.js';

await invoke('state.get');                       // -> UiState
dispatch({ type: 'activateItem', id: 12 });      // = invoke('dispatch', command); resolves null
on('state', (s) => render(s));                   // push; returns an unsubscribe fn
getState();                                      // last received UiState (or null)
```

- **Transport**: `__staQuery({request: JSON.stringify({cmd, payload}), persistent, onSuccess, onFailure})`.
  - **Always pass both callbacks.** The cef-rs port silently drops queries that omit one.
  - A response is a JSON string (`""` means `null`). A failure is `(code, message)` → rejected
    `Error` with `.code`.
- **Events**: `ipc.js` opens one persistent query `{cmd: "__subscribe"}` as soon as it loads. Each
  success delivers `{"event": name, "payload": ...}`.
- **Startup**:
  1. subscribe;
  2. `invoke('state.get')` and render;
  3. `invoke('ui.ready')` (the shell only shows an overlay after its page is ready).

  Snapshots carry `revision`: ignore any snapshot older than the one already rendered, so a push
  that raced `state.get` is harmless.
- **`dispatch` resolves `null`** once the command is queued. It rejects with code 400 when the
  command doesn't parse (unknown `type`, missing or mistyped required field) and 403 for commands
  not allowed from the UI (`Command::allowed_from_ui`: shell events, also when wrapped in
  `commitOmnibox`, and nested `commitOmnibox`). Semantic no-ops still resolve `null`, so read
  results from the next `state` push.
- **Mock mode**: used only when `location.protocol !== 'sta:'` **or** the URL has `?mock`.
  Inside sta, a missing `__staQuery` is an error, never a silent fallback to fake data.
  - `ipc.js` then imports `/common/mock.js`, which implements the same API over fixture JSON
    (`/common/fixtures/uiState.json`, `omnibox.json`, `omniboxActions.json`, `archive.json`,
    `history.json`, …; `omnibox.actions` answers the fixture actions with the per-space rows
    rebuilt from the live state), validates dispatched commands like the shell (400/403 above),
    and applies them with small local reducers, so pages can be developed and screenshot-tested
    without the shell (details in `ui/README.md`).
  - A valid command the mock has no reducer for is accepted (resolves `null`, like the shell) and
    only logs a console warning; it just doesn't change the mock state.
  - `omnibox.suggest` answers deterministic fake suggestions after ~60 ms (e.g. `rust` →
    `rust programming language`, `rust tutorial`, `rust vs go`), and the mock `omnibox.query`
    mirrors core's Suggestions rows and inline completion.
  - `node tools/check-mock-commands.mjs` keeps the mock's validation (`COMMAND_FIELDS`,
    `SHELL_ONLY_COMMANDS` in `mock-reducers.js`) in sync with `command.rs`: every `Command`
    variant, its required fields and their kinds, the shell-event list; exit 1 on a mismatch.
    Commands without a reducer are reported as warnings. Run it after changing `command.rs`.
  - Fixtures (all but `appInfo.json`) are generated from the Rust types with
    `cargo test -p sta-core -- --ignored gen_fixtures`; don't edit them by hand.
- `window.sta = { invoke, dispatch, on, getState }` is exposed for automation (CDP eval).

## 2. Requests (`invoke(cmd, payload)`)

| cmd | payload | result | notes |
|---|---|---|---|
| `dispatch` | `Command` | `null` | Queues a command (see `command.rs`). |
| `state.get` | – | `UiState` | |
| `ui.ready` | – | `null` | The calling surface finished its first render. The surface is identified by the calling browser. |
| `omnibox.query` | `OmniboxRequest {text, mode, splitSide?, preventInlineAutocomplete, suggestions?, seq}` | `OmniboxResponse` | Pure query. Commit with `dispatch({type:'commitOmnibox', command: r.command, alt:false})`, or `alt:true` with `r.altCommand ?? r.command` on Alt+Enter. `suggestions` (from `omnibox.suggest`) become Suggestions rows and may complete inline (§6). |
| `omnibox.suggest` | `{text}` | `{text, suggestions: string[]}` | Remote search suggestions for the typed text from the selected engine (§6 "Search suggestions"), answered asynchronously; `text` echoes the request. **403** unless sent by the command bar. Resolves `suggestions: []` (never rejects for network problems) when `settings.searchSuggestions` is off, the engine has no suggestion endpoint (Kagi, Perplexity, Custom), the text is blank, longer than 256 characters, URL-like with an explicit scheme (`https:`, `mailto:`, `sta:`, any `scheme://`…) or a Windows path, and when the request fails, gets a non-200 status or a body over 64 KB, takes longer than 1500 ms, or is superseded: each caller has one request at a time, and a new one resolves the one in flight with `[]` at once. A leading `?` is stripped before fetching. At most 8 suggestions, trimmed, without duplicates (case-insensitive) or the query itself. |
| `omnibox.actions` | – | `OmniboxResult[]` | Every command bar action available in the current state (`Store::omnibox_actions`): group `actions`, sorted A–Z by title, including the per-space "Go to Space: X" / "Move Tab to Space: X" rows. Pure; refetch when `state.revision` changes. Commit like `omnibox.query` results. |
| `archive.list` | – | `ArchiveEntryView[]` | Refetch when `state.archiveRevision` changes. |
| `history.list` | `{query, limit}` | `HistoryEntryView[]` | Refetch when `state.historyRevision` changes. |
| `boosts.get` | `{id}` | `Boost \| null` | Full CSS/JS; `state.boosts` holds summaries only. |
| `theme.colors` | `{theme}` | `ThemeColors` | Live preview in the space sheet. |
| `surface.setSize` | `{width?, height}` | `null` | Command bar, find bar, permission, agent, switcher and toast size to their content (DIP). The shell clamps and repositions; the surface is the calling browser (ignored from non-overlay pages). The size is the page's; the shell's rounded card around it adds its inner chrome (§4). |
| `surface.exited` | `{gen}` | `null` | The other half of an acknowledged exit (§14): this surface has rendered the blank frame the shell asked for with `surface.exit {gen}` (the sidebar: `sidebar.hover {gen}`), so the widget may be hidden now. `gen` echoes the request; one that is no longer pending (cancelled by a show, or already finished at the cap) is ignored. The shell hides at its cap anyway, so a missing or failed ack only costs the blank frame. |
| `sidebar.setWidth` | `{width}` | `null` | Live drag-resize (clamped 200–440). Send `dispatch setSidebarWidth` on pointerup to persist. **403** unless sent by the sidebar. |
| `sidebar.hoverLock` | `{locked}` | `null` | An HTML menu, popover or drag is open (`true`) or no longer (`false`) in the sidebar page: while locked, the floating sidebar stays open wherever the pointer is; unlocking with the pointer outside it hides it at once (§5 "Floating sidebar"). Send changes only. The shell drops the lock when the sidebar page reloads, navigates or its renderer goes away. **403** unless sent by the sidebar. |
| `dialog.pickFolder` | – | `string \| null` | Native folder picker (settings → download folder), modal to the main window, starting in the current download folder. Resolves when the dialog closes (`null` = cancelled). **409** while another picker is open. |
| `app.info` | – | `{version, cefVersion, chromiumVersion, dataDir, downloadDir}` | Settings "About"; `downloadDir` is the resolved default. |
| `agent.info` | – | `{bridgePath, bridgeFound, dataDir, dataDirIsDefault, endpointOpen, logPath, build}` | Settings → AI agents: the setup snippets use `bridgePath` (`sta-mcp.exe` next to the browser) and add `--data-dir` when `dataDirIsDefault` is false (a legacy default folder that sta runs in place after a failed move counts as default: the MCP server finds it without `--data-dir`). **403** unless sent by the Settings page or the agent overlay. |
| `agent.testConnection` | – | `{ok, ms, steps: [{id: "access"\|"endpoint"\|"bridge"\|"channel", ok, detail}]}` | Settings → Test connection: access on, endpoint open, bridge found, then `sta-mcp.exe --check --data-dir <dataDir>` (opens the pipe with the owner, session, integrity and server-process checks and disconnects without a hello: no prompt; 10 s at most). **403** unless sent by the Settings page; **409** while a test runs. |

Errors: 400 malformed request/payload or invalid command, 403 untrusted caller or command not
allowed from the UI, 404 unknown request, 409 conflict, 503 store not ready.

Debug builds also answer `debug.*` requests (test automation, see
`crates/sta/src/debug.rs`): `debug.info` (includes `suggest: {inFlight, started, succeeded,
failed, superseded, timedOut, skipped}`), `debug.pushState`, `debug.execute` (`Effect` or
`Effect[]`, JSON as in `effect.rs`, e.g.
`{"type":"answerPermission","id":4,"allow":false,"remember":true}` or
`{"type":"openExternal","url":"mailto:a@b.c"}`), `debug.dispatch` (any `Command`, shell events
included), `debug.openTab`, `debug.accelerator`, `debug.sendKey`, `debug.focus`,
`debug.realKeys`, `debug.foreign` (browsers Chromium created for extensions: entries, counters,
events, hidden windows and the activation-hook counters — ARCHITECTURE §4.5), `debug.foreign.trigger`
(`{url}`: `Target.createTarget`, so Chromium creates such a browser without an extension),
`debug.foreign.close` (`{id}`: closes one now, ignoring the close rules),
`debug.resetPermissions` (`{origin, bits}`: resets the content settings those
CEF permission request bits map to, like an expired one-time grant), `debug.hoverInput`
(`{enabled?, pointer?: {x, y, buttons?, overWindow?, ownedPopup?} | null}` or
`{realCursor: {x, y, holdMs?}}`: the sidebar hover reveal's pointer detection on/off, a virtual
pointer in window DIP, or one real-cursor check that only runs while our window is the foreground
window and answers `{snapshot, target, landed, userMoved, restored}`: the cursor is put back unless
it was moved away from where it landed), `debug.postMouse`
(`{steps: [{type: "move"|"down"|"up"|"dblclick", x, y, button?} | {waitMs}]}`: mouse messages
posted to our own window, 16 ms apart; `dblclick` posts both clicks at once, because with the real
cursor resting over the window Windows synthesizes a move there after each release, which would
split paced clicks), `debug.cdp` (`{tab, method, params?, sessionId?, timeoutMs?}` → `{result, ms}`
or `{error, ms}`: any DevTools method through the tab's in-process session, for measurements — the
agent tools themselves are limited to an allowlist; replies that carry a `sessionId` are dropped by
the agent client's session rule, so `sessionId` only works for fire-and-forget calls),
`debug.cdpEvents` (`{tab, clear?}` → the last
200 events of that session), `debug.tabKey` (`{tab, key?}`: a key press sent to that tab's
browser host; it reaches `on_pre_key_event` like the user's typing, for agent takeover tests) and
`debug.motion` (`{floorMs?: number | null}` → the motion snapshot; `floorMs` raises the acknowledged
exits' floor (§14) so a check can act *inside* a linger instead of racing a 108 ms timer, `null`
restores the real 50/60 ms — nothing else about the protocol changes).
`debug.info` includes
`motion: {level, off, systemAnimations, systemAnimationsRead, hideDelayMs, parkDelayMs,
toastDelayMs, switcherDelayMs, hideFloorMs, parkFloorMs, waitCapMs, fadeMs, themeFadeMs,
chromeDelayMs, chromeCalls, chromeDelayed, reads, changes, settingMessages, lingering, pending[],
exits, acks, ackTimeouts, cancels, earlyHides, staleAcks, lastExitMs, slowestExitMs}` (§14: the
delays the shell keeps whatever the settings say, and the acknowledged exits' counters),
`automation: {cdp, tasks, keyEvents, session, guards, frames, console: {tabs, messages}, ui}`,
`permissions: {pending, oneTimeGrants, grantResets, autoblockClears}` and
`sidebarHover: {enabled, ready, armed, polling, placement, visible, overlayVisible, bounds, pinned,
locked, needsExit, revealAfterMs, reveals, hides, dismisses, …}` (`revealAfterMs`: time from the
virtual pointer's last move to the latest reveal), `overlays.hosts[]: {bounds, hostBounds, card:
{built, radius, shadow, inner, orientation}, …}` (`bounds` = the visible card, `hostBounds` = the
overlay widget with its shadow) and `rounded: {masks[], peekMasks[], snapUnit, colors, stats}` (the
rounded-corner pieces, ARCHITECTURE §4.4). Release builds return 404.

## 3. Events (`on(event, cb)`)

| event | payload | who cares |
|---|---|---|
| `state` | `UiState` | every surface. Overlay intents live here too: `commandBar`, `find`, `toast`, `switcher`, `peek`, `sidebarPanel`, `permissionPrompts`, `agent` (§9), `motion` (§14) and `update` (§15) |
| `find.result` | `{tab, count, active, final}` | find bar counter (too frequent for `state`) |
| `sidebar.hover` | `{visible, dismiss, gen?}` | sidebar only (§5 "Floating sidebar"): `visible` = the floating sidebar is shown (or the sidebar docked) from now on, `false` = it is about to be hidden or parked (hide the contents now); `dismiss` = close menus, popovers and drags (every `Menu`/`Popover` that isn't a core panel: `dismissFloatingLayers()` in `components.js`); `gen` (hides only) = the shell is waiting for that blank frame — answer `surface.exited {gen}` (§14) |
| `surface.exit` | `{gen}` | the toast and the switcher: blank now, the shell is about to hide this overlay and waits for `surface.exited {gen}` (§14). Sent to that surface's browser only |

**`seq` handling**: `commandBar.seq`, `find.seq` and `sidebarPanel.seq` increase each time the
intent is (re)issued. When a surface sees a `seq` it hasn't handled:
- command bar: set the input to `commandBar.text`, select all, focus;
- find bar: prefill `find.text`, select all, focus;
- sidebar: open the requested panel.

A toast is identified by `toast.id`. After `durationMs`, or on the × button, the toast page
dispatches `dismissToast {id}`.

## 4. Surfaces

All surfaces share `ui/common/`:
- `ipc.js`, `mock.js`, `fixtures/`;
- `tokens.css`, `base.css`, `theme.js` (applies colors), `icons.js` (inline SVG glyphs), `util.js`;
- `vendor/htm-preact.js` (standalone `htm` + Preact + hooks: `import { html, render, useState, useEffect, useRef, useMemo, useCallback } from '/common/vendor/htm-preact.js'`).

Each surface lives at `ui/<host>/index.html` + `<host>.js` + `<host>.css` and is served at
`sta://<host>/`. If an overlay page (`find`, `permission`, `switcher`, `toast`, `peek`) is
missing, the shell serves a minimal built-in placeholder that renders the intent from `state` and
follows the same protocol (`surface.setSize`, `ui.ready`). `extension` (the popup card's header) has
one too.

Two hosts also serve **extension icons** at `sta://command/__ext-icon/<id>/<px>` and
`sta://settings/__ext-icon/<id>/<px>`: the file comes from that extension's own directory, and a
`sta://` page may not load `chrome-extension://` images (ARCHITECTURE §4.6).

A surface is locked to its own `sta://<host>/`: other navigations are cancelled (web URLs open
as a new tab; external protocols such as `mailto:` go to the OS when the click had a user
gesture), and a surface that ends up on `about:blank` is reloaded. Internal pages (`settings`,
`archive`, `history`, `boosts`) may move between `sta://` hosts; any other URL replaces them
with an untrusted web tab. Links with `target="_blank"` / `window.open` from UI pages open as tabs.

Every page:
- links `/common/tokens.css` and `/common/base.css`;
- loads one module script (no inline scripts);
- carries the CSP as a `<meta http-equiv="Content-Security-Policy">` (see ARCHITECTURE §6);
- on each state, sets `document.documentElement.dataset.theme = dark ? 'dark' : 'light'` and
  `style.colorScheme`, and applies the **active space's** `colors` as CSS custom properties on
  `:root`:
  `--frame --grad-start --grad-end --accent --text --text-muted --hover --pressed --active-row --divider --surface --border`
  (`theme.js` `applyTheme(state)`).

| host | where | size | purpose |
|---|---|---|---|
| `sidebar` | left column, full height; while hidden, a floating rounded card `{8, 8, width + 8, height − 16}` on hover | width = `state.window.sidebarWidth` (200–440) | the Arc sidebar (§5) |
| `topbar` | above content, right column | height 40 | drag area; when the sidebar is hidden: sidebar toggle, back/forward/reload, URL pill (centered); always: split button, min/max/close caption buttons (46×40 each, close hover `#c42b1c` with white glyph) |
| `empty` | content area when no tab | fills | themed empty state in a card like a tab's page (2px inset, `--content-radius`): large faint mark, "Ctrl+T to open a tab", "Alt+1..9 switch space" |
| `command` | overlay over content | width set by shell (480–680); height via `surface.setSize` | command bar (§6) |
| `find` | overlay, top-right of focused pane | 360×44 card (page 344×36) | find in page: input, `n/m`, ↑ ↓, Aa (match case), × |
| `permission` | overlay, top-left of focused pane | 340×auto | "`host` wants to use your camera" + Allow / Block, "Remember" checkbox (unchecked: Allow lasts until no tab shows the site, Block asks again next time). Allow and Block ignore input for **400 ms** after a fresh prompt and after the prompt is uncovered again — an always-on rule, not an animation and not a setting (§14) |
| `switcher` | overlay, centered | cards 132×150, up to 5 | Ctrl+Tab MRU switcher; click → `mruSelect` |
| `toast` | overlay, bottom-center of content | a pill of auto width ≤ 480 × 36 (page ≤ 448 × 28) via `surface.setSize` | one toast; action button dispatches `action.command` then `dismissToast` |
| `peek` | header strip of the Peek overlay | 40 high | favicon, host, buttons ×(`closePeek`), Split(`expandPeek{split:true}`), Expand(`expandPeek`); Split and Expand are hidden when `peek.popup` |
| `agent` | overlay, top-right of the content (8 px inset) | 380 (300–460) × auto | AI agents (§9): the first of `agent.prompts` (connection, site or tab approval), else the activity panel while `agent.panelOpen` |
| `extension` | header strip of the extension popup card, top-right of the focused pane (8 px inset, below a visible find bar) | 40 high above the extension's page, which is 25×25…800×600 as the popup asks | the open popup (`extensions.popup`): icon, name, **Options** (`runExtension{action:"options"}`), × (`closeExtensionPopup`). When `popup.failed`, the strip is the whole card and says "This popup doesn't work in sta yet" (§12) |
| `settings`, `archive`, `history`, `boosts` | internal pages opened as tabs | full tab | trusted pages (IPC available); `boosts/?id=N` selects a boost; `settings/?section=extensions&ext=<id>` opens Settings at one extension |

**Overlays are rounded cards drawn by the shell** (ARCHITECTURE §4.4). A browser view is always an
opaque rectangle, so the shell surrounds the page with a native card: rounded corners (command bar,
permission, agent, switcher, Peek, floating sidebar 12; find bar 8; toast 16), a 1px `--border`
edge and a soft shadow over the content. The page itself stays a square, borderless `--surface`
rectangle: `ipc.js` adds `html.native-card` to overlay pages inside sta, and `base.css` draws the
1px edge only without it (mock mode). The card adds, per side, between its edge and the page:
- command bar, permission, agent, switcher: 4px left and right, 12px above and below;
- Peek: 12px on every side (the page's own corners are rounded with 8px masks);
- find bar: 8px left and right, 4px above and below (the page is 344×36 for a 360×44 card);
- toast: 16px left and right, 4px above and below (the page is 28 high for a 36px pill);
- floating sidebar: 4px left and right, 12px above and below, `--frame` colored.
Pages size themselves (`surface.setSize`) without the card and tighten their own paddings under
`.native-card` so the distances from the card edge stay the same. The content card (web pages, the
empty state) is rounded too: 10px corners inside a 2px ring (`--content-radius`, `--content-ring`),
drawn by corner masks over the page; the ring is the frame color, the accent on the focused split
pane, or the agent color on a tab an AI agent is acting on (§9).

Overlays stack in the order Peek, command bar, find bar, permission, agent, floating sidebar,
switcher, toast (lowest first), whatever order they are shown in: an overlay that is already up when a lower one appears
(e.g. a permission prompt or the command bar when Peek opens) stays above it. An overlay becomes
visible only after its page sent `ui.ready` once, and the toast and the switcher stay visible until
their page reports a blank frame when they are hidden (§14). The command bar, find bar, permission
prompt and agent overlay take keyboard focus when shown and don't close an open Peek (they work on
it); Peek appearing doesn't take focus from them. An agent approval prompt that appears while the user is
typing (a key in the last 2 s) doesn't take focus. Focusing any other browser closes the command
bar, a non-popup Peek and the agent activity panel (not a prompt).

## 5. Sidebar anatomy (top → bottom)

1. **Top row (40px)**, `app-region: drag` except buttons:
   - ≡ app menu (`toggleSidebarPanel appMenu`);
   - sidebar toggle ◧ (`toggleSidebar`);
   - back / forward / reload-or-stop (`goBack` …), disabled per `current.canGoBack` etc.
2. **URL pill (36px, radius 12)**:
   - lock or info glyph + `current.pill`;
   - hover shows a copy button (`copyUrl {}`) and a paintbrush dot when `current.boosts` is
     non-empty (click → `toggleBoost {id}`; greyed when disabled);
   - click opens `openCommandBar {mode:'editUrl'}`;
   - a 2px progress bar along the bottom while `current.loading`;
   - with no tab it shows "Search or enter URL…" and a click opens `newTab` mode.
3. **Favorites grid** (`state.favorites`): tiles 48px high, min width 52px, gap 8, radius 12,
   20px favicon, 4 columns at the default width. Active tile highlighted; unloaded tiles have a
   desaturated favicon. Click → `activateItem`; double-click → `resetToPinned`.
4. **Space title row (32px)**: `icon name` in 13/600. Hover "…" → `openSidebarPanel {editSpace}`.
5. **Pinned list**:
   - tab rows (§5.1);
   - folders: chevron + folder glyph + name; click → `toggleFolder`; children indented 12px;
     inline rename when `sidebarPanel` is `renameItem` for it.
6. **Divider** with "↓ Clear" on hover → `clearToday {}`.
7. **"+ New Tab" row** → `openCommandBar {mode:'newTab'}`.
8. **Today list** (tab rows and split rows). Pinned and Today share one scroll area with a thin
   overlay scrollbar.
9. **Bottom bar (44px)**:
   - library/downloads button (progress ring while any download is `inProgress`; click →
     `toggleSidebarPanel downloads`);
   - space icons (28px, active highlighted; click → `switchSpace`; right-click →
     `openSidebarPanel editSpace`; mouse buttons 3/4 → `switchSpaceAdjacent`);
   - "+" → `openSidebarPanel newSpace`.

**Floating sidebar (hover reveal).** While the sidebar is hidden, the shell shows the same page
floating over the content when the pointer rests at the window's left edge (120 ms), and hides it
400 ms after the pointer leaves it. Esc in a page hides it too, and the page still gets the key.
`state.window.sidebarVisible` stays `false` meanwhile:
- the page adds `.is-floating` (no drag areas; inside sta the shell draws the rounded card
  around it, and the page only fades its gradient to `--frame` on every edge; without the shell a
  1px `--border` edge) and the
  ◧ button reads "Keep sidebar open (Ctrl+S)"; `toggleSidebar` docks it;
- the floating sidebar never takes keyboard focus (a click in it gives focus back to the page), so
  it never shows a typing target: panels that hold input render only in a docked sidebar (core
  docks a hidden sidebar for them), and the "Rename…" menu items show no F2 hint;
- a press anywhere in it dispatches `closeCommandBar` while the command bar is open (the page does
  it on `pointerdown`, before the click's own command), as focusing the docked sidebar does;
- while a menu, popover or drag is open in it the page sends `sidebar.hoverLock {locked:true}`
  (§2): it stays open wherever the pointer goes; `{locked:false}` with the pointer outside hides it
  at once (e.g. a menu closed by a press outside).

The shell sends `sidebar.hover` (§3): on `visible:false` the page fades its contents out over at most
60 ms and answers `surface.exited {gen}` once that blank frame is out — the shell hides (or parks) the
overlay then, at least 50/60 ms after asking, so the next reveal starts empty (§14). On `visible:true`
the contents slide in from −24px over `sidebar.hoverReveal`'s duration; on `dismiss` the page closes
its menus, popovers and drags (a press outside the floating sidebar).

At the `full` level the **card itself travels** instead (`ARCHITECTURE` §4.3): the shell puts the
host a whole card-width outside the window, where the window clips it to a sliver, sends
`visible:true` with no `gen`, and slides the host home — so the page's own contents do not move
(`.is-floating` skips the −24px slide; it lays out at `state.window.sidebarWidth` and stays pinned
to the right edge while the visible slice grows) and a reveal needs no blank frame to start from. A hide then sends `{visible:true, dismiss:true}` (close the
menus, keep the contents: they ride the card out), slides the card out of the window, hides it there
and only then sends `visible:false` — no `gen`, because nothing of that surface is on screen to
acknowledge. Parking a docked sidebar always uses the acknowledged exit above.

Panels (`state.sidebarPanel`, closed with `closeSidebarPanel` on Esc or click outside). The
transient panels (downloads popover, app menu), e.g. opened by Ctrl+J / Alt+F while a page keeps
focus, also close on Esc in that page (the shell's Esc chain) and when a page takes focus
(`TabFocused`, core); the space sheets, an inline rename and the edit pinned page popover, which
hold input, don't. Opening a panel while the sidebar is hidden reveals the sidebar only while the
panel is open (not saved): transient panels open in the floating sidebar, which stays open while
they are (`state.window.sidebarVisible` stays `false`); panels that hold input need keyboard focus,
so they dock the sidebar (`sidebarVisible` is `true` during that time and `false` again once the
panel closes). `toggleSidebar` while revealed docks it for real and keeps the panel open:
- **downloads** popover: anchored to the bottom bar, max 20 rows. Each row shows file name,
  progress bar and `12.3 MB of 40 MB · 1.2 MB/s`, with actions open / show in folder /
  pause-resume / cancel / retry / copy link (`copyText`) / dismiss.
- **appMenu**: New Space, New Folder, Settings, Archive, History, Downloads, Toggle theme, Quit.
- **newSpace / editSpace sheet**: emoji picker (common emoji grid + free text), name, theme
  presets (`state.themePresets` swatches, live preview via `theme.colors`), delete (edit only,
  disabled when it is the last space).
- **renameItem**: inline rename on the row.
- **editPinned** (pinned tabs and favorites): "Edit Pinned Page" popover anchored to the row, with
  Title (focused, selected) and Pinned URL fields. Save / Enter → `editPinned {id, title?, url?}`
  with the changed fields (core closes the panel); Cancel, Esc, a click outside it or an unchanged
  save → `closeSidebarPanel`. Core closes it when the tab stops being pinned.

### 5.1 Tab row (36px, radius 8, padding 0 10)

- Content: favicon 16 (spinner while `loading`; letter tile fallback from `host`), title
  (ellipsis), audible/muted glyph (click → `toggleMute`), hover × (→ `closeItem {id}`).
- Active row: `--active-row` background + 500 weight. Unloaded: 60% opacity. Crashed/failed:
  warning glyph.
- Pinned/favorite `navigated`: a "/" before the title; clicking the favicon → `resetToPinned`.
- Split row: one row divided into `panes.length` segments, each with favicon + title and its own
  hover ×. Click segment → `activateItem {id: pane.id}`; × → `closeItem {id: pane.id}`.
- Middle-click → `closeItem`. Double-click title (or F2) → inline rename → `renameItem`.
- Right-click → HTML context menu (arc_spec §2.23):
  - Copy URL `copyUrl {id}`, Rename, Pin/Unpin `togglePin`, Add to Favorites / Remove;
  - Edit Pinned Page ▸ (Replace with Current `replacePinnedUrl`, Edit… `openSidebarPanel
    {editPinned, id}`);
  - Reset to Pinned URL, Move to Space ▸ (other spaces), Open in Split View
    (`splitWith {tab:id, with: focusedTab, side:'right'}`), Duplicate, Unload `unloadTab`,
    Archive/Close.
- Drag & drop (pointer events, 4px threshold, 2px accent insertion line + 6px dot):
  - drop in a list → `moveItem {id, to:{container, before}}`;
  - onto a folder row (300 ms hover highlight) → `{container:{type:'folder', id}}`;
  - onto a space icon → `moveToSpace`;
  - dropping onto another tab row's center → `splitWith`;
  - links dropped from pages → `openUrlAt`.
- Keyboard: ↑/↓ moves the focus ring, Enter activates, F2 renames, Delete closes.

## 6. Command bar behaviour

- When `state.commandBar.seq` changes: input = `commandBar.text`, select all, focus, query
  immediately. When `state.commandBar` becomes `null`, clear.
- **Typing**: send `omnibox.query` on every input event (local, fast) with an incrementing `seq`,
  and drop responses whose `seq` isn't the latest. Set `preventInlineAutocomplete: true` when
  the last edit was a deletion, and whenever a completion couldn't be shown after the caret (an
  edit before the end of the text, prefilled selected text), so Enter never opens a completion the
  user can't see.
- **Inline completion**: when `inlineCompletion` extends the typed text, show the remainder
  selected in the input; →/End accept it. `results[0].command` already opens the completed text
  (with the history entry's scheme when typed text would resolve differently, e.g.
  `http://intranet.example:8080`), so the UI never builds commands itself. Core completes from
  history first (typed or frequent hosts, no whitespace); without one, from the request's
  `suggestions`:
  - only when `preventInlineAutocomplete` is false, the mode isn't actions, and the text doesn't
    start with `?` and isn't URL-like;
  - the first suggestion that starts with the typed text (case-insensitive, Unicode-aware; spaces
    allowed: `rust pro` → `rust programming`) and is longer, skipping URL-like suggestions;
  - `inlineCompletion` = the typed text (the user's case) + the rest of the suggestion;
  - `results[0]` (key `search`) searches the completion: `openUrl` of the engine's search URL in
    a new tab (`currentTab` in `editUrl`; `splitOpenInput` with `?completion` in `split`),
    `altCommand` a background tab. That suggestion isn't repeated as a Suggestions row, and no
    Top Hit is shown.
- **Search suggestions** (outside actions mode, `state.settings.searchSuggestions` on, non-empty
  text that isn't URL-like):
  1. query `omnibox.query` right away with cached `suggestions`: the exact text's, else the
     longest cached prefix's filtered to those that still start with the text
     (case-insensitive), so the inline completion stays stable while typing;
  2. after ~80 ms without typing, `invoke('omnibox.suggest', {text})`;
  3. when the reply's `text` still equals the typed text, cache it (LRU, ~50 entries) and query
     again with those suggestions, keeping the selected row (by `key`) if the user moved the
     selection.

  Deletions still pass `preventInlineAutocomplete: true`: no inline completion, but the
  Suggestions rows show. The shell only fetches for the command bar and only the newest request
  (see `omnibox.suggest`, §2).
- **Keys**:
  - ↑/↓ (and Ctrl+N/P) select;
  - Enter → `commitOmnibox {command: results[sel].command}`;
  - Alt+Enter → `commitOmnibox {command: results[sel].altCommand ?? command, alt: true}`;
  - Tab / Shift+Tab (without Ctrl or Alt) always toggle actions mode on and off (default
    prevented, focus stays in the input): the typed text (without an inline completion) is kept
    and queried again; toggling off returns to the mode the bar was opened in (`newTab`,
    `editUrl`, `split`). The mode chip shows the current mode with a `Tab` hint and toggles on
    click. A `>` prefix also means actions; Backspace on an empty input in toggled actions mode
    returns to the opening mode;
  - Esc → `closeCommandBar`.
  Core decides whether the bar closes after a commit.
- **External protocols**: `mailto:`, `tel:`, `sms:` and other `scheme://…` app URLs classify as
  URLs; committing one hands it to the OS (core's `Effect::OpenExternal`) and never creates or
  navigates a tab. The bar closes like after any commit.
- **Rows**:
  - Grouped by `group`, with a small-caps header for Recent Tabs, Suggested, Tabs, Actions, Spaces,
    History, Suggestions, Archive. TopHit/Go have no header.
  - Each row is 44px: 16px icon (favicon img / emoji / glyph), title 14px, subtitle 12px
    `--text-muted`, right hint chip in 11px mono.
  - Selected row: accent at 12% (`color-mix`) + 3px accent bar at the left edge.
- **Sizing**: after each render, `surface.setSize {height}` with the natural height (input 56 +
  rows; at most 8 rows visible, then scroll).

## 7. Design tokens (`ui/common/tokens.css`)

Use `arc_spec.md` §5 as the reference (fonts: Segoe UI Variable; geometry; motion). Key values:

```
--font-ui: "Segoe UI Variable Text","Segoe UI",system-ui,sans-serif
--font-mono: "Cascadia Mono",Consolas,monospace
--radius-xs 4px; --radius-sm 6px; --radius-md 8px; --radius-lg 12px; --radius-xl 16px; --radius-full 999px
--content-radius 10px; --content-ring 2px; --overlay-radius 12px; --overlay-shadow 8px
--find-radius 8px; --toast-radius 16px; --peek-page-radius 8px    (mirrors of rounded.rs)
--row-h 36px; --row-radius 8px; --sidebar-pad-x 8px; --topbar-h 40px; --url-pill-h 36px
--fav-tile-h 48px; --fav-gap 8px; --bottom-bar-h 44px; --space-icon 28px
--t-controls-hover-press 90ms; --t-menus-pop-in 140ms; --ease-out cubic-bezier(0.16,1,0.3,1)
(one --t-* per animation key, §14; --motion-distance 1 or 0)
```

Colors come only from `ThemeColors` (§4 mapping). **The sidebar and topbar background is
`--frame`**. The sidebar body may use a very subtle vertical gradient `--grad-start → --grad-end`
as long as its top and bottom 8px and right 8px equal `--frame`, so it joins the native frame
color seamlessly. Overlay pages use `--surface` with a 1px `--border`.

## 8. Commands quick reference (JSON)

```jsonc
{"type":"openInput","text":"rust lang","target":"newTab"}          // target: currentTab|newTab|backgroundTab
{"type":"commitOmnibox","command":{"type":"activateItem","id":3},"alt":false}
{"type":"activateItem","id":12}
{"type":"closeItem"}                                               // id omitted = focused
{"type":"togglePin","id":12}
{"type":"moveItem","id":12,"to":{"container":{"type":"pinned","space":3},"before":null}}
{"type":"switchSpace","id":3}
{"type":"newSpace","name":"Work","icon":"🚀","theme":{"hue":245,"hue2":275,"chroma":0.06}}
{"type":"openCommandBar","mode":"newTab"}                          // newTab|editUrl|split|actions
{"type":"closeCommandBar","seq":7}                                 // seq optional: a stale one is ignored (§14)
{"type":"openSidebarPanel","panel":{"type":"editSpace","id":3}}    // downloads|appMenu|newSpace|editSpace|renameItem|editPinned
{"type":"toggleSidebar"}                                           // docked → hidden; hidden or floating → docked
{"type":"windowControl","action":"toggleMaximize"}                 // minimize|toggleMaximize|close|toggleFullscreen
{"type":"updateSettings","patch":{"searchEngine":"duckDuckGo"}}
{"type":"updateSettings","patch":{"animations":{"enabled":false}}}  // animations (§14): reset, then scalars, then maps
{"type":"updateSettings","patch":{"animations":{"groups":{"overlays":false},"set":{"menus.popIn":null}}}}
{"type":"openInternalPage","page":"settings"}                      // settings|archive|history|boosts
{"type":"dismissToast","id":7}
{"type":"resolvePermission","id":4,"allow":false,"remember":false}  // one-off Block: asked again next time
{"type":"resolvePermission","id":5,"allow":true,"remember":false}   // allow this time: until no tab shows the origin (or restart)
{"type":"updateSettings","patch":{"agentAccess":"full"}}           // off|readOnly|full (AI agents, §9)
{"type":"answerAgentConnection","id":7,"allow":true,"remember":false} // agent overlay or Settings only (403 elsewhere)
{"type":"answerSitePermission","id":8,"allow":true,"remember":true}
{"type":"stopAgents"}                                               // resumeAgents undoes the pause
{"type":"toggleAgentPanel"}                                         // topbar chip; closeAgentPanel {focusLost?}
{"type":"archiveAgentTabs"}                                         // the session-end toast action
{"type":"shareTabWithAgent","tab":12,"shared":true}
{"type":"answerTabAccess","id":9,"allow":true}                     // request_tab_access prompt; agent overlay or Settings only
{"type":"resolveAgentDownload","id":3,"keep":false}
{"type":"toggleDevTools"}                                          // F12: dock DevTools for the focused tab, or close them
{"type":"focusDevTools"}                                           // Ctrl+Shift+I: open, then focus, then (frontend focused) close
{"type":"undockDevTools"}                                          // "Undock DevTools": CEF's own window until they close again
{"type":"openCommandBar","mode":"extensions"}                      // Ctrl+E: the extensions picker
{"type":"runExtension","id":"<32 letters a-p>","action":"primary"} // primary|options|popup|webStore|manage; never turns one on
{"type":"requestExtensionDetails","id":"…"}                       // Chrome's permission warnings, host access and source
{"type":"setExtensionEnabled","id":"…","enabled":true}            // Settings only; needs the details first for an external one
{"type":"removeExtension","id":"…"}                               // Chromium shows its own "Remove …?" confirmation
{"type":"closeExtensionPopup"}                                    // the card's × (Esc and blur close it too)
```

Shell-only (never from the UI; the shell sends them, see §10, §11, §12 and §14): `systemAnimationsChanged`, `foreignTabRequested`,
`extensionInstalled`, `foreignBlocked`, `devToolsClosed`, `devToolsUndockRequested`,
`devToolsLinkRequested`, `inspectElement`, `extensionsChanged`, `extensionDetailsLoaded`,
`extensionOpFailed`, `extensionPopupClosed`, `safeModeStarted`.

## 9. AI agents (MCP)

Agents connect through `sta-mcp.exe` (docs/MCP.md, ARCHITECTURE §5.2). What UI surfaces see:

- **Settings** (`state.settings`): `agentAccess` (`off`|`readOnly`|`full`, default `off`),
  `agentScope` (`agentTabs`|`allTabs`), `agentSites` (`ask`|`all`), `agentScripts`
  (`off`|`isolated`|`main`), `agentHistory`, `agentDownloads`, `agentAllowPrivateNetwork`,
  `agentBlockedHosts` and `agentAllowedSites` (host names, normalized by core),
  `agentTrustedClients` (`[{name, exe, signer, addedAt}]`; a patch can only remove entries).
- **`state.agent`**: `{paused, sessions: [{session, client, access, startedAt}], prompts: [{id,
  kind: "connection", client, requestedAt} | {id, kind: "site", session, site, tab, requestedAt} |
  {id, kind: "tab", session, tab, reason, requestedAt}],
  activity: [{session, tool, tab, site, at, error?}] (newest first, ≤ 5; `error` = the error code
  of a failed call), heldDownloads: [{id, tab, fileName}], panelOpen, openedTabs}`. `panelOpen` =
  the activity panel is open; `openedTabs` = tabs agents opened that are still in a Today list (what
  `archiveAgentTabs` archives). `client` = `{name, title, version, exe, signer, verified}` (`name`/`title`/`version`
  are self-reported; `exe`/`signer` are determined by the browser; only `verified` clients may be
  trusted permanently).
- **Tab views** carry `agent: true` for tabs in agent scope (the field is omitted otherwise).
- **Commands** (UI): `answerAgentConnection {id, allow, remember?}`, `answerSitePermission {id,
  allow, remember?}` and `answerTabAccess {id, allow}` (a `tab` prompt from `request_tab_access`:
  `allow` shares the tab with agents and allows its current site for that session; every waiting
  request of that session for the tab gets the same answer) — 403 unless sent by the agent overlay or the Settings page, also inside
  `commitOmnibox`; `stopAgents`, `resumeAgents`, `shareTabWithAgent {tab, shared?}`,
  `resolveAgentDownload {id, keep}`, `toggleAgentPanel`, `closeAgentPanel {focusLost?}` (a toggle
  within 400 ms after a focus-loss close is ignored: it is the click on the chip that caused it),
  `archiveAgentTabs` (archives the Today tabs agents opened as one Ctrl+Shift+T batch, toast with
  Undo). Shell events (never from UI): `agentConnectionRequested`, `agentSessionStarted`,
  `agentSessionEnded` (the last one ends with tabs agents opened still in Today: toast "Agent
  session ended" with the action "Archive N agent tabs"), `agentActivity {…, error?}`,
  `agentSiteRequested`, `openAgentTab`, `loadTab`, `showAgentTab`, `agentDownloadHeld`,
  `agentTabAdopted`, `agentTabAccessRequested {id, session, tab, reason}` (answered at once when the
  tab is already in scope, closed, or agents are off or paused; a prompt whose tab closes is denied).
- **Surfaces**: the `agent` overlay (prompts: connection, site, and tab — the tab's title and host,
  the agent's reason quoted as its own words, Deny / Share tab; buttons inert for 1 s after they
  appear and after every key, no default button, Esc denies once armed; activity panel), the topbar chip
  (`ui/topbar/agent-chip.js`: shown while `sessions`, `prompts`, `paused` or `heldDownloads`; opens
  the panel; Stop / Resume), the sidebar glyph on tab rows with `agent: true` and the tab menu item
  "Share with AI Agents" (while access is on and the scope is agent tabs), Settings → AI agents
  (MCP) (`agent.info`, `agent.testConnection`). The shell adds a 2 px agent-colored frame (the tab
  wrapper) around agent-controlled tabs of connected sessions; `--agent` in `ui/common/agent.css` is
  the same color.

## 10. Chrome-created browsers (extensions)

Chromium opens browsers of its own for extensions and the Web Store's post-install UI. The shell
hides those windows, adopts what they wanted to show and reports it to core with three
**shell-only** commands (ARCHITECTURE §4.5). No UI surface is involved beyond the toast.

| command | payload | what the user sees |
|---|---|---|
| `foreignTabRequested` | `{url, extension?: {id, name, pages[], webAccessible[], recentlyInstalled}}` | `http(s)` with a host, and extension pages the manifest declares (options, popup, side panel), resources web-accessible to every site, or any page of an extension installed in the last 60 s: a foreground Today tab after the active item. Any other extension page: the toast "An extension wants to open a page of *name*" with **Open** (`openUrl`) — *name* is the page's **owner**, not the extension that asked (sta cannot see who asked: ARCHITECTURE §4.5 "Who asked"). Anything else: nothing. Rate limited **by what core does**: 3 tabs and 2 asks per 10 s, then the `rateLimited` toast below (once per 10 s) instead. A request core only asks about, or refuses, leaves the tab budget alone. |
| `extensionInstalled` | `{id, name, external?}` | Toast "*name* added · Ctrl+E" (sta has no extension toolbar, so the toast says how to use it), or "*name* added by another program, off until you allow it" when another program registered it. The id may open its own pages for 60 s (welcome flows). |
| `foreignBlocked` | `{reason: "rateLimited" \| "incognito"}` | Toast "An extension keeps opening windows; sta blocked them" / "sta has no private windows". `incognito` is the shell's private-window refusal; `rateLimited` is the flood toast — the shell sends it for a flood it drops before core hears about it (24 navigations per 10 s), and core raises the same one, with the same once-per-10 s dedupe, when a budget above runs out. |

Extension popup windows (`_crx_…`) and sign-in flows (`identity.launchWebAuthFlow`) stay ordinary
Chromium windows with sta's caption colors; everything else is hidden and closed again.

## 11. Docked DevTools

DevTools live **inside the window**: the frontend is a BrowserView in the tab's card and the page
sits on top of it, at the rect the frontend asks for (ARCHITECTURE §4.1 "DevTools"). No UI surface
draws any of it; the UI only sends the three user commands above, and the shell reports four
**shell-only** commands back.

| command | payload | what it means |
|---|---|---|
| `devToolsClosed` | `{tab}` | The frontend closed or crashed, the undocked window was closed, or the frontend closed *itself* (its own ✕ / `closeWindow`, which names the tab it belongs to rather than the focused one). Core forgets the tab's DevTools (nothing about them is persisted). |
| `devToolsUndockRequested` | `{tab}` | The frontend's own **Undock** (`setIsDocked(false)`). Core closes the docked frontend and opens CEF's own DevTools window for that tab, until those DevTools close. |
| `devToolsLinkRequested` | `{tab, url, search?}` | The frontend's "open in new tab" (`openInNewTab`) or, with `search`, its "search in new tab" (`openSearchResultsInNewTab`, where `url` is the query). Checked like any link web content offers (`urls::web_content_may_open`); external protocols go to the OS. |
| `inspectElement` | `{tab, x, y}` | The page's context-menu **Inspect** at a point in the page view's own pixels. Core opens DevTools if needed, then `InspectAt` selects the node there (the shell divides the point by the page zoom and follows an `<iframe>` owner into that frame's session). |

State: none. `UiState` says nothing about DevTools, and `state.json` never mentions them — like
Chrome, sta does not restore an open DevTools.

## 12. Extensions (Ctrl+E)

The installed extensions are read from the profile by the shell and pushed into core; the UI only
renders `state.extensions` and sends the six commands of §8 (ARCHITECTURE §4.6).

```jsonc
"extensions": {
  "items": [{
    "id": "nbjocpdeikjicjcicjjgijlaekmikkdm",
    "name": "sta probe: windows", "shortName": "", "version": "1.0", "description": "…",
    "state": "enabled",            // enabled | off | needsApproval | blocked
    "blocked": null,               // with state "blocked", why: policy | unsupported | damaged |
                                   //   safety | requirement | custodian | unknown. Only `policy`
                                   //   says "Turned off by your organization"; an MV2 extension on
                                   //   Chromium 152 is `unsupported`, a broken profile `damaged`.
    "install": "unpacked",         // webStore | unpacked | externalStore | externalLocal | managed
    "sourceLabel": "Loaded from C:\…",
    "popup": "popup.html", "options": "options.html", "sidePanel": null,
    "commands": [{"name": "_execute_action", "description": "", "shortcut": "Ctrl+Shift+Y"}]
  }],
  "details": [{"id": "…", "warnings": ["Read and change all your data on all websites"],
               "hostAccess": "On all sites", "source": "Added by another program, not from the Chrome Web Store"}],
  "busy": null,                    // an id while one operation runs
  "popup": {"id": "…", "name": "…", "icon": "sta://command/__ext-icon/…/32",
            "tab": 12, "hasOptions": true, "failed": false, "seq": 3},
  "safeMode": false,               // two crashes while starting: tabs restored unloaded
  "needsOk": 2                     // extensions another program added, still waiting
}
```

**The picker** is the command bar in `mode: "extensions"`: `omnibox.query` answers rows whose
`group` is `extensions`, `needsOk`, `extensionsOff` or `more`, with `command`
`runExtension{action:"primary"}` and `altCommand` `runExtension{action:"options"}`. Each row's icon
is `{type:"favicon", url:"sta://command/__ext-icon/<id>/32"}`. An empty query lists every group in
that order plus **Manage Extensions** and **Get Extensions**.

**What Enter does** (core, never the UI): a popup page → the card; else an options page → a tab (one
per extension: the tab is matched by extension and path, so an options page that routes itself to
`…#general` on load is still that one tab); else its Web Store page, which the row said it would open
("Toolbar click isn't supported in sta · ↵ opens its Web Store page") so the press itself adds no
toast. An extension that is **off** or waiting for the user's OK opens
`sta://settings/?section=extensions&ext=<id>` instead — **Enter never turns an extension on**.

A row under the picker's own **Needs your OK** heading reads just "Added by another program"; the same
row in Settings, where there is no heading, carries the whole sentence.

`setExtensionEnabled{enabled:true}` for a `needsApproval` extension is allowed only while that
extension's **own** disclosure is loaded and fresh (60 s), and the permission is consumed by the press
it belongs to: a Turn on the user cancelled never pre-authorises a later one.

Shell-only commands:

| command | payload | what it means |
|---|---|---|
| `extensionsChanged` | `{extensions: ExtensionInfo[]}` | The profile was (re-)read. Replaces the whole list; components are already filtered out, and an extension Chromium has not written preferences for yet is not in it. A fresh listing also ends a pending write (the row stops being busy). |
| `extensionDetailsLoaded` | `{details: {id, warnings[], hostAccess, source}}` | The answer to `requestExtensionDetails`: Chrome's own words, shown before an extension another program added is turned on. |
| `extensionOpFailed` | `{id, op, message}` | An operation failed or timed out: toast "Couldn't *turn on* *name* (*message*)". A removal the user cancelled is not a failure and sends nothing. |
| `extensionPopupClosed` | `{failed?}` | The popup card is gone: the page called `window.close()`, or (`failed: true`) it never rendered within 3 s — the card then stays, showing one honest line. The shell judges that on a measurement taken **at** the deadline, so a popup that paints late (a cold service worker) is shown rather than declared broken, and the card is not shown at all until the page reports a size above the 25×25 minimum. |
| `safeModeStarted` | – | Two abnormal exits within 60 s of launching: core restores every tab unloaded and Settings › Extensions shows a banner. |

## 13. Link gestures (Alt+click preview)

A link can open somewhere other than its own tab in four ways. All four arrive at core as the same
**shell-only** command, `linkOpenRequested {opener, url, disposition}`, and all four pass
`urls::web_content_may_open` first (http/https with a host, `about:blank`, a `blob:` of one of
those, and `file:` only from a `file:` opener). Anything else — `javascript:`, `sta://`,
`chrome-extension://`, `mailto:` and every other external protocol — is refused with the toast
"Blocked a *scheme*: link" and opens nothing.

| `disposition` | gesture | what core does |
|---|---|---|
| `foregroundTab` | Chromium's `NEW_FOREGROUND_TAB` | a foreground Today tab |
| `backgroundTab` | Ctrl+click, middle-click, "Open Link in New Tab" | a background Today tab below the opener |
| `newWindow` | Shift+click, `NEW_POPUP`/`NEW_WINDOW`, "Open Link in Peek (Alt+Click)" | Peek while `settings.peekEnabled`, else a foreground tab |
| `preview` | **Alt+click / Alt+middle-click on a link** | Peek, from any tab of any kind (see below) |

**`preview`** is the only one that does not come from a CEF disposition. Chromium resolves
Alt+click to *download this link* inside the renderer, so neither `on_before_browse` nor
`on_open_urlfrom_tab` ever sees it: the gesture is caught in the renderer instead
(`renderer.rs`, ARCHITECTURE §4.1 "Alt+click preview") and the click is cancelled there, which is
what stops the download. The browser side (`client.rs::preview_target`) re-checks the sender before
core sees anything. Its rules:

- it previews **whatever tab it happens in** — Today, pinned, a split pane, an extension page, a
  subframe — and whatever the link's site is. `pinnedCrossSite` needs a pinned opener *and* another
  site; `preview` needs neither;
- it **replaces** an open Peek rather than nesting a second one, so an Alt+click inside Peek swaps
  the page Peek shows. Such a preview **inherits the Peek's own opener**, so Split/Expand still
  splits against the tab the first preview came from;
- the one Peek it will not replace is a **popup** Peek (a sign-in window) when the gesture was made
  in *another* tab: the flow the user is in the middle of keeps the overlay and the link opens in a
  background tab instead. An Alt+click *inside* that popup Peek is the user asking for it to move
  on, and does replace it;
- with `settings.peekEnabled` off it opens a foreground tab, exactly like Shift+click with Peek off.
  There is no separate setting: the switch that turns Peek off turns the gesture off with it. The link
  context menu spells the gesture out ("Open Link in Peek (Alt+Click)") because that menu is where a
  user looks for what a link can do;
- the gesture is **the user's**, never the page's, and never a *key*. The renderer only reports a
  click Chromium marked `isTrusted`, which a script-made event can never be (`isTrusted` is a
  `[LegacyUnforgeable]` own property: redefining it on `Event.prototype` has no effect and redefining
  it on an instance throws), and only a **pointer** click (`UIEvent.detail >= 1`). Blink's *keyboard*
  activation of a focused link — Alt+Enter, a screen reader — dispatches an equally trusted click with
  `detail === 0`, and pressing keys in a page is something an automation session can do
  (`press_key`), so keyboard activation is not the gesture. A page that redefines
  `MouseEvent.prototype.altKey` cannot steer it either — the listener calls the accessors it captured
  before any page script ran;
- it does not exist in a frame with an **opaque origin** (`sandbox` without `allow-same-origin`, a
  `data:` frame, a CSP-sandboxed page). Such a frame's embedder took even `target=_blank` away from
  it, and one user Alt+click must not hand it a top-level page of its choosing. Residue: a frame
  sandboxed *with* `allow-same-origin` keeps the gesture;
- it is refused in **agent-controlled tabs**, the way `intercept_navigation` is
  (`guards::allow_peek`), and no more than one preview per browser per 250 ms is accepted: the browser
  process never sees content-area input, so the rate limit is the only bound on a *compromised*
  renderer asking for overlay after overlay;
- `file:` needs a `file:` **frame**, not merely a `file:` tab. Core's allowlist compares against the
  tab's URL, so the browser side refuses the message first — otherwise a remote iframe inside a
  locally saved page could borrow its `file:` origin.

**What the gesture does not touch.** A `javascript:` href is the page's own button
(`javascript:void(0)` with a click handler, or a legacy `javascript:doThing()` link): the click is
left completely alone, so the page's handler runs exactly as it would and no toast is shown. Two more
shapes have nothing to preview and are **cancelled but not hidden** from the page — the page's own
handler still runs, and nothing is reported: a link into the document the click happened in (the same
URL apart from its fragment, an href of just `#` included) and a `file:` URL from a non-`file:`
document. The cancel matters: an Alt+click Chromium is allowed to keep **saves the page to disk**
(measured), which is the one thing this gesture exists to stop. What the cancel costs is small and
deliberate: Alt+clicking a plain in-page anchor does not scroll the page to it (a plain click still
does). An href longer than 64 KiB is cancelled too and then dropped in the renderer, so such a link
does nothing at all.

Alt+click therefore no longer saves a *previewable* link to disk. A link the **server** sends as an
attachment (`Content-Disposition`) still cannot be shown: it downloads exactly as a plain click would,
and the preview that has nothing to render closes itself instead of leaving an empty card over the
page (`DownloadInBlankTab`, which also cleans up after Shift+click and a pinned cross-site link). A
`download=` attribute is only a hint to the renderer and is previewed like any other link. One limit
is in the renderer's nature: a frame where **scripting is off** (`sandbox=""`, JavaScript disabled)
never runs the listener, so Chromium's own Alt+click download survives there.

## 14. Motion (animations)

Every animation sta plays has a **key** from one registry, `crates/sta-core/src/motion.rs`
(36 keys in 8 groups). Core decides which keys are on and how much motion is allowed at all; the
HTML surfaces do the animating (`ui/common/motion.js`, `ui/common/tokens.css`). The user switches
them in Settings › Animations, and the omnibox action "Turn Animations Off/On" flips the master
switch. See `docs/ARCHITECTURE.md` §4.7 for the rules the shell and the pages follow.

**`UiState.motion`** (in every `state` snapshot):

```jsonc
"motion": {
  "level": "full",            // full | reduced | off
  "off": ["overlays.toast"],  // registered keys that are off, in registry order
  "systemAnimations": true    // Windows "Animation effects" (SPI_GETCLIENTAREAANIMATION)
}
```

- `level` is `off` when the master switch is off, `reduced` when the settings follow Windows **and**
  Windows has animation effects off, and `full` otherwise.
- a key is in `off` when its group switch is off **or** its own stored choice is off.
- `systemAnimations` is **runtime** state: the shell reads it at init, on `WM_SETTINGCHANGE` and in
  the 5 s heartbeat, and reports it with the shell-only command
  `systemAnimationsChanged {enabled}`. It is never persisted, and it bumps the revision only when
  the value actually changes.

`theme.js applyMotion(state)` writes it onto `<html>` as `data-motion="full|reduced|off"` and
`data-anim-off="<key> <key> …"`, plus the `theme-fade` class for `theme.crossFade` — on pages that
paint their own background only, never inside a native card, where the shell's own fill and border
would snap while the page faded; a card page gets `theme-snap` instead, which holds its colours until
the `SetChrome` midpoint and takes them in one step. `applyWindowState(state)` adds `data-focused` while the window has
focus, which the infinite indicators in docked surfaces (the audio equalizer) run on. `ipc.js
startSurface` calls all of it beside `applyTheme`. `tokens.css` turns those into behaviour:

- `reduced` sets `--motion-distance: 0`, so an animation that multiplies its travel by it becomes a
  pure opacity fade with identity transforms;
- `off` sets `animation-duration` and `transition-duration` to `0s !important` on
  `:root[data-motion="off"]` and its `*`, `*::before` and `*::after` — **durations only**, so delays
  and end states survive (the 250 ms resize hint, the URL pill's progress fade-out, and
  `animationend` still firing). Loading state is still *shown*: a static ring and a static, visible
  bar, never nothing;
- a key that is off gets its own duration token zeroed
  (`:root[data-anim-off~="sidebar.reorder"] { --t-sidebar-reorder: 0ms }`). Each key owns exactly one
  `--t-*` token, named from the key (`sidebar.tabInsertRemove` → `--t-sidebar-tab-insert-remove`), so
  zeroing one never touches another. (`controls.hoverPress` is the one key with a second token,
  `--t-controls-press`, because a hover tint and a press are not the same length; its gate zeroes
  both.) The same rules give an indicator its static fallback when only *its* key is off, not just at
  the `off` level.

**Stored settings** are `settings.animations`:

```jsonc
"animations": {
  "enabled": true, "followSystem": true,
  "groups": {"overlays": false},          // group id → on (a missing group is on)
  "choices": {"menus.popIn": false}       // key → the user's explicit choice
}
```

`choices` holds what the user *chose*, never a difference from the default: if a later release flips
a key's default, a user who explicitly picked the old value keeps it. Unknown keys inside the
128-entry cap are kept, and an entry that is not a boolean is skipped on load rather than dropping
the whole object. There is **no `STATE_VERSION` bump**: a profile without the field loads the
defaults.

**The patch** is `updateSettings {patch: {animations: {reset?, enabled?, followSystem?, groups?,
set?}}}`, applied in one order — `reset`, then the scalars, then the maps. A map value of `null`
**clears** that entry (back to following the default / the group), which is how the settings page
tells "chose the default" from "chose nothing" apart. Unknown group ids and unknown keys in a patch
are ignored.

**`closeCommandBar {seq}`.** Activatable overlays never linger: a page-initiated close blanks the
page and dispatches after one frame. `seq` is the `commandBar.seq` the page was showing; core
**ignores** a close whose `seq` is not the open bar's, because Esc immediately followed by Ctrl+T
(a high-priority accelerator) can otherwise let the late dispatch close the *new* bar — the page
only learns about the reopen from a push that may be up to 33 ms behind. Omitting `seq` closes
whatever is open, which is what the shell's own Esc chain and focus rules want.

**Acknowledged exits (`surface.exit` / `surface.exited`).** A hidden page renders no frames, so
Chromium keeps showing its last one: whatever a surface had on screen when the shell hid its widget
is what the *next* reveal flashes first. So a surface presents a blank frame before the shell hides
it, and says so:

```jsonc
// shell → page (emit_raw_to, that surface's browser only)
"surface.exit"    {"gen": 12}
// page → shell (a request, one animation frame after nothing is left to see)
"surface.exited"  {"gen": 12}
```

- the **toast** and the **switcher** are asked this way; the **floating sidebar** is asked through the
  `sidebar.hover {visible:false, dismiss:true, gen}` it already reacts to, because it blanks there
  anyway. Nothing else uses it: activatable overlays never linger (see `closeCommandBar` below);
- `gen` is a generation. The shell hides the widget on the matching ack, or at a **cap** if none
  arrives; an ack for an exit that is no longer pending is ignored; a request without a usable `gen`
  is not answered at all;
- the widget **lingers** meanwhile — it is still visible, still restacked above a corner mask and
  still a no-drag hole. A show during the linger cancels the exit and the widget simply stays up;
- the waits are `max(floor, fade + ack)`, capped at 120 ms, where the fade is the page's own exit fade
  (`min(the key's duration, --t-surface-exit)` = 60 ms) and the floors are **50 ms** for a hide and
  **60 ms** for a park. Switching the animation off drops the *fade*, never the wait: the floors are
  correctness delays, so a page always has time for its blank frame
  (`crates/sta/src/motion.rs`, ARCHITECTURE §4.7);
- `debug.info.motion` reports it: `lingering` (surfaces waiting now), `exits`, `acks`, `ackTimeouts`
  (waits that ended at the cap), `cancels`, `earlyHides` (hides with no page to ask at all),
  `staleAcks`, `lastExitMs`, `slowestExitMs`.

**`SetChrome` at the cross-fade midpoint.** While pages cross-fade their own colours
(`theme.crossFade`, 300 ms), the native card fill, border and corner tiles can only snap — so the
shell applies `Effect::SetChrome` **150 ms** in, generation-guarded (the newest colours win) and
immediately where there is no fade to meet: the first call of the session, a minimized or closing
window, and the key or motion switched off. `debug.info.motion.chromeDelayMs` is that delay. A page
*inside* a card cannot fade (the fill around it is the shell's), so it holds the previous colours for
the same 150 ms and snaps with it — `html.theme-snap`, `ui/common/tokens.css`; otherwise a toast would
be a white pill inside a black card for the length of the delay.

## 15. Updates

`UiState.update` is what the browser knows about a newer release (`sta_core::update`,
`docs/RELEASING.md`). It is runtime state: never persisted, `idle` until the first check answers.

```jsonc
"update": { "stage": "available", "version": "0.2.0", "notes": "- …", "size": 214958080 }
```

| `stage` | fields | means |
|---|---|---|
| `idle` | — | nothing checked yet this run (or updates are switched off) |
| `checking` | — | the manifest is being fetched |
| `upToDate` | `checkedAt` | this build is the latest published release |
| `available` | `version`, `notes`, `size` | a newer release exists; nothing has been downloaded |
| `downloading` | `version`, `received`, `total` | `total` is 0 while the size is unknown |
| `ready` | `version` | downloaded, verified, unpacked: it is applied on the next start |
| `failed` | `message` | the last attempt failed; the UI offers to try again |

Three UI commands drive it, and the shell reports every transition with the shell-only
`updateStatusChanged` (§2 command catalogue):

- `checkForUpdate` — look now (the shell also checks ~8 s after startup, once);
- `downloadUpdate` — fetch the release the last check found. Core ignores it unless the status is
  `available` or `failed`, so a page cannot start two downloads or invent one;
- `installUpdate` — quit and let the staged build replace this one. Ignored unless the status is
  `ready`.

Core shows a toast for `available` ("sta 0.2.0 is available") and for `ready` ("sta 0.2.0 is
ready — restart to update"); the other stages only update the About section.
