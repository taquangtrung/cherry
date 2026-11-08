//! Text objects: after an operator or in a visual mode, `i` (inner) or `a`
//! (around) and an object name select a piece of text around the cursor,
//! as in Vim.
//!
//! - `iw` is the word, the run of punctuation, or the run of blanks under
//!   the cursor; `aw` adds the blanks after the word, or when there are
//!   none, the blanks before it (but not a line's indent); on blanks, `aw`
//!   is the blanks and the word after them. `iW` and `aW` take words as
//!   runs of non-blanks. A count takes that many words. Words stay on the
//!   cursor's line.
//! - `i(` and `a(` (also `i)`, `ib`), `i[`, `i{` (also `iB`), and `i<` are
//!   the text inside the brackets around the cursor, or with the brackets
//!   for `a`; the cursor may be on a bracket. A count picks an outer pair
//!   (`2i(`). When the brackets sit alone at the ends of their lines,
//!   `i` takes the whole lines between them.
//! - `i"`, `i'`, `` i` `` are the text inside quotes on the cursor's line
//!   (a quote after a backslash does not count); on a quote, the quotes
//!   pair up from the start of the line, and before the first quote the
//!   first quoted text after the cursor is taken. `a"` adds the quotes
//!   and the blanks after them, or before them when there are none; a
//!   count of two takes the quotes without blanks.
//! - `ip` is the paragraph (the run of non-blank lines) or the run of
//!   blank lines the cursor is on, `ap` adds the blank lines after it (or
//!   before it at the end); both take whole lines and a count.
//! - `is` is the sentence under the cursor (sentences as `(` and `)` find
//!   them), or the run of blanks between two; `as` adds the blanks after
//!   the sentence, or when it ends its paragraph the blanks before it on
//!   its line; on blanks, `as` is the blanks and the sentence after them.
//!   A count takes that many sentences.
//!
//! In a visual mode the object becomes the selection, switching between
//! Visual and VisualLine as the object is characters or lines. Typed
//! again on a selection of more than one char, a word or paragraph object
//! extends the selection with the next one, and a bracket or quote object
//! selects the next pair out.

use std::ops::Range;

use super::command::CommandContext;
use super::document::{Document, last_line, line_end, line_of, line_start, start_of_line};
use super::motion::{CharClass, word_class};
use super::operator::{Lines, Operand, operate_on};
use super::text_motion::is_sentence_start;
use super::view::{Mode, is_visual};
use super::visual::selection;

// ========================================================================
// Constants
// ========================================================================

/// The char that escapes a quote.
const ESCAPE: char = '\\';

// ========================================================================
// Data Structures
// ========================================================================

/// How an object typed again on a visual selection grows it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Growth {
    /// To the next object out that holds the selection: brackets, quotes.
    Enclosing,
    /// Past the selection's end by the next object: words, paragraphs.
    Forward,
}

/// Whether an object takes its delimiters (and blanks) or only what is
/// inside.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Reach {
    /// `a`: the object with its delimiters or blanks.
    Around,
    /// `i`: what is inside.
    Inner,
}

/// Where an object is found from: the document, a char offset, and a count.
type Finder = dyn Fn(&Document, usize, usize) -> Option<Operand>;

// ========================================================================
// Functions
// ========================================================================

/// `ab`, `a(`, `a)`: the parentheses and what is inside.
pub(crate) fn around_paren(cx: &mut CommandContext<'_>) {
    brackets(cx, '(', ')', Reach::Around);
}

/// `a[`, `a]`: the square brackets and what is inside.
pub(crate) fn around_bracket(cx: &mut CommandContext<'_>) {
    brackets(cx, '[', ']', Reach::Around);
}

/// `aB`, `a{`, `a}`: the braces and what is inside.
pub(crate) fn around_brace(cx: &mut CommandContext<'_>) {
    brackets(cx, '{', '}', Reach::Around);
}

