//! The mouse: what a host's mouse events do to a view, in cells counted
//! from the view's top left corner, with no window toolkit involved.
//!
//! - A press moves the cursor to the character in the cell, leaving a
//!   visual mode or a waiting operator (Insert mode stays, its cursor
//!   moved). A cell in the gutter counts as the start of its row; past the
//!   end of a line, its last character (its end in Insert mode); below the
//!   document, its last line. Wide characters and tabs take every cell
//!   they cover; with soft wrap each screen row is its part of the line.
//! - A drag after a press selects from the pressed character to the one
//!   under the pointer, in Visual mode, leaving Insert mode first.
//! - A press with Shift held selects from the cursor to the cell instead.
//! - A double click selects the word in the cell (as `viw`), a triple
//!   click its line (as `V`).
//! - The wheel scrolls the view three rows a notch, never past the first
//!   or last row, moving the cursor only as far as it must to stay in
//!   sight with the scroll margin.
//!
//! A click (a press, a double or triple click) leaves only the main
//! cursor of a view with several.
//!
//! A host whose toolkit does not count clicks (a terminal) passes each
//! event through a [`ClickCounter`], which turns quick presses on one cell
//! into double and triple clicks; a host whose toolkit counts them sends
//! [`MouseEventKind::DoubleClick`] and [`MouseEventKind::TripleClick`]
//! itself.

use std::time::Duration;

use crate::keys::{Mods, Timestamp};

use super::editor::{Editor, Handled, ViewId};
use super::insert::exit_insert_session;
use super::motion::last_cursor_offset;
use super::operator::Operand;
use super::text_object::{Reach, word_object};
use super::view::{Mode, is_visual};

// ========================================================================
// Constants
// ========================================================================

/// The most presses a click counts: a triple click.
const MAX_CLICKS: u8 = 3;

/// The screen rows one notch of the wheel scrolls.
const SCROLL_ROWS: usize = 3;

// ========================================================================
// Data Structures
// ========================================================================

/// Counts clicks for a host whose toolkit reports each press alone: a
/// press on the cell of the one before, within the interval the host
/// chose, is a double click, and the next a triple click; the press after
/// a triple click starts over. A press with Shift held extends a
/// selection, so it is never counted and starts the count over.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ClickCounter {
    /// The longest gap between two presses of one multiple click.
    interval: Duration,
    /// The last press counted, if any.
    last: Option<CountedPress>,
}

/// A press a [`ClickCounter`] counted.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct CountedPress {
    /// The cell column pressed.
    column: u16,
    /// How many presses in a row it was, 1 to [`MAX_CLICKS`].
    count: u8,
    /// When it was pressed.
    pressed_at: Timestamp,
    /// The cell row pressed.
    row: u16,
}

/// A mouse event over a view, in cells from the view's top left corner,
/// with the primary button unless it is a scroll.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct MouseEvent {
    /// The cell column, from 0 at the view's left edge, gutter included.
    pub column: u16,
    /// What happened.
    pub kind: MouseEventKind,
    /// The modifiers held.
    pub mods: Mods,
    /// The cell row, from 0 at the view's top edge.
    pub row: u16,
}

/// What a mouse event is.
///
/// New variants may come in a minor release, so a host's `match` needs a
/// fallback arm.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[non_exhaustive]
pub enum MouseEventKind {
    /// The second press of a double click, in place of a [`Press`](Self::Press).
    DoubleClick,
    /// The pointer moved with the button held.
    Drag,
    /// The button went down.
    Press,
    /// The button went up.
    Release,
    /// The wheel turned one notch toward the end of the document.
    ScrollDown,
    /// The wheel turned one notch toward the start of the document.
    ScrollUp,
    /// The third press of a triple click, in place of a [`Press`](Self::Press).
    TripleClick,
}

// ========================================================================
// ClickCounter Implementations
// ========================================================================

