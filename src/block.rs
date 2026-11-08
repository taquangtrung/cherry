//! Visual block mode (`C-v`): a rectangle of the text, by display columns.
//!
//! The block spans the lines from the visual anchor's to the cursor's and
//! the screen columns from the leftmost to the rightmost cell of the two
//! characters at its corners, counted without wrapping, so tabs and wide
//! characters take every cell they cover; a character partly inside the
//! block counts as inside. After `$` (and up and down from there) it
//! reaches the end of every line. `o` goes to the other corner, `O` to the
//! other end of the cursor's line.
//!
//! - `d`, `x`, and `y` delete or yank each line's part into a blockwise
//!   register; `c` deletes them and types on every line (as `I` does);
//!   `~`, `u`, and `U` change their case; `r{char}` replaces each of
//!   their characters. The cursor goes to the block's top left.
//! - `>` widens the blanks at the block's left column of each line that
//!   reaches it by an indent level per count, written as tabs and spaces
//!   or, with [`EditorOptions::expand_tab`], spaces only (as linewise `>`
//!   does), and `<` removes up to that many blank cells there; a block at
//!   the first column shifts whole lines.
//! - `I` types at the block's left column of every line that reaches it,
//!   `A` after its right column, padding short lines with spaces (after
//!   `$`, at each line's end). What is typed on the first line goes on
//!   the others when Insert mode is left, unless it broke the line or the
//!   cursor left it.
//! - `p` and `P` put a blockwise register as a block: each of its lines on
//!   a line of its own from the cursor's down, at the cursor's column (`p`
//!   after the cursor's character), short lines padded and new lines added
//!   at the end of the document as needed.

use std::iter;
use std::ops::Range;

use super::command::{CommandContext, await_char};
use super::comment::toggle_comment;
use super::document::{Document, last_line, line_end, line_of, start_of_line};
use super::editor::{Editor, ViewId};
use super::format::reflow_lines;
use super::insert::enter_insert;
use super::motion::last_cursor_offset;
use super::operator::Operator;
use super::options::EditorOptions;
use super::register::{RegisterKind, RegisterText};
use super::transaction::Edit;
use super::view::{Mode, View};
use super::visual::{Shift, blanks_between, shift_line_range};
use super::wrap::LineLayout;

// ========================================================================
// Constants
// ========================================================================

/// The line break between a block's lines in a register.
const LINE_BREAK: char = '\n';

/// The char that pads short lines.
const PAD: char = ' ';

// ========================================================================
// Data Structures
// ========================================================================

/// A rectangle of the text, by lines and display columns.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Block {
    /// The first line.
    pub first: usize,
    /// The last line.
    pub last: usize,
    /// The first cell column inside it.
    pub left: usize,
    /// The last cell column inside it, or `None` to reach every line's end.
    pub right: Option<usize>,
}

/// Text typed on a block's first line, to repeat on its other lines when
/// Insert mode is left.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct BlockInsert {
    /// The cell column the text goes at on the other lines.
    column: usize,
    /// The last line to type on.
    last: usize,
    /// Whether lines too short to reach the column are padded to it (`A`)
    /// rather than left alone (`I`, `c`).
    pad: bool,
    /// Where typing started on the first line.
    start: usize,
    /// Whether the text goes at each line's end (`A` after `$`).
    to_line_end: bool,
}

// ========================================================================
// Block Implementations
// ========================================================================

impl Block {
    /// The chars of line `line` inside the block: the clusters covering
    /// any of its cells, or an empty range where the block starts (the
    /// line end of a line too short to reach it).
    pub fn chars_on(
        &self,
        document: &Document,
        options: &EditorOptions,
        line: usize,
    ) -> Range<usize> {
        let start = start_of_line(document, line);
        let text = document.line_text(line).unwrap_or_default();
        let layout = LineLayout::new(&text, None, options.tab_stop());
        let right_end = self.right.map_or(usize::MAX, |right| right + 1);
        let mut covered: Option<Range<usize>> = None;
        for glyph in &layout.rows()[0].glyphs {
            if glyph.x + glyph.width > self.left && glyph.x < right_end {
                let end = glyph.column + glyph.text.chars().count();
                covered = Some(covered.map_or(glyph.column..end, |range| range.start..end));
            }
        }
        match covered {
            Some(range) => start + range.start..start + range.end,
            None => {
                let at = start + layout.column_at(self.left);
                at..at
            }
        }
    }

