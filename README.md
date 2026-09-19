# sta

> **Rust + CEF(Chromium Embedded Framework)로 만든 Arc 스타일 데스크톱 브라우저** (Windows 11 · macOS).
> 세로 사이드바, 스페이스, 즐겨찾기·고정 탭·Today 탭, 자동 아카이브, 커맨드 바, 분할 화면, Peek, Boosts 등
> Arc 브라우저의 핵심 경험을 Chromium 152 엔진 위에 구현했습니다. Claude Code 같은 AI 에이전트가 MCP로
> 브라우저를 안전하게 쓸 수 있습니다([한국어 안내](docs/MCP.ko.md)).
>
> **개발자 도구**: F12를 누르면 개발자도구가 **sta 창 안에** 열립니다(별도 Chrome 창이 아닙니다).
> 요소·스타일·콘솔·소스(중단점)·네트워크·성능·메모리, 요소 선택기(Ctrl+Shift+C), 디바이스 모드,
> 교차 출처 iframe과 워커까지 동작하고, 드래그로 크기를 바꾸거나 개발자도구 메뉴에서 창으로
> 분리(Undock)할 수 있습니다(분리하면 Chromium 자체 창으로 열립니다).
> 테마는 sta의 라이트/다크를 따라갑니다. sta 자신의 `sta://` 페이지에서는 열리지 않습니다.
>
> **링크 미리보기**: 웹 페이지의 링크를 **Alt+클릭**(또는 Alt+가운데 클릭)하면 그 페이지가 Peek 오버레이로
> 열립니다. 탭 종류(Today·고정 탭·분할 화면·확장 페이지·iframe)와 상관없이 동작하고, Peek 안에서 Alt+클릭하면
> 새 Peek이 겹치지 않고 보고 있던 페이지만 바뀝니다. Split(◫)·Expand(⤢)·Esc·Ctrl+O는 그대로 쓸 수 있습니다.
> `sta://`·확장 URL과 `mailto:` 같은 외부 프로토콜은 열리지 않고 토스트로 거절합니다.
> `file:` 링크는 `file:` 페이지에서만 미리 볼 수 있고, 웹 페이지에서는 토스트 없이 조용히 무시됩니다.
> `javascript:` 링크는 손대지 않습니다(웹 앱의 버튼이 Alt+클릭으로 먹통이 되지 않습니다). 같은 문서 안의
> `#` 링크는 미리 볼 것이 없지만 클릭 자체는 취소되므로, **Alt+클릭으로는 그 위치로 스크롤되지 않습니다**
> (그냥 클릭하면 그대로 스크롤됩니다).
> 페이지가 스크립트로 흉내 낸 클릭으로는 절대 열리지 않고(실제 입력인 `isTrusted` 클릭만 받습니다),
> 키보드로 링크를 활성화하는 Alt+Enter도 이 동작이 아닙니다(포인터 클릭만 받습니다).
> `allow-same-origin` 없이 샌드박스된 프레임(불투명 출처)에서는 동작하지 않습니다 — 임베더가 새 창조차
> 막아 둔 프레임이기 때문입니다. `allow-same-origin`이 있는 샌드박스 프레임에서는 그대로 동작합니다.
> 그 대신 **Alt+클릭으로 링크를 저장하는 Chrome 동작은 없어집니다**(다운로드 링크를 그냥 클릭하는 것은 그대로이고,
> 서버가 첨부 파일로 보내는 링크는 Alt+클릭해도 그냥 다운로드되며 빈 미리보기는 스스로 닫힙니다).
> 다만 스크립트가 꺼진 프레임에서는 이 리스너가 아예 실행되지 않으므로, 거기서는 Chrome의 Alt+클릭 저장이
> 그대로 남습니다.
>
> **확장 프로그램**: Chrome 웹 스토어에서 설치한 확장은 sta 탭에서 그대로 동작합니다(서비스 워커,
> 콘텐츠 스크립트, 광고 차단 규칙, 확장 페이지). 확장이나 설치 화면이 Chrome 창을 열려고 하면 sta가 그
> 창을 감추고 내용만 sta 탭으로 엽니다 — 이제 Chrome 창이 대신 뜨는 일은 없습니다. 확장 팝업 창과
> 로그인(OAuth) 창은 API가 필요해 Chromium 창 그대로 두되 sta 테마의 제목 표시줄을 씁니다.
>
> **Ctrl+E**를 누르면 설치된 확장 목록이 커맨드 바에 열립니다. Enter는 확장의 팝업을 페이지 오른쪽 위
> 카드로 열거나(팝업이 없으면) 옵션 페이지를 탭으로 열고, Alt+Enter는 옵션 페이지가 있는 확장에서 그 페이지를
> 엽니다(꺼져 있는 확장이면 설정의 해당 줄로, 옵션 페이지가 없으면 아무 일도 하지 않습니다).
> 한글 입력 상태든 아니든 찾을 수 있습니다(`ㄴㅅㅁ` → "sta …", `gksrmf` → "한글 …"). Ctrl+E는 페이지가 먼저 받기 때문에
> Ctrl+E를 쓰는 웹앱에서는 앱 메뉴나 `>` 명령으로 여세요. 켜기·끄기·삭제는 **설정 › 확장 프로그램**에
> 있고, 다른 프로그램이 추가한 확장은 Chrome의 권한 경고와 출처를 보여준 뒤 **허용해야** 켜집니다.
> 툴바 버튼, 확장 단축키, 사이드 패널은 아직 동작하지 않습니다
> (아래 [Known limitations](#known-limitations) 참고).
>
> **애니메이션**: **설정 › 애니메이션(Animations)** 에서 스위치 하나로 전부 끌 수 있고, "Follow Windows
> animation effects"를 켜 두면 Windows의 애니메이션 효과 설정을 따라 움직임이 줄어듭니다(줄어든 동안에는
> 그 이유를 설정에 적어 줍니다). 그 아래에는 36가지 애니메이션이 8개 그룹(사이드바·상단 바, 커맨드 바,
> 오버레이, 메뉴, 페이지, 테마, 컨트롤, 표시기)으로 나뉘어 있어 그룹째로도, 하나씩도 끌 수 있습니다
> (그룹을 끄면 그 안의 스위치는 흐려지지만 잠기지는 않으므로 미리 정해 둘 수 있습니다). 꺼도 *고장*이
> 아니라 *즉시*입니다 — 로딩 표시는 멈춘 고리나 막대로 그대로 보이고, 사라지는 화면은 사라지기 전에 자기를
> 비울 시간을 그대로 받으므로 다음에 다시 나타날 때 지난 프레임이 번쩍이는 일이 없습니다. 커맨드 바의
> **Turn Animations Off/On**으로도 한 번에 바꿀 수 있습니다.

An Arc-style browser for Windows and macOS built on **CEF 152 (Chromium 152)** via the
[`cef`](https://crates.io/crates/cef) crate, written in Rust. The browser chrome is HTML/CSS/JS
served from a custom `sta://` scheme; all state lives in a deterministic, unit-tested Rust
core.

On macOS the shortcuts below are **⌘** instead of Ctrl (⌘T, ⌘W, ⌘L, …), exactly as they are written
in every other Mac browser; what else differs is in [macOS](#macos).

## Features

- **Frameless Arc-style window** with a themed vertical sidebar and custom top bar — Windows 11
  caption buttons, native snap/resize, rounded corners and dark title bar integration there; the
  traffic lights, native full screen and the system appearance on macOS.
- **Rounded chrome**: web pages sit in a rounded content card (10 px, the focused split pane's
  accent ring and the AI agent frame follow the corners); the command bar, find bar, permission
  prompt, AI agent prompts and panel, switcher, toast, Peek (page corners included) and the floating
  sidebar are native rounded cards with a soft shadow; the HTML UI uses one radius scale
  (4/6/8/12/16 px and pills).
- **Sidebar**:
  - favorites grid (shared across spaces);
  - per-space pinned tabs with nested folders;
  - a Today list;
  - live favicons, titles, loading and audio state;
  - rename, context menus, keyboard navigation;
  - full drag & drop (reorder, pin, favorite, into folders, onto space icons, onto a tab to split,
    links dragged from pages);
  - resizable, and hideable with Ctrl+S (the top bar then shows navigation and the URL);
  - while hidden, resting the pointer at the window's left edge floats it over the page; it hides
    again when the pointer leaves (not while one of its menus is open), Esc hides it, and Ctrl+S or
    ◧ keeps it open. Ctrl+J / Alt+F open their panel in the floating sidebar while the page keeps
    focus; renaming or editing a pinned page docks it until you're done.
- **Spaces** with emoji icons and color themes (OKLCH presets and sliders), applied to every surface and
  the native frame in light, dark or system appearance. Switch with icons, Alt+1..9, Ctrl+Alt+←/→,
  or — **over the sidebar** — the mouse back/forward buttons and a horizontal wheel swipe (those two
  are handled by the sidebar surface itself, so they do nothing over the page or the top bar).
- **Command bar** (Ctrl+T / Ctrl+L):
  - URL-vs-search classification, 7 search engines plus a custom one;
  - open-tab / history / archive / space results with fuzzy ranking and frecency;
  - search suggestions from the selected engine (Google by default; Bing, DuckDuckGo, Brave and
    Ecosia too) as rows and inline completion, fetched without cookies and only while the
    setting is on;
  - inline completion, an actions mode (Tab toggles it on and off, or type `>`), background tabs
    (Alt+Enter), split mode.
- **Tab lifecycle like Arc**:
  - Today tabs auto-archive after 12 h / 24 h / 7 d / 30 d idle; Archive page with restore
    (including split groups);
  - Clear Today with undo, reopen closed (Ctrl+Shift+T);
  - pinned tabs reset to their pinned URL;
  - session restore with lazy loading.
- **Split view** with up to 4 panes (side by side or stacked), a focused-pane ring,
  Ctrl+Shift+1..4.
- **Peek**: cross-site links from pinned tabs and script popups (OAuth) open in an overlay with
  Split / Expand — and **Alt+click** (or Alt+middle-click) any link, in any tab, to preview it there.
  The preview replaces an open Peek instead of nesting one, so Alt+clicking inside Peek just moves it
  on; `sta://`, extension URLs and external protocols are refused with a toast, and a `file:` link
  is previewable only from a `file:` page (from a web page it is ignored, silently). A `javascript:`
  link is left completely alone — but an in-page `#` link, while equally unpreviewable, *is*
  cancelled, so Alt+clicking one does not scroll the page to it (a plain click still does). A page
  cannot trigger the preview with a scripted click, nor an automation tool with a key. Alt+click no
  longer saves a link to disk — with two exceptions: a link the server sends as an attachment still
  downloads (and the preview that cannot show it closes itself), and a frame with scripting turned
  off never runs the listener at all, so Chromium's own Alt+click download survives there. The
  gesture is off only in frames with an **opaque origin** (a `sandbox` without `allow-same-origin`);
  a frame sandboxed *with* `allow-same-origin` keeps it.
- **Browsing tools**:
  - find bar with a match counter;
  - print (Ctrl+P) and view page source (Ctrl+U);
  - zoom (chip in the URL pill);
  - downloads (progress ring, popover, pause/resume/retry), tagged with Mark-of-the-Web so
    SmartScreen and Office Protected View check them when opened;
  - site permission prompts: Allow / Block this time (no automatic blocking after repeated
    Blocks; an Allow lasts until no tab shows the site), or Remember;
  - Ctrl+Tab recent-tab switcher;
  - toasts;
  - external protocol hand-off (`mailto:`, `tel:`, app links; typed or clicked);
  - themed error pages;
  - DevTools **docked inside the window** (F12; Ctrl+Shift+I opens, focuses, then closes;
    Ctrl+Shift+C starts the element picker while a dock is open; Ctrl+= / Ctrl+- / Ctrl+0 inside
    DevTools zoom DevTools; Undock from DevTools' own menu or the `>` command, which opens
    Chromium's own DevTools window; not on sta's own pages).
- **Chrome extensions, with a picker of their own** (Ctrl+E):
  - **Ctrl+E** lists what is installed — icon, name and, when there is something to say, what it
    can't do here. Enter opens the extension's popup in a card at the top right of the page, or its
    options page in a tab; Alt+Enter opens the options page when the extension has one (an extension
    that is off opens its row in Settings instead, and one with no options page has no Alt+Enter at
    all). The key is page-first, so a page
    that uses Ctrl+E itself keeps it: the app menu and `>` open the picker everywhere. The Korean
    keyboard is undone in both directions, so the extension is found whether or not the IME was on
    (`ㄴㅅㅁ` finds "sta …", `gksrmf` finds "한글 …").
  - **The popup card** is sta's own header (icon, name, Options, ×) above the extension's page,
    sized the way Chrome sizes a popup. It appears once the page has something on screen — a popup
    that paints a second or two in (a cold service worker) is waited for, not called broken. A popup
    that needs the current tab cannot work here (see the limitations) — after three seconds the card
    says so instead of showing an empty rectangle.
  - **Settings › Extensions** turns extensions on and off and removes them. An extension another
    program added stays **off until you allow it**: Turn on shows Chrome's own permission warnings,
    its site access and where the code came from, and the confirm button is not the default one (and
    each Turn on needs its own disclosure — one you cancelled does not count for the next). One
    another program installed from a file on this computer can only be removed, and the row says so.
    A row that Chrome turned off says *why* — an organization's policy, an extension the current
    Chrome no longer supports, a damaged install or Safe Browsing — rather than blaming an
    organization for all of them.
  - If sta closes unexpectedly twice while starting, it comes back in **safe mode** with every tab
    unloaded and a banner in Settings › Extensions, so an extension that crashes the browser can be
    turned off.
- **Animations you choose** (Settings › Animations): one switch for all of them, "Follow Windows
  animation effects" ("Follow Reduce motion" on macOS, with a line that tells you when the system
  setting has sta using reduced motion), and then 36 animations in 8 groups — sidebar and top bar,
  command bar, overlays, menus, pages, theme, controls, indicators — each with a group switch, an
  "n of m on" count and a one-line description, plus Reset to defaults. Turning a group off greys its animations without
  locking them, so you can set one up before you switch its group back on. Off means *instant*, not
  *broken*: loading still shows, as a still ring or bar, and a surface still gets the moment it needs
  to clear itself before it disappears, so nothing ever flashes the previous frame. The command bar
  also has **Turn Animations Off/On**.
- **Boosts**: per-site CSS/JS injected at document start, toggled from the URL pill, edited on
  `sta://boosts/`.
- **Internal pages**: Settings, History, Archive, Boosts.
- **Single instance**: launching again with a URL opens it in the running browser.
- **AI agents (MCP)**: `sta-mcp.exe` lets Claude Code, Claude Desktop, VS Code, Cursor and
  other MCP clients use the browser with 23 tools — open tabs, read pages as an accessibility
  outline with element refs, text or screenshots, find text, click, hover, type, fill forms, choose
  options, scroll, answer dialogs, and (when allowed) run scripts, read the console, search history
  and list downloads — through a local per-user pipe (no debugging port), with access levels,
  approval prompts for new clients, sites and tab requests, a topbar chip with Stop and the last
  actions, an orange frame around tabs an agent is working in, and guards (see
  [AI agents](#ai-agents-mcp), [`docs/MCP.md`](docs/MCP.md), [한국어](docs/MCP.ko.md)).

### Keyboard shortcuts (Arc for Windows mappings)

**On macOS, read every `Ctrl` below as `⌘`** (and `Alt` as `⌥`): it is one table, mapped to the
platform's own modifier. One row differs there — **history is ⌘Y**, because ⌘H hides the app — and
the F-keys need `fn` unless the keyboard is set to send function keys.

| Keys | Action |
|---|---|
| Ctrl+T | Command bar (new tab) |
| Tab (in the command bar) | Toggle actions mode |
| Ctrl+L, Alt+D, F6 | Edit current URL |
| Ctrl+W, Ctrl+F4 | Close tab (Today → archive, pinned → unload) |
| Ctrl+Shift+W | Close the window (quit) |
| Ctrl+Shift+T | Reopen closed |
| Ctrl+S | Toggle sidebar (docks the floating sidebar) |
| Ctrl+D | Pin / unpin |
| Ctrl+Shift+C / +Alt | Copy URL / as Markdown (with DevTools docked: element picker, as in Chrome) |
| Ctrl+1..9 / Alt+1..9 | Go to item N / space N |
| Ctrl+Alt+↑/↓, ←/→ | Previous/next item, previous/next space |
| Ctrl+PgUp / Ctrl+PgDn | Previous/next item |
| Ctrl+Tab, Ctrl+Shift+Tab | Recent-tab switcher (forwards, backwards) |
| Ctrl+Shift+K | Clear Today |
| Ctrl+Shift+= / Ctrl+Shift+- | Add split / separate pane |
| Ctrl+Shift+1..4, [ ] | Focus split pane |
| Ctrl+O | Expand Peek into a tab |
| Ctrl+F, F3, Shift+F3 | Find in page, next match, previous match |
| Ctrl+P | Print |
| Ctrl+U | View page source |
| Ctrl+= / Ctrl+- / Ctrl+0 | Zoom |
| Ctrl+E | Extensions picker (page first: a page that uses Ctrl+E keeps it) |
| Ctrl+J, Alt+F, Ctrl+, , Ctrl+H | Downloads, app menu, settings, history |
| F5 / Ctrl+R, Alt+←/→ | Reload, back/forward |
| Ctrl+Shift+R, Shift+F5, Ctrl+F5 | Reload ignoring the cache |
| F11, Alt+Shift+F | Fullscreen |
| F12 | DevTools (docked in the window) |
| Ctrl+Shift+I | DevTools: open, then focus, then close |

F2 renames the item under the cursor, but it is **not** an accelerator: it is handled by the docked
sidebar's own keyboard focus, so it does nothing while the sidebar is hidden (Ctrl+S) or floating —
the sidebar's context menu hides the hint there, and the command bar's "Rename Tab" row shows it
only when the sidebar is docked.

### Mouse on a link

| Click | Action |
|---|---|
| **Alt+click, Alt+middle-click** | **Preview the link in Peek** — in any tab, and inside Peek it moves that preview on instead of nesting a second one. This replaces Chrome's Alt+click "save link as", which sta no longer does for a link it can preview (a `Content-Disposition` attachment still downloads, and no empty preview is left behind). The link's own context menu names the gesture. |
| Shift+click | Peek |
| Ctrl+click, middle-click | Background tab |
| Right-click (link) | Open Link in New Tab / Open Link in Peek (Alt+Click) / Copy Link Address, then Inspect |
| Right-click (image) | Open Image in New Tab / Copy Image Address, then Inspect |
| Right-click (page) | Inspect (sta's own `sta://` pages have no Inspect) |

## AI agents (MCP)

> **한국어 요약.** sta에는 AI 에이전트용 MCP 서버 `sta-mcp.exe`가 함께 들어 있습니다.
> Claude Code 등에 등록하면 에이전트가 탭을 열고, 페이지를 접근성 트리(요소 ref 포함)나 텍스트로
> 읽고, 클릭·입력·스크린샷을 할 수 있습니다. 통신은 현재 사용자만 열 수 있는 로컬 named pipe로만
> 이뤄지고(원격 디버깅 포트 없음), 기본값은 꺼짐입니다. 설정 → AI agents (MCP)에서 접근 수준(끄기/읽기
> 전용/전체)을 켜고 클라이언트별 설정을 복사한 뒤 "Test connection"으로 확인합니다. 처음 연결하는
> 클라이언트와 새 사이트는 창 오른쪽 위의 승인 창에서 사용자가 허용합니다. 연결 중에는 상단 바 칩에
> 이름과 Stop 버튼·최근 동작이 보이고, 에이전트가 조작하는 탭에는 주황색 테두리가 생깁니다. 에이전트는 기본적으로 에이전트가 연
> 탭(다른 에이전트나 이전 세션이 연 탭 포함)과 사용자가 공유한 탭만 보며(`request_tab_access`로 공유를 요청할 수 있음), 외부 프로토콜 실행·파일
> 선택·다운로드·전체 화면·권한 요청은 차단되거나 사용자 확인을 기다립니다. 스크립트 실행·방문 기록·
> 다운로드 목록은 설정에서 따로 켜야 합니다. 웹 페이지에서 온 내용은 신뢰할 수 없는 데이터로 표시됩니다.
> 설치·승인·도구 요약·보안·문제 해결은 [`docs/MCP.ko.md`](docs/MCP.ko.md), 전체 참조는
> [`docs/MCP.md`](docs/MCP.md)를 보세요.

1. Turn on agent access in sta: Settings → AI agents (MCP) → Agent access → *Full* (or
   *Read only*).
2. Register the server with your client. The same Settings section has copyable setup for Claude
   Code, Claude Desktop, VS Code and Cursor with this installation's path, and *Test connection*
   (below, `C:\Program Files\sta\` is only an example: use the folder of your `sta.exe`,
   e.g. `target\debug\` for a source build):

   ```powershell
   claude mcp add sta -s user -- "C:\Program Files\sta\sta-mcp.exe"
   claude mcp list
   ```

   Claude Desktop: `%APPDATA%\Claude\claude_desktop_config.json` →
   `{"mcpServers": {"sta": {"command": "C:\\Program Files\\sta\\sta-mcp.exe"}}}`.
3. Approve the client when sta asks (Deny / Allow for this session / Always allow for signed
   programs), and each new site. The topbar chip shows who is connected, the last actions and Stop.

Tools: `tabs_list`, `tab_open`, `tab_navigate`, `tab_show`, `tab_close`, `request_tab_access`,
`page_snapshot`, `page_text`, `page_find`, `page_screenshot`, `click`, `hover`, `type`, `press_key`,
`select_option`, `scroll`, `fill_form`, `wait_for`, `handle_dialog`, `evaluate` (behind *Page
scripts*), `console_messages`, `history_search` (behind *Browsing history*), `downloads_list` (behind
*Downloads list*). The tool reference, settings, threat model, prompt-injection guidance, limits
and troubleshooting are in [`docs/MCP.md`](docs/MCP.md); a Korean guide is in
[`docs/MCP.ko.md`](docs/MCP.ko.md).

## Building

Prerequisites (Windows 11 x64):
- Rust stable (MSVC toolchain);
- Visual Studio 2022 with the C++ workload;
- CMake and Ninja on `PATH` (e.g. `pip install --user cmake ninja`);
- Node.js 22+ (only for the test tools).

```powershell
cargo build -p sta            # first build downloads CEF (~600 MB) into .cef/
.\target\debug\sta.exe
cargo build -p sta --release  # UI assets are embedded in release builds
```

Prerequisites (macOS 11+, Apple silicon or Intel):
- Rust stable;
- Xcode command line tools (`xcode-select --install`);
- CMake and Ninja on `PATH` (`brew install cmake ninja`) — the CEF C++ wrapper is built from source;
- Node.js 22+ (only for the test tools).

```bash
cargo build -p sta            # first build downloads CEF (~600 MB) into .cef/
./target/debug/sta            # assembles target/debug/sta.app and runs it
cargo build -p sta --release  # UI assets are embedded in release builds
```

- The first build downloads CEF into `.cef/`. Keep the checkout on a short path, because the CEF
  wrapper's CMake build fails under very long directories.
- Windows: `libcef.dll`, the `.pak` files and `locales/` are copied next to the executable
  automatically.
- macOS: a CEF app has to run from an app bundle, so the debug binary builds one around itself
  (`target/debug/sta.app`, with the framework symlinked and the helper processes hard-linked) and
  re-executes into it — `cargo run` just works, and the terminal keeps the process and its output.
  `./target/release/sta --sta-bundle-mac[=<dir>]` writes a standalone bundle instead and exits;
  that is what a release archive holds.
- User data lives in `%LOCALAPPDATA%\sta` (release) or `%LOCALAPPDATA%\sta Dev` (debug) on Windows,
  and in `~/Library/Application Support/sta` (or `sta Dev`) on macOS.
- Override the data location with `--sta-data-dir=<path>`.

### macOS

The core, the UI and the browser itself are the same build; what the platform changes:

- **Shortcuts are ⌘-based.** The [shortcut table](#keyboard-shortcuts-arc-for-windows-mappings) is
  written with Ctrl and read as ⌘ (⌘T, ⌘W, ⌘⇧C, ⌘1…9); the UI writes them that way too. **History is
  ⌘Y**, because ⌘H hides the app. Ctrl itself is left to macOS and to the page.
- **The menu bar** holds the application and editing commands macOS expects (About, Hide, Quit,
  Undo/Cut/Copy/Paste/Select All). ⌘Q closes sta the way its own close button does — the session is
  saved first. sta's own shortcuts are deliberately *not* menu items: a menu key equivalent would
  take the key before the page or the shell ever saw it.
- **The window** has the traffic lights in a strip above the top bar rather than caption buttons
  inside it, and follows the system appearance (dark/light) and "Reduce motion".
- **AI agents** reach the browser through a Unix domain socket in the data directory
  (mode 0600, `agent.sock`) instead of a named pipe; the MCP bridge checks who serves it the same
  way ([`docs/MCP.md`](docs/MCP.md)).
- **Not there yet**: the Chrome-created-window plumbing (extension popups and sign-in windows stay
  Chromium's own windows), the sidebar's pointer-reveal keep-zone across owned popups, and the
  end-to-end suites, which drive real Win32 input. See [Known limitations](#known-limitations).

## Renamed from Astatine <!-- rename:keep -->

> 이 프로젝트의 이전 이름은 Astatine입니다. sta는 처음 시작할 때 기존 Astatine 데이터를 자동으로 옮깁니다. <!-- rename:keep -->

sta was called Astatine before. Everything was renamed: `sta.exe`, the `sta://` scheme, the <!-- rename:keep -->
crates, `STA_*` environment variables and `--sta-data-dir`. On startup, before anything else
touches the data:

- **Data folder.** Without a data dir override, sta moves `%LOCALAPPDATA%\Astatine` (release) or <!-- rename:keep -->
  `%LOCALAPPDATA%\Astatine Dev` (debug) to `%LOCALAPPDATA%\sta` / `%LOCALAPPDATA%\sta Dev` when <!-- rename:keep -->
  the new folder doesn't exist yet. The move is a rename in place, so nothing is copied. Inside the
  data folder, the profile subfolder `astatine\` becomes `sta\`. This also happens in a folder <!-- rename:keep -->
  given with `--sta-data-dir`. A `--sta-data-dir` that names the default folder (as a launcher may
  pass it) counts as no override, so the old folder is moved then too.
- **Existing sta folder.** If the new folder already exists, the old one is left untouched.
  sta never merges the two folders.
- **Old version still running.** If an old Astatine is still using its folder, sta doesn't start. <!-- rename:keep -->
  It shows a message asking you to close Astatine first. This way the data isn't split, and the <!-- rename:keep -->
  running old browser doesn't receive sta's URLs.
- **Move fails.** If the move fails for any other reason, sta uses the old folder where it is for
  that run, logs the failure, and tries again on the next start. No data is lost. If you start sta
  again during that run, the new launch hands its links to the running sta as usual, and
  `sta-mcp` (AI agents) finds that run's agent channel in the old folder too.
- **Saved links.** Internal pages saved as `astatine://…` become `sta://…` when sta loads its <!-- rename:keep -->
  state. This covers pinned pages, favorites, Today and archived tabs, the reopen stack and
  history. Typing `astatine://…` in the command bar, or passing it on the command line, opens the <!-- rename:keep -->
  `sta://` page.

## Project layout

```
crates/sta-core        pure-Rust core: data model, Command → Effect reducer, omnibox, history,
                       theme colors, persistence, AI agent policy and tool catalog (no CEF;
                       200+ tests)
crates/sta             CEF shell: bootstrap, Views window, tabs & handlers, overlays, keyboard,
                       IPC (message router), sta:// scheme, Windows integration, agent
                       automation (automation/), Chrome extensions (extensions.rs, ext_popup.rs,
                       ext_backend.rs, foreign.rs, safe_mode.rs), docked DevTools (devtools*.rs),
                       the animation timing the shell owns (motion.rs) and the debug-only test
                       surface (test_hooks/); e2e suites
crates/sta-mcp         MCP server (stdio) for AI agents: forwards tool calls to the browser's
                       agent pipe
ui/                    HTML UI (Preact + htm, no build step): sidebar, topbar, command bar,
                       overlays, internal pages, shared components, mock backend + fixtures
tools/                 CDP client, window capture, UI screenshot/mock tooling, the check-*.mjs
                       gates (mock commands, MCP docs, motion, console windows, release
                       cleanliness, the app icon)
docs/                  STATUS.md (what works today), ARCHITECTURE.md, PROTOCOL.md, TESTING.md
                       (the e2e reference), MCP.md + MCP.ko.md (AI agents), research/
                       (verified CEF & product notes)
```

[`docs/STATUS.md`](docs/STATUS.md) is the short, current answer to "what does this do, what does it
not do, and how do I build and check it". Start with [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md)
for the code. It covers the dispatch loop that keeps
CEF re-entrancy safe, the view tree, tab lifecycle, IPC trust model and testing. The UI ⇄ shell
contract is [`docs/PROTOCOL.md`](docs/PROTOCOL.md).

## Testing

```powershell
cargo test --workspace                   # unit, scenario and bridge tests
cargo clippy --workspace --all-targets
node tools/check-mock-commands.mjs       # UI mock ⇄ Rust command contract
node tools/check-mcp-docs.mjs            # docs/MCP.md + MCP.ko.md ⇄ the MCP server's tools/list
node tools/check-no-console.mjs          # no test helper may flash a console window
node tools/check-motion.mjs              # animation registry ⇄ UI catalog ⇄ CSS gates, + the motion rules
node tools/motion-check.mjs              # the motion runtime in mock mode (headless Edge)
node tools/check-icon.mjs                # res/sta.ico has every size, from one run of make_icon.py
node tools/package-release.mjs stage     # the release archive, exactly as CI builds it (docs/RELEASING.md)
powershell -File tools/ui-shot.ps1 -Path '/sidebar/?mock' -Width 248 -Height 900 -Out shot.png
```

> **A cargo command without `--features test-hooks` disarms the e2e suites.** The two `cargo` lines
> above rebuild `target/debug/sta.exe` and `sta-mcp.exe` **without** the test surface, and every
> suite then fails on its first call with `unknown tool: test_info` — which reads like a browser
> arming failure but is not. Re-run the armed build below before the suites. (The suites now check
> the bridge before they launch anything and say this instead, but the rebuild still costs a run.)

**The end-to-end suites drive a running browser through MCP**, so they need a build with the
**debug-only test surface** ([`docs/TESTING.md`](docs/TESTING.md)): 35 `test_*` tools that exist only
in a build made with `--features test-hooks` and only while it is armed by `--sta-test-hooks` +
`STA_E2E=1` + its own data directory. A release build cannot contain them — it fails to compile if the
feature is on (in `sta-core/build.rs` as well, which sees the *profile* and not just
`debug_assertions`), and `node tools/check-release-clean.mjs` proves the shipped bytes are clean.

```powershell
cargo build -p sta -p sta-mcp --features test-hooks
node crates/sta/e2e/mcp-smoke.mjs        # every test_* tool, the four locks, no console window
node tools/check-mcp-docs.mjs --armed    # docs/TESTING.md ⇄ the test surface's catalog
node crates/sta/e2e/shell-e2e.mjs        # layout, IPC, security, command bar, suggestions, window
node crates/sta/e2e/tabs-e2e.mjs         # popups, Alt+click preview, errors, downloads, permissions, …
node crates/sta/e2e/chrome-e2e.mjs       # overlays, real keyboard input, hover reveal, restart
node crates/sta/e2e/migration-e2e.mjs    # data from before the rename (see above)
node crates/sta/e2e/agent-e2e.mjs        # the MCP bridge and the agent policy, end to end
node crates/sta/e2e/extensions-e2e.mjs   # windows extensions open (in-repo probe extensions)
```

Each suite takes its own throw-away data directory (`E2E_DATA_DIR`) and nothing else: the pipe name
is random per browser, so **the data directory alone makes a run unique** — no DevTools port to
assign. `CDP_PORT` is still read by `shell-e2e` (one check that the debug build opens the port at
all), by `agent-e2e` and by `migration-e2e`, the two suites whose drivers cannot be MCP sessions;
`C:/ast/tmp/s6/cdp-residue.md` lists every remaining non-MCP check with its reason. Every suite also
asserts at runtime that **no console window was shown** while it ran — a flash included, and whoever
opened it, since a console window hosted by Windows Terminal belongs to no process tree of ours. The
one exception is `migration-e2e`, whose subject is a launch that passes no `--sta-data-dir` and so
cannot arm the in-browser watcher (lock 3): it *scans* the desktop for console windows around every
launch and at the end instead, which catches one that is up at a sample point but not one that
flashes entirely between two samples. Run
the ones that need the OS foreground (`chrome`, `agent`, `migration`, `extensions`, `mcp-smoke`) one
at a time, don't type while they do, and don't open a terminal either.

To develop the UI without the browser, serve `ui/` with `node tools/ui-serve.mjs` and open any
surface with `?mock` (see `ui/README.md`).

## Known limitations

- **Windows and macOS.** The core and UI are platform-neutral; the shell has a Win32 half and a
  Cocoa half (`crates/sta/src/platform/`). On macOS the windows Chromium opens for itself —
  extension popups, sign-in flows — are not adopted into sta's chrome the way they are on Windows,
  and the debug-only end-to-end suites are Windows-only because they drive real Win32 input. Linux
  builds are not set up.
- **Chrome extensions run, and Ctrl+E uses them, but sta has no extension toolbar.** Extensions
  installed from the Chrome Web Store work in sta's tabs: service workers, content scripts, blocking
  rules (ad blockers really block), extension pages and options pages. **Ctrl+E** opens a picker that
  shows the extension's popup in a card or its options page in a tab, and Settings › Extensions turns
  them on and off and removes them. What CEF's Alloy runtime — which sta's multi-view layout needs —
  does *not* give them is a Chrome window, so:
  - **no toolbar buttons**: sta has no extension toolbar to click, and no API can press one for you,
    so an extension whose only entry point is its toolbar button says so in the picker and offers its
    Web Store page. **No extension keyboard shortcuts**, no side panels, and no extension
    context-menu items;
  - extensions can't see sta's tabs and windows (`chrome.tabs.query`, `chrome.windows.*`), so
    anything built on "the current tab" — including popups that ask for it — doesn't work. A popup
    like that opens in sta's card and either shows its own error or nothing at all; after three
    seconds the card says "This popup doesn't work in sta yet" and offers the options page instead;
  - **removing an extension uses Chrome's own "Remove …?" dialog**, because Chromium only skips that
    confirmation for an extension removing itself. sta asks nothing of its own, so there is one
    dialog, not two;
  - **windows an extension opens become sta tabs.** When an extension or the Web Store's
    post-install page needs a window, Chromium makes one; sta hides it, opens what it wanted to
    show as a tab and closes it again, so no Chrome window ever appears. Extension popup windows
    (`windows.create({type:'popup'})`) and sign-in flows (`identity.launchWebAuthFlow`) stay
    ordinary Chromium windows on purpose, with sta's caption colors, icon and name in the title
    ("… - sta"): their pages need a window of their own. That also means such a window looks like
    sta's while an extension controls what it shows — treat a window with sta's icon as sta's chrome
    around someone else's page, the way a tab is. Private (incognito) windows are refused, and an
    extension that keeps opening windows is stopped after three tabs in ten seconds ("An extension
    keeps opening windows; sta blocked them");
  - an extension can ask sta to open **another** extension's declared page (its options page, say):
    sta sees the page, not who asked, so the question it shows names the page's owner rather than the
    extension that asked;
  - an extension that calls `chrome.tabs.discard` on an sta tab crashes the browser (a CEF bug,
    reported upstream);
  - password managers that pair with a desktop app (1Password) refuse sta, and there is **no
    autofill UI**;
  - extensions **other programs registered** (an app that ships a Chrome extension writes it into the
    Windows registry, and Chromium loads it into every profile) arrive turned off and stay off until
    you allow them in Settings › Extensions — with Chrome's own permission warnings and the source in
    front of you. One installed from a file on this computer can only be removed: sta cannot show you
    where that code came from. sta does not stop Chromium from loading them in the first place;
  - the extensions list comes from the profile on disk, which Chromium writes with a delay, so an
    extension can take a second or two to appear after it is installed.
- **DevTools gaps.** DevTools are docked in the window and work — Elements, Styles, Console,
  Sources with breakpoints, Network, Performance, Memory, Application, the element picker, device
  mode, local overrides, cross-origin frames and workers — but sta is not Chrome, so the few things
  its embedder would provide are missing, and these are all of them:
  - the color picker's **eyedropper** and the Security panel's **certificate viewer** do nothing;
  - **Developer resources** (and any other fetch DevTools makes for itself, rather than through the
    page) fails with status 409 while docked; source maps of the page load normally;
  - the **Application panel's "inspect" button for a service worker** does nothing: a worker's own
    DevTools window is not available (nothing opens, nothing breaks);
  - **Workspaces / "Add folder"** and **"Save as"** use Chromium's own file dialogs;
  - DevTools may only name **the page's own origins**: the Application panel clears and reads
    storage and cookies for the site you are inspecting, not for another one;
  - "Inspect" on a page inside another frame selects the frame's own node.

  Everything else a panel cannot do answers with a protocol error rather than silently misbehaving,
  the method's name shows up in `debug.info` so it is a one-line fix, and the browser stays up.
- **No DevTools on sta's own pages.** F12 works on web pages; on `sta://` pages (settings, history,
  archive, boosts and the browser's own surfaces) it says so instead, because DevTools extensions
  would otherwise reach sta's internal commands. The context menu of those pages doesn't offer
  Inspect at all.
- **No blur or dimming behind overlays.** CEF can't draw translucent browser views. Rounded
  corners and soft shadows are native pieces around the pages (see `docs/ARCHITECTURE.md` §4.4),
  which also swallow clicks in their small areas (about 10×10 DIP at each page corner and the
  8 DIP shadow ring of the command bar and other cards).
- **Web notifications are Chromium pop-ups.** Sites can ask for and show notifications, but they
  appear as Chromium's own pop-ups at the bottom-right of the screen, not in the Windows
  notification center. A permission request from a background tab waits until the tab is shown
  (as in Chrome).
- **Not implemented yet:**
  - split divider dragging;
  - favicon cache for `http:` sites;
  - multiple windows, profiles, incognito.
