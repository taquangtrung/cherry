//! Visual mode: a selection made with the motions, then acted on.
//!
//! `v` starts selecting characters, `V` whole lines, and `C-v` a block
//! (see [`super::block`]), from the cursor; every motion then moves the
//! cursor and the selection follows it, both ends included. `o` swaps the cursor to the other end. `v` or `V` typed
//! again in its own mode goes back to Normal mode, in the other mode
//! switches to it; `<esc>` goes back to Normal mode. `gv` selects the last
//! selection again, carried along by edits made since.
//!
//! On the selection: `d` and `x` delete, `c` changes, `y` yanks (each as
//! its operator would, by register), `>` and `<` indent and outdent the
//! selected lines by [`EditorOptions::indent_width`] per count, and `p`
//! replaces the selection with a register's text, the replaced text going
//! to the registers as a delete would (`P` leaves the registers alone).

use std::iter;
use std::ops::Range;

use super::block::{
    block_of, cluster_count, replace_block, replace_in_block, run_block_operator,
    swap_block_corner, view_block,
};
use super::command::{CommandContext, await_char};
use super::document::{Document, line_end, line_of, start_of_line};
use super::editor::{Editor, ViewId};
use super::motion::first_non_blank_of_line;
use super::operator::{MotionKind, Operator};
use super::options::EditorOptions;
use super::register::{RegisterKind, RegisterText};
use super::transaction::Edit;
use super::view::{Mode, View, is_visual};

// ========================================================================
// Constants
// ========================================================================

/// The line break between lines.
const LINE_BREAK: &str = "\n";

/// The line break between lines, as a char.
const LINE_BREAK_CHAR: char = '\n';

// ========================================================================
// Data Structures
// ========================================================================

/// Which way `>` and `<` move lines.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Shift {
    /// Less indent: `<`.
    Left,
    /// More indent: `>`.
    Right,
}

// ========================================================================
// Functions
// ========================================================================

/// `<esc>`: back to Normal mode, the selection dropped.
pub(crate) fn exit_visual(cx: &mut CommandContext<'_>) {
    cx.editor.set_mode(cx.view, Mode::Normal);
}

/// `>`: indents the selected lines one level per count.
pub(crate) fn indent(cx: &mut CommandContext<'_>) {
    shift_lines(cx, Shift::Right);
}

/// `<`: outdents the selected lines one level per count.
pub(crate) fn outdent(cx: &mut CommandContext<'_>) {
    shift_lines(cx, Shift::Left);
}

/// `p` and `P` in a visual mode: replaces the selection with the text of
/// the command's register, count times; the replaced text goes to the
/// registers as a delete with no register would, unless `keep_registers`.
pub(crate) fn replace_selection(cx: &mut CommandContext<'_>, keep_registers: bool) {
    let Some((anchor, kind)) = selection(cx.editor, cx.view) else {
        return;
    };
    let Some(text) = cx.editor.read_register(cx.view, cx.register) else {
        let name = cx.register.unwrap_or('"');
        cx.editor.error(format!("Nothing in register {name}"));
        return;
    };
    if let Some(block) = block_of(cx.editor, cx.view) {
        return replace_block(cx, block, &text, keep_registers);
    }
    let repeated = text.text.repeat(cx.count_or_one());
    let document = cx.editor.document_of(cx.view);
    let cursor = cx.editor.view_ref(cx.view).cursor();
    let (start, end) = (anchor.min(cursor), anchor.max(cursor));
    let (first, last) = (line_of(document, start), line_of(document, end));
    let replaced = selected_text(document, start, end, kind);
    let (range, inserted, put_line) = match (kind, text.kind) {
        (MotionKind::Linewise, RegisterKind::Linewise) => {
            let range_start = start_of_line(document, first);
            if last + 1 < document.len_lines() {
                let range = range_start..start_of_line(document, last + 1);
                (range, repeated, Some(first))
            } else {
                let body = repeated.strip_suffix(LINE_BREAK).unwrap_or(&repeated);
                let range = range_start..document.len_chars();
                (range, body.to_string(), Some(first))
            }
        }
        (MotionKind::Linewise, RegisterKind::Blockwise | RegisterKind::Charwise) => {
            let range = start_of_line(document, first)..line_end(document, end);
            (range, repeated, None)
        }
        (MotionKind::Exclusive | MotionKind::Inclusive, RegisterKind::Linewise) => {
            let range = start..document.next_grapheme_boundary(end);
            (range, format!("{LINE_BREAK}{repeated}"), Some(first + 1))
        }
        (
            MotionKind::Exclusive | MotionKind::Inclusive,
            RegisterKind::Blockwise | RegisterKind::Charwise,
        ) => {
            let range = start..document.next_grapheme_boundary(end);
            (range, repeated, None)
        }
    };
    cx.editor.set_mode(cx.view, Mode::Normal);
    if cx.editor.refuse_if_read_only(cx.view) {
        return;
    }
    if !keep_registers {
        cx.editor.registers_mut().record_delete(None, replaced);
    }
    let at = range.start;
    let len = inserted.chars().count();
    let is_multi_line = inserted.contains(LINE_BREAK);
    cx.editor
        .apply_edits(cx.view, vec![Edit::new(range, inserted)]);
    let document = cx.editor.document_of(cx.view);
    let target = match put_line {
        Some(line) => {
            let line_start = start_of_line(document, line);
            first_non_blank_of_line(cx.editor, cx.view, line_start)
        }
        None if is_multi_line || len == 0 => at,
        None => document.prev_grapheme_boundary(at + len),
    };
    cx.editor.set_cursor(cx.view, target);
}

