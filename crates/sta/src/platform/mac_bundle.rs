//! The macOS app bundle [owner: chrome] (docs/research/platform.md §10).
//!
//! A CEF app cannot run as a bare executable on macOS: the framework is loaded at runtime from
//! `Contents/Frameworks/`, and every child process is started from a helper *bundle* so the OS gives
//! it the right process type and keeps it out of the Dock. So the binary carries the bundle with it:
//!
//! ```text
//! sta.app/Contents/
//!   Info.plist                                        CFBundleExecutable = sta
//!   MacOS/sta                                         this binary
//!   Frameworks/Chromium Embedded Framework.framework  symlinked while developing, copied for release
//!   Frameworks/sta Helper.app/Contents/MacOS/sta Helper          hard links to the binary above,
//!   Frameworks/sta Helper (GPU).app/…                            started through
//!   Frameworks/sta Helper (Renderer).app/…                       `Settings.browser_subprocess_path`
//!   Frameworks/sta Helper (Plugin).app/…
//!   Frameworks/sta Helper (Alerts).app/…
//! ```
//!
//! [`ensure_bundled`] is the first thing `main` does on macOS: started from `target/<profile>/sta`
//! (a plain `cargo run`), a debug build assembles the bundle next to itself and re-executes the copy
//! inside it, so the terminal keeps the process and its output. A release build refuses instead —
//! an installed sta is always inside its bundle.
//!
//! `--sta-bundle-mac[=<dir>]` builds a *standalone* bundle — the framework copied in, and a real
//! copy of the binary in every helper — and exits. That is the one a release ships
//! (tools/package-release.mjs): hard links and a symlinked framework are for the build directory,
//! where nothing is signed and 500 MB per build would be absurd.
//!
//! Public API:
//! - `pub fn ensure_bundled() -> !`-ish: returns only when this process is inside a bundle
//! - `pub fn load_framework() -> Result<(), String>` — before the first CEF call, in every process
//! - `pub fn framework_dir() -> Option<PathBuf>`, `pub fn main_bundle() -> Option<PathBuf>`,
//!   `pub fn helper_exe() -> Option<PathBuf>` — the three `Settings` paths
//! - `pub fn build(exe: &Path, into: &Path, standalone: bool) -> io::Result<PathBuf>`

use std::ffi::CString;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

/// Command-line switch that builds a self-contained bundle and exits.
pub const BUNDLE_SWITCH: &str = "--sta-bundle-mac";
/// Guards against an endless re-exec if a bundle somehow does not look like one.
const RELAUNCHED_ENV: &str = "STA_MAC_RELAUNCHED";

const APP_NAME: &str = "sta";
const BUNDLE_ID: &str = "com.p-asta.sta";
const FRAMEWORK: &str = "Chromium Embedded Framework.framework";
/// The helper suffixes Chromium uses; the generic one (last) is what CEF is pointed at.
const HELPERS: &[&str] = &[" (GPU)", " (Renderer)", " (Plugin)", " (Alerts)", ""];

/// Where the build found the prebuilt CEF distribution (empty when it did not); `build.rs`.
const CEF_DIR: &str = env!("STA_CEF_DIR");

// ------------------------------------------------------------------------------------ locating

/// The `.app` this executable runs from, if any (`…/sta.app/Contents/MacOS/sta`).
pub fn main_bundle() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    // A helper is nested in the main bundle; take the outermost `.app`.
    let mut app = None;
    for dir in exe.ancestors() {
        if dir.extension().is_some_and(|e| e == "app") {
            app = Some(dir.to_path_buf());
        }
    }
    app
}

/// `<app>/Contents/Frameworks/Chromium Embedded Framework.framework`, resolved through the symlink
/// a development bundle uses.
pub fn framework_dir() -> Option<PathBuf> {
    let frameworks = main_bundle()?.join("Contents/Frameworks");
    let path = frameworks.join(FRAMEWORK);
    path.exists().then(|| path.canonicalize().unwrap_or(path))
}

/// The helper executable every child process is started from.
pub fn helper_exe() -> Option<PathBuf> {
    let path = main_bundle()?.join(format!("Contents/Frameworks/{APP_NAME} Helper.app/Contents/MacOS/{APP_NAME} Helper"));
    path.exists().then_some(path)
}

