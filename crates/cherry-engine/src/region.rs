//! The Emacs region, in the Emacs editing style
//! ([`EditingStyle::Emacs`]).
//!
//! `C-SPC` sets the mark at the cursor and activates the region: from then
//! on, after each command that changes no text, the selection runs from
//! the mark to the cursor, so the motions grow and shrink it. `C-w` kills
//! the region and `M-w` copies it, both to the kill ring and to the
//! clipboard (register `+`), as Emacs does with `select-enable-clipboard`;
//! `C-g` drops it, as does any edit; typed text goes in at the cursor,
//! the region left alone. `C-x C-x` swaps the cursor and the mark.
//!
//! [`EditingStyle::Emacs`]: crate::EditingStyle::Emacs

use super::command::CommandContext;
use super::editor::{Editor, ViewId};
use super::insert::delete_forward;
use super::kill::{Kill, KillDirection};
use super::options::EditingStyle;
use super::register::{CLIPBOARD, RegisterKind, RegisterText};
use super::selection::{Range, Selection};

// ========================================================================
// Constants
// ========================================================================

/// The notice for `C-x C-x` with no mark set.
const NO_MARK: &str = "No mark set in this buffer";

// ========================================================================
// Functions
// ========================================================================

/// `M-w`: copies the region to the kill ring and the clipboard and drops
/// it, the cursor staying where it is.
pub(crate) fn copy_region(cx: &mut CommandContext<'_>) {
    if save_region(cx) {
        let cursor = cx.editor.view_ref(cx.view).cursor();
        cx.editor.set_cursor(cx.view, cursor);
    }
}

/// Drops the region before text is typed in `view` in the Emacs style,
/// where typed text goes in at the cursor, not over the selection.
pub(crate) fn drop_region_for_typing(editor: &mut Editor, view: ViewId) {
    editor.view_mut(view).set_region_mark(None);
    if editor.options().editing_style == EditingStyle::Emacs {
        let cursor = editor.view_ref(view).cursor();
        editor.set_cursor(view, cursor);
    }
}

/// `C-x C-x`: swaps the cursor and the mark, the region kept active; with
/// no mark set, says so.
pub(crate) fn exchange_point_and_mark(cx: &mut CommandContext<'_>) {
    cx.editor.act_once();
    let Some(mark) = cx.editor.view_ref(cx.view).region_mark() else {
        return cx.editor.error(NO_MARK.to_string());
    };
    let cursor = cx.editor.view_ref(cx.view).cursor();
    cx.editor.set_cursor(cx.view, mark);
    cx.editor.view_mut(cx.view).set_region_mark(Some(cursor));
}

/// After a command ran in `view`, selects from the mark to the cursor
/// while the region is active; any change of the text, by a command, an
/// undo, or the host, drops the mark first.
pub(crate) fn follow_mark(editor: &mut Editor, view: ViewId) {
    let Some(mark) = editor.view_ref(view).region_mark() else {
        return;
    };
    let len = editor.document_of(view).len_chars();
    let head = editor.view_ref(view).cursor();
    let selection = Selection::single(Range::new(mark.min(len), head));
    editor.view_mut(view).set_selection(selection);
}

/// `C-w`: kills the region to the kill ring and the clipboard.
pub(crate) fn kill_region(cx: &mut CommandContext<'_>) {
    if save_region(cx) {
        delete_forward(cx);
    }
}

/// `C-SPC`: sets the mark at the cursor, activating the region.
pub(crate) fn set_region_mark(cx: &mut CommandContext<'_>) {
    cx.editor.act_once();
    let cursor = cx.editor.view_ref(cx.view).cursor();
    cx.editor.set_cursor(cx.view, cursor);
    cx.editor.view_mut(cx.view).set_region_mark(Some(cursor));
}

/// Puts the selected text on the kill ring and the clipboard and drops
/// the mark; returns false, saving nothing, when nothing is selected.
fn save_region(cx: &mut CommandContext<'_>) -> bool {
    cx.editor.act_once();
    cx.editor.view_mut(cx.view).set_region_mark(None);
    let range = cx.editor.view_ref(cx.view).selection().primary();
    if range.is_empty() {
        return false;
    }
    let text = cx
        .editor
        .document_of(cx.view)
        .slice(range.start()..range.end());
    let seq = cx.editor.command_seq();
    let kill = Kill {
        direction: KillDirection::Forward,
        text: text.clone(),
    };
    cx.editor.kill_ring_mut().kill(kill, seq);
    let text = RegisterText::new(RegisterKind::Charwise, text);
    cx.editor.registers_mut().record_yank(Some(CLIPBOARD), text);
    true
}

// ========================================================================
// Tests
// ========================================================================

#[cfg(test)]
mod tests {
    use crate::options::{EditingStyle, EditorOptions};
    use crate::request::HostRequest;
    use crate::testing::Harness;
    use crate::transaction::Edit;
    use crate::view::Mode;

    /// A harness editing `marked` in the Emacs style.
    fn emacs(marked: &str) -> Harness {
        let options = EditorOptions {
            editing_style: EditingStyle::Emacs,
            ..EditorOptions::default()
        };
        Harness::with_options(marked, options)
    }

    #[test]
    fn test_the_motions_after_the_mark_select_the_region_that_c_w_kills_and_c_y_yanks() {
        let mut harness = emacs("|one two");
        harness.keys("C-SPC M-f M-f C-b C-w");
        assert_eq!(harness.mode(), Mode::Insert);
        assert_eq!(harness.marked(), "|o");
        harness.keys("C-e C-y");
        assert_eq!(harness.marked(), "oone tw|");
        let clipboard = harness.editor.registers_mut().read(Some('+'));
        assert_eq!(clipboard.map(|text| text.text).as_deref(), Some("one tw"));
    }

    #[test]
    fn test_m_w_copies_the_region_and_c_g_or_an_edit_drops_it() {
        let mut harness = emacs("one |two");
        harness.keys("C-SPC C-e M-w C-a C-y");
        assert_eq!(harness.marked(), "two|one two");
        harness.keys("C-SPC C-e C-g C-a X");
        assert_eq!(harness.marked(), "X|twoone two", "C-g dropped the region");
        harness.keys("C-SPC C-f Y C-f Z");
        assert_eq!(
            harness.marked(),
            "XtYwZ|oone two",
            "typing dropped the mark"
        );
    }

    #[test]
    fn test_an_edit_by_the_host_drops_the_mark() {
        let mut harness = emacs("|one two");
        harness.keys("C-SPC M-f");
        let edit = Edit::new(7..7, "!");
        harness.editor.apply_edits(harness.view, vec![edit]);
        harness.keys("C-f");
        let view = harness.editor.view(harness.view).expect("the view is open");
        assert!(view.selection().primary().is_empty(), "no region follows");
    }

    #[test]
    fn test_c_x_keys_save_and_undo_and_c_s_searches_from_insert_mode() {
        let mut harness = emacs("|one two one");
        harness.keys("x C-x C-s");
        assert!(matches!(
            harness.requests().as_slice(),
            [HostRequest::Save(_)]
        ));
        harness.keys("C-/");
        assert_eq!(harness.marked(), "|one two one");
        harness.keys("C-s two <cr>");
        assert_eq!(harness.mode(), Mode::Insert);
        assert_eq!(harness.marked(), "one |two one");
        harness.keys("M-x");
        assert_eq!(harness.mode(), Mode::CommandLine);
        harness.keys("<esc>");
        assert_eq!(harness.mode(), Mode::Insert);
    }
}
