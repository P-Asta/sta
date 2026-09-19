//! Site permissions [owner: tabs] (ARCHITECTURE §4.1 "Permissions", docs/research/handlers.md §10.2).
//!
//! Responsibility: `PermissionHandler` logic for web tabs and `Effect::AnswerPermission`.
//! - `on_request_media_access_permission` (getUserMedia / getDisplayMedia) and
//!   `on_show_permission_prompt` (geolocation, notifications, clipboard, …) store their callback
//!   under a shell-allocated id, map the CEF bits to core `PermissionKind`s and dispatch
//!   `PermissionRequested{id, tab, origin, kinds}` (core queues a prompt or answers from a
//!   remembered decision). Media requests from browsers that are not tabs keep CEF's default
//!   (deny); prompts from them (UI pages, unmapped browsers) are dismissed at once, because CEF's
//!   Alloy default (IGNORE) leaves the page's promise pending forever.
//! - [`answer`]: allow → media `cont(requested bits)` / prompt `ACCEPT`; a remembered "no" →
//!   media `cont(0)` / prompt `DENY`; a one-off "no" (Block without Remember, stale or invalid
//!   requests) → media `cancel()` / prompt `DISMISS`, so Chromium doesn't block the origin for
//!   the rest of the session and the next request prompts again.
//! - **No auto-blocking**: Chromium's `PermissionDecisionAutoBlocker` embargoes an origin for
//!   7 days after 3 dismissed (or 4 ignored) prompts, and Chromium 152 has no feature switch for
//!   it any more (`BlockPromptsIfDismissedOften`/`…IgnoredOften` were removed). Its per-origin
//!   counters live in the `PERMISSION_AUTOBLOCKER_DATA` website setting, so the shell removes that
//!   setting for the origin after every dismissed or dropped prompt and whenever a tab commits a
//!   document of an origin that has any (this also lifts embargoes of older profiles).
//! - **Allow without Remember is "allow this time"**: CEF's `ACCEPT` stores a permanent content
//!   setting (CEF doesn't expose Chrome's `AcceptThisTime`). The shell records such grants
//!   (requesting origin, top-level origin, CEF bits) in `<profile>/one-time-permissions.json`
//!   *before* answering, and resets the matching content settings to their default when no live
//!   tab shows either origin any more (checked after main-frame commits and browser closes, not
//!   during shutdown), and at startup for everything still recorded. Remembered answers drop
//!   matching records; a grant core remembers as allowed is never reset.
//! - `on_dismiss_permission_prompt` (navigation, CEF-side timeout) → `PermissionDismissed{id}`.
//! - [`on_browser_closed`]: pending requests of a closing browser are dropped (CEF cancels a
//!   callback released without an answer) and reported dismissed.
//!
//! Public API:
//! - `pub fn startup()` — reset recorded one-time grants (after the store loaded, before windows)
//! - `pub fn on_request_media_access(browser_id: i32, origin: &str, requested: u32, callback: MediaAccessCallback) -> bool`
//! - `pub fn on_show_permission_prompt(browser_id: i32, prompt_id: u64, origin: &str, requested: u32, callback: PermissionPromptCallback) -> bool`
//! - `pub fn dismiss_unhandled_prompt(origin: &str, callback: &PermissionPromptCallback)` — UI client
//! - `pub fn on_dismiss_permission_prompt(prompt_id: u64)`
//! - `pub fn on_tab_navigated(url: &str)` — main-frame commit of a tab browser
//! - `pub fn answer(id: u64, allow: bool, remember: bool)`
//! - `pub fn on_browser_closed(browser_id: i32)`
//! - `pub fn clear()`, `pub fn debug_snapshot() -> serde_json::Value`,
//!   `pub fn debug_reset(origin: &str, bits: u32) -> Result<usize, String>` (`debug.resetPermissions`)
//! - pure helpers (unit-tested): `media_kinds`, `prompt_kinds`, `content_types`, `origin_url`,
//!   `Grants`, `remembered_allow`

use crate::{browsers, controller, paths, tabs, task, window};
use sta_core::{Command, PermissionKind, SitePermission, persist, urls};
use cef::*;
use serde::{Deserialize, Serialize};
use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet};

// cef_media_access_permission_types_t
const MEDIA_DEVICE_AUDIO: u32 = 1;
const MEDIA_DEVICE_VIDEO: u32 = 2;
const MEDIA_DESKTOP_AUDIO: u32 = 4;
const MEDIA_DESKTOP_VIDEO: u32 = 8;

