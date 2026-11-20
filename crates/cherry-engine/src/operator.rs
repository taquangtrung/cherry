//! Operators: `d` (delete), `c` (change), `y` (yank), `>` and `<` (indent
//! and outdent lines one level), and `g~`, `gu`, `gU` (swap case,
//! lowercase, uppercase), with the text they act on named by the motion
//! typed after them, and put (`p`, `P`).
//!
//! An operator key enters operator-pending mode and waits; the next motion
//! says what text it acts on, by its [`MotionKind`], and a count typed on
//! either side multiplies (`2d3w` deletes six words). Typing the operator
//! again acts on whole lines, the count of them (`dd`, `3cc`, `yy`);
//! `<esc>` or any key that is no motion cancels it. The short forms are
//! `x` (`dl`), `X` (`dh`), `D` (`d$`), `C` (`c$`), `s` (`cl`), `S` (`cc`),
//! and `Y` (`yy`); the case operators double with their last key too
//! (`g~~`, `guu`, `gUU`). `~` swaps the case of the count of characters
//! and moves past them; in a visual mode `~`, `u`, and `U` change the
//! selection's case. A linewise case change leaves the cursor at the
//! start of the first line.
//!
//! An exclusive motion that ends at the start of a later line ends at the
//! end of the line before instead, and when the text then starts in the
//! line's indent, the operator takes whole lines, as in Vim.
//!
//! `gc` is an operator too, commenting or uncommenting whole lines (see
//! [`super::comment`]); `gcc` and `gcgc` act on lines.
//!
//! In a visual mode the operators, `x`, `p`, and `P` act on the selection
//! instead (see [`super::visual`]).
//!
//! Deleted and yanked text goes to the registers (see [`super::register`]),
//! the one selected with `"` before the command or the unnamed one. Each
//! operator and each put is one undo step; a change undoes together with
//! the text typed after it.

use std::ops::Range;

use super::block::{block_of, put_block, run_block_operator};
use super::command::CommandContext;
use super::comment::toggle_comment;
use super::document::{
    Document, first_non_blank_in, last_line, line_end, line_of, line_start, start_of_line,
};
use super::editor::{Editor, ViewId};
use super::format::reflow_lines;
use super::insert::mark_insert_start;
use super::mark::{CHANGE_END_MARK, CHANGE_START_MARK};
use super::motion::{first_non_blank_of_line, last_cursor_offset};
use super::register::{RegisterKind, RegisterText};
use super::surround::wrap_operand;
use super::transaction::Edit;
use super::view::Mode;
use super::visual::{Shift, replace_selection, selection, shift_line_range};

// ========================================================================
// Constants
// ========================================================================

/// The line break between lines.
const LINE_BREAK: &str = "\n";

// ========================================================================
// Data Structures
// ========================================================================

/// What text a motion covers when an operator acts on it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum MotionKind {
    /// From the cursor up to the target, the target char left out.
    Exclusive,
    /// From the cursor to the target, the target char included.
    Inclusive,
    /// The whole lines from the cursor's to the target's.
    Linewise,
}

/// An operator: what is done to the text a motion covers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Operator {
    /// Delete the text and enter Insert mode where it was.
    Change,
    /// Comment or uncomment the lines of the text.
    Comment,
    /// Delete the text into a register.
    Delete,
    /// Refill the lines of the text to the text width (`gq`).
    Format,
    /// Indent the lines of the text one level.
    Indent,
    /// Lowercase the text.
    Lowercase,
    /// Outdent the lines of the text one level.
    Outdent,
    /// Wrap the text in a pair, whose char comes next (`ys`).
    Surround,
    /// Swap the case of each letter of the text.
    ToggleCase,
    /// Uppercase the text.
    Uppercase,
    /// Copy the text into a register.
    Yank,
}

/// An operator typed in Normal mode, waiting for its motion.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct PendingOperator {
    /// The count typed before the operator.
    pub count: Option<usize>,
    /// What the operator does once its motion is typed (`d`, `c`, `y`, …).
    pub operator: Operator,
    /// The register selected before the operator.
    pub register: Option<char>,
}

/// The text an operator acts on.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Operand {
    /// The chars of a range.
    Charwise(Range<usize>),
    /// Whole lines.
    Linewise(Lines),
}

/// A run of whole lines, from the first to the last.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Lines {
    /// The first line.
    pub first: usize,
    /// The last line.
    pub last: usize,
}

// ========================================================================
// Lines Implementations
// ========================================================================

impl Lines {
    /// The lines from the one holding `start` to the one holding `end`.
    fn spanning(document: &Document, start: usize, end: usize) -> Self {
        Self {
            first: line_of(document, start),
            last: line_of(document, end),
        }
    }

    /// The chars deleting the lines removes: their text and the line break
    /// after the last, or before the first when the last is the document's
    /// last line.
    pub(crate) fn deleted(self, document: &Document) -> Range<usize> {
        let first_start = start_of_line(document, self.first);
        if self.last + 1 < document.len_lines() {
            first_start..start_of_line(document, self.last + 1)
        } else {
            first_start.saturating_sub(1)..document.len_chars()
        }
    }

