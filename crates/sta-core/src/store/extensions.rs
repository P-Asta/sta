//! Extension rules (ext design FINAL PLAN §4 "Picker", "Settings › Extensions", "Core").
//!
//! Core keeps the list the shell read off disk and decides what the user can do with it. Nothing
//! here is persisted: the profile *is* the state, and re-reading it is the only truth
//! (`Command::ExtensionsChanged`).
//!
//! The rules that matter:
//! - **Enter never turns an extension on.** An extension that is off, or that another program added
//!   and the user has not answered for, opens Settings › Extensions at its row instead
//!   ([`Store::run_extension`]). Turning one on is [`Command::SetExtensionEnabled`], which core
//!   refuses until the details (Chrome's warnings, host access, source) have been loaded and shown,
//!   and always refuses for a local CRX another program registered (D6a).
//! - **The popup card is the only place an extension's own UI runs**, and it says so honestly when
//!   it doesn't work (`ExtensionPopupClosed{failed}` → [`extensions::POPUP_FAILED_TEXT`]).
//! - Pages sta opens for an extension are built with [`urls::extension_page_url`], so a hostile
//!   manifest cannot name anything but a file inside that extension.

use super::*;
use crate::extensions::{
    ExtensionAction, ExtensionDetails, ExtensionInfo, ExtensionOp, ExtensionPopupView, ExtensionState, ExtensionsView,
};
use crate::urls;

/// Toast when an extension can only be used by clicking its toolbar button (D1a: prebuilt CEF has
/// no way to press it, and no API reaches `action.onClicked` — design report `extensions.md`).
pub const NO_ACTION_TOAST: &str = "Toolbar click isn't supported in sta";
pub const NO_OPTIONS_TOAST: &str = "This extension has no options page";
/// A popup card may not sit over a permission prompt (SEC-4), so the prompt is answered first.
pub const POPUP_PROMPT_TOAST: &str = "Answer the site's permission request first";
/// The Web Store's "Switch to Chrome to install extensions and themes" banner is wrong about sta.
pub const WEB_STORE_TOAST: &str = "Extensions from this store install in sta; ignore its \"Switch to Chrome\"";
pub const EXTERNAL_LOCAL_TOAST: &str = "sta can't turn this on: another program installed it from a file";
pub const MANAGED_TOAST: &str = "Your organization manages this extension";
pub const SAFE_MODE_TOAST: &str = "sta restarted in safe mode; extensions are listed in Settings";

/// How long a loaded disclosure stands as the user's answer. Reading Chrome's warnings and pressing
/// "Turn on anyway" takes seconds; a panel left open and forgotten must not pre-authorise a Turn on
/// minutes later (SEC-P3-5).
const DISCLOSURE_TTL_MS: Millis = 60_000;

/// Settings › Extensions, optionally scrolled to one extension.
pub fn settings_extensions_url(id: Option<&str>) -> String {
    match id.filter(|id| urls::is_extension_id(id)) {
        Some(id) => format!("sta://settings/?section=extensions&ext={id}"),
        None => "sta://settings/?section=extensions".to_string(),
    }
}

impl Store {
    /// The installed extensions, A–Z (the Ctrl+E picker and Settings read this).
    pub fn extensions(&self) -> &[ExtensionInfo] {
        &self.rt.extensions
    }

    pub fn extension(&self, id: &str) -> Option<&ExtensionInfo> {
        self.rt.extensions.iter().find(|e| e.id == id)
    }

    /// sta started in safe mode after two crashes (R-SEC-11).
    pub fn safe_mode(&self) -> bool {
        self.rt.safe_mode
    }

    pub(super) fn extensions_view(&self) -> ExtensionsView {
        ExtensionsView {
            items: self.rt.extensions.clone(),
            details: self.rt.extension_details.values().cloned().collect(),
            busy: self.rt.extension_busy.as_ref().map(|(id, _)| id.clone()),
            popup: self.rt.extension_popup.clone(),
            safe_mode: self.rt.safe_mode,
            needs_ok: self.rt.extensions.iter().filter(|e| e.state == ExtensionState::NeedsApproval).count(),
        }
    }

    /// Opens Settings › Extensions (at `id`, if given) as an internal tab.
    fn open_extension_settings(&mut self, id: Option<&str>, now: Millis, fx: &mut Vec<Effect>) {
        let url = settings_extensions_url(id);
        self.open_url(url, OpenTarget::NewTab, None, false, now, fx);
    }

