# sta HTML UI

Browser chrome (sidebar, top bar, command bar, overlays) and internal pages, served from
`sta://<host>/` (see `docs/ARCHITECTURE.md` §6 and `docs/PROTOCOL.md`). Plain ES modules,
no build step, Preact + htm. Chromium 152 is the only target, so modern CSS (nesting,
`color-mix`, `oklch`, `:has`, container queries, `@property`) is fine.

```
ui/
  common/                 shared foundation (served at /common/ on every host)
    ipc.js                IPC client: invoke / dispatch / events / state / startSurface
    ipc-hooks.js          useUiState, useRequest
    mock.js               fixture-backed fake __staQuery (mock mode)
    mock-reducers.js      local command reducers used by mock.js
    mock-colors.js        mock-only OKLCH theme derivation (theme.colors in mock mode)
    mock-agent.js         mock-only AI agent state (?agent=…), agent command reducers, agent.* requests
    fixtures/*.json       UiState & response fixtures shaped like the Rust serde types
    tokens.css            design tokens (arc_spec §5) + theme color fallbacks + the motion levels
    base.css              reset, focus, scrollbars, utilities, component styles
    theme.js              applyTheme(state), applyMotion(state), applyColors, themeStyle
    motion.js             the motion runtime: enabled(key), animate, flip, stagger, ghost, blank
    motion-catalog.js     the 36 animation keys in 8 groups with labels and descriptions
    icons.js              <Icon> + 70 original 24px glyphs (`ICON_NAMES` is the live list)
    components.js         Button, IconButton, Menu, Popover, Select, TextField, …
    hooks.js              generic Preact hooks
    util.js               formatting, host colors, debounce/throttle, UiState helpers
    chrome.js/.css        NavButtons + UrlPill, shared by the sidebar and the top bar
    internal-page.js/.css page shell and building blocks of settings/archive/history/boosts
    agent-ui.js           AI agents: AgentGlyph, client/tool/error phrases, chip visibility (no IPC)
    agent.css             AI agents: --agent color, pulse, Verified/Unverified badges, sidebar glyph
    vendor/htm-preact.js  standalone htm + Preact 10 + hooks (vendored, don't edit)
  _gallery/               visual gallery of everything in common/ (dev only, not served by the app);
                          its "motion.js" card replays the primitives and shows the live level
  extension/              header strip of the extension popup card (Ctrl+E), see below
  <host>/index.html       one directory per surface: index.html + <host>.js + <host>.css
```

