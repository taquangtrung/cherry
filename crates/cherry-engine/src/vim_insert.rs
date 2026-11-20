//! The Insert mode keys of the Vim editing style ([`EditingStyle::Vim`])
//! that Cherry's other styles lack.
//!
//! `C-o` leaves Insert mode for one Normal mode command and comes back
//! once Normal mode is idle again (after a visual selection or a prompt
//! the command opened is done). `C-t` and `C-d` indent and outdent the
//! cursor's line one level, the cursor staying on its text. `C-a` types
//! the text typed in the last insert session, and `C-@` does too and
//! leaves Insert mode. `C-y` and `C-e` type the
//! character above and below the cursor, by display column.
//!
//! [`EditingStyle::Vim`]: crate::EditingStyle::Vim

use super::command::CommandContext;
use super::document::{line_end, line_of};
use super::editor::{Editor, ViewId};
use super::insert::{exit_insert, exit_insert_session, insert_text};
use super::motion::{display_column, last_cursor_offset};
use super::register::LAST_INSERTED;
use super::view::{Mode, ReturnToInsert};
use super::visual::{Shift, shift_line_range};
use super::wrap::LineLayout;

// ========================================================================
// Functions
// ========================================================================

/// `C-y`: types the character above the cursor.
pub(crate) fn copy_char_above(cx: &mut CommandContext<'_>) {
    copy_char_from_line(cx, false);
}

/// `C-e`: types the character below the cursor.
pub(crate) fn copy_char_below(cx: &mut CommandContext<'_>) {
    copy_char_from_line(cx, true);
}

/// `C-t`: indents the cursor's line one level.
pub(crate) fn indent_line(cx: &mut CommandContext<'_>) {
    shift_cursor_line(cx, Shift::Right);
}

/// `C-a`: types the text typed in the last insert session.
pub(crate) fn insert_last_inserted(cx: &mut CommandContext<'_>) {
    if let Some(text) = cx.editor.read_register(cx.view, Some(LAST_INSERTED)) {
        insert_text(cx.editor, cx.view, &text.text);
    }
}

/// `C-@`: types the text typed in the last insert session and leaves
/// Insert mode, as Escape does.
pub(crate) fn insert_last_inserted_and_exit(cx: &mut CommandContext<'_>) {
    insert_last_inserted(cx);
    exit_insert(cx);
}

/// `C-o`: leaves Insert mode for one Normal mode command, the cursor
/// moved back onto the line's last character from past it.
pub(crate) fn normal_one_command(cx: &mut CommandContext<'_>) {
    cx.editor.act_once();
    exit_insert_session(cx.editor, cx.view);
    let cursor = cx.editor.view_ref(cx.view).cursor();
    let last = last_cursor_offset(cx.editor, cx.view, cursor);
    let moved_back_to = (cursor > last).then_some(last);
    cx.editor.set_cursor(cx.view, cursor.min(last));
    let after = cx.editor.command_seq();
    let insert_after = ReturnToInsert {
        after,
        moved_back_to,
    };
    cx.editor
        .view_mut(cx.view)
        .set_insert_after(Some(insert_after));
}

/// `C-d`: outdents the cursor's line one level.
pub(crate) fn outdent_line(cx: &mut CommandContext<'_>) {
    shift_cursor_line(cx, Shift::Left);
}

/// Puts `view` back in Insert mode once the command `C-o` left it for has
/// run and Normal mode is idle, at the line end again when `C-o` moved the
/// cursor back from there and it has not moved since; or forgets `C-o`
/// when the command entered Insert mode itself.
pub(crate) fn return_to_insert(editor: &mut Editor, view: ViewId) {
    let state = editor.view_ref(view);
    let Some(insert_after) = state.insert_after() else {
        return;
    };
    let is_done = editor.command_seq() > insert_after.after && state.is_idle();
    if state.mode() == Mode::Insert || is_done {
        editor.view_mut(view).set_insert_after(None);
    }
    if !is_done {
        return;
    }
    editor.set_mode(view, Mode::Insert);
    let cursor = editor.view_ref(view).cursor();
    if insert_after.moved_back_to == Some(cursor) {
        let end = editor.document_of(view).next_grapheme_boundary(cursor);
        editor.set_cursor(view, end);
    }
}

