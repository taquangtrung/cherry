//! Motions: commands that move the cursor without changing the text, or,
//! after an operator, say what text it acts on.
//!
//! In Normal mode the cursor sits on a character, so it stops on the last
//! character of a line; in Insert mode it sits between characters and may
//! also stop after the last one. Every motion takes a count, repeating
//! itself or, for the line motions, naming the line.
//!
//! # Up and down
//!
//! Moving up and down keeps a goal column, in display cells: across a line
//! too short to reach it the cursor stops at the line's end, and on the
//! next line long enough it is back at the goal. Any other motion or edit
//! sets a new goal. `$` sets the goal to the end of every line.
//!
//! # Words
//!
//! A word is a run of letters, digits, and underscores, or a run of other
//! non-blank characters; blanks and line breaks separate words, and an
//! empty line counts as a word of its own. `w` moves to the start of the
//! next word, `e` to the end of the word (the next one when already at an
//! end), and `b` to the start of the word (the previous one when already at
//! a start); `ge` goes back to the end of a word. `W`, `B`, `E`, and `gE`
//! do the same over runs of non-blanks, where punctuation joins the word.
//! Characters are taken a grapheme cluster at a time, classed by their
//! first char, so a letter with a combining accent stays one letter.
//!
//! # Lines and columns
//!
//! `+` and `<cr>` go to the first non-blank of a line the count of lines
//! down, `-` up, `_` the count less one down; all three are linewise (`d_`
//! is `dd`) and fail at the document's edge. `|` goes to the screen
//! column the count names, counting tab and wide-character cells.
//!
//! # After an operator
//!
//! Each motion has a [`MotionKind`]: `h l w b W B 0 ^ $ |` are exclusive
//! (the text up to the target), `e E ge gE` are inclusive (the target char
//! too), and `j k gg G + - _` are linewise. A motion that cannot move,
//! such as `+` on the last line, cancels the operator. In operator-pending mode a motion may reach the
//! end of a line, as in Insert mode. Two of Vim's special cases hold: the
//! last word `w` moves over ends the text at its line's end, so `dw` on
//! the last word of a line does not join the next line; and `cw` on a
//! word changes to the end of the word, as `ce` does. `j` or `k` that
//! cannot move at all cancel the operator.

use super::command::CommandContext;
use super::document::{
    Document, first_non_blank_in, is_word_char, last_line, line_end, line_of, line_start,
    start_of_line,
};
use super::editor::{Editor, ViewId};
use super::fold::step_lines;
use super::mark::jump_to;
use super::operator::{MotionKind, Operator, operate};
use super::options::EditorOptions;
use super::request::request_submit;
use super::screen::{move_screen_rows, swaps_rows_and_lines};
use super::view::Mode;
use super::wrap::LineLayout;

// ========================================================================
// Data Structures
// ========================================================================

/// What a character is, for telling where words start and end.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CharClass {
    /// A non-blank character that is not a word character.
    Punctuation,
    /// A blank or a line break.
    Whitespace,
    /// A letter, a digit, or an underscore.
    Word,
}

// ========================================================================
// Functions
// ========================================================================

/// Moves the cursor one character left per count, across line breaks: the
/// Emacs backward-char.
pub(crate) fn backward_char(cx: &mut CommandContext<'_>) {
    let document = cx.editor.document_of(cx.view);
    let mut offset = cx.editor.view_ref(cx.view).cursor();
    for _ in 0..cx.count_or_one() {
        offset = document.prev_grapheme_boundary(offset);
    }
    finish(cx, MotionKind::Exclusive, offset);
}

/// `|`: moves the cursor to the display column the count names on its
/// line, the first without one, or as near as the line allows; up and
/// down then aim for that column.
pub(crate) fn column(cx: &mut CommandContext<'_>) {
    let goal = cx.count_or_one() - 1;
    let document = cx.editor.document_of(cx.view);
    let cursor = cx.editor.view_ref(cx.view).cursor();
    let line = line_of(document, cursor);
    let text = document.line_text(line).unwrap_or_default();
    let column = LineLayout::new(&text, None, cx.editor.options().tab_stop()).column_at(goal);
    let start = start_of_line(document, line);
    let target = (start + column).min(last_cursor_offset(cx.editor, cx.view, start));
    if !operate(cx, MotionKind::Exclusive, target) {
        cx.editor.set_cursor(cx.view, target);
        cx.editor.set_goal_column(cx.view, goal);
    }
}

