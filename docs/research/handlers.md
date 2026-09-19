# sta: client handlers for tab-browser features (CEF 152.0.6, cef-rs 152.3.0)

**Citation notation**
- `B:Lnnnn` = line in `cef-152.3.0+152.0.6/src/bindings/x86_64_pc_windows_msvc.rs` (the generated Rust wrappers)
- `S:Lnnnn` = line in `cef-dll-sys-152.3.0+152.0.6/src/bindings/x86_64_pc_windows_msvc.rs`
- `string.rs:Lnnn` = `cef-152.3.0+152.0.6/src/string.rs`
- Header quotes come from `.cef/152.0.6/cef_windows_x86_64/include/...`.
- "CEF source" means the libcef sources at the exact commit of this build (`CEF_COMMIT_HASH 708dc140cbc3286826a8abef89dc23a44ff9ea72`, from `cef_version.h`), downloaded from github.com/chromiumembedded/cef.

---

## 0. The main conclusions

1. **Every BrowserView in the main Window must be Alloy style.** `internal/cef_types_runtime.h`: *"Alloy style Windows with the Views framework can host only Alloy style BrowserViews but Chrome style Windows can host both style BrowserViews. Additionally, a Chrome style Window can host at most one Chrome style BrowserView but potentially multiple Alloy style BrowserViews."* CEF source `browser_view_impl.cc` `AddedToWidget()` enforces this with `LOG(ERROR) << "Cannot add multiple Chrome style BrowserViews"`. The sidebar, the command bar and N tabs therefore need `BrowserViewDelegate::browser_runtime_style() -> RuntimeStyle::ALLOY`. So the **Alloy defaults** (§1) decide which handlers you must write.
   - VERIFIED-FIX: the real check is stricter than the header. `AddedToWidget()` refuses a Chrome-style BrowserView when `cef_widget->IsChromeStyle() && cef_widget->GetThemeProfile()`, and `ChromeBrowserWidget::GetThemeProfile()` (`chrome_browser_widget.cc`) returns the first *associated* profile, which **Alloy BrowserViews also register** via `AddAssociatedProfile`. So a Chrome-style BrowserView can only be added to a Chrome-style Window **before** any Alloy BrowserView. Mixing "one Chrome tab + Alloy UI" (or docking a Chrome-style DevTools view later) doesn't work in practice.
2. **Client design:** use one `Client` per *kind* of browser (web content, internal UI, DevTools), with **cached handler objects**. Keep a **registry keyed by `browser.identifier()`**, filled from a **per-tab `BrowserViewDelegate`** that carries the `TabId`. Don't bake the tab id into the Client: popups and DevTools inherit the opener's client by default.
3. **Closing a tab without closing the Window (Alloy + Views):**
   - Call `host.close_browser(0)`.
   - In `do_close`, **return 1**. If you return 0, CEF calls `CloseHostWindow()`, which runs `widget->Close()` and **closes the whole Window**.
   - Then post a task that removes the BrowserView from its panel and **drops every Rust reference to it**. When the last reference is released, `~CefBrowserViewImpl` calls `WindowDestroyed()`, which destroys the browser and fires `on_before_close`.
   - VERIFIED-FIX: `BrowserViewDelegate::on_browser_destroyed` does **not** fire on this path. `~CefBrowserViewImpl` runs `weak_ptr_factory_.InvalidateWeakPtrs()` first, and `CefBrowserPlatformDelegateViews::NotifyBrowserDestroyed()` only calls the delegate `if (browser_view_ && ...)` (a WeakPtr). Only `on_before_close` arrives, so do per-tab cleanup there.
   - ~~Never use `close_browser(1)` combined with `do_close` returning 1. That sequence leaks the browser (CEF issue #3376).~~ VERIFIED-FIX: not supported by the 708dc140 source. When `DoClose` returns true, `CloseContents()` runs `else if (destruction_state_ != DESTRUCTION_STATE_NONE) destruction_state_ = DESTRUCTION_STATE_NONE;`, so `close_browser(0)` and `close_browser(1)` both leave state `NONE`, and the posted detach then destroys the browser normally. The real traps are:
     - (a) Removing the view but keeping a BrowserView reference. That is the #3376 repro: `CloseBrowser(false)`, then detach in `DoClose` and return true, and the browser is never destroyed.
     - (b) Releasing the last reference **synchronously inside `do_close`**. The destroy re-enters, and the outer `CloseContents` then writes state `NONE` onto a destroyed browser.
     - (c) Releasing the last reference while a close is still pending in state `ACCEPTED` (after `close_browser(1)`, before beforeunload/unload finishes and `do_close` runs). The destructor then skips `WindowDestroyed()`. A later `do_close`→1 orphans the browser; `do_close`→0 reaches `CloseHostWindow()` → `GetWindowWidget()`, which dereferences the invalidated `browser_view_` WeakPtr (crash risk).
4. **Middle-click, Ctrl+click and target=_blank all need handlers:**
   - With Alloy, if `on_open_urlfrom_tab` doesn't cancel, the URL **loads in the current tab whatever the disposition** (CEF source `AlloyBrowserHostImpl::OpenURLFromTab` → `LoadMainFrameURL`).
   - `window.open` and target=_blank go through `on_before_popup`. Allow the popup and adopt the popup BrowserView as a tab in `on_popup_browser_view_created`, which keeps `window.opener`. Or cancel it and open your own tab.
5. **Keyboard shortcuts:** use `Window::set_accelerator` (B:L44331) in `on_window_created`, handled in `WindowDelegate::on_accelerator` (B:L43257).
   - Use `high_priority=1` for browser-reserved keys (Ctrl+T/W/Shift+T/Tab/1-9) and `0` for keys a page may override (Ctrl+S/F, F5, F12, Alt+←/→).
   - This works whichever BrowserView (tab, sidebar or overlay) has focus.
   - **Each command_id holds exactly one key combo**: `SetAccelerator` with an existing id replaces the old combo.
   - Use `KeyboardHandler::on_pre_key_event` only for Esc-to-exit-fullscreen and special cases.
6. **Audio:**
   - CEF 152 has no "audible" / "playing audio" notification. The only "audible" text in `include/` is in `cef_pack_strings.h`.
   - `host.set_audio_muted` (B:L12742) and `is_audio_muted` (B:L12744) exist.
   - `AudioHandler` **captures and mutes** the tab (`mute_source = true` in `audio_loopback_stream_creator.cc`), but only when `audio_parameters` returns 1. VERIFIED-FIX: the Rust default `ImplAudioHandler::audio_parameters` returns `Default::default()` = **0** (B:L13895-13901), while the C++ default returns `true`. With cef-rs, an `AudioHandler` that doesn't override `audio_parameters` captures nothing and mutes nothing.
   - Returning **0** from `AudioHandler::audio_parameters` gives an "audio started" edge without muting, but there is no matching "stopped" edge.
7. **Behavior with Alloy style and no handler implemented:**

   | Feature | Result | Handler needed? |
   |---|---|---|
   | JS alert / confirm / prompt / beforeunload | Chrome tab-modal dialogs are shown | No |
   | File picker | Shown | No |
   | Context menu | Shown | No |
   | Media permission (camera/mic) | **Denied** | `PermissionHandler` |
   | Other permission prompts | **Ignored** | `PermissionHandler` |
   | Downloads | Header says cancel; 152 source falls through to Chrome's download delegate (silent save). VERIFIED-FIX: the fall-through is confirmed (`patch/patches/chrome_browser_download.patch`: `if (cef_delegate_->DetermineDownloadTarget(...)) return true;` and otherwise Chrome's logic runs). "Silent, default dir, no UI" is inferred, not runtime-tested. | Always implement `DownloadHandler` (and override `can_download`, see §9) |
   | Find | No find bar | `FindHandler` plus your own UI |
   | Fullscreen | You must drive it yourself | `DisplayHandler` |
   | `execute_chrome_command` | Chrome-style only; does nothing for Alloy tabs | — |
8. **Profiles:** each space's `RequestContextSettings.cache_path` must be an **immediate child** of `Settings.root_cache_path`. The CEF source checks `cache_path_.DirName() == user_data_dir`; any other path logs "Cannot create profile at path" and **silently falls back to incognito**.
9. **Threading:**
   - Every handler below runs on the **UI thread** (the main thread with `run_message_loop`), except the IO-thread `RequestHandler::resource_request_handler`/`auth_credentials` and the audio-thread `AudioHandler` packet callbacks.
   - CEF calls several callbacks **synchronously inside your calls**: `add_child_view` → `on_after_created`/`on_browser_created`; dropping the last BrowserView → `on_before_close` (VERIFIED-FIX: **not** `on_browser_destroyed`, whose WeakPtr is already invalidated; it is async if unload handlers still need to run).
   - **Never hold your state `Mutex` across a CEF call.**

---

## 1. Runtime-style defaults (why these handlers)

| Area | Alloy style (our tabs/UI) | Chrome style | Evidence |
|---|---|---|---|
| `LifeSpanHandler::do_close` | called | **not called** | `cef_browser.h` CloseBrowser: "DoClose (Alloy style only)" |
| JS dialogs | Chrome tab-modal dialog (TabHelpers attached, `GetWebContentsModalDialogHost` implemented) | same | `javascript_dialog_manager.cc`, `browser_platform_delegate_alloy.cc` |
| File chooser | `FileSelectHelper::RunFileChooser` (native dialog) | same | `alloy_browser_host_impl.cc` |
| Media permission | "With Alloy style, default handling will deny the request." | permission UI | `cef_permission_handler.h` |
| Permission prompts (geo, notifications, clipboard…) | "default handling is CEF_PERMISSION_RESULT_IGNORE" | prompt UI | `cef_permission_handler.h`, `permission_prompt.cc` |
| `on_before_download` returns 0 | header: "cancel with Alloy style"; 152 source: `alloy_bootstrap=false` → Chrome delegate decides (default Downloads dir, no UI) | download bubble | `cef_download_handler.h`, `download_manager_delegate.cc:14` |
| Fullscreen (JS Fullscreen API) | "client is responsible for triggering the fullscreen transition (e.g. CefWindow::SetFullscreen)" | automatic | `cef_display_handler.h` |
| Exit fullscreen on Esc | client must call `ExitFullscreen` from `OnPreKeyEvent` | internal | `cef_browser.h` ExitFullscreen |
| Unresponsive renderer | "continue waiting with Alloy style" | "Page unresponsive" dialog | `cef_request_handler.h` |
| `ExecuteChromeCommand` / `CommandHandler` | n/a | "Only used with Chrome style" | `cef_browser.h`, `cef_command_handler.h` |
| DevTools window | **always Chrome style**, even for an Alloy inspected browser | Chrome style | `chrome_browser_delegate.cc` `CreateDevToolsBrowser`: `CHECK(platform_delegate->IsChromeStyle())` |
| `WasHidden` | windowed: `DCHECK(false) << "Window rendering is not disabled"` and returns | `NOTIMPLEMENTED()` | `alloy_browser_host_impl.cc`, `chrome_browser_host_impl.cc` |

---

## 2. `wrap_*!` mechanics, Client design, shared state

### 2.1 How the macros work (verified from the macro bodies, e.g. `wrap_client!` B:L27923)

- **Form.** `wrap_xxx! { [vis] struct Name { [vis] field: Type, ... } impl Xxx { fn method(&self, ...) -> R { ... } ... } }`. A unit form `struct Name;` also exists.
  - VERIFIED-FIX: for **multi-level** macros (`wrap_browser_view_delegate!`, `wrap_window_delegate!`, …) the unit-form arm re-invokes the macro **without** the `impl ViewDelegate {}` / `impl PanelDelegate {}` blocks (see the first arm of `wrap_window_delegate!` at B:L43304). That call matches no arm and fails to compile. Use `struct Name {}` there.
- **Rust defaults are not C++ defaults.** Every trait default is `Default::default()`, i.e. 0/None, and `init_methods` always installs the function pointer, so CEF always calls the Rust default. VERIFIED-FIX: this flips every C++ default that returns `true`/`this`:
  - `ImplDownloadHandler::can_download` (B:L18844; C++ `true`)
  - `ImplAudioHandler::audio_parameters` (B:L13895; C++ `true`)
  - `ImplBrowserViewDelegate::delegate_for_popup_browser_view` (B:L37676; C++ `this`)
  - `ImplWindowDelegate::can_resize`/`can_maximize`/`can_minimize`/`can_close` (B:L43240-43255; C++ `true`)
  - `ImplResourceRequestHandler::can_send_cookie`/`can_save_cookie` (C++ `true`)
- **Generated struct.** The macro adds a hidden field `cef_object: *mut RcImpl<sys::_cef_xxx_t, Self>`, so you **cannot build `Name { .. }` yourself**.
  - The constructor is `Name::new(field1, field2, ...)`, taking the fields **in declaration order**.
  - It **returns the CEF type**, e.g. `Client` or `DisplayHandler`, not `Name`.
  - Internally it runs `Client::new(Self { fields, cef_object: null })` (B:L27816), then `init_methods` and `wrap_rc`.
- **Clone bound.** The macro implements `Clone for Name` as `add_ref()` followed by `field.clone()` for every field, so **every field type must be `Clone`**.
  - Cloning a `RefCell<Option<X>>` field **deep-copies** it rather than sharing it.
  - Put shared mutable state behind `Arc<Mutex<..>>` or `Arc<..>`.
- **Methods.** Methods use `&self` only, with no `pub`. You override only the methods you need; the rest keep the `ImplXxx` defaults.
  - `c_int` parameters can be written `i32`, as cefsimple does.
  - Types in the signatures must be in scope where the macro expands. For example, `KeyboardHandler` needs `use cef::sys::MSG;`: `MSG` is `cef_dll_sys::MSG` (S:L17889) and is **not** re-exported by `cef::*`.
- **Multi-level interfaces** need **every base block, in order, even when empty**:
  - `wrap_browser_view_delegate!` needs `impl ViewDelegate {}` then `impl BrowserViewDelegate {..}` (macro at B:L37739).
  - `wrap_window_delegate!` needs `impl ViewDelegate {}` + `impl PanelDelegate {}` + `impl WindowDelegate {..}` (cefsimple `simple_app.rs`).
- **No hidden Send bound.** `RefGuard<T>` is `unsafe impl Send + Sync` (`rc.rs`), so every `Client`, `Browser` etc. is `Send` even if your struct holds `Rc`/`RefCell`. The compiler **won't** stop cross-thread misuse.
- **Returning handlers.** A getter like `display_handler(&self) -> Option<DisplayHandler>` goes through `From<DisplayHandler> for *mut _cef_display_handler_t` (B:L18357), which does `get_raw` + `mem::forget`. Returning `Some(self.display.clone())` therefore hands CEF exactly one reference, which is correct.
  - CEF calls `GetXxxHandler()` for **every event**. cefsimple builds a new wrapper object per call; **cache the handlers** in Client fields instead.
- **Don't re-wrap `self`.** Never do `BrowserViewDelegate::new(self.clone())` inside a method. `clone()` adds a ref to the old object and `new()` overwrites `cef_object`, so a reference leaks. Build a fresh instance from the shared state instead.

### 2.2 One Client per BrowserView, or a shared Client?

**Answer: share one Client per kind, and look up roles by `browser.identifier()`.** Reasons:

- `on_before_popup`: *"The |client| and |settings| values will default to the source browser's values."* An adopted popup would otherwise report under the opener's tab id.
- DevTools: when `ShowDevTools` gets no client, CEF uses `BrowserProcessHandler::default_client`, else the opener's client (`chrome_browser_delegate.cc`). VERIFIED-FIX, more precisely: `CreateDevToolsBrowser` starts from the **`ShowDevTools` params** (`client_`, `settings_`) when DevTools was opened via `show_dev_tools`. It uses the **opener's client/settings** only when DevTools was opened another way (no pending params). `OnBeforeDevToolsPopup` may then change it. If the result is empty, `CreateBrowserHost` falls back to `GetDefaultClient()`, and if that is also empty it logs "Creating a chrome browser without a client".
- `cef_browser.h` `GetIdentifier`: *"Returns the globally unique identifier for this browser."* It is available in every browser-scoped callback: `ImplBrowser::identifier(&self) -> ::std::os::raw::c_int` (B:L11725). The reverse lookup is `pub fn browser_host_get_browser_by_identifier(browser_id: ::std::os::raw::c_int) -> Option<Browser>` (B:L57702).
- **Per-tab identity lives in the BrowserView delegate:**
  - Registration happens in `fn on_browser_created(&self, browser_view: Option<&mut BrowserView>, browser: Option<&mut Browser>)` (B:L37656).
  - Header: *"called after CefLifeSpanHandler::OnAfterCreated() is called"*.
  - Both fire **synchronously inside `panel.add_child_view(...)`**. `CefBrowserViewImpl::AddedToWidget()` calls `CefBrowserHostBase::Create()`, which fires `OnAfterCreated` and then `NotifyBrowserCreated()` (CEF source).

