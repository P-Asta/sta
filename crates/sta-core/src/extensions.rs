//! Installed extensions: the data the shell reads off disk and core ranks, groups and shows
//! (ext design FINAL PLAN §4 "Listing", "Picker", "Settings › Extensions").
//!
//! Core never touches the profile. The shell (`crates/sta/src/extensions.rs`) reads the `Extensions`
//! directory, the manifests, `_locales` and — read-only — Chromium's preferences, and pushes the
//! result in as [`crate::Command::ExtensionsChanged`]. Everything the user sees is decided here, so
//! it is unit-testable: what a row says, which group it lands in, what Enter does, and the one rule
//! that must never bend — **Enter never turns an extension on** (SEC-7/R-SEC-2: turning one on is a
//! decision the user makes in Settings, after Chrome's warnings and the source).
//!
//! Permissions (the warnings, host access and source shown before Turn on) are not part of the
//! listing: they are loaded on demand through the backend ([`ExtensionDetails`],
//! `Command::RequestExtensionDetails`).

use crate::Id;
use serde::{Deserialize, Serialize};

/// What sta can do with an extension right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ExtensionState {
    Enabled,
    /// Off, and the user (or sta) turned it off: it can be turned on again from Settings.
    Off,
    /// Another program added it and Chromium is holding it until the user allows it
    /// (`disable_reasons` bit 8192, `EXTERNAL_EXTENSION`). D6: Web Store items can be turned on from
    /// Settings after the warnings; local CRX files can only be removed.
    NeedsApproval,
    /// Off for a reason the user cannot undo here (policy, corrupt install, unsupported manifest).
    /// *Why* is [`ExtensionInfo::blocked`] — the row says which, because they are not the same thing.
    Blocked,
}

/// Why a [`ExtensionState::Blocked`] extension is off (Chromium's `disable_reason.h`, folded into the
/// distinctions a person can act on).
///
/// These used to share one sentence, "Turned off by your organization". On Chromium 152 an MV2
/// extension is disabled with `UNSUPPORTED_MANIFEST_VERSION` and a damaged profile yields
/// `CORRUPTED`, both perfectly ordinary on a personal machine with no organization at all — and
/// `GREYLIST` means Safe Browsing flagged it, which is close to the opposite. So each reason says
/// what actually happened.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ExtensionBlock {
    /// `BLOCKED_BY_POLICY`, `UPDATE_REQUIRED_BY_POLICY`, or a policy install.
    Policy,
    /// `UNSUPPORTED_MANIFEST_VERSION` (every MV2 extension on Chromium 152).
    Unsupported,
    /// `CORRUPTED`.
    Damaged,
    /// `GREYLIST` / `NOT_VERIFIED`: Safe Browsing turned it off.
    Safety,
    /// `UNSUPPORTED_REQUIREMENT`.
    Requirement,
    /// `CUSTODIAN_APPROVAL_REQUIRED` (a supervised account).
    Custodian,
    /// A disable reason sta does not know: say only what is certain.
    Unknown,
}

impl ExtensionBlock {
    pub fn status(self) -> &'static str {
        match self {
            ExtensionBlock::Policy => STATUS_BLOCKED_POLICY,
            ExtensionBlock::Unsupported => STATUS_BLOCKED_UNSUPPORTED,
            ExtensionBlock::Damaged => STATUS_BLOCKED_DAMAGED,
            ExtensionBlock::Safety => STATUS_BLOCKED_SAFETY,
            ExtensionBlock::Requirement => STATUS_BLOCKED_REQUIREMENT,
            ExtensionBlock::Custodian => STATUS_BLOCKED_CUSTODIAN,
            ExtensionBlock::Unknown => STATUS_BLOCKED_UNKNOWN,
        }
    }
}

impl ExtensionState {
    pub fn is_on(self) -> bool {
        self == ExtensionState::Enabled
    }
}

/// Where an extension came from. Decides what Settings offers (D6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ExtensionInstall {
    WebStore,
    /// `--load-extension` / a development copy.
    Unpacked,
    /// Another program registered it, and Chromium downloads it from the Web Store
    /// (`external_pref_download` / `external_registry` with an update URL).
    ExternalStore,
    /// Another program registered a CRX file on this machine. **Remove only** (D6a).
    ExternalLocal,
    /// Installed by enterprise policy, or something else sta must not touch.
    Managed,
}

impl ExtensionInstall {
    /// Another program put it there, so it starts as "Needs your OK".
    pub fn is_external(self) -> bool {
        matches!(self, ExtensionInstall::ExternalStore | ExtensionInstall::ExternalLocal)
    }

