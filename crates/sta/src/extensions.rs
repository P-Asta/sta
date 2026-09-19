//! The installed-extensions listing [owner: tabs] (ext design FINAL PLAN §4 "Listing"; ARCHITECTURE
//! §4.6).
//!
//! What the user sees in Ctrl+E and in Settings › Extensions is built here, **read-only**, from
//! three sources that all live in the profile:
//! - `Extensions/<id>/<version>/manifest.json` plus `_locales` ([`crate::extension_files`]);
//! - `--load-extension` directories (unpacked probes, debug builds and the e2e suite);
//! - `extensions.settings` / `extensions.commands` in `Secure Preferences` and `Preferences`, for
//!   what only Chromium knows: whether an extension is on, why it is off, and where it came from.
//!
//! Those preference files are MAC-protected and sta never writes them — turning an extension on or
//! off goes through Chromium's own `chrome://extensions` page ([`crate::ext_backend`]), which is the
//! only way to change that state without corrupting the profile.
//!
//! **Refresh** (FINAL PLAN §4): at startup, whenever the directory or either preferences file
//! changes (a 2 s modification-time poll, debounced by [`DEBOUNCE_MS`] — see the deviation note in
//! `gates-p3.md`: a poll of three `stat` calls costs less than a watcher thread and answers the same
//! question), after every backend operation, when the picker opens (`Effect::RefreshExtensions`) and
//! [`AFTER_INSTALL_MS`] after an install, because Chromium writes the preferences with a delay and a
//! just-installed extension is otherwise missing its state.
//!
//! **Icons** are served same-origin at `sta://command/__ext-icon/<id>/<px>` (and `sta://settings/…`):
//! a `sta://` page may not load `chrome-extension://` images. [`icon_response`] resolves the file
//! inside the extension's own directory — canonicalized, image types only — and runs on the IO
//! thread, where the scheme handler lives.
//!
//! Public API:
//! - `pub fn startup()`, `pub fn refresh()`, `pub fn refresh_soon(delay_ms: i64)`,
//!   `pub fn refresh_later(delay_ms: i64)`
//! - `pub fn list() -> Vec<ExtensionInfo>` (what the last refresh found)
//! - `pub fn note_state(id: &str, enabled: Option<bool>)` — an operation Chromium confirmed
//! - `pub fn note_installed(id: &str, external: bool)` — an extension was installed
//! - `pub fn icon_response(path: &str) -> Option<(&'static str, Vec<u8>)>` — IO thread
//! - `pub fn clear()`, `pub fn debug_snapshot() -> serde_json::Value`

use crate::extension_files::{self, ExtensionFiles, Source};
use crate::{controller, platform, task};
use serde_json::{Value, json};
use sta_core::extensions::{ExtensionBlock, ExtensionCommand, ExtensionInfo, ExtensionInstall, ExtensionState};
use sta_core::Command;
use std::cell::{Cell, RefCell};
use std::path::PathBuf;

/// How often the profile is checked for changes (three `stat` calls).
const POLL_MS: i64 = 2000;
/// A change is answered this long after it was seen, so a burst of writes costs one rebuild.
const DEBOUNCE_MS: i64 = 300;
/// Chromium writes `Secure Preferences` with a delay: an install is re-read once more after this
/// long, which is when its state and location are finally there (FINAL PLAN §4: "+11 s").
pub const AFTER_INSTALL_MS: i64 = 11_000;
/// Largest icon file sta will serve.
const MAX_ICON_BYTES: u64 = 512 * 1024;
/// When the profile is read again after launch, for the preferences Chromium writes late (the same
/// reason `foreign.rs` re-scans at 12 s).
const STARTUP_REREAD_MS: [i64; 2] = [3_000, 12_000];

