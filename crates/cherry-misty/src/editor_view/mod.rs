//! [`EditorView`]: one view of a Cherry [`Editor`] as a misty widget. The
//! app owns the editor (its documents, views, and
//! undo histories) and an `EditorView` per view it shows; the widget
//! feeds the view keys, keeps it sized to the area it is drawn in, and
//! paints it through the cell grid, Cherry's styles mapped to theme colors
//! and its cursor drawn as a block or a bar.
//!
//! Keys come in as misty [`misty::Input::Key`]s. What the editor asks
//! of its host (save a document, close a view, show a message) comes back
//! from the lent [`misty::Widget::handle`] as [`EditorResponse::Requests`],
//! for the app to act on in order. A half-typed key sequence (the `g` of
//! `g g`) times out on the deadline [`EditorView::about_to_wait`] returns,
//! which hands back what the settled sequence asked, in its
//! [`EditorWait`]. While
//! a search (`/`, `?`) or command line (`:`) prompt is open, the view gives
//! its bottom row to the prompt, whose cursor takes over.
//!
//! Input method events go to the lent [`misty::Widget::handle`]: text being
//! composed is drawn underlined over the cells from the cursor on, and
//! committed text is typed as keys while the view takes text (Insert mode
//! or a prompt). [`EditorView::ime_area`] says where the cursor is then,
//! for [`ImeFocus::follow`](misty::ImeFocus::follow).
//!
//! The mouse goes to the lent [`misty::Widget::handle`]: a press moves the
//! cursor to the character under the pointer, a drag selects, a double
//! click selects a word and a triple click a line, and the wheel scrolls,
//! all through Cherry's own mouse handling, in cells counted from the
//! view's top left corner.
//!
//! While Cherry's Insert-mode completion menu is open (`C-x C-n` and the
//! like), the view draws it as a popup under the text being completed:
//! each match's kind, its label with the typed characters marked, and its
//! detail, the match typed now on the selection color, and the selected
//! match's documentation in a card beside it.
//!
//! Text a syntax highlighter marks (with the `syntax` feature, through
//! the engine's `syntax` module) is drawn in the theme's
//! [`SyntaxColors`](misty::SyntaxColors) for its highlight
//! name. Decorations the app sets on a document are drawn as underlines
//! by kind (an undercurl in the danger color for an error, in the warning
//! color for a warning, in the accent color for information, a dotted
//! muted line for a hint), and signs in the sign column in the same
//! colors.
//!
//! A scrollbar runs down the view's right edge while its screen rows
//! overflow it, drawn and driven as a terminal's is: a thin thumb on a faint
//! track, lit under the pointer, dimmed out of focus; a press on the thumb
//! drags it, a press on the track brings the thumb to the pointer, and the
//! wheel scrolls the text as before.
//! [`EditorView::set_scrollbar_hidden`] leaves it out, for an app drawing
//! its own.
//!
//! When the view scrolls by a few lines its text glides to the new place
//! rather than jumping: the lines scrolled out slide away as the new ones
//! slide in, and a scroll while one glides glides on from where the text
//! is. A scroll that cannot be shown that way (wrapped lines, relative
//! line numbers, a jump of a screen or more) jumps. The glide needs
//! [`EditorView::about_to_wait`] called each loop, or [`EditorView::glide`]
//! by an app that ticks the editor itself, and
//! [`EditorView::set_reduced_motion`] turns it off.

mod access;
mod completion;
mod draw;
mod input;
mod scrollbar;

use std::time::{Duration, Instant};

use cherry_engine::{
    ClickCounter, Clipboard, ClipboardSelection, Editor, Handled, HostRequest, ScrollOffset,
    Surface, ViewId,
};
use misty::{
    Attributes, CellBuffer, CellMetrics, ClipboardKind, EventCx, Font, Frame, Input, Mods, Outcome,
    Point, Preedit, Rect, Rgb, SmoothScroll, SystemClipboard, Theme, Tooltip, Widget,
};

use self::draw::{Glided, Mark};
use self::input::takes_text;
use super::clock;

// ========================================================================
// Constants
// ========================================================================

/// The longest gap between two presses on one cell of a double or triple
/// click.
const MULTI_CLICK_INTERVAL: Duration = Duration::from_millis(400);

// ========================================================================
// Data Structures
// ========================================================================

/// One view of an [`Editor`] the app owns, drawn as a widget.
#[derive(Debug)]
pub struct EditorView {
    /// The view's cells as the grid draws them.
    cells: CellBuffer,
    /// Turns quick presses on one cell into double and triple clicks.
    clicks: ClickCounter,
    /// The cursor's cell in the last frame, in physical pixels, or `None`
    /// when it was out of sight.
    cursor_area: Option<Rect>,
    /// Whether the view has the keyboard; out of it the cursor is hollow.
    focused: bool,
    /// The font the text is drawn in, or `None` for the window's.
    font: Option<Font>,
    /// How far from its place the text was drawn in the last frame, in
    /// rows.
    drawn_offset: f32,
    /// How the host's highlight ids are drawn, by id, once it gave them;
    /// `None` draws the ids of the engine's `syntax` module in the theme's
    /// syntax colors.
    highlights: Option<Vec<HighlightStyle>>,
    /// Where the view's rows were drawn in the last frame, and the size of
    /// a cell, to find the cell under the pointer; `None` before the first
    /// frame.
    layout: Option<ViewLayout>,
    /// The modifier keys held with the last pointer input, for
    /// Shift-click.
    modifiers: Mods,
    /// The glide of the text toward its place after a scroll, in rows
    /// still to go: positive while the text slides up into place.
    motion: SmoothScroll,
    /// Scratch cells the next frame is painted into.
    next: CellBuffer,
    /// The marks drawn over `cells`.
    marks: Vec<Mark>,
    /// The marks drawn over `next`.
    next_marks: Vec<Mark>,
    /// Where the pointer is, in physical pixels, or `None` outside the
    /// window.
    pointer: Option<Point>,
    /// Text an input method is composing at the cursor.
    preedit: Option<Preedit>,
    /// Whether the primary button went down over the view and is still
    /// held, so pointer moves are drags.
    pressed: bool,
    /// The prompt row's cells as the grid draws them.
    prompt_cells: CellBuffer,
    /// What the editor last painted of the prompt.
    prompt_surface: Surface,
    /// Where the view was scrolled in the last frame.
    scroll: Option<ScrollOffset>,
    /// The scrollbar down the right edge.
    scrollbar: scrollbar::Scrollbar,
    /// Whether the scrollbar is left out, the text taking the whole width.
    scrollbar_hidden: bool,
    /// What the editor last painted of the view.
    surface: Surface,
    /// The frames from before the scrolls being glided, newest first,
    /// drawn where newer text has not slid in yet; empty at rest.
    trail: Vec<Glided>,
    /// What a change bar's tooltip says of each kind of change, as the app
    /// named them; `None` for change bars with no tooltip.
    change_notes: Option<ChangeNotes>,
    /// What the gutter sign under the pointer says, once it has rested.
    tooltip: Tooltip,
    /// The view shown.
    view: ViewId,
    /// The touchpad scroll not yet worth a wheel notch, in rows: positive
    /// toward the end of the document.
    wheel_rest: f32,
}

/// How text of one [`HighlightId`](cherry_engine::HighlightId) of the
/// host's is drawn: a syntax scope, a semantic token, an inlay hint, a
/// diff's added line. Its foreground colors highlighted text, an inlay, a
/// virtual line, and a sign of [`DecorationKind::Background`]; its
/// background is the wash of a [`DecorationKind::Background`] decoration,
/// and lies under highlighted text that has no background of its own. A
/// wash whose style sets a foreground colors the text it lies under too,
/// over the text's own highlight: a color swatch's text, dark on a light
/// color.
///
/// [`DecorationKind::Background`]: cherry_engine::DecorationKind::Background
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct HighlightStyle {
    /// Bold, italic, and the other attributes the text takes.
    pub attributes: Attributes,
    /// The background, or `None` for the one the cell has anyway.
    pub background: Option<Rgb>,
    /// The text color, or `None` for the one the cell has anyway.
    pub foreground: Option<Rgb>,
}

/// An [`EditorView`] lent the [`Editor`] it shows a view of, for one call:
/// the [`Widget`] [`EditorView::lend`] makes, to hand inputs and draw.
#[derive(Debug)]
pub struct LentEditor<'a> {
    /// The editor lent.
    editor: &'a mut Editor,
    /// The view.
    view: &'a mut EditorView,
}