/// `a<`, `a>`: the angle brackets and what is inside.
pub(crate) fn around_angle(cx: &mut CommandContext<'_>) {
    brackets(cx, '<', '>', Reach::Around);
}

/// `` a` ``: the backquotes, what is inside, and blanks.
pub(crate) fn around_backtick(cx: &mut CommandContext<'_>) {
    quotes(cx, '`', Reach::Around);
}

/// `aW`: a run of non-blanks and blanks.
pub(crate) fn around_big_word(cx: &mut CommandContext<'_>) {
    words(cx, true, Reach::Around);
}

/// `a"`: the double quotes, what is inside, and blanks.
pub(crate) fn around_double_quote(cx: &mut CommandContext<'_>) {
    quotes(cx, '"', Reach::Around);
}

/// `ap`: a paragraph and the blank lines after it.
pub(crate) fn around_paragraph(cx: &mut CommandContext<'_>) {
    paragraphs(cx, Reach::Around);
}

/// `as`: a sentence and blanks.
pub(crate) fn around_sentence(cx: &mut CommandContext<'_>) {
    sentences(cx, Reach::Around);
}

/// `a'`: the single quotes, what is inside, and blanks.
pub(crate) fn around_single_quote(cx: &mut CommandContext<'_>) {
    quotes(cx, '\'', Reach::Around);
}

/// `aw`: a word and blanks.
pub(crate) fn around_word(cx: &mut CommandContext<'_>) {
    words(cx, false, Reach::Around);
}

/// `i<`, `i>`: inside the angle brackets.
pub(crate) fn inner_angle(cx: &mut CommandContext<'_>) {
    brackets(cx, '<', '>', Reach::Inner);
}

/// `` i` ``: inside the backquotes.
pub(crate) fn inner_backtick(cx: &mut CommandContext<'_>) {
    quotes(cx, '`', Reach::Inner);
}

/// `iW`: a run of non-blanks, or of blanks.
pub(crate) fn inner_big_word(cx: &mut CommandContext<'_>) {
    words(cx, true, Reach::Inner);
}

/// `iB`, `i{`, `i}`: inside the braces.
pub(crate) fn inner_brace(cx: &mut CommandContext<'_>) {
    brackets(cx, '{', '}', Reach::Inner);
}

/// `i[`, `i]`: inside the square brackets.
pub(crate) fn inner_bracket(cx: &mut CommandContext<'_>) {
    brackets(cx, '[', ']', Reach::Inner);
}

/// `i"`: inside the double quotes.
pub(crate) fn inner_double_quote(cx: &mut CommandContext<'_>) {
    quotes(cx, '"', Reach::Inner);
}

/// `ip`: a paragraph, or a run of blank lines.
pub(crate) fn inner_paragraph(cx: &mut CommandContext<'_>) {
    paragraphs(cx, Reach::Inner);
}

/// `ib`, `i(`, `i)`: inside the parentheses.
pub(crate) fn inner_paren(cx: &mut CommandContext<'_>) {
    brackets(cx, '(', ')', Reach::Inner);
}

/// `is`: a sentence, or the blanks between two.
pub(crate) fn inner_sentence(cx: &mut CommandContext<'_>) {
    sentences(cx, Reach::Inner);
}

/// `i'`: inside the single quotes.
pub(crate) fn inner_single_quote(cx: &mut CommandContext<'_>) {
    quotes(cx, '\'', Reach::Inner);
}

/// `iw`: a word, a run of punctuation, or a run of blanks.
pub(crate) fn inner_word(cx: &mut CommandContext<'_>) {
    words(cx, false, Reach::Inner);
}

/// Selects or operates on the brackets `open` and `close` around the
/// cursor, by `reach`.
fn brackets(cx: &mut CommandContext<'_>, open: char, close: char, reach: Reach) {
    let find = move |document: &Document, at: usize, count: usize| {
        bracket_object(document, at, count, (open, close), reach)
    };
    select_object(cx, &find, Growth::Enclosing);
}

