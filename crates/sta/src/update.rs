//! In-app updates [owner: chrome] (ARCHITECTURE §4.10, docs/RELEASING.md, `sta_core::update`).
//!
//! A release is an archive and a `latest.json` on GitHub (`.github/workflows/release.yml`). This
//! module is the other half: a few seconds after the window is up, sta reads that manifest, and if
//! it describes a newer build it says so. Downloading and applying are the user's decision
//! (`Command::DownloadUpdate` / `Command::InstallUpdate` from Settings › About).
//!
//! The steps, and what each one refuses:
//!
//! 1. **Check** — `GET` [`MANIFEST_URL`] through the Chromium network stack (`Urlrequest`, like
//!    `suggest.rs`): system proxy, system certificates, no cookies, no cache. A body over
//!    [`MAX_MANIFEST_BYTES`], a non-200 status, a version that is not a version or one that is not
//!    newer than this build ends it. GitHub serves `releases/latest` from the latest **published**
//!    release, so a draft offers nobody anything.
//! 2. **Download** — only the archive this manifest names for this platform, only from this
//!    repository's own releases over HTTPS ([`sta_core::update::Asset::trusted`]), streamed to
//!    `<data>/updates/<version>.zip` and hashed as it arrives. A size that disagrees with the
//!    manifest, or a SHA-256 that does, deletes the file and fails the update.
//! 3. **Unpack** — into `<data>/updates/<version>/`, refusing any entry whose path escapes it
//!    (`..`, an absolute path, a drive letter) and any archive that does not carry `sta.exe`.
//!    The result is a complete browser directory, which is what makes the swap below a rename.
//! 4. **Apply** — on request, the *staged* `sta.exe` is started with [`APPLY_FLAG`] and sta quits.
//!    That helper waits for this process to exit, copies the staged files over the installed ones
//!    and starts the new build. It runs from the staging directory, never from the directory it is
//!    replacing, which is the only reason replacing a running program's own files can work at all.
//!
//! Nothing here ever runs while the MCP test surface is armed (`STA_E2E`), and
//! `STA_NO_UPDATE_CHECK=1` turns the startup check off for a run.
//!
//! Public API:
//! - `pub fn check_after_startup()` — post the delayed first check (window.rs, once)
//! - `pub fn start_check(reason: &'static str)`, `pub fn start_download()`, `pub fn install_now()`
//! - `pub fn apply_from_command_line() -> bool` — the helper mode, before CEF initializes
//! - `pub fn clean_staging()` — drop finished staging directories (startup)
//! - `pub fn clear()` — drop pending requests before `cef::shutdown()`
//! - `pub fn debug_snapshot() -> serde_json::Value` (debug builds)

use crate::{controller, paths, task};
use cef::*;
use sta_core::update::{Manifest, Sha256, UpdateStatus, MANIFEST_URL, MAX_ARCHIVE_BYTES, MAX_MANIFEST_BYTES};
use sta_core::Command;
use std::cell::{Cell, RefCell};
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command as Process;
#[cfg(windows)]
use std::os::windows::process::CommandExt;

/// The first check runs this long after the browser is up: late enough that it costs a starting
/// browser nothing, early enough that a session of any length sees it.
pub const CHECK_DELAY_MS: i64 = 8_000;
/// A manifest that has not arrived by then is a failed check.
const MANIFEST_TIMEOUT_MS: i64 = 20_000;
/// A download that has not finished by then is a failed download (a release is a few hundred MB).
const DOWNLOAD_TIMEOUT_MS: i64 = 30 * 60 * 1000;
/// Progress is reported to the UI at most this often (a 200 MB download is thousands of chunks).
const PROGRESS_EVERY_MS: i64 = 400;
/// The flag that turns a staged `sta.exe` into the helper that replaces the installed one.
pub const APPLY_FLAG: &str = "--sta-apply-update";
/// The helper gives the browser this long to exit before it gives up.
#[cfg(windows)]
const WAIT_FOR_EXIT_MS: u32 = 30_000;
/// `CREATE_NO_WINDOW`: neither the helper nor the browser it starts gets a console.
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

