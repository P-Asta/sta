# Testing sta through MCP — the debug-only test surface

sta's end-to-end suites drive a **running browser through MCP**: JSON-RPC over stdio to
`sta-mcp.exe`, which forwards to the browser over its named pipe. Everything the suites need that
the 23 shipped tools deliberately refuse — sta's own `sta://` surfaces, raw DevTools, real OS keys,
the native window, window captures, the clipboard — is served by a **separate, debug-only test
surface** of `test_*` tools that exists only in a build made with `--features test-hooks` and only
while that build is explicitly armed.

> **This surface is not for agents.** It bypasses the whole agent policy: access level, scope, site
> approval, the URL allowlist, the settings gates and the DevTools method allowlist. A browser that
> serves it must never be a browser a person browses with. The four locks below are what makes that
> true, and `test_cdp` is the single most dangerous tool in the repo.

```
node (a suite) ──stdio──► sta-mcp.exe ──\\.\pipe\sta-agent-<random>──► sta.exe
                                                                        ├ test_*  → test_hooks/   (armed debug build only, no policy)
                                                                        └ others  → automation/   (the 23 shipped tools, full policy)
```

- Rust: `crates/sta/src/test_hooks/` (`mod.rs` arming + dispatch, `js.rs`, `native.rs`,
  `capture.rs`, `console_watch.rs`), catalog in `crates/sta-core/src/agent/test_tools.rs`,
  bridge side in `crates/sta-mcp/src/server.rs`.
- Node: `crates/sta/e2e/mcp.mjs` (the client library), `crates/sta/e2e/mcp-smoke.mjs` (a smoke test
  that exercises every tool).

## 1. Building and arming

```bash
cargo build -p sta -p sta-mcp --features test-hooks        # both crates need the feature
node crates/sta/e2e/mcp-smoke.mjs                          # 56 checks, no DevTools port, no PowerShell

E2E_DATA_DIR=C:/ast/tmp/shell node crates/sta/e2e/shell-e2e.mjs        # any of the six suites
```

> **Any cargo command without `--features test-hooks` disarms this.** `test-hooks` is not a default
> feature, so a plain `cargo test`, `cargo clippy` or `cargo build` rebuilds `target/debug/sta.exe`
> and `target/debug/sta-mcp.exe` **without** the test surface compiled in — and the next suite dies
> on its first call with `{"code":-32602,"message":"unknown tool: test_info"}`. That message is also
> what an *un-armed browser* answers by design (lock 4 below), so it reads like an arming failure
> when the real cause is the rebuild. `lib.mjs` runs `sta-mcp.exe --test-tools` before it launches
> anything and refuses with that explanation, but the cure is always the same: re-run the armed
> build above. Interleaving cargo commands and suite runs (as §6 invites) is exactly the trap.

Every suite runs the same way: `E2E_DATA_DIR` (its own throw-away data directory) and nothing else.
`crates/sta/e2e/lib.mjs` adds `--sta-test-hooks` and `STA_E2E=1` to the launch, waits for
`<data>/sta/agent-endpoint.json`, and spawns `target/debug/sta-mcp.exe --data-dir <that> --no-launch`
for the session — so **the data directory alone makes a run unique**: the pipe name is random per
browser and there is no port to hand out. `CDP_PORT` is still read by three suites, for the reasons in
§4. The suites that need the OS foreground (`chrome`, `agent`, `migration`, `extensions`, `tabs` — whose
`devtools` section presses real F12, Ctrl+Shift+I and F11, and whose `preview` section presses a real
Esc, the only way to reach `keyboard.rs::escape_chain` — and `mcp-smoke`, which sends real keys
and therefore activates its window, though its checks tolerate `window_busy`) must not run at the
same time as each other, and nobody should type while they do.
Nor should anyone open a terminal while one runs: the console-window check (§5) fails on any console
window that appears, whoever opened it.

The browser arms only when **all four locks** are satisfied:

| # | Lock | What it stops |
|---|---|---|
| 1 | **Compile.** Every line is behind `#[cfg(all(debug_assertions, feature = "test-hooks"))]`, and `test-hooks` is not a default feature. A **release** build that tries to enable it fails with a `compile_error!` — and, because that guard reads `debug_assertions` rather than the profile, `crates/sta-core/build.rs` fails the build outright when Cargo says `PROFILE=release` and the feature is on (a "release with debug assertions" profile would otherwise compile the whole surface into an optimized binary). | A test surface reaching a shipped binary — and, just as important, somebody *assuming* a `cfg` protected them. |
| 2 | **Two runtime tokens.** The command line must contain `--sta-test-hooks` (or `--sta-test-hooks-no-approve`) **and** the environment `STA_E2E=1`. Both are read once, in `main`, before any profile work. | Either token alone is plausible by accident: a stale variable in a shell, a flag copied from a script. Nothing can arm the surface later — no IPC, command, setting or tool. |
| 3 | **Never a real profile.** Arming also needs an explicit `--sta-data-dir=<path>` (or `STA_DATA_DIR`) that is not, and is not inside, `%LOCALAPPDATA%\sta`, `sta Dev`, `Astatine` or `Astatine Dev`. If the first two locks pass and this one does not, the browser **logs and exits with code 2**. | A silent downgrade: a suite running against the user's own history, cookies and settings — and passing. | <!-- rename:keep -->
| 4 | **Invisible when un-armed.** The browser reports `testHooks: true` in its `welcome`; the bridge appends the test tools to `tools/list` only after it has seen that, with `ttlMs: 0` and one `notifications/tools/list_changed`. An un-armed browser answers every `test_*` name with `unknown_tool` — the same error a typo gets. | Probing: a client cannot even learn that the surface exists. |

An armed browser writes one `WARN` line per launch (`TEST HOOKS ARMED: …`), `test_info` answers
`testHooks: true`, and `agent.log` marks its sessions `testHooks=1`.

## 2. How it composes with the agent policy

**Separation, not relaxation.** Not one line of `sta-core/src/agent/policy.rs`,
`automation/guards.rs` or the settings gates changes.

- **A separate dispatch arm.** `automation/session.rs::start_call` routes a `test_` name to
  `test_hooks::run` *before* `check_access`, the pause check, scope, `check_url`, site approval and
  the rate limiter. The 23 shipped tools keep going through the unchanged path.
- **The harness is not an agent.** A test call never marks a tab agent-controlled and never appears
  in the user's activity list, so `chrome-e2e` still sees the user's browser: no orange frames, no
  held downloads, no refused fullscreen. It is logged to `agent.log` like any call.
- **Approval and access.** `--sta-test-hooks` welcomes its own sessions without the core's
  connection prompt and reports full access **in memory for that run** — nothing is written to
  `state.json`, and the user's settings are untouched. That removes the bootstrap deadlock that
  made the suites turn agent access on over CDP before MCP worked at all. One function answers
  "what access does this session have": `automation/session.rs::access()`, which both the
  dispatcher's `check_access` and `tools::full_access_required` ask — so an armed run cannot pass the
  gate and then be refused `read_only` by a shipped write tool.
- **The consent suite opts out.** `--sta-test-hooks-no-approve` arms the tools but leaves
  approvals, access and site prompts exactly as the profile has them. `agent-e2e`'s consent, site,
  tab-access and policy sections run that way and use the test surface only to *observe* and to act
  *as the user* (clicking the real prompt buttons), never to skip a prompt.
- **Two sessions, two identities.** A suite may hold two bridge connections (the channel allows
  two): the *driver* (test tools) and the *subject* (the 23 real tools under real policy). Policy
  assertions belong to the subject session; test tools may never stand in for an agent.

## 3. The tools

Conventions: every name matches `^test_[a-z_]+$`; every schema is closed
(`additionalProperties: false`); machine data comes back in `_meta["sta/structured"]` with a
one-line summary in `content[0].text`; every tool is tagged `_meta["sta/test"] = true` in
`tools/list`, so a client can filter the whole surface out in one predicate. None is read-only.

Errors reuse the shipped `ErrorCode` set plus three that only exist here: `test_hooks_off` (the
surface was closed by shutdown), `no_such_target` (a selector matched nothing) and `window_busy`
(real input while sta is not the foreground window).

`target` selectors take exactly one of `surface` (an `sta://` host: `sidebar`, `topbar`, `empty`,
`command`, `find`, `permission`, `switcher`, `toast`, `peek`, `agent`), `tab`, `browser` (a CEF id
from `test_targets`), `targetId` (from `test_targets`, after `test_attach`) or `match` (a substring of
the target URL).

### A. Core and shell

### `test_info`
`debug.info`: window, tabs, overlays, rounded, browsers, controller, ipc, keyboard, permissions,
suggest, sidebarHover, motion, automation, foreign, devtools, devtoolsCdp, extensions, extBackend,
extPopup, safeMode, downloads and focus, plus a monotonic `at` in milliseconds and `testHooks: true`.

