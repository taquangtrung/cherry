//! Emacs line editing: the commands that move by characters and words,
//! delete, kill, transpose, and change case where the user types, in Insert mode and in prompts
//! alike. Each command is planned over a document and a cursor as a
//! [`LineChange`] (the edit, the cursor after it, the text killed), which
//! Insert mode applies to the view's primary cursor and a prompt applies
//! to its own line.
//!
//! # Words
//!
//! A word is a run of letters, digits, and underscores, taken a grapheme
//! cluster at a time; everything else separates words. `M-f` moves to the
//! end of the next word, `M-b` to the start of the previous one, both
//! across lines. `C-w` kills back to the previous blank instead, as the
//! shell's does, so it takes `path/to/file` whole.
//!
//! # Transposing
//!
//! `C-t` swaps the characters before and under the cursor and moves past
//! both; at the end of a line it swaps the two before; at the start of a
//! line it does nothing. `M-t` swaps the word before the cursor with the
//! word after it (from inside a word, that word with the next), the cursor
//! after both; with no word after the cursor, the last two words before it.
//! `C-x C-t` swaps the cursor's line with the one above and moves to the
//! start of the next line, so pressing it again drags the line down; on
//! the first line it does nothing.
//!
//! # Case
//!
//! `M-u`, `M-l`, and `M-c` upcase, downcase, or capitalize from the cursor
//! to the end of the next word, and move there. Capitalizing upcases the
//! first letter or digit and downcases the rest.
//!
//! # Moving far and spacing
//!
//! `M-m` moves to the first non-blank of the line, `M-<` to the start of
//! the document, and `M->` to its end. `M-SPC` leaves one space where the
//! cursor is among blanks: a run of blanks around it becomes one space,
//! the cursor after it; a lone space goes; no blanks get a space.

use std::ops::Range;

use super::command::CommandContext;
use super::document::{Document, first_non_blank_in, line_end, line_of, line_start, start_of_line};
use super::kill::{Kill, KillDirection};
use super::motion::{CharClass, class_at};
use super::transaction::Edit;

// ========================================================================
// Data Structures
// ========================================================================

/// An Emacs line editing command.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum LineCommand {
    /// `M-m`: move to the first non-blank of the line.
    BackToIndentation,
    /// `C-b`: move back a character.
    BackwardChar,
    /// `M-<bs>`: kill back to the start of a word.
    BackwardKillWord,
    /// `M-b`: move back to the start of a word.
    BackwardWord,
    /// `M-c`: capitalize to the end of a word.
    CapitalizeWord,
    /// `M-SPC`: leave one space among the blanks around the cursor, or
    /// none for a lone space.
    CycleSpacing,
    /// `<bs>`: delete the character before the cursor.
    DeleteBackwardChar,
    /// `C-d`: delete the character under the cursor.
    DeleteChar,
    /// `M->`: move to the end of the document.
    DocumentEnd,
    /// `M-<`: move to the start of the document.
    DocumentStart,
    /// `M-l`: downcase to the end of a word.
    DowncaseWord,
    /// `C-f`: move forward a character.
    ForwardChar,
    /// `M-f`: move to the end of a word.
    ForwardWord,
    /// `C-k`: kill to the end of the line, or the line break at its end.
    KillLine,
    /// `C-u`: kill back to the start of the line.
    KillToLineStart,
    /// `M-d`: kill to the end of a word.
    KillWord,
    /// `C-e`: move to the end of the line.
    LineEnd,
    /// `C-a`: move to the start of the line.
    LineStart,
    /// `C-t`: swap two characters.
    TransposeChars,
    /// `C-x C-t`: swap the line with the one above.
    TransposeLines,
    /// `M-t`: swap two words.
    TransposeWords,
    /// `C-w`: kill back to the previous blank.
    UnixWordRubout,
    /// `M-u`: upcase to the end of a word.
    UpcaseWord,
}

/// What a line editing command does: the edit to make, if any, where the
/// cursor goes after it, and the text it kills, if any.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct LineChange {
    /// Where the cursor goes, in the text after the edit.
    pub cursor: usize,
    /// The edit to make.
    pub edit: Option<Edit>,
    /// The text killed, for the kill ring.
    pub kill: Option<Kill>,
}

/// How a word's case is changed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Case {
    /// The first letter or digit up, the rest down.
    Capitalize,
    /// Every letter down.
    Lower,
    /// Every letter up.
    Upper,
}

// ========================================================================
// LineChange Implementations
// ========================================================================

impl LineChange {
    /// The cursor moved to `cursor`, the text unchanged.
    fn moved(cursor: usize) -> Self {
        Self {
            cursor,
            edit: None,
            kill: None,
        }
    }
}

