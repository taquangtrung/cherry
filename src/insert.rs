//! Insert mode: entering it, leaving it, and the edits made in it.
//!
//! Insert mode is entered before the cursor (`i`), after it (`a`), at the
//! first non-blank of the line (`I`), at its end (`A`), or on a new line
//! opened below (`o`) or above (`O`). An insert session, from entering
//! Insert mode to leaving it, is one undo step however much is typed and
//! wherever the cursor moves in it; opening a line belongs to the session.
//! `gi` enters Insert mode where the last session in the document ended,
//! carried along by edits since.
//!
//! Besides the Emacs editing keys, `C-o` opens a line after the cursor
//! without moving it (Emacs's open-line, chosen over Vim's `C-o`, which
//! would run one Normal command), and `C-v` or `C-q` types the next key
//! literally: a Tab or Enter as itself whatever it is bound to, `C-a` as
//! the control character, no auto-pairing. `C-r` and a register name
//! types the text of that register (`C-r a`, `C-r "`, `C-r .`), as it is.
//!
//! # Typing helpers
//!
//! With [`EditorOptions::auto_indent`](crate::EditorOptions::auto_indent),
//! a line opened by Enter, `o`, or `O` starts with the indent of the line
//! it came from; when nothing is typed on it before Insert mode is left or
//! Enter is pressed again, the indent goes, so no line is left holding
//! only blanks.
//!
//! With [`EditorOptions::continue_comments`](crate::EditorOptions::continue_comments),
//! a line opened from a line comment (the document's comment token after
//! its indent) starts with the same indent, token, and space. With
//! [`EditorOptions::continue_lists`](crate::EditorOptions::continue_lists),
//! a line opened by Enter or `o` from a list item (`- `, `* `, `+ `, `1. `,
//! `1) `) starts with the next item's marker, a number counting up; Enter
//! on an item holding only its marker ends the list instead, leaving the
//! line empty. With [`EditorOptions::auto_pairs`](crate::EditorOptions::auto_pairs),
//! an opening bracket or quote types its closer after the cursor (only
//! before a blank, a closer, or the line end, and a quote not right after
//! a letter or digit, so `don't` types one quote); typing that closer just
//! before it steps over it, and Backspace between an empty pair deletes
//! both. Both helpers act at a single cursor.

use super::block::finish_block_insert;
use super::command::{CommandContext, await_char};
use super::completion::{cancel_completion, narrow_completion};
use super::cursors::end_occurrence_change;
use super::document::{Document, first_non_blank_in, line_end, line_of, line_start};
use super::editor::{Editor, ViewId};
use super::mark::{CHANGE_END_MARK, CHANGE_START_MARK};
use super::motion::first_non_blank_of_line;
use super::register::Registers;
use super::replace::repeat_replaced_text;
use super::request::request_submit;
use super::snippet::take_placeholder;
use super::transaction::Edit;
use super::view::Mode;

// ========================================================================
// Constants
// ========================================================================

/// The text Enter types.
const NEWLINE: &str = "\n";

/// The mark at the place the last insert session ended, for `gi`.
pub(crate) const LAST_INSERT_MARK: char = '^';

/// The brackets and quotes auto-pairing types in pairs, opener and closer.
const PAIRS: &[(char, char)] = &[
    ('"', '"'),
    ('\'', '\''),
    ('(', ')'),
    ('[', ']'),
    ('`', '`'),
    ('{', '}'),
];

// ========================================================================
// Functions
// ========================================================================

/// Enters Insert mode after the cursor's character, or where it is at the
/// end of the line.
pub(crate) fn append(cx: &mut CommandContext<'_>) {
    let document = cx.editor.document_of(cx.view);
    let offset = cx.editor.view_ref(cx.view).cursor();
    let after = document
        .next_grapheme_boundary(offset)
        .min(line_end(document, offset));
    cx.editor.set_cursor(cx.view, after);
    enter_insert(cx);
}

/// Enters Insert mode at the end of the cursor's line.
pub(crate) fn append_at_line_end(cx: &mut CommandContext<'_>) {
    let document = cx.editor.document_of(cx.view);
    let end = line_end(document, cx.editor.view_ref(cx.view).cursor());
    cx.editor.set_cursor(cx.view, end);
    enter_insert(cx);
}

