//! Normal-mode edits that take no motion: `r` replaces characters, `J` and
//! `gJ` join lines, `C-S-k` deletes lines, and `C-a` and `C-x` add to and
//! subtract from a number.
//!
//! - `r{char}` replaces the character under the cursor, and with a count
//!   that many, with `{char}`, the cursor on the last; a line too short
//!   for the count changes nothing. `r<cr>` replaces them with one line
//!   break.
//! - `J` joins the cursor's line and the next (a count of lines, at least
//!   two), dropping the next line's indent and putting one space between
//!   unless the line ends in a blank, the next starts with `)`, or either
//!   is empty; `gJ` joins them as they are. In a visual mode both join the
//!   selected lines. The cursor goes to where the last two lines met; on
//!   the last line nothing is joined.
//! - `C-S-k` deletes the cursor's line, or every line the selection
//!   touches, whole, in any mode but a prompt, as a desktop editor's Delete
//!   Line does: nothing goes to a register, and the cursor goes to the first
//!   non-blank of the line that takes their place. A selection ending at
//!   the start of a line, as Shift with Down leaves one, leaves that line.
//! - `C-a` adds the count (one by default) to the number under or after
//!   the cursor on its line, `C-x` subtracts it. A number is decimal, with
//!   a `-` before it read as its sign unless a word character comes just
//!   before the `-` (so `x-1` holds the number 1), or hexadecimal after
//!   `0x` or `0X`, whose digits keep their case and their count (leading
//!   zeros stay) and wrap around at 64 bits. A decimal with leading zeros
//!   keeps its width. The cursor goes to the number's last digit.

use super::command::{CommandContext, await_char};
use super::document::{
    Document, first_non_blank_in, is_word_char, last_line, line_end, line_of, line_start,
    start_of_line,
};
use super::motion::{fail, last_cursor_offset};
use super::transaction::Edit;
use super::view::{Mode, is_visual};
use super::visual::selection;

// ========================================================================
// Constants
// ========================================================================

/// The base of hexadecimal numbers.
const HEX_RADIX: u32 = 16;

/// The chars after a `0` that start a hexadecimal number.
const HEX_MARKERS: [char; 2] = ['x', 'X'];

/// The line break `r<cr>` puts in.
const LINE_BREAK: char = '\n';

/// The sign of a negative decimal number.
const MINUS: char = '-';

/// The char that keeps `J` from putting a space before it.
const NO_SPACE_BEFORE: char = ')';

// ========================================================================
// Data Structures
// ========================================================================

/// A number found on a line, by char columns.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct NumberSpan {
    /// The column after its last digit.
    end: usize,
    /// Whether it is hexadecimal, `0x` included in the span.
    hex: bool,
    /// The column of its first char, the sign or the `0` of `0x`.
    start: usize,
}

// ========================================================================
// Functions
// ========================================================================

/// `C-x`: subtracts the count from the number under or after the cursor.
pub(crate) fn decrement(cx: &mut CommandContext<'_>) {
    let amount = i64::try_from(cx.count_or_one()).unwrap_or(i64::MAX);
    add_to_number(cx, -amount);
}

/// `C-a`: adds the count to the number under or after the cursor.
pub(crate) fn increment(cx: &mut CommandContext<'_>) {
    let amount = i64::try_from(cx.count_or_one()).unwrap_or(i64::MAX);
    add_to_number(cx, amount);
}

/// `C-S-k`: deletes the cursor's line or the selected lines, as the module
/// docs say.
pub(crate) fn delete_lines(cx: &mut CommandContext<'_>) {
    let (first, last) = touched_lines(cx);
    let document = cx.editor.document_of(cx.view);
    let start = start_of_line(document, first);
    let span = match last < last_line(document) {
        true => start..start_of_line(document, last + 1),
        // The last line has no break after it: the one before goes instead,
        // so no empty line is left behind.
        false => start.saturating_sub(usize::from(first > 0))..document.len_chars(),
    };
    if is_visual(cx.editor.view_ref(cx.view).mode()) {
        cx.editor.set_mode(cx.view, Mode::Normal);
    }
    cx.editor.apply_edits(cx.view, vec![Edit::new(span, "")]);
    let document = cx.editor.document_of(cx.view);
    let line = first.min(last_line(document));
    let target = first_non_blank_in(document, start_of_line(document, line));
    cx.editor.set_cursor(cx.view, target);
}