    /// The chars inside the block on each of its lines, top first.
    pub fn ranges(&self, document: &Document, options: &EditorOptions) -> Vec<Range<usize>> {
        (self.first..=self.last)
            .map(|line| self.chars_on(document, options, line))
            .collect()
    }
}

// ========================================================================
// Functions
// ========================================================================

/// `A` in visual block mode: types after the block on every line.
pub(crate) fn append_to_block(cx: &mut CommandContext<'_>) {
    let Some(block) = block_of(cx.editor, cx.view) else {
        return;
    };
    let column = block.right.map_or(usize::MAX, |right| right + 1);
    cx.editor.set_mode(cx.view, Mode::Normal);
    let document = cx.editor.document_of(cx.view);
    let options = *cx.editor.options();
    let (at, pad) = match block.right {
        Some(_) => insertion_point(document, &options, block.first, column, true),
        None => (line_end(document, start_of_line(document, block.first)), 0),
    };
    cx.editor.set_cursor(cx.view, at);
    enter_insert(cx);
    if pad > 0 {
        let padding: String = iter::repeat_n(PAD, pad).collect();
        cx.editor
            .apply_edits(cx.view, vec![Edit::new(at..at, padding)]);
    }
    let start = at + pad;
    cx.editor.set_cursor(cx.view, start);
    cx.editor
        .view_mut(cx.view)
        .set_block_insert(Some(BlockInsert {
            column,
            last: block.last,
            pad: true,
            start,
            to_line_end: block.right.is_none(),
        }));
}

/// The block selected in `view`, in visual block mode.
pub(crate) fn block_of(editor: &Editor, view: ViewId) -> Option<Block> {
    let state = editor.view_ref(view);
    if state.mode() != Mode::VisualBlock {
        return None;
    }
    view_block(editor.document_of(view), editor.options(), state)
}

/// Repeats what was typed on a block's first line on its other lines, as
/// the module docs say, and returns whether it did; the cursor goes to
/// where typing started.
pub(crate) fn finish_block_insert(editor: &mut Editor, view: ViewId) -> bool {
    let Some(insert) = editor.view_mut(view).take_block_insert() else {
        return false;
    };
    let document = editor.document_of(view);
    let cursor = editor.view_ref(view).cursor();
    let first = line_of(document, insert.start);
    if cursor < insert.start || line_of(document, cursor) != first {
        return false;
    }
    let text = document.slice(insert.start..cursor);
    if text.is_empty() {
        return false;
    }
    let options = *editor.options();
    let edits: Vec<Edit> = (first + 1..=insert.last.min(last_line(document)))
        .filter_map(|line| {
            let (at, pad) = if insert.to_line_end {
                (line_end(document, start_of_line(document, line)), 0)
            } else {
                let (at, pad) =
                    insertion_point(document, &options, line, insert.column, insert.pad);
                if pad > 0 && !insert.pad {
                    return None;
                }
                (at, pad)
            };
            let padding: String = iter::repeat_n(PAD, pad).collect();
            Some(Edit::new(at..at, format!("{padding}{text}")))
        })
        .collect();
    editor.apply_edits(view, edits);
    editor.set_cursor(view, insert.start);
    true
}

/// `I` in visual block mode: types at the block's left column on every
/// line that reaches it.
pub(crate) fn insert_in_block(cx: &mut CommandContext<'_>) {
    let Some(block) = block_of(cx.editor, cx.view) else {
        return;
    };
    cx.editor.set_mode(cx.view, Mode::Normal);
    let document = cx.editor.document_of(cx.view);
    let (start, _) = insertion_point(
        document,
        cx.editor.options(),
        block.first,
        block.left,
        false,
    );
    start_block_insert(cx, start, block);
}

/// Puts `text`, blockwise register text, as a block at the cursor of
/// `view` (after its character when `after`), each line `count` times.
pub(crate) fn put_block(editor: &mut Editor, view: ViewId, text: &str, after: bool, count: usize) {
    let options = *editor.options();
    let document = editor.document_of(view);
    let cursor = editor.view_ref(view).cursor();
    let line = line_of(document, cursor);
    let (x, width) = cell_span(document, &options, cursor);
    let column = if after && cursor < line_end(document, cursor) {
        x + width
    } else {
        x
    };
    put_pieces(editor, view, (line, column), text, count);
}