/// Misty's [`SystemClipboard`] as the clipboard a Cherry [`Editor`] copies
/// to and pastes from, handed to `Editor::set_clipboard`: registers `+`
/// and `*` reach the system clipboard and the primary selection.
#[derive(Debug)]
pub struct EditorClipboard(SystemClipboard);

/// Where a view's rows were drawn, for finding the cell under a point.
#[derive(Clone, Copy, Debug, PartialEq)]
struct ViewLayout {
    /// The rows of the view, without an open prompt's row, in physical
    /// pixels.
    area: Rect,
    /// The size of a cell.
    metrics: CellMetrics,
}

/// What the tooltip of a change bar in the gutter says of each kind of
/// change, as the app means them (`Added since the last commit`).
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ChangeNotes {
    /// What an added line's bar says.
    pub added: String,
    /// What the bar of a line with lines deleted by it says.
    pub deleted: String,
    /// What a changed line's bar says.
    pub modified: String,
}

/// What became of a key fed to the lent [`misty::Widget::handle`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EditorResponse {
    /// The editor used the key and asked nothing of the app.
    Handled,
    /// The key ran nothing; the app may pass it on.
    Ignored,
    /// The editor used the key and asks these of the app, oldest first:
    /// save a document, close a view, show a message. They are everything
    /// the editor had queued, whatever queued it.
    Requests(Vec<HostRequest>),
}

/// What [`EditorView::about_to_wait`] found: what the key sequences that
/// timed out asked of the app, and when to call it next.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EditorWait {
    /// When the next half-typed sequence times out or the next step of a
    /// scroll's glide is due, or `None` when nothing is pending.
    pub deadline: Option<Instant>,
    /// What the editor asks of the app, oldest first, as in
    /// [`EditorResponse::Requests`]; empty when it asks nothing.
    pub requests: Vec<HostRequest>,
}

// ========================================================================
// EditorView Implementations
// ========================================================================

impl EditorView {
    /// A widget showing `view` of the app's editor.
    pub fn new(view: ViewId) -> Self {
        Self {
            cells: CellBuffer::new(0, 0),
            clicks: ClickCounter::new(MULTI_CLICK_INTERVAL),
            cursor_area: None,
            drawn_offset: 0.0,
            focused: true,
            font: None,
            highlights: None,
            layout: None,
            modifiers: Mods::NONE,
            motion: SmoothScroll::default(),
            next: CellBuffer::new(0, 0),
            next_marks: Vec::new(),
            marks: Vec::new(),
            pointer: None,
            preedit: None,
            pressed: false,
            prompt_cells: CellBuffer::new(0, 0),
            prompt_surface: Surface::new(0, 0),
            scroll: None,
            scrollbar: scrollbar::Scrollbar::default(),
            scrollbar_hidden: false,
            surface: Surface::new(0, 0),
            tooltip: Tooltip::new(),
            trail: Vec::new(),
            change_notes: None,
            view,
            wheel_rest: 0.0,
        }
    }

    /// The view shown.
    pub fn view(&self) -> ViewId {
        self.view
    }

    /// Tells the view whether it has the keyboard: out of it, its cursor
    /// is drawn hollow, as a terminal's is in a window that lost the focus.
    /// A view has it until told otherwise.
    pub fn set_focused(&mut self, focused: bool) {
        self.focused = focused;
    }

    /// Leaves the scrollbar out (`true`), the text laid out across the
    /// whole width, or shows it again; it shows by default.
    pub fn set_scrollbar_hidden(&mut self, hidden: bool) {
        self.scrollbar_hidden = hidden;
    }

    /// Draws the text in `font`, its cells that font's in size, rather than
    /// in the window's (`None`): what a host whose text panes have a font of
    /// their own (a terminal's) hands the view.
    pub fn set_font(&mut self, font: Option<Font>) {
        self.font = font;
    }

    /// Draws the host's highlight ids as `styles` gives them, indexed by
    /// [`HighlightId::index`](cherry_engine::HighlightId::index), in place of the
    /// theme's syntax colors: what a host whose own highlighter numbers its
    /// ids (syntect's scopes) hands the view, again whenever its theme
    /// changes. An id past the end draws plain.
    pub fn set_highlight_styles(&mut self, styles: Vec<HighlightStyle>) {
        self.highlights = Some(styles);
    }

    /// The cursor's cell in the last frame, in physical pixels: in the
    /// prompt while one is open; `None` when it was out of sight or before
    /// the first frame. Where a host hangs a popup of its own from the
    /// cursor, such as a hover card.
    pub fn cursor_rect(&self) -> Option<Rect> {
        self.cursor_area
    }

    /// The cell char `offset` of the document shown is in, in physical
    /// pixels, as the view was last drawn; `None` when it is out of sight
    /// or before the first frame. Where a host hangs a popup of its own
    /// from the text, such as a diagnostic's card or a code action's list.
    pub fn offset_rect(&self, editor: &Editor, offset: usize) -> Option<Rect> {
        let layout = self.layout?;
        let (column, row) = editor.screen_cell(self.view, offset)?;
        Some(layout.cell_rect(column, row))
    }

    /// The view lent the `editor` it shows a view of, as a [`Widget`] to
    /// hand inputs and draw.
    pub fn lend<'a>(&'a mut self, editor: &'a mut Editor) -> LentEditor<'a> {
        LentEditor { editor, view: self }
    }

    /// The cursor's cell in the last frame, in physical pixels, while the
    /// view in `editor` takes text (Insert mode or a prompt); `None` in
    /// other modes, when the input method should be off, or when the
    /// cursor was out of sight. What the app hands
    /// [`ImeFocus::follow`](misty::ImeFocus::follow) while the view has the
    /// focus.
    pub fn ime_area(&self, editor: &Editor) -> Option<Rect> {
        self.cursor_area.filter(|_| takes_text(editor, self.view))
    }

    /// Settles every half-typed key sequence of `editor` whose timeout has
    /// passed, asking for a redraw when one did, and hands back what the
    /// settled sequences asked of the app and when the next timeout or
    /// glide step is due. Call it from
    /// [`Application::about_to_wait`](misty::Application::about_to_wait),
    /// act on the requests as on [`EditorResponse::Requests`], and return
    /// the deadline from there.
    pub fn about_to_wait(&self, editor: &mut Editor, cx: &mut EventCx<'_>) -> EditorWait {
        let now = Instant::now();
        if editor.tick(clock::stamp(now)) {
            cx.request_redraw();
        }
        let requests = editor.take_requests();
        if !requests.is_empty() {
            cx.request_redraw();
        }
        let glide = self.glide(cx);
        EditorWait {
            deadline: earliest(editor.deadline().map(clock::instant), glide),
            requests,
        }
    }

    /// Asks for the next frame of a scroll's glide or the scrollbar's fade
    /// still under way, returning when it is due; `None` at rest.
    /// [`EditorView::about_to_wait`] calls it; an app that ticks the editor
    /// itself calls this from
    /// [`Application::about_to_wait`](misty::Application::about_to_wait)
    /// instead, or the text stops on the glide's first frame.
    pub fn glide(&self, cx: &mut EventCx<'_>) -> Option<Instant> {
        let now = Instant::now();
        // The last frame drawn may have been a step short of the end.
        let glide = (self.drawn_offset != 0.0).then(|| self.motion.next_frame(now).unwrap_or(now));
        let next = earliest(glide, self.scrollbar.next_frame(now))?;
        cx.request_redraw_at(next);
        Some(next)
    }

    /// Gives the change bars in the gutter a tooltip saying `notes` of each
    /// kind of change, joined after any sign's note on the same line.
    pub fn set_change_notes(&mut self, notes: ChangeNotes) {
        self.change_notes = Some(notes);
    }

    /// Makes the text jump to a new scroll position (`true`) rather than
    /// glide, for users who asked their system for reduced motion.
    pub fn set_reduced_motion(&mut self, reduced_motion: bool) {
        self.motion.set_reduced_motion(reduced_motion);
    }
}

