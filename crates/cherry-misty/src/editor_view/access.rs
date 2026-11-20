//! How an [`EditorView`](crate::EditorView) describes itself to screen
//! readers: the visible text as an AccessKit text node, with the cursor
//! and the selection.

use std::ops::RangeInclusive;

use cherry_engine::{Document, Editor, Mode, Position, View};
use misty::accesskit::{Node, NodeId, Role, TextPosition, TextSelection};
use misty::{FocusId, Frame, Rect};

use super::EditorView;

// ========================================================================
// EditorView Implementations
// ========================================================================

impl EditorView {
    /// Describes the view as last drawn to screen readers as `id`: a
    /// multiline text area holding the lines in sight as text runs (line
    /// `index` is the `index`th child of `id`), with the cursor as its
    /// text selection, clamped to the lines in sight: collapsed at the
    /// cursor, or in a visual mode running from where the selection started
    /// through the cursor (whole lines in Visual Line mode; a block as the
    /// characters between its corners, since a text selection has no
    /// rectangles). Nothing before the first frame.
    pub fn access(&self, editor: &Editor, frame: &mut Frame, id: FocusId) {
        let (Some(layout), Some(view)) = (self.layout, editor.view(self.view)) else {
            return;
        };
        let Some(document) = editor.document(view.document()) else {
            return;
        };
        let first = view.scroll().line;
        let last = (first + usize::from(view.rows()))
            .min(document.len_lines())
            .saturating_sub(1);
        let mut area = Node::new(Role::MultilineTextInput);
        let (anchor, focus) = selection_ends(view, document);
        let lines = first..=last;
        if let (Some(anchor), Some(focus)) = (
            text_position(document, anchor, lines.clone(), id),
            text_position(document, focus, lines, id),
        ) {
            area.set_text_selection(TextSelection { anchor, focus });
        }
        frame.enter_access(id, layout.area, area);
        for (row, line) in (first..=last).enumerate() {
            let mut text = document.line_text(line).unwrap_or_default();
            if line + 1 < document.len_lines() {
                text.push('\n');
            }
            let lengths = text.chars().map(|c| c.len_utf8() as u8).collect::<Vec<_>>();
            let mut run = Node::new(Role::TextRun);
            run.set_value(text);
            run.set_character_lengths(lengths);
            let height = layout.metrics.height;
            let top = layout.area.y + row as f32 * height;
            let rect = Rect::new(layout.area.x, top, layout.area.width, height);
            frame.access(id.child(line as u64), rect, run);
        }
        frame.leave_access();
    }
}

// ========================================================================
// Functions
// ========================================================================

/// The AccessKit id of the node misty describes as `id`.
pub(super) fn node_id(id: FocusId) -> NodeId {
    NodeId(id.get())
}

/// Where char `offset` of `document` is among the text runs of the
/// `lines` in sight, which are the children of `id` numbered by line: an
/// offset above them at the start of the first, one below them at the end
/// of the last; `None` when no line is in sight.
fn text_position(
    document: &Document,
    offset: usize,
    lines: RangeInclusive<usize>,
    id: FocusId,
) -> Option<TextPosition> {
    let position = document.char_to_position(offset)?;
    let (line, character_index) = if position.line < *lines.start() {
        (*lines.start(), 0)
    } else if position.line > *lines.end() {
        let end = document.line_text(*lines.end())?.chars().count();
        (*lines.end(), end)
    } else {
        (position.line, position.column)
    };
    lines.contains(&line).then(|| TextPosition {
        character_index,
        node: node_id(id.child(line as u64)),
    })
}

/// The char offsets between which `view`'s selection lies in `document`,
/// as a text selection's anchor and focus (positions between characters):
/// both at the cursor outside a visual mode; in one, from the anchor
/// through the cursor's character, or the whole lines of both in Visual
/// Line mode, the focus before the anchor when the cursor is.
fn selection_ends(view: &View, document: &Document) -> (usize, usize) {
    let cursor = view.cursor();
    let Some(anchor) = view.visual_anchor() else {
        return (cursor, cursor);
    };
    if view.mode() == Mode::VisualLine {
        let line_of = |offset| document.char_to_position(offset).map_or(0, |at| at.line);
        let (anchor_line, cursor_line) = (line_of(anchor), line_of(cursor));
        let start = |line| {
            document
                .position_to_char(Position { column: 0, line })
                .unwrap_or_else(|| document.len_chars())
        };
        let (top, bottom) = (anchor_line.min(cursor_line), anchor_line.max(cursor_line));
        let (top, bottom) = (start(top), start(bottom + 1));
        return if anchor_line <= cursor_line {
            (top, bottom)
        } else {
            (bottom, top)
        };
    }
    if anchor <= cursor {
        (anchor, document.next_grapheme_boundary(cursor))
    } else {
        (document.next_grapheme_boundary(anchor), cursor)
    }
}
