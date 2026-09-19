//! Animation registry and the stored animation settings (`docs/ARCHITECTURE.md` "Motion").
//!
//! One place names every animation sta can play. The HTML surfaces gate each animation on its
//! **key**; the settings page lists the same keys grouped by [`GROUPS`]; the checker
//! `tools/check-motion.mjs` keeps registry, UI catalog and CSS gates in sync. Core never animates
//! anything — it only decides *which* keys are on and at what [`MotionLevel`], and serves that as
//! [`MotionView`] in `UiState.motion`.
//!
//! Three things are deliberately separate:
//! - the **master switch** and "follow Windows animation effects" ([`AnimationSettings::enabled`],
//!   [`AnimationSettings::follow_system`]), which pick the level for everything;
//! - **group** switches ([`AnimationSettings::groups`]);
//! - **per-key explicit choices** ([`AnimationSettings::choices`]).
//!
//! `choices` holds what the user *chose*, never a difference from the default: if a later release
//! flips a key's default, a user who explicitly picked the old value keeps it. Clearing a choice
//! (patch value `null`) is how a key goes back to following its default.

use serde::{Deserialize, Deserializer, Serialize};
use std::collections::BTreeMap;

/// One animation the UI can play. `key` is what HTML gates on and what settings store.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AnimationSpec {
    /// `group.name`, both lowerCamelCase (see [`KEY_PATTERN`]). Unique across the registry.
    pub key: &'static str,
    /// Id of the [`MotionGroup`] this key belongs to.
    pub group: &'static str,
    /// Whether the animation plays for a user who never touched the setting.
    pub default_on: bool,
}

/// A group of animation keys, as the settings page lists them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MotionGroup {
    pub id: &'static str,
    /// English label for the settings page (the UI catalog owns the per-key labels).
    pub label: &'static str,
}

/// The shape every [`AnimationSpec::key`] has, as a human-readable regex (checked by the unit
/// tests and by `tools/check-motion.mjs`; no regex crate is pulled in for it).
pub const KEY_PATTERN: &str = r"^[a-z][A-Za-z]*\.[a-z][A-Za-z]*$";

/// At most this many entries are kept in each stored map, so a hand-edited or downgraded profile
/// can never grow `state.json` without bound. Unknown keys inside the limit are **kept** (a
/// profile written by a newer build keeps its choices when an older one saves it again).
pub const MAX_ANIMATION_ENTRIES: usize = 128;

/// The 8 groups, in settings order.
pub const GROUPS: &[MotionGroup] = &[
    MotionGroup { id: "sidebar", label: "Sidebar & top bar" },
    MotionGroup { id: "commandBar", label: "Command bar" },
    MotionGroup { id: "overlays", label: "Overlays" },
    MotionGroup { id: "menus", label: "Menus" },
    MotionGroup { id: "pages", label: "Pages" },
    MotionGroup { id: "theme", label: "Theme" },
    MotionGroup { id: "controls", label: "Controls" },
    MotionGroup { id: "indicators", label: "Indicators" },
];