/// Deletes the character before each cursor, joining the line to the one
/// above at its start; a selected range is deleted instead. An open
/// completion menu widens to the text left.
pub(crate) fn delete_backward(cx: &mut CommandContext<'_>) {
    if !take_placeholder(cx.editor, cx.view) {
        delete_backward_chars(cx);
    }
    narrow_completion(cx.editor, cx.view);
}

/// Deletes the character before each cursor, or the selected ranges, as
/// [`delete_backward`] does.
fn delete_backward_chars(cx: &mut CommandContext<'_>) {
    if let Some(cursor) = single_cursor(cx.editor, cx.view) {
        let document = cx.editor.document_of(cx.view);
        let around = (
            cursor
                .checked_sub(1)
                .and_then(|before| document.char_at(before)),
            document.char_at(cursor),
        );
        let is_empty_pair =
            matches!(around, (Some(open), Some(close)) if PAIRS.contains(&(open, close)));
        if cx.editor.options().auto_pairs && is_empty_pair {
            let edits = vec![deletion(cursor - 1, cursor + 1)];
            return cx.editor.apply_edits(cx.view, edits);
        }
    }
    let document = cx.editor.document_of(cx.view);
    let edits = cx
        .editor
        .view_ref(cx.view)
        .selection()
        .ranges()
        .iter()
        .map(|range| {
            let start = if range.is_empty() {
                document.prev_grapheme_boundary(range.head)
            } else {
                range.start()
            };
            deletion(start, range.end())
        })
        .collect();
    cx.editor.apply_edits(cx.view, edits);
}

/// Deletes the character under each cursor, joining the next line at the
/// end of a line; a selected range is deleted instead. The Emacs
/// delete-char.
pub(crate) fn delete_forward(cx: &mut CommandContext<'_>) {
    let document = cx.editor.document_of(cx.view);
    let edits = cx
        .editor
        .view_ref(cx.view)
        .selection()
        .ranges()
        .iter()
        .map(|range| {
            let end = if range.is_empty() {
                document.next_grapheme_boundary(range.head)
            } else {
                range.end()
            };
            deletion(range.start(), end)
        })
        .collect();
    cx.editor.apply_edits(cx.view, edits);
}

/// Enters Insert mode with the cursor where it is, opening the undo group
/// of the insert session.
pub(crate) fn enter_insert(cx: &mut CommandContext<'_>) {
    cx.editor.view_mut(cx.view).set_auto_indented_line(None);
    cx.editor.history_mut(cx.view).begin_group();
    cx.editor.set_mode(cx.view, Mode::Insert);
    mark_insert_start(cx.editor, cx.view);
}

/// `C-r` in Insert mode: waits for a register name and types the text of
/// that register at every cursor, as it is; a key that names no register
/// gives a notice.
pub(crate) fn insert_register(cx: &mut CommandContext<'_>) {
    await_char(cx, |cx, name| {
        if !Registers::is_name(name) {
            return cx.editor.error(format!("{name:?} names no register"));
        }
        if let Some(text) = cx.editor.read_register(cx.view, Some(name)) {
            insert_text(cx.editor, cx.view, &text.text);
        }
    });
}

/// Records that the text typed in the insert session of `view` starts at
/// its cursor, for register `.`.
pub(crate) fn mark_insert_start(editor: &mut Editor, view: ViewId) {
    let cursor = editor.view_ref(view).cursor();
    editor.view_mut(view).set_insert_start(Some(cursor));
}

/// Leaves Insert mode for Normal mode, closing the insert session's undo
/// group and moving the cursor back onto the character before it, as Vim
/// does, unless it is at the start of its line. In a view that never
/// leaves Insert mode, closes an open completion menu as `C-g` does, or
/// else asks the host to cancel the field.
pub(crate) fn exit_insert(cx: &mut CommandContext<'_>) {
    if cx.editor.is_insert_locked(cx.view) {
        return cancel_completion(cx);
    }
    end_occurrence_change(cx.editor, cx.view);
    if exit_insert_session(cx.editor, cx.view) {
        return;
    }
    let document = cx.editor.document_of(cx.view);
    let offset = cx.editor.view_ref(cx.view).cursor();
    let back = document
        .prev_grapheme_boundary(offset)
        .max(line_start(document, offset));
    cx.editor.set_cursor(cx.view, back);
}