// ========================================================================
// Functions
// ========================================================================

/// What `command` does in `document` with the cursor at `cursor`, as the
/// module documentation describes.
pub(crate) fn plan(document: &Document, cursor: usize, command: LineCommand) -> LineChange {
    match command {
        LineCommand::BackToIndentation => LineChange::moved(first_non_blank_in(document, cursor)),
        LineCommand::BackwardChar => LineChange::moved(document.prev_grapheme_boundary(cursor)),
        LineCommand::BackwardKillWord => {
            let start = backward_word_start(document, cursor);
            killed(document, start..cursor, KillDirection::Backward)
        }
        LineCommand::BackwardWord => LineChange::moved(backward_word_start(document, cursor)),
        LineCommand::CapitalizeWord => recased(document, cursor, Case::Capitalize),
        LineCommand::CycleSpacing => spaced(document, cursor),
        LineCommand::DeleteBackwardChar => deleted(document.prev_grapheme_boundary(cursor)..cursor),
        LineCommand::DeleteChar => deleted(cursor..document.next_grapheme_boundary(cursor)),
        LineCommand::DocumentEnd => LineChange::moved(document.len_chars()),
        LineCommand::DocumentStart => LineChange::moved(0),
        LineCommand::DowncaseWord => recased(document, cursor, Case::Lower),
        LineCommand::ForwardChar => LineChange::moved(document.next_grapheme_boundary(cursor)),
        LineCommand::ForwardWord => LineChange::moved(forward_word_end(document, cursor)),
        LineCommand::KillLine => {
            let end = line_end(document, cursor);
            let end = if cursor == end {
                document.next_grapheme_boundary(cursor)
            } else {
                end
            };
            killed(document, cursor..end, KillDirection::Forward)
        }
        LineCommand::KillToLineStart => {
            let start = line_start(document, cursor);
            killed(document, start..cursor, KillDirection::Backward)
        }
        LineCommand::KillWord => {
            let end = forward_word_end(document, cursor);
            killed(document, cursor..end, KillDirection::Forward)
        }
        LineCommand::LineEnd => LineChange::moved(line_end(document, cursor)),
        LineCommand::LineStart => LineChange::moved(line_start(document, cursor)),
        LineCommand::TransposeChars => transposed_chars(document, cursor),
        LineCommand::TransposeLines => transposed_lines(document, cursor),
        LineCommand::TransposeWords => transposed_words(document, cursor),
        LineCommand::UnixWordRubout => {
            let start = blank_word_start(document, cursor);
            killed(document, start..cursor, KillDirection::Backward)
        }
        LineCommand::UpcaseWord => recased(document, cursor, Case::Upper),
    }
}

/// `M-m` in Insert mode.
pub(crate) fn back_to_indentation(cx: &mut CommandContext<'_>) {
    run_in_view(cx, LineCommand::BackToIndentation);
}

/// `M-SPC` in Insert mode.
pub(crate) fn cycle_spacing(cx: &mut CommandContext<'_>) {
    run_in_view(cx, LineCommand::CycleSpacing);
}

/// `M->` in Insert mode.
pub(crate) fn document_end(cx: &mut CommandContext<'_>) {
    run_in_view(cx, LineCommand::DocumentEnd);
}

/// `M-<` in Insert mode.
pub(crate) fn document_start(cx: &mut CommandContext<'_>) {
    run_in_view(cx, LineCommand::DocumentStart);
}

/// `M-<bs>` in Insert mode.
pub(crate) fn backward_kill_word(cx: &mut CommandContext<'_>) {
    run_in_view(cx, LineCommand::BackwardKillWord);
}

/// `M-b` in Insert mode.
pub(crate) fn backward_word(cx: &mut CommandContext<'_>) {
    run_in_view(cx, LineCommand::BackwardWord);
}

/// `M-c` in Insert mode.
pub(crate) fn capitalize_word(cx: &mut CommandContext<'_>) {
    run_in_view(cx, LineCommand::CapitalizeWord);
}

/// `M-l` in Insert mode.
pub(crate) fn downcase_word(cx: &mut CommandContext<'_>) {
    run_in_view(cx, LineCommand::DowncaseWord);
}

/// `M-f` in Insert mode.
pub(crate) fn forward_word(cx: &mut CommandContext<'_>) {
    run_in_view(cx, LineCommand::ForwardWord);
}

/// `C-k` in Insert mode.
pub(crate) fn kill_line(cx: &mut CommandContext<'_>) {
    run_in_view(cx, LineCommand::KillLine);
}