// cef_permission_request_types_t
const PT_AR_SESSION: u32 = 1;
const PT_CAMERA_PAN_TILT_ZOOM: u32 = 2;
const PT_CAMERA_STREAM: u32 = 4;
const PT_CAPTURED_SURFACE_CONTROL: u32 = 8;
const PT_CLIPBOARD: u32 = 16;
const PT_TOP_LEVEL_STORAGE_ACCESS: u32 = 32;
const PT_LOCAL_FONTS: u32 = 128;
const PT_GEOLOCATION: u32 = 256;
const PT_HAND_TRACKING: u32 = 512;
const PT_IDLE_DETECTION: u32 = 2048;
const PT_MIC_STREAM: u32 = 4096;
const PT_MIDI_SYSEX: u32 = 8192;
const PT_MULTIPLE_DOWNLOADS: u32 = 16384;
const PT_NOTIFICATIONS: u32 = 32768;
const PT_KEYBOARD_LOCK: u32 = 65536;
const PT_POINTER_LOCK: u32 = 131_072;
const PT_PROTECTED_MEDIA_IDENTIFIER: u32 = 262_144;
const PT_STORAGE_ACCESS: u32 = 1_048_576;
const PT_VR_SESSION: u32 = 2_097_152;
const PT_WEB_APP_INSTALLATION: u32 = 4_194_304;
const PT_WINDOW_MANAGEMENT: u32 = 8_388_608;
const PT_FILE_SYSTEM_ACCESS: u32 = 16_777_216;
#[cfg(test)] // never reported by CEF 152; no content setting
const PT_LOCAL_NETWORK_ACCESS_DEPRECATED: u32 = 33_554_432;
const PT_LOCAL_NETWORK: u32 = 67_108_864;
const PT_LOOPBACK_NETWORK: u32 = 134_217_728;
const PT_SENSORS: u32 = 268_435_456;

/// One-time grants file in the sta profile directory.
const GRANTS_FILE: &str = "one-time-permissions.json";
/// Delay of the coalesced grant/auto-blocker sweep (lets CEF's posted prompt results run first).
const SWEEP_DELAY_MS: i64 = 100;
/// How long a reset one-time grant stays recorded: comfortably longer than Chromium's ~10 s
/// preferences write interval, so a crash before the reset reaches disk is repaired at startup.
const RESET_PERSIST_MS: i64 = 30_000;

fn now_ms() -> i64 {
    sta_core::command::now_ms()
}

enum Pending {
    Media { browser_id: i32, requested: u32, callback: MediaAccessCallback },
    Prompt { browser_id: i32, prompt_id: u64, origin: String, requested: u32, callback: PermissionPromptCallback },
}

impl Pending {
    fn browser_id(&self) -> i32 {
        match self {
            Pending::Media { browser_id, .. } | Pending::Prompt { browser_id, .. } => *browser_id,
        }
    }
}

thread_local! {
    static PENDING: RefCell<BTreeMap<u64, Pending>> = const { RefCell::new(BTreeMap::new()) };
    static NEXT_ID: Cell<u64> = const { Cell::new(1) };
    static GRANTS: RefCell<Grants> = const { RefCell::new(Grants { list: Vec::new() }) };
    /// Origins (as URLs) whose auto-blocker data is checked by the next sweep.
    static AUTOBLOCK_CHECK: RefCell<BTreeSet<String>> = const { RefCell::new(BTreeSet::new()) };
    static SWEEP_POSTED: Cell<bool> = const { Cell::new(false) };
    static RESETS: Cell<u64> = const { Cell::new(0) };
    static AUTOBLOCK_CLEARS: Cell<u64> = const { Cell::new(0) };
}

fn next_id() -> u64 {
    let id = NEXT_ID.get();
    NEXT_ID.set(id + 1);
    id
}

pub fn media_kinds(requested: u32) -> Vec<PermissionKind> {
    let mut kinds = Vec::new();
    if requested & MEDIA_DEVICE_VIDEO != 0 {
        kinds.push(PermissionKind::Camera);
    }
    if requested & MEDIA_DEVICE_AUDIO != 0 {
        kinds.push(PermissionKind::Microphone);
    }
    if requested & (MEDIA_DESKTOP_AUDIO | MEDIA_DESKTOP_VIDEO) != 0 {
        kinds.push(PermissionKind::ScreenCapture);
    }
    kinds
}

pub fn prompt_kinds(requested: u32) -> Vec<PermissionKind> {
    let table: [(u32, PermissionKind); 9] = [
        (PT_CAMERA_STREAM, PermissionKind::Camera),
        (PT_CAMERA_PAN_TILT_ZOOM, PermissionKind::Camera),
        (PT_MIC_STREAM, PermissionKind::Microphone),
        (PT_GEOLOCATION, PermissionKind::Geolocation),
        (PT_NOTIFICATIONS, PermissionKind::Notifications),
        (PT_CLIPBOARD, PermissionKind::Clipboard),
        (PT_MIDI_SYSEX, PermissionKind::MidiSysex),
        (PT_STORAGE_ACCESS, PermissionKind::StorageAccess),
        (PT_TOP_LEVEL_STORAGE_ACCESS, PermissionKind::StorageAccess),
    ];
    let mut kinds: Vec<PermissionKind> = Vec::new();
    let mut known = 0u32;
    for (bit, kind) in table {
        known |= bit;
        if requested & bit != 0 && !kinds.contains(&kind) {
            kinds.push(kind);
        }
    }
    if requested & !known != 0 {
        kinds.push(PermissionKind::Other);
    }
    kinds
}