    /// The chars of the lines, their line breaks left out, which a change
    /// replaces so one line is left.
    pub(crate) fn text_inside(self, document: &Document) -> Range<usize> {
        let first_start = start_of_line(document, self.first);
        first_start..line_end(document, start_of_line(document, self.last))
    }
}

// ========================================================================
// Operand Implementations
// ========================================================================

impl Operand {
    /// The chars the operand covers, whole lines with the line break after
    /// the last.
    pub fn chars(&self, document: &Document) -> Range<usize> {
        match self {
            Self::Charwise(range) => range.clone(),
            Self::Linewise(lines) => {
                start_of_line(document, lines.first)..start_of_line(document, lines.last + 1)
            }
        }
    }

    /// The chars deleting the operand removes.
    fn deleted(&self, document: &Document) -> Range<usize> {
        match self {
            Self::Charwise(range) => range.clone(),
            Self::Linewise(lines) => lines.deleted(document),
        }
    }

    /// The whole lines the operand touches: for a charwise operand, the
    /// lines from its first char to its last.
    fn lines(self, document: &Document) -> Lines {
        match self {
            Self::Charwise(range) => {
                let last = range.end.saturating_sub(1).max(range.start);
                Lines::spanning(document, range.start, last)
            }
            Self::Linewise(lines) => lines,
        }
    }

    /// The text the operand puts in a register.
    fn register_text(&self, document: &Document) -> RegisterText {
        match self {
            Self::Charwise(range) => {
                RegisterText::new(RegisterKind::Charwise, document.slice(range.clone()))
            }
            Self::Linewise(lines) => {
                let start = start_of_line(document, lines.first);
                let end = start_of_line(document, lines.last + 1);
                RegisterText::new(RegisterKind::Linewise, document.slice(start..end))
            }
        }
    }
}

// ========================================================================
// Functions
// ========================================================================

/// Cancels the waiting operator, back to Normal mode.
pub(crate) fn cancel_operator(cx: &mut CommandContext<'_>) {
    cx.editor.set_mode(cx.view, Mode::Normal);
}

/// `>` before a motion: waits for it and indents the lines it covers one
/// level, or the count of lines with `>>`.
pub(crate) fn indent_operator(cx: &mut CommandContext<'_>) {
    begin_or_double(cx, Operator::Indent);
}

/// `gu`: waits for a motion and lowercases what it covers (`guu` lines);
/// in a visual mode, lowercases the selection (also `u`).
pub(crate) fn lowercase_operator(cx: &mut CommandContext<'_>) {
    begin_or_double(cx, Operator::Lowercase);
}

/// `<` before a motion: waits for it and outdents the lines it covers one
/// level, or the count of lines with `<<`.
pub(crate) fn outdent_operator(cx: &mut CommandContext<'_>) {
    begin_or_double(cx, Operator::Outdent);
}

/// `s`: changes the count of characters from the cursor, staying on the
/// line, as `cl` does; in a visual mode, changes the selection.
pub(crate) fn substitute_chars(cx: &mut CommandContext<'_>) {
    if act_on_selection(cx, Operator::Change) {
        return;
    }
    let target = chars_right_target(cx);
    act_now(cx, Operator::Change, MotionKind::Exclusive, target);
}

/// `S`: changes the count of lines from the cursor's, as `cc` does; in a
/// visual mode, changes the selected lines.
pub(crate) fn substitute_lines(cx: &mut CommandContext<'_>) {
    let pending = PendingOperator {
        count: None,
        operator: Operator::Change,
        register: cx.register,
    };
    let cursor = cx.editor.view_ref(cx.view).cursor();
    if let Some((anchor, _)) = selection(cx.editor, cx.view) {
        return apply_operator(
            cx.editor,
            cx.view,
            pending,
            anchor,
            MotionKind::Linewise,
            cursor,
        );
    }
    let target = lines_down_target(cx);
    act_now(cx, Operator::Change, MotionKind::Linewise, target);
}

/// `~`: swaps the case of the count of characters from the cursor and
/// moves past them, staying on the line; in a visual mode, swaps the case
/// of the selection.
pub(crate) fn toggle_case_chars(cx: &mut CommandContext<'_>) {
    if act_on_selection(cx, Operator::ToggleCase) {
        return;
    }
    let target = chars_right_target(cx);
    act_now(cx, Operator::ToggleCase, MotionKind::Exclusive, target);
    let target = target.min(last_cursor_offset(cx.editor, cx.view, target));
    cx.editor.set_cursor(cx.view, target);
}

/// `g~`: waits for a motion and swaps the case of what it covers (`g~~`
/// lines).
pub(crate) fn toggle_case_operator(cx: &mut CommandContext<'_>) {
    begin_or_double(cx, Operator::ToggleCase);
}

/// `gU`: waits for a motion and uppercases what it covers (`gUU` lines);
/// in a visual mode, uppercases the selection (also `U`).
pub(crate) fn uppercase_operator(cx: &mut CommandContext<'_>) {
    begin_or_double(cx, Operator::Uppercase);
}

