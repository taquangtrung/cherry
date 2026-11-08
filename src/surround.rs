//! Surround, as vim-surround does it: `ys` and a motion or text object,
//! then a char, wraps the text they cover in a pair (`ysiw)` turns `word`
//! into `(word)`); `yss` and a char wraps the cursor's line, its indent
//! left out; `cs`, the char of the pair around the cursor, and a new
//! char changes that pair; `ds` and a char deletes it.
//!
//! A pair is named by either of its brackets (`(` or `)` for `()`, `[`,
//! `{`, `<`), by `b` for `()` and `B` for `{}`, or by a quote (`"`, `'`,
//! `` ` ``), which pairs with itself. The pair around the cursor is found
//! as the `a(` and `i"` objects find theirs. After a wrap or a change the
//! cursor is on the opening char; after a delete, where it was. A char
//! that names no pair, or no pair around the cursor, changes nothing.
//!
//! The `s` of `ys`, `cs`, and `ds` is a key of operator-pending mode, so
//! `y`, `c`, and `d` stay single keys that wait for no timeout.

use std::ops::Range;

use super::command::{CommandContext, PendingChar};
use super::document::{Document, first_non_blank_in, line_end};
use super::editor::{Editor, ViewId};
use super::operator::{Operand, Operator, PendingOperator};
use super::text_object::{Reach, bracket_object, quote_object};
use super::transaction::Edit;
use super::view::Mode;

// ========================================================================
// Data Structures
// ========================================================================

/// A surround waiting for its next char.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum PendingSurround {
    /// `cs` and the char of the pair to change: the new pair's char comes
    /// next.
    ChangeFrom(char),
    /// `ys` and the text to wrap: the pair's char comes next.
    Wrap(Range<usize>),
}

// ========================================================================
// Functions
// ========================================================================

/// `s` after an operator: `ys` makes the surround operator wait for its
/// motion, `yss` waits for the pair to wrap the cursor's line in, `cs` and
/// `ds` wait for the pair to change or delete; after any other operator,
/// the operator is cancelled.
pub(crate) fn surround(cx: &mut CommandContext<'_>) {
    let Some(pending) = cx.editor.view_ref(cx.view).operator() else {
        return;
    };
    match pending.operator {
        Operator::Yank => {
            let surround = PendingOperator {
                operator: Operator::Surround,
                ..pending
            };
            cx.editor.set_operator(cx.view, surround);
        }
        Operator::Surround => {
            cx.editor.set_mode(cx.view, Mode::Normal);
            let document = cx.editor.document_of(cx.view);
            let cursor = cx.editor.view_ref(cx.view).cursor();
            let line = first_non_blank_in(document, cursor)..line_end(document, cursor);
            await_surround(cx.editor, cx.view, PendingSurround::Wrap(line), wrap_with);
        }
        Operator::Change => {
            cx.editor.set_mode(cx.view, Mode::Normal);
            await_char(cx.editor, cx.view, change_from);
        }
        Operator::Delete => {
            cx.editor.set_mode(cx.view, Mode::Normal);
            await_char(cx.editor, cx.view, delete_pair);
        }
        Operator::Comment
        | Operator::Format
        | Operator::Indent
        | Operator::Lowercase
        | Operator::Outdent
        | Operator::ToggleCase
        | Operator::Uppercase => cx.editor.set_mode(cx.view, Mode::Normal),
    }
}

/// The surround operator run over `operand`: waits for the pair to wrap
/// its text in, whole lines taken from the first one's first non-blank.
pub(crate) fn wrap_operand(editor: &mut Editor, view: ViewId, operand: &Operand) {
    let document = editor.document_of(view);
    let range = match operand {
        Operand::Charwise(range) => range.clone(),
        Operand::Linewise(lines) => {
            let range = lines.text_inside(document);
            first_non_blank_in(document, range.start)..range.end
        }
    };
    await_surround(editor, view, PendingSurround::Wrap(range), wrap_with);
}

/// Wraps the text `ys` covered in the pair `name` names.
fn wrap_with(cx: &mut CommandContext<'_>, name: char) {
    let Some(PendingSurround::Wrap(range)) = cx.editor.view_mut(cx.view).take_surround() else {
        return;
    };
    let Some((open, close)) = pair_of(name) else {
        return;
    };
    let edits = vec![
        Edit::new(range.start..range.start, open.to_string()),
        Edit::new(range.end..range.end, close.to_string()),
    ];
    cx.editor.apply_edits(cx.view, edits);
    cx.editor.set_cursor(cx.view, range.start);
}

/// `cs` and `name`: waits for the char of the pair to change it to.
fn change_from(cx: &mut CommandContext<'_>, name: char) {
    let pending = PendingSurround::ChangeFrom(name);
    await_surround(cx.editor, cx.view, pending, change_to);
}