/// Content settings that Chromium stores when a prompt of these CEF request bits is accepted
/// (Chromium's `RequestTypeToContentSettingsType`). Request types without a content setting (disk
/// quota, identity provider, protocol handlers) and CEF's deprecated local-network-access bit (never
/// reported by CEF 152) map to nothing. Every type here must be a registered *content setting* on
/// Windows: `set_content_setting` CHECK-crashes the browser process for anything else (e.g.
/// `GEOLOCATION_WITH_OPTIONS`, a website setting).
pub fn content_types(bits: u32) -> Vec<ContentSettingTypes> {
    let table: [(u32, &[ContentSettingTypes]); 25] = [
        (PT_AR_SESSION, &[ContentSettingTypes::AR]),
        (PT_CAMERA_PAN_TILT_ZOOM, &[ContentSettingTypes::CAMERA_PAN_TILT_ZOOM]),
        (PT_CAMERA_STREAM, &[ContentSettingTypes::MEDIASTREAM_CAMERA]),
        (PT_CAPTURED_SURFACE_CONTROL, &[ContentSettingTypes::CAPTURED_SURFACE_CONTROL]),
        (PT_CLIPBOARD, &[ContentSettingTypes::CLIPBOARD_READ_WRITE]),
        (PT_TOP_LEVEL_STORAGE_ACCESS, &[ContentSettingTypes::TOP_LEVEL_STORAGE_ACCESS]),
        (PT_LOCAL_FONTS, &[ContentSettingTypes::LOCAL_FONTS]),
        (PT_GEOLOCATION, &[ContentSettingTypes::GEOLOCATION]),
        (PT_HAND_TRACKING, &[ContentSettingTypes::HAND_TRACKING]),
        (PT_IDLE_DETECTION, &[ContentSettingTypes::IDLE_DETECTION]),
        (PT_MIC_STREAM, &[ContentSettingTypes::MEDIASTREAM_MIC]),
        (PT_MIDI_SYSEX, &[ContentSettingTypes::MIDI_SYSEX]),
        (PT_MULTIPLE_DOWNLOADS, &[ContentSettingTypes::AUTOMATIC_DOWNLOADS]),
        (PT_NOTIFICATIONS, &[ContentSettingTypes::NOTIFICATIONS]),
        (PT_KEYBOARD_LOCK, &[ContentSettingTypes::KEYBOARD_LOCK]),
        (PT_POINTER_LOCK, &[ContentSettingTypes::POINTER_LOCK]),
        (PT_PROTECTED_MEDIA_IDENTIFIER, &[ContentSettingTypes::PROTECTED_MEDIA_IDENTIFIER]),
        (PT_STORAGE_ACCESS, &[ContentSettingTypes::STORAGE_ACCESS]),
        (PT_VR_SESSION, &[ContentSettingTypes::VR]),
        (PT_WEB_APP_INSTALLATION, &[ContentSettingTypes::WEB_APP_INSTALLATION]),
        (PT_WINDOW_MANAGEMENT, &[ContentSettingTypes::WINDOW_MANAGEMENT]),
        (PT_FILE_SYSTEM_ACCESS, &[ContentSettingTypes::FILE_SYSTEM_WRITE_GUARD]),
        (PT_LOCAL_NETWORK, &[ContentSettingTypes::LOCAL_NETWORK]),
        (PT_LOOPBACK_NETWORK, &[ContentSettingTypes::LOOPBACK_NETWORK]),
        (PT_SENSORS, &[ContentSettingTypes::SENSORS]),
    ];
    let mut out: Vec<ContentSettingTypes> = Vec::new();
    for (bit, types) in table {
        if bits & bit != 0 {
            for ty in types {
                if !out.contains(ty) {
                    out.push(*ty);
                }
            }
        }
    }
    out
}

/// The origin of `url` as a URL string (`https://example.com/`), the form content settings and
/// the auto-blocker are keyed on; `file:` URLs share `file:///`. `None` for opaque origins.
pub fn origin_url(url: &str) -> Option<String> {
    let parsed = url::Url::parse(url.trim()).ok()?;
    if parsed.scheme() == "file" {
        return Some("file:///".into());
    }
    let origin = parsed.origin();
    origin.is_tuple().then(|| format!("{}/", origin.ascii_serialization()))
}

// ----------------------------------------------------------------------------------- one-time grants

/// A prompt answered "Allow" without "Remember".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OneTimeGrant {
    /// Requesting origin as a URL (`https://example.com/`).
    pub origin: String,
    /// Origin of the tab's main frame when the grant was made (same form).
    pub top_level: String,
    /// `cef_permission_request_types_t` bits that were allowed.
    pub bits: u32,
    /// Unix ms when the grant was reset. Chromium writes the reset to disk only on its next
    /// preferences flush (~10 s later), so a reset record stays in the file for
    /// [`RESET_PERSIST_MS`]: if the process dies before the flush, the next startup resets it again
    /// (resetting twice is harmless). `None` = the grant is still active.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reset_at: Option<i64>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct GrantsFile {
    #[serde(default)]
    grants: Vec<OneTimeGrant>,
}

/// The recorded one-time grants (pure bookkeeping; CEF work happens in the callers).
#[derive(Debug, Default)]
pub struct Grants {
    list: Vec<OneTimeGrant>,
}

