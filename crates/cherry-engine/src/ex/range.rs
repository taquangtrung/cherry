//! Ex ranges: the addresses typed before an ex command name, read into
//! line numbers.
//!
//! An address is a base, offsets, or both: `.` (the cursor's line), `$`
//! (the last line), a line number, `'x` (the line of mark `x`: `a` to `z`,
//! `<` and `>` for the first and last line of the last visual selection,
//! `'` or `` ` `` for the place before the latest jump), `/pattern/` (the
//! next line matching it, after the cursor's, wrapping around) or
//! `?pattern?` (the previous one); then any number of `+n` and `-n`
//! (`n` is 1 when left out), counted from the cursor's line when no base
//! is typed (`+2`). A range is `%` (every line) or one address or two
//! joined by `,` or `;`; after `;` the second address counts from the
//! first rather than from the cursor. A pattern may be empty for the last
//! search's, and becomes the last search. Line numbers are counted from 1
//! here, with 0 for "before the first line"; a number past the end means
//! the last line. A document ending in a line break has no line after
//! it, as in Vim.

use crate::document::{Document, last_line, line_of};
use crate::editor::{Editor, ViewId};
use crate::search::{CaseRule, Direction, LastSearch, NO_PREVIOUS_PATTERN, compile};

// ========================================================================
// Constants
// ========================================================================

/// The address of the cursor's line.
const CURRENT_LINE: char = '.';

/// The address of the last line.
const LAST_LINE: char = '$';

/// The range of every line.
const WHOLE_FILE: char = '%';

/// What joins the two addresses of a range.
const RANGE_SEPARATOR: char = ',';

/// What joins the two addresses of a range, the second counted from the
/// first.
const RANGE_SEPARATOR_FROM_FIRST: char = ';';

/// What starts a mark address.
const MARK: char = '\'';

/// The mark of the first line of the last visual selection.
const VISUAL_START_MARK: char = '<';

/// The mark of the last line of the last visual selection.
const VISUAL_END_MARK: char = '>';

/// The marks of the place before the latest jump.
const PREVIOUS_CONTEXT_MARKS: [char; 2] = ['\'', '`'];

/// What starts and ends a pattern searched forward.
const FORWARD_PATTERN: char = '/';

/// What starts and ends a pattern searched backward.
const BACKWARD_PATTERN: char = '?';

/// What puts the next char of a pattern in literally.
const ESCAPE: char = '\\';

/// The sign of an offset down.
const PLUS: char = '+';

/// The sign of an offset up.
const MINUS: char = '-';

/// The base line numbers are written in.
const RADIX: u32 = 10;

// ========================================================================
// Data Structures
// ========================================================================

/// What addresses are read against, and the search a pattern address
/// made.
#[derive(Debug)]
pub(crate) struct AddressContext<'a> {
    /// The line addresses count from, from 0: the cursor's, or after `;`
    /// the first address's.
    pub cursor_line: usize,
    /// The editor, for marks and the last search.
    pub editor: &'a Editor,
    /// The search the last pattern address made, to become the last
    /// search.
    pub searched: Option<LastSearch>,
    /// The view the range is typed in.
    pub view: ViewId,
}

/// Why a range could not be read.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum AddressError {
    /// The range is malformed (`1,`).
    InvalidRange,
    /// The range names something that is not there, as this says.
    Message(String),
}

// ========================================================================
// AddressContext Implementations
// ========================================================================

impl<'a> AddressContext<'a> {
    /// The context of a range typed in `view`, counting from its cursor.
    pub(super) fn new(editor: &'a Editor, view: ViewId) -> Self {
        let cursor_line = line_of(editor.document_of(view), editor.view_ref(view).cursor());
        Self {
            cursor_line,
            editor,
            searched: None,
            view,
        }
    }

