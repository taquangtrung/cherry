//! The standard editing style ([`EditingStyle::Standard`]): editing
//! without modes, as in the text editors of most desktop apps.
//!
//! Views never leave Insert mode. A selection is the range between an
//! anchor and the cursor, so it sits between characters: Shift with a
//! motion key moves the cursor and keeps the anchor, a motion key alone
//! drops the selection (Left and Right to its near end), and typed text,
//! Backspace, and Delete replace what is selected. Ctrl+A selects the whole
//! document; Ctrl+C, Ctrl+X, and Ctrl+V copy, cut, and paste through the
//! clipboard, register `+`; Ctrl+Z undoes and Ctrl+Y redoes; Ctrl+S asks
//! the host to save and Ctrl+Q to close the view. With nothing selected,
//! Ctrl+C and Ctrl+X take the cursor's line, and Ctrl+V puts lines taken
//! so above the cursor's line. Ctrl+D adds a cursor at the next match of
//! the word, Alt+Up and Alt+Down move lines, Ctrl+Shift+K deletes them,
//! Ctrl+/ comments them, Tab and Shift+Tab indent and outdent the selected
//! lines, Ctrl+F searches, and Ctrl+H replaces, asking at each match. Each
//! command closes the undo step typing builds, but a run of one command
//! (Backspace held down) and the typing after it make one step.
//!
//! [`EditingStyle::Standard`]: crate::EditingStyle::Standard

use std::ops;

use super::command::CommandContext;
use super::comment::toggle_comment;
use super::document::{Document, line_end, line_of, line_start, start_of_line};
use super::insert::{delete_forward, insert_text};
use super::line_edit::{backward_word, document_end, document_start, forward_word};
use super::motion::{
    line_down, line_end_motion, line_start_motion, line_up, move_left, move_right,
};
use super::register::{CLIPBOARD, RegisterKind, RegisterText};
use super::selection::{Range, Selection};
use super::snippet::{next_stop_or_tab, previous_stop};
use super::transaction::Edit;
use super::visual::{Shift, shift_line_range};

// ========================================================================
// Constants
// ========================================================================

/// The line break between lines.
const LINE_BREAK: char = '\n';

// ========================================================================
// Functions
// ========================================================================

/// `C-c`: puts the selected text on the clipboard; with nothing
/// selected, the cursor's line, as whole lines.
pub(crate) fn copy(cx: &mut CommandContext<'_>) {
    cx.editor.act_once();
    let range = cx.editor.view_ref(cx.view).selection().primary();
    let document = cx.editor.document_of(cx.view);
    let text = match range.is_empty() {
        true => {
            let line = line_of(document, range.head);
            let text = document.slice(line_span(document, line, line));
            RegisterText::new(RegisterKind::Linewise, text)
        }
        false => RegisterText::new(
            RegisterKind::Charwise,
            document.slice(range.start()..range.end()),
        ),
    };
    cx.editor.registers_mut().record_yank(Some(CLIPBOARD), text);
}

/// `C-x`: puts the selected text on the clipboard and deletes it; with
/// nothing selected, the cursor's line.
pub(crate) fn cut(cx: &mut CommandContext<'_>) {
    copy(cx);
    let range = cx.editor.view_ref(cx.view).selection().primary();
    if !range.is_empty() {
        return delete_forward(cx);
    }
    let document = cx.editor.document_of(cx.view);
    let line = line_of(document, range.head);
    let span = line_span(document, line, line);
    let start = span.start;
    cx.editor.apply_edits(cx.view, vec![Edit::new(span, "")]);
    cx.editor.set_cursor(cx.view, start);
}

/// `S-<tab>`: the stop before in a snippet being filled in, else
/// outdents the cursor's line or the selected lines one level.
pub(crate) fn outdent_selection_or_previous_stop(cx: &mut CommandContext<'_>) {
    if cx.editor.view_ref(cx.view).snippet().is_some() {
        return previous_stop(cx);
    }
    shift_selected_lines(cx, Shift::Left);
}

/// `<tab>`: indents the selected lines one level; with nothing selected,
/// or in a snippet being filled in, does what Tab does in Insert mode.
pub(crate) fn indent_selection_or_tab(cx: &mut CommandContext<'_>) {
    let state = cx.editor.view_ref(cx.view);
    if state.snippet().is_some() || state.selection().primary().is_empty() {
        return next_stop_or_tab(cx);
    }
    shift_selected_lines(cx, Shift::Right);
}

