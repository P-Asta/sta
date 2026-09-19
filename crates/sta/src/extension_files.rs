//! Installed extensions on disk, read-only [owner: tabs] (foreign.rs; ARCHITECTURE §4.5).
//!
//! Phase 1 of the extensions work only needs what is on disk, never Chromium's preferences writes:
//! - the profile's `Extensions/<id>/<version>/manifest.json` (Web Store and external installs; the
//!   directory exists before `OnInstallSuccess` opens its post-install browser);
//! - `--load-extension` directories (unpacked, debug builds and tests): id from the manifest `key`,
//!   else from the absolute path like Chromium (`crx_file::id_util::GenerateIdForPath`);
//! - as a fallback, `extensions.settings.<id>.path` in `Secure Preferences` / `Preferences`
//!   (read only; Chromium writes them with a delay, so they can be missing for a fresh install).
//!
//! Names are localized from `_locales/<locale>/messages.json` (`__MSG_name__`). Pages come from the
//! manifest: options (`options_ui.page`, `options_page`), popup (`action`, `browser_action`,
//! `page_action` `default_popup`), side panel (`side_panel.default_path`) and resources web-accessible
//! to every site. Nothing here writes to the profile.
//!
//! Public API:
//! - `pub fn is_extension_id(s: &str) -> bool`
//! - `pub struct ExtensionFiles { id, dir, manifest, modified_ms }` with `name`, `options_page`,
//!   `popup_page`, `side_panel_page`, `web_accessible_to_all`
//! - `pub fn extensions_dir() -> Option<PathBuf>`, `pub fn scan(dir: &Path) -> BTreeMap<String, Vec<String>>`
//! - `pub fn find(id: &str) -> Option<ExtensionFiles>`, `pub fn load_version_dir(id: &str, dir: &Path) -> Option<ExtensionFiles>`
//! - `pub fn install_location(id: &str) -> Option<i64>`, `pub fn is_external_location(location: i64) -> bool`,
//!   `pub fn is_component_location`, `pub fn is_policy_location`
//! - `pub fn prefs_settings() -> BTreeMap<String, Value>`, `pub fn prefs_commands() -> BTreeMap<String, Vec<(String, String)>>`
//! - `pub fn command_line_extensions() -> Vec<(String, PathBuf)>`
//! - `pub fn normalize_page(path: &str) -> Option<String>`, `pub fn is_image_file`, `pub fn is_inside`

use cef::ImplCommandLine;
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// `[a-p]{32}`: Chromium extension ids.
pub fn is_extension_id(s: &str) -> bool {
    s.len() == 32 && s.bytes().all(|b| (b'a'..=b'p').contains(&b))
}

/// The id Chromium derives from 16 bytes of a SHA-256 (hex digits mapped to `a`..`p`).
fn id_from_hash(hash: &[u8]) -> String {
    hash.iter().take(16).flat_map(|b| [b >> 4, b & 15]).map(|n| (b'a' + n) as char).collect()
}

fn base64_decode(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len() * 3 / 4);
    let (mut acc, mut bits) = (0u32, 0u32);
    for c in text.bytes().filter(|c| !c.is_ascii_whitespace()) {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            b'=' => break,
            _ => return None,
        };
        acc = (acc << 6) | v as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Some(out)
}

/// Id of an extension with a manifest `key` (base64 DER public key).
pub fn id_from_key(key: &str) -> Option<String> {
    let der = base64_decode(key)?;
    crate::platform::sha256(&der).map(|h| id_from_hash(&h))
}

/// Id of an unpacked extension without a `key`: SHA-256 of the absolute path as UTF-16LE with an
/// upper-case drive letter (Windows `GenerateIdForPath`).
pub fn id_from_path(path: &Path) -> Option<String> {
    let mut units: Vec<u16> = path.as_os_str().to_string_lossy().encode_utf16().collect();
    if units.len() >= 2 && units[1] == u16::from(b':') && (u16::from(b'a')..=u16::from(b'z')).contains(&units[0]) {
        units[0] -= 32;
    }
    let bytes: Vec<u8> = units.iter().flat_map(|u| u.to_le_bytes()).collect();
    crate::platform::sha256(&bytes).map(|h| id_from_hash(&h))
}