/// `p` and `P` over a block: replaces it with the register's `text` (a
/// one-line charwise text on each of its lines, a blockwise one as a
/// block, any other at its top left), as one undo step; the replaced text
/// goes to the registers as a blockwise delete unless `keep_registers`.
pub(crate) fn replace_block(
    cx: &mut CommandContext<'_>,
    block: Block,
    text: &RegisterText,
    keep_registers: bool,
) {
    let options = *cx.editor.options();
    let document = cx.editor.document_of(cx.view);
    let ranges = block.ranges(document, &options);
    let replaced: Vec<String> = ranges
        .iter()
        .map(|range| document.slice(range.clone()))
        .collect();
    let line_count = block.last - block.first + 1;
    let pieces = match text.kind {
        RegisterKind::Blockwise => Some(text.text.clone()),
        RegisterKind::Charwise if !text.text.contains(LINE_BREAK) => {
            Some(vec![text.text.as_str(); line_count].join("\n"))
        }
        RegisterKind::Charwise | RegisterKind::Linewise => None,
    };
    if cx.editor.refuse_if_read_only(cx.view) {
        return cx.editor.set_mode(cx.view, Mode::Normal);
    }
    if !keep_registers {
        let deleted = RegisterText::new(RegisterKind::Blockwise, replaced.join("\n"));
        cx.editor.registers_mut().record_delete(None, deleted);
    }
    let deletions: Vec<Edit> = ranges
        .iter()
        .filter(|range| !range.is_empty())
        .map(|range| Edit::new(range.clone(), ""))
        .collect();
    let count = cx.count_or_one();
    cx.editor.set_mode(cx.view, Mode::Normal);
    cx.editor.history_mut(cx.view).begin_group();
    cx.editor.apply_edits(cx.view, deletions);
    match pieces {
        Some(pieces) => put_pieces(
            cx.editor,
            cx.view,
            (block.first, block.left),
            &pieces,
            count,
        ),
        None => {
            let at = ranges[0].start;
            let repeated = text.text.repeat(count);
            cx.editor
                .apply_edits(cx.view, vec![Edit::new(at..at, repeated)]);
            cx.editor.set_cursor(cx.view, at);
        }
    }
    cx.editor.history_mut(cx.view).end_group();
}

/// Puts the lines of `text` as a block from cell `column` of `line` down,
/// each `count` times, as the module docs say, the cursor at its top left.
fn put_pieces(
    editor: &mut Editor,
    view: ViewId,
    (line, column): (usize, usize),
    text: &str,
    count: usize,
) {
    let options = *editor.options();
    let document = editor.document_of(view);
    let pieces: Vec<String> = text
        .split(LINE_BREAK)
        .map(|piece| piece.repeat(count))
        .collect();
    let block_width = pieces
        .iter()
        .map(|piece| text_width(piece, &options))
        .max()
        .unwrap_or(0);
    let len = document.len_chars();
    let mut edits = Vec::new();
    let mut appended = String::new();
    for (index, piece) in pieces.iter().enumerate() {
        let target = line + index;
        if target >= document.len_lines() {
            let padding: String = iter::repeat_n(PAD, column).collect();
            appended.push_str(&format!("{LINE_BREAK}{padding}{piece}"));
            continue;
        }
        let (at, pad) = insertion_point(document, &options, target, column, true);
        if piece.is_empty() && pad > 0 {
            continue;
        }
        let follows = at < line_end(document, start_of_line(document, target));
        let fill = if follows {
            block_width - text_width(piece, &options)
        } else {
            0
        };
        let padding: String = iter::repeat_n(PAD, pad).collect();
        let filler: String = iter::repeat_n(PAD, fill).collect();
        edits.push(Edit::new(at..at, format!("{padding}{piece}{filler}")));
    }
    if !appended.is_empty() {
        match edits.last_mut() {
            Some(last) if last.range.end == len => last.text.push_str(&appended),
            Some(_) | None => edits.push(Edit::new(len..len, appended)),
        }
    }
    editor.apply_edits(view, edits);
    let document = editor.document_of(view);
    let (target, _) = insertion_point(document, &options, line, column, false);
    let target = target.min(last_cursor_offset(editor, view, target));
    editor.set_cursor(view, target);
}