/// `c`: waits for a motion to change over, or changes lines when typed
/// twice.
pub(crate) fn change_operator(cx: &mut CommandContext<'_>) {
    begin_or_double(cx, Operator::Change);
}

/// `C`: changes to the end of the line, the count less one lines down.
pub(crate) fn change_to_line_end(cx: &mut CommandContext<'_>) {
    let target = line_end_target(cx);
    act_now(cx, Operator::Change, MotionKind::Exclusive, target);
}

/// `gc`: waits for a motion and comments or uncomments the lines it
/// covers, or the cursor's line and the count less one below when `c` or
/// `gc` follows.
pub(crate) fn comment_operator(cx: &mut CommandContext<'_>) {
    begin_or_double(cx, Operator::Comment);
}

/// `gq`: waits for a motion and refills the lines it covers to the text
/// width, or the cursor's line and the count less one below when `q` or
/// `gq` follows.
pub(crate) fn format_operator(cx: &mut CommandContext<'_>) {
    begin_or_double(cx, Operator::Format);
}

/// `x`: deletes the character under the cursor, and more to the right
/// with a count, staying on the line.
pub(crate) fn delete_char(cx: &mut CommandContext<'_>) {
    if act_on_selection(cx, Operator::Delete) {
        return;
    }
    let target = chars_right_target(cx);
    act_now(cx, Operator::Delete, MotionKind::Exclusive, target);
}

/// `X`: deletes the character before the cursor, and more to the left
/// with a count, staying on the line.
pub(crate) fn delete_char_before(cx: &mut CommandContext<'_>) {
    let document = cx.editor.document_of(cx.view);
    let cursor = cx.editor.view_ref(cx.view).cursor();
    let start = line_start(document, cursor);
    let mut target = cursor;
    for _ in 0..cx.count_or_one() {
        target = document.prev_grapheme_boundary(target).max(start);
    }
    act_now(cx, Operator::Delete, MotionKind::Exclusive, target);
}

/// `d`: waits for a motion to delete over, or deletes lines when typed
/// twice.
pub(crate) fn delete_operator(cx: &mut CommandContext<'_>) {
    begin_or_double(cx, Operator::Delete);
}

/// `D`: deletes to the end of the line, the count less one lines down.
pub(crate) fn delete_to_line_end(cx: &mut CommandContext<'_>) {
    let target = line_end_target(cx);
    act_now(cx, Operator::Delete, MotionKind::Exclusive, target);
}

/// Runs the waiting operator of the view over the text a motion of `kind`
/// covers from the cursor to `target`, and returns whether one was
/// waiting; when none was, the motion should move the cursor instead.
pub(crate) fn operate(cx: &mut CommandContext<'_>, kind: MotionKind, target: usize) -> bool {
    let Some(pending) = cx.editor.view_ref(cx.view).operator() else {
        return false;
    };
    let from = cx.editor.view_ref(cx.view).cursor();
    apply_operator(cx.editor, cx.view, pending, from, kind, target);
    true
}

/// Runs the waiting operator of the view over `operand`, a text object,
/// and returns whether one was waiting; a yank leaves the cursor at the
/// object's start.
pub(crate) fn operate_on(cx: &mut CommandContext<'_>, operand: Operand) -> bool {
    let Some(pending) = cx.editor.view_ref(cx.view).operator() else {
        return false;
    };
    let start = operand.chars(cx.editor.document_of(cx.view)).start;
    run_operator(cx.editor, cx.view, pending, operand, start);
    true
}

/// `p`: puts the register's text after the cursor, or below its line when
/// linewise, count times.
pub(crate) fn put_after(cx: &mut CommandContext<'_>) {
    if selection(cx.editor, cx.view).is_some() {
        return replace_selection(cx, false);
    }
    put(cx, true);
}

/// `P`: puts the register's text before the cursor, or above its line when
/// linewise, count times.
pub(crate) fn put_before(cx: &mut CommandContext<'_>) {
    if selection(cx.editor, cx.view).is_some() {
        return replace_selection(cx, true);
    }
    put(cx, false);
}

/// `y`: waits for a motion to yank over, or yanks lines when typed twice.
pub(crate) fn yank_operator(cx: &mut CommandContext<'_>) {
    begin_or_double(cx, Operator::Yank);
}

/// `Y`: yanks the cursor's line and the count less one lines below, as
/// `yy` does.
pub(crate) fn yank_line(cx: &mut CommandContext<'_>) {
    let target = lines_down_target(cx);
    act_now(cx, Operator::Yank, MotionKind::Linewise, target);
}

/// Runs `operator` over the visual selection, when the view has one, and
/// returns whether it did.
fn act_on_selection(cx: &mut CommandContext<'_>, operator: Operator) -> bool {
    if let Some(block) = block_of(cx.editor, cx.view) {
        run_block_operator(cx, block, operator, 1);
        return true;
    }
    let Some((anchor, kind)) = selection(cx.editor, cx.view) else {
        return false;
    };
    let pending = PendingOperator {
        count: None,
        operator,
        register: cx.register,
    };
    let cursor = cx.editor.view_ref(cx.view).cursor();
    apply_operator(cx.editor, cx.view, pending, anchor, kind, cursor);
    true
}

