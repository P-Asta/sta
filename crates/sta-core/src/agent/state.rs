//! Agent view types (`UiState.agent`) and the client identity the shell reports.

use crate::model::AgentAccess;
use crate::{Id, Millis};
use serde::{Deserialize, Serialize};

/// Who is connecting. `name`/`title`/`version` come from the MCP client (self-reported); `exe`
/// and `signer` are determined by the browser for the process that hosts the bridge. `verified` =
/// `exe` carries a valid Authenticode signature (only verified hosts can be allowed permanently).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AgentClientInfo {
    pub name: String,
    pub title: Option<String>,
    pub version: Option<String>,
    pub exe: Option<String>,
    pub signer: Option<String>,
    pub verified: bool,
}

impl AgentClientInfo {
    /// Display name: title, else name, else "Unknown client".
    pub fn display_name(&self) -> String {
        let t = self.title.as_deref().map(str::trim).filter(|t| !t.is_empty());
        let n = Some(self.name.trim()).filter(|n| !n.is_empty());
        t.or(n).unwrap_or("Unknown client").chars().take(80).collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentSessionView {
    pub session: u64,
    pub client: AgentClientInfo,
    pub access: AgentAccess,
    pub started_at: Millis,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum AgentPromptKind {
    /// A new client wants to connect.
    Connection { client: AgentClientInfo },
    /// An agent wants to open or act on a site for the first time.
    Site { session: u64, site: String, tab: Option<Id> },
    /// An agent asks the user to share a tab (`request_tab_access`). `reason` is the agent's own
    /// words (at most 300 characters), shown as such.
    Tab { session: u64, tab: Id, reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentPromptView {
    pub id: u64,
    #[serde(flatten)]
    pub kind: AgentPromptKind,
    pub requested_at: Millis,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentActivityView {
    pub session: u64,
    pub tool: String,
    pub tab: Option<Id>,
    /// Site (registrable domain) the tool acted on, never a full URL.
    pub site: Option<String>,
    pub at: Millis,
    /// Error code of a failed call (omitted when it succeeded).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentHeldDownloadView {
    pub id: u32,
    pub tab: Option<Id>,
    pub file_name: String,
}

/// `UiState.agent`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentView {
    /// The user pressed Stop: agents are refused until Resume (not saved).
    pub paused: bool,
    pub sessions: Vec<AgentSessionView>,
    /// Oldest first; the approval overlay shows the first.
    pub prompts: Vec<AgentPromptView>,
    /// The last actions, newest first (at most 5).
    pub activity: Vec<AgentActivityView>,
    /// Downloads started by agent actions, waiting for Keep / Discard.
    pub held_downloads: Vec<AgentHeldDownloadView>,
    /// The activity panel (topbar chip) is open.
    #[serde(default)]
    pub panel_open: bool,
    /// Tabs agents opened that are still open (what "Archive agent tabs" would archive).
    #[serde(default)]
    pub opened_tabs: usize,
}
