//! Balanced expressions (sexps), as Emacs and paredit move over and edit
//! them. A sexp is a group in brackets (`()`, `[]`, `{}`), a quoted string
//! (`"…"`, `'…'`, a quote after a backslash not closing it), or a run of
//! any other non-blank chars; blanks and line breaks lie between them.
//!
//! # Moving
//!
//! `C-M-f` goes past the next sexp and `C-M-b` back to the start of the
//! one before, staying inside the brackets around the cursor; `C-M-u`
//! goes back up to the bracket opening them and `C-M-e` forward past the
//! one closing them; `C-M-d` goes down past the next opening bracket. Each
//! is a motion, so an operator acts up to it and Visual mode extends to
//! it; with nowhere to go, nothing moves.
//!
//! # Editing
//!
//! Each acts on the brackets around the cursor: slurping forward (`C-)`)
//! takes the sexp after the closing bracket inside it, barfing forward
//! (`C-}`) puts the last sexp inside out past it, and `C-(` and `C-{` do
//! the same at the opening bracket; splicing (`M-s`) drops the two
//! brackets, and raising (`M-r`) puts the sexp at the cursor in place of
//! the whole group.

use super::command::CommandContext;
use super::document::Document;
use super::motion::finish;
use super::operator::MotionKind;
use super::transaction::Edit;

// ========================================================================
// Constants
// ========================================================================

/// The bracket pairs sexps group by, opener and closer.
const PAIRS: [(char, char); 3] = [('(', ')'), ('[', ']'), ('{', '}')];

/// The quotes a string sexp is in.
const QUOTES: [char; 2] = ['"', '\''];

/// The char that escapes a quote.
const ESCAPE: char = '\\';

// ========================================================================
// Functions
// ========================================================================

/// `C-M-f`: past the next sexp.
pub(crate) fn forward_sexp(cx: &mut CommandContext<'_>) {
    let cursor = cx.editor.view_ref(cx.view).cursor();
    if let Some(target) = sexp_after(cx.editor.document_of(cx.view), cursor).map(|(_, end)| end) {
        finish(cx, MotionKind::Exclusive, target);
    }
}

/// `C-M-b`: back to the start of the sexp before.
pub(crate) fn backward_sexp(cx: &mut CommandContext<'_>) {
    let cursor = cx.editor.view_ref(cx.view).cursor();
    if let Some((start, _)) = sexp_before(cx.editor.document_of(cx.view), cursor) {
        finish(cx, MotionKind::Exclusive, start);
    }
}

/// `C-M-u`: back up to the bracket opening the group around the cursor.
pub(crate) fn backward_up_list(cx: &mut CommandContext<'_>) {
    let cursor = cx.editor.view_ref(cx.view).cursor();
    if let Some((open, _)) = enclosing(cx.editor.document_of(cx.view), cursor) {
        finish(cx, MotionKind::Exclusive, open);
    }
}

/// `C-M-e`: forward past the bracket closing the group around the cursor.
pub(crate) fn forward_up_list(cx: &mut CommandContext<'_>) {
    let cursor = cx.editor.view_ref(cx.view).cursor();
    if let Some((_, close)) = enclosing(cx.editor.document_of(cx.view), cursor) {
        finish(cx, MotionKind::Exclusive, close + 1);
    }
}

/// `C-M-d`: down past the next opening bracket inside the group around
/// the cursor.
pub(crate) fn down_list(cx: &mut CommandContext<'_>) {
    let document = cx.editor.document_of(cx.view);
    let mut at = cx.editor.view_ref(cx.view).cursor();
    while let Some((start, end)) = sexp_after(document, at) {
        if document.char_at(start).is_some_and(is_open) {
            return finish(cx, MotionKind::Exclusive, start + 1);
        }
        at = end;
    }
}

/// `C-)`: takes the sexp after the closing bracket into the group.
pub(crate) fn slurp_forward(cx: &mut CommandContext<'_>) {
    edit_group(cx, |document, _, close| {
        let (_, end) = sexp_after(document, close + 1)?;
        let closer = document.char_at(close)?;
        Some(vec![
            Edit::new(close..close + 1, ""),
            Edit::new(end..end, closer.to_string()),
        ])
    });
}

