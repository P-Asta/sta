# sta: Product and UX Spec (Arc-like browser on CEF Views, Windows 11)

Status: research-backed spec, v1 (2026-09-16). Target: CEF 152 / `cef` crate 152.3.0, Windows 11 x64.

**Labels used below**
- **[Arc]**: documented Arc behavior (source in §10). **[Arc-Win]** means Arc for Windows specifically.
- **[A]**: a sta decision where Arc's docs say nothing, or where we deliberately differ.
- `Lnnnn`: line number in `cef-152.3.0+152.0.6/src/bindings/x86_64_pc_windows_msvc.rs`.

---

## 0. TL;DR decisions

1. **Shortcuts follow Arc for Windows, not macOS.** Official Arc-Win mappings: **Ctrl+1..9 = go to tab N**, **Alt+1..9 = go to Space N**, **Ctrl+Alt+←/→ = previous/next Space**, **Ctrl+Alt+↑/↓ = previous/next tab**, **Ctrl+T = Command Bar (new tab)**, **Ctrl+L or Alt+D = edit current URL**, **Ctrl+S = toggle sidebar**, **Ctrl+D = pin/unpin**, **Ctrl+Shift+C = copy URL**, **Ctrl+Shift+Alt+C = copy URL as Markdown**, **Ctrl+Shift+K = clear unpinned (Today) tabs**, **Ctrl+Tab = recent-tabs switcher**, **Ctrl+Shift+= / Ctrl+Shift+- = add/close split**, **Ctrl+Shift+1..4 = focus split pane**, **Ctrl+H = history**, **Ctrl+J = downloads**, **Ctrl+, = settings**, **Ctrl+O = open Peek as tab** [Arc-Win]. The brief suggested Ctrl+1..9 for Spaces. **Recommendation: use Alt+1..9 for Spaces**, as Arc for Windows does, and keep Ctrl+1..9 for tabs (the standard Chromium mapping).
   - VERIFIED-FIX: provenance. The official Keyboard Shortcuts table (article 20595231349911) covers Ctrl+T/N/Shift+N/W/Shift+T/D/Shift+C/Shift+Alt+C, Ctrl+L or Alt+D, Ctrl+S, Ctrl+Shift+K, Ctrl+1..N, Alt+1..N, Ctrl+Tab, Ctrl+Alt+arrows, Alt+←/→, Ctrl+Shift+Plus/Minus, Ctrl+Shift+1.., Ctrl+H, Ctrl +/−/0, Ctrl+R, Ctrl+F. **Ctrl+J, Ctrl+, , Ctrl+O, Alt+F, Ctrl+F4, Ctrl+Shift+I, Ctrl+P, Ctrl+U and F11 come only from the Arc-Win release notes.** The spec also missed three documented Arc-Win shortcuts: **Alt+Shift+F = full screen** ("You can enter full screen with Alt-Shift-F"), **Ctrl+Z = undo sidebar actions** ("Any time you Archive Tabs or move Tabs in your Sidebar, simply press Ctrl+Z to undo that action"), and **Ctrl+Shift+M** (DevTools device toolbar, built into DevTools). They are now in §3.