    /// May the user turn this on from Settings? A local CRX another program registered may only be
    /// removed (D6a): sta cannot show where its code came from, so it never offers to run it.
    pub fn may_enable(self) -> bool {
        !matches!(self, ExtensionInstall::ExternalLocal | ExtensionInstall::Managed)
    }

    /// May sta uninstall it?
    pub fn may_remove(self) -> bool {
        self != ExtensionInstall::Managed
    }
}

/// A keyboard command an extension declares (phase 4 assigns them; the picker only shows them).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtensionCommand {
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// The key Chromium has assigned, e.g. `Ctrl+Shift+X` (empty when unassigned).
    #[serde(default)]
    pub shortcut: String,
}

/// One installed extension, as the shell read it off disk.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtensionInfo {
    /// `[a-p]{32}`.
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub short_name: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub description: String,
    pub state: ExtensionState,
    /// Why it is blocked, when it is (`None` for every other state).
    #[serde(default)]
    pub blocked: Option<ExtensionBlock>,
    pub install: ExtensionInstall,
    /// One line naming where it came from ("Chrome Web Store", "added by another program").
    #[serde(default)]
    pub source_label: String,
    /// Relative path of the action popup page, if it has one.
    #[serde(default)]
    pub popup: Option<String>,
    #[serde(default)]
    pub options: Option<String>,
    #[serde(default)]
    pub side_panel: Option<String>,
    /// The manifest counts on `activeTab` alone. sta tells a popup which tab it was opened over, but
    /// cannot grant `activeTab` (no toolbar button, D1a), so the popup card warns before the
    /// extension's own error page is all there is (`crates/sta/src/extension_files.rs`
    /// `needs_current_tab`).
    #[serde(default)]
    pub needs_current_tab: bool,
    #[serde(default)]
    pub commands: Vec<ExtensionCommand>,
}

impl ExtensionInfo {
    /// The name shown in a row (`short_name` when the manifest has one).
    pub fn display_name(&self) -> &str {
        let name = if self.short_name.trim().is_empty() { self.name.trim() } else { self.short_name.trim() };
        if name.is_empty() { &self.id } else { name }
    }

    /// Status line of a picker row / Settings row. `None` = an enabled extension whose popup works
    /// as far as sta can tell, which needs no explanation.
    pub fn status(&self) -> Option<&'static str> {
        match self.state {
            ExtensionState::NeedsApproval => Some(STATUS_NEEDS_OK),
            ExtensionState::Off => Some(STATUS_OFF),
            ExtensionState::Blocked => Some(self.blocked.unwrap_or(ExtensionBlock::Unknown).status()),
            // An extension with neither a popup nor an options page can only be used by clicking its
            // toolbar button, which sta has no way to press (D1a: prebuilt CEF).
            ExtensionState::Enabled if self.popup.is_none() && self.options.is_none() => Some(STATUS_NO_ACTION),
            ExtensionState::Enabled => None,
        }
    }
}

pub const STATUS_OFF: &str = "Off";
/// Where the extension came from, for a row that has no "Needs your OK" heading above it (Settings).
pub const STATUS_NEEDS_OK: &str = "Added by another program · needs your OK";
/// The same row under the picker's own "Needs your OK" heading: the heading already said it.
pub const STATUS_NEEDS_OK_SHORT: &str = "Added by another program";
pub const STATUS_BLOCKED_POLICY: &str = "Turned off by your organization";
pub const STATUS_BLOCKED_UNSUPPORTED: &str = "Not supported by this version of Chrome";
pub const STATUS_BLOCKED_DAMAGED: &str = "This extension looks damaged";
pub const STATUS_BLOCKED_SAFETY: &str = "Chrome turned this off for safety";
pub const STATUS_BLOCKED_REQUIREMENT: &str = "This extension needs something sta doesn't have";
pub const STATUS_BLOCKED_CUSTODIAN: &str = "Needs a parent's approval";
pub const STATUS_BLOCKED_UNKNOWN: &str = "Chrome turned this off";
pub const STATUS_NO_ACTION: &str = "Toolbar click isn't supported in sta";
/// What Enter does for a row whose only entry point is the toolbar button, said in the row itself
/// (the press opens a Web Store tab, which nothing else would have announced).
pub const STATUS_NO_ACTION_HINT: &str = "Toolbar click isn't supported in sta · ↵ opens its Web Store page";