impl Grants {
    /// Adds a grant (merged with one for the same origins). Returns whether the list changed.
    pub fn record(&mut self, origin: &str, top_level: &str, bits: u32) -> bool {
        if bits == 0 {
            return false;
        }
        if let Some(g) = self.list.iter_mut().find(|g| g.origin == origin && g.top_level == top_level) {
            let before = (g.bits, g.reset_at);
            // A re-grant revives a reset record (the old bits are reset again later; harmless).
            g.bits |= bits;
            g.reset_at = None;
            return (g.bits, g.reset_at) != before;
        }
        self.list.push(OneTimeGrant { origin: origin.into(), top_level: top_level.into(), bits, reset_at: None });
        true
    }

    /// Drops `bits` of every active grant for the requesting `origin` (a remembered answer replaced
    /// them).
    pub fn forget(&mut self, origin: &str, bits: u32) -> bool {
        let mut changed = false;
        for g in self.list.iter_mut().filter(|g| g.reset_at.is_none() && g.origin == origin && g.bits & bits != 0) {
            g.bits &= !bits;
            changed = true;
        }
        self.list.retain(|g| g.bits != 0);
        changed
    }

    /// Marks the active grants whose requesting and top-level origins are both no longer shown by
    /// any live tab (`live`: origin URLs of the tabs' main frames) as reset at `now`, and returns
    /// them (the caller resets their content settings).
    pub fn take_expired(&mut self, live: &BTreeSet<String>, now: i64) -> Vec<OneTimeGrant> {
        let mut expired = Vec::new();
        for g in self.list.iter_mut().filter(|g| g.reset_at.is_none()) {
            if !live.contains(&g.origin) && !live.contains(&g.top_level) {
                g.reset_at = Some(now);
                expired.push(g.clone());
            }
        }
        expired
    }

    /// Drops reset records old enough that Chromium has flushed the reset to disk. Returns whether
    /// the list changed.
    pub fn purge_reset(&mut self, now: i64) -> bool {
        let before = self.list.len();
        self.list.retain(|g| g.reset_at.is_none_or(|at| now.saturating_sub(at) < RESET_PERSIST_MS));
        self.list.len() != before
    }

    /// All records, including reset ones awaiting purge (this is what is written to disk).
    pub fn list(&self) -> &[OneTimeGrant] {
        &self.list
    }

    /// Grants that are still active (not reset).
    pub fn active(&self) -> impl Iterator<Item = &OneTimeGrant> {
        self.list.iter().filter(|g| g.reset_at.is_none())
    }
}

/// Is every kind of `bits` remembered as allowed for `origin` in core? (Such a grant is kept.)
pub fn remembered_allow(site_permissions: &[SitePermission], origin: &str, bits: u32) -> bool {
    let origin = urls::normalize_origin(origin);
    let kinds = prompt_kinds(bits);
    !kinds.is_empty() && kinds.iter().all(|k| site_permissions.iter().any(|s| s.origin == origin && s.kind == *k && s.allow))
}

fn grants_path() -> Option<std::path::PathBuf> {
    paths::try_dirs().map(|d| d.profile.join(GRANTS_FILE))
}

/// Writes the grant list synchronously (tiny, and it must be on disk before Chromium stores a
/// grant it describes).
fn save_grants() {
    let Some(path) = grants_path() else { return };
    let file = GRANTS.with(|g| GrantsFile { grants: g.borrow().list().to_vec() });
    let result = match serde_json::to_string_pretty(&file) {
        Ok(text) => persist::write_atomic(&path, &text),
        Err(e) => Err(std::io::Error::other(e)),
    };
    if let Err(e) = result {
        log_error!("cannot write {}: {e}", path.display());
    }
}

fn global_context() -> Option<RequestContext> {
    request_context_get_global_context()
}

/// Resets the content settings of a one-time grant to their default ("ask").
fn reset_grant(ctx: &RequestContext, grant: &OneTimeGrant) {
    let (origin, top_level) = (CefString::from(grant.origin.as_str()), CefString::from(grant.top_level.as_str()));
    for ty in content_types(grant.bits) {
        ctx.set_content_setting(Some(&origin), Some(&top_level), ty, ContentSettingValues::DEFAULT);
    }
    if grant.bits & PT_GEOLOCATION != 0 {
        // Geolocation with options (approximate location) is a website setting, not a content
        // setting: `set_content_setting` on it crashes the browser process.
        let with_options = ContentSettingTypes::GEOLOCATION_WITH_OPTIONS;
        if ctx.website_setting(Some(&origin), Some(&top_level), with_options).is_some() {
            ctx.set_website_setting(Some(&origin), Some(&top_level), with_options, None);
        }
    }
    RESETS.set(RESETS.get() + 1);
    log_info!("permissions: one-time grant {:#x} for {} (top level {}) reset", grant.bits, grant.origin, grant.top_level);
}

/// Schedules the removal of reset records once Chromium has persisted the resets.
fn schedule_purge() {
    task::post_ui_delayed(RESET_PERSIST_MS + 1_000, || {
        if GRANTS.with(|g| g.borrow_mut().purge_reset(now_ms())) {
            save_grants();
        }
    });
}