/// `C-u` in Insert mode.
pub(crate) fn kill_to_line_start(cx: &mut CommandContext<'_>) {
    run_in_view(cx, LineCommand::KillToLineStart);
}

/// `M-d` in Insert mode.
pub(crate) fn kill_word(cx: &mut CommandContext<'_>) {
    run_in_view(cx, LineCommand::KillWord);
}

/// `C-t` in Insert mode.
pub(crate) fn transpose_chars(cx: &mut CommandContext<'_>) {
    run_in_view(cx, LineCommand::TransposeChars);
}

/// `C-x C-t` in Insert mode.
pub(crate) fn transpose_lines(cx: &mut CommandContext<'_>) {
    run_in_view(cx, LineCommand::TransposeLines);
}

/// `M-t` in Insert mode.
pub(crate) fn transpose_words(cx: &mut CommandContext<'_>) {
    run_in_view(cx, LineCommand::TransposeWords);
}

/// `C-w` in Insert mode.
pub(crate) fn unix_word_rubout(cx: &mut CommandContext<'_>) {
    run_in_view(cx, LineCommand::UnixWordRubout);
}

/// `M-u` in Insert mode.
pub(crate) fn upcase_word(cx: &mut CommandContext<'_>) {
    run_in_view(cx, LineCommand::UpcaseWord);
}

/// The start of the word before `at`: back over what is not a word, then
/// over the word.
pub(crate) fn backward_word_start(document: &Document, at: usize) -> usize {
    let at = back_while(document, at, |class| class != CharClass::Word);
    back_while(document, at, |class| class == CharClass::Word)
}

/// The end of the word after `at`: forward over what is not a word, then
/// over the word.
pub(crate) fn forward_word_end(document: &Document, at: usize) -> usize {
    let at = forward_while(document, at, |class| class != CharClass::Word);
    forward_while(document, at, |class| class == CharClass::Word)
}

/// Moves back from `at` over clusters whose class passes `keep`.
fn back_while(document: &Document, mut at: usize, keep: impl Fn(CharClass) -> bool) -> usize {
    while at > 0 {
        let before = document.prev_grapheme_boundary(at);
        if !class_at(document, before).is_some_and(&keep) {
            break;
        }
        at = before;
    }
    at
}

/// The start of the run of non-blanks before `at`, blanks before `at`
/// skipped first.
fn blank_word_start(document: &Document, at: usize) -> usize {
    let at = back_while(document, at, |class| class == CharClass::Whitespace);
    back_while(document, at, |class| class != CharClass::Whitespace)
}

/// Moves forward from `at` over clusters whose class passes `keep`.
fn forward_while(document: &Document, mut at: usize, keep: impl Fn(CharClass) -> bool) -> usize {
    while class_at(document, at).is_some_and(&keep) {
        at = document.next_grapheme_boundary(at);
    }
    at
}

/// The change deleting `range` without killing it.
fn deleted(range: Range<usize>) -> LineChange {
    LineChange {
        cursor: range.start,
        edit: Some(Edit::new(range, "")),
        kill: None,
    }
}

/// Whether the cluster before `at` is part of a word.
fn is_word_before(document: &Document, at: usize) -> bool {
    at > 0 && class_at(document, document.prev_grapheme_boundary(at)) == Some(CharClass::Word)
}

/// The change killing `range`, which went `direction` from the cursor.
fn killed(document: &Document, range: Range<usize>, direction: KillDirection) -> LineChange {
    let text = document.slice(range.clone());
    LineChange {
        cursor: range.start,
        edit: Some(Edit::new(range, "")),
        kill: Some(Kill { direction, text }),
    }
}

/// The change giving the text from `cursor` to the end of the next word
/// the `case`, the cursor after it.
fn recased(document: &Document, cursor: usize, case: Case) -> LineChange {
    let end = forward_word_end(document, cursor);
    let mut text = String::new();
    let mut seen_word = false;
    for c in document.slice(cursor..end).chars() {
        let is_word = c.is_alphanumeric();
        let upper = match case {
            Case::Capitalize => is_word && !seen_word,
            Case::Lower => false,
            Case::Upper => true,
        };
        if upper {
            text.extend(c.to_uppercase());
        } else {
            text.extend(c.to_lowercase());
        }
        seen_word |= is_word;
    }
    let len = text.chars().count();
    LineChange {
        cursor: cursor + len,
        edit: Some(Edit::new(cursor..end, text)),
        kill: None,
    }
}