/// `disable_reasons` bit Chromium sets for an extension another program registered
/// (`DISABLE_EXTERNAL_EXTENSION`, verified on this machine's profile: design report
/// `extensions.md`, and run12b). It is the one bit sta's behaviour depends on.
const REASON_EXTERNAL: i64 = 8192;
/// Reasons the user cannot undo from sta (Chromium's `disable_reason.h`). They decide that a row is
/// *blocked* rather than merely off; **which** of them it is decides what the row says
/// ([`block_of`]), because "your organization turned this off" is false for most of them.
const REASON_REQUIREMENT: i64 = 8; // UNSUPPORTED_REQUIREMENT
const REASON_NOT_VERIFIED: i64 = 64;
const REASON_GREYLIST: i64 = 128;
const REASON_CORRUPTED: i64 = 256;
const REASON_POLICY: i64 = 2048 | 4096; // BLOCKED_BY_POLICY | UPDATE_REQUIRED_BY_POLICY
const REASON_CUSTODIAN: i64 = 1 << 15;
const REASON_MANIFEST_VERSION: i64 = 1 << 17;
const REASONS_BLOCKED: i64 =
    REASON_REQUIREMENT | REASON_NOT_VERIFIED | REASON_GREYLIST | REASON_CORRUPTED | REASON_POLICY | REASON_CUSTODIAN | REASON_MANIFEST_VERSION;
/// How long an install the user accepted in this session is listed before its preferences arrive
/// (see [`FRESH`]). Comfortably past [`AFTER_INSTALL_MS`]: if Chromium has still written nothing by
/// then, sta genuinely does not know what that extension is, and guessing stops.
const FRESH_INSTALL_TTL_MS: u128 = 30_000;
/// How long a state Chromium confirmed survives a profile that still says otherwise. Chromium commits
/// `Secure Preferences` 8–12 s after the write (gate S8), and the operation's own re-reads land long
/// before that — without this the row flipped back to the old state for seconds (P3-E2E-2).
const PENDING_TTL_MS: u128 = 15_000;

thread_local! {
    static LIST: RefCell<Vec<ExtensionInfo>> = const { RefCell::new(Vec::new()) };
    /// Extensions removed in this session whose files are still on disk: a `--load-extension`
    /// directory stays where it is when Chromium uninstalls the extension, and listing it again
    /// would tell the user their removal did nothing. Forgotten as soon as the profile has a
    /// preferences entry for it again (an install).
    static REMOVED: RefCell<std::collections::HashSet<String>> = RefCell::new(std::collections::HashSet::new());
    /// States Chromium **confirmed** that the profile has not caught up with yet (`note_state`), each
    /// with the moment it was confirmed. A rebuild applies them on top of what the preferences say
    /// until the preferences agree or [`PENDING_TTL_MS`] passes — the same idea as `REMOVED`, for
    /// on/off instead of gone.
    static PENDING: RefCell<std::collections::HashMap<String, (ExtensionState, std::time::Instant)>> = RefCell::new(std::collections::HashMap::new());
    /// Extensions **the user installed in this session** whose preferences Chromium has not written
    /// yet. Chromium commits `Secure Preferences` ~11 s after the install, and until it does, [`one`]
    /// refuses to list an extension it can say nothing certain about — so the "· Ctrl+E" the install
    /// toast promises was false for the whole 2.5 s the toast was on screen and for eight seconds
    /// after it (measured: 10.8 s from toast to the row appearing). The one install sta *can* be
    /// certain about is this one: the user accepted Chromium's own "Add extension?" dialog, which
    /// installs it enabled, and `foreign.rs` reports it with `external: false`. Extensions another
    /// program registered are never in here — those really do start off, awaiting the user's OK
    /// (D6), and are exactly what the "do not guess" rule exists for. Dropped as soon as the
    /// preferences say something, or after [`FRESH_INSTALL_TTL_MS`].
    static FRESH: RefCell<std::collections::HashMap<String, std::time::Instant>> = RefCell::new(std::collections::HashMap::new());
    /// Modification times the poll last saw (`Extensions` dir, `Secure Preferences`, `Preferences`).
    static SEEN: Cell<[i64; 3]> = const { Cell::new([0, 0, 0]) };
    static POLL_SCHEDULED: Cell<bool> = const { Cell::new(false) };
    static REBUILD_SCHEDULED: Cell<bool> = const { Cell::new(false) };
    static REFRESHES: Cell<u64> = const { Cell::new(0) };
}

