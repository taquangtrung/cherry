//! Scrolling and the motions that go by what the screen shows.
//!
//! - `C-e` and `C-y` scroll a row per count down or up, the cursor moving
//!   only as far as it must to stay in sight with the scroll margin, as the
//!   mouse wheel does.
//! - `C-d` and `C-u` scroll half the view's rows and move the cursor as
//!   many lines, keeping its column as `j` and `k` do; on the last or
//!   first line they do nothing. A count before either becomes the rows
//!   both scroll from then on in that view (cut to the view's height), as
//!   Vim's `scroll` option does, until the view's number of rows changes.
//! - `C-f` and `C-b` (and `<pagedown>`, `<pageup>`, also in Insert mode)
//!   scroll a page per count, two rows less, so the rows at the edge stay
//!   in sight; the cursor moves into sight. When the view cannot scroll
//!   further, the cursor goes to the last or first line.
//! - `zt`, `zz`, and `zb` scroll the cursor's row to the top, middle, or
//!   bottom of the view (the scroll margin kept), the cursor staying put;
//!   `z<cr>`, `z.`, and `z-` do the same and move to the first non-blank.
//!   With a count, each first goes to the line the count names.
//! - `H`, `M`, and `L` go to the first non-blank of the line at the top,
//!   middle, or bottom of the view (`H` and `L` the count's line from
//!   their edge), kept inside the scroll margin unless the document's
//!   start or end is in sight. Linewise jumps.
//! - `gj` and `gk` move a screen row down or up, which differs from `j`
//!   and `k` on soft-wrapped lines; exclusive.

use super::command::CommandContext;
use super::document::{last_line, line_count, line_end, line_of, start_of_line};
use super::editor::{Editor, ViewId};
use super::mark::jump_to;
use super::motion::{
    fail, finish, first_non_blank_of_line, last_cursor_offset, line_down, line_up, move_lines,
};
use super::operator::MotionKind;
use super::view::{CursorPlace, RowPlace};

// ========================================================================
// Constants
// ========================================================================

/// The rows of the old page `C-f` and `C-b` keep in sight.
const PAGE_OVERLAP: usize = 2;

// ========================================================================
// Data Structures
// ========================================================================

/// Which row of the view `H`, `M`, and `L` go to.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ScreenEdge {
    /// The bottom: `L`.
    Bottom,
    /// The middle: `M`.
    Middle,
    /// The top: `H`.
    Top,
}

// ========================================================================
// Functions
// ========================================================================

/// `L`: the line at the bottom of the view, or the count's line up from it.
pub(crate) fn screen_bottom(cx: &mut CommandContext<'_>) {
    goto_screen_line(cx, ScreenEdge::Bottom);
}

/// `M`: the line in the middle of the rows the view shows.
pub(crate) fn screen_middle(cx: &mut CommandContext<'_>) {
    goto_screen_line(cx, ScreenEdge::Middle);
}

/// `H`: the line at the top of the view, or the count's line down from it.
pub(crate) fn screen_top(cx: &mut CommandContext<'_>) {
    goto_screen_line(cx, ScreenEdge::Top);
}

/// `C-b` and `<pageup>`: scrolls a page up per count.
pub(crate) fn page_up(cx: &mut CommandContext<'_>) {
    scroll_page(cx, false);
}

/// `C-f` and `<pagedown>`: scrolls a page down per count.
pub(crate) fn page_down(cx: &mut CommandContext<'_>) {
    scroll_page(cx, true);
}

/// `C-d`: scrolls half a view down and moves the cursor as many lines.
pub(crate) fn half_page_down(cx: &mut CommandContext<'_>) {
    scroll_half_page(cx, true);
}

/// `C-u`: scrolls half a view up and moves the cursor as many lines.
pub(crate) fn half_page_up(cx: &mut CommandContext<'_>) {
    scroll_half_page(cx, false);
}

/// `C-e`: scrolls a row down per count.
pub(crate) fn scroll_down(cx: &mut CommandContext<'_>) {
    scroll_lines(cx, true);
}