/// `gv`: selects the last visual selection again, in the mode it was made
/// in. Does nothing when there was none.
pub(crate) fn reselect_visual(cx: &mut CommandContext<'_>) {
    let Some(area) = cx.editor.view_ref(cx.view).last_visual() else {
        return;
    };
    let len = cx.editor.document_of(cx.view).len_chars();
    cx.editor.set_mode(cx.view, area.mode);
    cx.editor.set_visual_anchor(cx.view, area.anchor.min(len));
    cx.editor.set_cursor(cx.view, area.cursor.min(len));
}

/// The selection of `view` in a visual mode: its anchor (the cursor is the
/// other end) and how an operator takes it, inclusive or linewise (also
/// for a block, by its lines, where a command has no block form); `None`
/// outside the visual modes.
pub(crate) fn selection(editor: &Editor, view: ViewId) -> Option<(usize, MotionKind)> {
    let state = editor.view_ref(view);
    let anchor = state.visual_anchor()?;
    let kind = match state.mode() {
        Mode::Visual => MotionKind::Inclusive,
        Mode::VisualBlock | Mode::VisualLine => MotionKind::Linewise,
        Mode::CommandLine | Mode::Insert | Mode::Normal | Mode::OperatorPending | Mode::Replace => {
            return None;
        }
    };
    Some((anchor, kind))
}

/// The chars the visual selection of `view` covers in `document`, laid
/// out with `options`, for painting: one range, or for a block one per
/// line of it among `lines`; empty outside the visual modes.
pub(crate) fn selected_ranges(
    document: &Document,
    options: &EditorOptions,
    view: &View,
    lines: Range<usize>,
) -> Vec<Range<usize>> {
    let Some(anchor) = view.visual_anchor() else {
        return Vec::new();
    };
    let (start, end) = (anchor.min(view.cursor()), anchor.max(view.cursor()));
    let extras = view.cursors().iter().zip(view.extra_anchors());
    let extra_ranges = extras.filter_map(|(cursor, anchor)| {
        let anchor = (*anchor)?;
        let (start, end) = (anchor.min(*cursor), anchor.max(*cursor));
        match view.mode() {
            Mode::Visual => Some(start..document.next_grapheme_boundary(end)),
            Mode::VisualLine => Some(line_span(document, start, end)),
            Mode::CommandLine
            | Mode::Insert
            | Mode::Normal
            | Mode::OperatorPending
            | Mode::Replace
            | Mode::VisualBlock => None,
        }
    });
    let extra_ranges: Vec<Range<usize>> = extra_ranges.collect();
    match view.mode() {
        Mode::Visual => iter::once(start..document.next_grapheme_boundary(end))
            .chain(extra_ranges)
            .collect(),
        Mode::VisualBlock => {
            let Some(block) = view_block(document, options, view) else {
                return Vec::new();
            };
            let first = block.first.max(lines.start);
            let last = block.last.min(lines.end.saturating_sub(1));
            (first..=last)
                .map(|line| block.chars_on(document, options, line))
                .collect()
        }
        Mode::VisualLine => iter::once(line_span(document, start, end))
            .chain(extra_ranges)
            .collect(),
        Mode::CommandLine | Mode::Insert | Mode::Normal | Mode::OperatorPending | Mode::Replace => {
            Vec::new()
        }
    }
}

/// The chars of the whole lines from the line of `start` to that of `end`,
/// with the last line's break.
fn line_span(document: &Document, start: usize, end: usize) -> Range<usize> {
    let first = start_of_line(document, line_of(document, start));
    first..start_of_line(document, line_of(document, end) + 1)
}

