//! CEF application objects [owner: chrome] (ARCHITECTURE §3, docs/research/platform.md §2–§4).
//!
//! Responsibility:
//! - `StaApp` (same object in every process): custom scheme registration, command-line
//!   tweaks (browser process), one stored `BrowserProcessHandler` and `RenderProcessHandler`, and a
//!   `ResourceBundleHandler` that renames Chromium's product in the windows sta keeps native
//!   ([`branding_string`]);
//! - `BrowserProcessHandler::on_context_initialized`, in ARCHITECTURE §3 startup order:
//!   1. load the store (quarantine corrupt files) and apply `SystemThemeChanged{dark}`; reset the
//!      one-time permission grants of the previous session (`permissions::startup`);
//!   2. register the scheme factory and the IPC handler;
//!   3. create the window (its `on_window_created` runs `Store::startup` and starts `Tick`);
//! - `default_client`: the client of browsers Chromium creates on its own (`foreign::client`);
//! - `on_already_running_app_relaunch`: argv → URLs (internal page URLs from before the rename →
//!   `sta://`, existing file paths → `file:///`, other text resolved like typed input) →
//!   `OpenUrl{NewTab}`; restore + activate the window (posted); return 1;
//! - `Settings` for `cef::initialize` and the final teardown before `cef::shutdown`.
//!
//! Public API:
//! - `pub fn create_app() -> App`
//! - `pub fn settings(dirs: &AppDirs) -> Settings`
//! - `pub fn startup_urls(command_line: &CommandLine, cwd: Option<&Path>) -> Vec<String>`
//! - `pub fn teardown()` — after `run_message_loop` returns, before `cef::shutdown`

use crate::paths::AppDirs;
use crate::{browsers, client, controller, downloads, ipc, overlays, permissions, platform, renderer, scheme, suggest, tabs, task, window};
use sta_core::{Command, OpenTarget};
use cef::*;
use std::path::Path;

/// `STA_REMOTE_DEBUGGING_PORT` (debug builds only).
pub const REMOTE_DEBUGGING_PORT_ENV: &str = "STA_REMOTE_DEBUGGING_PORT";

pub fn create_app() -> App {
    StaApp::new(StaBrowserProcessHandler::new(), renderer::create_handler())
}

wrap_app! {
    struct StaApp {
        browser_process_handler: BrowserProcessHandler,
        render_process_handler: RenderProcessHandler,
    }

    impl App {
        fn on_before_command_line_processing(&self, process_type: Option<&CefString>, command_line: Option<&mut CommandLine>) {
            let Some(cl) = command_line else { return };
            let is_browser = process_type.is_none_or(|p| p.to_string().is_empty());
            if !is_browser {
                return;
            }
            // (The permission auto-blocker has no feature switch in Chromium 152:
            // `BlockPromptsIfDismissedOften`/`…IgnoredOften` are gone. permissions.rs clears its data.)
            merge_list_switch(cl, "disable-features", &["Translate", "MediaRouter"]);
            // macOS debug builds: Chromium encrypts cookies with a key it keeps in the login
            // keychain, and an unsigned binary that is rebuilt on every `cargo build` can never
            // hold on to that key's ACL — each run stops on a keychain password prompt, and the
            // shutdown that reads the key again hangs behind it. Use Chromium's own mock key
            // instead, as its tests do; a signed release build asks once, like any browser.
            #[cfg(all(target_os = "macos", debug_assertions))]
            cl.append_switch(Some(&CefString::from("use-mock-keychain")));
            // While AI agents may connect, a covered window keeps rendering (screenshots, clicks).
            if crate::automation::occlusion_flag_needed() {
                cl.append_switch(Some(&CefString::from("disable-backgrounding-occluded-windows")));
            }
            if cfg!(not(debug_assertions)) {
                for switch in ["remote-debugging-port", "remote-debugging-pipe", "disable-web-security", "load-extension"] {
                    cl.remove_switch(Some(&CefString::from(switch)));
                }
            }
        }

        fn on_register_custom_schemes(&self, registrar: Option<&mut SchemeRegistrar>) {
            if let Some(registrar) = registrar {
                scheme::register_custom_scheme(registrar);
            }
        }

        fn browser_process_handler(&self) -> Option<BrowserProcessHandler> {
            Some(self.browser_process_handler.clone())
        }

        fn render_process_handler(&self) -> Option<RenderProcessHandler> {
            Some(self.render_process_handler.clone())
        }

        fn resource_bundle_handler(&self) -> Option<ResourceBundleHandler> {
            Some(StaResourceBundle::new())
        }
    }
}

// ------------------------------------------------------------------------------- Chromium's strings

