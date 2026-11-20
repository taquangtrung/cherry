//! Motions over the text's structure: `%` to the matching bracket (or with
//! a count to a percentage of the document), `{` and `}` over paragraphs,
//! and `(` and `)` over sentences. Each is a jump, recorded in the
//! jumplist.
//!
//! - `%` looks for the first bracket of `()`, `[]`, or `{}` from the cursor
//!   to the end of its line and goes to its match, counting nesting of
//!   that bracket only; inclusive after an operator. With no bracket on
//!   the rest of the line, or no match, it fails (cancelling an operator).
//!   `N%` goes to the first non-blank of the line N percent of the way
//!   through the document, linewise; past 100 it fails.
//! - `}` goes to the next empty line after a paragraph (a run of lines
//!   holding characters, blanks counting), the document end when none
//!   follows; `{` to the empty line before. Exclusive, so `d}` from a
//!   paragraph's first char deletes its lines.
//! - `)` goes to the start of the next sentence and `(` to the start of
//!   the sentence, or of the one before when already at a start. A
//!   sentence ends at `.`, `!`, or `?`, maybe followed by closing `)`,
//!   `]`, or quotes, then blanks or a line break; an empty line is a
//!   sentence of its own, and a paragraph's first non-blank starts one.

use std::ops::Range;

use super::command::CommandContext;
use super::document::{Document, last_line, line_count, line_end, line_start, start_of_line};
use super::mark::jump_to;
use super::motion::{fail, goto_line};
use super::operator::MotionKind;
use super::text_object::{find_close, find_open};

// ========================================================================
// Constants
// ========================================================================

/// The bracket pairs `%` matches.
const BRACKET_PAIRS: &[(char, char)] = &[('(', ')'), ('[', ']'), ('{', '}')];

/// The chars that may close a sentence after its end mark.
const SENTENCE_CLOSERS: &[char] = &[')', ']', '"', '\''];

/// The chars that end a sentence.
const SENTENCE_ENDS: &[char] = &['.', '!', '?'];

/// The whole document, as a percentage.
const FULL_PERCENT: usize = 100;

// ========================================================================
// Functions
// ========================================================================

/// `%`: goes to the bracket matching the first one from the cursor on its
/// line, or with a count, to that percentage of the document's lines.
pub(crate) fn match_bracket(cx: &mut CommandContext<'_>) {
    if let Some(percent) = cx.count {
        if percent > FULL_PERCENT {
            return fail(cx);
        }
        let lines = line_count(cx.editor.document_of(cx.view));
        let line = (percent * lines).div_ceil(FULL_PERCENT);
        return goto_line(cx, line.saturating_sub(1));
    }
    let document = cx.editor.document_of(cx.view);
    let cursor = cx.editor.view_ref(cx.view).cursor();
    match matching_bracket(document, cursor) {
        Some(target) => jump_to(cx, MotionKind::Inclusive, target),
        None => fail(cx),
    }
}

/// `}`: goes forward to the empty line ending a paragraph, once per count.
pub(crate) fn paragraph_forward(cx: &mut CommandContext<'_>) {
    let document = cx.editor.document_of(cx.view);
    let last = last_line(document);
    let mut line = line_of_cursor(cx);
    let mut target = None;
    for _ in 0..cx.count_or_one() {
        while line < last && is_empty_line(document, line) {
            line += 1;
        }
        while line <= last && !is_empty_line(document, line) {
            line += 1;
        }
        if line > last {
            target = Some(document.len_chars());
            break;
        }
        target = Some(start_of_line(document, line));
    }
    let target = target.unwrap_or_else(|| document.len_chars());
    jump_to(cx, MotionKind::Exclusive, target);
}

/// `{`: goes back to the empty line before a paragraph, once per count.
pub(crate) fn paragraph_backward(cx: &mut CommandContext<'_>) {
    let document = cx.editor.document_of(cx.view);
    let mut line = line_of_cursor(cx);
    for _ in 0..cx.count_or_one() {
        while line > 0 && is_empty_line(document, line) {
            line -= 1;
        }
        while line > 0 && !is_empty_line(document, line) {
            line -= 1;
        }
    }
    let target = start_of_line(document, line);
    jump_to(cx, MotionKind::Exclusive, target);
}