/// Every animation, grouped as [`GROUPS`] orders them.
pub const ANIMATIONS: &[AnimationSpec] = &[
    // ------------------------------------------------------------------ sidebar & top bar (14)
    AnimationSpec { key: "sidebar.tabInsertRemove", group: "sidebar", default_on: true },
    AnimationSpec { key: "sidebar.reorder", group: "sidebar", default_on: true },
    AnimationSpec { key: "sidebar.dragDrop", group: "sidebar", default_on: true },
    AnimationSpec { key: "sidebar.folderExpand", group: "sidebar", default_on: true },
    AnimationSpec { key: "sidebar.favorites", group: "sidebar", default_on: true },
    AnimationSpec { key: "sidebar.spaceSwitch", group: "sidebar", default_on: true },
    AnimationSpec { key: "sidebar.activeRow", group: "sidebar", default_on: true },
    AnimationSpec { key: "sidebar.hoverReveal", group: "sidebar", default_on: true },
    AnimationSpec { key: "sidebar.panels", group: "sidebar", default_on: true },
    AnimationSpec { key: "sidebar.downloads", group: "sidebar", default_on: true },
    AnimationSpec { key: "sidebar.clearToday", group: "sidebar", default_on: true },
    AnimationSpec { key: "sidebar.urlPill", group: "sidebar", default_on: true },
    AnimationSpec { key: "sidebar.splitRow", group: "sidebar", default_on: true },
    AnimationSpec { key: "topbar.navFade", group: "sidebar", default_on: true },
    // ------------------------------------------------------------------ command bar (4)
    AnimationSpec { key: "commandBar.open", group: "commandBar", default_on: true },
    AnimationSpec { key: "commandBar.results", group: "commandBar", default_on: true },
    AnimationSpec { key: "commandBar.selection", group: "commandBar", default_on: true },
    AnimationSpec { key: "commandBar.modeToggle", group: "commandBar", default_on: true },
    // ------------------------------------------------------------------ overlays (5)
    AnimationSpec { key: "overlays.toast", group: "overlays", default_on: true },
    AnimationSpec { key: "overlays.switcher", group: "overlays", default_on: true },
    AnimationSpec { key: "overlays.find", group: "overlays", default_on: true },
    AnimationSpec { key: "overlays.permission", group: "overlays", default_on: true },
    AnimationSpec { key: "overlays.peek", group: "overlays", default_on: true },
    // ------------------------------------------------------------------ menus (1)
    AnimationSpec { key: "menus.popIn", group: "menus", default_on: true },
    // ------------------------------------------------------------------ pages (5)
    AnimationSpec { key: "pages.enter", group: "pages", default_on: true },
    AnimationSpec { key: "pages.listRows", group: "pages", default_on: true },
    AnimationSpec { key: "pages.navIndicator", group: "pages", default_on: true },
    AnimationSpec { key: "pages.boostsEditor", group: "pages", default_on: true },
    AnimationSpec { key: "pages.emptyHero", group: "pages", default_on: true },
    // ------------------------------------------------------------------ theme (1)
    AnimationSpec { key: "theme.crossFade", group: "theme", default_on: true },
    // ------------------------------------------------------------------ controls (3)
    AnimationSpec { key: "controls.hoverPress", group: "controls", default_on: true },
    AnimationSpec { key: "controls.toggles", group: "controls", default_on: true },
    AnimationSpec { key: "controls.smoothScroll", group: "controls", default_on: true },
    // ------------------------------------------------------------------ indicators (3)
    AnimationSpec { key: "indicators.loading", group: "indicators", default_on: true },
    AnimationSpec { key: "indicators.badges", group: "indicators", default_on: true },
    AnimationSpec { key: "indicators.audio", group: "indicators", default_on: true },
];

/// The registered spec for `key`.
pub fn spec(key: &str) -> Option<&'static AnimationSpec> {
    ANIMATIONS.iter().find(|a| a.key == key)
}

/// The registered group for `id`.
pub fn group(id: &str) -> Option<&'static MotionGroup> {
    GROUPS.iter().find(|g| g.id == id)
}

/// Keys of one group, in registry order.
pub fn keys_of(group_id: &str) -> impl Iterator<Item = &'static AnimationSpec> {
    ANIMATIONS.iter().filter(move |a| a.group == group_id)
}

// ------------------------------------------------------------------------------------- settings

/// Persisted animation settings (`Settings::animations`). Never version-bumped: an older build
/// that does not know the field loads it as the default and writes it back unchanged only if it
/// rewrites `state.json` — which is why the maps keep unknown keys.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AnimationSettings {
    /// Master switch. Off means [`MotionLevel::Off`] for everything.
    #[serde(deserialize_with = "lenient_bool_true")]
    pub enabled: bool,
    /// Follow the Windows "Animation effects" setting: when Windows has them off, the level is
    /// [`MotionLevel::Reduced`].
    #[serde(deserialize_with = "lenient_bool_true")]
    pub follow_system: bool,
    /// Group id → on. A missing group is on. Entries that are not booleans are skipped on load.
    #[serde(deserialize_with = "lenient_bool_map")]
    pub groups: BTreeMap<String, bool>,
    /// Animation key → the choice the user made. A missing key follows
    /// [`AnimationSpec::default_on`]. Entries that are not booleans are skipped on load.
    #[serde(deserialize_with = "lenient_bool_map")]
    pub choices: BTreeMap<String, bool>,
}