impl Widget for LentEditor<'_> {
    type Output = ();
    type Response = EditorResponse;

    /// Reacts to `input` through the view in the lent editor: keys are fed
    /// to the editor (hand them only while the view has the keyboard),
    /// input method text is composed at the cursor and typed while the
    /// view takes text, and the pointer moves the cursor, selects (Shift
    /// extends, quick presses select a word then a line), and scrolls.
    /// Asks for a redraw when the editor used the input, and hands back
    /// what the editor asked of the app.
    fn handle(&mut self, input: &Input, cx: &mut EventCx<'_>) -> EditorResponse {
        let (view, editor) = (&mut *self.view, &mut *self.editor);
        match input {
            Input::Ime(ime) => view.handle_ime(editor, cx, ime),
            Input::Key(key) => {
                if view.tooltip.dismiss() {
                    cx.request_redraw();
                }
                view.handle_key(editor, cx, *key)
            }
            // An editor acts on presses alone.
            Input::KeyReleased(_) => EditorResponse::Ignored,
            Input::AppearanceChanged(_)
            | Input::CloseRequested
            | Input::FileDropped(_)
            | Input::FileHovered(_)
            | Input::FileHoverCancelled
            | Input::FocusGained
            | Input::FocusLost
            | Input::PointerButton(_)
            | Input::PointerLeft
            | Input::PointerMoved(_)
            | Input::Wheel(_) => view.handle_mouse(editor, cx, input),
            // Input misty adds later is none the editor acts on yet.
            _ => EditorResponse::Ignored,
        }
    }

    /// Draws the view in `area`, resizing it to the whole cells that fit:
    /// text in the theme's foreground, the cursor line on the hover color,
    /// the selection on the selection color, search matches on the warning
    /// color, line numbers muted, and the cursor as the editor shapes it.
    /// An open prompt takes the bottom row and the cursor.
    fn draw(&mut self, frame: &mut Frame, area: Rect, theme: &Theme) {
        self.view.draw_view(self.editor, frame, area, theme);
    }
}

impl Outcome for EditorResponse {
    fn is_ignored(&self) -> bool {
        matches!(self, Self::Ignored)
    }
}

// ========================================================================
// EditorClipboard Implementations
// ========================================================================

impl EditorClipboard {
    /// `clipboard` as the one an editor copies to and pastes from.
    pub fn new(clipboard: SystemClipboard) -> Self {
        Self(clipboard)
    }
}

impl Clipboard for EditorClipboard {
    fn read(&mut self, selection: ClipboardSelection) -> Option<String> {
        self.0.read_text(clipboard_kind(selection))
    }

    fn write(&mut self, selection: ClipboardSelection, text: &str) {
        // The text is kept in the process either way; Cherry has no way to
        // hear that other apps will not see it.
        let _ = self.0.write_text(clipboard_kind(selection), text);
    }
}

// ========================================================================
// ViewLayout Implementations
// ========================================================================

impl ViewLayout {
    /// The rect of the cell at `column` and `row` of the view.
    fn cell_rect(&self, column: u16, row: u16) -> Rect {
        let metrics = self.metrics;
        Rect::new(
            self.area.x + f32::from(column) * metrics.width,
            self.area.y + f32::from(row) * metrics.height,
            metrics.width,
            metrics.height,
        )
    }

    /// The cell column and row under `point`, from the view's top left
    /// corner: `None` outside the view, unless `clamp` asks for the
    /// nearest cell on the view's edge, as a drag past the edge wants.
    fn cell_at(&self, point: Point, clamp: bool) -> Option<(u16, u16)> {
        let (width, height) = (self.metrics.width, self.metrics.height);
        if width <= 0.0 || height <= 0.0 || self.area.width <= 0.0 || self.area.height <= 0.0 {
            return None;
        }
        if !clamp && !self.area.contains(point) {
            return None;
        }
        let cols = (self.area.width / width).round().max(1.0);
        let rows = (self.area.height / height).round().max(1.0);
        let column = ((point.x - self.area.x) / width)
            .floor()
            .clamp(0.0, cols - 1.0);
        let row = ((point.y - self.area.y) / height)
            .floor()
            .clamp(0.0, rows - 1.0);
        Some((column as u16, row as u16))
    }
}

// ========================================================================
// Functions
// ========================================================================

/// The misty clipboard a Cherry clipboard `selection` is.
fn clipboard_kind(selection: ClipboardSelection) -> ClipboardKind {
    match selection {
        ClipboardSelection::Clipboard => ClipboardKind::Clipboard,
        ClipboardSelection::Primary => ClipboardKind::Primary,
    }
}

/// The earlier of two optional deadlines: whichever is set when only one
/// is, and `None` when neither is.
fn earliest(first: Option<Instant>, second: Option<Instant>) -> Option<Instant> {
    match (first, second) {
        (Some(first), Some(second)) => Some(first.min(second)),
        (Some(only), None) | (None, Some(only)) => Some(only),
        (None, None) => None,
    }
}

/// What became of an event the editor took, `handled`, with what it asks
/// of the app taken from `editor`; asks for a redraw when the editor used
/// the event or asked anything.
fn respond(editor: &mut Editor, cx: &mut EventCx<'_>, handled: Handled) -> EditorResponse {
    let requests = editor.take_requests();
    match (handled, requests.is_empty()) {
        (Handled::Ignored, true) => EditorResponse::Ignored,
        (Handled::Consumed, true) => {
            cx.request_redraw();
            EditorResponse::Handled
        }
        (Handled::Consumed | Handled::Ignored, false) => {
            cx.request_redraw();
            EditorResponse::Requests(requests)
        }
    }
}

// ========================================================================
// Tests
// ========================================================================

#[cfg(test)]
mod tests {
    use std::ops::Range;

    use cherry_engine::{
        Decoration, DecorationKind, EditorOptions, Layers, LineChange, Mode, MouseEventKind, Sign,
        render_view,
    };
    use misty::accesskit::{Role, TextPosition, TextSelection};
    use misty::testing::{TestWindow, access_node, access_tree};
    use misty::{
        CellColor, CellSource, CellWidth, Composition, FocusId, ImeInput, UnderlineStyle,
        WheelDelta,
    };

    use crate::keys::to_cherry;

    use super::access::node_id;
    use super::draw::{
        MarkKind, PHRASE_COLORS, cell_style, fill_cells, guide_color, highlight_colors, is_guide,
        overlay_preedit, rows_line_up,
    };
    use super::input::{PointerInput, typed_key};
    use super::*;

    /// A key timeout a test can wait out.
    const SHORT_TIMEOUT: Duration = Duration::from_millis(1);

    /// The preedit an input method composing `text`, its cursor at the
    /// byte range `cursor`, makes.
    fn preedit(text: &str, cursor: Option<(usize, usize)>) -> Preedit {
        let mut preedit = None;
        let composition = Composition {
            cursor,
            text: text.to_owned(),
        };
        Preedit::apply(&mut preedit, &ImeInput::Preedit(composition));
        preedit.expect("a preedit")
    }

    /// The cells of a view 10 by 3 onto `text`, painted by Cherry, with
    /// `keys` typed first.
    fn painted(text: &str, keys: &[&str], theme: &Theme) -> CellBuffer {
        let mut editor = Editor::new(EditorOptions::default());
        let document = editor.open_document(text);
        let view = editor.new_view(document).expect("a view");
        editor.resize_view(view, 10, 3);
        for spec in keys {
            let key = cherry_engine::keys::parse_key(spec).expect("a valid key");
            editor.handle_key(view, key, clock::now());
        }
        let mut surface = Surface::new(0, 0);
        let cursor = render_view(&editor, view, &mut surface);
        let mut cells = CellBuffer::new(0, 0);
        fill_cells(
            &surface,
            cursor,
            theme,
            &highlight_colors(theme),
            &mut cells,
            &mut Vec::new(),
        );
        cells
    }

    #[test]
    fn test_a_sequence_that_times_out_hands_back_what_it_asked() {
        let options = EditorOptions {
            key_timeout: SHORT_TIMEOUT,
            ..EditorOptions::default()
        };
        let mut editor = Editor::new(options);
        let document = editor.open_document("text\n");
        let view = editor.new_view(document).expect("a view");
        // `C-s` alone saves, and also starts a longer binding, so it waits.
        editor.bind(Mode::Normal, &["C-s"], "save").expect("binds");
        editor
            .bind(Mode::Normal, &["C-s q"], "quit")
            .expect("binds");
        let key = cherry_engine::keys::parse_key("C-s").expect("a valid key");
        editor.handle_key(view, key, clock::now());
        assert!(editor.take_requests().is_empty(), "the sequence is pending");
        std::thread::sleep(SHORT_TIMEOUT * 10);
        let mut window = TestWindow::new(200, 120);
        let wait = EditorView::new(view).about_to_wait(&mut editor, &mut window.cx());
        assert!(
            matches!(wait.requests.as_slice(), [HostRequest::Save(save)] if save.document == document),
            "{:?}",
            wait.requests
        );
        assert_eq!(wait.deadline, None, "nothing is pending any more");
    }