    /// Opens (or re-activates) an extension's own page as a tab — **one per extension**: a second
    /// Enter on the same row switches to the tab that is already open instead of piling them up.
    ///
    /// The tab is found two ways, because an options page is free to route itself the moment it loads
    /// (`location.replace(pathname + '#general')` is what most of them do, and then the tab's URL is
    /// no longer the one sta opened): the tab this extension's page was last opened in, if it is still
    /// showing a page of that extension, and otherwise any tab at the same path (query and fragment
    /// ignored).
    fn open_extension_page(&mut self, id: &str, url: String, now: Millis, fx: &mut Vec<Effect>) {
        if let Some(existing) = self.extension_page_tab(id, &url) {
            self.activate(existing, now, fx);
            return;
        }
        self.open_url(url.clone(), OpenTarget::NewTab, None, false, now, fx);
        // `open_url` focuses what it opened, so this is the tab it just made.
        if let Some(tab) = self.focused_tab() {
            self.rt.extension_pages.insert(id.to_string(), tab);
        }
    }

    /// The tab that already shows this extension's page, if there is one.
    fn extension_page_tab(&self, id: &str, url: &str) -> Option<Id> {
        let origin = format!("chrome-extension://{id}/");
        let remembered = self
            .rt
            .extension_pages
            .get(id)
            .copied()
            .filter(|tab| matches!(self.state.items.get(tab), Some(Item::Tab(t)) if t.url.starts_with(&origin)));
        remembered.or_else(|| {
            let wanted = urls::without_query_or_fragment(url);
            self.state.items.iter().find_map(|(tab, item)| match item {
                Item::Tab(t) if urls::without_query_or_fragment(&t.url) == wanted => Some(*tab),
                _ => None,
            })
        })
    }

    fn show_extension_popup(&mut self, info: &ExtensionInfo, page: &str, fx: &mut Vec<Effect>) {
        let Some(url) = urls::extension_page_url(&info.id, page) else {
            self.toast(NO_ACTION_TOAST, None);
            return;
        };
        // SEC-4: the card never sits over a permission prompt, and `validate_runtime` enforces that
        // by closing it. Opening one anyway meant the picker's primary action created a browser and
        // killed it 7 ms later with nothing shown and nothing said — from the user's side, Enter
        // did nothing at all. Refuse it here instead, with the reason.
        if self.shown_prompt().is_some() {
            self.toast(POPUP_PROMPT_TOAST, None);
            return;
        }
        let tab = self.focused_tab();
        self.rt.seq += 1;
        self.rt.extension_popup = Some(ExtensionPopupView {
            id: info.id.clone(),
            name: info.display_name().to_string(),
            icon: crate::extensions::icon_url("command", &info.id, 32),
            tab,
            has_options: info.options.is_some(),
            failed: false,
            needs_current_tab: info.needs_current_tab,
            seq: self.rt.seq,
        });
        self.bump();
        fx.push(Effect::OpenExtensionPopup { id: info.id.clone(), url, tab });
    }

    fn close_extension_popup(&mut self, fx: &mut Vec<Effect>) {
        if self.rt.extension_popup.take().is_some() {
            fx.push(Effect::HideExtensionPopup);
            self.bump();
        }
    }

    /// Close the card when the thing it is anchored to goes away (validate_runtime, permission
    /// prompts, tab switches). The shell also closes it on blur and Esc.
    pub(super) fn close_extension_popup_if_open(&mut self, fx: &mut Vec<Effect>) {
        self.close_extension_popup(fx);
    }

    /// The tab the open popup card belongs to.
    pub(super) fn extension_popup_tab(&self) -> Option<Id> {
        self.rt.extension_popup.as_ref().and_then(|p| p.tab)
    }

    /// What Enter (and the row buttons) do. **Never** enables an extension.
    fn run_extension(&mut self, id: &str, action: ExtensionAction, now: Millis, fx: &mut Vec<Effect>) {
        let Some(info) = self.extension(id).cloned() else { return };
        match action {
            ExtensionAction::Manage => self.open_extension_settings(Some(&info.id), now, fx),
            ExtensionAction::WebStore => {
                let url = crate::extensions::web_store_url(&info.id);
                self.open_url(url, OpenTarget::NewTab, None, false, now, fx);
            }
            ExtensionAction::Options => match info.options.as_deref().and_then(|p| urls::extension_page_url(&info.id, p)) {
                Some(url) if info.state.is_on() => self.open_extension_page(&info.id, url, now, fx),
                // An options page of an extension that is off would load nothing.
                Some(_) => self.open_extension_settings(Some(&info.id), now, fx),
                None => self.toast(NO_OPTIONS_TOAST, None),
            },
            ExtensionAction::Popup => match info.popup.as_deref() {
                Some(page) if info.state.is_on() => self.show_extension_popup(&info, page, fx),
                Some(_) => self.open_extension_settings(Some(&info.id), now, fx),
                None => self.toast(NO_ACTION_TOAST, None),
            },
            ExtensionAction::Primary => {
                // Off, waiting for the user's OK, or blocked: the picker shows the user where the
                // decision lives. It never makes it for them.
                if !info.state.is_on() {
                    self.open_extension_settings(Some(&info.id), now, fx);
                    return;
                }
                if let Some(page) = info.popup.clone() {
                    self.show_extension_popup(&info, &page, fx);
                } else if let Some(url) = info.options.as_deref().and_then(|p| urls::extension_page_url(&info.id, p)) {
                    self.open_extension_page(&info.id, url, now, fx);
                } else {
                    // Nothing sta can open: its only entry point is the toolbar button. The row says
                    // that *and* that Enter opens the Web Store page (`STATUS_NO_ACTION_HINT`), so a
                    // toast repeating the row's own subtitle would add nothing (UXV-5).
                    let url = crate::extensions::web_store_url(&info.id);
                    self.open_url(url, OpenTarget::NewTab, None, false, now, fx);
                }
            }
        }
    }