impl Default for AnimationSettings {
    fn default() -> Self {
        Self { enabled: true, follow_system: true, groups: BTreeMap::new(), choices: BTreeMap::new() }
    }
}

/// A bool that falls back to `true` for anything that is not a JSON boolean, so one hand-edited
/// value cannot drop the whole `animations` object (and with it every per-key choice).
fn lenient_bool_true<'de, D: Deserializer<'de>>(d: D) -> Result<bool, D::Error> {
    Ok(serde_json::Value::deserialize(d).ok().and_then(|v| v.as_bool()).unwrap_or(true))
}

/// A `{key: bool}` map that skips entries which are not booleans (and anything beyond
/// [`MAX_ANIMATION_ENTRIES`]) instead of failing. Unknown keys are kept.
fn lenient_bool_map<'de, D: Deserializer<'de>>(d: D) -> Result<BTreeMap<String, bool>, D::Error> {
    let Ok(value) = serde_json::Value::deserialize(d) else { return Ok(BTreeMap::new()) };
    let Some(obj) = value.as_object() else { return Ok(BTreeMap::new()) };
    let mut out = BTreeMap::new();
    for (k, v) in obj {
        if out.len() >= MAX_ANIMATION_ENTRIES {
            break;
        }
        if let Some(b) = v.as_bool() {
            out.insert(k.clone(), b);
        }
    }
    Ok(out)
}

impl AnimationSettings {
    /// Whether the group `id` is on (a group nobody touched is on).
    pub fn group_on(&self, id: &str) -> bool {
        self.groups.get(id).copied().unwrap_or(true)
    }

    /// Whether a key with this group and default is on. Takes the group and default explicitly so
    /// the semantics can be tested across a default flip without editing the registry.
    pub fn key_on(&self, key: &str, group: &str, default_on: bool) -> bool {
        self.group_on(group) && self.choices.get(key).copied().unwrap_or(default_on)
    }

    /// Whether the registered `key` is on. An unregistered key is off (nothing gates on it).
    pub fn is_on(&self, key: &str) -> bool {
        spec(key).is_some_and(|s| self.key_on(s.key, s.group, s.default_on))
    }

    /// Resolved level for `system` = the Windows "Animation effects" setting.
    pub fn level(&self, system: bool) -> MotionLevel {
        if !self.enabled {
            MotionLevel::Off
        } else if self.follow_system && !system {
            MotionLevel::Reduced
        } else {
            MotionLevel::Full
        }
    }

    /// Registered keys that are off, in registry order.
    pub fn off_keys(&self) -> Vec<String> {
        ANIMATIONS.iter().filter(|a| !self.key_on(a.key, a.group, a.default_on)).map(|a| a.key.to_string()).collect()
    }

    /// `true` when nothing has been customised (what "Reset to defaults" leaves behind).
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    /// Applies an [`AnimationsPatch`] in the documented order: `reset`, then the scalars, then the
    /// maps (`None` clears an entry). Unknown keys in the patch are ignored; a stored map never
    /// grows past [`MAX_ANIMATION_ENTRIES`].
    pub fn apply_patch(&mut self, patch: &AnimationsPatch) {
        if patch.reset {
            *self = Self::default();
        }
        if let Some(v) = patch.enabled {
            self.enabled = v;
        }
        if let Some(v) = patch.follow_system {
            self.follow_system = v;
        }
        for (id, value) in &patch.groups {
            if group(id).is_none() {
                continue;
            }
            match value {
                Some(v) => {
                    if self.groups.len() < MAX_ANIMATION_ENTRIES || self.groups.contains_key(id) {
                        self.groups.insert(id.clone(), *v);
                    }
                }
                None => {
                    self.groups.remove(id);
                }
            }
        }
        for (key, value) in &patch.set {
            if spec(key).is_none() {
                continue;
            }
            match value {
                Some(v) => {
                    if self.choices.len() < MAX_ANIMATION_ENTRIES || self.choices.contains_key(key) {
                        self.choices.insert(key.clone(), *v);
                    }
                }
                None => {
                    self.choices.remove(key);
                }
            }
        }
    }
}