/// `C-y`: scrolls a row up per count.
pub(crate) fn scroll_up(cx: &mut CommandContext<'_>) {
    scroll_lines(cx, false);
}

/// `zb`: the cursor's row to the bottom of the view.
pub(crate) fn scroll_cursor_bottom(cx: &mut CommandContext<'_>) {
    place_cursor_row(cx, CursorPlace::Bottom, false);
}

/// `z-`: the cursor's row to the bottom, onto its first non-blank.
pub(crate) fn scroll_cursor_bottom_line_start(cx: &mut CommandContext<'_>) {
    place_cursor_row(cx, CursorPlace::Bottom, true);
}

/// `zz`: the cursor's row to the middle of the view.
pub(crate) fn scroll_cursor_center(cx: &mut CommandContext<'_>) {
    place_cursor_row(cx, CursorPlace::Center, false);
}

/// `z.`: the cursor's row to the middle, onto its first non-blank.
pub(crate) fn scroll_cursor_center_line_start(cx: &mut CommandContext<'_>) {
    place_cursor_row(cx, CursorPlace::Center, true);
}

/// `zt`: the cursor's row to the top of the view.
pub(crate) fn scroll_cursor_top(cx: &mut CommandContext<'_>) {
    place_cursor_row(cx, CursorPlace::Top, false);
}

/// `z<cr>`: the cursor's row to the top, onto its first non-blank.
pub(crate) fn scroll_cursor_top_line_start(cx: &mut CommandContext<'_>) {
    place_cursor_row(cx, CursorPlace::Top, true);
}

/// `C-l`: the cursor's row to the middle of the view; run again right
/// after, to the top, then the bottom, and round, as Emacs's
/// `recenter-top-bottom` does.
pub(crate) fn recenter_top_bottom(cx: &mut CommandContext<'_>) {
    let seq = cx.editor.command_seq();
    let place = match cx.editor.view_ref(cx.view).recentered() {
        Some((last, place)) if last + 1 == seq => match place {
            CursorPlace::Bottom => CursorPlace::Center,
            CursorPlace::Center => CursorPlace::Top,
            CursorPlace::Top => CursorPlace::Bottom,
        },
        Some(_) | None => CursorPlace::Center,
    };
    cx.editor.scroll_cursor_to(cx.view, place);
    cx.editor.view_mut(cx.view).set_recentered(seq, place);
}

/// `g0`: the first char in sight on the cursor's screen row.
pub(crate) fn row_start(cx: &mut CommandContext<'_>) {
    let cursor = cx.editor.view_ref(cx.view).cursor();
    let target = cx
        .editor
        .offset_at_row_place(cx.view, cursor, RowPlace::Start);
    finish(cx, MotionKind::Exclusive, target);
}

/// `g^`: the first non-blank in sight on the cursor's screen row.
pub(crate) fn row_first_non_blank(cx: &mut CommandContext<'_>) {
    let cursor = cx.editor.view_ref(cx.view).cursor();
    let mut target = cx
        .editor
        .offset_at_row_place(cx.view, cursor, RowPlace::Start);
    let end = row_last_char(cx, cursor);
    let document = cx.editor.document_of(cx.view);
    while target < end && matches!(document.char_at(target), Some(' ' | '\t')) {
        target += 1;
    }
    finish(cx, MotionKind::Exclusive, target);
}

/// `gm`: the char half the view's width along the cursor's screen row, or
/// the row's last when it is shorter.
pub(crate) fn row_middle(cx: &mut CommandContext<'_>) {
    let cursor = cx.editor.view_ref(cx.view).cursor();
    let middle = cx
        .editor
        .offset_at_row_place(cx.view, cursor, RowPlace::Middle);
    let target = middle.min(row_last_char(cx, cursor));
    finish(cx, MotionKind::Exclusive, target);
}

/// `g$`: the last char in sight on the cursor's screen row, inclusive
/// after an operator.
pub(crate) fn row_end(cx: &mut CommandContext<'_>) {
    let cursor = cx.editor.view_ref(cx.view).cursor();
    let target = row_last_char(cx, cursor);
    finish(cx, MotionKind::Inclusive, target);
}