Tools for working on the UI without the shell (all in `tools/`, Node 22+):
- `ui-serve.mjs`: static server for `ui/` in mock mode (see [Mock mode](#mock-mode));
- `ui-shot.ps1` / `ui-shot.mjs`: headless-Edge screenshots with console checks (see
  [Gallery and screenshots](#gallery-and-screenshots));
- `check-mock-commands.mjs`: checks the mock's command validation against `command.rs` (see
  [Behaviour](#behaviour));
- `check-motion.mjs`: holds the animation registry (`crates/sta-core/src/motion.rs`),
  `common/motion-catalog.js` and the CSS gates in `tokens.css` together, and enforces the motion rules
  that span files (see [Motion](#motion-motionjs-and-motion-catalogjs));
- `motion-check.mjs`: runs the motion runtime in mock mode in headless Edge.

## Page template

```html
<!doctype html>
<html lang="en">
  <head>
    <meta charset="utf-8" />
    <meta http-equiv="Content-Security-Policy"
      content="default-src 'none'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data: https:; font-src 'self' data:; connect-src 'self'; base-uri 'none'; form-action 'none'" />
    <title>Sidebar</title>
    <link rel="stylesheet" href="/common/tokens.css" />
    <link rel="stylesheet" href="/common/base.css" />
    <link rel="stylesheet" href="sidebar.css" />
    <script type="module" src="sidebar.js"></script>
  </head>
  <body></body>
</html>
```

- The meta CSP mirrors the header the scheme handler sends. Leave out `frame-ancestors`: it only
  works as a header, and Chromium logs a console error when it appears in a `<meta>` tag.
- No inline scripts or inline event-handler attributes. Inline `style` is allowed.
- Overlay pages (command, find, permission, agent, toast, switcher, peek) add `class="surface-overlay"`
  to `<html>`: opaque `--surface` background. Inside sta the shell draws a rounded card around
  the page (corners, 1px `--border` edge, shadow: `docs/PROTOCOL.md` §4) and `ipc.js` adds
  `html.native-card`: the page stays square and borderless and tightens its paddings under
  `.native-card` (the card adds 12px above and below or 8–16px beside the page). In mock mode
  (no `native-card`, or preview it with `?nativeCard`) `base.css` draws the 1px edge instead.
  A browser view can't be transparent, so never draw rounded corners or shadows on the page's
  outer edge yourself.
- Render page-provided strings (titles, URLs, hosts, favicons) only as text nodes or attributes.
  Never use `innerHTML` or `dangerouslySetInnerHTML`.

```js
// sidebar.js
import { html, render } from '/common/vendor/htm-preact.js';
import { dispatch, startSurface } from '/common/ipc.js';

function Sidebar({ state }) { /* … */ }

startSurface({ render: (state) => render(html`<${Sidebar} state=${state} />`, document.body) });
```

---

## ipc.js

Implements `docs/PROTOCOL.md` §1. On load it opens the single persistent
`{cmd: "__subscribe"}` query. Every query passes both `onSuccess` and `onFailure`, because the
cef-rs router silently drops queries that omit one.

| export | signature | notes |
|---|---|---|
| `invoke` | `(cmd: string, payload?: any) → Promise<any>` | Response `""` → `null`. A failure rejects with an `Error` that has `.code`. A `state.get` result also goes through the revision filter and notifies `state` listeners; it resolves to the newest known state (a push that raced the request may be newer than the fetched snapshot). |
| `dispatch` | `(command: {type, …}) → Promise<null>` | Same as `invoke('dispatch', command)`. Rejects locally when `type` is missing. |
| `on` | `(event, cb) → unsubscribe` | Events: `'state'`, `'find.result'`. |
| `off` | `(event, cb)` | |
| `getState` | `() → UiState \| null` | Last accepted snapshot. |
| `onState` | `(cb) → unsubscribe` | Like `on('state')`, and also calls `cb` right away if a snapshot is already known. |
| `refreshState` | `() → Promise<UiState>` | Runs `state.get`. |
| `ready` | `() → Promise<null>` | Sends `ui.ready` once per page load; later calls return the same promise. If it fails, the next call sends it again. |
| `setSurfaceSize` | `({height, width?}) → Promise<null>` | `surface.setSize`, rounded up (DIP). Identical consecutive sizes are sent only once, unless the previous request failed. |
| `trackSurfaceSize` | `(element, {width?: boolean}) → stop` | ResizeObserver → `setSurfaceSize` with the element's **layout** box (`borderBoxSize`, never `getBoundingClientRect`, which includes transforms). It keeps working while the overlay is hidden. Never transform a tracked root: the shell keeps the size it is told (see [Motion](#motion-motionjs-and-motion-catalogjs)). |
| `onSurfaceExit` | `((payload) => void\|Promise<void>) → unsubscribe` | Handles `surface.exit {gen}`: the shell is about to hide this overlay and waits for a blank frame first (the toast and the switcher). The callback blanks what is on screen (`motion.js blank`) and the ack follows one frame later. |
| `ackSurfaceExit` | `(gen, whenBlank) → Promise<void>` | Sends `surface.exited {gen}` one frame after `whenBlank` resolves — for a surface asked through another event (the floating sidebar's `sidebar.hover {gen}`). |
| `startSurface` | `({render, theme = true, ready = true}) → Promise<stop>` | Runs `state.get` → `applyTheme(state)` + `applyMotion(state)` → `render(state)` → `ui.ready`. Then re-renders on every newer snapshot. `ui.ready` goes out after the first render that doesn't throw (possibly a later snapshot), and is retried after the next successful render if it failed. |
| `isMock` | `boolean` | |
| `ipcAvailable` | `boolean` | `false` inside sta when `__staQuery` is missing. |

- **Revision ordering:** a snapshot with a `revision` lower than the current one is dropped before
  any listener runs. A push that races `state.get` is therefore harmless.
- **Missing IPC:** a page loaded from `sta://` without `__staQuery` logs a
  `console.error`, and every request rejects with `code: 'unavailable'`. It never falls back to
  mock data.
- **Resubscribe:** if the event stream fails, the client resubscribes with backoff (250 ms,
  doubling) and refetches state each time. After 5 consecutive failures it gives up. The budget
  starts over once a resubscribed stream delivers an event, or once the refetch succeeds while the
  new stream is still open.
- **No rAF before `ui.ready`:** don't wait on `requestAnimationFrame` before sending it. Hidden
  overlay views don't run frames, and the shell only shows them after `ui.ready`.
- **Automation:** `window.sta = {invoke, dispatch, on, off, getState, onState, refreshState, isMock}` (frozen).

### ipc-hooks.js

- `useUiState() → UiState | null`: re-renders on every accepted snapshot.
- `useRequest(cmd, payload = null, deps = []) → {data, error, loading, reload}`: re-runs when
  `cmd`, the JSON of `payload`, or `deps` change, and ignores stale responses. For example,
  `useRequest('history.list', {query, limit: 200}, [state.historyRevision])`.

---

## Mock mode

Mock mode is used **only** when `location.protocol !== 'sta:'` or the URL has `?mock`.
`ipc.js` then imports `mock.js`, whose `mockQuery` has exactly the `__staQuery` signature.
JSON strings go in and out, and `__subscribe` pushes `{event, payload}`, so the real client code
runs unchanged.

```bash
node tools/ui-serve.mjs            # --port 8123 (default) --host 127.0.0.1 --root ui --quiet
# http://127.0.0.1:8123/sidebar/          (mock implied)
# http://127.0.0.1:8123/_gallery/?dark=1
```

`ui-serve.mjs` sends correct MIME types (`text/javascript` for modules; Python's `http.server`
takes them from the Windows registry, which may say `text/plain` and leave every page blank),
`Cache-Control: no-store` and `nosniff`, redirects a directory without a trailing slash, serves
`index.html` for directories and never serves files outside the root. It can also be imported:
`startStaticServer({root, host, port = 0, onResponse}) → {origin, port, close()}`.

### Query parameters

| param | effect |
|---|---|
| `mock` | Forces mock mode (also inside `sta://`). |
| `nativeCard` | Adds `html.native-card`: the page as it looks inside the shell's rounded overlay card (no own edge, tighter paddings). |
| `fixture=<name>` | Base UiState file in `common/fixtures/` (default `uiState`, or `uiStateDark` with `dark=1`). |
| `dark=1` / `dark=0` | Dark or light colors. Spaces and presets are recolored from the dark/light fixtures. |
| `space=<id>` | Active space (fixture ids: 1 Personal, 8 Play, 15 Work; Work is active by default). |
| `empty=1` | No active item in the active space: `current = null`, empty state. |
| `sidebar=0` | Sidebar hidden. |
| `hover=1` | With `sidebar=0`: the floating sidebar is shown (pushes `sidebar.hover {visible: true}`). |
| `width=<px>` | Sidebar width (clamped to 200–440). |
| `maximized=1` | `window.maximized = true`. |
| `commandBar=newTab\|editUrl\|split\|actions\|extensions` | Opens the command bar. `text=` prefills it (editUrl defaults to the current URL); `extensions` is the Ctrl+E picker. |
| `panel=downloads\|appMenu\|newSpace\|editSpace[:id]\|renameItem[:id]\|editPinned[:id]` | `sidebarPanel`. With `sidebar=0`, transient panels (downloads, app menu) float and the others dock the sidebar, like core. |
| `find=1` | Find bar for the focused tab. `text=` sets the query. |
| `toast=<message>` | Toast (`toast=1` → "Cleared 4 tabs"). `toastAction=<label>` adds a `reopenClosed` action. |
| `permission=camera,microphone` | One permission prompt (`permission=1` → camera + microphone). |
| `switcher=1` | Ctrl+Tab switcher with the loaded tabs. |
| `peek=1` / `peek=popup` | Peek open; `popup` makes it a feature popup. |
| `extPopup=<id>` / `extPopup=1` / `extPopup=failed` | The extension popup card's header (`sta://extension/`): a given extension, the first one with a popup, or the honest-failure line. |
| `extensions=none\|safeMode` | Extensions list: empty, or the safe-mode banner in Settings › Extensions. |
| `agent=connection\|unverified\|site\|tab\|panel\|busy\|paused` | AI agent state (`mock-agent.js`): a verified connection prompt; an unsigned one with a second queued; a site prompt; a tab access prompt; the activity panel with a held download; two agents acting (chip pulse); paused with the panel open. Implies `agentAccess=full`; marks the first Today tabs as agent tabs. |
| `agentAccess=off\|readOnly\|full` | `settings.agentAccess`. |
| `motion=full\|reduced\|off` | Motion level: `off` turns the master switch off, `reduced` follows Windows with its animation effects off, `full` stops following Windows. |
| `animOff=<key>,<key>` | Turns individual animation keys off (`overlays.toast`, `sidebar.reorder`, …). |
| `systemAnimations=0` | Windows has animation effects switched off (`UiState.motion.systemAnimations`). |
| `latency=<ms>` | Delays every response and push. |
| `quiet=1` | No console logging of requests. |

### Behaviour

- **Requests:**
  - `state.get`
  - `ui.ready`
  - `dispatch`, validated like the shell's serde parse (`COMMAND_FIELDS` / `validateCommand` in
    `mock-reducers.js`, derived from `command.rs`):
    - 400 for a malformed command, an unknown `type` (a key of `COMMAND_FIELDS`; own keys only:
      `constructor`, `__proto__` etc. are unknown), or a required field that is missing or has
      the wrong JSON type (ids are non-negative integers, enums must be one of their values,
      `to`/`panel`/`command` are checked structurally). Optional fields (`Option` /
      `#[serde(default)]`) may be omitted and aren't checked; extra fields are ignored.
    - A valid command the mock doesn't simulate (no reducer) is accepted like the shell accepts
      it; applying it only logs a `console.warn`.
    - 403 for shell-only commands such as `tick`, also when wrapped in `commitOmnibox`, and for a
      `commitOmnibox` inside a `commitOmnibox`.
    - `node tools/check-mock-commands.mjs` keeps this honest. It parses `command.rs` (every
      `Command` variant, its non-default fields and their types, the enums, `SidebarPanel`,
      `Container` and the shell-event list of `allowed_from_ui`) and fails (exit 1) when
      `COMMAND_FIELDS` / `SHELL_ONLY_COMMANDS` disagree: missing or extra commands, a required field
      missing, listed while optional, or of the wrong kind (enum value lists included). It also
      runs `validateCommand` / `allowedFromUi` on a valid sample of every command, panel and
      container, and without each required field. Commands without a reducer are warnings. Run it
      after changing `command.rs`.
  - `omnibox.query` (commands and hints per mode as core builds them in `store/omni.rs`; the
    "focused" tab is core's content-focused tab, Peek ignored):
    - empty text → `omniboxEmpty.json`: recent tabs (not the focused one), then suggested actions
      (editUrl adds "Copy URL" first). Split mode lists only the recent tabs, as
      `splitWith {tab, with: focused, side}` with hint "Split";
    - `mode: 'actions'` or a `>` prefix → the actions of `omniboxActions.json`, filtered, with the
      per-space rows ("Go to Space: X", "Move Tab to Space: X") rebuilt from the live state;
    - otherwise a synthesized Go/Search row (`openInput` newTab / currentTab in editUrl /
      `splitOpenInput {text, side}` in split; `altCommand` is `openInput` backgroundTab in every
      mode), then substring matches over live tabs (not the focused one), actions (without "Go to
      Space", which core lists in actions mode only), `spaces` rows for the other spaces (title =
      name, hint "Go to Space"), fixture history/archive, and a Suggestions row (hint "Search")
      for each of the request's `suggestions` (trimmed, without empties, case-insensitive
      duplicates, the typed text and the completed suggestion), capped per group and at 12
      results. Suggestion rows search with `openUrl` (newTab; currentTab in editUrl;
      `splitOpenInput {text: '?query'}` in split; `altCommand` `openUrl` backgroundTab). Split
      mode lists tabs (as `splitWith`, hint "Split"), history (as `splitOpenInput {text: url}`)
      and suggestions only; editUrl turns history rows into `openUrl` currentTab;
    - inline completion from tab/history hosts, like core: only when the text has no surrounding
      or inner whitespace, isn't a `?` search or `scheme://` URL, and `preventInlineAutocomplete`
      is false. The typed part keeps its case (`GitHu` → `GitHub.com`);
    - without a host completion, inline completion from `suggestions`, like core: the first
      suggestion that extends `text` as typed (case-insensitive per character, whitespace allowed,
      not an address such as `naver.com`), unless `preventInlineAutocomplete`, a `?` prefix or
      URL-like text. `inlineCompletion` is `text` plus the suggestion's remainder (`Ru` →
      `Rust programming language`), and `results[0]` (key `search`, "Search Google") searches it;
    - `seq` is echoed.
  - `omnibox.suggest {text}` → `{text, suggestions}` after about 60 ms (`__mock.setSuggestDelay`),
    like the shell: 403 unless called from `/command/`, 400 without a string `text`; one request
    at a time (a newer one resolves the pending one with `[]`); `[]` when
    `settings.searchSuggestions` is off, the engine is Kagi, Perplexity or Custom, the text is
    blank, over 256 characters, or an address with an explicit scheme (`https:`, `mailto:`, any
    `scheme://`) or a Windows path (a leading `?` is stripped and skips that check). Otherwise
    deterministic fake suggestions: the canned ones starting with the query (`ru`/`rust` →
    "rust programming language", "rust tutorial", "rust vs go"; `git` → "github copilot", …),
    else `<text> tutorial`, `<text> meaning`, `<text> 뜻`.
  - `omnibox.actions`: every action of `omniboxActions.json`, with the per-space rows rebuilt from
    the live state, sorted A–Z (the same list actions mode filters)
  - `archive.list`
  - `history.list {query, limit}`
  - `boosts.get {id}`: `boost.json`, a stub built from the summary, or `null`
  - `theme.colors {theme}`
  - `surface.setSize`: recorded in `window.__mockSurfaceSize`
  - `sidebar.setWidth`
  - `surface.exited`: the generation is recorded in `window.__mockExited`
  - `sidebar.hoverLock`
  - `dialog.pickFolder`
  - `app.info`: `appInfo.json`
  - Unknown requests (own keys only) fail with code -1.
- **Timestamps:** fixture times are rebased so that 2026‑09‑16T12:00Z maps to "now".
- **Commands:** each command is logged with `console.info`, applied by `mock-reducers.js`, and
  followed by one coalesced `state` push with `revision + 1`.
  - Simulated: `openInput`, `openUrl`, `openUrlAt`, `navigate`, `goBack`, `goForward`, `reload`,
    `stopLoad`, `activateItem`, `activateNth`, `activateAdjacent`, `closeItem`, `reopenClosed`,
    `togglePin`, `addFavorite`, `removeFavorite`, `resetToPinned`, `replacePinnedUrl`,
    `editPinned`, `renameItem`,
    `duplicateTab`, `moveItem`, `moveToSpace`, `clearToday`, `newFolder`, `toggleFolder`,
    `deleteFolder`, `unloadTab`, `toggleMute`, `copyUrl`, `copyText`, `newSpace`, `updateSpace`,
    `deleteSpace`, `switchSpace`, `switchSpaceNth`, `switchSpaceAdjacent`, `moveSpace`,
    `splitWith`, `splitOpenInput`, `focusPane`, `focusPaneAdjacent`, `setSplitFractions`,
    `separatePane`, `separateAll`, `restoreArchived`, `deleteArchived`, `clearArchive`,
    `deleteHistoryEntry`, `clearHistory`, `closePeek`, `expandPeek`, `openCommandBar`,
    `closeCommandBar` (a stale `seq` is ignored, like core), `commitOmnibox` (applies the inner command, then closes the bar unless
    `alt` or the command opens a bar/panel), `toggleSidebar`, `setSidebarWidth`,
    `openSidebarPanel`, `toggleSidebarPanel`, `closeSidebarPanel`, `openInternalPage`,
    `openFind`, `closeFind`, `findInPage` and `findNext` (emit `find.result`), `zoom`,
    `windowControl`, `dismissToast`, `updateSettings` (`appearance` recolors), `upsertBoost`,
    `deleteBoost`, `toggleBoost`, `resolvePermission`, `mruStep`, `mruSelect`, `mruCommit`,
    `mruCancel`, `downloadControl`, `downloadDismiss`.
  - `updateSettings` applies an `animations` patch the way core does (`motion-catalog.js`
    `applyAnimationsPatch`) and recomputes `state.motion`, so the Settings › Animations switches and
    the omnibox action work in mock mode.
  - Accepted as no-ops: `toggleDevTools`, `print`, `viewSource`, `newBoostForSite`, `quit`, and
    any valid command without a reducer (with a `console.warn`).
  - Activating an unloaded tab sets `loading` for about 700 ms.
  - Opening a sidebar panel while the sidebar is hidden (also `sidebar=0&panel=…`) reveals it
    only while a panel is open, like core: panels that hold input (space sheets, rename, edit
    pinned page) dock it (`window.sidebarVisible` is `true` meanwhile), transient ones (downloads,
    app menu) float (`sidebarVisible` stays `false`); either way it is hidden again when the panel
    closes. `toggleSidebar` while revealed docks it for real and keeps the panel open.
  - `mruStep` opens the switcher like core: forward selects the second card, or the first when
    the first card isn't the focused tab (empty state, Peek open); backward selects the last card.
  - The mock is only plausible. Core (`command.rs`) defines the real semantics.
- **Test hooks:**
  - `window.__mockLog`: every request as `{time, cmd, payload}`.
  - `window.__mock.state`: the live state.
  - `__mock.setState(fn)`: mutate the state, then push.
  - `__mock.emit(event, payload)`
  - `__mock.dispatched()`: the dispatched commands.
  - `__mock.reset()`: back to the fixture; the revision stays monotonic.
  - `__mock.setSuggestDelay(ms)`: delay of `omnibox.suggest` replies (default 60).
  - `window.__mockReady`: set once `ui.ready` arrives.
  - `window.__commandBar` (command bar page only): `model`, `suggest` (settings, LRU `cache`,
    `requests` / `stale` counters), `onSuggestReply(text, reply)`, `toggleActions()`,
    `suggestionsFor(text)`, `applyResponse`, `rerender`.

### Fixtures

All fixtures except `appInfo.json` (shell data, hand-written) are generated from the real core
types by driving a `Store` through commands:

```bash
cargo test -p sta-core -- --ignored gen_fixtures   # writes ui/common/fixtures/*.json
```

The generator is `crates/sta-core/tests/gen_fixtures.rs`; extend it there (never edit the
generated JSON by hand). Its timestamps are relative to 2026-09-16T12:00Z, which `mock.js` rebases
to "now".

`uiState.json` / `uiStateDark.json` hold the same content with light and dark colors:
- 3 spaces with themes: Personal (1), Play (8), Work (15, active);
- 6 favorites (3 unloaded, 2 with remote favicons);
- Work space pinned items: a folder with a nested folder, a navigated pinned tab, an unloaded one;
- Work space Today items: an active split (focused second pane), an audible tab, a muted tab, a
  loading tab, a failed tab (load error), a crashed tab and unloaded tabs;
- 6 downloads covering every state (in progress, paused, complete, cancelled, interrupted),
  one in progress with unknown size;
- the command bar open in New Tab mode, the current tab, a boost for github.com;
- 8 theme presets.

The other fixtures:
- `omnibox.json`: the response to `git` with remote suggestions (inline completion
  `github.com`, Go, tabs, history, suggestions and archive rows);
- `omniboxEmpty.json`: the empty New Tab query (recent tabs, suggested actions);
- `omniboxActions.json`: every action (`Store::omnibox_actions`, as `results`);
- `archive.json` (auto-archived, closed, and a split group), `history.json` (typed and link
  visits), `boost.json`, `appInfo.json`.

Every field matches the Rust serde shapes:
- camelCase names;
- every `Option` is present as `null`;
- tags are `"kind"` for `NodeView` and `"type"` for `Command`, `ResultIcon`, `SidebarPanel` and
  `Container`.

Favicons are either `null` (letter tiles) or a few remote URLs; screenshots don't depend on them
loading.

---

## Theme: theme.js and tokens.css

- `applyTheme(state, target = <html>)`:
  - sets `data-theme="light|dark"` and `color-scheme`;
  - applies the active space's `ThemeColors`;
  - skips the work when nothing changed.
- `applyMotion(state, target = <html>)`:
  - sets `data-motion="full|reduced|off"` and `data-anim-off="<key> <key> …"` from `UiState.motion`
    (docs/PROTOCOL.md §14);
  - skips the work when nothing changed; `startSurface` calls it beside `applyTheme`.
- `applyColors(colors, target)`: sets the variables from any `ThemeColors`.
- `themeStyle(colors) → style object`: for scoped previews such as swatches or the space sheet.
  Put `class="theme-scope"` and `data-theme` on the same element, so derived tokens and text
  color are recomputed inside the scope.
- `activeSpaceOf(state)`
- `THEME_VARS`: the field → property map.

**ThemeColors mapping** (PROTOCOL §4):

| ThemeColors field | CSS variable |
|---|---|
| `frame` | `--frame` |
| `gradientStart` | `--grad-start` |
| `gradientEnd` | `--grad-end` |
| `accent` | `--accent` |
| `text` | `--text` |
| `textMuted` | `--text-muted` |
| `hover` | `--hover` |
| `pressed` | `--pressed` |
| `activeRow` | `--active-row` |
| `divider` | `--divider` |
| `surface` | `--surface` |
| `border` | `--border` |

`tokens.css` also defines:
- **Fallbacks** for the theme variables (light, plus `[data-theme=dark]`), used until the first
  state arrives.
- **Derived colors:** `--accent-soft` (12%), `--accent-softer`, `--accent-hover`, `--on-accent`,
  `--text-faint`, `--field-bg`, `--field-bg-hover`, `--field-border`, `--chip-bg`, `--track`,
  `--scrollbar-thumb`, `--active-row-shadow`, `--shadow-menu`, `--shadow-drag`, `--focus-ring`,
  `--danger` (`#c42b1c`, the caption close hover), `--danger-soft`, `--danger-text`,
  `--warning`, `--success`.
- **Typography:** `--font-ui`, `--font-display`, `--font-small`, `--font-mono`, `--font-emoji`,
  `--fs-*`, `--fw-*`, `--lh-*`.
- **Geometry** (arc_spec §5):
  - rows: `--row-h`, `--row-radius`, `--row-pad-x`, `--row-gap`, `--sidebar-pad-x`;
  - chrome: `--topbar-h`, `--url-pill-h`, `--url-pill-radius`, `--fav-tile-h`, `--fav-gap`,
    `--fav-radius`, `--fav-icon`, `--bottom-bar-h`, `--space-icon`, `--caption-btn-w`,
    `--split-gutter`;
  - command bar: `--cmd-*`;
  - radii: `--radius-xs` 4, `--radius-sm` 6, `--radius-md` 8, `--radius-lg` 12, `--radius-xl` 16,
    `--radius-full` (pills, tracks, thumbs); every rounded corner uses one of them (or 50% for
    circles, or 25% of the size for favicon tiles and tiny CSS-drawn glyphs such as the agent
    chip's stop square), picked by the element's size (chips and keys xs, small buttons sm, rows,
    buttons and inputs md, cards, tiles, menus and popovers lg, sheets and big cards xl). The native
    chrome's values are mirrored as `--content-radius`, `--content-ring`, `--overlay-radius`,
    `--overlay-shadow`, `--find-radius`, `--toast-radius`, `--peek-page-radius`;
  - generic: `--control-h`, `--icon-btn`, `--space-1…5`, `--z-*`.
- **Motion:** `--ease-out`, `--ease-in-out`, `--ease-spring` (a `linear()` spring),
  `--motion-distance` (a travel multiplier: 1 normally, 0 at the `reduced` level),
  `--t-surface-exit`, and one `--t-*` duration **per animation key** (36 of them, named from the key:
  `sidebar.tabInsertRemove` → `--t-sidebar-tab-insert-remove`). There are no shared durations left;
  the one key with two of them is `controls.hoverPress`, because a hover tint and a press are not the
  same length (`--t-controls-hover-press` and the shorter `--t-controls-press`), and its own gate
  zeroes both.
  - `prefers-reduced-motion` applies only **before the first state** (`:root:not([data-motion])`):
    once core has spoken about motion, core decides.
  - See [Motion](#motion-motionjs-and-motion-catalogjs) for what the levels do.
- **Cross-fade:** the theme variables are registered with `@property` as `<color>`, and
  `theme.js applyMotion` adds `class="theme-fade"` to `<html>` for `theme.crossFade` — but only on
  pages that paint their own background, never inside a native card, where the shell's fill and border
  would snap while the page faded. A card page gets `class="theme-snap"` instead: zero duration, half
  the fade of delay, so its colors change in the same frame as the native card around it (the shell
  delays `SetChrome` by `--t-theme-cross-fade / 2`, `crates/sta/src/motion.rs chrome_delay_ms`).
  Remember that the native frame color does not change instantly: it snaps at the fade's midpoint.

## Motion: motion.js and motion-catalog.js

Every animation has a **key** from the registry in `crates/sta-core/src/motion.rs` (36 keys in 8
groups). `UiState.motion` says which are on and how much motion is allowed at all; the rules and the
reasoning are in `docs/ARCHITECTURE.md` §4.7, the wire format in `docs/PROTOCOL.md` §14.

- **`theme.js applyMotion(state)`** puts `data-motion="full|reduced|off"` and
  `data-anim-off="<key> …"` on `<html>` (`startSurface` does it for you).
- **`tokens.css`** turns those into behaviour: `reduced` sets `--motion-distance: 0`; `off` zeroes
  `animation-duration` and `transition-duration` (`!important`) on `:root[data-motion="off"]` and its
  `*`, `*::before` and `*::after` — durations only, so delays and end states survive — and gives the
  indicators explicit static fallbacks; a key that is off gets **its own** duration token zeroed.
- **`motion.js`** is the only place a page starts an animation:

| export | notes |
|---|---|
| `enabled(key)` | The level allows finite motion, the key is not off, and the surface is on screen: a surface that declares its presence (`usePresence`) decides, and may animate while its view is still hidden — but only while the window is in front. Everything else follows `visibilityState`. Ask before **every** animation. |
| `level()`, `offKeys()` | Read back from the two attributes. |
| `distance(px)` | `px` at `full`, `0` at `reduced`: multiply every travel by it and a fade is what is left. |
| `duration(key, fallback)`, `tokenOf(key)` | The key's duration from its own CSS token, so CSS and WAAPI can never disagree. |
| `animate(el, key, frames, opts)` | `el.animate` with `id = key`: a second animation for the same key on the same element **replaces** the first. Returns `null` when the key may not animate. |
| `stagger(els, key, frames, opts)` | The same keyframes offset in time, total ≤ 150 ms (≤ 200 ms for a page's own enter). Fills **backwards**, so an element waiting for its delay holds its first keyframe instead of painting at its base style and blinking out. |
| `flip.capture(root, key, sel, {changed})` → `play(opts)` | Measures **before** cancelling a running FLIP, matches rows by `data-flip`/`data-id`, animates the individual `translate` property. Refuses while a drag is on, for `{pointer: true}` (a close made with the pointer in the list), for more than 8 **changed ids** — pass `changed`: what makes a change bulk is the ids it changes, not the rows that moved, or the Undo of Clear Today slides its one survivor the length of the list — for more than 8 moved rows, for rows that end up outside the box it measured, and for moves longer than that box. It asks `moves(key)`, so it snaps at `reduced` like the gliders. |
| `ghost(el, {slot}?)`, `fadeGhost(clone, key, frames, opts)`, `clearGhosts()` | An inert clone with no `id`, `data-*`, `role`, `aria-*`, `tabindex` or `title`, `aria-hidden` and `inert`, in a fixed `.motion-ghosts` layer at body level. Dropped on `sta:dismiss`, on leaving presentation, and when motion is turned off. `slot` keeps at most one alive (the space switch's pane ghost clones a whole list). |
| `finishAll(key?)` | Finishes our animations (and, with no key, the page's finite CSS animations), never an infinite indicator. |
| `blank(targets, key)`, `unblank()`, `isBlanked()` | An acknowledged exit's own half (PROTOCOL §14): fade the elements that *paint* out over `exitMs(key)` and leave them invisible — the end state is written to the elements, never left to a `fill` that settling could drop, and they stop taking pointer events (the widget is still up for the length of the wait). `unblank()` brings them back when the surface has something to show again. Never pass a tracked overlay root. |
| `closeBlank(targets)` | The other half of the rule, for an **activatable** overlay (command bar, find bar, permission prompt, Peek): nothing waits for these, so a page-initiated close blanks what paints *in the same task as the key or click* — no fade — and resolves one frame later (raced against 32 ms, because a hidden page never gets a frame). Dispatch after it, and `unblank()` on the next open. |
| `exitMs(key)`, `surfaceExitMs()`, `settle(key)` | The exit fade's length: the key's own duration capped at `--t-surface-exit` (60 ms), and 0 when it may not animate. `settle(key)` waits that long from the next frame, for a surface whose blanking is a CSS transition (the floating sidebar). |
| `viewTransition(key, update)` | A View Transition at `full`, else the plain update. `update` may wait for the framework's own render (one microtask) and **nothing longer**: the page is frozen on the old snapshot until it resolves, so fetch first, then start the transition. |
| `moves(key)` | `enabled(key)` **and** the level is `full`. For motion that animates a *position* (a glider, a ring following a selection): travelling 0 px would leave it in the wrong place, so it snaps at `reduced` instead. |
| `scrollBehavior()` | `controls.smoothScroll` as a `ScrollOptions.behavior`: `'smooth'` only at `full` with the key on. |
| `glide(el, key, {x, y}, opts)` | Moves a positioned element by writing `style.translate` **first** — so a refused glide still lands in the right place — and animates the travel from wherever it was. |
| `pop(el, key, opts)` | A short scale-out-and-back for a value that changed (a counter, a badge); nothing at `reduced`. |
| `usePresence(present)` | **Declare it first** in the component: a layout effect that tells the runtime whether the surface is on screen. Leaving presentation finishes everything pending and drops every ghost. |
| `useExitGhost(key, find, frames, opts)` | Leaves an inert ghost behind when a component unmounts. Preact runs hook cleanups **before** it removes any DOM, so `find()` still returns a live, measurable element — a component that renders into a portal has to *wrap* what it ghosts. |
| `setPresented(on)`, `isPresented()` | The same, for code with no component (an overlay's `onState`). |
| `stats()` | Counters for `tools/motion-check.mjs` and the in-app checks; also on `window.__motion`. |

**Never** trigger an animation from a render, a visibility event or `animationend` (it does not fire
while a surface renders no frames). Trigger from keyed diffs: an id, an order signature, a `seq`, a
`toast.id`, a space id.

`motion-catalog.js` is the UI half of the registry — the same groups and keys in the same order, each
with `defaultOn`, a label and a one-line description — plus the pure helpers the Settings page and the
mock backend share (`animationSettings`, `groupOn`, `keyOwnValue`, `motionLevel`, `motionOffKeys`,
`applyAnimationsPatch`, `motionView`, `isDefaultAnimations`). `node tools/check-motion.mjs` fails when
it, `motion.rs` and `tokens.css` drift apart.

**Where the keyed diffs live.** A surface computes its signatures **in its component body**, which
Preact runs before it patches the DOM: that is the only moment FLIP can measure the "first" rects and
an element that is about to unmount can still be cloned. `sidebar/sidebar.js` does exactly that —
`listOrder`, `collapsedFolders`, the favorite ids and a `layoutSignature` of everything around the
lists whose height moving would move every row — and turns the difference into (a) which key runs,
(b) the FLIP captures to play in the following layout effect, and (c) the *exit plan*
`sidebar/rows.js setRowExitMode` reads: whether a row that unmounts leaves a ghost, under which key,
how many this commit may leave and how far their delays may spread. `motion.js useExitGhost` is the
same idea for a whole panel (a menu, a popover, a space sheet): Preact runs hook cleanups **before** it
removes the DOM node (`options.unmount` in the vendored bundle), so the cleanup still sees a live,
measurable element. Only the **first** ghost of an element is taken, so a panel that two components
want to see leave — the sidebar's own `sidebar.panels`, the shared `menus.popIn` — is cloned once, by
the outermost of them.

**The shared pieces.** `internal-page.js` owns the internal pages' four keys, because settings,
archive, history and boosts share all of them: `mountPage` wraps the page in `pages.enter`,
`useListRowMotion(ref, ids)` is `pages.listRows` (call it in the **body**, with the ids that render is
about to show), `useNavIndicator(listRef, barRef, sel)` places and glides a `.ip-nav-indicator`, and
`Disclosure` animates a height — the one layout animation the rules allow, and only here.
`components.js` owns `menus.popIn` (the pop-in origin `placeFloating` resolves, and the exit ghosts of
`Menu` and `Popover`), the `controls.*` transitions in `base.css`, `AudioBars` (`indicators.audio`) and
`useCountPop(ref, value)` (`indicators.badges`).

## base.css classes

- **Surfaces:** `html.surface-overlay`, `.drag` / `.no-drag` (buttons, inputs and links inside
  `.drag` are already no-drag).
- **Focus:** `:focus-visible` shows a 2px accent ring; `.focus-inset` puts the ring inside, for
  rows.
- **Scrolling:** `.scroll-thin` gives a 6px overlay-style scrollbar that appears on hover and
  reserves its gutter. The default page scrollbars are thin as well.
- **Utilities:** `.row`, `.stack` (8px gap; override with an inline `gap`), `.grow`, `.ellipsis`,
  `.muted`, `.faint`, `.mono`, `.emoji`, `.divider`, `.sr-only`, `.selectable`. The body is
  `user-select: none`; fields and `.selectable` opt back in.
- **Component classes:** `.btn` (`.btn-primary`, `.btn-ghost`, `.btn-danger`, `.btn-sm`),
  `.icon-btn` (`.is-sm`, `.is-lg`, `.is-muted`, `[aria-pressed]`), `.input`, `.select`, `.field`,
  `.field-label`, `.field-hint`, `.field-error`, `.checkbox`, `.toggle`, `.toggle-row`, `.kbd`,
  `.kbd-group`, `.chip`, `.menu`, `.menu-item` (`.is-active`, `.is-danger`), `.menu-sep`,
  `.menu-header`, `.menu-hint`, `.popover`, `.favicon`, `.favicon-tile`, `.spinner`, `.progress`,
  `.progress-ring`, `.emoji-picker`.

## icons.js

```js
import { Icon, ICON_NAMES, hasIcon, createIconElement, GLYPHS } from '/common/icons.js';
html`<${Icon} name="lock" size=${14} label="Secure" />`   // role="img"; omit label → aria-hidden
```

- Props: `name`, `size = 16`, `label`, `strokeWidth = 1.5`, `class`, `style`.
- Drawings are original, on a 24 grid, with 1.5 strokes in `currentColor` and round caps and
  joins.
- An unknown name warns once and renders an empty SVG.
- `createIconElement(name, {size, label})` builds the same SVG with plain DOM calls, without
  `innerHTML`.

Glyphs: `archive arrow-down arrow-up back bell boost camera case check chevron-down chevron-left
chevron-right chevron-up clipboard close close-window code copy download drag-handle edit emoji
external find folder folder-open folder-plus forward globe history home info link location lock
maximize menu mic minimize minus moon more palette pause pin play plus print quit reload restore
restore-window screen search settings sidebar space speaker speaker-muted split star stop sun
trash undo warning zoom zoom-out`.

All `ResultIcon::Glyph` names in `omnibox.rs` exist. Use `minimize`, `maximize`,
`restore-window` and `close-window` for the 46×40 caption buttons.

## components.js

Every component is a Preact function component. All of them are accessible: they set roles,
labels, `aria-*` state and support the keyboard.

| component | props |
|---|---|
| `Button` | `variant='default'\|'primary'\|'ghost'\|'danger'`, `size='md'\|'sm'`, `icon`, `iconEnd`, `buttonRef`, plus any `<button>` attributes |
| `IconButton` | `icon`, `label` (required: `aria-label` and tooltip; `title=${null}` hides the tooltip), `size='sm'\|'md'\|'lg'`, `iconSize`, `pressed`, `muted`, `buttonRef` |
| `Kbd` | `keys="Ctrl+Shift+K"` or `['Alt','1…9']`; `Ctrl++` renders "Ctrl" and "+" |
| `Favicon` | `src`, `host`, `size=16`, `dim`, `lazy=true`, `title` |
| `Spinner` | `size=14`, `label='Loading'` (`null` = decorative) |
| `ProgressBar` | `value` (0–1, `null` = indeterminate), `height=4`, `bare`, `label` |
| `ProgressRing` | `value`, `size=20`, `stroke=2`, `label`, `children` (centered) |
| `Menu` | `items`, `x`/`y` **or** `anchor` (element or rect), `placement`, `onClose(reason)`, `onSelect(item)`, `role='menu'\|'listbox'`, `label`, `initialIndex`, `autoFocus=true`, `minWidth`, `bounds`, `inline`, `restoreFocus`, `closeOnDismiss=true` |
| `Popover` | `open`, `anchor`, `placement='bottom-start'`, `offset=6`, `onClose(reason)`, `label`, `role='dialog'`, `autoFocus=true`, `inline`, `class`, `style`, `closeOnDismiss=true` |
| `Toggle` | `checked`, `onChange(bool)`, `label`, `description`, `ariaLabel`, `disabled` |
| `Checkbox` | `checked`, `onChange(bool)`, `label`, `disabled` |
| `Select` | `value`, `options=[{value,label,icon?,disabled?}]`, `onChange(value)`, `label` or `ariaLabel`, `placeholder`, `disabled` |
| `TextField` | `value`, `onInput(value)`, `onCommit(value)`, `onCancel()`, `label`, `hint`, `error`, `icon`, `clearable`, `autoFocus`, `selectOnFocus`, `multiline`, `inputRef`, plus any `<input>` attributes |
| `EmojiPicker` | `value`, `onSelect(icon)`, `label`, `columns=10`, `emoji=COMMON_EMOJI`, `showInput=true` |
| `Portal` | `children`: renders into a `<body>`-level host (context doesn't cross it) |

Component notes:

- **`Favicon`:**
  - Falls back to a letter tile colored with `oklch(0.7 0.08 hash(host))` when `src` is missing
    or fails to load. `dim` desaturates the icon for unloaded tabs.
  - `src` is only ever used as an attribute, and the image is not draggable
    (`draggable=${false}`).
  - A hidden overlay may not load lazy images until it is shown, so pass `lazy=${false}` there.
- **`ProgressRing`:** `value={null}` is indeterminate.
- **`Menu`** covers context menus, "…" menus and select lists.
  - Items: `{label, icon?, hint?, disabled?, danger?, checked?, selected?, onSelect?, submenu?}`,
    `{type:'separator'}` and `{type:'header', label}`.
  - Keyboard: ↑ ↓ Home End, → ← for submenus, Enter/Space, Esc (closes one submenu level), Tab,
    and type-ahead.
  - Hovering an item opens its submenu after 140 ms.
  - It closes on an outside pointerdown (the anchor element is ignored, so toggle buttons work),
    on window blur, on resize and on `dismissFloatingLayers()` (reason `dismiss`, unless
    `closeOnDismiss={false}`).
  - It is clamped to the viewport minus 8px and flips when there is no room. Submenus flip to the
    left.
  - Focus returns to the previously focused element when it closes.
  - Render it only while open, from state you clear in `onClose`.
  - `inline` renders a static copy for galleries and tests.
  - `placeFloating(el, rect, {placement, offset, bounds})` and `rectOf(anchor)` are exported for
    custom floating UI.
- **`Popover`:** closes on an outside pointerdown, Esc or `dismissFloatingLayers()` (unless
  `closeOnDismiss={false}`), focuses `[autofocus]` or the first focusable element, follows its
  anchor when the viewport resizes, and restores focus when it closes. Menus opened from inside it
  count as inside.
- **`dismissFloatingLayers()`** (and the `DISMISS_EVENT` document event it dispatches) closes every
  open `Menu` and `Popover` of the page with reason `dismiss`: for presses the page never sees, e.g.
  the floating sidebar's `sidebar.hover {dismiss}`. Menus and popovers that mirror core state (the
  app menu and downloads panels, edit pinned page) pass `closeOnDismiss={false}`.
- **`Select`:** a button plus a listbox `Menu`. ↑/↓ on the closed button change the value; Alt+↓,
  F4, Enter or Space open the list.
- **`TextField`:** Enter commits (Ctrl+Enter when `multiline`), Esc cancels, and `hint`/`error`
  are linked with `aria-describedby`.
- **`EmojiPicker`:** about 120 emoji plus a free-text field that accepts any emoji or up to two
  characters. Arrow keys move through the grid.

### hooks.js

- `useLatest(value)`
- `usePrevious(value)`
- `useStableId(prefix)`
- `useEventListener(target|ref, type, handler, {capture, passive})`
- `useOutsidePointerDown(refs, handler, enabled)`
- `useDebouncedValue(value, ms)`
- `useMediaQuery(query)`

### util.js

- **Formatting:**
  - `formatBytes(n)`: `12.3 MB`, binary steps, localized number;
  - `formatSpeed(bps)`;
  - `formatDuration(sec)`: `1h 5m`;
  - `describeDownload(d)`: `128 MB of 412 MB · 8.4 MB/s · 34s left` / `Paused · …` / `Failed · …`;
  - `downloadFraction(d)`: 0–1, or `null` when the size is unknown.
- **Time:**
  - `relativeTime(ms, {now, style})`: "just now", "5 minutes ago", "yesterday", then a date;
  - `formatDate`, `formatTime`, `dayLabel(ms)` ("Today", "Yesterday", "Monday, September 14"),
    `startOfDay`.
- **Hosts:** `hostLetterColor(host)`, `hostHue`, `hostLetter`, `hashString` (FNV-1a),
  `firstGraphemes(text, n)`, `hostOf(url)`.
- **Functions:** `clamp`, `debounce(fn, ms)` (`.cancel` / `.flush`), `throttle(fn, ms)`
  (leading and trailing, `.cancel`), `classNames(...)`, `deepEqual`. (For "one frame from now", use
  `motion.closeBlank()` or `motion.settle()`: a bare `requestAnimationFrame` never fires while a page
  is hidden, so IPC must never be gated on one.)
- **UiState:** `activeSpace(state)`, `walkNodes(nodes, visit)` (folders and split panes;
  `ctx = {depth, parent, split}`), `allTabs(state)`, `findItem(state, id)`.

**Boolean props:** Preact assigns a prop that exists on the element as a DOM *property*, so pass
booleans as booleans: `draggable=${false}`, `spellcheck=${false}`. The string `"false"` sets
`el.draggable = "false"`, which is `true`. (`aria-*` values are attributes and stay strings.)

## chrome.js and chrome.css

Browser-chrome pieces shared by the sidebar and the top bar (PROTOCOL §4/§5). Pages that use them
link `/common/chrome.css` after `base.css`.

```js
import { NavButtons, UrlPill } from '/common/chrome.js';
html`<${NavButtons} current=${state.current} size="sm" />`
html`<${UrlPill} current=${state.current} compact />`
```

- **`NavButtons({current, size = 'md'})`:** back / forward / reload-or-stop for `state.current`
  (`goBack`, `goForward`, `reload`, `stopLoad`). Disabled per `canGoBack`/`canGoForward`, all
  disabled without a current tab. Shift+click on reload sends `ignoreCache: true`.
- **`UrlPill({current, compact = false, class})`:** the URL pill:
  - leading glyph from the current tab: warning (load error), the internal page's glyph, folder
    (`file:`), lock (secure) or info (not secure); search glyph and "Search or enter URL…" with no
    tab;
  - `current.pill` as text, the full URL as tooltip; a click opens `openCommandBar` (`editUrl`,
    or `newTab` without a tab);
  - hover/focus shows the copy button (`copyUrl`, Alt+click → `markdown: true`, brief check mark)
    and, when `current.boosts` is non-empty, a boost button: one boost toggles directly
    (`toggleBoost`), several open a checkable `Menu`;
  - a 2px loading bar while `current.loading` that completes and fades ~200 ms after the load.
- `chrome.css` classes: `.nav-buttons`, `.url-pill` (`.is-compact`, `.is-empty`, `.is-engaged`),
  `.url-pill-main`, `.url-pill-glyph` (`.tone-accent`, `.tone-warning`, `.tone-muted`),
  `.url-pill-text`, `.url-pill-actions`, `.url-pill-boost` (`.is-on`), `.url-pill-copy`
  (`.is-done`), `.url-pill-progress` (`.is-done`), `.url-pill-progress-fill`. The pill is
  `app-region: no-drag`.

## internal-page.js and internal-page.css

Shared shell for the internal pages opened as tabs (settings, archive, history, boosts). They
link `/common/internal-page.css` after `base.css`; the document scrolls (not `<body>`) on a
`--surface` background, so sticky toolbars and window scroll events work.

| export | notes |
|---|---|
| `mountPage(App)` | `startSurface` rendering `<App state/>` into `#app` (or `<body>`) on every accepted state; logs a startup failure. |
| `PageHeader({icon, title, subtitle, children})` | Icon tile, `h1`, subtitle, actions on the right. |
| `SearchField({value, onInput, placeholder, label, autoFocus})` | `TextField` with a search glyph and clear button. Ctrl+F and `/` (outside fields) focus it; Esc clears it, then blurs. |
| `ConfirmButton({label, icon, variant, size, title, message, confirmLabel, onConfirm, disabled})` | Button that asks in an anchored `Popover` (Cancel is focused) before `onConfirm`. |
| `EmptyState({icon, title, children})` | Centered empty state (`role="status"`). |
| `Segmented({value, options, onChange, label})` | Radio group styled as a segmented control; arrows/Home/End move the selection. |
| `groupByDay(items, timeOf)` | `[{key, label, items}]` by local day, order kept (`dayLabel`: Today, Yesterday, dates). |
| `matchesQuery(query, ...fields)` | Every whitespace-separated token appears in one of the fields (case-insensitive). |
| `useListKeyboard(ref)` | ↑/↓/Home/End move focus between `[data-row]` elements inside `ref`. |
| `useStuck(ref)` | `true` while a sticky toolbar is stuck to the top of the scrolled page. |
| `classNames` | Re-exported from `util.js`. |

`internal-page.css` classes: layout `.ip` (max 860px); header `.ip-header`, `.ip-header-icon`,
`.ip-header-text`, `.ip-title`, `.ip-subtitle`, `.ip-header-actions`; sticky toolbar `.ip-toolbar`
(`.is-stuck`), `.ip-search`, `.ip-toolbar-meta`; lists `.ip-group`, `.ip-group-label`,
`.ip-group-count`, `.ip-list`, `.ip-row` (`.is-menu-open`), `.ip-row-icon`, `.ip-row-main`,
`.ip-row-title`, `.ip-row-sub`, `.ip-row-host`, `.ip-dot`, `.ip-row-meta`, `.ip-row-actions`
(shown on hover/focus), `.ip-badge` (`.is-accent`), `.ip-more`; `.ip-empty*`; settings cards
`.ip-card`, `.ip-setting` (`.is-stacked`), `.ip-setting-text`, `.ip-setting-label`,
`.ip-setting-desc`, `.ip-setting-control`; `.ip-segmented`, `.ip-segment`; `.ip-confirm*`;
`.mono-path`.

---

## Command bar (command/)

The overlay behind Ctrl+T / Ctrl+L (PROTOCOL §6). The input is uncontrolled; every edit sends
`omnibox.query` with an increasing `seq` and stale responses are dropped. Core builds every row
and command; the page renders them and commits `results[sel].command` with `commitOmnibox`.

| key | effect |
|---|---|
| Tab / Shift+Tab | Toggle actions mode on/off, keeping the typed text (without the inline completion). Off returns to the mode the bar opened in (New Tab, Edit URL, Split; New Tab for a bar opened in actions mode) and drops a `>` prefix. Focus stays in the input. Clicking the mode chip does the same. |
| ↑ / ↓, Ctrl+P / Ctrl+N, PageUp / PageDown | Move the selection. |
| → / End | Accept the inline completion. |
| Enter / Alt+Enter, click / middle-click | `commitOmnibox` with `command` / `altCommand` (background tab). |
| Backspace on an empty input | Leaves toggled actions mode. |
| Esc | `closeCommandBar`. |

- `>` at the start also means actions mode (the chip says "Actions"). With nothing to filter, actions
  mode lists `omnibox.actions` (every action, scrollable).
- The mode chip shows the mode (New Tab, Edit URL, Split › side, Actions, the latter filled with the
  accent) and a Tab key cap.
- Inline completion is shown selected after the caret. Queries pass `preventInlineAutocomplete` for
  deletions, edits before the end of the text and prefilled (selected) text, where a completion
  couldn't be shown.
- **Search suggestions** (`state.settings.searchSuggestions`, not in actions mode, not for blank
  text, over 256 characters, addresses with an explicit scheme or Windows paths):
  - every query immediately carries the suggestions already known for the text: an exact cache hit,
    else the longest cached prefix's suggestions that still start with the text (case-insensitive),
    so the completion doesn't flicker while typing;
  - 80 ms after the last change (and only for text not in the cache) `omnibox.suggest {text}` asks
    the shell; a reply is used only if the input still holds that text (and the bar wasn't closed,
    or the setting or engine changed, meanwhile). It goes into a 50-entry LRU cache (cleared when
    the bar closes, the engine changes or suggestions are turned off) and re-runs the query;
  - results refreshed for the same text keep the row the user moved the selection to (by key);
    otherwise the default row stays selected;
  - a 404 (a shell without `omnibox.suggest`) turns suggestions off for the page's lifetime.
- Suggestion rows (and the default row while it completes a suggestion) show the typed part in
  normal weight and the added words emphasized, like Chrome.

### Extensions mode (Ctrl+E)

`commandBar.mode = "extensions"` (PROTOCOL §12). The rows come from `omnibox.query` like every other
mode — core groups them (`extensions`, `needsOk`, `extensionsOff`, `more`) and puts the exact command
on each row, so the page renders and commits them unchanged. Two things differ from the other modes:

- the mode chip is a **label**, not a toggle: there is no Tab key cap and Tab does nothing, because
  "back to search" is not where Ctrl+E came from. `>` still switches to the actions list;
- the empty state says "No extensions installed" rather than "No results".

Row icons are `sta://command/__ext-icon/<id>/32`, served by the shell from the extension's own
directory; in mock mode there is no such route, so the mock leaves the URL empty and the rows fall
back to their letter tiles.

Subtitles come from core and are written for *this* surface: a row under the "Needs your OK" heading
does not repeat it, and the one row whose Enter leaves sta says so before it is pressed ("Toolbar
click isn't supported in sta · ↵ opens its Web Store page").

---

## Extension popup card (extension/)

The 40 px header the shell draws above an extension's popup page (PROTOCOL §4, §12): icon, name,
**Options** and ×. It renders `state.extensions.popup` and nothing else; the page below it belongs to
the extension and is not part of this surface. When `popup.failed` the strip is the whole card and
says "This popup doesn't work in sta yet". Esc closes the card. The strip is the card's accessible
name too (`role="group"`, `aria-label="<name> popup"`, and `document.title` = "<name> — popup"): the
card is a floating panel over the page, and the extension's name is the only thing that identifies
it.

---

## AI agents (agent/, topbar chip, sidebar, settings)

PROTOCOL §9, docs/MCP.md. Everything reads `state.agent`; the shell answers only commands, plus
`agent.info` / `agent.testConnection` for Settings.

- **`agent/`** (overlay, top-right of the content, a 380 wide card): the first of `agent.prompts`,
  else the activity panel while `agent.panelOpen`. Inside sta it sits in the shell's rounded overlay
  card (`--overlay-radius`, shadow; `html.native-card` tightens `.ag` / `.ag-panel` paddings), like
  the permission prompt. The chip, badges, rows and Settings section use the radius tokens only.
  - Connection prompt: client name and version *as reported*, the program (`client.exe`) and its
    signer with a *Verified* / *Unverified* badge, what the access level allows and that page
    content goes to the AI provider; Deny / Allow for this session / Always allow (only when
    `client.verified`). Site prompt: "Allow <client> on <site>?" with Deny / This session / Always.
    Tab prompt (`request_tab_access`): "<client> asks for a tab" with the tab card (host letter,
    title, host), the agent's reason in a quote labeled "The agent says" (its own words, clamped to
    4 lines), what sharing allows, and Deny / Share tab (`answerTabAccess`).
  - Input protection: every choice is inert for `ARM_MS` (1 s) after the prompt appears and after
    any key in the page (a line fills the divider meanwhile); no button has focus; Enter outside a
    button does nothing; Esc denies once armed. "Closes in N s" shows in the last 30 s of the 2-minute
    timeout. `window.__agentOverlay.{isArmed(), promptId}` is for tests.
  - Panel: connected sessions, Stop / Resume, the last 5 actions (failed ones in red with
    `errorPhrase(error)`; a row with a live tab activates it), held downloads (Keep / Discard),
    Archive N agent tabs, Agent settings. Esc closes it.
- **Topbar chip** (`topbar/agent-chip.js`, `agent-chip.css`): shown by `agentChipVisible(agent)`;
  label = first client (+N), "Approval needed" (filled with `--agent`), "Agents paused" or
  "N downloads waiting"; a pulse ring for 2.5 s after each action; the chip toggles the panel,
  Stop / Resume sit next to it. While it is shown the topbar's right column never shrinks below it
  (`.topbar:has(.agent-chip)`).
- **Sidebar**: tab rows with `agent: true` show the `agent` glyph (hidden on hover, where the close
  button goes); the tab menu has "Share with AI Agents" / "Stop Sharing with AI Agents" while
  access isn't off and the scope is agent tabs (not for `sta:` pages).
- **Settings → Animations** (`settings/animations.js`, section id `animations`, catalog in
  `common/motion-catalog.js`): the master switch, "Follow Windows animation effects" with a live line
  while Windows has them off, the eight collapsible groups with a group switch and an "n of m on"
  count, a switch and a one-line description per animation key, and Reset to defaults (disabled at
  the defaults). An off parent greys its children with `aria-disabled` and keeps them editable — never
  `inert`, so a key can be set up before its group goes back on. No speed control and no per-key
  previews: the `_gallery` replay is the developer-facing substitute.
- **Settings → AI agents (MCP)** (`settings/agents.js`, `agents.css`, section id `agents`): waiting
  approvals (keyboard-friendly answers), access (Off / Read only / Full) with its status, Stop /
  Resume and the privacy note; scope, ask before a new site, always allowed and blocked sites,
  devices on your network; scripts, history and downloads (page scripts Off / Isolated / Page for
  `evaluate`, browsing history for `history_search`, downloads list for `downloads_list`); waiting
  tab requests show "<client> asks for “title”" with the reason and Deny / Share tab; trusted clients with Revoke; *Connect a client* snippets
  (`setupSnippets(bridgePath, dataDir)`) with Copy; *Test connection*; the activity log path.

Screenshots: `node tools/ui-shot.mjs --path '/agent/?mock&agent=connection' --width 380 --height 330
--out shot.png` (add `--dark`; don't put `dark=1` after a `#hash`; add `&nativeCard` for the look
inside the shell's card), `/topbar/?mock&agent=busy`
(900×40), `/settings/?mock&agent=site` (1000×2600), `/agent/?mock&agent=tab` (380×400).

---

## Gallery and screenshots

`ui/_gallery/` renders every glyph, component, token and utility in the fixture theme. Its
controls switch spaces and toggle light/dark. Use it to check changes to `common/` visually.

```powershell
# headless Edge (DevTools protocol) + an in-process static server; stops both afterwards
powershell -NoProfile -ExecutionPolicy Bypass -File tools/ui-shot.ps1 -Path '/_gallery/' -Width 1280 -Height 3200 -Out gallery.png
powershell -NoProfile -ExecutionPolicy Bypass -File tools/ui-shot.ps1 -Path '/sidebar/?mock&panel=downloads' -Width 248 -Height 900 -Out sidebar-dark.png -Dark -Console
# the same without PowerShell
node tools/ui-shot.mjs --path '/sidebar/?mock' --width 248 --height 900 --out sidebar.png
```

`ui-shot.ps1` is a thin wrapper around `tools/ui-shot.mjs` (Node 22+ on `PATH`), which does all the
work, so no native command's stderr ever reaches Windows PowerShell 5.1 (where redirected native
stderr becomes a terminating error under `$ErrorActionPreference = 'Stop'`):
1. serves `ui/` in-process with `tools/ui-serve.mjs` on a free 127.0.0.1 port;
2. starts headless Edge with a throwaway profile and `--remote-debugging-port=0` (the port is read
   from the profile's `DevToolsActivePort`, so parallel runs never collide);
3. pins the viewport with `Emulation.setDeviceMetricsOverride`, so the page lays out at exactly
   `-Width` × `-Height` CSS px, including sidebar widths (200–440) below Edge's minimum window width
   of about 500 px, which `msedge --screenshot --window-size` silently widens;
4. loads the page, waits for the load event and `-Budget` ms, and on mock pages until `ui.ready`
   arrived (up to 10 s after load), then writes the PNG;
5. always kills Edge's whole process tree, closes the server and deletes the profile.

Parameters:
- `-Path`
- `-Out` (relative to the current directory)
- `-Width` / `-Height`: the viewport size in CSS px
- `-Dark`: appends `dark=1` and emulates a dark `prefers-color-scheme`
- `-Console`: also prints every console message and failed external loads
- `-Budget`: real time in ms to wait after the load event before capturing (default 3000)
- `-Scale`: device scale factor (the PNG is `Width×Scale` by `Height×Scale` pixels)
- `-Edge` / `-Node`: executable paths (`-Python` is still accepted and ignored)

Exit codes (with or without `-Console`):
- **0**: captured, no problems;
- **1**: capture failed (no PNG; bad arguments, Edge missing or not responding);
- **2**: captured, but the page reported problems: console errors, uncaught exceptions, failed
  loads of `ui/` files (any 4xx/5xx from the server, e.g. a missing module or fixture), or a mock
  page that never sent `ui.ready`.

Problems are always printed, and so are warnings that don't change the exit code: a capture with no
visible text, icon or control (a blank render; some surfaces are legitimately empty in their
default mock state, e.g. `/toast/?mock` without `toast=`) and an unexpected viewport size. Failed
loads of remote resources (fixture favicons) only show with `-Console`.

From Git Bash, set `MSYS_NO_PATHCONV=1` first, or MSYS rewrites `-Path '/_gallery/'` into a
Windows path (`C:/Program Files/Git/_gallery/`) and the page 404s.

Inside the app, use `node tools/cdp.mjs eval <host> "<js>"` against `window.sta` (see
`docs/ARCHITECTURE.md` §8).