    /// The document the range is read in.
    fn document(&self) -> &'a Document {
        self.editor.document_of(self.view)
    }

    /// The line, from 1, of the mark `name`.
    fn mark_line(&self, name: char) -> Result<usize, AddressError> {
        let document = self.document();
        let view = self.editor.view_ref(self.view);
        let place = match name {
            'a'..='z' => self.editor.marks(self.view).get(name),
            VISUAL_START_MARK | VISUAL_END_MARK => view.last_visual().map(|area| {
                let (start, end) = (area.anchor.min(area.cursor), area.anchor.max(area.cursor));
                if name == VISUAL_START_MARK {
                    start
                } else {
                    end
                }
            }),
            _ if PREVIOUS_CONTEXT_MARKS.contains(&name) => view.jumps().previous(),
            _ => return Err(AddressError::Message(format!("Invalid mark: {name}"))),
        };
        let place = place.ok_or_else(|| AddressError::Message("Mark not set".to_string()))?;
        let line = line_of(document, place.min(document.len_chars()));
        Ok(line.min(last_line(document)) + 1)
    }

    /// The line, from 1, of the next line matching `pattern` after the
    /// cursor's (`Forward`, wrapping around) or the previous one; an empty
    /// pattern is the last search's.
    fn pattern_line(&mut self, pattern: &str, direction: Direction) -> Result<usize, AddressError> {
        let pattern = if pattern.is_empty() {
            let last = self
                .editor
                .last_search()
                .map(|last| last.pattern().to_string());
            last.ok_or_else(|| AddressError::Message(NO_PREVIOUS_PATTERN.to_string()))?
        } else {
            pattern.to_string()
        };
        let regex = compile(&pattern, CaseRule::Smart).map_err(AddressError::Message)?;
        let document = self.document();
        let last = last_line(document);
        let cursor = self.cursor_line.min(last);
        let order: Vec<usize> = match direction {
            Direction::Forward => (cursor + 1..=last).chain(0..=cursor).collect(),
            Direction::Backward => (0..cursor).rev().chain((cursor..=last).rev()).collect(),
        };
        let found = order.into_iter().find(|line| {
            let text = document.line_text(*line).unwrap_or_default();
            regex.is_match(&text)
        });
        let Some(line) = found else {
            return Err(AddressError::Message(format!(
                "Pattern not found: {pattern}"
            )));
        };
        self.searched = Some(LastSearch::new(direction, pattern, regex));
        Ok(line + 1)
    }
}

// ========================================================================
// Functions
// ========================================================================

/// The line, from 1 (0 before the first line), the address at the start of
/// `rest` names, consuming it; `None` when `rest` starts with no address.
pub(crate) fn parse_address(
    rest: &mut &str,
    cx: &mut AddressContext<'_>,
) -> Result<Option<usize>, AddressError> {
    let line_count = last_line(cx.document()) + 1;
    let mut line = match rest.chars().next() {
        Some(CURRENT_LINE) => {
            *rest = &rest[1..];
            Some(cx.cursor_line.min(line_count - 1) + 1)
        }
        Some(LAST_LINE) => {
            *rest = &rest[1..];
            Some(line_count)
        }
        Some(c) if c.is_digit(RADIX) => Some(take_number(rest).min(line_count)),
        Some(MARK) => {
            let mut chars = rest[1..].chars();
            let name = chars.next().ok_or(AddressError::InvalidRange)?;
            *rest = chars.as_str();
            Some(cx.mark_line(name)?)
        }
        Some(delimiter @ (FORWARD_PATTERN | BACKWARD_PATTERN)) => {
            let pattern = take_pattern(rest, delimiter);
            let direction = if delimiter == FORWARD_PATTERN {
                Direction::Forward
            } else {
                Direction::Backward
            };
            Some(cx.pattern_line(&pattern, direction)?)
        }
        Some(_) | None => None,
    };
    while let Some(sign @ (PLUS | MINUS)) = rest.chars().next() {
        *rest = &rest[1..];
        let amount = match rest.chars().next() {
            Some(c) if c.is_digit(RADIX) => take_number(rest),
            Some(_) | None => 1,
        };
        let from = line.unwrap_or(cx.cursor_line.min(line_count - 1) + 1);
        line = Some(if sign == PLUS {
            from.saturating_add(amount).min(line_count)
        } else {
            from.saturating_sub(amount)
        });
    }
    Ok(line)
}

/// The first and last lines, from 1 (0 before the first line), of the
/// range at the start of `rest`, consuming it, a backward range turned
/// around; `None` when `rest` starts with no range.
pub(crate) fn parse_range(
    rest: &mut &str,
    cx: &mut AddressContext<'_>,
) -> Result<Option<(usize, usize)>, AddressError> {
    if let Some(after) = rest.strip_prefix(WHOLE_FILE) {
        *rest = after;
        return Ok(Some((1, last_line(cx.document()) + 1)));
    }
    let Some(first) = parse_address(rest, cx)? else {
        return Ok(None);
    };
    let last = match rest.chars().next() {
        Some(separator @ (RANGE_SEPARATOR | RANGE_SEPARATOR_FROM_FIRST)) => {
            *rest = &rest[1..];
            if separator == RANGE_SEPARATOR_FROM_FIRST {
                cx.cursor_line = first.saturating_sub(1);
            }
            parse_address(rest, cx)?.ok_or(AddressError::InvalidRange)?
        }
        Some(_) | None => first,
    };
    Ok(Some((first.min(last), first.max(last))))
}

/// The pattern at the start of `rest` up to the next `delimiter` not
/// escaped (or the end), consuming both; an escaped delimiter is put in
/// as itself, other escapes are kept for the regular expression.
pub(crate) fn take_pattern(rest: &mut &str, delimiter: char) -> String {
    let mut pattern = String::new();
    let mut chars = rest[delimiter.len_utf8()..].chars();
    while let Some(c) = chars.next() {
        match c {
            ESCAPE => match chars.next() {
                Some(next) if next == delimiter => pattern.push(next),
                Some(next) => {
                    pattern.push(ESCAPE);
                    pattern.push(next);
                }
                None => pattern.push(ESCAPE),
            },
            c if c == delimiter => break,
            c => pattern.push(c),
        }
    }
    *rest = chars.as_str();
    pattern
}