/// Ends the insert session of `view` for Normal mode, closing its undo
/// group, with the cursor left where it is (for a mouse selection), and
/// marks where it ended for `gi`. Text typed for a visual block goes on
/// its other lines first, and then the cursor is at the block's start:
/// returns whether that happened.
pub(crate) fn exit_insert_session(editor: &mut Editor, view: ViewId) -> bool {
    clear_auto_indent(editor, view);
    repeat_replaced_text(editor, view);
    let cursor = editor.view_ref(view).cursor();
    if let Some(start) = editor.view_ref(view).insert_start() {
        let start = start.min(cursor);
        let typed = editor.document_of(view).slice(start..cursor);
        editor.set_last_inserted(typed);
        if start < cursor {
            let last = editor.document_of(view).prev_grapheme_boundary(cursor);
            editor.marks_mut(view).set(CHANGE_START_MARK, start);
            editor.marks_mut(view).set(CHANGE_END_MARK, last);
        }
    }
    editor.marks_mut(view).set(LAST_INSERT_MARK, cursor);
    let repeated = finish_block_insert(editor, view);
    editor.history_mut(view).end_group();
    editor.set_mode(view, Mode::Normal);
    repeated
}

/// `gi`: enters Insert mode where the last insert session in the document
/// ended, or at the cursor when there was none.
pub(crate) fn insert_at_last_insert(cx: &mut CommandContext<'_>) {
    if let Some(place) = cx.editor.marks(cx.view).get(LAST_INSERT_MARK) {
        let len = cx.editor.document_of(cx.view).len_chars();
        cx.editor.set_cursor(cx.view, place.min(len));
    }
    enter_insert(cx);
}

/// `C-o` in Insert mode: breaks the line after the cursor, which stays.
pub(crate) fn open_line(cx: &mut CommandContext<'_>) {
    let cursor = cx.editor.view_ref(cx.view).cursor();
    cx.editor
        .apply_edits(cx.view, vec![Edit::new(cursor..cursor, NEWLINE)]);
    cx.editor.set_cursor(cx.view, cursor);
}

/// `C-v` and `C-q` in Insert mode: waits for a key and types it as it is.
pub(crate) fn quoted_insert(cx: &mut CommandContext<'_>) {
    await_char(cx, |cx, c| {
        insert_text(cx.editor, cx.view, c.encode_utf8(&mut [0; 4]));
    });
}

/// Enters Insert mode at the first non-blank character of the line.
pub(crate) fn insert_at_line_start(cx: &mut CommandContext<'_>) {
    let offset = cx.editor.view_ref(cx.view).cursor();
    let target = first_non_blank_of_line(cx.editor, cx.view, offset);
    cx.editor.set_cursor(cx.view, target);
    enter_insert(cx);
}

/// Breaks the line at each cursor, auto-indenting the new line at a
/// single cursor; in a single-line document, asks the host to submit.
pub(crate) fn insert_newline(cx: &mut CommandContext<'_>) {
    if cx.editor.shows_single_line(cx.view) {
        return request_submit(cx.editor, cx.view);
    }
    let Some(cursor) = single_cursor(cx.editor, cx.view) else {
        return insert_text(cx.editor, cx.view, NEWLINE);
    };
    if end_empty_item(cx.editor, cx.view, cursor) {
        return;
    }
    let indent = continuation(cx.editor, cx.view, cursor, true)
        .unwrap_or_else(|| auto_indent(cx.editor, cx.view, cursor));
    clear_auto_indent(cx.editor, cx.view);
    let cursor = cx.editor.view_ref(cx.view).cursor();
    let text = format!("{NEWLINE}{indent}");
    cx.editor
        .apply_edits(cx.view, vec![Edit::new(cursor..cursor, text)]);
    note_auto_indent(cx.editor, cx.view, &indent);
}