/// `r` in visual block mode: waits for the character to replace every
/// character of the block with.
pub(crate) fn replace_in_block(cx: &mut CommandContext<'_>) {
    await_char(cx, |cx, c| {
        let Some(block) = block_of(cx.editor, cx.view) else {
            return;
        };
        if c == LINE_BREAK {
            return cx.editor.set_mode(cx.view, Mode::Normal);
        }
        let ranges = block.ranges(cx.editor.document_of(cx.view), cx.editor.options());
        let document = cx.editor.document_of(cx.view);
        let edits = ranges
            .iter()
            .filter(|range| !range.is_empty())
            .map(|range| {
                let clusters = cluster_count(document, range.clone());
                Edit::new(
                    range.clone(),
                    iter::repeat_n(c, clusters).collect::<String>(),
                )
            })
            .collect();
        finish_at_top_left(cx.editor, cx.view, &ranges, edits);
    });
}

/// Runs `operator` over the block selected in `view` with `register`, as
/// the module docs say; `levels` is how many indent levels `>` and `<`
/// shift by.
pub(crate) fn run_block_operator(
    cx: &mut CommandContext<'_>,
    block: Block,
    operator: Operator,
    levels: usize,
) {
    if operator != Operator::Yank && cx.editor.refuse_if_read_only(cx.view) {
        return cx.editor.set_mode(cx.view, Mode::Normal);
    }
    let options = *cx.editor.options();
    let document = cx.editor.document_of(cx.view);
    let ranges = block.ranges(document, &options);
    let text: Vec<String> = ranges
        .iter()
        .map(|range| document.slice(range.clone()))
        .collect();
    let register_text = RegisterText::new(RegisterKind::Blockwise, text.join("\n"));
    let deletions = || {
        ranges
            .iter()
            .filter(|range| !range.is_empty())
            .map(|range| Edit::new(range.clone(), ""))
            .collect::<Vec<_>>()
    };
    match operator {
        Operator::Change => {
            cx.editor
                .registers_mut()
                .record_delete(cx.register, register_text);
            cx.editor.set_mode(cx.view, Mode::Normal);
            cx.editor.history_mut(cx.view).begin_group();
            cx.editor.apply_edits(cx.view, deletions());
            start_block_insert(cx, ranges[0].start, block);
        }
        Operator::Comment => {
            cx.editor.set_mode(cx.view, Mode::Normal);
            toggle_comment(cx.editor, cx.view, block.first, block.last);
        }
        Operator::Format => {
            cx.editor.set_mode(cx.view, Mode::Normal);
            reflow_lines(cx.editor, cx.view, block.first, block.last);
        }
        Operator::Delete => {
            cx.editor
                .registers_mut()
                .record_delete(cx.register, register_text);
            finish_at_top_left(cx.editor, cx.view, &ranges, deletions());
        }
        Operator::Indent => shift_block(cx.editor, cx.view, block, levels, Shift::Right),
        Operator::Outdent => shift_block(cx.editor, cx.view, block, levels, Shift::Left),
        Operator::Lowercase | Operator::ToggleCase | Operator::Uppercase => {
            let edits = ranges
                .iter()
                .zip(&text)
                .map(|(range, text)| Edit::new(range.clone(), case_changed(text, operator)))
                .collect();
            finish_at_top_left(cx.editor, cx.view, &ranges, edits);
        }
        // `ys` waits for its motion in operator-pending mode, never on a
        // block.
        Operator::Surround => cx.editor.set_mode(cx.view, Mode::Normal),
        Operator::Yank => {
            cx.editor
                .registers_mut()
                .record_yank(cx.register, register_text);
            finish_at_top_left(cx.editor, cx.view, &ranges, Vec::new());
        }
    }
}