/// `C-}`: puts the last sexp inside the group out past the closing
/// bracket.
pub(crate) fn barf_forward(cx: &mut CommandContext<'_>) {
    edit_group(cx, |document, open, close| {
        let (start, _) = sexp_before(document, close).filter(|(start, _)| *start > open)?;
        let kept_end = blanks_back(document, start).max(open + 1);
        let closer = document.char_at(close)?;
        Some(vec![
            Edit::new(kept_end..kept_end, closer.to_string()),
            Edit::new(close..close + 1, ""),
        ])
    });
}

/// `C-(`: takes the sexp before the opening bracket into the group.
pub(crate) fn slurp_backward(cx: &mut CommandContext<'_>) {
    edit_group(cx, |document, open, _| {
        let (start, _) = sexp_before(document, open)?;
        let opener = document.char_at(open)?;
        Some(vec![
            Edit::new(start..start, opener.to_string()),
            Edit::new(open..open + 1, ""),
        ])
    });
}

/// `C-{`: puts the first sexp inside the group out before the opening
/// bracket.
pub(crate) fn barf_backward(cx: &mut CommandContext<'_>) {
    edit_group(cx, |document, open, close| {
        let (_, end) = sexp_after(document, open + 1).filter(|(_, end)| *end < close)?;
        let next = blanks_forward(document, end).min(close);
        let opener = document.char_at(open)?;
        Some(vec![
            Edit::new(open..open + 1, ""),
            Edit::new(next..next, opener.to_string()),
        ])
    });
}

/// `M-s`: drops the brackets of the group around the cursor.
pub(crate) fn splice(cx: &mut CommandContext<'_>) {
    edit_group(cx, |_, open, close| {
        Some(vec![
            Edit::new(open..open + 1, ""),
            Edit::new(close..close + 1, ""),
        ])
    });
}

/// `M-r`: puts the sexp at the cursor in place of the group around it.
pub(crate) fn raise(cx: &mut CommandContext<'_>) {
    let document = cx.editor.document_of(cx.view);
    let cursor = cx.editor.view_ref(cx.view).cursor();
    let Some((open, close)) = enclosing(document, cursor) else {
        return;
    };
    let Some((start, end)) = sexp_after(document, cursor).filter(|(_, end)| *end <= close) else {
        return;
    };
    let text = document.slice(start..end);
    cx.editor
        .apply_edits(cx.view, vec![Edit::new(open..close + 1, text)]);
    cx.editor.set_cursor(cx.view, open);
}

/// Makes the edits `plan` gives for the group around the cursor, its
/// brackets at `open` and `close`, the cursor carried along by them.
fn edit_group(
    cx: &mut CommandContext<'_>,
    plan: impl Fn(&Document, usize, usize) -> Option<Vec<Edit>>,
) {
    let document = cx.editor.document_of(cx.view);
    let cursor = cx.editor.view_ref(cx.view).cursor();
    let Some(edits) =
        enclosing(document, cursor).and_then(|(open, close)| plan(document, open, close))
    else {
        return;
    };
    cx.editor.apply_edits(cx.view, edits);
}

/// The offsets of the brackets of the innermost group around `at`, the
/// opening one before it and the closing one at or after it.
pub(crate) fn enclosing(document: &Document, at: usize) -> Option<(usize, usize)> {
    let mut closers: Vec<char> = Vec::new();
    let mut open = None;
    for offset in (0..at).rev() {
        let c = document.char_at(offset)?;
        if is_close(c) {
            closers.push(c);
        } else if let Some(closer) = closer_of(c) {
            match closers.last() {
                Some(last) if *last == closer => {
                    closers.pop();
                }
                Some(_) => {}
                None => {
                    open = Some((offset, closer));
                    break;
                }
            }
        }
    }
    let (open, closer) = open?;
    let mut at = open + 1;
    loop {
        let c = document.char_at(at)?;
        if c == closer {
            return Some((open, at));
        }
        at = match sexp_after(document, at) {
            Some((_, end)) if end > at => end,
            Some(_) | None => at + 1,
        };
    }
}

