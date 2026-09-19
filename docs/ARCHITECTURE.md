# sta architecture

sta is an Arc-style desktop browser built with **CEF 152 (Chromium 152)** through the
[`cef`](https://crates.io/crates/cef) crate `=152.3.0`, written in Rust. The primary target is
Windows 11 x64; **macOS 11+** is the second (§3.2). The browser chrome (sidebar, top bar, command bar,
internal pages) is HTML/CSS/JS served from a custom `sta://` scheme. All state lives in Rust.

Verified research notes, with exact API signatures and CEF behaviour, are in `docs/research/`:
`views.md` (window, layout, overlays), `ipc.md` (scheme and IPC), `handlers.md` (browser handlers),
`platform.md` (bootstrap, settings, Windows, macOS §10), `extensions.md` (what Chrome extensions can and
can't do in Alloy tabs, and the windows Chromium opens for them, §4.5), `devtools.md` (how DevTools
are docked inside the window, and the five approaches that were tried, §4.1), `automation.md`
(DevTools measurements) and `arc_spec.md` (product/UX spec). **Read the relevant one before touching CEF
code.** Every claim there was checked against the bindings and
the CEF 152 sources.

---

## 1. Repository layout

```
Cargo.toml                 workspace (cef pinned =152.3.0, default-features = false: no sandbox)
.cargo/config.toml         CEF_PATH = <repo>/.cef  (prebuilt CEF downloaded once by cef-dll-sys)
crates/sta-core/           pure Rust: model, Command→Effect reducer (Store), omnibox, history,
                           theme colors, the animation registry (motion.rs, §4.7), URL helpers,
                           persistence helpers, legacy names from
                           before the rename (legacy.rs), AI agent policy/tools (agent/). NO CEF.
                           Unit-tested.
crates/sta/                the browser executable (CEF shell): window and tabs, overlays, keyboard,
                           IPC, the sta:// scheme, OS integration (platform/: win.rs, mac.rs and
                           the macOS app bundle mac_bundle.rs), agent automation,
                           extensions (§4.5-4.6), docked DevTools (§4.1), and the debug-only test
                           surface (`test_hooks/`, §8.4). `motion.rs` is the only timing the shell
                           owns (acknowledged exits, the SetChrome midpoint, §4.7)
crates/sta/e2e/            end-to-end suites against a debug build (§8); `extensions/` holds the
                           in-repo probe extensions those suites load (§4.5)
crates/sta-mcp/            MCP server (stdio) for AI agents; talks to the browser's agent pipe (§5.2)
ui/                        HTML UI, embedded with rust-embed (read from disk in debug builds)
tools/cdp.mjs              DevTools-protocol client for automated checks of a running build
tools/capture-window.ps1   screenshot of the sta window only (never the whole screen)
tools/ui-serve.mjs         static server for ui/ in mock mode (correct MIME types, no-store)
tools/ui-shot.mjs, .ps1    headless-Edge screenshots of UI pages in mock mode with console checks
tools/check-mock-commands.mjs  UI mock command validation ⇄ command.rs contract check
tools/check-mcp-docs.mjs   docs/MCP.md + MCP.ko.md ⇄ the bridge's real tools/list
tools/check-motion.mjs     animation registry ⇄ UI catalog ⇄ CSS gates, and the motion rules (§4.7)
tools/motion-check.mjs     the motion runtime in mock mode (headless Edge)
tools/check-no-console.mjs static half of "no console window while testing" (§8.4)
tools/check-release-clean.mjs  the shipped binaries carry none of the test surface (§8.4)
tools/check-icon.mjs       res/sta.ico is complete and one render with the runtime PNGs
docs/                      this file, PROTOCOL.md, TESTING.md (the e2e reference),
                           MCP.md (AI agents, + MCP.ko.md), research/
```

Build prerequisites (Windows): Rust stable (MSVC), Visual Studio 2022 C++ tools, CMake and Ninja
on `PATH` (`pip install --user cmake ninja`), then `cargo build`. The first build downloads CEF
(~600 MB extracted) into `.cef/`. `cef-dll-sys` copies `libcef.dll`, the `.pak` files and
`locales/` next to the exe in `target/<profile>/`. Build from a short path: the CEF wrapper's CMake
build fails with `C1083` under very long directories.

Build prerequisites (macOS): Rust stable, the Xcode command line tools, CMake and Ninja on `PATH`
(`brew install cmake ninja` — the CEF C++ wrapper is compiled from source), then `cargo build`.
Nothing is copied next to the exe there: libcef is a framework the binary loads at runtime from
inside an app bundle, so a debug build assembles `target/<profile>/sta.app` around itself and
re-executes into it (§9, `crates/sta/src/platform/mac_bundle.rs`).

> **Editing files on Windows:** don't round-trip UTF-8 source files through Windows PowerShell 5.1
> `Get-Content`/`Set-Content`. It reads them as the ANSI code page and corrupts non-ASCII text. Use
> an editor, Git Bash tools, or `[IO.File]::ReadAllText(path, [Text.Encoding]::UTF8)`.

---

## 2. Big picture

```
 ┌──────────────────────────── browser process (UI thread = main thread) ─────────────────────────┐
 │                                                                                                │
 │  HTML UI surfaces ──IPC (message_router)──► ipc.rs ──┐                                          │
 │  (sidebar, topbar, command bar,                      │ enqueue                                  │
 │   settings, archive, …)                              ▼                                          │
 │        ▲                                   controller: FIFO of Commands ──(posted task)──┐      │
 │        │  `state` event (UiState, ≤30 Hz)          ▲                                     ▼      │
 │        └────────────── controller ◄────────────────┼───────────────────────── core::Store.apply │
 │                                                    │ enqueue                             │      │
 │  CEF callbacks (title, url, loading, popups, ...) ─┘                                     │      │
 │                                                                                          ▼      │
 │  window.rs / tabs.rs / overlays.rs ◄──────────────── execute(Effect) ◄─── effects ◄──────┘      │
 └────────────────────────────────────────────────────────────────────────────────────────────────┘
      renderer processes: the same exe; RenderProcessHandler = message router (UI pages only)
                          + boost CSS/JS injection + media play/pause reporting (web tabs)
```

Shell modules (`crates/sta/src`): `controller` (dispatch loop, saving), `window` (window, docked
views, sidebar placement, chrome colors, drag regions, page fullscreen, shutdown), `overlays`,
`rounded` (rounded corners: content corner masks, overlay cards, §4.4),
`sidebar_hover` (hover reveal of the hidden sidebar, §4.3), `tabs` (tab browsers,
content layout, popups, boosts delivery, page actions), `client` (+ `context_menu`, `error_page`:
CEF clients and handlers), `browsers` (live-browser registry and roles), `ipc`, `renderer`,
`scheme`, `keyboard`, `downloads`, `permissions`, `external` (external protocols), `suggest`
(remote search suggestions, §5.1), `automation` (AI agents over MCP, §5.2),
`foreign` (+ `extension_files`, `platform/hidden_windows`: browsers Chromium creates for
extensions, §4.5), `extensions` (+ `ext_popup`, `ext_shim`, `ext_backend`, `safe_mode`: the
installed-extensions listing, the Ctrl+E popup card and the tab it tells the extension about, turning
extensions on and off, the crash-loop guard, §4.6),
`devtools` (+ `devtools_policy`, `devtools_shim`: DevTools docked in the window,
§4.1), `devtools_cdp` (the shell's own in-process DevTools client and the docked frontend's session
bridge, §5.2),
`motion` (the shell's own animation timing, §4.7),
`platform` (Win32), `app`, `paths`, `task`, `log`, and `debug` + `test_hooks/` (debug builds only).

### 2.1 The dispatch loop (controller.rs) — the rule that keeps CEF happy

CEF Views calls delegate and handler callbacks **synchronously inside** many of our calls
(`window_create_top_level`, `add_child_view`, dropping a BrowserView, `close_browser`, `layout`, …).
The message router also holds a mutex while it runs our IPC handler, and CEF runs
`on_before_browse` synchronously during navigations. If a `RefCell` borrow, a `Mutex` guard or
the router lock is held when that happens, the callback panics or deadlocks. A panic inside an
`extern "C"` callback **aborts the process**.

So:
1. **`controller::dispatch(cmd)` only enqueues** onto a FIFO. If no drain task is pending, it
   posts one with `post_task(ThreadId::UI, …)`. Commands never run inside an IPC handler or a CEF
   callback. Every source (IPC, accelerators, CEF events) uses this one FIFO, so ordering is
   preserved: e.g. `PopupAdopted` is always queued before that popup's first `TabAddressChanged`.
2. The drain task pops a command, **borrows the Store, calls `apply`, and releases the borrow**,
   then executes the returned effects one by one against CEF, with no store borrow held. Commands
   enqueued meanwhile (by synchronous callbacks) are processed by the same drain, after the
   current effects.
3. Inside CEF callbacks only **read-only store queries** (`store.tab()`, `intercept_navigation`,
   `boosts_for_url`, …) and `store.alloc_id()` are allowed synchronously. Take a short borrow,
   copy the data out, and release it.
4. After the queue is empty:
   - if `store.revision()` changed, schedule one coalesced `state` push (max ~30/s, snapshot taken
     when sending, not when scheduling);
   - if `store.take_dirty().any()`, schedule a debounced save (1 s). Serialize on the UI thread
     and write on a background thread; the final save on quit is synchronous.
5. Shell runtime registries (tab id → BrowserView/wrapper panel, browser id → tab id, overlay
   controllers) live in their own `thread_local! RefCell`s. **Pattern: clone the handles out in a
   `let` statement, end the borrow, then call CEF.** Never keep a `Ref` alive across a CEF call:
   no `if let Some(v) = x.borrow().y.clone() { cef_call() }`.

All state is UI-thread-only. The only multi-threaded pieces are the scheme handler (IO/worker
threads, serving immutable embedded assets), message-router callbacks (`success_str` may be
called from any thread), the save writer thread, and short-lived worker threads for blocking OS
calls (folder picker, `ShellExecuteW` for downloads and external protocols, show in folder). Those
threads never touch shell state; they post results back with `task::post_ui_from_any_thread`.
Search suggestion URL requests (§5.1) are created on the UI thread, so CEF calls their client
there too.

A panic inside any `extern "C"` callback aborts the process, so callback paths contain no
`unwrap`/`expect` (thread spawn failures, missing handles etc. are logged and handled);
`Store::apply` and effect execution additionally run under `catch_unwind`.

---

## 3. Process bootstrap (main.rs, app.rs)

Following `platform.md` §1:
- `api_hash(sys::CEF_API_VERSION_LAST, 0)` is the first call in every process.
- `execute_process(args, Some(&mut app), null)` gets the **same** `StaApp` in every process, so
  the `sta` scheme is registered everywhere and renderers get our RenderProcessHandler.
  A return value `>= 0` means this is a subprocess: exit with that code. Nothing with side effects
  may run before this call.
- Browser process:
  - resolve data dirs;
  - `initialize(settings)`. If it returns 0, `get_exit_code() == 24` means the command line was
    forwarded to an already running instance, so exit 0;
  - `run_message_loop()`;
  - drop every CEF handle;
  - `shutdown()`.
- Settings:
  - `no_sandbox = 1`
  - `root_cache_path = cache_path = <data>/User Data`
  - `persist_session_cookies = 1`
  - `log_file = <data>/Logs/cef.log`
  - `background_color` = opaque frame color
  - `locale` = OS UI language (`GetUserPreferredUILanguages`)
  - `remote_debugging_port` from `STA_REMOTE_DEBUGGING_PORT` (debug builds only; release
    builds also strip `--remote-debugging-*`, `--disable-web-security` and `--load-extension`)
  - `command_line_args_disabled = 0`
- Debug-build-only environment switches (tests):
  - `STA_REMOTE_DEBUGGING_PORT=<port>`: DevTools protocol port;
  - `STA_TEST_CONTEXT_MENU=<label>`: native context menus are not shown; their items are
    logged (`context menu (ui|tab): …`) and the item with that label runs (anything else cancels);
  - `STA_TEST_EXTERNAL_PROTOCOL=1`: external protocol launches are logged
    (`external protocol (test): <url>`) instead of handed to Windows;
  - `STA_DEBUG_FAIL_STARTUP=1`: forces the fatal startup error path (error box, exit 1);
  - `STA_DEBUG_SHUTDOWN_TIMEOUT_MS=<ms>`: shortens the shutdown timeout (§3.1);
  - `STA_DEBUG_HOVER_REVEAL=0`: starts with the sidebar hover reveal's pointer detection off
    (§4.3; the e2e suites set it, `debug.hoverInput` turns it on);
  - `STA_SUGGEST_URL=<template>`: search suggestion endpoint (`{q}` = the percent-encoded
    query) used instead of the engine's (§5.1), for engines that have one;
  - `STA_DEBUG_AGENT_AUTO_APPROVE=1`: agent connections and sites are approved without a
    prompt (§5.2; for suites that aren't about consent — agent-e2e clicks the real prompts);
  - `STA_DEVTOOLS_INTERNAL=1`: allows DevTools on `sta://` pages, which are refused otherwise
    (§4.1 "DevTools"). Debug builds only, and only for working on the UI itself.
- Data dir:
  - `%LOCALAPPDATA%\sta` (release) / `~/Library/Application Support/sta` on macOS;
  - `%LOCALAPPDATA%\sta Dev` (debug) / `~/Library/Application Support/sta Dev`;
  - override with `--sta-data-dir=<abs path>` or `STA_DATA_DIR`, which tests use so they
    never touch the user's profile or collide with the process singleton;
  - resolved from the `LOCALAPPDATA` environment variable (not the Known Folder API), so
    `migration-e2e.mjs` points it at a temporary folder; `HOME` plays that role on macOS;
  - migration from before the rename (`paths::resolve`, decisions in `sta_core::legacy`, all
    before the log file, the panic hook or CEF touch the directories) — **Windows only**: sta has
    never shipped anywhere else under the old name, so elsewhere `paths` offers no legacy folder and
    the steps below are a no-op:
    1. without an override, the legacy default folder (`legacy::DATA_DIR_RELEASE` /
       `DATA_DIR_DEBUG`) is renamed to the new default name (`MoveFileExW` without flags: same
       volume, never replaces) when the new folder doesn't exist yet. Both existing → the new one
       is used and the legacy one is left alone (never merged). An override that names the new
       default folder (compared case-insensitively, separators normalized; a launcher may pass it
       explicitly) counts as no override;
    2. in `<data>` (default or override), the legacy profile subfolder (`legacy::PROFILE_DIR`)
       becomes `sta/` the same way;
    3. a legacy folder whose Chromium profile is in use (`User Data/lockfile` exists and can't be
       opened for writing: Chromium's process singleton holds it share-read, delete-on-close) by
       an old browser (step 4's `sta-in-place.lock` isn't held) is neither moved nor used.
       `resolve` returns `LegacyInUse`, `main` shows an error box ("sta can't start while … is
       running") and exits with code 1, before `initialize`. Using the folder in place would share
       the old browser's `root_cache_path`, so the singleton would forward this launch's command
       line (URLs) to the old browser;
    4. a move that fails otherwise (retried 5 × 200 ms for antivirus/indexer handles) → the legacy
       folder is used in place for this run, keeping its layout (profile subfolder included), and
       that run holds `<legacy folder>/sta-in-place.lock` (`platform::hold_lock_file`: share-read,
       delete-on-close). A launch meanwhile finds the profile in use and that lock held, so it uses
       the folder in place too (`Note::SharingLegacyInPlace`) and the singleton forwards its
       command line to the running sta. The next launch after that run tries the move again. Its
       saved internal URLs are upgraded like any profile's (§7), so an old build started on the
       folder later finds `sta://` pages it can't open. Its `agent-endpoint.json` (§5.2) is in the
       legacy layout too: `sta-mcp` also looks there (`channel::endpoint_candidates`: the legacy
       profile subfolder of its data directory and, for the default data directory, the legacy
       default folder; a legacy-layout file naming a pipe sta never creates is ignored), and
       `agent.info` counts the legacy default folder as the default (no `--data-dir` in the
       setup snippets);
    5. the outcome is logged once the log file is open (`data from before the rename moved: …`,
       `… left alone`, `could not move …`, `… is used in place by a running sta`).
- Single instance: the process singleton is keyed on `root_cache_path`. The running instance gets
  `on_already_running_app_relaunch(cmdline, cwd)`, turns the arguments into URLs (file paths →
  `file:///`, internal page URLs from before the rename → `sta://`), dispatches `OpenUrl{NewTab}`, activates the window and **returns 1**. sta's
  `root_cache_path` differs from the one used before the rename, so a running old build never
  receives sta's command line (see the migration's step 3 for its legacy folder). sta creates no
  other named objects (mutexes, pipes, window classes of its own).
- `App::browser_process_handler()` returns one stored instance (it is called many times, on
  several threads).
- Windows resources: `build.rs` embeds a manifest (PerMonitorV2 DPI, supportedOS, Common Controls
  v6), the icon and VERSIONINFO via `embed-resource` (`platform.md` §5). Release builds use
  `#![windows_subsystem = "windows"]`.
- Startup order in `on_context_initialized`:
  1. load the store (quarantine corrupt files), then reset the one-time permission grants of the
     previous session (§4.1 "Permissions");
  2. register the scheme factory and IPC handler;
  3. dispatch `SystemThemeChanged{dark}` (registry `AppsUseLightTheme`);
  4. create the window at `store.window_state()` bounds, clamped to an existing display, and
     maximized if it was;
  5. in `on_window_created`, after the view tree exists, run `store.startup(urls)` effects
     (the only effects that run inside a CEF callback; commands they enqueue are drained by a
     posted task);
  6. start the 60 s `Tick` timer (a 5 s heartbeat that also ticks after wall-clock jumps and polls
     the OS theme).

  If the view tree can't be built, an error box is shown and the process exits with code 1, so no
  invisible instance keeps the process singleton.

### 3.1 Shutdown sequence

Alt+F4, the taskbar, the caption close button, `Quit` and Ctrl+W with nothing to close all take
the same path:
1. `WindowDelegate::can_close` (first call): dispatch `WindowCloseRequested` and return 0.
   The caption button and `Quit` dispatch the command directly.
2. Core sets shutting-down and returns `[SaveNow, Quit]`. From then on it ignores every command,
   so browser teardown can't archive tabs.
3. `Effect::SaveNow`: write `state.json` and `history.json` synchronously.
4. `Effect::Quit`:
   - set `closing = true`;
   - cancel every download in progress (`downloads::cancel_all_in_progress`), close every docked
     DevTools frontend (`devtools::close_all`), then close the browsers Chromium created for
     extensions (`foreign::close_all`, §4.5): closing Chromium's last window with downloads running
     could otherwise ask about them;
   - close DevTools, then `close_browser(1)` on every browser (tabs, Peek, UI views, overlays);
   - the beforeunload handler auto-accepts;
   - `do_close` returns 1 and posts the detach.
5. When the last `on_before_close` fires, call `window.close()` again. `can_close` now sees
   `closing` with no live browsers and returns 1. "Live" is `browsers::live_count()` **plus**
   `browsers::extra_count()`, the Chrome-created browsers sta doesn't host (§4.5); their client
   calls `window::on_browser_closed` itself.
6. `on_window_destroyed`: drop every handle, `quit_message_loop()`. `main` then calls
   `shutdown()`.

If browsers are still alive 8 s after `Quit`, state is saved and the process exits without
`cef::shutdown()` (which must never run with live browsers). If core returns no `Quit` for a close
request (e.g. it already set shutting-down after a panic), the controller runs `[SaveNow, Quit]`
itself, so the window can always be closed.

---

### 3.2 macOS bootstrap (platform/mac.rs, platform/mac_bundle.rs)

Three things are different before `api_hash` on macOS, and `main` does them in this order:

1. **The app bundle.** libcef is not linked into the binary there: it is a framework loaded at
   runtime from `Contents/Frameworks`, and every child process is started from a helper *bundle* so
   the OS gives it the right process type and keeps it out of the Dock. `mac_bundle::ensure_bundled`
   therefore refuses to run outside one. A **debug** build instead assembles
   `target/<profile>/sta.app` around itself — Info.plist, five helper apps whose executables are
   hard links to the binary, and a symlink to the CEF distribution `build.rs` found — and
   `exec`s the copy inside it, keeping the terminal and its output. `--sta-bundle-mac[=<dir>]`
   builds a standalone bundle (framework copied, helpers copied so each can be signed) and exits;
   that is what `tools/package-release.mjs` ships.
2. **The framework.** `mac_bundle::load_framework` `dlopen`s
   `Contents/Frameworks/Chromium Embedded Framework.framework` in *every* process, before the first
   CEF call.
3. **`NSApp`.** `cef_initialize` checks that the application object implements `CefAppProtocol`, so
   `platform::init_application` creates `NSApp` from sta's own `NSApplication` subclass
   (`StaApplication`, which tracks `handlingSendEvent` like Chromium's `CrApplication`), sets the
   activation policy and installs the menu bar. Only the browser process needs it. The delegate's
   `applicationShouldTerminate:` routes ⌘Q and the Dock's Quit into the window's own close path, so
   the session is saved before CEF shuts down.

`Settings` then carries three more paths: `framework_dir_path`, `main_bundle_path` and
`browser_subprocess_path` (the generic `sta Helper`). Debug builds also pass
`--use-mock-keychain`: Chromium keeps the cookie encryption key in the login keychain, and an
unsigned binary that changes on every `cargo build` can never hold on to that key's ACL — without it
every run stops on a keychain password prompt, and the shutdown that reads the key again hangs
behind it.

Shortcuts are the other visible difference: `Window::set_accelerator` has no flag for ⌘, so on macOS
the table in `keyboard.rs` is not registered as accelerators at all — it is matched against the key
events the handlers already see and routed through the same `on_accelerator` (§4.2).

---

## 4. Window and view tree (window.rs)

All windows and BrowserViews use **`RuntimeStyle::ALLOY`**. The WindowDelegate returns it from
`window_runtime_style` and *every* BrowserViewDelegate returns it from `browser_runtime_style`.
An Alloy window refuses Chrome-style views. Chrome style allows only one BrowserView per window
and no `do_close`, which rules it out for us (`views.md` §6a). DevTools is the exception: it is
always Chrome style in its own window, so return `None` for its delegate.

```
Window  (frameless; can_resize/maximize/minimize = 1 — Rust defaults are 0!; horizontal BoxLayout;
         titlebar_height = 40 so CEF dialogs sit below the topbar)
├── Sidebar  BrowserView  sta://sidebar/               flex 0, width = preferred_size from a Cell
│                                                     (while hidden: parked in overlay host 5, §4.3)
└── Right column Panel  (vertical BoxLayout, flex 1)
    ├── Topbar  BrowserView  sta://topbar/            preferred_size {1, 40}
    └── Content frame Panel  (flex 1, background = frame color, insets: top 0, right 8,
        │                     bottom 8, left 0 (8 when the sidebar is hidden; 0 in page fullscreen))
        └── Content Panel  (BoxLayout, background = frame color)
            ├── Empty-state BrowserView  sta://empty/  (visible only for layout = Empty)
            └── Tab wrapper Panel × N   (BoxLayout, inside_border_insets 2 (0 in page fullscreen),
                │                        STRETCH, preferred_size {1,1}; background = accent when
                │                        it is the focused pane of a split, else frame; the
                │                        agent color while an agent acts on the tab, §5.2)
                └── tab BrowserView      (one per *loaded* tab, flex 1)
Content corner masks (rounded.rs, §4.4): 16 image overlays created first, lowest z-order.
Overlay hosts, created in on_window_created in this z-order (lowest first). Each is a transparent
Panel holding a rounded card (§4.4: corner images, border, shadow; its inner panel parents the
views), CUSTOM docking, hidden:
    1. Peek        card(vertical)[ header BrowserView sta://peek/ (40) | tab BrowserView (moved in) ]  can_activate 1
       + 4 Peek page corner masks right above it
    2. Command bar BrowserView sta://command/          can_activate 1   (created at startup: pre-warmed)
    3. Find bar    BrowserView sta://find/             can_activate 1
    4. Permission  BrowserView sta://permission/       can_activate 1
    5. Agent       BrowserView sta://agent/            can_activate 1   (AI agent prompts / activity panel, §5.2)
    6. Floating sidebar  [ the parked Sidebar BrowserView while the sidebar is hidden ]  can_activate 0
    7. Switcher    BrowserView sta://switcher/         can_activate 0
    8. Toast       BrowserView sta://toast/            can_activate 0
```

- **Overlay views** are created lazily on first show, except the command bar, which is pre-warmed
  at startup. An overlay page missing from `ui/` (`find`, `permission`, `switcher`, `toast`,
  `peek`) is served a small built-in placeholder page (`res/overlay-placeholder.{html,js}`: renders
  the intent from `UiState`, sizes itself, sends `ui.ready`); the real page always wins.
- **Overlay visibility**: `Show*`/`Hide*` effects toggle `OverlayController::set_visible`. The
  shell makes an overlay visible only after that surface has sent `ui.ready` at least once, so a
  cold page never flashes blank (Peek alone shows after 2 s without a ready header: its content
  is the web page). Hiding the **toast** and the **switcher** waits for their page to present a blank
  frame first (`surface.exit` → `surface.exited`, at least 50 ms, at most the cap): they **linger**
  visible until then, and a show meanwhile cancels the exit (§4.7). Everything else hides at once.
- **Overlay z-order**: showing an overlay's widget raises it above all others, whatever the
  creation order. So whenever an overlay becomes visible, every visible overlay that belongs above
  it is hidden and shown again, lowest first (e.g. a permission prompt or a focused command bar
  that is up when Peek appears stays above Peek). Re-showing an activatable overlay moves keyboard
  focus through it (hiding a focused one hands focus to the main window), so while a restack runs
  a *focus guard* (≈50 ms) suppresses the focus policy and `TabFocused`; the intended target is
  focused again (through the overlay just shown when the re-shown one had focus: Views still
  counts its view as focused while its page lost focus), and when the guard ends the policy runs
  once for the browser that ends up focused. Peek never takes focus from a visible command bar,
  find bar, permission prompt or agent overlay above it (e.g. Peek becoming ready late while the
  user types).
- **Overlay bounds** are recomputed in `WindowDelegate::on_layout_changed`, on content,
  sidebar and fullscreen changes and on `surface.setSize`, relative to the content panel rect.
  The rects below are the visible **card**; the page inside is smaller by the card's inner chrome
  and the host bigger by its shadow, snapped to device pixels (§4.4).
  (`View::convert_point_to_window` does not write its result back in cef 152.3.0:
  `window::view_rect_in_window` sums parent bounds instead, and uses the overlay controller's
  bounds for views inside overlay hosts.)
  - command bar: width `clamp(480, 0.56·content_w, 680)`, x centered on the content, y =
    content_top + max(72, 0.14·content_h), height from the page;
  - find bar: 360×44 (page size 344×36 plus the card, clamped) at the top-right of the target
    pane (the tab's view in Peek, else its wrapper), 8 px inset;
  - permission: 340×(page) at the top-left of the target pane, 8 px inset;
  - agent: `clamp(380, 300, 460)`×(page) at the top-right of the content, 8 px inset (an overlay
    card like the permission prompt: `--overlay-radius`, 8 DIP shadow);
  - switcher: centered on the content, shown 250 ms after `ShowSwitcher` unless hidden meanwhile;
  - toast: bottom-center of the content, 12 px above its bottom edge (at least 32 high: two 16 DIP
    arcs);
  - Peek: width `min(content_w − 96, 1200)`, height `content_h − 56`, centered, top +28;
  - floating sidebar: `{8, 8, min(sidebar width + 8, client_w − 16), client_h − 16}` in window
    coordinates (inside the content inset; the page inside keeps the sidebar width; the host with
    its 4 DIP shadow stays clear of the 4 DIP resize bands).
- **Focus policy** (`FocusHandler::on_got_focus` of every browser → `overlays::on_browser_got_focus`,
  which only enqueues):
  - a tab gaining focus dispatches `TabFocused{tab}`;
  - any browser but the command bar gaining focus while it is visible → `CloseCommandBar` (a press
    in the floating sidebar, which never takes focus, too: §4.3);
  - any browser but Peek's tab, Peek's header, the command bar, the find bar, the permission
    prompt or the agent overlay (they work *on* Peek) gaining focus while Peek is visible →
    `ClosePeek{focusLost:true}` (core ignores it for popup Peeks);
  - any browser but the agent overlay gaining focus while it is visible →
    `CloseAgentPanel{focusLost:true}` (core ignores it while an approval prompt is shown);
  - the agent overlay requests focus when shown like the permission prompt, except an approval
    prompt while the user is typing (any key in a browser in the last 2 s, `automation::ui`);
  - the floating sidebar is never a focus target: `browser_in_overlay` maps the parked sidebar to
    it, `restore_main_focus` never leaves focus there, and `focus_fallback` skips a parked sidebar
    (its `is_drawn()` stays 1 while the host is hidden, verified: picking it would bounce focus);
  - keyboard focus never stays in a hidden overlay (Views may give it to the pre-warmed command
    bar or to a lazily created overlay view; hiding a focused overlay leaves it there): a hidden
    overlay swallows key-downs, so focus moves (posted) back to the visible command bar, find bar,
    permission prompt or agent overlay it came from, else to core's focused tab if visible (not a
    tab an agent just showed with `tab_show`, `automation::ui::may_restore_focus_to`), else the
    empty-state view, the sidebar or the topbar;
  - keyboard focus never stays in a hidden **tab** either: after every `ShowContent` (e.g. `Empty`
    when switching to a space without tabs), a posted check moves focus that is still in a tab
    view the layout hid the same way (a hidden page never hands keys back, so page-first
    accelerators such as Alt+1..9, Ctrl+S, Ctrl+L, Ctrl+D, Ctrl+J would stop working);
  - `FocusHandler::on_set_focus` cancels focus for tabs that aren't visible (background loads).
  A docked DevTools frontend reports `TabFocused` for the tab it inspects, so that tab stays the
  focused one (Ctrl+L, Ctrl+R and the like keep acting on the page being inspected); DevTools on a
  Peek page opens undocked, in CEF's own window, which has no shell focus handler at all, so
  inspecting a Peek page doesn't close Peek.
- **Content layout** (`Effect::ShowContent{layout}`):
  - `Empty`: only the empty view is visible.
  - `Single`: only that tab's wrapper is visible.
  - `Split`: re-set the content panel's BoxLayout with `horizontal = orientation == Horizontal`
    and `between_child_spacing = 6`. Reorder the pane wrappers to indices 0..n
    (`reorder_child_view`), set flex = `round(fraction·1000)` (preferred size {1,1} so fractions
    decide), color the focused wrapper accent and the others frame. Hide everything else.
  - Then `content.layout()`.
  - Hidden children take no BoxLayout space. Hiding a BrowserView sets its WebContents to HIDDEN
    (throttled). Never detach views to switch tabs (`views.md` §6b). `was_hidden` is OSR-only;
    don't call it.
  - A tab view that currently sits in the Peek overlay is moved back into its wrapper first.
- **Page fullscreen** (`Effect::SetPageFullscreen{tab}`) = `window::set_page_fullscreen` +
  `tabs::set_page_fullscreen_tab`:
  - `Some(tab)`: hide the sidebar and topbar, drop the content insets, fullscreen the window
    (remembering whether it already was, F11); show only that tab's wrapper with a 0 px inset (no
    2 px frame line, other panes hidden). A Peek tab is taken out of the Peek overlay into its
    wrapper for the duration;
  - `None`: restore the chrome, the window state and the last `ShowContent` layout; a tab that came
    from Peek goes back into the (visible) Peek;
  - `ShowContent` while in page fullscreen keeps the fullscreen presentation if the tab is still
    part of it; destroying the fullscreen tab restores the layout.
- **Sidebar width**: `Rc<Cell<i32>>` read by `SidebarDelegate::preferred_size` (height must be > 0).
  Change it with `cell.set`, then `sidebar.invalidate_layout()` and `window.layout()`.
  `sidebar.setWidth` from IPC applies live; `SetSidebarWidth` persists.
- **Colors** (`Effect::SetChrome{frame_argb, dark, accent_argb, surface_argb, border_argb,
  frame_border_argb}`, the last four `theme::chrome_argb`, `0` = derived by the shell for older
  JSON):
  - apply `set_background_color(frame_argb)` to the window, right column, content panel and
    unfocused wrappers; re-apply in `on_theme_changed` (CEF resets it when views are added);
  - recolor the corner masks and the overlay cards (§4.4);
  - `DWMWA_USE_IMMERSIVE_DARK_MODE = dark` and `DWMWA_WINDOW_CORNER_PREFERENCE = ROUND`;
  - `BrowserSettings.background_color` for new UI views = frame color, and white for web tabs.
- **Frameless drag**:
  - sidebar/topbar HTML uses CSS `app-region: drag` (and `no-drag` on controls);
  - the UI client's `DragHandler::on_draggable_regions_changed` stores each UI view's regions;
  - `apply_draggable_regions()` clips them to their view, offsets them by the view's origin
    (`view_rect_in_window`), adds `draggable: 0` rects for visible overlays, and calls
    `window.set_draggable_regions`. A parked sidebar contributes no regions (the floating sidebar
    is a no-drag hole like every visible overlay);
  - re-run it (coalesced, posted) on layout changes, sidebar toggles, overlay show/hide/resize and
    page fullscreen (no regions while the sidebar and topbar are hidden);
  - tabs' clients have **no** DragHandler, so web pages can't define drag areas.
- **Caption buttons**: min/max/close are HTML buttons in the topbar and dispatch
  `WindowControl{…}`. `WindowStateChanged` updates `state.window.maximized` so the UI can swap the
  glyph.

### 4.1 Tabs (tabs.rs, client.rs)

- **`CreateBrowser{tab,url,internal,muted}`**:
  1. create the wrapper Panel (hidden) and add it to the content panel;
  2. `browser_view_create` with the **UI client** + `extra_info {sta_ui: true}` when
     `internal`, else the **tab client** + `extra_info {sta_tab: tab, boosts, boosts_version}`
     (boosts of the URL's host only); the per-tab `TabDelegate{tab}` returns Alloy and handles
     popups;
  3. add it to the wrapper (the browser is created synchronously here);
  4. map `browser.identifier() → tab` in `TabDelegate::on_browser_created`, which fires after
     `on_after_created` and carries the tab id;
  5. dispatch `TabBrowserCreated`, and mute the host if `muted`.

  If creation fails, dispatch `TabBrowserClosed`. Core never creates a tab for a typed
  external-protocol URL (`Effect::OpenExternal`), but a tab whose stored URL is one (a pinned URL
  edited to `mailto:…`, an older profile) hands it to the OS (not during session restore) and
  loads `about:blank` instead.
- **`ReplaceBrowser`**: create the new view in the same wrapper, then force-close the old one
  silently (don't report `TabBrowserClosed` for it).
- **Close** (`DestroyBrowser`, `views.md` §5 and `handlers.md` §5.3):
  1. mark the tab as closing, then `host.close_browser(1)`; beforeunload is auto-accepted via
     `JsdialogHandler::on_before_unload_dialog` → `cont(1)` for closing browsers;
  2. `LifeSpanHandler::do_close` **returns 1** and posts a task;
  3. the task calls `wrapper.remove_child_view(view)` and `content.remove_child_view(wrapper)`,
     then **drops every reference**;
  4. CEF destroys the browser;
  5. `on_before_close` unregisters the tab and dispatches `TabBrowserClosed{tab}`.

  Never drop the last reference synchronously inside `do_close`. Never keep a reference after
  removal. Per-tab cleanup belongs in `on_before_close`; `on_browser_destroyed` doesn't fire on
  this path.
- **Popups (adopt all, Strategy B, `handlers.md` §5.1)**:
  - web tabs' `on_before_popup`: cancel `sta://` targets; hand external-protocol targets to
    the OS (§4.1 "External protocols") and cancel; PiP keeps CEF's default window (queued as "not
    adopted"); for everything else **allocate the tab id right here** (`store.alloc_id()`, short
    borrow), set the popup's `extra_info` (tab id + host-matched boosts), queue
    `{popup_id, tab, url, popup: features.is_popup or disposition == NEW_POPUP, foreground:
    disposition != NEW_BACKGROUND_TAB}` per opener browser, and return 0 (allow);
    `on_before_popup_aborted` drops the queued entry;
  - core's `PopupAdopted` rules for the URL: an empty URL or `about:blank` (`window.open()` +
    `document.write`) is shown as `about:blank` and **never navigated** (the popup renders its own
    content); any other URL outside the web-content allowlist (`urls::web_content_may_open`:
    http(s) with a host, `about:blank`, `blob:` of an http(s) origin, and `file:` only when the
    opener itself is a `file:` page) is replaced by `about:blank` (`LoadUrl`) with a "Blocked a
    `scheme:` link" toast. The same allowlist guards `LinkOpenRequested` and `OpenUrlAt`;
  - `TabDelegate::delegate_for_popup_browser_view` gives the popup a `TabDelegate` with that id
    (DevTools: none — an *undocked* DevTools window is CEF's own; the docked frontend is not a popup
    at all, it is a BrowserView sta creates, §4.1);
  - `TabDelegate::on_popup_browser_view_created` (fires **before** `on_after_created`): pop the
    queue entry, create a wrapper, add the popup view hidden, map browser id → tab, enqueue
    `PopupAdopted{tab, opener, url, popup, foreground}` and return 1 (0 = CEF's own window, e.g.
    DevTools/PiP);
  - UI pages never open windows: their popups are cancelled and become `OpenUrl{NewTab}`.
- **`RequestHandler::on_open_urlfrom_tab`** (middle/Ctrl+click): cancel (return 1) and dispatch
  `LinkOpenRequested`. With Alloy, not cancelling loads the URL in the *current* tab.
- **`RequestHandler::on_before_browse`** (main frame):
  - web tabs → cancel `sta://` URLs (any frame); cancel external protocols (handed to the OS
    with a user gesture);
  - then `store.intercept_navigation(tab, url, user_gesture, is_redirect)`; if `Some(d)`, cancel
    and dispatch `LinkOpenRequested{d}`;
  - otherwise push the target host's boosts to the renderer when the navigation is same-site;
  - UI surfaces (sidebar, topbar, overlays) are locked to their own host: cancel and dispatch
    `OpenUrl{NewTab}` for anything else;
  - internal-page tabs may move between `sta://` hosts only; anything else becomes
    `Navigate` (core replaces the trusted browser by a web one).
- **UI location guard**: `on_before_browse` is never called for `about:blank`/`about:srcdoc`, so
  every main-frame address change and load start of a UI browser is checked too. A surface that
  left its `sta://<host>/` reloads its URL (posted); an internal-page tab dispatches
  `Navigate{url}` (→ `ReplaceBrowser` with a web client). The escaped document's address and
  title are not reported. The renderer never injects IPC into a non-`sta://` document, and
  the browser side re-checks the frame URL on every query.
- **Address, title, loading, favicon**: `DisplayHandler` / `LoadHandler` → `TabAddressChanged`,
  `TabTitleChanged`, `TabFaviconChanged` (first URL), `TabLoadingStateChanged`,
  `TabLoadProgress` (throttled to ~10/s per tab). A browser whose close is in progress (e.g. the
  old browser of a `ReplaceBrowser`) reports nothing.
- **Load errors**: `on_load_error` (main frame, not `ERR_ABORTED = -3`) → dispatch `TabLoadFailed`.
  For web tabs, Chromium has already committed its `chrome-error://chromewebdata/` document into
  the *failed* navigation entry; the shell **redraws that document in place** with
  `execute_java_script` (themed card, error text, failed URL as text nodes, Retry =
  `location.reload()`). No `data:` URL and no extra history entry: the address stays the failed
  URL, Back/Forward work, Chromium's auto-reload still replaces it. `chrome-error:` addresses and
  the error document's title/favicon are never reported (the title falls back to the host).
  Internal pages keep Chromium's error page.
- **Crashes**: a web tab's `on_render_process_terminated` → `TabCrashed`; a crashed UI surface
  reloads itself (at most 5 times per minute); a crashed internal-page tab → `TabCrashed`.
- **Zoom**: `Effect::Zoom{direction}` steps through Chrome's preset levels
  (25,33,50,67,75,80,90,100,110,125,150,175,200,250,300,400,500 %) with `set_zoom_level`. Then, and
  on `on_load_end`, read `zoom_level()` in a posted task and dispatch `TabZoomChanged`. Chromium
  persists zoom per host itself.
- **Alt+click preview** (PROTOCOL §13): Alt+click, or Alt+middle-click, on a link opens it in Peek
  instead of navigating. Chromium resolves Alt+click to *download this link* **inside the
  renderer** (measured: `downloadUpdated` fires and the file lands; `on_before_browse` and
  `on_open_urlfrom_tab` are never called), so there is nothing for a request or life-span handler
  to intercept and nothing `CefDownloadHandler` could tell apart from a real download. The gesture
  is therefore caught where Chromium makes the decision: the tab renderer installs a capture-phase
  `click`/`auxclick` listener on `window` at `on_context_created`, before any page script runs
  (`renderer.rs`, next to the media reporter and with the same closure-argument native function —
  web tabs never get a global), in every frame whose origin is **not opaque**: a sandboxed frame
  (`sandbox` without `allow-same-origin`, a `data:` frame) was denied even `target=_blank` by its
  embedder, so one user Alt+click there must not become a top-level page of the frame's choosing.
  A trusted (`Event.isTrusted`) Alt+left or Alt+middle **pointer** click (`UIEvent.detail >= 1`)
  whose `composedPath` contains a link — `<a>`, `<area>`, an SVG `<a>`, one inside a shadow tree —
  is cancelled with `preventDefault` + `stopImmediatePropagation`, which is what suppresses the
  download, and its absolute URL goes to the browser as `sta.preview`. The `detail` check is what
  separates a click from a **key**: Blink's keyboard activation of a focused link (Alt+Enter, a
  screen reader) dispatches an equally trusted click with `detail === 0`, and `press_key` is a
  shipped agent tool. Two shapes are deliberately handled differently, because an Alt+click Chromium
  keeps *saves the page to disk* (measured): a `javascript:` href is left completely untouched (the
  page's own button must keep working, and Chromium saves no file for those), while a same-document
  link (same URL apart from the fragment) and a `file:` URL from a non-`file:` document are cancelled
  but **not** hidden from the page and never reported. An href over `MAX_PREVIEW_URL` stays cancelled
  and is dropped with a log line. The tab client
  (`client.rs::on_process_message_received` → `preview_target`, never the router) checks that the
  sender is a web tab, that it is **not agent-controlled** (`guards::allow_peek`, the same gate
  `intercept_navigation` uses), that no preview was accepted for that browser in the last 250 ms, and
  that a `file:` URL comes from a `file:` **frame** (core's allowlist sees only the *tab's* URL, which
  a remote iframe inside a saved page must not borrow); then it dispatches
  `LinkOpenRequested{disposition: Preview}` and core applies the ordinary web-content URL allowlist
  and the Peek rules (PROTOCOL §13). Nothing about the gesture is trusted from the page: `isTrusted`
  is unforgeable, and the listener reads `altKey`, `button`, `detail`, `composedPath`, the anchor
  `href` getters and `Document.prototype.URL` it captured at install time, so a page that redefines
  them later cannot steer it. A **compromised renderer** can still send the message for any URL the
  allowlist accepts, with nothing here corroborating a real click (the browser process never sees
  content-area input) — the rate limit is the bound on that, not user activation. Two residues are
  documented rather than fixed: a frame sandboxed *with* `allow-same-origin` keeps the gesture, and a
  frame where scripting is off never runs the listener at all, so Chromium's Alt+click download
  survives there. The page never sees an Alt+click on an ordinary link
  (`stopImmediatePropagation`): that is the point — otherwise an SPA router would navigate the tab as
  well as opening the preview — and the shapes where it matters (`javascript:`, `href="#"`) are the
  exceptions above.
- **Audio**: the tab renderer (RenderProcessHandler, web tabs only) listens in the capture phase
  for `play`, `playing`, `pause`, `ended`, `emptied` and `volumechange` (plus a
  `HTMLMediaElement.prototype.play` hook for detached `new Audio()`) and sends
  `sta.media{audible}` process messages per frame through a native function passed as a
  closure argument (never a global). The tab client's `on_process_message_received` (never the
  router) aggregates frames and dispatches `TabAudioChanged` debounced (250 ms, only on changes;
  reset on new main-frame documents and crashes). Mute uses `host.set_audio_muted`.
- **Downloads** (`handlers.md` §9):
  - `can_download` → 1 (Rust default 0!);
  - `on_before_download` → `callback.cont(path, ask_download_location)` with a de-duplicated path
    (`name (n).ext`) in the download dir (`Settings.download_dir` when absolute and creatable, else
    the user's Downloads folder; native separators). The suggested name is sanitized (path
    separators, reserved characters, trailing dots/spaces, DOS device names like `CON`/`nul.txt`);
  - `on_download_updated` → `DownloadUpdated` (tab = the browser's tab), plus
    `DownloadInBlankTab{tab}` once per download when that browser has **no document at all** — a Peek
    opened for a link the server answered with `Content-Disposition: attachment`. Core closes that
    empty overlay (`store/events.rs::download_in_blank_tab`) and the download toast is the whole
    outcome; the browser stays alive until the file arrives, because `release_browser` defers while a
    download is in progress;
  - **Mark-of-the-Web**: when a download completes, before `DownloadUpdated{complete}` is
    dispatched (so "Open" never runs on an unmarked file), the shell writes the
    `<path>:Zone.Identifier` alternate data stream: `[ZoneTransfer]`, `ZoneId=3`,
    `ReferrerUrl=<page that started the download>` (omitted when unknown or not http(s)),
    `HostUrl=<download URL>` (`about:internet` for `data:`/`blob:`/`file:` sources); credentials
    are stripped and URLs longer than 2083 characters are not written. SmartScreen and Office
    Protected View use it when the file is opened (CEF's Alloy runtime doesn't annotate
    downloads; on volumes without alternate streams it is only logged);
  - keep the `DownloadItemCallback` per id for pause/resume/cancel (also for interrupted ones);
  - Open / ShowInFolder use `ShellExecuteW` / `SHOpenFolderAndSelectItems` on a worker thread;
    Retry is core's `StartDownload{tab, url}` → `host.start_download`.
- **Permissions** (`permissions.rs`, `handlers.md` §10.2):
  - `PermissionHandler::on_request_media_access_permission` and `on_show_permission_prompt` store
    their callbacks by a shell id and dispatch `PermissionRequested`.
    `on_dismiss_permission_prompt` dispatches `PermissionDismissed`; pending requests of a closing
    browser are dropped and reported dismissed.
  - `AnswerPermission{id, allow, remember}` answers: `allow` → media `cont(requested)` / prompt
    `ACCEPT`; `!allow && remember` (Block with "Remember", or a remembered block) → media `cont(0)`
    / prompt `DENY`; `!allow && !remember` (Block without "Remember", stale or invalid requests) →
    media `cancel()` / prompt `DISMISS`, because a `DENY` makes Chromium refuse that origin for the
    rest of the session instead of asking again.
  - **No auto-block**: Chromium's `PermissionDecisionAutoBlocker` embargoes an origin for 7 days
    after 3 dismissed (or 4 ignored) prompts, across restarts. Chromium 152 has no feature switch
    for it any more (`BlockPromptsIfDismissedOften`/`BlockPromptsIfIgnoredOften` are gone from
    `libcef.dll`; unknown `--disable-features` names are silently ignored). Its counters live in
    the per-origin `PERMISSION_AUTOBLOCKER_DATA` website setting, so the shell removes that setting
    (`RequestContext::set_website_setting(origin, none, type, none)`) right after every `DISMISS`,
    after prompts CEF dropped (navigation, closed tab), and for the origin of every main-frame
    commit that has any (which also lifts embargoes left in older profiles).
  - **Allow without "Remember" = allow this time**: CEF's `ACCEPT` stores a permanent content
    setting (Chrome's `AcceptThisTime` isn't exposed; media access stores nothing). Before
    answering, the shell records `{origin, topLevel, bits}` in
    `<profile>/one-time-permissions.json`. A coalesced sweep (posted after main-frame commits and
    tab browser closes, never while the window is closing) resets the content settings the CEF
    request bits map to (`permissions::content_types`, Chromium's
    `RequestTypeToContentSettingsType`) to `DEFAULT` once no live tab's main frame shows the
    requesting or the top-level origin. At startup (`permissions::startup`, before any browser)
    everything still recorded is reset. **Crash safety:** Chromium writes a reset to disk only on
    its next preferences flush (~10 s), so a reset record stays in the file marked `resetAt` for
    30 s (`RESET_PERSIST_MS`) and is purged afterwards; if the process dies earlier, the next
    startup resets it again (chrome-e2e `[c]` kills the browser right after a reset). A re-grant
    revives the record. A remembered answer drops the records it covers, and a
    grant core remembers as allowed is never reset. `set_content_setting` CHECK-crashes the browser
    process for a type that isn't a registered content setting (e.g. `GEOLOCATION_WITH_OPTIONS`, a
    website setting); tabs-e2e resets every mapped type once (`debug.resetPermissions`).
  - Prompts nobody shows are dismissed at once instead of keeping CEF's Alloy default (IGNORE),
    which leaves the page's promise pending forever: the UI client has a permission handler that
    dismisses every prompt, and the tab client dismisses prompts from browsers that aren't tabs.
    Media requests from non-tab browsers keep CEF's default (deny).
  - A request from a hidden tab (background tab, other space) is held by Chromium's
    `PermissionRequestManager` until the tab is shown, as in Chrome. Notifications work like any
    other prompt (`Notification.requestPermission()` on http(s) pages and `localhost` reaches the
    prompt); shown notifications are Chromium's own pop-ups, not Windows toasts.
- **Fullscreen**: `on_fullscreen_mode_change` → `TabFullscreenChanged` → core →
  `SetPageFullscreen` (§4). Esc in `on_pre_key_event` while in page fullscreen →
  `host.exit_fullscreen(1)`; F11 / `toggleFullscreen` while in page fullscreen exits it too.
- **Boosts** (host-matched delivery: a renderer only ever receives the enabled boosts of the host
  it shows, never the whole list):
  - `extra_info` carries the boosts of the URL the browser is created for, with a version (hash of
    the JSON list);
  - on each main-frame `on_context_created` the renderer applies the boosts matching the frame URL:
    CSS as `<style id="sta-boost">` (MutationObserver guard for a missing `documentElement`,
    moved last at `DOMContentLoaded`), JS after `DOMContentLoaded` in an IIFE with try/catch; then
    it sends `sta.boosts.check{version}`. The browser answers with `sta.boosts{list,
    version, apply_now}` for the frame's **committed URL** when that list differs (new renderer
    processes only know the creation-time `extra_info`);
  - before same-site main-frame navigations, `LoadUrl` and reloads, the browser pushes the target
    host's list (without applying it to the current document). Cross-site navigations get a new
    renderer process, which asks for its own list instead, so a site's renderer doesn't learn
    another site's boosts. (`extra_info` is frozen per browser: a tab that goes cross-site hands
    the creation host's list to the new process once; nothing is applied there.)
  - `UpsertBoost`/`ToggleBoost`/`DeleteBoost` reload affected tabs.
- **DevTools** (`devtools.rs`, `devtools_cdp.rs`, `devtools_policy.rs`, `devtools_shim.rs`):
  **docked inside the window**. `ToggleDevTools` (F12) / `FocusDevTools` (Ctrl+Shift+I) / "Inspect"
  ask core, which owns *whether* DevTools are open for a tab (`store/devtools.rs`, never persisted)
  and answers with `OpenDevTools{tab, docked}`, `CloseDevTools`, `FocusDevTools` or `InspectAt`.
  ```text
  wrapper (BoxLayout, 2 px accent inset)
    stack     Panel, CEF's fill layout: both children fill it
      frontend  BrowserView  devtools://devtools/bundled/devtools_app.html?can_dock=true
      page host Panel, a BoxLayout whose insets place the page inside it (added last: on top)
        page    BrowserView  the tab's own browser, at `setInspectedPageBounds`
  ```
  - The page host exists because a `CefPanel` always has a layout manager (CEF offers no way to
    clear it) and its fill layout would reset the page's bounds on every layout pass — writing them
    back spun a ~560 Hz layout loop. A BoxLayout's `inside_border_insets` place a single child at an
    arbitrary rect, which is what the layout manager itself computes, so nothing fights it. The empty
    host above the frontend does not swallow mouse input: each BrowserView owns an aura window and
    aura targets the topmost *window* under the pointer.
  - **Transport**: no socket. The renderer shim (`devtools_shim.rs`) replaces the
    `InspectorFrontendHost` methods Chromium's `DefaultBindingsDelegate` leaves as no-ops — page
    bounds, dock state, close, bring-to-front, open-in-new-tab, the debugger keys it wants forwarded
    — and `sendMessageToBackend`, which becomes a CEF process message. `devtools_cdp.rs` relays it
    onto a child session **S** of the inspected browser's in-process DevTools session
    (`Target.attachToTarget{self, flatten}`) and sends answers back as renderer messages, chunked at
    4 MB, which the shim hands to `DevToolsAPI.dispatchMessage`. A round trip measures ~0.4 ms and a
    10 MB answer 153 ms.
  - **Policy** (`devtools_policy.rs`): every message the frontend sends is checked, on S and on every
    nested session. `Browser.*`, `SystemInfo.*`, every `Target.*` but `setAutoAttach`,
    `autoAttachRelated`, `getTargetInfo` (S only) and `detachFromTarget`, plus
    `Page.setDownloadBehavior`, `DOM.setFileInputFiles`, `Network.getAllCookies`, `Storage.*Cookies`
    and `Page.navigate` outside http(s)/file/about are always refused; a cookie or storage method that
    names an **origin, storage key or cookie URL** (`Network.getCookies{urls}`,
    `Network.deleteCookies`, `Storage.clearDataForOrigin` and its family) may only name one of the
    inspected browser's own frame origins, which the shell reads on demand for exactly those methods;
    everything else must be in the measured method inventory, and an unknown method gets a protocol
    error, a WARN and an entry in `debug.info`'s `devtools.refused`. A nested session is admitted only for an iframe, worker,
    shared worker, service worker or worklet target, and never for an `sta`, `devtools`, `chrome`,
    `chrome-untrusted` or `chrome-search` document. Root-session traffic is never forwarded, and the
    frontend never learns S's id (it is added and removed by string surgery on the raw message,
    because Chromium appends `sessionId` as the map's last key — only the **top-level** field, so a
    `sessionId` inside `params` survives: `Target.detachFromTarget` is the one nested-session method
    the frontend may send). The frontend's own message ids are held below `devtools_cdp`'s root range,
    so the three clients of one browser really partition the id space. A message over 64 MiB is
    refused in either direction, counted and answered with a protocol error.
  - **Undock** (the frontend's own button, the `>` command "Undock DevTools", or a Peek page) opens
    CEF's own Chrome-style window instead — today's `tabs::show_dev_tools` path — and lasts until
    those DevTools close. That window is Chromium's `DevToolsWindow`, with its own embedder and agent
    host, so the renderer shim must stay out of it: only the frontend sta builds itself carries
    `sta_devtools_shim` (`renderer.rs`). Marking both alike left the undocked window rendering empty
    panels with zero protocol traffic and nothing logged. A saved `undocked` dock state from Chromium's profile is mapped to `right`
    on the way *in* (the shim filters `getPreferences`), so DevTools never reopen as a stray window.
  - **Keys**: F12 toggles, Ctrl+Shift+I opens → focuses → closes (the third step needs to know what
    has focus, so `keyboard::on_accelerator` resolves it), Ctrl+Shift+C starts the element picker
    while the focused pane has a dock (`DevToolsAPI.enterInspectElementMode`, Chrome's own route) and
    copies the URL otherwise, DevTools' own zoom keys reach `CefBrowserHost::SetZoomLevel` on the
    frontend (an Alloy BrowserView has no `ZoomController`, so Chromium's implementation does
    nothing), page-first sta keys reach a focused
    frontend first, `escape_chain` leaves Esc to it, plain F11 goes to a focused frontend (step out)
    while fullscreen stays on F11 from the page and on Alt+Shift+F, and the debugger keys the
    frontend asks for (`setWhitelistedShortcuts`: F8, F10, Shift+F11, Ctrl+\, Ctrl+') are forwarded
    from the focused page.
  - **Theme**: the frontend follows sta's dark mode live, because the shell emulates
    `prefers-color-scheme` in the frontend's own page (`Emulation.setEmulatedMedia`); DevTools 152
    decides its theme from that media feature and ignores the `uiTheme` preference an embedder
    writes. A theme the user picks inside DevTools' own settings still wins.
  - **Inspect**: `InspectElement{tab,x,y}` divides the point by the page zoom (`1.2^zoom_level`),
    asks `DOM.getNodeForLocation` on S and injects `Overlay.inspectNodeRequested` into the frontend.
    A node that turns out to be an `<iframe>` owner is re-resolved in that frame's nested session,
    offset by its content box.
  - Closing the dock hands the keyboard back: `build` focuses the frontend, so `tear_down` focuses
    the page view again when the frontend was the focus owner — otherwise F12 → F12 left the card
    with no focused view and typing went nowhere until the user clicked.
  - The frontend browser has `Role::DevTools{tab}`,
    `extra_info {sta_devtools, sta_devtools_shim, sta_devtools_tab}`,
    its own client (one URL, no popups of its own — links become `DevToolsLinkRequested`, checked
    with `urls::web_content_may_open` — and no console output), and no IPC trust: `__staQuery` is
    never injected into a `devtools://` document.
  - The page's overlays (find bar, permission prompt) anchor to `tabs::tab_rect_in_window`, which is
    the **page** rect while DevTools are docked; the rounded corner masks use
    `wrapper_rect_in_window`, so the card is rounded around DevTools too.
  - **Never on sta's own pages**, in any build. A DevTools window on an internal page would give
    DevTools extensions (and anything typed into its console) sta's own IPC surface, so all three
    routes are closed: core refuses `ToggleDevTools`/`FocusDevTools`/`InspectElement` for a tab
    showing a `sta://` page with the toast "DevTools isn't available on sta pages"
    (`store/foreign.rs`); the context menu's **Inspect** is never offered there, because internal
    pages run on the trusted UI client, whose menu keeps only the edit commands (§4.1 context
    menus); and `tabs::show_dev_tools` refuses a trusted UI browser a second time, whichever route
    reached it. The shim also appends every `sta://<host>` origin to the origins DevTools extensions
    may not touch. Debug builds can override all of it with `STA_DEVTOOLS_INTERNAL=1`
    (`tabs::init_devtools_policy` tells core at startup).
- **External protocols** (`external.rs`): schemes Chromium doesn't load (anything except http,
  https, file, about, data, blob, filesystem, view-source, javascript, sta, chrome*,
  devtools, ws, wss) never navigate. Main-frame navigations, popups (`target=_blank`),
  `on_open_urlfrom_tab`, context-menu link items, blocked UI navigations/popups and
  `LoadUrl`/`CreateBrowser` effects cancel them; only with a user gesture (effects count as user
  actions, session restore doesn't) the percent-escaped URL goes to `ShellExecuteW` on a worker
  thread. Handler schemes with a history of abuse (`ms-msdt`, `search-ms`, `ms-officecmd`,
  `ms-appinstaller`, `shell`, `vbscript`, … and Chromium's denylist) are never launched, and
  neither are one-letter "schemes" (`c:/…` is a drive path). The list of schemes Chromium loads is
  `sta_core::urls::BROWSER_SCHEMES`.
  - Typed URLs: core classifies `mailto:`, `tel:`, `sms:` and any other `scheme://…` as URLs, and
    `OpenInput` / `OpenUrl` / `Navigate` / `SplitOpenInput` of an external-scheme URL
    (`urls::is_external_scheme`) emit `Effect::OpenExternal{url}` instead of creating or navigating
    a tab; the shell runs it through the same launcher (source "typed URL").
- **Context menus**: UI pages keep only edit commands in editable fields (Copy for a text
  selection, else no menu). Web tabs get "Open Link in New Tab" (`LinkOpenRequested{BackgroundTab}`),
  "Open Link in Peek" (`{NewWindow}`), "Copy Link Address", "Open Image in New Tab", "Copy Image
  Address" and "Inspect"; Chromium's "View page source" runs core's `ViewSource`.

### 4.2 Keyboard (keyboard.rs)

`Window::set_accelerator` in `on_window_created`, with one command id per key combo.
`on_accelerator` maps id → `Command` and **enqueues** it. Accelerators work whether a tab, the
sidebar or an overlay has focus. Ids and bindings (Arc for Windows, `arc_spec.md` §3):

> **macOS reads the same table with ⌘** (`PRIMARY_MODIFIER`). `set_accelerator` takes only
> shift/ctrl/alt — there is no flag for the Command key — so nothing is registered as an accelerator
> there. Instead the handlers below match the table themselves (`modifiers()` reports ⌘ as the
> table's `ctrl`, and a real Ctrl is dropped: it belongs to macOS text editing) and call the same
> `on_accelerator`, so both platforms take one dispatch path with one set of special cases. A
> reserved binding fires in `on_pre_key_event`, before the page; a page-first one in `on_key_event`,
> after the page left the key alone; `on_window_key_event` catches what no view handled. **⌘H is
> Hide** on macOS, so history moves to ⌘Y — the one binding that is not the same table.

- **high_priority = 1** (reserved; pages can't intercept):
  - Ctrl+T `OpenCommandBar{newTab}`
  - Ctrl+W / Ctrl+F4 `CloseItem{None}`
  - Ctrl+Shift+T `ReopenClosed`
  - Ctrl+Tab / Ctrl+Shift+Tab `MruStep`
  - Ctrl+1..9 `ActivateNth`
  - Ctrl+Alt+↑/↓, Ctrl+PgUp/PgDn `ActivateAdjacent`
  - Ctrl+Alt+←/→ `SwitchSpaceAdjacent`
  - Ctrl+Shift+K `ClearToday`
  - Ctrl+Shift+= (VK_OEM_PLUS+Shift) and Ctrl+Shift+VK_ADD `OpenCommandBar{split}`
  - Ctrl+Shift+- (VK_OEM_MINUS+Shift) and Ctrl+Shift+VK_SUBTRACT `SeparatePane`
  - Ctrl+Shift+1..4 `FocusPane`
  - Ctrl+Shift+[ / ] `FocusPaneAdjacent`
  - F12 `ToggleDevTools`, Ctrl+Shift+I `FocusDevTools` (open → focus → close; the shell resolves
    the last step, and leaves plain F11 to a focused DevTools frontend)
  - F11 / Alt+Shift+F `WindowControl{toggleFullscreen}`
  - Ctrl+Shift+W `WindowCloseRequested`
- **high_priority = 0** (page-first):
  - Ctrl+L / Alt+D / F6 `OpenCommandBar{editUrl}`
  - Ctrl+S `ToggleSidebar`
  - Ctrl+D `TogglePin`
  - Ctrl+Shift+C / Ctrl+Shift+Alt+C `CopyUrl`
  - Alt+1..9 `SwitchSpaceNth`
  - Alt+← / Alt+→ `GoBack`/`GoForward`
  - Ctrl+R / F5 `Reload`; Ctrl+Shift+R / Ctrl+F5 / Shift+F5 `Reload{ignoreCache}`
  - Ctrl+F `OpenFind`; F3 / Shift+F3 `FindNext`
  - Ctrl+= (VK_OEM_PLUS), Ctrl+VK_ADD `Zoom{in}`; Ctrl+- (VK_OEM_MINUS), Ctrl+VK_SUBTRACT
    `Zoom{out}`; Ctrl+0 / Ctrl+VK_NUMPAD0 `Zoom{reset}`
  - Ctrl+P `Print`
  - Ctrl+U `ViewSource`
  - Ctrl+J `ToggleSidebarPanel{downloads}`
  - Ctrl+, (VK_OEM_COMMA) `OpenInternalPage{settings}`
  - Ctrl+H `OpenInternalPage{history}`
  - Ctrl+O `ExpandPeek` (ignored when no Peek)
  - Alt+F `ToggleSidebarPanel{appMenu}`
- **`KeyboardHandler::on_pre_key_event`** (installed on the UI and tab clients):
  - unmodified Esc (not in the command bar or the agent overlay, whose pages handle their own Esc): exit page fullscreen
    → close the find bar if it has focus → close Peek → close the find bar → `MruCancel` →
    close the command bar → `CloseSidebarPanel` when a *transient* sidebar panel
    (`SidebarPanel::is_transient`: downloads, app menu) is open and the sidebar doesn't have focus
    (its page handles Esc itself; e.g. Ctrl+J downloads or Alt+F app menu opened while a page keeps
    focus). The space sheets, an inline rename and the edit pinned page panel, which hold input,
    stay open → hide the floating sidebar (hover reveal) unless a panel pins it; this last step
    doesn't consume the key, so the page still gets its Escape. Ctrl+Esc while the switcher is up →
    `MruCancel`;
  - core also closes the transient panels (downloads, app menu; not the panels that hold input) on
    `TabFocused`;
  - Ctrl key-up after a `MruStep` (or while the switcher is requested): `MruCommit`, also from
    `WindowDelegate::on_key_event` for key-ups no browser saw (each commit happens once).
- CDP `Input.dispatchKeyEvent` never reaches Views accelerators; tests use `debug.accelerator`
  (same table and handler) or `debug.realKeys` (real OS input, only while our window is the
  foreground window).

### 4.3 Sidebar placement and hover reveal (window.rs, sidebar_hover.rs)

The one sidebar BrowserView is either **docked** (first child of the window) or, while the sidebar
is hidden, **parked** in the floating sidebar overlay host (`Overlay::SidebarHover`,
`can_activate 0`, above Peek, the command bar, the find bar and the permission prompt, below the
switcher and toast). Hover only shows and hides that host; the view moves only when docking or
hiding (same browser, DOM and scroll position; a reparent cancels an IME composition, which never
happens on hover).

- **`SetSidebar{visible, width, floating}`** (core): `visible` = docked (the setting, or a hidden
  sidebar docked for a panel that holds input: space sheets, inline rename, edit pinned page);
  `floating` = a hidden
  sidebar pinned open for a transient panel (downloads, app menu). `window::apply_sidebar_placement`
  is the **only writer** of the view's parent and visibility: a docked view is visible unless in
  page fullscreen, a parked view always stays `set_visible(1)` inside its host.
- **Park**: the page gets `sidebar.hover {visible:false, dismiss:true, gen}` and hides its contents,
  and when it reports that blank frame (`surface.exited {gen}`) — at the earliest 60 ms later, at the
  latest at the cap — `window.remove_child_view` → `overlays::adopt_sidebar`. A parked page renders no
  frames, so the first reveal would otherwise show the docked frame; the wait is a *correctness* delay
  that no motion setting scales away, only shortens to its floor (§4.7, `motion.rs`). No fade when the
  docked view isn't shown, the page isn't ready, the window is minimized or closing. Focus in the
  sidebar is moved to the page (`restore_main_focus`). A sidebar hidden at startup is created in the
  host directly (never attached and detached right after its browser was created).
- **Dock**: `release_sidebar` (hide the host, remove) → `add_child_view_at(0)` → layout, one task;
  the page gets `{visible:true}`. Docked for a panel that holds input while it was floating (e.g.
  double-click → rename): the sidebar takes keyboard focus. When that panel closes with the pointer
  still over the sidebar, it parks and floats on at once (no dwell); `ToggleSidebar` clears that
  state (controller.rs), so Ctrl+S never counts as the end of a panel dock.
- **Detection**: a UI-thread poll of `GetCursorPos`, `WindowFromPoint`→`GA_ROOT` (our window, or a
  popup owned by it) and `GetAsyncKeyState` (buttons held, plus the "pressed since" bit for clicks
  shorter than the poll), converted with the window's client rect and scale factor. Not a window
  subclass: Chromium passes mouse messages over web content straight to its handler, the edge band
  is non-client, a resting cursor sends nothing and OLE drag loops send no `WM_MOUSEMOVE`.
  `WindowFromPoint` may run Chromium's hit test synchronously: no borrows across it. The poll runs
  while *armed* (parked, page ready, no page fullscreen, window active and not minimized or
  closing) or while the overlay is shown (never while minimized, even with a pinned overlay):
  every 33 ms while the pointer is over our window, within 96 DIP of its left edge or while shown,
  else 50 ms, plus at the dwell and hide deadlines, so a reveal comes at most one poll after the
  dwell. Docked, inactive or minimized: no poll.
- **Rules** (`sidebar_hover::Machine`, unit-tested): reveal after 120 ms in the edge zone — from
  `x < 12` DIP inside a restored window (the 4 DIP resize band included) or `x < 8` in a maximized
  or fullscreen one, out to 64 DIP **beyond** the window's left edge, full client height. The zone
  reaches outside on purpose: a pointer thrown at the edge of a window that is not itself at the
  screen's edge overshoots it, and landing just past the window is the same gesture as landing on
  it (only the active window polls at all, and over the window the zone is ours only while no other
  window covers it). A button pressed in the zone (resizing from the left edge, a click in the gap)
  blocks the reveal until it is released, a drag entering the zone from elsewhere counts; after a
  park, an Esc hide, or a window move/resize with the pointer in the zone, the pointer must leave
  the zone first. Hide at the first sample outside the keep zone (overlay bounds + 8 DIP, `x < 16`, and
  the same slop outside the edge) unless a button is held, the pointer is over an owned popup, core
  pins it, or the page locks it. **The card travels**: at the `full` level the shell puts the host a
  whole card-width outside the window (the window clips it to a 1 DIP sliver), tells the page to
  show its contents, and slides the host home over 160 ms (ease-out, `motion.rs slide_sidebar`);
  a hide starts the moment the pointer leaves (no grace period: `HIDE_MS` is 0) and slides it back
  out over 240 ms, hiding it there — which is also why that hide needs no
  acknowledged exit — what is left of it is one clipped sliver, and the next reveal starts there.
  The page is laid out at the settled width throughout and pinned to the card's right edge (§4.7),
  so nothing inside reflows while the slice grows. At `reduced` and with the key off
  nothing travels: the card is shown and hidden where it belongs and the page fades its contents,
  hidden on the blank frame it reports — at least 50 ms later (§4.7). Pinning shows it at once;
  un-pinning or unlocking hides it at once when nothing
  else holds it and the pointer is outside the keep zone. A press outside the shown overlay
  dismisses: `{visible:true, dismiss:true}` (the page closes menus, popovers, drags) and
  `CloseSidebarPanel` for a transient panel (the page never gets `blur`, it never has focus). Esc in
  a page (end of the Esc chain, not consumed) hides an overlay core doesn't pin. Page fullscreen,
  docking, a crashed sidebar renderer and shutdown hide it at once.
- **Page lock** (`sidebar.hoverLock {locked}`, sidebar only): the page reports whether any HTML
  menu or popover (a `body > .portal-host`) or drag (`is-dragging` / `is-resizing` on `<html>`) is
  open. A lock never shows the overlay; while it is shown it stays open wherever the pointer goes,
  so a context menu survives the pointer crossing the page, and the press outside that dismisses
  the menu also hides the sidebar (the unlock follows). The lock is dropped on a sidebar main-frame
  load start (reload, navigation) and when its renderer goes away. Esc ignores it (the hide closes
  the menus).
- **No typing in the floating sidebar**: every panel with a text field is a non-transient core
  panel (`NewSpace`, `EditSpace`, `RenameItem`, `EditPinned`) that docks a hidden sidebar and
  takes keyboard focus; the page renders them only while docked, and its "Rename…" menu items drop
  the F2 hint while floating.
- **Command bar**: a press in the floating sidebar closes an open command bar like focusing the
  docked sidebar does. The page dispatches `closeCommandBar` on `pointerdown` (the shell's focus
  rule never sees the non-activatable overlay), ordered before the click's own command; Peek
  stays open (it keeps working with the floating sidebar).
- **Focus** (verified with a probe): showing and hiding the non-activatable host sends the page no
  blur/focus. A **click inside it**, however, moves aura focus to the window itself: the page blurs,
  no browser has focus, keys and even accelerators go nowhere, and Views still counts the page's
  view as focused (a plain `request_focus` on it is a no-op). After the button release (+60 ms, so
  the click's own commands such as `activateItem` run first) `overlays::refocus_after_floating_click`
  passes focus through the parked sidebar view (which can't keep it) back to the browser that had
  it, under the focus guard (Peek and the command bar stay open).
- **Shutdown**: `release_surface(Sidebar)` removes a parked view from the host (not the window)
  before dropping it, so its browser closes without waiting for the timeout.

### 4.4 Rounded corners (rounded.rs)

CEF Views can't clip a BrowserView, and a windowed browser is always opaque, but an overlay widget
is translucent: images and panels in it composite over whatever is below. The rounded look is built
from small pieces on that basis (a spike measured every piece at 100, 125, 150 and 175 %).

- **Tokens** (DIP; mirrored in `ui/common/tokens.css`):

  | token | value | image | where |
  |---|---|---|---|
  | `CONTENT_RADIUS` (+ 2 DIP ring) | 10 | 12 | web pages, the empty state (`--content-radius`) |
  | `MASK_BLEED` | 4 | 16 | masks reach into native frame gaps |
  | `OVERLAY_RADIUS` | 12 | 20 | command bar, permission prompt, agent overlay, switcher, Peek, floating sidebar (`--overlay-radius`) |
  | `OVERLAY_SHADOW` | 8 | – | the soft shadow of overlay cards (floating sidebar: 4) |
  | `OVERLAY_PAD` | 3 | – | fill between a card's side border and its page (Peek: 11) |
  | `FIND_RADIUS` | 8 | 16 | find bar (`--find-radius`) |
  | `TOAST_RADIUS` | 16 | 24 | toast (`--toast-radius`) |
  | `PEEK_PAGE_RADIUS` | 8 | 12 | the web page inside the Peek card (`--peek-page-radius`) |

- **Content corner masks**: 4 pane slots × 4 corners = 16 hidden, non-activatable CUSTOM
  overlays created *before* the overlay hosts (lowest z-order). Each is a `LabelButton` (insets 0,
  not focusable, no ink drop; hover and press fall back to the normal image) showing a runtime
  image: frame color outside the arc, the wrapper ring color (frame, accent for the focused split
  pane, or the agent frame color while an agent acts on the tab, §5.2) in the 2 DIP ring,
  transparent inside.
  - Images: 4×4 supersampled premultiplied BGRA (`tile_pixels`, unit-tested), one bitmap per scale
    1, 1.25, 1.5, 1.75, 2, 2.25, 2.5, 3, 3.5 where the pixel size is whole (every image is a
    multiple of 4 DIP: a 13 DIP image has no exact 150 % bitmap and gets resampled with ringing).
    Cached by tile and corner.
  - `tabs::visible_pane_rects()`: wrapper rects and ring colors of the panes on screen (the wrapper
    color as shown, `tabs::shown_wrapper_color`); the empty-state view counts as one unfocused pane (its page draws the same card: `inset: 2px`,
    radius 10), so the masks stay up between Empty and Single; empty in page fullscreen (square);
    a tab in Peek is excluded.
  - Bleed 4 DIP only into native frame: right and bottom insets, split gaps, the left inset while
    the sidebar is hidden; never over the top bar or the docked sidebar (HTML). Without it the
    overlay widget and the painted ring snap differently at fractional pane edges (a stray 1 px
    accent column at 150 %).
  - `rounded::layout_masks()` runs after `content.layout()` in `tabs::apply_layout` and
    `apply_fullscreen`, in `on_layout_changed` (resize, sidebar), after `set_page_fullscreen` and on
    `SetChrome` (recolor). Showing a widget raises it above every other overlay, so when a mask goes
    from hidden to visible every visible overlay is re-shown lowest first
    (`overlays::restack_all_visible`, under the focus guard; a focused overlay gets focus back
    through another view first).
- **Overlay cards** (`rounded::Card`, one per overlay host, built on first use): the host's
  contents are a transparent box; `Rows` (tall surfaces) = `[top row | middle | bottom row]`,
  `Columns` (find bar, toast: the height isn't padded) = the same turned sideways.
  - Corner row: `[corner image (shadow + radius)² | edge: shadow strips (1 DIP translucent black
    panels), 1 DIP border, fill (radius − 1, flex) | corner image]`. Corner image: fill inside
    `r ≤ radius − 1`, border to `radius`, quadratic shadow falloff outside (peak alpha 0.16 light,
    0.34 dark), transparent beyond.
  - Middle: `[shadow strips | border-colored host with 1 DIP insets (the side borders) [fill panel
    with pad insets [BrowserView(s)]] | shadow strips]`. The BrowserView layer snaps to device
    pixels on its own: the colored host and fill behind it make sure only fill or border can show
    beside it, never the page underneath.
  - Every row and column has an explicit `preferred_size` (derived sizes gave the first row all the
    height and left the BrowserView 1×1). Fill panels re-apply their color in `on_theme_changed`.
  - Chrome thicknesses are multiples of 4 DIP (`shadow + 1 + pad`, `shadow + radius`), and the
    host rect is snapped to the display's device-pixel grid (`snap_unit`: 1 DIP at 100/200 %, 2 at
    150/250 %, 4 at 125/175 %; grown outwards, the floating sidebar's inwards so its shadow stays
    clear of the 4 DIP resize bands). Without it the right border column disappeared at 125 and
    150 %.
  - Geometry (§4 "Overlay bounds"): the positioning rules give the **card** rect; a page's
    `surface.setSize` is its content size and the card adds `CardSpec::inner()` per side (Rows:
    1 + pad beside, radius above and below; Columns: radius beside, 1 + pad above and below); the
    host adds the shadow. `debug.info.overlays.hosts[].bounds` is the card, `hostBounds` the host
    (the drag-region hole), `card` the spec.
  - Colors: `Effect::SetChrome` carries `theme::chrome_argb` (frame, accent, surface, border over
    surface, border over frame); `overlays::on_chrome_colors_changed` recolors fills and swaps the
    corner images. Overlay pages get `html.native-card` (ipc.js) and drop their own square edge.
  - Peek: header (40) over the page, 12 DIP of surface around both; 4 more mask overlays created
    right after the Peek host round the page's corners (surface outside, 8 DIP radius, 4 DIP into
    the fill). They are shown right after Peek itself (before `restack_overlays_above(Peek)`), hidden
    with it, re-shown after Peek in `restack_all_visible`, and follow its layout.
- **Known limits:**
  - an overlay swallows mouse input in its whole rectangle (CEF has no pass-through): about 10×10
    DIP at each page corner plus the bleed over the frame (including the corner of a page's
    scrollbar), 12×12 DIP at the Peek page corners, and a card's shadow ring (a click there doesn't
    close the command bar);
  - scale factors other than 100–350 % in quarter steps get resampled corner images;
  - at fractional scales a pane that starts at a fractional device pixel can leave one device-pixel
    column (or row) of its BrowserView uncovered by the page layer along the left (top) edge. CEF
    resets a view's background to the native theme's `#1f1f1f` in `on_theme_changed`, which drew a
    dark line between the rounded corners; the tab delegate now paints the tab view with its
    wrapper color (the surface in Peek, tabs.rs `view_background`), so that pixel reads as frame, or
    as a 1 px thicker accent ring beside the focused split pane (a 1 px step where the ring meets
    its corner mask);
  - the corner images are unnamed buttons to accessibility tools.

### 4.5 Chrome-created browsers (foreign.rs, platform/hidden_windows.rs, extension_files.rs)

Chromium creates `Browser` objects of its own when the profile has none and something needs a
window: the Web Store's post-install UI (`ExtensionInstallUIDesktop::OnInstallSuccess` opens a new
tab page and the "added" dialog), an extension's `tabs.create`, `windows.create` or
`runtime.openOptionsPage`, `identity.launchWebAuthFlow`, and the DevTools protocol's
`Target.createTarget`. Each tab of such a browser gets
`BrowserProcessHandler::GetDefaultClient` — `foreign::client()` — with a LifeSpan, Request,
Keyboard, Display and Download handler. Without it the user sees a full Chrome window (tab strip,
omnibox, puzzle menu) instead of sta. Every browser here is counted by `browsers::extra_register`,
so the main window waits for it at shutdown (§3.1).

- **`on_after_created`** decides what a browser is, before its window is on screen (CEF calls this
  while the root window exists but has no `WS_VISIBLE`):
  - `has_view()` browsers are sta's own Chrome-style views (none today) and are ignored;
  - a request context that doesn't share the global one's storage (an incognito window) → hidden,
    closed, `ForeignBlocked{Incognito}` ("sta has no private windows");
  - **KeepNative** — kept as an ordinary Chromium window (user decision D2: extension popups and
    sign-in flows need their own window and their APIs): a `_crx_<id>` window title
    (`windows.create({type:'popup'})`), a main frame that has already navigated, a `devtools://`
    page, a browser with no root window of its own (`launchWebAuthFlow` creates its window later,
    so the root is resolved again and the window gets sta's dark caption and icon), or a tab of a
    window that is already native. They are never adopted and close only at shutdown. Chromium writes
    their title, so `app.rs`'s `ResourceBundleHandler` renames its product (`IDS_PRODUCT_NAME`,
    `IDS_BROWSER_WINDOW_TITLE_FORMAT`, resolved by name through CEF's version-safe id mapper): a
    browser-framed native window reads "… - sta", not "… - Chromium". The caption, icon and title are
    sta's while the *content* is the extension's — see README's limitations;
  - otherwise **Pending**: the root window is hidden (below) and every key it sees is consumed. A
    Pending browser that hasn't navigated after 500 ms becomes KeepNative with a WARN.
- **Hiding** (`platform/hidden_windows.rs`): the root window is **cloaked**
  (`DWMWA_CLOAK`) — DWM draws nothing and the shell keeps it out of the taskbar and Alt+Tab, while
  the window keeps working (`ShowWindow(SW_HIDE)` would be undone by Chromium). A thread-local
  `WH_CBT` hook on the UI thread (Chromium creates and shows its windows there) refuses
  `HCBT_ACTIVATE` for hidden roots and everything they own, and cloaks windows owned by a hidden
  root at `HCBT_CREATEWND`, before their first show — that is how the post-install dialog never
  appears. Two `SetWinEventHook` backstops (this process only) cloak a window that shows up
  uncloaked and hand the foreground back to sta if a hidden window took it. The hook procedures
  touch only that module's thread-locals (with `try_borrow`) and never call CEF. A window is only
  treated as hidden while its owner chain still reaches a hidden root: Windows reuses `HWND` values
  inside a process, so a remembered owned window whose chain no longer checks out is forgotten rather
  than believed — otherwise a later, unrelated window that got that handle would be cloaked and
  refused activation.
- **Navigation** (`on_before_browse`, main frame, hidden browsers) is always cancelled (return 1),
  so nothing loads in a window nobody can see, and then classified:
  - `chrome://newtab/`, `chrome://new-tab-page/` → the profile's `Extensions` directory is diffed
    against the last scan; every new extension is reported as `ExtensionInstalled{id, name,
    external}` (toast "<name> added", or "… added by another program, off until you allow it" when
    the preferences say the install location is external). `startup` re-scans silently after 12 s, so
    the extensions other programs registered — which Chromium installs during startup — are not
    announced later; that re-scan **absorbs only ids with an external install location**, because
    swallowing the whole directory would also swallow an install the user started in the first
    seconds of a run (no toast, no `recent_installs` entry); at most 3 toasts per window;
  - `http(s)` with a host → `ForeignTabRequested{url}`;
  - `chrome-extension://<id>/<page>` → `ForeignTabRequested{url, extension}`, where `extension` is
    what that extension's own files say (name, its options/popup/side-panel pages, resources
    web-accessible to every site, whether it was just installed). Core decides
    (`urls::foreign_tab_verdict`): a declared page opens as a foreground Today tab after the active
    item, anything else asks with a toast ("An extension wants to open a page of <name> · Open");
  - **Who asked.** The verdict is about the **page**, never about who asked for it. Chromium creates
    the window and sta cancels the navigation before it commits, so with prebuilt CEF (decision D1a)
    nothing in `on_before_browse` carries an initiator: a `_crx_<id>` title exists only for popup
    windows, which are never adopted, and `chrome-extension://X/<page>` looks the same whether X or
    another installed extension asked. So an installed extension can have sta open another
    extension's *declared* page — a navigation Chromium itself refuses for resources that are not
    web-accessible. Accepted: every alternative either breaks `runtime.openOptionsPage` (the flow
    the rule exists for) or asks the user a question they cannot answer. What is done instead: the
    question names the page's **owner** as its owner and never claims that extension asked, and the
    page opens in a foreground tab the user sees. Phase 3's picker owns the popup that starts these
    flows and can attribute them, so the rule is worth tightening then
    (`scenarios_foreign.rs::the_question_names_the_pages_owner_not_the_asker` pins today's answer);
  - `chrome://extensions/?options=<id>` (embedded `options_ui`, which is blocked in Alloy tabs) is
    rewritten to `chrome-extension://<id>/<options page>` first;
  - anything else → a WARN with the scheme and browser id only.
- **Rate limit.** Two limits, at the two places that can see what they are limiting:
  - **core** (`store/foreign.rs`) caps what it *does*, after the verdict: 3 tabs and 2 "wants to
    open" toasts per 10 s, then the toast "An extension keeps opening windows; sta blocked them"
    once per window. The budget is spent on the verdict, not on the request — an `Ask` the user
    never answers, and a refused URL, leave the tab budget for the next legitimate window (an
    extension that offers a page sta won't open on its own usually opens its welcome tab too). Each
    path having its own budget is what keeps the toast row spam-proof.
  - **the shell** (`foreign.rs`, `HANDLE_MAX`) caps its own work: classifying a navigation reads an
    extension's manifest, or rescans the whole `Extensions` directory, on the UI thread, so at most
    24 navigations per 10 s are classified and the rest are dropped with the same toast
    (`ForeignBlocked{RateLimited}`). A resource guard, an order of magnitude above what any real
    flow needs, so it never decides what the user sees.
- **Closing**: a hidden window's browsers are closed (`close_browser(1)`) once the window is at
  least 3 s old and got no new tab for 1 s (an extension's welcome tab often arrives a moment after
  the post-install window), at 10 s at the latest, and never while a download is in progress
  (`downloads::in_progress_count`; `notify_when_idle` wakes the poll).
- **Install dialog**: Chromium's "Add …?" dialog is a window owned by sta's main window with
  `WS_EX_DLGMODALFRAME` and no `WS_EX_TOOLWINDOW` (menus, `<select>` popups and tooltips always
  have the tool-window style; `is_install_dialog_style` is a pure function of the two style words, so
  the signature gate S17 measured is unit-tested). sta centers it at
  `content_top + max(72 px, 14 %)`, clamped to the work area: over the focused pane when that pane
  shows the Web Store, else — an install whose tab was switched away from — over the focused pane or
  sta's whole content area, as long as some tab still shows the Web Store. With no Web Store tab open
  the window is left where Chromium put it: the same style signature also fits sta's own native
  dialogs (a file picker), which must never be moved. The arithmetic is
  `install_dialog_position` (unit-tested), and the dialog's *buttons* are views, so a test answers it
  with `test_dialog` (docs/TESTING.md), not by clicking.
- **`extension_files.rs`** reads what is on disk, never Chromium's preference writes: the profile's
  `Extensions/<id>/<version>/manifest.json`, `--load-extension` directories (id from the manifest
  `key`, else from the absolute path like Chromium) and, as a fallback, the `path` in the
  preferences. Names come from `_locales/<locale>/messages.json`.
- **Tests**: `crates/sta/e2e/extensions-e2e.mjs` with the in-repo probe extensions
  (`crates/sta/e2e/extensions/`), the shell unit tests of `extension_files.rs`, and the core
  scenarios in `crates/sta-core/tests/scenarios_foreign.rs`. `debug.foreign` (a snapshot with the
  window log and hook counters), `debug.foreign.trigger` (`Target.createTarget` through
  `devtools_cdp.rs`) and `debug.foreign.close` drive it without an extension.

### 4.6 Extensions: the Ctrl+E picker, the popup card and the backend (extensions.rs, ext_popup.rs, ext_backend.rs, safe_mode.rs)

Phase 3 of the extensions work (`C:/ast/tmp/ext-design/critique.md` §4). §4.5 keeps an extension's
windows out of the way; this is how the user reaches an extension on purpose.

- **The listing (`extensions.rs`)** is read-only and comes from three places in the profile: the
  `Extensions/<id>/<version>/` directories plus `_locales` (`extension_files.rs`, §4.5),
  `--load-extension` directories, and `extensions.settings` / `extensions.commands` in
  `Secure Preferences` and `Preferences`. Those preference files are MAC-protected and sta never
  writes them.
  - **Chromium 152 writes no `state` key**: an extension is disabled exactly when `disable_reasons`
    is non-empty (a *list*: `[]`, `[1]` for the user's own "off", `[8192]` for one another program
    added). Older profiles still carry `state`, and 0 there still means disabled.
  - An id with **no preferences entry is not listed yet** (except a `--load-extension` one, which
    Chromium always enables): only Chromium knows whether an extension runs, and it writes that with
    a delay — guessing "on" would misreport exactly the extensions that are waiting for the user's
    OK. The listing is re-read at startup (+3 s, +12 s), after every operation, when the picker opens
    (`Effect::RefreshExtensions`), +11 s after an install, and whenever the directory or either
    preferences file changes (a 2 s modification-time poll, debounced 300 ms).
  - Icons are served same-origin at `sta://command/__ext-icon/<id>/<px>` and `sta://settings/…`
    (scheme.rs): a `sta://` page may not load `chrome-extension://` images. The file is resolved
    inside the extension's own directory (canonicalized, image types only, ≤ 512 KiB) on the IO
    thread.
  - A state Chromium **confirmed** (`note_state`) is remembered for up to 15 s and applied on top of
    what the preferences still say. Patching the cached list alone was not enough: the operation's own
    re-reads (300 ms, 1.5 s) rebuild from preferences Chromium has not committed yet, which flipped a
    row back to its old state — including back to "Needs your OK" for an extension the user had just
    allowed. The entry is dropped as soon as a rebuild agrees with it.
  - A blocked extension carries **why** (`ExtensionBlock`): only the policy bits (and a policy
    install) say "Turned off by your organization". On Chromium 152 an MV2 extension is disabled with
    `UNSUPPORTED_MANIFEST_VERSION` and a damaged profile yields `CORRUPTED` — both ordinary on a
    personal machine, and `GREYLIST` means Safe Browsing turned it off, which is close to the
    opposite.
  - The result is pushed into core as `Command::ExtensionsChanged`; **core decides everything the
    user sees** (`sta-core/src/extensions.rs`, `store/extensions.rs`): the rows and their groups
    (Extensions / Needs your OK / Off / More), what Enter does, and the rule that Enter never turns
    an extension on.
- **The picker** is a command bar mode (`CommandBarMode::Extensions`, Ctrl+E, page-first). Ranking is
  the omnibox's, with the 2-set Korean keyboard applied in **both** directions
  (`omnibox::jamo_to_qwerty`): a query typed with a Hangul IME on finds a Latin-named extension, and
  the letters of a Korean name (`gksrmf` → 한글) find it with the IME off. Both rank below a direct
  hit.
- **The popup card (`ext_popup.rs`, `Overlay::ExtensionPopup`)** loads
  `chrome-extension://<id>/<default_popup>` in an Alloy BrowserView under a 40 DIP header sta draws
  (`sta://extension/`). The card is shown only once the page reports a size — measured in the page
  itself (`width: max-content` for the preferred width, then the height at that width), clamped to
  Chrome's 25×25…800×600 and re-measured while an asynchronous popup renders. A size at the clamp
  minimum is **no answer** (an empty document measures as exactly 25×25), so the card waits instead of
  showing a sliver with a clipped header. A popup that has still rendered nothing at the 3 s deadline
  gets the card with the header alone and one honest line — judged on a measurement taken *at* the
  deadline, because a popup whose service worker was cold paints a second or two in and a working
  popup must not be called broken. The page is untrusted web content (`Role::ExtensionPopup`): its
  main frame may only be that extension's origin, anything else becomes a checked `OpenUrl`, external
  protocols only with a real user gesture (`external.rs`), downloads, file choosers and permission
  requests are refused, and it never reaches the IPC surface. The card sits **below** the permission
  prompt in the z-order and a prompt closes it (SEC-4).
- **The current tab (`ext_shim.rs`, `ext_shim.js`)**. Chromium's `tabs.query` / `windows.*` walk Chrome
  windows, and sta's tabs are in none, so a popup that asks which tab it is over gets `[]` — and so
  does its service worker, which is who real popups ask (1Password's first message is "which tab?").
  While a card is open, sta evaluates one script in the popup page (registered for its document with
  `Page.addScriptToEvaluateOnNewDocument` after `Page.enable` — without which it never runs — so it
  is there before the page's own scripts) and in **that
  extension's** service worker (`Target.setAutoAttach` on the popup's DevTools session attaches it to
  every extension's worker; the others are let go of at once, and a worker session only ever gets
  `Runtime.evaluate` — `devtools_cdp::worker_evaluate`). The script supplies exactly one fact, the tab
  the card was opened over, and asks the extension's own `tabs.get` for everything else, so what an
  extension sees of that tab is still Chromium's decision from its permissions; sta's own pages
  (`sta://`, `chrome://`) are never offered. The tab's id is **found in the popup page**: tab ids and
  CEF browser ids are handed out in the same order, so the tab is `popup browser − tab browser` ids
  below the popup's own `tabs.getCurrent()`, further when Chromium made windows of its own in
  between, and the URL sta knows the tab by must match. That URL travels in the script's
  configuration, which the extension can read, so it is only handed to an extension whose manifest
  lets it read that URL anyway (`ExtensionFiles::may_read_url`). An id found is remembered per browser, and
  the last pair tells a worker where to look before the page has answered. It ends with the card:
  the worker sessions close with the popup's browser, and the worker's copy checks
  `runtime.getContexts` for the popup before every answer. The same script lets `tabs.create` fall
  back to `windows.create` when there is no Chrome window ("No current window") — a window §4.5
  hides and turns into an sta tab under core's verdict and budget; an extension page that is itself
  an sta tab gets the script for that alone (`client.rs` load handler →
  `ext_shim::on_extension_tab_document`), because a page's own "Sign in" is a `tabs.create` too.
  **The toolbar click**: what the button does is the extension's to decide at run time
  (`action.setPopup('')` = "no popup, send me `onClicked`"), not its manifest's. Once per card the
  worker is asked `action.getPopup`; an empty popup with `onClicked` listeners gets
  `onClicked.dispatch(<the card's tab>)` and the card closes, instead of showing a popup page that
  was never meant to be seen (1Password without an account). What it cannot do is grant
  `activeTab`: an extension that counts on that alone still gets the card's "Needs the current tab"
  (`extension_files.rs` `needs_current_tab`). Measurements: docs/research/extensions.md §6.
- **The backend (`ext_backend.rs`)** turns extensions on and off and removes them, because
  `chrome.management` only exists inside a Chromium page. Each operation gets one **never-shown**
  Chrome-style window on `chrome://extensions/`, runs one fixed script in it through
  `devtools_cdp.rs` (`User::Extensions`; `Runtime.evaluate` is all it sends) and closes again. The window is
  cloaked and cannot be activated; if it is ever shown, sta re-hides it and abandons the operation.
  Two things are worth knowing:
  - the operation is recorded **before** the window is created, because `window_create_top_level`
    calls `on_window_created` synchronously and the browser (and its first load) can arrive before
    that call returns;
  - **removing an extension shows Chromium's own "Remove …?" dialog** — `showConfirmDialog: false` is
    honoured only for an extension removing itself, and `developerPrivate` has no uninstall function
    at all. That dialog *is* the confirmation (sta asks nothing of its own): the backend window stays
    cloaked while only its dialog is left visible and answerable
    (`hidden_windows::allow_dialogs`, cleared as soon as the operation ends), centered over sta. The
    exemption is granted with the uninstall script, not when the window is created, and admits
    **one** owned window: a second window that root opens is cloaked and refused like any other.
  An operation Chromium confirmed patches the cached listing at once; the profile re-reads that
  follow only confirm it, because `Secure Preferences` is committed up to ten seconds later.
- **Settings › Extensions** (`ui/settings/extensions.js`) is where an extension can be turned **on**:
  for one another program added, "Turn on" first loads Chrome's own permission warnings, host access
  and source (`ExtensionOp::GetInfo`) and shows them, with the confirm button *not* the default: "Not
  now" is focused when the panel opens, the warnings arrive in an `aria-live` region, Esc closes it,
  and focus returns to the button. Core allows the write only while that extension's own disclosure is
  fresh (60 s) and consumes it, so each Turn on has its own. One another program installed from a file
  can only be removed (D6a), and the row says that in words — a dimmed button with a tooltip said it
  to nobody.
- **The crash-loop guard (`safe_mode.rs`)** writes a launch marker and clears it on a clean shutdown.
  Two abnormal exits within 60 s of launching start the next run in safe mode: core restores every
  tab **unloaded** and Settings › Extensions shows a banner, so an extension that crashes the browser
  can be turned off.
- **Tests**: `crates/sta-core/tests/scenarios_extensions.rs` (the rules), the shell unit tests of
  `extensions.rs`, `ext_backend.rs` and `safe_mode.rs`, and the `(e) (c) (g) (l)` sections of
  `crates/sta/e2e/extensions-e2e.mjs`.

### 4.7 Motion (animations; sta-core/motion.rs, sta/motion.rs, ui/common/motion.js)

Almost all motion is HTML, because native geometry has no opacity to animate (a `BrowserView` cannot
be faded: `OverlayController` has none) and stepping a widget's position on a timer moves something
that holds a browser. Native code otherwise only *coordinates timing*.

The one exception is the floating sidebar's own arrival and departure (§4.3): what has to travel
there is the **card**, not what is inside it, and no page can move a native card. So the shell steps
that one host's `x` from a card-width outside the window to its home rect. An overlay cannot hang
outside the window — CEF fits its bounds to it — so the *host* is cut down to the slice that is
inside the window, growing from 1 DIP at the window's left edge, and the **card inside it is not**:
the host's contents are a clip panel (`rounded::clip_root`) whose box layout has a negative left
inset (`set_clip_cut`), so the card keeps its settled size, sits `cut` DIP left of the host and is
clipped by Views. The page is therefore **moved, never resized**, during a slide. (The first cut
resized it — every step was a new viewport for the renderer to lay out and raster before the
compositor could show it, ~146 `resize` events per reveal-and-leave, and the slide visibly dropped
frames. `sidebar.css .sidebar.is-floating` still pins the contents to `--float-width` for the one
frame in which the view is parked.) The steps are asked for every 4 ms, not every 16: Windows runs
delayed tasks on its 15.6 ms tick, a 16 ms delay lands on every second tick, and a 160 ms slide got
7 steps with 31 ms gaps; now it gets ~20 and no gap over 16 ms (`debug.info.motion.lastSlide`). The
draggable regions are rescheduled once, where the card lands; and the hover keep zone stays at the
card's home rect, so the pointer's meaning never moves with it.

**One registry.** `sta-core/src/motion.rs` names every animation: `AnimationSpec {key, group,
default_on}`, 36 keys in 8 groups (`GROUPS`), with the key pattern
`^[a-z][A-Za-z]*\.[a-z][A-Za-z]*$` and unit-tested invariants. `Settings::animations`
(`AnimationSettings {enabled, follow_system, groups, choices}`) stores what the user switched, and
`Store::motion_view()` resolves it into `UiState.motion {level, off, systemAnimations}`
(docs/PROTOCOL.md §14). `choices` holds explicit choices, not differences from the default, so a
later change of a default cannot silently flip a user who chose the old one. The maps load
leniently (a bad entry is skipped, not fatal) and keep unknown keys up to 128, so the field needed no
`STATE_VERSION` bump.

**The Windows setting.** `SPI_GETCLIENTAREAANIMATION` (Settings › Accessibility › Visual effects) is
read in `platform::system_animations()` at init, on every `WM_SETTINGCHANGE` and in the controller's
5 s heartbeat, and reported as the shell-only `SystemAnimationsChanged {enabled}`. The
`WM_SETTINGCHANGE` observer is a **thread-local `WH_CALLWNDPROC` hook** on the UI thread
(`platform::watch_setting_change`): it changes no window's behaviour and reaches no other process,
and because it runs inside a *sent* message it only counts the message and posts a UI task — reading
the setting and dispatching a command never happen re-entrantly. The command is runtime state: it
never marks state dirty, is never saved, and bumps the revision only when the value changes.

**The stale-frame rule.** A hidden page renders no frames, so Chromium shows its last frame — or a
blank surface — until it draws again. Whenever the shell *can* wait, a surface presents a blank frame
before the shell hides it:

- the waits are **correctness delays, not animations** (`crates/sta/src/motion.rs`):
  `max(floor, fade + ACK_MS)` capped at `WAIT_CAP_MS` (120 ms), where `floor` is `HIDE_FLOOR_MS`
  (50 ms) for hiding an overlay and `PARK_FLOOR_MS` (60 ms) for parking the sidebar view (§4.3).
  Turning the animation off drops the page's *fade* — the wait falls to the floor and is never 0;
- **acknowledged exits.** The shell asks the page to present that frame and waits for the answer:
  `surface.exit {gen}` → `surface.exited {gen}` for the non-activatable toast and switcher, and the
  floating sidebar's existing `sidebar.hover {visible:false, dismiss:true, gen}` → the same ack. The
  widget is hidden on the ack (never before the floor) or at the cap, so a page that cannot answer
  costs 120 ms and nothing else. The exit fade is `min(the key's duration, --t-surface-exit)` = 60 ms,
  short enough to finish inside the wait — `tools/check-motion.mjs` keeps `EXIT_FADE_MS` and
  `--t-surface-exit` equal and every exit key's duration at least that long;
- an exit **lingers**: the widget stays visible while the page blanks, so it is still restacked above
  a corner mask and still a no-drag hole. That is accepted (a lingering toast is a toast that is still
  there). A show during the linger cancels the exit by generation and the widget simply stays up;
- **activatable overlays never linger** (command bar, find bar, permission prompt, Peek): a
  page-initiated close or a commit blanks the page synchronously and dispatches after one frame, and
  `CloseCommandBar` carries an optional `seq` so a close for a bar that is already gone cannot close
  the one that replaced it. Keeping keyboard focus in a bar the user just dismissed would be worse
  than a stale frame. `motion.closeBlank(els)` is that pattern: it writes `opacity: 0` and
  `pointer-events: none` **in the same task as the key or click** (no fade — instant is the point),
  then resolves on the next frame, raced against a 32 ms timeout because a hidden page never gets one
  and a close that never dispatched would leave the overlay up for good. The next open calls
  `motion.unblank()` (the command bar on a fresh `seq`, the find bar's `fadeIn`, the permission
  prompt's enter effect, Peek's first state with something to show). Nothing acknowledges these, so
  the frame their renderer produced last really is the frame the next open would show — the previous
  search, the previous site's title, the previous prompt's host;
- **forced hides skip the protocol**: docking the sidebar re-parents the same view into the window,
  and page fullscreen or shutdown takes the whole host away — there is no frame left to blank. A hide
  that *should* have asked but had no live page to ask (its renderer is gone) counts as an
  `earlyHides`, which stays 0 in normal runs;
- the **rare stale frame after a focus-loss close** is accepted; it is today's behaviour, and the
  close path there is not page-initiated.

**What the pages do.** `theme.js applyMotion(state)` puts `data-motion` and `data-anim-off` on
`<html>` (and, for `theme.crossFade`, the `theme-fade` class on pages that paint their own background
or `theme-snap` on the ones inside a native card); `applyWindowState(state)` adds
`data-focused` while the window has focus, which is what an infinite indicator in a docked surface
runs on. `tokens.css` turns all of that into zeroed durations, `--motion-distance: 0` and static
indicators (PROTOCOL §14); `motion.js` gates WAAPI. Its rules:

- animations start from **keyed diffs only** — an id, an order/height signature, a `seq`, a
  `toast.id`, a space id — never from a render, never from a visibility event and never from
  `animationend`, which does not fire while a surface renders no frames;
- `motion.enabled(key)` also requires the surface to be **presented** — and a surface that answers
  for itself (`usePresence`: the sidebar's `presence !== 'hidden'`, an overlay's intent being
  non-null) is believed over `visibilityState` in *both* directions. A parked sidebar keeps
  `is_drawn = 1` and `visibilityState === 'visible'` while rendering nothing, so the flag says yes
  when the answer is no; an overlay's view is still hidden when the state that fills it arrives — the
  shell shows it a moment later, and the switcher's view deliberately 250 ms later — so the flag says
  no when the answer is yes, and refusing there would mean the overlays never animated in at all. An
  animation created in that gap stays *pending* until the surface's first frame, which is exactly the
  entrance to show. That second case is trusted only while the **window is in front**
  (`<html data-focused>`, from `applyWindowState`): a minimized window hides pages that still believe
  they are on screen, and rows arriving there would pile up as pending animations and play all at
  once on restore. Pages that never declare presence (the internal pages, which are ordinary tabs)
  keep the document's own flag. Leaving presentation calls `finishAll()`, and so does the document
  going `hidden` (a minimized window hides pages that still believe they are on screen), so nothing
  created while a surface was away can burst on the next reveal — that is settling, not triggering. Nothing is ever keyed on
  `visibilitychange` or a rAF gap — `overlays::restack_*` hides and re-shows visible overlays
  synchronously (a content corner mask appearing is enough), which would re-trigger or cut such an
  animation;
- **transform and opacity only** in docked surfaces and overlays; layout animation only in internal
  pages. Travel inside a native card stays ≤ 4 px (the card clips its page). A **tracked** overlay
  root is never transformed: `ipc.js trackSurfaceSize` reports the *layout* box (ResizeObserver's
  `borderBoxSize`, never `getBoundingClientRect`, which includes transforms) and the shell keeps
  whatever size it is told, so the toast and the command bar animate an inner wrapper. A focused text
  input is never transformed either — it moves the IME candidate window away from the caret, and
  Korean input starts the moment the command bar opens;
- **ghosts are inert clones**: `motion.ghost()` strips `id`, `data-*`, `role`, `aria-*`, `tabindex`
  and `title`, sets `aria-hidden` and `inert`, and puts the clone in a fixed layer at body level
  (list heights shrink the moment the real row is gone). Nothing that finds rows by `data-id`,
  `[data-nav]`, `[data-row]`, `aria-activedescendant` or `role` can reach one. They are dropped on
  `sta:dismiss`, on leaving presentation and when motion is turned off;
- **FLIP** measures *before* it cancels a running animation (`getBoundingClientRect` includes the
  transform FLIP is applying), is suspended while `html.is-dragging` / `is-resizing` is set (drag
  targeting reads row rects live), and animates the individual `translate` property so a caller's own
  inline `transform` (the drag ghost) survives. It refuses a change of more than
  `FLIP_BULK_LIMIT` (8) **ids** — what makes a change bulk is the ids it changes, not the rows that
  happened to move: emptying or refilling a list changes dozens of ids and displaces only the
  survivors, and sliding one of those the length of the list (the Undo of Clear Today, Ctrl+Shift+T)
  is the opposite of a rearrangement anyone can follow. Callers pass that count
  (`flip.capture(root, key, sel, {changed})`); `sidebar.js` diffs its own id lists for it and
  `internal-page.js` passes `added + removed`. It also refuses pointer-initiated closes, rows that
  end up outside the box it measured (clipped to the window — a row scrolled far out of its scroller
  still has a size) and moves longer than that box is wide or tall. And it is a **position**
  animation, so it asks `moves(key)` rather than `enabled(key)`: at `reduced` it snaps, like the
  gliders and the toggle thumb, because travelling `--motion-distance × the distance` would leave
  rows in the wrong place;
- stagger totals stay ≤ 150 ms (≤ 200 ms for a page's own enter), and every staggered animation fills
  **backwards**: during its delay an animation with `fill: 'none'` contributes nothing, so an element
  waiting its turn in an entrance would paint at its base style — fully opaque — and blink out the
  moment its delay ended;
- a **single key** switched off mid-flight settles what is playing under that key (`finishAll(key)`)
  and drops the ghosts, exactly as the master switch settles everything: the switch on the settings
  page means the same thing while something is playing as it does before it starts. `duration()`
  caches per `data-motion` / `data-anim-off`, never on a MutationObserver, because `applyMotion` and
  the surface's render happen in the same task and a record is only delivered at the end of it;
- a farewell that clones something big takes a **single-slot ghost** (`ghost(el, {slot})`): the space
  switch's pane ghost is a deep clone of the whole list, and switching faster than the fade can finish
  would otherwise stack panes in the layer.

**The sidebar and the top bar (14 keys).** Every one of them starts from a signature `sidebar.js`
reads off the UiState in its own render body — which Preact runs *before* it patches the DOM, so the
FLIP rects are the "first" ones and the rows about to unmount are still there to clone:

- the **row ids in list order**, the **collapsed folder ids**, the **favorite ids**, and a **layout
  signature** of everything around the lists whose height moving would move every row at once (the
  favorites grid, the favorite rename row, the "Drop here to pin" placeholder, the download card, the
  space). A change of the layout signature suspends FLIP altogether, and so does a changed
  `scrollTop`: a shrinking list clamps its scroll, which moves every row by the same amount and is a
  scroll, not a rearrangement;
- what changed decides the key: a collapsed flag → `sidebar.folderExpand`, the same ids in another
  order → `sidebar.reorder`, different ids → `sidebar.tabInsertRemove`, three or more Today rows gone
  in one push → `sidebar.clearToday` (core keeps the visible and audible rows, so the list rarely
  reaches zero);
- that decision is also the **exit plan** the rows read (`rows.js setRowExitMode`): whether a row
  that unmounts leaves a ghost, under which key, how many ghosts this commit may leave (8 ordinary,
  12 for the Clear Today sweep, 20 for a collapse) and how far their delays may spread. A space
  switch leaves none: the whole pane leaves as one ghost instead;
- the **active row's fill** moved into `.row::before`, so the highlight can crossfade from the row
  the user left to the one they opened while hover and pressed still read through it. A glider would
  have to track FLIP, folder toggles, rename, width drags and scrolling;
- the **drag** keeps its individual properties apart: the ghost lifts with `scale` (its inline
  `transform` follows the pointer), and the drop line glides with `translate`, keyed on the insertion
  point actually changing. A refused drop shakes; a committed one settles the ghost; a cancelled one
  gets nothing;
- the **Today divider** clips its line (`clip-path`) instead of animating a margin, which was the
  last layout transition in a docked surface;
- `topbar.navFade` holds the controls blank for the shell's own park delay and then fades them: the
  top bar learns `sidebarVisible` from the push, but its width only changes when the shell relays out
  the window, and sliding them in would show them twice and then jump.

**The command bar (4 keys).** The selected row's fill is one `.cmd-glider` layer the layout effect
moves: it glides on a keyboard move and snaps on hover, because a hover glide lags the pointer and a
held arrow key at repeat speed never catches up. Only the **first** results after an open stagger in
(later queries replace the list under every keystroke). A mode change crossfades the chip's label and
the leading glyph — the chip's width changes in one step — and flips a `data-ph` attribute on the
input, which is what restarts the native `::placeholder`'s own fade: no script can animate a
`::placeholder`, and there is only ever one of it to crossfade. The input itself is never
transformed, and neither is the card whose size the shell tracks.

**The overlays (5 keys).** Each of them is a *page inside a card the shell has already placed*, so
none of them moves its own card: what animates is what the card says.

- `overlays.toast` rises 4 px on `toast.id` — but **not inside the shell's card**, where the page is
  28 DIP high, `html, body { overflow: hidden }` clips it and the pill's own controls have about 1 px
  of slack above and below: a 4 px rise there sliced the Undo button and the × flat against the card's
  rounded edge for the first frames, so in a card the pill fades, which is what `reduced` does
  everywhere anyway. A **replacement** crossfades only `.toast-msg`, so the pill neither moves under
  the pointer nor re-plays a rise the user is still looking at, and `role="status"` is not
  re-announced for text that did not change;
- `overlays.switcher` fades every card **together** over 100 ms — the overlay already waits 250 ms for
  the Ctrl hold, and a five-card stagger would add another ~265 ms — and moves the selection with one
  `.sw-ring` layer that glides between cards. The ring is *placed* from the selected card's own layout
  box, so it is exactly right even when the glide is refused;
- `overlays.find` fades the bar in with **opacity only**: it holds a focused input, and a transform
  would move both the caret and the IME candidate window. The "no matches" shake is the same rule in
  the other direction — it runs only when the user asks *again* for a search this bar already reported
  as empty (F3 is a shell accelerator, so the page never sees the key), never on the `find.result`
  that arrives with every keystroke, and never while a composition is running;
- `overlays.permission` is a **fade and nothing else**, on `.perm-inner` rather than the tracked
  `.perm` root. What protects Allow from a click aimed at the page underneath is the 400 ms input
  guard, which is always on and is not a setting;
- `overlays.peek` crossfades the header's centre on `peek.tab`, so Alt+clicking a second link into an
  open Peek swaps the title and host instead of blinking.

The **Peek card and the extension popup card have no entrance of their own, on purpose**: what fills
most of either card is a page sta does not own — the peeked site, the extension's popup document, each
its own view that the shell places under sta's 28–40 DIP strip. Fading or rising the strip alone would
animate a third of the card while the rest hard-cut, which reads worse than the hard cut does on its
own. That is also why `ui/extension/extension.js` is the one surface that never imports `motion.js`
and why the overlays group has no key for the popup: there would be nothing for the switch to take
away. What both cards *do* animate is what changes inside the strip (Peek's centre, above).

**Menus (1 key).** `menus.popIn` grows a menu or popover out of the side it is anchored to:
`placeFloating` writes `--pop-origin` and the `--pop-dx` / `--pop-dy` travel direction from the
placement it actually *resolved* (after the flip and the viewport clamp), and `sta-pop-in` reads them
with `--motion-distance` so `reduced` is left with a pure fade. A drill-down submenu slides out of its
parent instead (`sta-drill-in`). Closing leaves an exit ghost, which is also why `motion.ghost()`
takes **one ghost per element**: the sidebar wraps its own panels to ghost them under
`sidebar.panels`, and a parent's cleanup runs first, so the more specific key wins and no panel is
ever cloned twice.

**The internal pages (5 keys).** `internal-page.js` owns three of them, because settings, archive,
history and boosts share all three; the other two belong to the surface that plays them —
`pages.boostsEditor` to `ui/boosts/boosts.js` and `pages.emptyHero` to `ui/empty/empty.css`, which is
the empty *surface*, not a page. `pages.enter` staggers at most six cards when a page **first has
something to show** (a page's body arrives after its request resolves, and the latch makes sure no
later render replays it) — the six the user is about to be looking at: a page renders every section
into one scroller and scrolls afterwards, so a deep link (`#<section>`, or `?section=<id>`, which is
how the extensions picker links into Settings) scopes the stagger to that section instead of animating
six cards at the top that are never seen; `pages.listRows` fades an arriving row in, leaves a removed one as a ghost and glides the
rest, bounded at 20 changed rows so a search or a "clear all" is not mistaken for a rearrangement;
`pages.navIndicator` is one `.ip-nav-indicator` box placed from the current item's own layout box and
glided between places (settings' scroll-spy, the boosts list); `pages.boostsEditor` crossfades the
editor with a View Transition, with `:root { view-transition-name: none }` so *only* `.bst-main` is
captured and with the boost **fetched before** the transition starts (an async update callback would
freeze rendering until the IPC returned, and would cross-fade into an empty editor);
`pages.emptyHero` is the empty state's existing rise, now on its own token.

**Theme (1 key).** `theme.crossFade` transitions the twelve `@property`-registered theme colors on
`:root`. `theme.js applyThemeFade` adds `html.theme-fade` only to pages that paint their own
background: inside a native card the fill, the border and the corner tiles are the shell's and snap at
the fade's midpoint, so a page that faded there would show a 3–4 DIP ring of mismatched colour for the
whole fade. The level `reduced` keeps it — a colour fade is not motion.

A card page does not fade, but it must not *jump ahead* either: it gets `html.theme-snap`, which
transitions the same twelve properties with a **zero duration and half the fade of delay**, so its
colours change in the same frame as the shell's fill around it. Without it a toast was a white pill
inside a black card for the 150 ms the shell waits (the card page paints `--surface` itself:
`base.css html.surface-overlay`). The class is added under exactly the condition the shell delays
under (the key on and the level not `off`), and only *after* `applyTheme` has set this state's colours
— `ipc.js renderState` calls them in that order — so a surface's first colours still arrive instantly
instead of being held for 150 ms of fallback white.

The shell's half is that **midpoint**: `window::set_chrome` delays `Effect::SetChrome` by
`motion::chrome_delay_ms()` (half of `THEME_FADE_MS`, 150 ms) while the fade can run, so the native
fill, border, corner tiles, tab wrappers and DWM attributes change in the middle of it instead of
150 ms before it. The delay is generation-guarded — a fast run of space switches applies the newest
colours once — and skipped where there is no fade to meet: the **first** `SetChrome` of the session
(nothing has been painted yet, and the window may not even be visible), a minimized or closing window,
and the key or the master switch being off. `debug.info.motion` reports `chromeDelayMs`,
`chromeCalls` and `chromeDelayed`.

**Controls (3 keys) and indicators (3 keys).** These are `base.css` and `components.js`, so they reach
every surface at once. `controls.hoverPress` owns **two** durations, because a hover tint and a press
are not the same length (`--t-controls-hover-press` and the shorter `--t-controls-press`); its gate
zeroes both. `controls.toggles` covers the switch thumb, the checkbox, the segmented control and the
`Disclosure`, whose **height** animation is the one layout animation rule 5 allows, because an
internal page is the only place it runs. `controls.smoothScroll` is `motion.scrollBehavior()`, which
answers `'auto'` at `reduced` as well as when the key is off. `indicators.loading` is one *period*:
the spinner turns once in it, the indeterminate bar sweeps in 1.5× it, the indeterminate ring turns in
1.25× and the agent activity pulse in 1.375×, so `reduced` slows all of them together and `off` leaves
a **static ring and a visible bar** rather than nothing. `indicators.badges` pops a counter that
changed and pulses the sidebar's pending-permission dot. `indicators.audio` is the three-bar
equalizer that replaces the speaker glyph on a tab playing sound — an infinite animation in a docked
surface, so `tokens.css` stops it (leaving a legible still glyph) whenever `<html data-focused>` is
missing: hours of music in an unfocused window cost no compositing.

A last shape worth naming: three of these animate a **position** rather than a travel — a press
scale, a toggle thumb, a glider. `--motion-distance: 0` cannot express "reduced" for them, because a
thumb that travels 0 px simply ends up in the wrong place; so `reduced` zeroes their duration instead
(`--t-controls-press`, `--t-controls-toggles`) and the JS gliders ask `motion.moves(key)`, which is
false at `reduced`, and snap.

**The shell's own part (`crates/sta/src/motion.rs`).** The shell coordinates *timing*, and all of it
lives in one module: the acknowledged exits above (the generation registry, the floors, the cap, and
the counters `debug.info.motion` reports), the `SetChrome` midpoint, and the floating sidebar's slide
(`slide_sidebar`, `slide_offset` — ease-out, unit-tested, guarded by a generation so an arrival that
interrupts a leave simply carries on from where the card is). Three call sites use the exits —
`overlays::hide_after_blank` (toast, switcher), `sidebar_hover::begin_exit` (the hover hide when the
card does not slide: `reduced`, or the key off) and `window::place_sidebar` (the park) — so the
floors, the cap and the counters cannot drift apart between them. `ipc.rs` answers the ack
(`surface.exited` → `motion::on_exited`), and a page that closes or a surface that is shown again ends
the wait early (`motion::cancel`).

**Checks.** `tools/check-motion.mjs` holds the registry, the UI catalog
(`ui/common/motion-catalog.js`) and the CSS gates together, and enforces the rules a reviewer cannot
see in one file (no `animationend`, no transform on a tracked root, no *animated property* whose
duration comes from two keys' tokens, an allowlist for layout transitions that are not converted yet,
and the exit-fade cap matching the shell's `EXIT_FADE_MS`). `tools/motion-check.mjs` drives the mock UI
in headless Edge — including the page half of an acknowledged exit — and the `m.motion` sections of
`chrome-e2e` check the real thing (docs/TESTING.md).

### 4.8 Updates (update.rs, unzip.rs, sta-core/update.rs)

Full description: `docs/RELEASING.md`. In short: a `vX.Y.Z` tag builds a release archive and a
`latest.json` (`.github/workflows/release.yml`), and a running sta finds it by itself.

- **core** (`sta_core::update`) parses the manifest, compares versions, picks this platform's
  archive (`platform_key()`), refuses one that is not served from this repository's releases, and
  holds `UiState.update` — the status Settings › About renders. Its SHA-256 lives here too: the
  hash a download is judged by belongs in the crate that can unit-test it against the standard's
  vectors, not in a dependency nobody in this project can check.
- **shell** (`update.rs`) does the fetching with `Urlrequest` (the same path as `suggest.rs`:
  Chromium's network stack, no cookies, no cache), streams the archive to `%LOCALAPPDATA%\sta  updates\` while hashing it, unpacks it (`unzip.rs`, which refuses any entry that would be written
  outside the staging directory), and applies it by starting the **staged** `sta.exe` with
  `--sta-apply-update`. That helper — the first thing `main` looks at, before CEF exists — waits
  for this process to exit, copies the staged files over the installed ones and starts the new
  build. A program cannot replace its own files while it runs; a copy of it, from somewhere else,
  can.
- **What never happens**: no check under the e2e harness (`STA_E2E=1`) or with
  `STA_NO_UPDATE_CHECK=1`; no download that was not asked for (`DownloadUpdate` from the UI, gated
  on the status core holds); nothing unpacked before its SHA-256 matches; nothing installed that
  does not contain `sta.exe`.

---

## 5. IPC (ipc.rs, renderer.rs) — see `docs/PROTOCOL.md` for the message catalogue

- `cef::wrapper::message_router` with `js_query_function = "__staQuery"`.
- **Trust**: only UI browsers are trusted. They are created with the UI client and
  `extra_info.sta_ui`; the browser side records their ids in `on_after_created`; the frame is
  the main frame; the URL starts with `sta://`. The renderer injects the query function only
  into such contexts. The browser-side handler re-checks everything (trusted browser id, main
  frame, committed `sta://` URL) on every query. Tab clients never forward process messages
  to the router, and web tabs get no `__staQuery` and no native function on `window` (the
  media reporter and the Alt+click preview reporter are closure arguments). The shell-defined
  renderer→browser messages a **tab** client accepts are exactly `sta.media`, `sta.boosts.check`
  and `sta.preview` (§4.1); everything else is ignored. UI pages can't leave `sta://` (§4.1 "UI location
  guard") and DevTools of a UI page don't inherit `sta_ui`.
- **Handler rules**:
  - `dispatch` enqueues (after `Command::allowed_from_ui()`, which also checks the command inside
    `commitOmnibox` and rejects nested ones) and replies `null` right away;
  - queries (`state.get`, `omnibox.query`, `omnibox.actions`, …) take a short store borrow and
    reply inline;
  - never call router methods, navigate, or close UI browsers from inside the handler; Views work
    (`ui.ready`, `surface.setSize`, `sidebar.setWidth`) is posted.
- `ui.ready` and `surface.setSize`: the shell derives the surface from the calling browser id,
  not from the payload. `sidebar.setWidth` is refused (403) unless the caller is the sidebar.
- `dialog.pickFolder`: one picker at a time (409 otherwise). The `IFileOpenDialog` runs on its own
  STA thread with the main window as owner (the window is disabled, the UI thread keeps running);
  the answer (`string | null`) is posted back.
- Requests are non-persistent queries. Push events use one persistent `__subscribe` query per
  page; `emit(event, payload)` calls `success_str` on every subscriber.
- `omnibox.suggest` is refused (403) unless the caller is the command bar; the work is posted to
  `suggest::start` and answered later (§5.1).
- `agent.info` / `agent.testConnection` (§5.2) are refused (403) unless the caller is the Settings
  page (`agent.info` also from the agent overlay); the test runs `sta-mcp.exe --check` on a
  worker thread and is answered later.
- Router gotchas (`ipc.md` §4.A): always pass both `onSuccess` and `onFailure` in JS;
  `on_query_canceled` must be idempotent.

### 5.1 Search suggestions (suggest.rs)

`omnibox.suggest {text}` → `{text, suggestions}` feeds the command bar's Suggestions rows and
inline completion (PROTOCOL §6). Core stays pure: `omnibox::suggest_url(engine, query, lang)`
builds the endpoint and `omnibox::parse_suggestions(body, query)` reads the answer; the shell only
moves bytes.

- **Endpoints** (OpenSearch suggestion JSON `[query, [s1, s2, …], …]`, each checked live):
  Google `https://suggestqueries.google.com/complete/search?client=firefox&ie=utf-8&oe=utf-8&hl=<lang>&q=<q>`
  (`hl` = the primary OS UI language, omitted when unknown), Bing
  `https://www.bing.com/osjson.aspx?query=<q>`, DuckDuckGo `https://duckduckgo.com/ac/?q=<q>&type=list`,
  Brave `https://search.brave.com/api/suggest?q=<q>`, Ecosia
  `https://ac.ecosia.org/autocomplete?q=<q>&type=list`. Kagi, Perplexity and custom engines have
  none. `<q>` is `encodeURIComponent` of the text (UTF-8).
- **Privacy**:
  - nothing is sent unless `settings.searchSuggestions` is on (the default; Settings explains that
    typed text goes to the selected search engine);
  - only the command bar may ask, and only for search-like text: blank text, more than 256
    characters, URL-like text with an explicit scheme (`https:`, `file:`, `mailto:`, any
    `scheme://`) and Windows paths are never sent; a leading `?` (forced search) is stripped
    first;
  - no cookies or credentials: the request doesn't set `UR_FLAG_ALLOW_STORED_CREDENTIALS`, so the
    profile's cookies aren't sent and cookies a response sets aren't saved (Bing's endpoint sets
    tracking cookies). shell-e2e checks both: a cookie stored for the endpoint's host is never
    sent, and the `Set-Cookie` of every suggestion answer never reaches the jar.
    `UR_FLAG_DISABLE_CACHE` keeps typed text out of the HTTP cache;
  - newest request only, and typed text is never logged.
- **Request** (`suggest::start`, a posted UI task, never inside the router lock):
  1. resolve the caller's request in flight with `[]` and cancel its URL request (one slot per
     calling browser);
  2. `request_create()`, `set_url`, `set_method("GET")`,
     `set_flags(UR_FLAG_DISABLE_CACHE | UR_FLAG_NO_RETRY_ON_5XX)`;
  3. `urlrequest_create(request, SuggestClient{browser, id}, request_context_get_global_context())`:
     the Chromium network stack of the global context, so system proxy settings apply. CEF calls
     `on_download_data` / `on_request_complete` on the thread that created the request (the UI
     thread);
  4. `post_ui_delayed(1500)` ends the request if it is still pending (`[]`, cancel).
  The body is collected up to 64 KB (more ends the request with `[]`); a completed request with
  `UR_SUCCESS` and HTTP 200 is parsed. Every end goes through one path that removes the pending
  entry first (so a late or synchronous callback is a no-op), replies, and posts the
  cancel/release of the URL request handle (a cancel can complete the request synchronously).
- `debug.info.suggest` has counters (`inFlight`, `started`, `succeeded`, `failed`, `superseded`,
  `timedOut`, `skipped`); `suggest::clear()` drops pending requests at teardown.

### 5.2 AI agents (automation/, sta-mcp)

User and security documentation: `docs/MCP.md` (Korean guide `docs/MCP.ko.md`). DevTools measurements:
`docs/research/automation.md`.

```
MCP client ─stdio─► sta-mcp ─NDJSON over the channel─► automation/pipe.rs (threads)
                   \\.\pipe\sta-agent-<random128>            └► session.rs → tools*.rs → page.rs → cdp.rs (UI thread)
                   or <data>/sta/agent.sock (macOS)
```

- **Core** (`sta-core/src/agent/`, `store/agent.rs`): channel message types (golden JSON
  tests), the tool catalog and argument types (shared with the bridge's static `tools/list`),
  URL/site/private-network/scope/access policy, the AX-tree → outline builder with redaction and
  token budgets, refs (`tab.generation.n`), key combos, error codes. Settings `agent*` (serde
  defaults, unknown enum values load as off). The store keeps sessions, approval prompts, agent
  tabs (opened/shared), the last 5 actions and held downloads (`UiState.agent`, `TabView.agent`);
  `Effect::AgentEndpoint{enabled}` follows `settings.agentAccess`.
- **Channel** (`pipe.rs` + `win.rs` on Windows, `socket.rs` + `unix.rs` elsewhere — one module
  name, `automation::pipe`, and one API: `endpoint_name`, `start`, `send`, `close`, `PipeEvent`,
  `ClientIdentity`). Windows: `CreateNamedPipeW` with `FILE_FLAG_FIRST_PIPE_INSTANCE`,
  `PIPE_REJECT_REMOTE_CLIENTS` and SDDL `D:P(A;;0x12019f;;;<user SID>)S:(ML;;NWNR;;;ME)` (read and
  write for the user, including `FILE_CREATE_PIPE_INSTANCE`, which the server's second and later
  instances need; at most 4 instances); overlapped
  I/O on a listener thread plus a reader and writer thread per connection; clients from other logon
  sessions are dropped; the bridge's parent process image and Authenticode signer are determined
  on the reader thread. Threads only post `(connection, PipeEvent)` to the UI thread. The pipe name
  goes to `<data>/sta/agent-endpoint.json` (removed when access turns off and at shutdown).
  Unix: a socket at `<data>/sta/agent.sock`, mode 0600 in the user's own data directory (a stale one
  is replaced only after a connection attempt proves nothing listens on it), blocking I/O on the
  same three kinds of thread, and `getsockopt(SOL_LOCAL, LOCAL_PEERCRED/LOCAL_PEERPID)` for the
  client's uid (another user's is dropped) and pid. No signer is read there, so *Always allow* —
  which is keyed on it — is never offered.
- **Sessions** (`session.rs`): `hello` within 5 s → access/paused checks → approval
  (`AgentConnectionRequested` → core prompt or trusted client → `Effect::AgentAnswer`; at most one
  pending, 60 s back-off after Deny) → `welcome` → calls. At most 2 connections; per-session token
  buckets; every call is a UI task with its deadline and a cancel channel; results and errors go
  back as `result` lines; `agent.log` gets one line per call without content. Stop
  (`Effect::AgentDisconnect`) and access off send `bye`.
- **Tools** (`tools.rs`, `tools_input.rs`, `tools_page.rs`, `tools_browser.rs`, `page.rs`): 23 tools
  (catalog in core `agent/tools.rs`). Per-call tab targeting (explicit, the ref's tab, or the
  session's current tab), scope/URL/site checks, a per-tab FIFO (`busy` after 8; a waiting call
  that is cancelled or times out leaves the queue, or passes the tab on if it was already handed
  it), refs validated against the main frame's loader id, the tools' page functions only in the isolated world
  `sta-agent`. Reads, keyboard input, `select_option`, `fill_form` and `evaluate` work in
  background tabs; screenshots and mouse input (`click`, `hover`) require `tabs::is_tab_visible`
  and a non-minimized window; `scroll` needs a tab that was on screen once (a never-shown tab has
  a 0×0 viewport). Key presses first wait for `page::KEY_READY_IN` (a document that hasn't painted
  drops input for ~500 ms after its commit; background tabs never paint); `Input.insertText`
  doesn't need it. Results report `urlChanged` by comparing the committed URL before and after.
  `tab_show` marks the tab (`ui::note_agent_show`) so the hidden-tab focus rescue
  (`overlays::restore_main_focus`) falls back to the sidebar instead of focusing it.
  - `click` and `hover` share `element_point` (scroll into view → content quad center → hit test);
    `type` and `fill_form` share `fill_text`; `fill_form` classifies each field with a page
    function (text, select, check, value, file) and sets checkboxes with focus + Space (a mouse
    click as fallback on screen).
  - `page_find` runs Rust `regex` (core `agent/find.rs`) over the same extracted text as
    `page_text`, so offsets are interchangeable; no page-side regex.
  - `evaluate` (full access + `agentScripts`): `page::run_agent_function` resolves the document or
    the ref in the isolated world, or — `world: main` — with `DOM.resolveNode` without a context
    (the page's main world), then `Runtime.callFunctionOn` with `awaitPromise`; no new DevTools
    method. The tab becomes agent-controlled (scripts can navigate).
  - `page_screenshot {fullPage}`: `Page.captureScreenshot` with `captureBeyondViewport` and a clip
    of the scroll size (≤ 16384 CSS px), only in tabs agents opened (it briefly resizes the view).
  - `console_messages`: the tab client's `DisplayHandler::on_console_message` (client.rs) →
    `automation::console`, a 500-entry ring per tab (core `agent/console.rs`), recorded only while
    the endpoint is open and dropped when access turns off; never for UI browsers.
  - `request_tab_access` (read-only access suffices): `session::ensure_tab_access` dispatches
    `AgentTabAccessRequested` → core queues an `AgentPromptKind::Tab {session, tab, reason}`
    prompt (answered at once when the tab is already in scope, gone, or agents are off/paused);
    `AnswerTabAccess` (caller-checked like the other answers) shares the tab and answers every
    waiting request of that session for it; a prompt for a tab that closes is denied in
    `reconcile_agent`. The call holds 20 s like site prompts; the prompt stays 2 min.
  - `history_search` / `downloads_list` read the store (`History::search`, `UiState.downloads`)
    behind `agentHistory` / `agentDownloads` and format lines in core `agent/listing.rs` (only URLs
    `policy::check_url` accepts; download file names and states, never paths or URLs).
  - Cross-site frames are not traversed (no `Target.setAutoAttach` sessions): snapshots mark them,
    element calls inside frames fail with `unsupported_frame`.
- **DevTools client** (`cdp.rs`, `exec.rs`): one observer registration per browser, registered
  before the first send; the observer only copies bytes and posts (it runs inside the first
  `send_dev_tools_message`); one id allocator per browser; a timeout per call; detach fails pending
  calls. The method allowlist is an enum with typed parameter structs, and a unit test scans
  `src/automation/` for any other quoted method name. Recipes are `async fn`s on a small UI-thread
  executor whose wakers post poll tasks (never polled inside a CEF callback). Accessibility is
  released 30 s after the last snapshot and on Stop. Calls go to **tab browsers only** (UI
  surfaces, Chrome-created browsers and DevTools frontends are refused).
  - CEF gives a browser **one** DevTools session that every in-process client sees, so the agent
    client shares it with the shell's own client (`devtools_cdp.rs`: debug requests, the docked
    DevTools bridge — whose session S and nested sessions carry the frontend's own traffic — and the
    extensions backend later, each with a closed method list). They
    stay apart by id and session: the three ranges partition the id space — agent ids stay
    ≤ `0x3FFF_FFFF`, `devtools_cdp` uses `0x4000_0000`–`0x6FFF_FFFF` (wrapping inside it), and the
    debug-only test surface's raw client starts at `0x7000_0000` — each client ignores ids outside
    its own range (a unit test asserts they are disjoint), and the agent client drops every message
    carrying a `sessionId` it did not create (it creates none) — checked on the raw bytes before
    anything is copied. A unit test keeps `src/automation/` from naming `devtools_cdp`.
- **Guards** (`guards.rs`) for agent-controlled tabs (set by an agent action, inherited by popups,
  cleared by Stop, access off or the user's own key input — DevTools key events never reach
  `on_pre_key_event`): external protocols get no gesture (client.rs), downloads are held
  (downloads.rs → `AgentDownloadHeld`), page fullscreen is exited (client.rs), file choosers are
  cancelled (tab client `DialogHandler`), permission prompts are dismissed (permissions.rs),
  JavaScript dialogs are held for `handle_dialog` and `beforeunload` accepted (`ShellJsDialog`),
  Peek interception is skipped and popups open as background tabs (client.rs, tabs.rs), and
  `on_before_browse` cancels blocked and private-network hosts in every frame and unapproved sites
  in the main frame.
- `app.rs` adds `--disable-backgrounding-occluded-windows` when `state.json` has agent access on.
- `AnswerAgentConnection` / `AnswerSitePermission` / `AnswerTabAccess` are refused over IPC (403)
  unless they come from the agent overlay surface or the Settings page
  (`automation::may_answer_prompts`, also inside `commitOmnibox`; `sta.log` records which
  browser answered).
- **Threading.** Pipe I/O runs on a listener thread plus a reader and a writer thread per
  connection; they only post `(connection, PipeEvent)` to the UI thread. Everything else — sessions,
  policy (store reads through `controller::with_store`, commands through `controller::dispatch`),
  tools, guards, the DevTools client — runs on the CEF UI thread: a tool call is an `async fn` on
  the `exec` executor whose wakers post poll tasks, so no future is polled inside a CEF callback
  and no `RefCell` borrow is held across `send_dev_tools_message`. The bridge is a separate process
  with a single-threaded tokio runtime.
- **What the user sees** (`ui.rs`, `frame.rs`, core `store/agent.rs`):
  - core emits `ShowAgentOverlay{prompt}` / `HideAgentOverlay` for the first prompt (re-emitted when
    it changes) or the activity panel (`ToggleAgentPanel`, `CloseAgentPanel{focusLost}`);
    `automation::ui` shows `Overlay::Agent`, flashes the taskbar button (`FlashWindowEx`, until the
    window comes to the front) for connection prompts, and keeps a prompt from taking focus while
    the user types (`on_pre_key_event` timestamps). The page keeps its buttons inert for 1 s after
    a prompt appears and after each key. Unanswered connection and site prompts are denied after
    2 min (`session.rs`);
  - the **agent frame**: the tab wrapper's 2 DIP ring. tabs.rs keeps each wrapper's base color
    (frame, or accent for the focused split pane) and paints it through
    `shown_wrapper_color(tab, base)` → `frame::wrapper_color(tab, base)`, which returns the agent
    color (`#E8641B`, dark `#FF9150`) for agent-controlled tabs of connected, not paused sessions
    (not in page fullscreen: no inset). The same shown color is used everywhere the wrapper color
    matters for the rounded content card (§4.4): the wrapper background (`set_wrapper_color`,
    `WrapperDelegate::on_theme_changed`), the tab view's background (`view_background`; the surface
    while the tab is in Peek, which has no frame) and the ring of the pane's corner masks
    (`visible_pane_rects`), so the frame follows the rounded corners. `frame::schedule_refresh`
    (posted, coalesced) calls `tabs::refresh_wrapper_color` when control changes
    (`guards::mark_controlled`, user keys, Stop, access off, session end —
    `guards::release_session` also stops the guards of a session that ended), which re-applies the
    wrapper and view colors and re-lays out the corner masks when the tab is on screen;
  - the topbar chip, the sidebar glyph and Settings → AI agents (MCP) are pages reading
    `UiState.agent` (PROTOCOL §9); `agent.testConnection` spawns `sta-mcp.exe --check`
    (`CREATE_NO_WINDOW`, 10 s); the session-end toast "Archive N agent tabs" comes from core.
- **Bridge** (`crates/sta-mcp`): `rmcp =3.4.0` stdio server (hand-written `ServerHandler`, no
  macros; answers `initialize` and `server/discover`), static tool list, results as content plus
  `_meta["sta/structured"]` (never `structuredContent`), lazy pipe connection with owner-SID,
  server-session, mandatory-label (≥ medium) and server-pid (= endpoint pid) checks and
  `SECURITY_IDENTIFICATION` QoS, approval hold (20 s),
  cancel forwarding, no reconnect after `bye{user_stopped|access_off}`, and launching the sibling
  `sta.exe` (clean environment, no inherited handles, job breakaway, never from an MSIX
  package). The endpoint file is looked up in `<data>/sta/` first, then in the layouts of a legacy
  data folder that sta runs in place (§3 step 4); of the files found, the first whose process is
  alive wins.

## 6. Custom scheme and assets (scheme.rs)

- `sta` is registered with `STANDARD | SECURE | CORS_ENABLED | FETCH_ENABLED |
  DISPLAY_ISOLATED` in every process.
- One `SchemeHandlerFactory` serves embedded files from `ui/` with an in-memory `BytesHandler`
  (`ipc.md` §2.4). Don't use `wrapper::resource_manager` (it deadlocks) or `StreamResourceHandler`
  for empty bodies (the load fails).
- **URL mapping**: `sta://<host>/<path>?query#frag`. Query and fragment are ignored for
  lookup.
  - `path` starting with `common/` → `ui/common/<rest>`
  - empty `path`, or a path with no file extension → `ui/<host>/index.html`
  - otherwise → `ui/<host>/<path>`
  - a path under `__ext-icon/<id>/<px>` keeps its path (an extension icon, served same-origin by
    the `command` and `settings` hosts); its last segment is a number, which the "no extension means
    index.html" rule above would otherwise swallow
  - hosts (15): `sidebar`, `topbar`, `command`, `empty`, `peek`, `find`, `permission`, `switcher`,
    `toast`, `settings`, `archive`, `history`, `boosts`, `agent`, `extension` (the Ctrl+E popup
    card's header). `scheme::DEBUG_HOSTS`, the debug-build-only extra list, is empty today
  - `..`, `.`, `\` or `:` path segments → 400; non-GET/HEAD → 405; anything else unknown → 404
  - a missing `ui/<host>/index.html` of `find`, `permission`, `switcher`, `toast`, `peek` or
    `extension` is served the built-in placeholder page (§4)
- **Response headers**:
  - `Content-Security-Policy: default-src 'none'; script-src 'self'; style-src 'self'
    'unsafe-inline'; img-src 'self' data: https:; font-src 'self' data:; connect-src 'self';
    base-uri 'none'; form-action 'none'; frame-ancestors 'none'`
  - `Cache-Control: no-store`
  - `X-Content-Type-Options: nosniff`
  - `Referrer-Policy: no-referrer`
- UI rules: no inline scripts in UI pages; each `index.html` also carries the same CSP minus
  `frame-ancestors` as a `<meta http-equiv>`, so mock mode enforces it too (`frame-ancestors` only
  works as a header, and Chromium logs a console error for it in a meta tag). Render every
  page-provided string (titles, URLs) with text nodes, never `innerHTML`/`dangerouslySetInnerHTML`.

## 7. Persistence

- Profile dir `<data>/sta/`:
  - `state.json` (`core::State`, pretty);
  - `history.json`;
  - `one-time-permissions.json` (shell: permission grants allowed without "Remember", written
    synchronously before Chromium stores them, reset at startup; §4.1 "Permissions");
  - `agent-endpoint.json` (shell: the agent pipe name while agent access is on; §5.2).
- `<data>/Logs/agent.log`: agent connections and tool calls without content (§5.2).
- `settings.animations` (§4.7) needed **no** version bump: a profile without the field loads the
  defaults, its maps load leniently, and unknown keys inside the 128-entry cap are kept.
- `state.json` carries `version` (`STATE_VERSION` = 2). Loading an older one migrates it: below 2
  (or without a version) `settings.searchSuggestions` becomes `true`, because the setting was
  hidden and did nothing before. The load report notes the migration, so the profile is saved
  again.
- Internal URLs saved before the rename: `Store::load` rewrites every string of `state.json` and
  `history.json` that starts with the legacy scheme (also behind `view-source:`, scheme
  case-insensitive) to `sta://…` before parsing (`legacy::upgrade_json`: pinned and favorite
  pages, Today and archived tabs, split snapshots, the reopen stack, boosts pages, history, and
  anything added later). A rewrite adds a load warning, so the upgraded file is saved.
  `omnibox::classify` maps typed legacy URLs the same way, and `app::argument_to_url` maps
  command-line and relaunch arguments (`legacy::upgrade_url`); `OpenUrl` itself doesn't, since
  web content reaches it.
  Tests: `crates/sta-core/tests/legacy_migration.rs`, `legacy.rs` unit tests.
- Written atomically (`persist::write_atomic`): debounced (1 s, at most 5 s under continuous
  changes) after `take_dirty()` reports changes (only the dirty file is written), serialized on
  the UI thread and written in order by a writer thread; synchronously on `SaveNow` (if the writer
  thread can't start, every save is synchronous).
- `Store::load` returns a `LoadReport`; corrupt files are quarantined before any save.
- Ids: when a loaded profile has any id above `MAX_ID / 2` (`MAX_ID` = 2^53 − 1, the largest safe
  JavaScript integer), or a `nextId` that high, load renumbers every id to `1..=n` in order and
  rewrites all references (items, containers, spaces, favorites, MRU, archive entries and their
  split snapshots, reopen stack, boosts); the report carries a "renumbered" warning.
  `Store::alloc_id` never returns more than `MAX_ID`.
- Window bounds and maximized state are saved through `WindowStateChanged`.
- Session restore loads only the active item of the active space; other tabs stay unloaded until
  activated.

## 8. Testing strategy

1. **Core**: `cargo test -p sta-core`. The reducer is deterministic (pass `now`).
   `cargo test -p sta-core -- --ignored gen_fixtures` regenerates
   `ui/common/fixtures/*.json` (except `appInfo.json`) from the real serde types for UI mock mode.
2. **UI in isolation**: every page works in a normal browser against `ui/common/mock.js` (fake
   backend with fixture state). Serve with `node tools/ui-serve.mjs` (port 8123 by default;
   correct MIME types for module scripts, `Cache-Control: no-store`) and open
   `http://127.0.0.1:8123/sidebar/?mock`. Headless screenshots with `tools/ui-shot.ps1` /
   `tools/ui-shot.mjs` (serves `ui/` in-process, drives headless Edge over the DevTools protocol
   with an exact viewport, and stops everything afterwards; see `ui/README.md`):
   `powershell -NoProfile -ExecutionPolicy Bypass -File tools/ui-shot.ps1 -Path '/sidebar/?mock' -Width 248 -Height 900 -Out <png> [-Dark] [-Console]`.
   Exit codes: 0 = captured and clean; 1 = capture failed (no PNG); 2 = captured, but the page
   reported problems (console errors, uncaught exceptions, failed loads of its own `ui/` files, or
   a mock page that never sent `ui.ready`). Don't use `msedge --screenshot --window-size=…`
   directly: headless Edge widens windows narrower than about 500 px, so sidebar-width captures
   come out wrong. `node tools/check-mock-commands.mjs` checks the mock's command validation
   against `command.rs` (exit 1 on a mismatch; run it after changing commands).
   `node tools/check-mcp-docs.mjs` (after `cargo build -p sta-mcp`) checks `docs/MCP.md` and
   `docs/MCP.ko.md` against the bridge's real `tools/list` (tools, inputs, enums, limits, access
   lines, error codes).
   **Motion** (§4.7) has two of its own: `node tools/check-motion.mjs` holds the registry
   (`sta-core/src/motion.rs`), the UI catalog (`ui/common/motion-catalog.js`) and the CSS gates
   (`ui/common/tokens.css`) together and enforces the rules that span files (no `animationend`, no
   transform on a tracked overlay root, no animated property whose duration comes from two keys'
   tokens, an allowlist for the layout transitions that are allowed to stay); `node
   tools/motion-check.mjs` runs the motion runtime in mock mode in headless Edge (levels, per-key
   gates, a keyed animation starting by itself, static indicators at `off`, ghosts being inert
   clones, size tracking under transforms, a burst of state pushes starting nothing, and every
   area's own keys in its own surface) — exit 0 clean, 2 a failed check.
3. **Shell unit tests**: `cargo test --workspace` runs everything; `-p sta` the shell's
   (rounded-corner images, snapping, mask placement incl. the agent frame ring, card grid, agent
   pipe / security descriptor / DevTools allowlist / tool helpers, URL → asset mapping,
   external-protocol classification/escaping, download file names and Mark-of-the-Web contents,
   permission answers, request bits → content settings, one-time grant bookkeeping, accelerator
   table and Esc panel rule, which text search suggestions may send, extension manifests on disk
   (ids from a `key` or a path, localized names, pages, containment), the shell's DevTools client
   (id ranges, per-user method lists, `automation/` never using it), platform helpers, data-dir
   migration); `-p sta-mcp` the bridge's (channel messages against fake pipe servers, untrusted
   pipes, endpoint lookup incl. a legacy data folder used in place, launch environment, both MCP
   protocol eras over real stdio).
4. **End to end** — **the suites drive the browser through MCP** (§8.5), not the DevTools port:
   `cargo build -p sta -p sta-mcp --features test-hooks` first, then `node crates/sta/e2e/<suite>.mjs`.
   - `lib.mjs`'s `Instance` starts `target/debug/sta.exe --sta-data-dir=<tmp>
     --disable-backgrounding-occluded-windows --sta-test-hooks` with `STA_E2E=1` (the occlusion flag
     keeps a covered window rendering, so window captures and rAF-driven pages work while other
     windows are on top), waits for `<data>/sta/agent-endpoint.json`, and spawns
     `target/debug/sta-mcp.exe --data-dir <tmp> --no-launch` for the session. **The data directory
     alone makes a run unique** — the pipe name is random per browser, so there is no port to assign.
   - The `debug.*` requests (`crates/sta/src/debug.rs`: `debug.info` snapshot, `debug.dispatch` incl.
     shell events, `debug.execute` effects, `debug.openTab`, `debug.accelerator`, `debug.focus`,
     `debug.realKeys`, `debug.resetPermissions`, `debug.hoverInput` (hover reveal on/off, a virtual
     pointer, one guarded real-cursor check that compares against where the cursor landed and puts it
     back) and `debug.postMouse` (mouse messages posted to our own window: native clicks without
     moving the OS cursor)) are reached as the matching `test_*` tools; `Instance.invoke()` is
     `test_invoke`, a **real** `window.sta.invoke` in the surface's own frame, so the trusted-frame
     check, the message router and the `window.sta` shim stay covered; raw DevTools is `test_cdp`.
   - The suites start instances with `STA_DEBUG_HOVER_REVEAL=0`, so a cursor resting at the
     window's left edge never floats a hidden sidebar during unrelated checks.
   - Window info, `WM_NCHITTEST`, `WM_CLOSE`/`SC_RESTORE`, window-only screenshots, PNG pixels, the
     clipboard and Mark-of-the-Web are `test_window`, `test_hit_test`, `test_window_message`,
     `test_capture`, `test_pixels`, `test_clipboard_*`, `test_zone_identifier` — **no PowerShell on the
     suites' path, and no console window** (each suite also asserts that at runtime with
     `test_console_windows`: every console window *shown* while it ran, flashes and
     Windows-Terminal-hosted ones included). The one exception is `agent-e2e`'s consent phase, which by
     definition runs before any session may call a tool: its two captures there are PowerShell.
     `tools/capture-window.ps1`, `e2e/win-probe.ps1` and `e2e/png-pixel.ps1` remain the human/agent
     debugging tools.
   - **Never capture the whole screen.** Always kill the test instance afterwards by its PID
     (`taskkill /PID <pid> /T /F`), never every `sta` process.
   - By hand, against an instance started with `STA_REMOTE_DEBUGGING_PORT=<port>`:
     `node tools/cdp.mjs list | eval <host> <js> | shot <host> <png>` (`CDP_PORT` selects the port).
     Debug builds still open that port; no suite uses it except the liveness check below.
   - What is still *not* MCP, and why, is listed in `C:/ast/tmp/s6/cdp-residue.md`: launching and
     killing the browser, exit codes, a startup that fails, a refused or forwarded second launch, the
     data-folder migration, files and the local HTTP fixtures — plus one `GET /json/version` check
     that the debug build opens `STA_REMOTE_DEBUGGING_PORT` at all, and the two suites whose *driver*
     cannot be an MCP session: `agent-e2e` (its subject is the channel: it counts the channel's
     connections and asserts that a client is welcomed only after a user click) and `migration-e2e`
     (arming needs an explicit data dir, which is the thing it must not pass). Both use
     `lib.mjs`'s `CdpInstance`, which says so.
   - Suites (each takes `E2E_DATA_DIR`, refuses to start only if its *own* data dir is in use, and
     kills only its own process tree; `CDP_PORT` still matters for `shell-e2e`, `agent-e2e` and
     `migration-e2e`):
     - `node crates/sta/e2e/shell-e2e.mjs [--keep-open]`: layout, IPC, security (web tabs,
       UI location guard, external protocols incl. typed ones via `OpenExternal`), command bar,
       search suggestions (`STA_SUGGEST_URL` → a local server it starts: parsing, Korean
       text, caller check, no cookies sent or stored, URL-like text never sent, superseded
       requests, timeout, oversized/failed bodies, the setting and engines without suggestions,
       suggestions in `omnibox.query`), window, tab lifecycle, relaunch, shutdown (`E2E_EXE`
       selects another build);
     - `node crates/sta/e2e/tabs-e2e.mjs [--keep-open] [--only=popups,errors,…]`: popups,
       Peek interception, the **Alt+click preview** (`preview`: every kind of link — plain,
       `target=_blank`, SVG, shadow tree, iframe, cross-origin iframe, `download=` — plus no download,
       the refusals, the shapes it leaves to the page (`javascript:`, `href="#"`, a fragment, an
       over-long href), a sandboxed frame, Alt+Enter (keyboard activation is not a click), a
       page that cannot forge the gesture even after redefining `isTrusted`/`altKey`, Expand into a
       split, Alt+click inside Peek replacing it, real Esc, the popup-Peek rule from the tab behind it,
       `peekEnabled` off, a `Content-Disposition` attachment (downloads, and the empty preview closes
       itself), and light/dark captures at 100 % and 150 %),
       menus, error page, downloads (incl. the Mark-of-the-Web stream),
       permissions (incl. Block without Remember asking again, four one-off Blocks each prompting,
       Allow without Remember ending with the origin's last tab or a navigation away, remembered
       allows persisting, UI-page prompts settling), find, zoom, audio, fullscreen,
       boosts, focus, replace, close, crash, DevTools (also `E2E_WWW`; its two local HTTP fixtures
       are Node servers on ephemeral ports, `E2E_HTTP_PORT` / `E2E_RETRY_PORT` pin them);
     - `node crates/sta/e2e/chrome-e2e.mjs [--keep-open]`: overlays and their stacking
       (toast, permission prompt and command bar above a later Peek, focus kept), rounded corners
       in window captures (`o.round`: content corner masks, the focused ring, the command bar card,
       Peek's page corners, a toast staying above newly shown masks; `o.round150`: a second
       instance at `--force-device-scale-factor=1.5` with its own data dir), real keyboard
       input (don't type while it runs; incl. Alt+3 to an empty space then Alt+1, Ctrl+J / Esc
       from a page, Esc in a page keeping space sheets open), window states, page fullscreen,
       folder picker, Alt+F4, restart/session restore (incl. a one-time notifications grant reset
       at startup while a remembered one persists), stuck-shutdown path; the sidebar hover reveal
       (`w.hover`: dwell, reveal latency and hide timing, no page blur, focus back after a click,
       a press in the resize band, an open menu locking it and a press outside closing the menu and
       hiding it, a row click closing the command bar, Ctrl+J pinned float + Esc, panels that dock
       incl. "Edit Pinned Page" with real typing, Esc reaching the page, Ctrl+S, drag holes, page
       fullscreen / F11 / resize band, Peek, live width, page reload, no poll while minimized, a
       real-cursor check; Alt+F4 while floating; a restart with the sidebar parked); the animation
       settings (`m.motion`, §4.7): `debug.info.motion`, the Windows "Animation effects" setting as
       runtime-only state, the master switch and the per-group and per-key switches reaching every
       surface as `data-motion` / `data-anim-off`, the shell's hide delays never dropping below
       50/60 ms at any level, what `state.json` does and does not hold, and a stale
       `closeCommandBar {seq}` being ignored. Its baseline level is **derived** from the machine's
       own Windows setting (`reduced` on a machine that has animation effects off — that is the
       correct answer with the shipped `followSystem: true`), then pinned to "Windows animates" for
       the middle of the section and put back at its end. `m.motion.ui` then checks the sidebar's and
       the command bar's own animations in the real surfaces — a row fading in, a closed row's inert
       ghost (found with its duration token stretched, so the probe can look at it), the selection
       glider under a real ArrowDown, the top bar's controls fading in after the resize, and one
       per-key switch taking exactly one animation away — and `m.motion.perf` measures a 200-row
       sidebar in a space of its own (unloaded tabs, no browsers, deleted again afterwards) over a
       reorder, an insert, a remove, Clear Today and two space switches, against the plan's budgets:
       no long-animation-frame entry over 50 ms, p95 frame ≤ 20 ms, a FLIP measure ≤ 4 ms.
       `m.motion.shell` (after `w.hover`) is the shell's own half: the waits around a blank frame and
       their floors at every level, a **lingering** toast that is still visible and still a no-drag
       hole while its page blanks, a split's restack starting no animation there, a show during a
       linger cancelling the exit by generation (a toast shown again, a pin during the sidebar's hide
       fade, a dock during the park), `ackTimeouts` and `earlyHides` staying 0 for the whole run, FLIP
       staying suspended while `html.is-dragging` is set as a real state push arrives, and `SetChrome`
       landing at the midpoint of the theme cross-fade. The restart section (`t`) then checks that a
       stored group switch and per-key choice come back through the real load path. `debug.motion {floorMs}` raises
       the exit floor, so a linger is a state the check walks into instead of a race against a 108 ms
       timer; every measurement of real timing runs with the real floors.
       `--only=<section,…>` runs a subset;
     - `node crates/sta/e2e/migration-e2e.mjs [--keep-open]`: the migration from before the
       rename (§3, §7) with `LOCALAPPDATA` pointed at `<E2E_DATA_DIR>/LocalAppData` and no data dir
       switch: a legacy profile made by a build from before the rename (`E2E_OLD_EXE`, default
       `target/debug/astatine.exe`; written by hand when missing), sta refused with an error box <!-- rename:keep -->
       while that build runs (exit 1, nothing moved, no URL forwarded), the folder and profile
       subfolder moved, pinned / favorite / Today pages back as `sta://` with IPC, a typed legacy
       URL, a legacy URL on a forwarded launch's and on a first launch's command line (never
       handed to the OS), a second start with nothing to do, a new legacy folder next to the
       migrated one left alone, and a legacy folder that can't be moved (used in place with
       `sta-in-place.lock`, a second launch forwarded to it, then moved by a launch that passes
       the default folder as `--sta-data-dir`). Needs the foreground for the error box (run it
       under the desktop lock);
     - `node crates/sta/e2e/agent-e2e.mjs [--keep-open]`: the MCP path end to end — speaks
       MCP JSON-RPC to `target/debug/sta-mcp.exe` (`E2E_BRIDGE`) against two local sites, and
       answers every prompt like a user, with trusted DevTools mouse clicks on the agent overlay,
       the topbar chip, the toast and Settings (no auto-approve): consent (unverified client
       without Always, clicks in the first second ignored, 403 from another surface, Deny, Allow
       for this session, Always + Revoke, Deny from Settings), site prompts (This session, Always,
       Deny, no focus while typing), endpoint file, tools/list, open → snapshot → type → click →
       text → screenshot, `stale_ref`, background-tab limits, internal pages, URL schemes, private
       network, blocked hosts, scope, read-only, guards (mailto:, dialogs, file chooser, popups,
       held download, fullscreen), the agent frame (edge pixel, the rounded corner masks' ring and
       an arc pixel), sidebar glyph, chip and activity panel, takeover (`debug.tabKey` →
       `user_active`), Stop from the chip → paused without reconnect, the session-end toast
       archiving agent tabs, Resume, Test connection, access off, launching the browser. Run it
       under the desktop lock (approval clicks need the window in front). Its browser is armed with
       `--sta-test-hooks-no-approve`, which serves the test tools but relaxes nothing: no
       auto-approval, no in-memory full access and no endpoint until the user turns access on.
     - `node crates/sta/e2e/extensions-e2e.mjs [--keep-open] [--only=a,n,p,…]`: the browsers
       Chromium creates for extensions (§4.5), driven through the in-repo probe extensions
       (`crates/sta/e2e/extensions/`, loaded with `--load-extension`; their service workers are
       browser-level DevTools targets, reached with `test_targets` + `test_attach` + `test_eval`) and
       `debug.foreign*`: adoption (options
       page, embedded options rewritten, `windows.create`, `Target.createTarget`) with no visible
       Chrome window and one request per page; the post-install path (a probe copied into the
       profile's `Extensions` directory plus a new-tab-page window → the "… added" toast, a welcome
       tab that arrives 1.5 s later, the window closing when it goes quiet); the policy (undeclared
       extension pages ask, web-accessible ones open, `chrome://settings` is dropped, a window flood
       is rate limited, incognito is refused, and a legitimate window right after a burst of asks
       still opens — the phase-1 rate-limit decision); an install that lands before the +12 s startup
       re-scan still being announced (its own instance); `_crx_` popup windows staying native, titled
       "… - sta", while real keys land in sta; a download postponing the close; DevTools refused on
       `sta://` pages; shutdown with Chrome-created browsers open; toast screenshots.
       `STA_E2E_WEBSTORE=1` adds an opt-in run against the real Chrome Web Store (network, throwaway
       profile) that accepts the install dialog with `test_dialog`. Run it under the desktop lock
       (real keys).
     Shared helpers are in `e2e/lib.mjs` (`Instance` on MCP, `CdpInstance` for the two exceptions
     above, the console-window assertion) and `e2e/mcp.mjs` (the MCP client, `PipeClient`).
5. **Through MCP** (`docs/TESTING.md`). Everything above that talks to a *running* browser can go
   through the agent channel instead of the DevTools port and the PowerShell helpers:
   `cargo build -p sta -p sta-mcp --features test-hooks` adds a debug-only surface of 35 `test_*`
   tools (`crates/sta/src/test_hooks/`, catalog in `sta-core/src/agent/test_tools.rs`, served by
   the bridge only to a browser that welcomed it with `testHooks: true`). It re-exposes the
   `debug.*` requests, plus the native window, `WM_NCHITTEST`, window captures and PNG pixels, the
   clipboard, Mark-of-the-Web, raw DevTools (`test_cdp`, no allowlist) and console-window
   detection. Four locks keep it out of anything a person runs: the feature (a release build with
   it fails to compile), `--sta-test-hooks` **and** `STA_E2E=1`, an explicit non-default data
   directory (else exit 2), and invisibility in `tools/list` when un-armed.
   `crates/sta/e2e/mcp.mjs` is the client library and `crates/sta/e2e/mcp-smoke.mjs` the smoke test
   (56 checks); `node tools/check-release-clean.mjs` proves the shipped binaries carry none of it, and
   `node tools/check-no-console.mjs` keeps every test helper from flashing a console window. All six
   suites in §8.4 run on this surface; `test_*` calls never mark a tab agent-controlled and never
   appear in the user's activity list, so the suites still see the browser a person would.

## 9. Milestones

- **M1 – core browsing**:
  - bootstrap, scheme, IPC;
  - frameless window with sidebar/topbar/content;
  - spaces, favorites, pinned, today;
  - tab lifecycle;
  - command bar (new tab / edit URL / switch tab / actions);
  - navigation;
  - persistence and session restore;
  - accelerators;
  - downloads (basic);
  - window controls and drag;
  - shutdown path.
- **M2 – Arc features**:
  - split view, Peek, archive page and auto-archive, clear today, reopen;
  - folders and drag & drop;
  - Ctrl+Tab switcher;
  - find bar, zoom, fullscreen;
  - permission prompts;
  - boosts, settings page, history page.
- **M3 – polish**:
  - sidebar hover-reveal (done, §4.3), space swipe (done: the wheel swipe and the
    `sidebar.spaceSwitch` animation, §4.7), theme editor (done: Settings › Appearance and the space
    sheet's OKLCH hue/chroma sliders);
  - context menus (done: `context_menu.rs` — link, image and Inspect);
  - search suggestions (done, §5.1);
  - favicon cache (`sta://favicon/`);
  - drag-to-content-edge split;
  - JS dialogs in hidden tabs.
- **M4 – the phases after M3** (all shipped; each has its own section above and a phase record
  under `C:/ast/tmp/`):
  - rounded corners and the radius scale (§4.4);
  - the rename Astatine → sta, with data-folder migration and legacy URL upgrades (§6); <!-- rename:keep -->
  - the MCP server and the agent permission model (§5.2, docs/MCP.md);
  - the debug-only test surface and the MCP-driven e2e suites (§8.4, docs/TESTING.md);
  - Chrome extensions: the listing, the Ctrl+E picker, popup cards, Settings › Extensions and the
    foreign-window rules (§4.5-4.6);
  - DevTools docked inside the window (§4.1);
  - the Alt+click link preview in Peek (§4, PROTOCOL §13);
  - the 36-key animation system and Settings › Animations (§4.7).
- **Not done** (and not scheduled): onboarding, importing bookmarks/history/passwords from another
  browser, autofill, more than one window, and anything but Windows. `docs/STATUS.md` is the
  current list of what works and what does not.
