//! Finding a character on the cursor's line: `f`, `F`, `t`, `T`, and their
//! repeats `;` and `,`.
//!
//! `f{char}` moves to the count-th `{char}` after the cursor on its line,
//! `F{char}` to the count-th before it; `t` and `T` stop one character
//! short of it, on the near side. A character the line lacks (or lacks
//! that many of) leaves the cursor where it is and cancels a waiting
//! operator. After an operator, `f` and `t` are inclusive (`dfx` deletes
//! the `x` too), `F` and `T` exclusive (the char under the cursor stays).
//!
//! `;` repeats the last find, the same way; `,` repeats it the other way.
//! A repeated `t` or `T` whose char is right next to the cursor looks for
//! the one after, so `;` keeps moving instead of sticking in place. The
//! last find is shared by every view.

use super::command::{CommandContext, await_char};
use super::document::{Document, line_end, line_start};
use super::motion::{fail, finish};
use super::operator::MotionKind;

// ========================================================================
// Data Structures
// ========================================================================

/// A find on the line, kept for `;` and `,`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct CharFind {
    /// Whether it looks after the cursor (`f`, `t`) or before it (`F`, `T`).
    pub forward: bool,
    /// The character looked for.
    pub target: char,
    /// Whether it stops one character short of the target (`t`, `T`).
    pub till: bool,
}

// ========================================================================
// Functions
// ========================================================================

/// `F`: waits for the character to find before the cursor.
pub(crate) fn find_backward(cx: &mut CommandContext<'_>) {
    await_char(cx, |cx, target| {
        run_find(
            cx,
            CharFind {
                forward: false,
                target,
                till: false,
            },
        );
    });
}

/// `f`: waits for the character to find after the cursor.
pub(crate) fn find_forward(cx: &mut CommandContext<'_>) {
    await_char(cx, |cx, target| {
        run_find(
            cx,
            CharFind {
                forward: true,
                target,
                till: false,
            },
        );
    });
}

/// `,`: repeats the last find the other way.
pub(crate) fn repeat_find_reversed(cx: &mut CommandContext<'_>) {
    repeat_find(cx, true);
}

/// `;`: repeats the last find the same way.
pub(crate) fn repeat_find_same(cx: &mut CommandContext<'_>) {
    repeat_find(cx, false);
}

/// `T`: waits for the character to stop just after, before the cursor.
pub(crate) fn till_backward(cx: &mut CommandContext<'_>) {
    await_char(cx, |cx, target| {
        run_find(
            cx,
            CharFind {
                forward: false,
                target,
                till: true,
            },
        );
    });
}

/// `t`: waits for the character to stop just before, after the cursor.
pub(crate) fn till_forward(cx: &mut CommandContext<'_>) {
    await_char(cx, |cx, target| {
        run_find(
            cx,
            CharFind {
                forward: true,
                target,
                till: true,
            },
        );
    });
}

/// Where `find` from `cursor` lands, the count-th match on the line;
/// `None` when the line has too few. A `repeated` till skips a match right
/// next to the cursor.
fn find_target(
    document: &Document,
    cursor: usize,
    find: CharFind,
    count: usize,
    repeated: bool,
) -> Option<usize> {
    let (start, end) = (line_start(document, cursor), line_end(document, cursor));
    let mut at = cursor;
    let skip_adjacent = repeated && find.till;
    for index in 0..count {
        at = if find.forward {
            let from = if index == 0 && skip_adjacent {
                at + 2
            } else {
                at + 1
            };
            (from..end).find(|offset| document.char_at(*offset) == Some(find.target))?
        } else {
            let to = if index == 0 && skip_adjacent {
                at.checked_sub(1)?
            } else {
                at
            };
            (start..to)
                .rev()
                .find(|offset| document.char_at(*offset) == Some(find.target))?
        };
    }
    let target = match (find.forward, find.till) {
        (true, true) => at - 1,
        (false, true) => at + 1,
        (true, false) | (false, false) => at,
    };
    Some(target)
}