/// The last char in sight on the screen row of `offset`: not the line's
/// end past its last char, but the line's start on an empty line.
fn row_last_char(cx: &CommandContext<'_>, offset: usize) -> usize {
    let end = cx
        .editor
        .offset_at_row_place(cx.view, offset, RowPlace::End);
    let document = cx.editor.document_of(cx.view);
    let line_start = start_of_line(document, line_of(document, offset));
    match end == line_end(document, offset) {
        true => document.prev_grapheme_boundary(end).max(line_start),
        false => end,
    }
}

/// `gj`: a screen row down per count, or a line where `j` and `k` go by
/// rows.
pub(crate) fn screen_row_down(cx: &mut CommandContext<'_>) {
    match swaps_rows_and_lines(cx) {
        true => line_down(cx),
        false => move_screen_rows(cx, true),
    }
}

/// `gk`: a screen row up per count, or a line where `j` and `k` go by
/// rows.
pub(crate) fn screen_row_up(cx: &mut CommandContext<'_>) {
    match swaps_rows_and_lines(cx) {
        true => line_up(cx),
        false => move_screen_rows(cx, false),
    }
}

/// Whether `j` and `k` go by screen rows and `gj` and `gk` by lines: with
/// [`EditorOptions::screen_row_motions`](super::options::EditorOptions::screen_row_motions)
/// in a view that wraps, unless an operator waits, which `j` and `k` give
/// whole lines.
pub(crate) fn swaps_rows_and_lines(cx: &CommandContext<'_>) -> bool {
    let options = cx.editor.layout_options(cx.view);
    options.screen_row_motions && options.wrap && cx.editor.view_ref(cx.view).operator().is_none()
}

/// Goes to the first non-blank of the line on the row of `edge`.
fn goto_screen_line(cx: &mut CommandContext<'_>, edge: ScreenEdge) {
    let screen = cx.editor.screen_lines(cx.view);
    let last = screen.lines.len() - 1;
    let rows = usize::from(cx.editor.view_ref(cx.view).rows()).max(1);
    let margin = cx
        .editor
        .layout_options(cx.view)
        .scroll_margin
        .min((rows - 1) / 2);
    let from_edge = cx.count_or_one() - 1;
    let margin = match cx.editor.layout_options(cx.view).edge_row_motions {
        true => 0,
        false => margin,
    };
    let row = match edge {
        ScreenEdge::Bottom => {
            let kept = if screen.shows_end { 0 } else { margin };
            last.saturating_sub(from_edge.max(kept))
        }
        ScreenEdge::Middle => last / 2,
        ScreenEdge::Top => {
            let kept = if screen.shows_start { 0 } else { margin };
            from_edge.max(kept).min(last)
        }
    };
    let document = cx.editor.document_of(cx.view);
    let start = start_of_line(document, screen.lines[row]);
    let target = first_non_blank_of_line(cx.editor, cx.view, start);
    jump_to(cx, MotionKind::Linewise, target);
    if cx.editor.layout_options(cx.view).edge_row_motions {
        cx.editor.view_mut(cx.view).waive_margin_at(target);
    }
}

/// Moves the cursor a screen row per count, down or up.
pub(crate) fn move_screen_rows(cx: &mut CommandContext<'_>, down: bool) {
    let cursor = cx.editor.view_ref(cx.view).cursor();
    let Some(offset) = cx
        .editor
        .offset_rows_away(cx.view, cursor, down, cx.count_or_one())
    else {
        return fail(cx);
    };
    let target = offset.min(last_cursor_offset(cx.editor, cx.view, offset));
    finish(cx, MotionKind::Exclusive, target);
}

/// Moves the cursor to `offset`, kept off the line end outside Insert mode.
fn place_cursor(editor: &mut Editor, view: ViewId, offset: usize) {
    let target = offset.min(last_cursor_offset(editor, view, offset));
    editor.set_cursor(view, target);
}