/// The object of the `count`-th pair of `brackets` around `at`.
pub(crate) fn bracket_object(
    document: &Document,
    at: usize,
    count: usize,
    brackets: (char, char),
    reach: Reach,
) -> Option<Operand> {
    let (open, close) = brackets;
    let mut open_at = at;
    for level in 0..count {
        open_at = match (level, document.char_at(at)) {
            (0, Some(c)) if c == open => at,
            (0, Some(c)) if c == close => find_open(document, at, brackets)?,
            _ => find_open(document, open_at, brackets)?,
        };
    }
    let close_at = find_close(document, open_at + 1, brackets)?;
    match reach {
        Reach::Around => Some(Operand::Charwise(open_at..close_at + 1)),
        Reach::Inner => Some(inner_brackets(document, open_at, close_at)),
    }
}

/// The offset of the unmatched `open` bracket before `before`.
pub(crate) fn find_open(
    document: &Document,
    before: usize,
    (open, close): (char, char),
) -> Option<usize> {
    let mut depth = 0_usize;
    for at in (0..before).rev() {
        match document.char_at(at) {
            Some(c) if c == close => depth += 1,
            Some(c) if c == open && depth == 0 => return Some(at),
            Some(c) if c == open => depth -= 1,
            Some(_) | None => {}
        }
    }
    None
}

/// The offset of the unmatched `close` bracket from `from` on.
pub(crate) fn find_close(
    document: &Document,
    from: usize,
    (open, close): (char, char),
) -> Option<usize> {
    let mut depth = 0_usize;
    for at in from..document.len_chars() {
        match document.char_at(at) {
            Some(c) if c == open => depth += 1,
            Some(c) if c == close && depth == 0 => return Some(at),
            Some(c) if c == close => depth -= 1,
            Some(_) | None => {}
        }
    }
    None
}

/// What is inside the brackets at `open_at` and `close_at`: the whole
/// lines between them when each sits alone at its end of its line, else
/// the chars between them.
fn inner_brackets(document: &Document, open_at: usize, close_at: usize) -> Operand {
    let (open_line, close_line) = (line_of(document, open_at), line_of(document, close_at));
    let is_blank = |text: String| text.chars().all(|c| matches!(c, ' ' | '\t'));
    let open_ends_line = is_blank(document.slice(open_at + 1..line_end(document, open_at)));
    let close_starts_line = is_blank(document.slice(line_start(document, close_at)..close_at));
    if open_ends_line && close_starts_line && close_line > open_line + 1 {
        return Operand::Linewise(Lines {
            first: open_line + 1,
            last: close_line - 1,
        });
    }
    Operand::Charwise(open_at + 1..close_at)
}

/// Whether line `line` holds only blanks.
fn is_blank_line(document: &Document, line: usize) -> bool {
    let text = document.line_text(line).unwrap_or_default();
    text.chars().all(|c| matches!(c, ' ' | '\t'))
}

/// Selects or operates on paragraphs, by `reach`.
fn paragraphs(cx: &mut CommandContext<'_>, reach: Reach) {
    let find = move |document: &Document, at: usize, count: usize| {
        Some(paragraph_object(document, at, count, reach))
    };
    select_object(cx, &find, Growth::Forward);
}

/// The object of `count` paragraphs from the one at `at`.
fn paragraph_object(document: &Document, at: usize, count: usize, reach: Reach) -> Operand {
    let last_line = last_line(document);
    let line = line_of(document, at);
    let blank = is_blank_line(document, line);
    let run_end = |from: usize| {
        let kind = is_blank_line(document, from);
        let mut end = from;
        while end < last_line && is_blank_line(document, end + 1) == kind {
            end += 1;
        }
        end
    };
    let mut first = line;
    while first > 0 && is_blank_line(document, first - 1) == blank {
        first -= 1;
    }
    let runs_per_count = match reach {
        Reach::Around => 2,
        Reach::Inner => 1,
    };
    let mut last = run_end(line);
    let mut runs = 1;
    while runs < count * runs_per_count && last < last_line {
        last = run_end(last + 1);
        runs += 1;
    }
    if reach == Reach::Around && runs < runs_per_count && !blank {
        while first > 0 && is_blank_line(document, first - 1) {
            first -= 1;
        }
    }
    Operand::Linewise(Lines { first, last })
}