/// Runs `operator` at once over the text a motion of `kind` covers from the
/// cursor to `target`, with the command's register.
fn act_now(cx: &mut CommandContext<'_>, operator: Operator, kind: MotionKind, target: usize) {
    let pending = PendingOperator {
        count: cx.count,
        operator,
        register: cx.register,
    };
    let from = cx.editor.view_ref(cx.view).cursor();
    apply_operator(cx.editor, cx.view, pending, from, kind, target);
}

/// Runs `pending` over the text a motion of `kind` covers from `from` to
/// `target`, back in Normal mode (or in Insert mode after a change).
fn apply_operator(
    editor: &mut Editor,
    view: ViewId,
    pending: PendingOperator,
    from: usize,
    kind: MotionKind,
    target: usize,
) {
    let operand = operand(editor.document_of(view), from, kind, target);
    run_operator(editor, view, pending, operand, from.min(target));
}

/// Runs `pending` over `operand`, back in Normal mode (or in Insert mode
/// after a change); a yank leaves the cursor at `yank_cursor`. Only a yank
/// runs in a read-only document, so a refused delete keeps the registers.
fn run_operator(
    editor: &mut Editor,
    view: ViewId,
    pending: PendingOperator,
    operand: Operand,
    yank_cursor: usize,
) {
    editor.set_mode(view, Mode::Normal);
    if pending.operator != Operator::Yank && editor.refuse_if_read_only(view) {
        return;
    }
    let document = editor.document_of(view);
    match pending.operator {
        Operator::Change => change_operand(editor, view, pending.register, operand),
        Operator::Delete => delete_operand(editor, view, pending.register, operand),
        Operator::Comment => {
            let lines = operand.lines(document);
            toggle_comment(editor, view, lines.first, lines.last);
        }
        Operator::Format => {
            let lines = operand.lines(document);
            reflow_lines(editor, view, lines.first, lines.last);
        }
        Operator::Indent => {
            let lines = operand.lines(document);
            shift_line_range(editor, view, (lines.first, lines.last), 1, Shift::Right);
        }
        Operator::Outdent => {
            let lines = operand.lines(document);
            shift_line_range(editor, view, (lines.first, lines.last), 1, Shift::Left);
        }
        Operator::Lowercase | Operator::ToggleCase | Operator::Uppercase => {
            change_operand_case(editor, view, pending.operator, operand, yank_cursor);
        }
        Operator::Surround => wrap_operand(editor, view, &operand),
        Operator::Yank => yank_operand(editor, view, pending.register, operand, yank_cursor),
    }
}

/// Deletes `operand` into `register` and starts Insert mode in its place,
/// keeping the indent of a linewise operand; the delete and what is typed
/// after it are one undo step.
fn change_operand(editor: &mut Editor, view: ViewId, register: Option<char>, operand: Operand) {
    let document = editor.document_of(view);
    let text = operand.register_text(document);
    let range = match &operand {
        Operand::Charwise(range) => range.clone(),
        Operand::Linewise(lines) => lines.text_inside(document),
    };
    editor.history_mut(view).begin_group();
    if !text.text.is_empty() {
        editor.registers_mut().record_delete(register, text);
    }
    delete(editor, view, range.clone());
    editor.set_cursor(view, range.start);
    editor.set_mode(view, Mode::Insert);
    mark_insert_start(editor, view);
}

/// Changes the case of the chars of `operand` as `operator` says, leaving
/// the cursor at `yank_cursor` for a charwise operand and at the start of
/// a linewise one.
fn change_operand_case(
    editor: &mut Editor,
    view: ViewId,
    operator: Operator,
    operand: Operand,
    yank_cursor: usize,
) {
    let document = editor.document_of(view);
    let range = operand.chars(document);
    let cursor = match operand {
        Operand::Charwise(_) => yank_cursor,
        Operand::Linewise(_) => range.start,
    };
    let original = document.slice(range.clone());
    let changed = change_case(&original, operator);
    if changed != original {
        editor.apply_edits(view, vec![Edit::new(range, changed)]);
    }
    editor.set_cursor(view, cursor);
}

/// Deletes `operand` into `register`, leaving the cursor where the text
/// was (on the first non-blank of the line after deleted lines); an empty
/// operand deletes nothing.
fn delete_operand(editor: &mut Editor, view: ViewId, register: Option<char>, operand: Operand) {
    let document = editor.document_of(view);
    let range = operand.deleted(document);
    if range.is_empty() {
        return;
    }
    let text = operand.register_text(document);
    editor.registers_mut().record_delete(register, text);
    delete(editor, view, range.clone());
    let cursor = match operand {
        Operand::Charwise(_) => range
            .start
            .min(last_cursor_offset(editor, view, range.start)),
        Operand::Linewise(lines) => {
            let document = editor.document_of(view);
            let line = lines.first.min(last_line(document));
            first_non_blank_of_line(editor, view, start_of_line(document, line))
        }
    };
    editor.set_cursor(view, cursor);
}