/// `O` in visual block mode: the cursor and the anchor trade columns, each
/// staying on its line.
pub(crate) fn swap_block_corner(cx: &mut CommandContext<'_>) {
    let Some(anchor) = cx.editor.view_ref(cx.view).visual_anchor() else {
        return;
    };
    let options = *cx.editor.options();
    let document = cx.editor.document_of(cx.view);
    let cursor = cx.editor.view_ref(cx.view).cursor();
    let (anchor_x, _) = cell_span(document, &options, anchor);
    let (cursor_x, _) = cell_span(document, &options, cursor);
    let (new_cursor, _) = insertion_point(
        document,
        &options,
        line_of(document, cursor),
        anchor_x,
        false,
    );
    let (new_anchor, _) = insertion_point(
        document,
        &options,
        line_of(document, anchor),
        cursor_x,
        false,
    );
    let new_cursor = new_cursor.min(last_cursor_offset(cx.editor, cx.view, new_cursor));
    let new_anchor = new_anchor.min(last_cursor_offset(cx.editor, cx.view, new_anchor));
    cx.editor.set_visual_anchor(cx.view, new_anchor);
    cx.editor.set_cursor(cx.view, new_cursor);
}

/// The block `view` selects in `document`, when it has a visual anchor:
/// to every line's end after `$`.
pub(crate) fn view_block(
    document: &Document,
    options: &EditorOptions,
    view: &View,
) -> Option<Block> {
    let anchor = view.visual_anchor()?;
    let cursor = view.cursor();
    let (anchor_x, anchor_width) = cell_span(document, options, anchor);
    let (cursor_x, cursor_width) = cell_span(document, options, cursor);
    let to_line_end = view.goal_column() == Some(usize::MAX);
    let right = (anchor_x + anchor_width).max(cursor_x + cursor_width) - 1;
    let (first, last) = (line_of(document, anchor), line_of(document, cursor));
    Some(Block {
        first: first.min(last),
        last: first.max(last),
        left: anchor_x.min(cursor_x),
        right: (!to_line_end).then_some(right),
    })
}

/// `text` with its case changed as `operator`, a case operator, says.
fn case_changed(text: &str, operator: Operator) -> String {
    match operator {
        Operator::Lowercase => text.to_lowercase(),
        Operator::Uppercase => text.to_uppercase(),
        Operator::Change
        | Operator::Comment
        | Operator::Delete
        | Operator::Format
        | Operator::Indent
        | Operator::Outdent
        | Operator::Surround
        | Operator::ToggleCase
        | Operator::Yank => text
            .chars()
            .flat_map(|c| {
                let swapped: Vec<char> = if c.is_uppercase() {
                    c.to_lowercase().collect()
                } else {
                    c.to_uppercase().collect()
                };
                swapped
            })
            .collect(),
    }
}

/// The cell column and width of the cluster at `offset`, laid out without
/// wrapping; one cell just past the end of its line.
fn cell_span(document: &Document, options: &EditorOptions, offset: usize) -> (usize, usize) {
    let line = line_of(document, offset);
    let start = start_of_line(document, line);
    let text = document.line_text(line).unwrap_or_default();
    let layout = LineLayout::new(&text, None, options.tab_stop());
    let column = offset - start;
    layout.rows()[0]
        .glyphs
        .iter()
        .find(|glyph| column < glyph.column + glyph.text.chars().count())
        .map_or((layout.rows()[0].width, 1), |glyph| (glyph.x, glyph.width))
}

/// How many grapheme clusters `range` of `document` holds.
pub(crate) fn cluster_count(document: &Document, range: Range<usize>) -> usize {
    let mut at = range.start;
    let mut count = 0;
    while at < range.end {
        at = document.next_grapheme_boundary(at);
        count += 1;
    }
    count
}

/// Back in Normal mode, makes `edits` and puts the cursor at the block's
/// top left, the start of `ranges[0]`.
fn finish_at_top_left(
    editor: &mut Editor,
    view: ViewId,
    ranges: &[Range<usize>],
    edits: Vec<Edit>,
) {
    editor.set_mode(view, Mode::Normal);
    editor.apply_edits(view, edits);
    let top_left = ranges[0].start;
    let target = top_left.min(last_cursor_offset(editor, view, top_left));
    editor.set_cursor(view, target);
}

