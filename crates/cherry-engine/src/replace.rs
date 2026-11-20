//! Replace mode (`R`): Insert mode where each typed character takes the
//! place of the one under the cursor, until the line end, past which it is
//! added.
//!
//! Every other key works as in Insert mode (Replace mode's key layer sits
//! over Insert mode's), and `<esc>` leaves it the same way; the session is
//! one undo step. Backspace steps back over what was typed, bringing back
//! each character it replaced and removing each it added; over text not
//! typed in this run (before where it started, or after the cursor moved
//! some other way) it only moves the cursor left.
//!
//! A count types the replacing text again, count less one times, when
//! Replace mode is left, inserted after it rather than over the text
//! (`3Rab<esc>` turns `xxxxyz` into `abababxxyz`, Vim's rule).

use super::command::CommandContext;
use super::document::{line_end, line_start};
use super::editor::{Editor, ViewId};
use super::insert::mark_insert_start;
use super::transaction::Edit;
use super::view::Mode;

// ========================================================================
// Data Structures
// ========================================================================

/// The count typed before `R` and the characters typed since, to type
/// again when Replace mode is left.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct ReplaceRepeat {
    /// How many times the text goes in, the first typed over the text.
    count: usize,
    /// The characters typed over the text, Backspace taking them back.
    typed: String,
}

/// What a run of typing in Replace mode replaced, for Backspace to bring
/// back.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct ReplaceSession {
    /// Where the cursor was left after the last typed character; a cursor
    /// elsewhere means the run was broken.
    cursor: usize,
    /// What each typed character replaced, oldest first: the cluster it
    /// took the place of, or `None` when it was added at a line end.
    replaced: Vec<Option<String>>,
}

// ========================================================================
// Functions
// ========================================================================

/// Backspace in Replace mode: brings back what the last typed character
/// replaced, or moves the cursor left.
pub(crate) fn backspace(cx: &mut CommandContext<'_>) {
    let cursor = cx.editor.view_ref(cx.view).cursor();
    let mut session = cx
        .editor
        .view_mut(cx.view)
        .take_replace_session()
        .unwrap_or_default();
    let typed = (session.cursor == cursor && cursor > 0)
        .then(|| session.replaced.pop())
        .flatten();
    let Some(replaced) = typed else {
        let document = cx.editor.document_of(cx.view);
        let back = document
            .prev_grapheme_boundary(cursor)
            .max(line_start(document, cursor));
        cx.editor.set_cursor(cx.view, back);
        return;
    };
    if let Some(repeat) = cx.editor.view_mut(cx.view).replace_repeat_mut() {
        repeat.typed.pop();
    }
    let before = cursor - 1;
    let text = replaced.unwrap_or_default();
    cx.editor
        .apply_edits(cx.view, vec![Edit::new(before..cursor, text)]);
    cx.editor.set_cursor(cx.view, before);
    session.cursor = before;
    cx.editor
        .view_mut(cx.view)
        .set_replace_session(Some(session));
}

/// `R`: enters Replace mode at the cursor, opening the session's undo
/// group, keeping the count to type the text again on leaving; does
/// nothing in a view that never leaves Insert mode.
pub(crate) fn enter_replace(cx: &mut CommandContext<'_>) {
    if cx.editor.is_insert_locked(cx.view) {
        return;
    }
    cx.editor.view_mut(cx.view).set_auto_indented_line(None);
    cx.editor.history_mut(cx.view).begin_group();
    cx.editor.set_mode(cx.view, Mode::Replace);
    mark_insert_start(cx.editor, cx.view);
    let repeat = ReplaceRepeat {
        count: cx.count_or_one(),
        typed: String::new(),
    };
    cx.editor.view_mut(cx.view).set_replace_repeat(Some(repeat));
}