```rust
use cef::*;
use std::{collections::HashMap, sync::{Arc, Mutex, OnceLock}};

pub type TabId = u64;
#[derive(Clone, Copy, Debug)]
pub enum Role { Tab(TabId), Sidebar, CommandBar, Popup { opener: i32 }, DevTools }

pub struct AppState {
    pub roles: HashMap<i32, Role>,          // browser.identifier() -> role
    pub tabs: HashMap<TabId, TabEntry>,
    pub content_panel: Option<Panel>,
    pub window: Option<Window>,
    pub shutting_down: bool,
}
pub struct TabEntry { pub view: BrowserView, pub browser: Option<Browser>, pub space: String /*...*/ }

pub struct AppShared {
    pub state: Mutex<AppState>,
    pub web_client: OnceLock<Client>,   // shared by all web tabs (set once after Arc creation)
    pub ui_client: OnceLock<Client>,    // sidebar/command bar (IPC router, no context menu, ...)
}
impl AppShared {
    pub fn role_of(&self, id: i32) -> Option<Role> { self.state.lock().unwrap().roles.get(&id).copied() }
    pub fn emit(&self, _browser_id: i32, _ev: TabEvent) { /* enqueue, flush to sidebar UI via IPC in a posted task */ }
}
pub enum TabEvent { Url(String), Title(String), Favicons(Vec<String>), Loading { is_loading: bool, back: bool, fwd: bool },
                    Progress(f64), Status(String), Fullscreen(bool), Media { video: bool, audio: bool } }
```

**Client with cached handlers** (struct fields are all `Clone`):

```rust
wrap_client! {
    pub struct WebClient {
        display: DisplayHandler,
        load: LoadHandler,
        life_span: LifeSpanHandler,
        request: RequestHandler,
        keyboard: KeyboardHandler,
        context_menu: ContextMenuHandler,
        download: DownloadHandler,
        permission: PermissionHandler,
        find: FindHandler,
    }

    impl Client {
        fn display_handler(&self) -> Option<DisplayHandler> { Some(self.display.clone()) }
        fn load_handler(&self) -> Option<LoadHandler> { Some(self.load.clone()) }
        fn life_span_handler(&self) -> Option<LifeSpanHandler> { Some(self.life_span.clone()) }
        fn request_handler(&self) -> Option<RequestHandler> { Some(self.request.clone()) }
        fn keyboard_handler(&self) -> Option<KeyboardHandler> { Some(self.keyboard.clone()) }
        fn context_menu_handler(&self) -> Option<ContextMenuHandler> { Some(self.context_menu.clone()) }
        fn download_handler(&self) -> Option<DownloadHandler> { Some(self.download.clone()) }
        fn permission_handler(&self) -> Option<PermissionHandler> { Some(self.permission.clone()) }
        fn find_handler(&self) -> Option<FindHandler> { Some(self.find.clone()) }
    }
}

pub fn make_web_client(app: &Arc<AppShared>) -> Client {
    WebClient::new(                      // arg order == field order
        StaDisplay::new(app.clone()),
        StaLoad::new(app.clone()),
        StaLifeSpan::new(app.clone()),
        StaRequest::new(app.clone()),
        StaKeyboard::new(app.clone()),
        StaContextMenu::new(app.clone()),
        StaDownload::new(app.clone()),
        StaPermission::new(app.clone()),
        StaFind::new(app.clone()),
    )
}
```

The Client getters available are `ImplClient` (B:L27833): `audio_handler`, `command_handler`, `context_menu_handler`, `dialog_handler`, `display_handler`, `download_handler`, `drag_handler`, `find_handler`, `focus_handler`, `frame_handler`, `permission_handler`, `jsdialog_handler`, `keyboard_handler`, `life_span_handler`, `load_handler`, `print_handler`, `render_handler`, `request_handler`, `on_process_message_received`.

**Per-tab BrowserView delegate.** The fields are `Clone`, and the delegate **must not hold its BrowserView** (that would be a reference cycle).

```rust
wrap_browser_view_delegate! {
    pub struct TabViewDelegate {
        app: Arc<AppShared>,
        tab: Option<TabId>,          // None for delegates handed to popups
    }

    impl ViewDelegate {}

    impl BrowserViewDelegate {
        fn on_browser_created(&self, _browser_view: Option<&mut BrowserView>, browser: Option<&mut Browser>) {
            let (Some(tab), Some(b)) = (self.tab, browser) else { return };
            let mut s = self.app.state.lock().unwrap();           // no CEF calls while locked
            s.roles.insert(b.identifier(), Role::Tab(tab));
            if let Some(t) = s.tabs.get_mut(&tab) { t.browser = Some(b.clone()); }
        }
        fn on_browser_destroyed(&self, _browser_view: Option<&mut BrowserView>, browser: Option<&mut Browser>) {
            // header: "called before CefLifeSpanHandler::OnBeforeClose()". Drop Browser refs here.
            // VERIFIED-FIX: only fires on Window tear-down (CefBrowserViewImpl::Detach). When a tab is closed by
            // dropping its last BrowserView ref, ~CefBrowserViewImpl invalidates the WeakPtr first and this is
            // SKIPPED. Duplicate any cleanup in LifeSpanHandler::on_before_close.
            let Some(b) = browser else { return };
            let id = b.identifier();
            let mut s = self.app.state.lock().unwrap();
            if let Some(Role::Tab(t)) = s.roles.get(&id).copied() { if let Some(e) = s.tabs.get_mut(&t) { e.browser = None; } }
        }
        fn delegate_for_popup_browser_view(&self, _browser_view: Option<&mut BrowserView>, _settings: Option<&BrowserSettings>,
                                           _client: Option<&mut Client>, is_devtools: i32) -> Option<BrowserViewDelegate> {
            // Rust default returns None (B:L37676) whereas C++ default returns `this`:
            // without this override the popup BrowserView has NO delegate (its own popups -> default windows).
            if is_devtools != 0 { return None; }
            Some(TabViewDelegate::new(self.app.clone(), None))
        }
        fn on_popup_browser_view_created(&self, browser_view: Option<&mut BrowserView>,
                                         popup_browser_view: Option<&mut BrowserView>, is_devtools: i32) -> i32 {
            if is_devtools != 0 { return 0; }        // CEF creates a default (Chrome-style) DevTools Window
            let (Some(opener_bv), Some(pbv)) = (browser_view, popup_browser_view) else { return 0 };
            let opener_id = opener_bv.browser().map(|b| b.identifier()).unwrap_or(0);
            adopt_popup_as_tab(&self.app, opener_id, pbv.clone())   // §5.1: add to content panel, register role, return 1
        }
        fn browser_runtime_style(&self) -> RuntimeStyle { RuntimeStyle::ALLOY }
    }
}
```

**Creating a tab** (UI thread):
- `pub fn browser_view_create(client: Option<&mut Client>, url: Option<&CefString>, settings: Option<&BrowserSettings>, extra_info: Option<&mut DictionaryValue>, request_context: Option<&mut RequestContext>, delegate: Option<&mut BrowserViewDelegate>) -> Option<BrowserView>` (B:L59126)
- `ImplPanel::add_child_view(&self, view: Option<&mut View>)` (B:L41652)
- `impl std::convert::From<&BrowserView> for View` (B:L39340)

```rust
pub fn open_tab(app: &Arc<AppShared>, url: &str, mut space_ctx: Option<RequestContext>, foreground: bool) -> TabId {
    let tab = next_tab_id();
    let mut client = app.web_client.get().cloned();
    let mut delegate = TabViewDelegate::new(app.clone(), Some(tab));
    let bv = browser_view_create(client.as_mut(), Some(&CefString::from(url)), Some(&BrowserSettings::default()),
                                 None, space_ctx.as_mut(), Some(&mut delegate)).expect("BrowserView");
    let panel = {
        let mut s = app.state.lock().unwrap();
        s.tabs.insert(tab, TabEntry { view: bv.clone(), browser: None, space: String::new() });
        s.content_panel.clone().expect("content panel")
    };                                                    // lock released BEFORE the CEF call below
    bv.set_visible(foreground as i32);                    // ImplView::set_visible (B:L38365)
    panel.add_child_view(Some(&mut View::from(&bv)));     // browser created synchronously here → on_after_created, on_browser_created
    tab
}
```

### 2.3 Posting to the UI thread (closure task)

Signatures:
- `pub fn post_task(thread_id: ThreadId, task: Option<&mut Task>) -> ::std::os::raw::c_int` (B:L57830)
- `pub fn post_delayed_task(thread_id: ThreadId, task: Option<&mut Task>, delay_ms: i64) -> ::std::os::raw::c_int` (B:L57846)
- `pub fn currently_on(thread_id: ThreadId) -> ::std::os::raw::c_int` (B:L57820)
- `ImplTask::execute(&self)` (B:L29490)
- `ThreadId::UI` (B:L47510)

`cef_task.h` warns: *"If the task fails to post then the task object may be destroyed on the source thread instead of the target thread."* The helper below is modeled on `tests_shared/src/browser/main_message_loop.rs`:

```rust
type Job = Arc<Mutex<Option<Box<dyn FnOnce() + Send + 'static>>>>;

wrap_task! {
    struct UiJob { job: Job }
    impl Task {
        fn execute(&self) {
            let f = self.job.lock().ok().and_then(|mut g| g.take());   // guard dropped before running f
            if let Some(f) = f { f(); }
        }
    }
}

/// Always async: use it to escape re-entrancy inside CEF callbacks (do_close, on_accelerator, ...).
pub fn post_ui(f: impl FnOnce() + Send + 'static) {
    let mut task = UiJob::new(Arc::new(Mutex::new(Some(Box::new(f)))));
    post_task(ThreadId::UI, Some(&mut task));
}
```

---

## 3. DisplayHandler (`wrap_display_handler!` B:L17698, trait `ImplDisplayHandler` B:L17601)

`cef_display_handler.h`: *"The methods of this class will be called on the UI thread."*

| Method (verbatim) | Line | Notes |
|---|---|---|
| `fn on_address_change(&self, browser: Option<&mut Browser>, frame: Option<&mut Frame>, url: Option<&CefString>)` | B:L17603 | Fires for every frame. Filter with `frame.is_main() != 0` (B:L7557). |
| `fn on_title_change(&self, browser: Option<&mut Browser>, title: Option<&CefString>)` | B:L17611 | |
| `fn on_favicon_urlchange(&self, browser: Option<&mut Browser>, icon_urls: Option<&mut CefStringList>)` | B:L17613 | See the iteration gotcha below. |
| `fn on_fullscreen_mode_change(&self, browser: Option<&mut Browser>, fullscreen: ::std::os::raw::c_int)` | B:L17620 | Alloy: you must do the transition yourself. |
| `fn on_tooltip(&self, browser: Option<&mut Browser>, text: Option<&mut CefString>) -> ::std::os::raw::c_int` | B:L17627 | |
| `fn on_status_message(&self, browser: Option<&mut Browser>, value: Option<&CefString>)` | B:L17635 | Hovered link URL; source is `UpdateTargetURL`. |
| `fn on_console_message(&self, browser: Option<&mut Browser>, level: LogSeverity, message: Option<&CefString>, source: Option<&CefString>, line: ::std::os::raw::c_int) -> ::std::os::raw::c_int` | B:L17637 | *"Return true to stop the message from being output to the console."* |
| `fn on_loading_progress_change(&self, browser: Option<&mut Browser>, progress: f64)` | B:L17656 | *"ranges from 0.0 to 1.0"* |
| `fn on_media_access_change(&self, browser: Option<&mut Browser>, has_video_access: ::std::os::raw::c_int, has_audio_access: ::std::os::raw::c_int)` | B:L17668 | Camera/mic-in-use indicator. **Not** the "playing audio" signal. |
| `fn on_contents_bounds_change(&self, browser: Option<&mut Browser>, new_bounds: Option<&Rect>) -> ::std::os::raw::c_int` | B:L17676 | `window.moveTo`/`resizeTo`. Return 1 in tabs to ignore it. |

**Iterating `CefStringList` (important):**
- `CefStringList` has `new()` (string.rs:L861), `append(&mut self, &str)` (L867), `Default` (L879, which *allocates*) and `impl IntoIterator for CefStringList { type Item = String; ... }` (L937, **by value**).
- The callback gives you `&mut CefStringList`, which is a *borrowed* list. **Don't `.clone()` it.** `CefStringCollection::clone` copies the zero-sized opaque `_cef_string_list_t` into `Borrowed(Some(copy))`, so the pointer then targets a local ZST rather than the CEF list, and you get UB or a crash.
  - VERIFIED-FIX, more precisely: `clone().into_iter()` **silently yields an empty Vec**, because `From<&mut CefStringCollection>` maps `Borrowed` to `None`, so the list pointer is null. Passing `&clone` back to CEF (`From<&CefStringList> for *const`) or `{:?}`-printing it dereferences a pointer to the local ZST copy, which is UB. `_cef_string_list_t` is `{ _unused: [u8; 0] }` (sys L17194).
- **Move it out** with `std::mem::take`: the original `BorrowedMut` is iterated and not freed, and the freshly allocated placeholder is freed by the glue.

```rust
use std::os::raw::c_int;

wrap_display_handler! {
    pub struct StaDisplay { app: Arc<AppShared> }

    impl DisplayHandler {
        fn on_address_change(&self, browser: Option<&mut Browser>, frame: Option<&mut Frame>, url: Option<&CefString>) {
            let (Some(b), Some(f)) = (browser, frame) else { return };
            if f.is_main() == 0 { return; }
            self.app.emit(b.identifier(), TabEvent::Url(url.map(CefString::to_string).unwrap_or_default()));
        }
        fn on_title_change(&self, browser: Option<&mut Browser>, title: Option<&CefString>) {
            let Some(b) = browser else { return };
            self.app.emit(b.identifier(), TabEvent::Title(title.map(CefString::to_string).unwrap_or_default()));
        }
        fn on_favicon_urlchange(&self, browser: Option<&mut Browser>, icon_urls: Option<&mut CefStringList>) {
            let Some(b) = browser else { return };
            let urls: Vec<String> = icon_urls.map(|l| std::mem::take(l).into_iter().collect()).unwrap_or_default();
            self.app.emit(b.identifier(), TabEvent::Favicons(urls));   // then host.download_image(...) §11.6
        }
        fn on_fullscreen_mode_change(&self, browser: Option<&mut Browser>, fullscreen: c_int) {
            let Some(b) = browser else { return };
            let id = b.identifier();
            let app = self.app.clone();
            // Alloy: hide sidebar, let the tab view fill the Window, then Window::set_fullscreen (B:L44274)
            post_ui(move || app_set_tab_fullscreen(&app, id, fullscreen != 0));
        }
        fn on_loading_progress_change(&self, browser: Option<&mut Browser>, progress: f64) {
            if let Some(b) = browser { self.app.emit(b.identifier(), TabEvent::Progress(progress)); }
        }
        fn on_status_message(&self, browser: Option<&mut Browser>, value: Option<&CefString>) {
            if let Some(b) = browser { self.app.emit(b.identifier(), TabEvent::Status(value.map(CefString::to_string).unwrap_or_default())); }
        }
        fn on_console_message(&self, _browser: Option<&mut Browser>, _level: LogSeverity, _message: Option<&CefString>,
                              _source: Option<&CefString>, _line: c_int) -> c_int { 0 }
        fn on_media_access_change(&self, browser: Option<&mut Browser>, has_video_access: c_int, has_audio_access: c_int) {
            if let Some(b) = browser { self.app.emit(b.identifier(), TabEvent::Media { video: has_video_access != 0, audio: has_audio_access != 0 }); }
        }
    }
}
```