    #[test]
    fn test_guides_draw_as_lines_and_phrases_take_colors_of_their_own() {
        let theme = Theme::dark();
        let options = EditorOptions {
            indent_guides: true,
            ..EditorOptions::default()
        };
        let mut editor = Editor::new(options);
        let document = editor.open_document("ab ab\n    cd");
        let view = editor.new_view(document).expect("a view");
        for spec in ["M-s", "h", "."] {
            let key = cherry_engine::keys::parse_key(spec).expect("a valid key");
            editor.handle_key(view, key, clock::now());
        }
        let cells = cells_of(&mut editor, view, &theme);
        let at = |row: usize, col: usize| cells.cell(row, col).expect("in the grid").clone();
        assert_eq!(at(1, 0).ch, '\u{2502}', "a guide where the indent starts");
        assert!(is_guide(&at(1, 0)), "its glyph hidden, drawn as a hairline");
        assert_eq!(
            at(1, 0).style.foreground,
            CellColor::Rgb(guide_color(&theme))
        );
        assert_ne!(guide_color(&theme), theme.border, "fainter than a border");
        assert!(!is_guide(&at(1, 1)));
        assert_eq!(at(1, 1).ch, ' ', "no guide between levels");
        let phrase = CellColor::Rgb(theme.palette.colors[PHRASE_COLORS[0]]);
        assert_eq!(at(0, 3).style.background, phrase, "the second ab lit too");
    }

    #[test]
    fn test_lit_occurrences_are_outlined_runs_with_no_fill_of_their_own() {
        let theme = Theme::dark();
        let options = EditorOptions {
            light_occurrences: true,
            ..EditorOptions::default()
        };
        let mut editor = Editor::new(options);
        let document = editor.open_document("ab x ab\nab");
        let view = editor.new_view(document).expect("a view");
        editor.resize_view(view, 10, 3);
        let mut surface = Surface::new(0, 0);
        let cursor = render_view(&editor, view, &mut surface);
        let (mut cells, mut marks) = (CellBuffer::new(0, 0), Vec::new());
        let colors = highlight_colors(&theme);
        fill_cells(&surface, cursor, &theme, &colors, &mut cells, &mut marks);
        let runs: Vec<(usize, Range<usize>)> = marks
            .iter()
            .map(|run| (run.row, run.cols.clone()))
            .collect();
        assert_eq!(runs, [(0, 0..2), (0, 5..7), (1, 0..2)], "one run a word");
        let below = cells.cell(1, 0).expect("in the grid");
        assert_eq!(below.style.background, CellColor::Default, "no fill");
    }

    #[test]
    fn test_the_gutter_says_a_line_s_change_and_its_signs_notes_and_the_text_nothing() {
        let options = EditorOptions {
            sign_column: true,
            ..EditorOptions::default()
        };
        let mut editor = Editor::new(options);
        let document = editor.open_document("a\nb\nc");
        let changes = [cherry_engine::ChangedLine {
            change: LineChange::Modified,
            line: 1,
        }];
        editor.set_line_changes(document, "git", changes.to_vec());
        let sign =
            cherry_engine::Sign::new(1, " ●", DecorationKind::Error).with_note("no such name");
        editor.set_signs(document, "lsp", vec![sign]);
        let view = editor.new_view(document).expect("a view");
        editor.resize_view(view, 10, 3);
        assert_eq!(
            editor.line_change_at(view, 0, 1),
            Some(LineChange::Modified)
        );
        assert_eq!(
            editor.sign_note_at(view, 0, 1).as_deref(),
            Some("no such name")
        );
        assert_eq!(editor.line_change_at(view, 0, 0), None, "an unchanged line");
        assert_eq!(editor.sign_note_at(view, 9, 1), None, "a cell of the text");
    }

    #[test]
    fn test_a_changed_line_marks_its_sign_cell_and_keeps_the_sign_s_text() {
        let theme = Theme::dark();
        let options = EditorOptions {
            sign_column: true,
            ..EditorOptions::default()
        };
        let mut editor = Editor::new(options);
        let document = editor.open_document("a\nb\nc");
        let changes = [(0, LineChange::Added), (2, LineChange::Deleted)];
        let changes = changes.map(|(line, change)| cherry_engine::ChangedLine { change, line });
        editor.set_line_changes(document, "git", changes.to_vec());
        let sign = cherry_engine::Sign::new(0, " ●", DecorationKind::Error);
        editor.set_signs(document, "lsp", vec![sign]);
        let view = editor.new_view(document).expect("a view");
        editor.resize_view(view, 10, 3);
        let mut surface = Surface::new(0, 0);
        let cursor = render_view(&editor, view, &mut surface);
        let (mut cells, mut marks) = (CellBuffer::new(0, 0), Vec::new());
        let colors = highlight_colors(&theme);
        fill_cells(&surface, cursor, &theme, &colors, &mut cells, &mut marks);
        let shown: Vec<(usize, Range<usize>, MarkKind)> = marks
            .iter()
            .map(|mark| (mark.row, mark.cols.clone(), mark.kind))
            .collect();
        let added = MarkKind::Change(LineChange::Added);
        let deleted = MarkKind::Change(LineChange::Deleted);
        let dot = MarkKind::Problem {
            color: theme.danger,
            filled: true,
        };
        assert_eq!(
            shown,
            [(0, 0..1, added), (0, 1..2, dot), (2, 0..1, deleted)]
        );
        let under_the_dot = cells.cell(0, 1).expect("in the grid");
        assert_eq!(
            under_the_dot.ch, ' ',
            "drawn as a shape, not the font's glyph"
        );
    }

    /// The cells of `editor`'s `view`, 10 by 3, as `theme` draws them.
    fn cells_of(editor: &mut Editor, view: ViewId, theme: &Theme) -> CellBuffer {
        editor.resize_view(view, 10, 3);
        let mut surface = Surface::new(0, 0);
        let cursor = render_view(editor, view, &mut surface);
        let mut cells = CellBuffer::new(0, 0);
        fill_cells(
            &surface,
            cursor,
            theme,
            &highlight_colors(theme),
            &mut cells,
            &mut Vec::new(),
        );
        cells
    }

    /// An editor widget whose view was drawn 10 cells by 3 of 10 by 20
    /// pixels at the window's top left corner, onto `text`.
    fn drawn_view(text: &str) -> (Editor, EditorView) {
        let mut editor = Editor::new(EditorOptions::default());
        let document = editor.open_document(text);
        let id = editor.new_view(document).expect("a view");
        editor.resize_view(id, 10, 3);
        let mut view = EditorView::new(id);
        view.layout = Some(ViewLayout {
            area: Rect::new(0.0, 0.0, 100.0, 60.0),
            metrics: CellMetrics {
                baseline: 15.0,
                height: 20.0,
                width: 10.0,
            },
        });
        (editor, view)
    }

    /// Cells showing `rows`, one line of text each, 4 columns wide.
    fn lines(rows: &[&str]) -> CellBuffer {
        let mut cells = CellBuffer::new(4, rows.len());
        for (row, text) in rows.iter().enumerate() {
            for (col, ch) in text.chars().enumerate() {
                cells.set(row, col, misty::Cell::new(ch, misty::CellStyle::default()));
            }
        }
        cells
    }

    #[test]
    fn test_rows_line_up_only_when_moved_by_the_lines_scrolled() {
        let top = lines(&["one", "two", "six"]);
        let down = lines(&["two", "six", "ten"]);
        assert!(rows_line_up(&top, &down, 1), "scrolled a line down");
        assert!(rows_line_up(&down, &top, -1), "and back up");
        assert!(!rows_line_up(&top, &down, 2));
        assert!(!rows_line_up(&top, &lines(&["two", "six"]), 1), "resized");
    }