/// A relative extension page path without a leading `/`, query or fragment, `..`, `\` or a scheme.
pub fn normalize_page(path: &str) -> Option<String> {
    let path = path.trim().trim_start_matches('/');
    let path = path.split(['?', '#']).next().unwrap_or_default();
    if path.is_empty() || path.contains('\\') || path.contains(':') || path.split('/').any(|s| s == ".." || s == ".") {
        return None;
    }
    Some(path.to_string())
}

/// Where the files were found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// The profile's `Extensions/<id>/<version>/` (Web Store, external installs).
    Installed,
    /// A `--load-extension` directory.
    CommandLine,
    /// The `path` in the preferences.
    Preferences,
}

#[derive(Debug, Clone)]
pub struct ExtensionFiles {
    pub id: String,
    /// The directory holding `manifest.json`.
    pub dir: PathBuf,
    pub manifest: Value,
    /// Modification time of `dir` (ms since the Unix epoch).
    pub modified_ms: Option<i64>,
    pub source: Source,
}

impl ExtensionFiles {
    fn str_at(&self, path: &[&str]) -> Option<&str> {
        let mut v = &self.manifest;
        for key in path {
            v = v.get(key)?;
        }
        v.as_str()
    }

    /// Localized name (`__MSG_x__` resolved for `locale`, then the default locale), else the id.
    pub fn name(&self, locale: &str) -> String {
        let raw = self.str_at(&["name"]).unwrap_or_default();
        let resolved = match raw.strip_prefix("__MSG_").and_then(|r| r.strip_suffix("__")) {
            Some(key) => self.message(key, locale),
            None => Some(raw.to_string()),
        };
        resolved.map(|n| n.trim().to_string()).filter(|n| !n.is_empty()).unwrap_or_else(|| self.id.clone())
    }

    fn message(&self, key: &str, locale: &str) -> Option<String> {
        let mut candidates: Vec<String> = Vec::new();
        let ui = locale.replace('-', "_");
        if !ui.is_empty() {
            candidates.push(ui.clone());
            if let Some((lang, _)) = ui.split_once('_') {
                candidates.push(lang.to_string());
            }
        }
        if let Some(default) = self.str_at(&["default_locale"]) {
            candidates.push(default.to_string());
        }
        candidates.push("en".into());
        for candidate in candidates {
            if candidate.contains(['/', '\\', '.']) {
                continue;
            }
            let file = self.dir.join("_locales").join(&candidate).join("messages.json");
            let Some(messages) = read_json(&file) else { continue };
            let Some(map) = messages.as_object() else { continue };
            if let Some((_, entry)) = map.iter().find(|(k, _)| k.eq_ignore_ascii_case(key))
                && let Some(message) = entry.get("message").and_then(Value::as_str)
            {
                return Some(message.to_string());
            }
        }
        None
    }

    pub fn version(&self) -> String {
        self.str_at(&["version"]).unwrap_or_default().to_string()
    }

    /// Localized `short_name` (empty when the manifest has none).
    pub fn short_name(&self, locale: &str) -> String {
        self.localized(self.str_at(&["short_name"]).unwrap_or_default(), locale)
    }

    pub fn description(&self, locale: &str) -> String {
        self.localized(self.str_at(&["description"]).unwrap_or_default(), locale)
    }

    /// Resolves a `__MSG_key__` value for `locale` (a plain string passes through).
    fn localized(&self, raw: &str, locale: &str) -> String {
        match raw.strip_prefix("__MSG_").and_then(|r| r.strip_suffix("__")) {
            Some(key) => self.message(key, locale).unwrap_or_default().trim().to_string(),
            None => raw.trim().to_string(),
        }
    }