/// Types the character at the cursor's display column on the line below
/// (`below`) or above; nothing when there is no such line or it is too
/// short.
fn copy_char_from_line(cx: &mut CommandContext<'_>, below: bool) {
    let document = cx.editor.document_of(cx.view);
    let cursor = cx.editor.view_ref(cx.view).cursor();
    let line = line_of(document, cursor);
    let target = if below {
        line.checked_add(1)
            .filter(|next| *next < document.len_lines())
    } else {
        line.checked_sub(1)
    };
    let Some(text) = target.and_then(|target| document.line_text(target)) else {
        return;
    };
    let goal = display_column(document, cx.editor.options(), cursor);
    let index = LineLayout::new(&text, None, cx.editor.options().tab_stop()).column_at(goal);
    if let Some(c) = text.chars().nth(index) {
        insert_text(cx.editor, cx.view, &c.to_string());
    }
}

/// Shifts the cursor's line one indent level the way `shift` says, the
/// cursor kept on the same character of its text.
fn shift_cursor_line(cx: &mut CommandContext<'_>, shift: Shift) {
    let document = cx.editor.document_of(cx.view);
    let cursor = cx.editor.view_ref(cx.view).cursor();
    let line = line_of(document, cursor);
    let from_end = line_end(document, cursor) - cursor;
    shift_line_range(cx.editor, cx.view, (line, line), 1, shift);
    let document = cx.editor.document_of(cx.view);
    let end = line_end(document, cx.editor.view_ref(cx.view).cursor());
    cx.editor.set_cursor(cx.view, end.saturating_sub(from_end));
}

// ========================================================================
// Tests
// ========================================================================

#[cfg(test)]
mod tests {
    use crate::options::{EditingStyle, EditorOptions};
    use crate::testing::Harness;
    use crate::view::Mode;

    /// A harness editing `marked` in the Vim style.
    fn vim(marked: &str) -> Harness {
        let options = EditorOptions {
            editing_style: EditingStyle::Vim,
            expand_tab: true,
            shift_width: 2,
            ..EditorOptions::default()
        };
        Harness::with_options(marked, options)
    }

    #[test]
    fn test_c_o_runs_one_normal_command_and_comes_back_to_insert_mode() {
        let mut harness = vim("|one two");
        harness.keys("i C-o w X");
        assert_eq!(harness.mode(), Mode::Insert);
        assert_eq!(harness.marked(), "one X|two");
        harness.keys("C-o d d");
        assert_eq!(
            harness.mode(),
            Mode::Insert,
            "after an operator and its motion"
        );
        assert_eq!(harness.marked(), "|");
    }

    #[test]
    fn test_c_o_at_the_line_end_comes_back_to_the_line_end() {
        let mut harness = vim("on|e");
        harness.keys("A C-o m a X");
        assert_eq!(harness.marked(), "oneX|");
        harness.keys("C-o 0 Y");
        assert_eq!(harness.marked(), "Y|oneX", "a motion moves it");
    }

    #[test]
    fn test_c_at_types_the_last_insert_and_leaves_insert_mode() {
        let mut harness = vim("|x");
        harness.keys("i ab <esc> o C-@");
        assert_eq!(harness.mode(), Mode::Normal);
        assert_eq!(harness.marked(), "abx\na|b");
    }

    #[test]
    fn test_normal_mode_and_the_command_line_have_vims_keys_alone() {
        let mut harness = vim("|one two");
        harness.keys("M-% C-M-f");
        assert_eq!(harness.mode(), Mode::Normal);
        assert_eq!(harness.marked(), "|one two", "Cherry's extras are unbound");
        harness.keys(": abc C-b C-<right> X");
        let prompt = harness
            .editor
            .view(harness.view)
            .and_then(|view| view.prompt());
        assert_eq!(prompt.map(|prompt| prompt.text()).as_deref(), Some("abcX"));
    }

    #[test]
    fn test_c_t_and_c_d_shift_the_line_and_keep_the_cursor_on_its_text() {
        let mut harness = vim("ab|c");
        harness.keys("i C-t C-t");
        assert_eq!(harness.marked(), "    ab|c");
        harness.keys("C-d");
        assert_eq!(harness.marked(), "  ab|c");
    }

    #[test]
    fn test_c_a_types_the_last_insert_and_c_y_c_e_copy_the_char_above_and_below() {
        let mut harness = vim("abc\n|\nxyz");
        harness.keys("i C-y C-e");
        assert_eq!(harness.marked(), "abc\nay|\nxyz");
        harness.keys("<esc> o C-a");
        assert_eq!(harness.marked(), "abc\nay\nay|\nxyz");
    }

    #[test]
    fn test_insert_mode_has_vim_keys_and_no_emacs_keys() {
        let mut harness = vim("one |two");
        harness.keys("i C-f C-b C-k X C-w");
        assert_eq!(harness.marked(), "one |two", "C-f, C-b, C-k do nothing");
        harness.keys("<esc>");
        assert_eq!(harness.mode(), Mode::Normal);
    }
}