const UR_FLAG_DISABLE_CACHE: i32 = sys::cef_urlrequest_flags_t::UR_FLAG_DISABLE_CACHE.0 as i32;
const UR_FLAG_NO_RETRY_ON_5XX: i32 = sys::cef_urlrequest_flags_t::UR_FLAG_NO_RETRY_ON_5XX.0 as i32;

/// What a request in flight is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Manifest,
    Archive,
}

struct Pending {
    id: u64,
    kind: Kind,
    request: Option<Urlrequest>,
    /// The manifest body (`Kind::Manifest`) — an archive never goes through memory.
    body: Vec<u8>,
    /// The archive being written, its path, and what is known about it.
    file: Option<File>,
    path: PathBuf,
    hasher: Sha256,
    received: u64,
    total: u64,
    version: String,
    expected_sha256: String,
    last_progress: i64,
}

thread_local! {
    static PENDING: RefCell<Option<Pending>> = const { RefCell::new(None) };
    static NEXT_ID: Cell<u64> = const { Cell::new(1) };
    /// The manifest of the last successful check, so a download does not need a second one.
    static LATEST: RefCell<Option<Manifest>> = const { RefCell::new(None) };
    /// Where the update that is `Ready` was unpacked.
    static STAGED: RefCell<Option<PathBuf>> = const { RefCell::new(None) };
    static CHECKED: Cell<bool> = const { Cell::new(false) };
}

// ------------------------------------------------------------------------------------ status

fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

/// Tells core (and through it the UI) where the update stands.
fn report(status: UpdateStatus) {
    controller::dispatch(Command::UpdateStatusChanged { status });
}

fn fail(message: impl Into<String>) {
    let message = message.into();
    log_warn!("update: {message}");
    PENDING.with(|p| p.borrow_mut().take()); // the request, and the half-written file handle
    report(UpdateStatus::Failed { message });
}

/// Whether this run may talk to the update server at all: never under the e2e harness (a suite
/// must not depend on the network, and an armed browser is not a browser anybody updates), and
/// never with `STA_NO_UPDATE_CHECK=1`.
fn updates_allowed() -> bool {
    if std::env::var("STA_NO_UPDATE_CHECK").is_ok_and(|v| v != "0") {
        return false;
    }
    if std::env::var("STA_E2E").is_ok_and(|v| v == "1") {
        return false;
    }
    true
}

// ------------------------------------------------------------------------------------- check

/// Posts the first check (window.rs, when the browser is up). Does nothing when updates are off.
pub fn check_after_startup() {
    if !updates_allowed() || CHECKED.replace(true) {
        return;
    }
    task::post_ui_delayed(CHECK_DELAY_MS, || start_check("startup"));
}

/// Fetches the manifest. A check while anything is already in flight is ignored.
pub fn start_check(reason: &'static str) {
    if !updates_allowed() {
        return;
    }
    if PENDING.with(|p| p.borrow().is_some()) {
        log_debug!("update: a request is already in flight ({reason})");
        return;
    }
    if STAGED.with(|s| s.borrow().is_some()) {
        return; // something is already staged for the next start
    }
    let id = NEXT_ID.with(|n| n.replace(n.get() + 1));
    log_info!("update: checking {MANIFEST_URL} ({reason})");
    report(UpdateStatus::Checking);
    PENDING.with(|p| {
        *p.borrow_mut() = Some(Pending {
            id,
            kind: Kind::Manifest,
            request: None,
            body: Vec::new(),
            file: None,
            path: PathBuf::new(),
            hasher: Sha256::new(),
            received: 0,
            total: 0,
            version: String::new(),
            expected_sha256: String::new(),
            last_progress: 0,
        });
    });
    start_request(MANIFEST_URL, id, Kind::Manifest, MANIFEST_TIMEOUT_MS);
}