    /// The icon file closest to `px` (the smallest one at least that big, else the largest), as an
    /// existing path **inside** the extension's directory. `manifest.icons` first, then the action's
    /// `default_icon` — the two places a browser looks.
    ///
    /// Containment is checked on the resolved path, not on the manifest string: a manifest is just a
    /// file on disk, and `"icons": {"16": "../../../../Windows/win.ini"}` must find nothing.
    pub fn icon_path(&self, px: u32) -> Option<PathBuf> {
        let mut candidates: Vec<(u32, &str)> = Vec::new();
        let sources = [self.manifest.get("icons")]
            .into_iter()
            .chain(["action", "browser_action", "page_action"].map(|key| self.manifest.get(key).and_then(|a| a.get("default_icon"))));
        for value in sources.flatten() {
            match value {
                Value::Object(map) => {
                    for (size, path) in map {
                        if let (Ok(size), Some(path)) = (size.parse::<u32>(), path.as_str()) {
                            candidates.push((size, path));
                        }
                    }
                }
                // A single `default_icon: "icon.png"` has no declared size.
                Value::String(path) => candidates.push((0, path)),
                _ => {}
            }
        }
        candidates.sort_by_key(|(size, _)| *size);
        let best = candidates
            .iter()
            .find(|(size, _)| *size >= px && *size > 0)
            .or_else(|| candidates.iter().rfind(|(size, _)| *size > 0))
            .or_else(|| candidates.first())?;
        let relative = normalize_page(best.1)?;
        let path = self.dir.join(&relative);
        if !is_image_file(&path) || !is_inside(&self.dir, &path) {
            return None;
        }
        path.is_file().then_some(path)
    }

    pub fn options_page(&self) -> Option<String> {
        self.str_at(&["options_ui", "page"]).or(self.str_at(&["options_page"])).and_then(normalize_page)
    }

    pub fn popup_page(&self) -> Option<String> {
        ["action", "browser_action", "page_action"]
            .iter()
            .find_map(|key| self.str_at(&[key, "default_popup"]))
            .and_then(normalize_page)
    }

    pub fn side_panel_page(&self) -> Option<String> {
        self.str_at(&["side_panel", "default_path"]).and_then(normalize_page)
    }

    /// The extension asks for the tab the user is looking at (`tabs` or `activeTab`, in either
    /// manifest version's permission list).
    ///
    /// Prebuilt CEF cannot give an action popup a current tab (D1a), so a popup of such an
    /// extension is the one sta knows may not work — and the card has to say so *before* the
    /// extension's own "Oops!" page is the only thing on screen, which is what a popup that asks
    /// for the current tab and gets nothing renders (measured on AdBlock).
    pub fn needs_current_tab(&self) -> bool {
        ["permissions", "optional_permissions"].iter().any(|key| {
            self.manifest
                .get(key)
                .and_then(Value::as_array)
                .is_some_and(|list| list.iter().filter_map(Value::as_str).any(|p| p == "tabs" || p == "activeTab"))
        })
    }

    /// Resource patterns web-accessible to every site (`<all_urls>`, `*://*/*`; MV2 lists count).
    pub fn web_accessible_to_all(&self) -> Vec<String> {
        let Some(list) = self.manifest.get("web_accessible_resources").and_then(Value::as_array) else { return Vec::new() };
        let mut out = Vec::new();
        for entry in list {
            match entry {
                Value::String(s) => out.push(s.trim_start_matches('/').to_string()),
                Value::Object(o) => {
                    let all = o
                        .get("matches")
                        .and_then(Value::as_array)
                        .is_some_and(|m| m.iter().filter_map(Value::as_str).any(|m| m == "<all_urls>" || m == "*://*/*"));
                    if all && let Some(resources) = o.get("resources").and_then(Value::as_array) {
                        out.extend(resources.iter().filter_map(Value::as_str).map(|r| r.trim_start_matches('/').to_string()));
                    }
                }
                _ => {}
            }
        }
        out
    }
}

/// Image types sta will serve as an extension icon (UX7).
pub fn is_image_file(path: &Path) -> bool {
    let ext = path.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
    matches!(ext.as_str(), "png" | "jpg" | "jpeg" | "webp" | "ico")
}