/// `_`: moves the cursor to the first non-blank of the line the count
/// less one lines down; after an operator, takes those lines (`d_` is
/// `dd`).
pub(crate) fn first_non_blank_down(cx: &mut CommandContext<'_>) {
    let count = cx.count_or_one() - 1;
    lines_to_first_non_blank(cx, |line, last| {
        line.checked_add(count).filter(|line| *line <= last)
    });
}

/// `+`: moves the cursor to the first non-blank of the line the count of
/// lines down; fails on the last line.
pub(crate) fn next_line_start(cx: &mut CommandContext<'_>) {
    let count = cx.count_or_one();
    lines_to_first_non_blank(cx, |line, last| {
        (line < last).then(|| line.saturating_add(count).min(last))
    });
}

/// `<cr>`: in Normal mode in a single-line document, asks the host to
/// submit; anywhere else, moves as `+` does.
pub(crate) fn submit_or_next_line_start(cx: &mut CommandContext<'_>) {
    let mode = cx.editor.view_ref(cx.view).mode();
    if mode == Mode::Normal && cx.editor.shows_single_line(cx.view) {
        return request_submit(cx.editor, cx.view);
    }
    next_line_start(cx);
}

/// `-`: moves the cursor to the first non-blank of the line the count of
/// lines up; fails on the first line.
pub(crate) fn previous_line_start(cx: &mut CommandContext<'_>) {
    let count = cx.count_or_one();
    lines_to_first_non_blank(cx, |line, _| (line > 0).then(|| line.saturating_sub(count)));
}

/// Moves the cursor to the first non-blank character of its line.
pub(crate) fn first_non_blank(cx: &mut CommandContext<'_>) {
    let offset = cx.editor.view_ref(cx.view).cursor();
    let target = first_non_blank_of_line(cx.editor, cx.view, offset);
    finish(cx, MotionKind::Exclusive, target);
}

/// Moves the cursor one character right per count, across line breaks:
/// the Emacs forward-char.
pub(crate) fn forward_char(cx: &mut CommandContext<'_>) {
    let document = cx.editor.document_of(cx.view);
    let mut offset = cx.editor.view_ref(cx.view).cursor();
    for _ in 0..cx.count_or_one() {
        offset = document.next_grapheme_boundary(offset);
    }
    finish(cx, MotionKind::Exclusive, offset);
}

/// Moves the cursor to the first non-blank character of the line the count
/// names, the first line without one.
pub(crate) fn goto_first_line(cx: &mut CommandContext<'_>) {
    let line = cx.count.map_or(0, |count| count.saturating_sub(1));
    goto_line(cx, line);
}

/// Moves the cursor to the first non-blank character of the line the count
/// names, the last line without one.
pub(crate) fn goto_last_line(cx: &mut CommandContext<'_>) {
    let last = last_line(cx.editor.document_of(cx.view));
    let line = cx.count.map_or(last, |count| count.saturating_sub(1));
    goto_line(cx, line);
}

/// Moves the cursor to the end of its line, the count less one lines down:
/// onto the last character in Normal mode, after it in Insert mode. Up and
/// down then keep to line ends.
pub(crate) fn line_end_motion(cx: &mut CommandContext<'_>) {
    let document = cx.editor.document_of(cx.view);
    let cursor = cx.editor.view_ref(cx.view).cursor();
    let line = (line_of(document, cursor) + cx.count_or_one() - 1).min(last_line(document));
    let start = start_of_line(document, line);
    let target = last_cursor_offset(cx.editor, cx.view, start);
    if !operate(cx, MotionKind::Exclusive, target) {
        cx.editor.set_cursor(cx.view, target);
        cx.editor.set_goal_column(cx.view, usize::MAX);
    }
}