/// The start and end of the sexp after `at`, past blanks; `None` at a
/// closing bracket or the end of the document.
fn sexp_after(document: &Document, at: usize) -> Option<(usize, usize)> {
    let start = blanks_forward(document, at);
    let c = document.char_at(start)?;
    if is_close(c) {
        return None;
    }
    if let Some(closer) = closer_of(c) {
        let mut at = start + 1;
        loop {
            let next = document.char_at(at)?;
            if next == closer {
                return Some((start, at + 1));
            }
            at = match sexp_after(document, at) {
                Some((_, end)) => end,
                None => at + 1,
            };
        }
    }
    if QUOTES.contains(&c) {
        let mut at = start + 1;
        while let Some(next) = document.char_at(at) {
            match next {
                ESCAPE => at += 2,
                next if next == c => return Some((start, at + 1)),
                _ => at += 1,
            }
        }
        return None;
    }
    let mut end = start;
    while document.char_at(end).is_some_and(is_atom) {
        end += 1;
    }
    Some((start, end))
}

/// The start and end of the sexp before `at`, past blanks; `None` at an
/// opening bracket or the start of the document.
fn sexp_before(document: &Document, at: usize) -> Option<(usize, usize)> {
    let end = blanks_back(document, at);
    let c = document.char_at(end.checked_sub(1)?)?;
    if closer_of(c).is_some() {
        return None;
    }
    if is_close(c) || QUOTES.contains(&c) {
        let mut start = end - 1;
        loop {
            start = start.checked_sub(1)?;
            if sexp_after(document, start) == Some((start, end)) {
                return Some((start, end));
            }
        }
    }
    let mut start = end;
    while start > 0 && document.char_at(start - 1).is_some_and(is_atom) {
        start -= 1;
    }
    Some((start, end))
}

/// The first offset at or after `at` that is not a blank.
fn blanks_forward(document: &Document, mut at: usize) -> usize {
    while document.char_at(at).is_some_and(char::is_whitespace) {
        at += 1;
    }
    at
}

/// The offset just past the last char before `at` that is not a blank.
fn blanks_back(document: &Document, mut at: usize) -> usize {
    while at > 0 && document.char_at(at - 1).is_some_and(char::is_whitespace) {
        at -= 1;
    }
    at
}

/// The closing bracket `c` opens a group with, if it is an opening one.
fn closer_of(c: char) -> Option<char> {
    PAIRS
        .iter()
        .find(|(open, _)| *open == c)
        .map(|(_, close)| *close)
}

/// Whether `c` is an opening bracket.
fn is_open(c: char) -> bool {
    closer_of(c).is_some()
}

/// Whether `c` is a closing bracket.
fn is_close(c: char) -> bool {
    PAIRS.iter().any(|(_, close)| *close == c)
}

/// Whether `c` is part of a run of other chars: not a blank, a bracket,
/// or a quote.
fn is_atom(c: char) -> bool {
    !c.is_whitespace() && !is_open(c) && !is_close(c) && !QUOTES.contains(&c)
}

// ========================================================================
// Tests
// ========================================================================

#[cfg(test)]
mod tests {
    use crate::testing::assert_keys;

    #[test]
    fn test_moving_over_sexps_and_up_and_down_the_groups() {
        assert_keys("|(a (b) \"c d\") e", "i C-M-f", "(a (b) \"c d\")| e");
        assert_keys(
            "(|a (b) \"c d\") e",
            "i C-M-f C-M-f C-M-f",
            "(a (b) \"c d\"|) e",
        );
        assert_keys("(a (b) \"c d\"|) e", "i C-M-b C-M-b", "(a |(b) \"c d\") e");
        assert_keys("(a (b|) c)", "i C-M-u", "(a |(b) c)");
        assert_keys("(a (b|) c)", "i C-M-e", "(a (b)| c)");
        assert_keys("|f(a, [b])", "i C-M-d C-M-d", "f(a, [|b])");
        assert_keys("(a b|)", "i C-M-f", "(a b|)");
    }

    #[test]
    fn test_slurping_barfing_splicing_and_raising_edit_the_group() {
        assert_keys("(a |b) c d", "i C-)", "(a |b c) d");
        assert_keys("(a |b c) d", "i C-}", "(a |b) c d");
        assert_keys("x (|a b)", "i C-(", "(x |a b)");
        assert_keys("(x a |b)", "i C-{", "x (a |b)");
        assert_keys("f(a, |[b])", "i M-s", "fa, |[b]");
        assert_keys("f(a, |[b])", "i M-r", "f|[b]");
    }
}
