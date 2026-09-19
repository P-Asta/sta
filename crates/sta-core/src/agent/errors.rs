//! Tool error codes (docs/MCP.md "Errors"). Errors reach the model as `isError` tool results with
//! the text `Error [code]: message. Hint: …`, so it can recover (retry, snapshot again, ask the
//! user) instead of seeing an opaque protocol error.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    // access
    AccessOff,
    ReadOnly,
    ScriptsDisabled,
    HistoryDisabled,
    DownloadsDisabled,
    NotApproved,
    Paused,
    NotInScope,
    SiteNotApproved,
    SiteBlocked,
    // channel
    BrowserNotRunning,
    EndpointUntrusted,
    VersionMismatch,
    // target
    NoSuchTab,
    InternalPage,
    UrlNotAllowed,
    TabNotLoaded,
    TabNotVisible,
    UnsupportedFrame,
    // page
    StaleRef,
    ElementNotFound,
    ElementObscured,
    FocusLost,
    UserActive,
    DialogOpen,
    NavigationFailed,
    FileChooserBlocked,
    ScriptError,
    // limits
    Timeout,
    RateLimited,
    TooLarge,
    Busy,
    // arguments / tools
    InvalidArguments,
    UnknownTool,
    Internal,
    // The debug-only MCP test surface (docs/TESTING.md); never compiled into a release build.
    #[cfg(feature = "test-hooks")]
    TestHooksOff,
    #[cfg(feature = "test-hooks")]
    NoSuchTarget,
    #[cfg(feature = "test-hooks")]
    WindowBusy,
}

/// Every code (docs, the docs check and tests). The last three exist only in a debug build with
/// the `test-hooks` feature.
pub const ALL: &[ErrorCode] = &[
    ErrorCode::AccessOff,
    ErrorCode::ReadOnly,
    ErrorCode::ScriptsDisabled,
    ErrorCode::HistoryDisabled,
    ErrorCode::DownloadsDisabled,
    ErrorCode::NotApproved,
    ErrorCode::Paused,
    ErrorCode::NotInScope,
    ErrorCode::SiteNotApproved,
    ErrorCode::SiteBlocked,
    ErrorCode::BrowserNotRunning,
    ErrorCode::EndpointUntrusted,
    ErrorCode::VersionMismatch,
    ErrorCode::NoSuchTab,
    ErrorCode::InternalPage,
    ErrorCode::UrlNotAllowed,
    ErrorCode::TabNotLoaded,
    ErrorCode::TabNotVisible,
    ErrorCode::UnsupportedFrame,
    ErrorCode::StaleRef,
    ErrorCode::ElementNotFound,
    ErrorCode::ElementObscured,
    ErrorCode::FocusLost,
    ErrorCode::UserActive,
    ErrorCode::DialogOpen,
    ErrorCode::NavigationFailed,
    ErrorCode::FileChooserBlocked,
    ErrorCode::ScriptError,
    ErrorCode::Timeout,
    ErrorCode::RateLimited,
    ErrorCode::TooLarge,
    ErrorCode::Busy,
    ErrorCode::InvalidArguments,
    ErrorCode::UnknownTool,
    ErrorCode::Internal,
    #[cfg(feature = "test-hooks")]
    ErrorCode::TestHooksOff,
    #[cfg(feature = "test-hooks")]
    ErrorCode::NoSuchTarget,
    #[cfg(feature = "test-hooks")]
    ErrorCode::WindowBusy,
];