/// Types the text typed over in Replace mode again at the cursor of
/// `view`, the count before `R` less one times, as Replace mode is left;
/// nothing outside Replace mode or without a count.
pub(crate) fn repeat_replaced_text(editor: &mut Editor, view: ViewId) {
    let Some(repeat) = editor.view_mut(view).take_replace_repeat() else {
        return;
    };
    if repeat.count <= 1 || repeat.typed.is_empty() {
        return;
    }
    let cursor = editor.view_ref(view).cursor();
    let text = repeat.typed.repeat(repeat.count - 1);
    let end = cursor + text.chars().count();
    editor.apply_edits(view, vec![Edit::new(cursor..cursor, text)]);
    editor.set_cursor(view, end);
}

/// Types `c` over the cluster under the cursor of `view`, or adds it at
/// the line end, remembering what it replaced.
pub(crate) fn type_over(editor: &mut Editor, view: ViewId, c: char) {
    let document = editor.document_of(view);
    let cursor = editor.view_ref(view).cursor();
    let replaced = (cursor < line_end(document, cursor)).then(|| {
        let next = document.next_grapheme_boundary(cursor);
        (next, document.slice(cursor..next))
    });
    let end = replaced.as_ref().map_or(cursor, |(next, _)| *next);
    editor.apply_edits(view, vec![Edit::new(cursor..end, c.to_string())]);
    editor.set_cursor(view, cursor + 1);
    let mut session = editor
        .view_mut(view)
        .take_replace_session()
        .filter(|session| session.cursor == cursor)
        .unwrap_or_default();
    session.replaced.push(replaced.map(|(_, text)| text));
    session.cursor = cursor + 1;
    let state = editor.view_mut(view);
    state.set_replace_session(Some(session));
    if let Some(repeat) = state.replace_repeat_mut() {
        repeat.typed.push(c);
    }
}

// ========================================================================
// Tests
// ========================================================================

#[cfg(test)]
mod tests {
    use crate::paint::{CursorShape, Surface};
    use crate::render::render_view;
    use crate::testing::{Harness, assert_keys};
    use crate::view::Mode;

    #[test]
    fn test_typing_in_replace_mode_overwrites_then_appends_past_the_line_end() {
        assert_keys("a|bc\nxy", "R XYZW <esc>", "aXYZ|W\nxy");
        assert_keys("|日本x", "R ab <esc>", "a|bx");
    }

    #[test]
    fn test_backspace_restores_replaced_chars_and_removes_added_ones() {
        assert_keys("a|bc", "R XYZ <bs> <bs> <esc>", "a|Xc");
        assert_keys("a|bc", "R XYZ <bs> <bs> <bs> <bs> <esc>", "|abc");
        assert_keys("|日本x", "R ab <bs> <bs> <esc>", "|日本x");
    }

    #[test]
    fn test_replace_mode_is_one_undo_step_with_an_underline_cursor() {
        let mut harness = Harness::new("|abc");
        harness.keys("R xy");
        assert_eq!(harness.mode(), Mode::Replace);
        let mut surface = Surface::new(10, 2);
        let cursor = render_view(&harness.editor, harness.view, &mut surface).unwrap();
        assert_eq!(cursor.shape, CursorShape::Underline);
        harness.keys("<esc> u");
        assert_eq!(harness.marked(), "|abc");
        harness.keys("R z <esc> l .");
        assert_eq!(harness.marked(), "z|zc", "dot replaces again at the cursor");
    }

    #[test]
    fn test_insert_keys_work_in_replace_mode() {
        assert_keys("|abc", "R x C-e y <esc>", "xbc|y");
        assert_keys("|abcd", "R x <cr> y <esc>", "x\n|ycd");
    }

    #[test]
    fn test_a_count_types_the_replacing_text_again_on_escape() {
        assert_keys("|abcdefgh", "3 R xy <esc>", "xyxyx|ycdefgh");
        assert_keys("|abcdefgh", "2 R xyz <bs> <esc>", "xyx|ycdefgh");
        assert_keys("|abcdefgh", "3 R xy <esc> u", "|abcdefgh");
        assert_keys("|abcdefgh", "2 R x <esc> l .", "xxx|xcdefgh");
    }
}