    #[test]
    fn test_an_auto_scrollbar_waits_still_until_the_view_scrolls_then_fades() {
        let theme = Theme {
            scrollbar_show: misty::ScrollbarShow::Auto,
            ..Theme::dark()
        };
        let window = TestWindow::new(400, 400);
        let metrics = window.frame().cell_metrics();
        let text: String = (0..200).map(|line| format!("line {line}\n")).collect();
        let mut editor = Editor::new(EditorOptions::default());
        let document = editor.open_document(&text);
        let id = editor.new_view(document).expect("a view");
        let mut view = EditorView::new(id);
        let area = Rect::new(0.0, 0.0, 20.0 * metrics.width, 10.0 * metrics.height);
        let draw = |editor: &mut Editor, view: &mut EditorView| {
            let mut frame = window.frame();
            view.lend(editor).draw(&mut frame, area, &theme);
        };
        draw(&mut editor, &mut view);
        let now = Instant::now();
        assert_eq!(view.scrollbar.next_frame(now), None, "hidden and still");
        let key = cherry_engine::keys::parse_key("G").expect("a key");
        editor.handle_key(id, key, clock::now());
        draw(&mut editor, &mut view);
        assert!(
            view.scrollbar.next_frame(Instant::now()).is_some(),
            "shown by the scroll, with a fade to come"
        );
    }

    #[test]
    fn test_a_short_scroll_glides_and_a_long_one_jumps() {
        let theme = Theme::dark();
        let window = TestWindow::new(400, 400);
        let metrics = window.frame().cell_metrics();
        let text: String = (0..200).map(|line| format!("line {line}\n")).collect();
        let mut editor = Editor::new(EditorOptions::default());
        let document = editor.open_document(&text);
        let id = editor.new_view(document).expect("a view");
        let mut view = EditorView::new(id);
        // Ten rows, so twelve lines down is a short scroll.
        let area = Rect::new(0.0, 0.0, 20.0 * metrics.width, 10.0 * metrics.height);
        let draw = |editor: &mut Editor, view: &mut EditorView| {
            let mut frame = window.frame();
            view.lend(editor).draw(&mut frame, area, &theme);
        };
        draw(&mut editor, &mut view);
        for _ in 0..12 {
            let key = cherry_engine::keys::parse_key("j").expect("a key");
            editor.handle_key(id, key, clock::now());
        }
        draw(&mut editor, &mut view);
        assert!(
            view.motion.is_animating(Instant::now()),
            "a short scroll glides"
        );
        assert!(view.trail.first().is_some_and(|glided| glided.shift > 0));
        let key = cherry_engine::keys::parse_key("G").expect("a key");
        editor.handle_key(id, key, clock::now());
        draw(&mut editor, &mut view);
        assert!(
            !view.motion.is_animating(Instant::now()),
            "a long one jumps"
        );
    }

    #[test]
    fn test_a_scroll_mid_glide_keeps_every_frame_still_in_sight_and_asks_for_the_next() {
        let theme = Theme::dark();
        let mut window = TestWindow::new(400, 400);
        let metrics = window.frame().cell_metrics();
        let text: String = (0..200).map(|line| format!("line {line}\n")).collect();
        let mut editor = Editor::new(EditorOptions::default());
        let document = editor.open_document(&text);
        let id = editor.new_view(document).expect("a view");
        let mut view = EditorView::new(id);
        let area = Rect::new(0.0, 0.0, 20.0 * metrics.width, 10.0 * metrics.height);
        let draw = |editor: &mut Editor, view: &mut EditorView, window: &TestWindow| {
            let mut frame = window.frame();
            view.lend(editor).draw(&mut frame, area, &theme);
        };
        let scroll = |editor: &mut Editor| {
            let key = cherry_engine::keys::parse_key("C-e").expect("a key");
            editor.handle_key(id, key, clock::now());
        };
        draw(&mut editor, &mut view, &window);
        assert_eq!(view.glide(&mut window.cx()), None, "at rest");
        scroll(&mut editor);
        draw(&mut editor, &mut view, &window);
        scroll(&mut editor);
        draw(&mut editor, &mut view, &window);
        let shifts: Vec<isize> = view.trail.iter().map(|glided| glided.shift).collect();
        assert_eq!(shifts, [1, 2], "both frames before, newest first");
        assert!(
            view.glide(&mut window.cx()).is_some(),
            "the next frame asked for"
        );
        view.motion.jump_to(0.0);
        draw(&mut editor, &mut view, &window);
        assert!(view.trail.is_empty(), "nothing kept at rest");
        assert_eq!(view.glide(&mut window.cx()), None);
    }

    #[test]
    fn test_a_wide_character_becomes_a_wide_cell_and_its_continuation() {
        let cells = painted("a漢b", &[], &Theme::dark());
        assert_eq!((cells.cols(), cells.rows()), (10, 3));
        let cell = |col: usize| cells.cell(0, col).expect("a cell");
        assert_eq!((cell(1).ch, cell(1).width), ('漢', CellWidth::Wide));
        assert_eq!(cell(2).width, CellWidth::Continuation);
        assert_eq!((cell(3).ch, cell(3).width), ('b', CellWidth::Single));
    }

    #[test]
    fn test_the_cursor_keeps_its_place_and_shape_from_the_editor() {
        let theme = Theme::dark();
        let normal = painted("abc", &["l"], &theme).cursor().expect("a cursor");
        assert_eq!((normal.row, normal.col), (0, 1));
        assert_eq!(normal.shape, misty::CursorShape::Block);
        let insert = painted("abc", &["l", "i"], &theme)
            .cursor()
            .expect("a cursor");
        assert_eq!(insert.shape, misty::CursorShape::Bar);
    }

    #[test]
    fn test_a_preedit_covers_cells_from_the_cursor_underlined_with_the_caret_inside() {
        let mut cells = painted("abcdef", &["i"], &Theme::dark());
        let preedit = preedit("x漢y", Some((1, 1)));
        overlay_preedit(&mut cells, &preedit);
        let cell = |col: usize| cells.cell(0, col).expect("a cell");
        assert_eq!(
            (cell(0).ch, cell(0).style.underline),
            ('x', UnderlineStyle::Single)
        );
        assert_eq!((cell(1).ch, cell(1).width), ('漢', CellWidth::Wide));
        assert_eq!(cell(2).width, CellWidth::Continuation);
        assert_eq!(cell(3).ch, 'y');
        assert_eq!(cell(4).ch, 'e', "the cells past the preedit stay");
        let cursor = cells.cursor().expect("a cursor");
        assert_eq!((cursor.col, cursor.shape), (1, misty::CursorShape::Bar));
    }

    #[test]
    fn test_a_preedit_stops_at_the_row_end_and_a_hidden_caret_hides_the_cursor() {
        let mut cells = painted("abcdefgh", &["$", "a"], &Theme::dark());
        let preedit = preedit("漢漢漢", None);
        overlay_preedit(&mut cells, &preedit);
        // The cursor sits at column 8 of 10: one wide character fits.
        let cell = |col: usize| cells.cell(0, col).expect("a cell");
        assert_eq!((cell(8).ch, cell(8).width), ('漢', CellWidth::Wide));
        assert_eq!(cells.cursor(), None);
    }

    #[test]
    fn test_a_commit_types_into_insert_mode_and_is_dropped_in_normal_mode() {
        let mut editor = Editor::new(EditorOptions::default());
        let document = editor.open_document("");
        let view = editor.new_view(document).expect("a view");
        assert!(!takes_text(&editor, view));
        let i = cherry_engine::keys::parse_key("i").expect("a valid key");
        editor.handle_key(view, i, clock::now());
        assert!(takes_text(&editor, view));
        for key in "你好\n".chars().filter_map(typed_key) {
            let key = to_cherry(key).expect("a typed char is a Cherry key");
            editor.handle_key(view, key, clock::now());
        }
        let text = editor
            .document(document)
            .expect("a document")
            .text_with_line_endings();
        assert_eq!(text, "你好\n");
        assert_eq!(typed_key('\u{7}'), None);
    }

    #[test]
    fn test_an_open_prompt_takes_the_bottom_row_and_the_cursor() {
        let theme = Theme::dark();
        let mut editor = Editor::new(EditorOptions::default());
        let document = editor.open_document("ab\ncd\nef\ngh");
        let view = editor.new_view(document).expect("a view");
        let key = cherry_engine::keys::parse_key("/").expect("a valid key");
        editor.handle_key(view, key, clock::now());
        let mut editor_view = EditorView::new(view);
        let mut rows = 0;
        let Some(snapshot) = TestWindow::new(200, 120).render(|frame| {
            let metrics = frame.cell_metrics();
            let area = Rect::new(0.0, 0.0, 10.0 * metrics.width, 3.0 * metrics.height);
            editor_view.lend(&mut editor).draw(frame, area, &theme);
            rows = editor.view(view).map_or(0, |view| view.rows());
        }) else {
            return;
        };
        assert_eq!(rows, 2, "the view gave up a row");
        let metrics = snapshot.metrics();
        let prompt_row = 2.5 * metrics.height;
        snapshot.assert_pixel(metrics.width, prompt_row, theme.cursor, "the prompt's bar");
        snapshot.assert_pixel(1.0, 1.0, theme.cursor_line, "no block cursor in the view");
    }