/// Types `text` at every cursor of `view`, replacing what each range
/// selects, and moves each cursor past what it typed.
pub(crate) fn insert_text(editor: &mut Editor, view: ViewId, text: &str) {
    let edits = editor
        .view_ref(view)
        .selection()
        .ranges()
        .iter()
        .map(|range| Edit {
            range: range.start()..range.end(),
            text: text.to_string(),
        })
        .collect();
    editor.apply_edits(view, edits);
}

/// Opens a new line above the cursor's line and enters Insert mode on it,
/// auto-indented as the cursor's line; in a single-line document, enters
/// Insert mode at its first non-blank as `I` does.
pub(crate) fn open_above(cx: &mut CommandContext<'_>) {
    if cx.editor.shows_single_line(cx.view) {
        return insert_at_line_start(cx);
    }
    let document = cx.editor.document_of(cx.view);
    let start = line_start(document, cx.editor.view_ref(cx.view).cursor());
    let indent = continuation(cx.editor, cx.view, start, false)
        .unwrap_or_else(|| auto_indent(cx.editor, cx.view, line_end(document, start)));
    cx.editor.set_cursor(cx.view, start);
    enter_insert(cx);
    let text = format!("{indent}{NEWLINE}");
    cx.editor
        .apply_edits(cx.view, vec![Edit::new(start..start, text)]);
    cx.editor
        .set_cursor(cx.view, start + indent.chars().count());
    mark_insert_start(cx.editor, cx.view);
    note_auto_indent(cx.editor, cx.view, &indent);
}

/// Opens a new line below the cursor's line and enters Insert mode on it,
/// auto-indented as the cursor's line; in a single-line document, enters
/// Insert mode at its end as `A` does.
pub(crate) fn open_below(cx: &mut CommandContext<'_>) {
    append_at_line_end(cx);
    if cx.editor.shows_single_line(cx.view) {
        return;
    }
    let end = cx.editor.view_ref(cx.view).cursor();
    let indent = continuation(cx.editor, cx.view, end, true)
        .unwrap_or_else(|| auto_indent(cx.editor, cx.view, end));
    let text = format!("{NEWLINE}{indent}");
    cx.editor
        .apply_edits(cx.view, vec![Edit::new(end..end, text)]);
    mark_insert_start(cx.editor, cx.view);
    note_auto_indent(cx.editor, cx.view, &indent);
}

/// Types `c` at the cursors of `view` in Insert mode, pairing brackets and
/// quotes at a single cursor as the module documentation describes.
pub(crate) fn type_char(editor: &mut Editor, view: ViewId, c: char) {
    let mut buffer = [0; 4];
    let text: &str = c.encode_utf8(&mut buffer);
    let cursor = single_cursor(editor, view).filter(|_| editor.options().auto_pairs);
    let Some(cursor) = cursor else {
        return insert_text(editor, view, text);
    };
    let document = editor.document_of(view);
    let next = document.char_at(cursor);
    let is_closer = PAIRS.iter().any(|(_, close)| *close == c);
    if is_closer && next == Some(c) {
        return editor.set_cursor(view, cursor + 1);
    }
    match PAIRS.iter().find(|(open, _)| *open == c) {
        Some((open, close)) if pairs_here(document, cursor, *open, *close) => {
            let pair = format!("{open}{close}");
            editor.apply_edits(view, vec![Edit::new(cursor..cursor, pair)]);
            editor.set_cursor(view, cursor + 1);
        }
        Some(_) | None => insert_text(editor, view, text),
    }
}

/// The indent a line opened after char `offset` starts with: the blanks
/// at the start of its line, up to `offset`; empty with auto-indent off.
fn auto_indent(editor: &Editor, view: ViewId, offset: usize) -> String {
    if !editor.options().auto_indent {
        return String::new();
    }
    let document = editor.document_of(view);
    let start = line_start(document, offset);
    let end = first_non_blank_in(document, offset).min(offset).max(start);
    document.slice(start..end)
}

