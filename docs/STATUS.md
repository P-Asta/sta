# sta — status

What this browser does today, what it does not, how to build it and how to check it. Written to be
read by someone deciding whether to use or work on it, so it is a list of facts rather than a pitch.
Anything here that is not true is a bug; the e2e suites in §5 are what keeps most of it honest.

- Platform: **Windows 11 only** (Windows 10 is untested).
- Engine: **Chromium 152** through the `cef` crate `=152.3.0`, **Alloy** style with CEF Views.
- Language: Rust (two crates plus the MCP bridge) and plain HTML/CSS/JS for every surface.
- Profile: `%LOCALAPPDATA%\sta` (release) or `%LOCALAPPDATA%\sta Dev` (debug), overridable with
  `--sta-data-dir=<path>`.

---

## 1. What works

### Window and sidebar
A frameless window with a vertical sidebar, a top bar and the page. The sidebar is resizable and
hides with Ctrl+S; while hidden, resting the pointer at the window's left edge floats it over the
page, and it hides again when the pointer leaves. Rounded content corners and a radius token scale.
Light, dark and system appearance, applied to every surface *and* to the native window frame.

### Spaces, tabs and the Arc lifecycle
- **Spaces** with an emoji icon and an OKLCH colour theme each (presets and hue/chroma sliders).
  Switch with the space icons, Alt+1–9, Ctrl+Alt+←/→, or — **over the sidebar only** — the mouse
  back/forward buttons and a horizontal wheel swipe.
- **Favorites** (a grid, max 12), **pinned tabs** (reset to their pinned URL) and **Today** tabs.
- Today tabs **auto-archive** after 12 h / 24 h / 7 d / 30 d idle (a setting). The Archive page
  restores them, split groups included.
- **Clear Today** with undo, **reopen closed** (Ctrl+Shift+T), folders, drag and drop, rename.
- Session restore with lazy loading: a restored tab has no browser until it is shown.

### Command bar (Ctrl+T / Ctrl+L)
URL-vs-search classification, 7 search engines plus a custom one, live suggestions from the selected
engine (fetched without cookies, and only while that setting is on), open-tab / history / archive /
space results with fuzzy ranking and frecency, inline completion, an actions mode (Tab, or type
`>`), background tabs (Alt+Enter) and a split mode. The Korean keyboard layout is undone in both
directions, so `ㄴㅅㅁ` finds "sta …" and `gksrmf` finds "한글 …".

### Split view and Peek
Up to 4 panes, side by side or stacked, with a focused-pane accent ring and Ctrl+Shift+1–4.
**Peek** is an overlay card: cross-site links from pinned tabs, script popups (OAuth), Shift+click,
and **Alt+click** (or Alt+middle-click) on any link in any tab. Split (◫) / Expand (⤢) / Esc /
Ctrl+O act on it. Alt+clicking inside Peek moves that preview on instead of nesting a second one.

### Browsing tools
Find bar with a match counter, print (Ctrl+P), view source (Ctrl+U), zoom (a chip in the URL pill),
downloads with a progress ring, popover and pause/resume/retry — every finished file tagged with
Mark-of-the-Web so SmartScreen and Office Protected View check it — site permission prompts
(Allow / Block this time, or Remember), the Ctrl+Tab recent-tab switcher, toasts, external protocol
hand-off (`mailto:`, `tel:`, app links), themed error pages, and internal pages at `sta://settings/`,
`sta://history/`, `sta://archive/` and `sta://boosts/` (per-site CSS/JS boosts with an editor).