`motion` is what the animation settings resolved to and the timing the shell owns
(ARCHITECTURE §4.7, PROTOCOL §14): `level` and `off` as core resolved them, `systemAnimations` (the
Windows "Animation effects" setting) with the value the shell last read, the waits around a surface's
blank frame (`hideDelayMs`, `parkDelayMs`, `toastDelayMs`, `switcherDelayMs`, with `hideFloorMs` 50,
`parkFloorMs` 60, `waitCapMs` 120 and the page's own `fadeMs`) — correctness delays that must never
drop below their floor at any level — the `SetChrome` midpoint (`themeFadeMs`, `chromeDelayMs`,
`chromeCalls`, `chromeDelayed`) and the counters `reads`, `changes` and `settingMessages`
(`WM_SETTINGCHANGE` messages seen). The acknowledged exits report `lingering` with a `pending[]` entry
per surface waiting right now (`{gen, key, browserId, floorMs, waitedMs}`), plus `exits`, `acks`,
`ackTimeouts` (a page that never answered — **0** in a normal run), `cancels` (a show during a linger),
`earlyHides` (a hide with no page to ask at all — also 0), `staleAcks`, `lastExitMs` and
`slowestExitMs`. `floorOverrideMs` is what `debug.motion {floorMs}` raised the floor to, `null`
normally. `sections` (at most 20 entries) keeps the
answer small — **use browser-side `at` for timing assertions**, never a wall clock around the call.

`devtools` is the docked DevTools of each tab: the frontend browser, the inspected browser, session
S, the stack/page/frontend bounds and the rect the frontend last reported, the debugger keys it asked
the page to forward, and the layout counters (`relayouts`, `boundsWrites` — a layout loop shows up
as `boundsWrites` that never stop growing). `devtoolsCdp` adds the bridge per inspected browser:
its session, nested sessions, how many messages crossed each way, how many were dropped, the largest
one, and `refused` — methods the frontend sent that are not in the measured inventory
(`devtools_policy.rs`), which is how a DevTools feature nobody exercised is found. `dropped` counts
every message the bridge did not pass on: one on a session it never admitted, one over 64 MiB (which
is answered with a protocol error rather than lost), one that is not a protocol message, and one
whose id belongs to another DevTools client's range — so a suite that provokes any of those asserts
against the count it read *before* the provocation, not against 0.

`extensions` is the installed-extensions listing the shell last read off disk (with its refresh
counter, each row's `blocked` reason, and `pending` — states Chromium confirmed that the profile has
not committed yet, with their age), `extBackend` the hidden `chrome://extensions` operation window
(the current operation, its window's cloak state, and the `ok` / `failed` / `abortedVisible` /
`timedOut` counters), `extPopup` the open popup card (its extension, browser, measured size, and
whether it is showing the honest failure line), and `safeMode` the crash-loop guard (whether this run
started in safe mode and how many startup crashes are counted). `foreign.windows` adds
`extraDialogsCloaked` and `dialogRoots`: while an extension is being removed exactly one window owned
by the hidden backend root is admitted (Chromium's "Remove …?"), and anything else it opens is cloaked
and counted there.

### `test_state`
The core `UiState`, exactly as the `state.get` IPC request returns it to sta's own surfaces — the
extensions listing the user sees is `extensions` there (§12 of docs/PROTOCOL.md).

### `test_dispatch`
Dispatches one core `command` (shell events included) through the controller.

### `test_execute`
Runs one `effects` object or an array of them through `controller::run_effects`.

### `test_push_state`
Pushes a `state` event to every IPC subscriber now.

### `test_open_tab`
Opens any `url` (internal pages included) in a tab with a fresh id, without policy and without
marking it agent-controlled; `show` (default true) puts it on screen. → `{tab}`.

### `test_focus`
`request_focus` on a `surface`'s or a `tab`'s `BrowserView`.

### `test_accelerator`
Runs the accelerator bound to `key` (a virtual-key code, 0 to 255) with `ctrl`, `shift`, `alt`
→ `{commandId}`; `no_such_target` when nothing is bound.

### `test_send_key`
`Window::send_key_press` for `key` (0 to 255) with `ctrl`, `shift`, `alt`; `window_busy` when the
window is not active.

### `test_reset_permissions`
Resets every CEF content setting the permission `bits` map to for `origin`, like an expired
one-time grant → `{reset}`.

### `test_counts`
Per-command dispatch counters and the accelerator count (sugar over `test_info`).

### B. JavaScript

### `test_eval`
`Runtime.evaluate` of `expression` in a `target` — **including sta's own surfaces**, which the
shipped `evaluate` refuses (`internal_page`) — with `awaitPromise` (default true),
`returnByValue` (default true), `userGesture` (default false), an optional `sessionId` and
`timeoutMs` (default 10000, at most 120000). → `{value}`, or `{error: {text, stack}}` when the
expression threw (the call itself succeeds). An expression that evaluates to `undefined` answers
`{}`, not `{value: null}`, so a caller can still tell the two apart.

**Target selectors** (`test_eval`, `test_invoke`, `test_cdp`, `test_cdp_events`, `test_attach`):
`{surface: "<host>"}`, `{tab: <id>}`, `{browser: <cef id from test_targets>}` — the only way to name a
browser that is neither a tab nor a surface, such as a DevTools window or a Chrome-created popup —
`{targetId: "<id>"}` or `{match: "<substring of the URL>"}`. A `targetId` needs a session:
`test_attach` first (`lib.mjs`'s `selector` does it for you), or pass the `sessionId` yourself.
Without one the call is refused with `no_such_target` — it must never quietly answer from some other
page, or a check that asserts an *absence* would pass while measuring the wrong target.

### `test_invoke`
A real `window.sta.invoke` of `cmd` with `payload` inside the `target`'s own frame — the
trusted-frame check, the CEF message router and the surface's `window.sta` shim all stay covered —
with `timeoutMs` (default 10000, at most 120000). → `{ok}` or `{err, msg}`.

Debug builds answer the `debug.*` requests through it too (PROTOCOL §2), which is how a check reaches
one that needs no tool of its own: `debug.hoverInput {realCursor}`, `debug.motion {floorMs}` (raise
the acknowledged exits' floor so a check can act *inside* a linger; `null` restores the real 50/60 ms)
and `debug.motion {slideMs}` (stretch the floating sidebar's slide so a check — or an eye — can catch
the card part-way out; `null` restores the real 160/240 ms) are all driven this way.

### C. Input

### `test_real_keys`
Real OS keyboard input (`SendInput`), so Views accelerators and the keyboard handler run: `combo`
(`ctrl+shift+k`, `f5`, `alt+1`, `escape`, …) or explicit `steps`, at most 64, each a key name
`key` with `down` or `up` (neither = a press) or a pause `waitMs` (at most 5000). `delayMs` between
transitions (at most 2000); `activate` (default true) takes the foreground first.
→ `{sent, aborted, reason}`; `window_busy` when another window holds the foreground.

### `test_post_mouse`
Mouse messages posted to sta's own window in client DIP (no OS cursor, no other window sees them):
`steps`, at most 64, each either a `type` of `move`, `down`, `up` or `dblclick` at `x` and `y` with
`button` `left` (default) or `right`, or a pause `waitMs` (at most 5000). → `{posted}`.

### `test_hover_input`
Drives the sidebar hover reveal: `enabled` on or off, a virtual `pointer` (`{x, y, buttons,
overWindow, ownedPopup}`, or `null` for the real cursor), or the guarded `realCursor`
(`{x, y, holdMs}`) path. → the hover snapshot.

### `test_tab_key`
Sends `key` (one character, default `a`) to a `tab`'s browser host, so it reaches
`on_pre_key_event` like the user's typing, without the OS foreground.

### D. Native window, pixels, OS

These replace `e2e/win-probe.ps1`, `tools/capture-window.ps1`, `e2e/png-pixel.ps1`,
`Get-Clipboard` and `Get-Content -Stream`. The scripts stay in the repo as human and agent
debugging tools; they are off the suites' path with **one** exception: `agent-e2e`'s consent phase
runs before its session is allowed to call a tool (that is what it tests), so its two diagnostic
captures there still go through `capture-window.ps1`. From the user's click on, `CdpInstance.probeVia`
puts `win` / `capture` / `pixels` on the suite's own MCP session like everywhere else.

### `test_window`
`hwnd`, position, size, `dpi`, `zoomed`, `iconic`, `thickFrame`, `enabled`, `foreground`, class,
title and the held modifier keys. With `all`, every top-level window of this process (class, title,
owner, visible, hidden, enabled, bounds and the two style words `style` / `exStyle` as numbers) —
dialogs and Chrome-created windows included. `lib.mjs`'s `ownedWindows()` filters that list down to
the visible windows sta's main window **owns**, which is how a suite finds Chromium's own dialogs:
they are `Chrome_WidgetWin_1` views widgets, not `#32770`, so `dialogs()` never sees them, and the
install dialog is recognised by its styles (`WS_EX_DLGMODALFRAME` set, `WS_EX_TOOLWINDOW` clear,
`WS_CHILD` clear — `foreign.rs::is_install_dialog_style`).

### `test_hit_test`
`WM_NCHITTEST` at window-relative `points` (at most 64 `[x, y]` pairs in device pixels; `space`
`device` (default) or `dip` scales by the window DPI first) → `{codes}`: 1 CLIENT, 2 CAPTION,
10 to 17 the borders.

### `test_window_message`
Posts `message` — `close`, `restore` or `minimize` — to sta's own window, or to one `hwnd` of this
process (a native dialog).

### `test_dialog`
Presses one key in a modal dialog sta's main window owns — the only way to answer Chromium's "Add
extension?" dialog: its buttons are views (no child window to post a message to) and it takes the
foreground itself, so `test_real_keys`, which insists on sta's *main* window being foreground, both
refuses and would steal the focus. `press` is `enter` (default: the dialog's default button),
`escape`, `space` or `tab`; `hwnd` defaults to the foreground window and must belong to this process,
be owned by **one of sta's own top-level windows** (never the main window itself — that is
`test_real_keys` — and never an unowned window), be visible and be the foreground window
— otherwise `no_such_target`, `invalid_arguments` or `window_busy`, and nothing is sent. The second
dialog it answers is Chromium's "Remove …?" confirmation, which belongs to the hidden
`chrome://extensions` window of an extension operation rather than to sta's main window
(`lib.mjs ownedWindows({any: true})` lists those).
→ `{hwnd, press, sent, window}`. Chromium enables an install dialog's button about 275 ms after it
appears (its input-protection delay), so wait before pressing. **Accepting the "Add extension?"
dialog is `tab` then `space`**: it opens with *Cancel* focused and deliberately has no default button,
so `enter` does nothing and `space` alone cancels (all three measured against the live Web Store;
`extensions-e2e`'s (webstore) section does it this way).

### `test_capture`
A PNG of sta's own window (`PrintWindow` with `PW_RENDERFULLCONTENT`, never the screen), written to
`out` (default: the profile's `Logs` folder) → `{path, width, height, scale, bytes}`. `region` is
`window` (default) or `client`; `hwnd` picks another window of this process; `inline` returns
`{data}` base64 instead, up to 6 MB (`too_large` above that).

### `test_pixels`
Colors of a capture at `points` (at most 64 `[x, y]` pairs): `path` is a PNG written by
`test_capture` (or by `capture-window.ps1`), `space` is `dip` (default, scaled by the window DPI)
or `device` → `{colors}` as `#rrggbb`.

### `test_clipboard_get`
The clipboard's Unicode text → `{text}` (null when it holds none).

### `test_clipboard_set`
Puts `text` (at most 100000 characters) on the clipboard.

### `test_zone_identifier`
The `Zone.Identifier` alternate data stream (Mark of the Web) of the file at `path` → `{zone}`,
null when it has none.

### `test_console_windows`
The runtime half of the no-console-window rule → `{current, seen, ours, watching, at}`: console
windows visible now (each with `userVisible`), every console window *shown* since arming or the last
`reset` (so a **flash** is caught too), and the subset of `seen` this browser's tree can be shown to
have caused. `roots` (at most 16 process ids; the suite passes its own pid) extends that tree.

Each `seen` entry carries `className`, `title`, `pid`, `hwnd`, `shownAt`, `chain` (the owning
process's ancestry, resolved **while it was alive** — a helper that flashes a console and exits is
gone from every later process snapshot), `userVisible` (a `ConsoleWindowClass` or
`CASCADIA_HOSTING_WINDOW_CLASS` window, as opposed to the internal `PseudoConsoleWindow` host window
that every hidden ConPTY creates), `hosted` (a console *host*'s window, so its owning process is not
the client) and `ours`.

**Assert on `seen`, not on `ours`.** Under the Windows 11 default terminal the console window belongs
to `WindowsTerminal.exe`, a child of `svchost.exe` — it descends from nothing of ours, so ancestry
alone cannot see the one case this tool exists for. `ours` is a precise *hint* (ancestry, plus the
title Terminal sets to the client's image path); `lib.mjs`'s `checkNoConsoleWindows` fails on every
`userVisible` entry that was not already on the desktop at the baseline.

### E. DevTools and targets

### `test_targets`
Every target: sta's own browsers as `{id, browser, type, url, role}` with their role
(`surface:<host>`, `tab:<id>`, `other`) and, with `cdp` (default true), what `Target.getTargets` adds
as `{id, targetId, type, url, title, role: "cdp"}` — extension service workers, DevTools frontends,
Chrome-created pages. `lib.mjs`'s `Instance.targets()` merges the two lists (so `.title` keeps
working), sorts them newest-browser-first the way `/json/list` did, and gives each entry the selector
the other tools take.

### `test_cdp`
Any DevTools `method` with `params` on a `target`, with an optional `sessionId` and `timeoutMs`
(default 10000, at most 120000) — **no allowlist**, exactly like the `debug.cdp` request the suites
use today. → `{result, ms}` or `{error, ms}`.

### `test_cdp_events`
The last 200 DevTools events of a `target`'s session (params truncated), newest last; `clear`
empties the buffer.

### `test_attach`
`Target.attachToTarget` on `targetId` (a `target` picks which session sends it) → `{sessionId}`,
remembered so `{target: {targetId}}` selectors work afterwards.

### F. Chrome-created browsers

### `test_foreign`
`foreign.rs`'s snapshot: entries, counters, events, hidden windows, hook counters.

### `test_foreign_close`
Closes the Chrome-created browser `id` now, ignoring the close rules → `{closed}`.

### `test_foreign_trigger`
`Target.createTarget` for `url` on the shell's own DevTools client, so Chromium creates a browser
the way an extension's `tabs.create` does → `{targetId}`.

### G. Batching

### `test_batch`
Runs up to 16 `calls` in order in one round trip — a poll predicate that needs two or three
observations — each a tool `name` with its `args`, with `stopOnError` (default true). No nested
`test_batch` and no `test_real_keys` inside (its foreground guard must own the UI thread).

## 4. What does *not* go through MCP

MCP needs a running browser; these tests are about *not* having one, or about the machine around
it. They stay in Node and that is by design — "all tests run through MCP" means **every test that
talks to a running browser talks to it through MCP**.

| Capability | Why |
|---|---|
| Launching `sta.exe` with args, env and a data dir | The browser is the thing under test. |
| Killing the process tree, asserting no process remains | Post-mortem: nothing is there to answer. |
| Exit codes and clean-exit assertions | Same. |
| Startup failure (`STA_DEBUG_FAIL_STARTUP`), the fatal error box | The pipe never exists. |
| A second launch that is refused or forwarded (single instance) | The launch under test exits at once. |
| Data-folder migration (folder moves, profile subfolder, lock files) | Happens before anything is serviceable. |
| Reading `state.json`, `agent-endpoint.json`, the stderr log, downloaded files | Plain `fs`; nothing to gain. |
| Local HTTP fixtures | Node `http`; the browser must not serve what it is tested against. |
| `STA_*` environment and launch flags | Set at launch by definition. |
| The desktop foreground lock between suites | Coordination between *suites*, not a browser capability. |

One more thing stays outside: `crates/sta/e2e/mcp.mjs`'s `PipeClient` speaks the raw NDJSON channel
for the handful of checks that are *about* the pipe (no `hello`, a wrong protocol version, more
than two sessions, the 8 MiB line limit). A conformant bridge cannot misbehave on purpose — which is
exactly why `mcp-smoke`'s `(pipe)` section sends an over-long line from there and then asserts the
browser still answers the next call: the limit must cost one message, never the session
(`sta-core/src/agent/channel.rs`). Its other half, an answer that does not fit coming back as
`too_large` on a session that keeps working, is a plain MCP call.

### The three places that still use the DevTools port

1. **`shell-e2e (c)`** asks `GET http://127.0.0.1:$CDP_PORT/json/version` once, to prove the debug
   build still opens `STA_REMOTE_DEBUGGING_PORT` at all. It is the only *assertion about* the port,
   and the only use of it outside the two suites below — which do drive their browser over it.
2. **`agent-e2e`'s driver** (`CdpInstance` in `lib.mjs`). That suite's *subject* is this channel: it
   asserts that `tools/list` is the static 23 and `tabs_list` answers `browser_not_running` while
   access is off, that a client is welcomed only after the **user** clicks Allow, that `Stop` leaves
   `session.connections` empty, and that access off removes the endpoint file. A harness session on
   the same pipe would change those numbers, and hiding it would mean exempting it from the connection
   list, from `disconnect_all`, from the paused refusal and from the topbar chip — four places where a
   real regression could then hide. So everything the *agent* does is MCP (that is what the suite
   tests) and only the part that acts as the **user** — trusted `Input.dispatchMouseEvent` clicks on
   prompt buttons — uses the port. Its browser is still armed, with `--sta-test-hooks-no-approve`, so
   it gets `test_console_windows` and nothing about the agent path is relaxed. Its native probes
   (`win`, `capture`, `pixels`) run on that same subject session from the moment it may call a tool
   (`CdpInstance.probeVia`); only the two captures taken *during* the consent phase are PowerShell,
   because at that point no session is allowed to call anything.
3. **`migration-e2e`** (`CdpInstance` too). Arming requires an explicit `--sta-data-dir`, and this
   suite is about the launch that passes **none** and has to resolve, move and lock its own default
   folder under a redirected `%LOCALAPPDATA%` — where lock 3 would reject that folder as a real
   profile and exit 2. It also drives a build from before the rename, which has no test surface.

`C:/ast/tmp/s6/cdp-residue.md` is the full list, check by check.

## 5. No console windows

A console window that flashes during a run steals the foreground from the window under test, and
the user asked for none. Two halves:

- **Static** — `node tools/check-no-console.mjs`: every `spawn` / `execFile` of a *console* helper
  (powershell, taskkill, python, node, `sta-mcp.exe`) in `tools/` and `crates/sta/e2e/` passes
  `windowsHide: true`; every `std::process::Command` in `crates/*/src` and `crates/*/tests` sets
  `creation_flags(CREATE_NO_WINDOW)`; every raw `CreateProcessW` passes `CREATE_NO_WINDOW` and never
  `DETACHED_PROCESS`; nothing goes through `cmd /c` (in one string or in argv form), `{ shell: true }`
  or `Start-Process`. The only allowlist is a `// console-ok: <reason>` comment on the call or the line
  above it.
- **Runtime** — `test_console_windows`, which also catches a console that opened and closed again
  between two polls. It asserts on every console window *shown* during the run, not on the ones it can
  attribute: see that tool above for why ancestry is not enough.

Every suite ends with the two `[hygiene]` checks that runtime half produces — **except
`migration-e2e`**. Its whole subject is a launch that passes no `--sta-data-dir`, which lock 3
refuses to arm, so there is no armed browser in it to host the watcher. Instead it samples the
*desktop* with `crates/sta/e2e/win-probe.ps1 -ProcessId 0 consoles` (every `ConsoleWindowClass` /
`CASCADIA_HOSTING_WINDOW_CLASS` / `PseudoConsoleWindow` top-level window, whoever owns it) around
every launch and quit and once at the end, and asserts that no visible console window appeared that
was not already on the desktop at the baseline. That is a sample, not a hook: a console window that
is up when a sample runs fails the suite, one that flashes entirely between two samples does not.
README says the same, so the gap is visible outside this file too.

**The debug `sta.exe` is a console-subsystem binary** (`main.rs` asks for `windows_subsystem =
"windows"` only when `debug_assertions` are off), so *how* it is started decides whether a console
window appears: a child of a parent that has a console inherits it (nothing new appears), a child
started with `CREATE_NO_WINDOW` gets an invisible one, and a child given **no** console —
`DETACHED_PROCESS`, or a GUI parent with no console — makes Windows allocate a fresh console, which
Windows 11 hands to Windows Terminal: one visible window for the browser and one per CEF subprocess.
That is why `crates/sta-mcp/src/win.rs::launch` uses `CREATE_NO_WINDOW` (the two flags are mutually
exclusive) and why the static check forbids the other spelling.

**`windowsHide` does not belong on the `spawn(sta.exe)` calls.** Node's flag is libuv's
`HIDE_CONSOLE | HIDE_GUI`: besides `CREATE_NO_WINDOW` it sets `STARTF_USESHOWWINDOW` with
`SW_HIDE`, so a GUI child whose first `ShowWindow` uses `SW_SHOWDEFAULT` starts **invisible**. Those
call sites carry the `// console-ok:` comment instead, and the static check enforces exactly that.

## 6. Keeping it honest

- `cargo test -p sta-core --features test-hooks` — the catalog's shape, and that it is disjoint
  from the shipped one.
- `cargo test -p sta --features test-hooks` — arming refuses every real profile; an un-armed build
  answers `unknown_tool`; every catalog entry has a dispatch arm; the PNG writer and reader round
  trip; and the console watcher's own recording path: a console window recorded for a process that no
  longer exists is still attributed, a Terminal-hosted one is attributed by title, a
  `PseudoConsoleWindow` is `userVisible: false` and a non-console class is not recorded at all.
- `node tools/check-mcp-docs.mjs --armed` — this file against `sta-mcp --test-tools`, the way the
  plain run checks `docs/MCP.md` against `tools/list`.
- `node tools/check-no-console.mjs`, `node crates/sta/e2e/mcp-smoke.mjs`.
- `node tools/check-icon.mjs` — `crates/sta/res/sta.ico` carries every size Windows asks for (16,
  32, 48, 256 at least) and its entries are byte-identical to the runtime `icon-*.png`, so the ICO
  and the PNGs are always one run of `res/make_icon.py`.
- **Run `check-mcp-docs` both ways.** The plain run checks `docs/MCP.md` + `MCP.ko.md`; `--armed`
  checks *this file*. They are two different gates with one name, and a sweep that runs only the
  plain one leaves this file's tool catalog unverified.
- `node tools/check-motion.mjs` — the animation registry (`sta-core/src/motion.rs`) against the UI
  catalog (`ui/common/motion-catalog.js`) and the CSS gates (`ui/common/tokens.css`), plus the motion
  rules that span files: nothing waits on `animationend`, no tracked overlay root is animated or
  transformed, no *animated property* takes its duration from two different keys' tokens (a
  `transition` list may mix keys — a hover tint and a chevron rotate are different animations — but
  one entry of it may not), and every CSS transition of a layout property is allowlisted one selector
  and property at a time (ARCHITECTURE §4.7).
- `node tools/motion-check.mjs` — the motion runtime in **mock mode** (headless Edge, no console
  window): the level attribute and `--motion-distance`, identity transforms at `reduced`, a key that
  is off zeroing only its own token and refusing WAAPI, a keyed animation starting by itself (counted
  in frames, never against a wall clock) and replacing rather than stacking, a surface that declares
  its presence being believed over a hidden `visibilityState` and one that does not being refused, no finite animation at `off` with a *visible* static ring and
  bar, the scoped `off` selectors not leaking into pseudo-element motion, turning animations off
  mid-run finishing what runs — the master switch and a **single key**, which settles that key's
  animations and leaves the others playing — a staggered entrance holding its first keyframe while it
  waits for its delay (no element opaque while an earlier one is still fading in), ghosts being inert
  clones with no id, `data-*`, `role`, `aria-*` or
  `tabindex`, `trackSurfaceSize` staying correct while the surface is transformed, and 30 state pushes
  in a row starting nothing. Per area, it also drives the real surfaces in mock mode: `sidebar` (a row
  arriving and a row leaving, the followers' FLIP and its bulk / drag / pointer-close opt-outs, a
  folder collapse, the Clear Today sweep, the space switch's single pane ghost — and two fast switches
  never leaving more than one of those alive — the Undo of Clear Today running no FLIP at all (dozens
  of ids change while one row would slide the length of the list) and a 10-id change running none
  either however few rows moved, a reorder snapping instead of FLIPping at `reduced`, the active-row
  crossfade, the split glider, the hover reveal's ≤ 60 ms hide with no pointer events, the drag
  ghost's `scale` lift and the drop line's glide, the favorites pop and grid FLIP, a closing panel's
  exit ghost, the download card and the check its ring hands over to, the URL pill's copy check, and
  one key off taking exactly its own motion away), `cmdbar` (the selection glider gliding on a keyboard move
  and snapping on hover, the first results' stagger, the mode crossfade and the placeholder's restart
  attribute, and the input and card staying untransformed), `topbar` (`topbar.navFade` starting
  only after the shell's resize delay, and the URL pill's host crossfade firing for the *same* tab
  only), `overlays` (the toast's rise, the fade it becomes inside the native card — where a rise would
  clip the Undo button against the card's edge — its text-only replacement, the switcher's single card fade
  and gliding ring — which snaps at `reduced` — the find bar's opacity-only entrance, its shake only
  on a *repeat* ask and never while the IME composes, the permission prompt's fade on an inner wrapper
  with its queue counter popping, and Peek's header crossfade firing on the peeked tab only),
  `menus` (the pop-in's resolved origin and travel direction, the drill-down slide, the inert exit
  ghost with nothing left that carries a role or an id, and the key off leaving neither), `pages` (the
  enter stagger inside its 200 ms budget, the settings nav indicator gliding, the disclosure's height
  animation settling back to `auto`, an archive row's ghost and its followers' glide, a search that
  empties the list not counting as a row change, the boosts editor's View Transition with the boost
  already fetched, and the empty state's hero), `theme` (`theme.crossFade` on a page that paints its
  own background, never inside a native card, and kept at `reduced`), `controls` (the two durations of
  `controls.hoverPress` switching off together while `controls.toggles` stays, and a thumb arriving at
  once at `reduced`) and `indicators` (one period for the spinner, bar and ring; the audio bars
  stopping the moment the window loses focus; a static ring and a *visible* bar when
  `indicators.loading` is off) and `exit` (the page half of an acknowledged exit: a surface blanks when
  the shell asks and *stays* blank, answers `surface.exited` with the generation it was given — even
  with the key off, where it blanks instantly — ignores a request without a usable one, never touches
  the tracked root, and un-blanks when there is something to show again — plus the other half of the
  rule for the **activatable** overlays, which nothing waits for: Esc blanks the command bar's card and
  the find bar in the *same task as the key*, the close is dispatched only after that and still carries
  the bar's own `seq`, and the next open un-blanks the card completely). Exit 0 all passed, 1 the
  harness could not run, 2 a check failed.
  Checks that have to *see* a short animation running stretch its token first (`slow()` in the
  harness), because a headless renderer's frame pacing is not a timing guarantee. It cannot see stale
  frames, restacks, the `SetChrome` midpoint, ack timing, the native card or IME placement: those are
  `chrome-e2e`'s `m.motion`, `m.motion.ui`, `m.motion.ui2`, `m.motion.perf` and `m.motion.shell`
  sections.
- `node tools/check-release-clean.mjs` — builds nothing, but proves a release binary contains no
  `test_*` tool name (run it after `cargo build --release`).
