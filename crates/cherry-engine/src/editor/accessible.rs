//! What a screen reader needs of a view, as plain data: the lines in
//! sight, the cursor, the selections, and the mode.
//!
//! Cherry draws into cells and knows nothing of the platform's
//! accessibility interface, so the host bridges the two: after each key,
//! it takes an [`AccessibleView`] and hands it to its accessibility layer
//! (AccessKit, say, as a text field with these lines, this caret, and these
//! selections), and passes on the notices the editor gives as
//! announcements. Positions are lines and chars, as the document counts
//! them.

use crate::document::Position;
use crate::view::{Mode, is_visual};
use crate::visual::selected_ranges;

use super::{Editor, ViewId};

// ========================================================================
// Data Structures
// ========================================================================

/// A view as a screen reader reads it, from [`Editor::accessible_view`].
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct AccessibleView {
    /// Where the cursor is.
    pub cursor: Position,
    /// The lines in sight, top first; a closed fold is its first line.
    pub lines: Vec<AccessibleLine>,
    /// The mode the view is in.
    pub mode: Mode,
    /// The selected spans, start before end; empty when nothing is
    /// selected.
    pub selections: Vec<AccessibleRange>,
}

/// A line in sight in an [`AccessibleView`].
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct AccessibleLine {
    /// How many lines a closed fold starting here hides; 0 for most.
    pub folded: usize,
    /// The line, from 0.
    pub line: usize,
    /// The line's text, without its line break.
    pub text: String,
}

/// A selected span in an [`AccessibleView`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct AccessibleRange {
    /// Just past the last selected char.
    pub end: Position,
    /// The first selected char.
    pub start: Position,
}

// ========================================================================
// Editor Implementations
// ========================================================================

impl Editor {
    /// The view `view` as a screen reader reads it; `None` when the editor
    /// has no such view.
    pub fn accessible_view(&self, view: ViewId) -> Option<AccessibleView> {
        let state = self.views.get(view)?;
        let document = self.document_of(view);
        let mut shown = self.screen_lines(view).lines;
        shown.dedup();
        let lines = shown
            .iter()
            .map(|line| AccessibleLine {
                folded: state
                    .folds()
                    .closed_at(document, *line)
                    .filter(|lines| lines.start() == line)
                    .map_or(0, |lines| lines.end() - lines.start()),
                line: *line,
                text: document.line_text(*line).unwrap_or_default(),
            })
            .collect();
        let mut spans: Vec<(usize, usize)> = state
            .selection()
            .ranges()
            .iter()
            .filter(|range| !range.is_empty())
            .map(|range| (range.start(), range.end()))
            .collect();
        if is_visual(state.mode()) {
            let first = shown.first().copied().unwrap_or(0);
            let last = shown.last().copied().unwrap_or(0);
            let visual = selected_ranges(document, self.options(), state, first..last + 1);
            spans.extend(visual.into_iter().map(|range| (range.start, range.end)));
        }
        let position = |offset: usize| document.char_to_position(offset).unwrap_or_default();
        let selections = spans
            .into_iter()
            .map(|(start, end)| AccessibleRange {
                end: position(end),
                start: position(start),
            })
            .collect();
        Some(AccessibleView {
            cursor: position(state.cursor()),
            lines,
            mode: state.mode(),
            selections,
        })
    }
}

// ========================================================================
// Tests
// ========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::Harness;

    #[test]
    fn test_the_lines_in_sight_the_cursor_and_a_fold_read_as_data() {
        let mut harness = Harness::new("fn a() {\n    one\n}\n|end");
        harness.editor.resize_view(harness.view, 20, 3);
        harness.keys("g g z c");
        let view = harness.editor.accessible_view(harness.view).expect("open");
        let lines: Vec<(usize, &str, usize)> = view
            .lines
            .iter()
            .map(|line| (line.line, line.text.as_str(), line.folded))
            .collect();
        assert_eq!(lines, [(0, "fn a() {", 1), (2, "}", 0), (3, "end", 0)]);
        assert_eq!(view.cursor, Position { column: 0, line: 0 });
        assert_eq!(view.mode, Mode::Normal);
        assert!(view.selections.is_empty());
    }

    #[test]
    fn test_a_visual_selection_reads_as_its_span() {
        let mut harness = Harness::new("o|ne two");
        harness.keys("v l l");
        let view = harness.editor.accessible_view(harness.view).expect("open");
        let span = AccessibleRange {
            end: Position { column: 4, line: 0 },
            start: Position { column: 1, line: 0 },
        };
        assert_eq!(view.selections, [span]);
    }
}
