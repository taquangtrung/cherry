//! Saved undo histories: a document's history taken out of the editor, to
//! keep across sessions as Vim's `undofile` does, and put back.
//!
//! A [`SavedHistory`] holds the whole undo tree and a fingerprint of the
//! text it reached. With the `serde` feature it serializes, for the host to
//! write next to the file (or in its own store) and read back when the
//! file opens again. Putting it back checks first that the text is the one
//! it was saved with and that the history holds together, so a history for
//! a file changed elsewhere, or a damaged one, is refused instead of
//! corrupting the undo steps.

use thiserror::Error;

use crate::history::History;

use super::{DocumentId, Editor};

// ========================================================================
// Constants
// ========================================================================

/// The offset basis of the 64-bit FNV-1a hash.
const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;

/// The prime of the 64-bit FNV-1a hash.
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

// ========================================================================
// Data Structures
// ========================================================================

/// Why a saved history was not put back.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
#[non_exhaustive]
pub enum HistoryError {
    /// The history does not hold together (a damaged or hand-edited file).
    #[error("the saved undo history is damaged")]
    Damaged,
    /// The editor has no such document.
    #[error("no such document")]
    NoDocument,
    /// The document's text is not the one the history was saved with.
    #[error("the text changed since the undo history was saved")]
    TextChanged,
}

/// A document's undo history, taken out of the editor with
/// [`Editor::saved_history`] and put back with [`Editor::restore_history`].
#[derive(Clone, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
pub struct SavedHistory {
    /// The undo tree.
    history: History,
    /// The FNV-1a hash of the text the history reached, a stable one, so a
    /// file saved by one build of the host reads back in another.
    text_hash: u64,
}

// ========================================================================
// Editor Implementations
// ========================================================================

impl Editor {
    /// The undo history of the document `id` as it stands, to keep and put
    /// back later; `None` for a document the editor lacks.
    pub fn saved_history(&self, id: DocumentId) -> Option<SavedHistory> {
        let open = self.documents.get(id)?;
        Some(SavedHistory {
            history: open.history.clone(),
            text_hash: text_hash(&open.document.to_string()),
        })
    }

    /// Puts `saved` back as the undo history of the document `id`, so undo
    /// walks back through the steps of an earlier session, and counts the
    /// text as saved at the step it reached. Refused, changing nothing,
    /// when the document's text is not the one `saved` was taken with, when
    /// `saved` does not hold together, or for a document the editor lacks.
    pub fn restore_history(
        &mut self,
        id: DocumentId,
        saved: SavedHistory,
    ) -> Result<(), HistoryError> {
        let open = self.documents.get_mut(id).ok_or(HistoryError::NoDocument)?;
        let text = open.document.to_string();
        if text_hash(&text) != saved.text_hash {
            return Err(HistoryError::TextChanged);
        }
        if !saved.history.is_consistent_with(open.document.len_chars()) {
            return Err(HistoryError::Damaged);
        }
        open.history = saved.history;
        open.saved_revision = open.history.revision();
        Ok(())
    }
}

// ========================================================================
// Functions
// ========================================================================

/// The 64-bit FNV-1a hash of `text`'s UTF-8 bytes.
fn text_hash(text: &str) -> u64 {
    text.bytes().fold(FNV_OFFSET, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(FNV_PRIME)
    })
}

// ========================================================================
// Tests
// ========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::options::EditorOptions;
    use crate::testing::Harness;

    /// A history saved after typing ` two` after `one`, with the text it
    /// reached.
    fn saved_after_typing() -> (SavedHistory, String) {
        let mut harness = Harness::new("one|");
        harness.keys("a SPC two <esc>");
        let document = harness.editor.view(harness.view).expect("open").document();
        let saved = harness.editor.saved_history(document).expect("open");
        let text = harness.marked().replace('|', "");
        (saved, text)
    }

    /// A harness editing `text` with its cursor at the end.
    fn reopened(text: &str) -> Harness {
        Harness::with_options(&format!("{text}|"), EditorOptions::default())
    }

    #[test]
    fn test_a_restored_history_undoes_the_earlier_sessions_steps() {
        let (saved, text) = saved_after_typing();
        let mut harness = reopened(&text);
        let document = harness.editor.view(harness.view).expect("open").document();
        assert_eq!(harness.editor.restore_history(document, saved), Ok(()));
        assert!(
            !harness.editor.is_modified(document),
            "saved where it stands"
        );
        harness.keys("u");
        assert_eq!(harness.marked().replace('|', ""), "one");
    }

    #[test]
    fn test_a_history_for_other_text_is_refused() {
        let (saved, _) = saved_after_typing();
        let mut harness = reopened("one three");
        let document = harness.editor.view(harness.view).expect("open").document();
        let restored = harness.editor.restore_history(document, saved);
        assert_eq!(restored, Err(HistoryError::TextChanged));
        harness.keys("u");
        assert_eq!(
            harness.marked().replace('|', ""),
            "one three",
            "nothing to undo"
        );
    }

    #[test]
    fn test_the_text_hash_is_stable() {
        assert_eq!(text_hash(""), FNV_OFFSET);
        assert_eq!(text_hash("a"), 0xaf63_dc4c_8601_ec8c);
    }

    #[cfg(feature = "serde")]
    #[test]
    fn test_a_history_round_trips_through_json_and_a_damaged_one_is_refused() {
        let (saved, text) = saved_after_typing();
        let json = serde_json::to_string(&saved).expect("serializes");
        let read: SavedHistory = serde_json::from_str(&json).expect("deserializes");
        let mut harness = reopened(&text);
        let document = harness.editor.view(harness.view).expect("open").document();
        assert_eq!(harness.editor.restore_history(document, read), Ok(()));
        let damaged = json.replacen("\"len_after\":7", "\"len_after\":70", 1);
        assert_ne!(damaged, json, "the test damages the history");
        let read: SavedHistory = serde_json::from_str(&damaged).expect("deserializes");
        assert_eq!(
            harness.editor.restore_history(document, read),
            Err(HistoryError::Damaged)
        );
    }
}