/// `J`: joins lines, fixing the blanks between them.
pub(crate) fn join_lines(cx: &mut CommandContext<'_>) {
    join(cx, true);
}

/// `gJ`: joins lines as they are.
pub(crate) fn join_lines_raw(cx: &mut CommandContext<'_>) {
    join(cx, false);
}

/// `r`: waits for the character to replace the ones under the cursor with.
pub(crate) fn replace_char(cx: &mut CommandContext<'_>) {
    await_char(cx, replace_with);
}

/// Adds `amount` to the number under or after the cursor, as the module
/// docs say; does nothing when the line holds none after the cursor.
fn add_to_number(cx: &mut CommandContext<'_>, amount: i64) {
    let document = cx.editor.document_of(cx.view);
    let cursor = cx.editor.view_ref(cx.view).cursor();
    let start = line_start(document, cursor);
    let chars: Vec<char> = document
        .slice(start..line_end(document, cursor))
        .chars()
        .collect();
    let column = cursor - start;
    let Some(span) = number_spans(&chars).find(|span| span.end > column) else {
        return;
    };
    let text: String = chars[span.start..span.end].iter().collect();
    let Some(new_text) = added(&text, span.hex, amount) else {
        return;
    };
    let len = new_text.chars().count();
    let range = start + span.start..start + span.end;
    cx.editor
        .apply_edits(cx.view, vec![Edit::new(range, new_text)]);
    cx.editor.set_cursor(cx.view, start + span.start + len - 1);
}

/// `text`, a number, with `amount` added, written as the module docs say;
/// `None` when the result does not fit.
fn added(text: &str, hex: bool, amount: i64) -> Option<String> {
    if hex {
        let (marker, digits) = text.split_at(2);
        let value = u64::from_str_radix(digits, HEX_RADIX).ok()?;
        let width = digits.len();
        let sum = value.wrapping_add_signed(amount);
        let written = if digits.chars().any(|c| c.is_ascii_uppercase()) {
            format!("{sum:0width$X}")
        } else {
            format!("{sum:0width$x}")
        };
        return Some(format!("{marker}{written}"));
    }
    let value: i64 = text.parse().ok()?;
    let sum = value.checked_add(amount)?;
    let digits = text.trim_start_matches(MINUS);
    let width = if digits.len() > 1 && digits.starts_with('0') {
        digits.len()
    } else {
        0
    };
    let sign = if sum < 0 { "-" } else { "" };
    Some(format!("{sign}{:0width$}", sum.unsigned_abs()))
}

/// Joins lines, the selected ones in a visual mode, else the count of them
/// from the cursor's (at least two); with `fix_blanks`, as `J` does.
fn join(cx: &mut CommandContext<'_>, fix_blanks: bool) {
    let document = cx.editor.document_of(cx.view);
    let cursor = cx.editor.view_ref(cx.view).cursor();
    let last_line = last_line(document);
    let (first, last) = match selection(cx.editor, cx.view) {
        Some((anchor, _)) => {
            let first = line_of(document, anchor.min(cursor));
            let last = line_of(document, anchor.max(cursor));
            (first, last.max(first + 1).min(last_line))
        }
        None => {
            let first = line_of(document, cursor);
            let count = cx.count_or_one().max(2);
            (first, first.saturating_add(count - 1).min(last_line))
        }
    };
    let edits: Vec<Edit> = (first..last)
        .map(|line| join_edit(document, line, fix_blanks))
        .collect();
    if is_visual(cx.editor.view_ref(cx.view).mode()) {
        cx.editor.set_mode(cx.view, Mode::Normal);
    }
    if edits.is_empty() {
        return;
    }
    let growth: isize = edits
        .iter()
        .take(edits.len() - 1)
        .map(|edit| edit.text.chars().count() as isize - edit.range.len() as isize)
        .sum();
    let meeting = edits.last().map_or(cursor, |edit| {
        edit.range.start.saturating_add_signed(growth)
    });
    cx.editor.apply_edits(cx.view, edits);
    let target = meeting.min(last_cursor_offset(cx.editor, cx.view, meeting));
    cx.editor.set_cursor(cx.view, target);
}