/// Where text goes at cell `column` of line `line`: the first cluster
/// starting at or after the column, with no padding; for a line too short,
/// its end and the pad cells to reach the column (zero unless `pad`).
fn insertion_point(
    document: &Document,
    options: &EditorOptions,
    line: usize,
    column: usize,
    pad: bool,
) -> (usize, usize) {
    let start = start_of_line(document, line);
    let text = document.line_text(line).unwrap_or_default();
    let layout = LineLayout::new(&text, None, options.tab_stop());
    let row = &layout.rows()[0];
    if let Some(glyph) = row.glyphs.iter().find(|glyph| glyph.x >= column) {
        return (start + glyph.column, 0);
    }
    let end = start + text.chars().count();
    let short_by = column.saturating_sub(row.width);
    (end, if pad { short_by } else { short_by.min(1) })
}

/// Shifts the block's text right (or left) by `levels` indent levels at
/// its left column, as the module docs say.
fn shift_block(editor: &mut Editor, view: ViewId, block: Block, levels: usize, shift: Shift) {
    let options = *editor.options();
    if block.left == 0 {
        editor.set_mode(view, Mode::Normal);
        return shift_line_range(editor, view, (block.first, block.last), levels, shift);
    }
    let step = options.indent_width().saturating_mul(levels);
    let document = editor.document_of(view);
    let ranges = block.ranges(document, &options);
    let edits = (block.first..=block.last)
        .filter_map(|line| {
            let (at, short) = insertion_point(document, &options, line, block.left, false);
            if short > 0 {
                return None;
            }
            match shift {
                Shift::Right => {
                    let (x, _) = cell_span(document, &options, at);
                    let mut end = at;
                    let mut width = 0;
                    while matches!(document.char_at(end), Some(' ' | '\t')) {
                        width += cell_span(document, &options, end).1;
                        end += 1;
                    }
                    let blanks = blanks_between(x, x + width + step, &options);
                    Some(Edit::new(at..end, blanks))
                }
                Shift::Left => {
                    let mut end = at;
                    let mut removed = 0;
                    while matches!(document.char_at(end), Some(' ' | '\t')) {
                        let (_, width) = cell_span(document, &options, end);
                        if removed + width > step {
                            break;
                        }
                        removed += width;
                        end += 1;
                    }
                    (end > at).then(|| Edit::new(at..end, ""))
                }
            }
        })
        .collect();
    finish_at_top_left(editor, view, &ranges, edits);
}

/// Enters Insert mode at `start` on the block's first line, to repeat what
/// is typed at the block's left column on its other lines.
fn start_block_insert(cx: &mut CommandContext<'_>, start: usize, block: Block) {
    cx.editor.set_cursor(cx.view, start);
    enter_insert(cx);
    cx.editor
        .view_mut(cx.view)
        .set_block_insert(Some(BlockInsert {
            column: block.left,
            last: block.last,
            pad: false,
            start,
            to_line_end: false,
        }));
}

/// The cells `text`, a line's piece, takes from the line start.
fn text_width(text: &str, options: &EditorOptions) -> usize {
    LineLayout::new(text, None, options.tab_stop()).rows()[0].width
}

// ========================================================================
// Tests
// ========================================================================

#[cfg(test)]
mod tests {
    use crate::options::EditorOptions;
    use crate::paint::{Style, Surface};
    use crate::render::render_view;
    use crate::testing::{Harness, assert_keys};

    #[test]
    fn test_block_delete_takes_a_rectangle_over_wide_chars_and_tabs() {
        assert_keys("|abcd\nefgh\nijkl", "l C-v j l d", "a|d\neh\nijkl");
        assert_keys("ab|日cd\nabcdef", "C-v j d", "ab|cd\nabef");
        assert_keys("\t|x\nabcdefgh", "C-v j h h h d", "|\nafgh");
        assert_keys("|abcd\nefgh", "l C-v j l d u", "|abcd\nefgh");
    }

    #[test]
    fn test_dollar_extends_the_block_to_every_line_end() {
        assert_keys("a|b\nabcd\nxy", "C-v j j $ d", "|a\na\nx");
        assert_keys("|ab\nx\nabc", "C-v j j $ A ! <esc>", "ab|!\nx!\nabc!");
    }

