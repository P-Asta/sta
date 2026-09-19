//! Names from before the product was renamed from Astatine to sta, and the pure decisions of the
//! migration that needs them (README "Renamed from Astatine", docs/ARCHITECTURE.md §3 and §7).
//!
//! rename:keep-file — the mechanical rename script skips this file: every "astatine" in it is a
//! legacy name on purpose. Nothing else in the tree may use these names.
//!
//! - Persisted JSON (`state.json`, `history.json`) is upgraded on load: every string that is a
//!   legacy internal URL (`astatine://…`, also behind `view-source:`) becomes `sta://…`
//!   ([`upgrade_json`], called by `Store::load`).
//! - Typed or command-line `astatine://…` input opens `sta://…` ([`upgrade_url`], called by
//!   `omnibox::classify` and by the shell for command-line and relaunch arguments).
//! - The shell moves the old default data folder (`%LOCALAPPDATA%\Astatine`,
//!   `%LOCALAPPDATA%\Astatine Dev`) and the old profile subfolder (`<data>\astatine`) to their new
//!   names at startup; [`plan_move`] and [`after_failed_move`] decide what happens, depending on
//!   who holds the legacy folder ([`LegacyHolder`]).

use serde_json::Value;

/// The product's display name before the rename.
pub const PRODUCT_NAME: &str = "Astatine";
/// Internal URL scheme before the rename.
pub const SCHEME: &str = "astatine";
/// Default data folder under `%LOCALAPPDATA%` of release builds before the rename.
pub const DATA_DIR_RELEASE: &str = "Astatine";
/// Default data folder under `%LOCALAPPDATA%` of debug builds before the rename.
pub const DATA_DIR_DEBUG: &str = "Astatine Dev";
/// Profile subfolder of the data folder (`state.json`, `history.json`, …) before the rename.
pub const PROFILE_DIR: &str = "astatine";

const LEGACY_PREFIX: &str = "astatine://";
const PREFIX: &str = "sta://";
const VIEW_SOURCE: &str = "view-source:";

/// The legacy default data folder name of this build flavor.
pub fn data_dir_name(debug: bool) -> &'static str {
    if debug { DATA_DIR_DEBUG } else { DATA_DIR_RELEASE }
}

fn strip_prefix_ignore_case<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    s.get(..prefix.len()).filter(|head| head.eq_ignore_ascii_case(prefix)).map(|_| &s[prefix.len()..])
}

/// `astatine://…` → `sta://…` (scheme matched case-insensitively, the rest kept verbatim), also as
/// `view-source:astatine://…`; `None` for anything else.
pub fn upgrade_url(url: &str) -> Option<String> {
    if let Some(rest) = strip_prefix_ignore_case(url, LEGACY_PREFIX) {
        return Some(format!("{PREFIX}{rest}"));
    }
    let inner = strip_prefix_ignore_case(url, VIEW_SOURCE)?;
    let rest = strip_prefix_ignore_case(inner, LEGACY_PREFIX)?;
    Some(format!("{}{PREFIX}{rest}", &url[..VIEW_SOURCE.len()]))
}

/// Rewrites every string value (and object key) of `value` that is a legacy internal URL, at any
/// depth. Returns how many were rewritten.
pub fn upgrade_json(value: &mut Value) -> usize {
    match value {
        Value::String(s) => match upgrade_url(s) {
            Some(u) => {
                *s = u;
                1
            }
            None => 0,
        },
        Value::Array(items) => items.iter_mut().map(upgrade_json).sum(),
        Value::Object(map) => {
            let mut n = 0;
            let legacy_keys: Vec<String> = map.keys().filter(|k| upgrade_url(k).is_some()).cloned().collect();
            for key in legacy_keys {
                let upgraded = upgrade_url(&key).expect("filtered above");
                // Never overwrite an entry that already uses the new name.
                if !map.contains_key(&upgraded)
                    && let Some(v) = map.remove(&key)
                {
                    map.insert(upgraded, v);
                    n += 1;
                }
            }
            n + map.values_mut().map(upgrade_json).sum::<usize>()
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => 0,
    }
}

/// Who holds a legacy folder's Chromium profile (its process singleton lock is open).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LegacyHolder {
    /// No running browser uses it.
    Nobody,
    /// A running sta that uses the legacy folder in place because its move failed (it also holds
    /// the folder's in-place lock, see the shell's `paths`).
    StaInPlace,
    /// Another browser: a running older Astatine.
    OldBrowser,
}

/// What to do with a legacy folder (`%LOCALAPPDATA%\Astatine Dev`, `<data>\astatine`) whose new
/// counterpart (`%LOCALAPPDATA%\sta Dev`, `<data>\sta`) should be used.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MovePlan {
    /// Use the new folder: it already exists (a legacy folder next to it is never merged or
    /// touched), or neither exists (a fresh start).
    UseNew,
    /// Rename the legacy folder to the new name (same parent, so same volume), then use it.
    Move,
    /// Use the legacy folder where it is, like the running sta that already does: both share its
    /// Chromium profile, so the process singleton hands this launch's command line to that instance.
    /// (It can't be moved while in use.)
    ShareInPlace,
    /// Don't start: a running older Astatine holds the legacy folder. Moving is impossible, and
    /// using it in place would share its profile, so the process singleton would hand this launch's
    /// command line (URLs) to the old browser.
    Refuse,
}

/// Decides before touching anything.
pub fn plan_move(new_exists: bool, legacy_exists: bool, holder: LegacyHolder) -> MovePlan {
    if new_exists || !legacy_exists {
        return MovePlan::UseNew;
    }
    match holder {
        LegacyHolder::Nobody => MovePlan::Move,
        LegacyHolder::StaInPlace => MovePlan::ShareInPlace,
        LegacyHolder::OldBrowser => MovePlan::Refuse,
    }
}