### 3.1 Audio: mute, and "is this tab playing?"

- **Mute.** `ImplBrowserHost::set_audio_muted(&self, mute: ::std::os::raw::c_int)` (B:L12742) calls `web_contents->SetAudioMuted(mute)` and works from any thread; it posts to UI internally. `ImplBrowserHost::is_audio_muted(&self) -> ::std::os::raw::c_int` (B:L12744) is *"can only be called on the UI thread"*.
- **Playing indicator.** There is no API for it. `ImplAudioHandler` (B:L13893, macro `wrap_audio_handler!` B:L13930) provides:
  - `fn audio_parameters(&self, browser: Option<&mut Browser>, params: Option<&mut AudioParameters>) -> ::std::os::raw::c_int` (B:L13895). `cef_audio_handler.h`: *"Called on the UI thread … Return true to proceed with audio stream capture, or false to cancel it."*
  - `fn on_audio_stream_started(&self, browser: Option<&mut Browser>, params: Option<&AudioParameters>, channels: ::std::os::raw::c_int)` (B:L13903), called *on a browser audio capture thread*.
  - `fn on_audio_stream_stopped(&self, browser: Option<&mut Browser>)` (B:L13920), called on the UI thread.
- What the CEF source shows:
  - `AlloyBrowserHostImpl::OnAudioStateChanged(bool audible)` → `StartAudioCapturer()` → `GetAudioParameters()`, **each time the tab becomes audible**.
  - The loopback capture uses `const bool mute_source = true;`. Returning 1 therefore **silences the tab** unless you play the PCM back yourself.
  - The capturer stops 2 s after the tab goes inaudible (`kRecentlyAudibleTimeout`).
- VERIFIED-FIX (cef-rs): the Rust trait default for `audio_parameters` is **0** (B:L13899 `Default::default()`), not C++'s `true`. Capture and muting therefore happen only if you explicitly return 1. `StartAudioCapturer()` returns early when `audio_capturer_` already exists; since returning 0 never creates one, `audio_parameters` is re-invoked on **every** audible=true transition.
- **Usable hack:** implement only `audio_parameters`, emit "audio started" for that browser id, and **return 0**. There is no "stopped" edge, so pair it with a renderer-side check (media element `play`/`pause` events over IPC) or a timeout. Treat this as an implementation detail that could change.

---

## 4. LoadHandler (`wrap_load_handler!` B:L21457, `ImplLoadHandler` B:L21414)

| Method (verbatim) | Line |
|---|---|
| `fn on_loading_state_change(&self, browser: Option<&mut Browser>, is_loading: ::std::os::raw::c_int, can_go_back: ::std::os::raw::c_int, can_go_forward: ::std::os::raw::c_int)` | B:L21416 |
| `fn on_load_start(&self, browser: Option<&mut Browser>, frame: Option<&mut Frame>, transition_type: TransitionType)` | B:L21425 |
| `fn on_load_end(&self, browser: Option<&mut Browser>, frame: Option<&mut Frame>, http_status_code: ::std::os::raw::c_int)` | B:L21433 |
| `fn on_load_error(&self, browser: Option<&mut Browser>, frame: Option<&mut Frame>, error_code: Errorcode, error_text: Option<&CefString>, failed_url: Option<&CefString>)` | B:L21441 |

Semantics (`cef_load_handler.h`):
- `OnLoadingStateChange`: *"executed twice -- once when loading is initiated … and once when loading is terminated … before any calls to OnLoadStart and after all calls to OnLoadError and/or OnLoadEnd."* **Use it for the spinner and the back/forward buttons.**
- `OnLoadStart`/`OnLoadEnd`: *"will not be called for same page navigations (fragments, history state, etc.)"*. Filter on `frame.is_main()`.
- `OnLoadError`: *"may be called by itself if before commit"*. `Errorcode::ABORTED` (B:L46173) is also delivered for canceled navigations and downloads, so ignore it.
- With Alloy, cefsimple renders its own error page (it loads a `data:` URI only when `is_alloy_style`). Do the same with `sta://error?...`.
  - VERIFIED-FIX (consistency): §6 suggests blocking `sta://` in web tabs from `on_before_browse`. If you do, allow-list the `neterror` host, or the error page below is cancelled (and `on_load_error(ABORTED)` fires again).

```rust
wrap_load_handler! {
    pub struct StaLoad { app: Arc<AppShared> }
    impl LoadHandler {
        fn on_loading_state_change(&self, browser: Option<&mut Browser>, is_loading: i32, can_go_back: i32, can_go_forward: i32) {
            if let Some(b) = browser {
                self.app.emit(b.identifier(), TabEvent::Loading { is_loading: is_loading != 0, back: can_go_back != 0, fwd: can_go_forward != 0 });
            }
        }
        fn on_load_error(&self, _browser: Option<&mut Browser>, frame: Option<&mut Frame>, error_code: Errorcode,
                         error_text: Option<&CefString>, failed_url: Option<&CefString>) {
            if error_code == Errorcode::ABORTED { return; }
            let Some(frame) = frame else { return };
            if frame.is_main() == 0 { return; }
            let code = sys::cef_errorcode_t::from(error_code) as i32;
            let url = format!("sta://neterror/?code={code}&text={}&url={}",
                              urlencode(&error_text.map(CefString::to_string).unwrap_or_default()),
                              urlencode(&failed_url.map(CefString::to_string).unwrap_or_default()));
            frame.load_url(Some(&CefString::from(url.as_str())));     // ImplFrame::load_url (B:L7548)
        }
    }
}
```

---

## 5. LifeSpanHandler (`wrap_life_span_handler!` B:L20763, `ImplLifeSpanHandler` B:L20710)

The header says methods run *"on the UI thread unless otherwise indicated"*.

```rust
// B:L20712
fn on_before_popup(
    &self,
    browser: Option<&mut Browser>,
    frame: Option<&mut Frame>,
    popup_id: ::std::os::raw::c_int,
    target_url: Option<&CefString>,
    target_frame_name: Option<&CefString>,
    target_disposition: WindowOpenDisposition,
    user_gesture: ::std::os::raw::c_int,
    popup_features: Option<&PopupFeatures>,
    window_info: Option<&mut WindowInfo>,
    client: Option<&mut Option<Client>>,
    settings: Option<&mut BrowserSettings>,
    extra_info: Option<&mut Option<DictionaryValue>>,
    no_javascript_access: Option<&mut ::std::os::raw::c_int>,
) -> ::std::os::raw::c_int
// B:L20731
fn on_before_popup_aborted(&self, browser: Option<&mut Browser>, popup_id: ::std::os::raw::c_int)
// B:L20738
fn on_before_dev_tools_popup(&self, browser: Option<&mut Browser>, window_info: Option<&mut WindowInfo>,
    client: Option<&mut Option<Client>>, settings: Option<&mut BrowserSettings>,
    extra_info: Option<&mut Option<DictionaryValue>>, use_default_window: Option<&mut ::std::os::raw::c_int>)
fn on_after_created(&self, browser: Option<&mut Browser>)                       // B:L20749
fn do_close(&self, browser: Option<&mut Browser>) -> ::std::os::raw::c_int      // B:L20751
fn on_before_close(&self, browser: Option<&mut Browser>)                        // B:L20755
```

**`WindowOpenDisposition`** (B:L46974). Constants: `UNKNOWN`, `CURRENT_TAB` (B:L46999), `SINGLETON_TAB`, `NEW_FOREGROUND_TAB` (B:L47003), `NEW_BACKGROUND_TAB` (B:L47006), `NEW_POPUP` (B:L47009), `NEW_WINDOW` (B:L47011), `SAVE_TO_DISK`, `OFF_THE_RECORD`, `IGNORE_ACTION`, `SWITCH_TO_TAB` (B:L47019), `NEW_PICTURE_IN_PICTURE` (B:L47021). It derives `PartialEq, Eq` and has `get_raw()`.
- VERIFIED-FIX: the list was incomplete. Chromium 152 also has `NEW_SPLIT_VIEW` (B:L47024) and `NUM_VALUES`. `NEW_SPLIT_VIEW` is relevant to sta's split view; handle it in `on_open_urlfrom_tab`/`on_before_popup`.

**`PopupFeatures`** (B:L1212) has fields `x, x_set, y, y_set, width, width_set, height, height_set, is_popup`, all `c_int` (VERIFIED-FIX: plus a leading `size: usize`).

Header semantics (`cef_life_span_handler.h`):
- *"To allow creation of the popup browser optionally modify |windowInfo|, |client|, |settings| and |no_javascript_access| and return false. To cancel creation of the popup browser return true."*
- *"Any modifications to |windowInfo| will be ignored if the parent browser is wrapped in a CefBrowserView."*
- *"A default popup window is created if this method returns false … without implementing CefBrowserViewDelegate::OnPopupBrowserViewCreated (for Views-hosted popups)."*
- `OnBeforePopupAborted`: only fires if the popup was allowed and creation then failed. Clear pending-popup state there, in `OnAfterCreated` of the popup, or in `OnBeforeClose` of the opener.

**cef-rs glue gotcha (B:L20855-20906).** The out-client is written back only `if let (Some(out_client), Some(wrap_client))`.
- **Setting `*client = None` does not clear the client.** The popup keeps the opener's client.
- Replacing it with `Some(other)` works.
- cefclient returns default handling for PiP with `client = nullptr`. In Rust just `return 0` without touching `client`.

### 5.1 Popups: open as a new sta tab

**Strategy A: cancel the popup and open our own tab.** Simple, but there is **no `window.opener`**. That is fine for target=_blank, which is `noopener` by default in modern Chromium, but it breaks OAuth-style `window.open` + `postMessage`.

**Strategy B (recommended default): allow the popup and adopt the popup BrowserView as a tab.** The opener relationship and shared process/context are preserved (`cef_request_context.h`: *"Browser objects created indirectly via the JavaScript window.open function or targeted links will share the same render process and the same request context as the source browser."*).

**Callback order in 152** (`alloy_browser_host_impl.cc` `Create()`; VERIFIED-FIX: the function is `AlloyBrowserHostImpl::CreateInternal()`, reached for popups from `WebContentsCreated()`):
1. `opener->platform_delegate_->PopupBrowserCreated(...)` → `OnPopupBrowserViewCreated`, *"Do this first for consistency with Chrome style"*
2. `OnAfterCreated`
3. `OnBrowserCreated`

The header text says `OnPopupBrowserViewCreated` comes *after* `OnAfterCreated`. **Don't depend on the order.** Register the role inside `on_popup_browser_view_created`, where `popup_browser_view.browser()` is already valid.

```rust
wrap_life_span_handler! {
    pub struct StaLifeSpan { app: Arc<AppShared> }

    impl LifeSpanHandler {
        fn on_before_popup(&self, browser: Option<&mut Browser>, _frame: Option<&mut Frame>, _popup_id: i32,
                           target_url: Option<&CefString>, _target_frame_name: Option<&CefString>,
                           target_disposition: WindowOpenDisposition, user_gesture: i32,
                           popup_features: Option<&PopupFeatures>, _window_info: Option<&mut WindowInfo>,
                           _client: Option<&mut Option<Client>>, _settings: Option<&mut BrowserSettings>,
                           _extra_info: Option<&mut Option<DictionaryValue>>, _no_javascript_access: Option<&mut i32>) -> i32 {
            let Some(opener) = browser else { return 1 };
            let d = target_disposition;
            if d == WindowOpenDisposition::NEW_PICTURE_IN_PICTURE { return 0; }       // CEF's default PiP window
            // Popups from our own UI views (sidebar/command bar) never create browsers: route to a new tab.
            if !matches!(self.app.role_of(opener.identifier()), Some(Role::Tab(_))) {
                let url = target_url.map(CefString::to_string).unwrap_or_default();
                let app = self.app.clone();
                post_ui(move || { open_tab(&app, &url, None, true); });
                return 1;                                                                  // cancel
            }
            // Strategy B: allow. Remember how to present it (tab vs little window) for on_popup_browser_view_created.
            let as_window = d == WindowOpenDisposition::NEW_POPUP
                && popup_features.map(|f| f.is_popup != 0).unwrap_or(false);
            remember_popup_presentation(&self.app, opener.identifier(), as_window, d == WindowOpenDisposition::NEW_BACKGROUND_TAB, user_gesture != 0);
            0
        }

        fn on_after_created(&self, browser: Option<&mut Browser>) {
            // Roles are registered by the BrowserView delegate (§2.2). Here: optional bookkeeping only.
            let _ = browser;
        }

        fn do_close(&self, browser: Option<&mut Browser>) -> i32 { tab_do_close(&self.app, browser) }      // §5.3
        fn on_before_close(&self, browser: Option<&mut Browser>) { tab_on_before_close(&self.app, browser) } // §5.3
    }
}

/// Called from TabViewDelegate::on_popup_browser_view_created (UI thread, synchronous).
fn adopt_popup_as_tab(app: &Arc<AppShared>, opener_id: i32, popup_view: BrowserView) -> i32 {
    let Some(browser) = popup_view.browser() else { return 0 };
    if take_popup_presentation_as_window(app, opener_id) { return 0; }   // let CEF create a default popup Window
    let tab = next_tab_id();
    let panel = {
        let mut s = app.state.lock().unwrap();
        s.roles.insert(browser.identifier(), Role::Tab(tab));
        s.tabs.insert(tab, TabEntry { view: popup_view.clone(), browser: Some(browser), space: String::new() });
        s.content_panel.clone()
    };
    let Some(panel) = panel else { return 0 };
    panel.add_child_view(Some(&mut View::from(&popup_view)));
    1                                                                     // "we added it to the hierarchy"
}
```

To use Strategy A for a disposition, use the "own UI views" branch above: `post_ui(open_tab...)` and `return 1`. Never create browsers synchronously inside `on_before_popup`.

### 5.2 DevTools popups

- `on_before_dev_tools_popup` **is** called for Alloy openers in 152. `chrome_browser_delegate.cc` invokes it whenever the opener client has a LifeSpanHandler, and the DevTools browser itself is always Chrome style.
  - VERIFIED: source confirms this even though `cef_life_span_handler.h` says *"Only used with Chrome style."*
  - `browser_view_impl.cc` `ComputeAlloyStyle()` forces Chrome style for DevTools popups and logs an error if the delegate asks for Alloy.
- `use_default_window` starts as `!life_span_handler`, i.e. **0**, so the DevTools window is **Views-hosted**. `BrowserViewDelegate::on_popup_browser_view_created(.., is_devtools = 1)` is then called on the inspected BrowserView's delegate:
  - Return **0** to let CEF open a top-level Window.
  - Only a Chrome-style Window can dock it, and only one at a time; an Alloy-style Window **cannot**.
  - VERIFIED-FIX: even a Chrome-style Window refuses it once **any** BrowserView (Alloy included) has been added, because `ChromeBrowserWidget::GetThemeProfile()` returns the Alloy views' associated profile and `AddedToWidget` then logs "Cannot add multiple Chrome style BrowserViews". In practice DevTools must live in its own Window.
- The DevTools browser is Chrome style, so it gets Chrome accelerators and `CommandHandler::on_chrome_command`, and it does **not** get `do_close`.
- Give it a dedicated client (§11.3), either via `show_dev_tools` or here:

```rust
fn on_before_dev_tools_popup(&self, _browser: Option<&mut Browser>, _window_info: Option<&mut WindowInfo>,
                             client: Option<&mut Option<Client>>, _settings: Option<&mut BrowserSettings>,
                             _extra_info: Option<&mut Option<DictionaryValue>>, _use_default_window: Option<&mut i32>) {
    if let (Some(c), Some(dt)) = (client, devtools_client()) { *c = Some(dt); }
}
```

### 5.3 Correct close sequence for a BrowserView-hosted tab (Alloy)

APIs:
- `ImplBrowserHost::close_browser(&self, force_close: ::std::os::raw::c_int)` (B:L12544)
- `try_close_browser(&self) -> ::std::os::raw::c_int` (B:L12546)
- `is_ready_to_be_closed(&self) -> ::std::os::raw::c_int` (B:L12548)
- `ImplPanel::remove_child_view(&self, view: Option<&mut View>)` (B:L41658)
- `pub fn quit_message_loop()` (B:L58318)

