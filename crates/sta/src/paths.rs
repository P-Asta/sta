//! Data directories [skeleton, frozen].
//!
//! Responsibility: resolve and create the per-user data directory tree once, in the browser
//! process, right after `execute_process` returned -1 (never in subprocesses).
//!
//! ```text
//! <base>/                 %LOCALAPPDATA%\sta (release) | %LOCALAPPDATA%\sta Dev (debug)
//!   User Data/            Settings.root_cache_path == cache_path (Chromium profile, singleton lock)
//!   Logs/                 cef.log, sta.log, panic.log
//!   sta/                  state.json, history.json (sta's own profile data)
//! ```
//!
//! Override `<base>` with `--sta-data-dir=<path>` (first) or `STA_DATA_DIR`. Tests use
//! it so they never touch the user's profile or collide with the process singleton.
//!
//! Folders from before the rename (`sta_core::legacy`, docs/ARCHITECTURE.md §3) are migrated
//! before anything uses the directories:
//! - for the default folder, a legacy default folder is moved to the new default name when the new
//!   one doesn't exist yet (a rename in `%LOCALAPPDATA%`, so nothing is copied). An existing new
//!   folder is never merged with a legacy one; the legacy folder then stays untouched. An override
//!   that names the default folder (a launcher passing it explicitly) counts as the default folder;
//! - in `<base>` (default or override), a legacy profile subfolder is renamed to `sta/` the same
//!   way;
//! - a legacy folder held by a running older browser (its Chromium singleton `lockfile` is open) is
//!   neither moved nor used: `resolve` fails with [`ResolveError::LegacyInUse`] and `main` tells
//!   the user to close it (sharing its profile would hand this launch's URLs to the old browser);
//! - a move that fails for another reason (retried for about a second first) leaves the legacy
//!   folder in use where it is for this run; the next launch tries again. That run holds
//!   [`IN_PLACE_LOCK_FILE`] in the folder, so a launch while it runs knows the folder's holder is sta
//!   and uses it in place too: the process singleton then hands its command line to that run.
//!
//! Public API:
//! - `pub struct AppDirs { base, user_data, logs, profile }`
//! - `pub fn resolve() -> Result<(AppDirs, Vec<Note>), ResolveError>` (migrates, creates the directories)
//! - `pub fn init(dirs: AppDirs) -> &'static AppDirs`
//! - `pub fn dirs() -> &'static AppDirs` (panics before `init`) / `pub fn try_dirs()`

use std::io;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

use sta_core::legacy::{self, AfterFailedMove, LegacyHolder, MovePlan};

#[derive(Debug, Clone)]
pub struct AppDirs {
    /// Root of everything sta writes.
    pub base: PathBuf,
    /// Chromium user data (`Settings.root_cache_path` and `cache_path`).
    pub user_data: PathBuf,
    /// Log files.
    pub logs: PathBuf,
    /// sta state (`state.json`, `history.json`).
    pub profile: PathBuf,
}

static DIRS: OnceLock<AppDirs> = OnceLock::new();
/// Held for the whole run while a legacy folder is used in place (see [`IN_PLACE_LOCK_FILE`]).
static IN_PLACE_LOCK: OnceLock<std::fs::File> = OnceLock::new();

/// Command-line switch that overrides the data directory.
pub const DATA_DIR_SWITCH: &str = "--sta-data-dir=";
/// Environment variable that overrides the data directory.
pub const DATA_DIR_ENV: &str = "STA_DATA_DIR";
/// Default data folder under `%LOCALAPPDATA%`.
pub const DEFAULT_DIR_RELEASE: &str = "sta";
pub const DEFAULT_DIR_DEBUG: &str = "sta Dev";
/// Profile subfolder of `<base>`.
pub const PROFILE_DIR: &str = "sta";
/// Chromium user data subfolder of `<base>`.
const USER_DATA_DIR: &str = "User Data";
/// Chromium's process singleton keeps this file in the user data directory open while a browser
/// runs with it (share-read only, deleted on close).
const SINGLETON_LOCK_FILE: &str = "lockfile";
/// A sta that uses a legacy folder in place holds this file inside it the same way
/// (`platform::hold_lock_file`), so a launch that finds the folder busy can tell sta from an old
/// browser.
pub const IN_PLACE_LOCK_FILE: &str = "sta-in-place.lock";
/// Attempts of a legacy folder move (antivirus or indexer handles are short-lived).
const MOVE_ATTEMPTS: u32 = 5;
const MOVE_RETRY_DELAY: Duration = Duration::from_millis(200);