    #[test]
    fn test_capital_o_trades_columns_and_o_trades_corners() {
        let mut harness = Harness::new("|abc\ndef");
        harness.keys("C-v j l O");
        assert_eq!(harness.marked(), "abc\n|def");
        harness.keys("o");
        assert_eq!(harness.marked(), "a|bc\ndef");
        harness.keys("d");
        assert_eq!(harness.marked(), "|c\nf");
    }

    #[test]
    fn test_a_yanked_block_puts_as_a_block_padding_and_adding_lines() {
        assert_keys("|abc\ndef\nghi", "l C-v j y G $ p", "abc\ndef\nghi|b\n   e");
        assert_keys("|abc\ndef\nghi", "l C-v j y g g 0 P", "|babc\nedef\nghi");
        assert_keys(
            "|abcd\nx\nefgh",
            "l l C-v j j y g g $ p",
            "abcd|c\nx\nefghg",
        );
        assert_keys("|ab\ncd\nxy", "C-v j y G p", "ab\ncd\nx|ay\n c");
    }

    #[test]
    fn test_capital_i_types_on_every_line_reaching_the_block() {
        assert_keys("|ab\ncd\n\nef", "C-v j j j I X <esc>", "|Xab\nXcd\nX\nXef");
        assert_keys("ab|c\nx\ndef", "C-v j j I Y <esc>", "ab|Yc\nx\ndeYf");
        assert_keys("|ab\ncd", "C-v j I X <cr> <esc>", "X\n|ab\ncd");
        assert_keys("|ab\ncd", "C-v j I X <esc> u", "|ab\ncd");
    }

    #[test]
    fn test_capital_a_types_after_the_block_padding_short_lines() {
        assert_keys("|ab\nx\nabc", "l C-v j j A Z <esc>", "ab|Z\nx Z\nabZc");
    }

    #[test]
    fn test_block_change_replace_and_case_act_on_each_line() {
        assert_keys("|abc\ndef", "l C-v j c X <esc>", "a|Xc\ndXf");
        assert_keys("|abc\ndef", "l C-v j l r x", "a|xx\ndxx");
        assert_keys("|abc\ndef", "l C-v j U", "a|Bc\ndEf");
        assert_keys("|abc\ndef", "l C-v j \"ay \"ap", "ab|bc\ndeef");
    }

    #[test]
    fn test_block_shift_inserts_and_removes_blanks_at_the_left_column() {
        assert_keys("|ab\ncd", "l C-v j >", "a|\t b\nc\t d");
        assert_keys("a|\t b\nc\t d", "C-v j <", "a|b\ncd");
        assert_keys("|ab\ncd", "l C-v j 2 >", "a|\t\t b\nc\t\t d");
        assert_keys("a|    b\nc    d", "C-v j <", "a|b\ncd");
        assert_keys("|a\nb", "C-v j >", "\t|a\n\tb");
    }

    #[test]
    fn test_block_shift_writes_spaces_only_with_expand_tab() {
        let options = EditorOptions {
            expand_tab: true,
            ..EditorOptions::default()
        };
        let mut harness = Harness::with_options("|ab\ncd", options);
        harness.keys("l C-v j >");
        assert_eq!(harness.marked(), "a|    b\nc    d");
        harness.keys("C-v j >");
        assert_eq!(
            harness.marked(),
            "a|        b\nc        d",
            "blanks there are merged"
        );
    }

    #[test]
    fn test_visual_r_replaces_characters_but_not_line_breaks() {
        assert_keys("a|bc\ndef", "v j r x", "a|xx\nxxf");
        assert_keys("a|bc\nde", "V j r -", "|---\n--");
    }

    #[test]
    fn test_a_block_paints_only_its_columns_and_skips_short_lines() {
        let mut harness = Harness::with_options("|abcd\nx\nabcd", EditorOptions::default());
        harness.editor.resize_view(harness.view, 6, 3);
        harness.keys("l C-v j j l");
        let mut surface = Surface::new(6, 3);
        render_view(&harness.editor, harness.view, &mut surface);
        let selected = |row| {
            (0..4)
                .filter(|&x| surface.cell(x, row).unwrap().style == Style::Selection)
                .collect::<Vec<_>>()
        };
        assert_eq!(selected(0), [1, 2]);
        assert_eq!(selected(1), Vec::<u16>::new());
        assert_eq!(selected(2), [1, 2]);
    }
}