Behavior verified in CEF source at this commit:

1. `AlloyBrowserHostImpl::CloseBrowser(false)`: sets `DESTRUCTION_STATE_PENDING`, dispatches `beforeunload` (default dialog, or `JsdialogHandler::on_before_unload_dialog`), then calls `CloseContents()`.
2. `CloseContents()`: `close_browser = !handler->DoClose(this)`. If that is true and the window isn't destroyed, it calls `platform_delegate_->CloseHostWindow()`. For Views that is `widget->Close()`, i.e. **the whole sta Window** goes through `WindowDelegate::can_close`.
3. If `do_close` **returns 1**, nothing more happens and the browser waits in `PENDING` state.
   - VERIFIED-FIX: it does **not** stay `PENDING`. `CloseContents()` ends with `} else if (destruction_state_ != DESTRUCTION_STATE_NONE) { destruction_state_ = DESTRUCTION_STATE_NONE; }`, so the browser returns to state `NONE`, fully alive, whether you called `close_browser(0)` or `close_browser(1)`. The page has already run unload: `WebContentsImpl::NeedToFireBeforeUnloadOrUnloadEvents()` returns false once `IsPageReadyToBeClosed()`, so it won't prompt again.
4. Remove the BrowserView from its panel and release the last reference. `~CefBrowserViewImpl()` runs `if (browser_ && !browser_->WillBeDestroyed()) browser->WindowDestroyed();`, which leads to `CloseBrowser(true)`, then `CloseContents` (`DoClose` is skipped because `window_destroyed_`), then `DestroyBrowser()`, then `on_browser_destroyed`, then **`on_before_close`**. This runs **synchronously during the drop** unless a beforeunload still has to be dispatched.
   - VERIFIED-FIX: `on_browser_destroyed` is **skipped** here. The destructor first calls `weak_ptr_factory_.InvalidateWeakPtrs()`, and `CefBrowserPlatformDelegateViews::NotifyBrowserDestroyed()` checks the `browser_view_` WeakPtr. The actual order is `DestroyBrowser()` → WebContents destroyed → `CefBrowserHostBase::DestroyWebContents()` → (no delegate call) → `on_before_close`.
5. **Leak trap:** after `close_browser(1)`, the state is `ACCEPTED`, so `WillBeDestroyed()` is true. The destructor then skips `WindowDestroyed()` and the browser is never destroyed. This is the failure mode of open issue #3376: "OnBeforeClose() method will not be called".
   - VERIFIED-FIX: wrong as stated. By the time the posted detach runs, `do_close`→1 has reset the state to `NONE`, so `close_browser(1)` + `do_close`→1 + posted detach works. The actual traps:
     - (a) #3376's repro (`CloseBrowser(false)`, detach in `DoClose`, return true) never releases the last BrowserView reference. Removing a view does not destroy it.
     - (b) Releasing the last reference *synchronously inside* `do_close` re-enters `CloseContents`/`DestroyBrowser`, then the outer `CloseContents` rewrites state on a destroyed browser. Always post.
     - (c) Releasing the last reference while state is `ACCEPTED` and the close is still in flight (between `close_browser(1)` and `do_close`) makes the destructor skip `WindowDestroyed()`. The later `do_close`→1 orphans the browser; `do_close`→0 calls `CloseHostWindow()` → `GetWindowWidget()`, which dereferences the dead `browser_view_` WeakPtr.
     - Consequence: never detach a tab whose close is already in progress. Wait for `do_close`.
6. `window.close()` from JS calls `CloseContents` → `do_close` directly, with state still `NONE`, so the same path works.
7. The canonical pattern is cefclient `views_overlay_browser.cc`: *"We hold the last reference to the BrowserView, and releasing it will trigger overlay Browser destruction. OnBeforeClose for that Browser may be called synchronously or asynchronously depending on whether beforeunload needs to be dispatched."*

```rust
/// UI → "close tab" (honors beforeunload)
pub fn request_close_tab(app: &Arc<AppShared>, tab: TabId) {
    let (host, view_only) = {
        let s = app.state.lock().unwrap();
        match s.tabs.get(&tab) { Some(t) => (t.browser.as_ref().and_then(|b| b.host()), t.browser.is_none()), None => return }
    };
    match host { Some(h) => h.close_browser(0), None if view_only => detach_tab_view(app, tab), None => {} }
}

pub fn tab_do_close(app: &Arc<AppShared>, browser: Option<&mut Browser>) -> i32 {
    let Some(b) = browser else { return 0 };
    match app.role_of(b.identifier()) {
        Some(Role::Tab(tab)) => { let app = app.clone(); post_ui(move || detach_tab_view(&app, tab)); 1 } // NOT re-entrant
        Some(Role::Sidebar) | Some(Role::CommandBar) => 1,   // UI window.close() must never close the main Window
        _ => 0,                                              // popups/devtools living in CEF-created windows: default
    }
}

pub fn detach_tab_view(app: &Arc<AppShared>, tab: TabId) {
    let (panel, entry) = {
        let mut s = app.state.lock().unwrap();
        (s.content_panel.clone(), s.tabs.remove(&tab))
    };                                                          // lock released: drop below re-enters handlers
    let (Some(panel), Some(entry)) = (panel, entry) else { return };
    let TabEntry { view, browser, .. } = entry;
    drop(browser);
    panel.remove_child_view(Some(&mut View::from(&view)));
    drop(view);   // must be the LAST BrowserView ref (no clones in delegates/closures) → on_before_close
}

pub fn tab_on_before_close(app: &Arc<AppShared>, browser: Option<&mut Browser>) {
    let Some(b) = browser else { return };
    let quit = {
        let mut s = app.state.lock().unwrap();
        s.roles.remove(&b.identifier());
        s.shutting_down && s.roles.is_empty()
    };
    if quit { quit_message_loop(); }
}
```

- **Force-close a tab without prompts:** call `detach_tab_view` directly, without `close_browser`. If a `beforeunload` handler still exists, the forced destroy path dispatches it (`CloseBrowser(true)` still calls `DispatchBeforeUnload`, and `BeforeUnloadFired` proceeds when state is `ACCEPTED`). To be safe, auto-accept in `JsdialogHandler::on_before_unload_dialog` while a forced close is in progress: `callback.cont(1, None); return 1`.
  - VERIFIED-FIX (stronger than "to be safe"): only do this when no close is already pending (see trap (c) above).
  - When the page has beforeunload/unload handlers, `on_before_close` arrives **asynchronously**, after the view is gone.
  - A default Chrome tab-modal beforeunload dialog would need `GetWebContentsModalDialogHost()`. `CefBrowserPlatformDelegateViews::GetDialogPosition()` dereferences `browser_view_` without a null check, and that WeakPtr is already invalid here. So implementing the auto-accepting `on_before_unload_dialog` is **required** on this path, not optional. This is a source inference, not runtime-tested.
- **Closing the main Window** in `WindowDelegate::can_close` (B:L43253):
  - Simplest: set `shutting_down = true`, `close_browser(1)` any extra top-level popup/DevTools browsers, and **return 1**. Views tear-down (`CefBrowserViewImpl::Detach()` → `WindowDestroyed()`) destroys every child browser, and `on_before_close` for the last one calls `quit_message_loop()`.
  - Clear your `AppState` BrowserView/Window references in `on_window_destroyed` to break cycles, as cefsimple does.
  - To honor beforeunload on app exit: return 0, `close_browser(0)` every tab, and call `window.close()` again once the tabs are gone.

---

## 6. RequestHandler (`wrap_request_handler!` B:L26882, `ImplRequestHandler` B:L26779)

| Method (verbatim) | Line | Thread |
|---|---|---|
| `fn on_before_browse(&self, browser: Option<&mut Browser>, frame: Option<&mut Frame>, request: Option<&mut Request>, user_gesture: ::std::os::raw::c_int, is_redirect: ::std::os::raw::c_int) -> ::std::os::raw::c_int` | B:L26781 | UI |
| `fn on_open_urlfrom_tab(&self, browser: Option<&mut Browser>, frame: Option<&mut Frame>, target_url: Option<&CefString>, target_disposition: WindowOpenDisposition, user_gesture: ::std::os::raw::c_int) -> ::std::os::raw::c_int` | B:L26792 | UI |
| `fn resource_request_handler(&self, browser: Option<&mut Browser>, frame: Option<&mut Frame>, request: Option<&mut Request>, is_navigation: ::std::os::raw::c_int, is_download: ::std::os::raw::c_int, request_initiator: Option<&CefString>, disable_default_handling: Option<&mut ::std::os::raw::c_int>) -> Option<ResourceRequestHandler>` | B:L26803 | **IO** (skip; leave default None) |
| `fn auth_credentials(...)` | B:L26816 | **IO** |
| `fn on_certificate_error(&self, browser: Option<&mut Browser>, cert_error: Errorcode, request_url: Option<&CefString>, ssl_info: Option<&mut Sslinfo>, callback: Option<&mut Callback>) -> ::std::os::raw::c_int` | B:L26830 | UI |
| `fn on_render_process_unresponsive(&self, browser: Option<&mut Browser>, callback: Option<&mut UnresponsiveProcessCallback>) -> ::std::os::raw::c_int` | B:L26855 | UI |
| `fn on_render_process_terminated(&self, browser: Option<&mut Browser>, status: TerminationStatus, error_code: ::std::os::raw::c_int, error_string: Option<&CefString>)` | B:L26865 | UI |

Semantics:
- `on_before_browse`: *"Return true to cancel the navigation … If the navigation is canceled CefLoadHandler::OnLoadError will be called with an |errorCode| value of ERR_ABORTED."* Use it to block `sta://` inside web tabs, or to hand `mailto:` etc. to `ShellExecuteW`.
- `on_open_urlfrom_tab`: *"Called … before OnBeforeBrowse in certain limited cases … links clicked via middle-click or ctrl + left-click … Return true to cancel the navigation or false to allow the navigation to proceed in the source browser's top-level frame."* The CEF source (`browser_contents_delegate.cc` `OpenURLFromTabEx`) confirms that if the call is not canceled, **Alloy loads it in the current tab whatever the disposition**.
- `on_render_process_terminated`: `TerminationStatus` constants are `ABNORMAL_TERMINATION` (B:L46018), `PROCESS_WAS_KILLED`, `PROCESS_CRASHED`, `PROCESS_OOM`, `LAUNCH_FAILED`, `INTEGRITY_FAILURE`. Alloy has no sad-tab UI: show your own and offer `browser.reload()` (B:L11719).

```rust
wrap_request_handler! {
    pub struct StaRequest { app: Arc<AppShared> }
    impl RequestHandler {
        fn on_open_urlfrom_tab(&self, _browser: Option<&mut Browser>, _frame: Option<&mut Frame>,
                               target_url: Option<&CefString>, target_disposition: WindowOpenDisposition, _user_gesture: i32) -> i32 {
            let d = target_disposition;
            let new_tab = d == WindowOpenDisposition::NEW_FOREGROUND_TAB || d == WindowOpenDisposition::NEW_BACKGROUND_TAB
                       || d == WindowOpenDisposition::NEW_WINDOW || d == WindowOpenDisposition::NEW_POPUP
                       || d == WindowOpenDisposition::OFF_THE_RECORD;
            if !new_tab { return 0; }                      // CURRENT_TAB etc.: proceed in this tab
            let url = target_url.map(CefString::to_string).unwrap_or_default();
            let fg = d != WindowOpenDisposition::NEW_BACKGROUND_TAB;
            let app = self.app.clone();
            post_ui(move || { open_tab(&app, &url, None, fg); });   // TODO: same space/RequestContext as source tab
            1                                                        // cancel in source tab
        }
        fn on_render_process_terminated(&self, browser: Option<&mut Browser>, status: TerminationStatus, error_code: i32, _error_string: Option<&CefString>) {
            if let Some(b) = browser { notify_crashed(&self.app, b.identifier(), status, error_code); }
        }
    }
}
```

---

## 7. Keyboard: KeyboardHandler vs Views accelerators

### 7.1 KeyboardHandler (`wrap_keyboard_handler!` B:L20497, `ImplKeyboardHandler` B:L20470)

```rust
// B:L20472
fn on_pre_key_event(&self, browser: Option<&mut Browser>, event: Option<&KeyEvent>, os_event: Option<&mut MSG>,
                    is_keyboard_shortcut: Option<&mut ::std::os::raw::c_int>) -> ::std::os::raw::c_int
// B:L20482
fn on_key_event(&self, browser: Option<&mut Browser>, event: Option<&KeyEvent>, os_event: Option<&mut MSG>) -> ::std::os::raw::c_int
```

- `cef_keyboard_handler.h`: *"Called before a keyboard event is sent to the renderer … Return true if the event was handled … If the event will be handled in OnKeyEvent() as a keyboard shortcut set |is_keyboard_shortcut| to true and return false."* In the CEF source that maps to `NOT_HANDLED_IS_SHORTCUT`: the renderer still sees the keydown, and the following Char is suppressed if the page doesn't handle it.
- `OnKeyEvent`: *"Called after the renderer and JavaScript in the page has had a chance to handle the event."*
- `KeyEvent` (B:L1155, `#[derive(Clone, Debug)]`) fields:
  - `size: usize`
  - `type_: KeyEventType`
  - `modifiers: u32`
  - `windows_key_code: c_int`
  - `native_key_code: c_int`
  - `is_system_key: c_int` (WM_SYSKEY*, e.g. Alt combos)
  - `character: char16_t`
  - `unmodified_character: char16_t`
  - `focus_on_editable_field: c_int`
- `KeyEventType` constants: `RAWKEYDOWN` (B:L48314), `KEYDOWN` (B:L48316), `KEYUP` (B:L48318), `CHAR` (B:L48320). On Windows, shortcut matching should use `RAWKEYDOWN`, and you must also swallow the following `CHAR`.
- **Modifier constants.** The Rust `EventFlags` newtype (B:L48044) has **no associated constants**. Use sys, where the values are `c_int`: `sys::cef_event_flags_t::EVENTFLAG_SHIFT_DOWN` (2, S:L19664), `EVENTFLAG_CONTROL_DOWN` (4, S:L19665), `EVENTFLAG_ALT_DOWN` (8, S:L19666), `EVENTFLAG_COMMAND_DOWN` (128), `EVENTFLAG_ALTGR_DOWN` (4096), `EVENTFLAG_IS_REPEAT` (8192, S:L19683).
- **Where the handler runs.** It lives on the browser's Client, so it only sees keys aimed at *that* browser. The CEF source path is `AlloyBrowserHostImpl::HandleKeyboardEvent` → `OnKeyEvent` → `CefBrowserViewImpl::HandleKeyboardEvent` → `HandleAccelerator` (Window accelerators) → `WindowDelegate::on_key_event`.

```rust
use cef::sys::{self, MSG};

wrap_keyboard_handler! {
    pub struct StaKeyboard { app: Arc<AppShared> }
    impl KeyboardHandler {
        fn on_pre_key_event(&self, browser: Option<&mut Browser>, event: Option<&KeyEvent>, _os_event: Option<&mut MSG>,
                            _is_keyboard_shortcut: Option<&mut i32>) -> i32 {
            let (Some(b), Some(ev)) = (browser, event) else { return 0 };
            const VK_ESCAPE: i32 = 0x1B;
            let ctrl = (ev.modifiers & sys::cef_event_flags_t::EVENTFLAG_CONTROL_DOWN.0 as u32) != 0;
            let _ = ctrl;
            if ev.type_ == KeyEventType::RAWKEYDOWN && ev.windows_key_code == VK_ESCAPE {
                if let Some(host) = b.host() {
                    if host.is_fullscreen() != 0 {         // B:L12746 (UI thread)
                        host.exit_fullscreen(1);            // B:L12748 — Alloy must do this itself
                        return 1;
                    }
                }
            }
            0
        }
    }
}
```

### 7.2 Views accelerators