### Chrome extensions
Extensions installed from the Chrome Web Store run in sta's tabs: service workers, content scripts,
blocking rules (ad blockers really block), extension pages and options pages. **Ctrl+E** opens a
picker of what is installed; Enter shows an extension's popup in a card at the top right of the page
or opens its options page in a tab; Alt+Enter opens the options page when there is one.
**Settings › Extensions** turns extensions on and off and removes them, showing Chrome's own
permission warnings and the source first. A window an extension (or the store's post-install page)
opens is hidden and its content is adopted as an sta tab, so no Chrome window appears. §2 lists what
does not work.

### DevTools
F12 docks DevTools **inside the sta window** (not a separate Chrome window): Elements, Styles,
Console, Sources with breakpoints, Network, Performance, Memory, Application, the element picker
(Ctrl+Shift+C), device mode, local overrides, cross-origin frames and workers. Ctrl+Shift+I opens,
then focuses, then closes. Ctrl+= / Ctrl+- / Ctrl+0 inside DevTools zoom DevTools. Undock (from
DevTools' own menu, or the `>` command) moves them to Chromium's own window for that tab and that
session. A pane narrower than 640 DIP — a split pane of a default-size window is about 505 — cannot
give the inspected page a usable width, so DevTools open in their own window there with a toast
saying why. §2 lists the gaps.

### AI agents (MCP)
`sta-mcp.exe` is an MCP server over stdio for Claude Code, Claude Desktop, VS Code, Cursor and other
clients. **23 tools**: open and close tabs, read a page as an accessibility outline with element
refs / as text / as a screenshot, find text, click, hover, type, fill forms, choose options, scroll,
answer dialogs, and — when allowed — run scripts, read the console, search history and list
downloads. Transport is a per-user named pipe in the profile folder (there is **no** debugging
port). A new client, a new site and a request for a tab each need the user's approval in the sta
window; the top bar shows a chip with Stop and the last actions, and a tab an agent is working in
gets an orange frame. `docs/MCP.md` (and `docs/MCP.ko.md`) document every tool and error code.

### Animations
36 animation keys in 8 groups, with **Settings › Animations**: a master switch, "Follow Windows
animation effects" (on by default — Windows' own animation setting then gives sta reduced motion:
fades without movement), a switch per group and a switch per key, and Reset to defaults. Turning a
key off while its animation is playing settles that animation immediately.

### Updates
A `vX.Y.Z` tag builds a release archive and leaves a **draft** GitHub release
(`.github/workflows/release.yml`); publishing that draft is what offers it to anybody. A running
sta checks ~8 s after it starts, toasts "sta X is available", and Settings › About downloads it
(SHA-256 checked before anything is unpacked) and applies it on the next restart. Windows x64 only,
portable archive, no installer and no code signature yet — SmartScreen warns once. The whole thing
is off under the e2e harness and with `STA_NO_UPDATE_CHECK=1`. See `docs/RELEASING.md`.

### Keyboard

| Keys | Action |
|---|---|
| Ctrl+T | Command bar (new tab) |
| Tab (in the command bar) | Toggle actions mode |
| Ctrl+L, Alt+D, F6 | Edit current URL |
| Ctrl+W, Ctrl+F4 | Close tab (Today → archive, pinned → unload) |
| Ctrl+Shift+W | Close the window (quit) |
| Ctrl+Shift+T | Reopen closed |
| Ctrl+S | Toggle sidebar |
| Ctrl+D | Pin / unpin |
| Ctrl+Shift+C / +Alt | Copy URL / as Markdown (with DevTools docked: the element picker) |
| Ctrl+1..9 / Alt+1..9 | Go to item N / space N |
| Ctrl+Alt+↑/↓, ←/→ | Previous/next item, previous/next space |
| Ctrl+PgUp / Ctrl+PgDn | Previous/next item |
| Ctrl+Tab, Ctrl+Shift+Tab | Recent-tab switcher, forwards and backwards |
| Ctrl+Shift+K | Clear Today |
| Ctrl+Shift+= / Ctrl+Shift+- | Add split / separate pane |
| Ctrl+Shift+1..4, [ ] | Focus split pane |
| Ctrl+O | Expand Peek into a tab |
| Ctrl+F, F3, Shift+F3 | Find in page, next, previous |
| Ctrl+P / Ctrl+U | Print / view page source |
| Ctrl+= / Ctrl+- / Ctrl+0 | Zoom |
| Ctrl+E | Extensions picker (page first: a page that uses Ctrl+E keeps it) |
| Ctrl+J, Alt+F, Ctrl+, , Ctrl+H | Downloads, app menu, settings, history |
| F5 / Ctrl+R, Alt+←/→ | Reload, back/forward |
| Ctrl+Shift+R, Shift+F5, Ctrl+F5 | Reload ignoring the cache |
| F11, Alt+Shift+F | Fullscreen |
| F12 / Ctrl+Shift+I | DevTools (docked) / open, focus, close |

**F2 renames**, but it is not an accelerator: the docked sidebar handles it in its own keyboard
focus, so it does nothing while the sidebar is hidden or floating.

### Mouse on a link

| Click | Action |
|---|---|
| Alt+click, Alt+middle-click | Preview the link in Peek (see §2 for the exceptions) |
| Shift+click | Peek |
| Ctrl+click, middle-click | Background tab |
| Right-click on a link | Open Link in New Tab / Open Link in Peek / Copy Link Address, then Inspect |
| Right-click on an image | Open Image in New Tab / Copy Image Address, then Inspect |
| Right-click on a page | Inspect (not offered on sta's own `sta://` pages) |

---

## 2. What does not work, and why

### Not built
- **No onboarding and no import.** A first run opens straight onto the empty state with three
  keyboard hints. Nothing explains spaces, Today or Peek, and there is no way to bring bookmarks,
  history or passwords over from Chrome or Edge. (The one thing sta does inherit from Chrome
  automatically is the set of extensions other programs registered on the machine — §2 below.)
- **No autofill UI** of any kind, and password managers that pair with a desktop app (1Password)
  refuse sta.
- **One window, one profile.** No second window, no profiles, no incognito.
- **No split divider dragging** (fractions are equalised, or set by adding and removing panes).
- **No favicon cache** for `http:` sites.
- **Windows only.** `sta-core` and `ui/` are platform-neutral; the shell is Win32 throughout — and
  so is everything under `crates/sta/src/platform/`, the named-pipe agent endpoint, the window
  chrome, the hidden-window hooks and the registry work. A macOS build is therefore not a matrix
  entry away: it is a port. The release pipeline and the updater are already keyed by platform and
  wait for it (`docs/RELEASING.md` §4).
- **No update for anything but Windows x64**, for the same reason; the update itself is a full
  ~150 MB archive each time (no deltas).

### Chrome extensions
CEF's Alloy runtime — which sta's multi-view layout needs — gives an extension no Chrome window and
no view of sta's tabs, so:
- **no toolbar buttons** (sta has no extension toolbar, and no API can press one), **no extension
  keyboard shortcuts**, **no side panels**, **no extension context-menu items**;
- `chrome.tabs.query` / `chrome.windows.*` see nothing of sta, so anything built on "the current
  tab" does not work — **including popups that need it**. Such a popup opens in sta's card and
  usually shows its own error page; the card's header says "Needs the current tab" whenever the
  extension asks for `tabs`/`activeTab`, and a popup that renders nothing at all is replaced after
  three seconds by "This popup doesn't work in sta yet" with a link to the options page;
- **removing an extension uses Chrome's own "Remove …?" dialog** (Chromium skips that confirmation
  only for an extension removing itself). sta adds no dialog of its own;
- extension **popup windows** (`windows.create({type:'popup'})`) and sign-in flows
  (`identity.launchWebAuthFlow`) stay ordinary Chromium windows, with sta's caption colours, icon
  and "… - sta" title: their pages need a window of their own. Such a window therefore looks like
  sta's while an extension controls what it shows — read it as sta's chrome around someone else's
  page, exactly like a tab. Private (incognito) windows are refused, and an extension that keeps
  opening windows is stopped after three tabs in ten seconds;
- an extension can ask sta to open **another** extension's declared page; sta sees the page, not who
  asked, so its question names the page's owner rather than the asker;
- **`chrome.tabs.discard` on an sta tab crashes the browser.** This is an upstream CEF bug. Two
  crashes in a row start sta in safe mode, with every tab unloaded and extensions listed in
  Settings;
- extensions **other programs registered** (an app writes one into the Windows registry and
  Chromium loads it into every profile) arrive turned off and stay off until you allow them in
  Settings › Extensions, with Chrome's warnings and the source in front of you. One installed from a
  file on this computer can only be removed — sta cannot show where that code came from. sta does
  not stop Chromium from loading them;
- the listing comes from the profile on disk, which Chromium writes with a delay. An extension the
  user installs in this session is listed immediately anyway; one that appears for any other reason
  can take a second or two;
- the **Chrome Web Store** shows sta a "Switch to Chrome to install extensions and themes" banner
  and, at the default window width, pushes its own "Add to Chrome" button off to the right. Both are
  wrong about sta — installing works — and sta says so once per run when a store page opens. The
  store's layout is not something sta changes.

### DevTools
Docked DevTools work; the gaps are all of these and no others:
- the colour picker's **eyedropper** and the Security panel's **certificate viewer** do nothing;
- **Developer resources**, and any other fetch DevTools makes *for itself* rather than through the
  page (`loadNetworkResource`), fails with status 409 while docked. Source maps of the page load
  normally;
- the Application panel's **"inspect" button for a service worker** does nothing: a service worker
  has no DevTools window of its own here;
- **Workspaces / "Add folder"** and **"Save as"** use Chromium's own file dialogs;
- DevTools may only name **the page's own origins**: the Application panel reads and clears storage
  and cookies for the site being inspected, never for another one;
- "Inspect" inside a nested frame selects that frame's own node;
- **no DevTools on sta's own `sta://` pages** at all (a DevTools session there would reach sta's
  internal commands), and their context menu does not offer Inspect.