/// `<left>`: moves the cursor a character left, or to the start of the
/// selection, dropping it.
pub(crate) fn left_or_selection_start(cx: &mut CommandContext<'_>) {
    let range = cx.editor.view_ref(cx.view).selection().primary();
    if range.is_empty() {
        return move_left(cx);
    }
    cx.editor.set_cursor(cx.view, range.start());
}

/// `M-<down>`: moves the cursor's line, or the selected lines, below the
/// line after them.
pub(crate) fn move_lines_down(cx: &mut CommandContext<'_>) {
    move_lines(cx, true);
}

/// `M-<up>`: moves the cursor's line, or the selected lines, above the
/// line before them.
pub(crate) fn move_lines_up(cx: &mut CommandContext<'_>) {
    move_lines(cx, false);
}

/// `C-v`: types the text on the clipboard in place of the selection;
/// whole lines, copied with nothing selected, go in above the cursor's
/// line, the cursor staying on it.
pub(crate) fn paste(cx: &mut CommandContext<'_>) {
    let Some(text) = cx.editor.read_register(cx.view, Some(CLIPBOARD)) else {
        return;
    };
    let range = cx.editor.view_ref(cx.view).selection().primary();
    if text.kind != RegisterKind::Linewise || !range.is_empty() {
        return insert_text(cx.editor, cx.view, &text.text);
    }
    let start = line_start(cx.editor.document_of(cx.view), range.head);
    let cursor = range.head + text.text.chars().count();
    cx.editor
        .apply_edits(cx.view, vec![Edit::new(start..start, text.text)]);
    cx.editor.set_cursor(cx.view, cursor);
}

/// `<right>`: moves the cursor a character right, or to the end of the
/// selection, dropping it.
pub(crate) fn right_or_selection_end(cx: &mut CommandContext<'_>) {
    let range = cx.editor.view_ref(cx.view).selection().primary();
    if range.is_empty() {
        return move_right(cx);
    }
    cx.editor.set_cursor(cx.view, range.end());
}

/// `C-a`: selects the whole document, the cursor at its end.
pub(crate) fn select_all(cx: &mut CommandContext<'_>) {
    cx.editor.act_once();
    let len = cx.editor.document_of(cx.view).len_chars();
    let selection = Selection::single(Range::new(0, len));
    cx.editor.view_mut(cx.view).set_selection(selection);
}

/// `C-S-<end>`: extends the selection to the end of the document.
pub(crate) fn select_document_end(cx: &mut CommandContext<'_>) {
    extend_selection(cx, document_end);
}

/// `C-S-<home>`: extends the selection to the start of the document.
pub(crate) fn select_document_start(cx: &mut CommandContext<'_>) {
    extend_selection(cx, document_start);
}

/// `S-<down>`: extends the selection a line down.
pub(crate) fn select_down(cx: &mut CommandContext<'_>) {
    extend_selection(cx, line_down);
}

/// `S-<left>`: extends the selection a character left.
pub(crate) fn select_left(cx: &mut CommandContext<'_>) {
    extend_selection(cx, move_left);
}

/// `S-<end>`: extends the selection to the end of the line.
pub(crate) fn select_line_end(cx: &mut CommandContext<'_>) {
    extend_selection(cx, line_end_motion);
}

/// `S-<home>`: extends the selection to the start of the line.
pub(crate) fn select_line_start(cx: &mut CommandContext<'_>) {
    extend_selection(cx, line_start_motion);
}

/// `S-<right>`: extends the selection a character right.
pub(crate) fn select_right(cx: &mut CommandContext<'_>) {
    extend_selection(cx, move_right);
}

/// `S-<up>`: extends the selection a line up.
pub(crate) fn select_up(cx: &mut CommandContext<'_>) {
    extend_selection(cx, line_up);
}

/// `C-S-<left>`: extends the selection to the start of the word.
pub(crate) fn select_word_backward(cx: &mut CommandContext<'_>) {
    extend_selection(cx, backward_word);
}

/// `C-S-<right>`: extends the selection to the end of the word.
pub(crate) fn select_word_forward(cx: &mut CommandContext<'_>) {
    extend_selection(cx, forward_word);
}