/// `g_`: moves the cursor to the last non-blank of its line, or of the
/// line count less one below, inclusive after an operator.
pub(crate) fn last_non_blank(cx: &mut CommandContext<'_>) {
    let document = cx.editor.document_of(cx.view);
    let cursor = cx.editor.view_ref(cx.view).cursor();
    let line = (line_of(document, cursor) + cx.count_or_one() - 1).min(last_line(document));
    let start = start_of_line(document, line);
    let mut target = line_end(document, start);
    while target > start && matches!(document.char_at(target - 1), Some(' ' | '\t')) {
        target -= 1;
    }
    let target = document.prev_grapheme_boundary(target).max(start);
    finish(cx, MotionKind::Inclusive, target);
}

/// Moves the cursor to the start of its line.
pub(crate) fn line_start_motion(cx: &mut CommandContext<'_>) {
    let document = cx.editor.document_of(cx.view);
    let offset = line_start(document, cx.editor.view_ref(cx.view).cursor());
    finish(cx, MotionKind::Exclusive, offset);
}

/// `j`: moves the cursor down a line per count, aiming for the goal
/// column, or a screen row per count where `j` and `k` go by rows.
pub(crate) fn move_down(cx: &mut CommandContext<'_>) {
    if swaps_rows_and_lines(cx) {
        return move_screen_rows(cx, true);
    }
    line_down(cx);
}

/// Moves the cursor down a line per count, aiming for the goal column.
pub(crate) fn line_down(cx: &mut CommandContext<'_>) {
    let count = cx.count_or_one();
    let line = line_of(
        cx.editor.document_of(cx.view),
        cx.editor.view_ref(cx.view).cursor(),
    );
    let target = step_lines(cx.editor, cx.view, line, count, true);
    move_lines(cx, |_, _| target);
}

/// Moves the cursor one character left per count, stopping at the start of
/// its line.
pub(crate) fn move_left(cx: &mut CommandContext<'_>) {
    let document = cx.editor.document_of(cx.view);
    let mut offset = cx.editor.view_ref(cx.view).cursor();
    let start = line_start(document, offset);
    for _ in 0..cx.count_or_one() {
        offset = document.prev_grapheme_boundary(offset).max(start);
    }
    finish(cx, MotionKind::Exclusive, offset);
}

/// Moves the cursor one character right per count, stopping at the last
/// character of its line, or after it in Insert mode.
pub(crate) fn move_right(cx: &mut CommandContext<'_>) {
    let document = cx.editor.document_of(cx.view);
    let mut offset = cx.editor.view_ref(cx.view).cursor();
    let last = last_cursor_offset(cx.editor, cx.view, offset);
    for _ in 0..cx.count_or_one() {
        offset = document.next_grapheme_boundary(offset).min(last);
    }
    finish(cx, MotionKind::Exclusive, offset);
}

/// `k`: moves the cursor up a line per count, aiming for the goal
/// column, or a screen row per count where `j` and `k` go by rows.
pub(crate) fn move_up(cx: &mut CommandContext<'_>) {
    if swaps_rows_and_lines(cx) {
        return move_screen_rows(cx, false);
    }
    line_up(cx);
}

/// Moves the cursor up a line per count, aiming for the goal column.
pub(crate) fn line_up(cx: &mut CommandContext<'_>) {
    let count = cx.count_or_one();
    let line = line_of(
        cx.editor.document_of(cx.view),
        cx.editor.view_ref(cx.view).cursor(),
    );
    let target = step_lines(cx.editor, cx.view, line, count, false);
    move_lines(cx, |_, _| target);
}

/// `B`: moves the cursor back to the start of a run of non-blanks, once
/// per count.
pub(crate) fn big_word_backward(cx: &mut CommandContext<'_>) {
    words_backward(cx, true);
}

/// `E`: moves the cursor to the end of a run of non-blanks, once per
/// count.
pub(crate) fn big_word_end(cx: &mut CommandContext<'_>) {
    words_to_end(cx, true);
}