/// Removes the auto-blocker counters and embargo of `origin` (an origin URL), if it has any.
fn clear_autoblock(ctx: &RequestContext, origin: &str) {
    let url = CefString::from(origin);
    if ctx.website_setting(Some(&url), None, ContentSettingTypes::PERMISSION_AUTOBLOCKER_DATA).is_none() {
        return;
    }
    ctx.set_website_setting(Some(&url), None, ContentSettingTypes::PERMISSION_AUTOBLOCKER_DATA, None);
    AUTOBLOCK_CLEARS.set(AUTOBLOCK_CLEARS.get() + 1);
    log_debug!("permissions: auto-blocker data of {origin} cleared");
}

/// Startup: every grant still recorded belongs to a previous session → reset it (unless core now
/// remembers it as allowed). Call after the store loaded and before any browser exists.
pub fn startup() {
    let Some(path) = grants_path() else { return };
    let text = match persist::read_optional(&path) {
        Ok(Some(text)) => text,
        Ok(None) => return,
        Err(e) => {
            log_error!("cannot read {}: {e}", path.display());
            return;
        }
    };
    let file: GrantsFile = serde_json::from_str(&text).unwrap_or_else(|e| {
        log_warn!("{} is corrupt ({e}); ignored", path.display());
        GrantsFile::default()
    });
    if file.grants.is_empty() {
        return;
    }
    let remembered = controller::with_store(|s| s.state().site_permissions.clone()).unwrap_or_default();
    let Some(ctx) = global_context() else {
        log_error!("permissions: no global request context; one-time grants kept for the next start");
        return;
    };
    let now = now_ms();
    let mut kept = Vec::new();
    let mut reset = 0;
    for mut grant in file.grants {
        if remembered_allow(&remembered, &grant.origin, grant.bits) {
            continue;
        }
        // Reset again even if it was already marked reset: the previous process may have died
        // before Chromium flushed that reset.
        reset_grant(&ctx, &grant);
        clear_autoblock(&ctx, &grant.origin);
        grant.reset_at = Some(now);
        kept.push(grant);
        reset += 1;
    }
    log_info!("permissions: {reset} one-time grant(s) of the previous session reset at startup");
    // Keep the reset records until the resets are on disk, then drop them.
    GRANTS.with(|g| g.borrow_mut().list = kept);
    save_grants();
    schedule_purge();
}

/// Posts one coalesced sweep: expire one-time grants no live tab uses any more and clear
/// queued auto-blocker data. Nothing runs while the window is closing (startup handles it).
fn schedule_sweep() {
    if SWEEP_POSTED.get() || window::is_closing() {
        return;
    }
    SWEEP_POSTED.set(true);
    task::post_ui_delayed(SWEEP_DELAY_MS, sweep);
}

fn sweep() {
    SWEEP_POSTED.set(false);
    if window::is_closing() {
        return;
    }
    let live: BTreeSet<String> = tabs::live_tab_urls().iter().filter_map(|u| origin_url(u)).collect();
    let expired = GRANTS.with(|g| g.borrow_mut().take_expired(&live, now_ms()));
    let check = AUTOBLOCK_CHECK.with(|c| std::mem::take(&mut *c.borrow_mut()));
    if expired.is_empty() && check.is_empty() {
        return;
    }
    let Some(ctx) = global_context() else { return };
    if !expired.is_empty() {
        let remembered = controller::with_store(|s| s.state().site_permissions.clone()).unwrap_or_default();
        for grant in &expired {
            if !remembered_allow(&remembered, &grant.origin, grant.bits) {
                reset_grant(&ctx, grant);
            }
        }
        save_grants(); // records stay (marked reset) until Chromium flushed the resets
        schedule_purge();
    }
    for origin in &check {
        clear_autoblock(&ctx, origin);
    }
}

fn queue_autoblock_check(origin: &str) {
    if let Some(origin) = origin_url(origin) {
        AUTOBLOCK_CHECK.with(|c| c.borrow_mut().insert(origin));
        schedule_sweep();
    }
}

/// A tab browser committed a main-frame document (`DisplayHandler::on_address_change`).
pub fn on_tab_navigated(url: &str) {
    queue_autoblock_check(url);
    schedule_sweep();
}

// ----------------------------------------------------------------------------------- requests

/// Returns `true` if the request is handled (callback kept); `false` = CEF default (deny).
pub fn on_request_media_access(browser_id: i32, origin: &str, requested: u32, callback: MediaAccessCallback) -> bool {
    let Some(tab) = tabs::tab_for_browser(browser_id) else {
        log_debug!("media access request from non-tab browser {browser_id}: default (deny)");
        return false;
    };
    let kinds = media_kinds(requested);
    if kinds.is_empty() {
        return false;
    }
    // Agent-controlled tabs never ask the user (automation/guards.rs).
    if crate::automation::guards::dismiss_permission(browser_id) {
        callback.cancel();
        return true;
    }
    let id = next_id();
    PENDING.with(|p| p.borrow_mut().insert(id, Pending::Media { browser_id, requested, callback }));
    log_debug!("permission {id}: media {requested:#x} for tab {tab} ({origin})");
    controller::dispatch(Command::PermissionRequested { id, tab, origin: origin.to_string(), kinds });
    true
}