/// The change of `C-t`, as the module documentation describes.
fn transposed_chars(document: &Document, cursor: usize) -> LineChange {
    let start = line_start(document, cursor);
    if cursor == start {
        return LineChange::moved(cursor);
    }
    let (middle, end) = if cursor == line_end(document, cursor) {
        (document.prev_grapheme_boundary(cursor), cursor)
    } else {
        (cursor, document.next_grapheme_boundary(cursor))
    };
    let first = document.prev_grapheme_boundary(middle);
    if middle == start || first < start {
        return LineChange::moved(cursor);
    }
    swapped(document, first..middle, middle..end)
}

/// The change of `M-t`, as the module documentation describes.
fn transposed_words(document: &Document, cursor: usize) -> LineChange {
    let inside =
        is_word_before(document, cursor) && class_at(document, cursor) == Some(CharClass::Word);
    let at = if inside {
        forward_word_end(document, cursor)
    } else {
        cursor
    };
    let after = forward_word_end(document, at);
    let second_end = if is_word_before(document, after) && after > at {
        after
    } else {
        forward_word_end(document, backward_word_start(document, at))
    };
    let second_start = backward_word_start(document, second_end);
    let first_start = backward_word_start(document, second_start);
    let first_end = forward_word_end(document, first_start);
    let is_pair = first_start < first_end && first_end <= second_start;
    if !is_pair || !is_word_before(document, first_end) {
        return LineChange::moved(cursor);
    }
    swapped(document, first_start..first_end, second_start..second_end)
}

/// The change swapping `first` and `second`, which come in that order,
/// keeping what is between them, the cursor after both.
fn swapped(document: &Document, first: Range<usize>, second: Range<usize>) -> LineChange {
    let between = document.slice(first.end..second.start);
    let text = format!(
        "{}{between}{}",
        document.slice(second.clone()),
        document.slice(first.clone())
    );
    LineChange {
        cursor: second.end,
        edit: Some(Edit::new(first.start..second.end, text)),
        kill: None,
    }
}

/// What `C-x C-t` does with the cursor at `cursor`: its line and the one
/// above swapped, the cursor at the start of the line after both (the
/// document's end when there is none); nothing on the first line.
fn transposed_lines(document: &Document, cursor: usize) -> LineChange {
    let line = line_of(document, cursor);
    if line == 0 {
        return LineChange::moved(cursor);
    }
    let above = document.line_text(line - 1).unwrap_or_default();
    let this = document.line_text(line).unwrap_or_default();
    let start = start_of_line(document, line - 1);
    let end = line_end(document, cursor);
    let after = match line + 1 < document.len_lines() {
        true => end + 1,
        false => end,
    };
    LineChange {
        cursor: after,
        edit: Some(Edit::new(start..end, format!("{this}\n{above}"))),
        kill: None,
    }
}

/// What `M-SPC` does with the cursor at `cursor`: the blanks (spaces and
/// tabs, never a line break) around it become one space with the cursor
/// after it, or go when they are a lone space already.
fn spaced(document: &Document, cursor: usize) -> LineChange {
    let is_blank = |at: usize| matches!(document.char_at(at), Some(' ' | '\t'));
    let mut start = cursor;
    while start > line_start(document, cursor) && is_blank(start - 1) {
        start -= 1;
    }
    let mut end = cursor;
    while end < line_end(document, cursor) && is_blank(end) {
        end += 1;
    }
    let lone_space = end - start == 1 && document.char_at(start) == Some(' ');
    let text = if lone_space { "" } else { " " };
    LineChange {
        cursor: start + text.len(),
        edit: Some(Edit::new(start..end, text)),
        kill: None,
    }
}

/// Plans `command` at the primary cursor of the view and applies it, once
/// per count: the edit as one undo step (or part of the insert session),
/// the cursor moved, the kill recorded in the kill ring. With a count,
/// `C-k` kills that many whole lines instead, as Emacs's kill-line does
/// with an argument. In a read-only document, an edit is refused before
/// anything is killed or moved.
fn run_in_view(cx: &mut CommandContext<'_>, command: LineCommand) {
    if command == LineCommand::KillLine
        && let Some(lines) = cx.count
    {
        let document = cx.editor.document_of(cx.view);
        let cursor = cx.editor.view_ref(cx.view).cursor();
        let target = line_of(document, cursor).saturating_add(lines);
        let end = match target < document.len_lines() {
            true => start_of_line(document, target),
            false => document.len_chars(),
        };
        let change = killed(document, cursor..end, KillDirection::Forward);
        return apply_line_change(cx, change);
    }
    for _ in 0..cx.count_or_one() {
        let document = cx.editor.document_of(cx.view);
        let cursor = cx.editor.view_ref(cx.view).cursor();
        let change = plan(document, cursor, command);
        apply_line_change(cx, change);
    }
}