/// `W`: moves the cursor to the start of the next run of non-blanks, once
/// per count, with the special cases of `w` after an operator.
pub(crate) fn big_word_forward(cx: &mut CommandContext<'_>) {
    words_forward(cx, true);
}

/// `gE`: moves the cursor back to the end of a run of non-blanks, once
/// per count.
pub(crate) fn big_word_end_backward(cx: &mut CommandContext<'_>) {
    words_to_end_backward(cx, true);
}

/// `b`: moves the cursor back to the start of a word, once per count.
pub(crate) fn word_backward(cx: &mut CommandContext<'_>) {
    words_backward(cx, false);
}

/// `e`: moves the cursor to the end of a word, once per count.
pub(crate) fn word_end(cx: &mut CommandContext<'_>) {
    words_to_end(cx, false);
}

/// `ge`: moves the cursor back to the end of a word, once per count.
pub(crate) fn word_end_backward(cx: &mut CommandContext<'_>) {
    words_to_end_backward(cx, false);
}

/// `w`: moves the cursor to the start of the next word, once per count.
/// After an operator, the last word moved over ends the text at its line
/// end, and `c` on a word changes to its end, as the module docs say.
pub(crate) fn word_forward(cx: &mut CommandContext<'_>) {
    words_forward(cx, false);
}

/// Moves back to the start of a word (`big` or not), once per count.
fn words_backward(cx: &mut CommandContext<'_>, big: bool) {
    let document = cx.editor.document_of(cx.view);
    let mut offset = cx.editor.view_ref(cx.view).cursor();
    for _ in 0..cx.count_or_one() {
        offset = prev_word_start(document, offset, big);
    }
    finish(cx, MotionKind::Exclusive, offset);
}

/// Moves to the start of the next word (`big` or not), once per count,
/// with Vim's special cases after an operator.
fn words_forward(cx: &mut CommandContext<'_>, big: bool) {
    let document = cx.editor.document_of(cx.view);
    let cursor = cx.editor.view_ref(cx.view).cursor();
    let count = cx.count_or_one();
    let Some(pending) = cx.editor.view_ref(cx.view).operator() else {
        let mut offset = cursor;
        for _ in 0..count {
            offset = next_word_start(document, offset, big);
        }
        let target = offset.min(last_cursor_offset(cx.editor, cx.view, offset));
        return cx.editor.set_cursor(cx.view, target);
    };
    let on_word = class_at(document, cursor).is_some_and(|class| class != CharClass::Whitespace);
    if pending.operator == Operator::Change && on_word {
        let target = word_end_from(document, cursor, count, big);
        operate(cx, MotionKind::Inclusive, target);
        return;
    }
    let (mut target, mut before_last) = (cursor, cursor);
    for _ in 0..count {
        before_last = target;
        target = next_word_start(document, target, big);
    }
    let end_of_line = line_end(document, before_last);
    if line_of(document, target) > line_of(document, before_last) && end_of_line > before_last {
        target = end_of_line;
    }
    operate(cx, MotionKind::Exclusive, target);
}

/// Moves to the end of a word (`big` or not), once per count.
fn words_to_end(cx: &mut CommandContext<'_>, big: bool) {
    let document = cx.editor.document_of(cx.view);
    let mut offset = cx.editor.view_ref(cx.view).cursor();
    for _ in 0..cx.count_or_one() {
        offset = next_word_end(document, offset, big);
    }
    let target = offset.min(last_cursor_offset(cx.editor, cx.view, offset));
    finish(cx, MotionKind::Inclusive, target);
}

/// Moves back to the end of a word (`big` or not), once per count.
fn words_to_end_backward(cx: &mut CommandContext<'_>, big: bool) {
    let document = cx.editor.document_of(cx.view);
    let mut offset = cx.editor.view_ref(cx.view).cursor();
    for _ in 0..cx.count_or_one() {
        offset = prev_word_end(document, offset, big);
    }
    let target = offset.min(last_cursor_offset(cx.editor, cx.view, offset));
    finish(cx, MotionKind::Inclusive, target);
}

