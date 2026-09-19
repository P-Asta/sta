# sta: how the process starts, CEF Settings, and Windows setup (cef 152.3.0+152.0.6 / CEF 152.0.6 / Chromium 152.0.7977.83)

Where the facts come from (all read locally unless marked [web]):
- `B` = `~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/cef-152.3.0+152.0.6/src/bindings/x86_64_pc_windows_msvc.rs`. `L123` below means line 123 of that file.
- `args.rs`, `string.rs`, `rc.rs` in the same crate. `build_util/win/mod.rs` plus `cef-app.exe.manifest`.
- `cef-dll-sys-152.3.0+152.0.6/build.rs`.
- The cefsimple and osr examples, plus `tests_shared` (cef-rs git clone).
- The headers in `.cef/152.0.6/cef_windows_x86_64/include`.
- The existing `Astatine/crates/sta/src/main.rs` and `target/debug/debug.log` (read only).
- `windows-sys-0.61.2` and `embed-resource-3.0.6` sources.

---

## 0. Summary

| Question | Answer |
|---|---|
| Is `bootstrap.exe` needed when the sandbox is off? | **No.** Build a normal `sta.exe` and pass `null` as `windows_sandbox_info`. Keep `cef` at `default-features = false`, which the workspace already does. |
| Can the same exe run the subprocesses? | Yes. Leave `browser_subprocess_path` empty. Every process must call `api_hash` first, then `execute_process(args, Some(&mut app), null)`, using the **same App type** so custom schemes and the render handler exist in every process. |
| Does it need a manifest? | CEF 152 starts without one: our exe has none today and still launches. **Embed one anyway.** CEF's own `bootstrap.exe` embeds a compatibility manifest (supportedOS Vista…10/11, `maxversiontested 10.0.18362.0`, Common-Controls v6, asInvoker). Add PerMonitorV2 DPI to that. Use `embed-resource` 3 with a generated `.rc` that also carries the icon and VERSIONINFO. |
| Where does user data go? | `%LOCALAPPDATA%\sta\User Data` as both `root_cache_path` and `cache_path`. Use `…\sta Dev\…` for debug builds. Put logs in `%LOCALAPPDATA%\sta\Logs\cef.log`. |
| Single instance | Comes free, keyed on `root_cache_path`. The second launch's `initialize` returns 0 and `get_exit_code()` returns `24` (`NORMAL_EXIT_PROCESS_NOTIFIED`). The first process gets `on_already_running_app_relaunch(cmdline, cwd)` on the UI thread. **Always return 1**, otherwise CEF opens a Chrome-style window. |
| Remote debugging for tests | `Settings.remote_debugging_port` (1024–65535), gated to debug builds or an env var. Or `--remote-debugging-port=0` combined with a temp `--sta-data-dir`, then read `DevToolsActivePort`. Clients that send an `Origin` header (browsers, Python `websocket-client`) need `--remote-allow-origins=…`. |
| **Gotcha 1: window flags default to off** | `ImplWindowDelegate::can_resize/can_maximize/can_minimize/can_close` return `Default::default()` = **0** in Rust, while the C++ defaults are `true`. The `wrap_window_delegate!` macro always installs those callbacks. **Override them to return 1**, or the frameless window cannot be resized or maximized. |
| **Gotcha 2: handler created per call** | `App::browser_process_handler()` "is called on multiple threads" and more than once. cefsimple returns a **new** handler object on every call, so any state stored in it is lost. Create it once, store it in the App struct and return `.clone()`. |

---

## 1. `main()` for the browser process and subprocesses (same exe, no sandbox)

### 1.1 Does Windows need bootstrap.exe? No

- **CEF docs** ([sandbox_setup][sbx], [web]): M138 changed the sandbox on Windows so the client builds as a DLL and runs through `bootstrap.exe`/`bootstrapc.exe`. The same page offers the alternative: *"Build your client application as an executable with the sandbox disabled. This requires no code changes to existing applications beyond passing nullptr as the `windows_sandbox_info` parameter to `CefExecuteProcess` and `CefInitialize`."*
- **`cef_sandbox_win.h`**: `RunWinMain(...)` is only the *"Entry point to be implemented by client DLLs using bootstrap.exe"*.
- **cefsimple `main.rs`**:
  - `#[cfg(not(all(feature = "sandbox", target_os = "windows")))] fn main()` is an ordinary exe that passes `std::ptr::null_mut()` as `sandbox_info`.
  - With `sandbox` on Windows, `main()` just returns `Err("Running in sandbox mode on Windows requires bootstrap.exe or bootstrapc.exe.")`. The real entry point becomes `#[unsafe(no_mangle)] extern "C" fn RunWinMain` in the cdylib (`win.rs`).
- **`build_util/win/mod.rs` `copy_app`** copies `bootstrap.exe` → `<name>.exe` only under `#[cfg(feature = "sandbox")]`.
- **cef crate features**: `default = ["sandbox","build-util","resources"]` and `sandbox = ["cef-dll-sys/sandbox"]`. The sys build script passes `USE_SANDBOX=ON|OFF` to the wrapper CMake. The workspace pins `cef = { version = "=152.3.0", default-features = false }`, which is correct.
- **Harmless leftovers**: the sys build script still copies `bootstrap.exe`/`bootstrapc.exe` into `target/<profile>` because `copy_directory` copies every file in the CEF root. They are not used at runtime and should not ship.

### 1.2 API used, verbatim from the bindings

```rust
pub fn api_hash(version: ::std::os::raw::c_int, entry: ::std::os::raw::c_int) -> *const ::std::os::raw::c_char   // B L56201
pub fn execute_process(args: Option<&MainArgs>, application: Option<&mut App>, windows_sandbox_info: *mut u8) -> ::std::os::raw::c_int   // B L58226
pub fn initialize(args: Option<&MainArgs>, settings: Option<&Settings>, application: Option<&mut App>, windows_sandbox_info: *mut u8) -> ::std::os::raw::c_int   // B L58252
pub fn get_exit_code() -> ::std::os::raw::c_int      // B L58289
pub fn shutdown()                                     // B L58297
pub fn run_message_loop()                             // B L58311
pub fn quit_message_loop()                            // B L58318
pub fn command_line_create() -> Option<CommandLine>   // B L57770
pub fn command_line_get_global() -> Option<CommandLine> // B L57782
pub fn currently_on(thread_id: ThreadId) -> ::std::os::raw::c_int // B L57820
pub struct MainArgs { pub instance: HINSTANCE }       // B L375
pub struct Resultcode(cef_resultcode_t);              // B L46853; Resultcode::NORMAL_EXIT_PROCESS_NOTIFIED (L46900); pub fn get_raw(&self) -> i32 (L46962)
// cef-dll-sys: pub const CEF_API_VERSION_LAST: i32 = 15200;
// cef::args (args.rs): impl Args { pub fn new() -> Self (L17, Windows: MainArgs{instance: GetModuleHandleW(null)}); pub fn as_main_args(&self) -> &MainArgs (L53); pub fn as_cmd_line(&self) -> Option<CommandLine> (L65) }
```

### 1.3 How each step behaves (from the headers)

- **`api_hash` (`cef_api_hash.h`)**: *"Configures the CEF API version … any changes to this value will be ignored after the first call."* The C++ wrapper does this inside `CefExecuteProcess`/`CefInitialize` (`libcef_dll_wrapper.cc` L65: `cef_api_hash(CEF_API_VERSION, 0)` plus a `CHECK`). The Rust crate calls the C API `cef_execute_process` directly, so **you must call `api_hash(sys::CEF_API_VERSION_LAST, 0)` yourself, first, in every process**. The sys build compiles the wrapper with `CEF_API_VERSION=15200` to match.
- **`execute_process` (`cef_app.h`)**:
  - *"If called for the browser process (identified by no "type" command-line value) it will return immediately with a value of -1. If called for a recognized secondary process it will block until the process should exit and then return the process exit code."*
  - Pass the App: `CefApp::OnRegisterCustomSchemes` *"is called on the main thread for each process and the registered schemes should be the same across all processes."* Renderers also get `render_process_handler()` from this App.
  - The existing sta `main.rs` already does this correctly. cefsimple passes `None`, which is wrong for us.
- **`initialize`**: *"Returns false if initialization fails or if early exit is desired (for example, due to process singleton relaunch behavior). If this function returns false then the application should exit immediately without calling any other CEF functions except, optionally, CefGetExitCode."*
- **`get_exit_code`**: after a failed `initialize`, returns `CEF_RESULT_CODE_NORMAL_EXIT_PROCESS_NOTIFIED = 24` (relaunch forwarded), `CEF_RESULT_CODE_PROFILE_IN_USE = 21`, and so on (`cef_types.h` L1106/L1113).
- **`run_message_loop`**: *"should only be called on the main application thread and only if CefInitialize() is called with a cef_settings_t.multi_threaded_message_loop value of false. This function will block until a quit message is received."*
- **`shutdown`**: *"Do not call any other CEF functions after calling this function."* So **release every `Window`/`BrowserView`/`Browser`/`Client` you hold, including ones in statics or thread-locals, before calling `shutdown()`**. A `RefGuard` dropped later calls `release` into libcef. Rust-implemented objects (`App`) are fine: their release only touches `RcImpl`.

Extra gotchas:
- **Nothing with side effects before `execute_process`.** Renderer, GPU and utility processes re-run `main()`, so no log truncation, directory creation, panic dialogs or single-instance mutexes before it. Keep `StaApp::new(...)` cheap.
- **`Args::as_cmd_line()` breaks quoting on Windows** (args.rs L65–73). It rebuilds the line as `std::env::args().collect::<Vec<_>>().join(" ")`, which loses quotes, e.g. for paths containing spaces. Avoid it.
  - To tell browser from subprocess, use the `execute_process` return value.
  - For a CEF `CommandLine` before `initialize`, use `command_line_create()` + `init_from_string(GetCommandLineW())`.
  - After `initialize`, use `command_line_get_global()`, which is read-only.
- **A panic inside any `wrap_*!` callback aborts the process.** The callbacks are `extern "C" fn`, so unwinding cannot cross them. Log panics with a panic hook (see §6).

### 1.4 How the `wrap_*!` macros work

Macro source (each is a `macro_rules!` one-liner): `wrap_app!` B L33673, `wrap_browser_process_handler!` L29211, `wrap_browser_view_delegate!` L37739, `wrap_window_delegate!` L43304.

- **Form**: `wrap_xxx! { [vis] struct Name { [vis] field: Type, ... } impl <Iface> { fn ...(&self, ...) -> R { ... } ... } }`.
- **Unit form `[vis] struct Name;`** works only for single-interface macros (`wrap_app!`, `wrap_browser_process_handler!`, `wrap_client!` …).
  - For the Views delegate macros, the unit-form rule re-invokes the macro with **only** the leaf `impl` block. That doesn't match the field-form rule, which requires the `ViewDelegate`/`PanelDelegate` blocks, and it would discard any `ViewDelegate` methods anyway.
  - So **always use braces** (`struct Name {}`) for `wrap_window_delegate!` and `wrap_browser_view_delegate!`.