/// `C-/`: comments or uncomments the cursor's line or the selected lines.
pub(crate) fn toggle_comment_lines(cx: &mut CommandContext<'_>) {
    let (first, last) = selected_lines(cx);
    let from_end = cursor_from_line_end(cx);
    toggle_comment(cx.editor, cx.view, first, last);
    keep_place_on_lines(cx, (first, last), from_end);
}

/// The chars from the cursor to the end of its line when nothing is
/// selected, to put it back on the same text after its line changes.
fn cursor_from_line_end(cx: &CommandContext<'_>) -> Option<usize> {
    let range = cx.editor.view_ref(cx.view).selection().primary();
    let document = cx.editor.document_of(cx.view);
    range
        .is_empty()
        .then(|| line_end(document, range.head) - range.head)
}

/// After the lines `first` to `last` changed, puts the cursor back on the
/// same text of its line (`from_end` chars before its end), or selects
/// the lines when they were selected.
fn keep_place_on_lines(
    cx: &mut CommandContext<'_>,
    (first, last): (usize, usize),
    from_end: Option<usize>,
) {
    let document = cx.editor.document_of(cx.view);
    let start = start_of_line(document, first);
    let end = line_end(document, start_of_line(document, last));
    match from_end {
        Some(from_end) => cx.editor.set_cursor(cx.view, end.saturating_sub(from_end)),
        None => {
            let selection = Selection::single(Range::new(start, end));
            cx.editor.view_mut(cx.view).set_selection(selection);
        }
    }
}

/// The span of the lines `first` to `last` of `document` with the line
/// break after the last, when there is one.
fn line_span(document: &Document, first: usize, last: usize) -> ops::Range<usize> {
    let start = start_of_line(document, first);
    let end = match last + 1 < document.len_lines() {
        true => start_of_line(document, last + 1),
        false => document.len_chars(),
    };
    start..end
}

/// Moves the cursor's line or the selected lines past the line below
/// (`down`) or above them, the selection moving with them.
fn move_lines(cx: &mut CommandContext<'_>, down: bool) {
    let (first, last) = selected_lines(cx);
    let document = cx.editor.document_of(cx.view);
    let other = match down {
        true => last + 1,
        false => match first.checked_sub(1) {
            Some(above) => above,
            None => return,
        },
    };
    if other >= document.len_lines() {
        return;
    }
    let block = document
        .slice(start_of_line(document, first)..line_end(document, start_of_line(document, last)));
    let neighbor = document.line_text(other).unwrap_or_default();
    let (span, text) = match down {
        true => (
            start_of_line(document, first)..line_end(document, start_of_line(document, other)),
            format!("{neighbor}{LINE_BREAK}{block}"),
        ),
        false => (
            start_of_line(document, other)..line_end(document, start_of_line(document, last)),
            format!("{block}{LINE_BREAK}{neighbor}"),
        ),
    };
    let moved = neighbor.chars().count() + 1;
    let range = cx.editor.view_ref(cx.view).selection().primary();
    let shift = |offset: usize| match down {
        true => offset + moved,
        false => offset - moved,
    };
    cx.editor.apply_edits(cx.view, vec![Edit::new(span, text)]);
    let selection = Selection::single(Range::new(shift(range.anchor), shift(range.head)));
    cx.editor.view_mut(cx.view).set_selection(selection);
}

/// The first and last lines the selection covers, a selection ending at
/// the start of a line leaving that line out; the cursor's line alone
/// when nothing is selected.
fn selected_lines(cx: &CommandContext<'_>) -> (usize, usize) {
    let range = cx.editor.view_ref(cx.view).selection().primary();
    let document = cx.editor.document_of(cx.view);
    let first = line_of(document, range.start());
    let last = line_of(document, range.end());
    let ends_at_line_start = range.end() == start_of_line(document, last);
    match !range.is_empty() && ends_at_line_start && last > first {
        true => (first, last - 1),
        false => (first, last),
    }
}

/// Indents or outdents the cursor's line or the selected lines one level
/// the way `shift` says, keeping the cursor on its text or the lines
/// selected.
fn shift_selected_lines(cx: &mut CommandContext<'_>, shift: Shift) {
    let (first, last) = selected_lines(cx);
    let from_end = cursor_from_line_end(cx);
    shift_line_range(cx.editor, cx.view, (first, last), 1, shift);
    keep_place_on_lines(cx, (first, last), from_end);
}

