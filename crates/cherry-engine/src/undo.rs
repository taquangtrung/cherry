//! Undo and redo: the commands that walk the document's history, one step
//! per count. An insert session is one step, however the cursor moved in
//! it. Both put the cursor where Vim does: at the start of the change
//! undone or redone, which is where the cursor was when the change began,
//! or the first non-blank of the first changed line when the change began
//! on another line. The cursor is kept onto a character in Normal mode.
//!
//! `g-` and `g+` go to the state of the text a count of steps earlier or
//! later in the order the steps were made, across the branches of the undo
//! tree (a change after an undo makes a new branch and keeps the undone
//! one), as Vim's do; they walk the tree there step by step, the cursor
//! placed as by the last undo or redo.

use super::command::CommandContext;
use super::editor::{Editor, HistoryStep, ViewId};
use super::motion::last_cursor_offset;

// ========================================================================
// Functions
// ========================================================================

/// Redoes the next undone step, once per count.
pub(crate) fn redo(cx: &mut CommandContext<'_>) {
    walk_history(cx, HistoryStep::Redo);
}

/// `g-`: the state a count of steps earlier in time.
pub(crate) fn earlier(cx: &mut CommandContext<'_>) {
    let count = cx.count_or_one();
    let target = cx.editor.history_mut(cx.view).earlier(count);
    walk_in_time(cx, target, "Already at oldest change");
}

/// `g+`: the state a count of steps later in time.
pub(crate) fn later(cx: &mut CommandContext<'_>) {
    let count = cx.count_or_one();
    let target = cx.editor.history_mut(cx.view).later(count);
    walk_in_time(cx, target, "Already at newest change");
}

/// Walks the tree to the state of `target`, or gives `notice` when there
/// is none.
fn walk_in_time(cx: &mut CommandContext<'_>, target: Option<u64>, notice: &str) {
    cx.editor.act_once();
    let Some(target) = target else {
        return cx.editor.error(notice.to_string());
    };
    while cx.editor.step_history(cx.view, HistoryStep::Toward(target)) {}
    keep_cursor_on_line(cx.editor, cx.view);
}

/// Undoes the newest step, once per count.
pub(crate) fn undo(cx: &mut CommandContext<'_>) {
    walk_history(cx, HistoryStep::Undo);
}

/// Takes `step` once per count, stopping at the first that cannot be
/// taken, then keeps the cursor on a character.
fn walk_history(cx: &mut CommandContext<'_>, step: HistoryStep) {
    cx.editor.act_once();
    for _ in 0..cx.count_or_one() {
        if !cx.editor.step_history(cx.view, step) {
            break;
        }
    }
    keep_cursor_on_line(cx.editor, cx.view);
}

/// Moves the cursor of `view` back onto the last character of its line when
/// it is past it.
fn keep_cursor_on_line(editor: &mut Editor, view: ViewId) {
    let cursor = editor.view_ref(view).cursor();
    let last = last_cursor_offset(editor, view, cursor);
    if cursor > last {
        editor.set_cursor(view, last);
    }
}

// ========================================================================
// Tests
// ========================================================================

#[cfg(test)]
mod tests {
    use crate::request::{HostRequest, NoticeLevel, NoticeRequest};
    use crate::testing::{Harness, assert_keys};

    #[test]
    fn test_g_minus_and_g_plus_reach_an_undone_branch() {
        let mut harness = Harness::new("|a");
        harness.keys("x i b <esc> u i c <esc>");
        assert_eq!(harness.marked(), "|c");
        harness.keys("g -");
        assert_eq!(
            harness.editor.document_of(harness.view).to_string(),
            "b",
            "the branch undo left"
        );
        harness.keys("g - g -");
        assert_eq!(harness.editor.document_of(harness.view).to_string(), "a");
        harness.keys("3 g +");
        assert_eq!(harness.editor.document_of(harness.view).to_string(), "c");
    }

    #[test]
    fn test_an_insert_session_undoes_as_one_step_and_redoes_back() {
        assert_keys("ab|c", "i X <bs> Y <cr> Z <esc> u", "ab|c");
        assert_keys("ab|c", "i XY <esc> u C-r", "ab|XYc");
    }

    #[test]
    fn test_redo_puts_the_cursor_at_the_start_of_the_change_not_where_it_ended() {
        assert_keys("ab|c\nd", "i X <down> Y <esc> u C-r", "ab|Xc\ndY");
    }

    #[test]
    fn test_undo_of_a_change_begun_on_another_line_goes_to_its_first_non_blank() {
        assert_keys("  a\n|b", "i X <up> Y <esc> u", "  |a\nb");
        assert_keys("  a\n|b", "i X <up> Y <esc> u C-r", " |Y a\nXb");
    }

    #[test]
    fn test_opening_a_line_undoes_with_its_insert_session() {
        assert_keys("a|b\nc", "o new <esc> u", "a|b\nc");
    }

    #[test]
    fn test_counted_undo_takes_several_steps_and_stops_at_the_oldest() {
        assert_keys("|", "i a <esc> a b <esc> a c <esc> 2u", "|a");
        assert_keys("|", "i a <esc> a b <esc> 9u", "|");
        assert_keys("|", "i a <esc> a b <esc> 2u 2 C-r", "a|b");
    }

    #[test]
    fn test_undo_and_redo_with_nothing_to_walk_say_so() {
        let mut harness = Harness::new("|x");
        harness.keys("u C-r");
        let notice = |message: &str| {
            HostRequest::Notice(NoticeRequest {
                level: NoticeLevel::Error,
                message: message.to_string(),
            })
        };
        assert_eq!(
            harness.requests(),
            [
                notice("Already at oldest change"),
                notice("Already at newest change"),
            ]
        );
    }

    #[test]
    fn test_typing_after_undo_drops_the_redo_steps() {
        assert_keys("|", "i a <esc> u i b <esc> C-r", "|b");
    }

    #[test]
    fn test_undo_keeps_the_cursor_of_another_view_inside_the_text() {
        let mut harness = Harness::new("|");
        let document = harness.editor.view(harness.view).unwrap().document();
        let other = harness.editor.new_view(document).unwrap();
        harness.keys("i hello <esc>");
        harness.editor.set_cursor(other, 5);
        harness.keys("u");
        assert_eq!(harness.editor.view(other).unwrap().cursor(), 0);
    }
}