    #[test]
    fn test_presses_drags_and_releases_become_cells_clamped_while_dragging() {
        let (_, mut view) = drawn_view("");
        let now = Instant::now();
        let mut feed = |input| -> Vec<(MouseEventKind, u16, u16)> {
            view.pointer_events(input, now)
                .into_iter()
                .map(|event| (event.kind, event.column, event.row))
                .collect()
        };
        assert!(feed(PointerInput::Moved(Point::new(15.0, 25.0))).is_empty());
        assert_eq!(feed(PointerInput::Pressed), [(MouseEventKind::Press, 1, 1)]);
        assert_eq!(
            feed(PointerInput::Moved(Point::new(35.0, 500.0))),
            [(MouseEventKind::Drag, 3, 2)],
            "a drag below the view stays on its last row"
        );
        assert_eq!(
            feed(PointerInput::Released),
            [(MouseEventKind::Release, 3, 2)]
        );
        assert!(feed(PointerInput::Moved(Point::new(45.0, 5.0))).is_empty());
        assert!(feed(PointerInput::Moved(Point::new(150.0, 5.0))).is_empty());
        assert!(feed(PointerInput::Pressed).is_empty(), "outside the view");
        assert!(feed(PointerInput::Released).is_empty(), "nothing pressed");
    }

    #[test]
    fn test_quick_presses_on_one_cell_count_as_a_double_click() {
        let (_, mut view) = drawn_view("");
        let now = Instant::now();
        view.pointer_events(PointerInput::Moved(Point::new(15.0, 5.0)), now);
        let kinds: Vec<MouseEventKind> = [
            PointerInput::Pressed,
            PointerInput::Released,
            PointerInput::Pressed,
        ]
        .into_iter()
        .flat_map(|input| view.pointer_events(input, now))
        .map(|event| event.kind)
        .collect();
        assert_eq!(
            kinds,
            [
                MouseEventKind::Press,
                MouseEventKind::Release,
                MouseEventKind::DoubleClick
            ]
        );
    }

    #[test]
    fn test_wheel_notches_scroll_and_touchpad_pixels_add_up_to_notches() {
        let (_, mut view) = drawn_view("");
        let now = Instant::now();
        view.pointer_events(PointerInput::Moved(Point::new(15.0, 5.0)), now);
        let mut kinds = |delta| -> Vec<MouseEventKind> {
            view.pointer_events(PointerInput::Wheel(delta), now)
                .into_iter()
                .map(|event| event.kind)
                .collect()
        };
        assert_eq!(
            kinds(WheelDelta::Lines(Point::new(0.0, -2.0))),
            [MouseEventKind::ScrollDown, MouseEventKind::ScrollDown]
        );
        // A notch is three rows of 20 pixels; 30 pixels is half of one.
        let pixels = WheelDelta::Pixels(Point::new(0.0, 30.0));
        assert!(kinds(pixels).is_empty());
        assert_eq!(kinds(pixels), [MouseEventKind::ScrollUp]);
    }

    #[test]
    fn test_a_press_on_the_scrollbars_track_scrolls_there_and_hiding_it_widens_the_text() {
        let text: String = (0..100).map(|line| format!("line {line}\n")).collect();
        let mut editor = Editor::new(EditorOptions::default());
        let document = editor.open_document(&text);
        let id = editor.new_view(document).expect("a view");
        let mut view = EditorView::new(id);
        let window = TestWindow::new(200, 120);
        let area = Rect::new(0.0, 0.0, 200.0, 120.0);
        let draw = |editor: &mut Editor, view: &mut EditorView| {
            let mut frame = window.frame();
            view.lend(editor).draw(&mut frame, area, &Theme::dark());
            editor.view(id).map(|shown| shown.cols()).expect("the view")
        };
        let narrow = draw(&mut editor, &mut view);
        let press = |pressed| {
            Input::PointerButton(misty::ButtonInput {
                button: misty::PointerButton::Left,
                mods: Mods::NONE,
                position: Point::new(area.width - 2.0, area.height - 2.0),
                pressed,
            })
        };
        let mut window_cx = TestWindow::new(200, 120);
        let response = view
            .lend(&mut editor)
            .handle(&press(true), &mut window_cx.cx());
        assert_eq!(
            response,
            EditorResponse::Handled,
            "the bar's, not the text's"
        );
        view.lend(&mut editor)
            .handle(&press(false), &mut window_cx.cx());
        let top = editor.view(id).map(|shown| shown.scroll().line);
        assert!(top.is_some_and(|line| line > 0), "paged down: {top:?}");
        view.set_scrollbar_hidden(true);
        let wide = draw(&mut editor, &mut view);
        assert!(wide > narrow, "{wide} columns beside the bar's {narrow}");
    }

    #[test]
    fn test_a_press_moves_the_editor_cursor_to_the_character_under_the_pointer() {
        let (mut editor, mut view) = drawn_view("abc\ndef\n");
        let now = Instant::now();
        view.pointer_events(PointerInput::Moved(Point::new(15.0, 25.0)), now);
        for event in view.pointer_events(PointerInput::Pressed, now) {
            editor.handle_mouse(view.view(), event);
        }
        let cursor = editor.view(view.view()).map(|view| view.cursor());
        assert_eq!(cursor, Some(5), "line 2, column 2");
    }

    #[test]
    fn test_decorations_draw_undercurls_and_signs_in_their_kind_color() {
        let theme = Theme::dark();
        let options = EditorOptions {
            sign_column: true,
            ..EditorOptions::default()
        };
        let mut editor = Editor::new(options);
        let document = editor.open_document("abc def\n");
        let view = editor.new_view(document).expect("a view");
        let decorations = vec![
            Decoration::new(0..3, DecorationKind::Error),
            Decoration::new(4..7, DecorationKind::Hint),
        ];
        editor.set_decorations(document, "test", decorations);
        editor.set_signs(
            document,
            "test",
            vec![Sign::new(0, "W", DecorationKind::Warning)],
        );
        let cells = cells_of(&mut editor, view, &theme);
        let row: Vec<&misty::Cell> = (0..cells.cols())
            .filter_map(|col| cells.cell(0, col))
            .collect();
        let sign = row.iter().find(|cell| cell.ch == 'W').expect("the sign");
        assert_eq!(sign.style.foreground, CellColor::Rgb(theme.warning));
        let text = row
            .iter()
            .position(|cell| cell.ch == 'a')
            .expect("the text");
        let underline = |col: usize| (row[col].style.underline, row[col].style.underline_color);
        assert_eq!(
            underline(text),
            (UnderlineStyle::Curly, CellColor::Rgb(theme.danger))
        );
        assert_eq!(underline(text + 3).0, UnderlineStyle::None, "the space");
        assert_eq!(
            underline(text + 4),
            (UnderlineStyle::Dotted, CellColor::Rgb(theme.muted))
        );
    }

    #[test]
    fn test_an_error_undercurl_is_drawn_in_the_danger_color_under_its_text() {
        let theme = Theme::dark();
        let mut editor = Editor::new(EditorOptions::default());
        let document = editor.open_document("abc\n");
        let view = editor.new_view(document).expect("a view");
        let decorations = vec![Decoration::new(0..3, DecorationKind::Error)];
        editor.set_decorations(document, "test", decorations);
        let key = cherry_engine::keys::parse_key("j").expect("a valid key");
        editor.handle_key(view, key, clock::now());
        let mut editor_view = EditorView::new(view);
        let Some(snapshot) = TestWindow::new(200, 120).render(|frame| {
            let metrics = frame.cell_metrics();
            let area = Rect::new(0.0, 0.0, 10.0 * metrics.width, 3.0 * metrics.height);
            editor_view.lend(&mut editor).draw(frame, area, &theme);
        }) else {
            return;
        };
        let metrics = snapshot.metrics();
        let band = |col: f32| {
            Rect::new(
                (col + 1.0) * metrics.width,
                metrics.height * 0.6,
                metrics.width * 0.2,
                metrics.height * 0.4,
            )
        };
        assert!(
            snapshot.contains_color(band(1.0), theme.danger),
            "an undercurl under the decorated text"
        );
        assert!(
            !snapshot.contains_color(band(4.0), theme.danger),
            "none past it"
        );
    }