/// Changes the pair `cs` named around the cursor to the pair `name`
/// names.
fn change_to(cx: &mut CommandContext<'_>, name: char) {
    let Some(PendingSurround::ChangeFrom(from)) = cx.editor.view_mut(cx.view).take_surround()
    else {
        return;
    };
    let cursor = cx.editor.view_ref(cx.view).cursor();
    let found = pair_around(cx.editor.document_of(cx.view), cursor, from);
    let (Some((open_at, close_at)), Some((open, close))) = (found, pair_of(name)) else {
        return;
    };
    let edits = vec![
        Edit::new(open_at..open_at + 1, open.to_string()),
        Edit::new(close_at..close_at + 1, close.to_string()),
    ];
    cx.editor.apply_edits(cx.view, edits);
    cx.editor.set_cursor(cx.view, open_at);
}

/// `ds` and `name`: deletes the pair it names around the cursor.
fn delete_pair(cx: &mut CommandContext<'_>, name: char) {
    let cursor = cx.editor.view_ref(cx.view).cursor();
    let Some((open_at, close_at)) = pair_around(cx.editor.document_of(cx.view), cursor, name)
    else {
        return;
    };
    let edits = vec![
        Edit::new(open_at..open_at + 1, ""),
        Edit::new(close_at..close_at + 1, ""),
    ];
    cx.editor.apply_edits(cx.view, edits);
    let back = usize::from(cursor > open_at) + usize::from(cursor > close_at);
    cx.editor.set_cursor(cx.view, cursor - back);
}

/// The offsets of the opening and closing chars of the pair `name` names
/// around `at`, as the `a(` and `i"` objects find them.
fn pair_around(document: &Document, at: usize, name: char) -> Option<(usize, usize)> {
    let (open, close) = pair_of(name)?;
    if open == close {
        let Some(Operand::Charwise(inside)) = quote_object(document, at, 1, open, Reach::Inner)
        else {
            return None;
        };
        return Some((inside.start - 1, inside.end));
    }
    match bracket_object(document, at, 1, (open, close), Reach::Around)? {
        Operand::Charwise(range) => Some((range.start, range.end - 1)),
        Operand::Linewise(_) => None,
    }
}

/// The opening and closing chars of the pair `name` names, as the module
/// docs say.
fn pair_of(name: char) -> Option<(char, char)> {
    match name {
        '"' | '\'' | '`' => Some((name, name)),
        '(' | ')' | 'b' => Some(('(', ')')),
        '<' | '>' => Some(('<', '>')),
        '[' | ']' => Some(('[', ']')),
        '{' | '}' | 'B' => Some(('{', '}')),
        _ => None,
    }
}

/// Keeps `pending` in `view` and waits for the char `run` takes.
fn await_surround(
    editor: &mut Editor,
    view: ViewId,
    pending: PendingSurround,
    run: fn(&mut CommandContext<'_>, char),
) {
    editor.view_mut(view).set_surround(pending);
    await_char(editor, view, run);
}

/// Waits in `view` for the char `run` takes.
fn await_char(editor: &mut Editor, view: ViewId, run: fn(&mut CommandContext<'_>, char)) {
    let pending = PendingChar {
        count: None,
        register: None,
        run,
    };
    editor.view_mut(view).set_pending_char(Some(pending));
}

// ========================================================================
// Tests
// ========================================================================

#[cfg(test)]
mod tests {
    use crate::testing::assert_keys;

    #[test]
    fn test_ys_wraps_what_a_motion_or_object_covers_and_yss_the_line() {
        assert_keys("one t|wo three", "y s i w )", "one |(two) three");
        assert_keys("one t|wo three", "y s i w b", "one |(two) three");
        assert_keys("|one two", "y s e \"", "|\"one\" two");
        assert_keys("  one t|wo", "y s s ]", "  |[one two]");
        assert_keys("a|b\ncd", "y s j B", "|{ab\ncd}");
        assert_keys("one t|wo", "y s i w x", "one t|wo");
    }

    #[test]
    fn test_cs_changes_and_ds_deletes_the_pair_around_the_cursor() {
        assert_keys("f(a, |b)", "c s ( ]", "f|[a, b]");
        assert_keys("say \"h|i\"", "c s \" '", "say |'hi'");
        assert_keys("f(a, |b)", "d s )", "fa, |b");
        assert_keys("x [(|a)]", "d s [", "x (|a)");
        assert_keys("say 'h|i'", "d s '", "say h|i");
        assert_keys("n|o pair", "d s (", "n|o pair");
    }

    #[test]
    fn test_s_after_another_operator_cancels_it() {
        assert_keys("|ab", "> s l x", "|a");
    }
}