/// Selects or operates on `count` sentences from the cursor's, by
/// `reach`.
fn sentences(cx: &mut CommandContext<'_>, reach: Reach) {
    let find = move |document: &Document, at: usize, count: usize| {
        Some(sentence_object(document, at, count, reach))
    };
    select_object(cx, &find, Growth::Forward);
}

/// The object of `count` sentences from the one at `at`, as the module
/// docs say.
fn sentence_object(document: &Document, at: usize, count: usize, reach: Reach) -> Operand {
    let len = document.len_chars();
    let is_blank = |offset: usize| matches!(document.char_at(offset), Some(' ' | '\t' | '\n'));
    let next_start = |from: usize| {
        (from + 1..len)
            .find(|offset| is_sentence_start(document, *offset))
            .unwrap_or(len)
    };
    let body_end = |start: usize, next: usize| {
        let mut end = next;
        while end > start && is_blank(end - 1) {
            end -= 1;
        }
        end
    };
    if is_blank(at) && !is_sentence_start(document, at) {
        let mut start = at;
        while start > 0 && is_blank(start - 1) {
            start -= 1;
        }
        let mut end = at;
        while end < len && is_blank(end) {
            end += 1;
        }
        return match reach {
            Reach::Inner => Operand::Charwise(start..end),
            Reach::Around => Operand::Charwise(start..body_end(end, next_start(end))),
        };
    }
    let start = (0..=at)
        .rev()
        .find(|offset| is_sentence_start(document, *offset))
        .unwrap_or(0);
    let mut next = start;
    for _ in 0..count {
        next = next_start(next);
    }
    let end = body_end(start, next);
    let ends_paragraph = next == len || document.char_at(next) == Some('\n');
    match (reach, ends_paragraph) {
        (Reach::Inner, _) => Operand::Charwise(start..end),
        (Reach::Around, false) => Operand::Charwise(start..next),
        (Reach::Around, true) => {
            let mut before = start;
            while before > line_start(document, start)
                && matches!(document.char_at(before - 1), Some(' ' | '\t'))
            {
                before -= 1;
            }
            Operand::Charwise(before..end)
        }
    }
}

/// Selects or operates on the `quote` pair on the cursor's line, by
/// `reach`.
fn quotes(cx: &mut CommandContext<'_>, quote: char, reach: Reach) {
    let find = move |document: &Document, at: usize, count: usize| {
        quote_object(document, at, count, quote, reach)
    };
    select_object(cx, &find, Growth::Enclosing);
}

/// The object of the `quote` pair on the line of `at`, as the module
/// docs say.
pub(crate) fn quote_object(
    document: &Document,
    at: usize,
    count: usize,
    quote: char,
    reach: Reach,
) -> Option<Operand> {
    let (start, end) = (line_start(document, at), line_end(document, at));
    let quotes: Vec<usize> = (start..end)
        .filter(|offset| document.char_at(*offset) == Some(quote))
        .filter(|offset| *offset == start || document.char_at(offset - 1) != Some(ESCAPE))
        .collect();
    let (open, close) = match quotes.iter().position(|offset| *offset == at) {
        Some(index) if index % 2 == 0 => (at, *quotes.get(index + 1)?),
        Some(index) => (quotes[index - 1], at),
        None => {
            let before = quotes.iter().rev().find(|offset| **offset < at);
            let mut after = quotes.iter().filter(|offset| **offset > at);
            match (before, after.next()) {
                (Some(open), Some(close)) => (*open, *close),
                (None, Some(open)) => (*open, *after.next()?),
                (Some(_) | None, None) => return None,
            }
        }
    };
    match (reach, count) {
        (Reach::Inner, 0 | 1) => Some(Operand::Charwise(open + 1..close)),
        (Reach::Inner, _) => Some(Operand::Charwise(open..close + 1)),
        (Reach::Around, _) => Some(Operand::Charwise(with_blanks(document, open, close + 1))),
    }
}