/// `o`: moves the cursor to the other end of the selection.
pub(crate) fn swap_anchor(cx: &mut CommandContext<'_>) {
    let Some(anchor) = cx.editor.view_ref(cx.view).visual_anchor() else {
        return;
    };
    let cursor = cx.editor.view_ref(cx.view).cursor();
    cx.editor.set_visual_anchor(cx.view, cursor);
    cx.editor.set_cursor(cx.view, anchor);
}

/// `r` in a visual mode: waits for the character to replace every
/// selected character with (line breaks stay).
pub(crate) fn replace_selected_chars(cx: &mut CommandContext<'_>) {
    if block_of(cx.editor, cx.view).is_some() {
        return replace_in_block(cx);
    }
    await_char(cx, replace_selected_with);
}

/// `O` in a visual mode: in a block, moves the cursor to the other end of
/// its line of the block; elsewhere, as `o`.
pub(crate) fn swap_visual_corner(cx: &mut CommandContext<'_>) {
    if block_of(cx.editor, cx.view).is_some() {
        return swap_block_corner(cx);
    }
    swap_anchor(cx);
}

/// `C-v`: starts selecting a block, switches another selection to a
/// block, or in visual block mode goes back to Normal mode.
pub(crate) fn visual_blockwise(cx: &mut CommandContext<'_>) {
    switch_visual(cx, Mode::VisualBlock);
}

/// `v`: starts selecting characters, switches a line selection to
/// characters, or in Visual mode goes back to Normal mode.
pub(crate) fn visual_charwise(cx: &mut CommandContext<'_>) {
    switch_visual(cx, Mode::Visual);
}

/// `V`: starts selecting lines, switches a character selection to lines,
/// or in VisualLine mode goes back to Normal mode.
pub(crate) fn visual_linewise(cx: &mut CommandContext<'_>) {
    switch_visual(cx, Mode::VisualLine);
}

/// The blanks that fill the cells from `from` to `to` of a line, as tabs
/// up to the last tab stop they reach then spaces, or spaces only when
/// `options` expand tabs.
pub(crate) fn blanks_between(from: usize, to: usize, options: &EditorOptions) -> String {
    if options.expand_tab {
        return " ".repeat(to.saturating_sub(from));
    }
    let tab = options.tab_stop();
    let mut text = String::new();
    let mut x = from;
    while (x / tab + 1) * tab <= to {
        text.push('\t');
        x = (x / tab + 1) * tab;
    }
    text.push_str(&" ".repeat(to.saturating_sub(x)));
    text
}

/// The text of the selection from `start` to `end`, taken as `kind`.
fn selected_text(document: &Document, start: usize, end: usize, kind: MotionKind) -> RegisterText {
    match kind {
        MotionKind::Linewise => {
            let first = start_of_line(document, line_of(document, start));
            let after = start_of_line(document, line_of(document, end) + 1);
            RegisterText::new(RegisterKind::Linewise, document.slice(first..after))
        }
        MotionKind::Exclusive | MotionKind::Inclusive => {
            let after = document.next_grapheme_boundary(end);
            RegisterText::new(RegisterKind::Charwise, document.slice(start..after))
        }
    }
}

/// Moves the selected lines one indent level per count `shift`, leaving
/// empty lines alone when indenting, then goes back to Normal mode with
/// the cursor on the first non-blank of the first line.
fn shift_lines(cx: &mut CommandContext<'_>, shift: Shift) {
    if let Some(block) = block_of(cx.editor, cx.view) {
        let operator = match shift {
            Shift::Left => Operator::Outdent,
            Shift::Right => Operator::Indent,
        };
        return run_block_operator(cx, block, operator, cx.count_or_one());
    }
    let Some((anchor, _)) = selection(cx.editor, cx.view) else {
        return;
    };
    let document = cx.editor.document_of(cx.view);
    let cursor = cx.editor.view_ref(cx.view).cursor();
    let first = line_of(document, anchor.min(cursor));
    let last = line_of(document, anchor.max(cursor));
    cx.editor.set_mode(cx.view, Mode::Normal);
    shift_line_range(cx.editor, cx.view, (first, last), cx.count_or_one(), shift);
}