Anything else a panel cannot do answers with a protocol error rather than misbehaving quietly, and
the method's name shows up in `debug.info`.

### The Alt+click preview
- The gesture is **off in frames with an opaque origin** — a `sandbox` without `allow-same-origin`,
  because such a frame must not get a top-level page its embedder denied it. A frame sandboxed
  *with* `allow-same-origin` keeps the gesture.
- A frame where **scripting is off** never runs the listener, so **Chromium's own Alt+click download
  still happens there**. Everywhere else Alt+click no longer saves a link to disk.
- `sta://`, extension URLs and external protocols (`mailto:` and friends) are refused with a toast.
  A `file:` link can be previewed only from a `file:` page; from a web page it is ignored silently.
- A `javascript:` link is left completely alone. An in-page `#` link has nothing to preview but the
  click **is** cancelled, so Alt+clicking one does not scroll the page to it (a plain click does).
- A scripted click never triggers it (`isTrusted` only), and neither does a key: Alt+Enter on a
  focused link activates the link the ordinary way.
- A link that turns out to be a `Content-Disposition` attachment still downloads; the empty preview
  closes itself and the download toast is the outcome.

### Overlays and notifications
- **No blur or dimming behind overlays**: CEF cannot draw translucent browser views. The rounded
  corners and soft shadows are native pieces around the pages, and they swallow clicks in their own
  small areas (about 10×10 DIP at each page corner, and the 8 DIP shadow ring of a card).