/// A manifest arrived and was 200: decide what it means.
fn on_manifest(body: Vec<u8>) {
    let manifest = match Manifest::parse(&body) {
        Ok(m) => m,
        Err(e) => return fail(e),
    };
    let current = env!("CARGO_PKG_VERSION");
    if !manifest.is_newer_than(current) {
        log_info!("update: {current} is current (latest release is {})", manifest.version);
        report(UpdateStatus::UpToDate { checked_at: now_ms() });
        LATEST.with(|l| *l.borrow_mut() = Some(manifest));
        return;
    }
    let Some(asset) = manifest.asset() else {
        log_info!("update: release {} has nothing for {}", manifest.version, sta_core::update::platform_key());
        report(UpdateStatus::UpToDate { checked_at: now_ms() });
        return;
    };
    log_info!("update: {} is available ({} bytes)", manifest.version, asset.size);
    report(UpdateStatus::Available { version: manifest.version.clone(), notes: manifest.notes.clone(), size: asset.size });
    LATEST.with(|l| *l.borrow_mut() = Some(manifest));
}

// ---------------------------------------------------------------------------------- download

/// `Effect::DownloadUpdate`: fetch the archive of the release the last check found.
pub fn start_download() {
    if !updates_allowed() {
        return;
    }
    if PENDING.with(|p| p.borrow().is_some()) {
        return;
    }
    let Some((version, url, sha256, size)) = LATEST.with(|l| {
        let latest = l.borrow();
        let manifest = latest.as_ref()?;
        let asset = manifest.asset()?;
        Some((manifest.version.clone(), asset.url.clone(), asset.sha256.clone(), asset.size))
    }) else {
        return fail("nothing to download: check for an update first");
    };
    let dir = match staging_dir() {
        Ok(d) => d,
        Err(e) => return fail(format!("cannot create the staging directory: {e}")),
    };
    let path = dir.join(format!("sta-{version}.zip"));
    let _ = fs::remove_file(&path);
    let file = match File::create(&path) {
        Ok(f) => f,
        Err(e) => return fail(format!("cannot write {}: {e}", path.display())),
    };
    let id = NEXT_ID.with(|n| n.replace(n.get() + 1));
    log_info!("update: downloading {version} from {url}");
    report(UpdateStatus::Downloading { version: version.clone(), received: 0, total: size });
    PENDING.with(|p| {
        *p.borrow_mut() = Some(Pending {
            id,
            kind: Kind::Archive,
            request: None,
            body: Vec::new(),
            file: Some(file),
            path,
            hasher: Sha256::new(),
            received: 0,
            total: size,
            version,
            expected_sha256: sha256,
            last_progress: now_ms(),
        });
    });
    start_request(&url, id, Kind::Archive, DOWNLOAD_TIMEOUT_MS);
}

/// The archive is complete: check the hash, then unpack it.
fn on_archive(version: String, path: PathBuf, digest: String, expected: String, received: u64) {
    if digest != expected {
        let _ = fs::remove_file(&path);
        return fail(format!("the download of {version} does not match the release ({received} bytes, sha256 {digest})"));
    }
    log_info!("update: {version} downloaded and verified ({received} bytes)");
    let dir = path.with_extension(""); // …/sta-1.2.3.zip → …/sta-1.2.3
    match unpack(&path, &dir) {
        Ok(root) => {
            let _ = fs::remove_file(&path); // the archive is unpacked; the tree is what matters
            log_info!("update: {version} is staged in {}", root.display());
            STAGED.with(|s| *s.borrow_mut() = Some(root));
            report(UpdateStatus::Ready { version });
        }
        Err(e) => {
            let _ = fs::remove_dir_all(&dir);
            let _ = fs::remove_file(&path);
            fail(format!("cannot unpack {version}: {e}"));
        }
    }
}

/// Unpacks `archive` into `dir` and returns the directory holding `sta.exe` (an archive made by
/// `tools/package-release.mjs` has exactly one directory in it). Every path in it is checked
/// against `dir` first (`unzip::extract`).
fn unpack(archive: &Path, dir: &Path) -> Result<PathBuf, String> {
    let _ = fs::remove_dir_all(dir);
    fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let bytes = fs::read(archive).map_err(|e| format!("{}: {e}", archive.display()))?;
    let files = crate::unzip::extract(&bytes, dir)?;
    log_debug!("update: {files} files unpacked into {}", dir.display());
    let root = payload_root(dir).ok_or_else(|| format!("no {} in the archive", exe_name()))?;
    Ok(root)
}