/// The offset of the first non-blank character on the line of `offset`,
/// kept to where the cursor of `view` may sit on a blank line.
pub(crate) fn first_non_blank_of_line(editor: &Editor, view: ViewId, offset: usize) -> usize {
    let target = first_non_blank_in(editor.document_of(view), offset);
    target.min(last_cursor_offset(editor, view, offset))
}

/// The last offset the cursor of `view` may take on the line of `offset`:
/// the start of the last character in Normal mode (the line start when the
/// line is empty), the line end in Insert mode.
pub(crate) fn last_cursor_offset(editor: &Editor, view: ViewId, offset: usize) -> usize {
    let document = editor.document_of(view);
    let end = line_end(document, offset);
    match editor.view_ref(view).mode() {
        Mode::Insert | Mode::OperatorPending | Mode::Replace => end,
        Mode::CommandLine | Mode::Normal | Mode::Visual | Mode::VisualBlock | Mode::VisualLine => {
            document
                .prev_grapheme_boundary(end)
                .max(line_start(document, offset))
        }
    }
}

/// The class of the character at `offset`; `None` at the end.
pub(crate) fn class_at(document: &Document, offset: usize) -> Option<CharClass> {
    let c = document.char_at(offset)?;
    let class = if c.is_whitespace() {
        CharClass::Whitespace
    } else if is_word_char(c) {
        CharClass::Word
    } else {
        CharClass::Punctuation
    };
    Some(class)
}

/// The class of the character at `offset` for words, or for runs of
/// non-blanks when `big` (where punctuation counts as a word character).
pub(crate) fn word_class(document: &Document, offset: usize, big: bool) -> Option<CharClass> {
    match class_at(document, offset) {
        Some(CharClass::Punctuation) if big => Some(CharClass::Word),
        other => other,
    }
}

/// The display column, in cells, of char `offset` on its line, laid out
/// without wrapping.
pub(crate) fn display_column(document: &Document, options: &EditorOptions, offset: usize) -> usize {
    let line = line_of(document, offset);
    let text = document.line_text(line).unwrap_or_default();
    let column = offset - start_of_line(document, line);
    LineLayout::new(&text, None, options.tab_stop())
        .locate(column)
        .x
}

/// Moves the cursor to the first non-blank character of `line`, the last
/// line for one past it; after an operator, operates linewise to there.
pub(crate) fn goto_line(cx: &mut CommandContext<'_>, line: usize) {
    let document = cx.editor.document_of(cx.view);
    let start = start_of_line(document, line.min(last_line(document)));
    let target = first_non_blank_of_line(cx.editor, cx.view, start);
    jump_to(cx, MotionKind::Linewise, target);
}

/// Ends a motion that cannot move: a waiting operator is cancelled, the
/// cursor stays.
pub(crate) fn fail(cx: &mut CommandContext<'_>) {
    if cx.editor.view_ref(cx.view).mode() == Mode::OperatorPending {
        cx.editor.set_mode(cx.view, Mode::Normal);
    }
}

/// Moves the cursor to the first non-blank of the line `target` picks from
/// the cursor's line and the last line, linewise after an operator; fails
/// when it picks none.
fn lines_to_first_non_blank(
    cx: &mut CommandContext<'_>,
    target: impl FnOnce(usize, usize) -> Option<usize>,
) {
    let document = cx.editor.document_of(cx.view);
    let cursor = cx.editor.view_ref(cx.view).cursor();
    let Some(line) = target(line_of(document, cursor), last_line(document)) else {
        return fail(cx);
    };
    let start = start_of_line(document, line);
    let offset = first_non_blank_of_line(cx.editor, cx.view, start);
    finish(cx, MotionKind::Linewise, offset);
}

/// Moves the cursor to `target`, or after an operator, operates on the
/// text the motion of `kind` covers to it.
pub(crate) fn finish(cx: &mut CommandContext<'_>, kind: MotionKind, target: usize) {
    if !operate(cx, kind, target) {
        cx.editor.set_cursor(cx.view, target);
    }
}

/// Whether `offset` is on an empty line.
fn is_empty_line(document: &Document, offset: usize) -> bool {
    line_start(document, offset) == line_end(document, offset)
}