/// Yanks `operand` into `register`, marking where it starts and ends with
/// `[` and `]`, and leaves the cursor at `yank_cursor`.
fn yank_operand(
    editor: &mut Editor,
    view: ViewId,
    register: Option<char>,
    operand: Operand,
    yank_cursor: usize,
) {
    let document = editor.document_of(view);
    let range = operand.chars(document);
    let text = operand.register_text(document);
    if !text.text.is_empty() {
        editor.registers_mut().record_yank(register, text);
    }
    let document = editor.document_of(view);
    let mut last = document.prev_grapheme_boundary(range.end).max(range.start);
    if last > range.start && document.char_at(last) == Some('\n') {
        last = document.prev_grapheme_boundary(last);
    }
    editor.marks_mut(view).set(CHANGE_START_MARK, range.start);
    editor.marks_mut(view).set(CHANGE_END_MARK, last);
    editor.set_cursor(view, yank_cursor);
}

/// `text` with its case changed as `operator`, a case operator, says.
fn change_case(text: &str, operator: Operator) -> String {
    match operator {
        Operator::Lowercase => text.to_lowercase(),
        Operator::Uppercase => text.to_uppercase(),
        Operator::ToggleCase => text
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
        Operator::Change
        | Operator::Comment
        | Operator::Delete
        | Operator::Format
        | Operator::Indent
        | Operator::Outdent
        | Operator::Surround
        | Operator::Yank => text.to_string(),
    }
}

/// The target of the count of characters right of the cursor, kept on its
/// line: what `x`, `s`, and `~` act up to.
fn chars_right_target(cx: &CommandContext<'_>) -> usize {
    let document = cx.editor.document_of(cx.view);
    let cursor = cx.editor.view_ref(cx.view).cursor();
    let end = line_end(document, cursor);
    let mut target = cursor;
    for _ in 0..cx.count_or_one() {
        target = document.next_grapheme_boundary(target).min(end);
    }
    target
}

/// Starts `operator`, waiting for its motion; typed again while waiting,
/// runs it over the cursor's line and the count less one below (so does
/// `c` after `gc`, for `gcc`); typed while another operator waits, cancels
/// that one.
fn begin_or_double(cx: &mut CommandContext<'_>, operator: Operator) {
    if act_on_selection(cx, operator) {
        return;
    }
    match cx.editor.view_ref(cx.view).operator() {
        Some(pending) if doubles(pending.operator, operator) => {
            let target = lines_down_target(cx);
            operate(cx, MotionKind::Linewise, target);
        }
        Some(_) => cx.editor.set_mode(cx.view, Mode::Normal),
        None => {
            let pending = PendingOperator {
                count: cx.count,
                operator,
                register: cx.register,
            };
            cx.editor.set_mode(cx.view, Mode::OperatorPending);
            cx.editor.set_operator(cx.view, pending);
        }
    }
}

/// Whether typing the key of `typed` while `pending` waits acts on lines:
/// the same operator again, or `c` after `gc`.
fn doubles(pending: Operator, typed: Operator) -> bool {
    pending == typed || (pending, typed) == (Operator::Comment, Operator::Change)
}

/// Deletes `range` from the document of `view`, as one undo step or part
/// of the open group; an empty range changes nothing.
fn delete(editor: &mut Editor, view: ViewId, range: Range<usize>) {
    if range.is_empty() {
        return;
    }
    editor.apply_edits(view, vec![Edit::new(range, "")]);
}

/// The target of `$` with the command's count: the end of the line the
/// count less one lines down.
fn line_end_target(cx: &CommandContext<'_>) -> usize {
    let document = cx.editor.document_of(cx.view);
    let cursor = cx.editor.view_ref(cx.view).cursor();
    let last = last_line(document);
    let line = (line_of(document, cursor) + cx.count_or_one() - 1).min(last);
    line_end(document, start_of_line(document, line))
}

/// The start of the line the count less one lines below the cursor's, for
/// a linewise operator over that many lines.
fn lines_down_target(cx: &CommandContext<'_>) -> usize {
    let document = cx.editor.document_of(cx.view);
    let cursor = cx.editor.view_ref(cx.view).cursor();
    let last = last_line(document);
    let line = (line_of(document, cursor) + cx.count_or_one() - 1).min(last);
    start_of_line(document, line)
}

/// The text a motion of `kind` from `from` to `target` covers, with Vim's
/// adjustment of an exclusive motion ending at the start of a later line.
fn operand(document: &Document, from: usize, kind: MotionKind, target: usize) -> Operand {
    let (start, end) = (from.min(target), from.max(target));
    match kind {
        MotionKind::Exclusive => {
            let ends_at_line_start = end > start
                && end == line_start(document, end)
                && line_of(document, end) > line_of(document, start);
            if !ends_at_line_start {
                return Operand::Charwise(start..end);
            }
            if start <= first_non_blank_in(document, start) {
                return Operand::Linewise(Lines::spanning(document, start, end - 1));
            }
            Operand::Charwise(start..end - 1)
        }
        MotionKind::Inclusive => Operand::Charwise(start..document.next_grapheme_boundary(end)),
        MotionKind::Linewise => Operand::Linewise(Lines::spanning(document, start, end)),
    }
}