/// Runs `motion` from the cursor and selects from where the selection
/// was anchored to where the cursor lands, keeping the goal column the
/// motion left, so Shift with Up and Down keeps its column.
fn extend_selection(cx: &mut CommandContext<'_>, motion: fn(&mut CommandContext<'_>)) {
    cx.editor.act_once();
    let anchor = cx.editor.view_ref(cx.view).selection().primary().anchor;
    motion(cx);
    let state = cx.editor.view_ref(cx.view);
    let (head, goal) = (state.cursor(), state.goal_column());
    let selection = Selection::single(Range::new(anchor, head));
    cx.editor.view_mut(cx.view).set_selection(selection);
    if let Some(goal) = goal {
        cx.editor.set_goal_column(cx.view, goal);
    }
}

// ========================================================================
// Tests
// ========================================================================

#[cfg(test)]
mod tests {
    use crate::options::{EditingStyle, EditorOptions};
    use crate::request::HostRequest;
    use crate::testing::Harness;
    use crate::view::Mode;

    /// A harness editing `marked` in the standard style.
    fn standard(marked: &str) -> Harness {
        let options = EditorOptions {
            editing_style: EditingStyle::Standard,
            ..EditorOptions::default()
        };
        Harness::with_options(marked, options)
    }

    #[test]
    fn test_views_type_at_once_and_escape_stays_in_insert_mode() {
        let mut harness = standard("|one");
        harness.keys("x <esc> y");
        assert_eq!(harness.mode(), Mode::Insert);
        assert_eq!(harness.marked(), "xy|one");
        assert!(harness.requests().is_empty(), "Escape cancels nothing");
    }

    #[test]
    fn test_shift_motions_select_and_typing_replaces_the_selection() {
        let mut harness = standard("|one two");
        harness.keys("C-S-<right> X");
        assert_eq!(harness.marked(), "X| two");
        harness.keys("S-<home> Y");
        assert_eq!(harness.marked(), "Y| two");
        harness.keys("S-<home> S-<end> Z");
        assert_eq!(
            harness.marked(),
            "YZ|",
            "the anchor stays where Shift started"
        );
    }

    #[test]
    fn test_ctrl_shift_k_deletes_the_lines_the_selection_touches() {
        let mut harness = standard("|one\ntwo\nthree");
        harness.keys("S-<down> C-K");
        assert_eq!(
            harness.marked(),
            "|two\nthree",
            "a line it ends at the start of stays"
        );
        harness.keys("S-<right> S-<down> C-K");
        assert_eq!(harness.marked(), "|", "both lines it touches go");
    }

    #[test]
    fn test_left_and_right_drop_the_selection_at_its_near_end() {
        let mut harness = standard("o|ne two");
        harness.keys("S-<right> S-<right> <left>");
        assert_eq!(harness.marked(), "o|ne two");
        harness.keys("S-<right> S-<right> <right>");
        assert_eq!(harness.marked(), "one| two");
    }

    #[test]
    fn test_copy_cut_and_paste_go_through_the_clipboard() {
        let mut harness = standard("|one two");
        harness.keys("S-<right> S-<right> S-<right> C-c <end> C-v");
        assert_eq!(harness.marked(), "one twoone|");
        harness.keys("C-a C-x");
        assert_eq!(harness.marked(), "|");
        harness.keys("C-v");
        assert_eq!(harness.marked(), "one twoone|");
        let clipboard = harness.editor.registers_mut().read(Some('+'));
        assert_eq!(
            clipboard.map(|text| text.text).as_deref(),
            Some("one twoone")
        );
    }

    #[test]
    fn test_undo_takes_back_one_command_or_run_of_typing_at_a_time() {
        let mut harness = standard("|");
        harness.keys("one SPC two <bs> <bs> o <left> X C-z");
        assert_eq!(harness.marked(), "one t|o");
        harness.keys("C-z");
        assert_eq!(harness.marked(), "one two|");
        harness.keys("C-y");
        assert_eq!(harness.marked(), "one to|");
    }