- **Generated struct**: your fields plus a hidden `cef_object: *mut $crate::rc::RcImpl<$crate::sys::_cef_xxx_t, Self>`.
- **Constructor**: `pub fn new(field1, field2, ...) -> Xxx`. Arguments follow **field declaration order** and it returns the ref-counted CEF type (`App`, `BrowserProcessHandler`, `WindowDelegate` …). Example: `StaApp::new(bph)` returns `App`.
- **Impls the macro emits**:
  - `WrapXxx` (stores `cef_object`).
  - `Clone`: does `add_ref` and clones **each field**, so every field type must be `Clone`. A `RefCell<T>` field gets *copied*, so prefer `Arc<…>` for shared state.
  - `cef::rc::Rc`.
  - `ImplXxx` containing your methods. Methods you leave out use the trait defaults.
- **Required `impl` blocks** are exactly the interface chain, in order, and may be empty:
  - `wrap_app!` → `impl App {}`
  - `wrap_browser_process_handler!` → `impl BrowserProcessHandler {}`
  - `wrap_browser_view_delegate!` → `impl ViewDelegate {} impl BrowserViewDelegate {}`
  - `wrap_window_delegate!` → `impl ViewDelegate {} impl PanelDelegate {} impl WindowDelegate {}`
- **Method signatures** must match the trait exactly; copy them from the bindings. `i32` is accepted for `::std::os::raw::c_int`. Parameter names must be identifiers, so write `_window`, not `_`. `#[attr]` on methods is allowed; attributes on fields are not.
- **Imports**: the expansion names `App::new`, `WrapApp`, `ImplApp` (and so on) *unqualified*, so the call site needs `use cef::*;` or explicit imports of those names, as the osr example does.
- **Threads**: nothing is forced to be `Send`/`Sync`. `RefGuard` is `unsafe impl Send + Sync` (rc.rs L283), and CEF calls some interfaces from several threads (see §4), so thread safety is your responsibility.

### 1.5 `main.rs` to adapt

```rust
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;    // wrap_app!, wrap_browser_process_handler!, settings()
mod paths;  // AppDirs (§9)
#[cfg(windows)]
mod win;    // DWM, AUMID, console attach (§6, §7)

use cef::*;

fn main() {
    // 1) FIRST CEF call in EVERY process (browser + renderer/gpu/utility...).
    let _ = api_hash(sys::CEF_API_VERSION_LAST, 0);

    // 2) Windows MainArgs = { instance: GetModuleHandleW(null) } (args.rs L17).
    let args = args::Args::new();

    // 3) Same App in every process (custom schemes, render_process_handler). Cheap, no side effects.
    let shared = app::Shared::new(); // Arc<Mutex<State>>; empty in subprocesses
    let mut app = app::StaApp::new(app::StaBph::new(shared.clone()));

    // 4) Subprocess: blocks and returns exit code >= 0. Browser process: -1 immediately.
    let code = execute_process(Some(args.as_main_args()), Some(&mut app), std::ptr::null_mut());
    if code >= 0 {
        std::process::exit(code);
    }

    // ---------------- browser process only ----------------
    let dirs = paths::AppDirs::resolve().expect("create data dirs");
    #[cfg(windows)]
    win::init_process(&dirs); // panic hook -> log file, optional AttachConsole, AUMID

    let settings = app::settings(&dirs);
    if initialize(Some(args.as_main_args()), Some(&settings), Some(&mut app), std::ptr::null_mut()) != 1 {
        let rc = get_exit_code();
        if rc == Resultcode::NORMAL_EXIT_PROCESS_NOTIFIED.get_raw() {
            std::process::exit(0); // argv was forwarded to the running instance (§4)
        }
        std::process::exit(rc); // e.g. 21 PROFILE_IN_USE
    }
    drop(settings); // CEF copied the struct; our CefStrings can be freed now

    run_message_loop(); // returns after quit_message_loop() (call it when the last window is destroyed)
    *shared.0.lock().unwrap() = Default::default(); // drop Window/BrowserView/Browser/Client handles BEFORE shutdown
    shutdown();
}
```

---

## 2. `Settings` (B L513–L544; `Default` at L625)

`impl Default for Settings` = `Self { size: size_of::<_cef_settings_t>(), ..unsafe { std::mem::zeroed() } }`. Every int field starts at 0, every string is empty, and `LogSeverity(0)` = DEFAULT.

| Field (exact) | Type | Line | Header semantics (cef_types.h) and recommendation |
|---|---|---|---|
| `size` | `usize` | 514 | Filled by `Default`. |
| `no_sandbox` | `c_int` | 515 | **1**. |
| `browser_subprocess_path` | `CefString` | 516 | *"If this value is empty on Windows or Linux then the main process executable will be used."* **Leave empty.** |
| `framework_dir_path` / `main_bundle_path` | `CefString` | 517/518 | macOS only. |
| `multi_threaded_message_loop` | `c_int` | 519 | **0**: use `run_message_loop` on the main thread (the Views UI thread = main thread). |
| `external_message_pump` | `c_int` | 520 | 0. |
| `windowless_rendering_enabled` | `c_int` | 521 | **0**: *"may reduce rendering performance"*. |
| `command_line_args_disabled` | `c_int` | 522 | 0 in dev so `--remote-debugging-port` etc. work. ~~Consider **1 in release**~~ **VERIFIED-FIX: keep 0 in release too.** CEF's `ChromeMainDelegateCef::BasicStartupComplete` does, for the browser process, `// Remove any existing command-line arguments.` `argv.push_back(command_line->GetProgram().value()); command_line->InitFromArgv(argv);` and clears the switch map ([chrome_main_delegate_cef.cc][cefmain], [web]). That removes **non-switch arguments (URLs/files) too**, so `command_line_get_global().arguments()` is empty in `on_context_initialized` (§4). Worse, Chromium's `AttemptToNotifyRunningChrome` forwards `base::CommandLine new_command_line(*base::CommandLine::ForCurrentProcess())` to the running instance ([chrome_process_finder.cc][finder], [web]), i.e. the already-stripped line, so `on_already_running_app_relaunch` receives no URLs either. Instead, in release strip dangerous switches in `on_before_command_line_processing` with `remove_switch` (e.g. `remote-debugging-port`, `remote-debugging-pipe`, `disable-web-security`, `load-extension`; `--user-data-dir` is already ignored in the browser process when `root_cache_path` is set, per CEF `resource_util.cc` `GetUserDataPath`). OnBeforeCommandLineProcessing still runs either way (it is called after the reset), and `std::env::args` is unaffected. |
| `cache_path` | `CefString` | 523 | *"If this value is empty then browsers will be created in "incognito mode"… Any child directory value will be ignored and the "default" profile … will be used instead."* **Set it equal to `root_cache_path`.** |
| `root_cache_path` | `CefString` | 524 | *"A process singleton lock based on the root_cache_path value is therefore used… You should customize root_cache_path … and implement … OnAlreadyRunningAppRelaunch."* Our current debug.log already shows `WARNING: Please customize CefSettings.root_cache_path …`. **`%LOCALAPPDATA%\sta\User Data`** (must be absolute). |
| `persist_session_cookies` | `c_int` | 525 | **1** for a browser; needs `cache_path`. |
| `user_agent` | `CefString` | 526 | Empty. |
| `user_agent_product` | `CefString` | 527 | *"inserted as the product portion of the default User-Agent string. If empty the Chromium product version will be used."* **Leave empty**: replacing `Chrome/152…` breaks UA sniffing. |
| `locale` | `CefString` | 528 | *"If empty the default locale of "en-US" will be used"*, which is **not** the OS locale (confirmed in CEF source: `else if (!command_line->HasSwitch(switches::kLang)) command_line->AppendSwitchASCII(switches::kLang, "en-US");`). Pass the OS UI language. VERIFIED-FIX: `GetUserDefaultLocaleName` returns the user's *regional-format* locale (e.g. `de-DE` formats on an English UI), not the display language. Use `GetUserPreferredUILanguages(MUI_LANGUAGE_NAME, ...)` (windows-sys `Win32_Globalization`; snippet in §6). |
| `log_file` | `CefString` | 529 | *"If empty … a "debug.log" file will be written in the main executable directory"* (confirmed at `target/debug/debug.log`). **Set to `%LOCALAPPDATA%\sta\Logs\cef.log`** and create the directory first. |
| `log_severity` | `LogSeverity` | 530 | `LogSeverity::{DEFAULT(=INFO), VERBOSE, INFO, WARNING, ERROR, FATAL, DISABLE}` (B L45718). Use INFO in dev and WARNING in release. |
| `log_items` | `LogItems` | 531 | `LogItems::DEFAULT`. The flags are a newtype over a C enum, so OR-ing them needs raw ints; not worth it. |
| `javascript_flags` | `CefString` | 532 | Empty. |
| `resources_dir_path` / `locales_dir_path` | `CefString` | 533/534 | Empty: .pak files and `locales\` sit next to the exe (§9). |
| `remote_debugging_port` | `c_int` | 535 | *"Set to a value between 1024 and 65535 to enable remote debugging"*. Debug/automation builds only (§8). |
| `uncaught_exception_stack_size` | `c_int` | 536 | E.g. 20, if our renderer-side Rust wants `OnUncaughtException` for UI error reporting. |
| `background_color` | `u32` (ARGB) | 537 | *"alpha … must be either fully opaque (0xFF) or fully transparent… transparent for a windowed browser then the default value of opaque white"*. Use `0xFF1C1C1E` to avoid white flashes. |
| `accept_language_list` | `CefString` | 538 | e.g. `"en-US,en"`, derived from the OS. |
| `cookieable_schemes_list` / `cookieable_schemes_exclude_defaults` | `CefString` / `c_int` | 539/540 | Empty/0 unless `sta://` pages need cookies. |
| `chrome_policy_id` | `CefString` | 541 | Optional: `"SOFTWARE\\Policies\\sta"` enables policy registry. |
| `chrome_app_icon_id` | `c_int` | 542 | *"ID for an ICON resource … loaded from the main executable and used when creating default Chrome windows such as DevTools and Task Manager. If unspecified … IDR_MAINFRAME [101] … from libcef.dll."* **Set to our icon's resource ID (1).** |
| `disable_signal_handlers` | `c_int` | 543 | POSIX only. |
| `use_views_default_popup` | `c_int` | 544 | Only matters for native-hosted (non-Views) Chrome-style popups; irrelevant for us. |

### How `CefString` fields work (string.rs)

- `pub type CefString = CefStringUtf16;` (B L42).
- `impl From<&str> for CefStringUtf16` (string.rs L489) allocates an **owned** copy (`CefStringData::Clear`) that `Drop` frees with `cef_string_utf16_clear`.
- `initialize` does `settings.cloned().map(|arg| arg.into())`. `Clone` for `CefStringData` produces a **borrowed** shallow copy, and `From<CefStringUtf16> for _cef_string_utf16_t` (L551) only yields data for `Borrowed`. So the original `Settings` must stay alive until `initialize` returns, which it naturally does because you pass `&settings`.
- Reading strings back:
  - `CefStringUserfree` → `CefString`: `CefString::from(&userfree)` (L504).
  - `CefString` → Rust: `.to_string()` (via `Display`, L624).
  - Shorthand argument: `Some(&"x".into())` works for `Option<&CefString>`.

