# Chrome extensions in CEF 152 (Chrome bootstrap, Alloy views)

Verified notes behind `crates/sta/src/foreign.rs` (ARCHITECTURE §4.5) and the extensions work of
§4.6 (`extensions.rs`, `ext_popup.rs`, `ext_backend.rs`). **VERIFIED** = observed in a run or read in
the cited source; **UNVERIFIED** is marked as such. Full research and spike logs:
`C:/ast/tmp/ext-design/` (`extensions.md`, `plan.md`, `critique.md`) and the gate results in
`gates-p1.md` / `gates-p3.md` there. CEF 152.0.6 / Chromium 152.0.7977.83.

## 1. What works in sta's Alloy tabs

Extensions are installed and run by Chromium itself (the Chrome bootstrap loads the whole extension
system), not by CEF's removed Alloy extension API. VERIFIED in spike runs:

| Works | Does not work |
|---|---|
| MV3 service workers (Web Store, sideloaded, `--load-extension`) | `chrome.tabs.query`, `chrome.windows.*` (they never see Alloy tabs) |
| Content scripts, `runtime.sendMessage` both ways, `sender.tab.id` | `tabs.create` from a service worker without a Chrome window ("No current window") |
| `declarativeNetRequest`, `webRequest`, `webNavigation` (ad blockers really block) | `action.onClicked`, `action.openPopup`, keyboard `commands`, `sidePanel.open`, extension context-menu items |
| `tabs.get/update/reload/setZoom`, `scripting.executeScript` with a tab id | `chrome://extensions` in an Alloy tab (blocked: `alloy_browser_host_impl.cc` `IsAllowedWebUIHost`) |
| Extension pages (`chrome-extension://…`) in tabs, with every `chrome.*` namespace | Popups whose service worker asks for the current tab (AdBlock, 1Password) |
| `management`, `debugger`, native messaging (the host starts) | Downloads a service worker starts (cancelled: no CEF browser) |

- The tab id extensions see is the `SessionID`, **not** `CefBrowser::GetIdentifier()` (the CEF header
  says otherwise). VERIFIED.
- `chrome.tabs.discard(<sta tab id>)` **crashes the browser process**: `TabsDiscardFunction::Run`
  dereferences the null window CEF's `GetAlloyTabById` patch returns (only `TabsUpdateFunction` is
  patched). VERIFIED; reported in the README's limitations.
- Extensions other programs registered (`HKCU\Software\Google\Chrome\Extensions`) are installed into
  every profile and start **disabled** with `disable_reasons: [8192]` (`DISABLE_EXTERNAL_EXTENSION`).
  Chromium installs them during the first seconds of a run. VERIFIED (gate S5).

## 2. Why a Chrome window appears

With no Chrome `Browser` in the profile, several Chromium paths create one, and CEF gives each of
its tabs `BrowserProcessHandler::GetDefaultClient()`. VERIFIED paths:

| Trigger | Chromium path |
|---|---|
| Web Store install success | `ExtensionInstallUIDesktop::OnInstallSuccess` → `FindOrCreateVisibleBrowser` (new tab page) → `TriggerPostInstallDialog` (a window owned by the new browser) |
| the extension's `onInstalled` → `tabs.create` | lands in that window |
| `runtime.openOptionsPage` (`options_page`) | `ChromeRuntimeAPIDelegate::OpenOptionsPage` → `CreateBrowserWindow(TYPE_NORMAL)` |
| `runtime.openOptionsPage` (embedded `options_ui`) | a window on `chrome://extensions/?options=<id>` (blank in an Alloy tab; sta rewrites it to the extension's own options page) |
| `windows.create` | a Browser; a `popup` one has the initial window title `_crx_<id>` |
| `identity.launchWebAuthFlow` | `WebAuthFlow` → a `TYPE_POPUP` Browser. Its CEF browser arrives **without a root window** (`GetAncestor(GA_ROOT) == 0` in `OnAfterCreated`); the window appears later |
| DevTools `Target.createTarget` | the same path (sta's `debug.foreign.trigger` uses it) |

At `OnAfterCreated` the root window exists but has **no `WS_VISIBLE`** yet (it is shown in the next
UI task), `has_view()` is 0 and the host runtime style is `CHROME`. `CefCommandHandler` can't trim
their toolbar (`create_params_.client` is empty for them), so restyling a Chrome window is not an
option. VERIFIED.

## 3. Hiding such a window

- `DWMWA_CLOAK` alone is enough: DWM draws nothing, and the shell keeps the window out of the
  taskbar (VERIFIED with UI Automation on `Shell_TrayWnd` during a real Web Store install: the
  button count never changed) and, by the same rule, out of Alt+Tab. Extended styles are left alone.
- Activation must be refused, or `ShowWindow` takes the keyboard from sta: a thread-local `WH_CBT`
  hook on the UI thread returns 1 for `HCBT_ACTIVATE` of hidden roots and the windows they own
  (7 blocked activations in one real install), and cloaks owned windows at `HCBT_CREATEWND` before
  their first show — that is how the "… has been added to Chromium" dialog never appears. DWM can
  answer `HCBT_CREATEWND` cloaking with a failure HRESULT and cloak the window anyway; read
  `DWMWA_CLOAKED` back instead of trusting the return value. VERIFIED.
- Cancelling the first navigation (`on_before_browse` → 1) is safe: the post-install dialog still
  arrives, nothing crashes, and the extension's welcome tab still opens. VERIFIED (gate S5).
- Windows that must stay native (`_crx_` popups, `launchWebAuthFlow`) paint, get a taskbar button
  and can take sta's dark caption and icon (`DWMWA_USE_IMMERSIVE_DARK_MODE`, `WM_SETICON`).
  VERIFIED (gate S6).
- Chromium's install dialog is a window owned by sta's main window with `WS_EX_DLGMODALFRAME` and
  no `WS_EX_TOOLWINDOW` (`ui/views/widget/widget_hwnd_utils.cc`: menus, `<select>` popups, bubbles
  and tooltips always get the tool-window style, and only dialogs get the modal frame). sta uses
  that signature to center it over the Web Store pane. VERIFIED (gate S17).
- Force-closing the last Chrome-created browser while a download runs shows **no** dialog and does
  not cancel the download: `close_browser(force=1)` bypasses Chromium's
  `CanCloseWithInProgressDownloads`. VERIFIED (gate S15); sta still waits for downloads.

## 4. Installing in a test

A local gallery (`--apps-gallery-url`, `--apps-gallery-download-url`) exposes `webstorePrivate` to
that origin and shows the real install dialog, but `CrxInstaller::InstallCrx` then requires a CRX
signed with Chromium's **test publisher** key (`GetWebstoreVerifierFormat(true)` →
`CRX3_WITH_TEST_PUBLISHER_PROOF`, `components/crx_file/crx_verifier.cc`). That key is not public and
the only sample CRX signed with it is MV2, which Chromium 152 refuses. So CI drives the post-install
path with `Target.createTarget('chrome://newtab/')` plus a probe copied into the profile's
`Extensions/<id>/<version>/`, and the real Web Store path is an opt-in run
(`STA_E2E_WEBSTORE=1`). VERIFIED (gate S13).

## 5. Managing extensions from a hidden `chrome://extensions` page (phase 3)

Behind `crates/sta/src/ext_backend.rs` and `extensions.rs` (ARCHITECTURE §4.6). Measurements:
`C:/ast/tmp/ext-design/gates-p3.md` (gate S7).

- **Chromium 152 writes no `extensions.settings.<id>.state` key.** An extension is disabled exactly
  when its `disable_reasons` is non-empty, and 152 writes that as a **list**: `[]` for one that runs,
  `[1]` (user action) for one the user turned off, `[8192]` for one another program added. Older
  profiles carry `state` (0 = disabled) and a bitmask; both are read. VERIFIED (the profile of every
  gate run).
- `chrome://extensions` in a **Chrome-style** window works: `chrome.management` and
  `chrome.developerPrivate` are both there, and `Runtime.evaluate` over CEF's in-process DevTools
  session reaches them. The window never has to be shown. VERIFIED (spike run12b and gate S7).
- The **BrowserView must get a size**: a Chrome-style browser in a window with no layout manager
  never commits its navigation, and the operation times out with nothing in the log. VERIFIED.
- `window_create_top_level` calls `on_window_created` **synchronously**, so the browser (and its
  first `on_load_end`) can arrive before the call returns. VERIFIED.
- **Removing an extension always shows Chromium's own dialog.** `chrome.management.uninstall`
  honours `showConfirmDialog: false` only for an extension removing *itself*, and
  `chrome.developerPrivate` has **no uninstall function at all** in 152 — the full listing measured
  in the page is in `gates-p3.md`; its `removeMultipleExtensions` confirms too. sta therefore lets
  that dialog be the confirmation and asks nothing of its own. VERIFIED (gate S7).
- `chrome.management.setEnabled` needs no dialog and clears `disable_reasons` (including the `8192`
  of an extension another program added): ~300 ms end to end, including sta's own re-read.
  VERIFIED.
- **`Secure Preferences` is committed up to ten seconds late** (`JsonPrefStore`'s commit interval;
  8–12 s measured), so an operation's effect is not on disk when it returns. sta patches its cached
  listing from the confirmed operation and re-reads the profile afterwards. VERIFIED.
- A popup page hosted in an Alloy BrowserView runs as a real extension page (`chrome.storage` works)
  but sees **no tabs**: `chrome.tabs.query({active: true, currentWindow: true})` answers `[]`, as
  §1 predicts. VERIFIED (gate S3, with the in-repo probe).