/// Scrolls the cursor's row to `place`, after going to the count's line,
/// onto its first non-blank when `to_line_start`.
fn place_cursor_row(cx: &mut CommandContext<'_>, place: CursorPlace, to_line_start: bool) {
    let document = cx.editor.document_of(cx.view);
    let cursor = cx.editor.view_ref(cx.view).cursor();
    let line = match cx.count {
        Some(count) => (count - 1).min(last_line(document)),
        None => line_of(document, cursor),
    };
    let start = start_of_line(document, line);
    if to_line_start {
        let target = first_non_blank_of_line(cx.editor, cx.view, start);
        cx.editor.set_cursor(cx.view, target);
    } else if cx.count.is_some() {
        let column = cursor - start_of_line(document, line_of(document, cursor));
        place_cursor(cx.editor, cx.view, start + column);
    }
    cx.editor.scroll_cursor_to(cx.view, place);
}

/// Scrolls the view's half-page rows down or up and moves the cursor as
/// many lines, a count first becoming those rows; does nothing on the last
/// or first line.
fn scroll_half_page(cx: &mut CommandContext<'_>, down: bool) {
    if let Some(count) = cx.count {
        cx.editor.view_mut(cx.view).set_half_page(count);
    }
    let amount = cx.editor.view_ref(cx.view).half_page();
    let document = cx.editor.document_of(cx.view);
    let cursor = cx.editor.view_ref(cx.view).cursor();
    let line = line_of(document, cursor);
    let at_edge = if down {
        line + 1 >= line_count(document)
    } else {
        line == 0
    };
    if at_edge {
        return;
    }
    cx.editor.scroll_view_rows(cx.view, down, amount);
    if down {
        move_lines(cx, |line, last| line.saturating_add(amount).min(last));
    } else {
        move_lines(cx, |line, _| line.saturating_sub(amount));
    }
}

/// Scrolls a row per count, down or up, the cursor kept in sight.
fn scroll_lines(cx: &mut CommandContext<'_>, down: bool) {
    if let Some(offset) = cx.editor.scroll_view_rows(cx.view, down, cx.count_or_one()) {
        place_cursor(cx.editor, cx.view, offset);
    }
}

/// Scrolls a page per count, down or up; when the view cannot scroll, the
/// cursor goes to the last or first line instead.
fn scroll_page(cx: &mut CommandContext<'_>, down: bool) {
    let rows = usize::from(cx.editor.view_ref(cx.view).rows());
    let page = rows.saturating_sub(PAGE_OVERLAP).max(1);
    let amount = page.saturating_mul(cx.count_or_one());
    let before = cx.editor.view_ref(cx.view).scroll();
    let target = cx.editor.scroll_view_rows(cx.view, down, amount);
    if cx.editor.view_ref(cx.view).scroll() == before {
        let document = cx.editor.document_of(cx.view);
        let offset = if down { document.len_chars() } else { 0 };
        let line_start = start_of_line(document, line_of(document, offset));
        let target = first_non_blank_of_line(cx.editor, cx.view, line_start);
        return cx.editor.set_cursor(cx.view, target);
    }
    if let Some(offset) = target {
        place_cursor(cx.editor, cx.view, offset);
    }
}

// ========================================================================
// Tests
// ========================================================================

#[cfg(test)]
mod tests {
    use crate::options::EditorOptions;
    use crate::testing::Harness;

    /// A harness over `count` lines `l0`, `l1`, ..., the cursor on the
    /// first, in a view 20 by `rows` with a scroll margin of `margin`.
    fn numbered(count: usize, rows: u16, margin: usize) -> Harness {
        let text: Vec<_> = (0..count).map(|n| format!("l{n}")).collect();
        let options = EditorOptions {
            scroll_margin: margin,
            ..EditorOptions::default()
        };
        let mut harness = Harness::with_options(&format!("|{}", text.join("\n")), options);
        harness.editor.resize_view(harness.view, 20, rows);
        harness
    }

    /// The top line of the view and the cursor's line.
    fn top_and_line(harness: &Harness) -> (usize, usize) {
        let view = harness.editor.view(harness.view).unwrap();
        let document = harness.editor.document_of(harness.view);
        let line = document.char_to_position(view.cursor()).unwrap().line;
        (view.scroll().line, line)
    }