/// What the picker's Enter (or a row button) does with an extension.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ExtensionAction {
    /// Enter: popup card → options tab → Web Store page; Off / "needs your OK" → Settings.
    /// **Never** turns an extension on.
    Primary,
    /// Alt+Enter and the card's Options button.
    Options,
    /// The popup card alone (does nothing without a popup page).
    Popup,
    /// Its Chrome Web Store page in a tab.
    WebStore,
    /// Settings › Extensions, scrolled to that extension.
    Manage,
}

/// One operation of the hidden `chrome://extensions` backend (`crates/sta/src/ext_backend.rs`).
/// The enum is closed on purpose (SEC-6): each variant is one fixed JS template.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ExtensionOp {
    /// Permission warnings, host access and source, for the Turn on disclosure.
    GetInfo,
    SetEnabled { enabled: bool },
    Uninstall,
}

impl ExtensionOp {
    /// Short name for toasts and logs.
    pub fn label(self) -> &'static str {
        match self {
            ExtensionOp::GetInfo => "read",
            ExtensionOp::SetEnabled { enabled: true } => "turn on",
            ExtensionOp::SetEnabled { enabled: false } => "turn off",
            ExtensionOp::Uninstall => "remove",
        }
    }
}

/// What the backend answered for `RequestExtensionDetails`: Chrome's own words.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtensionDetails {
    pub id: String,
    /// Chrome's permission warnings, verbatim.
    #[serde(default)]
    pub warnings: Vec<String>,
    /// "On all sites", "On specific sites", … (empty when the extension asks for no host access).
    #[serde(default)]
    pub host_access: String,
    /// Where the code came from: "Chrome Web Store", or the CRX path another program registered.
    #[serde(default)]
    pub source: String,
}

/// The extensions half of [`crate::UiState`] (Settings › Extensions and the popup card).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtensionsView {
    /// A–Z by display name, components hidden.
    pub items: Vec<ExtensionInfo>,
    /// Details loaded for the Turn on disclosure, keyed by id (only what the user asked for).
    #[serde(default)]
    pub details: Vec<ExtensionDetails>,
    /// An operation is running for this id (rows disable themselves).
    #[serde(default)]
    pub busy: Option<String>,
    /// The open popup card (`Overlay::ExtensionPopup`), if any.
    #[serde(default)]
    pub popup: Option<ExtensionPopupView>,
    /// sta started in safe mode after two crashes: extensions may be the cause (R-SEC-11).
    #[serde(default)]
    pub safe_mode: bool,
    /// Extensions other programs added that the user has not answered for yet.
    #[serde(default)]
    pub needs_ok: usize,
}

/// The popup card's own header strip (sta draws it, never the extension).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtensionPopupView {
    pub id: String,
    pub name: String,
    /// Same-origin icon URL (`sta://command/__ext-icon/<id>/32`).
    pub icon: String,
    /// The pane the card is anchored to.
    pub tab: Option<Id>,
    pub has_options: bool,
    /// The popup never reported a size, or said it has no window: show the honest failure line
    /// instead of pretending (`FINAL PLAN` §4 "Popup card").
    #[serde(default)]
    pub failed: bool,
    /// This extension asks for the current tab, which sta cannot give a popup (D1a): the header
    /// says so, whatever the page itself paints. Without it the card for the most-installed
    /// extension in the store showed AdBlock's own "Oops!" page and nothing from sta.
    #[serde(default)]
    pub needs_current_tab: bool,
    /// Bumped whenever the card is (re)opened, so the header page resets.
    pub seq: u64,
}

/// Text the card shows when the popup never came up (UX1: a failing popup says so).
pub const POPUP_FAILED_TEXT: &str = "This popup doesn't work in sta yet";

/// Groups of the Ctrl+E picker, in display order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ExtensionGroup {
    Extensions,
    NeedsOk,
    Off,
    More,
}

impl ExtensionGroup {
    pub fn of(info: &ExtensionInfo) -> ExtensionGroup {
        match info.state {
            ExtensionState::Enabled => ExtensionGroup::Extensions,
            ExtensionState::NeedsApproval => ExtensionGroup::NeedsOk,
            ExtensionState::Off | ExtensionState::Blocked => ExtensionGroup::Off,
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            ExtensionGroup::Extensions => "Extensions",
            ExtensionGroup::NeedsOk => "Needs your OK",
            ExtensionGroup::Off => "Off",
            ExtensionGroup::More => "More",
        }
    }
}

/// Same-origin icon URL for an extension, served by the shell out of the extension's own directory
/// (UX7: never `chrome-extension://`, which a `sta://` page may not load).
pub fn icon_url(host: &str, id: &str, px: u32) -> String {
    format!("sta://{host}/__ext-icon/{id}/{px}")
}