/// What a line opened from the line of `offset` starts with when it
/// continues a line comment, or with `lists` a list item, as the module
/// docs say: the indent, then the token and a space or the next marker.
fn continuation(editor: &Editor, view: ViewId, offset: usize, lists: bool) -> Option<String> {
    let document = editor.document_of(view);
    let text = document.line_text(line_of(document, offset))?;
    let rest = text.trim_start_matches([' ', '\t']);
    let indent = &text[..text.len() - rest.len()];
    let options = editor.options();
    let comment = editor
        .comment_token_of(view)
        .filter(|token| options.continue_comments && rest.starts_with(*token));
    if let Some(token) = comment {
        return Some(format!("{indent}{token} "));
    }
    if !(lists && options.continue_lists) {
        return None;
    }
    let (marker, _) = list_marker(rest)?;
    Some(format!("{indent}{} ", next_marker(marker)))
}

/// With list continuation on, Enter at the end of a list item holding
/// only its marker: empties the line, ending the list; returns whether it
/// did.
fn end_empty_item(editor: &mut Editor, view: ViewId, cursor: usize) -> bool {
    if !editor.options().continue_lists {
        return false;
    }
    let document = editor.document_of(view);
    let (start, end) = (line_start(document, cursor), line_end(document, cursor));
    let text = document.slice(start..end);
    let is_empty_item = list_marker(text.trim_start_matches([' ', '\t']))
        .is_some_and(|(_, item)| item.trim().is_empty());
    if cursor != end || !is_empty_item {
        return false;
    }
    editor.apply_edits(view, vec![Edit::new(start..end, "")]);
    editor.set_cursor(view, start);
    true
}

/// The list marker `text` (a line past its indent) starts with, and the
/// item's text after the marker and its blank: `-`, `*`, `+`, or a number
/// and `.` or `)`.
fn list_marker(text: &str) -> Option<(&str, &str)> {
    let digits = text.chars().take_while(char::is_ascii_digit).count();
    let marker_len = match text[digits..].chars().next() {
        Some('-' | '*' | '+') if digits == 0 => 1,
        Some('.' | ')') if digits > 0 => digits + 1,
        Some(_) | None => return None,
    };
    let item = text[marker_len..].strip_prefix(' ')?;
    Some((&text[..marker_len], item))
}

/// The marker of the item after one marked `marker`: the same bullet, or
/// the number one more.
fn next_marker(marker: &str) -> String {
    let digits = marker.trim_end_matches(['.', ')']);
    match digits.parse::<u64>() {
        Ok(number) => format!("{}{}", number + 1, &marker[digits.len()..]),
        Err(_) => marker.to_string(),
    }
}

/// Empties the cursor's line when it holds only the indent auto-indent
/// gave it and the cursor is at its end, and forgets that indent.
fn clear_auto_indent(editor: &mut Editor, view: ViewId) {
    let Some(line) = editor.view_mut(view).auto_indented_line() else {
        return;
    };
    editor.view_mut(view).set_auto_indented_line(None);
    let document = editor.document_of(view);
    let cursor = editor.view_ref(view).cursor();
    let start = line_start(document, cursor);
    let only_blanks = first_non_blank_in(document, cursor) == line_end(document, cursor);
    if line_of(document, cursor) == line && only_blanks && cursor == line_end(document, cursor) {
        editor.apply_edits(view, vec![deletion(start, cursor)]);
    }
}

/// Remembers the cursor's line as holding only the indent `indent` it was
/// just given, when there is one.
fn note_auto_indent(editor: &mut Editor, view: ViewId, indent: &str) {
    if indent.is_empty() {
        return;
    }
    let line = line_of(editor.document_of(view), editor.view_ref(view).cursor());
    editor.view_mut(view).set_auto_indented_line(Some(line));
}

/// Whether typing `open` at `cursor` of `document` types `close` too: only
/// before a blank, a closer, or the line end, and for a quote, not right
/// after a letter, a digit, or the same quote.
fn pairs_here(document: &Document, cursor: usize, open: char, close: char) -> bool {
    let next = document.char_at(cursor);
    let next_fits = next
        .is_none_or(|next| next.is_whitespace() || PAIRS.iter().any(|(_, closer)| *closer == next));
    let previous = cursor
        .checked_sub(1)
        .and_then(|before| document.char_at(before));
    let is_quote = open == close;
    let after_word =
        previous.is_some_and(|previous| previous.is_alphanumeric() || previous == open);
    next_fits && !(is_quote && after_word)
}