/// The first and last lines a line command acts on: those a visual
/// selection covers, else those the selection touches, one ending at the
/// start of a line leaving that line out, else the cursor's line.
fn touched_lines(cx: &CommandContext<'_>) -> (usize, usize) {
    let document = cx.editor.document_of(cx.view);
    let state = cx.editor.view_ref(cx.view);
    let cursor = state.cursor();
    if let Some((anchor, _)) = selection(cx.editor, cx.view) {
        return (
            line_of(document, anchor.min(cursor)),
            line_of(document, anchor.max(cursor)),
        );
    }
    let range = state.selection().primary();
    let first = line_of(document, range.start());
    let last = line_of(document, range.end());
    let ends_at_line_start =
        !range.is_empty() && last > first && range.end() == start_of_line(document, last);
    (first, last - usize::from(ends_at_line_start))
}

/// The edit joining line `line` of `document` to the next.
pub(crate) fn join_edit(document: &Document, line: usize, fix_blanks: bool) -> Edit {
    let end = line_end(document, start_of_line(document, line));
    let next = start_of_line(document, line + 1);
    if !fix_blanks {
        return Edit::new(end..next, "");
    }
    let next_end = line_end(document, next);
    let mut text_start = next;
    while text_start < next_end && matches!(document.char_at(text_start), Some(' ' | '\t')) {
        text_start += 1;
    }
    let line_start = start_of_line(document, line);
    let ends_blank = end == line_start || matches!(document.char_at(end - 1), Some(' ' | '\t'));
    let next_empty = text_start == next_end;
    let next_closes = document.char_at(text_start) == Some(NO_SPACE_BEFORE);
    let space = if ends_blank || next_empty || next_closes {
        ""
    } else {
        " "
    };
    Edit::new(end..text_start, space)
}

/// The numbers on a line of `chars`, left to right.
fn number_spans(chars: &[char]) -> impl Iterator<Item = NumberSpan> + '_ {
    let mut at = 0;
    std::iter::from_fn(move || {
        while at < chars.len() {
            let c = chars[at];
            let is_hex = c == '0'
                && chars.get(at + 1).is_some_and(|c| HEX_MARKERS.contains(c))
                && chars.get(at + 2).is_some_and(char::is_ascii_hexdigit);
            if is_hex {
                let start = at;
                at += 2;
                while chars.get(at).is_some_and(char::is_ascii_hexdigit) {
                    at += 1;
                }
                return Some(NumberSpan {
                    end: at,
                    hex: true,
                    start,
                });
            }
            if c.is_ascii_digit() {
                let digits_start = at;
                while chars.get(at).is_some_and(char::is_ascii_digit) {
                    at += 1;
                }
                let signed = digits_start > 0
                    && chars[digits_start - 1] == MINUS
                    && !(digits_start > 1 && is_word_char(chars[digits_start - 2]));
                return Some(NumberSpan {
                    end: at,
                    hex: false,
                    start: digits_start - usize::from(signed),
                });
            }
            at += 1;
        }
        None
    })
}