/// Where to go after the rename of [`MovePlan::Move`] failed (checked again right after it).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AfterFailedMove {
    /// The new folder exists now (a concurrent launch moved it), or the legacy one is gone:
    /// use the new folder.
    UseNew,
    /// Use the legacy folder where it is for this run (nothing is lost; the move is retried at the
    /// next launch). Also when a concurrent sta started using it in place in the meantime.
    UseLegacyInPlace,
    /// An older Astatine started using the legacy folder in the meantime: don't start (see
    /// [`MovePlan::Refuse`]).
    Refuse,
}

pub fn after_failed_move(new_exists: bool, legacy_exists: bool, holder: LegacyHolder) -> AfterFailedMove {
    if new_exists || !legacy_exists {
        return AfterFailedMove::UseNew;
    }
    match holder {
        LegacyHolder::Nobody | LegacyHolder::StaInPlace => AfterFailedMove::UseLegacyInPlace,
        LegacyHolder::OldBrowser => AfterFailedMove::Refuse,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn upgrade_url_cases() {
        assert_eq!(upgrade_url("astatine://settings/").as_deref(), Some("sta://settings/"));
        assert_eq!(upgrade_url("ASTATINE://boosts/?id=5#x").as_deref(), Some("sta://boosts/?id=5#x"));
        assert_eq!(upgrade_url("Astatine://history").as_deref(), Some("sta://history"));
        assert_eq!(upgrade_url("astatine://").as_deref(), Some("sta://"));
        assert_eq!(upgrade_url("view-source:astatine://settings/").as_deref(), Some("view-source:sta://settings/"));
        assert_eq!(upgrade_url("VIEW-SOURCE:astatine://settings/").as_deref(), Some("VIEW-SOURCE:sta://settings/"));
        for other in [
            "sta://settings/",
            "https://astatine.example/astatine://x",
            "astatine:settings",
            "astatine:/settings",
            " astatine://settings/",
            "view-source:https://example.com",
            "astatine",
            "",
            "한국어",
        ] {
            assert_eq!(upgrade_url(other), None, "{other:?}");
        }
    }

    #[test]
    fn upgrade_json_rewrites_every_legacy_url_at_any_depth() {
        let mut v = json!({
            "favorites": [{ "kind": "tab", "url": "astatine://settings/", "pinnedUrl": "ASTATINE://history/", "title": "astatine://in a title?" }],
            "spaces": [{ "name": "astatine", "today": [{ "url": "https://example.com/astatine://" }] }],
            "archive": [{ "tabs": [{ "url": "view-source:astatine://boosts/?id=3", "favicon": "astatine://ui/favicon/x" }] }],
            "closed": [[{ "url": "astatine://archive/" }]],
            "byUrl": { "astatine://settings/": 1, "sta://history/": 2, "astatine://history/": 3 },
            "n": 5, "b": true, "z": null
        });
        assert_eq!(upgrade_json(&mut v), 7);
        assert_eq!(
            v,
            json!({
                "favorites": [{ "kind": "tab", "url": "sta://settings/", "pinnedUrl": "sta://history/", "title": "sta://in a title?" }],
                "spaces": [{ "name": "astatine", "today": [{ "url": "https://example.com/astatine://" }] }],
                "archive": [{ "tabs": [{ "url": "view-source:sta://boosts/?id=3", "favicon": "sta://ui/favicon/x" }] }],
                "closed": [[{ "url": "sta://archive/" }]],
                // An existing new-name key wins; the legacy duplicate stays as it was.
                "byUrl": { "sta://settings/": 1, "sta://history/": 2, "astatine://history/": 3 },
                "n": 5, "b": true, "z": null
            })
        );
        assert_eq!(upgrade_json(&mut v), 0, "idempotent");
    }

    #[test]
    fn move_plan_matrix() {
        use LegacyHolder::*;
        use MovePlan::*;
        // (new exists, legacy exists, holder of the legacy folder) → plan
        let cases = [
            ((false, false, Nobody), UseNew),
            ((false, false, OldBrowser), UseNew),
            ((true, false, Nobody), UseNew),
            ((true, true, Nobody), UseNew),
            ((true, true, OldBrowser), UseNew),
            ((true, true, StaInPlace), UseNew),
            ((false, true, Nobody), Move),
            ((false, true, StaInPlace), ShareInPlace),
            ((false, true, OldBrowser), Refuse),
        ];
        for ((n, l, holder), want) in cases {
            assert_eq!(plan_move(n, l, holder), want, "new={n} legacy={l} holder={holder:?}");
        }
    }

    #[test]
    fn after_failed_move_matrix() {
        use AfterFailedMove::*;
        use LegacyHolder::*;
        let cases = [
            ((true, false, Nobody), UseNew),
            ((true, true, OldBrowser), UseNew),
            ((false, false, Nobody), UseNew),
            ((false, true, Nobody), UseLegacyInPlace),
            ((false, true, StaInPlace), UseLegacyInPlace),
            ((false, true, OldBrowser), Refuse),
        ];
        for ((n, l, holder), want) in cases {
            assert_eq!(after_failed_move(n, l, holder), want, "new={n} legacy={l} holder={holder:?}");
        }
    }

    #[test]
    fn legacy_names() {
        assert_eq!(data_dir_name(true), "Astatine Dev");
        assert_eq!(data_dir_name(false), "Astatine");
        assert_eq!(PROFILE_DIR, "astatine");
        assert_eq!(SCHEME, "astatine");
    }
}