/// Runs the waiting operator over `operand`, or selects it in a visual
/// mode; elsewhere, nothing.
pub(crate) fn act_on_operand(cx: &mut CommandContext<'_>, operand: &Operand) {
    match cx.editor.view_ref(cx.view).mode() {
        Mode::OperatorPending => {
            operate_on(cx, operand.clone());
        }
        Mode::Visual | Mode::VisualBlock | Mode::VisualLine => select_operand(cx, operand),
        Mode::CommandLine | Mode::Insert | Mode::Normal | Mode::Replace => {}
    }
}

/// Makes `operand` the visual selection, switching to the visual mode it
/// needs; an empty object selects nothing.
fn select_operand(cx: &mut CommandContext<'_>, operand: &Operand) {
    let document = cx.editor.document_of(cx.view);
    let (mode, anchor, cursor) = match operand {
        Operand::Charwise(range) if range.is_empty() => return,
        Operand::Charwise(range) => (
            Mode::Visual,
            range.start,
            document.prev_grapheme_boundary(range.end),
        ),
        Operand::Linewise(lines) => (
            Mode::VisualLine,
            start_of_line(document, lines.first),
            start_of_line(document, lines.last),
        ),
    };
    cx.editor.set_mode(cx.view, mode);
    cx.editor.set_visual_anchor(cx.view, anchor);
    cx.editor.set_cursor(cx.view, cursor);
}

/// Finds an object with `find` and operates on it after an operator
/// (cancelling the operator when there is none), or selects it in a
/// visual mode, growing a wider selection by `growth`.
fn select_object(cx: &mut CommandContext<'_>, find: &Finder, growth: Growth) {
    let count = cx.count_or_one();
    let cursor = cx.editor.view_ref(cx.view).cursor();
    if !is_visual(cx.editor.view_ref(cx.view).mode()) {
        let operand = find(cx.editor.document_of(cx.view), cursor, count);
        match operand {
            Some(operand) => {
                operate_on(cx, operand);
            }
            None => cx.editor.set_mode(cx.view, Mode::Normal),
        }
        return;
    }
    let Some((anchor, _)) = selection(cx.editor, cx.view) else {
        return;
    };
    let document = cx.editor.document_of(cx.view);
    let selected = anchor.min(cursor)..document.next_grapheme_boundary(anchor.max(cursor));
    let is_single = anchor == cursor;
    let operand = match growth {
        Growth::Enclosing => {
            let mut count = count;
            let mut previous = None;
            loop {
                let Some(operand) = find(document, cursor, count) else {
                    return;
                };
                let range = operand.chars(document);
                let holds = range.start <= selected.start && range.end >= selected.end;
                if holds && (is_single || range != selected) {
                    break operand;
                }
                if previous.as_ref() == Some(&range) {
                    return;
                }
                previous = Some(range);
                count += 1;
            }
        }
        Growth::Forward if is_single || cursor < anchor => {
            let Some(operand) = find(document, cursor, count) else {
                return;
            };
            operand
        }
        Growth::Forward => {
            let Some(operand) = find(document, selected.end, count) else {
                return;
            };
            match operand {
                Operand::Charwise(range) => Operand::Charwise(selected.start..range.end),
                Operand::Linewise(lines) => Operand::Linewise(Lines {
                    first: line_of(document, selected.start),
                    last: lines.last,
                }),
            }
        }
    };
    select_operand(cx, &operand);
}