    #[test]
    fn test_save_asks_the_host_and_close_refuses_to_lose_changes() {
        let mut harness = standard("|one");
        harness.keys("x C-s C-q");
        let requests = harness.requests();
        assert!(
            matches!(
                requests.as_slice(),
                [HostRequest::Save(_), HostRequest::Notice(_)]
            ),
            "{requests:?}"
        );
        harness.keys("C-z C-q");
        let requests = harness.requests();
        assert!(
            matches!(requests.as_slice(), [HostRequest::Quit(quit)] if !quit.force),
            "{requests:?}"
        );
    }

    #[test]
    fn test_each_view_breaks_its_own_undo_steps() {
        let mut harness = standard("|");
        let first = harness.view;
        let document = harness.editor.open_document("");
        let second = harness
            .editor
            .new_view(document)
            .expect("the document is open");
        harness.view = second;
        harness.keys("ab");
        harness.view = first;
        harness.keys("x <bs>");
        harness.view = second;
        harness.keys("<bs> C-z");
        assert_eq!(
            harness.marked().replace('|', ""),
            "ab",
            "only the Backspace undone"
        );
    }

    #[test]
    fn test_ctrl_d_adds_a_cursor_at_the_next_match_and_escape_drops_it() {
        let mut harness = standard("|ab x ab");
        harness.keys("C-d C-d X");
        assert_eq!(harness.marked(), "X|ab x Xab");
        harness.keys("<esc> Y");
        assert_eq!(harness.marked(), "XY|ab x Xab", "one cursor again");
    }

    #[test]
    fn test_alt_up_and_down_move_the_lines() {
        let mut harness = standard("one\nt|wo\nthree");
        harness.keys("M-<up>");
        assert_eq!(harness.marked(), "t|wo\none\nthree");
        harness.keys("M-<down> M-<down>");
        assert_eq!(harness.marked(), "one\nthree\nt|wo");
    }

    #[test]
    fn test_ctrl_slash_toggles_a_comment_and_tab_indents_the_selected_lines() {
        let mut harness = standard("a|b\ncd");
        let document = harness.editor.view(harness.view).expect("open").document();
        harness.editor.set_comment_token(document, Some("//"));
        harness.keys("C-/");
        assert_eq!(harness.marked(), "// a|b\ncd");
        harness.keys("C-/");
        assert_eq!(harness.marked(), "a|b\ncd");
        harness.keys("S-<down> <tab>");
        assert_eq!(harness.marked().replace('|', ""), "\tab\n\tcd");
        harness.keys("S-<tab> S-<tab>");
        assert_eq!(harness.marked().replace('|', ""), "ab\ncd");
        harness.keys("<esc> C-<home> <tab>");
        assert_eq!(
            harness.marked(),
            "\t|ab\ncd",
            "with nothing selected, a tab"
        );
    }

    #[test]
    fn test_copy_and_cut_with_nothing_selected_take_the_line() {
        let mut harness = standard("o|ne\ntwo");
        harness.keys("C-c <down> C-v");
        assert_eq!(
            harness.marked(),
            "one\none\nt|wo",
            "pasted above, the cursor kept"
        );
        harness.keys("C-x");
        assert_eq!(harness.marked(), "one\none\n|");
    }

    #[test]
    fn test_ctrl_f_finds_and_ctrl_h_replaces_asking_at_each_match() {
        let mut harness = standard("|a b a b a");
        harness.keys("C-f b <cr>");
        assert_eq!(harness.mode(), Mode::Insert);
        assert_eq!(harness.marked(), "a |b a b a");
        harness.keys("C-<home> C-h a <cr> X <cr> y n y");
        assert_eq!(harness.mode(), Mode::Insert);
        assert_eq!(harness.marked().replace('|', ""), "X b a b X");
    }

    #[test]
    fn test_escape_drops_the_selection() {
        let mut harness = standard("|one");
        harness.keys("S-<end> <esc> X");
        assert_eq!(harness.marked(), "oneX|");
    }

    #[test]
    fn test_switching_the_style_puts_open_views_in_insert_mode() {
        let mut harness = Harness::new("|one");
        let options = EditorOptions {
            editing_style: EditingStyle::Standard,
            ..EditorOptions::default()
        };
        harness.editor.set_options(options);
        harness.keys("C-a X");
        assert_eq!(harness.mode(), Mode::Insert);
        assert_eq!(harness.marked(), "X|");
    }
}