/// `path` resolves inside `dir` (both canonicalized, so symlinks and `..` cannot escape).
pub fn is_inside(dir: &Path, path: &Path) -> bool {
    match (std::fs::canonicalize(dir), std::fs::canonicalize(path)) {
        (Ok(dir), Ok(path)) => path.starts_with(dir),
        _ => false,
    }
}

fn read_json(path: &Path) -> Option<Value> {
    let bytes = std::fs::read(path).ok()?;
    // Manifests may start with a UTF-8 BOM.
    let bytes = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(&bytes);
    serde_json::from_slice(bytes).ok()
}

fn modified_ms(path: &Path) -> Option<i64> {
    let t = std::fs::metadata(path).ok()?.modified().ok()?;
    Some(t.duration_since(std::time::UNIX_EPOCH).ok()?.as_millis() as i64)
}

/// `<user data>/Default`: the Chromium profile sta uses.
pub fn profile_dir() -> Option<PathBuf> {
    crate::paths::try_dirs().map(|d| d.user_data.join("Default"))
}

pub fn extensions_dir() -> Option<PathBuf> {
    profile_dir().map(|p| p.join("Extensions"))
}

/// `id → version directory names` under an `Extensions` directory (ids only, `Temp` skipped).
pub fn scan(dir: &Path) -> BTreeMap<String, Vec<String>> {
    let mut out = BTreeMap::new();
    let Ok(entries) = std::fs::read_dir(dir) else { return out };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !is_extension_id(&name) || !entry.path().is_dir() {
            continue;
        }
        let mut versions: Vec<String> = std::fs::read_dir(entry.path())
            .map(|v| v.flatten().filter(|e| e.path().join("manifest.json").is_file()).map(|e| e.file_name().to_string_lossy().into_owned()).collect())
            .unwrap_or_default();
        versions.sort();
        if !versions.is_empty() {
            out.insert(name, versions);
        }
    }
    out
}

/// The manifest in `dir` (a version directory or an unpacked extension) for `id`.
pub fn load_version_dir(id: &str, dir: &Path) -> Option<ExtensionFiles> {
    let manifest = read_json(&dir.join("manifest.json"))?;
    manifest.is_object().then(|| ExtensionFiles { id: id.to_string(), dir: dir.to_path_buf(), manifest, modified_ms: modified_ms(dir), source: Source::Installed })
}

/// The newest version directory of `id` under `extensions_dir` (by modification time).
fn load_installed(extensions_dir: &Path, id: &str) -> Option<ExtensionFiles> {
    let base = extensions_dir.join(id);
    let versions = scan(extensions_dir).remove(id)?;
    versions
        .iter()
        .filter_map(|v| load_version_dir(id, &base.join(v)))
        .max_by_key(|e| e.modified_ms.unwrap_or(0))
}

/// Unpacked extensions from `--load-extension` (comma-separated absolute or relative paths).
pub fn command_line_extensions() -> Vec<(String, PathBuf)> {
    let Some(cl) = cef::command_line_get_global() else { return Vec::new() };
    let key = cef::CefString::from("load-extension");
    let value = cef::CefString::from(&cl.switch_value(Some(&key))).to_string();
    value
        .split(',')
        .filter(|p| !p.trim().is_empty())
        .filter_map(|p| {
            let path = std::path::absolute(p.trim()).ok()?;
            let manifest = read_json(&path.join("manifest.json"))?;
            let id = match manifest.get("key").and_then(Value::as_str) {
                Some(key) => id_from_key(key)?,
                None => id_from_path(&path)?,
            };
            Some((id, path))
        })
        .collect()
}

/// `extensions.settings.<id>` from `Secure Preferences`, else `Preferences`.
fn prefs_entry(id: &str) -> Option<Value> {
    let profile = profile_dir()?;
    ["Secure Preferences", "Preferences"].iter().find_map(|file| {
        let prefs = read_json(&profile.join(file))?;
        prefs.get("extensions")?.get("settings")?.get(id).cloned()
    })
}