/// Partial update of [`AnimationSettings`] (`SettingsPatch::animations`).
///
/// Applied in one order, always: `reset` first, then the scalars, then the maps. A map value of
/// `null` **clears** that entry (the key goes back to following its default / its group), which is
/// how the settings page distinguishes "the user chose the default" from "the user chose nothing".
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AnimationsPatch {
    /// Back to defaults before anything else in this patch is applied.
    pub reset: bool,
    pub enabled: Option<bool>,
    pub follow_system: Option<bool>,
    /// Group id → on, or `null` to clear. Unknown groups are ignored.
    pub groups: BTreeMap<String, Option<bool>>,
    /// Animation key → an explicit choice, or `null` to clear it. Unknown keys are ignored.
    pub set: BTreeMap<String, Option<bool>>,
}

// ----------------------------------------------------------------------------------------- view

/// How much motion a surface may play (`UiState.motion.level`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MotionLevel {
    /// Everything a key allows.
    #[default]
    Full,
    /// Opacity only: no travel, no smooth scroll, no View Transition; loading indicators slower.
    Reduced,
    /// No finite animation at all. Loading state is still *shown*, as a static ring or bar.
    Off,
}

impl MotionLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            MotionLevel::Full => "full",
            MotionLevel::Reduced => "reduced",
            MotionLevel::Off => "off",
        }
    }
}

/// `UiState.motion`: everything a surface needs to decide whether an animation may run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MotionView {
    pub level: MotionLevel,
    /// Registered keys that are off (group off, or an explicit choice of off).
    pub off: Vec<String>,
    /// The Windows "Animation effects" setting as the shell last read it
    /// (`SPI_GETCLIENTAREAANIMATION`). Runtime only: never persisted.
    pub system_animations: bool,
}

impl Default for MotionView {
    fn default() -> Self {
        Self { level: MotionLevel::Full, off: Vec::new(), system_animations: true }
    }
}