    #[test]
    fn test_an_open_completion_menu_is_drawn_under_the_completed_text() {
        let theme = Theme::dark();
        let mut editor = Editor::new(EditorOptions::default());
        let document = editor.open_document("alpha\nalpine\n");
        let view = editor.new_view(document).expect("a view");
        for spec in ["G", "o", "a", "l", "C-x", "C-n"] {
            let key = cherry_engine::keys::parse_key(spec).expect("a valid key");
            editor.handle_key(view, key, clock::now());
        }
        let items = editor
            .view(view)
            .and_then(|view| view.completion())
            .map(|completion| completion.items().len());
        assert_eq!(items, Some(2), "alpha and alpine");
        let mut editor_view = EditorView::new(view);
        let Some(snapshot) = TestWindow::new(300, 200).render(|frame| {
            let metrics = frame.cell_metrics();
            let area = Rect::new(0.0, 0.0, 20.0 * metrics.width, 6.0 * metrics.height);
            editor_view.lend(&mut editor).draw(frame, area, &theme);
        }) else {
            return;
        };
        let metrics = snapshot.metrics();
        // The new line is the third; the menu's first row is the fourth.
        let first_row = Rect::new(
            0.0,
            3.0 * metrics.height,
            12.0 * metrics.width,
            metrics.height,
        );
        assert!(
            snapshot.contains_color(first_row, theme.selection),
            "the typed match is lit"
        );
        assert!(
            snapshot.contains_color(first_row, theme.border),
            "inside the popup's border"
        );
    }

    #[test]
    fn test_the_view_describes_its_lines_as_text_runs_with_the_cursor() {
        let id = FocusId::new("editor");
        let mut editor = Editor::new(EditorOptions::default());
        let document = editor.open_document("ab\ncd\n");
        let view = editor.new_view(document).expect("a view");
        let mut editor_view = EditorView::new(view);
        let mut frame = TestWindow::new(400, 300).frame();
        for spec in ["j", "l"] {
            let key = cherry_engine::keys::parse_key(spec).expect("a valid key");
            editor.handle_key(view, key, clock::now());
        }
        let area = Rect::new(0.0, 0.0, 400.0, 300.0);
        editor_view
            .lend(&mut editor)
            .draw(&mut frame, area, &Theme::dark());
        editor_view.access(&editor, &mut frame, id);
        let update = access_tree(&frame, "t");
        let node = |id| access_node(&update, id).expect("described");
        assert_eq!(node(id).role(), Role::MultilineTextInput);
        let second = node(id.child(1));
        assert_eq!(
            (second.role(), second.value()),
            (Role::TextRun, Some("cd\n"))
        );
        assert_eq!(second.character_lengths(), [1, 1, 1]);
        let caret = TextPosition {
            character_index: 1,
            node: node_id(id.child(1)),
        };
        assert_eq!(
            node(id).text_selection().copied(),
            Some(TextSelection {
                anchor: caret,
                focus: caret
            }),
            "the cursor, on line 2 column 2"
        );
    }

    #[test]
    fn test_a_visual_selection_is_described_from_its_anchor_through_the_cursor() {
        let id = FocusId::new("editor");
        let mut editor = Editor::new(EditorOptions::default());
        let document = editor.open_document("ab\ncd\nef\n");
        let view = editor.new_view(document).expect("a view");
        let mut editor_view = EditorView::new(view);
        let area = Rect::new(0.0, 0.0, 400.0, 300.0);
        let mut selection_after = |keys: &[&str]| {
            for spec in keys {
                let key = cherry_engine::keys::parse_key(spec).expect("a valid key");
                editor.handle_key(view, key, clock::now());
            }
            let mut frame = TestWindow::new(400, 300).frame();
            editor_view
                .lend(&mut editor)
                .draw(&mut frame, area, &Theme::dark());
            editor_view.access(&editor, &mut frame, id);
            let update = access_tree(&frame, "t");
            let selection = access_node(&update, id)
                .expect("described")
                .text_selection();
            selection.map(|selection| {
                let at = |position: TextPosition| {
                    let line = (0..3).find(|line| node_id(id.child(*line)) == position.node);
                    (line.expect("a line in sight"), position.character_index)
                };
                (at(selection.anchor), at(selection.focus))
            })
        };
        assert_eq!(
            selection_after(&["l", "v", "j"]),
            Some(((0, 1), (1, 2))),
            "both ends included: b through d"
        );
        assert_eq!(
            selection_after(&["o"]),
            Some(((1, 2), (0, 1))),
            "o swaps the ends, so the focus is before the anchor"
        );
        assert_eq!(
            selection_after(&["V"]),
            Some(((2, 0), (0, 0))),
            "whole lines, from the end of line 2 back to the start of line 1"
        );
        assert_eq!(
            selection_after(&["<esc>"]),
            Some(((0, 1), (0, 1))),
            "collapsed at the cursor again in Normal mode"
        );
    }

    /// A painted cell showing `x` in `style`, highlighted as `highlight`.
    fn painted_cell(style: cherry_engine::Style, highlight: Option<u16>) -> cherry_engine::Cell {
        cherry_engine::Cell {
            highlight: highlight.map(cherry_engine::HighlightId::new),
            layers: Layers::default(),
            style,
            symbol: "x".to_owned(),
            underline: None,
        }
    }

    /// Host styles: id 0 plain, id 1 a bold red text color, id 2 a green
    /// wash with a blue text color.
    fn host_styles() -> Vec<HighlightStyle> {
        vec![
            HighlightStyle::default(),
            HighlightStyle {
                attributes: Attributes::BOLD,
                background: None,
                foreground: Some(Rgb::new(200, 0, 0)),
            },
            HighlightStyle {
                attributes: Attributes::NONE,
                background: Some(Rgb::new(0, 90, 0)),
                foreground: Some(Rgb::new(0, 0, 200)),
            },
        ]
    }

    #[test]
    fn test_host_styles_color_highlighted_text_but_a_selection_keeps_its_background() {
        let theme = Theme::dark();
        let styles = host_styles();
        let text = cell_style(
            &painted_cell(cherry_engine::Style::Text, Some(1)),
            &theme,
            &styles,
        );
        assert_eq!(text.foreground, CellColor::Rgb(Rgb::new(200, 0, 0)));
        assert!(text.attributes.contains(Attributes::BOLD));
        let selected = cell_style(
            &painted_cell(cherry_engine::Style::Selection, Some(2)),
            &theme,
            &styles,
        );
        assert_eq!(selected.background, CellColor::Rgb(theme.selection));
        assert_eq!(selected.foreground, CellColor::Rgb(Rgb::new(0, 0, 200)));
        let past_the_end = cell_style(
            &painted_cell(cherry_engine::Style::Text, Some(9)),
            &theme,
            &styles,
        );
        assert_eq!(
            past_the_end,
            misty::CellStyle::default(),
            "an id the host gave no style"
        );
    }

    #[test]
    fn test_a_guide_keeps_its_faded_color_under_a_highlight_with_a_text_color() {
        let theme = Theme::dark();
        let mut guide = painted_cell(cherry_engine::Style::Text, Some(1));
        guide.layers = Layers::INDENT_GUIDE;
        let style = cell_style(&guide, &theme, &host_styles());
        assert_eq!(style.foreground, CellColor::Rgb(guide_color(&theme)));
    }

    #[test]
    fn test_a_wash_and_a_sign_of_the_hosts_own_kind_take_their_ids_colors() {
        let theme = Theme::dark();
        let styles = host_styles();
        let id = cherry_engine::HighlightId::new(2);
        let wash = cell_style(
            &painted_cell(cherry_engine::Style::Background(id), None),
            &theme,
            &styles,
        );
        assert_eq!(wash.background, CellColor::Rgb(Rgb::new(0, 90, 0)));
        let over_text = cell_style(
            &painted_cell(cherry_engine::Style::Background(id), Some(1)),
            &theme,
            &styles,
        );
        assert_eq!(
            over_text.foreground,
            CellColor::Rgb(Rgb::new(0, 0, 200)),
            "the wash's text color over the text's own highlight"
        );
        let unstyled = cherry_engine::Style::Background(cherry_engine::HighlightId::new(7));
        let fallback = cell_style(&painted_cell(unstyled, None), &theme, &styles);
        assert_eq!(
            fallback.background,
            CellColor::Rgb(theme.raised),
            "no style: raised"
        );
        let sign = cherry_engine::Style::Sign(DecorationKind::Background(id));
        let sign = cell_style(&painted_cell(sign, None), &theme, &styles);
        assert_eq!(sign.foreground, CellColor::Rgb(Rgb::new(0, 0, 200)));
    }