/// Puts the register's text after or before the cursor, count times, and
/// leaves the cursor where Vim does: on the first non-blank of the first
/// put line, the last char of single-line text, or the start of
/// multi-line text.
fn put(cx: &mut CommandContext<'_>, after: bool) {
    let Some(text) = cx.editor.read_register(cx.view, cx.register) else {
        let name = cx.register.unwrap_or('"');
        cx.editor.error(format!("Nothing in register {name}"));
        return;
    };
    let repeated = text.text.repeat(cx.count_or_one());
    let document = cx.editor.document_of(cx.view);
    let cursor = cx.editor.view_ref(cx.view).cursor();
    match text.kind {
        RegisterKind::Blockwise => {
            put_block(cx.editor, cx.view, &text.text, after, cx.count_or_one());
        }
        RegisterKind::Linewise => {
            let line = line_of(document, cursor);
            let (at, inserted, put_line) = match (after, line + 1 < document.len_lines()) {
                (true, true) => (start_of_line(document, line + 1), repeated, line + 1),
                (true, false) => {
                    let body = repeated.strip_suffix(LINE_BREAK).unwrap_or(&repeated);
                    let inserted = format!("{LINE_BREAK}{body}");
                    (document.len_chars(), inserted, line + 1)
                }
                (false, _) => (start_of_line(document, line), repeated, line),
            };
            cx.editor
                .apply_edits(cx.view, vec![Edit::new(at..at, inserted)]);
            let line_start = start_of_line(cx.editor.document_of(cx.view), put_line);
            let target = first_non_blank_of_line(cx.editor, cx.view, line_start);
            cx.editor.set_cursor(cx.view, target);
        }
        RegisterKind::Charwise => {
            let at = if after && cursor < line_end(document, cursor) {
                document.next_grapheme_boundary(cursor)
            } else {
                cursor
            };
            let len = repeated.chars().count();
            let is_multi_line = repeated.contains(LINE_BREAK);
            cx.editor
                .apply_edits(cx.view, vec![Edit::new(at..at, repeated)]);
            let target = if is_multi_line || len == 0 {
                at
            } else {
                cx.editor
                    .document_of(cx.view)
                    .prev_grapheme_boundary(at + len)
            };
            cx.editor.set_cursor(cx.view, target);
        }
    }
}