// ----------------------------------------------------------------------------------- install

/// `Effect::InstallUpdate`: start the staged build as the apply helper and let the shell quit. The
/// helper waits for this process to go away before it touches anything.
pub fn install_now() -> bool {
    let Some(source) = STAGED.with(|s| s.borrow().clone()) else {
        log_warn!("update: nothing is staged to install");
        return false;
    };
    let Some(target) = install_dir() else {
        fail("cannot find the directory sta runs from");
        return false;
    };
    let helper = exe_in(&source);
    if !helper.exists() {
        fail(format!("the staged build has no {}", exe_name()));
        return false;
    }
    let mut command = Process::new(&helper);
    command
        .arg(APPLY_FLAG)
        .arg("--source")
        .arg(&source)
        .arg("--target")
        .arg(&target)
        .arg("--pid")
        .arg(std::process::id().to_string());
    // `CREATE_NO_WINDOW` and **not** `DETACHED_PROCESS`: a console child of a console-less parent
    // is handed a fresh console, which Windows 11 opens as a Windows Terminal window
    // (docs/TESTING.md, tools/check-no-console.mjs). The helper outlives us either way — a child
    // does not die with its parent on Windows.
    #[cfg(windows)]
    command.creation_flags(CREATE_NO_WINDOW);
    match command.spawn() {
        Ok(child) => {
            log_info!("update: apply helper {} started (pid {})", helper.display(), child.id());
            true
        }
        Err(e) => {
            fail(format!("cannot start the apply helper: {e}"));
            false
        }
    }
}

/// The helper: replaces the installed files with the staged ones and starts the new build.
///
/// Runs **before** anything else in `main`, in a process started from the staging directory, so
/// nothing it copies over is in use. Returns `true` when this process was the helper (and has done
/// its work): `main` then exits without starting a browser.
pub fn apply_from_command_line() -> bool {
    let args: Vec<String> = std::env::args().collect();
    if !args.iter().any(|a| a == APPLY_FLAG) {
        return false;
    }
    let value = |name: &str| {
        args.iter()
            .position(|a| a == name)
            .and_then(|i| args.get(i + 1))
            .map(|s| s.to_string())
    };
    let (Some(source), Some(target)) = (value("--source"), value("--target")) else {
        eprintln!("[sta] {APPLY_FLAG} needs --source and --target");
        return true;
    };
    let (source, target) = (PathBuf::from(source), PathBuf::from(target));
    let pid: u32 = value("--pid").and_then(|p| p.parse().ok()).unwrap_or(0);
    let log = |line: &str| {
        let stamp = now_ms();
        eprintln!("[sta apply] {line}");
        if let Some(parent) = source.parent()
            && let Ok(mut f) = fs::OpenOptions::new().create(true).append(true).open(parent.join("apply.log"))
        {
            let _ = writeln!(f, "{stamp} {line}");
        }
    };
    log(&format!("replacing {} with {}", target.display(), source.display()));
    if pid != 0 {
        wait_for_exit(pid);
    }
    match copy_tree(&source, &target) {
        Ok(files) => log(&format!("{files} files copied")),
        Err(e) => {
            log(&format!("FAILED: {e}"));
            // The installed build is untouched (or half-copied and about to be repaired by the
            // next attempt); start what is there so the user is not left with nothing.
        }
    }
    let exe = exe_in(&target);
    let mut browser = Process::new(&exe);
    // The window it opens is its own; this only stops a console being allocated for it.
    #[cfg(windows)]
    browser.creation_flags(CREATE_NO_WINDOW);
    match browser.spawn() {
        Ok(child) => log(&format!("started {} (pid {})", exe.display(), child.id())),
        Err(e) => log(&format!("cannot start {}: {e}", exe.display())),
    }
    true
}