    #[test]
    fn test_ctrl_e_and_ctrl_y_scroll_rows_dragging_the_cursor_into_sight() {
        let mut harness = numbered(30, 10, 2);
        harness.keys("C-e");
        assert_eq!(top_and_line(&harness), (1, 3));
        harness.keys("5 C-e");
        assert_eq!(top_and_line(&harness), (6, 8));
        harness.keys("C-y");
        assert_eq!(top_and_line(&harness), (5, 8), "the cursor stays in sight");
        harness.keys("9 C-y");
        assert_eq!(top_and_line(&harness), (0, 7));
    }

    #[test]
    fn test_ctrl_d_and_ctrl_u_scroll_half_a_view_and_move_the_cursor_as_far() {
        let mut harness = numbered(30, 10, 0);
        harness.keys("j C-d");
        assert_eq!(top_and_line(&harness), (5, 6));
        harness.keys("C-u");
        assert_eq!(top_and_line(&harness), (0, 1));
        harness.keys("3 C-d");
        assert_eq!(top_and_line(&harness), (3, 4));
        harness.keys("G C-d");
        assert_eq!(
            top_and_line(&harness),
            (20, 29),
            "nothing below the last line"
        );
        harness.keys("g g C-u");
        assert_eq!(top_and_line(&harness), (0, 0));
    }

    #[test]
    fn test_a_count_before_ctrl_d_sets_the_amount_plain_ctrl_d_and_ctrl_u_reuse() {
        let mut harness = numbered(40, 10, 0);
        harness.keys("3 C-d");
        assert_eq!(top_and_line(&harness), (3, 3));
        harness.keys("C-d");
        assert_eq!(top_and_line(&harness), (6, 6), "C-d reuses the count");
        harness.keys("C-u");
        assert_eq!(top_and_line(&harness), (3, 3), "so does C-u");
        harness.keys("2 C-u C-d");
        assert_eq!(top_and_line(&harness), (3, 3), "a count on C-u sets it too");
    }

    #[test]
    fn test_the_half_page_amount_is_per_view() {
        let mut harness = numbered(40, 10, 0);
        harness.keys("3 C-d");
        let document = harness.editor.view(harness.view).unwrap().document();
        let other = harness.editor.new_view(document).unwrap();
        harness.editor.resize_view(other, 20, 10);
        harness
            .editor
            .run_command(other, "half-page-down", None)
            .unwrap();
        let top = harness.editor.view(other).unwrap().scroll().line;
        assert_eq!(top, 5, "the other view scrolls half its rows");
    }

    #[test]
    fn test_the_half_page_amount_stays_when_the_view_shows_another_document() {
        let mut harness = numbered(40, 10, 0);
        harness.keys("3 C-d");
        let text: Vec<_> = (0..40).map(|n| format!("m{n}")).collect();
        let other = harness.editor.open_document(&text.join("\n"));
        assert!(harness.editor.show_document(harness.view, other));
        harness.keys("C-d");
        assert_eq!(top_and_line(&harness), (3, 3));
    }

    #[test]
    fn test_resizing_the_rows_resets_the_half_page_amount() {
        let mut harness = numbered(40, 10, 0);
        harness.keys("3 C-d");
        harness.editor.resize_view(harness.view, 30, 10);
        harness.keys("C-d");
        assert_eq!(top_and_line(&harness), (6, 6), "same rows: the count stays");
        harness.editor.resize_view(harness.view, 30, 8);
        harness.keys("C-d");
        assert_eq!(top_and_line(&harness), (10, 10), "new rows: half the view");
    }

    #[test]
    fn test_a_count_larger_than_the_view_is_cut_to_the_view_height() {
        let mut harness = numbered(60, 10, 0);
        harness.keys("25 C-d");
        assert_eq!(top_and_line(&harness), (10, 10));
        harness.keys("C-d");
        assert_eq!(top_and_line(&harness), (20, 20));
    }