2. **Three sidebar sections with different lifecycles.** **Favorites**: shared across Spaces in a profile, max 12. **Pinned**: per Space, never auto-archive, and always reopen at the pinned URL. **Today**: per Space; archived after 12h/24h/7d/30d idle (default 12h). [Arc]
3. **Ctrl+W depends on the section.** On a Today tab it closes the tab and moves it to Archive. On a Pinned tab or Favorite it only unloads: the row stays and the tab reopens at its pinned URL. [A], consistent with [Arc] "Pinned Tabs and Favorites always revert back to the original link".
4. **Peek.** A link from a Pinned tab or Favorite to *another site* opens in Peek, a large overlay. Ctrl+O or the expand button makes it a tab. Esc, Ctrl+W, or clicking outside closes it. [Arc]
5. **Split View.** 2 to 4 panes, arranged horizontally or vertically, with resizable dividers. In the sidebar, a split group shows as one row. [Arc]
6. **CEF constraints shape the UI** (§8). Views-hosted (windowed) browsers are **opaque**: `background_color` alpha works only for off-screen browsers (cef_types.h), and transparent overlay BrowserViews are an open request (cef#4035). As a result, **overlays (Command Bar, Peek, floating sidebar) can't cast shadows or dim the page**, and native gaps can only be solid colors. Overlay BrowserViews work in **Alloy** style (cef#3790, fixed Oct 2024). Design for that from the start.
   - VERIFIED-FIX (overstated): only **BrowserView** overlays are forced opaque. CEF's overlay host (`libcef/browser/views/overlay_view_host.cc`, branch 7977 = M152, L198–210) creates the overlay widget with `WindowOpacity::kTranslucent` and a `SK_ColorTRANSPARENT` compositor background ("Make the Widget background transparent. The View might still be opaque."). `CefViewImpl::SetBackgroundColor` uses `views::CreateSolidBackground(color)`, which keeps alpha. So a **plain Panel overlay with an ARGB background** (e.g. `0x66000000`) should give a real translucent scrim under the Command Bar or Peek, and stacked 1–2px translucent Panels can fake a hard shadow. This still needs a prototype; §8.2 is updated.
   - VERIFIED-FIX (missing constraint): cef_types_runtime.h says "Alloy style Windows with the Views framework can host only Alloy style BrowserViews but Chrome style Windows can host both style BrowserViews. Additionally, a Chrome style Window can host at most one Chrome style BrowserView". sta has many BrowserViews per window, so **every BrowserViewDelegate must also return `RuntimeStyle::ALLOY`**; setting it on the Window alone isn't enough. CEF logs "Cannot add Chrome style BrowserView to Alloy style Window" (browser_view_impl.cc) and never creates the browser. Alloy also gives you **no** Chrome UI: no find bar, permission prompts, download shelf, extensions, autofill/datalist UI, or built-in zoom/print/devtools accelerators (§7, §8.1).

---

## 1. Feature inventory and priorities

| # | Feature | Arc behavior (short) | sta | Notes |
|---|---|---|---|---|
| 1 | Vertical sidebar (tabs) | The sidebar is the tab strip. Top to bottom: nav/URL, Favorites grid, Space title, Pinned, divider, "+ New Tab", Today tabs, bottom bar with Library/Downloads, Space icons and "+". [Arc] | **P0** | Left side only in the MVP. Right-side sidebar is P2. |
| 2 | Resizable sidebar | Drag the sidebar edge; width is clamped. [Arc-Win] | **P0** | 200–440px (§5). |
| 3 | Toggle sidebar + hover reveal | Ctrl+S hides it. Hovering the left edge shows a floating sidebar. [Arc-Win] | **P0** | §2.15 |
| 4 | Spaces | Each has a name, emoji icon, theme (solid/gradient, noise), its own Pinned and Today, and an optional Profile. Switch by clicking the icon, Alt+N, Ctrl+Alt+←/→, the mouse back/forward buttons, a touchpad swipe, or the Command Bar. Spaces can be reordered by dragging icons. [Arc]/[Arc-Win] | **P0** | Profile per Space is P1. |
| 5 | Favorites | Icon grid above the Space title, shared across Spaces, max 12, reverts to the saved URL. Favorites are per Profile. [Arc] | **P0** | |
| 6 | Pinned tabs | Per Space; never auto-archive. Clicking the favicon resets to the pinned URL. A "/" before the title means you navigated away. Menu offers "Replace Pinned URL with Current" and "Edit…". [Arc] | **P0** | |
| 7 | Folders (in Pinned) | Nestable, collapsible, drag in and out. Tabs in folders don't auto-archive. [Arc] | **P0** | Nesting depth capped at 3 [A]. |
| 8 | Today tabs + auto-archive | Idle unpinned tabs archive after 12h (default), 24h, 7d or 30d. Viewing a tab resets its timer. Auto-archive can't be disabled. The setting is per Profile. [Arc] | **P0** | |
| 9 | Archive view | Open via Ctrl+T → "View Archive". Click an entry to restore it. "Clear archive" command. [Arc-Win] | **P0** | Built-in `sta://archive` page. |
| 10 | Clear Today | Ctrl+Shift+K archives all Today tabs. [Arc] | **P0** | |
| 11 | Command Bar | Ctrl+T searches open tabs, history, actions and Spaces. Tab lists actions and completes site-search keywords. Autocomplete. Click outside to dismiss. [Arc]/[Arc-Win] | **P0** | §6 |
| 12 | Edit URL | Ctrl+L or Alt+D opens the Command Bar prefilled with the current URL. [Arc-Win] | **P0** | |
| 13 | URL pill at sidebar top | Shows the domain only. The full URL is only in the optional toolbar. Copy button. [Arc] VERIFIED-FIX: the optional-toolbar/full-URL article (25625458052247) is marked "applies to Arc on macOS, but not Arc on Windows". Arc-Win has a top toolbar containing "pinned extensions, window controls, URL bar" (release notes) and "a clickable URL bar when no web contents are being shown". | **P0** | §2.16 |
| 14 | Copy URL / as Markdown | Ctrl+Shift+C strips trackers. Ctrl+Shift+Alt+C copies `[title](url)`. [Arc-Win] | **P0** | |
| 15 | Split View | Created via Ctrl+Shift+=, drag to an edge or next to a tab, a context menu, or commands ("Add Right/Left/Top/Bottom Split"). Horizontal and vertical, resizable divider. Shows as one sidebar tab. "Separate All Tabs". [Arc] | **P0** | Max 4 panes [Arc-mac shortcut list]. |
| 16 | Recent-tab switcher | Ctrl+Tab cycles the 5 most recently visited tabs. [Arc] | **P0** | §2.13 |
| 17 | Peek | Links from Pinned/Favorite tabs to other sites open in Peek. Shift+click opens Peek from any tab. Ctrl+O opens as a tab, Esc closes. Can be disabled in Settings. [Arc]/[Arc-Win] | **P0** (pinned/fav + popups), **P1** (Shift+click) | §2.18 |
| 18 | Little Arc (external-link mini window) | macOS only. [Arc] | **P2** | MVP: external links open as a Today tab (§2.19). |
| 19 | Boosts | Per-domain CSS/JS, color, font, zap; macOS only. [Arc] | **P0-lite** (code-only CSS/JS per host), **P2** (visual editor, zap) | §2.22 |
| 20 | Session restore | Restores window size, position and maximized state. The sidebar is persistent data. [Arc-Win] | **P0** | Lazy loading (§2.21). |
| 21 | Downloads | Ctrl+J opens a downloads popover. In-progress downloads appear at the sidebar bottom. Downloads can be dragged into pages. [Arc-Win] | **P0** | |
| 22 | Settings | Opened with Ctrl+,. Search engine and archive timing are per Profile. Mica/Acrylic option on Windows. [Arc-Win] | **P0** | Built-in page, no Mica (§8). |
| 23 | Find in page | Ctrl+F, F3/Ctrl+G next, Shift+F3/Ctrl+Shift+G previous. | **P0** | VERIFIED-FIX: Alloy style has no find bar UI, so sta has to draw it (§2.27). Recent Arc-Win maps Ctrl+F to "Ask on Page" (AI), which is a non-goal. |
| 24 | Zoom | Ctrl +/−/0 and Ctrl+wheel, with a zoom % toast. Remembered per host [A]. [Arc-Win] | **P0** | |
| 25 | Back/forward/reload/stop | Alt+←/→, Ctrl+R/F5, Ctrl+Shift+R/Ctrl+F5, Esc. [Arc-Win] | **P0** | |
| 26 | DevTools | Ctrl+Shift+I / F12, dockable. [Arc-Win] | **P0** (separate window) | Docked DevTools is P2. |
| 27 | History page | Ctrl+H. [Arc-Win] | **P1** | History appears in the Command Bar in P0. |
| 28 | Multiple windows | Ctrl+N. Windows share the sidebar data. [Arc] | **P1** | Data model supports it from day 1. |
| 29 | Incognito window | Ctrl+Shift+N. [Arc] | **P1** | In-memory RequestContext. |
| 30 | Profiles | Separate logins, cookies, history, favorites, archive timing and search engine. Assigned per Space. [Arc] | **P1** | MVP ships a single "Default" profile. |
| 31 | Tab audio indicator / mute | Click the audio icon in the sidebar. [Arc-Win] | **P1** | |
| 32 | Sidebar audio mini-player, auto-PiP | [Arc-Win] | **P2** | |
| 33 | Tab discarding (memory) | Freezes and discards background tabs. [Arc-Win] | **P1** | Unload tabs idle for more than 30 min [A]. |
| 34 | Drag tab out → new window | [Arc-Win] | **P2** | |
| 35 | Site search keywords (e.g. `yt` + Tab) | [Arc] | **P1** | |
| 36 | Air Traffic Control (URL → Space routing) | macOS only. [Arc] | **P2** | |
| 37 | Share Space/Folder, sync, Easels, Notes, Live Folders, Arc Max AI, Calendar live icon | [Arc] | **Non-goal** | §7 |
| 38 | VERIFIED-FIX (gap) Site permission prompts (camera/mic/geo/notifications…) | Chromium UI in Arc | **P0** | Alloy default: media access "will deny the request", other prompts are `CEF_PERMISSION_RESULT_IGNORE` (cef_permission_handler.h). Without our own prompt, video calls break. §2.25 |
| 39 | VERIFIED-FIX (gap) HTML5 fullscreen (video) and window fullscreen | F11, Alt+Shift+F [Arc-Win] | **P0** | cef_display_handler.h: "With Alloy style the client is responsible for triggering the fullscreen transition (for example, by calling CefWindow::SetFullscreen when using Views)". §2.26 |
| 40 | VERIFIED-FIX (gap) Undo sidebar actions | Ctrl+Z undoes archive/move in the sidebar [Arc-Win] | **P1** | Reuses the reopen stack (§2.4) plus move records. |
| 41 | VERIFIED-FIX (gap) Double-click a Favorite resets it | "Double clicking a favorite resets it to its pinned URL." [Arc-Win] | **P0** | Same action as the favicon click (§2.2). |

---

## 2. P0 interaction spec

### 2.1 Window anatomy (docked sidebar)

```
+---------------------------------------------------------------------------------+
| [≡][◧] [←][→][⟳]   (sidebar top row, 40px)  |  top strip 40px: drag area  [split][–][□][×] |
| ( 🔒 github.com            ⧉ )  URL pill 36px |+-----------------------------------------------+|
| [fav][fav][fav][fav]   Favorites grid          ||                                               ||
| [fav][fav]                                     ||             active tab BrowserView            ||
| 🚀 Work                        Space title 32px ||           (or 2–4 split panes)                ||
|  ▸ 📁 Projects                  Pinned          ||                                               ||
|    Linear — Issues                              ||                                               ||
|  / GitHub                (slash = navigated)    ||                                               ||
| ────────────────────── ↓ Clear  divider        ||                                               ||
|  + New Tab                                      ||                                               ||
|  Today tab 1 (newest)                           ||                                               ||
|  Today tab 2                                    ||                                               ||
|  …                                              |+-----------------------------------------------+|
| [⬇ lib]     (🚀)(🏠)(🎮)          [+]  bottom 44px |                 8px frame gap                  |
+---------------------------------------------------------------------------------+
```
- **Top row.** `≡` opens the app menu (Alt+F also opens it [Arc-Win]). `◧` toggles the sidebar. Then back, forward, reload/stop.
- **Caption buttons.** Min/max/close sit at the top-right of the top strip in the Windows position (46×40px each). The split button sits next to them [Arc-Win: "the Split icon next to the minimize window control icon"].
- **Frameless window.** The top strip and the empty sidebar top-row area are draggable (§8).
- VERIFIED-FIX (gap): HTML caption buttons have two problems.
  - They need IPC → `ImplWindow::minimize/maximize/restore/close`.
  - They **won't show the Windows 11 Snap Layouts flyout**, which only appears when `WM_NCHITTEST` returns `HTMAXBUTTON` over the maximize button. Getting it requires the same Win32 subclass on `impl ImplWindow … fn window_handle(&self) -> cef_window_handle_t;` (L44318) used in §8.4, with the maximize-button rect reported from the top-strip HTML. P1; the MVP accepts no Snap flyout.
  - Instead of a custom IPC for drag rects, the top-strip/sidebar HTML can use CSS `app-region: drag` / `no-drag`, which arrives via `impl ImplDragHandler … fn on_draggable_regions_changed(&self, browser: Option<&mut Browser>, frame: Option<&mut Frame>, regions: Option<&[DraggableRegion]>)` (L19155). Offset those rects by the BrowserView's origin in the window before passing them to `set_draggable_regions`, because cef_window.h requires "window coordinates". This is the cefclient views_window pattern.

### 2.2 Tab model and state machine

Every sidebar row is a **SidebarItem**: a Tab, Folder or SplitGroup, in section `Favorites | Pinned | Today`.

| State | Meaning | Visual |
|---|---|---|
| `Loaded` | A browser exists (it may be hidden) | Normal text. Active row is highlighted. |
| `Unloaded` | Row exists, no browser | Title at 60% opacity; favicon desaturated to 70% [A] |
| `Navigated` (Pinned/Fav only, `url != pinned_url`) | Loaded and navigated away | "/" glyph before the title. Favicon tooltip "Back to Pinned URL" [Arc-Win] |
| `Archived` | Row removed; an ArchiveEntry exists | Shows only in the Archive |

Transitions:

| Trigger | Today tab | Pinned tab | Favorite |
|---|---|---|---|
| Click row (Unloaded) | Load `url` | Load **`pinned_url`** | Load **`pinned_url`** |
| Click row (Loaded) | Activate | Activate | Activate |
| Click favicon while `Navigated` | – | Navigate to `pinned_url` (reset) | same; VERIFIED-FIX: **double-clicking a Favorite tile also resets** [Arc-Win: "Double clicking a favorite resets it to its pinned URL."] |
| Ctrl+W / hover × | **Archive** (reason `UserClosed`), push onto the reopen stack | **Unload**: destroy the browser, keep the row, push `ReopenPinned{url_at_close}` | same as Pinned |
| Idle > `archive_after` | **Archive** (reason `Auto`) | never | never |
| Ctrl+D | Pin: `pinned_url = url`, move to the **bottom** of Pinned (top level) — VERIFIED-FIX: was "top of Pinned", which contradicted §2.5 ("bottom of Pinned, top level [A]"); aligned to §2.5 | Unpin: clear `pinned_url`, move to the top of Today (stays loaded) | Unfavorite: move to the top of Today |
| Memory discard (P1) | Unload and keep `url` | Unload and keep `url` until the next explicit close | same |

**Activation after closing the active tab** [A]:
1. If the closed tab has an opener that is still open, and the user never left the closed tab since it opened, activate the opener.
2. Otherwise activate the most recently used tab in the same Space.
3. Otherwise show the empty state, a blank themed content area with the hint "Ctrl+T to open a tab".

**Ctrl+W with nothing to close:**
- If the Space has no active tab, Ctrl+W closes the window (Arc: "Close current tab or window").
- If Peek is open, Ctrl+W closes Peek only.
- In a split, Ctrl+W closes only the focused pane.

### 2.3 New tab (Ctrl+T) and edit URL (Ctrl+L / Alt+D)

- **Ctrl+T** opens the Command Bar in `NewTab` mode with an empty input. **No tab exists until the user commits** [Arc: "New tab / open the Command Bar"]. The "+ New Tab" row and the bottom-bar "+ → New Tab" do the same.
- Committing a URL or search creates a **Today tab at the top of Today**, directly under "+ New Tab" [Arc], activates it, focuses the page, and records a `Typed` visit.
- Committing "Switch to Tab" activates that tab, switching Space if needed. No new tab is created.
- **Alt+Enter** on a URL or search opens a background Today tab and keeps the bar open [A]. **Shift+Enter** opens the result in a new split pane (P1).
- **Esc** or a click outside dismisses the bar with no side effects [Arc-Win].
- **Ctrl+L / Alt+D** opens the bar in `EditUrl` mode:
  - The input is prefilled with the full current URL, all text selected.
  - Enter navigates the **current** tab, including a Pinned tab, which then shows "/".
  - Alt+Enter opens a new Today tab.
  - With no active tab, `EditUrl` behaves like `NewTab`.
- Link opened with **middle-click, Ctrl+click, or `target=_blank` with a user gesture** from a Today tab: create a Today tab **directly below the opener**. Middle-click and Ctrl+click open in the background; `_blank` opens in the foreground [A].
  - VERIFIED-FIX (implementation gotcha): a background tab only loads once its BrowserView is **in the view hierarchy**. browser_view_impl.cc (M152): "Top-level browsers will be created when this view is added to the views hierarchy." So add the new BrowserView to the content Panel with `set_visible(0)`, not just create it.
  - VERIFIED-FIX (gap, [Arc-Win]): when the sidebar is hidden, show a "New Tab Created" toast with a go-to-tab button (release notes: "when opening a link in a new tab from the context menu or Ctrl+click and the Sidebar is hidden").
- The same link from a Pinned tab or Favorite: open Peek if the target is another site (§2.18). Otherwise open a new Today tab at the top of Today.

### 2.4 Reopen (Ctrl+Shift+T)

- Each window keeps a LIFO stack (last 25 entries) of **manual** closures: Today close, pinned/favorite unload, split close, and Clear Today as one batch entry.
- **Auto-archive never pushes onto this stack** [A], so Ctrl+Shift+T never resurrects something the user didn't close.
- Pop behavior by entry type:
  - `ArchivedTab`: restore to its original Space, section, folder and sort_key. If the folder is gone, restore to the top of Today in that Space. If the Space is gone, use the current Space. Remove the ArchiveEntry, activate the tab, and switch Space if needed.
  - `ReopenPinned`: reload the pinned tab at `url_at_close`, so its navigated state comes back.
  - `ClearToday` batch: restore all tabs in their original order without activating any.
  - `SplitClosed`: rebuild the group with the same orientation and fractions.
- Back/forward history of a restored tab is **not** restored, because CEF has no session-history restore API (§8). Only the URL comes back.

### 2.5 Pin, favorite and unpin

| Action | Result |
|---|---|
| Ctrl+D on a Today tab | Moves to the **bottom of Pinned, top level** [A]. `pinned_url = url`, `pinned_title = None` (live title shown). Toast: "Pinned". |
| Ctrl+D on a Pinned tab | Unpins to the top of Today [Arc]. |
| Ctrl+D on a Favorite | Unfavorites to the top of Today. |
| Command "Add to Favorites" / drag to the grid | Only allowed if the Favorites count is under 12 [Arc]. Otherwise the drop is rejected with a shake animation and the toast "Favorites are full (12)". |
| Context menu → "Edit Pinned Page" → "Replace Pinned URL with Current" | `pinned_url = url`; the "/" disappears. [Arc] |
| → "Edit…" | Inline dialog with Title (custom) and URL fields. |
| Context menu → "Rename" | Sets `custom_title`; it overrides the page title. |

### 2.6 Drag and drop matrix

Drop targets are the Favorites grid, Pinned list, a folder row, the Today list, a Space icon (bottom bar), the content-area edge zones, and the Archive.

| Source ↓ / Target → | Favorites grid | Pinned (between rows) | Onto folder row | Today (between rows) | Space icon | Content edge zone |
|---|---|---|---|---|---|---|
| Today tab | favorite (if under 12) | pin (`pinned_url = url`) | pin into folder (at end) | reorder | move to that Space's Today (top) | create split with active tab |
| Pinned tab | favorite | reorder / out of folder | move into folder | unpin | move to that Space's Pinned (bottom) | **duplicate** into Today, then split [Arc: split tabs from pinned "open again in the Today section"] |
| Favorite | reorder | unfavorite → Pinned of current Space | into folder | unfavorite → Today | n/a (rejected) | duplicate → split |
| Folder | rejected | reorder | nest (max depth 3) | rejected (folders live in Pinned only [A]) | move folder to that Space | rejected |
| Split group row | rejected | pin group | into folder | reorder | move group | rejected |
| Link dragged from page | new favorite | new pinned tab | new tab in folder | new Today tab at drop index [Arc-Win] | new Today tab in that Space | open in new split pane |
| Download from Ctrl+J list | – | – | – | – | – | Chromium file drop into the page [Arc-Win] |

Drag rules [A]:
- **Start threshold.** A drag starts after 4px of movement. Esc cancels.
- **Insertion indicator.** A 2px accent line with a 6px dot at the leading end.
- **Hovering a folder row** for 300ms highlights it as the drop target. Hovering a collapsed folder for 600ms expands it.
- **Spring-loading.** Hovering a Space icon for 500ms switches the Space so the user can drop at a precise spot.
- **Edge zones** are the outer 25% of the content width (left/right) or height (top/bottom). The hovered zone shows a translucent accent overlay. That overlay must be drawn by a native overlay (§8), so the MVP uses a solid 30% tint rectangle.
- **Auto-scroll.** The sidebar scrolls at up to 600px/s when dragging within 32px of its top or bottom edge.

### 2.7 Folders

- Create with "+ → New Folder" or the Command Bar "New Folder". It is inserted at the top of Pinned with inline rename focused. Default name "New Folder".
- The row is 32px: chevron, folder icon (full-color with the Space accent [Arc-Win]), name. Click toggles collapse. Double-click or F2 renames.
- Context menu: Rename, New Subfolder, Open All in Split (P1, max 4), Delete (moves the tabs to the Archive with reason `FolderDeleted` [A]), Move to Space.
- Folder collapse state persists per folder.

### 2.8 Auto-archive algorithm

```
every 60s and on app resume:
  for tab in Today tabs of every Space (not Pinned, not Favorite, not inside Folder):
     if tab is the active tab of any window or visible in a split/peek: continue
     if tab.is_playing_audio: continue                       // [A]
     idle = now - max(tab.last_active_at, tab.created_at)
     if idle >= profile.archive_after: archive(tab, reason=Auto)
```
- `archive_after ∈ {12h (default), 24h, 7d, 30d}` [Arc]. Arc can't disable it; sta follows that [A].
- A tab's timer resets when the tab becomes active or visible, and every 60s while it stays active [Arc: "Viewing or clicking on a tab will always reset the timer"].
- Archiving a split group archives each pane as its own ArchiveEntry, sharing a `group_snapshot_id` so restoring one offers "Restore split".
- Archive retention: 30 days, then purge [A]. Arc doesn't document a limit.

### 2.9 Archive view (`sta://archive`)

- Opened by the command "View Archive". Opens as a singleton Today tab.
- Layout: search field, then entries grouped by day ("Today", "Yesterday", date). Each row shows favicon, title, host, time, the Space emoji, and a reason badge (Auto/Closed).
- Click restores the entry (same logic as Ctrl+Shift+T) [Arc-Win: click restores]. Context menu: Restore, Copy URL, Delete. The header has a "Clear archive" button [Arc], with a confirmation.

### 2.10 Spaces

- **Create** with "+ → New Space" or the command "New Space". A creation sheet in the sidebar has three steps: emoji picker, name, theme swatch. The new Space is appended and activated.
- **Switching:**
  - Click the Space icon in the bottom bar.
  - Alt+1..9 [Arc-Win].
  - Ctrl+Alt+←/→, with no wrap-around [A].
  - Mouse back/forward buttons (X1/X2) while the pointer is over the sidebar [Arc-Win]. VERIFIED-FIX: the release note only says "You can now use the back/forward buttons on your mouse to switch Spaces". Limiting it to "over the sidebar" is [A]. Implement it in the sidebar HTML (`mouseup` with `button` 3/4); X1/X2 over a page stays history navigation.
  - Horizontal touchpad scroll over the sidebar: switch once accumulated |dx| exceeds 30% of the sidebar width.
  - Command Bar: "Go to Space: Name".
- **On switch:**
  1. Hide the current content, then show the target Space's `last_active_item_id`. If it is Unloaded, load it. If it is gone, show the empty state.
  2. Sidebar sections (Space title, Pinned, Today) slide horizontally, 220ms. **The Favorites grid stays static** [Arc-Win: Favorites render smoothly when swiping]. VERIFIED-FIX: the release note says "when swiping between Spaces **associated with different Profiles**", and Favorites are per Profile (Profiles article lists "Favorites"). So the grid stays static only when both Spaces share a Profile; otherwise cross-fade it to the target Profile's Favorites. With one profile in the MVP it is always static.
  3. The theme gradient cross-fades over 300ms. The native frame color is set to the target theme's `frame` token at the midpoint (§5).
- **Edit** via right-click on a Space icon [Arc-Win] or hover-edit on the Space title: Rename, Change Icon, Theme…, Move Up/Down, Delete (with confirmation; Today and Pinned tabs go to the Archive with reason `SpaceDeleted`). Reorder by dragging icons [Arc-Win].
- The last Space can't be deleted.
- The bottom bar shows up to 8 Space icons; more than that turns it into a horizontally scrollable strip.

### 2.11 Ctrl+1..9, Ctrl+Alt+↑/↓

- **Visual order** = Favorites (grid order, row-major), then Pinned (depth-first, **visible rows only**: collapsed folders are skipped), then Today.
- A split group counts as one item; activating it focuses its last focused pane.
- **Ctrl+1..8** activates the Nth item in visual order. **Ctrl+9** activates the **last** item (Chromium convention) [A on top of Arc's "go directly to tab N"].
- **Ctrl+Alt+↑/↓** goes to the previous/next item in visual order with no wrap. Ctrl+PgUp/PgDn are aliases [A].

### 2.12 Tab activation bookkeeping

On activation:
- Set `last_active_at = now`.
- Push onto the window's MRU list (deduplicated, cap 50).
- Set the Space's `last_active_item_id`.
- Update the URL pill.
- ~~Call `BrowserHost::was_hidden(0)` for the new browser and `was_hidden(1)` for the previous one, unless it is still visible in a split.~~
  - VERIFIED-FIX: **wrong API for windowed browsers.** cef_browser.h on `WasHidden`: "This method is only used when window rendering is disabled." (OSR only.)
  - For Views-hosted tabs, call `impl ImplView … fn set_visible(&self, visible: ::std::os::raw::c_int);` (L38365) on the BrowserView: `1` for the new tab, `0` for the previous one unless it is still in a split. Hiding the view hides the web contents' native view, which Chromium uses for visibility/throttling.
  - Verify with `document.visibilityState` in the prototype.

### 2.13 Ctrl+Tab recent-tab switcher

[Arc: the Tab Switcher (Ctrl+Tab) cycles the five most recently visited tabs.] sta behavior:

1. **Ctrl↓ + Tab↓**: set the selection to MRU[1] (the previous tab). Do not show UI yet.
2. **Ctrl released within 250ms** of the first Tab (a quick tap): commit immediately. No overlay flashes.
3. **Ctrl still held after 250ms**, or **Tab pressed again**: show the switcher overlay, centered on the content area.
   - Up to **5** cards (MRU[0..5]), each with favicon, title (1 line), host and the Space emoji.
   - Each card is 120px wide with a 72px thumbnail area. The MVP uses a large favicon on the Space color instead of a page thumbnail.
4. **Tab** moves to the next card and **Shift+Tab** to the previous, both wrapping within the 5 cards.
5. **Ctrl released**: commit the selection, which activates the tab and may switch Space. **Esc** or window deactivation cancels.
6. The MRU list is updated **only on commit**, never on intermediate selections.
7. MRU covers the whole window, across Spaces [A].

The implementation needs Ctrl **key-up**, which CEF accelerators don't report (§8.3).

VERIFIED-FIX (gaps in the key-up plan):
- `on_pre_key_event` fires only for the browser that has keyboard focus. Install the **same** KeyboardHandler on every Client: tabs, sidebar, top strip, and every overlay.
- Show the switcher overlay with `can_activate=0` so focus (and the Ctrl key-up) stays in the current browser.
- When focus is on a non-browser View, or in the empty state with no browser, the fallback is `impl ImplWindowDelegate … fn on_key_event(&self, window: Option<&mut Window>, event: Option<&KeyEvent>) -> ::std::os::raw::c_int` (L43265). cef_window_delegate.h: "Called after all other controls in the window have had a chance to handle the event". It is unverified whether KEYUP events reach it.
- Last-resort safety net: poll `GetAsyncKeyState(VK_CONTROL)` on a 50ms timer while the switcher is open.

### 2.14 Clear Today (Ctrl+Shift+K)

- Archives all Today tabs of the current Space. **Excluded:** tabs visible in the content area and tabs playing audio [A].
- The divider shows a "↓ Clear" affordance on hover [Arc].
- The whole operation is one entry on the reopen stack, and an undo toast appears for 6s ("Cleared 14 tabs · Undo").

### 2.15 Sidebar toggle and hover reveal

- **Ctrl+S** or the `◧` button toggles `sidebar_docked`.
  - **Hiding:** the content frame expands to the full window width, keeping the 8px gap on all sides (the top strip stays 40px). The top strip then shows the URL pill centered, so the URL stays visible [A].
  - **Native layout changes are instant (no animation)** (§8.5). The sidebar HTML fades its contents over 120ms before hiding.
- **Hover reveal** (only while hidden):
  - Trigger zone: the pointer within **8px of the window's left edge** for **120ms**, or a drag in progress entering that zone.
  - **Show:** the floating sidebar overlay appears at full height minus 8px margins, with the current width and radius 12px. Its HTML contents slide in from −24px with 0→1 opacity over 160ms.
  - **Hide:** 400ms after the pointer leaves the overlay bounds. Hiding is suppressed while a context menu, rename field, drag, or emoji picker is open.
  - **Ctrl+S** while the overlay is floating docks it.
  - **Click on a tab** in the floating sidebar activates the tab. The overlay stays open until the pointer leaves.
- VERIFIED-FIX (feasibility gaps):
  1. **Edge detection.** With the sidebar hidden, the 8px left strip is the native frame gap. CEF Views has no mouse callbacks for Panels, so hover detection needs the Win32 subclass on the top-level HWND (`WM_MOUSEMOVE` / `TrackMouseEvent`, see §8.4). The page BrowserView can't report it because the pointer isn't over it.
  2. **Don't create a second sidebar renderer.** cef_panel.h `RemoveChildView`: "Remove a child View. The View can then be added to another Panel." Move the one sidebar BrowserView into a wrapper Panel that is the floating overlay's contents, and move it back when docking. This keeps one DOM, scroll position and state. browser_view_impl.cc confirms that removing a BrowserView only disassociates it from the widget; the browser lives as long as Rust holds the `BrowserView` reference.
  3. The floating overlay needs `can_activate=1` (rename fields, emoji picker).
  4. The "radius 12px" above sits over web content, so it conflicts with the §8.2 rule. MVP: square corners, or a translucent Panel ring (§8.2).

### 2.16 URL pill (sidebar top)

- **Content:**
  - Security glyph, then the **host without `www.`**, e.g. `github.com`. For IDN hosts show Unicode, but fall back to punycode for mixed-script hosts (Chromium's spoof-check rule) [A].
  - For `file:` show the file name. For `sta://settings` show "Settings", and similarly for other built-in pages.
  - For the empty state show a "Search or enter URL…" placeholder.
- **Hover** shows a ⧉ copy button at the right, plus a tooltip with the full URL after 600ms.
- **Clicking the pill** opens `EditUrl` mode (same as Ctrl+L) [Arc: domain in sidebar; the URL is edited via the Command Bar].
- **⧉ or Ctrl+Shift+C** copies the **cleaned URL** and shows the toast "Copied URL". Cleaning removes:
  - `utm_*`, `fbclid`, `gclid`, `dclid`, `gbraid`, `wbraid`, `msclkid`, `mc_cid`, `mc_eid`, `igshid`, `igsh`, `si` (only on youtube.com/youtu.be/spotify.com), `ref_src`, `_hsenc`, `_hsmi`, `yclid`.

  [Arc-Win strips extra link trackers, e.g. on Instagram.]
- **Ctrl+Shift+Alt+C** copies `[title](cleaned_url)` [Arc-Win].
- **Loading:** a 2px progress bar along the pill's bottom edge, driven by `on_loading_progress_change`. It shows a determinate 0–100% value and fades out 200ms after completion.

### 2.17 Split View

- **Create:**
  - **Ctrl+Shift+=** opens the Command Bar in `Split` mode. The chosen result opens in a new pane to the **right** of the focused pane.
  - Commands: "Add Right/Left/Top/Bottom Split" [Arc].
  - Drag a tab onto a content edge zone, or next to another tab in the sidebar [Arc-Win].
  - Tab context menu: "Open in Split View".
- **Limits:** 2–4 panes, one orientation per group (Horizontal = side by side, Vertical = stacked). Mixed nesting is a non-goal in the MVP.
- **Layout:**
  - Panes are separated by a **6px gutter**. A **draggable divider** changes the fractions; minimum pane size is 240px wide or 160px tall.
  - Double-clicking a divider equalizes the panes.
  - Adding a pane re-equalizes all panes.
- **Focused pane:** a 2px accent ring, drawn as the pane's wrapper-panel background showing through a 2px inset (§8.4). Unfocused panes get a 2px ring in the `frame` color. Clicking into a pane focuses it.
- **Navigation:**
  - **Ctrl+Shift+1..4** focuses pane N [Arc-Win].
  - **Ctrl+Shift+[ / ]** focuses the previous/next pane [Arc-mac mapping, adopted].
  - **Ctrl+L** edits the focused pane's URL [Arc].
- **Sidebar row:** one 36px row split into N equal segments with a 1px divider between them. Each segment shows favicon + title (ellipsized) and clicking it focuses that pane. ~~The hover × closes the whole group (archiving all panes).~~ VERIFIED-FIX: Arc's Split View article lists "Hit the X next to either Split View Tab in the Sidebar" and "Hit the X above either split view panel" as ways to exit. So each **segment** gets its own hover × that closes (archives) that pane only, and the group dissolves at 1 pane. "Archive whole group" moves to the row context menu.
- **Remove a pane:**
  - **Ctrl+Shift+-** separates the focused pane into its own Today tab, placed below the group [A].
  - **Ctrl+W** archives the focused pane's tab.
  - The × in the pane header (top-right, P1) archives the pane. (Arc has this "X above either split view panel". It is P1 because each pane header needs an extra HTML surface: a thin BrowserView per pane or a small overlay.)
  - When 1 pane remains, the group dissolves into a normal tab.
- **"Separate All Tabs"** (context menu) turns every pane into its own Today tab, in order [Arc].
- **Pinning a group** (Ctrl+D on the group) is P1. Arc supports pinned splits.

### 2.18 Peek

- **Triggers:**
  - **P0:** a main-frame navigation from a **Pinned tab or Favorite** that meets all of these:
    - `user_gesture = 1`, `is_redirect = 0`, HTTP method `GET`
    - transition source is `LINK`
    - target registrable domain (eTLD+1) ≠ that of `pinned_url`
  - **P0:** `window.open`/`target=_blank` from a Pinned tab or Favorite.
  - **P0 [A]:** script popups that request popup features (width/height), such as OAuth windows, from **any** tab. These are hosted in Peek so `window.opener` keeps working (§8.6).
  - **P1:** Shift+click from any tab [Arc]. Chromium reports it as the `NEW_WINDOW` disposition.
  - **Never Peek:** form submissions (`FORM_SUBMIT`), redirects, same-site links, and navigations the user typed.
- **Geometry** (relative to the content frame):
  - width `min(content_w − 96, 1200)`, height `content_h − 56`, horizontally centered, top offset 28px
  - 36px header with favicon, host, and buttons in Arc order: **×**, **Split**, **Expand** [Arc-Win reordered to match macOS]
  - Peek body radius 12px (header HTML); the web area is square (§8.2). VERIFIED-FIX: Peek sits over web content, so any HTML radius shows the overlay's own background in the corners. MVP: radius 0 (§8.2 rule).
- **Actions:**
  - **Esc** [Arc-Win], **Ctrl+W**, or **×** closes Peek.
  - **Clicking outside** closes Peek. Implemented as "the underlying tab's browser got focus" (§8.6).
  - **Ctrl+O** or **Expand** turns Peek into a Today tab, inserted at the top of Today, keeping the same browser (no reload). VERIFIED-FIX (how): make the Peek overlay's contents a wrapper **Panel**, then `remove_child_view` the BrowserView from it and `add_child_view` it into the content Panel. Keep a Rust `BrowserView` reference during the move: browser_view_impl.cc destroys the browser only when "the last BrowserView reference was released" after removal from the hierarchy.
  - **Split** turns Peek into a Today tab and makes a split with the originating tab.
- One Peek per window. Opening a second Peek replaces the first, which is closed.
- **Settings → Links:** "Open Peek for links from Pinned tabs and Favorites" (default on), "Shift+click opens Peek" (default on, P1) [Arc-Win 1.7.1].
- VERIFIED-FIX (P0 details that were missing or wrong):
  - **Overlay config.** Peek hosts a live web page, so it must be added with `can_activate=1`; §8.1 had omitted it. cef_window.h: "Setting |can_activate| to true will allow the overlay view to receive input focus."
  - **Focus on show.** When Peek opens, give it focus with `impl ImplView … fn request_focus(&self);` (L38383) on the Peek BrowserView. Otherwise the underlying page keeps focus, and a click on it never fires `on_got_focus`, so "click outside closes" silently fails.
  - **Click outside** includes the sidebar and top strip: `on_got_focus` from *any* other browser in the window closes Peek. Clicks on the native frame gap need the §8.4 subclass (P1).
  - **OAuth-in-Peek is [A]**; Arc documents nothing about script popups. For popup-feature windows:
    - disable click-outside dismissal (Esc/× only) so an accidental click doesn't kill a login flow;
    - close Peek when the popup calls `window.close()`, i.e. when `impl ImplLifeSpanHandler … fn on_before_close(&self, browser: Option<&mut Browser>)` (L20755) fires for the popup browser;
    - never replace a popup-Peek with a link-Peek; open the link as a Today tab instead.
  - **Scrim.** Arc dims the page behind Peek. A translucent **non-browser Panel overlay** added *before* the Peek overlay should give a real scrim (§8.2 corrected); P1 after a prototype.

### 2.19 External links (from other apps)

- Arc's Little Arc is a P2 item for sta.
- **P0:** when sta is the default browser and another app opens a URL, the URL goes to the most recently active window. It opens as a **Today tab at the top of the current Space**, and the window is brought to the foreground.
- If sta isn't running, launch it, restore the session, then open the URL.
- Second-instance forwarding: named mutex plus `WM_COPYDATA`, or a named pipe.

### 2.20 Downloads

- **Default save:** no prompt; files go to `%USERPROFILE%\Downloads`, with ` (1)` style numbering to de-duplicate names.
- **Settings:** "Ask where to save each file" (default off), plus the download folder.
- **Sidebar bottom bar:**
  - While a download is active, the Library button becomes a circular progress ring.
  - A compact card slides up above the bottom bar: file name, `12.3 MB of 40 MB · 8s left`, and a cancel button [Arc].
- **Ctrl+J** opens the **Downloads popover**, anchored to the bottom-left of the sidebar: 360×(auto, max 480)px, the last 20 downloads.
  - Row actions: open (click), Show in folder, Copy link, Retry, Remove from list, Cancel/Pause/Resume.
  - A row can be dragged into a page [Arc-Win].
  - The popover is an HTML surface in the sidebar view. If the sidebar is hidden, use the floating-sidebar overlay.
- **Completion:** a 4s toast "Downloaded name.pdf · Open".
- VERIFIED-FIX (implementation-critical): in Alloy style, downloads are **cancelled unless handled**. cef_download_handler.h: "Return false to proceed with default handling (cancel with Alloy style, download shelf with Chrome style)".
  - `on_before_download` must return 1 and call `impl ImplBeforeDownloadCallback … fn cont(&self, download_path: Option<&CefString>, show_dialog: ::std::os::raw::c_int);` (L18694), with the de-duplicated path and `show_dialog` = the "Ask where to save" setting.
  - The header also says: "Do not keep a reference to |download_item| outside of this method."
- **Non-goals:** Safe Browsing and dangerous-file verdicts (§7).

### 2.21 Session restore

- **Persistence:**
  - Sidebar data is always persisted to SQLite, with a 300ms debounced write and immediate writes on structural changes.
  - Window bounds and maximized state are persisted on change [Arc-Win].
  - Per-window state (`active_space_id`, sidebar docked/width, split fractions, per-Space active item) is persisted on change.
- **Startup:**
  1. Recreate windows at their saved bounds; windows that were maximized open maximized.
  2. Create a browser **only for the active item** of each window's active Space. For a split, create a browser for every pane.
  3. Every other tab starts **Unloaded**. Unloaded Today tabs are loaded from their stored `url` on click. Pinned tabs and Favorites are loaded from `pinned_url`.
  4. Run the auto-archive pass **after** the first paint, so tabs that expired while the app was closed archive quietly.
- **Crash recovery** is the same path: the database is always current.
- **Setting** "On startup": `Restore previous session` (default) | `Open a new tab` (restores the sidebar but no active tab).

### 2.22 Boosts (P0-lite)

- **Data:** host pattern (`example.com` also matches subdomains; the P2 editor adds other matchers), `css`, `js`, `enabled`, `name`.
- **UI:**
  - Command "New Boost for this site" opens `sta://boosts/<id>` as a tab with two code panes (textarea with monospace font; no Monaco in the MVP), an enable toggle, and "Reload affected tabs".
  - When a Boost applies, the URL pill shows a small paintbrush dot. Clicking the dot toggles the Boost [Arc: a greyed paintbrush means disabled].
- **Injection:**
  - At **document start**, in the renderer's `on_context_created` for the main frame. Boost content comes from the browser process via `extra_info` at browser creation, or a process message on update.
  - CSS is added as a `<style id="sta-boost">` appended to `document.documentElement`. VERIFIED-FIX: at V8-context creation the document is usually still empty, so `document.documentElement` can be `null` and a direct append throws. Guard it: `(document.documentElement ? append() : new MutationObserver((_, o) => { if (document.documentElement) { o.disconnect(); append(); } }).observe(document, {childList: true}))`. Also re-check on `DOMContentLoaded`. It is also unverified that `on_context_created` fires before first paint on script-less pages; keep a browser-side fallback that injects the same CSS on load end.
  - JS runs wrapped in an IIFE after `DOMContentLoaded` [A].
- **Limitation:** Boosts are not applied to Peek or to popups in the MVP [A].

### 2.23 Sidebar context menus (tab row)

- **Today tab:** Copy URL · Rename · Pin / Add to Favorites · Move to Space ▸ · Open in Split View · Duplicate · Unload (P1) · Mute (P1) · Archive
- **Pinned tab / Favorite:** Copy URL · Rename · Edit Pinned Page ▸ (Replace Pinned URL with Current, Edit…) · Reset to Pinned URL · Unpin / Remove from Favorites · Move to Space ▸ · Duplicate · Close (unload)
- **Space icon:** Rename · Change Icon · Theme… · Move Left/Right · Delete Space

Menus are HTML-drawn inside the sidebar BrowserView. A menu that would overflow the sidebar width is clamped to the sidebar width minus 16px [A], so no native menu is needed.

### 2.24 Toasts

- **Placement:** bottom-center of the content area, 12px above the frame bottom. Only one toast shows at a time and it auto-dismisses after 2.5s (6s if it has an action).
- **Rendering:** toasts are drawn by a small overlay view (§8.2). A toast over the page is opaque: 36px height, radius 18px on the HTML side.
  - VERIFIED-FIX: "12px above the frame bottom" puts the toast over web content (the bottom gap is only 8px), so an HTML 18px radius shows square overlay corners, which contradicts §8.2.
  - Use radius 0 in the MVP. Alternatively, make the toast a **non-browser** overlay (Panel + `CefLabelButton`/label with an ARGB background). CEF overlays are translucent widgets, so the Panel background can carry alpha (§8.2).

### 2.25 Site permission prompts (VERIFIED-FIX: new P0 section)

Alloy style shows no permission UI. cef_permission_handler.h says media access defaults to "With Alloy style, default handling will deny the request", and other prompts default to "CEF_PERMISSION_RESULT_IGNORE". Without this section, camera, mic, notifications and geolocation silently fail.

- **Hooks:**
  - `impl ImplPermissionHandler … fn on_request_media_access_permission(&self, browser: Option<&mut Browser>, frame: Option<&mut Frame>, requesting_origin: Option<&CefString>, requested_permissions: u32, callback: Option<&mut MediaAccessCallback>) -> ::std::os::raw::c_int` (L21884). Answer with `fn cont(&self, allowed_permissions: u32);` (L21745) or `fn cancel(&self);`.
  - `fn on_show_permission_prompt(&self, browser: Option<&mut Browser>, prompt_id: u64, requesting_origin: Option<&CefString>, requested_permissions: u32, callback: Option<&mut PermissionPromptCallback>) -> ::std::os::raw::c_int` (L21895). Answer with `fn cont(&self, result: PermissionRequestResult);` (L21810), where `PermissionRequestResult::ACCEPT` / `DENY`.
  - Both return 1 and answer asynchronously.
- **UI [A]:**
  - A 320px card anchored to the top-left of the requesting tab's content frame (a `can_activate=1` overlay).
  - Text: "`host` wants to use your camera and microphone". Buttons: Allow / Block. "Remember for this site" is checked by default.
  - Esc = dismiss (ignore).
  - If the tab isn't visible, queue the prompt and show a dot on its sidebar row.
- **Persistence:** a `SitePermission { profile_id, origin, kind, decision }` table. The URL pill hover shows a "Site settings" list to revoke.

### 2.26 Fullscreen (VERIFIED-FIX: new P0 section)

- **Window fullscreen.** F11 and Alt+Shift+F [Arc-Win] call `impl ImplWindow … fn set_fullscreen(&self, fullscreen: ::std::os::raw::c_int);` (L44274). Arc-Win release notes say "We fixed a bug where the sidebar would stay visible when in full screen mode". So in window fullscreen the sidebar **auto-hides** into hover-reveal (§2.15) and the top strip collapses. The rest of this layout is [A].
- **Page (HTML5) fullscreen:**
  - cef_display_handler.h: "With Alloy style the client is responsible for triggering the fullscreen transition (for example, by calling CefWindow::SetFullscreen when using Views)".
  - In `impl ImplDisplayHandler … fn on_fullscreen_mode_change(&self, browser: Option<&mut Browser>, fullscreen: ::std::os::raw::c_int)` (L17620):
    - enter: hide the sidebar, top strip and all overlays, zero the frame insets, hide the other split panes, then `set_fullscreen(1)`;
    - exit: restore the exact previous layout.
- **Esc to exit** is also the app's job in Alloy. cef_browser.h `ExitFullscreen`: "With Alloy style this method should be called in response to a user action such as … pressing the "ESC" key (CefKeyboardHandler::OnPreKeyEvent callback)".
  - Call `impl ImplBrowserHost … fn exit_fullscreen(&self, will_cause_resize: ::std::os::raw::c_int);` (L12748) from `on_pre_key_event` on VK_ESCAPE RAWKEYDOWN while in page fullscreen.
  - Show a 3s "Press Esc to exit full screen" toast on entry.

### 2.27 Find bar (VERIFIED-FIX: new P0 section; Alloy has no find UI)

- **Surface:** an overlay (`can_activate=1`) at the top-right of the focused pane: 360×44px with an input, "3/17" counter, ↑, ↓ and ×.
- **Search:** `impl ImplBrowserHost … fn find(&self, search_text: Option<&CefString>, forward: ::std::os::raw::c_int, match_case: ::std::os::raw::c_int, find_next: ::std::os::raw::c_int);` (L12603). cef_browser.h: "|findNext| indicates whether this is the first request or a follow-up. The search will be restarted if |searchText| or |matchCase| change."
- **Results:** `impl ImplFindHandler … fn on_find_result(&self, browser: Option<&mut Browser>, identifier: ::std::os::raw::c_int, count: ::std::os::raw::c_int, selection_rect: Option<&Rect>, active_match_ordinal: ::std::os::raw::c_int, final_update: ::std::os::raw::c_int)` (L19362) drives the counter.
- **Close:** Esc or × → `stop_finding(1)`, then refocus the page.
- **Persistence:** the query persists per tab while the tab stays loaded.

---

## 3. Keyboard shortcuts (Windows)

"Src" column: `W` = Arc-Win official list or release notes, `M` = Arc-mac mapping adapted to Ctrl, `C` = Chromium convention, `A` = sta.

"Dispatch" column:
- **R** = reserved. Registered as a window accelerator with `high_priority=1`; web pages can't intercept it.
- **P** = page-first. `high_priority=0`; a page's `preventDefault()` wins.
- **X** = contextual. Handled in the overlay or sidebar HTML.

| id | Action | Keys | Src | Pri | Dispatch |
|---|---|---|---|---|---|
| `tab.new` | Command Bar (new tab) | Ctrl+T | W | P0 | R |
| `tab.edit_url` | Command Bar with current URL | Ctrl+L, Alt+D, F6 | W/C | P0 | P |
| `tab.close` | Close/archive tab (unload if pinned) | Ctrl+W, Ctrl+F4 | W | P0 | R |
| `tab.reopen` | Reopen last closed | Ctrl+Shift+T | W | P0 | R |
| `tab.pin_toggle` | Pin/unpin | Ctrl+D | W | P0 | P |
| `tab.copy_url` | Copy URL (cleaned) | Ctrl+Shift+C | W | P0 | P |
| `tab.copy_url_md` | Copy URL as Markdown | Ctrl+Shift+Alt+C | W | P0 | P |
| `tab.goto_n` | Go to sidebar item 1–8 / last | Ctrl+1…Ctrl+9 | W/C | P0 | P |
| `tab.prev` / `tab.next` | Previous/next item | Ctrl+Alt+↑ / ↓ (also Ctrl+PgUp/PgDn) | W/A | P0 | R |
| `tab.mru_switch` | Recent-tab switcher | Ctrl+Tab / Ctrl+Shift+Tab | W | P0 | R (+keyup) |
| `tabs.clear_today` | Archive all Today tabs | Ctrl+Shift+K | W | P0 | R |
| `space.goto_n` | Go to Space N | Alt+1…Alt+9 | W | P0 | P |
| `space.prev` / `space.next` | Previous/next Space | Ctrl+Alt+← / → | W | P0 | R |
| `sidebar.toggle` | Show/hide sidebar | Ctrl+S | W | P0 | P |
| `split.add` | Add split pane | Ctrl+Shift+= (also Ctrl+Shift+Num+) | W | P0 | R |
| `split.close` | Separate focused pane | Ctrl+Shift+- | W | P0 | R |
| `split.focus_n` | Focus pane N | Ctrl+Shift+1…4 | W | P0 | R |
| `split.focus_prev/next` | Focus prev/next pane | Ctrl+Shift+[ / ] | M | P0 | R |
| `peek.expand` | Open Peek as tab | Ctrl+O | W | P0 | X (only while Peek open, else no-op) |
| `peek.close` | Close Peek | Esc, Ctrl+W | W | P0 | X |
| `nav.back` / `nav.forward` | History back/forward | Alt+← / Alt+→, mouse X1/X2 over content | W | P0 | P |
| `nav.reload` | Reload | Ctrl+R, F5 | W | P0 | P |
| `nav.hard_reload` | Reload ignoring cache | Ctrl+Shift+R, Ctrl+F5, Shift+F5 | C | P0 | P |
| `nav.stop` | Stop loading | Esc (while loading, page focused) | C | P0 | P |
| `page.find` | Find in page | Ctrl+F | W | P0 | P |
| `page.find_next/prev` | Next/previous match | F3 / Shift+F3, Ctrl+G / Ctrl+Shift+G | W | P0 | X (find bar) + P |
| `page.zoom_in/out/reset` | Zoom | Ctrl+= , Ctrl+Numpad+, Ctrl+-, Ctrl+Numpad-, Ctrl+0, Ctrl+wheel | W | P0 | P |
| `page.devtools` | DevTools | Ctrl+Shift+I, F12 | W | P0 | R |
| `page.print` | Print | Ctrl+P | W | P0 | P |
| `page.view_source` | View source | Ctrl+U | W | P0 | P |
| `page.save_as` | Save page as | Ctrl+Shift+S | M/A | P1 | P |
| `view.history` | History page | Ctrl+H | W | P1 | P |
| `view.downloads` | Downloads popover | Ctrl+J | W | P0 | P |
| `view.settings` | Settings | Ctrl+, | W | P0 | P |
| `app.menu` | App menu | Alt+F | W | P0 | P |
| `window.new` | New window | Ctrl+N | W | P1 | R |
| `window.new_incognito` | New incognito window | Ctrl+Shift+N | W | P1 | R |
| `window.close` | Close window | Ctrl+Shift+W, Alt+F4 | C | P0 | R |
| `window.fullscreen` | Toggle fullscreen | F11, Alt+Shift+F | W (both in Arc-Win release notes) | P0 | R |
| `privacy.clear_data` | Clear browsing data | Ctrl+Shift+Delete | C | P1 | R |
| `sidebar.undo` (VERIFIED-FIX: added) | Undo last archive/move | Ctrl+Z (only while the sidebar has focus; never steal it from pages) | W | P1 | X |
| `page.exit_fullscreen` (VERIFIED-FIX: added) | Exit page fullscreen | Esc (while in HTML5 fullscreen) | C | P0 | X (via `on_pre_key_event`, §2.26) |

**Conflicts, noted and accepted:**
- Ctrl+S (page save) and Ctrl+Shift+C (Chromium's inspect-element shortcut) are taken by Arc; we follow Arc.
- Ctrl+S, Ctrl+D and Alt+digits are **page-first**, so web apps that handle them (Google Docs Ctrl+S, etc.) keep working. ~~Arc's precedence here is undocumented; this is [A].~~
  - VERIFIED-FIX: Arc-Win documents it for two keys: "When Control-S and Control-Shift-C conflict with a website's keyboard shortcut we'll give you the chance to run the Arc keyboard shortcut instead of the website's". So Ctrl+S and Ctrl+Shift+C are page-first. When the page consumes one (the page's `keydown` called `preventDefault`, so the accelerator did not fire), show a 4s toast "Ctrl+S was used by this site · Toggle sidebar instead".
  - Detecting that needs `on_pre_key_event` to note the keydown, plus the missing `on_accelerator` call. That's unverified; P1.
- VERIFIED-FIX: **zoom vs split conflict.** On US layouts "Ctrl++" *is* Ctrl+Shift+= (VK_OEM_PLUS 0xBB + Shift), which Arc-Win uses for Add Split. The zoom row therefore must not register Shift+VK_OEM_PLUS. Register zoom-in on Ctrl+VK_OEM_PLUS (no Shift) and Ctrl+VK_ADD (0x6B). Non-US layouts where "+" is unshifted need a prototype check.
- VERIFIED-FIX: **Alloy style ships none of Chromium's browser accelerators.** cef_browser_view.h describes built-in accelerators as registered "internally for standard accelerators supported by Chrome style". It is unverified whether Ctrl+wheel zoom works in Alloy; if not, handle it with `set_zoom_level`. Every row in this table, including Ctrl+F/P/U, zoom, F5, Ctrl+Shift+I and F12, must be registered via `set_accelerator` and implemented by sta. Arc's help-center line "You can edit or remap keyboard shortcuts in Arc Settings" means remapping is Arc behavior; it stays a non-goal here (§7).
- Command Bar-internal keys: ↑/↓ select, Enter commit, Alt+Enter background tab, Tab shows actions or accepts a keyword, Ctrl+Backspace deletes a word, Esc closes.

---

## 4. Data model

Persistence: SQLite (WAL) at `%LOCALAPPDATA%\sta\User Data\sta.db`. CEF cache per profile: `…\User Data\Profiles\<profile_id>\` (the RequestContext cache path).

- **IDs:** UUIDv7 stored as `TEXT` (time-sortable).
- **Timestamps:** `i64` Unix milliseconds (UTC).
- **Ordering:** `sort_key: String` using fractional indexing (lexorank), so moving an item only rewrites that item.

```rust
// ---- core ----
struct Profile {
    id: Id, name: String, emoji: Option<String>,
    search_engine_id: String,            // "google" | "bing" | "duckduckgo" | "ecosia" | "brave" | "kagi" | "perplexity" | custom
    archive_after: ArchiveAfter,         // H12 (default) | H24 | D7 | D30
    download_dir: Option<PathBuf>, ask_download_location: bool,
    created_at: i64,
}
enum ArchiveAfter { H12, H24, D7, D30 }

struct Space {
    id: Id, profile_id: Id, name: String, icon_emoji: String,
    theme: SpaceTheme, sort_key: String,
    last_active_item_id: Option<Id>, pinned_collapsed: bool, created_at: i64,
}
struct SpaceTheme {
    kind: ThemeKind,                     // Solid | Gradient
    stops: Vec<Oklch>,                   // 1..=3 user-picked colors (L, C, H)
    angle_deg: u16,                      // gradient angle, default 160
    noise: f32,                          // 0.0..=0.2 grain opacity
    appearance: Appearance,              // System | Light | Dark (per-space override; default System) — VERIFIED-FIX: [A] deviation; Arc's light/dark choice "applies to all of your Spaces" (Spaces article). P1; MVP uses global Settings.appearance only
}

enum Section { Favorites, Pinned, Today }

struct SidebarItem {                     // common row record (single table, polymorphic)
    id: Id,
    profile_id: Id,                      // Favorites are scoped by profile (Arc: favorites per profile)
    space_id: Option<Id>,                // None iff section == Favorites
    section: Section,
    parent_folder_id: Option<Id>,        // only in Pinned
    sort_key: String,
    kind: ItemKind,                      // Tab(Tab) | Folder(Folder) | Split(SplitGroup)
    created_at: i64,
}

struct Tab {
    url: String,                         // current (last committed) URL
    title: String, custom_title: Option<String>,
    favicon_url: Option<String>, favicon_hash: Option<String>,   // blob cached in favicons table
    pinned_url: Option<String>,          // Some for Pinned/Favorite
    last_active_at: i64,                 // drives auto-archive + MRU restore
    opener_item_id: Option<Id>,
    zoom_level: Option<f64>,             // per-tab override; default per-host table
    muted: bool,
    // runtime only (not persisted): is_loaded, browser_id, is_loading, progress, can_go_back/forward, audible
}
struct Folder { name: String, collapsed: bool }
struct SplitGroup {
    orientation: Orientation,            // Horizontal | Vertical
    panes: Vec<SplitPane>,               // 2..=4, each pane is a Tab (not a SidebarItem)
    focused_index: u8,
}
struct SplitPane { tab: Tab, fraction: f32 }   // fractions sum to 1.0

// ---- archive / reopen ----
struct ArchiveEntry {
    id: Id, profile_id: Id, space_id: Option<Id>,
    url: String, title: String, favicon_hash: Option<String>,
    archived_at: i64, reason: ArchiveReason,     // Auto | UserClosed | ClearToday | SpaceDeleted | FolderDeleted
    original_section: Section, original_parent_folder_id: Option<Id>, original_sort_key: String,
    group_snapshot: Option<SplitSnapshot>,       // restores split groups
}
enum ReopenEntry {                                // per window, in-memory + persisted (cap 25)
    Archived(Id), ReopenPinned { item_id: Id, url_at_close: String },
    ClearToday(Vec<Id>), SplitClosed(Vec<Id>),
}

// ---- history ----
struct HistoryUrl {
    id: i64, profile_id: Id, url: String, title: String,
    visit_count: u32, typed_count: u32, last_visit_at: i64,
    frecency: i32,                       // recomputed on visit (see §6.3)
}
struct Visit { id: i64, url_id: i64, visited_at: i64, transition: Transition, item_id: Option<Id> }
enum Transition { Link, Typed, Pinned, Reload, Redirect, FormSubmit, Other }

// ---- misc ----
struct Boost { id: Id, host: String, name: String, enabled: bool, css: String, js: String, created_at: i64, updated_at: i64 }
struct Download { id: Id, profile_id: Id, url: String, path: PathBuf, mime: String,
                  total_bytes: Option<i64>, received_bytes: i64, state: DlState, started_at: i64, ended_at: Option<i64> }
struct HostZoom { profile_id: Id, host: String, zoom_level: f64 }

struct WindowState {
    id: Id, bounds: Rect, maximized: bool, active_space_id: Id,
    sidebar_docked: bool, sidebar_width: u16,
    mru: Vec<Id>,                        // item ids, cap 50
}
struct Settings {                        // app-global; per-profile bits live on Profile
    appearance: Appearance,              // System (default) | Light | Dark
    startup: Startup,                    // RestoreSession (default) | NewTab
    peek_from_pinned: bool,              // true
    peek_on_shift_click: bool,           // true (P1)
    hover_reveal_delay_ms: u16,          // 120
    show_top_strip_url_when_hidden: bool,// true
    default_browser_prompted: bool,
    telemetry: bool,                     // VERIFIED-FIX: was `telemetry: false` (not a type); always false, no telemetry
}
```
Invariants:
1. `section == Favorites` implies `space_id == None`, `parent_folder_id == None`, and `kind == Tab` with `pinned_url` set; count ≤ 12 per profile.
2. `section == Pinned` implies `pinned_url` is `Some` on every Tab, including tabs inside SplitGroup panes when the group is pinned (P1).
3. Folders exist only in `Pinned`.
4. `Today` tabs have `pinned_url == None`.

---

## 5. Visual design tokens

These tokens are original values meant to evoke Arc's look: a soft tinted frame, a floating content card, and a quiet sidebar. They do not copy Arc assets. Chromium renders them as CSS custom properties in all `sta://` UI. Values for native views are mirrored in Rust.

```css
:root {
  /* typography: Windows 11 variable font with optical sizes */
  --font-ui: "Segoe UI Variable Text", "Segoe UI Variable", "Segoe UI", system-ui, sans-serif;
  --font-display: "Segoe UI Variable Display", "Segoe UI Variable", "Segoe UI", sans-serif;
  --font-small: "Segoe UI Variable Small", "Segoe UI Variable", "Segoe UI", sans-serif;
  --font-mono: "Cascadia Mono", "Consolas", monospace;
  --font-emoji: "Segoe UI Emoji";
  --fs-tab: 13px;  --fw-tab: 400;  --fw-tab-active: 500;
  --fs-space-title: 13px; --fw-space-title: 600;
  --fs-url-pill: 12.5px; --fw-url-pill: 500;
  --fs-caption: 11px; /* use with --font-small (VERIFIED-FIX: "11px (font-small)" was invalid CSS) */  --fs-cmd-input: 17px; --fs-cmd-title: 14px; --fs-cmd-sub: 12px;
  --lh-tight: 1.25;

  /* geometry */
  --sidebar-w-default: 248px; --sidebar-w-min: 200px; --sidebar-w-max: 440px;
  --sidebar-pad-x: 8px;          /* rows inset from sidebar edges */
  --row-h: 36px; --row-pad-x: 10px; --row-gap: 2px; --row-radius: 8px;
  --favicon: 16px; --favicon-gap: 10px;
  --row-close-btn: 22px;         /* hover-only, right aligned, 12px glyph */
  --topbar-h: 40px;              /* sidebar top row == top strip height */
  --url-pill-h: 36px; --url-pill-radius: 10px;
  --space-title-h: 32px; --folder-row-h: 32px; --section-gap: 8px;
  --fav-tile-h: 48px; --fav-tile-min-w: 52px; --fav-gap: 8px; --fav-radius: 10px; --fav-icon: 20px;
                                  /* columns = clamp(floor((w-16+8)/(52+8)), 3, 6) → 4 at 248px */
  --bottom-bar-h: 44px; --space-icon: 28px; --space-icon-gap: 4px;
  --frame-gap: 8px;              /* gap around the content card (right, bottom; left when sidebar hidden) */
  --content-radius: 8px;         /* aspirational; see §8.2 (MVP: 0 on the native BrowserView) */
  --split-gutter: 6px; --split-ring: 2px;
  --caption-btn-w: 46px;
  --scrollbar-w: 6px;            /* overlay-style thin scrollbar in sidebar, visible on hover */

  /* command bar */
  --cmd-w: clamp(480px, 56%, 680px);  /* of content width */
  --cmd-top: max(72px, 14%);          /* of content height, from content top */
  --cmd-input-h: 56px; --cmd-row-h: 44px; --cmd-row-h-compact: 36px; --cmd-group-h: 26px;
  --cmd-max-rows: 8; --cmd-radius: 12px; --cmd-pad: 8px;

  /* motion */
  --ease-out: cubic-bezier(0.16, 1, 0.3, 1);
  --ease-in-out: cubic-bezier(0.65, 0, 0.35, 1);
  --t-hover: 90ms; --t-press: 60ms; --t-popover: 140ms; --t-cmd-in: 140ms; --t-cmd-out: 90ms;
  --t-space-slide: 220ms; --t-theme-fade: 300ms; --t-sidebar-reveal: 160ms; --t-row-insert: 180ms;
  --t-toast-in: 180ms; --t-drag-lift: 120ms;
}
```

**Color system (per Space):** the theme input is 1–3 OKLCH stops. Derived tokens:

| Token | Light | Dark |
|---|---|---|
| `--bg-grad` | `linear-gradient(angle, oklch(0.93 c h1), oklch(0.89 c h2))`, c = min(userC, 0.07) | `linear-gradient(angle, oklch(0.25 c h1), oklch(0.20 c h2))`, c = min(userC, 0.06) |
| `--frame` (solid, for native panels) | mix of the stops at 60% along the gradient | same (dark) |
| `--noise-opacity` | theme.noise (0–0.12) | theme.noise × 0.6 |
| `--text` | `#1C1B20` | `#F3F2F7` |
| `--text-2` (secondary) | `rgb(28 27 32 / 0.62)` | `rgb(243 242 247 / 0.62)` |
| `--hover` | `rgb(0 0 0 / 0.05)` | `rgb(255 255 255 / 0.07)` |
| `--pressed` | `rgb(0 0 0 / 0.09)` | `rgb(255 255 255 / 0.11)` |
| `--active-row` | `rgb(255 255 255 / 0.78)` + `0 1px 2px rgb(0 0 0 / 0.06)` | `rgb(255 255 255 / 0.13)` |
| `--accent` | `oklch(0.58 0.15 h1)` | `oklch(0.74 0.13 h1)` |
| `--divider` | `rgb(0 0 0 / 0.08)` | `rgb(255 255 255 / 0.08)` |
| `--surface` (command bar, menus, Peek header) | `#FFFFFF` | `#232228` |
| `--surface-border` | `rgb(0 0 0 / 0.10)` | `rgb(255 255 255 / 0.10)` |
| `--focus-ring` | 2px `--accent`, 2px offset | same |

**Preset swatches** (h1/h2 hue, C). All are original values.

| Preset | h1 | h2 | C |
|---|---|---|---|
| Dusk | 300 | 340 | 0.06 |
| Lagoon | 190 | 230 | 0.06 |
| Ember | 50 | 20 | 0.07 |
| Moss | 130 | 150 | 0.05 |
| Slate | 255 | 255 | 0.015 |
| Rose | 10 | 350 | 0.06 |
| Sky | 245 | 275 | 0.06 |
| Sand | 80 | 60 | 0.05 |

**Light/dark:**
- `Appearance::System` follows `prefers-color-scheme` in the HTML views. Rust watches `WM_SETTINGCHANGE` ("ImmersiveColorSet") and updates native panel colors and the DWM caption color.
- A per-Space override lets a Space stay dark in light mode. VERIFIED-FIX: this is **[A], not Arc**. The Arc Spaces article says the Light/Dark/Automatic choice "applies to all of your Spaces, not just the one you're currently in". Moved to P1.
- Theme changes cross-fade over `--t-theme-fade`.

**States:**
- Hover: `--hover` background, `--t-hover`.
- Pressed: `--pressed`, scale 0.98 for 60ms (favorite tiles only).
- Active tab row: `--active-row` + `--fw-tab-active`.
- Keyboard focus: `--focus-ring`.
- Unloaded: text 60% opacity.
- Dragging row: 0.9 opacity, lift shadow `0 6px 16px rgb(0 0 0 / 0.18)` (inside sidebar HTML only).
- Loading: favicon replaced by a 14px spinner (1.5px stroke, `--accent`, 800ms/rev).
- Audible: 12px speaker glyph before the close button.

**Favicons:**
- Rows use 16px (request 32px for HiDPI).
- Favorites use 20px on a 48px tile (request the 48px icon).
- If no favicon exists, draw a 16px rounded square in `oklch(0.7 0.08 hash(host))` with the host's first letter in 10px/600.

**Command Bar visuals:**
- Surface: `--surface`, 1px `--surface-border`, radius 12px.
- Input row: 56px, 20px leading search/globe icon, placeholder "Search or enter URL…".
- Result row: 44px with favicon/icon 16px, title 14px, subtitle 12px `--text-2` (host or type), right-aligned hint chip ("Switch to Tab", "↵", shortcut text) in 11px `--font-mono`.
- Selected row: `--accent` at 12% alpha background plus a 3px accent bar on the left edge.
- Group header: 26px, 11px/600 `--text-2`, letter-spacing 0.02em.
- Open animation (HTML only): opacity 0→1 and translateY(−6px→0) over `--t-cmd-in`. Close over `--t-cmd-out`.
- No drop shadow (§8.2). The border does the separation work.
- VERIFIED-FIX: the Command Bar card sits over web content, so `--cmd-radius: 12px` has the same corner problem as §8.2 (radius 0 in the MVP). `--cmd-w`/`--cmd-top` percentages can't be resolved in the overlay's HTML, because the overlay *is* the card. Rust must compute the overlay bounds from the content-frame size and push them with `set_bounds`.

---

## 6. Command Bar spec

### 6.1 Modes

| Mode | Opened by | Prefill | Enter on URL/search | Empty-query list |
|---|---|---|---|---|
| `NewTab` | Ctrl+T, "+ New Tab", pill when no tab | "" | New Today tab (top) | "Recent tabs" (MRU 1..6 open tabs), "Suggested actions" (New Space, View Archive, Toggle sidebar, Settings) |
| `EditUrl` | Ctrl+L, Alt+D, F6, pill click | full URL, selected | Navigate current tab/pane | same as NewTab + "Copy URL" action first |
| `Split` | Ctrl+Shift+= | "" | Open in new split pane | Recent tabs (choosing one moves it into the split) |
| `MoveToSpace` | "Move to Space…" | "" | – | List of Spaces |
| `Actions` | Tab on empty input, or type `>` prefix | ">" | – | All actions A–Z |

### 6.2 Result groups (display order) and caps

With a non-empty query, candidates are gathered from all sources, scored, and rendered in **fixed group order**. A single **Top hit** is promoted above all groups.

| Order | Group | Source | Cap | Row hint |
|---|---|---|---|---|
| 0 | **Top hit** | The max-scored candidate over all groups, but only if its score ≥ 0.85 × max(URL-what-you-typed score, 1.0); otherwise the what-you-typed row | 1 | depends |
| 1 | **Go / Search** (what-you-typed) | Heuristic §6.4: "Go to *example.com*" or "Search Google for "*q*"", plus inline autocompletion (§6.3) | 1 | ↵ |
| 2 | **Tabs** | Open or unloaded tabs in all Spaces, Favorites, Pinned | 4 | "Switch to Tab" (open), "Open" (unloaded), plus the Space emoji |
| 3 | **Actions** | §6.5 registry | 3 | shortcut chip |
| 4 | **Spaces** | Space names | 2 | "Go to Space" |
| 5 | **History** | HistoryUrl (dedupe against Tabs by normalized URL) | 4 | host · relative time |
| 6 | **Suggestions** | Remote search suggest (debounced 120ms, fetched in Rust, cancel on keystroke) | 4 | "Search" |
| 7 | **Archive** (P1) | ArchiveEntry | 2 | "Restore" |

Max 12 rows rendered. Empty groups are hidden.

### 6.3 Scoring

**Fuzzy match** `fz(query, text) ∈ [0,1]` (fzf-like subsequence), computed over the lowercased NFKD-folded title, host, and path. Bonuses:

| Match kind | Bonus |
|---|---|
| Exact full match | +1.0 |
| Prefix of the text | +0.6 |
| Prefix of a word (after `space . / - _`) | +0.35 |
| Each consecutive run char | +0.08 |
| Gap penalty (per skipped char, max −0.3) | −0.02 |

Normalize by the ideal score for the query length. A candidate matches if `fz ≥ 0.35`. The fields are weighted `max(title × 1.0, host × 1.1, path × 0.6)`.

**Candidate score:**
```
score = fz * group_weight + boosts
group_weight: tabs 1.00, actions 0.95 (1.10 if query starts with ">"), spaces 0.90, history 0.85, suggestions 0.60, archive 0.55
boosts:
  tabs:     +0.15 if in current space; +0.10 * mru_recency (1.0 for MRU[1] decaying 0.8^i); +0.05 if favorite
  history:  +0.25 * normalize(frecency)
  actions:  +0.10 if alias exactly equals query token
```

**Frecency** (Firefox-style, recomputed on visit and nightly):

`frecency = visit_count_factor × Σ over the last 10 visits of bucket(age) × transition_bonus`

| Visit age | bucket | Transition | bonus |
|---|---|---|---|
| ≤4d | 100 | Typed | 2.0 |
| ≤14d | 70 | Link | 1.0 |
| ≤31d | 50 | Pinned | 1.2 |
| ≤90d | 30 | Redirect | 0 |
| older | 10 | Reload | 0 |

`visit_count_factor = visit_count / min(visit_count, 10)`.

**Inline autocompletion** (what-you-typed row):
1. If the query (with no spaces) is a prefix of a history host with `typed_count ≥ 1` or `frecency ≥ 200` (ignoring `www.`), complete to that host.
2. The completed tail is selected, so typing overwrites it.
3. Only the host is completed, never a path, unless the user already typed a `/`.
4. **Backspace** removes the completion.

### 6.4 URL vs search heuristic

```
fn classify(input) -> Go(url) | Search(q):
  s = input.trim()
  if s.starts_with('?')                         -> Search(s[1..])           // force search
  if has_scheme(s, [http, https, file, sta, about, chrome, view-source, data, mailto, ftp])
                                                -> Go(s)
  if keyword_mode_active (e.g. "yt"+Tab, P1)    -> Search via that engine
  if s contains whitespace                      -> Search(s)
  if s == "localhost" or s matches ^localhost(:\d+)?([/?#].*)?$  -> Go("http://" + s)
  if s is IPv4 (a.b.c.d with optional :port/path) or [IPv6]       -> Go("http://" + s)
  if s matches ^[^\s/?#@]+(:\d{1,5})?([/?#].*)?$ and host part contains '.' :
       tld = last label of host (lowercase, punycode)
       if tld in PUBLIC_SUFFIX_LIST or explicit port or path present -> Go("https://" + s)
  if s matches ^[\w.+-]+@[\w-]+\.[\w.]+$        -> Search(s)                 // emails are searches
  if Ctrl+Enter                                 -> Go("https://www." + s + ".com")   // Chromium convention
  else                                          -> Search(s)
```
- **HTTPS-first** [A]: typed hosts without a scheme get `https://`. If that fails with a connection or SSL error (not a certificate *warning*), show an interstitial with a "Continue to http://" button. Do not downgrade automatically.
- `sta://` pages are internal. Web content may not navigate to them (block in `on_before_browse` unless the source is internal).

### 6.5 Search engines

| id | Search URL | Suggest endpoint (unofficial; must be verified; fetched from Rust) |
|---|---|---|
| `google` (default) | `https://www.google.com/search?q={q}` | `https://suggestqueries.google.com/complete/search?client=firefox&q={q}` (OpenSearch JSON) |
| `bing` | `https://www.bing.com/search?q={q}` | `https://api.bing.com/osjson.aspx?query={q}` |
| `duckduckgo` | `https://duckduckgo.com/?q={q}` | `https://duckduckgo.com/ac/?q={q}&type=list` |
| `ecosia` | `https://www.ecosia.org/search?q={q}` | `https://ac.ecosia.org/autocomplete?q={q}&type=list` |
| `brave` | `https://search.brave.com/search?q={q}` | `https://search.brave.com/api/suggest?q={q}` |
| `kagi` | `https://kagi.com/search?q={q}` | `https://kagi.com/api/autosuggest?q={q}` |
| `perplexity` | `https://www.perplexity.ai/search?q={q}` | none |
| custom | user template containing `{q}` | optional |

~~Arc's engine list includes Google, Bing, DuckDuckGo, Ecosia, Perplexity and Kagi [Arc].~~ The engine is chosen per Profile [Arc-Win].
- VERIFIED-FIX: the cited article (25614032197783) lists **no** engines.
  - Verified: Perplexity is a built-in option (Arc-Win notes: "Set a default search engine including support for Perplexity AI as default").
  - Arc's "Can You Add a New Default Search Engine Option" article (25619122951063) uses **Ecosia as the example of adding a custom engine** through Chromium "Site search" settings (`%s` template). So Ecosia is probably *not* built in, and Kagi is unverified.
  - The rest of the list is Chromium's regional defaults. The sta list above is [A].

- **"Search suggestions"** setting defaults to on. When off, no network requests are made while typing.
- **Encoding:** `{q}` is `encodeURIComponent(q)`.

### 6.6 Actions registry (≥25)

Each action: `id`, title, aliases (fuzzy-matched), shortcut, and an availability predicate.

| id | Title (aliases) | Shortcut | Available when |
|---|---|---|---|
| `tab.new` | New Tab | Ctrl+T | always |
| `tab.close` | Close Tab / Archive Tab | Ctrl+W | active tab |
| `tab.reopen` | Reopen Closed Tab (undo close) | Ctrl+Shift+T | stack non-empty |
| `tab.pin_toggle` | Pin Tab / Unpin Tab | Ctrl+D | active tab |
| `tab.favorite_toggle` | Add to Favorites / Remove from Favorites (unfavorite) | – | active tab; add only if < 12 |
| `tab.reset_pinned` | Reset Tab (back to pinned URL) | click favicon | pinned/fav & navigated |
| `tab.replace_pinned_url` | Replace Pinned URL with Current | – | pinned/fav & navigated |
| `tab.rename` | Rename Tab | F2 (sidebar focus) | active tab |
| `tab.duplicate` | Duplicate Tab | – | active tab |
| `tab.copy_url` | Copy URL (copy link) | Ctrl+Shift+C | active tab |
| `tab.copy_url_md` | Copy URL as Markdown | Ctrl+Shift+Alt+C | active tab |
| `tab.move_to_space` | Move Tab to Space… | – | ≥2 spaces |
| `tab.mute_toggle` (P1) | Mute Tab / Unmute Tab | – | audible or muted |
| `tab.unload` (P1) | Unload Tab (free memory) | – | loaded & not active |
| `tabs.clear_today` | Clear Today Tabs (archive unpinned) | Ctrl+Shift+K | Today non-empty |
| `folder.new` | New Folder | – | always |
| `space.new` | New Space | – | always |
| `space.goto` | Go to Space: {name} | Alt+1..9 | per space |
| `space.next` / `space.prev` | Next Space / Previous Space | Ctrl+Alt+→ / ← | ≥2 spaces |
| `space.rename` | Rename Space | – | always |
| `space.theme` | Change Space Theme (color) | – | always |
| `space.icon` | Change Space Icon (emoji) | – | always |
| `space.delete` | Delete Space | – | ≥2 spaces |
| `split.add_right` / `left` / `top` / `bottom` | Add Right/Left/Top/Bottom Split | Ctrl+Shift+= (right) | active tab, < 4 panes |
| `split.separate_all` | Separate All Tabs (unsplit) | – | split active |
| `split.close_pane` | Remove Pane from Split | Ctrl+Shift+- | split active |
| `sidebar.toggle` | Toggle Sidebar (hide/show) | Ctrl+S | always |
| `view.archive` | View Archive | – | always |
| `archive.clear` | Clear Archive | – | archive non-empty |
| `view.downloads` | Show Downloads | Ctrl+J | always |
| `view.history` (P1) | Show History | Ctrl+H | always |
| `view.settings` | Open Settings (preferences) | Ctrl+, | always |
| `page.find` | Find in Page | Ctrl+F | active tab |
| `page.zoom_in/out/reset` | Zoom In / Zoom Out / Actual Size | Ctrl+= / Ctrl+- / Ctrl+0 | active tab |
| `page.reload_hard` | Hard Reload (clear cache reload) | Ctrl+Shift+R | active tab |
| `page.devtools` | Developer Tools (inspect) | Ctrl+Shift+I | active tab |
| `page.print` | Print… | Ctrl+P | active tab |
| `page.view_source` | View Page Source | Ctrl+U | active tab |
| `boost.new` | New Boost for this Site (custom css js) | – | http(s) tab |
| `boost.toggle` | Disable/Enable Boost for this Site | – | boost matches |
| `theme.appearance` | Appearance: Light / Dark / System | – | always |
| `privacy.clear_data` (P1) | Clear Browsing Data… | Ctrl+Shift+Delete | always |
| `app.set_default` | Make sta Default Browser | – | not default |
| `window.new` (P1) | New Window | Ctrl+N | always |
| `app.quit` | Quit sta (exit) | – | always |

---

## 7. Explicit non-goals for MVP

- Account, sync, sharing Spaces/Folders, cloud anything. No telemetry.
- Chrome Web Store extensions. ~~Alloy style has only partial extension support~~, so extensions are not planned before post-P2.
  - VERIFIED-FIX: CEF 152 has no *Alloy extension API* (`include/` has no `LoadExtension`/`CefExtension`; the CEF M125 announcement: "The Alloy extension API is not supported … The Chrome extension API is supported with Chrome style browsers/windows only"), **but the Chrome bootstrap runs Chromium's own extension system process-wide**. Extensions install from the Web Store and work in sta's Alloy tabs: service workers, content scripts, `declarativeNetRequest`/`webRequest`, extension pages, `tabs.get/update` with a tab id (`docs/research/extensions.md`, VERIFIED with real extensions).
  - What Alloy style has no place for is the extension **UI and window model**: toolbar actions, shortcuts, side panels and menu items, and `tabs.query`/`windows.*` never see Alloy tabs. Everything Chromium wants a Chrome window for (post-install page, `windows.create`, `openOptionsPage`) is hidden and adopted by `foreign.rs` (ARCHITECTURE §4.5). **sta's own extension UI is built on that** (ARCHITECTURE §4.6, shipped): Ctrl+E lists what is installed, a popup page runs in an Alloy BrowserView inside an sta card (a popup that needs the current tab still cannot work, and the card says so), and turning extensions on or off goes through a hidden Chrome-style `chrome://extensions` window — not through Chrome-style tabs, which a Chrome-style Window limits to one BrowserView.
- Easels, Notes, Arc Max/AI features, Live Folders, calendar live icons, Air Traffic Control, Little Arc windows.
- Safe Browsing, dangerous-download verdicts, password manager, autofill UI beyond Chromium defaults, and passkey UI. Chromium defaults that CEF provides still work.
  - VERIFIED-FIX: in **Alloy** style those "defaults" mostly don't exist. CEF's architecture doc credits the Chrome runtime with "many advanced features (including datalist/autofill, extensions, gamepad, webhid, webmidi, etc.), along with standard Chrome UI toolbars and dialogs for device selection, user permissions, settings".
  - Expect no password save, no autofill, no `<datalist>` dropdown, and no WebHID/WebUSB/Bluetooth device choosers. Permission prompts are now a P0 item (§2.25). Test `<input type=date>` / `<select>` popups early.
- Visual Boost editor (color wheel, font picker, Zap), sharing Boosts, Boosts across multiple domains.
- Mica/Acrylic window translucency. Opaque views hide it (§8.5).
- Real translucent or blurred overlays and drop shadows over web content (§8.2). VERIFIED-FIX: narrowed. *Blur* and translucent *BrowserView* overlays are non-goals. A flat translucent scrim from a non-browser Panel overlay looks feasible (§8.2) and is P1 pending a prototype.
- Page thumbnails in the Ctrl+Tab switcher (P2 via CDP `Page.captureScreenshot`).
- Mixed or nested split layouts, more than 4 panes, pinned split groups (P1), dragging tabs out to new windows.
- Multiple windows, incognito, and multiple profiles (all P1; the data model is ready).
- Restoring back/forward history of tabs after restart (not supported by CEF).
- Right-side sidebar, compact "no top strip" mode, custom keyboard remapping. ~~Arc-Win has no remapping either.~~ VERIFIED-FIX: refuted. Arc's Keyboard Shortcuts article, covering macOS and Windows, says "You can edit or remap keyboard shortcuts in Arc Settings." Remapping stays a non-goal for sta [A].
- macOS/Linux builds. Cross-platform note: shortcuts would map Ctrl→Cmd per the Arc-mac table, and caption buttons move left on macOS.
- Touch-first UI, pen, accessibility beyond keyboard navigation, focus rings, and ARIA roles in the HTML UI. Keyboard, focus rings and ARIA ARE in scope.

---

## 8. CEF implementation constraints that shape the UX

All Rust signatures are copied verbatim from the bindings (line numbers given). Header quotes are from `.cef/152.0.6/cef_windows_x86_64/include/`.

### 8.1 View tree (recommended)

- **Window:** `is_frameless` returns 1. `window_runtime_style` returns `RuntimeStyle::ALLOY`.
- **Root:** a horizontal BoxLayout Panel containing:
  - the Sidebar BrowserView (`sta://sidebar`, fixed width)
  - a right column Panel (vertical BoxLayout) containing:
    - the TopStrip BrowserView (`sta://topstrip`, 40px)
    - the Content Panel (flex 1, background `--frame`, insets 0/8/8/0), containing either one tab BrowserView (the others are hidden with `set_visible(0)`) or a split Panel with wrapper panels
- **Overlays, added last and in z-order:**
  1. Peek (CUSTOM) — VERIFIED-FIX: `can_activate=1` (interactive page)
  2. Command Bar (CUSTOM, `can_activate=1`)
  3. Floating sidebar (CUSTOM) — VERIFIED-FIX: `can_activate=1` (rename, emoji picker); contents = wrapper Panel that the one sidebar BrowserView is moved into (§2.15)
  4. Toast (CUSTOM, `can_activate=0`)
  5. Switcher (CUSTOM, `can_activate=0` so Ctrl key-up keeps reaching the focused tab, §2.13)
  - VERIFIED-FIX additions:
    - Scrim (CUSTOM, plain Panel with ARGB background, `can_activate=0`), added **before** Peek (P1, §8.2).
    - Find bar and permission card (CUSTOM, `can_activate=1`), §2.25/§2.27.
    - Overlays are created once and toggled with `OverlayController::set_visible`; z-order is fixed by creation order.
- VERIFIED-FIX (missing, blocking): **every BrowserViewDelegate must return Alloy**: `impl ImplBrowserViewDelegate … fn browser_runtime_style(&self) -> RuntimeStyle` (L37708) → `RuntimeStyle::ALLOY`. cef_types_runtime.h: "Alloy style Windows with the Views framework can host only Alloy style BrowserViews". The DEFAULT style is rejected at `AddedToWidget` with "Cannot add Chrome style BrowserView to Alloy style Window" (browser_view_impl.cc, M152), and the browser is never created.
- VERIFIED-FIX (lifecycle):
  - The browser is created only when the BrowserView is added to a widget ("Top-level browsers will be created when this view is added to the views hierarchy").
  - Removing it keeps the browser alive while Rust holds a `BrowserView` reference. Dropping the last reference after removal destroys it (`~CefBrowserViewImpl` → `WindowDestroyed()`).
  - **Closing one tab ≠ closing the window.** cef_life_span_handler.h: with windowed rendering, "returning false from DoClose() will send the standard close notification to the browser's top-level parent window (… CefWindowDelegate::CanClose() callback from Views)". The default path therefore tries to close the **whole sta window**.
  - For "Unload"/archive (§2.2), call `close_browser(0)` so unload handlers run. Then in `impl ImplLifeSpanHandler … fn do_close(&self, browser: Option<&mut Browser>) -> ::std::os::raw::c_int` (L20751) return **1** for tab browsers, `remove_child_view` their BrowserView, and drop the Rust reference. That hierarchy tear-down completes the close, then `on_before_close` (L20755) fires.
  - Return 0 only during a real window close, where `can_close` calls `try_close_browser` for every browser.

Relevant APIs:
- `impl ImplWindowDelegate … fn is_frameless(&self, window: Option<&mut Window>) -> ::std::os::raw::c_int` (L43221). cef_window_delegate.h: "Return true if |window| should be created without a frame or title bar. The window will be resizable if CanResize() returns true. Use CefWindow::SetDraggableRegions() to specify draggable regions."
- `impl ImplWindow … fn set_draggable_regions(&self, regions: Option<&[DraggableRegion]>);` (L44316). The top strip and sidebar HTML report their `-webkit-app-region`-like rects to Rust over IPC. Rust then calls this with **window coordinates**. (VERIFIED-FIX: `ImplDragHandler::on_draggable_regions_changed` (L19155) delivers CSS `app-region` rects without custom IPC; see §2.1.)
- `impl ImplPanel … fn set_to_box_layout(&self, settings: Option<&BoxLayoutSettings>) -> Option<BoxLayout>;` (L41646).
- `impl ImplBoxLayout … fn set_flex_for_view(&self, view: Option<&mut View>, flex: ::std::os::raw::c_int);` (L37040). `BoxLayoutSettings` has `inside_border_insets: Insets` and `between_child_spacing` (L1395).
- `impl ImplPanel … fn add_child_view(&self, view: Option<&mut View>);` (L41652). Also `fn remove_child_view(&self, view: Option<&mut View>);` (L41658).
- `impl ImplView … fn set_visible(&self, visible: ::std::os::raw::c_int);` (L38365). `fn set_background_color(&self, color: u32);` (L38385).
- `impl ImplBrowserHost … fn was_hidden(&self, hidden: ::std::os::raw::c_int);` (L12653).

### 8.2 Overlays are opaque rectangles, so there are no shadows, dimming, or rounded web corners

> **UPDATE (implemented, ARCHITECTURE §4.4, `crates/sta/src/rounded.rs`):** rounded corners and
> soft shadows *are* possible without transparency in the browser: the overlay widget is
> translucent, so an overlay that holds `LabelButton` corner images (premultiplied BGRA with alpha),
> 1 DIP border panels and translucent shadow strips around an inset BrowserView draws a rounded card
> with a shadow (command bar, find bar, permission, switcher, toast, Peek, floating sidebar), and
> small image overlays over a pane's corners (frame color outside the arc, transparent inside)
> round the web content itself. The square-corner rule below is superseded; blur and dimming are
> still impossible.

- `impl ImplWindow … fn add_overlay_view(&self, view: Option<&mut View>, docking_mode: DockingMode, can_activate: ::std::os::raw::c_int) -> Option<OverlayController>;` (L44296). The controller has `fn set_bounds(&self, bounds: Option<&Rect>);` (L41194) and `fn set_visible(&self, visible: ::std::os::raw::c_int);` (L41214).
- cef_window.h: "Overlays created by this method will receive a higher z-order then any child Views added previously. It is therefore recommended to call this method last after all other child Views have been added". The same header says "Overlays are hidden by default."
- cef_types.h on `background_color`: "If the alpha component is fully transparent for a windowed browser then the default value of opaque white be used. If the alpha component is fully transparent for a windowless (off-screen) browser then transparent painting will be enabled."
- Open request **chromiumembedded/cef#4035** "views: Support transparent overlay BrowserViews": the current `GetColor` "requires windowed browsers to be fully opaque".
- **cef#3790** (overlay BrowserView not shown in CEF 125+) was closed as completed on 2024-10-17. The maintainer reported: "this appears to work with Alloy style browsers (tested M130)". → **Use Alloy style for overlay BrowserViews.**
- No View API for corner radius exists in `include/views/*.h` (grep finds only background-color APIs).
- VERIFIED-FIX (the "no dimming" conclusion is too strong): the opacity limit applies to **browser** content, not to overlays as such.
  - CEF M152 source `libcef/browser/views/overlay_view_host.cc` (branch 7977) L198: `params.opacity = views::Widget::InitParams::WindowOpacity::kTranslucent;` and L207–209: "Make the Widget background transparent. The View might still be opaque." → `SetBackgroundColor(SK_ColorTRANSPARENT)`.
  - `CefViewImpl::SetBackgroundColor` → `views::CreateSolidBackground(color)`, which keeps alpha.
  - So `impl ImplView … fn set_background_color(&self, color: u32);` (L38385) with e.g. `0x59000000` on a **Panel** overlay (no BrowserView inside) should composite over web content as a scrim. A Panel overlay that holds an inset BrowserView with a translucent border can fake a 1–3px hard shadow.
  - Blur, and translucency *inside* a BrowserView, are still impossible until cef#4035. **Prototype before designing around it**, and keep the opaque fallback.
- **UX consequences, already baked into §5:**
  - The Command Bar overlay bounds equal the card bounds exactly. Resize the bounds when the row count changes (quantized to whole rows) with `set_bounds`.
  - There is no scrim or shadow; separation comes from a 1px border.
  - Peek, Toast and Switcher are all opaque rectangles.
  - The HTML inside each overlay should paint its own background edge-to-edge in `--surface`. Rounded corners are visible only where the overlay sits over **native `--frame` color** (for example, a toast in the bottom gap), so default radius-cornered HTML surfaces must fill their corners with the underlying color. Practical rule: **give overlays square corners unless they are fully over the frame gap.**
  - **P2 option:** capture the page with CDP `Page.captureScreenshot` through `impl ImplBrowserHost … fn execute_dev_tools_method(&self, message_id: ::std::os::raw::c_int, method: Option<&CefString>, params: Option<&mut DictionaryValue>) -> ::std::os::raw::c_int;` (L12627). Render it dimmed and blurred as the background of a full-content-sized overlay, which gives a fake scrim and shadow.

### 8.3 Shortcuts: accelerators plus key-up for Ctrl+Tab

- `impl ImplWindow … fn set_accelerator(&self, command_id: ::std::os::raw::c_int, key_code: ::std::os::raw::c_int, shift_pressed: ::std::os::raw::c_int, ctrl_pressed: ::std::os::raw::c_int, alt_pressed: ::std::os::raw::c_int, high_priority: ::std::os::raw::c_int);` (L44331).
- `impl ImplWindowDelegate … fn on_accelerator(&self, window: Option<&mut Window>, command_id: ::std::os::raw::c_int) -> ::std::os::raw::c_int` (L43257).
- cef_window.h: "If |high_priority| is true then the key event will not be forwarded to the web content (`keydown` event handler) or CefKeyboardHandler first. If |high_priority| is false then the behavior will depend on the CefBrowserView::SetPreferAccelerators configuration." That maps to R vs P in §3.
- `impl ImplBrowserView … fn set_prefer_accelerators(&self, prefer_accelerators: ::std::os::raw::c_int);` (L39164). cef_browser_view.h: "If |prefer_accelerators| is false then the matching accelerator will only be triggered if the event is not handled by web content … The default value is false." Keep the default for web tabs.
- `impl ImplKeyboardHandler … fn on_pre_key_event(&self, browser: Option<&mut Browser>, event: Option<&KeyEvent>, os_event: Option<&mut MSG>, is_keyboard_shortcut: Option<&mut ::std::os::raw::c_int>) -> ::std::os::raw::c_int` (L20472). cef_keyboard_handler.h: "The methods of this class will be called on the UI thread." `KeyEvent` fields: `type_: KeyEventType`, `modifiers: u32`, `windows_key_code: c_int`, … (L1155).
  - Use it to detect **VK_CONTROL key-up** and commit the Ctrl+Tab switcher.
  - Also cancel on `impl ImplWindowDelegate … fn on_window_activation_changed(&self, window: Option<&mut Window>, active: ::std::os::raw::c_int)` (L43184).
- `impl ImplBrowserViewDelegate … fn on_gesture_command(&self, browser_view: Option<&mut BrowserView>, gesture_command: GestureCommand) -> ::std::os::raw::c_int` (L37700). Use it to consume back/forward gestures in the sidebar view.

```rust
use cef::*;
use std::sync::{Arc, Mutex};

pub mod cmd { pub const TAB_NEW: i32 = 1001; pub const TAB_CLOSE: i32 = 1002; pub const SIDEBAR_TOGGLE: i32 = 1010; pub const MRU_NEXT: i32 = 1020; pub const MRU_PREV: i32 = 1021; /* … */ }
struct Accel { id: i32, vk: i32, shift: bool, ctrl: bool, alt: bool, reserved: bool }
const ACCELS: &[Accel] = &[
    Accel { id: cmd::TAB_NEW,        vk: 0x54 /* T */,   shift: false, ctrl: true, alt: false, reserved: true  },
    Accel { id: cmd::TAB_CLOSE,      vk: 0x57 /* W */,   shift: false, ctrl: true, alt: false, reserved: true  },
    Accel { id: cmd::SIDEBAR_TOGGLE, vk: 0x53 /* S */,   shift: false, ctrl: true, alt: false, reserved: false },
    Accel { id: cmd::MRU_NEXT,       vk: 0x09 /* Tab */, shift: false, ctrl: true, alt: false, reserved: true  },
    Accel { id: cmd::MRU_PREV,       vk: 0x09,           shift: true,  ctrl: true, alt: false, reserved: true  },
];

pub struct Shell { /* store, views, overlays, mru switcher state … */ }
impl Shell {
    fn dispatch(&mut self, _command_id: i32) -> bool { true }
    fn switcher_active(&self) -> bool { false }
    fn commit_switcher(&mut self) {}
    fn cancel_switcher(&mut self) {}
}

wrap_window_delegate! {
    pub struct StaWindowDelegate {
        shell: Arc<Mutex<Shell>>,
    }

    // All three impl blocks are required by the macro's matcher (order: ViewDelegate, PanelDelegate, WindowDelegate).
    impl ViewDelegate {}
    impl PanelDelegate {}

    impl WindowDelegate {
        fn on_window_created(&self, window: Option<&mut Window>) {
            let Some(window) = window else { return };
            for a in ACCELS {
                window.set_accelerator(a.id, a.vk, a.shift as i32, a.ctrl as i32, a.alt as i32, a.reserved as i32);
            }
            // … build panels / browser views, add overlays last, window.show();
        }
        fn is_frameless(&self, _window: Option<&mut Window>) -> i32 { 1 }
        fn can_resize(&self, _window: Option<&mut Window>) -> i32 { 1 }
        fn on_accelerator(&self, _window: Option<&mut Window>, command_id: i32) -> i32 {
            self.shell.lock().unwrap().dispatch(command_id) as i32
        }
        fn on_window_activation_changed(&self, _window: Option<&mut Window>, active: i32) {
            if active == 0 { self.shell.lock().unwrap().cancel_switcher(); }
        }
        fn window_runtime_style(&self) -> RuntimeStyle { RuntimeStyle::ALLOY }
    }
}

wrap_keyboard_handler! {
    pub struct StaKeyboardHandler {
        shell: Arc<Mutex<Shell>>,
    }

    impl KeyboardHandler {
        fn on_pre_key_event(
            &self,
            _browser: Option<&mut Browser>,
            event: Option<&KeyEvent>,
            _os_event: Option<&mut cef::sys::MSG>,
            _is_keyboard_shortcut: Option<&mut ::std::os::raw::c_int>,
        ) -> ::std::os::raw::c_int {
            const VK_CONTROL: i32 = 0x11;
            if let Some(e) = event {
                if e.type_ == KeyEventType::KEYUP && e.windows_key_code == VK_CONTROL {
                    let mut s = self.shell.lock().unwrap();
                    if s.switcher_active() { s.commit_switcher(); }
                }
            }
            0 // never swallow; let the page see the key
        }
    }
}
// Hook it from every Client:  impl Client { fn keyboard_handler(&self) -> Option<KeyboardHandler> { Some(StaKeyboardHandler::new(self.shell.clone())) } }
// (ImplClient::keyboard_handler, L27883). `::new(...)` takes the struct fields in declaration order.
```
Macro notes:
- `wrap_*!` generates `Name::new(field1, field2, …) -> InterfaceType` with parameters in **field declaration order**.
- `wrap_window_delegate!` matches `impl ViewDelegate {…} impl PanelDelegate {…} impl WindowDelegate {…}`, in that order. The blocks may be empty but must be present, as in cefsimple.
- Methods you don't write keep the trait defaults (`Default::default()`, i.e. 0/None).
- Modifier bits in `KeyEvent::modifiers` are `cef::sys::cef_event_flags_t::EVENTFLAG_CONTROL_DOWN.0 as u32` (=4), `EVENTFLAG_SHIFT_DOWN` (=2), `EVENTFLAG_ALT_DOWN` (=8). These come from cef-dll-sys, re-exported as `cef::sys`.
- VERIFIED-FIX (re-entrancy hazard in the snippet above): every callback runs on the CEF UI thread, and many CEF calls made inside `dispatch()` re-enter delegates synchronously. Examples: `set_visible`/`request_focus` → `on_got_focus`/`on_window_activation_changed`; `add_child_view` → `on_layout_changed`; `close_browser` → `do_close`. If those callbacks also do `self.shell.lock().unwrap()`, a `std::sync::Mutex` **deadlocks** because it isn't re-entrant.
  - Either keep `Shell` in a `Rc<RefCell<…>>` and never call CEF while a borrow is held (collect "effects", drop the borrow, then apply), or
  - post re-entrant work with `post_task`/a queue.
  - `wrap_*!` fields only need `Clone` (the macro's `Clone` impl clones every field), so `Rc<RefCell<Shell>>` works on the single UI thread.
- VERIFIED-FIX (missing snippet): the BrowserView delegate every tab, sidebar and overlay needs. Modeled on cefsimple's `SimpleBrowserViewDelegate`; the macro requires `impl ViewDelegate {}` then `impl BrowserViewDelegate {}`.

```rust
wrap_browser_view_delegate! {
    pub struct StaBrowserViewDelegate {
        role: ViewRole,                 // #[derive(Clone, Copy)] enum ViewRole { Tab, Sidebar, TopStrip, Overlay }
    }

    impl ViewDelegate {}

    impl BrowserViewDelegate {
        // ImplBrowserViewDelegate::browser_runtime_style (L37708). REQUIRED: Alloy window hosts Alloy views only.
        fn browser_runtime_style(&self) -> RuntimeStyle { RuntimeStyle::ALLOY }

        // L37680: host popups ourselves (Peek / Today tab) instead of a new top-level Window.
        fn on_popup_browser_view_created(
            &self,
            _browser_view: Option<&mut BrowserView>,
            popup_browser_view: Option<&mut BrowserView>,
            is_devtools: i32,
        ) -> i32 {
            if is_devtools != 0 { return 0; }            // default CEF window for DevTools
            let Some(_popup) = popup_browser_view.cloned() else { return 0 };
            // shell.host_popup_in_peek(_popup):
            //   let mut view = View::from(&_popup);            // same conversion cefsimple uses
            //   peek_panel.add_child_view(Some(&mut view));  peek_overlay.set_visible(1);  view.request_focus();
            1
        }
    }
}
// Creation: `pub fn browser_view_create(client: Option<&mut Client>, url: Option<&CefString>, settings: Option<&BrowserSettings>, extra_info: Option<&mut DictionaryValue>, request_context: Option<&mut RequestContext>, delegate: Option<&mut BrowserViewDelegate>) -> Option<BrowserView>` (L59126)
// let mut d = StaBrowserViewDelegate::new(ViewRole::Tab);
// let bv = browser_view_create(Some(&mut client), Some(&CefString::from(url)), Some(&BrowserSettings::default()), None, None, Some(&mut d));
```
(`View::from(&browser_view)` then `add_child_view(Some(&mut view))` is copied from cefsimple `simple_app.rs` `on_window_created`. Deferred work: `pub fn post_task(thread_id: ThreadId, task: Option<&mut Task>) -> ::std::os::raw::c_int` (L57830).)

### 8.4 Split focus ring and gutters without drawing over web content

- Each pane is a wrapper Panel with BoxLayout `inside_border_insets` of 2px, holding the BrowserView.
- Set the wrapper background with `ImplView::set_background_color(&self, color: u32)` (L38385): `--accent` (ARGB) when focused, `--frame` otherwise.
- The 6px gutters are `between_child_spacing` in the parent BoxLayout.
- Divider dragging needs a hit target. Options:
  - Make the gutter a tiny native View whose delegate handles the mouse. CEF Views has no raw mouse delegate for plain Views, so this may not be possible.
  - Or, recommended for the MVP: handle the drag in the **TopStrip/Content** layer. When the user presses on a gutter (detected via the window's `WM_*` subclass on `window_handle()`), Rust updates flex fractions live. The fallback is a keyboard/command "Equalize panes" plus 25/50/75 presets.
  - Flag for prototype.

### 8.5 No native animation or translucency

- Native layout changes (sidebar dock/undock, split resize) should be **instant**. Stepping `set_bounds` every frame reflows web content and janks.
- All motion in §5 happens inside HTML surfaces.
- Mica/Acrylic (Arc-Win uses Mica) would require transparent views, which windowed browsers can't provide (§8.2). Non-goal.
- IMPLEMENTED: the shell's only part in motion is *timing* (`crates/sta/src/motion.rs`,
  docs/ARCHITECTURE.md §4.7). A hidden page renders no frames, so a surface presents a **blank frame**
  before its widget is hidden and says so (`surface.exit {gen}` → `surface.exited {gen}`; the floating
  sidebar is asked through `sidebar.hover {gen}`). The wait around it is a correctness delay — at least
  50 ms for an overlay, 60 ms for the sidebar park, at most 120 ms — that switching the animation off
  shortens to its floor but never to 0. While pages cross-fade their own colours, `SetChrome` lands at
  the midpoint, because the native card fill, border and corner tiles can only snap.

### 8.6 Navigation hooks for pinned, Peek, and new-tab rules

- `impl ImplRequestHandler … fn on_before_browse(&self, browser: Option<&mut Browser>, frame: Option<&mut Frame>, request: Option<&mut Request>, user_gesture: ::std::os::raw::c_int, is_redirect: ::std::os::raw::c_int) -> ::std::os::raw::c_int` (L26781).
  - cef_request_handler.h: "Called on the UI thread before browser navigation. Return true to cancel the navigation … If the navigation is canceled CefLoadHandler::OnLoadError will be called with an |errorCode| value of ERR_ABORTED." Ignore that `ERR_ABORTED` for Peek-redirected navigations.
  - Peek predicate: `frame.is_main()` (`ImplFrame::is_main`, L7557), `request.method()` == "GET" (`ImplRequest::method`, L6681; VERIFIED-FIX: it returns `CefStringUserfree`, so compare with `CefString::from(&request.method()).to_string() == "GET"` using `impl From<&CefStringUserfreeUtf16> for CefStringUtf16`, string.rs L504), and `request.transition_type().get_raw() & TransitionType::SOURCE_MASK.get_raw() == TransitionType::LINK.get_raw()` (`ImplRequest::transition_type`, L6726; `get_raw` L47356).
- `impl ImplRequestHandler … fn on_open_urlfrom_tab(&self, browser: Option<&mut Browser>, frame: Option<&mut Frame>, target_url: Option<&CefString>, target_disposition: WindowOpenDisposition, user_gesture: ::std::os::raw::c_int) -> ::std::os::raw::c_int` (L26792). The header covers "links clicked via middle-click or ctrl + left-click".
  - Map `WindowOpenDisposition::NEW_BACKGROUND_TAB` / `NEW_FOREGROUND_TAB` to Today tabs below the opener.
  - Map `NEW_WINDOW` (Shift+click) to Peek (P1).
  - Return 1 to cancel the in-place navigation.
- `impl ImplLifeSpanHandler … fn on_before_popup(&self, browser: Option<&mut Browser>, frame: Option<&mut Frame>, popup_id: ::std::os::raw::c_int, target_url: Option<&CefString>, target_frame_name: Option<&CefString>, target_disposition: WindowOpenDisposition, user_gesture: ::std::os::raw::c_int, popup_features: Option<&PopupFeatures>, window_info: Option<&mut WindowInfo>, client: Option<&mut Option<Client>>, settings: Option<&mut BrowserSettings>, extra_info: Option<&mut Option<DictionaryValue>>, no_javascript_access: Option<&mut ::std::os::raw::c_int>) -> ::std::os::raw::c_int` (L20712).
  - Header: "Any modifications to |windowInfo| will be ignored if the parent browser is wrapped in a CefBrowserView."
  - Return 0 to let CEF create the popup (this keeps `window.opener`). Then host it in `impl ImplBrowserViewDelegate … fn on_popup_browser_view_created(&self, browser_view: Option<&mut BrowserView>, popup_browser_view: Option<&mut BrowserView>, is_devtools: ::std::os::raw::c_int) -> ::std::os::raw::c_int` (L37680).
  - Instead of `window_create_top_level`, as cefsimple does, add the popup view into the **Peek overlay** (or into a new Today tab slot for `NEW_FOREGROUND_TAB`) and return 1.
  - `window_create_top_level(delegate: Option<&mut WindowDelegate>) -> Option<Window>` (L59477) remains the fallback for DevTools (`is_devtools=1`).
- "Click outside Peek closes it": `impl ImplFocusHandler … fn on_got_focus(&self, browser: Option<&mut Browser>)` (L19557) on the underlying tab's browser closes the Peek. VERIFIED-FIX: this only works if Peek took focus when shown (`request_focus`, L38383), and it should apply to *any* non-Peek browser (sidebar, top strip). See §2.18.
- Tab metadata:
  - `impl ImplDisplayHandler … fn on_address_change(&self, browser: Option<&mut Browser>, frame: Option<&mut Frame>, url: Option<&CefString>)` (L17603), `fn on_title_change(&self, browser: Option<&mut Browser>, title: Option<&CefString>)` (L17611), `fn on_favicon_urlchange(&self, browser: Option<&mut Browser>, icon_urls: Option<&mut CefStringList>)` (L17613).
  - `fn on_loading_progress_change(&self, browser: Option<&mut Browser>, progress: f64)` (L17656) is on the same trait (the URL-pill progress bar).
  - `impl ImplLoadHandler … fn on_loading_state_change(&self, browser: Option<&mut Browser>, is_loading: ::std::os::raw::c_int, can_go_back: ::std::os::raw::c_int, can_go_forward: ::std::os::raw::c_int)` (L21416).
- Closing and unloading: `impl ImplBrowserHost … fn close_browser(&self, force_close: ::std::os::raw::c_int);` (L12544), `fn try_close_browser(&self) -> ::std::os::raw::c_int;` (L12546). VERIFIED-FIX: per-tab close must override `do_close` (L20751) to return 1, otherwise CEF forwards the close to the whole Window; see §8.1.
- VERIFIED-FIX (popup routing detail): a plain `target=_blank` link click arrives in `on_before_popup` with `NEW_FOREGROUND_TAB`, **not** in `on_open_urlfrom_tab`. Since Chromium 88, `_blank` links imply `noopener`, so it's safe to return 1 (cancel) and open `target_url` as a fresh Today tab. Only `window.open` calls that keep an opener (usually `NEW_POPUP` with `popup_features`) need the "return 0 + host in `on_popup_browser_view_created`" path.
- Session history: only `impl ImplBrowserHost … fn navigation_entries(&self, visitor: Option<&mut NavigationEntryVisitor>, current_only: ::std::os::raw::c_int);` (L12639) exists, and it is read-only. There is **no restore API**, hence §2.4 and §7.
- Find, zoom, mute:
  - `impl ImplBrowserHost … fn find(&self, search_text: Option<&CefString>, forward: ::std::os::raw::c_int, match_case: ::std::os::raw::c_int, find_next: ::std::os::raw::c_int);` (L12603), `fn stop_finding(&self, clear_selection: ::std::os::raw::c_int);` (L12611)
  - `fn zoom(&self, command: ZoomCommand);` (L12566), `fn set_zoom_level(&self, zoom_level: f64);` (L12572)
  - `fn set_audio_muted(&self, mute: ::std::os::raw::c_int);` (L12742)
- Downloads: `impl ImplDownloadHandler … fn on_before_download(&self, browser: Option<&mut Browser>, download_item: Option<&mut DownloadItem>, suggested_name: Option<&CefString>, callback: Option<&mut BeforeDownloadCallback>) -> ::std::os::raw::c_int` (L18853), `fn on_download_updated(&self, browser: Option<&mut Browser>, download_item: Option<&mut DownloadItem>, callback: Option<&mut DownloadItemCallback>)` (L18863).
- Boost injection:
  - Renderer side: `impl ImplRenderProcessHandler … fn on_context_created(&self, browser: Option<&mut Browser>, frame: Option<&mut Frame>, context: Option<&mut V8Context>)` (L32531). cef_render_process_handler.h: "Called immediately after the V8 context for a frame has been created."
  - Inject with `impl ImplFrame … fn execute_java_script(&self, code: Option<&CefString>, script_url: Option<&CefString>, start_line: ::std::os::raw::c_int);` (L7550).

---

## 9. Open questions / risks

1. **Arc's exact Ctrl+W on pinned tabs is undocumented.** The help center only says pinned tabs "always revert back to the original link". §2.2 treats it as unload + reset. Validate against Arc-Win if possible.
2. **Page-first vs reserved per shortcut.** Arc's precedence is undocumented. Revisit after dogfooding (Google Docs, Figma, VS Code web).
3. **Split divider hit-testing without a native mouse delegate** (§8.4). Needs a prototype.
4. **Overlay BrowserViews under Alloy style on CEF 152.** cef#3790 was verified on M130. Re-verify focus (`can_activate`), IME in the Command Bar, and context menus inside overlays. Also: the maintainer reported a macOS dangling-pointer crash after a context menu in an overlay; irrelevant on Windows but worth watching.
5. **Remote suggest endpoints are unofficial.** Ship Google/Bing/DDG first and handle breakage silently (hide the group).
6. VERIFIED-FIX (new): **Translucent Panel overlay as scrim/shadow.** The source code supports it (§8.2), but it hasn't been run on Windows/M152. Prototype it in week 1; it changes the Command Bar and Peek visuals.
7. VERIFIED-FIX (new): **Ctrl key-up delivery.** When focus is on a non-browser view, it's unknown whether `ImplWindowDelegate::on_key_event` receives KEYUP (§2.13).
8. VERIFIED-FIX (new): **Alloy feature gaps.**
   - Ctrl+wheel zoom, `<datalist>`, autofill/password, device choosers, and whether JS `alert/confirm/beforeunload` dialogs render acceptably in Views-hosted Alloy browsers.
   - Build a checklist page and test it on day 1.
9. VERIFIED-FIX (new): **Hidden BrowserView visibility.** Confirm `set_visible(0)` sets `document.visibilityState = "hidden"` and throttles timers. `was_hidden` is OSR-only.
10. VERIFIED-FIX (new): **Snap Layouts on HTML caption buttons** needs an `HTMAXBUTTON` hit-test via a Win32 subclass (§2.1).

---

## 10. Sources

- Arc Help Center, Keyboard Shortcuts (official macOS/Windows table; fetched through the Zendesk API `resources.arc.net/api/v2/help_center/en-us/articles/20595231349911.json`): https://resources.arc.net/hc/en-us/articles/20595231349911-Keyboard-Shortcuts
- Arc for Windows 2023–2026 Release Notes: https://resources.arc.net/hc/en-us/articles/22513842649623-Arc-for-Windows-2023-2026-Release-Notes
- Pinned Tabs: https://resources.arc.net/hc/en-us/articles/19231060187159-Pinned-Tabs-Tabs-you-want-to-stick-around
- Why Are My Pinned Tabs and Favorites Reverting…: https://resources.arc.net/hc/en-us/articles/25541939922199
- Why is There a Slash Next to My Pinned Tab's Name?: https://resources.arc.net/hc/en-us/articles/25625148480279
- Favorites (max 12, across Spaces): https://resources.arc.net/hc/en-us/articles/19230755904151-Favorites-Top-Tabs-Across-Every-Space
- Auto Archive: https://resources.arc.net/hc/en-us/articles/19228855311127-Auto-Archive-Clean-as-you-go
- Spaces: https://resources.arc.net/hc/en-us/articles/19228064149143-Spaces-Distinct-Browsing-Areas
- Profiles: https://resources.arc.net/hc/en-us/articles/19227964556183-Profiles-Separate-Work-Personal-Browsing
- Folders: https://resources.arc.net/hc/en-us/articles/19228419623447-Folders-Stash-Similar-Tabs-Together
- Split View: https://resources.arc.net/hc/en-us/articles/19335393146775-Split-View-View-Multiple-Tabs-at-Once
- Peek: https://resources.arc.net/hc/en-us/articles/19335302900887-Peek-Preview-Sites-From-Pinned-Tabs
- Little Arc: https://resources.arc.net/hc/en-us/articles/19235387524503-Little-Arc-Quick-Lookups-Instant-Triaging
- Boosts: https://resources.arc.net/hc/en-us/articles/19212718608151-Boosts-Customize-Any-Website
- Library (Downloads/Archive): https://resources.arc.net/hc/en-us/articles/19230634389911
- How Do You Switch Between Tabs Quickly (Tab Switcher, 5 recent): https://resources.arc.net/hc/en-us/articles/25619402657303
- Full URL / Toolbar (macOS): https://resources.arc.net/hc/en-us/articles/25625458052247
- Search engine articles (list of engines): https://resources.arc.net/hc/en-us/articles/25614032197783
- Arc Split Views (drag/resize/vertical): https://blog.warrenweb.net/arc-split-views/
- Arc Peek (Shift+hover/Shift+click triggers): https://blog.warrenweb.net/arc-peek/
- Arc sidebar / auto-archive options 12h/24h/7d/30d: https://warrenweb.net/arc-sidebar/
- allthings.how, Split View on Windows: https://allthings.how/how-to-open-tabs-in-split-view-in-arc-browser-on-windows/
- allthings.how, Spaces: https://allthings.how/how-to-use-spaces-in-arc-browser/
- Hongkiat, 60+ Arc shortcuts (macOS; "Add Split View (Max. 4)"): https://www.hongkiat.com/blog/arc-browser-keyboard-shortcuts/
- dev.to, Arc shortcuts for Windows: https://dev.to/sushmoy/arc-shortcuts-windows-18ec
- CEF issue #4035, transparent overlay BrowserViews (open): https://github.com/chromiumembedded/cef/issues/4035
- CEF issue #3790, overlay BrowserView not shown in 125+ (closed 2024-10-17; works with Alloy style): https://github.com/chromiumembedded/cef/issues/3790
- VERIFIED-FIX (added sources):
  - Can You Add a New Default Search Engine Option (Ecosia is the custom-engine example): https://resources.arc.net/hc/en-us/articles/25619122951063
  - CEF M152 source, overlay host (translucent overlay widget): https://github.com/chromiumembedded/cef/blob/7977/libcef/browser/views/overlay_view_host.cc
  - CEF M152 source, BrowserView impl (Alloy-only check, deferred creation, reparent lifetime): https://github.com/chromiumembedded/cef/blob/7977/libcef/browser/views/browser_view_impl.cc
  - CEF announce, "Alloy style is supported in M125…" ("The Alloy extension API is not supported … The Chrome extension API is supported with Chrome style browsers/windows only"): https://groups.google.com/g/cef-announce/c/s1WaovAopFo
  - CEF architecture (Chrome runtime features: datalist/autofill, extensions, permission/device dialogs): https://chromiumembedded.github.io/cef/architecture

---

## Verification log

Adversarial pass on 2026-09-16. About 130 claims checked. Arc help-center articles were pulled as raw JSON from the Zendesk API (`resources.arc.net/api/v2/help_center/en-us/articles/<id>.json`): shortcuts, Arc-Win release notes (full 2023–2026 text), Pinned, Favorites, Auto Archive, Split View, Peek, Tab switching, Search engine ×2, Library, Folders, Spaces, Profiles, Boosts, Little Arc, Toolbar. Other sources: CEF GitHub issues via the API, CEF branch 7977 source via raw.githubusercontent, local CEF 152 headers, and the `cef` 152.3.0 bindings plus cefsimple.

**Confirmed correct**
- Arc-Win shortcut table. Every mapping in §0 item 1 is either in the official table or in the release notes: Ctrl+J/Ctrl+,/Ctrl+O/Alt+F/Alt+D/Ctrl+F4/Ctrl+Shift+I/Ctrl+P/Ctrl+U/F11.
- Arc behavior:
  - Favorites: max 12, per Profile, shared across Spaces. Pinned tabs never auto-archive, "/" means navigated, favicon click resets.
  - Auto-archive: 12h default, can't be disabled, per Profile, "Viewing or clicking … reset the timer".
  - Archive restore by click on Windows. Split: H/V, Ctrl+L per pane, divider resize, drag beside a tab in the sidebar. Max 4 panes is sourced only from the macOS list (Hongkiat).
  - Peek: expand/Ctrl+O, click-outside/X/Ctrl+W/Esc, toggles added in 1.7.1, button order matches macOS.
  - Ctrl+Tab = 5 most recent tabs. Ctrl+Alt+↑/↓ follows sidebar order. Space reorder by drag, right-click Space icon, mouse buttons switch Spaces, Mica/Acrylic setting, zoom toast, maximized-state restore, Little Arc and Boosts macOS-only.
- CEF:
  - All 49 cited binding line numbers and signatures match the file byte-for-byte, including traits.
  - `wrap_window_delegate!` block order and `::new(fields…)` claims match the macro source.
  - `KeyEventType::KEYUP`, `TransitionType::SOURCE_MASK/LINK` + `get_raw`, `RuntimeStyle::ALLOY` and `DockingMode::CUSTOM` exist and derive `PartialEq`. `cef::sys::MSG` exists, and `EVENTFLAG_*` values are 2/4/8.
  - Header quotes for IsFrameless, AddOverlayView ("Overlays are hidden by default."), SetAccelerator, SetPreferAccelerators, background_color, OnBeforeBrowse and OnBeforePopup are verbatim.
  - cef#3790 closed 2024-10-17; the maintainer comment "this appears to work with Alloy style browsers (tested M130)" is verbatim. cef#4035 opened 2025-11-19, still open, and the GetColor quote is accurate.

**Errors found and fixed (all marked VERIFIED-FIX inline)**
1. **Missing blocking constraint.** BrowserViews must also return `RuntimeStyle::ALLOY` (L37708). An Alloy Window rejects Chrome-style BrowserViews ("Cannot add Chrome style BrowserView to Alloy style Window"). §0, §8.1, new snippet in §8.3.
2. **`was_hidden` is OSR-only** ("only used when window rendering is disabled"). §2.12 now uses `View::set_visible`.
3. **Closing a tab browser would close the window.** Default `DoClose` forwards to `CefWindowDelegate::CanClose`. §8.1/§8.6 now require `do_close` → 1 plus view removal.
4. **"Overlays can't dim the page" is overstated.** CEF M152 overlay widgets are `kTranslucent` with a transparent compositor; only BrowserView content is forced opaque. §0, §8.2, §7 and §2.18 now describe a Panel-overlay scrim (pending a prototype).
5. **Peek and floating-sidebar overlays lacked `can_activate=1`.** Peek also needs `request_focus` on show, or "click outside" never fires.
6. **Alloy gaps missing from P0:**
   - downloads are *cancelled* by default (§2.20 fix);
   - permission prompts deny/ignore by default (new §2.25);
   - HTML5 fullscreen and Esc-to-exit are the client's job (new §2.26);
   - no find bar UI (new §2.27);
   - no Chromium accelerators (§3).
7. **Extensions.** "Alloy has partial extension support" is misleading in both directions: there is no Alloy extension *API* in CEF 152, yet extensions themselves run (Chrome bootstrap) — what is missing is Chromium's extension UI and tabs/windows visibility, and Chrome style is limited to one BrowserView per Window (§7, `docs/research/extensions.md`). sta supplies the UI itself (picker, popup card, Settings › Extensions, ARCHITECTURE §4.6); the visibility of tabs to extensions is the part that cannot be supplied. The autofill/password "Chromium defaults still work" claim is false for Alloy.
8. **Arc facts wrong or mislabelled:**
   - "Arc-Win has no remapping" is refuted by the shortcuts article.
   - "Arc's precedence is undocumented" is wrong: Arc-Win documents a conflict prompt for Ctrl+S / Ctrl+Shift+C.
   - The search-engine list was cited to an article that lists none; Ecosia is Arc's *custom-engine example*, and Kagi is unverified.
   - Per-Space light/dark is [A]; Arc's is global.
   - The toolbar/full-URL article is macOS-only; Arc-Win has a top toolbar with a URL bar.
   - Mouse-button Space switching "over sidebar" is [A].
   - Favorites stay static only across same-Profile Spaces.
   - Window fullscreen should hide the sidebar (Arc-Win fix note).
9. **Missing Arc-Win features:** Alt+Shift+F fullscreen, Ctrl+Z undo sidebar actions, double-click a Favorite to reset, "New Tab Created" toast when the sidebar is hidden, and per-segment × in split rows (was "close whole group").
10. **Internal inconsistencies:**
    - Ctrl+D pin destination: top (§2.2) vs bottom (§2.5); now bottom.
    - Rounded overlays (floating sidebar 12px, Peek 12px, toast 18px, command bar 12px) contradicted §8.2's own square-corner rule.
    - Zoom "Ctrl++" collided with Split's Ctrl+Shift+= on US layouts.
    - `--fs-caption` was invalid CSS; `telemetry: false` was not a Rust type.
11. **Code gotchas:**
    - `std::sync::Mutex` in the delegate snippet deadlocks on synchronous re-entrant callbacks.
    - `request.method()` returns `CefStringUserfree`, so it needs conversion.
    - Boost CSS injection at context creation can hit a null `documentElement`.
    - A background tab's browser isn't created until its BrowserView is added to the hierarchy.
    - `_blank` links go through `on_before_popup`, not `on_open_urlfrom_tab`.
    - Draggable regions can come from `on_draggable_regions_changed` (L19155) instead of custom IPC.

**Still uncertain (not verifiable without running code or Arc-Win)**
- Arc's real Ctrl+W on pinned tabs/Favorites (Hongkiat's macOS list says Cmd+W = "Archive Tab"; nothing Windows-specific found).
- Arc-Win split max panes, Ctrl+Shift+[ / ] on Windows, and whether Arc-Win supports remapping (the article is platform-generic).
- The translucent Panel scrim: the code path is read, not run.
- Whether `ImplWindowDelegate::on_key_event` sees KEYUP, and whether `set_visible(0)` sets page visibility to hidden.
- Ctrl+wheel zoom in Alloy, and whether `on_context_created` fires early on script-less pages.
- Split-divider hit-testing and left-edge hover detection both depend on a Win32 subclass of the top-level HWND (not prototyped).
- Unofficial suggest endpoints (Kagi's likely needs auth).