/// Copies every file of `from` over `to`, creating directories as needed. Files `to` has and
/// `from` does not are left alone: an old locale is harmless, a missing DLL is not.
fn copy_tree(from: &Path, to: &Path) -> Result<u32, String> {
    let mut files = 0;
    let mut stack = vec![from.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir).map_err(|e| format!("{}: {e}", dir.display()))?.flatten() {
            let path = entry.path();
            let relative = path.strip_prefix(from).map_err(|e| e.to_string())?;
            let destination = to.join(relative);
            if path.is_dir() {
                fs::create_dir_all(&destination).map_err(|e| format!("{}: {e}", destination.display()))?;
                stack.push(path);
                continue;
            }
            if let Some(parent) = destination.parent() {
                fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
            }
            // A file that is still locked (a renderer that has not quite exited) is retried a few
            // times before the whole update is called off.
            let mut last = String::new();
            let mut copied = false;
            for attempt in 0..10 {
                match fs::copy(&path, &destination) {
                    Ok(_) => {
                        copied = true;
                        break;
                    }
                    Err(e) => {
                        last = e.to_string();
                        std::thread::sleep(std::time::Duration::from_millis(100 * (attempt + 1)));
                    }
                }
            }
            if !copied {
                return Err(format!("{}: {last}", destination.display()));
            }
            files += 1;
        }
    }
    Ok(files)
}

/// Waits for `pid` to exit (at most [`WAIT_FOR_EXIT_MS`]).
#[cfg(windows)]
fn wait_for_exit(pid: u32) {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{OpenProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE};
    // SAFETY: a handle to another process, waited on and closed; no pointers are involved.
    unsafe {
        let handle = OpenProcess(PROCESS_SYNCHRONIZE, 0, pid);
        if handle.is_null() {
            return; // already gone
        }
        WaitForSingleObject(handle, WAIT_FOR_EXIT_MS);
        CloseHandle(handle);
    }
    // Renderers and the GPU process follow the browser out; give them a moment to release files.
    std::thread::sleep(std::time::Duration::from_millis(400));
}

#[cfg(not(windows))]
fn wait_for_exit(_pid: u32) {
    std::thread::sleep(std::time::Duration::from_millis(1500));
}

// ------------------------------------------------------------------------------------- paths

const fn exe_name() -> &'static str {
    if cfg!(windows) {
        "sta.exe"
    } else {
        "sta"
    }
}

/// The browser executable inside a payload root — an update's staged copy or the installed one.
/// On macOS the root is the `.app` bundle and the binary lives in `Contents/MacOS`.
fn exe_in(root: &Path) -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        root.join("Contents/MacOS").join(exe_name())
    }
    #[cfg(not(target_os = "macos"))]
    {
        root.join(exe_name())
    }
}

/// The payload inside an unpacked archive: the directory that holds the browser. A release wraps
/// everything in `sta-<version>-<platform>/`, and on macOS the bundle is another level in
/// (`…/sta.app`), so three levels are searched.
fn payload_root(dir: &Path) -> Option<PathBuf> {
    let mut level = vec![dir.to_path_buf()];
    for _ in 0..3 {
        if let Some(found) = level.iter().find(|p| exe_in(p).is_file()) {
            return Some(found.clone());
        }
        level = level.iter().filter_map(|p| fs::read_dir(p).ok()).flatten().flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect();
    }
    None
}

/// `<data>/updates`, created if needed. Outside the installed directory on purpose: the helper
/// runs from in here while it replaces that one.
fn staging_dir() -> std::io::Result<PathBuf> {
    let dir = paths::dirs().base.join("updates");
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// What an update replaces: the app bundle on macOS, the directory sta was started from
/// elsewhere.
fn install_dir() -> Option<PathBuf> {
    // macOS installs are app bundles: the whole `sta.app` is replaced, not the directory the
    // binary happens to sit in (the framework and the helper apps are in there too).
    #[cfg(target_os = "macos")]
    {
        crate::platform::mac_bundle::main_bundle()
    }
    #[cfg(not(target_os = "macos"))]
    {
        std::env::current_exe().ok()?.parent().map(|p| p.to_path_buf())
    }
}

/// Drops staging directories left behind by an update that has been applied (startup). The build
/// that is running now is never the one in there: the helper started it from the installed copy.
pub fn clean_staging() {
    let Ok(dir) = staging_dir() else { return };
    let Ok(entries) = fs::read_dir(&dir) else { return };
    let current = format!("sta-{}", env!("CARGO_PKG_VERSION"));
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if name == "apply.log" {
            continue;
        }
        // Every `sta-*` in here is finished business: this build runs from the installed copy the
        // helper started, never from the staging directory, and an interrupted download is fetched
        // again rather than resumed.
        if name.starts_with("sta-") {
            log_debug!("update: dropping {} (staged for {current} or earlier)", path.display());
            let _ = if path.is_dir() { fs::remove_dir_all(&path) } else { fs::remove_file(&path) };
        }
    }
}