/// Moves the cursor to the line `target` picks from the cursor's line and
/// the last line, at the goal column or as near as the line allows, and
/// keeps the goal. After an operator, the lines from the cursor's to that
/// one are operated on, unless the line is the cursor's own: a motion that
/// cannot move cancels the operator.
pub(crate) fn move_lines(cx: &mut CommandContext<'_>, target: impl FnOnce(usize, usize) -> usize) {
    let (editor, view) = (&*cx.editor, cx.view);
    let document = editor.document_of(view);
    let cursor = editor.view_ref(view).cursor();
    let goal = editor
        .view_ref(view)
        .goal_column()
        .unwrap_or_else(|| display_column(document, editor.options(), cursor));
    let from = line_of(document, cursor);
    let line = target(from, last_line(document));
    let text = document.line_text(line).unwrap_or_default();
    let column = LineLayout::new(&text, None, editor.options().tab_stop()).column_at(goal);
    let start = start_of_line(document, line);
    let offset = (start + column).min(last_cursor_offset(editor, view, start));
    if editor.view_ref(view).operator().is_some() && line == from {
        cx.editor.set_mode(view, Mode::Normal);
        return;
    }
    if !operate(cx, MotionKind::Linewise, offset) {
        cx.editor.set_cursor(view, offset);
        cx.editor.set_goal_column(view, goal);
    }
}

/// Where `w` (`W` when `big`) from `offset` lands: past the rest of the
/// word under it, then past blanks, stopping at an empty line.
fn next_word_start(document: &Document, offset: usize, big: bool) -> usize {
    let class_at = |offset| word_class(document, offset, big);
    let mut at = offset;
    if let Some(class) = class_at(at).filter(|class| *class != CharClass::Whitespace) {
        while class_at(at) == Some(class) {
            at = document.next_grapheme_boundary(at);
        }
    }
    while class_at(at) == Some(CharClass::Whitespace) {
        if at != offset && is_empty_line(document, at) {
            break;
        }
        at = document.next_grapheme_boundary(at);
    }
    at
}

/// Where `e` (`E` when `big`) from `offset` lands: one character on, past
/// blanks, then to the last character of the word reached.
fn next_word_end(document: &Document, offset: usize, big: bool) -> usize {
    let class_at = |offset| word_class(document, offset, big);
    let mut at = document.next_grapheme_boundary(offset);
    while class_at(at) == Some(CharClass::Whitespace) {
        at = document.next_grapheme_boundary(at);
    }
    let Some(class) = class_at(at) else {
        return at;
    };
    loop {
        let next = document.next_grapheme_boundary(at);
        if next == at || class_at(next) != Some(class) {
            return at;
        }
        at = next;
    }
}

/// Where `count` word ends from `offset` land, the first staying put when
/// `offset` is already the end of a word: what `cw` (`cW` when `big`)
/// changes to.
fn word_end_from(document: &Document, offset: usize, count: usize, big: bool) -> usize {
    let class_at = |offset| word_class(document, offset, big);
    let mut at = offset;
    for step in 0..count {
        let next = document.next_grapheme_boundary(at);
        let is_word_end = class_at(next) != class_at(at);
        if step > 0 || !is_word_end {
            at = next_word_end(document, at, big);
        }
    }
    at
}

/// Where `ge` (`gE` when `big`) from `offset` lands: back to the start of
/// the word under it, one character further back, then back past blanks
/// (stopping at an empty line), onto the last character of a word.
fn prev_word_end(document: &Document, offset: usize, big: bool) -> usize {
    let class_at = |offset| word_class(document, offset, big);
    let mut at = offset;
    if let Some(class) = class_at(at).filter(|class| *class != CharClass::Whitespace) {
        while at > 0 && class_at(document.prev_grapheme_boundary(at)) == Some(class) {
            at = document.prev_grapheme_boundary(at);
        }
    }
    at = document.prev_grapheme_boundary(at);
    while at > 0 && class_at(at) == Some(CharClass::Whitespace) && !is_empty_line(document, at) {
        at = document.prev_grapheme_boundary(at);
    }
    at
}