/// Why the data directories are not usable.
#[derive(Debug)]
pub enum ResolveError {
    Io(io::Error),
    /// A running older browser holds the legacy folder that should become `new`.
    LegacyInUse { legacy: PathBuf, new: PathBuf },
}

impl From<io::Error> for ResolveError {
    fn from(e: io::Error) -> Self {
        ResolveError::Io(e)
    }
}

/// What `resolve` did about folders from before the rename, for the log (which only exists once
/// the directories are resolved).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Note {
    Moved { from: PathBuf, to: PathBuf },
    /// The move failed: the legacy folder is used where it is for this run.
    UsingLegacyInPlace { path: PathBuf, error: String },
    /// A running sta uses the legacy folder in place: this launch uses it too (and is normally
    /// handed to that run by the process singleton).
    SharingLegacyInPlace { path: PathBuf },
    /// Both exist: the new one is used, the legacy one is left alone.
    LegacyLeftAlone { path: PathBuf },
}

/// Resolves the directories (migrating legacy folders first) and creates them.
pub fn resolve() -> Result<(AppDirs, Vec<Note>), ResolveError> {
    let from_arg = std::env::args_os().find_map(|a| {
        a.to_str()?.strip_prefix(DATA_DIR_SWITCH).filter(|s| !s.is_empty()).map(PathBuf::from)
    });
    let from_env = || std::env::var_os(DATA_DIR_ENV).filter(|v| !v.is_empty()).map(PathBuf::from);
    let local_app_data = std::env::var_os("LOCALAPPDATA").filter(|v| !v.is_empty()).map(PathBuf::from);
    let (dirs, notes) = resolve_with(from_arg.or_else(from_env), local_app_data, cfg!(debug_assertions))?;
    for note in &notes {
        if let Note::UsingLegacyInPlace { path, .. } = note {
            // Fails only when a concurrent sta that uses the folder in place holds it already.
            if let Ok(lock) = crate::platform::hold_lock_file(&path.join(IN_PLACE_LOCK_FILE)) {
                let _ = IN_PLACE_LOCK.set(lock);
            }
        }
    }
    Ok((dirs, notes))
}

/// [`resolve`] without the process environment: `explicit` data dir override, `%LOCALAPPDATA%`,
/// build flavor.
fn resolve_with(explicit: Option<PathBuf>, local_app_data: Option<PathBuf>, debug: bool) -> Result<(AppDirs, Vec<Note>), ResolveError> {
    let mut notes = Vec::new();
    let default = local_app_data.map(|local| {
        let new = local.join(if debug { DEFAULT_DIR_DEBUG } else { DEFAULT_DIR_RELEASE });
        (new, local.join(legacy::data_dir_name(debug)))
    });
    // root_cache_path must be absolute.
    let explicit = explicit.map(std::path::absolute).transpose()?;
    let base = match (explicit, default) {
        // The default folder passed explicitly (e.g. by a launcher) migrates like the default. The
        // legacy data folder is in use when its Chromium profile is.
        (Some(dir), Some((new, old))) if same_folder(&dir, &new) => migrate(&new, &old, &old.join(USER_DATA_DIR), &mut notes)?,
        (Some(dir), _) => dir,
        (None, Some((new, old))) => migrate(&new, &old, &old.join(USER_DATA_DIR), &mut notes)?,
        (None, None) => return Err(io::Error::other("LOCALAPPDATA is not set").into()),
    };
    let (new_profile, old_profile) = (base.join(PROFILE_DIR), base.join(legacy::PROFILE_DIR));
    let profile = if matches!(notes.last(), Some(Note::UsingLegacyInPlace { .. } | Note::SharingLegacyInPlace { .. })) {
        // A legacy data folder used in place keeps its layout (the profile subfolder isn't renamed
        // either), so it changes as little as possible until its move succeeds. Its saved
        // internal-page URLs are still upgraded to `sta://` when sta saves (`Store::load`).
        if !new_profile.exists() && old_profile.exists() { old_profile } else { new_profile }
    } else {
        // The profile subfolder is in use when `<base>`'s Chromium profile is.
        migrate(&new_profile, &old_profile, &base.join(USER_DATA_DIR), &mut notes)?
    };
    let dirs = AppDirs { user_data: base.join(USER_DATA_DIR), logs: base.join("Logs"), profile, base };
    for d in [&dirs.user_data, &dirs.logs, &dirs.profile] {
        std::fs::create_dir_all(d)?;
    }
    Ok((dirs, notes))
}