// ----------------------------------------------------------------------------------- requests

/// `GET url` on the global request context, reported to [`UpdateClient`].
fn start_request(url: &str, id: u64, kind: Kind, timeout_ms: i64) {
    let Some(mut request) = request_create() else {
        return fail("cannot create a URL request");
    };
    request.set_url(Some(&CefString::from(url)));
    request.set_method(Some(&CefString::from("GET")));
    // No credentials: a release is public, and nothing of the user's belongs in this request.
    request.set_flags(UR_FLAG_DISABLE_CACHE | UR_FLAG_NO_RETRY_ON_5XX);
    let mut client = UpdateClient::new(id);
    let mut context = request_context_get_global_context();
    let Some(handle) = urlrequest_create(Some(&mut request), Some(&mut client), context.as_mut()) else {
        return fail("cannot start the request");
    };
    let orphan = PENDING.with(|p| match p.borrow_mut().as_mut() {
        Some(pending) if pending.id == id => {
            pending.request = Some(handle);
            None
        }
        _ => Some(handle),
    });
    if let Some(handle) = orphan {
        task::post_ui(move || drop(handle));
    }
    task::post_ui_delayed(timeout_ms, move || {
        let timed_out = PENDING.with(|p| p.borrow().as_ref().is_some_and(|pending| pending.id == id));
        if timed_out {
            fail(match kind {
                Kind::Manifest => "the update check timed out",
                Kind::Archive => "the download timed out",
            });
        }
    });
}

