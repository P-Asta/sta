//! AI agent (MCP) support that doesn't need CEF (docs/MCP.md):
//! - [`channel`]: bridge ⇄ browser NDJSON messages and the endpoint file (golden-tested);
//! - [`tools`]: the tool catalog (schemas for `tools/list`) and argument types;
//! - [`policy`]: URL, site, private-network, blocked-host, scope and access rules;
//! - [`snapshot`], [`text`], [`refs`], [`keys`]: page snapshots, output budgets and untrusted-content
//!   framing, element refs, key combos;
//! - [`errors`]: tool error codes and hints;
//! - [`find`], [`console`], [`listing`]: `page_find` matching, the console ring buffer, and the
//!   `history_search` / `downloads_list` output;
//! - [`state`]: `UiState.agent` view types.
//!
//! The store keeps sessions, approval prompts, agent tabs and the activity list (commands in
//! `command.rs`, "agents" sections; handlers in `store/agent.rs`).

pub mod channel;
pub mod console;
pub mod errors;
pub mod find;
pub mod keys;
pub mod listing;
pub mod policy;
pub mod refs;
pub mod snapshot;
pub mod state;
/// The debug-only MCP test surface's catalog (docs/TESTING.md); never in a release build.
#[cfg(feature = "test-hooks")]
pub mod test_tools;
pub mod text;
pub mod tools;

pub use errors::ErrorCode;
pub use state::*;