/// `(`: goes back to the start of a sentence, once per count.
pub(crate) fn sentence_backward(cx: &mut CommandContext<'_>) {
    let document = cx.editor.document_of(cx.view);
    let mut at = cx.editor.view_ref(cx.view).cursor();
    for _ in 0..cx.count_or_one() {
        at = (0..at)
            .rev()
            .find(|offset| is_sentence_start(document, *offset))
            .unwrap_or(0);
    }
    jump_to(cx, MotionKind::Exclusive, at);
}

/// `)`: goes to the start of the next sentence, once per count.
pub(crate) fn sentence_forward(cx: &mut CommandContext<'_>) {
    let document = cx.editor.document_of(cx.view);
    let len = document.len_chars();
    let mut at = cx.editor.view_ref(cx.view).cursor();
    for _ in 0..cx.count_or_one() {
        at = (at + 1..len)
            .find(|offset| is_sentence_start(document, *offset))
            .unwrap_or(len);
    }
    jump_to(cx, MotionKind::Exclusive, at);
}

/// Whether `offset` starts a sentence, as the module docs say.
pub(crate) fn is_sentence_start(document: &Document, offset: usize) -> bool {
    let Some(c) = document.char_at(offset) else {
        return false;
    };
    let is_blank = |at: usize| document.char_at(at).is_some_and(char::is_whitespace);
    if c == '\n' {
        let is_empty = line_start(document, offset) == offset;
        let follows_text = offset == 0 || line_start(document, offset - 1) != offset - 1;
        return is_empty && follows_text;
    }
    if c.is_whitespace() {
        return false;
    }
    let mut before = offset;
    let mut breaks = 0;
    while before > 0 && is_blank(before - 1) {
        before -= 1;
        breaks += usize::from(document.char_at(before) == Some('\n'));
    }
    if before == offset {
        return offset == 0;
    }
    if before == 0 || breaks >= 2 {
        return true;
    }
    while before > 0
        && document
            .char_at(before - 1)
            .is_some_and(|c| SENTENCE_CLOSERS.contains(&c))
    {
        before -= 1;
    }
    before > 0
        && document
            .char_at(before - 1)
            .is_some_and(|c| SENTENCE_ENDS.contains(&c))
}

/// Whether line `line` holds no characters at all.
fn is_empty_line(document: &Document, line: usize) -> bool {
    let start = start_of_line(document, line);
    line_end(document, start) == start
}

/// The line of the cursor of the command's view.
fn line_of_cursor(cx: &CommandContext<'_>) -> usize {
    let document = cx.editor.document_of(cx.view);
    let cursor = cx.editor.view_ref(cx.view).cursor();
    document
        .char_to_position(cursor)
        .map_or(0, |position| position.line)
}

/// The offset of the bracket matching the first bracket from `cursor` to
/// the end of its line, if both exist.
fn matching_bracket(document: &Document, cursor: usize) -> Option<usize> {
    let end = line_end(document, cursor);
    (cursor..end).find_map(|at| {
        let c = document.char_at(at)?;
        BRACKET_PAIRS.iter().find_map(|&(open, close)| {
            if c == open {
                Some(find_close(document, at + 1, (open, close)))
            } else if c == close {
                Some(find_open(document, at, (open, close)))
            } else {
                None
            }
        })
    })?
}

/// The offset of the bracket matching the bracket at `at`, if `at` holds
/// one of `()`, `[]`, or `{}`, looked for only among the chars `within`,
/// so a caller painting the text in sight never scans past it.
pub(crate) fn bracket_match_within(
    document: &Document,
    at: usize,
    within: Range<usize>,
) -> Option<usize> {
    let c = document.char_at(at)?;
    let &(open, close) = BRACKET_PAIRS
        .iter()
        .find(|(open, close)| c == *open || c == *close)?;
    let forward = c == open;
    let mut depth = 0_usize;
    let offsets: Box<dyn Iterator<Item = usize>> = if forward {
        Box::new(at + 1..within.end.min(document.len_chars()))
    } else {
        Box::new((within.start..at).rev())
    };
    for offset in offsets {
        match document.char_at(offset) {
            Some(found) if found == c => depth += 1,
            Some(found) if (found == close || found == open) && depth == 0 => return Some(offset),
            Some(found) if found == close || found == open => depth -= 1,
            Some(_) | None => {}
        }
    }
    None
}

// ========================================================================
// Tests
// ========================================================================

#[cfg(test)]
mod tests {
    use crate::testing::{Harness, assert_keys};
    use crate::view::Mode;

