//! Document change events: what a host reads to keep up with the text, such
//! as a language server client syncing a document incrementally, a tab
//! showing an unsaved dot, or a highlighter of its own reparsing.
//!
//! # Versions
//!
//! Every open document has a version, 0 when it is opened, which each
//! change of its text raises by one, whatever made it: typing, an
//! operator, undo and redo, an ex command, `:g`, a host's
//! [`Editor::apply_edits`](crate::Editor::apply_edits) or
//! [`Editor::insert_lines`](crate::Editor::insert_lines). A command that
//! leaves the text as it was makes no change and keeps the version. See
//! [`Editor::document_version`](crate::Editor::document_version).
//!
//! # Tracking
//!
//! A host that wants the changes themselves turns tracking on for a
//! document with
//! [`Editor::set_change_tracking`](crate::Editor::set_change_tracking), and
//! from then on the editor queues a [`DocumentChange`] for each change,
//! which the host drains with
//! [`Editor::take_changes`](crate::Editor::take_changes) after feeding
//! keys. An untracked document queues nothing and works out no positions,
//! so a host that does not listen pays only for the version count. A
//! tracked document's queue grows until drained.

use std::mem;
use std::ops::Range;

use super::document::{Document, Utf16Position};
use super::transaction::ChangeSet;

// ========================================================================
// Data Structures
// ========================================================================

/// One change of a document's text, as queued for a host that tracks the
/// document's changes.
///
/// New fields may come in a minor release.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct DocumentChange {
    /// The change as a whole, from the text at the version before to the
    /// text at [`version`](Self::version); a host composes the changes
    /// of several versions into one with [`ChangeSet::compose`].
    pub changes: ChangeSet,
    /// The edits the change made, last in the text first, so that a host
    /// applying them one after another, in this order, to the text it had
    /// at the version before reaches the text at this version: each
    /// range is of the text as it is when that edit applies, which the
    /// edits before it in the list left alone. This is the order a
    /// Language Server Protocol `didChange` notification takes them in.
    pub edits: Vec<TextChange>,
    /// The version of the document after the change.
    pub version: u64,
}

/// One replacement in a [`DocumentChange`]: a range of the text and the text
/// that takes its place, the range given both as char offsets and as
/// lines and UTF-16 columns.
///
/// New fields may come in a minor release.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct TextChange {
    /// Where the range ends, exclusive, as a line and a UTF-16 column.
    pub end: Utf16Position,
    /// The chars replaced, as offsets.
    pub range: Range<usize>,
    /// Where the range starts, as a line and a UTF-16 column.
    pub start: Utf16Position,
    /// The text put in their place, with `\n` line breaks.
    pub text: String,
}

/// A document's version and the changes queued for a host tracking them.
#[derive(Debug, Default)]
pub(crate) struct ChangeLog {
    /// The changes not yet taken, or `None` while the host does not track
    /// them.
    queue: Option<Vec<DocumentChange>>,
    /// The version of the text, raised by every change.
    version: u64,
}

// ========================================================================
// ChangeLog Implementations
// ========================================================================

impl ChangeLog {
    /// Whether a host tracks the changes, so they are queued.
    pub(crate) fn is_tracking(&self) -> bool {
        self.queue.is_some()
    }

    /// Records that `changes` were made to the text, which was `before`
    /// them: raises the version unless they change nothing, and queues them
    /// while tracking. `before` is needed only while tracking.
    pub(crate) fn record(&mut self, changes: &ChangeSet, before: Option<&Document>) {
        if changes.is_identity() {
            return;
        }
        self.version += 1;
        if let (Some(queue), Some(before)) = (&mut self.queue, before) {
            queue.push(DocumentChange::new(self.version, changes, before));
        }
    }

    /// Starts or stops queueing changes; stopping drops those not taken.
    pub(crate) fn set_tracking(&mut self, tracking: bool) {
        match (tracking, self.queue.is_some()) {
            (true, false) => self.queue = Some(Vec::new()),
            (false, _) => self.queue = None,
            (true, true) => {}
        }
    }

    /// The changes queued since they were last taken, oldest first.
    pub(crate) fn take(&mut self) -> Vec<DocumentChange> {
        self.queue.as_mut().map(mem::take).unwrap_or_default()
    }

    /// The version of the text.
    pub(crate) fn version(&self) -> u64 {
        self.version
    }
}

// ========================================================================
// DocumentChange Implementations
// ========================================================================

impl DocumentChange {
    /// The change `changes` made to the text `before`, reaching `version`.
    fn new(version: u64, changes: &ChangeSet, before: &Document) -> Self {
        let position = |offset| {
            before
                .char_to_utf16_position(offset)
                .expect("an edit of the change fits the text before it")
        };
        let edits = changes
            .edits()
            .into_iter()
            .rev()
            .map(|edit| TextChange {
                end: position(edit.range.end),
                start: position(edit.range.start),
                range: edit.range,
                text: edit.text,
            })
            .collect();
        Self {
            changes: changes.clone(),
            edits,
            version,
        }
    }
}

// ========================================================================
// Tests
// ========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transaction::Edit;

    #[test]
    fn test_edits_come_last_first_with_utf16_ends_and_apply_in_turn() {
        let before = Document::new("😀ab\ncd\nef");
        let edits = vec![
            Edit::new(1..2, "X"),
            Edit::new(4..5, ""),
            Edit::new(9..9, "!"),
        ];
        let changes = ChangeSet::from_edits(before.len_chars(), edits).unwrap();
        let mut log = ChangeLog::default();
        log.record(&changes, Some(&before));
        assert!(log.take().is_empty(), "nothing queued until tracked");
        log.set_tracking(true);
        log.record(&changes, Some(&before));
        let queued = log.take();
        assert_eq!(log.version(), 2);
        assert_eq!(queued[0].version, 2);
        let starts: Vec<_> = queued[0].edits.iter().map(|e| e.range.start).collect();
        assert_eq!(starts, [9, 4, 1]);
        let first = &queued[0].edits[2];
        let utf16 = |line, column| Utf16Position { column, line };
        assert_eq!((first.start, first.end), (utf16(0, 2), utf16(0, 3)));
        let mut text = before.clone();
        for edit in &queued[0].edits {
            let start = text.utf16_position_to_char(edit.start).unwrap();
            let end = text.utf16_position_to_char(edit.end).unwrap();
            text.delete(start..end).unwrap();
            text.insert(start, &edit.text).unwrap();
        }
        assert_eq!(text.to_string(), "😀Xb\nd\nef!");
        let nothing = ChangeSet::from_edits(text.len_chars(), Vec::new()).unwrap();
        log.record(&nothing, Some(&text));
        assert_eq!(log.version(), 2, "a change of nothing keeps the version");
    }
}