/// Everything known on disk about `id`: the Extensions directory, `--load-extension`, preferences.
pub fn find(id: &str) -> Option<ExtensionFiles> {
    if !is_extension_id(id) {
        return None;
    }
    if let Some(dir) = extensions_dir()
        && let Some(found) = load_installed(&dir, id)
    {
        return Some(found);
    }
    if let Some((_, path)) = command_line_extensions().into_iter().find(|(x, _)| x == id) {
        return load_version_dir(id, &path).map(|f| ExtensionFiles { source: Source::CommandLine, ..f });
    }
    let path = prefs_entry(id)?.get("path")?.as_str()?.to_string();
    let path = PathBuf::from(path);
    let path = if path.is_absolute() { path } else { extensions_dir()?.join(path) };
    load_version_dir(id, &path).map(|f| ExtensionFiles { source: Source::Preferences, ..f })
}

/// Chromium's `ManifestLocation` of `id` from the preferences (`None` until Chromium wrote them).
pub fn install_location(id: &str) -> Option<i64> {
    prefs_entry(id)?.get("location")?.as_i64()
}

/// Everything under `extensions.settings` from `Secure Preferences` merged with `Preferences`
/// (Chromium splits the map between the two files; the secure one wins). Read-only: those files are
/// MAC-protected and sta must never write them.
pub fn prefs_settings() -> BTreeMap<String, Value> {
    let Some(profile) = profile_dir() else { return BTreeMap::new() };
    let mut out: BTreeMap<String, Value> = BTreeMap::new();
    // Least trusted first, so `Secure Preferences` overwrites.
    for file in ["Preferences", "Secure Preferences"] {
        let Some(prefs) = read_json(&profile.join(file)) else { continue };
        let Some(map) = prefs.get("extensions").and_then(|e| e.get("settings")).and_then(Value::as_object) else { continue };
        for (id, entry) in map {
            if is_extension_id(id) {
                out.insert(id.clone(), entry.clone());
            }
        }
    }
    out
}

/// `extensions.commands`: `id → [(command name, assigned shortcut)]`. Chromium keys the map by
/// `"<id>: <command>"`, with the actually assigned key in `suggested_key`.
pub fn prefs_commands() -> BTreeMap<String, Vec<(String, String)>> {
    let mut out: BTreeMap<String, Vec<(String, String)>> = BTreeMap::new();
    let Some(profile) = profile_dir() else { return out };
    for file in ["Preferences", "Secure Preferences"] {
        let Some(prefs) = read_json(&profile.join(file)) else { continue };
        let Some(map) = prefs.get("extensions").and_then(|e| e.get("commands")).and_then(Value::as_object) else { continue };
        for (key, entry) in map {
            let Some((id, name)) = key.split_once(':') else { continue };
            let (id, name) = (id.trim(), name.trim());
            if !is_extension_id(id) || name.is_empty() {
                continue;
            }
            let shortcut = entry.get("suggested_key").and_then(Value::as_str).unwrap_or_default().to_string();
            let list = out.entry(id.to_string()).or_default();
            if !list.iter().any(|(n, _)| n == name) {
                list.push((name.to_string(), shortcut));
            }
        }
    }
    out
}

/// A component of the browser itself (`COMPONENT`, `EXTERNAL_COMPONENT`): never shown to the user.
pub fn is_component_location(location: i64) -> bool {
    matches!(location, 5 | 10)
}

/// Installed by enterprise policy: sta must not turn it on or off.
pub fn is_policy_location(location: i64) -> bool {
    matches!(location, 7 | 9)
}