- `ImplWindow::set_accelerator(&self, command_id: ::std::os::raw::c_int, key_code: ::std::os::raw::c_int, shift_pressed: ::std::os::raw::c_int, ctrl_pressed: ::std::os::raw::c_int, alt_pressed: ::std::os::raw::c_int, high_priority: ::std::os::raw::c_int)` (B:L44331)
- `remove_accelerator(&self, command_id: ::std::os::raw::c_int)` (B:L44341), `remove_all_accelerators(&self)` (B:L44343)
- `ImplWindowDelegate::on_accelerator(&self, window: Option<&mut Window>, command_id: ::std::os::raw::c_int) -> ::std::os::raw::c_int` (B:L43257)
- `ImplWindowDelegate::on_key_event(&self, window: Option<&mut Window>, event: Option<&KeyEvent>) -> ::std::os::raw::c_int` (B:L43265)
- `ImplBrowserView::set_prefer_accelerators(&self, prefer_accelerators: ::std::os::raw::c_int)` (B:L39164)

Header semantics:
- `views/cef_window.h`: *"|key_code| can be any virtual key or character value … The |high_priority| value will be considered if a child CefBrowserView has focus … If |high_priority| is true then the key event will not be forwarded to the web content (`keydown` event handler) or CefKeyboardHandler first. If |high_priority| is false then the behavior will depend on the CefBrowserView::SetPreferAccelerators configuration."*
- `views/cef_browser_view.h`: *"If |prefer_accelerators| is false then the matching accelerator will only be triggered if the event is not handled by web content (`keydown` event handler that calls `event.preventDefault()`) or by CefKeyboardHandler. The default value is false."*

Verified in CEF source `window_impl.cc`:
- `accelerator_map_` is keyed by **command_id**; `SetAccelerator` with an existing id first calls `RemoveAccelerator(command_id)`. **One key combo per command id**, so F5 and Ctrl+R need different ids.
- Accelerators register with `widget_->GetFocusManager()` at `kHighPriority` or `kNormalPriority`.
- The call returns silently `if (!widget_)`, so **call it in `on_window_created`**.
- cefclient uses `SetAccelerator(ID_POPOUT_OVERLAY, 'O', shift, ctrl, …, /*high_priority=*/true)` to toggle an overlay BrowserView.

### 7.3 Recommendation

**Primary: Window accelerators.** They fire whether focus is in a web tab, the sidebar BrowserView or the command-bar BrowserView, because they belong to one FocusManager per top-level widget. Leave `set_prefer_accelerators` at the default `0` on tabs. You may set it to `1` on the internal UI BrowserViews.

| Shortcut | key_code | shift, ctrl, alt | high_priority | Rationale |
|---|---|---|---|---|
| Ctrl+T new tab | `'T' as i32` (0x54) | 0,1,0 | **1** | reserved in Chrome |
| Ctrl+W close tab | `'W'` | 0,1,0 | **1** | reserved |
| Ctrl+Shift+T reopen | `'T'` | 1,1,0 | **1** | reserved |
| Ctrl+Tab / Ctrl+Shift+Tab | `0x09` | 0/1,1,0 | **1** | reserved |
| Ctrl+1…9 | `0x31…0x39` | 0,1,0 | **1** | 9 ids |
| Ctrl+L command bar | `'L'` | 0,1,0 | 1 (Arc-like) | |
| Ctrl+S save | `'S'` | 0,1,0 | 0 | pages (Docs) override |
| Ctrl+F find | `'F'` | 0,1,0 | 0 | |
| F5 / Ctrl+R reload | `0x74` / `'R'` | 0,0,0 / 0,1,0 | 0 | 2 ids |
| F12 / Ctrl+Shift+I DevTools | `0x7B` / `'I'` | … | 0 | |
| Ctrl+Shift+C inspect | `'C'` | 1,1,0 | 0 | |
| Alt+← / Alt+→ | `0x25` / `0x27` | 0,0,1 | 0 | |

```rust
#[repr(i32)] #[derive(Clone, Copy)]
pub enum Cmd { NewTab = 1000, CloseTab, ReopenTab, NextTab, PrevTab, CommandBar, Save, Find, ReloadF5, ReloadCtrlR,
               DevToolsF12, DevToolsCtrlShiftI, Inspect, Back, Forward, Tab1 = 1100 /* ..=1108 */ }

pub fn install_accelerators(w: &Window) {
    use Cmd::*;
    let t: &[(Cmd, i32, i32, i32, i32, i32)] = &[
        (NewTab, 'T' as i32, 0,1,0, 1), (CloseTab, 'W' as i32, 0,1,0, 1), (ReopenTab, 'T' as i32, 1,1,0, 1),
        (NextTab, 0x09, 0,1,0, 1), (PrevTab, 0x09, 1,1,0, 1), (CommandBar, 'L' as i32, 0,1,0, 1),
        (Save, 'S' as i32, 0,1,0, 0), (Find, 'F' as i32, 0,1,0, 0), (ReloadF5, 0x74, 0,0,0, 0),
        (ReloadCtrlR, 'R' as i32, 0,1,0, 0), (DevToolsF12, 0x7B, 0,0,0, 0), (DevToolsCtrlShiftI, 'I' as i32, 1,1,0, 0),
        (Inspect, 'C' as i32, 1,1,0, 0), (Back, 0x25, 0,0,1, 0), (Forward, 0x27, 0,0,1, 0),
    ];
    for &(id, key, shift, ctrl, alt, hi) in t { w.set_accelerator(id as i32, key, shift, ctrl, alt, hi); }
    for n in 0..9 { w.set_accelerator(Cmd::Tab1 as i32 + n, 0x31 + n, 0, 1, 0, 1); }
}

wrap_window_delegate! {
    pub struct MainWindowDelegate { app: Arc<AppShared> }
    impl ViewDelegate {}
    impl PanelDelegate {}
    impl WindowDelegate {
        fn on_window_created(&self, window: Option<&mut Window>) {
            let Some(window) = window else { return };
            install_accelerators(window);            // needs widget_ → only valid from here on
            /* build sidebar/content/overlay views ... */
        }
        fn on_accelerator(&self, _window: Option<&mut Window>, command_id: i32) -> i32 {
            let app = self.app.clone();
            post_ui(move || run_command(&app, command_id));   // avoid re-entering Views/CEF inside dispatch
            1
        }
        fn can_close(&self, _window: Option<&mut Window>) -> i32 { begin_app_shutdown(&self.app); 1 }
        // VERIFIED-FIX: Rust defaults are 0 (B:L43240-43250) whereas C++ defaults are true;
        // without these overrides the main Window cannot be resized, maximized or minimized.
        fn can_resize(&self, _window: Option<&mut Window>) -> i32 { 1 }
        fn can_maximize(&self, _window: Option<&mut Window>) -> i32 { 1 }
        fn can_minimize(&self, _window: Option<&mut Window>) -> i32 { 1 }
        fn window_runtime_style(&self) -> RuntimeStyle { RuntimeStyle::ALLOY }
    }
}
```

Caveats:
- Verify Ctrl+Tab on the target build. If Views swallows it, fall back to `on_pre_key_event` with `RAWKEYDOWN`, `0x09` and CONTROL, returning 1.
- DevTools and default popup Windows are separate widgets with no accelerators unless you add them.
- The DevTools window is Chrome style, so Chrome's own shortcuts there go to `CommandHandler::on_chrome_command` (B:L14206); return 1 to block them.

---

## 8. ContextMenuHandler (`wrap_context_menu_handler!` B:L16366, `ImplContextMenuHandler` B:L16301)

```rust
// B:L16303
fn on_before_context_menu(&self, browser: Option<&mut Browser>, frame: Option<&mut Frame>,
                          params: Option<&mut ContextMenuParams>, model: Option<&mut MenuModel>)
// B:L16323
fn on_context_menu_command(&self, browser: Option<&mut Browser>, frame: Option<&mut Frame>,
                           params: Option<&mut ContextMenuParams>, command_id: ::std::os::raw::c_int,
                           event_flags: EventFlags) -> ::std::os::raw::c_int
// B:L16312 (custom rendering, e.g. HTML menu in overlay)
fn run_context_menu(&self, browser: Option<&mut Browser>, frame: Option<&mut Frame>, params: Option<&mut ContextMenuParams>,
                    model: Option<&mut MenuModel>, callback: Option<&mut RunContextMenuCallback>) -> ::std::os::raw::c_int
fn on_context_menu_dismissed(&self, browser: Option<&mut Browser>, frame: Option<&mut Frame>)   // B:L16334
```

Header semantics (`cef_context_menu_handler.h`):
- *"|model| initially contains the default context menu. The |model| can be cleared to show no context menu or modified to show a custom menu. Do not keep references to |params| or |model| outside of this callback."*
- *"All user-defined command ids should be between MENU_ID_USER_FIRST and MENU_ID_USER_LAST."*
- `ContextMenuParams` and `MenuModel`: *"can only be accessed on browser process the UI thread."*
- `RunContextMenuCallback::cont(&self, command_id: ::std::os::raw::c_int, event_flags: EventFlags)` (B:L16153) and `cancel(&self)` (B:L16155) handle a custom menu UI.

`ImplContextMenuParams` (B:L16932) methods:
- `xcoord` (B:L16934), `ycoord` (B:L16936)
- `type_flags(&self) -> ContextMenuTypeFlags` (B:L16938)
- `link_url(&self) -> CefStringUserfree` (B:L16940), `unfiltered_link_url` (B:L16942)
- `source_url(&self) -> CefStringUserfree` (B:L16944)
- `has_image_contents` (B:L16946), `title_text` (B:L16948), `page_url` (B:L16950), `frame_url` (B:L16952)
- `media_type(&self) -> ContextMenuMediaType` (B:L16956)
- `selection_text(&self) -> CefStringUserfree` (B:L16960), `misspelled_word` (B:L16962)
- `dictionary_suggestions(&self, suggestions: Option<&mut CefStringList>) -> c_int` (B:L16964)
- `is_editable` (B:L16969), `edit_state_flags` (B:L16973)

Converting a `CefStringUserfree` (alias B:L39) to a `String`: `CefString::from(&params.link_url()).to_string()`, using `impl From<&CefStringUserfreeUtf16> for CefStringUtf16` (string.rs:L504).

`ImplMenuModel` (B:L14853) methods:
- `clear(&self) -> c_int` (B:L14857), `count(&self) -> usize` (B:L14859)
- `add_separator(&self) -> c_int` (B:L14861)
- `add_item(&self, command_id: ::std::os::raw::c_int, label: Option<&CefString>) -> ::std::os::raw::c_int` (B:L14863)
- `add_check_item` (B:L14869), `add_sub_menu(&self, command_id, label) -> Option<MenuModel>` (B:L14882)
- `insert_item_at` (B:L14890), `remove(&self, command_id) -> c_int` (B:L14919), `index_of` (B:L14923)
- `set_enabled(&self, command_id, enabled) -> c_int` (B:L14986), `set_visible` (B:L14973)
- `set_accelerator` (B:L15012)

Constants:
- `MenuId::USER_FIRST` (B:L47850) and `USER_LAST` (B:L47852) wrap `MENU_ID_USER_FIRST = 26500` and `MENU_ID_USER_LAST = 28500` (S:L19544-19545).
- `get_raw()` is not `const`; in a `const` use `sys::cef_menu_id_t::MENU_ID_USER_FIRST as i32`.
- `ContextMenuTypeFlags` has no Rust constants. Use `sys::cef_context_menu_type_flags_t::CM_TYPEFLAG_LINK` (etc.) together with `sys::cef_context_menu_type_flags_t::from(flags).0`.
- `ContextMenuMediaType::IMAGE` (B:L48176) and the rest (`NONE`, `VIDEO`, `AUDIO`, `CANVAS`, `FILE`, `PLUGIN`) derive `PartialEq`.

```rust
const CM_BASE: i32 = sys::cef_menu_id_t::MENU_ID_USER_FIRST as i32;
const CM_OPEN_LINK_NEW_TAB: i32 = CM_BASE + 1;
const CM_COPY_LINK: i32 = CM_BASE + 2;
const CM_SAVE_IMAGE: i32 = CM_BASE + 3;
const CM_SEARCH_SELECTION: i32 = CM_BASE + 4;
const CM_INSPECT: i32 = CM_BASE + 5;

wrap_context_menu_handler! {
    pub struct StaContextMenu { app: Arc<AppShared> }
    impl ContextMenuHandler {
        fn on_before_context_menu(&self, _browser: Option<&mut Browser>, _frame: Option<&mut Frame>,
                                  params: Option<&mut ContextMenuParams>, model: Option<&mut MenuModel>) {
            let (Some(p), Some(m)) = (params, model) else { return };
            let flags = sys::cef_context_menu_type_flags_t::from(p.type_flags()).0;
            let has = |f: sys::cef_context_menu_type_flags_t| flags & f.0 != 0;
            if has(sys::cef_context_menu_type_flags_t::CM_TYPEFLAG_LINK) {
                m.insert_item_at(0, CM_OPEN_LINK_NEW_TAB, Some(&CefString::from("Open link in new tab")));
                m.insert_item_at(1, CM_COPY_LINK, Some(&CefString::from("Copy link address")));
                m.insert_separator_at(2);
            }
            if p.media_type() == ContextMenuMediaType::IMAGE {
                m.add_item(CM_SAVE_IMAGE, Some(&CefString::from("Save image as…")));
            }
            if has(sys::cef_context_menu_type_flags_t::CM_TYPEFLAG_SELECTION) {
                m.add_item(CM_SEARCH_SELECTION, Some(&CefString::from("Search the web")));
            }
            if m.count() > 0 { m.add_separator(); }
            m.add_item(CM_INSPECT, Some(&CefString::from("Inspect")));
        }
        fn on_context_menu_command(&self, browser: Option<&mut Browser>, _frame: Option<&mut Frame>,
                                   params: Option<&mut ContextMenuParams>, command_id: i32, _event_flags: EventFlags) -> i32 {
            let (Some(b), Some(p)) = (browser, params) else { return 0 };
            match command_id {
                CM_OPEN_LINK_NEW_TAB => { let u = CefString::from(&p.link_url()).to_string(); let a = self.app.clone();
                                          post_ui(move || { open_tab(&a, &u, None, false); }); 1 }
                CM_COPY_LINK => { set_clipboard_text(&CefString::from(&p.unfiltered_link_url()).to_string()); 1 }
                CM_SAVE_IMAGE => { if let Some(h) = b.host() { h.start_download(Some(&CefString::from(&p.source_url()))); } 1 } // B:L12583
                CM_SEARCH_SELECTION => { /* open_tab(search_url(selection_text)) */ 1 }
                CM_INSPECT => { show_devtools(b, Some(Point { x: p.xcoord(), y: p.ycoord() })); 1 }   // §11.3
                _ => 0,   // default ids (MenuId::BACK, COPY, PRINT, VIEW_SOURCE...) keep default behavior
            }
        }
    }
}
```

---

## 9. DownloadHandler (`wrap_download_handler!` B:L18877, `ImplDownloadHandler` B:L18842)

```rust
fn can_download(&self, browser: Option<&mut Browser>, url: Option<&CefString>, request_method: Option<&CefString>) -> ::std::os::raw::c_int   // B:L18844
fn on_before_download(&self, browser: Option<&mut Browser>, download_item: Option<&mut DownloadItem>,
                      suggested_name: Option<&CefString>, callback: Option<&mut BeforeDownloadCallback>) -> ::std::os::raw::c_int         // B:L18853
fn on_download_updated(&self, browser: Option<&mut Browser>, download_item: Option<&mut DownloadItem>, callback: Option<&mut DownloadItemCallback>) // B:L18863
```

- VERIFIED-FIX (cef-rs default trap): `can_download` defaults to **0** in Rust (B:L18844-18851 `Default::default()`), while the C++ default is `return true`. An `StaDownload` that omits the `can_download` override **cancels every user-initiated download** (alt+click, `Content-Disposition: attachment`). The snippet below overrides it correctly; keep it.
- **Callbacks:**
  - `ImplBeforeDownloadCallback::cont(&self, download_path: Option<&CefString>, show_dialog: ::std::os::raw::c_int)` (B:L18694)
  - `ImplDownloadItemCallback::cancel(&self)` (B:L18752), `pause(&self)` (B:L18754), `resume(&self)` (B:L18756)