wrap_urlrequest_client! {
    struct UpdateClient {
        id: u64,
    }

    impl UrlrequestClient {
        fn on_download_progress(&self, _request: Option<&mut Urlrequest>, current: i64, total: i64) {
            let report_now = PENDING.with(|p| {
                let mut pending = p.borrow_mut();
                let pending = pending.as_mut().filter(|x| x.id == self.id && x.kind == Kind::Archive)?;
                if total > 0 {
                    pending.total = total as u64;
                }
                let now = now_ms();
                if now - pending.last_progress < PROGRESS_EVERY_MS {
                    return None;
                }
                pending.last_progress = now;
                Some(UpdateStatus::Downloading { version: pending.version.clone(), received: current.max(0) as u64, total: pending.total })
            });
            if let Some(status) = report_now {
                report(status);
            }
        }

        fn on_download_data(&self, _request: Option<&mut Urlrequest>, data: *const u8, data_length: usize) {
            if data.is_null() || data_length == 0 {
                return;
            }
            // SAFETY: CEF passes `data_length` readable bytes that stay valid for this call.
            let chunk = unsafe { std::slice::from_raw_parts(data, data_length) };
            let problem = PENDING.with(|p| {
                let mut pending = p.borrow_mut();
                let pending = pending.as_mut().filter(|x| x.id == self.id)?;
                match pending.kind {
                    Kind::Manifest => {
                        if pending.body.len() + chunk.len() > MAX_MANIFEST_BYTES {
                            return Some("the manifest is too large".to_string());
                        }
                        pending.body.extend_from_slice(chunk);
                    }
                    Kind::Archive => {
                        pending.received += chunk.len() as u64;
                        if pending.received > MAX_ARCHIVE_BYTES {
                            return Some("the download is larger than any release".to_string());
                        }
                        pending.hasher.update(chunk);
                        if let Some(file) = pending.file.as_mut()
                            && let Err(e) = file.write_all(chunk)
                        {
                            return Some(format!("cannot write the download: {e}"));
                        }
                    }
                }
                None
            });
            if let Some(problem) = problem {
                fail(problem);
            }
        }

        fn on_request_complete(&self, request: Option<&mut Urlrequest>) {
            let status = match request {
                Some(r) if r.request_status() == UrlrequestStatus::SUCCESS => r.response().map(|resp| resp.status()).unwrap_or(0),
                _ => 0,
            };
            let Some(mut pending) = PENDING.with(|p| {
                let mut slot = p.borrow_mut();
                if slot.as_ref().is_some_and(|x| x.id == self.id) { slot.take() } else { None }
            }) else {
                return;
            };
            // The handle is dropped on its own turn: CEF is inside this callback.
            if let Some(handle) = pending.request.take() {
                task::post_ui(move || drop(handle));
            }
            if status != 200 {
                let what = if pending.kind == Kind::Manifest { "check" } else { "download" };
                if pending.kind == Kind::Archive {
                    let _ = fs::remove_file(&pending.path);
                }
                return fail(match status {
                    0 => format!("the update {what} could not reach GitHub"),
                    other => format!("the update {what} was answered with HTTP {other}"),
                });
            }
            match pending.kind {
                Kind::Manifest => {
                    let body = std::mem::take(&mut pending.body);
                    task::post_ui(move || on_manifest(body));
                }
                Kind::Archive => {
                    drop(pending.file.take()); // flush before it is read back
                    if pending.total != 0 && pending.received != pending.total {
                        let _ = fs::remove_file(&pending.path);
                        return fail(format!("the download is {} bytes, the release says {}", pending.received, pending.total));
                    }
                    let digest = pending.hasher.clone().hex();
                    let (version, path, expected, received) =
                        (pending.version.clone(), pending.path.clone(), pending.expected_sha256.clone(), pending.received);
                    task::post_ui(move || on_archive(version, path, digest, expected, received));
                }
            }
        }
    }
}

/// Drops anything in flight before `cef::shutdown()`.
pub fn clear() {
    let pending = PENDING.with(|p| p.borrow_mut().take());
    drop(pending);
}

#[cfg_attr(not(debug_assertions), allow(dead_code))] // reported by `debug.info` only
pub fn debug_snapshot() -> serde_json::Value {
    let (kind, id) = PENDING.with(|p| {
        p.borrow()
            .as_ref()
            .map(|x| (format!("{:?}", x.kind), x.id))
            .unwrap_or_else(|| ("none".to_string(), 0))
    });
    serde_json::json!({
        "allowed": updates_allowed(),
        "checked": CHECKED.get(),
        "pending": kind,
        "pendingId": id,
        "latest": LATEST.with(|l| l.borrow().as_ref().map(|m| m.version.clone())),
        "staged": STAGED.with(|s| s.borrow().as_ref().map(|p| p.display().to_string())),
        "platform": sta_core::update::platform_key(),
        "manifestUrl": MANIFEST_URL,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What `stage_archive` has to find in an unpacked release: the browser is one directory deep
    /// (`sta-<version>-<platform>/`), and on macOS one more (`sta.app/Contents/MacOS/`).
    #[test]
    fn the_payload_is_found_inside_the_archive_wrapper() {
        let root = std::env::temp_dir().join(format!("sta-payload-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let wrapper = root.join("sta-9.9.9-platform");
        let payload = if cfg!(target_os = "macos") { wrapper.join("sta.app") } else { wrapper.clone() };
        let exe = exe_in(&payload);
        fs::create_dir_all(exe.parent().unwrap()).unwrap();
        fs::write(&exe, b"binary").unwrap();

        assert_eq!(payload_root(&root), Some(payload.clone()));
        assert_eq!(payload_root(&wrapper), Some(payload.clone()));
        // A directory with nothing that looks like a browser in it.
        fs::remove_file(&exe).unwrap();
        assert_eq!(payload_root(&root), None);
        let _ = fs::remove_dir_all(&root);
    }
}