    #[test]
    fn test_percent_jumps_between_nested_brackets_from_the_first_on_the_line() {
        assert_keys("|f(a(b)c)", "%", "f(a(b)c|)");
        assert_keys("f(a(b)c|)", "%", "f|(a(b)c)");
        assert_keys("f(a|(b)c)", "%", "f(a(b|)c)");
        assert_keys("|x [\n  {y}\n]", "%", "x [\n  {y}\n|]");
        assert_keys("|{ ( }", "%", "{ ( |}");
        assert_keys("|( x", "%", "|( x");
        assert_keys("|abc", "%", "|abc");
    }

    #[test]
    fn test_percent_after_an_operator_is_inclusive_and_a_miss_cancels_it() {
        assert_keys("|(ab) c", "d %", "| c");
        assert_keys("a|b (c) d", "d %", "a| d");
        let mut harness = Harness::new("|abc");
        harness.keys("d %");
        assert_eq!(harness.mode(), Mode::Normal);
        assert_eq!(harness.marked(), "|abc");
    }

    #[test]
    fn test_a_count_before_percent_goes_to_that_share_of_the_lines() {
        let text: String = (1..=10).map(|n| format!(" l{n}\n")).collect();
        let text = text.trim_end();
        let mut harness = Harness::new(&format!("|{text}"));
        harness.keys("50%");
        let line = |harness: &Harness| {
            let cursor = harness.editor.view(harness.view).unwrap().cursor();
            let document = harness.editor.document_of(harness.view);
            (
                document.char_to_position(cursor).unwrap(),
                document.char_at(cursor),
            )
        };
        assert_eq!(line(&harness).0.line, 4);
        assert_eq!(line(&harness).1, Some('l'), "on the first non-blank");
        harness.keys("100%");
        assert_eq!(line(&harness).0.line, 9);
        harness.keys("1%");
        assert_eq!(line(&harness).0.line, 0);
        harness.keys("101%");
        assert_eq!(line(&harness).0.line, 0, "past 100 fails");
    }

    #[test]
    fn test_braces_move_over_paragraphs_to_empty_lines_with_counts() {
        let text = "a\nb\n\n\nc\n  \nd";
        assert_keys(&format!("|{text}"), "}", "a\nb\n|\n\nc\n  \nd");
        assert_keys(&format!("|{text}"), "} }", "a\nb\n\n\nc\n  \n|d");
        assert_keys(&format!("|{text}"), "2}", "a\nb\n\n\nc\n  \n|d");
        assert_keys("a\nb\n\n\nc\n  \n|d", "{", "a\nb\n\n|\nc\n  \nd");
        assert_keys("a\nb\n\n\nc\n  \n|d", "2{", "|a\nb\n\n\nc\n  \nd");
    }

    #[test]
    fn test_d_brace_deletes_the_rest_of_a_paragraph_and_whole_lines_from_its_start() {
        assert_keys("|a\nb\n\nc", "d }", "|\nc");
        assert_keys("a|x\nb\n\nc", "d }", "|a\n\nc");
        assert_keys("|a\nb", "d }", "|");
    }

    #[test]
    fn test_parens_move_over_sentences_and_empty_lines() {
        let text = "One two.  Three? (Four.) Five\n\nSix";
        assert_keys(
            &format!("|{text}"),
            ")",
            "One two.  |Three? (Four.) Five\n\nSix",
        );
        assert_keys(
            &format!("|{text}"),
            "2)",
            "One two.  Three? |(Four.) Five\n\nSix",
        );
        assert_keys(
            &format!("|{text}"),
            "3)",
            "One two.  Three? (Four.) |Five\n\nSix",
        );
        assert_keys(
            &format!("|{text}"),
            "4)",
            "One two.  Three? (Four.) Five\n|\nSix",
        );
        assert_keys(
            &format!("|{text}"),
            "5)",
            "One two.  Three? (Four.) Five\n\n|Six",
        );
        assert_keys("One two.  Thr|ee? x", "(", "One two.  |Three? x");
        assert_keys("One two.  |Three? x", "(", "|One two.  Three? x");
        assert_keys("a.b c. |d", "(", "|a.b c. d");
    }

    #[test]
    fn test_d_paren_deletes_to_the_next_sentence() {
        assert_keys("|One. Two.", "d )", "|Two.");
    }
}