/// Applies `change` in the view, as [`run_in_view`] describes.
fn apply_line_change(cx: &mut CommandContext<'_>, change: LineChange) {
    if change.edit.is_some() && cx.editor.refuse_if_read_only(cx.view) {
        return;
    }
    if let Some(kill) = change.kill {
        let seq = cx.editor.command_seq();
        cx.editor.kill_ring_mut().kill(kill, seq);
    }
    if let Some(edit) = change.edit {
        cx.editor.apply_edits(cx.view, vec![edit]);
    }
    cx.editor.set_cursor(cx.view, change.cursor);
}

// ========================================================================
// Tests
// ========================================================================

#[cfg(test)]
mod tests {
    use crate::testing::assert_keys;

    #[test]
    fn test_m_f_and_m_b_move_by_words_over_punctuation_and_lines() {
        assert_keys("|foo.bar baz", "i M-f", "foo|.bar baz");
        assert_keys("foo|.bar baz", "i M-f M-f", "foo.bar baz|");
        assert_keys("foo bar|\n  baz", "i M-f", "foo bar\n  baz|");
        assert_keys("foo bar\n  |baz", "i M-b", "foo |bar\n  baz");
        assert_keys("snake_case |x", "i M-b", "|snake_case x");
        assert_keys("ca\u{301}fe|", "i M-b", "|ca\u{301}fe");
    }

    #[test]
    fn test_word_and_line_kills_take_the_text_emacs_and_the_shell_do() {
        assert_keys("foo |bar.baz", "i M-d", "foo |.baz");
        assert_keys("foo.bar| baz", "i M-<bs>", "foo.| baz");
        assert_keys("cat path/to/file|", "i C-w", "cat |");
        assert_keys("cat path/to  |", "i C-w", "cat |");
        assert_keys("one\nab|cd", "i C-u", "one\n|cd");
        assert_keys("one\nab|cd", "i C-u C-e C-y", "one\ncdab|");
    }

    #[test]
    fn test_c_t_swaps_characters_and_at_the_line_end_the_two_before() {
        assert_keys("ab|cd", "i C-t", "acb|d");
        assert_keys("abc|\nx", "i C-t", "acb|\nx");
        assert_keys("ab\n|cd", "i C-t", "ab\n|cd");
        assert_keys("a\n|", "i C-t", "a\n|");
        assert_keys("x|日本", "i C-t", "日x|本");
    }

    #[test]
    fn test_m_t_swaps_the_words_around_the_cursor() {
        assert_keys("foo |bar baz", "i M-t", "bar foo| baz");
        assert_keys("fo|o bar", "i M-t", "bar foo|");
        assert_keys("foo, bar|", "i M-t", "bar, foo|");
        assert_keys("|foo", "i M-t", "|foo");
    }

    #[test]
    fn test_m_m_and_m_angles_move_to_the_indent_and_the_document_edges() {
        assert_keys("one\n  two| three", "i M-m", "one\n  |two three");
        assert_keys("one\n   |", "i C-a M-m", "one\n   |");
        assert_keys("one\nt|wo", "i M-<", "|one\ntwo");
        assert_keys("o|ne\ntwo", "i M->", "one\ntwo|");
    }

    #[test]
    fn test_c_x_c_t_swaps_the_line_with_the_one_above_and_drags_it_down() {
        assert_keys("one\ntw|o\nthree", "i C-x C-t", "two\none\n|three");
        assert_keys("a\n|b\nc\nd", "i C-x C-t C-x C-t", "b\nc\na\n|d");
        assert_keys("one\ntw|o", "i C-x C-t", "two\none|");
        assert_keys("o|ne\ntwo", "i C-x C-t", "o|ne\ntwo");
    }

    #[test]
    fn test_m_spc_leaves_one_space_then_none() {
        assert_keys("a  \t|  b", "i M-SPC", "a |b");
        assert_keys("a  \t|  b", "i M-SPC M-SPC", "a|b");
        assert_keys("ab|c", "i M-SPC", "ab |c");
        assert_keys("a |\n b", "i M-SPC", "a|\n b");
    }

    #[test]
    fn test_case_commands_change_to_the_end_of_the_word_and_move_there() {
        assert_keys("|hello World", "i M-u", "HELLO| World");
        assert_keys("HELLO| WORLD", "i M-l", "HELLO world|");
        assert_keys("|hELLO wORLD", "i M-c M-c", "Hello World|");
        assert_keys("|straße", "i M-u", "STRASSE|");
        assert_keys("|hello world", "i M-c <esc> u", "|hello world");
    }
}