/// Path prefix of [`icon_url`].
pub const ICON_PATH_PREFIX: &str = "__ext-icon/";

/// The hosts that serve extension icons (the picker and the settings page).
pub const ICON_HOSTS: &[&str] = &["command", "settings"];

/// The Chrome Web Store page of an extension.
pub fn web_store_url(id: &str) -> String {
    format!("https://chromewebstore.google.com/detail/{id}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(state: ExtensionState, popup: bool, options: bool) -> ExtensionInfo {
        ExtensionInfo {
            id: "a".repeat(32),
            name: "Probe".into(),
            short_name: String::new(),
            version: "1.0".into(),
            description: String::new(),
            state,
            blocked: (state == ExtensionState::Blocked).then_some(ExtensionBlock::Policy),
            install: ExtensionInstall::WebStore,
            source_label: "Chrome Web Store".into(),
            popup: popup.then(|| "popup.html".to_string()),
            options: options.then(|| "options.html".to_string()),
            side_panel: None,
            needs_current_tab: false,
            commands: Vec::new(),
        }
    }

    #[test]
    fn status_explains_only_what_needs_explaining() {
        assert_eq!(info(ExtensionState::Enabled, true, false).status(), None);
        assert_eq!(info(ExtensionState::Enabled, false, false).status(), Some(STATUS_NO_ACTION));
        assert_eq!(info(ExtensionState::Off, true, true).status(), Some(STATUS_OFF));
        assert_eq!(info(ExtensionState::NeedsApproval, true, true).status(), Some(STATUS_NEEDS_OK));
        assert_eq!(info(ExtensionState::Blocked, true, true).status(), Some(STATUS_BLOCKED_POLICY));
    }

    /// UXV-2: only a policy block may name an organization. Everything else says what happened.
    #[test]
    fn every_blocked_reason_has_its_own_sentence() {
        let mut e = info(ExtensionState::Blocked, true, true);
        for (reason, text) in [
            (ExtensionBlock::Policy, STATUS_BLOCKED_POLICY),
            (ExtensionBlock::Unsupported, STATUS_BLOCKED_UNSUPPORTED),
            (ExtensionBlock::Damaged, STATUS_BLOCKED_DAMAGED),
            (ExtensionBlock::Safety, STATUS_BLOCKED_SAFETY),
            (ExtensionBlock::Requirement, STATUS_BLOCKED_REQUIREMENT),
            (ExtensionBlock::Custodian, STATUS_BLOCKED_CUSTODIAN),
            (ExtensionBlock::Unknown, STATUS_BLOCKED_UNKNOWN),
        ] {
            e.blocked = Some(reason);
            assert_eq!(e.status(), Some(text));
            assert_eq!(text == STATUS_BLOCKED_POLICY, reason == ExtensionBlock::Policy, "{reason:?} must not name an organization");
        }
        // A reason the shell could not read at all is still honest.
        e.blocked = None;
        assert_eq!(e.status(), Some(STATUS_BLOCKED_UNKNOWN));
    }

    #[test]
    fn a_local_crx_another_program_added_can_only_be_removed() {
        assert!(!ExtensionInstall::ExternalLocal.may_enable());
        assert!(ExtensionInstall::ExternalLocal.may_remove());
        assert!(ExtensionInstall::ExternalStore.may_enable());
        assert!(!ExtensionInstall::Managed.may_enable() && !ExtensionInstall::Managed.may_remove());
        assert!(ExtensionInstall::WebStore.may_enable() && ExtensionInstall::Unpacked.may_enable());
    }

    #[test]
    fn groups_and_names() {
        assert_eq!(ExtensionGroup::of(&info(ExtensionState::Enabled, true, true)), ExtensionGroup::Extensions);
        assert_eq!(ExtensionGroup::of(&info(ExtensionState::NeedsApproval, true, true)), ExtensionGroup::NeedsOk);
        assert_eq!(ExtensionGroup::of(&info(ExtensionState::Blocked, true, true)), ExtensionGroup::Off);
        let mut e = info(ExtensionState::Enabled, true, true);
        assert_eq!(e.display_name(), "Probe");
        e.short_name = "P".into();
        assert_eq!(e.display_name(), "P");
        e.short_name = "  ".into();
        e.name = String::new();
        assert_eq!(e.display_name(), &e.id);
        assert_eq!(icon_url("command", "abc", 32), "sta://command/__ext-icon/abc/32");
    }
}