```rust
// app.rs (part)
use crate::paths::AppDirs;
use cef::*;

pub fn settings(d: &AppDirs) -> Settings {
    let s = |p: &std::path::Path| CefString::from(p.to_string_lossy().as_ref());
    Settings {
        no_sandbox: 1,
        root_cache_path: s(&d.user_data),
        cache_path: s(&d.user_data),
        persist_session_cookies: 1,
        log_file: s(&d.logs.join("cef.log")),
        log_severity: if cfg!(debug_assertions) { LogSeverity::INFO } else { LogSeverity::WARNING },
        locale: CefString::from(crate::win::os_ui_locale().as_str()), // "" -> en-US
        background_color: 0xFF1C_1C1E,
        chrome_app_icon_id: 1,
        // VERIFIED-FIX: was `if cfg!(debug_assertions) { 0 } else { 1 }`. 1 strips argv URLs and the
        // relaunch-forwarded command line (see table). Filter switches in on_before_command_line_processing instead.
        command_line_args_disabled: 0,
        remote_debugging_port: crate::app::remote_debugging_port().unwrap_or(0),
        ..Default::default()
    }
}

pub fn remote_debugging_port() -> Option<i32> {
    if !(cfg!(debug_assertions) || cfg!(feature = "automation")) {
        return None;
    }
    std::env::var("STA_REMOTE_DEBUGGING_PORT").ok()?.parse::<i32>().ok()
        .filter(|p| (1024..=65535).contains(p))
}
```

---

## 3. `App::on_before_command_line_processing` and useful switches

Verbatim signatures:

```rust
// trait ImplApp (B L33644)
fn on_before_command_line_processing(&self, process_type: Option<&CefString>, command_line: Option<&mut CommandLine>) // L33646
fn on_register_custom_schemes(&self, registrar: Option<&mut SchemeRegistrar>)   // L33653
fn browser_process_handler(&self) -> Option<BrowserProcessHandler>              // L33659
fn render_process_handler(&self) -> Option<RenderProcessHandler>                // L33663
// trait ImplCommandLine (B L28530)
fn has_switch(&self, name: Option<&CefString>) -> ::std::os::raw::c_int          // L28558
fn switch_value(&self, name: Option<&CefString>) -> CefStringUserfree            // L28560
fn append_switch(&self, name: Option<&CefString>)                                // L28564
fn append_switch_with_value(&self, name: Option<&CefString>, value: Option<&CefString>) // L28566
fn arguments(&self, arguments: Option<&mut CefStringList>)                       // L28570
fn remove_switch(&self, name: Option<&CefString>)                                // L28576
fn init_from_string(&self, command_line: Option<&CefString>)                     // L28544
// trait ImplSchemeRegistrar (B L33367)
fn add_custom_scheme(&self, scheme_name: Option<&CefString>, options: ::std::os::raw::c_int) -> ::std::os::raw::c_int
```