impl MotionView {
    pub fn of(settings: &AnimationSettings, system_animations: bool) -> Self {
        Self { level: settings.level(system_animations), off: settings.off_keys(), system_animations }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key_ok(key: &str) -> bool {
        // `^[a-z][A-Za-z]*\.[a-z][A-Za-z]*$` without a regex dependency.
        let Some((head, tail)) = key.split_once('.') else { return false };
        let part = |p: &str| {
            let mut cs = p.chars();
            cs.next().is_some_and(|c| c.is_ascii_lowercase()) && cs.all(|c| c.is_ascii_alphabetic())
        };
        !key.contains("..") && part(head) && part(tail)
    }

    #[test]
    fn the_registry_has_36_keys_in_8_groups() {
        assert_eq!(GROUPS.len(), 8, "8 groups");
        assert_eq!(ANIMATIONS.len(), 36, "36 animation keys");
        let counts: Vec<(&str, usize)> = GROUPS.iter().map(|g| (g.id, keys_of(g.id).count())).collect();
        assert_eq!(
            counts,
            vec![
                ("sidebar", 14),
                ("commandBar", 4),
                ("overlays", 5),
                ("menus", 1),
                ("pages", 5),
                ("theme", 1),
                ("controls", 3),
                ("indicators", 3),
            ]
        );
    }

    #[test]
    fn keys_are_unique_well_formed_and_in_a_known_group() {
        let mut seen = std::collections::BTreeSet::new();
        for a in ANIMATIONS {
            assert!(seen.insert(a.key), "duplicate key {}", a.key);
            assert!(key_ok(a.key), "key {} does not match {KEY_PATTERN}", a.key);
            assert!(group(a.group).is_some(), "key {} is in unknown group {}", a.key, a.group);
            assert!(spec(a.key).is_some(), "spec() cannot find {}", a.key);
        }
        let mut group_ids = std::collections::BTreeSet::new();
        for g in GROUPS {
            assert!(group_ids.insert(g.id), "duplicate group {}", g.id);
            assert!(!g.label.is_empty());
            assert!(keys_of(g.id).next().is_some(), "group {} has no keys", g.id);
        }
        // The registry order groups keys: every group's keys are contiguous.
        let order: Vec<&str> = ANIMATIONS.iter().map(|a| a.group).collect();
        let mut compact: Vec<&str> = Vec::new();
        for g in &order {
            if compact.last() != Some(g) {
                assert!(!compact.contains(g), "group {g} appears twice in ANIMATIONS");
                compact.push(g);
            }
        }
        assert_eq!(compact, GROUPS.iter().map(|g| g.id).collect::<Vec<_>>(), "registry order must match GROUPS");
    }

    #[test]
    fn defaults_are_all_on_and_nothing_is_off_by_default() {
        let s = AnimationSettings::default();
        assert!(s.enabled && s.follow_system);
        assert!(s.is_default());
        assert!(s.off_keys().is_empty(), "every key is on by default");
        for a in ANIMATIONS {
            assert!(a.default_on, "{} is off by default (update the docs and the settings copy)", a.key);
            assert!(s.is_on(a.key));
        }
        assert!(!s.is_on("nope.notAKey"), "an unregistered key is never on");
    }

    #[test]
    fn a_group_switch_turns_off_its_keys_only() {
        let mut s = AnimationSettings::default();
        s.groups.insert("overlays".into(), false);
        assert_eq!(s.off_keys(), keys_of("overlays").map(|a| a.key.to_string()).collect::<Vec<_>>());
        assert!(!s.is_on("overlays.toast"));
        assert!(s.is_on("sidebar.reorder"));
        // A key's own choice cannot re-enable it while its group is off.
        s.choices.insert("overlays.toast".into(), true);
        assert!(!s.is_on("overlays.toast"));
    }

    #[test]
    fn an_explicit_choice_survives_a_default_flip() {
        let mut s = AnimationSettings::default();
        // The user explicitly chose what is the default today.
        s.choices.insert("menus.popIn".into(), true);
        assert!(s.key_on("menus.popIn", "menus", true));
        // A later release flips that default to off: the stored choice still wins.
        assert!(s.key_on("menus.popIn", "menus", false));
        // A key nobody chose follows the (new) default.
        assert!(!s.key_on("menus.other", "menus", false));
        // Clearing the choice gives the default back.
        s.apply_patch(&AnimationsPatch { set: [("menus.popIn".to_string(), None)].into(), ..Default::default() });
        assert!(s.choices.is_empty());
    }

    #[test]
    fn the_level_matrix() {
        let mut s = AnimationSettings::default();
        assert_eq!(s.level(true), MotionLevel::Full);
        assert_eq!(s.level(false), MotionLevel::Reduced, "follow_system + Windows animations off");
        s.follow_system = false;
        assert_eq!(s.level(false), MotionLevel::Full, "not following Windows");
        s.enabled = false;
        assert_eq!(s.level(true), MotionLevel::Off);
        assert_eq!(s.level(false), MotionLevel::Off, "the master switch wins");
        s.follow_system = true;
        assert_eq!(s.level(true), MotionLevel::Off);
    }

    #[test]
    fn the_patch_runs_reset_then_scalars_then_maps() {
        let mut s = AnimationSettings { enabled: false, ..Default::default() };
        s.groups.insert("pages".into(), false);
        s.choices.insert("theme.crossFade".into(), false);
        s.apply_patch(&AnimationsPatch {
            reset: true,
            enabled: Some(false),
            groups: [("menus".to_string(), Some(false))].into(),
            set: [("pages.enter".to_string(), Some(false))].into(),
            ..Default::default()
        });
        assert!(!s.enabled, "the scalar in the same patch is applied after the reset");
        assert!(s.follow_system, "reset restored it");
        assert_eq!(s.groups, [("menus".to_string(), false)].into());
        assert_eq!(s.choices, [("pages.enter".to_string(), false)].into());
    }

    #[test]
    fn the_patch_ignores_unknown_keys_and_clears_with_null() {
        let mut s = AnimationSettings::default();
        s.apply_patch(&AnimationsPatch {
            groups: [("nope".to_string(), Some(false))].into(),
            set: [("nope.nothing".to_string(), Some(false)), ("sidebar.reorder".to_string(), Some(false))].into(),
            ..Default::default()
        });
        assert_eq!(s.groups, BTreeMap::new());
        assert_eq!(s.choices, [("sidebar.reorder".to_string(), false)].into());
        s.apply_patch(&AnimationsPatch { set: [("sidebar.reorder".to_string(), None)].into(), ..Default::default() });
        assert!(s.choices.is_empty());
        assert!(s.is_default());
    }

    #[test]
    fn stored_maps_are_capped_but_keep_unknown_keys() {
        let mut obj = serde_json::Map::new();
        for i in 0..(MAX_ANIMATION_ENTRIES + 20) {
            obj.insert(format!("zz.k{i:04}"), serde_json::Value::Bool(false));
        }
        let json = serde_json::json!({"choices": obj});
        let s: AnimationSettings = serde_json::from_value(json).expect("lenient");
        assert_eq!(s.choices.len(), MAX_ANIMATION_ENTRIES, "capped");
        assert!(s.choices.contains_key("zz.k0000"), "unknown keys inside the cap are kept");
        // A patch cannot add an unknown key, so the cap cannot be reached from the UI.
        let mut fresh = AnimationSettings::default();
        fresh.apply_patch(&AnimationsPatch { set: [("zz.k0000".to_string(), Some(false))].into(), ..Default::default() });
        assert!(fresh.choices.is_empty());
    }

    #[test]
    fn one_bad_entry_does_not_drop_the_others() {
        let json = serde_json::json!({
            "enabled": "yes",
            "followSystem": 0,
            "groups": {"overlays": false, "menus": "maybe"},
            "choices": {"sidebar.reorder": false, "pages.enter": [1], "theme.crossFade": true}
        });
        let s: AnimationSettings = serde_json::from_value(json).expect("lenient");
        assert!(s.enabled, "a non-boolean master switch falls back to on");
        assert!(s.follow_system);
        assert_eq!(s.groups, [("overlays".to_string(), false)].into());
        assert_eq!(s.choices, [("sidebar.reorder".to_string(), false), ("theme.crossFade".to_string(), true)].into());
    }

    #[test]
    fn the_serde_shape_is_camel_case() {
        let mut s = AnimationSettings { enabled: false, ..Default::default() };
        s.groups.insert("menus".into(), false);
        s.choices.insert("overlays.toast".into(), false);
        assert_eq!(
            serde_json::to_value(&s).unwrap(),
            serde_json::json!({
                "enabled": false,
                "followSystem": true,
                "groups": {"menus": false},
                "choices": {"overlays.toast": false}
            })
        );
        let patch: AnimationsPatch =
            serde_json::from_value(serde_json::json!({"reset": true, "followSystem": false, "set": {"menus.popIn": null}}))
                .unwrap();
        assert!(patch.reset);
        assert_eq!(patch.follow_system, Some(false));
        assert_eq!(patch.enabled, None);
        assert_eq!(patch.set, [("menus.popIn".to_string(), None)].into());
        assert_eq!(serde_json::from_value::<AnimationSettings>(serde_json::json!({})).unwrap(), AnimationSettings::default());
        assert_eq!(
            serde_json::to_value(MotionView::default()).unwrap(),
            serde_json::json!({"level": "full", "off": [], "systemAnimations": true})
        );
    }

    #[test]
    fn the_view_reports_the_level_and_the_off_keys() {
        let mut s = AnimationSettings::default();
        s.groups.insert("menus".into(), false);
        s.choices.insert("indicators.audio".into(), false);
        let v = MotionView::of(&s, false);
        assert_eq!(v.level, MotionLevel::Reduced);
        assert!(!v.system_animations);
        assert_eq!(v.off, vec!["menus.popIn".to_string(), "indicators.audio".to_string()]);
        assert_eq!(MotionLevel::Off.as_str(), "off");
    }
}