/// `start..end` with the blanks after it on its line, or when there are
/// none, the blanks before it unless they are the line's indent.
fn with_blanks(document: &Document, start: usize, end: usize) -> Range<usize> {
    let line_end = line_end(document, start);
    let is_blank = |offset: usize| matches!(document.char_at(offset), Some(' ' | '\t'));
    let mut after = end;
    while after < line_end && is_blank(after) {
        after += 1;
    }
    if after > end {
        return start..after;
    }
    let line_start = line_start(document, start);
    let mut before = start;
    while before > line_start && is_blank(before - 1) {
        before -= 1;
    }
    if before == line_start {
        return start..end;
    }
    before..end
}

/// Selects or operates on words, runs of non-blanks when `big`, by
/// `reach`.
fn words(cx: &mut CommandContext<'_>, big: bool, reach: Reach) {
    let find = move |document: &Document, at: usize, count: usize| {
        Some(word_object(document, at, count, big, reach))
    };
    select_object(cx, &find, Growth::Forward);
}

/// The object of `count` words from the one at `at`, as the module docs
/// say.
pub(crate) fn word_object(
    document: &Document,
    at: usize,
    count: usize,
    big: bool,
    reach: Reach,
) -> Operand {
    let (line_start, line_end) = (line_start(document, at), line_end(document, at));
    if at >= line_end {
        return Operand::Charwise(at..at);
    }
    let class = |offset: usize| word_class(document, offset, big);
    let run_end = |from: usize| {
        let kind = class(from);
        let mut end = from;
        while end < line_end && class(end) == kind {
            end = document.next_grapheme_boundary(end);
        }
        end
    };
    let mut start = at;
    while start > line_start {
        let prev = document.prev_grapheme_boundary(start);
        if class(prev) != class(at) {
            break;
        }
        start = prev;
    }
    let starts_on_blank = class(at) == Some(CharClass::Whitespace);
    let mut end = start;
    for _ in 0..count {
        if end >= line_end {
            break;
        }
        let first = class(end);
        end = run_end(end);
        let takes_next = match reach {
            Reach::Around => end < line_end,
            Reach::Inner => false,
        };
        let next_is_blank = class(end) == Some(CharClass::Whitespace);
        if takes_next && (first == Some(CharClass::Whitespace) || next_is_blank) {
            end = run_end(end);
        }
    }
    let ends_on_blank = class(document.prev_grapheme_boundary(end)) == Some(CharClass::Whitespace);
    if reach == Reach::Around && !starts_on_blank && !ends_on_blank {
        return Operand::Charwise(with_blanks(document, start, end));
    }
    Operand::Charwise(start..end)
}

// ========================================================================
// Tests
// ========================================================================

#[cfg(test)]
mod tests {
    use crate::testing::assert_keys;

    #[test]
    fn test_is_and_as_take_sentences_and_the_blanks_around_them() {
        let text = "One. Tw|o here.  Three!\n\nFour.";
        assert_keys(text, "d i s", "One. |  Three!\n\nFour.");
        assert_keys(text, "d a s", "One. |Three!\n\nFour.");
        assert_keys(text, "d 2 i s", "One.| \n\nFour.");
        assert_keys(
            "One. Two here.  Thr|ee!\n\nFour.",
            "d a s",
            "One. Two here|.\n\nFour.",
        );
        assert_keys("One.| Two.", "d i s", "One.|Two.");
        assert_keys("One.| Two.", "d a s", "One|.");
        assert_keys("|One. Two.", "v i s d", "| Two.");
    }

    #[test]
    fn test_iw_takes_the_word_punctuation_or_blanks_under_the_cursor() {
        assert_keys("foo b|ar baz", "d i w", "foo | baz");
        assert_keys("foo|   bar", "d i w", "foo|bar");
        assert_keys("a.b|c(d)", "d i w", "a.|(d)");
        assert_keys("foo b|ar baz", "d 3 i w", "foo| ");
        assert_keys("foo h|é\u{301}llo", "d i w", "foo| ");
    }