- **`ImplDownloadItem`** (B:L18368) getters:
  - `is_valid` (L18370), `is_in_progress` (L18372), `is_complete` (L18374), `is_canceled` (L18376), `is_interrupted` (L18378), `interrupt_reason` (L18380)
  - `current_speed() -> i64` (L18382), `percent_complete() -> c_int` (L18384), `total_bytes() -> i64` (L18386), `received_bytes() -> i64` (L18388)
  - `start_time`/`end_time -> Basetime`
  - `full_path() -> CefStringUserfree` (L18394), `id() -> u32` (L18396), `url` (L18398), `original_url`, `suggested_file_name` (L18402), `content_disposition`, `mime_type` (L18406), `is_paused` (L18408)
- **Header semantics** (`cef_download_handler.h` / `cef_download_item.h`):
  - `OnBeforeDownload`: *"Return true and execute |callback| either asynchronously or in this method … Return false to proceed with default handling … Do not keep a reference to |download_item| outside of this method."*
  - `Continue`: *"Set |download_path| to the full file path for the download including the file name or leave blank to use the suggested name and the default temp directory. Set |show_dialog| to true if you do wish to show the default 'Save As' dialog."*
  - `OnDownloadUpdated`: *"may be called multiple times before and after OnBeforeDownload()"*.
  - `GetPercentComplete`: *"-1 if the receive total size is unknown"*.
  - Threading: *"called on the browser process UI thread"*.
- **Behavior verified in CEF source** (`download_manager_delegate_impl.cc`):
  - The target directory is created if missing.
  - An empty path goes to `base::DIR_TEMP` + suggested name, **not** Downloads.
  - Calling the callback but returning 0 logs *"Should return true from OnBeforeDownload when executing the callback"*.
  - `OnBrowserDestroyed` sets `item_browser = nullptr`: *"Don't call back into browsers that have been destroyed … it will continue silently"*. **If the tab that started the download closes, `on_download_updated` stops arriving.** `DownloadItemCallback` is keyed by manager + id, not by browser, so a stored callback can still pause, resume or cancel.
- **Default download directory on Windows** is the Known Folder `FOLDERID_Downloads`, normally `%USERPROFILE%\Downloads`. Resolve it with `SHGetKnownFolderPath(&FOLDERID_Downloads, …)` (windows crate) or `dirs::download_dir()`. Chrome's pref is `download.default_directory` (§12.3).

```rust
wrap_download_handler! {
    pub struct StaDownload { app: Arc<AppShared> }
    impl DownloadHandler {
        fn can_download(&self, _browser: Option<&mut Browser>, _url: Option<&CefString>, _request_method: Option<&CefString>) -> i32 { 1 }
        fn on_before_download(&self, browser: Option<&mut Browser>, download_item: Option<&mut DownloadItem>,
                              suggested_name: Option<&CefString>, callback: Option<&mut BeforeDownloadCallback>) -> i32 {
            let (Some(item), Some(cb)) = (download_item, callback) else { return 0 };
            let name = suggested_name.map(CefString::to_string).filter(|s| !s.is_empty()).unwrap_or_else(|| "download".into());
            let path = unique_path(&downloads_dir_for(&self.app, browser), &name);   // your helper
            register_download(&self.app, item.id(), CefString::from(&item.url()).to_string(), &path);
            cb.cont(Some(&CefString::from(path.to_string_lossy().as_ref())), 0);    // 1 = native "Save As"
            1
        }
        fn on_download_updated(&self, _browser: Option<&mut Browser>, download_item: Option<&mut DownloadItem>,
                               callback: Option<&mut DownloadItemCallback>) {
            let Some(it) = download_item else { return };
            update_download(&self.app, DownloadSnapshot {
                id: it.id(), percent: it.percent_complete(), received: it.received_bytes(), total: it.total_bytes(),
                speed: it.current_speed(), complete: it.is_complete() != 0, canceled: it.is_canceled() != 0,
                interrupted: it.is_interrupted() != 0, paused: it.is_paused() != 0,
                full_path: CefString::from(&it.full_path()).to_string(),
            }, callback.cloned());   // keep DownloadItemCallback for cancel()/pause()/resume() from the UI
        }
    }
}
```

---

## 10. JS dialogs, permissions and the file chooser

### 10.1 JsdialogHandler (`wrap_jsdialog_handler!` B:L20131, `ImplJsdialogHandler` B:L20096). Optional with Alloy.

```rust
fn on_jsdialog(&self, browser: Option<&mut Browser>, origin_url: Option<&CefString>, dialog_type: JsdialogType,
               message_text: Option<&CefString>, default_prompt_text: Option<&CefString>,
               callback: Option<&mut JsdialogCallback>, suppress_message: Option<&mut ::std::os::raw::c_int>) -> ::std::os::raw::c_int   // B:L20098
fn on_before_unload_dialog(&self, browser: Option<&mut Browser>, message_text: Option<&CefString>,
                           is_reload: ::std::os::raw::c_int, callback: Option<&mut JsdialogCallback>) -> ::std::os::raw::c_int   // B:L20111
fn on_reset_dialog_state(&self, browser: Option<&mut Browser>)   // B:L20121
fn on_dialog_closed(&self, browser: Option<&mut Browser>)        // B:L20123
```

- `ImplJsdialogCallback::cont(&self, success: ::std::os::raw::c_int, user_input: Option<&CefString>)` (B:L20022).
- `JsdialogType` constants: `ALERT` (B:L47755), `CONFIRM`, `PROMPT`.
- **Default without a handler:** CEF source `javascript_dialog_manager.cc` uses the Chrome `TabModalDialogManager`. Alloy browsers get Chrome tab helpers (`TabHelpers::AttachTabHelpers` in `browser_platform_delegate_alloy.cc`) and a `GetWebContentsModalDialogHost`, so **alert, confirm, prompt and beforeunload work out of the box** in windowed Alloy BrowserViews. Only windowless browsers without a parent handle cancel them.
- Implement the handler only for Arc-style in-UI dialogs. Header: *"Set |suppress_message| to true and return false to suppress … Return true if the application will use a custom dialog … the application must execute |callback| once the custom dialog is dismissed."*

### 10.2 PermissionHandler (`wrap_permission_handler!` B:L21920, `ImplPermissionHandler` B:L21882). Required with Alloy.

```rust
fn on_request_media_access_permission(&self, browser: Option<&mut Browser>, frame: Option<&mut Frame>,
    requesting_origin: Option<&CefString>, requested_permissions: u32, callback: Option<&mut MediaAccessCallback>) -> ::std::os::raw::c_int  // B:L21884
fn on_show_permission_prompt(&self, browser: Option<&mut Browser>, prompt_id: u64, requesting_origin: Option<&CefString>,
    requested_permissions: u32, callback: Option<&mut PermissionPromptCallback>) -> ::std::os::raw::c_int                                   // B:L21895
fn on_dismiss_permission_prompt(&self, browser: Option<&mut Browser>, prompt_id: u64, result: PermissionRequestResult)                       // B:L21906
```

- **Callbacks:**
  - `ImplMediaAccessCallback::cont(&self, allowed_permissions: u32)` (B:L21745) and `cancel(&self)` (B:L21747). The sys doc says *"If this callback was initiated in response to a getUserMedia … |allowed_permissions| must match |required_permissions|"*.
  - `ImplPermissionPromptCallback::cont(&self, result: PermissionRequestResult)` (B:L21810).
- **Constants:**
  - `PermissionRequestResult::ACCEPT` (B:L51057), `DENY`, `DISMISS`, `IGNORE`.
  - `MediaAccessPermissionTypes::DEVICE_AUDIO_CAPTURE` (B:L50884) and `DEVICE_VIDEO_CAPTURE` (B:L50887), via `.get_raw() as u32`.
  - `PermissionRequestTypes::*` (B:L50931, e.g. `GEOLOCATION` = 1<<8, `NOTIFICATIONS` = 1<<15, `CLIPBOARD` = 1<<4).
- **Defaults (Alloy):** media access is denied. Other prompts give IGNORE, with a source log line *"Implement OnShowPermissionPrompt to override default IGNORE handling"*. The comment there explains: *"The default UI prompt is not supported because there is no Chrome Browser object"*. The `--enable-media-stream` switch grants all media without calling the handler.
- **Verified behaviour (CEF 152, sta e2e):**
  - IGNORE (a client without a PermissionHandler, or `on_show_permission_prompt` returning 0) leaves the page's promise pending forever (`Notification.requestPermission()`, `getCurrentPosition` never call back). Answer `DISMISS` instead (inside the handler CEF runs the callback asynchronously).
  - `permission_prompt.cc` (CEF): `ExecuteResult` calls `OnDismissPermissionPrompt(prompt_id, result)` for every result, *before* notifying `PermissionRequestManager`; `Continue` runs synchronously once the handler returned. `ACCEPT` maps to `PermissionRequestManager::Accept` (a permanent content setting); `AcceptThisTime` is not exposed. Media access (`on_request_media_access_permission`) stores nothing.
  - `PermissionDecisionAutoBlocker` (Chromium 152): 3 dismissals (`DISMISS`) or 4 ignores of one permission type embargo the origin for 7 days (`GetEmbargoResult` → DENIED without calling the handler; console *"…blocked as the user has dismissed the permission prompt several times"*). There is no feature gate on desktop any more: the strings `BlockPromptsIfDismissedOften`/`BlockPromptsIfIgnoredOften` are not in `libcef.dll`. The counters are the website setting `PERMISSION_AUTOBLOCKER_DATA` keyed `(origin, GURL())`; `request_context.set_website_setting(Some(origin), None, PERMISSION_AUTOBLOCKER_DATA, None)` removes them (and an embargo). Recorded synchronously inside `Dismiss`.
  - `RequestContext::set_content_setting(url, url, type, DEFAULT)` removes a grant (`SetContentSettingDefaultScope`), but `HostContentSettingsMap` **CHECKs** that `type` is a registered content setting: `GEOLOCATION_WITH_OPTIONS` (a website setting) crashes the browser process. `website_setting`/`set_website_setting` CHECK the website-settings registry instead. CEF validates neither.
  - Chromium's `PermissionRequestManager` holds a request of a hidden WebContents until it is visible again (no `on_show_permission_prompt` meanwhile).
  - Notifications: `Notification.requestPermission()` reaches `on_show_permission_prompt` (bit `NOTIFICATIONS`) on https, `http://127.0.0.1`, `localhost` and `file:`; an insecure origin resolves `denied` without a prompt. Shown notifications are Chromium message-center pop-ups (a `Chrome_WidgetWin_1` window of the browser process at the bottom-right of the screen), not Windows toasts.

```rust
wrap_permission_handler! {
    pub struct StaPermission { app: Arc<AppShared> }
    impl PermissionHandler {
        fn on_show_permission_prompt(&self, browser: Option<&mut Browser>, prompt_id: u64, requesting_origin: Option<&CefString>,
                                     requested_permissions: u32, callback: Option<&mut PermissionPromptCallback>) -> i32 {
            let (Some(b), Some(cb)) = (browser, callback.cloned()) else { return 0 };
            // Show in sidebar/overlay UI; later: cb.cont(PermissionRequestResult::ACCEPT / DENY / DISMISS)
            queue_permission_prompt(&self.app, b.identifier(), prompt_id,
                                    requesting_origin.map(CefString::to_string).unwrap_or_default(), requested_permissions, cb);
            1
        }
        fn on_dismiss_permission_prompt(&self, _browser: Option<&mut Browser>, prompt_id: u64, _result: PermissionRequestResult) {
            remove_permission_prompt(&self.app, prompt_id);  // navigation/close dismissed it
        }
        fn on_request_media_access_permission(&self, browser: Option<&mut Browser>, _frame: Option<&mut Frame>,
                                              requesting_origin: Option<&CefString>, requested_permissions: u32,
                                              callback: Option<&mut MediaAccessCallback>) -> i32 {
            let (Some(b), Some(cb)) = (browser, callback.cloned()) else { return 0 };
            queue_media_prompt(&self.app, b.identifier(), requesting_origin.map(CefString::to_string).unwrap_or_default(),
                               requested_permissions, cb);   // later: cb.cont(requested_permissions) or cb.cancel()
            1
        }
    }
}
```

### 10.3 DialogHandler (file chooser: `wrap_dialog_handler!` B:L17374, `ImplDialogHandler` B:L17352). Optional.

```rust
fn on_file_dialog(&self, browser: Option<&mut Browser>, mode: FileDialogMode, title: Option<&CefString>,
    default_file_path: Option<&CefString>, accept_filters: Option<&mut CefStringList>, accept_extensions: Option<&mut CefStringList>,
    accept_descriptions: Option<&mut CefStringList>, callback: Option<&mut FileDialogCallback>) -> ::std::os::raw::c_int   // B:L17354
```

- `ImplFileDialogCallback::cont(&self, file_paths: Option<&mut CefStringList>)` (B:L17268) and `cancel(&self)` (B:L17270). Build the list with `CefStringList::new()` + `append`.
- `FileDialogMode` constants: `OPEN` (B:L48924), `OPEN_MULTIPLE`, `OPEN_FOLDER`, `SAVE`.
- **Default without a handler:** the native Windows dialog in both styles (`AlloyBrowserHostImpl::RunFileChooser` → `FileSelectHelper`).
- Header: *"If this method returns false it may be called an additional time for the same dialog (both before and after MIME type expansion)."*
- The same `std::mem::take` rule from §3 applies when reading `accept_filters`.

---

## 11. BrowserHost / Browser / Frame operations

All signatures below are verbatim from `ImplBrowserHost` (B:L12540), `ImplBrowser` (B:L11703) and `ImplFrame` (B:L7520).

### 11.1 Find (`wrap_find_handler!` B:L19379)

- `fn find(&self, search_text: Option<&CefString>, forward: ::std::os::raw::c_int, match_case: ::std::os::raw::c_int, find_next: ::std::os::raw::c_int)` (B:L12603)
- `fn stop_finding(&self, clear_selection: ::std::os::raw::c_int)` (B:L12611)
- `fn on_find_result(&self, browser: Option<&mut Browser>, identifier: ::std::os::raw::c_int, count: ::std::os::raw::c_int, selection_rect: Option<&Rect>, active_match_ordinal: ::std::os::raw::c_int, final_update: ::std::os::raw::c_int)` (B:L19362)

Header: *"The search will be restarted if |searchText| or |matchCase| change. The search will be stopped if |searchText| is empty."* Use `find_next=0` for the first query and `1` for Enter or F3. Alloy has no find bar, so render one in the overlay.

### 11.2 Zoom

- `fn can_zoom(&self, command: ZoomCommand) -> ::std::os::raw::c_int` (B:L12564)
- `fn zoom(&self, command: ZoomCommand)` (B:L12566), with `ZoomCommand::IN` (B:L51368), `OUT` (B:L51364), `RESET` (B:L51366)
- `fn default_zoom_level(&self) -> f64` (B:L12568)
- `fn zoom_level(&self) -> f64` (B:L12570)
- `fn set_zoom_level(&self, zoom_level: f64)` (B:L12572)

Semantics:
- Getters are UI-thread only. `Zoom`/`SetZoomLevel` post internally.
- The CEF source uses Chrome's `zoom::ZoomController`, so zoom is **per host (origin) within the profile** in the default zoom mode. Zooming example.com in one tab changes other example.com tabs in the same space.
- Zoom level is logarithmic: factor = 1.2^level.

### 11.3 DevTools

- `fn show_dev_tools(&self, window_info: Option<&WindowInfo>, client: Option<&mut Client>, settings: Option<&BrowserSettings>, inspect_element_at: Option<&Point>)` (B:L12613)
- `fn close_dev_tools(&self)` (B:L12621)
- `fn has_dev_tools(&self) -> ::std::os::raw::c_int` (B:L12623), UI thread only

Header semantics:
- *"If the DevTools browser is already open then it will be focused, in which case the |windowInfo|, |client| and |settings| parameters will be ignored. If |inspect_element_at| is non-empty then the element at the specified (x,y) location will be inspected. The |windowInfo| parameter will be ignored if this browser is wrapped in a CefBrowserView."*
- An empty client means CEF uses `BrowserProcessHandler::default_client`, per the source.
- Presentation is covered in §5.2. The popup arrives in `on_popup_browser_view_created(is_devtools=1)`; return 0 for a separate window.