/// Repeats the last find, the other way when `reversed`; does nothing (and
/// cancels a waiting operator) when there was none.
fn repeat_find(cx: &mut CommandContext<'_>, reversed: bool) {
    let Some(last) = cx.editor.last_find() else {
        return fail(cx);
    };
    let find = CharFind {
        forward: last.forward != reversed,
        ..last
    };
    move_by_find(cx, find, true);
}

/// Runs `find`, typed now, and keeps it for `;` and `,`.
fn run_find(cx: &mut CommandContext<'_>, find: CharFind) {
    cx.editor.set_last_find(find);
    move_by_find(cx, find, false);
}

/// Moves the cursor by `find`, or operates over what it covers.
fn move_by_find(cx: &mut CommandContext<'_>, find: CharFind, repeated: bool) {
    let document = cx.editor.document_of(cx.view);
    let cursor = cx.editor.view_ref(cx.view).cursor();
    let Some(target) = find_target(document, cursor, find, cx.count_or_one(), repeated) else {
        return fail(cx);
    };
    let kind = if find.forward {
        MotionKind::Inclusive
    } else {
        MotionKind::Exclusive
    };
    finish(cx, kind, target);
}

// ========================================================================
// Tests
// ========================================================================

#[cfg(test)]
mod tests {
    use crate::testing::{Harness, assert_keys};
    use crate::view::Mode;

    #[test]
    fn test_f_and_t_find_the_count_th_char_after_the_cursor_on_its_line() {
        assert_keys("|a,b,c,d", "f ,", "a|,b,c,d");
        assert_keys("|a,b,c,d", "3f ,", "a,b,c|,d");
        assert_keys("|a,b,c,d", "t ,", "|a,b,c,d");
        assert_keys("|a,b,c,d", "2t ,", "a,|b,c,d");
        assert_keys("|ab\nx", "f x", "|ab\nx");
        assert_keys("|a,b", "5f ,", "|a,b");
    }

    #[test]
    fn test_capital_f_and_t_find_before_the_cursor() {
        assert_keys("a,b,c|d", "F ,", "a,b|,cd");
        assert_keys("a,b,c|d", "2F ,", "a|,b,cd");
        assert_keys("a,b,c|d", "T ,", "a,b,|cd");
        assert_keys("x\na|b", "F x", "x\na|b");
    }

    #[test]
    fn test_semicolon_and_comma_repeat_the_find_and_till_steps_past_its_char() {
        assert_keys("|a,b,c,d", "f , ;", "a,b|,c,d");
        assert_keys("|a,b,c,d", "f , ; ,", "a|,b,c,d");
        assert_keys("|a,b,c,d", "t , ;", "a,|b,c,d");
        assert_keys("|a,b,c,d", "t , 2;", "a,b,|c,d");
        assert_keys("a,b,c,|d", "T , ;", "a,b,|c,d");
        assert_keys("|a,b,c,d", ";", "|a,b,c,d");
    }

    #[test]
    fn test_finds_after_an_operator_are_inclusive_forward_and_exclusive_back() {
        assert_keys("|one,two", "d f ,", "|two");
        assert_keys("|one,two", "d t ,", "|,two");
        assert_keys("one,tw|o", "d F ,", "one|o");
        assert_keys("one,tw|o", "d T ,", "one,|o");
        assert_keys("|a,b,c", "d 2f ,", "|c");
        assert_keys("|a,b,c", "2d f ,", "|c");
        assert_keys("|a,b,c,d", "f , d ;", "a|c,d");
    }

    #[test]
    fn test_a_failed_find_cancels_the_operator_and_dot_repeats_a_find_delete() {
        let mut harness = Harness::new("|abc");
        harness.keys("d f z");
        assert_eq!(harness.mode(), Mode::Normal);
        assert_eq!(harness.marked(), "|abc");
        assert_keys("|a,b,c,d", "d f , .", "|c,d");
        assert_keys("|a,b,c,d", "v f , ; d", "|c,d");
    }
}