/// What the last refresh found (A–Z is core's job; this is manifest order).
pub fn list() -> Vec<ExtensionInfo> {
    LIST.with(|l| l.borrow().clone())
}

/// `on_context_initialized`: read the profile once and start watching it.
///
/// Chromium writes `Secure Preferences` for the extensions it installs at startup (the ones other
/// programs registered) a moment later, and an extension with no preferences entry is not listed yet
/// (see [`one`]) — so the first read is followed by two more.
pub fn startup() {
    refresh();
    refresh_later(STARTUP_REREAD_MS[0]);
    refresh_later(STARTUP_REREAD_MS[1]);
    schedule_poll();
}

/// Re-read the profile and push the result into core.
pub fn refresh() {
    let extensions = build();
    REFRESHES.set(REFRESHES.get() + 1);
    log_debug!("extensions: {} installed", extensions.len());
    LIST.with(|l| *l.borrow_mut() = extensions.clone());
    controller::dispatch(Command::ExtensionsChanged { extensions });
}

/// Refresh after `delay_ms`, **coalesced**: one pending rebuild at a time. For the paths that can
/// fire repeatedly — the watch's debounce, and opening the picker.
pub fn refresh_soon(delay_ms: i64) {
    if REBUILD_SCHEDULED.replace(true) {
        return;
    }
    task::post_ui_delayed(delay_ms.max(0), || {
        REBUILD_SCHEDULED.set(false);
        refresh();
    });
}

/// Refresh after `delay_ms`, **always**: for the two paths that need a *second, later* read because
/// Chromium writes the preferences after the fact — an install (+[`AFTER_INSTALL_MS`]) and a backend
/// operation. Coalescing those would drop exactly the read that carries the new state.
pub fn refresh_later(delay_ms: i64) {
    task::post_ui_delayed(delay_ms.max(0), refresh);
}

// ----------------------------------------------------------------------------------- watching

fn watched_paths() -> Option<[PathBuf; 3]> {
    let profile = extension_files::profile_dir()?;
    Some([profile.join("Extensions"), profile.join("Secure Preferences"), profile.join("Preferences")])
}