/// Where `b` (`B` when `big`) from `offset` lands: one character back,
/// back past blanks (stopping at an empty line), then to the first
/// character of the word reached.
fn prev_word_start(document: &Document, offset: usize, big: bool) -> usize {
    let class_at = |offset| word_class(document, offset, big);
    let mut at = document.prev_grapheme_boundary(offset);
    while at > 0 && class_at(at) == Some(CharClass::Whitespace) && !is_empty_line(document, at) {
        at = document.prev_grapheme_boundary(at);
    }
    let Some(class) = class_at(at).filter(|class| *class != CharClass::Whitespace) else {
        return at;
    };
    while at > 0 {
        let prev = document.prev_grapheme_boundary(at);
        if class_at(prev) != Some(class) {
            break;
        }
        at = prev;
    }
    at
}

// ========================================================================
// Tests
// ========================================================================

#[cfg(test)]
mod tests {
    use crate::testing::assert_keys;

    #[test]
    fn test_g_underscore_goes_to_the_last_non_blank_counting_lines_down() {
        assert_keys("|ab cd  \nef", "g _", "ab c|d  \nef");
        assert_keys("|ab\nef \t", "2 g _", "ab\ne|f \t");
        assert_keys("|ab cd  \nef", "d g _", "|  \nef");
        assert_keys("|   \nef", "g _", "|   \nef");
    }

    #[test]
    fn test_h_and_l_stay_on_the_line_and_step_over_clusters() {
        assert_keys("ab|c\nde", "l", "ab|c\nde");
        assert_keys("a|bc", "h h", "|abc");
        assert_keys("|e\u{301}日x", "l", "e\u{301}|日x");
        assert_keys("|e\u{301}日x", "2l", "e\u{301}日|x");
    }

    #[test]
    fn test_j_and_k_keep_the_goal_column_across_a_short_line() {
        assert_keys("abc|def\nx\nabcdefg", "j", "abcdef\n|x\nabcdefg");
        assert_keys("abc|def\nx\nabcdefg", "j j", "abcdef\nx\nabc|defg");
        assert_keys("abcdefg\nx\nabc|def", "k k", "abc|defg\nx\nabcdef");
        assert_keys("abc|def\nxy\nabcdefg", "j h j", "abcdef\nxy\n|abcdefg");
    }

    #[test]
    fn test_j_and_k_aim_by_display_column_over_wide_chars_and_tabs() {
        assert_keys("ab|cd\n日本語", "j", "abcd\n日|本語");
        assert_keys("日|本語\nabcd", "j", "日本語\nab|cd");
        assert_keys("abcd|e\n\tx", "j", "abcde\n\t|x");
    }

    #[test]
    fn test_j_and_k_with_counts_stop_at_the_document_edges() {
        assert_keys("|a\nb\nc\nd", "2j", "a\nb\n|c\nd");
        assert_keys("|a\nb\nc\nd", "9j", "a\nb\nc\n|d");
        assert_keys("a\nb\n|c\nd", "9k", "|a\nb\nc\nd");
    }

    #[test]
    fn test_w_moves_to_word_starts_by_class_and_stops_at_empty_lines() {
        assert_keys("|foo.bar baz", "w", "foo|.bar baz");
        assert_keys("|foo.bar baz", "3w", "foo.bar |baz");
        assert_keys("fo|o\n\nbar", "w", "foo\n|\nbar");
        assert_keys("fo|o\n\nbar", "w w", "foo\n\n|bar");
        assert_keys("|héllo wörld", "w", "héllo |wörld");
        assert_keys("foo |bar", "w", "foo ba|r");
    }

    #[test]
    fn test_e_moves_to_word_ends_and_b_back_to_word_starts() {
        assert_keys("|foo bar", "e", "fo|o bar");
        assert_keys("fo|o bar", "e", "foo ba|r");
        assert_keys("|foo.bar", "e e", "foo|.bar");
        assert_keys("foo b|ar", "b", "foo |bar");
        assert_keys("foo |bar", "b", "|foo bar");
        assert_keys("foo\n\n|bar", "b", "foo\n|\nbar");
        assert_keys("foo.bar |baz", "2b", "foo|.bar baz");
        assert_keys("e\u{301}e\u{301} |x", "b", "|e\u{301}e\u{301} x");
    }