impl ClickCounter {
    /// A counter that takes presses at most `interval` apart, on one
    /// cell, as one double or triple click (400 ms is usual).
    pub fn new(interval: Duration) -> Self {
        Self {
            interval,
            last: None,
        }
    }

    /// `event`, happening at `now`, with a [`MouseEventKind::Press`] turned
    /// into a double or triple click when it follows presses on its cell
    /// quickly enough; any other event passes as it is.
    pub fn count(&mut self, event: MouseEvent, now: Timestamp) -> MouseEvent {
        if event.kind != MouseEventKind::Press {
            return event;
        }
        if event.mods.contains(Mods::SHIFT) {
            self.last = None;
            return event;
        }
        let count = match self.last {
            Some(last)
                if (last.column, last.row) == (event.column, event.row)
                    && now.saturating_duration_since(last.pressed_at) <= self.interval =>
            {
                last.count % MAX_CLICKS + 1
            }
            Some(_) | None => 1,
        };
        self.last = Some(CountedPress {
            column: event.column,
            count,
            pressed_at: now,
            row: event.row,
        });
        let kind = match count {
            1 => MouseEventKind::Press,
            2 => MouseEventKind::DoubleClick,
            _ => MouseEventKind::TripleClick,
        };
        MouseEvent { kind, ..event }
    }
}

// ========================================================================
// MouseEvent Implementations
// ========================================================================

impl MouseEvent {
    /// An event of `kind` at cell `column` and `row` of the view, with no
    /// modifiers held.
    pub fn new(kind: MouseEventKind, column: u16, row: u16) -> Self {
        Self {
            column,
            kind,
            mods: Mods::NONE,
            row,
        }
    }

    /// This event with `mods` held.
    pub fn with_mods(self, mods: Mods) -> Self {
        Self { mods, ..self }
    }
}

// ========================================================================
// Functions
// ========================================================================

/// Acts on `event` over `view`, as the module documentation describes.
/// Ignored while a prompt is open.
pub(crate) fn handle_mouse(editor: &mut Editor, view: ViewId, event: MouseEvent) -> Handled {
    let mode = editor.view_ref(view).mode();
    if mode == Mode::CommandLine {
        return Handled::Ignored;
    }
    let is_click = matches!(
        event.kind,
        MouseEventKind::DoubleClick | MouseEventKind::Press | MouseEventKind::TripleClick
    );
    if is_click {
        editor.view_mut(view).extra_cursors_mut().clear();
    }
    match event.kind {
        MouseEventKind::DoubleClick => select_word(editor, view, event),
        MouseEventKind::Drag => drag(editor, view, event),
        MouseEventKind::Press if event.mods.contains(Mods::SHIFT) => extend(editor, view, event),
        MouseEventKind::Press => press(editor, view, event),
        MouseEventKind::Release => editor.view_mut(view).set_mouse_anchor(None),
        MouseEventKind::ScrollDown => scroll(editor, view, true),
        MouseEventKind::ScrollUp => scroll(editor, view, false),
        MouseEventKind::TripleClick => select_line(editor, view, event),
    }
    Handled::Consumed
}

/// The char offset in the cell of `event`.
fn cell_offset(editor: &Editor, view: ViewId, event: MouseEvent) -> usize {
    let state = editor.view_ref(view);
    let document = editor.document_of(view);
    let decorations = editor.decorations_of(state.document());
    let options = editor.layout_options(view);
    state.offset_at_cell(document, decorations, &options, event.column, event.row)
}

/// Moves the cursor of `view` to `offset`, kept off the line end outside
/// Insert mode.
fn place_cursor(editor: &mut Editor, view: ViewId, offset: usize) {
    let target = offset.min(last_cursor_offset(editor, view, offset));
    editor.set_cursor(view, target);
}

/// Extends a visual selection to the dragged cell, starting one at the
/// pressed character on the first move away from it.
fn drag(editor: &mut Editor, view: ViewId, event: MouseEvent) {
    let Some(anchor) = editor.view_ref(view).mouse_anchor() else {
        return;
    };
    let offset = cell_offset(editor, view, event);
    let mode = editor.view_ref(view).mode();
    if !is_visual(mode) {
        if offset == anchor {
            return;
        }
        enter_visual(editor, view, Mode::Visual, anchor);
    }
    place_cursor(editor, view, offset);
}