Header semantics (`cef_app.h`, `cef_command_line.h`):
- *"The |process_type| value will be empty for the browser process. Do not keep a reference to the CefCommandLine object… Any values specified in CefSettings that equate to command-line arguments will be set before this method is called. Be cautious when using this method to modify command-line arguments for non-browser processes as this may result in undefined behavior including crashes."*
- Switch names go in **without `--`** and must be lowercase ASCII.
- **Don't** do `append_switch("enable-logging=stderr")` as the osr example does. That creates a switch literally named `enable-logging=stderr`. Use `append_switch_with_value`.
- Appending the same switch twice keeps the **last** value (Chromium's switch map). `--enable-features` / `--disable-features` must therefore be **merged** into one comma-separated value.

Switches worth considering. Chromium switch and feature names drift, and unknown names are ignored silently, so verify in `chrome://version` (command line) once running.

| Switch | Use |
|---|---|
| `remote-debugging-port=<n>` (or `0` = ephemeral) | Automation (§8). Prefer the Settings field; the switch is needed for `0`. |
| `remote-allow-origins=http://127.0.0.1:<port>` or `*` | Only if a CDP client sends an `Origin` header (§8). Debug builds only. |
| `force-dark-mode` / `force-light-mode` | Mentioned in `cef_window_delegate.h`: *"Native/OS theme changes can be disabled by passing the `--force-dark-mode` or `--force-light-mode` command-line flag."* |
| `autoplay-policy=no-user-gesture-required` | Test builds only. A real browser should keep Chromium's default. |
| `disable-features=A,B` / `enable-features=…` | Puppeteer's defaults disable `Translate,MediaRouter,OptimizationHints,AcceptCHFrame` [web]. `Translate` and `MediaRouter` are sensible for a Chrome-style embedded browser that lacks those UIs. |
| `force-device-scale-factor=1`, `force-color-profile=srgb` | Deterministic CDP screenshots in tests. |
| `disable-renderer-backgrounding`, `disable-background-timer-throttling` | Test mode only, so hidden tab views keep running. Not for production: they cost battery. |
| `hide-crash-restore-bubble`, `noerrdialogs` | Used by the osr example. Avoids Chrome-style restore UI we don't render. |
| `enable-logging=stderr`, `v=1`, `log-severity=verbose` | Temporary debugging (§6). |
| `disable-gpu`, `disable-gpu-compositing` | Diagnosis only (GPU driver issues). |

```rust
wrap_app! {
    pub struct StaApp {
        bph: BrowserProcessHandler, // created once (see §4)
    }

    impl App {
        fn on_before_command_line_processing(
            &self,
            process_type: Option<&CefString>,
            command_line: Option<&mut CommandLine>,
        ) {
            let Some(cl) = command_line else { return };
            let is_browser = process_type.map_or(true, |p| p.to_string().is_empty());
            if !is_browser {
                return;
            }
            merge_list_switch(cl, "disable-features", &["Translate", "MediaRouter"]);
            if cfg!(not(debug_assertions)) {
                // VERIFIED-FIX: this filtering replaces command_line_args_disabled=1 (which also drops argv URLs).
                // (user-data-dir needs no filtering: CEF's resource_util GetUserDataPath prefers a non-empty root_cache_path.)
                for sw in ["remote-debugging-port", "remote-debugging-pipe", "disable-web-security", "load-extension"] {
                    cl.remove_switch(Some(&sw.into()));
                }
            } else if std::env::var_os("STA_TEST").is_some() {
                cl.append_switch_with_value(Some(&"force-device-scale-factor".into()), Some(&"1".into()));
                cl.append_switch(Some(&"disable-renderer-backgrounding".into()));
            }
        }

        fn on_register_custom_schemes(&self, registrar: Option<&mut SchemeRegistrar>) {
            // Runs in EVERY process (that's why execute_process gets this App). Options: see scheme report.
            let Some(r) = registrar else { return };
            let opts = SchemeOptions::STANDARD.get_raw() | SchemeOptions::SECURE.get_raw()
                | SchemeOptions::CORS_ENABLED.get_raw() | SchemeOptions::FETCH_ENABLED.get_raw();
            r.add_custom_scheme(Some(&"sta".into()), opts);
        }

        fn browser_process_handler(&self) -> Option<BrowserProcessHandler> {
            Some(self.bph.clone())
        }
    }
}

fn merge_list_switch(cl: &CommandLine, name: &str, add: &[&str]) {
    let key = CefString::from(name);
    let cur = CefString::from(&cl.switch_value(Some(&key))).to_string();
    let mut items: Vec<String> = cur.split(',').filter(|s| !s.is_empty()).map(Into::into).collect();
    for a in add {
        if !items.iter().any(|i| i == a) {
            items.push((*a).into());
        }
    }
    cl.append_switch_with_value(Some(&key), Some(&CefString::from(items.join(",").as_str())));
}
```

`SchemeOptions::get_raw(&self) -> i32` (B L50038).

---

## 4. `BrowserProcessHandler`

```rust
// trait ImplBrowserProcessHandler (B L29172)
fn on_register_custom_preferences(&self, type_: PreferencesType, registrar: Option<&mut PreferenceRegistrar>) // L29174
fn on_context_initialized(&self)                                                     // L29181
fn on_before_child_process_launch(&self, command_line: Option<&mut CommandLine>)     // L29183
fn on_already_running_app_relaunch(&self, command_line: Option<&mut CommandLine>, current_directory: Option<&CefString>) -> ::std::os::raw::c_int // L29185
fn on_schedule_message_pump_work(&self, delay_ms: i64)                               // L29193
fn default_client(&self) -> Option<Client>                                           // L29195
fn default_request_context_handler(&self) -> Option<RequestContextHandler>           // L29199
```

What `cef_browser_process_handler.h` says about each callback:
- **`OnContextInitialized`**: *"Called on the browser process UI thread immediately after the CEF context has been initialized."* Create the Client, the top-level `Window` (`window_create_top_level`, B L59477) and the initial tabs here.
- **`OnBeforeChildProcessLaunch`**: *"Will be called on the browser process UI thread when launching a render process and on the browser process IO thread when launching a GPU process… Do not keep a reference to |command_line|."*
  - **Don't touch UI-thread-only state here.** A `RefCell` accessed from the IO thread is undefined behavior.
  - Chromium only copies a whitelist of browser switches to children. Custom switches such as `--sta-ipc-version=1` for the renderer must be appended here. Read them in the renderer via `command_line_get_global()`.
- **`OnAlreadyRunningAppRelaunch`**:
  - *"called … when an already running app is relaunched with the same CefSettings.root_cache_path value… |command_line| will be read-only… Return true if the relaunch is handled or false for default relaunch behavior. Default behavior will create a new default styled Chrome window."*
  - *"On relaunch the app checks a process singleton lock and then forwards the new launch arguments to the already running app process before exiting early. Client apps should therefore check the CefInitialize() return value for early exit."*
  - *"called on the browser process UI thread."*
  - Upstream cefclient's version creates a root window from `command_line->Copy()` and returns `true`, logging that `--multi-threaded-message-loop`, `--off-screen-rendering-enabled` and `--use-views` *"are ignored on app relaunch"* ([client_browser.cc][clientbrowser], [web]).
- **`GetDefaultClient`**: *"If null is returned the CefBrowser will be unmanaged … and application shutdown will be blocked until the browser window is closed manually. This method is currently only used with Chrome style when creating new browser windows via Chrome UI."* sta returns `foreign::client()`: every browser Chromium creates on its own (extensions, the Web Store's post-install window, `Target.createTarget`) goes through it, is hidden and adopted as sta tabs, and is counted for shutdown (`docs/research/extensions.md`, ARCHITECTURE §4.5).
- **`App::GetBrowserProcessHandler`** (`cef_app.h`): *"This method is called on multiple threads in the browser process."* Return one stored instance.

How a second launch is forwarded:
1. The user runs `sta.exe https://x` while sta is running.
2. In the new process, `execute_process` returns -1, then `initialize` detects the singleton owned by the running process for that `root_cache_path` and sends it the argv plus the cwd.
3. The new process's `initialize` returns 0 and `get_exit_code()` returns 24, so exit quietly with 0.
4. The running process gets `on_already_running_app_relaunch` on its UI thread.
   - Non-switch arguments (URLs, file paths) come from `command_line.arguments(&mut CefStringList)`. Resolve relative paths against `current_directory`.
   - Our own switches come from `switch_value`.
   - VERIFIED-FIX (addition): what gets forwarded is **not** the raw `GetCommandLineW()`. It is `base::CommandLine::ForCurrentProcess()` of the second process, after CEF applied Settings-derived switches and after our `on_before_command_line_processing` ran there, plus `--source-shortcut=<lnk>` / `--source-app-id` when launched from a shortcut ([chrome_process_finder.cc][finder], [web]). So ignore unknown switches, and don't set `command_line_args_disabled=1`, which empties it (§2).
   - Activate the window. Chromium's process finder calls `AllowSetForegroundWindow` for the running process before notifying it, so `Window::activate()` normally gets foreground (Chromium behavior; verify empirically).
   - Return **1**.
- Different `root_cache_path` values give **independent instances**. This is how tests avoid colliding with a dev instance (§8).
- **Debug builds must use a different root than release**, otherwise `cargo run` silently hands off to the installed sta and exits 0.
- **Register protocol and file handlers with `--single-argument`**, e.g. `"…\sta.exe" --single-argument %1`. Chromium's Windows command-line parser then treats everything after it as one argument, which prevents switch injection through crafted URLs. This is Chromium `base::CommandLine` behavior; verify.

```rust
use cef::*;
use std::sync::{Arc, Mutex};

#[derive(Clone, Default)]
pub struct Shared(pub Arc<Mutex<State>>);
#[derive(Default)]
pub struct State {
    pub client: Option<Client>,
    pub main_window: Option<Window>, /* tabs, spaces ... */
}
impl Shared {
    pub fn new() -> Self {
        Self::default()
    }
}

wrap_browser_process_handler! {
    pub struct StaBph {
        shared: Shared,
    }

    impl BrowserProcessHandler {
        fn on_context_initialized(&self) {
            debug_assert_ne!(currently_on(ThreadId::UI), 0);
            let urls = command_line_get_global().map(|cl| startup_urls(&cl, None)).unwrap_or_default();
            let mut st = self.shared.0.lock().unwrap();
            st.client = Some(crate::client::StaClient::new(self.shared.clone()));
            drop(st);
            crate::ui::create_main_window(&self.shared, urls); // window_create_top_level(...)
        }

        fn on_before_child_process_launch(&self, command_line: Option<&mut CommandLine>) {
            // UI thread (renderer) OR IO thread (GPU): only touch the command line.
            let Some(cl) = command_line else { return };
            let ty = CefString::from(&cl.switch_value(Some(&"type".into()))).to_string();
            if ty == "renderer" {
                cl.append_switch_with_value(Some(&"sta-ipc-version".into()), Some(&"1".into()));
            }
        }

        fn on_already_running_app_relaunch(
            &self,
            command_line: Option<&mut CommandLine>,
            current_directory: Option<&CefString>,
        ) -> i32 {
            let cwd = current_directory.map(|c| std::path::PathBuf::from(c.to_string()));
            let urls = command_line.map(|cl| startup_urls(cl, cwd.as_deref())).unwrap_or_default();
            crate::ui::open_urls_or_focus(&self.shared, urls); // restore()/activate() main Window, add tabs
            1 // handled: never let CEF open a Chrome-style window
        }

        fn default_client(&self) -> Option<Client> {
            self.shared.0.lock().ok()?.client.clone()
        }
    }
}

pub fn startup_urls(cl: &CommandLine, cwd: Option<&std::path::Path>) -> Vec<String> {
    let mut list = CefStringList::new();
    cl.arguments(Some(&mut list));
    list.into_iter() // impl IntoIterator for CefStringList { type Item = String } (string.rs L937)
        .filter(|a| !a.is_empty())
        .map(|a| match cwd {
            Some(dir) if !a.contains("://") && std::path::Path::new(&a).is_relative() => {
                dir.join(&a).to_string_lossy().into_owned()
            }
            _ => a,
        })
        .collect()
}
```

Wiring in `main`: `StaApp::new(StaBph::new(Shared::new()))`, as in §1.5.

VERIFIED-FIX (addition): **the `std::sync::Mutex` in `Shared` is not re-entrant, and CEF Views calls delegates synchronously.** `CefWindowImpl::Create` runs `window->Initialize(); window->CreateWidget(parent_widget);`, and `CreateWidget` ends with `delegate()->OnWindowCreated(this)`, all inside `window_create_top_level` ([window_impl.cc][winimpl], [web]). Likewise `cef_window_delegate.h` says `OnThemeColorsChanged` *"will be triggered if/when a BrowserView is added to the Window's component hierarchy"*. So **never hold the lock across a CEF call.** `window_create_top_level`, `add_child_view`, `show`, `activate` and similar can re-enter `on_window_created`/`on_theme_colors_changed`, which lock again and deadlock. The snippet above drops `st` before `create_main_window`; `open_urls_or_focus` must do the same (clone the `Window` out, drop the guard, then call it). Since all of these callbacks run on the UI thread, a UI-thread-only `thread_local!`/`RefCell` store is a simpler alternative, keeping only the `Client` clone for `default_client` behind a lock.

---

## 5. Windows application manifest

**What exists today:**
- **CEF 152's own `bootstrap.exe` / `bootstrapc.exe`** (extracted from the PE with PowerShell) embed: `asInvoker`; `Microsoft.Windows.Common-Controls 6.0.0.0`; `supportedOS` Vista `{e2011457…}`, 7 `{35138b9a…}`, 8 `{4a2f28e3…}`, 8.1 `{1f676c76…}`, 10/11 `{8e0f7a12-bfb3-4fe8-b9a5-48fd50a15a9a}`; `<maxversiontested Id="10.0.18362.0"/>`. There is **no dpiAware element**.
- **cef-rs `bundle-cef-app`** (`build_util/win/mod.rs` `copy_app`) writes an **external** `<name>.exe.manifest` next to the exe. Its content (`cef-app.exe.manifest`) is the same, and it notes the section *"is from … %CEF_ROOT%/tests/cefsimple/win/compatibility.manifest"* and that `maxversiontested` *"is required for XAML islands usage in the process for media scenarios."*
- **The cefsimple example** only embeds icons via `winres` (`set_icon_with_id(..., IDI_CEFSIMPLE=120)`, `IDI_SMALL=121`) and relies on the bundler's external manifest.
- **Our current `target/debug/sta.exe`** has **no** manifest: a binary search for `urn:schemas-microsoft-com:asm.v1` finds 0 hits. rustc/link.exe add none, so embedding a resource manifest won't clash with a linker-generated one.

**Is it required?** It is not needed to start: the exe already runs. It is still recommended, because without it:
- Windows applies Win8 compatibility shims to the process (version lies, some APIs gated on `supportedOS`).
- Native dialogs lose v6 visual styles.
- DPI awareness is only set later, when Chromium sets it. CEF ≥108 *"sets the DPI awareness of the current process to 'Per monitor DPI aware' if no value has been previously set"* ([cef#3452][dpi], [web]). Declaring PerMonitorV2 in the manifest matches that, covers any Win32 calls we make before `initialize`, and gives the GPU subprocess (same exe) the same awareness.

Prefer **embedded** over external: an external `.exe.manifest` is ignored when an embedded one exists, and Windows caches activation contexts, so external files are unreliable.

On the optional elements:
- `activeCodePage UTF-8`: **omit**. Chromium uses wide APIs, and changing the ANSI code page affects `CP_ACP` conversions in Chromium and third-party DLLs for no benefit.
- `longPathAware`: harmless, and only takes effect if the OS policy is enabled. Include it.
- `heapType SegmentHeap`: omit (Chromium uses PartitionAlloc).
- `assemblyIdentity`: omit. A wrong `processorArchitecture` or `version` causes "side-by-side configuration is incorrect".

`crates/sta/res/sta.exe.manifest`:

```xml
<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <trustInfo xmlns="urn:schemas-microsoft-com:asm.v3">
    <security>
      <requestedPrivileges>
        <requestedExecutionLevel level="asInvoker" uiAccess="false"/>
      </requestedPrivileges>
    </security>
  </trustInfo>
  <compatibility xmlns="urn:schemas-microsoft-com:compatibility.v1">
    <application>
      <!-- Windows 10 and 11 (Win11 has no separate GUID) -->
      <supportedOS Id="{8e0f7a12-bfb3-4fe8-b9a5-48fd50a15a9a}"/>
      <!-- Windows 8.1, 8, 7, Vista (kept identical to CEF's bootstrap.exe) -->
      <!-- VERIFIED-FIX: the Vista GUID was missing; bootstrap.exe and cef-app.exe.manifest both list it -->
      <supportedOS Id="{1f676c76-80e1-4239-95bb-83d0f6d0da78}"/>
      <supportedOS Id="{4a2f28e3-53b9-4441-ba9c-d69d4a4a6e38}"/>
      <supportedOS Id="{35138b9a-5d96-4fbd-8e2d-a2440225f93a}"/>
      <supportedOS Id="{e2011457-1546-43c5-a5fe-008deee3d3f0}"/>
      <!-- Required for XAML islands usage in the process for media scenarios (CEF comment) -->
      <maxversiontested Id="10.0.18362.0"/>
    </application>
  </compatibility>
  <application xmlns="urn:schemas-microsoft-com:asm.v3">
    <windowsSettings>
      <dpiAware xmlns="http://schemas.microsoft.com/SMI/2005/WindowsSettings">true/pm</dpiAware>
      <dpiAwareness xmlns="http://schemas.microsoft.com/SMI/2016/WindowsSettings">PerMonitorV2</dpiAwareness>
      <longPathAware xmlns="http://schemas.microsoft.com/SMI/2016/WindowsSettings">true</longPathAware>
    </windowsSettings>
  </application>
  <dependency>
    <dependentAssembly>
      <assemblyIdentity type="win32" name="Microsoft.Windows.Common-Controls" version="6.0.0.0"
                        processorArchitecture="*" publicKeyToken="6595b64144ccf1df" language="*"/>
    </dependentAssembly>
  </dependency>
</assembly>
```

**Embedding with `embed-resource` 3.x** (3.0.6 is in the local registry).
- API used: `embed_resource::compile(path, embed_resource::NONE) -> CompilationResult`, then `.manifest_required()`.
- It emits `cargo:rustc-link-arg-bins=<res.lib>` (lib.rs L444), so the resource goes into **bins only**, not tests.
- It finds `rc.exe` in the Windows SDK and runs `rc.exe /fo <out> /I <OUT_DIR> <file.rc>`.
- The build script below generates the `.rc` with absolute paths, so it doesn't depend on rc.exe's working directory.
- It uses numeric constants, so no `#include <winres.h>` is needed: `24` = RT_MANIFEST, `1` = CREATEPROCESS_MANIFEST_RESOURCE_ID.
- Alternative: `winresource`/`winres` like cefsimple. `winres` 0.1 is unmaintained.

```toml
# crates/sta/Cargo.toml
[target.'cfg(windows)'.build-dependencies]
embed-resource = "3"
```

```rust
// crates/sta/build.rs
fn main() {
    #[cfg(windows)]
    {
        use std::{env, fs, path::PathBuf};
        let dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap()).join("res");
        let esc = |p: PathBuf| p.to_string_lossy().replace('\\', "\\\\");
        let ver = env::var("CARGO_PKG_VERSION").unwrap(); // "0.1.0"
        let mut v: Vec<u32> = ver.split(|c: char| !c.is_ascii_digit()).filter_map(|s| s.parse().ok()).collect();
        v.resize(4, 0);
        let rc = format!(
r#"1 ICON "{icon}"
1 24 "{manifest}"
1 VERSIONINFO
FILEVERSION {a},{b},{c},{d}
PRODUCTVERSION {a},{b},{c},{d}
FILEFLAGSMASK 0x3fL
FILEFLAGS 0x0L
FILEOS 0x40004L
FILETYPE 0x1L
FILESUBTYPE 0x0L
BEGIN
  BLOCK "StringFileInfo"
  BEGIN
    BLOCK "040904b0"
    BEGIN
      VALUE "CompanyName", "sta"
      VALUE "FileDescription", "sta"
      VALUE "FileVersion", "{ver}"
      VALUE "InternalName", "sta"
      VALUE "OriginalFilename", "sta.exe"
      VALUE "ProductName", "sta"
      VALUE "ProductVersion", "{ver}"
    END
  END
  BLOCK "VarFileInfo"
  BEGIN
    VALUE "Translation", 0x409, 1200
  END
END
"#,
            icon = esc(dir.join("sta.ico")),
            manifest = esc(dir.join("sta.exe.manifest")),
            a = v[0], b = v[1], c = v[2], d = v[3], ver = ver
        );
        let out = PathBuf::from(env::var("OUT_DIR").unwrap()).join("sta.rc");
        fs::write(&out, rc).unwrap();
        println!("cargo:rerun-if-changed=build.rs");
        println!("cargo:rerun-if-changed=res/sta.ico");
        println!("cargo:rerun-if-changed=res/sta.exe.manifest");
        embed_resource::compile(&out, embed_resource::NONE).manifest_required().unwrap();
    }
}
```

Notes:
- **Icon ID 1**: Explorer uses the lowest icon ID for the exe icon, and it matches `Settings.chrome_app_icon_id = 1`.
- **`FileDescription`** is what Task Manager shows for every sta subprocess.
- **Views window icons**: also set them at runtime with `ImplWindow::set_window_icon(&self, image: Option<&mut Image>)` (B L44288) and `set_window_app_icon(&self, image: Option<&mut Image>)` (L44292). Header: *"On Windows, this is the ICON_BIG used in Alt-Tab list and Windows taskbar."*
  - Build the `Image` with `pub fn image_create() -> Option<Image>` (L57259) and `ImplImage::add_png(&self, scale_factor: f32, png_data: Option<&[u8]>) -> c_int` (L4081).
  - Embed the PNGs with `include_bytes!` at 1x and 2x.
- **Taskbar grouping**: optionally call `SetCurrentProcessExplicitAppUserModelID(appid: PCWSTR) -> HRESULT` (windows-sys `Win32_UI_Shell`) in the browser process before any window is shown.

---

## 6. `#![windows_subsystem = "windows"]` and logging

- **Pattern from cefsimple**: `#![cfg_attr(all(not(debug_assertions), not(feature = "sandbox"), target_os = "windows"), windows_subsystem = "windows")]`. Use `#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]` in the **bin crate root** (`main.rs`). Rust still enters through `fn main` (`mainCRTStartup`); no WinMain is needed.
- **Why use it in release**:
  - With the console subsystem, launching from Explorer or a shortcut pops a console window.
  - Every subprocess re-runs the same exe and shares the parent's console.
  - The GUI subsystem shows no console for the browser or any subprocess.
- **What happens to output under the GUI subsystem**:
  - With no inherited handles, `GetStdHandle` returns null, and Rust's Windows stdio maps the invalid handle to success, so `println!`/`eprintln!` are **silently discarded** rather than panicking.
  - Chromium's `--enable-logging=stderr` output is lost too.
  - Default panic messages are lost, so install a hook.
- **Redirected handles still work.** When a parent process (Node `child_process.spawn` with pipes, PowerShell `Start-Process -RedirectStandardOutput`, a test harness) passes pipes through `STARTF_USESTDHANDLES`, a GUI-subsystem exe **does** write to them. Automated tests can capture stdout/stderr from release builds this way.
- **Getting logs**:
  1. **CEF/Chromium**: `Settings.log_file` = `%LOCALAPPDATA%\sta\Logs\cef.log` (directory created first), plus `log_severity`. Default location without it: `debug.log` next to the exe, which fails silently when installed under Program Files. Temporary extra verbosity: `--log-severity=verbose`, `--enable-logging=stderr`, `--v=1`. Subprocesses receive the log switches and write to the same file with pid prefixes (verify). ~~Rotate or trim the file yourself at browser start, after `execute_process` returns -1.~~ VERIFIED-FIX: **don't trim or rotate between `execute_process` and `initialize`.** A second launch also gets -1 from `execute_process` and only learns it is the forwarder when `initialize` returns 0, so trimming there wipes the *running* instance's log. Rotate after `shutdown()` on clean exit, or only once `initialize` has returned 1 (the file is then already open by CEF, so rename rather than truncate, and verify share modes).
  2. **Rust side**: a file logger (e.g. `tracing` + `tracing-appender`) to `Logs\sta.log`, initialized **only after** `execute_process` returns -1. Open it in append mode: a forwarding second launch also reaches this point (VERIFIED-FIX note, same reason as above). Renderer-side Rust should log via process messages to the browser, or to a per-pid file.
  3. **Panic hook** that writes to the log file, since the default hook prints to a stderr that doesn't exist. Remember that panics in `extern "C"` callbacks abort.
  4. **Optional `--console` flag**: `AttachConsole(ATTACH_PARENT_PROCESS)` (windows-sys `Win32_System_Console`: `fn AttachConsole(dwprocessid: u32) -> BOOL`, `ATTACH_PARENT_PROCESS: u32 = 0xFFFFFFFF`). Rust re-queries `GetStdHandle` per write, so `println!` starts working. Output interleaves with the shell prompt because the shell doesn't wait for GUI apps.

```rust
// win.rs (part)
pub fn init_process(d: &crate::paths::AppDirs) {
    let log = d.logs.join("panic.log");
    std::panic::set_hook(Box::new(move |info| {
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&log) {
            let _ = writeln!(f, "[{:?}] {info}\n{}", std::time::SystemTime::now(), std::backtrace::Backtrace::force_capture());
        }
    }));
    if std::env::args().any(|a| a == "--console") {
        unsafe { windows_sys::Win32::System::Console::AttachConsole(windows_sys::Win32::System::Console::ATTACH_PARENT_PROCESS) };
    }
}

/// e.g. "en-US", "de-DE". Used for Settings.locale (empty => CEF uses en-US).
/// VERIFIED-FIX: was GetUserDefaultLocaleName, which returns the *regional format* locale, not the UI language.
/// windows-sys 0.61.2 (Win32_Globalization):
///   fn GetUserPreferredUILanguages(dwflags: u32, pulnumlanguages: *mut u32,
///       pwszlanguagesbuffer: PWSTR, pcchlanguagesbuffer: *mut u32) -> BOOL
///   pub const MUI_LANGUAGE_NAME: u32 = 8u32;
/// Returns the preferred UI languages, most preferred first (also usable for Settings.accept_language_list).
pub fn os_ui_languages() -> Vec<String> {
    use windows_sys::Win32::Globalization::{GetUserPreferredUILanguages, MUI_LANGUAGE_NAME};
    let (mut num, mut len) = (0u32, 0u32);
    unsafe {
        if GetUserPreferredUILanguages(MUI_LANGUAGE_NAME, &mut num, std::ptr::null_mut(), &mut len) == 0 || len == 0 {
            return Vec::new();
        }
        let mut buf = vec![0u16; len as usize];
        if GetUserPreferredUILanguages(MUI_LANGUAGE_NAME, &mut num, buf.as_mut_ptr(), &mut len) == 0 {
            return Vec::new();
        }
        // Double-NUL-terminated multi-string: "en-US\0de-DE\0\0"
        buf.split(|&c| c == 0).filter(|s| !s.is_empty()).map(String::from_utf16_lossy).collect()
    }
}
pub fn os_ui_locale() -> String {
    os_ui_languages().into_iter().next().unwrap_or_default()
}
```

---

## 7. Frameless window polish on Windows (DWM)

### 7.1 CEF side

```rust
// trait ImplWindow (B L44242)
fn window_handle(&self) -> cef_window_handle_t;   // L44318;  cef_dll_sys: pub type cef_window_handle_t = HWND; pub struct HWND(pub *mut HWND__)
fn set_draggable_regions(&self, regions: Option<&[DraggableRegion]>); // L44316 (DraggableRegion { bounds: Rect, draggable: c_int } L933)
fn activate(&self);            // L44256
fn bring_to_top(&self);        // L44262
fn restore(&self);             // L44272
fn is_minimized(&self) -> ::std::os::raw::c_int; // L44278
fn send_key_press(&self, key_code: ::std::os::raw::c_int, event_flags: u32); // L44320 ("exposed primarily for testing purposes")
// trait ImplWindowDelegate (B L43176)
fn on_window_created(&self, window: Option<&mut Window>)                         // L43178
fn is_frameless(&self, window: Option<&mut Window>) -> ::std::os::raw::c_int     // L43221
fn can_resize(&self, window: Option<&mut Window>) -> ::std::os::raw::c_int       // L43241  default body: Default::default() == 0 !!
fn can_maximize(&self, window: Option<&mut Window>) -> ::std::os::raw::c_int     // L43245  default 0 !!
fn can_minimize(&self, window: Option<&mut Window>) -> ::std::os::raw::c_int     // L43249  default 0 !!
fn can_close(&self, window: Option<&mut Window>) -> ::std::os::raw::c_int        // L43253  default 0 !!
fn on_theme_colors_changed(&self, window: Option<&mut Window>, chrome_theme: ::std::os::raw::c_int) // L43273
```

- **`cef_window_delegate.h`**: `IsFrameless`: *"Return true if |window| should be created without a frame or title bar. The window will be resizable if CanResize() returns true. Use CefWindow::SetDraggableRegions() to specify draggable regions."* In C++, `CanResize`/`CanMaximize`/`CanMinimize`/`CanClose` all default to `{ return true; }` (VERIFIED-FIX: CanMinimize was left out; `cef_window_delegate.h` L198/204/210/217).
- **In Rust these default to 0.** `impl_cef_window_delegate_t::init_methods` (B L43307) **always** sets `object.can_resize = Some(can_resize::<I, R>)` and friends, so the trait default wins. Override all four. The existing sta and cefsimple delegates don't, so they are not resizable or maximizable. VERIFIED-FIX (precision): cefsimple *does* override `can_close` (via `try_close_browser()`), but not the other three. sta's current `HelloWindow` overrides none, so `can_close` returns 0 there too. The header says CanClose *"will be called for user-initiated window close actions and when CefWindow::Close() is called"*, so even a programmatic `close()` is refused. With several BrowserViews in one Window (sidebar, tabs, overlay), returning a bare `1` skips each browser's beforeunload/close handshake. Follow the cefsimple/`cef_life_span_handler.h` DoClose pattern for every hosted browser.
- **Theme callback**: `OnThemeColorsChanged` *"is not triggered on Window creation"*. Apply DWM attributes in `on_window_created` (the HWND exists; do it before `show()` to avoid a flash) and again in `on_theme_colors_changed`. VERIFIED-FIX (addition): the same header says it *"will be triggered if/when a BrowserView is added to the Window's component hierarchy"*. So it fires synchronously while `on_window_created` adds the sidebar BrowserView, which makes it re-entrant with `on_window_created` (see the Mutex note in §4).
- **HWND conversion**: `window.window_handle().0.cast::<core::ffi::c_void>()` gives a windows-sys `HWND`. cefsimple does exactly `browser?.host()?.window_handle().0` followed by `.cast()` (VERIFIED-FIX: the file is `examples/cefsimple/src/shared/simple_handler/win.rs`, not `src/win.rs`, which only holds `RunWinMain`).

### 7.2 windows-sys version and features

- The cef crate depends on `windows-sys = "0.61"` (non-optional on Windows, with features `Win32_System_Environment`, `Win32_System_LibraryLoader`, `Win32_UI_WindowsAndMessaging`). Both lockfiles resolve **0.61.2**. Use the same major so there is one copy. (VERIFIED-FIX precision: `Astatine/Cargo.lock` also contains `windows-sys 0.52.0`, pulled in by some other dependency, so there are already two copies. Adding `0.61` adds no third.)

```toml
[target.'cfg(windows)'.dependencies]
windows-sys = { version = "0.61", features = [
  "Win32_Foundation",            # HWND (= *mut c_void), COLORREF (= u32)
  "Win32_Graphics_Dwm",          # DwmSetWindowAttribute + DWMWA_* consts
  "Win32_UI_WindowsAndMessaging",
  "Win32_System_Console",        # AttachConsole
  "Win32_UI_Shell",              # SetCurrentProcessExplicitAppUserModelID
  "Win32_Globalization",         # GetUserPreferredUILanguages (VERIFIED-FIX: not GetUserDefaultLocaleName)
  "Win32_System_Environment",    # GetCommandLineW
] }
# add "Win32_UI_Controls" only if you need MARGINS / DwmExtendFrameIntoClientArea
```

Verbatim from `windows-sys-0.61.2/src/Windows/Win32/Graphics/Dwm/mod.rs`:
```rust
fn DwmSetWindowAttribute(hwnd : HWND, dwattribute : u32, pvattribute : *const core::ffi::c_void, cbattribute : u32) -> windows_sys::core::HRESULT; // L30
fn DwmGetWindowAttribute(hwnd : HWND, dwattribute : u32, pvattribute : *mut core::ffi::c_void, cbattribute : u32) -> HRESULT;            // L17
fn DwmExtendFrameIntoClientArea(hwnd : HWND, pmarinset : *const UI::Controls::MARGINS) -> HRESULT;                                          // L9 (needs Win32_UI_Controls)
pub type DWMWINDOWATTRIBUTE = i32;
pub const DWMWA_USE_IMMERSIVE_DARK_MODE: DWMWINDOWATTRIBUTE = 20i32;   // Win11 22000+ (and Win10 20H1+)
pub const DWMWA_WINDOW_CORNER_PREFERENCE: DWMWINDOWATTRIBUTE = 33i32;  // Win11 22000+
pub const DWMWA_BORDER_COLOR: DWMWINDOWATTRIBUTE = 34i32;              // Win11 22000+
pub const DWMWA_CAPTION_COLOR: DWMWINDOWATTRIBUTE = 35i32;
pub const DWMWA_TEXT_COLOR: DWMWINDOWATTRIBUTE = 36i32;
pub const DWMWA_SYSTEMBACKDROP_TYPE: DWMWINDOWATTRIBUTE = 38i32;       // Win11 22621+
pub const DWMWA_COLOR_DEFAULT: u32 = 4294967295u32;  pub const DWMWA_COLOR_NONE: u32 = 4294967294u32;
pub type DWM_WINDOW_CORNER_PREFERENCE = i32; DWMWCP_DEFAULT=0, DWMWCP_DONOTROUND=1, DWMWCP_ROUND=2, DWMWCP_ROUNDSMALL=3
pub type DWM_SYSTEMBACKDROP_TYPE = i32; DWMSBT_AUTO=0, DWMSBT_NONE=1, DWMSBT_MAINWINDOW=2 (Mica), DWMSBT_TRANSIENTWINDOW=3 (Acrylic), DWMSBT_TABBEDWINDOW=4
// windows_sys::core::BOOL = i32
```

```rust
// win.rs
use cef::*;
use core::ffi::c_void;
use windows_sys::Win32::{Foundation::HWND, Graphics::Dwm::*};

fn set_attr<T>(hwnd: HWND, attr: DWMWINDOWATTRIBUTE, v: &T) -> i32 {
    // SAFETY: v is a valid readable T for the duration of the call; hwnd is a live top-level window.
    unsafe { DwmSetWindowAttribute(hwnd, attr as u32, (v as *const T).cast::<c_void>(), size_of::<T>() as u32) }
}

/// border: COLORREF 0x00BBGGRR, or DWMWA_COLOR_NONE to hide the 1px Win11 border.
pub fn apply_dwm_chrome(window: &Window, dark: bool, border: Option<u32>) {
    let hwnd: HWND = window.window_handle().0.cast();
    if hwnd.is_null() {
        return;
    }
    // Failures (E_INVALIDARG on older builds) are harmless; ignore HRESULTs.
    set_attr(hwnd, DWMWA_WINDOW_CORNER_PREFERENCE, &DWMWCP_ROUND);
    set_attr(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, &(dark as windows_sys::core::BOOL));
    set_attr(hwnd, DWMWA_BORDER_COLOR, &border.unwrap_or(DWMWA_COLOR_DEFAULT));
}
```

```rust
wrap_window_delegate! {
    pub struct MainWindowDelegate {
        shared: crate::app::Shared,
    }

    impl ViewDelegate {
        fn preferred_size(&self, _view: Option<&mut View>) -> Size {
            Size { width: 1280, height: 800 }
        }
    }

    impl PanelDelegate {}

    impl WindowDelegate {
        fn on_window_created(&self, window: Option<&mut Window>) {
            let Some(window) = window else { return };
            #[cfg(windows)]
            crate::win::apply_dwm_chrome(window, crate::theme::is_dark(), None);
            // build root Panel (sidebar BrowserView, content Panel, overlay) ... set icons ...
            self.shared.0.lock().unwrap().main_window = Some(window.clone());
            window.show();
        }
        fn is_frameless(&self, _window: Option<&mut Window>) -> i32 { 1 }
        fn can_resize(&self, _window: Option<&mut Window>) -> i32 { 1 }   // Rust default is 0!
        fn can_maximize(&self, _window: Option<&mut Window>) -> i32 { 1 } // Rust default is 0!
        fn can_minimize(&self, _window: Option<&mut Window>) -> i32 { 1 } // Rust default is 0!
        fn can_close(&self, _window: Option<&mut Window>) -> i32 { 1 }    // or try_close_browser() logic like cefsimple
        fn on_theme_colors_changed(&self, window: Option<&mut Window>, _chrome_theme: i32) {
            #[cfg(windows)]
            if let Some(w) = window {
                crate::win::apply_dwm_chrome(w, crate::theme::is_dark(), None);
            }
        }
    }
}
```

What each attribute does for this window:
- **Rounded corners** (`DWMWCP_ROUND`, 8px). Microsoft: rounding is automatic for windows *"setting the WS_THICKFRAME and WS_CAPTION window styles"*. Otherwise use the opt-in API, which *"is a hint to the system and does not guarantee rounding"*. Windows using *"per-pixel alpha layering"* or *"window regions"* *"cannot ever be rounded"*, and nothing is rounded *"when maximized, snapped, running in a VM…"* ([MS Learn][round], [web]).
  - Chromium's frameless `HWNDMessageHandler` keeps `WS_CAPTION|WS_THICKFRAME` and removes the non-client area, so Win11 will likely round and shadow automatically. This is inferred from Chromium code, not verified.
  - Still set `DWMWCP_ROUND` as the hint, and check with a screenshot.
- **`DWMWA_USE_IMMERSIVE_DARK_MODE`** darkens the DWM border and shadow tint to match a dark UI. Worth setting.
- **`DWMWA_CAPTION_COLOR` / `DWMWA_TEXT_COLOR`** have **no visible effect** without a system caption.
- **`DWMWA_BORDER_COLOR`** is visible on Win11: the 1px border. Use it to match the sidebar color, or `DWMWA_COLOR_NONE`.
- **Mica/Acrylic (`DWMWA_SYSTEMBACKDROP_TYPE`) is effectively not feasible.** The backdrop only shows through transparent client pixels. The CEF browser views are opaque windowed composited surfaces: `cef_types.h` `background_color` says *"If the alpha component is fully transparent for a windowed browser then the default value of opaque white be used"*, and CEF Views exposes no translucent-window option. Setting `DWMSBT_MAINWINDOW` would at most tint the invisible frame or resize gutters. Emulate the look in CSS (solid or tinted sidebar) instead.
- **Snap Layouts flyout.** An HTML maximize button won't get it unless something answers `WM_NCHITTEST` with `HTMAXBUTTON`. That would mean subclassing Chromium's HWND (`SetWindowSubclass`), which is fragile. Defer it.

---

## 8. Remote debugging (CDP) for automated testing

- **`cef_types.h`**:
  - *"Set to a value between 1024 and 65535 to enable remote debugging on the specified port. Also configurable using the "remote-debugging-port" command-line switch."*
  - *"Specifying 0 via the command-line switch will result in the selection of an ephemeral port and the port number will be printed as part of the WebSocket endpoint URL to stderr. If a cache directory path is provided the port will also be written to the <cache-dir>/DevToolsActivePort file."*
- **Endpoints**: `GET http://127.0.0.1:<port>/json/list` (and `/json/version` for the browser endpoint `webSocketDebuggerUrl`). Each Views `BrowserView` is its own `type:"page"` target. That covers the sidebar `sta://…`, the command bar overlay and every tab, hidden or not. Our existing `tools/cdp.mjs` already filters on `page|other`, matches by URL substring and uses `Runtime.evaluate`, `Page.captureScreenshot` and `Input.dispatchKeyEvent`.
- **Origin check** (Chromium ≥111): a WebSocket upgrade **carrying an `Origin` header** is rejected unless allowed: *"Rejected an incoming WebSocket connection from the … origin. Use the command line flag --remote-allow-origins=… or --remote-allow-origins=\*"* ([CefSharp #4470][cefsharp-origins], [web]).
  - **Browsers** (a DevTools front-end page) always send `Origin`.
  - **Python `websocket-client`** sends one by default; pass `suppress_origin=True`.
  - **Node's `ws` / Puppeteer** send none by default, and Node 22 global `WebSocket` in practice doesn't either, so the flag isn't needed there.
  - If needed, add `--remote-allow-origins=http://127.0.0.1:<port>` in **debug/test builds only**.
- **HTML index page is blank.** With Chrome-style CEF (≥126/128), `http://localhost:<port>/` shows an empty page; use `/json/list` or `chrome://inspect` in Chrome ([forum][fw128], [cef#3740][cef3740], [web]).
- **Chrome 136 rule.** Chrome 136+ ignores `--remote-debugging-port`/`--remote-debugging-pipe` *"if attempting to debug the default Chrome data directory"* ([Chrome blog][chrome136], [web]). ~~Whether CEF enforces it is unverified… grep `cef.log` for that string.~~ VERIFIED-FIX: **it does not apply to CEF.** Chromium's `chrome/browser/devtools/remote_debugging_server.cc` enables the check only under `#if BUILDFLAG(GOOGLE_CHROME_BRANDING)`; otherwise it is a testing-only flag (`g_enable_default_user_data_dir_check_for_chromium_branding_for_testing`). CEF is not Google-Chrome-branded. It returns `NotStartedReason::kDisabledByDefaultUserDataDir`, with no log string in that file. The previously quoted message text is not in the blog post, so don't grep for it.
- **Singleton collision.** A test launch with the same `root_cache_path` as a running dev instance **forwards to it and exits 0**. Every automated run needs its own data dir.
- **Security.** Anyone local can drive the browser and read cookies through the port. Never enable it by default in release; gate it behind `cfg!(debug_assertions)`, an `automation` feature or an env var, and ~~consider `command_line_args_disabled=1` in release~~ strip debugging switches in `on_before_command_line_processing` in release (§2, §3; VERIFIED-FIX: `command_line_args_disabled=1` also drops argv URLs and relaunch forwarding).
- **Hidden views and screenshots.** `Page.captureScreenshot` on a **hidden** BrowserView (inactive tab) can hang, because hidden widgets produce no frames; Puppeteer calls `Page.bringToFront` for the same reason in Chrome. Capture only visible views, or make the tab active first through our own IPC.
- **Whole-window screenshots.** The native composite of several BrowserViews needs `PrintWindow(hwnd, hdc, PW_RENDERFULLCONTENT=2)`, which `tools/capture-window.ps1` already does, or Windows.Graphics.Capture.
- **Accelerators.** CDP `Input.*` events go to one renderer and do not exercise `Window::set_accelerator` handling. For those, use `ImplWindow::send_key_press` (B L44320, *"exposed primarily for testing purposes"*) through a debug IPC, or `SendInput`.

Launch recipe, parallel-safe:

```powershell
$dir = Join-Path $env:TEMP ("sta-test-" + [guid]::NewGuid())
$p = Start-Process target\debug\sta.exe -ArgumentList "--sta-data-dir=$dir","--remote-debugging-port=0" -PassThru
# wait for "$dir\User Data\DevToolsActivePort" (line 1 = port, line 2 = /devtools/browser/<id>)
$port = (Get-Content "$dir\User Data\DevToolsActivePort" -TotalCount 1)
$env:CDP_PORT = $port; node tools/cdp.mjs list
```

Chrome writes `DevToolsActivePort` to the user-data dir, which is `root_cache_path` here. The header says `<cache-dir>`, so check both if it isn't found. The fixed-port equivalent is `STA_REMOTE_DEBUGGING_PORT=9333` (Settings) with a temp data dir.

---

## 9. Distribution layout and data directory

**Build output.** `cef-dll-sys` `build.rs` computes `target_dir = OUT_DIR/../../..` (i.e. `target/<profile>`, or `target/<triple>/<profile>` with `--target`). On Windows, `copy_cef_runtime_files` copies **every file** in the CEF root plus `locales/`. Consequences:
- `cargo run` works.
- ~~`cargo test` binaries in `target/debug/deps` do **not** find `libcef.dll` unless `target/debug` is on `PATH`. `cargo run --example` has the same problem.~~ VERIFIED-FIX: the Cargo reference ("Dynamic library paths") says `cargo run`/`cargo test` put *"The base output directory, such as `target/debug`, and the "deps" directory"* on `PATH`, so `libcef.dll` **is** found there. Only launching `target/debug/deps/*.exe` or `target/debug/examples/*.exe` directly (IDE/debugger) needs `target/debug` on `PATH`. Separate caveat (inference, not run): with `browser_subprocess_path` empty, a libtest harness exe would be re-launched as the renderer/GPU subprocess, and libtest's argument parsing doesn't handle `--type=…`. CEF integration tests therefore need `harness = false` or a separate `browser_subprocess_path` helper.

The CEF 152 dist here is the `minimal` archive (`cef_binary_152.0.6+g708dc14+chromium-152.0.7977.83_windows64_minimal`).

Ship this set, **flat next to `sta.exe`**. `libcef.dll` is import-linked, and `icudtl.dat` plus the v8 snapshot are loaded from the module directory. Per CEF `README.redistrib.txt` ([web][redist]):

| File(s) | Size | Status |
|---|---|---|
| `libcef.dll` | 285 MB | **Required** |
| `chrome_elf.dll` | 2.8 MB | **Required** (crash reporting, loaded by libcef) |
| `icudtl.dat` | 10.9 MB | **Required** (Unicode) |
| `v8_context_snapshot.bin` | 0.7 MB | **Required** (V8 startup) |
| `resources.pak`, `chrome_100_percent.pak`, `chrome_200_percent.pak` | 21.7/0.7/1.3 MB | Needed in practice (non-localized resources: devtools, error pages, UI). Relocatable via `resources_dir_path`. |
| `locales\*.pak` | 52 MB total, 220 files = 55 locales × {base, `_FEMININE`, `_MASCULINE`, `_NEUTER`} | *"Without these files arbitrary Web components may display incorrectly."* Always keep `en-US*.pak` (en-US.pak = 606 KB). Trimming to shipped UI languages is fine. |
| `d3dcompiler_47.dll` | 4.7 MB | GPU-accelerated canvas/CSS/WebGL. Ship it. |
| `dxcompiler.dll`, `dxil.dll` | 25.8/1.5 MB | WebGPU (x64). Ship it. |
| `libEGL.dll`, `libGLESv2.dll` | 0.46 MB each | ANGLE: the GL renderer on Windows. Ship it. |
| `vk_swiftshader.dll`, `vk_swiftshader_icd.json`, `vulkan-1.dll` | 5.4 MB / 106 B / 1.0 MB | SwiftShader software fallback (blocklisted GPUs, VMs). Ship it. |
| `CREDITS.html` | 20 MB | Not loaded at runtime, but ship it or expose it for Chromium license notices. |
| `libcef.lib`, `CMakeLists.txt`, `archive.json`, `bootstrap.exe`, `bootstrapc.exe` | — | **Do not ship.** Build-time only; bootstrap is only for sandbox mode. |

Runtime total is about 362 MB before locales, plus 52 MB of locales. `sta.exe` embeds the manifest, icon and VERSIONINFO (§5), so there is no external `.exe.manifest` or `.ico` to ship.

**Install location.** Use per-user `%LOCALAPPDATA%\Programs\sta\` (no elevation, auto-update friendly), or `Program Files`. The exe directory must not need to be writable, so set `log_file`.

**Data directory:**

```
%LOCALAPPDATA%\sta\            (debug builds: %LOCALAPPDATA%\sta Dev\ ; override: --sta-data-dir=<abs>)
  User Data\        <- Settings.root_cache_path == Settings.cache_path  (Chromium user-data-dir; profile lands in "Default")
                       contains SingletonLock-equivalents, DevToolsActivePort (when debugging), Local State, Default\...
  Logs\cef.log      <- Settings.log_file
  Logs\sta.log, Logs\panic.log  <- Rust-side logs
  (sta state: spaces/pinned tabs DB — keep outside "User Data" so Chromium never touches it)
```

Everything under the base dir is protected by the same process singleton, since one browser process owns `User Data`.

```rust
// paths.rs
use std::{io, path::PathBuf};

pub struct AppDirs {
    pub base: PathBuf,
    pub user_data: PathBuf,
    pub logs: PathBuf,
}

impl AppDirs {
    pub fn resolve() -> io::Result<Self> {
        let override_dir = std::env::args_os().find_map(|a| {
            a.to_str()?.strip_prefix("--sta-data-dir=").map(PathBuf::from)
        });
        let base = match override_dir {
            Some(p) => std::path::absolute(p)?, // root_cache_path must be absolute
            None => PathBuf::from(std::env::var_os("LOCALAPPDATA").ok_or_else(|| io::Error::other("LOCALAPPDATA unset"))?)
                .join(if cfg!(debug_assertions) { "sta Dev" } else { "sta" }),
        };
        let (user_data, logs) = (base.join("User Data"), base.join("Logs"));
        std::fs::create_dir_all(&user_data)?;
        std::fs::create_dir_all(&logs)?;
        Ok(Self { base, user_data, logs })
    }
}
```

---

## Sources
- Local:
  - the cef crate files listed at the top (bindings line numbers as cited)
  - `cef-dll-sys-152.3.0+152.0.6/build.rs`
  - cef-rs `examples/cefsimple/{Cargo.toml, build.rs, src/main.rs, src/win.rs, src/shared/*.rs}`, `examples/osr/src/{main.rs, webrender.rs}`, `examples/tests_shared/src/browser/client_app_browser.rs`
  - `cef/src/build_util/win/{mod.rs, cef-app.exe.manifest}`, cef-rs `README.md`
  - headers `cef_app.h`, `cef_sandbox_win.h`, `internal/cef_app_win.h`, `cef_api_hash.h`, `cef_browser_process_handler.h`, `cef_command_line.h`, `internal/cef_types.h`, `views/cef_window.h`, `views/cef_window_delegate.h`
  - `libcef_dll/wrapper/libcef_dll_wrapper.cc`
  - embedded manifest of `bootstrap.exe`/`bootstrapc.exe`
  - `windows-sys-0.61.2` (Dwm, Console, Shell, Globalization, Environment modules), `embed-resource-3.0.6/src/lib.rs`
  - `Astatine/target/debug/debug.log`, `Astatine/tools/{cdp.mjs, capture-window.ps1}`
- [web]
  - [sbx]: https://chromiumembedded.github.io/cef/sandbox_setup
  - [dpi]: https://github.com/chromiumembedded/cef/issues/3452
  - [clientbrowser]: https://raw.githubusercontent.com/chromiumembedded/cef/master/tests/cefclient/browser/client_browser.cc
  - [round]: https://learn.microsoft.com/en-us/windows/apps/desktop/modernize/ui/apply-rounded-corners
  - [redist]: https://raw.githubusercontent.com/chromiumembedded/cef/master/tools/distrib/win/README.redistrib.txt
  - [cefsharp-origins]: https://github.com/cefsharp/CefSharp/discussions/4470
  - [fw128]: https://www.magpcss.org/ceforum/viewtopic.php?f=6&t=19950
  - [cef3740]: https://github.com/chromiumembedded/cef/issues/3740
  - [chrome136]: https://developer.chrome.com/blog/remote-debugging-port
  - Singleton forum threads: https://www.magpcss.org/ceforum/viewtopic.php?f=6&t=19677 and https://github.com/cefsharp/CefSharp/issues/4668

[sbx]: https://chromiumembedded.github.io/cef/sandbox_setup
[dpi]: https://github.com/chromiumembedded/cef/issues/3452
[clientbrowser]: https://raw.githubusercontent.com/chromiumembedded/cef/master/tests/cefclient/browser/client_browser.cc
[round]: https://learn.microsoft.com/en-us/windows/apps/desktop/modernize/ui/apply-rounded-corners
[redist]: https://raw.githubusercontent.com/chromiumembedded/cef/master/tools/distrib/win/README.redistrib.txt
[cefsharp-origins]: https://github.com/cefsharp/CefSharp/discussions/4470
[fw128]: https://www.magpcss.org/ceforum/viewtopic.php?f=6&t=19950
[cef3740]: https://github.com/chromiumembedded/cef/issues/3740
[chrome136]: https://developer.chrome.com/blog/remote-debugging-port
[cefmain]: https://raw.githubusercontent.com/chromiumembedded/cef/master/libcef/common/chrome/chrome_main_delegate_cef.cc
[finder]: https://raw.githubusercontent.com/chromium/chromium/main/chrome/browser/win/chrome_process_finder.cc
[winimpl]: https://raw.githubusercontent.com/chromiumembedded/cef/master/libcef/browser/views/window_impl.cc

---

## Verification log

An adversarial pass on 2026-09-16 checked everything against the local sources: cef 152.3.0 bindings (`B`), `args.rs`/`string.rs`/`rc.rs`/`lib.rs`/`Cargo.toml`, the cef-dll-sys build.rs, windows-sys 0.61.2, embed-resource 3.0.6, the CEF 152 headers, `libcef_dll_wrapper.cc`, the cef-rs examples and `Astatine/` (read only), plus the web sources below. Nothing was built or run.

### Confirmed verbatim (name, params, return type, trait, line)
- **Global functions**: `api_hash` L56201, `execute_process` L58226, `initialize` L58252, `get_exit_code` L58289, `shutdown` L58297, `run_message_loop` L58311, `quit_message_loop` L58318, `command_line_create` L57770, `command_line_get_global` L57782, `currently_on` L57820, `image_create` L57259, `window_create_top_level` L59477.
- **Types and constants**: `MainArgs { instance: HINSTANCE }` (struct at L375). `Resultcode` L46853, `NORMAL_EXIT_PROCESS_NOTIFIED` L46900, `get_raw -> i32` L46962. `ThreadId::UI`. `SchemeOptions::{STANDARD, SECURE, CORS_ENABLED, FETCH_ENABLED}` with `get_raw` L50038. `LogSeverity` L45718 (DEFAULT…DISABLE). `LogItems::DEFAULT`, which has no BitOr impl. `DraggableRegion` L933. `Size {width, height}`. `CefString` = `CefStringUtf16` (L42). `sys::CEF_API_VERSION_LAST = 15200`. `pub use cef_dll_sys as sys`. `cef_window_handle_t = HWND(pub *mut HWND__)`.
- **`Settings`**: all 31 fields, types and line numbers 513–544; `Default` at L625 with `size` plus zeroed.
- **Traits**:
  - `ImplApp` L33644 (methods L33646/33653/33659/33663).
  - `ImplCommandLine` L28530 (`init_from_string` L28544, `has_switch` L28558, `switch_value` L28560, `append_switch` L28564, `append_switch_with_value` L28566, `arguments` L28570, `remove_switch` L28576).
  - `ImplSchemeRegistrar::add_custom_scheme` L33367.
  - `ImplBrowserProcessHandler` L29172 (all 7 methods and lines match).
  - `ImplWindow` L44242 (`activate` 44256, `bring_to_top` 44262, `restore` 44272, `is_minimized` 44278, `set_window_icon` 44288, `set_window_app_icon` 44292, `set_draggable_regions` 44316, `window_handle` 44318, `send_key_press` 44320).
  - `ImplWindowDelegate` L43176 (`on_window_created` 43178, `is_frameless` 43221, `can_resize/maximize/minimize/close` 43241/45/49/53 with `Default::default()` bodies, `on_theme_colors_changed` 43273).
  - `impl_cef_window_delegate_t::init_methods` L43307 always installs `can_*`.
  - `ImplImage::add_png` L4081.
- **Macros**: `wrap_app!` L33673, `wrap_browser_process_handler!` L29211, `wrap_browser_view_delegate!` L37739, `wrap_window_delegate!` L43304. Confirmed against the macro source:
  - the unit-form rule for the Views delegate macros re-invokes with only the leaf impl, so it can't match;
  - `new(fields...)` takes fields in declaration order;
  - `Clone` clones each field;
  - `App::new`/`WrapApp`/`ImplApp` are unqualified;
  - params are `ident` (so `_` is rejected) and fields take no attributes.
- **`args.rs`**: L17 `new` (GetModuleHandleW), L53 `as_main_args`, L65-72 `as_cmd_line` (rebuilds from `std::env::args().join(" ")`, so quoting is lost).
- **`rc.rs`**: L283/284 `unsafe impl Send/Sync for RefGuard`; `RcImpl` refcount is `AtomicUsize`.
- **`string.rs`**: L489 `From<&str>` gives `Clear`. `CefStringData::clone` gives `Borrowed` (a shallow copy). L551 only takes `Borrowed`. L504 `From<&CefStringUserfreeUtf16>`, L624 `Display`, L937 `IntoIterator for CefStringList { Item = String }`.
- **Features and build script**: `default = ["sandbox","build-util","resources"]`, `sandbox = ["cef-dll-sys/sandbox"]`. The build script defines `USE_SANDBOX`, compiles the wrapper with `CEF_API_VERSION=<last>`, copies every root file plus `locales` to `OUT_DIR/../../..`, and depends on windows-sys 0.61 with the three features listed.
- **Wrapper**: `libcef_dll_wrapper.cc` L65/L84 `cef_api_hash(CEF_API_VERSION, 0)` plus CHECK, so the C++ wrapper does the hash call itself and Rust must do it manually. cefsimple `load_cef()` confirms this.
- **Header quotes**: `cef_app.h` (execute_process, initialize, shutdown, run_message_loop, OnBeforeCommandLineProcessing, OnRegisterCustomSchemes, GetBrowserProcessHandler "called on multiple threads"), `cef_browser_process_handler.h` (all quotes), `cef_types.h` (`Settings` field quotes; result codes 21 at L1106 and 24 at L1113; `LOGSEVERITY_DEFAULT` "currently INFO"), `cef_window_delegate.h` (IsFrameless, force-dark-mode, "not triggered on Window creation"), `cef_window.h` (ICON_BIG, "primarily for testing purposes"), `cef_sandbox_win.h` (RunWinMain), `cef_api_hash.h`, `cef_command_line.h` ("lowercase ASCII", global line read-only).
- **Examples**: cefsimple's `main()` has the sandbox split. `SimpleApp::browser_process_handler` builds a new handler on every call (gotcha 2 is real). `execute_process(..., None, ...)`. The winres icons are IDI_CEFSIMPLE=120 and IDI_SMALL=121. The osr example's `append_switch("enable-logging=stderr")`. sta's `main.rs` already passes the App to `execute_process` and has no `can_*` overrides.
- **windows-sys 0.61.2**: `DwmSetWindowAttribute` L30, `DwmGetWindowAttribute` L17, `DwmExtendFrameIntoClientArea` L9. All DWMWA/DWMWCP/DWMSBT constants and values match. `HWND = *mut c_void`, `COLORREF = u32`, `core::BOOL = i32`. Also `AttachConsole`, `ATTACH_PARENT_PROCESS`, `SetCurrentProcessExplicitAppUserModelID` and `GetCommandLineW -> PCWSTR`.
- **embed-resource 3.0.6**: `compile(resource_file, parameters) -> CompilationResult`, `NONE`, `manifest_required()`. L444 prints `cargo:rustc-link-arg-bins`. `rc.exe /fo <out> /I <out_dir>`.
- **Distribution files**: the CEF dist root file list, the sizes (about 362 MB runtime), `locales` (220 files = 55x4, 51 MB, en-US.pak 605,830 B) and `archive.json` type `minimal` all match.
- **Manifests**: `bootstrap.exe` embeds the manifest described (no dpiAware). `target/debug/sta.exe` has 0 `asm.v1` hits. `debug.log` contains the root_cache_path warning.
- **Web**:
  - The sandbox_setup quote.
  - cef#3452: "CEF 108+ sets the DPI awareness...".
  - CEF `chrome_main_delegate_cef.cc`: locale defaults to en-US; the `remote_debugging_port` range 1024-65535 is appended before OnBeforeCommandLineProcessing; `log_file` is applied.
  - Chromium `AttemptToNotifyRunningChrome` calls `AllowSetForegroundWindow(process_id)` before `SendMessageTimeout`.
  - MS Learn rounded-corner quotes.
  - README.redistrib file roles.
  - cef#3740 (blank remote-debugging page with Chrome bootstrap).

### Errors found and fixed (marked VERIFIED-FIX inline)
1. **`command_line_args_disabled = 1` in release was harmful** (§2 table, `settings()` snippet, §8). CEF resets the browser command line with `InitFromArgv({program})`, which drops URL/file arguments, and Chromium forwards `ForCurrentProcess()` on relaunch. With the flag set, `command_line_get_global().arguments()` and `on_already_running_app_relaunch` URLs are both empty. The fix sets it to 0 and filters switches with `remove_switch` in `on_before_command_line_processing`.
2. **Locale API was wrong.** `GetUserDefaultLocaleName` gives the regional-format locale, not the UI language. Replaced with a `GetUserPreferredUILanguages(MUI_LANGUAGE_NAME, ...)` snippet (signature verified).
3. **Log trimming "after execute_process returns -1" would wipe the running instance's log.** A forwarding second launch also gets -1. Now rotate after `shutdown()`, or after `initialize` returns 1 by renaming.
4. **Chrome 136 default-user-data-dir rule doesn't apply to CEF.** It is `GOOGLE_CHROME_BRANDING`-only, and the quoted message wasn't in the blog.
5. **The §9 claim that `cargo test` / `cargo run --example` can't find libcef.dll was wrong.** Cargo adds `target/debug` to PATH. Added the libtest subprocess caveat.
6. **The manifest dropped the Vista supportedOS GUID** while claiming to match bootstrap.exe. Added it.
7. **Wrong file reference**: the cefsimple HWND conversion is in `src/shared/simple_handler/win.rs`.
8. **Precision fixes**: CanMinimize also defaults to true in C++. cefsimple does override `can_close`. sta's lockfile also has windows-sys 0.52.0.
9. **Additions**:
   - The std `Mutex` in `Shared` is non-reentrant, but `window_create_top_level` calls `OnWindowCreated` synchronously (verified in CEF `window_impl.cc`) and `OnThemeColorsChanged` fires when a BrowserView is added. Never hold the lock across CEF calls.
   - The relaunch command line carries switches from the second process (`--source-shortcut`/`--source-app-id`, Settings-derived ones).

### Snippet compile-plausibility notes (no issues requiring change)
- **Coercions**: `Option<&mut CommandLine>` values are passed where `&CommandLine` is expected; `&mut T` to `&T` coercion at call sites is fine.
- **String literals**: `Some(&"x".into())` infers `CefString`.
- **Clone on `&mut`**: `window.clone()` on `&mut Window` resolves to `Window::clone`.
- **Attributes**: `#[cfg(windows)]` on expression statements inside macro bodies is allowed.
- **`size_of`**: prelude since Rust 1.80, and edition 2024 requires 1.85 or newer.
- **Globs**: the glob imports `cef::*` + `Dwm::*` don't collide on the names used.
- **Nit**: `cfg!(feature = "automation")` needs the feature declared in Cargo.toml, or rustc warns `unexpected_cfgs`.
- **Nit**: `[target.'cfg(windows)'.build-dependencies]` plus `#[cfg(windows)]` in build.rs are host-based checks; that's fine for native Windows builds only.

### Still uncertain (not verifiable without running or deeper source reading)
- **Resizing**: whether `can_resize=0` really makes the frameless CEF window non-resizable on Windows. The Rust default of 0 is certain; the Chromium effect is inferred. Likewise, that sta's current window can't be closed with X is expected but was not run.
- **Rounded corners**: whether Chromium's frameless HWND keeps `WS_CAPTION|WS_THICKFRAME`, so Win11 rounds automatically. Also whether Chromium later overwrites `DWMWA_USE_IMMERSIVE_DARK_MODE` on theme changes.
- **Locale in Chrome style**: whether the `--lang` that CEF sets from `Settings.locale` fully controls the UI language on Windows, as opposed to the `intl.app_locale` pref.
- **`DevToolsActivePort` location**: `root_cache_path` vs `cache_path`. They are equal in this design, so it doesn't matter here.
- **Logging and CDP details**: whether subprocesses append to the same `log_file`; whether hidden BrowserViews hang `Page.captureScreenshot`; whether CDP `Input.dispatchKeyEvent` bypasses Views accelerators; whether Node's global WebSocket omits `Origin`.
- **`--single-argument`**: parsing behavior was not re-verified (Chromium `base::CommandLine`).
- **`OnBeforeCommandLineProcessing` in subprocesses**: whether Chrome style calls it there at all. The fetched `BasicStartupComplete` only calls it when `process_type.empty()`; the call may happen elsewhere and wasn't traced. The report's snippet returns early for non-browser processes, so it is unaffected.
- **External `.exe.manifest` precedence and activation-context caching**: standard Windows behavior, not re-verified.