/// Always handles the prompt (returns `true`): a tab's prompt goes to core; any other browser's
/// prompt is dismissed right away (see [`dismiss_unhandled_prompt`]).
pub fn on_show_permission_prompt(
    browser_id: i32,
    prompt_id: u64,
    origin: &str,
    requested: u32,
    callback: PermissionPromptCallback,
) -> bool {
    let Some(tab) = tabs::tab_for_browser(browser_id) else {
        log_debug!("permission prompt {prompt_id} from non-tab browser {browser_id}: dismissed");
        dismiss_unhandled_prompt(origin, &callback);
        return true;
    };
    if crate::automation::guards::dismiss_permission(browser_id) {
        dismiss_unhandled_prompt(origin, &callback);
        return true;
    }
    let id = next_id();
    PENDING.with(|p| {
        p.borrow_mut().insert(id, Pending::Prompt { browser_id, prompt_id, origin: origin.to_string(), requested, callback })
    });
    log_debug!("permission {id}: prompt {prompt_id} {requested:#x} for tab {tab} ({origin})");
    controller::dispatch(Command::PermissionRequested { id, tab, origin: origin.to_string(), kinds: prompt_kinds(requested) });
    true
}

/// A prompt nobody shows (UI pages, browsers that are not tabs): answered `DISMISS` (CEF runs it
/// asynchronously) so the page's promise settles instead of hanging with Alloy's default IGNORE.
pub fn dismiss_unhandled_prompt(origin: &str, callback: &PermissionPromptCallback) {
    callback.cont(PermissionRequestResult::DISMISS);
    queue_autoblock_check(origin);
}

pub fn on_dismiss_permission_prompt(prompt_id: u64) {
    let dropped = PENDING.with(|p| {
        let mut p = p.borrow_mut();
        let id = p.iter().find(|(_, x)| matches!(x, Pending::Prompt { prompt_id: pid, .. } if *pid == prompt_id)).map(|(id, _)| *id)?;
        p.remove(&id).map(|x| (id, x))
    });
    if let Some((id, pending)) = dropped {
        if let Pending::Prompt { origin, .. } = &pending {
            queue_autoblock_check(origin); // an ignored prompt counts towards an embargo
        }
        drop(pending);
        controller::dispatch(Command::PermissionDismissed { id });
    }
}

/// How a pending request is answered (`Effect::AnswerPermission`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Answer {
    Allow,
    /// A lasting "no" (remembered block).
    Deny,
    /// A one-off "no": Chromium keeps asking.
    Dismiss,
}

fn answer_kind(allow: bool, remember: bool) -> Answer {
    match (allow, remember) {
        (true, _) => Answer::Allow,
        (false, true) => Answer::Deny,
        (false, false) => Answer::Dismiss,
    }
}

/// `Effect::AnswerPermission`.
pub fn answer(id: u64, allow: bool, remember: bool) {
    let Some(pending) = PENDING.with(|p| p.borrow_mut().remove(&id)) else {
        log_debug!("AnswerPermission({id}): no pending request");
        return;
    };
    let kind = answer_kind(allow, remember);
    log_debug!("permission {id}: {kind:?}{}", if remember { " (remembered)" } else { "" });
    match pending {
        Pending::Media { requested, callback, .. } => match kind {
            // getUserMedia requires allowed == requested; 0 denies. CEF stores nothing for media.
            Answer::Allow => callback.cont(requested),
            Answer::Deny => callback.cont(0),
            Answer::Dismiss => callback.cancel(),
        },
        Pending::Prompt { browser_id, origin, requested, callback, .. } => {
            let origin_key = origin_url(&origin);
            if let Some(key) = &origin_key {
                let changed = match kind {
                    // Allow without Remember: a one-time grant.
                    Answer::Allow if !remember => {
                        let top_level = browsers::browser(browser_id)
                            .and_then(|b| b.main_frame())
                            .and_then(|f| origin_url(&CefString::from(&f.url()).to_string()))
                            .unwrap_or_else(|| key.clone());
                        GRANTS.with(|g| g.borrow_mut().record(key, &top_level, requested))
                    }
                    // A remembered decision replaces a one-time grant it covers.
                    Answer::Allow | Answer::Deny => GRANTS.with(|g| g.borrow_mut().forget(key, requested)),
                    Answer::Dismiss => false,
                };
                if changed {
                    save_grants(); // on disk before Chromium stores the grant it describes
                }
            }
            callback.cont(match kind {
                Answer::Allow => PermissionRequestResult::ACCEPT,
                Answer::Deny => PermissionRequestResult::DENY,
                Answer::Dismiss => PermissionRequestResult::DISMISS,
            });
            if kind == Answer::Dismiss {
                // Chromium recorded the dismissal synchronously: never let it add up to an embargo.
                if let (Some(ctx), Some(key)) = (global_context(), &origin_key) {
                    clear_autoblock(&ctx, key);
                }
                queue_autoblock_check(&origin);
            }
        }
    }
}