/// Replaces the count of clusters from the cursor with `c` (one line break
/// for `\n`); a line too short changes nothing.
fn replace_with(cx: &mut CommandContext<'_>, c: char) {
    let document = cx.editor.document_of(cx.view);
    let cursor = cx.editor.view_ref(cx.view).cursor();
    let end = line_end(document, cursor);
    let count = cx.count_or_one();
    let mut at = cursor;
    for _ in 0..count {
        if at >= end {
            return fail(cx);
        }
        at = document.next_grapheme_boundary(at);
    }
    if c == LINE_BREAK {
        cx.editor
            .apply_edits(cx.view, vec![Edit::new(cursor..at, LINE_BREAK.to_string())]);
        return cx.editor.set_cursor(cx.view, cursor + 1);
    }
    let text: String = std::iter::repeat_n(c, count).collect();
    cx.editor
        .apply_edits(cx.view, vec![Edit::new(cursor..at, text)]);
    cx.editor.set_cursor(cx.view, cursor + count - 1);
}

// ========================================================================
// Tests
// ========================================================================

#[cfg(test)]
mod tests {
    use crate::testing::assert_keys;

    #[test]
    fn test_r_replaces_count_clusters_and_a_short_line_changes_nothing() {
        assert_keys("a|bcd", "r x", "a|xcd");
        assert_keys("a|bcd", "3r x", "axx|x");
        assert_keys("a|bcd", "4r x", "a|bcd");
        assert_keys("|e\u{301}b", "r x", "|xb");
        assert_keys("ab|cd", "r <cr>", "ab\n|d");
        assert_keys("a|bcd", "2r x u", "a|bcd");
        assert_keys("|abcd", "r x l .", "x|xcd");
        assert_keys("a|bc", "r <esc>", "a|bc");
    }

    #[test]
    fn test_j_joins_lines_with_one_space_and_gj_as_they_are() {
        assert_keys("|a\n  b\nc", "J", "a| b\nc");
        assert_keys("|a\n  b\nc", "3J", "a b| c");
        assert_keys("|a \n b", "J", "a |b");
        assert_keys("|f(\n)", "J", "f(|)");
        assert_keys("|a\n\nb", "J", "|a\nb");
        assert_keys("|a\n  b", "g J", "a|  b");
        assert_keys("a\n|b", "J", "a\n|b");
        assert_keys("|a\nb\nc\nd", "v j j J", "a b| c\nd");
        assert_keys("|a\nb", "v J", "a| b");
    }

    #[test]
    fn test_ctrl_shift_k_deletes_whole_lines_in_every_mode() {
        assert_keys("a\n  b|c\nd", "C-K", "a\n|d");
        assert_keys("a\n|b", "C-K", "|a");
        assert_keys("|a", "C-K", "|");
        assert_keys("a\n|b\nc\nd", "V j C-K", "a\n|d");
        assert_keys("a\nb|x\nc\nd", "v j C-K", "a\n|d");
        assert_keys("a\nb\n|c", "i C-K", "a\n|b");
        assert_keys("a\n|b\nc", "C-K u", "a\n|b\nc");
    }

    #[test]
    fn test_ctrl_a_and_ctrl_x_add_to_the_number_at_or_after_the_cursor() {
        assert_keys("|x 9 y", "C-a", "x 1|0 y");
        assert_keys("x 1|23", "5 C-x", "x 11|8");
        assert_keys("|x -1", "C-a", "x |0");
        assert_keys("|x 0", "C-x", "x -|1");
        assert_keys("|x-1", "C-a", "x-|2");
        assert_keys("|0x0f", "C-a", "0x1|0");
        assert_keys("|0X0F", "C-a", "0X1|0");
        assert_keys("|0x00ff", "C-a", "0x010|0");
        assert_keys("|0x0", "C-x", "0xfffffffffffffff|f");
        assert_keys("|007", "C-a", "00|8");
        assert_keys("|none", "C-a", "|none");
        assert_keys("a1 b|2", "C-a", "a1 b|3");
    }
}