    #[test]
    fn test_an_inlay_is_muted_unless_its_highlight_colors_it() {
        let theme = Theme::dark();
        let styles = host_styles();
        let plain = cell_style(
            &painted_cell(cherry_engine::Style::Inlay, None),
            &theme,
            &styles,
        );
        assert_eq!(plain.foreground, CellColor::Rgb(theme.muted));
        assert!(plain.attributes.contains(Attributes::ITALIC));
        let colored = cell_style(
            &painted_cell(cherry_engine::Style::Inlay, Some(1)),
            &theme,
            &styles,
        );
        assert_eq!(colored.foreground, CellColor::Rgb(Rgb::new(200, 0, 0)));
        assert_eq!(
            colored.attributes,
            Attributes::BOLD,
            "the host's, not slanted"
        );
    }

    #[test]
    fn test_brackets_extra_cursors_and_preedits_stand_apart_from_text() {
        let theme = Theme::dark();
        let drawn = |style| cell_style(&painted_cell(style, None), &theme, &[]);
        let bracket = drawn(cherry_engine::Style::MatchingBracket);
        assert_eq!(bracket.background, CellColor::Rgb(theme.border));
        let extra = drawn(cherry_engine::Style::ExtraCursor);
        assert_eq!(extra.background, CellColor::Rgb(theme.cursor));
        assert_eq!(
            drawn(cherry_engine::Style::Preedit).underline,
            UnderlineStyle::Single
        );
    }

    #[test]
    fn test_a_view_in_a_font_of_its_own_takes_as_many_cells_as_that_font_fits() {
        let (mut editor, mut view) = drawn_view("text");
        let id = view.view();
        let font = Font::new(30.0);
        view.set_font(Some(font.clone()));
        // The text alone, the scrollbar's strip taking none of the width.
        view.set_scrollbar_hidden(true);
        let theme = Theme::dark();
        let drawn = TestWindow::new(600, 300).render(|frame| {
            let cell = frame.cell_metrics_of(&font);
            let area = Rect::new(0.0, 0.0, 7.0 * cell.width, 2.0 * cell.height);
            view.lend(&mut editor).draw(frame, area, &theme);
        });
        if drawn.is_none() {
            return;
        }
        let shown = editor.view(id).expect("the view");
        assert_eq!((shown.cols(), shown.rows()), (7, 2), "the big font's cells");
    }

    #[test]
    fn test_an_offset_rect_is_the_cell_its_char_was_drawn_in() {
        let (editor, view) = drawn_view("ab\ncd");
        assert_eq!(
            view.offset_rect(&editor, 4),
            Some(Rect::new(10.0, 20.0, 10.0, 20.0)),
            "d: the second cell of the second row"
        );
        assert_eq!(
            EditorView::new(view.view()).offset_rect(&editor, 0),
            None,
            "never drawn"
        );
    }

    #[test]
    fn test_a_preedit_in_insert_mode_is_laid_into_the_line_before_the_text_after_it() {
        let (mut editor, mut view) = drawn_view("ab");
        let id = view.view();
        editor.handle_key(
            id,
            cherry_engine::keys::parse_key("a").expect("a key"),
            clock::now(),
        );
        let composition = Composition {
            cursor: None,
            text: "\u{1ea1}".to_owned(),
        };
        let input = Input::Ime(ImeInput::Preedit(composition));
        let mut window = TestWindow::new(200, 120);
        let response = window.feed(&mut view.lend(&mut editor), &input);
        assert_eq!(response, EditorResponse::Handled);
        let theme = Theme::dark();
        let cells = cells_of(&mut editor, id, &theme);
        let at = |col: usize| cells.cell(0, col).expect("in the grid").clone();
        assert_eq!(
            (at(0).ch, at(1).ch, at(2).ch),
            ('a', '\u{1ea1}', 'b'),
            "b moved right"
        );
        assert_eq!(at(1).style.underline, UnderlineStyle::Single);
        let document = editor.view(id).expect("the view").document();
        let text = editor
            .document(document)
            .expect("open")
            .text_with_line_endings();
        assert_eq!(text, "ab", "nothing typed");
    }

    #[cfg(feature = "syntax-engine")]
    #[test]
    fn test_every_highlight_name_but_plain_variables_has_a_color_in_both_themes() {
        for theme in [Theme::dark(), Theme::light()] {
            let uncolored: Vec<&str> = cherry_engine::syntax::HIGHLIGHT_NAMES
                .iter()
                .copied()
                .filter(|name| theme.syntax.color(name).is_none())
                .collect();
            assert_eq!(uncolored, ["variable"]);
        }
    }

    #[cfg(feature = "syntax-textmate")]
    #[test]
    fn test_textmate_highlights_take_the_theme_s_syntax_colors() {
        // Regression: the colors were drawn only with tree-sitter's
        // `syntax-engine`, so TextMate highlights alone were drawn plain.
        let theme = Theme::dark();
        let colors = draw::highlight_colors(&theme);
        let keyword = cherry_engine::syntax::HIGHLIGHT_NAMES
            .iter()
            .position(|name| *name == "keyword")
            .expect("a keyword name");
        assert!(colors[keyword].foreground.is_some());
        assert_eq!(colors[keyword].foreground, theme.syntax.color("keyword"));
    }

    /// An editor with one view onto Rust `text`, highlighted.
    #[cfg(feature = "syntax-rust")]
    fn rust_editor(text: &str) -> (Editor, ViewId) {
        let mut editor = Editor::new(EditorOptions::default());
        let document = editor.open_document(text);
        let language = cherry_engine::syntax::Language::Rust;
        let source = editor.document(document).expect("just opened");
        let highlighter =
            cherry_engine::syntax::SyntaxHighlighter::new(language, source).expect("loads");
        editor.set_highlighter(document, Some(Box::new(highlighter)));
        let view = editor.new_view(document).expect("a view");
        (editor, view)
    }

    #[cfg(feature = "syntax-rust")]
    #[test]
    fn test_highlighted_text_takes_its_syntax_color_but_a_search_match_keeps_its_own() {
        let theme = Theme::dark();
        let keyword = theme.syntax.color("keyword").expect("a keyword color");
        let (mut editor, view) = rust_editor("fn x() {}\n");
        let cells = cells_of(&mut editor, view, &theme);
        let foreground =
            |cells: &CellBuffer, col: usize| cells.cell(0, col).map(|cell| cell.style.foreground);
        assert_eq!(foreground(&cells, 0), Some(CellColor::Rgb(keyword)));
        assert_ne!(foreground(&cells, 3), Some(CellColor::Rgb(keyword)));
        for spec in ["/", "f", "n", "RET"] {
            let key = cherry_engine::keys::parse_key(spec).expect("a valid key");
            editor.handle_key(view, key, clock::now());
        }
        let cells = cells_of(&mut editor, view, &theme);
        assert_eq!(
            foreground(&cells, 0),
            Some(CellColor::Rgb(theme.background)),
            "dark text on the match color"
        );
    }

    #[cfg(feature = "syntax-rust")]
    #[test]
    fn test_a_keyword_is_drawn_in_the_keyword_color() {
        let theme = Theme::dark();
        let keyword = theme.syntax.color("keyword").expect("a keyword color");
        let (mut editor, view) = rust_editor("\nfn x() {}\n");
        let mut editor_view = EditorView::new(view);
        let Some(snapshot) = TestWindow::new(200, 120).render(|frame| {
            let metrics = frame.cell_metrics();
            let area = Rect::new(0.0, 0.0, 10.0 * metrics.width, 3.0 * metrics.height);
            editor_view.lend(&mut editor).draw(frame, area, &theme);
        }) else {
            return;
        };
        let metrics = snapshot.metrics();
        let row = |col: f32, cols: f32| {
            Rect::new(
                col * metrics.width,
                metrics.height,
                cols * metrics.width,
                metrics.height,
            )
        };
        assert!(snapshot.contains_color(row(0.0, 2.0), keyword), "fn");
        assert!(
            !snapshot.contains_color(row(3.0, 1.0), keyword),
            "the name is not a keyword"
        );
    }
}