/// Drops the pending requests of a closing browser (their callbacks cancel on release).
pub fn on_browser_closed(browser_id: i32) {
    let gone: Vec<(u64, Pending)> = PENDING.with(|p| {
        let mut p = p.borrow_mut();
        let ids: Vec<u64> = p.iter().filter(|(_, x)| x.browser_id() == browser_id).map(|(id, _)| *id).collect();
        ids.into_iter().filter_map(|id| p.remove(&id).map(|x| (id, x))).collect()
    });
    for (id, pending) in gone {
        if let Pending::Prompt { origin, .. } = &pending {
            queue_autoblock_check(origin);
        }
        drop(pending);
        controller::dispatch(Command::PermissionDismissed { id });
    }
    // The last tab of a one-time grant's origin may be gone.
    schedule_sweep();
}

pub fn clear() {
    let taken = PENDING.with(|p| std::mem::take(&mut *p.borrow_mut()));
    drop(taken);
    AUTOBLOCK_CHECK.with(|c| c.borrow_mut().clear());
}

/// `debug.resetPermissions`: resets the content settings `bits` map to for `origin` (the path an
/// expired one-time grant takes). Returns how many content setting types were reset.
#[cfg_attr(not(debug_assertions), allow(dead_code))] // used by debug.rs only
pub fn debug_reset(origin: &str, bits: u32) -> Result<usize, String> {
    let origin = origin_url(origin).ok_or("no origin")?;
    let ctx = global_context().ok_or("no request context")?;
    reset_grant(&ctx, &OneTimeGrant { origin: origin.clone(), top_level: origin, bits, reset_at: None });
    Ok(content_types(bits).len())
}