/// The cursor of `view` when it has one cursor and no selected range.
fn single_cursor(editor: &Editor, view: ViewId) -> Option<usize> {
    match editor.view_ref(view).selection().ranges() {
        [range] if range.is_empty() => Some(range.head),
        _ => None,
    }
}

/// The edit deleting the chars from `start` to `end`.
fn deletion(start: usize, end: usize) -> Edit {
    Edit {
        range: start..end,
        text: String::new(),
    }
}

// ========================================================================
// Tests
// ========================================================================

#[cfg(test)]
mod tests {
    use crate::options::EditorOptions;
    use crate::testing::{Harness, assert_keys};
    use crate::view::Mode;

    /// Asserts `keys` turn `before` into `after` with auto-pairs on.
    #[track_caller]
    fn assert_paired_keys(before: &str, keys: &str, after: &str) {
        let options = EditorOptions {
            auto_pairs: true,
            ..EditorOptions::default()
        };
        let mut harness = Harness::with_options(before, options);
        harness.keys(keys);
        assert_eq!(harness.marked(), after, "{before:?} after {keys:?}");
    }

    /// The text of `marked` after `keys`, comments and lists continuing,
    /// the comment token `#`.
    fn continued(marked: &str, keys: &str) -> String {
        let options = EditorOptions {
            continue_comments: true,
            continue_lists: true,
            ..EditorOptions::default()
        };
        let mut harness = Harness::with_options(marked, options);
        let document = harness.editor.view(harness.view).unwrap().document();
        harness.editor.set_comment_token(document, Some("#"));
        harness.keys(keys);
        harness.marked()
    }

    #[test]
    fn test_comments_and_list_items_continue_and_an_empty_item_ends_the_list() {
        assert_eq!(continued("  # note|", "A <cr> x"), "  # note\n  # x|");
        assert_eq!(continued("  # n|ote", "O y"), "  # y|\n  # note");
        assert_eq!(continued("- a|", "A <cr> b"), "- a\n- b|");
        assert_eq!(continued("9. a|", "o b"), "9. a\n10. b|");
        assert_eq!(
            continued("1) a|", "A <cr> <cr>"),
            "1) a\n|",
            "an empty item ends it"
        );
        assert_eq!(continued("-a|", "A <cr> b"), "-a\nb|", "no blank, no item");
    }

    #[test]
    fn test_enter_o_and_capital_o_copy_the_indent_of_their_line() {
        assert_keys("  ab|cd", "i <cr> x", "  ab\n  x|cd");
        assert_keys("\tab|", "o x", "\tab\n\tx|");
        assert_keys("  a|b\nc", "O x", "  x|\n  ab\nc");
        assert_keys(" | ab", "i <cr>", " \n | ab");
        assert_keys("ab|", "o x", "ab\nx|");
    }

    #[test]
    fn test_an_indent_left_untouched_goes_on_escape_or_another_enter() {
        assert_keys("  ab|", "o <esc>", "  ab\n|");
        assert_keys("  ab|", "a <cr> <cr> x", "  ab\n\n  x|");
        assert_keys("  ab|", "o x <esc>", "  ab\n  |x");
        assert_keys("  ab|", "o <esc> u", "  a|b");
        let options = EditorOptions {
            auto_indent: false,
            ..EditorOptions::default()
        };
        let mut harness = Harness::with_options("  ab|", options);
        harness.keys("a <cr> x");
        assert_eq!(harness.marked(), "  ab\nx|");
    }

    #[test]
    fn test_auto_pairs_type_closers_step_over_them_and_delete_empty_pairs() {
        assert_paired_keys("|", "i ( x )", "(x)|");
        assert_paired_keys("|", "i [ { <bs> <bs>", "|");
        assert_paired_keys("|x", "i (", "(|x");
        assert_paired_keys("|", "i \" a \"", "\"a\"|");
        assert_paired_keys("don|", "a ' t", "don't|");
        assert_paired_keys("|", "i ( <esc> u", "|");
        assert_keys("|", "i ( x", "(x|");
    }

