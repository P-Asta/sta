//! sta core: everything about the browser that does not need CEF.
//!
//! The shell (crate `sta`) owns one [`Store`] on the CEF UI thread and drives it with
//! [`Command`]s. [`Store::apply`] mutates state and returns [`Effect`]s, which the shell executes
//! against CEF *after* releasing its borrow of the store. CEF callbacks come back as more commands
//! (e.g. [`Command::TabTitleChanged`]). The HTML UI renders [`UiState`] snapshots and sends
//! commands over IPC as JSON (serde representation of [`Command`]).
//!
//! Invariants of this crate:
//! - No I/O except the explicit helpers in [`persist`]; no clocks (callers pass `now` in Unix ms);
//!   no threads. Everything is deterministic and unit-testable.
//! - All public JSON-facing types use `camelCase` field names and `"type"`/`"kind"` tags so the
//!   JavaScript UI can consume them directly. See `docs/PROTOCOL.md`.

// The debug-only MCP test surface (docs/TESTING.md) must never reach a release binary. The module
// itself is behind `debug_assertions` as well, which would silently *drop* it here instead — so
// this guard sits where a release build sees it and fails loudly.
#[cfg(all(feature = "test-hooks", not(debug_assertions)))]
compile_error!("the `test-hooks` feature is debug-only: it must never be built into a release binary");

pub mod agent;
pub mod command;
pub mod effect;
pub mod extensions;
pub mod history;
pub mod legacy;
pub mod model;
pub mod motion;
pub mod omnibox;
pub mod persist;
pub mod store;
pub mod theme;
pub mod update;
pub mod urls;
pub mod view;

pub use command::*;
pub use effect::*;
pub use extensions::{
    ExtensionAction, ExtensionCommand, ExtensionDetails, ExtensionGroup, ExtensionInfo, ExtensionInstall, ExtensionOp,
    ExtensionPopupView, ExtensionState, ExtensionsView,
};
pub use model::*;
pub use motion::{AnimationSettings, AnimationSpec, AnimationsPatch, MotionGroup, MotionLevel, MotionView};
pub use omnibox::{OmniboxRequest, OmniboxResponse, OmniboxResult, ResultGroup, ResultIcon};
pub use store::Store;
pub use update::{Asset as UpdateAsset, Manifest as UpdateManifest, UpdateStatus};
pub use view::*;

/// Identifier for persisted entities (tabs, folders, splits, spaces, boosts, archive entries).
/// Allocated from a single monotonically increasing counter stored in [`model::State::next_id`],
/// so ids are unique across entity kinds and stay below 2^53 (safe as JS numbers).
pub type Id = u64;

/// Largest valid [`Id`] (`Number.MAX_SAFE_INTEGER`). When a loaded profile has any id above
/// `MAX_ID / 2` (including ids beyond `MAX_ID`), `Store::load` renumbers every id compactly and
/// rewrites all references, and `Store::alloc_id` never returns more than `MAX_ID`, so id
/// arithmetic can never overflow.
pub const MAX_ID: Id = (1 << 53) - 1;

/// Unix time in milliseconds (UTC).
pub type Millis = i64;

/// Largest persisted timestamp (the maximum JavaScript `Date`, +275760-09-13). Loaded timestamps
/// are clamped to `0..=MAX_MILLIS`; time differences use saturating arithmetic.
pub const MAX_MILLIS: Millis = 8_640_000_000_000_000;