    /// Starts a backend operation unless one is already running.
    fn start_op(&mut self, id: &str, op: ExtensionOp, fx: &mut Vec<Effect>) {
        if self.rt.extension_busy.is_some() {
            return;
        }
        self.rt.extension_busy = Some((id.to_string(), op));
        self.bump();
        fx.push(Effect::ExtensionOp { id: id.to_string(), op });
    }

    /// Were this extension's warnings loaded (and therefore shown) recently enough to stand as the
    /// user's answer for the Turn on happening now?
    fn disclosure_is_fresh(&self, id: &str, now: Millis) -> bool {
        self.rt.extension_details.contains_key(id)
            && self.rt.extension_disclosed.get(id).is_some_and(|at| now.saturating_sub(*at) <= DISCLOSURE_TTL_MS)
    }

    fn finish_op(&mut self, id: &str) {
        if self.rt.extension_busy.as_ref().is_some_and(|(busy, _)| busy == id) {
            self.rt.extension_busy = None;
            self.bump();
        }
    }

    pub(super) fn handle_extensions(&mut self, cmd: Command, now: Millis, fx: &mut Vec<Effect>) {
        match cmd {
            Command::RunExtension { id, action } => self.run_extension(&id, action, now, fx),
            Command::RequestExtensionDetails { id } if self.extension(&id).is_some() => {
                self.start_op(&id, ExtensionOp::GetInfo, fx)
            }
            Command::SetExtensionEnabled { id, enabled } => {
                let Some(info) = self.extension(&id).cloned() else { return };
                if info.state == ExtensionState::Enabled && enabled {
                    return;
                }
                if enabled && !info.install.may_enable() {
                    let message = match info.install {
                        crate::extensions::ExtensionInstall::Managed => MANAGED_TOAST,
                        _ => EXTERNAL_LOCAL_TOAST,
                    };
                    self.toast(message, None);
                    return;
                }
                // The disclosure is not decoration: an extension another program added is turned on
                // only after its warnings, host access and source were loaded (and therefore shown).
                // The permission is **one-shot and fresh** (SEC-P3-5): it is consumed by the Turn on
                // it belongs to, and a disclosure the user left open and walked away from stops
                // counting after [`DISCLOSURE_TTL_MS`], so every Turn on has its own.
                if enabled && info.state == ExtensionState::NeedsApproval && !self.disclosure_is_fresh(&id, now) {
                    self.start_op(&id, ExtensionOp::GetInfo, fx);
                    return;
                }
                if info.state == ExtensionState::NeedsApproval {
                    self.rt.extension_disclosed.remove(&id);
                }
                self.start_op(&id, ExtensionOp::SetEnabled { enabled }, fx);
            }
            Command::RemoveExtension { id } => {
                let Some(info) = self.extension(&id).cloned() else { return };
                if !info.install.may_remove() {
                    self.toast(MANAGED_TOAST, None);
                    return;
                }
                if self.rt.extension_popup.as_ref().is_some_and(|p| p.id == id) {
                    self.close_extension_popup(fx);
                }
                self.start_op(&id, ExtensionOp::Uninstall, fx);
            }
            Command::CloseExtensionPopup => self.close_extension_popup(fx),

            // -------------------------------------------------------------- shell events
            Command::ExtensionsChanged { extensions } => {
                let mut extensions: Vec<ExtensionInfo> =
                    extensions.into_iter().filter(|e| urls::is_extension_id(&e.id)).collect();
                extensions.sort_by(|a, b| a.display_name().to_lowercase().cmp(&b.display_name().to_lowercase()).then_with(|| a.id.cmp(&b.id)));
                extensions.dedup_by(|a, b| a.id == b.id);
                if self.rt.extensions == extensions {
                    return;
                }
                self.rt.extensions = extensions;
                let ids: BTreeSet<String> = self.rt.extensions.iter().map(|e| e.id.clone()).collect();
                self.rt.extension_details.retain(|id, _| ids.contains(id));
                self.rt.extension_disclosed.retain(|id, _| ids.contains(id));
                self.rt.extension_pages.retain(|id, _| ids.contains(id));
                // A popup whose extension is gone (or was turned off) can't be showing anything.
                if self.rt.extension_popup.as_ref().is_some_and(|p| !self.rt.extensions.iter().any(|e| e.id == p.id && e.state.is_on())) {
                    self.close_extension_popup(fx);
                }
                // A **write** ends when the listing has been re-read: that fresh listing *is* its
                // answer (`ext_backend` re-reads the profile after every operation), so the row stops
                // being busy. A `GetInfo` is answered by `ExtensionDetailsLoaded` instead, and a
                // listing that no longer has the extension ends anything pending for it.
                if let Some((busy, op)) = self.rt.extension_busy.clone()
                    && (!ids.contains(&busy) || !matches!(op, ExtensionOp::GetInfo))
                {
                    self.rt.extension_busy = None;
                }
                self.announce_external_extensions();
                self.bump();
            }
            Command::ExtensionDetailsLoaded { details } => {
                let id = details.id.clone();
                if self.extension(&id).is_none() {
                    return;
                }
                self.rt.extension_details.insert(id.clone(), details);
                self.rt.extension_disclosed.insert(id.clone(), now);
                self.finish_op(&id);
                self.bump();
            }
            Command::ExtensionOpFailed { id, op, message } => {
                let name = self.extension(&id).map(|e| e.display_name().to_string()).unwrap_or_else(|| "this extension".into());
                self.finish_op(&id);
                let detail = message.trim();
                let detail = if detail.is_empty() { String::new() } else { format!(" ({detail})") };
                self.toast(format!("Couldn't {} {name}{detail}", op.label()), None);
            }
            Command::ExtensionPopupClosed { failed } => match self.rt.extension_popup.as_mut() {
                // The popup never came up: the card stays, showing what actually happened.
                Some(popup) if failed && !popup.failed => {
                    popup.failed = true;
                    self.bump();
                }
                Some(_) if !failed => self.close_extension_popup(fx),
                _ => {}
            },
            Command::SafeModeStarted if !self.rt.safe_mode => {
                self.rt.safe_mode = true;
                self.toast(SAFE_MODE_TOAST, None);
            }
            _ => {}
        }
    }