/// Loads the CEF framework into this process. **The first thing every process does**, before
/// `api_hash` — on macOS libcef is not linked, it is `dlopen`ed from the bundle.
pub fn load_framework() -> Result<(), String> {
    let dir = framework_dir().ok_or("the CEF framework is missing from Contents/Frameworks")?;
    let binary = dir.join("Chromium Embedded Framework");
    let path = CString::new(binary.as_os_str().as_bytes()).map_err(|_| "the framework path is not a C string")?;
    // SAFETY: a NUL-terminated path to the framework binary, loaded once per process.
    let loaded = unsafe { cef::load_library(Some(&*path.as_ptr().cast())) };
    if loaded != 1 {
        return Err(format!("cannot load {}", binary.display()));
    }
    Ok(())
}

// ------------------------------------------------------------------------------------ bootstrap

/// Returns when this process runs from a bundle; otherwise assembles one and re-executes into it
/// (debug builds), or ends the process with an explanation (release builds).
///
/// Also serves `--sta-bundle-mac[=<dir>]`, which only builds and exits.
pub fn ensure_bundled() {
    if let Some(arg) = std::env::args().find(|a| a == BUNDLE_SWITCH || a.starts_with(&format!("{BUNDLE_SWITCH}="))) {
        let into = arg.split_once('=').map(|(_, dir)| PathBuf::from(dir));
        std::process::exit(bundle_and_exit(into.as_deref()));
    }
    if main_bundle().is_some() {
        return;
    }
    let exe = std::env::current_exe().unwrap_or_default();
    if !cfg!(debug_assertions) || std::env::var_os(RELAUNCHED_ENV).is_some() {
        eprintln!(
            "[sta] {} is not inside an app bundle.\n      macOS needs sta.app (the CEF framework and the helper processes live in it).\n      Build one with: {} {BUNDLE_SWITCH}",
            exe.display(),
            exe.display()
        );
        std::process::exit(1);
    }
    // `cargo run`: assemble `target/<profile>/sta.app` around this binary and run that instead.
    let Some(dir) = exe.parent() else { std::process::exit(1) };
    let app = match build(&exe, dir, false) {
        Ok(app) => app,
        Err(e) => {
            eprintln!("[sta] cannot build {}/{APP_NAME}.app: {e}", dir.display());
            std::process::exit(1);
        }
    };
    let bundled = app.join(format!("Contents/MacOS/{APP_NAME}"));
    // console-ok: macOS-only code — there are no console windows to hide here.
    let mut relaunch = std::process::Command::new(&bundled);
    relaunch.args(std::env::args_os().skip(1)).env(RELAUNCHED_ENV, "1");
    let error = std::os::unix::process::CommandExt::exec(&mut relaunch);
    // `exec` only returns on failure.
    eprintln!("[sta] cannot start {}: {error}", bundled.display());
    std::process::exit(1);
}

fn bundle_and_exit(into: Option<&Path>) -> i32 {
    let Ok(exe) = std::env::current_exe() else {
        eprintln!("[sta] cannot read the path of this executable");
        return 1;
    };
    let into = into.map(Path::to_path_buf).or_else(|| exe.parent().map(Path::to_path_buf)).unwrap_or_default();
    match build(&exe, &into, true) {
        Ok(app) => {
            println!("{}", app.display());
            0
        }
        Err(e) => {
            eprintln!("[sta] cannot build the app bundle: {e}");
            1
        }
    }
}

// ------------------------------------------------------------------------------------ building