/// Permission bookkeeping for `debug.info`.
#[cfg_attr(not(debug_assertions), allow(dead_code))] // used by debug.rs only
pub fn debug_snapshot() -> serde_json::Value {
    serde_json::json!({
        "pending": PENDING.with(|p| p.borrow().len()),
        "oneTimeGrants": GRANTS.with(|g| serde_json::to_value(g.borrow().active().collect::<Vec<_>>()).unwrap_or_default()),
        "resetGrantsAwaitingFlush": GRANTS.with(|g| g.borrow().list().iter().filter(|x| x.reset_at.is_some()).count()),
        "grantResets": RESETS.get(),
        "autoblockClears": AUTOBLOCK_CLEARS.get(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remember_decides_between_deny_and_dismiss() {
        assert_eq!(answer_kind(true, true), Answer::Allow);
        assert_eq!(answer_kind(true, false), Answer::Allow);
        assert_eq!(answer_kind(false, true), Answer::Deny);
        assert_eq!(answer_kind(false, false), Answer::Dismiss);
    }

    #[test]
    fn maps_media_bits() {
        assert_eq!(media_kinds(3), vec![PermissionKind::Camera, PermissionKind::Microphone]);
        assert_eq!(media_kinds(8), vec![PermissionKind::ScreenCapture]);
        assert!(media_kinds(0).is_empty());
    }

    #[test]
    fn maps_prompt_bits() {
        assert_eq!(prompt_kinds(PT_GEOLOCATION | PT_NOTIFICATIONS), vec![PermissionKind::Geolocation, PermissionKind::Notifications]);
        assert_eq!(prompt_kinds(PT_CAMERA_STREAM | PT_CAMERA_PAN_TILT_ZOOM), vec![PermissionKind::Camera]);
        assert_eq!(prompt_kinds(1 << 23), vec![PermissionKind::Other]);
    }

    #[test]
    fn maps_prompt_bits_to_content_settings() {
        assert_eq!(content_types(PT_GEOLOCATION), vec![ContentSettingTypes::GEOLOCATION]);
        assert_eq!(content_types(PT_NOTIFICATIONS), vec![ContentSettingTypes::NOTIFICATIONS]);
        assert_eq!(content_types(PT_CLIPBOARD), vec![ContentSettingTypes::CLIPBOARD_READ_WRITE]);
        assert_eq!(content_types(PT_MIDI_SYSEX), vec![ContentSettingTypes::MIDI_SYSEX]);
        assert_eq!(
            content_types(PT_STORAGE_ACCESS | PT_TOP_LEVEL_STORAGE_ACCESS),
            vec![ContentSettingTypes::TOP_LEVEL_STORAGE_ACCESS, ContentSettingTypes::STORAGE_ACCESS]
        );
        assert_eq!(content_types(PT_CAMERA_STREAM | PT_MIC_STREAM), vec![ContentSettingTypes::MEDIASTREAM_CAMERA, ContentSettingTypes::MEDIASTREAM_MIC]);
        assert_eq!(content_types(PT_MULTIPLE_DOWNLOADS), vec![ContentSettingTypes::AUTOMATIC_DOWNLOADS]);
        assert_eq!(content_types(PT_SENSORS), vec![ContentSettingTypes::SENSORS]);
        // Disk quota, identity provider, protocol handlers, deprecated local network access: none.
        const NONE: [u32; 4] = [64, 1024, 524_288, PT_LOCAL_NETWORK_ACCESS_DEPRECATED];
        assert!(content_types(NONE.iter().fold(0, |a, b| a | b)).is_empty());
        assert!(!content_types(u32::MAX).contains(&ContentSettingTypes::GEOLOCATION_WITH_OPTIONS), "a website setting");
        // Every other bit of the CEF enum up to SENSORS maps to something.
        for shift in 0..=28 {
            let bit = 1u32 << shift;
            if !NONE.contains(&bit) {
                assert!(!content_types(bit).is_empty(), "bit {bit:#x}");
            }
        }
    }

    #[test]
    fn origin_urls() {
        assert_eq!(origin_url("https://Example.com:443/a?b#c").as_deref(), Some("https://example.com/"));
        assert_eq!(origin_url("http://127.0.0.1:8397/perm.html").as_deref(), Some("http://127.0.0.1:8397/"));
        assert_eq!(origin_url("http://127.0.0.1:8397/").as_deref(), Some("http://127.0.0.1:8397/"));
        assert_eq!(origin_url("file:///C:/x/page.html").as_deref(), Some("file:///"));
        assert_eq!(origin_url("data:text/html,x"), None);
        assert_eq!(origin_url("about:blank"), None);
        assert_eq!(origin_url(""), None);
    }

    #[test]
    fn one_time_grants_expire_with_the_last_tab_of_their_origins() {
        let mut g = Grants::default();
        let (a, b, frame) = ("https://a.com/", "https://b.com/", "https://widgets.example/");
        assert!(g.record(a, a, PT_GEOLOCATION));
        assert!(g.record(a, a, PT_NOTIFICATIONS), "merged into the same record");
        assert!(!g.record(a, a, PT_NOTIFICATIONS), "no change");
        assert!(!g.record(a, a, 0));
        assert!(g.record(frame, b, PT_STORAGE_ACCESS), "an embedded requester lives with its top-level page");
        assert_eq!(g.list().len(), 2);
        assert_eq!(g.list()[0].bits, PT_GEOLOCATION | PT_NOTIFICATIONS);

        let live = |xs: &[&str]| xs.iter().map(|s| s.to_string()).collect::<BTreeSet<_>>();
        assert!(g.take_expired(&live(&[a, b]), 1_000).is_empty(), "both origins still shown");
        assert!(g.take_expired(&live(&[a, frame]), 1_000).is_empty(), "the requester itself is shown");
        let expired = g.take_expired(&live(&[b]), 1_000);
        assert_eq!(
            expired,
            vec![OneTimeGrant { origin: a.into(), top_level: a.into(), bits: PT_GEOLOCATION | PT_NOTIFICATIONS, reset_at: Some(1_000) }]
        );
        assert!(g.take_expired(&live(&[]), 1_000).len() == 1, "the embedded grant expires too");
        assert!(g.take_expired(&live(&[]), 2_000).is_empty(), "reset records are not reset twice by sweeps");
        assert_eq!(g.active().count(), 0);
        // Reset records stay on disk until Chromium has flushed the resets.
        assert_eq!(g.list().len(), 2);
        assert!(!g.purge_reset(1_000 + RESET_PERSIST_MS - 1));
        assert!(g.purge_reset(1_000 + RESET_PERSIST_MS));
        assert!(g.list().is_empty());

        // A re-grant revives a reset record instead of adding a second one.
        g.record(a, a, PT_GEOLOCATION);
        g.take_expired(&live(&[]), 5_000);
        assert!(g.record(a, a, PT_GEOLOCATION), "revived");
        assert_eq!(g.list().len(), 1);
        assert_eq!(g.active().count(), 1);
        assert!(!g.purge_reset(5_000 + RESET_PERSIST_MS), "active records are never purged");
        g.list.clear();

        // A remembered answer replaces the one-time part it covers.
        g.record(a, a, PT_GEOLOCATION | PT_NOTIFICATIONS);
        assert!(g.forget(a, PT_GEOLOCATION));
        assert_eq!(g.list()[0].bits, PT_NOTIFICATIONS);
        assert!(!g.forget(b, PT_NOTIFICATIONS), "other origin");
        assert!(g.forget(a, PT_NOTIFICATIONS));
        assert!(g.list().is_empty(), "empty records are dropped");
    }

    #[test]
    fn grants_file_round_trip() {
        let file = GrantsFile { grants: vec![OneTimeGrant { origin: "https://a.com/".into(), top_level: "https://a.com/".into(), bits: 256, reset_at: None }] };
        let text = serde_json::to_string(&file).unwrap();
        assert_eq!(text, r#"{"grants":[{"origin":"https://a.com/","topLevel":"https://a.com/","bits":256}]}"#);
        let back: GrantsFile = serde_json::from_str(&text).unwrap();
        assert_eq!(back.grants, file.grants);
        assert!(serde_json::from_str::<GrantsFile>("{}").unwrap().grants.is_empty());
    }

    #[test]
    fn remembered_allows_are_never_reset() {
        let perms = vec![
            SitePermission { origin: "https://a.com".into(), kind: PermissionKind::Geolocation, allow: true },
            SitePermission { origin: "https://a.com".into(), kind: PermissionKind::Notifications, allow: false },
        ];
        assert!(remembered_allow(&perms, "https://A.com/", PT_GEOLOCATION));
        assert!(!remembered_allow(&perms, "https://a.com/", PT_NOTIFICATIONS), "a remembered block is no allow");
        assert!(!remembered_allow(&perms, "https://a.com/", PT_GEOLOCATION | PT_NOTIFICATIONS));
        assert!(!remembered_allow(&perms, "https://b.com/", PT_GEOLOCATION));
    }
}