    /// At most once per run: "N extensions were added by other programs · Review" (D11a). Only for
    /// the ones still waiting for an answer — an external extension the user already allowed is not
    /// news — and only for ids they have **not been told about before**, which is remembered in
    /// `state.announcedExternal`. Ignoring three registry extensions forever is a decision, and
    /// before this the same toast interrupted every single launch; the standing case is Settings ›
    /// Extensions' banner, which says the same thing without taking over the screen.
    fn announce_external_extensions(&mut self) {
        if self.rt.external_announced {
            return;
        }
        let mut waiting: Vec<String> =
            self.rt.extensions.iter().filter(|e| e.state == ExtensionState::NeedsApproval).map(|e| e.id.clone()).collect();
        waiting.sort();
        if waiting.is_empty() {
            return;
        }
        let fresh = waiting.iter().filter(|id| !self.state.announced_external.contains(id)).count();
        // Remember exactly the set waiting now: one that is allowed, removed or registered again
        // later is news again.
        if self.state.announced_external != waiting {
            self.state.announced_external = waiting;
            self.dirty.state = true;
        }
        if fresh == 0 {
            self.rt.external_announced = true;
            return;
        }
        self.rt.external_announced = true;
        let message = if fresh == 1 {
            "1 extension was added by another program".to_string()
        } else {
            format!("{fresh} extensions were added by other programs")
        };
        let open = Command::OpenUrl { url: settings_extensions_url(None), target: OpenTarget::NewTab, opener: None };
        self.toast(message, Some(ToastAction { label: "Review".into(), command: Box::new(open) }));
    }

    /// Safe mode restores tabs unloaded (R-SEC-11): the active item is dropped at startup, so the
    /// window opens on the empty state with every tab still in the sidebar.
    pub(super) fn apply_safe_mode_startup(&mut self) {
        if !self.rt.safe_mode {
            return;
        }
        let active = self.state.window.active_space;
        if let Some(space) = self.space_mut(active)
            && space.active_item.take().is_some()
        {
            self.dirty.state = true;
        }
    }
}

/// The details of `id` (the Turn on disclosure), if they were loaded.
impl Store {
    pub fn extension_details(&self, id: &str) -> Option<&ExtensionDetails> {
        self.rt.extension_details.get(id)
    }
}