impl ErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            ErrorCode::AccessOff => "access_off",
            ErrorCode::ReadOnly => "read_only",
            ErrorCode::ScriptsDisabled => "scripts_disabled",
            ErrorCode::HistoryDisabled => "history_disabled",
            ErrorCode::DownloadsDisabled => "downloads_disabled",
            ErrorCode::NotApproved => "not_approved",
            ErrorCode::Paused => "paused",
            ErrorCode::NotInScope => "not_in_scope",
            ErrorCode::SiteNotApproved => "site_not_approved",
            ErrorCode::SiteBlocked => "site_blocked",
            ErrorCode::BrowserNotRunning => "browser_not_running",
            ErrorCode::EndpointUntrusted => "endpoint_untrusted",
            ErrorCode::VersionMismatch => "version_mismatch",
            ErrorCode::NoSuchTab => "no_such_tab",
            ErrorCode::InternalPage => "internal_page",
            ErrorCode::UrlNotAllowed => "url_not_allowed",
            ErrorCode::TabNotLoaded => "tab_not_loaded",
            ErrorCode::TabNotVisible => "tab_not_visible",
            ErrorCode::UnsupportedFrame => "unsupported_frame",
            ErrorCode::StaleRef => "stale_ref",
            ErrorCode::ElementNotFound => "element_not_found",
            ErrorCode::ElementObscured => "element_obscured",
            ErrorCode::FocusLost => "focus_lost",
            ErrorCode::UserActive => "user_active",
            ErrorCode::DialogOpen => "dialog_open",
            ErrorCode::NavigationFailed => "navigation_failed",
            ErrorCode::FileChooserBlocked => "file_chooser_blocked",
            ErrorCode::ScriptError => "script_error",
            ErrorCode::Timeout => "timeout",
            ErrorCode::RateLimited => "rate_limited",
            ErrorCode::TooLarge => "too_large",
            ErrorCode::Busy => "busy",
            ErrorCode::InvalidArguments => "invalid_arguments",
            ErrorCode::UnknownTool => "unknown_tool",
            ErrorCode::Internal => "internal",
            #[cfg(feature = "test-hooks")]
            ErrorCode::TestHooksOff => "test_hooks_off",
            #[cfg(feature = "test-hooks")]
            ErrorCode::NoSuchTarget => "no_such_target",
            #[cfg(feature = "test-hooks")]
            ErrorCode::WindowBusy => "window_busy",
        }
    }

    /// What the model can do about it.
    pub fn default_hint(self) -> Option<&'static str> {
        Some(match self {
            ErrorCode::AccessOff => "Ask the user to turn on AI agent access in sta Settings (AI agents).",
            ErrorCode::ReadOnly => "Agent access is read-only; ask the user to allow full access in sta Settings.",
            ErrorCode::ScriptsDisabled => "Page scripts are off for agents (or limited to the isolated world) in sta Settings; use page_snapshot, page_text and page_find, or ask the user to allow scripts.",
            ErrorCode::HistoryDisabled => "Browsing history is off for agents; ask the user to allow it in sta Settings (AI agents).",
            ErrorCode::DownloadsDisabled => "The downloads list is off for agents; ask the user to allow it in sta Settings (AI agents).",
            ErrorCode::NotApproved => "Ask the user to approve this client in the sta window, then retry.",
            ErrorCode::Paused => "The user stopped agents. Ask the user to press Resume in sta.",
            ErrorCode::NotInScope => "Use tabs_list to see the tabs you can use, open your own with tab_open, or ask the user to share the tab.",
            ErrorCode::SiteNotApproved => "Ask the user to allow this site in sta, then retry.",
            ErrorCode::SiteBlocked => "The user blocked this site for agents; don't retry.",
            ErrorCode::BrowserNotRunning => "Ask the user to open sta and turn on AI agent access.",
            ErrorCode::EndpointUntrusted => "The browser endpoint failed a security check; ask the user to restart sta.",
            ErrorCode::VersionMismatch => "sta-mcp and sta versions differ; update both.",
            ErrorCode::NoSuchTab => "Call tabs_list for the current tab ids.",
            ErrorCode::InternalPage => "sta's own pages can't be automated.",
            ErrorCode::UrlNotAllowed => "Only http(s) URLs (and about:blank) can be opened.",
            ErrorCode::TabNotLoaded => "The tab is unloaded; with full access call tab_show or tab_navigate first.",
            ErrorCode::TabNotVisible => "Call tab_show to bring the tab on screen (the sta window must not be minimized), then retry.",
            ErrorCode::UnsupportedFrame => "Elements inside cross-site frames aren't supported yet; open the frame's URL with tab_open.",
            ErrorCode::StaleRef => "The page changed. Call page_snapshot again and use the new refs.",
            ErrorCode::ElementNotFound => "Call page_snapshot again and use a ref from it.",
            ErrorCode::ElementObscured => "Another element covers it (a dialog or banner?). Close that first, scroll, or take a screenshot.",
            ErrorCode::FocusLost => "The page moved focus elsewhere; check the page with page_snapshot and retry.",
            ErrorCode::UserActive => "The user is typing in this tab; wait and retry later.",
            ErrorCode::DialogOpen => "A JavaScript dialog is open; answer it with handle_dialog.",
            ErrorCode::NavigationFailed => "Check the URL; page_text shows the error page.",
            ErrorCode::FileChooserBlocked => "File uploads aren't available to agents; ask the user to pick the file.",
            ErrorCode::ScriptError => "Your function threw or didn't compile; fix it and retry (it must be a function expression such as () => document.title).",
            ErrorCode::Timeout => "Retry, or use wait_for with a longer timeoutMs.",
            ErrorCode::RateLimited => "Slow down and retry in a moment.",
            ErrorCode::TooLarge => "Ask for less: use root, ref, offset or a smaller maxTokens.",
            ErrorCode::Busy => "Too many calls are queued for this tab; wait for earlier calls to finish.",
            ErrorCode::InvalidArguments => "Check the tool's input schema.",
            ErrorCode::UnknownTool | ErrorCode::Internal => return None,
            #[cfg(feature = "test-hooks")]
            ErrorCode::TestHooksOff => "This build has a test surface, but it is not armed (docs/TESTING.md).",
            #[cfg(feature = "test-hooks")]
            ErrorCode::NoSuchTarget => "No surface, tab or DevTools target matched the selector.",
            #[cfg(feature = "test-hooks")]
            ErrorCode::WindowBusy => "The sta window is not the foreground window; no real input was sent.",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_serialize_as_their_names() {
        assert_eq!(ALL.len(), if cfg!(feature = "test-hooks") { 38 } else { 35 });
        for &code in ALL {
            assert_eq!(serde_json::to_value(code).unwrap(), serde_json::Value::String(code.as_str().into()));
            assert_eq!(serde_json::from_value::<ErrorCode>(serde_json::Value::String(code.as_str().into())).unwrap(), code);
        }
        let unique: std::collections::HashSet<&str> = ALL.iter().map(|c| c.as_str()).collect();
        assert_eq!(unique.len(), ALL.len());
    }
}
