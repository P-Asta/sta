//! Build script [skeleton, frozen]: embeds the Windows resources into `sta.exe`, and on macOS
//! records where the CEF framework lives so the app bundle can be assembled (platform/mac_bundle.rs).
//!
//! - `1 ICON res/sta.ico` (lowest id = Explorer/taskbar icon; also `Settings.chrome_app_icon_id`)
//! - `1 RT_MANIFEST res/sta.exe.manifest` (PerMonitorV2 DPI, supportedOS, Common Controls v6)
//! - `VERSIONINFO` from `CARGO_PKG_VERSION` (Task Manager shows `FileDescription` for every process)
//!
//! The `.rc` file is generated into `OUT_DIR` with absolute paths so `rc.exe` does not depend on
//! its working directory (docs/research/platform.md §5). `res/make_icon.py` regenerates the icon.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=res/sta.ico");
    println!("cargo:rerun-if-changed=res/sta.exe.manifest");

    #[cfg(windows)]
    windows_resources();
    #[cfg(target_os = "macos")]
    macos_framework_dir();
}

/// The macOS binary has to be started from an app bundle that carries "Chromium Embedded
/// Framework.framework" (docs/research/platform.md §6). `cef-dll-sys` finds the prebuilt CEF
/// through `CEF_PATH` (set by `.cargo/config.toml`) but tells only its own dependents where it
/// landed, so resolve it here the same way and bake the path into the binary: `mac_bundle.rs`
/// copies or links the framework from there. An empty value means "not found at build time"; the
/// bundler then reports it instead of building a bundle that cannot start.
#[cfg(target_os = "macos")]
fn macos_framework_dir() {
    use std::{env, path::{Path, PathBuf}};

    println!("cargo:rerun-if-env-changed=CEF_PATH");
    const FRAMEWORK: &str = "Chromium Embedded Framework.framework";
    let has_framework = |dir: &Path| dir.join(FRAMEWORK).is_dir();

    let root = env::var_os("CEF_PATH")
        .map(PathBuf::from)
        .map(|p| if p.is_relative() {
            // `.cargo/config.toml` sets it relative to the workspace root (two levels up).
            PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR")).join("../..").join(p)
        } else {
            p
        });
    // `<CEF_PATH>/<cef version>/cef_macos_<arch>/` (the layout cef-dll-sys downloads into), or
    // `<CEF_PATH>/` itself when it points straight at a distribution.
    let found = root.and_then(|root| {
        if has_framework(&root) {
            return Some(root);
        }
        let mut versions: Vec<PathBuf> = std::fs::read_dir(&root).ok()?.flatten().map(|e| e.path()).collect();
        versions.sort();
        versions.into_iter().rev().find_map(|version| {
            if has_framework(&version) {
                return Some(version);
            }
            std::fs::read_dir(&version).ok()?.flatten().map(|e| e.path()).find(|d| has_framework(d))
        })
    });
    let path = found.and_then(|p| p.canonicalize().ok()).unwrap_or_default();
    println!("cargo:rustc-env=STA_CEF_DIR={}", path.display());
}

#[cfg(windows)]
fn windows_resources() {
    use std::{env, fs, path::PathBuf};

    let res = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR")).join("res");
    let esc = |p: PathBuf| p.to_string_lossy().replace('\\', "\\\\");
    let version = env::var("CARGO_PKG_VERSION").expect("CARGO_PKG_VERSION");
    let mut parts: Vec<u32> = version
        .split(|c: char| !c.is_ascii_digit())
        .filter_map(|s| s.parse().ok())
        .collect();
    parts.resize(4, 0);
    let (a, b, c, d) = (parts[0], parts[1], parts[2], parts[3]);

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
      VALUE "FileVersion", "{version}"
      VALUE "InternalName", "sta"
      VALUE "LegalCopyright", "MIT OR Apache-2.0"
      VALUE "OriginalFilename", "sta.exe"
      VALUE "ProductName", "sta"
      VALUE "ProductVersion", "{version}"
    END
  END
  BLOCK "VarFileInfo"
  BEGIN
    VALUE "Translation", 0x409, 1200
  END
END
"#,
        icon = esc(res.join("sta.ico")),
        manifest = esc(res.join("sta.exe.manifest")),
    );
    let out = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR")).join("sta.rc");
    fs::write(&out, rc).expect("write sta.rc");
    embed_resource::compile(&out, embed_resource::NONE)
        .manifest_required()
        .expect("embed Windows resources (rc.exe from the Windows SDK is required)");
}