    #[test]
    fn test_aw_takes_trailing_blanks_or_else_leading_ones_but_never_the_indent() {
        assert_keys("foo b|ar baz", "d a w", "foo |baz");
        assert_keys("foo b|ar", "d a w", "fo|o");
        assert_keys("    fo|o", "d a w", "   | ");
        assert_keys("    fo|o bar", "d a w", "    |bar");
        assert_keys("foo |  bar baz", "d a w", "foo| baz");
        assert_keys("|a b c d", "d 2 a w", "|c d");
    }

    #[test]
    fn test_big_word_objects_take_runs_of_non_blanks() {
        assert_keys("x a.b|-c y", "d i W", "x | y");
        assert_keys("x a.b|-c y", "d a W", "x |y");
    }

    #[test]
    fn test_bracket_objects_take_the_inside_or_the_pair_and_a_count_goes_out() {
        assert_keys("f(a, |b)", "d i (", "f(|)");
        assert_keys("f(a, |b)", "d a b", "|f");
        assert_keys("f(a, (|b), c)", "d 2 i )", "f(|)");
        assert_keys("f(a, (|b), c)", "2 d i b", "f(|)");
        assert_keys("x [1, |2] y", "c i [ 9 <esc>", "x [|9] y");
        assert_keys("a {b {|c} d}", "d a B", "a {b | d}");
        assert_keys("<a|b>", "d i <", "<|>");
        assert_keys("|(ab)", "d i )", "(|)");
        assert_keys("(ab|)", "d a (", "|");
        assert_keys("no |brackets", "d i (", "no |brackets");
    }

    #[test]
    fn test_inner_brackets_alone_on_their_lines_take_the_lines_between() {
        assert_keys("fn {\n    a|;\n    b;\n}", "d i {", "fn {\n|}");
        assert_keys("{\n  |x\n}", "c i { y <esc>", "{\n|y\n}");
        assert_keys("{ a,\n |b }", "d i {", "{|}");
    }

    #[test]
    fn test_quote_objects_stay_on_the_cursor_line_and_pair_up_from_its_start() {
        assert_keys("say \"hi |there\" now", "d i \"", "say \"|\" now");
        assert_keys("say \"hi |there\" now", "d a \"", "say |now");
        assert_keys("say \"h|i\"", "d a \"", "sa|y");
        assert_keys("|x = 'a' + 'b'", "d i '", "x = '|' + 'b'");
        assert_keys("'a' + |'b'", "d i '", "'a' + '|'");
        assert_keys("'a' |+ 'b'", "d i '", "'a'|'b'");
        assert_keys("\"a\\\"|b\"", "d i \"", "\"|\"");
        assert_keys("`co|de`", "d 2 i `", "|");
        assert_keys("\"a\nb|\"", "d i \"", "\"a\nb|\"");
    }

    #[test]
    fn test_paragraph_objects_take_whole_lines_and_the_blank_lines_after() {
        assert_keys("a\n|b\n\nc", "d i p", "|\nc");
        assert_keys("a\n|b\n\nc", "d a p", "|c");
        assert_keys("a\n\n|c", "d a p", "|a");
        assert_keys("a\n|\n\nc", "d i p", "a\n|c");
        assert_keys("|a\n\nb\n\nc", "d 2 a p", "|c");
    }

    #[test]
    fn test_text_objects_select_in_visual_mode_and_grow_when_typed_again() {
        assert_keys("foo b|ar baz", "v i w d", "foo | baz");
        assert_keys("foo b|ar baz qux", "v i w i w i w d", "foo | qux");
        assert_keys("f((a|b))", "v i ( i ( d", "f(|)");
        assert_keys("f((a|b))", "v a ( a ( d", "|f");
        assert_keys("a\n|b\n\nc", "v i p d", "|\nc");
        assert_keys("say \"h|i\"", "v i \" i \" d", "say| ");
    }
}