/// Assembles `<into>/sta.app` around `exe`, refreshing what changed. `standalone` makes a bundle
/// that can be moved anywhere and signed: the CEF framework is copied in (~500 MB) instead of
/// symlinked, and each helper gets its own copy of the binary instead of a hard link to it — a
/// signature belongs to a file, and five names for one file cannot carry five identities.
pub fn build(exe: &Path, into: &Path, standalone: bool) -> io::Result<PathBuf> {
    let app = into.join(format!("{APP_NAME}.app"));
    let contents = app.join("Contents");
    let frameworks = contents.join("Frameworks");
    std::fs::create_dir_all(contents.join("MacOS"))?;
    std::fs::create_dir_all(contents.join("Resources"))?;
    std::fs::create_dir_all(&frameworks)?;

    write_if_changed(&contents.join("Info.plist"), app_plist(APP_NAME, BUNDLE_ID, false).as_bytes())?;
    let main_exe = contents.join("MacOS").join(APP_NAME);
    let refreshed = copy_if_changed(exe, &main_exe)?;

    for suffix in HELPERS {
        let name = format!("{APP_NAME} Helper{suffix}");
        let id = format!("{BUNDLE_ID}.helper{}", suffix.trim().trim_matches(['(', ')']).to_ascii_lowercase());
        let helper = frameworks.join(format!("{name}.app/Contents"));
        std::fs::create_dir_all(helper.join("MacOS"))?;
        write_if_changed(&helper.join("Info.plist"), app_plist(&name, &id, true).as_bytes())?;
        let helper_exe = helper.join("MacOS").join(&name);
        if standalone {
            copy_if_changed(&main_exe, &helper_exe)?;
        } else if refreshed || !helper_exe.exists() {
            // Hard links: five more copies of the binary would cost hundreds of megabytes, and a
            // symlink would make `current_exe` (which macOS does not resolve) point outside the
            // bundle. They are re-made whenever the binary they point at is replaced.
            let _ = std::fs::remove_file(&helper_exe);
            std::fs::hard_link(&main_exe, &helper_exe).or_else(|_| std::fs::copy(&main_exe, &helper_exe).map(|_| ()))?;
        }
    }

    install_framework(&frameworks.join(FRAMEWORK), standalone)?;
    Ok(app)
}

/// Puts the CEF framework in the bundle: a symlink to the prebuilt distribution while developing,
/// a copy when the bundle has to stand on its own. An existing entry of the wrong kind is replaced.
fn install_framework(target: &Path, copy: bool) -> io::Result<()> {
    let source = Path::new(CEF_DIR).join(FRAMEWORK);
    let is_link = std::fs::symlink_metadata(target).is_ok_and(|m| m.file_type().is_symlink());
    if !copy {
        if is_link && std::fs::read_link(target).is_ok_and(|l| l == source) {
            return Ok(());
        }
    } else if !is_link && target.join("Resources").exists() {
        return Ok(());
    }
    if !source.exists() {
        if target.exists() {
            // Nothing to install, but what is there already works.
            return Ok(());
        }
        let hint = if CEF_DIR.is_empty() { "the build found no CEF distribution — see the cargo warning from sta's build script" } else { CEF_DIR };
        return Err(io::Error::other(format!("the CEF framework is not at {} ({hint})", source.display())));
    }
    if is_link {
        std::fs::remove_file(target)?;
    } else if target.exists() {
        std::fs::remove_dir_all(target)?;
    }
    if copy { copy_dir(&source, target) } else { std::os::unix::fs::symlink(&source, target) }
}

fn copy_dir(from: &Path, to: &Path) -> io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let (source, target) = (entry.path(), to.join(entry.file_name()));
        let kind = entry.file_type()?;
        if kind.is_symlink() {
            let link = std::fs::read_link(&source)?;
            let _ = std::fs::remove_file(&target);
            std::os::unix::fs::symlink(link, &target)?;
        } else if kind.is_dir() {
            copy_dir(&source, &target)?;
        } else {
            std::fs::copy(&source, &target)?;
        }
    }
    Ok(())
}

/// Copies when the target is missing or differs in size or modification time. `true` = it was
/// written (the helper links have to be re-made against the new file).
fn copy_if_changed(from: &Path, to: &Path) -> io::Result<bool> {
    let source = std::fs::metadata(from)?;
    if let Ok(target) = std::fs::metadata(to)
        && target.len() == source.len()
        && target.modified().ok() == source.modified().ok()
    {
        return Ok(false);
    }
    // Never write into the running copy in place: unlink first, so a process using it keeps the
    // file it started from.
    let _ = std::fs::remove_file(to);
    std::fs::copy(from, to)?;
    filetime_from(from, to)?;
    Ok(true)
}

/// Gives `to` the modification time of `from`, so [`copy_if_changed`] can compare them.
fn filetime_from(from: &Path, to: &Path) -> io::Result<()> {
    let modified = std::fs::metadata(from)?.modified()?;
    std::fs::File::options().write(true).open(to)?.set_modified(modified)
}