/// The same folder on Windows' case-insensitive file systems (separator style and trailing
/// separators ignored).
fn same_folder(a: &Path, b: &Path) -> bool {
    let normalized = |p: &Path| {
        let p = std::path::absolute(p).unwrap_or_else(|_| p.to_path_buf());
        p.to_string_lossy().replace('/', "\\").trim_end_matches('\\').to_lowercase()
    };
    normalized(a) == normalized(b)
}

/// Chooses between `new` and its legacy counterpart `old` (moving `old` to `new` when that's
/// safe) and returns the folder to use. `busy_profile`: the Chromium user data directory whose
/// singleton lock tells whether a running browser uses `old`.
fn migrate(new: &Path, old: &Path, busy_profile: &Path, notes: &mut Vec<Note>) -> Result<PathBuf, ResolveError> {
    // Case-insensitive file systems: the legacy name may be the new one (never the case today).
    if new.as_os_str().eq_ignore_ascii_case(old.as_os_str()) {
        return Ok(new.to_path_buf());
    }
    let refuse = || ResolveError::LegacyInUse { legacy: old.to_path_buf(), new: new.to_path_buf() };
    match legacy::plan_move(new.exists(), old.exists(), holder(old, busy_profile)) {
        MovePlan::UseNew => {
            if new.exists() && old.exists() {
                notes.push(Note::LegacyLeftAlone { path: old.to_path_buf() });
            }
            Ok(new.to_path_buf())
        }
        MovePlan::Refuse => Err(refuse()),
        MovePlan::ShareInPlace => {
            notes.push(Note::SharingLegacyInPlace { path: old.to_path_buf() });
            Ok(old.to_path_buf())
        }
        MovePlan::Move => {
            let mut result = crate::platform::move_dir(old, new);
            for _ in 1..MOVE_ATTEMPTS {
                if result.is_ok() || new.exists() {
                    break;
                }
                std::thread::sleep(MOVE_RETRY_DELAY);
                result = crate::platform::move_dir(old, new);
            }
            match result {
                Ok(()) => {
                    notes.push(Note::Moved { from: old.to_path_buf(), to: new.to_path_buf() });
                    Ok(new.to_path_buf())
                }
                Err(e) => match legacy::after_failed_move(new.exists(), old.exists(), holder(old, busy_profile)) {
                    AfterFailedMove::UseNew => Ok(new.to_path_buf()),
                    AfterFailedMove::Refuse => Err(refuse()),
                    AfterFailedMove::UseLegacyInPlace => {
                        notes.push(Note::UsingLegacyInPlace { path: old.to_path_buf(), error: e.to_string() });
                        Ok(old.to_path_buf())
                    }
                },
            }
        }
    }
}

/// Who uses the legacy folder `old`, whose Chromium profile is `busy_profile`: nobody (the profile
/// isn't in use), a sta that uses `old` in place (its in-place lock is held too), or else an older
/// browser.
fn holder(old: &Path, busy_profile: &Path) -> LegacyHolder {
    if !old.exists() || !profile_in_use(busy_profile) {
        LegacyHolder::Nobody
    } else if lock_held(&old.join(IN_PLACE_LOCK_FILE)) {
        LegacyHolder::StaInPlace
    } else {
        LegacyHolder::OldBrowser
    }
}

/// A running browser holds the Chromium profile in `user_data`: its singleton lock file is held.
pub fn profile_in_use(user_data: &Path) -> bool {
    lock_held(&user_data.join(SINGLETON_LOCK_FILE))
}

/// `lock` exists and can't be opened for writing: a live process holds it (shared for reading
/// only).
fn lock_held(lock: &Path) -> bool {
    if !lock.exists() {
        return false;
    }
    match std::fs::OpenOptions::new().write(true).open(lock) {
        Ok(_) => false,
        // Sharing violation (32) while it's open; access denied while its deletion is pending.
        Err(e) => e.raw_os_error() == Some(32) || e.kind() == io::ErrorKind::PermissionDenied,
    }
}