/// Added by another program: external preferences (2), the registry (3) or an external update URL
/// (6). Policy installs (7, 9) and components are not "added by another program" for the user.
pub fn is_external_location(location: i64) -> bool {
    matches!(location, 2 | 3 | 6)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_and_pages() {
        assert!(is_extension_id("abcdefghijklmnopabcdefghijklmnop"));
        assert!(!is_extension_id("abcdefghijklmnopabcdefghijklmnoq"));
        assert!(!is_extension_id("ABCDEFGHIJKLMNOPABCDEFGHIJKLMNOP"));
        assert_eq!(normalize_page("/options.html?x=1#y").as_deref(), Some("options.html"));
        assert_eq!(normalize_page("app/app.html").as_deref(), Some("app/app.html"));
        for bad in ["", "../x.html", "a/../b.html", "a\\b.html", "https://x/y.html", "./x.html"] {
            assert_eq!(normalize_page(bad), None, "{bad}");
        }
        assert_eq!(id_from_hash(&[0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef, 0, 0, 0, 0, 0, 0, 0, 0xff]), "abcdefghijklmnopaaaaaaaaaaaaaapp");
        assert_eq!(base64_decode("aGVsbG8=").as_deref(), Some(&b"hello"[..]));
    }

    #[test]
    #[cfg(windows)]
    fn ids_match_chromium() {
        // Ids Chromium gave unpacked extensions loaded from these paths (extensions spike, run7).
        assert_eq!(id_from_path(Path::new(r"C:\ast\tmp\ext-design\ext\exts\probe")).as_deref(), Some("hddfdjogkjfobjliinodemdhkjoemdbe"));
        assert_eq!(id_from_path(Path::new(r"c:\ast\tmp\ext-design\ext\exts\probe2")).as_deref(), Some("edddjolgbfnmhdgklhdbbmeojfmmfake"));
        // The in-repo probes carry a `key`: their ids are fixed (crates/sta/e2e/extensions/README.md).
        let probes = Path::new(env!("CARGO_MANIFEST_DIR")).join("e2e").join("extensions");
        let expected = read_json(&probes.join("ids.json")).expect("e2e/extensions/ids.json");
        for (name, id) in expected.as_object().unwrap() {
            let manifest = read_json(&probes.join(name).join("manifest.json")).unwrap();
            let key = manifest.get("key").and_then(Value::as_str).unwrap();
            assert_eq!(id_from_key(key).as_deref(), id.as_str(), "{name}");
        }
    }

    #[test]
    fn manifest_reading() {
        let dir = std::env::temp_dir().join(format!("sta-ext-files-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let id = "abcdefghijklmnopabcdefghijklmnop";
        let v = dir.join(id).join("1.2_0");
        std::fs::create_dir_all(v.join("_locales/ko")).unwrap();
        std::fs::create_dir_all(v.join("_locales/en")).unwrap();
        std::fs::write(
            v.join("manifest.json"),
            "\u{feff}{\"name\":\"__MSG_appName__\",\"default_locale\":\"en\",\"options_ui\":{\"page\":\"/opts.html\"},\"action\":{\"default_popup\":\"popup.html\"},\"side_panel\":{\"default_path\":\"panel.html\"},\"web_accessible_resources\":[{\"resources\":[\"welcome.html\"],\"matches\":[\"<all_urls>\"]},{\"resources\":[\"only.html\"],\"matches\":[\"https://a.com/*\"]}]}",
        )
        .unwrap();
        std::fs::write(v.join("_locales/en/messages.json"), r#"{"appName":{"message":"Blocker"}}"#).unwrap();
        std::fs::write(v.join("_locales/ko/messages.json"), r#"{"APPNAME":{"message":"차단기"}}"#).unwrap();
        std::fs::create_dir_all(dir.join("Temp")).unwrap();
        let scanned = scan(&dir);
        assert_eq!(scanned.keys().collect::<Vec<_>>(), vec![id]);
        let e = load_installed(&dir, id).unwrap();
        assert_eq!(e.name("ko-KR"), "차단기");
        assert_eq!(e.name("de-DE"), "Blocker");
        assert_eq!(e.options_page().as_deref(), Some("opts.html"));
        assert_eq!(e.popup_page().as_deref(), Some("popup.html"));
        assert_eq!(e.side_panel_page().as_deref(), Some("panel.html"));
        assert_eq!(e.web_accessible_to_all(), vec!["welcome.html".to_string()]);
        assert!(is_external_location(3) && !is_external_location(1) && !is_external_location(4));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