```rust
pub fn show_devtools(browser: &mut Browser, at: Option<Point>) {
    let Some(host) = browser.host() else { return };
    let mut client = devtools_client();     // Client with LifeSpanHandler (do_close unused: DevTools is Chrome style) + CommandHandler
    host.show_dev_tools(None, client.as_mut(), Some(&BrowserSettings::default()), at.as_ref());
}
```

`Point` (B:L162) has fields `x`, `y`.

- **Ctrl+Shift+C:** there's no element-picker API. Open DevTools, or inspect at the last right-click point.
- **CDP** is available on any browser without DevTools open:
  - `fn execute_dev_tools_method(&self, message_id: ::std::os::raw::c_int, method: Option<&CefString>, params: Option<&mut DictionaryValue>) -> ::std::os::raw::c_int` (B:L12627)
  - `fn add_dev_tools_message_observer(&self, observer: Option<&mut DevToolsMessageObserver>) -> Option<Registration>` (B:L12634), macro `wrap_dev_tools_message_observer!` (B:L3771)

### 11.4 Visibility, focus, navigation, identity

- **Hiding a tab:** don't use `fn was_hidden(&self, hidden: ::std::os::raw::c_int)` (B:L12653). The header says *"only used when window rendering is disabled"*, and the Alloy source for windowed browsers is `DCHECK(false)` followed by return. Use `ImplView::set_visible(&self, visible: ::std::os::raw::c_int)` (B:L38365) on the tab's BrowserView. `views/cef_view.h`: *"If this View is set as hidden then it and any child views will not be drawn and, if any of those views currently have focus, then focus will also be cleared."*
- **Focus:**
  - `ImplView::request_focus(&self)` (B:L38383). The CEF source posts it asynchronously (issue #3040).
  - `ImplBrowserHost::set_focus(&self, focus: ::std::os::raw::c_int)` (B:L12550).
- **Navigation:**
  - `ImplBrowser::go_back(&self)` (B:L11711), `go_forward(&self)` (B:L11715), `can_go_back` (B:L11709), `can_go_forward` (B:L11713)
  - `reload(&self)` (B:L11719), `reload_ignore_cache(&self)` (B:L11721), `stop_load(&self)` (B:L11723), `is_loading` (B:L11717)
  - `main_frame(&self) -> Option<Frame>` (B:L11733) → `ImplFrame::load_url(&self, url: Option<&CefString>)` (B:L7548)
  - `ImplFrame::execute_java_script(&self, code: Option<&CefString>, script_url: Option<&CefString>, start_line: ::std::os::raw::c_int)` (B:L7550)
  - `cef_browser.h`: in the browser process these *"may be called on any thread unless otherwise indicated"*.
- **Identity:**
  - `ImplBrowser::identifier(&self) -> ::std::os::raw::c_int` (B:L11725)
  - `is_same(&self, that: Option<&mut Browser>) -> ::std::os::raw::c_int` (B:L11727)
  - `host(&self) -> Option<BrowserHost>` (B:L11707)
  - `pub fn browser_view_get_for_browser(browser: Option<&mut Browser>) -> Option<BrowserView>` (B:L59186)
  - `ImplBrowserHost::opener_identifier(&self) -> ::std::os::raw::c_int` (B:L12556)

### 11.5 Print, save, fullscreen

- `fn print(&self)` (B:L12594). The source calls `print_util::Print(web_contents, print_preview_disabled)`, with preview when the platform delegate supports it.
- `fn print_to_pdf(&self, path: Option<&CefString>, settings: Option<&PdfPrintSettings>, callback: Option<&mut PdfPrintCallback>)` (B:L12596), macro `wrap_pdf_print_callback!` (B:L12328).
- **Ctrl+S (save page):** Alloy has no "Save page as" API. Options:
  - (a) `host.start_download(Some(&url))` (B:L12583) re-fetches the resource and routes through `DownloadHandler`.
  - (b) CDP `Page.captureSnapshot {format:"mhtml"}` via `execute_dev_tools_method`, with the result read through a DevTools message observer, gives a complete-page .mhtml.
  - (c) `ImplFrame::source(&self, visitor: Option<&mut CefStringVisitor>)` (B:L7542) gives HTML only (`wrap_string_visitor!` B:L7446).
- **Fullscreen:**
  - `fn is_fullscreen(&self) -> ::std::os::raw::c_int` (B:L12746)
  - `fn exit_fullscreen(&self, will_cause_resize: ::std::os::raw::c_int)` (B:L12748)
  - `ImplWindow::set_fullscreen(&self, fullscreen: ::std::os::raw::c_int)` (B:L44274)

### 11.6 Favicons via download_image (`wrap_download_image_callback!` B:L12440)

- `fn download_image(&self, image_url: Option<&CefString>, is_favicon: ::std::os::raw::c_int, max_image_size: u32, bypass_cache: ::std::os::raw::c_int, callback: Option<&mut DownloadImageCallback>)` (B:L12585)
- `fn on_download_image_finished(&self, image_url: Option<&CefString>, http_status_code: ::std::os::raw::c_int, image: Option<&mut Image>)` (B:L12424)
- `ImplImage::is_empty` (B:L4067)
- `ImplImage::as_png(&self, scale_factor: f32, with_transparency: ::std::os::raw::c_int, pixel_width: Option<&mut ::std::os::raw::c_int>, pixel_height: Option<&mut ::std::os::raw::c_int>) -> Option<BinaryValue>` (B:L4110)
- `ImplBinaryValue::size(&self) -> usize` (B:L2227)
- `ImplBinaryValue::data(&self, buffer: Option<&mut Vec<u8>>, data_offset: usize) -> usize` (B:L2229). The glue copies `buffer.len()` bytes, so **pre-size the Vec**.

Header: *"If |is_favicon| is true then cookies are not sent and not accepted … If there are no image results <= |max_image_size| then the smallest image is resized to |max_image_size|."*

```rust
wrap_download_image_callback! {
    struct FaviconDone { app: Arc<AppShared>, browser_id: i32 }
    impl DownloadImageCallback {
        fn on_download_image_finished(&self, image_url: Option<&CefString>, _http_status_code: i32, image: Option<&mut Image>) {
            let Some(img) = image else { return };
            if img.is_empty() != 0 { return; }
            let (mut w, mut h) = (0, 0);
            if let Some(png) = img.as_png(2.0, 1, Some(&mut w), Some(&mut h)) {   // pick device scale factor
                let mut bytes = vec![0u8; png.size()];
                png.data(Some(&mut bytes), 0);
                store_favicon(&self.app, self.browser_id, image_url.map(CefString::to_string).unwrap_or_default(), bytes);
            }
        }
    }
}
// in on_favicon_urlchange (UI thread):
// let mut cb = FaviconDone::new(app.clone(), browser.identifier());
// if let Some(h) = browser.host() { h.download_image(Some(&CefString::from(first_url.as_str())), 1, 32, 0, Some(&mut cb)); }
```

### 11.7 Chrome commands

- `fn can_execute_chrome_command(&self, command_id: ::std::os::raw::c_int) -> ::std::os::raw::c_int` (B:L12750)
- `fn execute_chrome_command(&self, command_id: ::std::os::raw::c_int, disposition: WindowOpenDisposition)` (B:L12755)
- Header: *"Only used with Chrome style"*. They are useless for Alloy tabs and only apply to the DevTools browser.
- The version-safe `cef_id_for_command_id_name` is **not bound** in cef-dll-sys. It is exported by `libcef.lib` (verified), so declare it yourself: `unsafe extern "C" { fn cef_id_for_command_id_name(name: *const std::os::raw::c_char) -> std::os::raw::c_int; }`.

---

## 12. RequestContext: space profiles, clearing data, preferences

### 12.1 Creating contexts

- `pub fn request_context_create_context(settings: Option<&RequestContextSettings>, handler: Option<&mut RequestContextHandler>) -> Option<RequestContext>` (B:L57507)
- `pub fn request_context_get_global_context() -> Option<RequestContext>` (B:L57495)
- `pub fn request_context_cef_create_context_shared(other: Option<&mut RequestContext>, handler: Option<&mut RequestContextHandler>) -> Option<RequestContext>` (B:L57534)
- `RequestContextSettings` (B:L636): `size, cache_path: CefString, persist_session_cookies: c_int, accept_language_list: CefString, cookieable_schemes_list: CefString, cookieable_schemes_exclude_defaults: c_int`. `Default` fills `size`.
- `wrap_request_context_handler!` (B:L28928) provides `on_request_context_initialized`. Header: *"Called on the browser process UI thread immediately after the request context has been initialized."*

Header rules (`internal/cef_types.h`):
- `cache_path` *"must be an absolute path that is either equal to or a child directory of CefSettings.root_cache_path. If this value is empty then browsers will be created in 'incognito mode'"*.
- *"HTML5 databases such as localStorage will only persist across sessions if a cache path is specified."*
- `persist_session_cookies` and `accept_language_list` are *"ignored if |cache_path| matches the CefSettings.cache_path value"*.
- `cef_request_context.h`: *"Browser objects with different request contexts will never be hosted in the same render process."*

Verified in CEF source:
- `chrome_browser_context.cc`: `if (cache_path_ == user_data_dir)` → default profile; `else if (cache_path_.DirName() == user_data_dir)` → `CreateProfileAsync`; else `LOG(ERROR) << "Cannot create profile at path"` → *"Default to creating a new/unique OffTheRecord profile"*. So **space dirs must be direct children** of `root_cache_path`, e.g. `%LOCALAPPDATA%\sta\User Data\space-<uuid>`.
- `request_context_impl.cc`: `CefBrowserContext::FromCachePath(cache_path)`. **The same `cache_path` returns the same underlying profile**; that's a safe way to reuse contexts.
- An incognito space uses an empty `cache_path`, which gives a unique OffTheRecord profile.

```rust
pub fn create_space_context(root_cache: &std::path::Path, space_id: &str) -> Option<RequestContext> {
    let dir = root_cache.join(format!("space-{space_id}"));   // DIRECT child of Settings.root_cache_path
    let settings = RequestContextSettings {
        cache_path: CefString::from(dir.to_string_lossy().as_ref()),
        persist_session_cookies: 1,
        ..Default::default()
    };
    request_context_create_context(Some(&settings), None)       // UI thread, after on_context_initialized
}
// Tab in that space: browser_view_create(client, url, settings, None, space_ctx.as_mut(), delegate)
```

### 12.2 Clearing browsing data

`ImplRequestContext` (B:L11102):
- `fn cookie_manager(&self, callback: Option<&mut CompletionCallback>) -> Option<CookieManager>` (B:L11114)
- `fn clear_http_cache(&self, callback: Option<&mut CompletionCallback>)` (B:L11175), added in API 14400
- `fn clear_http_auth_credentials(&self, callback: Option<&mut CompletionCallback>)` (B:L11127)
- `fn clear_certificate_exceptions(&self, callback: Option<&mut CompletionCallback>)` (B:L11125)
- `fn close_all_connections(&self, callback: Option<&mut CompletionCallback>)` (B:L11129)
- `fn set_content_setting(...)` (B:L11157), `fn set_website_setting(...)` (B:L11142)

`ImplCookieManager` (B:L8881):
- `fn delete_cookies(&self, url: Option<&CefString>, cookie_name: Option<&CefString>, callback: Option<&mut DeleteCookiesCallback>) -> ::std::os::raw::c_int` (B:L8899). `cef_cookie.h`: *"If |url| is empty all cookies for all hosts and domains will be deleted. If |callback| is non-NULL it will be executed asnychronously on the UI thread."*
- `fn flush_store(&self, callback: Option<&mut CompletionCallback>) -> ::std::os::raw::c_int` (B:L8906)

Callback macros are `wrap_completion_callback!` (B:L8818, `on_complete(&self)`) and `wrap_delete_cookies_callback!` (B:L9354, `on_complete(&self, num_deleted: c_int)`). For localStorage, IndexedDB, service workers etc., send the CDP `Storage.clearDataForOrigin` (`storageTypes: "all"`) from any browser in that space via `execute_dev_tools_method`.

```rust
pub fn clear_space(ctx: &RequestContext) {
    if let Some(cm) = ctx.cookie_manager(None) { cm.delete_cookies(None, None, None); }
    ctx.clear_http_cache(None);
    ctx.clear_http_auth_credentials(None);
    ctx.close_all_connections(None);
}
```

### 12.3 Preferences and color scheme

- `ImplPreferenceManager::set_preference(&self, name: Option<&CefString>, value: Option<&mut Value>, error: Option<&mut CefString>) -> ::std::os::raw::c_int` (B:L10671). `RequestContext` inherits it; the header says *"must be called on the browser process UI thread"*.
- Supporting calls: `can_set_preference` (B:L10669), `preference` (B:L10665), `pub fn value_create() -> Option<Value>` (B:L57200), `ImplValue::set_string(&self, value: Option<&CefString>) -> c_int` (B:L1841), `set_bool` (B:L1835).
- **Out-param gotcha:** `CefString::default()` is `CefStringData::Borrowed(None)` (string.rs:L256), which converts to a **null** `*mut cef_string_t` (string.rs:L538-L541, L288-L297). Pass `&mut CefString::from("")`, which is backed by a real struct, for `error`.
- **Dark mode:**
  - `fn set_chrome_color_scheme(&self, variant: ColorVariant, user_color: u32)` (B:L11165), with `ColorVariant::SYSTEM` (B:L51407), `LIGHT` (B:L51409), `DARK` (B:L51411).
  - Header: *"Sets the Chrome color scheme for all browsers that share this request context. |variant| values of SYSTEM, LIGHT and DARK change the underlying color mode."* The source sets the profile's `ThemeService` browser color scheme.
  - Expect it to drive `prefers-color-scheme` for web content through Chrome's content client, but verify.
  - Getters: `chrome_color_scheme_mode` (B:L11167), UI thread.

```rust
pub fn set_download_dir(ctx: &RequestContext, dir: &str) -> Result<(), String> {
    let mut v = value_create().ok_or("value_create")?;
    v.set_string(Some(&CefString::from(dir)));
    let mut err = CefString::from("");
    if ctx.set_preference(Some(&CefString::from("download.default_directory")), Some(&mut v), Some(&mut err)) != 0 { Ok(()) }
    else { Err(err.to_string()) }
}
// ctx.set_chrome_color_scheme(ColorVariant::DARK, 0);
```

Useful Chrome pref names are `download.default_directory`, `download.prompt_for_download` and `intl.accept_languages`. They only matter when your `DownloadHandler` returns 0.

---

## 13. Threading reference

| Callback / API | Thread | Source |
|---|---|---|
| Display, Keyboard, ContextMenu, Find, JSDialog, Command, Focus handlers | UI | class docs ("called on the UI thread") |
| Download, Permission, Dialog handlers | browser-process UI | class docs |
| LifeSpan (popup, created, do_close, before_close) | UI | `cef_life_span_handler.h` |
| Load handler (browser process) | UI | "browser process UI thread or render process main thread" |
| RequestHandler `on_before_browse`, `on_open_urlfrom_tab`, cert error, render-process callbacks | UI | per-method docs |
| RequestHandler `resource_request_handler`, `auth_credentials`; all ResourceRequestHandler methods | **IO** | per-method docs |
| AudioHandler `audio_parameters` / `on_audio_stream_stopped` | UI | |
| AudioHandler `on_audio_stream_started` / `packet` | audio capture/stream thread | |
| All Views (`Window`, `Panel`, `BrowserView`, delegates) | UI; *"Methods must be called on the browser process UI thread"* | views headers |
| `MenuModel`, `ContextMenuParams` | UI only | headers |
| `Browser`/`Frame` methods | any thread (browser process) | `cef_browser.h`, `cef_frame.h` |
| `BrowserHost` `try_close_browser`, `is_ready_to_be_closed`, `zoom_level`, `can_zoom`, `has_dev_tools`, `is_audio_muted`, `is_fullscreen`, `visible_navigation_entry`, `can_execute_chrome_command`, `execute_dev_tools_method` (for a success result) | **UI only** | `cef_browser.h` |
| `close_browser`, `set_zoom_level`, `zoom`, `find`, `set_audio_muted`, `show_dev_tools`, `download_image`, `print` | any thread (post to UI internally) | CEF source `browser_host_base.cc` |
| `set_preference`, `set_chrome_color_scheme` getters, website/content setting getters | UI | headers |
| Completion / DeleteCookies callbacks | UI | `cef_cookie.h`, `cef_request_context.h` |

- With `Settings.multi_threaded_message_loop = 0` and `run_message_loop()`, TID_UI is the main thread. From other Rust threads (IPC, tokio), use `post_ui(...)` (§2.3).
- Check the thread with `currently_on(ThreadId::UI) != 0` (B:L57820), as cefsimple does in `debug_assert_ne!(currently_on(ThreadId::UI), 0)`.

---

## 14. Gotcha checklist

1. **Chrome-style BrowserViews:** a Window can host at most one, so tabs, sidebar and command bar must all be ALLOY (§0.1).
2. **`do_close` return value:** returning 0 for a tab closes the **entire Window** (`CloseHostWindow` → `widget->Close()`). Return 1 and remove the view.
3. ~~`close_browser(1)` + `do_close` returning 1 leaves the browser undestroyed with no `on_before_close` (issue #3376).~~ VERIFIED-FIX: `do_close`→1 resets state to `NONE`, so a *posted* detach works after either `close_browser(0)` or `close_browser(1)`. The real hazards:
   - removing the view while still holding a reference (the #3376 repro)
   - detaching synchronously inside `do_close`
   - dropping the view while a close is pending in state `ACCEPTED`
4. **Dropping the last `BrowserView` reference** runs `on_browser_destroyed`/`on_before_close` synchronously. VERIFIED-FIX: only `on_before_close` runs (the WeakPtr is invalidated first), and it is async if unload handlers must still run. Don't hold the state lock, and don't keep BrowserView clones in delegates or closures. The same applies to `add_child_view`, which runs `on_after_created`/`on_browser_created` synchronously.
5. **Middle-click, Ctrl+click and new-tab dispositions** navigate the current tab unless `on_open_urlfrom_tab` returns 1.
6. **`delegate_for_popup_browser_view`:** the Rust default returns `None`, unlike C++ which returns `this`. Override it or adopted popups lose their delegate.
7. **`on_before_popup` client:** writing `*client = None` is ignored; only `Some(..)` is written back.
8. **Popup callback order:** in 152, `on_popup_browser_view_created` fires before `on_after_created` for the popup, contrary to the header. Don't depend on the order.
9. **`CefStringList` from callbacks:** never `.clone()` it; iterate `std::mem::take(list)`.
10. **`CefString::default()` as an out-param** is a null pointer; use `CefString::from("")`.
11. **`EventFlags`/`ContextMenuTypeFlags`** have no Rust constants; use the `cef::sys` constants with `.0`. `MSG` must be imported from `cef::sys`.
12. **Window accelerators:** one combo per command_id, and they are only effective after `on_window_created`.
13. **`was_hidden`** DCHECKs for windowed browsers; use `View::set_visible`.
14. **Downloads:**
    - Implement `on_before_download` and always return 1 when you call `cont`.
    - An empty path means **TEMP**, not Downloads.
    - Progress stops when the originating browser is destroyed. Keep the tab alive or track progress yourself.
15. **Permissions** are denied/IGNOREd with Alloy unless `PermissionHandler` is implemented.
16. **DevTools** is always a Chrome-style browser, so it can't be docked in an Alloy-style Window. VERIFIED-FIX: it also can't be docked in a Chrome-style Window that already holds any BrowserView (§0.1).
17. **`AudioHandler` capture mutes the tab**; there is no "audible" callback. VERIFIED-FIX: that is only when `audio_parameters` returns 1, and the Rust default returns 0.
21. VERIFIED-FIX (added): **Rust trait defaults are 0/None even where C++ defaults are `true`/`this`.**
    - Override `DownloadHandler::can_download`.
    - Override `WindowDelegate::can_resize`/`can_maximize`/`can_minimize`/`can_close`.
    - Override `BrowserViewDelegate::delegate_for_popup_browser_view` when you need the C++ behavior.
22. VERIFIED-FIX (added): the unit-struct form of multi-level `wrap_*!` macros (`wrap_window_delegate! { struct X; ... }`) doesn't compile. Use `struct X {}`.
23. VERIFIED-FIX (added): `on_browser_destroyed` is not delivered when a tab is closed by releasing its BrowserView. Clean up in `on_before_close`.
18. **Space `cache_path`** must be a *direct* child of `root_cache_path`, otherwise the space silently becomes incognito.
19. **Zoom** is per origin within a profile (Chrome `ZoomController`), not per tab.
20. **`wrap_*!` fields** must be `Clone` and are deep-cloned. Share state through `Arc`.

---

## Sources

- Local headers: `.cef/152.0.6/cef_windows_x86_64/include/` (`cef_display_handler.h`, `cef_load_handler.h`, `cef_life_span_handler.h`, `cef_request_handler.h`, `cef_keyboard_handler.h`, `cef_context_menu_handler.h`, `cef_download_handler.h`, `cef_download_item.h`, `cef_jsdialog_handler.h`, `cef_permission_handler.h`, `cef_dialog_handler.h`, `cef_find_handler.h`, `cef_audio_handler.h`, `cef_command_handler.h`, `cef_browser.h`, `cef_request_context.h`, `cef_preference.h`, `cef_cookie.h`, `cef_task.h`, `internal/cef_types.h`, `internal/cef_types_runtime.h`, `views/cef_window.h`, `views/cef_window_delegate.h`, `views/cef_browser_view.h`, `views/cef_browser_view_delegate.h`, `views/cef_view.h`, `views/cef_panel.h`)
- Local cef-rs: `src/bindings/x86_64_pc_windows_msvc.rs`, `src/string.rs`, `src/rc.rs`, `wrapper/message_router.rs`; examples `cefsimple/src/shared/{simple_app.rs, simple_handler/mod.rs}` and `tests_shared/src/browser/main_message_loop.rs`
- Local `libcef_dll/cpptoc/life_span_handler_cpptoc.cc`, `libcef_dll/ctocpp/ctocpp_ref_counted.h`; `libcef.lib` export check for `cef_id_for_command_id_name`
- CEF source at commit 708dc140 (raw.githubusercontent.com/chromiumembedded/cef/708dc140cbc3286826a8abef89dc23a44ff9ea72/…):
  - `libcef/browser/alloy/alloy_browser_host_impl.cc`, `libcef/browser/alloy/browser_platform_delegate_alloy.cc`
  - `libcef/browser/views/browser_view_impl.cc`, `libcef/browser/views/browser_platform_delegate_views.cc`, `libcef/browser/views/window_impl.cc`
  - `libcef/browser/browser_platform_delegate.cc`, `libcef/browser/browser_contents_delegate.cc`, `libcef/browser/browser_host_base.cc`, `libcef/browser/browser_info_manager.cc`
  - `libcef/browser/download_manager_delegate.cc`, `libcef/browser/download_manager_delegate_impl.cc`, `libcef/browser/javascript_dialog_manager.cc`, `libcef/browser/permission_prompt.cc`, `libcef/browser/media_access_query.cc`
  - `libcef/browser/audio_capturer.cc`, `libcef/browser/audio_loopback_stream_creator.cc`
  - `libcef/browser/chrome/chrome_browser_host_impl.cc`, `libcef/browser/chrome/chrome_browser_delegate.cc`, `libcef/browser/chrome/chrome_browser_context.cc`
  - `libcef/browser/devtools/devtools_window_runner.cc`, `libcef/browser/request_context_impl.cc`
  - `tests/cefclient/browser/{views_overlay_browser.cc, views_window.cc, client_handler.cc}`
  - VERIFIED-FIX additions:
    - `libcef/browser/views/browser_view_impl.h`, `libcef/browser/views/browser_platform_delegate_views.h`, `libcef/browser/views/widget.cc`, `libcef/browser/views/widget_impl.cc`
    - `libcef/browser/chrome/views/chrome_browser_widget.cc`
    - `patch/patches/chrome_browser_download.patch`
    - Chromium `content/browser/web_contents/web_contents_impl.cc` @ 152.0.7977.83
- [CEF issue #3376: Views: Support closing CefBrowserView without closing CefWindow (open)](https://github.com/chromiumembedded/cef/issues/3376)
- [CEF issue #3790: Overlay browser view not shown (Chrome vs Alloy BrowserViews in one Window)](https://github.com/chromiumembedded/cef/issues/3790)
- [CEF Forum: How to close Browser without closing CefWindow](https://www.magpcss.org/ceforum/viewtopic.php?f=6&t=20094)
- [CEF Forum t=19186: overlay BrowserView not destroyed by CloseBrowser (CEF 104)](https://www.magpcss.org/ceforum/viewtopic.php?f=6&t=19186)

---

## Verification log

Adversarial pass against the local cef-rs 152.3.0 bindings, `string.rs`/`rc.rs`, cef-dll-sys, the 152.0.6 headers, local `libcef_dll`, cefsimple, and CEF source re-downloaded at commit `708dc140`. Also checked: Chromium `content/browser/web_contents/web_contents_impl.cc` @ `152.0.7977.83`, and GitHub issue #3376.

**Checked and correct (about 300 items)**

*Bindings (B:L citations)*
- All 261 unique B:L citations point at the named item.
- Every multi-line signature was compared field by field and matches: display, load, life-span (including `on_before_popup`/`on_before_dev_tools_popup` out-params), request, keyboard, context menu, download, JS dialog, permission, dialog, find, audio, Client getters, BrowserViewDelegate, WindowDelegate, `ImplWindow::set_accelerator`, BrowserHost methods (find, zoom, devtools, download_image, audio, fullscreen, chrome commands), MenuModel, ContextMenuParams, DownloadItem getters, RequestContext, CookieManager, PreferenceManager, `browser_view_create`, `post_task`/`post_delayed_task`/`currently_on`, `browser_host_get_browser_by_identifier`.
- Struct fields: `KeyEvent`, `PopupFeatures`, `RequestContextSettings` (with a size-filling `Default`), `Point`.
- Enum constants and derives: `WindowOpenDisposition`, `TerminationStatus`, `KeyEventType`, `FileDialogMode`, `ContextMenuMediaType`, `PermissionRequestResult`, `MediaAccessPermissionTypes`, `ZoomCommand`, `ColorVariant`, `ThreadId::UI`, `MenuId::USER_FIRST/LAST`, `Errorcode::ABORTED`. The `PartialEq` derives are present.

*cef-dll-sys (S:L citations)*
- `EventFlags` and `ContextMenuTypeFlags` have no Rust constants.
- The sys flag newtypes `cef_event_flags_t(pub c_int)` and `cef_context_menu_type_flags_t(pub c_int)` exist, with the cited S:L values.
- `cef_errorcode_t` and `cef_menu_id_t` are `#[repr(i32)]` enums, so `as i32` works (cefsimple does it too).
- `MSG` is at S:L17889 and is not re-exported.
- `cef::sys` = `pub use cef_dll_sys as sys`.
- `cef_id_for_command_id_name` is unbound in sys, present in `libcef.lib`, and declared `CEF_EXPORT` in `cef_id_mappers.h`.

*Macro and glue mechanics*
- Macro mechanics: `new(fields…)` returns the CEF type, `Clone` clones every field, hidden `cef_object`, required base blocks.
- `From<DisplayHandler> for *mut` does `forget`.
- `RefGuard` is `unsafe impl Send + Sync`.
- The `on_before_popup` client write-back only happens for `Some`. Setting `None` releases the in-ref (balanced), and the pointer stays unchanged.
- `mem::take` on a `CefStringList` is correct; the glue frees the placeholder.
- `CefString::default()` → null out-pointer. The libcef cpptoc verifies non-optional `string_byref` params (pattern seen in `menu_model_delegate_cpptoc.cc`), and `SetPreference` has only `optional_param=value`.
- `BinaryValue::data` uses `buffer.len()`.

*Headers*
- All quoted header sentences were found in the 152 headers.

*CEF source*
- `browser_view_impl.cc`: "Cannot add multiple Chrome style BrowserViews" and the destructor → `WindowDestroyed` logic.
- Popup order in `CreateInternal`.
- Alloy `OpenURLFromTab` → `LoadMainFrameURL`.
- `CloseContents` → `CloseHostWindow` → `widget->Close()`.
- `WasHidden` DCHECK (Alloy) and `NOTIMPLEMENTED` (Chrome).
- JS dialogs through `TabModalDialogManager` unless windowless with no parent.
- `FileSelectHelper`.
- Permission defaults: DENY for media, IGNORE for prompts.
- Download delegate: DIR_TEMP fallback, directory creation, the "Should return true…" log, `OnBrowserDestroyed` nulling, callbacks keyed by id.
- Audio: `mute_source = true`, 2 s `kRecentlyAudibleTimeout`.
- `SetAccelerator` replaces by `command_id`, does nothing without a widget, and `widget_` is set before `OnWindowCreated`.
- DevTools: `CHECK(IsChromeStyle())`, `use_default_window = !life_span_handler`, `OnBeforeDevToolsPopup` called for Alloy openers.
- `ChromeBrowserContext` direct-child rule and `FromCachePath` reuse.
- `RequestFocus` is async (#3040).
- The cefclient overlay quote and the `ID_POPOUT_OVERLAY` accelerator.
- `SetAudioMuted`, `Zoom`, `Find`, `Print` and `StartDownload` all post to UI.

**Errors found and fixed inline (VERIFIED-FIX)**
1. **Close sequence (most important).**
   - `do_close`→1 does *not* leave the browser `PENDING`. `CloseContents` resets it to `NONE`.
   - "`close_browser(1)` + `do_close`→1 leaks (#3376)" is not supported by the source. #3376's repro is `CloseBrowser(false)` plus a synchronous detach in `DoClose` that never releases the view.
   - Replaced with the real traps: a lingering reference, a synchronous detach inside `do_close`, and dropping the view while in state `ACCEPTED`. The last one also has a null WeakPtr crash risk in `GetWindowWidget`.
2. `on_browser_destroyed` is **not** called when a tab is destroyed by releasing its last BrowserView reference (`InvalidateWeakPtrs()` runs before `WindowDestroyed()`). Fixed in §0.3, §0.9, §2.2 snippet comment, §5.3 step 4 and §14.4.
3. Rust trait defaults differ from C++:
   - `audio_parameters` is 0 (so AudioHandler does *not* mute by default in cef-rs).
   - `can_download` is 0 (cancels downloads).
   - `can_resize`/`can_maximize`/`can_minimize` are 0. The §7.2 `MainWindowDelegate` snippet now overrides them.
4. The unit-struct form of multi-level `wrap_*!` macros doesn't compile.
5. Cloning a `CefStringList`: iteration silently returns empty; passing the clone back to CEF is UB (was "UB or crash").
6. Chrome-style Window nuance: Alloy BrowserViews set the theme profile, so a Chrome-style BrowserView or DevTools can't be added after them.
7. DevTools client selection: `ShowDevTools` params vs the opener's client vs `default_client`.
8. Smaller fixes:
   - Missing `WindowOpenDisposition::NEW_SPLIT_VIEW`.
   - `PopupFeatures.size`.
   - `Create()` → `CreateInternal()`.
   - The sta:// blocking vs error-page conflict.
   - The force-close path needs the beforeunload auto-accept (async `on_before_close`).

**Remaining uncertainties (not runtime-tested)**
- Downloads with no handler: the fall-through to Chrome's delegate is confirmed. "Silent save to the Downloads dir with no UI" for Alloy is inferred.
- Force-close with a page `beforeunload` that needs a dialog: the crash risk in `GetDialogPosition` is inferred from source only.
- High-priority accelerators bypassing web content, and Ctrl+Tab behavior, rely on header semantics and Chromium FocusManager behavior, not a test.
- Zoom scope per host is plausible (Chrome `ZoomController`) but wasn't re-read in source.
- `set_chrome_color_scheme` effect on `prefers-color-scheme`: unverified.
- The forum threads t=20094 and t=19186 weren't re-read.
- Snippets were checked by inspection against the signatures, not compiled. Helpers such as `urlencode`, `unique_path` and `devtools_client` are placeholders.