/// Shifts the lines `first` to `last` of the document of `view` by
/// `levels` indent levels, the way `shift` says (empty lines are never
/// indented), and puts the cursor on the first line's first non-blank.
pub(crate) fn shift_line_range(
    editor: &mut Editor,
    view: ViewId,
    (first, last): (usize, usize),
    levels: usize,
    shift: Shift,
) {
    let options = *editor.options();
    let step = options.indent_width().saturating_mul(levels);
    let document = editor.document_of(view);
    let mut edits = Vec::new();
    for line in first..=last {
        let text = document.line_text(line).unwrap_or_default();
        let blanks: String = text
            .chars()
            .take_while(|c| matches!(c, ' ' | '\t'))
            .collect();
        if text.is_empty() && shift == Shift::Right {
            continue;
        }
        let width = blanks.chars().fold(0, |width, c| match c {
            '\t' => (width / options.tab_stop() + 1) * options.tab_stop(),
            _ => width + 1,
        });
        let new_width = match shift {
            Shift::Left => width.saturating_sub(step),
            Shift::Right => width + step,
        };
        let new_blanks = blanks_between(0, new_width, &options);
        if new_blanks != blanks {
            let start = start_of_line(document, line);
            edits.push(Edit::new(start..start + blanks.chars().count(), new_blanks));
        }
    }
    editor.apply_edits(view, edits);
    let line_start = start_of_line(editor.document_of(view), first);
    let target = first_non_blank_of_line(editor, view, line_start);
    editor.set_cursor(view, target);
}

/// Replaces every selected character (line breaks left out) with `c`, one
/// per cluster, back in Normal mode at the selection's start.
fn replace_selected_with(cx: &mut CommandContext<'_>, c: char) {
    let Some((anchor, kind)) = selection(cx.editor, cx.view) else {
        return;
    };
    let document = cx.editor.document_of(cx.view);
    let cursor = cx.editor.view_ref(cx.view).cursor();
    let (start, end) = (anchor.min(cursor), anchor.max(cursor));
    let (first, last) = (line_of(document, start), line_of(document, end));
    let edits: Vec<Edit> = (first..=last)
        .filter_map(|line| {
            let line_start = start_of_line(document, line);
            let line_end = line_end(document, line_start);
            let range = match kind {
                MotionKind::Linewise => line_start..line_end,
                MotionKind::Exclusive | MotionKind::Inclusive => {
                    start.max(line_start)..document.next_grapheme_boundary(end).min(line_end)
                }
            };
            (c != LINE_BREAK_CHAR && !range.is_empty()).then(|| {
                let clusters = cluster_count(document, range.clone());
                Edit::new(range, c.to_string().repeat(clusters))
            })
        })
        .collect();
    let top = match kind {
        MotionKind::Linewise => start_of_line(document, first),
        MotionKind::Exclusive | MotionKind::Inclusive => start,
    };
    cx.editor.set_mode(cx.view, Mode::Normal);
    cx.editor.apply_edits(cx.view, edits);
    cx.editor.set_cursor(cx.view, top);
}

/// Starts selecting in `mode`, switches the selection to it, or leaves it
/// for Normal mode when already in it.
fn switch_visual(cx: &mut CommandContext<'_>, mode: Mode) {
    let current = cx.editor.view_ref(cx.view).mode();
    if current == mode {
        cx.editor.set_mode(cx.view, Mode::Normal);
    } else if is_visual(current) {
        cx.editor.set_mode(cx.view, mode);
    } else {
        let cursor = cx.editor.view_ref(cx.view).cursor();
        cx.editor.set_mode(cx.view, mode);
        cx.editor.set_visual_anchor(cx.view, cursor);
    }
}

// ========================================================================
// Tests
// ========================================================================

#[cfg(test)]
mod tests {
    use crate::options::EditorOptions;
    use crate::paint::{Style, Surface};
    use crate::render::render_view;
    use crate::testing::{Harness, assert_keys};
    use crate::view::Mode;

    /// The styles of the first `cols` cells of row `row` after `keys`.
    fn styles_after(before: &str, keys: &str, cols: u16) -> Vec<Style> {
        let mut harness = Harness::new(before);
        harness.editor.resize_view(harness.view, cols, 2);
        harness.keys(keys);
        let mut surface = Surface::new(0, 0);
        render_view(&harness.editor, harness.view, &mut surface);
        (0..cols)
            .map(|column| surface.cell(column, 0).unwrap().style)
            .collect()
    }

    #[test]
    fn test_motions_extend_the_selection_painted_with_both_ends_included() {
        use Style::{CursorLine as C, Selection as S};
        assert_eq!(styles_after("a|bcde", "v l l", 6), [C, S, S, S, C, C]);
        assert_eq!(styles_after("abc|de", "v h h", 6), [C, S, S, S, C, C]);
        assert_eq!(styles_after("ab|cde", "v <esc>", 6), [C; 6]);
    }