/// Starts `mode`, a visual mode, selecting from `anchor`, leaving Insert
/// mode or a waiting operator first.
fn enter_visual(editor: &mut Editor, view: ViewId, mode: Mode, anchor: usize) {
    match editor.view_ref(view).mode() {
        Mode::Insert | Mode::Replace => {
            exit_insert_session(editor, view);
        }
        Mode::CommandLine
        | Mode::Normal
        | Mode::OperatorPending
        | Mode::Visual
        | Mode::VisualBlock
        | Mode::VisualLine => {}
    }
    editor.set_mode(view, mode);
    editor.set_visual_anchor(view, anchor);
}

/// Selects from the cursor to the cell of a Shift press, or moves the end
/// of a visual selection there.
fn extend(editor: &mut Editor, view: ViewId, event: MouseEvent) {
    let offset = cell_offset(editor, view, event);
    if !is_visual(editor.view_ref(view).mode()) {
        let cursor = editor.view_ref(view).cursor();
        enter_visual(editor, view, Mode::Visual, cursor);
    }
    place_cursor(editor, view, offset);
}

/// Moves the cursor to the pressed cell, leaving a visual mode or a
/// waiting operator, and remembers the place for a drag.
fn press(editor: &mut Editor, view: ViewId, event: MouseEvent) {
    match editor.view_ref(view).mode() {
        Mode::OperatorPending | Mode::Visual | Mode::VisualBlock | Mode::VisualLine => {
            editor.set_mode(view, Mode::Normal);
        }
        Mode::CommandLine | Mode::Insert | Mode::Normal | Mode::Replace => {}
    }
    let offset = cell_offset(editor, view, event);
    place_cursor(editor, view, offset);
    let cursor = editor.view_ref(view).cursor();
    editor.view_mut(view).set_mouse_anchor(Some(cursor));
}

/// Scrolls `view` one notch down or up, moving the cursor into sight.
fn scroll(editor: &mut Editor, view: ViewId, down: bool) {
    if let Some(offset) = editor.scroll_view_rows(view, down, SCROLL_ROWS) {
        place_cursor(editor, view, offset);
    }
}

/// Selects the line of the cell, in VisualLine mode.
fn select_line(editor: &mut Editor, view: ViewId, event: MouseEvent) {
    let offset = cell_offset(editor, view, event);
    enter_visual(editor, view, Mode::VisualLine, offset);
    place_cursor(editor, view, offset);
}

/// Selects the word in the cell, in Visual mode: its first character to
/// its last.
fn select_word(editor: &mut Editor, view: ViewId, event: MouseEvent) {
    let offset = cell_offset(editor, view, event);
    let document = editor.document_of(view);
    let range = match word_object(document, offset, 1, false, Reach::Inner) {
        Operand::Charwise(range) if !range.is_empty() => range,
        Operand::Charwise(_) | Operand::Linewise(_) => offset..offset,
    };
    let last = document.prev_grapheme_boundary(range.end).max(range.start);
    enter_visual(editor, view, Mode::Visual, range.start);
    place_cursor(editor, view, last);
}