fn modified_ms(path: &std::path::Path) -> i64 {
    std::fs::metadata(path)
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn schedule_poll() {
    if POLL_SCHEDULED.replace(true) {
        return;
    }
    task::post_ui_delayed(POLL_MS, poll);
}

/// The watch: the `Extensions` directory and the two preference files. A change in any of them means
/// something was installed, removed, turned on or turned off — possibly by Chrome itself, or by this
/// profile's own backend window.
fn poll() {
    POLL_SCHEDULED.set(false);
    if crate::window::is_closing() {
        return;
    }
    if let Some(paths) = watched_paths() {
        let now = [modified_ms(&paths[0]), modified_ms(&paths[1]), modified_ms(&paths[2])];
        let previous = SEEN.replace(now);
        if previous != [0, 0, 0] && previous != now {
            log_debug!("extensions: the profile changed; re-reading");
            refresh_soon(DEBOUNCE_MS);
        }
    }
    schedule_poll();
}

// ----------------------------------------------------------------------------------- building

/// Every id the profile knows about: the `Extensions` directory, `--load-extension`, and the
/// preferences (an extension whose directory Chromium has already removed is gone, so ids that
/// resolve to no files at all are dropped).
fn build() -> Vec<ExtensionInfo> {
    let locale = platform::os_ui_locale();
    let prefs = extension_files::prefs_settings();
    let commands = extension_files::prefs_commands();
    let mut ids: Vec<String> = Vec::new();
    if let Some(dir) = extension_files::extensions_dir() {
        ids.extend(extension_files::scan(&dir).into_keys());
    }
    ids.extend(extension_files::command_line_extensions().into_iter().map(|(id, _)| id));
    ids.extend(prefs.keys().cloned());
    ids.sort();
    ids.dedup();
    // An extension the user removed stays gone for this session: the profile cannot be asked, because
    // Chromium commits the preferences up to ten seconds later and the *old* entry is still in the
    // file meanwhile (which brought the row straight back). An install of the same id clears the mark
    // ([`note_installed`]).
    ids.into_iter()
        .filter(|id| REMOVED.with(|r| !r.borrow().contains(id)))
        .filter_map(|id| one(&id, prefs.get(&id), commands.get(&id), &locale))
        .collect()
}

fn one(id: &str, prefs: Option<&Value>, commands: Option<&Vec<(String, String)>>, locale: &str) -> Option<ExtensionInfo> {
    let files = extension_files::find(id)?;
    // **Not listed until Chromium has written its preferences.** Only Chromium knows whether an
    // extension is on, and it writes that with a delay — so in the first seconds of a new profile
    // there are files on disk and nothing to say about them. Guessing "on" there would be a lie
    // exactly where it matters: Chromium installs the extensions *other programs registered* off,
    // awaiting the user's OK (D6), and their directories are written during those same first seconds.
    // A `--load-extension` extension is the one case sta knows without asking: Chromium always
    // enables it. Everything else appears at the next refresh (the watch, or the +11 s read after an
    // install), a second or two later.
    // A user install this session is the one exception: sta watched the user accept it, Chromium
    // installs it enabled, and `state_of(None, …)` says exactly that (see [`FRESH`]).
    if prefs.is_some() {
        FRESH.with(|f| f.borrow_mut().remove(id));
    }
    if prefs.is_none() && files.source != Source::CommandLine && !fresh_install(id) {
        return None;
    }
    let location = prefs
        .and_then(|p| p.get("location"))
        .and_then(Value::as_i64)
        .unwrap_or(match files.source {
            Source::CommandLine => 4,
            _ => 1,
        });
    if extension_files::is_component_location(location) {
        return None;
    }
    let install = install_of(location, prefs);
    let from_profile = state_of(prefs, install);
    // A state Chromium confirmed wins over a profile that has not committed it yet (P3-E2E-2).
    let state = pending_state(id, from_profile);
    let blocked = (state == ExtensionState::Blocked).then(|| block_of(disable_reasons(prefs), install));
    let name = files.name(locale);
    Some(ExtensionInfo {
        id: id.to_string(),
        name,
        short_name: files.short_name(locale),
        version: files.version(),
        description: files.description(locale),
        state,
        blocked,
        install,
        source_label: source_label(install, &files),
        popup: files.popup_page(),
        options: files.options_page(),
        side_panel: files.side_panel_page(),
        needs_current_tab: files.needs_current_tab(),
        commands: commands
            .map(|list| list.iter().map(|(name, shortcut)| ExtensionCommand { name: name.clone(), description: String::new(), shortcut: shortcut.clone() }).collect())
            .unwrap_or_default(),
    })
}

/// `disable_reasons`, which Chromium writes either as one bitmask or as a list of them.
fn disable_reasons(prefs: Option<&Value>) -> i64 {
    match prefs.and_then(|p| p.get("disable_reasons")) {
        Some(Value::Number(n)) => n.as_i64().unwrap_or(0),
        Some(Value::Array(list)) => list.iter().filter_map(Value::as_i64).fold(0, |acc, r| acc | r),
        _ => 0,
    }
}

fn state_of(prefs: Option<&Value>, install: ExtensionInstall) -> ExtensionState {
    let reasons = disable_reasons(prefs);
    // **Chromium 152 does not write `state` at all**: an extension is disabled exactly when it has a
    // disable reason, and the ones this machine's profile holds are lists — `[]` for the extensions
    // that run, `[1]` (user action) for one the user turned off, `[8192]` for one another program
    // added (measured: `C:/ast/tmp/ext-design/gates-p3.md`, S7). Older profiles still carry `state`,
    // where 0 means disabled, so it is honoured when it is there.
    let disabled_by_state = prefs.and_then(|p| p.get("state")).and_then(Value::as_i64).is_some_and(|s| s == 0);
    if reasons == 0 && !disabled_by_state {
        return ExtensionState::Enabled;
    }
    if reasons & REASON_EXTERNAL != 0 {
        return ExtensionState::NeedsApproval;
    }
    if reasons & REASONS_BLOCKED != 0 || install == ExtensionInstall::Managed {
        return ExtensionState::Blocked;
    }
    ExtensionState::Off
}

/// Which sentence a blocked row shows. Only the policy bits (and a policy install) may name an
/// organization: on Chromium 152 every MV2 extension is disabled with `UNSUPPORTED_MANIFEST_VERSION`
/// and a damaged profile yields `CORRUPTED`, neither of which has anything to do with an employer
/// (UXV-2). Most specific first.
fn block_of(reasons: i64, install: ExtensionInstall) -> ExtensionBlock {
    if reasons & REASON_POLICY != 0 || install == ExtensionInstall::Managed {
        ExtensionBlock::Policy
    } else if reasons & REASON_MANIFEST_VERSION != 0 {
        ExtensionBlock::Unsupported
    } else if reasons & REASON_CORRUPTED != 0 {
        ExtensionBlock::Damaged
    } else if reasons & (REASON_GREYLIST | REASON_NOT_VERIFIED) != 0 {
        ExtensionBlock::Safety
    } else if reasons & REASON_REQUIREMENT != 0 {
        ExtensionBlock::Requirement
    } else if reasons & REASON_CUSTODIAN != 0 {
        ExtensionBlock::Custodian
    } else {
        ExtensionBlock::Unknown
    }
}

/// What a rebuild does with a pending state: `Some` overrides the profile, `None` means the entry is
/// spent (the profile agrees now, or it waited long enough).
fn pending_verdict(pending: ExtensionState, from_profile: ExtensionState, age_ms: u128) -> Option<ExtensionState> {
    (age_ms < PENDING_TTL_MS && from_profile != pending).then_some(pending)
}

/// Applies (and prunes) [`PENDING`] for one id.
fn pending_state(id: &str, from_profile: ExtensionState) -> ExtensionState {
    PENDING.with(|p| {
        let mut pending = p.borrow_mut();
        let Some((state, since)) = pending.get(id).copied() else { return from_profile };
        match pending_verdict(state, from_profile, since.elapsed().as_millis()) {
            Some(state) => state,
            None => {
                pending.remove(id);
                from_profile
            }
        }
    })
}

/// Chromium's `ManifestLocation` → what the user may do with it. The one distinction that matters
/// (R-SEC-2) is *store* vs *file*: an external extension Chromium downloads from the Web Store can
/// be turned on after its warnings, one another program dropped on the disk as a CRX can only be
/// removed, because sta cannot show the user where that code came from.
fn install_of(location: i64, prefs: Option<&Value>) -> ExtensionInstall {
    let from_store = prefs.and_then(|p| p.get("from_webstore")).and_then(Value::as_bool).unwrap_or(false);
    match location {
        4 | 8 => ExtensionInstall::Unpacked,
        // EXTERNAL_PREF_DOWNLOAD: registered with an update URL, so its code comes from the store.
        6 => ExtensionInstall::ExternalStore,
        // EXTERNAL_PREF / EXTERNAL_REGISTRY: a path on this machine unless Chromium recorded the
        // Web Store as its origin.
        2 | 3 if from_store => ExtensionInstall::ExternalStore,
        2 | 3 => ExtensionInstall::ExternalLocal,
        l if extension_files::is_policy_location(l) => ExtensionInstall::Managed,
        _ => ExtensionInstall::WebStore,
    }
}

fn source_label(install: ExtensionInstall, files: &ExtensionFiles) -> String {
    match install {
        ExtensionInstall::WebStore => "Chrome Web Store".into(),
        ExtensionInstall::Unpacked => format!("Loaded from {}", files.dir.display()),
        ExtensionInstall::ExternalStore => "Added by another program · Chrome Web Store".into(),
        // **No path.** The path here is sta's own profile copy, which means nothing to a person, is
        // the longest string on the Settings page, and directly contradicts the sentence printed
        // right under it ("sta cannot show you where that code came from"). The directory is still
        // in the log for anyone who needs it.
        ExtensionInstall::ExternalLocal => "Added by another program · from a file on this computer".into(),
        ExtensionInstall::Managed => "Installed by your organization".into(),
    }
}

// -------------------------------------------------------------------- what an operation confirmed

/// An operation Chromium **confirmed** (`ext_backend`): the cached listing is patched and pushed at
/// once, instead of waiting for the profile to say the same thing.
///
/// Chromium commits `Secure Preferences` up to ten seconds later (`JsonPrefStore`'s commit interval;
/// measured at 8–12 s in gate S7), and until then the row would still read "On" after the user turned
/// it off. This is not a guess: `chrome.management.setEnabled` / `uninstall` came back without an
/// error, so that *is* the state — and the profile re-reads that follow will correct it if Chromium
/// ever disagrees.
///
/// Patching the cached list is not enough on its own: the operation's own re-reads (300 ms and 1.5 s)
/// rebuild from those same uncommitted preferences, which flipped the row back for 1–11 s. The state
/// is therefore also remembered in [`PENDING`], which every rebuild applies until the profile agrees
/// (or [`PENDING_TTL_MS`] passes).
pub fn note_state(id: &str, enabled: Option<bool>) {
    let changed = LIST.with(|l| {
        let mut list = l.borrow_mut();
        match enabled {
            Some(enabled) => {
                REMOVED.with(|r| r.borrow_mut().remove(id));
                let state = if enabled { ExtensionState::Enabled } else { ExtensionState::Off };
                PENDING.with(|p| p.borrow_mut().insert(id.to_string(), (state, std::time::Instant::now())));
                let Some(entry) = list.iter_mut().find(|e| e.id == id) else { return false };
                let changed = entry.state != state;
                entry.state = state;
                entry.blocked = None;
                changed
            }
            None => {
                REMOVED.with(|r| r.borrow_mut().insert(id.to_string()));
                PENDING.with(|p| p.borrow_mut().remove(id));
                let before = list.len();
                list.retain(|e| e.id != id);
                list.len() != before
            }
        }
    });
    if changed {
        controller::dispatch(Command::ExtensionsChanged { extensions: list() });
    }
}

/// An extension was installed (`foreign.rs` saw the post-install window): if it had been removed in
/// this session, it is back. A **user** install (`external == false`) is also listed right away,
/// before Chromium writes its preferences (see [`FRESH`]).
pub fn note_installed(id: &str, external: bool) {
    PENDING.with(|p| p.borrow_mut().remove(id));
    if !external {
        FRESH.with(|f| f.borrow_mut().insert(id.to_string(), std::time::Instant::now()));
    }
    if REMOVED.with(|r| r.borrow_mut().remove(id)) {
        log_debug!("extensions: {id} was installed again after being removed");
    }
}

/// This id is a user install of this session that Chromium has not written preferences for yet
/// (and prunes the mark once it has aged out).
fn fresh_install(id: &str) -> bool {
    FRESH.with(|f| {
        let mut fresh = f.borrow_mut();
        let Some(since) = fresh.get(id).copied() else { return false };
        if since.elapsed().as_millis() >= FRESH_INSTALL_TTL_MS {
            fresh.remove(id);
            return false;
        }
        true
    })
}

// ----------------------------------------------------------------------------------- icons

/// `__ext-icon/<id>/<px>` → the icon's bytes, for the `sta://` scheme handler (IO thread). `None`
/// leaves the request a 404: a bad id, an extension that is gone, or a file that is not an image
/// inside that extension's own directory.
pub fn icon_response(path: &str) -> Option<(&'static str, Vec<u8>)> {
    let rest = path.strip_prefix(sta_core::extensions::ICON_PATH_PREFIX)?;
    let mut parts = rest.split('/');
    let id = parts.next()?;
    let px: u32 = parts.next()?.parse().ok()?;
    if parts.next().is_some() || !extension_files::is_extension_id(id) || !(1..=512).contains(&px) {
        return None;
    }
    let file = extension_files::find(id)?.icon_path(px)?;
    if std::fs::metadata(&file).ok()?.len() > MAX_ICON_BYTES {
        return None;
    }
    let bytes = std::fs::read(&file).ok()?;
    Some((crate::scheme::mime_for(&file.to_string_lossy()), bytes))
}

// ----------------------------------------------------------------------------------- teardown

pub fn clear() {
    LIST.with(|l| l.borrow_mut().clear());
    REMOVED.with(|r| r.borrow_mut().clear());
    PENDING.with(|p| p.borrow_mut().clear());
    FRESH.with(|f| f.borrow_mut().clear());
}

#[cfg_attr(not(debug_assertions), allow(dead_code))] // debug.rs only
pub fn debug_snapshot() -> Value {
    let list = list();
    json!({
        "refreshes": REFRESHES.get(),
        "count": list.len(),
        "pending": PENDING.with(|p| p.borrow().iter().map(|(id, (state, since))| json!({ "id": id, "state": format!("{state:?}"), "ageMs": since.elapsed().as_millis() as u64 })).collect::<Vec<_>>()),
        "extensions": list.iter().map(|e| json!({
            "id": e.id,
            "name": e.name,
            "state": format!("{:?}", e.state),
            "blocked": e.blocked.map(|b| format!("{b:?}")),
            "install": format!("{:?}", e.install),
            "popup": e.popup,
            "options": e.options,
            "commands": e.commands.iter().map(|c| format!("{}={}", c.name, c.shortcut)).collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
        "seen": SEEN.get(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prefs(json: &str) -> Value {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn preferences_decide_on_off_and_needs_your_ok() {
        // What a Chromium 152 profile actually holds: no `state` key, `disable_reasons` as a list
        // (the entries measured on this machine in gate S7).
        let external = prefs(r#"{"disable_reasons":[8192],"location":3}"#);
        assert_eq!(state_of(Some(&external), ExtensionInstall::ExternalLocal), ExtensionState::NeedsApproval);
        let off = prefs(r#"{"disable_reasons":[1],"location":8}"#);
        assert_eq!(state_of(Some(&off), ExtensionInstall::Unpacked), ExtensionState::Off);
        let running = prefs(r#"{"disable_reasons":[],"location":8}"#);
        assert_eq!(state_of(Some(&running), ExtensionInstall::Unpacked), ExtensionState::Enabled);
        // Older profiles still carry `state` (0 = disabled) and a bitmask.
        let old_external = prefs(r#"{"state":0,"disable_reasons":8192,"location":3}"#);
        assert_eq!(state_of(Some(&old_external), ExtensionInstall::ExternalLocal), ExtensionState::NeedsApproval);
        let blocked = prefs(r#"{"state":0,"disable_reasons":2048,"location":1}"#);
        assert_eq!(state_of(Some(&blocked), ExtensionInstall::WebStore), ExtensionState::Blocked);
        let on = prefs(r#"{"state":1,"location":1}"#);
        assert_eq!(state_of(Some(&on), ExtensionInstall::WebStore), ExtensionState::Enabled);
        // A `state` of 0 with no reason at all is still off (a profile mid-write).
        let odd = prefs(r#"{"state":0,"location":1}"#);
        assert_eq!(state_of(Some(&odd), ExtensionInstall::WebStore), ExtensionState::Off);
        // No entry at all: only reachable for a `--load-extension` extension (see `one`), which
        // Chromium always enables.
        assert_eq!(state_of(None, ExtensionInstall::Unpacked), ExtensionState::Enabled);
        // A policy install is never "just off".
        let policy = prefs(r#"{"disable_reasons":[1],"location":7}"#);
        assert_eq!(state_of(Some(&policy), ExtensionInstall::Managed), ExtensionState::Blocked);
    }

    #[test]
    fn locations_separate_store_installs_from_files_on_disk() {
        let plain = prefs("{}");
        let store = prefs(r#"{"from_webstore":true}"#);
        assert_eq!(install_of(1, Some(&plain)), ExtensionInstall::WebStore);
        assert_eq!(install_of(4, Some(&plain)), ExtensionInstall::Unpacked);
        assert_eq!(install_of(8, Some(&plain)), ExtensionInstall::Unpacked);
        assert_eq!(install_of(6, Some(&plain)), ExtensionInstall::ExternalStore);
        assert_eq!(install_of(3, Some(&plain)), ExtensionInstall::ExternalLocal);
        assert_eq!(install_of(2, Some(&plain)), ExtensionInstall::ExternalLocal);
        assert_eq!(install_of(3, Some(&store)), ExtensionInstall::ExternalStore);
        assert_eq!(install_of(7, Some(&plain)), ExtensionInstall::Managed);
        assert_eq!(install_of(9, Some(&plain)), ExtensionInstall::Managed);
        assert!(extension_files::is_component_location(5) && extension_files::is_component_location(10));
    }

    /// UXV-2: the reason decides the sentence, and only a policy block may name an organization.
    #[test]
    fn each_blocked_reason_maps_to_its_own_kind() {
        assert_eq!(block_of(2048, ExtensionInstall::WebStore), ExtensionBlock::Policy);
        assert_eq!(block_of(4096, ExtensionInstall::WebStore), ExtensionBlock::Policy);
        assert_eq!(block_of(0, ExtensionInstall::Managed), ExtensionBlock::Policy);
        assert_eq!(block_of(1 << 17, ExtensionInstall::WebStore), ExtensionBlock::Unsupported);
        assert_eq!(block_of(256, ExtensionInstall::WebStore), ExtensionBlock::Damaged);
        assert_eq!(block_of(128, ExtensionInstall::WebStore), ExtensionBlock::Safety);
        assert_eq!(block_of(64, ExtensionInstall::WebStore), ExtensionBlock::Safety);
        assert_eq!(block_of(8, ExtensionInstall::WebStore), ExtensionBlock::Requirement);
        assert_eq!(block_of(1 << 15, ExtensionInstall::WebStore), ExtensionBlock::Custodian);
        assert_eq!(block_of(0, ExtensionInstall::WebStore), ExtensionBlock::Unknown);
        // Policy wins over everything else: it is the one the user really cannot undo here.
        assert_eq!(block_of(2048 | 256 | (1 << 17), ExtensionInstall::WebStore), ExtensionBlock::Policy);
    }

    /// P3-E2E-2: a state Chromium confirmed survives the re-reads of a profile that still says the
    /// old thing, and is dropped as soon as the profile agrees (or after the timeout).
    #[test]
    fn a_confirmed_state_outlives_the_uncommitted_profile() {
        // The profile still says "on" right after the user turned it off: the confirmed state wins.
        assert_eq!(pending_verdict(ExtensionState::Off, ExtensionState::Enabled, 300), Some(ExtensionState::Off));
        // Including a flash back to "needs your OK" for an extension the user just allowed.
        assert_eq!(pending_verdict(ExtensionState::Enabled, ExtensionState::NeedsApproval, 1500), Some(ExtensionState::Enabled));
        // The profile caught up: the entry is spent.
        assert_eq!(pending_verdict(ExtensionState::Off, ExtensionState::Off, 1500), None);
        // And it never masks the profile forever — after the timeout the profile is the truth again.
        assert_eq!(pending_verdict(ExtensionState::Off, ExtensionState::Enabled, PENDING_TTL_MS), None);
    }

    /// The icon route is the one place a `sta://` page reaches into an extension's directory, so
    /// everything that is not exactly `__ext-icon/<valid id>/<size>` has to be refused before any
    /// file is touched.
    #[test]
    fn the_icon_route_only_accepts_an_id_and_a_size() {
        let id = "a".repeat(32);
        for bad in [
            "".to_string(),
            "other/thing".to_string(),
            format!("__ext-icon/{id}"),
            format!("__ext-icon/{id}/32/extra"),
            format!("__ext-icon/{id}/0"),
            format!("__ext-icon/{id}/9999"),
            format!("__ext-icon/{id}/px"),
            "__ext-icon/../../secret/16".to_string(),
            "__ext-icon/ABCDEFGHIJKLMNOPABCDEFGHIJKLMNOP/16".to_string(),
        ] {
            assert!(icon_response(&bad).is_none(), "{bad}");
        }
    }
}