/// Stores the resolved directories for the lifetime of the process.
pub fn init(dirs: AppDirs) -> &'static AppDirs {
    DIRS.get_or_init(|| dirs)
}

/// The resolved directories. Panics if [`init`] was not called (browser process bug).
pub fn dirs() -> &'static AppDirs {
    DIRS.get().expect("paths::init must run before paths::dirs")
}

/// The resolved directories, or `None` in subprocesses.
pub fn try_dirs() -> Option<&'static AppDirs> {
    DIRS.get()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("sta-paths-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Holds `user_data`'s singleton lock the way Chromium does: write access, shared for reading
    /// only.
    #[cfg(windows)]
    fn hold_singleton_lock(user_data: &Path) -> std::fs::File {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_SHARE_READ: u32 = 1;
        std::fs::create_dir_all(user_data).unwrap();
        std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .share_mode(FILE_SHARE_READ)
            .open(user_data.join(SINGLETON_LOCK_FILE))
            .unwrap()
    }

    #[test]
    fn migrate_moves_a_legacy_folder_with_its_contents() {
        let root = temp("move");
        let (new, old) = (root.join("new"), root.join("old"));
        std::fs::create_dir_all(old.join("User Data")).unwrap();
        std::fs::write(old.join("state.json"), "{}").unwrap();
        let mut notes = Vec::new();
        let used = migrate(&new, &old, &old.join("User Data"), &mut notes).unwrap();
        assert_eq!(used, new);
        assert!(!old.exists());
        assert_eq!(std::fs::read_to_string(new.join("state.json")).unwrap(), "{}");
        assert_eq!(notes, vec![Note::Moved { from: old.clone(), to: new.clone() }]);
        // Second launch: nothing left to do.
        let mut notes = Vec::new();
        assert_eq!(migrate(&new, &old, &old.join("User Data"), &mut notes).unwrap(), new);
        assert!(notes.is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn migrate_never_merges_into_an_existing_new_folder() {
        let root = temp("both");
        let (new, old) = (root.join("new"), root.join("old"));
        std::fs::create_dir_all(&new).unwrap();
        std::fs::create_dir_all(&old).unwrap();
        std::fs::write(old.join("state.json"), "old").unwrap();
        let mut notes = Vec::new();
        assert_eq!(migrate(&new, &old, &old.join("User Data"), &mut notes).unwrap(), new);
        assert_eq!(std::fs::read_to_string(old.join("state.json")).unwrap(), "old", "legacy folder untouched");
        assert!(!new.join("state.json").exists());
        assert_eq!(notes, vec![Note::LegacyLeftAlone { path: old.clone() }]);
        // Neither exists: a fresh start in the new folder, nothing created here.
        let fresh = root.join("fresh");
        assert_eq!(migrate(&fresh, &root.join("none"), &root.join("none"), &mut Vec::new()).unwrap(), fresh);
        assert!(!fresh.exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn migrate_refuses_a_legacy_folder_held_by_a_running_browser() {
        let root = temp("busy");
        let (new, old) = (root.join("new"), root.join("old"));
        let user_data = old.join("User Data");
        std::fs::create_dir_all(&user_data).unwrap();
        assert!(!profile_in_use(&user_data), "no lock file");
        // A stale lock file (not open) doesn't count.
        std::fs::write(user_data.join(SINGLETON_LOCK_FILE), "").unwrap();
        assert!(!profile_in_use(&user_data), "stale lock file");
        std::fs::remove_file(user_data.join(SINGLETON_LOCK_FILE)).unwrap();
        #[cfg(windows)]
        {
            let held = hold_singleton_lock(&user_data);
            assert!(profile_in_use(&user_data));
            assert_eq!(holder(&old, &user_data), LegacyHolder::OldBrowser);
            match migrate(&new, &old, &user_data, &mut Vec::new()) {
                Err(ResolveError::LegacyInUse { legacy, new: target }) => assert_eq!((legacy, target), (old.clone(), new.clone())),
                other => panic!("{other:?}"),
            }
            assert!(old.exists() && !new.exists(), "nothing moved or created");
            drop(held);
            assert!(!profile_in_use(&user_data));
        }
        assert_eq!(migrate(&new, &old, &user_data, &mut Vec::new()).unwrap(), new);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[cfg(windows)]
    #[test]
    fn migrate_uses_the_legacy_folder_in_place_when_the_move_fails() {
        let root = temp("stuck");
        let (new, old) = (root.join("new"), root.join("old"));
        std::fs::create_dir_all(&old).unwrap();
        // An open file inside a directory makes renaming the directory fail on Windows.
        let open = std::fs::File::create(old.join("held.txt")).unwrap();
        let mut notes = Vec::new();
        let used = migrate(&new, &old, &old.join("User Data"), &mut notes).unwrap();
        assert_eq!(used, old);
        assert!(!new.exists());
        assert!(matches!(&notes[..], [Note::UsingLegacyInPlace { path, .. }] if *path == old), "{notes:?}");
        drop(open);
        assert_eq!(migrate(&new, &old, &old.join("User Data"), &mut Vec::new()).unwrap(), new, "retried next launch");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[cfg(windows)]
    #[test]
    fn migrate_shares_a_legacy_folder_that_sta_uses_in_place() {
        let root = temp("shared");
        let (new, old) = (root.join("new"), root.join("old"));
        let user_data = old.join("User Data");
        // A running sta using `old` in place: its Chromium profile and its in-place lock are held.
        let singleton = hold_singleton_lock(&user_data);
        let in_place = crate::platform::hold_lock_file(&old.join(IN_PLACE_LOCK_FILE)).unwrap();
        assert!(crate::platform::hold_lock_file(&old.join(IN_PLACE_LOCK_FILE)).is_err(), "held once");
        assert_eq!(holder(&old, &user_data), LegacyHolder::StaInPlace);
        let mut notes = Vec::new();
        assert_eq!(migrate(&new, &old, &user_data, &mut notes).unwrap(), old);
        assert_eq!(notes, vec![Note::SharingLegacyInPlace { path: old.clone() }]);
        assert!(old.exists() && !new.exists(), "nothing moved or created");
        // The in-place lock disappears with its handle; a profile held without it is an old browser's.
        drop(in_place);
        assert!(!old.join(IN_PLACE_LOCK_FILE).exists(), "deleted on close");
        assert!(matches!(migrate(&new, &old, &user_data, &mut Vec::new()), Err(ResolveError::LegacyInUse { .. })));
        drop(singleton);
        assert_eq!(migrate(&new, &old, &user_data, &mut Vec::new()).unwrap(), new);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn an_override_that_names_the_default_folder_migrates_like_the_default() {
        let root = temp("override");
        let local = root.join("LocalAppData");
        let legacy_base = local.join(legacy::data_dir_name(true));
        std::fs::create_dir_all(legacy_base.join(legacy::PROFILE_DIR)).unwrap();
        std::fs::write(legacy_base.join(legacy::PROFILE_DIR).join("state.json"), "{}").unwrap();
        let new_base = local.join(DEFAULT_DIR_DEBUG);

        // Another override: no migration of the default folder.
        let other = root.join("other");
        let (dirs, notes) = resolve_with(Some(other.clone()), Some(local.clone()), true).unwrap();
        assert_eq!((dirs.base, notes), (other, vec![]));
        assert!(legacy_base.exists() && !new_base.exists());

        // The default folder spelled differently (case, separators, trailing separator).
        let spelled = format!("{}/{}/", local.display(), DEFAULT_DIR_DEBUG.to_uppercase());
        let (dirs, notes) = resolve_with(Some(PathBuf::from(spelled)), Some(local.clone()), true).unwrap();
        assert_eq!(dirs.base, new_base);
        assert_eq!(dirs.profile, new_base.join(PROFILE_DIR));
        assert_eq!(std::fs::read_to_string(dirs.profile.join("state.json")).unwrap(), "{}");
        assert!(!legacy_base.exists());
        assert_eq!(
            notes,
            vec![
                Note::Moved { from: legacy_base.clone(), to: new_base.clone() },
                Note::Moved { from: new_base.join(legacy::PROFILE_DIR), to: new_base.join(PROFILE_DIR) },
            ]
        );
        // Same result without an override; LOCALAPPDATA is needed only then.
        let (dirs, notes) = resolve_with(None, Some(local.clone()), true).unwrap();
        assert_eq!((dirs.base, notes), (new_base, vec![]));
        assert!(matches!(resolve_with(None, None, true), Err(ResolveError::Io(_))));
        let _ = std::fs::remove_dir_all(&root);
    }
}