// ========================================================================
// Tests
// ========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::options::EditorOptions;
    use crate::testing::Harness;

    /// A harness editing `marked` in a view `cols` by `rows` with `options`.
    fn harness(marked: &str, options: EditorOptions, cols: u16, rows: u16) -> Harness {
        let mut harness = Harness::with_options(marked, options);
        harness.editor.resize_view(harness.view, cols, rows);
        harness
    }

    /// Sends a `kind` event at `column` and `row` of the harness's view.
    fn mouse(harness: &mut Harness, kind: MouseEventKind, column: u16, row: u16) {
        let event = MouseEvent::new(kind, column, row);
        harness.editor.handle_mouse(harness.view, event);
    }

    #[test]
    fn test_a_click_lands_on_the_character_in_the_cell_past_gutter_tabs_and_wide_chars() {
        let options = EditorOptions {
            line_numbers: true,
            tab_width: 4,
            ..EditorOptions::default()
        };
        let mut harness = harness("|a\tb\n日本x", options, 20, 5);
        let mut click = |column, row| {
            mouse(&mut harness, MouseEventKind::Press, column, row);
            harness.marked()
        };
        assert_eq!(click(6, 0), "a|\tb\n日本x", "inside the tab");
        assert_eq!(click(8, 0), "a\t|b\n日本x");
        assert_eq!(click(15, 0), "a\t|b\n日本x", "past the end, the last char");
        assert_eq!(
            click(7, 1),
            "a\tb\n日|本x",
            "the second cell of a wide char"
        );
        assert_eq!(click(1, 1), "a\tb\n|日本x", "the gutter");
        assert_eq!(click(8, 4), "a\tb\n日本|x", "below the document");
    }

    #[test]
    fn test_a_click_follows_wrapped_rows_and_reaches_the_line_end_in_insert_mode() {
        let options = EditorOptions {
            wrap: true,
            ..EditorOptions::default()
        };
        let mut harness = harness("|abcdefgh\nz", options, 4, 5);
        mouse(&mut harness, MouseEventKind::Press, 1, 1);
        assert_eq!(harness.marked(), "abcde|fgh\nz");
        mouse(&mut harness, MouseEventKind::Press, 3, 2);
        assert_eq!(harness.marked(), "abcdefgh\n|z");
        harness.keys("i");
        mouse(&mut harness, MouseEventKind::Press, 3, 2);
        assert_eq!(harness.mode(), Mode::Insert);
        assert_eq!(harness.marked(), "abcdefgh\nz|");
    }

    #[test]
    fn test_a_drag_selects_in_visual_mode_and_a_press_or_shift_press_ends_or_extends_it() {
        let mut harness = harness("|one two three", EditorOptions::default(), 20, 2);
        mouse(&mut harness, MouseEventKind::Press, 4, 0);
        mouse(&mut harness, MouseEventKind::Drag, 4, 0);
        assert_eq!(harness.mode(), Mode::Normal, "no move, no selection");
        mouse(&mut harness, MouseEventKind::Drag, 6, 0);
        mouse(&mut harness, MouseEventKind::Release, 6, 0);
        assert_eq!(harness.mode(), Mode::Visual);
        harness.keys("d");
        assert_eq!(harness.marked(), "one | three");
        mouse(&mut harness, MouseEventKind::Drag, 8, 0);
        assert_eq!(harness.mode(), Mode::Normal, "released, so no drag");
        let shift_press = MouseEvent::new(MouseEventKind::Press, 7, 0).with_mods(Mods::SHIFT);
        harness.editor.handle_mouse(harness.view, shift_press);
        harness.keys("y");
        assert_eq!(harness.marked(), "one | three");
        harness.keys("P");
        assert_eq!(harness.marked(), "one  th|r three");
    }

    #[test]
    fn test_double_and_triple_clicks_select_a_word_and_a_line_even_from_insert_mode() {
        let mut harness = harness("ab |cd ef\ngh", EditorOptions::default(), 20, 2);
        harness.keys("i X");
        mouse(&mut harness, MouseEventKind::Press, 5, 0);
        mouse(&mut harness, MouseEventKind::DoubleClick, 5, 0);
        assert_eq!(harness.mode(), Mode::Visual);
        harness.keys("d");
        assert_eq!(harness.marked(), "ab | ef\ngh");
        harness.keys("u");
        assert_eq!(
            harness.editor.document_of(harness.view).to_string(),
            "ab Xcd ef\ngh"
        );
        mouse(&mut harness, MouseEventKind::TripleClick, 1, 0);
        assert_eq!(harness.mode(), Mode::VisualLine);
        harness.keys("d");
        assert_eq!(harness.marked(), "|gh");
        harness.keys("u u");
        assert_eq!(
            harness.editor.document_of(harness.view).to_string(),
            "ab cd ef\ngh"
        );
    }

    #[test]
    fn test_presses_on_one_cell_in_quick_succession_count_up_to_a_triple_click() {
        const INTERVAL: Duration = Duration::from_millis(400);
        let mut counter = ClickCounter::new(INTERVAL);
        let start = Timestamp::default();
        let kinds: Vec<_> = [(0, 3), (100, 3), (200, 3), (300, 3), (400, 4), (2000, 4)]
            .into_iter()
            .map(|(ms, column)| {
                let event = MouseEvent::new(MouseEventKind::Press, column, 2);
                counter.count(event, start + Duration::from_millis(ms)).kind
            })
            .collect();
        assert_eq!(
            kinds,
            [
                MouseEventKind::Press,
                MouseEventKind::DoubleClick,
                MouseEventKind::TripleClick,
                MouseEventKind::Press,
                MouseEventKind::Press,
                MouseEventKind::Press,
            ],
            "the fourth starts over, another cell and a slow press count one"
        );
    }

    #[test]
    fn test_a_shift_press_passes_uncounted_and_other_events_pass_as_they_are() {
        let mut counter = ClickCounter::new(Duration::from_millis(400));
        let now = Timestamp::default();
        let press = MouseEvent::new(MouseEventKind::Press, 1, 1);
        counter.count(press, now);
        let shift_press = press.with_mods(Mods::SHIFT);
        assert_eq!(counter.count(shift_press, now), shift_press);
        assert_eq!(
            counter.count(press, now).kind,
            MouseEventKind::Press,
            "the shift press started the count over"
        );
        let release = MouseEvent::new(MouseEventKind::Release, 1, 1);
        assert_eq!(counter.count(release, now), release);
        assert_eq!(counter.count(press, now).kind, MouseEventKind::DoubleClick);
    }

    #[test]
    fn test_counted_clicks_drive_the_editor_like_toolkit_clicks() {
        let mut harness = harness(
            "|ab cd
ef",
            EditorOptions::default(),
            20,
            2,
        );
        let mut counter = ClickCounter::new(Duration::from_millis(400));
        let now = Timestamp::default();
        for _ in 0..2 {
            let event = counter.count(MouseEvent::new(MouseEventKind::Press, 4, 0), now);
            harness.editor.handle_mouse(harness.view, event);
        }
        harness.keys("d");
        assert_eq!(harness.marked(), "ab| \nef");
    }

    #[test]
    fn test_the_wheel_scrolls_within_the_document_moving_the_cursor_into_sight() {
        let text: String = (0..20).map(|line| format!("line {line}\n")).collect();
        let mut harness = harness(
            &format!("|{}", text.trim_end()),
            EditorOptions::default(),
            10,
            5,
        );
        let top = |harness: &Harness| harness.editor.view(harness.view).unwrap().scroll().line;
        let cursor_line = |harness: &Harness| {
            let cursor = harness.editor.view(harness.view).unwrap().cursor();
            harness
                .editor
                .document_of(harness.view)
                .char_to_position(cursor)
                .unwrap()
                .line
        };
        mouse(&mut harness, MouseEventKind::ScrollDown, 0, 0);
        assert_eq!((top(&harness), cursor_line(&harness)), (3, 5));
        mouse(&mut harness, MouseEventKind::ScrollUp, 0, 0);
        assert_eq!((top(&harness), cursor_line(&harness)), (0, 2));
        for _ in 0..10 {
            mouse(&mut harness, MouseEventKind::ScrollDown, 0, 0);
        }
        assert_eq!((top(&harness), cursor_line(&harness)), (15, 17));
        mouse(&mut harness, MouseEventKind::Press, 0, 4);
        assert_eq!((top(&harness), cursor_line(&harness)), (15, 19));
    }
}