    #[test]
    fn test_d_deletes_the_selection_by_characters_or_by_lines() {
        assert_keys("a|bcde", "v l l d", "a|e");
        assert_keys("ab|c\nde\nfg", "v j d", "a|b\nfg");
        assert_keys("a\nb|c\nd\ne", "V j d", "a\n|e");
        assert_keys("a\nb|c", "V d", "|a");
        assert_keys("a|bcde", "v l x", "a|de");
        assert_keys("日|本語x", "v l d", "日|x");
    }

    #[test]
    fn test_o_swaps_the_ends_so_the_other_end_moves() {
        assert_keys("ab|cde", "v l o h d", "a|e");
    }

    #[test]
    fn test_c_changes_the_selection_and_y_yanks_it_to_the_start() {
        assert_keys("a|bcd", "v l c X <esc>", "a|Xd");
        assert_keys("a\n|b\nc", "V c X <esc>", "a\n|X\nc");
        assert_keys("abc|d", "v h h y P", "abc|dbcd");
        assert_keys("a\n|b", "V y p", "a\nb\n|b");
    }

    #[test]
    fn test_v_and_capital_v_switch_or_leave_the_visual_modes() {
        let mut harness = Harness::new("|ab\ncd");
        harness.keys("v");
        assert_eq!(harness.mode(), Mode::Visual);
        harness.keys("V");
        assert_eq!(harness.mode(), Mode::VisualLine);
        harness.keys("j v");
        assert_eq!(harness.mode(), Mode::Visual, "the anchor is kept");
        harness.keys("d");
        assert_eq!(harness.marked(), "|d");
        harness.keys("V V");
        assert_eq!(harness.mode(), Mode::Normal);
    }

    #[test]
    fn test_indent_and_outdent_the_selected_lines_by_the_indent_width() {
        assert_keys("|a\nb\n\nc", "V j j j >", "\t|a\n\tb\n\n\tc");
        assert_keys("\t\ta\n\t|b", "V k <", "\t|a\nb");
        assert_keys("|a", "V 2 >", "\t\t|a");
        assert_keys("  |a", "V <", "|a");
        let options = EditorOptions {
            expand_tab: true,
            shift_width: 2,
            ..EditorOptions::default()
        };
        let mut harness = Harness::with_options("|a\n   b", options);
        harness.keys("V j >");
        assert_eq!(harness.marked(), "  |a\n     b");
        harness.keys("V j < u");
        assert_eq!(harness.marked(), "  |a\n     b", "one undo step");
    }

    #[test]
    fn test_p_replaces_the_selection_and_the_replaced_text_takes_the_register() {
        assert_keys("|foo bar", "y w w v e p", "foo foo| ");
        assert_keys("|foo bar", "y w w v e p 0 P", "ba|rfoo foo ");
        assert_keys("|foo bar", "y w w v e P 0 P", "foo| foo foo ");
        assert_keys("|one\ntwo\nthree", "y y j V p", "one\n|one\nthree");
        assert_keys("|one\ntwo", "y y j V p", "one\n|one");
        assert_keys("|one\nab cd", "y y j w v p", "one\nab \n|one\nd");
        assert_keys("|x\none\ntwo", "y l j V j p", "x\n|x");
    }

    #[test]
    fn test_gv_selects_the_last_selection_again_carried_by_edits() {
        assert_keys("|abcdef", "v l <esc> $ g v d", "|cdef");
        assert_keys("ab|cdef", "v l <esc> 0 x g v d", "b|ef");
        assert_keys("|a\nb\nc", "V j <esc> G g v d", "|c");
    }

    #[test]
    fn test_a_register_and_a_count_work_in_visual_mode() {
        assert_keys("|foo bar", "v e \" a y w \" a P", "foo fo|obar");
        assert_keys("|abcdef", "v 3 l d", "|ef");
    }

    #[test]
    fn test_a_host_reads_the_visual_anchor_while_a_visual_mode_lasts() {
        let mut harness = Harness::new("ab|cdef");
        let anchor = |harness: &Harness| harness.editor.view(harness.view).unwrap().visual_anchor();
        assert_eq!(anchor(&harness), None);
        harness.keys("v l l");
        assert_eq!(anchor(&harness), Some(2));
        harness.keys("o");
        assert_eq!(anchor(&harness), Some(4), "o swaps the ends");
        harness.keys("V");
        assert_eq!(anchor(&harness), Some(4), "kept between visual modes");
        harness.keys("<esc>");
        assert_eq!(anchor(&harness), None);
    }
}