- **Web notifications are Chromium pop-ups** at the bottom right of the screen, not Windows
  notification-centre entries. A permission request from a background tab waits until that tab is
  shown, as in Chrome.

### Motion
A CSS **transition** already running is not retro-actively ended when its key is switched off (only
WAAPI animations are settled); the longest visible tail is about 320 ms.

---

## 3. Building

Prerequisites: Rust stable (MSVC), Visual Studio 2022 C++ build tools, CMake and Ninja on `PATH`
(`pip install --user cmake ninja`). Build from a short path — the CEF wrapper's CMake build fails
with `C1083` under very long directories.

```powershell
cargo build --release          # or: cargo build
```

The first build downloads CEF (~600 MB extracted) into `.cef/`; `cef-dll-sys` copies `libcef.dll`,
the `.pak` files and `locales/` next to the executable. The Windows resources (icon, manifest,
version info) need `rc.exe` from the Windows SDK. `crates/sta/res/make_icon.py` regenerates the app
icon (Pillow required); commit `sta.ico`, the four `icon-*.png` and `preview-256.png` together —
`node tools/check-icon.mjs` refuses anything else.

Release binaries: `target/release/sta.exe` and `target/release/sta-mcp.exe`. What a release ships
(and what CI zips) is `node tools/package-release.mjs stage` — the two binaries, the CEF runtime and
`locales/`, ~430 MB unpacked (`docs/RELEASING.md`).

---

## 4. Running the MCP server