    #[test]
    fn test_line_motions_go_to_the_start_the_first_non_blank_and_the_end() {
        assert_keys("  ab|cd", "0", "|  abcd");
        assert_keys("  ab|cd", "^", "  |abcd");
        assert_keys("  ab|cd\nxyz", "$", "  abc|d\nxyz");
        assert_keys("a|b\nxyz\n", "2$", "ab\nxy|z\n");
        assert_keys("a|bcd\nx\nabcdef", "$ j j", "abcd\nx\nabcde|f");
    }

    #[test]
    fn test_gg_and_g_go_to_a_line_by_count_on_its_first_non_blank() {
        assert_keys("a\n  b\nc|", "g g", "|a\n  b\nc");
        assert_keys("|a\n  b\nc", "G", "a\n  b\n|c");
        assert_keys("|a\n  b\nc", "2G", "a\n  |b\nc");
        assert_keys("|a\n  b\nc", "2 g g", "a\n  |b\nc");
        assert_keys("a\n  b\n|c", "9G", "a\n  b\n|c");
    }

    #[test]
    fn test_big_word_motions_take_runs_of_non_blanks() {
        assert_keys("|foo.bar baz", "W", "foo.bar |baz");
        assert_keys("|a.b c.d e", "2W", "a.b c.d |e");
        assert_keys("|foo.bar baz", "E", "foo.ba|r baz");
        assert_keys("foo.bar ba|z", "B", "foo.bar |baz");
        assert_keys("foo.bar |baz", "B", "|foo.bar baz");
        assert_keys("|a.b c.d", "d W", "|c.d");
        assert_keys("|a.b c.d", "c W x <esc>", "|x c.d");
    }

    #[test]
    fn test_ge_moves_back_to_word_ends_and_stops_at_empty_lines() {
        assert_keys("foo ba|r", "g e", "fo|o bar");
        assert_keys("foo b|ar", "g e", "fo|o bar");
        assert_keys("foo.ba|r", "g e", "foo|.bar");
        assert_keys("foo.bar ba|z", "g E", "foo.ba|r baz");
        assert_keys("one two thr|ee", "2 g e", "on|e two three");
        assert_keys("foo\n\nb|ar", "g e", "foo\n|\nbar");
        assert_keys("foo\n|\nbar", "g e", "fo|o\n\nbar");
        assert_keys("|foo", "g e", "|foo");
        assert_keys("foo b|ar", "d g e", "fo|r");
    }

    #[test]
    fn test_bar_goes_to_a_screen_column_over_tabs() {
        assert_keys("|abcdef", "4|", "abc|def");
        assert_keys("abc|def", "|", "|abcdef");
        assert_keys("|\tab", "6|", "\ta|b");
        assert_keys("|abc", "9|", "ab|c");
        assert_keys("|abcdef", "d 4|", "|def");
        assert_keys("|abcdef\nxy\nabcdef", "5| j j", "abcdef\nxy\nabcd|ef");
    }

    #[test]
    fn test_line_start_motions_plus_minus_underscore_and_enter() {
        assert_keys("|a\n  b\n c", "+", "a\n  |b\n c");
        assert_keys("|a\n  b\n c", "2 <cr>", "a\n  b\n |c");
        assert_keys("a\n  b\n |c", "-", "a\n  |b\n c");
        assert_keys("a\n  b\n |c", "9-", "|a\n  b\n c");
        assert_keys("a\n  b|x\n c", "_", "a\n  |bx\n c");
        assert_keys("|a\nb\nc\nd", "d +", "|c\nd");
        assert_keys("|a\nb\nc", "d _", "|b\nc");
        assert_keys("|a\nb\nc", "2d _", "|c");
        assert_keys("a\n|b", "d +", "a\n|b");
        assert_keys("a\n|b", "d -", "|");
    }
}