fn write_if_changed(path: &Path, contents: &[u8]) -> io::Result<()> {
    if std::fs::read(path).is_ok_and(|current| current == contents) {
        return Ok(());
    }
    std::fs::write(path, contents)
}

/// The `Info.plist` of the app or of one of its helpers. Written by hand: the alternative is a
/// plist crate for eleven static keys.
fn app_plist(executable: &str, identifier: &str, helper: bool) -> String {
    let version = env!("CARGO_PKG_VERSION");
    // A helper has no UI of its own: `LSUIElement` keeps it out of the Dock and the app switcher.
    let ui_element = if helper { "\t<key>LSUIElement</key>\n\t<string>1</string>\n" } else { "" };
    // The permission prompts Chromium can raise from a page. macOS kills the process that asks
    // without them.
    let usage = if helper {
        String::new()
    } else {
        ["NSCameraUsageDescription", "NSMicrophoneUsageDescription", "NSBluetoothAlwaysUsageDescription", "NSLocationWhenInUseUsageDescription"]
            .iter()
            .map(|key| format!("\t<key>{key}</key>\n\t<string>A site you are visiting in sta asked for this.</string>\n"))
            .collect()
    };
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>CFBundleDevelopmentRegion</key>
	<string>en</string>
	<key>CFBundleDisplayName</key>
	<string>{executable}</string>
	<key>CFBundleExecutable</key>
	<string>{executable}</string>
	<key>CFBundleIdentifier</key>
	<string>{identifier}</string>
	<key>CFBundleInfoDictionaryVersion</key>
	<string>6.0</string>
	<key>CFBundleName</key>
	<string>{executable}</string>
	<key>CFBundlePackageType</key>
	<string>APPL</string>
	<key>CFBundleShortVersionString</key>
	<string>{version}</string>
	<key>CFBundleVersion</key>
	<string>{version}</string>
	<key>LSEnvironment</key>
	<dict>
		<key>MallocNanoZone</key>
		<string>0</string>
	</dict>
	<key>LSMinimumSystemVersion</key>
	<string>11.0</string>
{ui_element}	<key>NSHighResolutionCapable</key>
	<true/>
	<key>NSSupportsAutomaticGraphicsSwitching</key>
	<true/>
{usage}</dict>
</plist>
"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_plist_names_the_executable_and_hides_helpers() {
        let app = app_plist("sta", BUNDLE_ID, false);
        assert!(app.contains("<key>CFBundleExecutable</key>\n\t<string>sta</string>"), "{app}");
        assert!(!app.contains("LSUIElement"));
        assert!(app.contains("NSCameraUsageDescription"));
        let helper = app_plist("sta Helper (GPU)", "com.p-asta.sta.helper.gpu", true);
        assert!(helper.contains("<key>LSUIElement</key>\n\t<string>1</string>"), "{helper}");
        assert!(!helper.contains("NSCameraUsageDescription"));
    }

    #[test]
    fn a_bundle_is_assembled_and_refreshed_in_place() {
        let root = std::env::temp_dir().join(format!("sta-bundle-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let exe = root.join("sta");
        std::fs::write(&exe, b"#!/bin/sh\ntrue\n").unwrap();

        let app = build(&exe, &root, false).unwrap();
        let main_exe = app.join("Contents/MacOS/sta");
        assert!(main_exe.exists() && app.join("Contents/Info.plist").exists());
        for suffix in HELPERS {
            let name = format!("sta Helper{suffix}");
            assert!(app.join(format!("Contents/Frameworks/{name}.app/Contents/MacOS/{name}")).exists(), "{name}");
        }
        // Same binary: nothing is rewritten.
        assert!(!copy_if_changed(&exe, &main_exe).unwrap());
        std::fs::write(&exe, b"#!/bin/sh\nfalse\n").unwrap();
        build(&exe, &root, false).unwrap();
        assert_eq!(std::fs::read(&main_exe).unwrap(), b"#!/bin/sh\nfalse\n");
        let helper = app.join("Contents/Frameworks/sta Helper.app/Contents/MacOS/sta Helper");
        assert_eq!(std::fs::read(helper).unwrap(), b"#!/bin/sh\nfalse\n", "the helper links follow the binary");

        let _ = std::fs::remove_dir_all(&root);
    }
}