/// The number written by the digits at the start of `rest`, consuming
/// them; one too large to hold is the largest number.
fn take_number(rest: &mut &str) -> usize {
    let digits = rest
        .find(|c: char| !c.is_digit(RADIX))
        .unwrap_or(rest.len());
    let number = rest[..digits].parse().unwrap_or(usize::MAX);
    *rest = &rest[digits..];
    number
}

// ========================================================================
// Tests
// ========================================================================

#[cfg(test)]
mod tests {
    use crate::request::{HostRequest, NoticeRequest};
    use crate::testing::{Harness, assert_keys};
    use crate::view::Mode;

    /// The notices among `requests`, oldest first.
    fn notices(requests: Vec<HostRequest>) -> Vec<String> {
        requests
            .into_iter()
            .filter_map(|request| match request {
                HostRequest::Notice(NoticeRequest { message, .. }) => Some(message),
                HostRequest::Cancel(_)
                | HostRequest::Command(_)
                | HostRequest::Open(_)
                | HostRequest::Quit(_)
                | HostRequest::ReadInto(_)
                | HostRequest::Save(_)
                | HostRequest::Submit(_) => None,
            })
            .collect()
    }

    #[test]
    fn test_offsets_count_from_the_base_or_the_cursor() {
        let text = "a\n|b\nc\nd\ne";
        assert_keys(text, ": .+2 <cr>", "a\nb\nc\n|d\ne");
        assert_keys(text, ": +2 <cr>", "a\nb\nc\n|d\ne");
        assert_keys(text, ": ++ <cr>", "a\nb\nc\n|d\ne");
        assert_keys(text, ": $-1 <cr>", "a\nb\nc\n|d\ne");
        assert_keys(text, ": - <cr>", "|a\nb\nc\nd\ne");
        assert_keys(text, ": 1+99 <cr>", "a\nb\nc\nd\n|e");
        assert_keys("|a\nb\nc\n", ": $ <cr>", "a\nb\n|c\n");
    }

    #[test]
    fn test_semicolon_counts_the_second_address_from_the_first() {
        assert_keys("|a\nb\nc\nd\ne", ": 2;+2d <cr>", "a\n|e");
        assert_keys("|a\nb\nc\nd\ne", ": 2,+2d <cr>", "a\n|d\ne");
    }

    #[test]
    fn test_mark_addresses_name_mark_lines_and_the_last_selection() {
        assert_keys("a\n|b\nc\nd", "m a G : 'a <cr>", "a\n|b\nc\nd");
        assert_keys("a\n|b\nc\nd", "V j <esc> gg : '<,'>d <cr>", "a\n|d");
        assert_keys("|a\nb\nc\nd", "G : '' <cr>", "|a\nb\nc\nd");
        let mut harness = Harness::new("|a");
        harness.keys(": 'b <cr> : 'Q <cr>");
        assert_eq!(
            notices(harness.requests()),
            ["Mark not set", "Invalid mark: Q"]
        );
    }

    #[test]
    fn test_colon_from_visual_or_with_a_count_types_a_range() {
        let mut harness = Harness::new("a\n|b\nc\nd");
        harness.keys("V j :");
        let prompt = harness.editor.view(harness.view).unwrap().prompt().unwrap();
        assert_eq!(prompt.text(), "'<,'>");
        harness.keys("d <cr>");
        assert_eq!(harness.marked(), "a\n|d");
        assert_eq!(harness.mode(), Mode::Normal);
        harness.keys("gg 3 :");
        let prompt = harness.editor.view(harness.view).unwrap().prompt().unwrap();
        assert_eq!(prompt.text(), ".,.+2");
        harness.keys("<esc> 1 :");
        let prompt = harness.editor.view(harness.view).unwrap().prompt().unwrap();
        assert_eq!(prompt.text(), ".");
    }

    #[test]
    fn test_pattern_addresses_search_from_the_cursor_and_wrap() {
        let text = "x1\n|a\nx2\nb";
        assert_keys(text, ": /x/ <cr>", "x1\na\n|x2\nb");
        assert_keys(text, ": ?x? <cr>", "|x1\na\nx2\nb");
        assert_keys("x1\na\n|x2\nb", ": /x <cr>", "|x1\na\nx2\nb");
        assert_keys(text, ": /a/;/b/d <cr>", "|x1");
        let mut harness = Harness::new("|ab\nb\nab");
        harness.keys(": /ab/ <cr> n");
        assert_eq!(
            harness.marked(),
            "|ab\nb\nab",
            "the pattern is searched by n"
        );
        harness.requests();
        harness.keys(": /zz/ <cr>");
        assert_eq!(notices(harness.requests()), ["Pattern not found: zz"]);
    }
}
