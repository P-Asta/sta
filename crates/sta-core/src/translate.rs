//! What "Translate page" is doing to one tab, for the top bar to show.
//!
//! The shape copies [`crate::update::UpdateStatus`]: a tagged enum the shell drives with commands,
//! projected onto `UiState.current.translate`. It lives on the tab's **runtime** state, never in
//! `state.json` — a translated page is a fact about a live document, and a profile that remembered
//! it would come back claiming a page is translated that has not even loaded.
//!
//! The record of record is the page itself (`translate.js` keeps the originals). This enum is a
//! cache of that, kept so the chip can be drawn without asking the page on every repaint; the shell
//! re-asks the page on every click, so a stale cache misleads the eye but never the behaviour.

use serde::{Deserialize, Serialize};

/// A page with more prose than this is translated down to here rather than refused. Core owns the
/// number because it is the toast that has to explain it.
pub const MAX_STRINGS: usize = 4000;

/// Which half of the work is running. Text always runs first: the image overlay draws text into the
/// page, and collecting after that would send sta's own translations back to be translated again.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum TranslatePhase {
    /// Asking the page for its text.
    #[default]
    Collecting,
    /// Translating the page's own text.
    Text,
    /// Reading the text in the page's pictures and translating that.
    Images,
}

impl TranslatePhase {
    /// What the chip says while this phase runs.
    pub fn label(self) -> &'static str {
        match self {
            TranslatePhase::Collecting => "Reading page",
            TranslatePhase::Text => "Translating",
            TranslatePhase::Images => "Reading images",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(tag = "stage", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum TranslateStatus {
    /// The page is as its author wrote it.
    #[default]
    Idle,
    /// Translation is running. `total` is 0 until there is something to count.
    Working { target: String, phase: TranslatePhase, done: u32, total: u32 },
    /// The page is showing a translation. `note` carries a remark the toast also made (an OCR
    /// language that is not installed, say), so the chip's tooltip can repeat it.
    Translated { target: String, strings: u32, images: u32, note: Option<String> },
    /// The last attempt failed. The message belongs in the chip's tooltip, not in a dialog.
    Failed { message: String },
}

impl TranslateStatus {
    /// Work is in flight: a second Translate must not start, and the chip offers Stop.
    pub fn is_busy(&self) -> bool {
        matches!(self, TranslateStatus::Working { .. })
    }

    /// The page is translated right now (so the control's job is to put the original back).
    pub fn is_on(&self) -> bool {
        matches!(self, TranslateStatus::Translated { .. })
    }

    /// The language this status is about, when it is about one.
    pub fn target(&self) -> Option<&str> {
        match self {
            TranslateStatus::Working { target, .. } | TranslateStatus::Translated { target, .. } => Some(target),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_wire_shape_is_tagged_and_camel_case() {
        let working = TranslateStatus::Working { target: "en".into(), phase: TranslatePhase::Text, done: 3, total: 40 };
        let json = serde_json::to_value(&working).unwrap();
        assert_eq!(json, serde_json::json!({ "stage": "working", "target": "en", "phase": "text", "done": 3, "total": 40 }));
        assert_eq!(serde_json::to_value(TranslateStatus::Idle).unwrap(), serde_json::json!({ "stage": "idle" }));
    }

    #[test]
    fn a_status_round_trips() {
        for status in [
            TranslateStatus::Idle,
            TranslateStatus::Working { target: "ko".into(), phase: TranslatePhase::Images, done: 1, total: 2 },
            TranslateStatus::Translated { target: "en".into(), strings: 10, images: 2, note: Some("no OCR".into()) },
            TranslateStatus::Failed { message: "nope".into() },
        ] {
            let json = serde_json::to_string(&status).unwrap();
            assert_eq!(serde_json::from_str::<TranslateStatus>(&json).unwrap(), status, "{json}");
        }
    }

    #[test]
    fn busy_and_on_are_exclusive() {
        let working = TranslateStatus::Working { target: "en".into(), phase: TranslatePhase::Text, done: 0, total: 0 };
        assert!(working.is_busy() && !working.is_on());
        let done = TranslateStatus::Translated { target: "en".into(), strings: 1, images: 0, note: None };
        assert!(done.is_on() && !done.is_busy());
        assert!(!TranslateStatus::Idle.is_busy() && !TranslateStatus::Idle.is_on());
    }
}