/// The product name Chromium puts in the windows sta keeps native — extension popups and sign-in
/// flows (user decision D2a) — and in its own dialogs. Those windows are Chromium's, with Chromium's
/// title bar: without this they read "… - Chromium", which is exactly the "a Chrome window opened"
/// the user reported (request 3). sta's own windows set their title themselves and never ask for
/// these strings.
///
/// The ids are resolved by **name** through CEF's version-safe mapper, so a CEF update can only make
/// the override inert (id `-1`), never point it at another string.
fn branding_string(string_id: i32) -> Option<&'static str> {
    static TABLE: std::sync::OnceLock<[(i32, &'static str); 3]> = std::sync::OnceLock::new();
    let table = TABLE.get_or_init(|| {
        let id = |name: &std::ffi::CStr| {
            // SAFETY: a libcef helper taking a NUL-terminated name; -1 when this build has no such
            // string id.
            unsafe { cef::sys::cef_id_for_pack_string_name(name.as_ptr()) }
        };
        [
            // "<page title> - Chromium" (Chromium bakes the product name into the format string).
            (id(c"IDS_BROWSER_WINDOW_TITLE_FORMAT"), "$1 - sta"),
            (id(c"IDS_PRODUCT_NAME"), "sta"),
            (id(c"IDS_SHORT_PRODUCT_NAME"), "sta"),
        ]
    });
    table.iter().find(|(id, _)| *id > 0 && *id == string_id).map(|(_, text)| *text)
}

wrap_resource_bundle_handler! {
    struct StaResourceBundle;

    impl ResourceBundleHandler {
        fn localized_string(&self, string_id: i32, string: Option<&mut CefString>) -> i32 {
            let Some((text, out)) = branding_string(string_id).zip(string) else { return 0 };
            out.try_set(text) as i32
        }
    }
}

/// Appends to a comma-separated switch instead of replacing it (Chromium keeps the last value).
fn merge_list_switch(cl: &CommandLine, name: &str, add: &[&str]) {
    let key = CefString::from(name);
    let current = CefString::from(&cl.switch_value(Some(&key))).to_string();
    let mut items: Vec<String> = current.split(',').filter(|s| !s.is_empty()).map(Into::into).collect();
    for a in add {
        if !items.iter().any(|i| i == a) {
            items.push((*a).into());
        }
    }
    cl.append_switch_with_value(Some(&key), Some(&CefString::from(items.join(",").as_str())));
}

wrap_browser_process_handler! {
    struct StaBrowserProcessHandler;

    impl BrowserProcessHandler {
        fn on_context_initialized(&self) {
            on_context_initialized();
        }

        fn default_client(&self) -> Option<Client> {
            // Tabs of browsers Chromium creates on its own (extensions, the Web Store's
            // post-install window): hidden, adopted as sta tabs or kept native (foreign.rs).
            Some(crate::foreign::client())
        }

        fn on_already_running_app_relaunch(&self, command_line: Option<&mut CommandLine>, current_directory: Option<&CefString>) -> i32 {
            let cwd = current_directory.map(|c| std::path::PathBuf::from(c.to_string())).filter(|c| !c.as_os_str().is_empty());
            let urls = command_line.map(|cl| startup_urls(cl, cwd.as_deref())).unwrap_or_default();
            log_info!("relaunch forwarded {} url(s)", urls.len());
            for url in urls {
                controller::dispatch(Command::OpenUrl { url, target: OpenTarget::NewTab, opener: None });
            }
            // Activation fires window delegate callbacks synchronously: not inside this callback.
            task::post_ui(window::activate);
            1 // handled: never let CEF open a default Chrome-style window
        }
    }
}

fn on_context_initialized() {
    // ⌘Q and the Dock's Quit take the window's own close path (platform/mac.rs).
    #[cfg(target_os = "macos")]
    platform::set_quit_handler(window::request_close);
    let dark = platform::system_dark_mode();
    let animations = platform::system_animations();
    controller::init(dark, animations);
    // One-time permission grants of the previous session end before any browser exists.
    permissions::startup();
    crate::foreign::startup();
    // The crash-loop guard reads (and rewrites) the launch marker before anything else can crash.
    crate::extensions::startup();
    tabs::init_devtools_policy();
    scheme::register_factory();
    ipc::install();
    window::init_system_dark(dark);
    window::init_system_animations(animations);
    // An update that has been applied leaves its staging directory behind; and this run's own
    // check is posted, to run once the browser is actually up (update.rs).
    crate::update::clean_staging();
    crate::update::check_after_startup();
    let urls = command_line_get_global().map(|cl| startup_urls(&cl, None)).unwrap_or_default();
    window::create_main_window(urls);
}

/// CEF settings for the browser process.
pub fn settings(dirs: &AppDirs) -> Settings {
    let s = |p: &Path| CefString::from(p.to_string_lossy().as_ref());
    let dark = platform::system_dark_mode();
    Settings {
        no_sandbox: 1,
        // macOS: libcef lives in a framework inside the bundle, and every child process is started
        // from a helper bundle next to it (platform/mac_bundle.rs). Empty (and ignored) elsewhere.
        #[cfg(target_os = "macos")]
        framework_dir_path: platform::mac_bundle::framework_dir().map(|p| s(&p)).unwrap_or_default(),
        #[cfg(target_os = "macos")]
        main_bundle_path: platform::mac_bundle::main_bundle().map(|p| s(&p)).unwrap_or_default(),
        #[cfg(target_os = "macos")]
        browser_subprocess_path: platform::mac_bundle::helper_exe().map(|p| s(&p)).unwrap_or_default(),
        root_cache_path: s(&dirs.user_data),
        cache_path: s(&dirs.user_data),
        persist_session_cookies: 1,
        log_file: s(&dirs.logs.join("cef.log")),
        log_severity: if cfg!(debug_assertions) { LogSeverity::INFO } else { LogSeverity::WARNING },
        locale: CefString::from(platform::os_ui_locale().as_str()),
        background_color: sta_core::theme::frame_argb(&sta_core::Theme::default(), dark),
        chrome_app_icon_id: 1,
        command_line_args_disabled: 0,
        remote_debugging_port: remote_debugging_port().unwrap_or(0),
        ..Default::default()
    }
}

fn remote_debugging_port() -> Option<i32> {
    if !cfg!(debug_assertions) {
        return None;
    }
    std::env::var(REMOTE_DEBUGGING_PORT_ENV).ok()?.parse::<i32>().ok().filter(|p| (1024..=65535).contains(p))
}

/// Non-switch command-line arguments as absolute URLs (internal page URLs from before the rename →
/// `sta://`, existing file paths → `file:///`).
pub fn startup_urls(command_line: &CommandLine, cwd: Option<&Path>) -> Vec<String> {
    let mut list = CefStringList::new();
    command_line.arguments(Some(&mut list));
    list.into_iter().filter(|a| !a.trim().is_empty()).filter_map(|a| argument_to_url(a.trim(), cwd)).collect()
}

fn argument_to_url(arg: &str, cwd: Option<&Path>) -> Option<String> {
    // An internal page URL from before the rename (an old shortcut or script) opens its `sta://`
    // page instead of going to the OS as an unknown protocol (`OpenUrl` hands those out).
    if let Some(url) = sta_core::legacy::upgrade_url(arg) {
        return Some(url);
    }
    let lower = arg.to_ascii_lowercase();
    if arg.contains("://") || ["about:", "data:", "mailto:", "view-source:"].iter().any(|p| lower.starts_with(p)) {
        return Some(arg.to_string());
    }
    let path = Path::new(arg);
    let absolute = match cwd {
        Some(dir) if path.is_relative() => dir.join(path),
        _ => std::path::absolute(path).ok()?,
    };
    if absolute.exists() {
        let text = absolute.to_string_lossy().replace('\\', "/");
        let mut encoded = String::with_capacity(text.len() + 8);
        for c in text.chars() {
            match c {
                ' ' => encoded.push_str("%20"),
                '#' => encoded.push_str("%23"),
                '%' => encoded.push_str("%25"),
                '?' => encoded.push_str("%3F"),
                _ => encoded.push(c),
            }
        }
        return Some(format!("file:///{}", encoded.trim_start_matches('/')));
    }
    // Not a path: resolve it like typed text (host names become https URLs, words a search).
    let (engine, custom) = controller::with_store(|s| (s.settings().search_engine, s.settings().custom_search_url.clone()))
        .unwrap_or((sta_core::SearchEngineId::Google, String::new()));
    Some(sta_core::omnibox::resolve_input(arg, engine, &custom))
}

/// Drops every CEF handle the shell holds (thread_locals and statics) after the message loop
/// ended. `cef::shutdown()` must not run with live references.
pub fn teardown() {
    if !window::is_closing() && controller::with_store(|s| !s.is_shutting_down()).unwrap_or(false) {
        log_warn!("message loop ended without a shutdown sequence; saving");
        controller::save_now();
    }
    controller::shutdown();
    overlays::clear();
    tabs::clear();
    downloads::clear();
    permissions::clear();
    suggest::clear();
    crate::update::clear();
    ipc::clear();
    crate::automation::clear();
    crate::devtools_cdp::clear();
    crate::foreign::clear();
    crate::ext_popup::clear();
    crate::ext_backend::clear();
    crate::extensions::clear();
    browsers::clear();
    client::clear();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arguments_become_urls() {
        assert_eq!(argument_to_url("https://example.com/a", None).as_deref(), Some("https://example.com/a"));
        assert_eq!(argument_to_url("about:blank", None).as_deref(), Some("about:blank"));
        let dir = std::env::temp_dir().join("sta arg test#1");
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("page.html");
        std::fs::write(&file, "<p>x</p>").unwrap();
        let url = argument_to_url("page.html", Some(&dir)).unwrap();
        assert!(url.starts_with("file:///"), "{url}");
        assert!(url.ends_with("/sta%20arg%20test%231/page.html"), "{url}");
        let _ = std::fs::remove_dir_all(&dir);
        // No store in unit tests: plain text resolves with the default engine.
        assert_eq!(argument_to_url("example.com", None).as_deref(), Some("https://example.com"));
        assert!(argument_to_url("two words", None).unwrap().starts_with("https://www.google.com/search?q=two"));
    }
}