Register `target/release/sta-mcp.exe` with an MCP client. It talks to a running sta over a named
pipe in the profile folder and, unless `--no-launch` is passed, starts sta if none is running.
`--data-dir <path>` picks a profile. Agent access is off until the user turns it on in
Settings › AI agents; every new client, every new site and every request for a tab is approved in
the sta window. See `docs/MCP.md`.

---

## 5. Testing

```powershell
cargo test --workspace                   # unit, scenario and bridge tests
cargo clippy --workspace --all-targets
node tools/check-mock-commands.mjs       # the UI mock ⇄ crates/sta-core/src/command.rs
node tools/check-mcp-docs.mjs            # docs/MCP.md + MCP.ko.md ⇄ the bridge's tools/list
node tools/check-no-console.mjs          # no test helper may flash a console window
node tools/check-motion.mjs              # animation registry ⇄ UI catalog ⇄ CSS gates
node tools/motion-check.mjs              # the motion runtime in mock mode (headless Edge)
node tools/check-icon.mjs                # res/sta.ico is complete and one render
node tools/check-release-clean.mjs       # the release binaries carry no test surface
```

> **A cargo command without `--features test-hooks` disarms the e2e suites.** The two `cargo` lines
> above rebuild `target/debug/sta.exe` and `sta-mcp.exe` **without** the debug-only test surface,
> after which every suite fails on its first call with `unknown tool: test_info`. Re-run the armed
> build below first; the suites now check the bridge before launching anything and say so.

The end-to-end suites drive a real browser **through MCP** — the same transport an AI agent uses —
so they need a build with the debug-only test surface (35 `test_*` tools, `docs/TESTING.md`). It
exists only with `--features test-hooks`, only in a debug build, only while armed by
`--sta-test-hooks` + `STA_E2E=1`, and only with an explicit non-default data directory.

```powershell
cargo build -p sta -p sta-mcp --features test-hooks
node tools/check-mcp-docs.mjs --armed    # docs/TESTING.md ⇄ the test surface's catalog
node crates/sta/e2e/mcp-smoke.mjs        # every test_* tool, the four locks
node crates/sta/e2e/shell-e2e.mjs        # layout, IPC, security, command bar, suggestions, window
node crates/sta/e2e/tabs-e2e.mjs         # popups, Alt+click preview, errors, downloads, DevTools
node crates/sta/e2e/chrome-e2e.mjs       # overlays, real keyboard input, hover reveal, motion, restart
node crates/sta/e2e/agent-e2e.mjs        # the MCP bridge and the agent policy, end to end
node crates/sta/e2e/migration-e2e.mjs    # the data folder from before the rename
node crates/sta/e2e/extensions-e2e.mjs   # extensions, foreign windows, the popup card, safe mode
```

Each suite takes its own throw-away data directory (`E2E_DATA_DIR`) and nothing else. Run the ones
that need the OS foreground (`chrome`, `agent`, `migration`, `extensions`, `mcp-smoke`) **one at a
time**, and do not type or open a terminal while they run — they send real OS keys and assert that
no console window was shown. `migration-e2e` is the one suite that cannot arm the in-browser console
watcher (its subject is a launch with no data-directory switch); it scans the desktop around every
launch instead, which is a sample rather than a hook.

`extensions-e2e --only=webstore` with `STA_E2E_WEBSTORE=1` installs a real extension from the live
Chrome Web Store; it needs the network and is not part of a normal run.

---

## 6. Where things are

```
crates/sta-core   the model, the Command → Effect reducer, the omnibox, history, themes, the
                  animation registry, URL rules and the AI-agent policy. No CEF; unit-tested.
crates/sta        the browser: window and tabs, overlays, keyboard, IPC, the sta:// scheme,
                  Windows integration, extensions, docked DevTools, agent automation.
crates/sta-mcp    the MCP server (stdio) that forwards to the browser's pipe.
ui/               every surface as plain HTML + Preact/htm, no build step, plus a mock backend.
tools/            the static gates (check-*.mjs), mock-mode screenshots, a CDP client.
docs/             this file, ARCHITECTURE.md, PROTOCOL.md, TESTING.md, MCP.md (+ MCP.ko.md).
```

Start with `docs/ARCHITECTURE.md` for the dispatch loop, the view tree and the trust model, and
`docs/PROTOCOL.md` for the UI ⇄ shell contract.