    #[test]
    fn test_a_stored_amount_keeps_the_edge_and_column_rules() {
        let mut harness = numbered(12, 10, 0);
        harness.keys("$ 3 C-d");
        assert_eq!(top_and_line(&harness), (2, 3));
        harness.keys("C-d");
        assert_eq!(
            top_and_line(&harness),
            (2, 6),
            "the cursor moves the amount"
        );
        harness.keys("C-d C-d");
        assert_eq!(top_and_line(&harness), (2, 11), "stops at the last line");
        assert_eq!(harness.marked().lines().last(), Some("l1|1"));
        harness.keys("C-d");
        assert_eq!(
            top_and_line(&harness),
            (2, 11),
            "nothing below the last line"
        );
    }

    #[test]
    fn test_ctrl_f_and_ctrl_b_page_with_two_rows_kept_and_stop_at_the_ends() {
        let mut harness = numbered(30, 10, 0);
        harness.keys("C-f");
        assert_eq!(top_and_line(&harness), (8, 8));
        harness.keys("<pagedown>");
        assert_eq!(top_and_line(&harness), (16, 16));
        harness.keys("C-f");
        assert_eq!(top_and_line(&harness), (20, 20), "the last page");
        harness.keys("C-f");
        assert_eq!(top_and_line(&harness), (20, 29), "then the last line");
        harness.keys("C-b");
        assert_eq!(top_and_line(&harness), (12, 21));
        harness.keys("9 <pageup>");
        assert_eq!(top_and_line(&harness), (0, 9));
        harness.keys("C-b");
        assert_eq!(top_and_line(&harness), (0, 0));
    }

    #[test]
    fn test_z_commands_scroll_the_cursor_row_to_the_top_middle_and_bottom() {
        let mut harness = numbered(30, 10, 1);
        harness.keys("15G z t");
        assert_eq!(top_and_line(&harness), (13, 14));
        harness.keys("z z");
        assert_eq!(top_and_line(&harness), (10, 14));
        harness.keys("z b");
        assert_eq!(top_and_line(&harness), (6, 14));
        harness.keys("20 z <cr>");
        assert_eq!(top_and_line(&harness), (18, 19));
        harness.keys("5 z .");
        assert_eq!(top_and_line(&harness), (0, 4), "no rows above the first");
    }

    #[test]
    fn test_z_dot_moves_to_the_first_non_blank_and_zz_keeps_the_column() {
        let mut harness = Harness::new("  ab|cd\nx");
        harness.keys("z z");
        assert_eq!(harness.marked(), "  ab|cd\nx");
        harness.keys("z .");
        assert_eq!(harness.marked(), "  |abcd\nx");
    }

    #[test]
    fn test_h_m_and_l_go_to_screen_lines_inside_the_margin() {
        let mut harness = numbered(30, 10, 2);
        harness.keys("L");
        assert_eq!(top_and_line(&harness), (0, 7));
        harness.keys("H");
        assert_eq!(top_and_line(&harness), (0, 0), "the start is in sight");
        harness.keys("10 C-e");
        harness.keys("H");
        assert_eq!(top_and_line(&harness), (10, 12));
        harness.keys("M");
        assert_eq!(top_and_line(&harness), (10, 14));
        harness.keys("4H");
        assert_eq!(top_and_line(&harness), (10, 13));
        harness.keys("G L");
        assert_eq!(top_and_line(&harness), (20, 29), "the end is in sight");
        harness.keys("3L");
        assert_eq!(top_and_line(&harness), (20, 27));
    }

    #[test]
    fn test_c_l_puts_the_row_in_the_middle_then_top_then_bottom_while_run_in_a_row() {
        let mut harness = numbered(30, 10, 2);
        harness.keys("16 G i");
        let mut tops = Vec::new();
        for _ in 0..4 {
            harness.keys("C-l");
            tops.push(top_and_line(&harness).0);
        }
        assert_eq!(tops, [11, 13, 8, 11], "middle, top, bottom, middle");
        harness.keys("C-l x C-l");
        assert_eq!(top_and_line(&harness).0, 11, "typing starts it over");
    }