// ========================================================================
// Tests
// ========================================================================

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use crate::register::{Clipboard, ClipboardSelection};
    use crate::testing::{Harness, assert_keys};
    use crate::view::Mode;

    /// The text register `name` holds after `keys`, typed in `before`.
    fn register_after(before: &str, keys: &str, name: char) -> Option<String> {
        let mut harness = Harness::new(before);
        harness.keys(keys);
        let text = harness.editor.registers_mut().read(Some(name));
        text.map(|text| text.text)
    }

    #[test]
    fn test_d_with_a_motion_deletes_what_the_motion_covers_by_its_kind() {
        assert_keys("foo |bar baz", "d w", "foo |baz");
        assert_keys("foo |bar baz", "d e", "foo | baz");
        assert_keys("foo b|ar baz", "d b", "foo |ar baz");
        assert_keys("ab|cdef", "d $", "a|b");
        assert_keys("ab|cdef", "d 0", "|cdef");
        assert_keys("ab|c\nde\nfg", "d j", "|fg");
        assert_keys("ab\nde\nf|g", "d k", "|ab");
        assert_keys("a\nb|c\nd", "d G", "|a");
        assert_keys("a\nb|c\nd", "d g g", "|d");
    }

    #[test]
    fn test_counts_on_both_sides_of_the_operator_multiply() {
        assert_keys("|a b c d e f g h", "2 d 3 w", "|g h");
        assert_keys("|a b c d e f g h", "d 3 w", "|d e f g h");
        assert_keys("|1\n2\n3\n4\n5\n6\n7", "2 d 2 d", "|5\n6\n7");
    }

    #[test]
    fn test_dw_on_the_last_word_of_a_line_does_not_join_the_next_line() {
        assert_keys("one |two\nthree", "d w", "one| \nthree");
        assert_keys("|a b\nc d", "2 d w", "|\nc d");
        assert_keys("one\n|\nthree", "d w", "one\n|three");
    }

    #[test]
    fn test_doubled_operators_act_on_whole_lines() {
        assert_keys("a\n  b|c\nd", "d d", "a\n|d");
        assert_keys("a\nb\n|c", "d d", "a\n|b");
        assert_keys("|only", "d d", "|");
        assert_keys("a\n  b|c\nd", "c c X <esc>", "a\n|X\nd");
        assert_keys("|1\n2\n3", "9 d d", "|");
    }

    #[test]
    fn test_cw_changes_to_the_end_of_the_word_like_ce() {
        assert_keys("|foo bar", "c w X <esc>", "|X bar");
        assert_keys("fo|o bar", "c w X <esc>", "fo|X bar");
        assert_keys("|foo bar baz", "c 2 w X <esc>", "|X baz");
        assert_keys("foo|  bar", "c w X <esc>", "foo|Xbar");
    }

    #[test]
    fn test_a_change_and_what_is_typed_after_it_undo_as_one_step() {
        assert_keys("foo |bar baz", "c w new <esc> u", "foo |bar baz");
        assert_keys("a\n|b\nc", "c c new <esc> u", "a\n|b\nc");
    }

    #[test]
    fn test_every_operator_is_one_undo_step() {
        assert_keys("|a b c d", "d 3 w u", "|a b c d");
        assert_keys("|a\nb\nc", "d j u", "|a\nb\nc");
        assert_keys("ab|c", "y w p u", "ab|c");
    }

    #[test]
    fn test_escape_or_an_unbound_key_cancels_the_operator() {
        let mut harness = Harness::new("|abc");
        harness.keys("d");
        assert_eq!(harness.mode(), Mode::OperatorPending);
        harness.keys("<esc>");
        assert_eq!(harness.mode(), Mode::Normal);
        harness.keys("d Z l");
        assert_eq!(harness.mode(), Mode::Normal);
        assert_eq!(harness.marked(), "a|bc", "l moved after the cancel");
        assert_keys("|abc", "d y l", "a|bc");
    }

    #[test]
    fn test_j_or_k_that_cannot_move_cancel_the_operator() {
        assert_keys("a\n|b", "d j", "a\n|b");
        assert_keys("|a\nb", "d k", "|a\nb");
    }

    #[test]
    fn test_short_forms_x_capital_x_d_c_and_y() {
        assert_keys("a|bcd", "x", "a|cd");
        assert_keys("a|bcd", "5x", "|a");
        assert_keys("|", "x", "|");
        assert_keys("ab|cd", "X", "a|cd");
        assert_keys("ab|cd", "9X", "|cd");
        assert_keys("ab|cd\nef", "D", "a|b\nef");
        assert_keys("ab|cd\nef\ngh", "2D", "a|b\ngh");
        assert_keys("ab|cd", "C X <esc>", "ab|X");
        assert_keys("a|b\nc", "Y j p", "ab\nc\n|ab");
    }

    #[test]
    fn test_x_on_multibyte_text_deletes_whole_clusters() {
        assert_keys("|日本語", "x", "|本語");
        assert_keys("a|e\u{301}b", "x", "a|b");
        assert_keys("日本|語", "X", "日|語");
    }

    #[test]
    fn test_an_exclusive_motion_ending_at_a_line_start_stops_at_the_line_end_before() {
        assert_keys("  |foo\nbar", "d w", " | \nbar");
        assert_keys("foo\n|bar", "d b", "|bar");
        assert_keys("x foo\n|bar", "d b", "x| \nbar");
    }

    #[test]
    fn test_charwise_put_goes_after_or_before_the_cursor_and_ends_on_its_last_char() {
        assert_keys("|abc", "x p", "b|ac");
        assert_keys("|abc", "x P", "|abc");
        assert_keys("|abc", "x 3p", "baa|ac");
        assert_keys("a|b", "x p", "a|b");
        assert_keys("|日本", "x p", "本|日");
    }

    #[test]
    fn test_linewise_put_goes_below_or_above_the_line_on_its_first_non_blank() {
        assert_keys("|  one\ntwo", "d d p", "two\n  |one");
        assert_keys("one\n|  two", "y y k P", "  |two\none\n  two");
        assert_keys("one\n|two", "y y 2p", "one\ntwo\n|two\ntwo");
        assert_keys("|one\ntwo", "y j G p", "one\ntwo\n|one\ntwo");
    }

    #[test]
    fn test_multi_line_charwise_put_leaves_the_cursor_at_its_start() {
        assert_keys("a|b\ncd ef", "d e p", "a |b\ncdef");
    }

    #[test]
    fn test_a_register_selected_with_a_quote_takes_the_text() {
        assert_eq!(
            register_after("|foo bar", "\" a d w", 'a').as_deref(),
            Some("foo ")
        );
        assert_keys("|foo bar", "\" a d w \" a P", "foo| bar");
        assert_keys("|foo bar", "\" a y w w \" A y w \" a P", "foo foo ba|rbar");
        assert_keys("|a\nb", "\" _ d d p", "|b");
    }

    #[test]
    fn test_yanks_fill_register_zero_and_deletes_the_numbered_and_small_ones() {
        assert_eq!(
            register_after("|foo bar", "y w", '0').as_deref(),
            Some("foo ")
        );
        assert_eq!(register_after("|a\nb", "d d", '1').as_deref(), Some("a\n"));
        assert_eq!(register_after("a\n|b", "d d", '1').as_deref(), Some("b\n"));
        assert_eq!(
            register_after("|foo bar", "d w", '-').as_deref(),
            Some("foo ")
        );
        assert_eq!(
            register_after("|foo bar", "y w d w", '0').as_deref(),
            Some("foo ")
        );
        assert_keys("|one\ntwo", "y y j d d \" 0 p", "one\n|one");
    }

    #[test]
    fn test_putting_an_empty_register_says_so() {
        let mut harness = Harness::new("|x");
        harness.keys("\" q p");
        assert_eq!(harness.marked(), "|x");
        let requests = harness.requests();
        assert_eq!(requests.len(), 1, "{requests:?}");
    }

    #[test]
    fn test_plus_yanks_to_the_host_clipboard_and_puts_from_it() {
        let mut harness = Harness::new("|word\nline");
        harness.keys("\" + y w j \" + P");
        assert_eq!(harness.marked(), "word\nwor|dline");
    }

    #[test]
    fn test_a_count_works_on_either_side_of_the_register() {
        assert_keys("|a\nb\nc\nd", "2 \" a y y G \" a p", "a\nb\nc\nd\n|a\nb");
        assert_keys("|a\nb\nc\nd", "\" a 2 y y G \" a p", "a\nb\nc\nd\n|a\nb");
    }

    #[test]
    fn test_a_quote_before_a_key_that_names_no_register_says_so_and_drops_the_count() {
        let mut harness = Harness::new("|abc");
        harness.keys("2 \" ! x");
        assert_eq!(harness.marked(), "|bc", "x ran once, without the count");
        assert_eq!(harness.requests().len(), 1);
    }

    #[test]
    fn test_the_host_clipboard_set_on_the_editor_backs_plus_and_star() {
        #[derive(Debug, Default)]
        struct Host(Arc<Mutex<Vec<(ClipboardSelection, String)>>>);
        impl Clipboard for Host {
            fn read(&mut self, _: ClipboardSelection) -> Option<String> {
                Some("from host".to_string())
            }
            fn write(&mut self, selection: ClipboardSelection, text: &str) {
                self.0.lock().unwrap().push((selection, text.to_string()));
            }
        }
        let writes = Arc::default();
        let mut harness = Harness::new("|one two");
        harness.editor.set_clipboard(Host(Arc::clone(&writes)));
        harness.keys("\" * y w \" + P");
        assert_eq!(harness.marked(), "from hos|tone two");
        let writes = writes.lock().unwrap().clone();
        assert_eq!(writes, [(ClipboardSelection::Primary, "one ".to_string())]);
    }

    #[test]
    fn test_a_yank_moves_the_cursor_to_the_start_of_what_it_yanked() {
        assert_keys("foo b|ar", "y b", "foo |bar");
        assert_keys("a\n|b", "y k", "|a\nb");
        assert_keys("|a\nb", "y j", "|a\nb");
    }

    #[test]
    fn test_tilde_swaps_case_and_moves_and_case_operators_take_motions() {
        assert_keys("|aBc", "~", "A|Bc");
        assert_keys("|aBc", "5~", "Ab|C");
        assert_keys("|ab cd", "g U w", "|AB cd");
        assert_keys("|AB cd", "g u i w", "|ab cd");
        assert_keys("|aB cd", "g ~ e", "|Ab cd");
        assert_keys("|stra\u{df}e", "g U $", "|STRASSE");
    }

    #[test]
    fn test_doubled_case_operators_act_on_lines() {
        assert_keys("a|b\ncd\nef", "g U U", "|AB\ncd\nef");
        assert_keys("a|b\ncd\nef", "2 g U g U", "|AB\nCD\nef");
        assert_keys("A|B\nCD", "g u u", "|ab\nCD");
        assert_keys("a|B\ncd", "g ~ ~", "|Ab\ncd");
        assert_keys("|ab", "d u", "|ab");
    }

    #[test]
    fn test_visual_case_keys_act_on_the_selection() {
        assert_keys("|abc def", "v e U", "|ABC def");
        assert_keys("|ABC def", "v e u", "|abc def");
        assert_keys("|aBc def", "v e ~", "|AbC def");
        assert_keys("|ab\ncd", "V j U", "|AB\nCD");
    }

    #[test]
    fn test_shift_operators_indent_lines_by_motion_and_count() {
        assert_keys("|a\nb\nc", ">>", "\t|a\nb\nc");
        assert_keys("|a\nb\nc", "2>>", "\t|a\n\tb\nc");
        assert_keys("|a\nb\nc", "> j", "\t|a\n\tb\nc");
        assert_keys("\t\t|a\n\tb", "<<", "\t|a\n\tb");
        assert_keys("\t|a\n\tb", "< G", "|a\nb");
        assert_keys("|a\n\nb", "3>>", "\t|a\n\n\tb");
        assert_keys("|a\nb", ">> j .", "\ta\n\t|b");
    }

    #[test]
    fn test_s_changes_characters_and_capital_s_whole_lines() {
        assert_keys("a|bcd", "s X <esc>", "a|Xcd");
        assert_keys("a|bcd", "2s X <esc>", "a|Xd");
        assert_keys("ab\n|cd\nef", "S X <esc>", "ab\n|X\nef");
        assert_keys("|ab\ncd\nef", "2S X <esc>", "|X\nef");
        assert_keys("|abc", "v l s X <esc>", "|Xc");
        assert_keys("a|b\ncd", "v S X <esc>", "|X\ncd");
    }
}