    #[test]
    fn test_each_way_into_insert_mode_puts_the_cursor_where_vim_does() {
        assert_keys("a|bc", "i X <esc>", "a|Xbc");
        assert_keys("a|bc", "a X <esc>", "ab|Xc");
        assert_keys("ab|c", "a X <esc>", "abc|X");
        assert_keys("  ab|c", "I X <esc>", "  |Xabc");
        assert_keys("a|bc\nd", "A X <esc>", "abc|X\nd");
        assert_keys("a|bc\nd", "o X <esc>", "abc\n|X\nd");
        assert_keys("abc\nd|e", "O X <esc>", "abc\n|X\nde");
        assert_keys("|", "A <esc>", "|");
    }

    #[test]
    fn test_backspace_deletes_a_whole_cluster_and_joins_lines_at_line_start() {
        assert_keys("ab|c", "i <bs>", "a|c");
        assert_keys("e\u{301}|x", "i <bs>", "|x");
        assert_keys("ab\n|cd", "i <bs>", "ab|cd");
        assert_keys("|ab", "i <bs>", "|ab");
    }

    #[test]
    fn test_enter_breaks_the_line_and_tab_types_a_tab() {
        assert_keys("ab|cd", "i <cr>", "ab\n|cd");
        assert_keys("ab|cd", "i <tab>", "ab\t|cd");
    }

    #[test]
    fn test_emacs_motions_in_insert_mode_cross_lines_and_reach_line_ends() {
        assert_keys("ab|cd\nef", "i C-a", "|abcd\nef");
        assert_keys("ab|cd\nef", "i C-e", "abcd|\nef");
        assert_keys("abc|d\nef", "i C-f C-f", "abcd\n|ef");
        assert_keys("abcd\n|ef", "i C-b", "abcd|\nef");
        assert_keys("日|本", "i C-b", "|日本");
    }

    #[test]
    fn test_c_d_deletes_forward_and_joins_the_next_line_at_line_end() {
        assert_keys("a|bc", "i C-d", "a|c");
        assert_keys("ab|\ncd", "i C-d", "ab|cd");
        assert_keys("ab|", "i C-d", "ab|");
        assert_keys("|日本", "i <del>", "|本");
    }

    #[test]
    fn test_emacs_control_aliases_delete_break_lines_and_move_lines() {
        assert_keys("ab|c", "i C-h", "a|c");
        assert_keys("ab|c", "i C-j", "ab\n|c");
        assert_keys("ab|c", "i C-m", "ab\n|c");
        assert_keys("ab|c\nxyz", "i C-n", "abc\nxy|z");
        assert_keys("abc\nxy|z", "i C-p", "ab|c\nxyz");
    }

    #[test]
    fn test_escape_stays_put_at_the_start_of_a_line() {
        let mut harness = Harness::new("ab\n|cd");
        harness.keys("i <esc>");
        assert_eq!(harness.mode(), Mode::Normal);
        assert_eq!(harness.marked(), "ab\n|cd");
    }

    #[test]
    fn test_gi_inserts_where_the_last_insert_ended_carried_by_edits() {
        assert_keys("|abc\nxyz", "A Q <esc> j 0 g i R <esc>", "abcQ|R\nxyz");
        assert_keys("abc\n|xyz", "i Q <esc> k d d g i !", "Q!|xyz");
        assert_keys("a|b", "g i X", "aX|b");
    }

    #[test]
    fn test_ctrl_o_opens_a_line_after_the_cursor_which_stays() {
        assert_keys("ab|cd", "i C-o", "ab|\ncd");
        assert_keys("ab|cd", "i C-o C-o x", "abx|\n\ncd");
    }

    #[test]
    fn test_ctrl_v_types_the_next_key_literally() {
        let options = EditorOptions {
            auto_pairs: true,
            ..EditorOptions::default()
        };
        let mut harness = Harness::with_options("|", options);
        harness.keys("i C-v ( C-q <tab> C-v C-a C-v <cr> x");
        assert_eq!(harness.marked(), "(\t\u{1}\nx|");
    }

    #[test]
    fn test_ctrl_r_types_a_register_in_insert_mode() {
        assert_keys("|word end", "y w A SPC C-r 0 <esc>", "word end word| ");
        assert_keys("|ab", "y y A C-r \" <esc>", "abab\n|");
        assert_keys("|ab", "i xy <esc> A C-r . <esc>", "xyabx|y");
        assert_keys("|ab", "A C-r q <esc>", "a|b");
    }
}