    #[test]
    fn test_g0_g_caret_gm_and_g_dollar_go_along_the_screen_row() {
        let options = EditorOptions {
            wrap: true,
            ..EditorOptions::default()
        };
        let mut harness = Harness::with_options("ab  cdef|ghij  kl\nxy", options);
        harness.editor.resize_view(harness.view, 6, 5);
        harness.keys("g 0");
        assert_eq!(harness.marked(), "ab  cd|efghij  kl\nxy", "the row's start");
        harness.keys("g $");
        assert_eq!(harness.marked(), "ab  cdefghi|j  kl\nxy", "the row's end");
        harness.keys("g m");
        assert_eq!(harness.marked(), "ab  cdefg|hij  kl\nxy", "three cells in");
        harness.keys("j g m");
        assert_eq!(
            harness.marked(),
            "ab  cdefghij  kl\nx|y",
            "a short row's last"
        );
        harness.keys("g g g ^");
        assert_eq!(harness.marked(), "|ab  cdefghij  kl\nxy");
        harness.keys("g j g j g ^");
        assert_eq!(
            harness.marked(),
            "ab  cdefghij  |kl\nxy",
            "past the row's blanks"
        );
        harness.keys("g g w d g $");
        assert_eq!(
            harness.marked(),
            "ab  |efghij  kl\nxy",
            "inclusive of the d"
        );
    }

    #[test]
    fn test_edge_row_motions_go_to_the_very_top_and_bottom_and_leave_the_view() {
        let mut harness = numbered(30, 10, 2);
        let options = EditorOptions {
            edge_row_motions: true,
            scroll_margin: 2,
            ..EditorOptions::default()
        };
        harness.editor.set_options(options);
        harness.keys("10 C-e H");
        assert_eq!(top_and_line(&harness), (10, 10), "the top row, unscrolled");
        harness.keys("L");
        assert_eq!(top_and_line(&harness), (10, 19), "the bottom row");
        harness.keys("2 H");
        assert_eq!(
            top_and_line(&harness),
            (10, 11),
            "a count's row from the top"
        );
        harness.keys("k");
        assert_eq!(
            top_and_line(&harness),
            (8, 10),
            "moving on keeps the margin"
        );
    }

    #[test]
    fn test_d_capital_l_deletes_the_lines_to_the_bottom_of_the_view() {
        let mut harness = numbered(30, 10, 0);
        harness.keys("d L");
        assert_eq!(harness.marked().lines().next(), Some("|l10"));
    }

    #[test]
    fn test_gj_and_gk_move_by_screen_rows_on_wrapped_lines() {
        let options = EditorOptions {
            wrap: true,
            ..EditorOptions::default()
        };
        let mut harness = Harness::with_options("a|bcdefgh\nxy", options);
        harness.editor.resize_view(harness.view, 4, 5);
        harness.keys("g j");
        assert_eq!(harness.marked(), "abcde|fgh\nxy");
        harness.keys("g j");
        assert_eq!(harness.marked(), "abcdefgh\nx|y");
        harness.keys("2 g k");
        assert_eq!(harness.marked(), "a|bcdefgh\nxy");
        harness.keys("j");
        assert_eq!(harness.marked(), "abcdefgh\nx|y", "j goes by lines");
    }

    #[test]
    fn test_screen_row_motions_swap_j_and_k_with_gj_and_gk_on_wrapped_lines() {
        let options = EditorOptions {
            screen_row_motions: true,
            wrap: true,
            ..EditorOptions::default()
        };
        let mut harness = Harness::with_options("a|bcdefgh\nxy\nz", options);
        harness.editor.resize_view(harness.view, 4, 5);
        harness.keys("j");
        assert_eq!(harness.marked(), "abcde|fgh\nxy\nz", "a row");
        harness.keys("k g j");
        assert_eq!(harness.marked(), "abcdefgh\nx|y\nz", "a line");
        harness.keys("g g d j");
        assert_eq!(
            harness.marked(),
            "|z",
            "an operator still takes whole lines"
        );
        let no_wrap = EditorOptions {
            wrap: false,
            ..options
        };
        let mut harness = Harness::with_options("a|bcdefgh\nxy", no_wrap);
        harness.editor.resize_view(harness.view, 4, 5);
        harness.keys("j");
        assert_eq!(harness.marked(), "abcdefgh\nx|y", "no rows to go by");
    }
}
