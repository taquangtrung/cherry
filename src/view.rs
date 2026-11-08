//! The view: a window onto a document, with its own cursor, its own size in
//! cells, and its own scroll position. Several views may show one document,
//! each scrolled and with its cursor where its user left it.
//!
//! # Scrolling
//!
//! The view scrolls by screen rows, so with soft wrap on, the top of the
//! view may fall inside a long line. [`View::scroll_to_cursor`] moves the
//! top just enough to show the cursor with
//! [`EditorOptions::scroll_margin`] rows above and below it, fewer where the
//! document starts or ends: at the last line the cursor sits on the bottom
//! row, never above blank rows. Without wrap, the view also scrolls
//! sideways, just enough to show the cursor's cell.
//!
//! # Modes and pending keys
//!
//! The [`Mode`], the keys of a half-typed sequence, a half-typed count, the
//! register selected with `"`, and an operator waiting for its motion
//! belong to the view, not to the editor, so two views (or two editors
//! embedded side by side) never share a mode or a pending key.

use std::ops;

use crate::keys::{Key, Resolver, Timestamp};

use super::block::BlockInsert;
use super::command::PendingChar;
use super::completion::{Completion, CompletionWait};
use super::cursors::ExtraCursors;
use super::decoration::Decorations;
use super::document::{Document, Position, last_line, line_count};
use super::editor::DocumentId;
use super::mark::JumpList;
use super::operator::PendingOperator;
use super::options::{ConcealMode, EditorOptions, ViewOptions};
use super::prompt::Prompt;
use super::repeat::{ChangeCandidate, MacroRecording};
use super::replace::{ReplaceRepeat, ReplaceSession};
use super::selection::Selection;
use super::snippet::SnippetSession;
use super::substitute::QueryReplace;
use super::surround::PendingSurround;
use super::wrap::{LineInlay, LineLayout, RowCell, char_cells};

// ========================================================================
// Constants
// ========================================================================

/// The fewest digits the line number gutter makes room for, so it does not
/// widen as a short document grows.
const MIN_LINE_NUMBER_DIGITS: usize = 3;

/// The blank cells between the line numbers and the text.
const GUTTER_PADDING: usize = 1;

/// The cells the sign column takes, when it is on.
const SIGN_COLUMN_WIDTH: usize = 2;

/// The chars past which a line that is not wrapped is laid out only where
/// it is in sight.
const LONG_LINE_CHARS: usize = 4096;

// ========================================================================
// Data Structures
// ========================================================================

/// Where a view is scrolled to: the screen row at its top, and without
/// wrap, the cell column at its left edge.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub struct ScrollOffset {
    /// The cell column of the text shown at the left edge; always 0 with
    /// soft wrap on.
    pub column: usize,
    /// The line shown at the top.
    pub line: usize,
    /// The row of that line shown at the top, past 0 only when a wrapped
    /// line starts above the view.
    pub row: usize,
}

/// The editing mode of a view: which key bindings are active and how the
/// cursor is drawn.
///
/// New variants may come in a minor release, so a host's `match` needs a
/// fallback arm.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
#[non_exhaustive]
pub enum Mode {
    /// A prompt at the bottom of the view takes the keys (a search pattern
    /// after `/` or `?`); the view goes back to the mode it came from when
    /// the prompt closes, an operator still waiting or a selection still
    /// selected.
    CommandLine,
    /// Keys type text; the cursor sits between characters.
    Insert,
    /// Keys move and run commands; the cursor sits on a character. The
    /// mode a view starts in.
    #[default]
    Normal,
    /// An operator (`d`, `c`, `y`) was typed and waits for the motion that
    /// says what it acts on; any key that is not one goes back to Normal.
    OperatorPending,
    /// As [`Mode::Insert`], but each typed character replaces the one under
    /// the cursor (`R`); Backspace brings back what was replaced.
    Replace,
    /// Motions extend a selection of characters from where Visual mode
    /// was entered to the cursor, both ends included; operators act on it.
    Visual,
    /// As [`Mode::Visual`], selecting the rectangle of display columns
    /// between where the mode was entered and the cursor, on every line
    /// between them (`C-v`).
    VisualBlock,
    /// As [`Mode::Visual`], selecting the whole lines from the one Visual
    /// mode was entered on to the cursor's.
    VisualLine,
}

/// A window onto a document: a cursor, a size, a scroll position, and the
/// mode and pending keys of the user typing in it.
#[derive(Clone, Debug)]
pub struct View {
    /// The line given only the indent of the line it was opened from, while
    /// nothing has been typed on it, for Insert mode to empty again.
    auto_indented_line: Option<usize>,
    /// Whether a `"` was typed and the register name is awaited.
    awaiting_register: bool,
    /// The change being made, recorded for `.`.
    change: Option<ChangeCandidate>,
    /// The open completion menu, in Insert mode.
    completion: Option<Completion>,
    /// The completion waiting for late answers from its providers.
    completion_wait: Option<CompletionWait>,
    /// The number of cells in a row, gutter included.
    cols: u16,
    /// The count typed so far in Normal mode, before a command.
    count: Option<usize>,
    /// The document shown.
    document: DocumentId,
    /// The cursors besides the main one.
    extra_cursors: ExtraCursors,
    /// The display column up and down motions aim for, kept across lines
    /// too short to reach it; `None` until such a motion sets it.
    goal_column: Option<usize>,
    /// The rows `C-d` and `C-u` scroll, set by a count before either as
    /// Vim's `scroll` option is; `None` (half the rows) until then and
    /// again once the number of rows changes.
    half_page: Option<usize>,
    /// Where the text typed in the insert session starts, carried through
    /// edits, for register `.`.
    insert_start: Option<usize>,
    /// The places jumps started from, for `C-o` and `C-i`.
    jumps: JumpList,
    /// Text typed on a block's first line, to repeat on its other lines.
    block_insert: Option<BlockInsert>,
    /// The visual selection last left, for `gv`.
    last_visual: Option<VisualArea>,
    /// Where `H` or `L` put the cursor on a row inside the scroll margin,
    /// which the view leaves where it is while the cursor stays there.
    margin_waived_at: Option<usize>,
    /// The editing mode.
    mode: Mode,
    /// Where the mouse button went down, for a drag to select from.
    mouse_anchor: Option<usize>,
    /// The operator waiting for its motion in operator-pending mode.
    operator: Option<PendingOperator>,
    /// What the view shows differently from the editor's options.
    options: ViewOptions,
    /// The keys typed so far toward a binding.
    pending: Resolver,
    /// A command waiting for a character, such as the register of `q`.
    pending_char: Option<PendingChar>,
    /// The text an input method is composing at the cursor, if any.
    preedit: Option<String>,
    /// The open prompt, in command-line mode.
    prompt: Option<Prompt>,
    /// The sequence number of the last `C-l` and where it put the
    /// cursor's row, for the next one run right after to go on from.
    recentered: Option<(u64, CursorPlace)>,
    /// Whether the extra cursors change every occurrence of a word at once,
    /// to drop when Insert mode is left.
    changing_occurrences: bool,
    /// The selections the selection grew from, oldest first, each with
    /// whether it was a visual one, for shrinking back.
    growths: Vec<(ops::Range<usize>, bool)>,
    /// The selection the last growth made, while it stands.
    grown: Option<ops::Range<usize>>,
    /// The confirming substitution asking about a match.
    query_replace: Option<QueryReplace>,
    /// The snippet being filled in, in Insert mode.
    snippet: Option<SnippetSession>,
    /// A surround waiting for its next char.
    surround: Option<PendingSurround>,
    /// The register selected with `"` for the next command.
    register: Option<char>,
    /// The keys being recorded into a register with `q`.
    recording: Option<MacroRecording>,
    /// The count of `R` and what was typed over since, to type again.
    replace_repeat: Option<ReplaceRepeat>,
    /// What typing in Replace mode replaced, for Backspace.
    replace_session: Option<ReplaceSession>,
    /// The number of rows.
    rows: u16,
    /// Where the view is scrolled to.
    scroll: ScrollOffset,
    /// The cursors and selected ranges; the primary head is the cursor
    /// scrolling follows.
    selection: Selection,
    /// Where the visual selection was started, in a visual mode.
    visual_anchor: Option<usize>,
}

/// A cell of the screen row the cursor is on, for `g0`, `gm`, and `g$`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RowPlace {
    /// The last cell of the view's width.
    End,
    /// The cell half the view's width in.
    Middle,
    /// The first cell in sight.
    Start,
}

/// Where `zt`, `zz`, and `zb` put the cursor's row in the view.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CursorPlace {
    /// At the bottom, the scroll margin kept below it.
    Bottom,
    /// In the middle.
    Center,
    /// At the top, the scroll margin kept above it.
    Top,
}

/// The lines a view shows, one per screen row from the top, for `H`, `M`,
/// and `L`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ScreenLines {
    /// The line shown on each row with text, from the top.
    pub lines: Vec<usize>,
    /// Whether the last row of the document is shown.
    pub shows_end: bool,
    /// Whether the first row of the document is at the top.
    pub shows_start: bool,
}

/// A visual selection as it was left, for `gv` to select again.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct VisualArea {
    /// Where the selection was started.
    pub anchor: usize,
    /// Where the cursor was.
    pub cursor: usize,
    /// Visual or VisualLine.
    pub mode: Mode,
}

/// A screen row of a document: a line and a row of it.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct ScreenRow {
    /// The line.
    line: usize,
    /// The row of the line, from 0.
    row: usize,
}

/// How a document lays out in a view: the options and the text width.
#[derive(Clone, Copy, Debug)]
pub(crate) struct TextArea<'a> {
    /// The decorations of the document, for its inlays and conceals.
    pub decorations: &'a Decorations,
    /// The document laid out.
    pub document: &'a Document,
    /// The cells left of the text taken by the gutter: the host's lanes,
    /// the sign column, then the line numbers.
    pub gutter_width: usize,
    /// The options it is laid out with.
    pub options: &'a EditorOptions,
    /// The text an input method is composing and the char offset it shows
    /// at, laid out after the inlays there, as one of them.
    pub preedit: Option<(usize, &'a str)>,
    /// The line whose concealed text shows, the cursor's.
    pub revealed: Option<usize>,
    /// The cells of the gutter taken by the lanes and the sign column, at
    /// its left.
    pub sign_width: usize,
    /// The cells a row of text holds, the gutter left out.
    pub width: usize,
}

// ========================================================================
// TextArea Implementations
// ========================================================================

impl<'a> TextArea<'a> {
    /// The text area of `document`, with its `decorations`, in a view
    /// `cols` cells wide, every conceal hidden.
    pub fn new(
        document: &'a Document,
        decorations: &'a Decorations,
        options: &'a EditorOptions,
        cols: u16,
    ) -> Self {
        let number_width = if options.line_numbers || options.relative_line_numbers {
            let digits = line_count(document).to_string().len();
            digits.max(MIN_LINE_NUMBER_DIGITS) + GUTTER_PADDING
        } else {
            0
        };
        let sign_width = lane_widths(decorations, options)
            .map(|(_, width)| width)
            .sum::<usize>();
        let gutter_width = sign_width + number_width;
        Self {
            decorations,
            document,
            gutter_width,
            options,
            preedit: None,
            revealed: None,
            sign_width,
            width: usize::from(cols).saturating_sub(gutter_width),
        }
    }

    /// The lanes of signs, left to right: each lane's name (`None` for the
    /// sign column), the cell it starts at, and its width.
    pub fn lanes(&self) -> Vec<(Option<&'a str>, usize, usize)> {
        let mut start = 0;
        lane_widths(self.decorations, self.options)
            .map(|(name, width)| {
                let lane = (name, start, width);
                start += width;
                lane
            })
            .collect()
    }

    /// Whether line `line` is long enough, and not wrapped, to be laid out
    /// only where it is in sight.
    pub fn is_long_line(&self, line: usize) -> bool {
        !self.options.wrap && self.line_len(line) > LONG_LINE_CHARS
    }

    /// The text of the part of long line `line` covering cells `from` to
    /// `to`, with the char column and the cell its first cluster starts at.
    pub fn window(&self, line: usize, from: usize, to: usize) -> (String, usize, usize) {
        let tab = self.options.tab_stop();
        let (mut column, mut x) = (0, 0);
        let mut start = None;
        let mut text = String::new();
        for c in self.line_chars(line) {
            let cells = char_cells(c, x, tab);
            match start {
                None if cells > 0 && x + cells > from => start = Some((column, x)),
                None => {
                    column += 1;
                    x += cells;
                    continue;
                }
                Some(_) if cells > 0 && x >= to => break,
                Some(_) => {}
            }
            text.push(c);
            x += cells;
        }
        let (column, x) = start.unwrap_or((column, x));
        (text, column, x)
    }

    /// The char column of long line `line` whose cluster covers cell `x`,
    /// or the line's length past its end.
    fn column_at_cell(&self, line: usize, x: usize) -> usize {
        let tab = self.options.tab_stop();
        let (mut column, mut at) = (0, 0);
        for c in self.line_chars(line) {
            let cells = char_cells(c, at, tab);
            if cells > 0 && at + cells > x {
                return column;
            }
            column += 1;
            at += cells;
        }
        column
    }

    /// The chars of line `line`, its break left out.
    fn line_chars(&self, line: usize) -> impl Iterator<Item = char> + '_ {
        self.document
            .rope()
            .line(line)
            .chars()
            .take_while(|c| *c != '\n')
    }

    /// The chars of line `line`, its break left out; zero past the last.
    fn line_len(&self, line: usize) -> usize {
        let start = self.document.position_to_char(Position { column: 0, line });
        let end = self.document.position_to_char(Position {
            column: usize::MAX,
            line,
        });
        match (start, end) {
            (Some(start), Some(end)) => end - start,
            (Some(_) | None, _) => 0,
        }
    }

    /// The cell char `column` of long line `line` starts at.
    fn x_of_column(&self, line: usize, column: usize) -> usize {
        let tab = self.options.tab_stop();
        self.line_chars(line)
            .take(column)
            .fold(0, |x, c| x + char_cells(c, x, tab))
    }

    /// This text area with the concealed text of line `line` shown.
    pub fn revealing(self, line: Option<usize>) -> Self {
        Self {
            revealed: line,
            ..self
        }
    }

    /// Whether the glyph of inlay `index`, as [`TextArea::layout`] numbers
    /// them, is of the text an input method is composing: the index past
    /// the document's inlays.
    pub fn is_preedit(&self, index: usize) -> bool {
        index == self.decorations.inlays_by_offset().len()
    }

    /// The layout of `text`, line `line` of the document, with the inlays
    /// on it and its concealed text hidden, unless it is the revealed line.
    pub fn layout<'t>(&self, line: usize, text: &'t str) -> LineLayout<'t>
    where
        'a: 't,
    {
        let wrap_width = self.options.wrap.then_some(self.width);
        let start = self
            .document
            .position_to_char(Position { column: 0, line })
            .unwrap_or_else(|| self.document.len_chars());
        let end = start + text.chars().count();
        let all = self.decorations.inlays_by_offset();
        let from = all.partition_point(|inlay| inlay.offset < start);
        let to = all.partition_point(|inlay| inlay.offset <= end);
        let mut inlays: Vec<_> = (from..to)
            .map(|index| LineInlay {
                column: all[index].offset - start,
                index,
                text: &all[index].text,
            })
            .collect();
        if let Some((offset, text)) = self
            .preedit
            .filter(|(offset, _)| (start..=end).contains(offset))
        {
            let column = offset - start;
            let at = inlays.partition_point(|inlay| inlay.column <= column);
            let index = all.len();
            inlays.insert(
                at,
                LineInlay {
                    column,
                    index,
                    text,
                },
            );
        }
        let shown = match self.options.conceal {
            ConcealMode::Hide => false,
            ConcealMode::RevealCursorLine => self.revealed == Some(line),
            ConcealMode::Show => true,
        };
        let hidden: Vec<_> = if shown {
            Vec::new()
        } else {
            let conceals = self.decorations.conceals_by_offset();
            conceals[..conceals.partition_point(|range| range.start < end)]
                .iter()
                .filter(|range| range.end > start)
                .map(|range| range.start.max(start) - start..range.end.min(end) - start)
                .collect()
        };
        let tab = self.options.tab_stop();
        let break_indent = self.options.break_indent;
        let mut layout =
            LineLayout::decorated(text, &inlays, &hidden, wrap_width, tab, break_indent);
        let virtual_lines = self
            .decorations
            .virtual_lines_after(self.document, line)
            .len();
        layout.push_virtual_rows(virtual_lines);
        layout
    }

    /// The row and cell where the cursor at char `offset` is shown, with
    /// the line it is on.
    pub fn locate(&self, offset: usize) -> (usize, RowCell) {
        let offset = offset.min(self.document.len_chars());
        let Position { column, line } = self
            .document
            .char_to_position(offset)
            .expect("the offset is clamped to the document");
        if self.is_long_line(line) {
            let x = self.x_of_column(line, column);
            return (line, RowCell { row: 0, x });
        }
        let text = self.document.line_text(line).unwrap_or_default();
        (line, self.layout(line, &text).locate(column))
    }

    /// The char offset shown at text cell `x` of screen row `at`: the
    /// cluster covering the cell, else past the end of the row, the end of
    /// the line on its last row and the row's last cluster on a wrapped
    /// row.
    fn offset_in_row(&self, at: ScreenRow, x: usize) -> usize {
        let start = self
            .document
            .position_to_char(Position {
                column: 0,
                line: at.line,
            })
            .unwrap_or_else(|| self.document.len_chars());
        if self.is_long_line(at.line) {
            return start + self.column_at_cell(at.line, x);
        }
        let text = self.document.line_text(at.line).unwrap_or_default();
        let layout = self.layout(at.line, &text);
        let Some(row) = layout.rows().get(at.row) else {
            return start + text.chars().count();
        };
        if let Some(glyph) = row.glyphs.iter().find(|glyph| x < glyph.x + glyph.width) {
            return start + glyph.column;
        }
        let is_last_row = at.row + 1 == layout.rows().len();
        match row.glyphs.last() {
            Some(glyph) if !is_last_row => start + glyph.column,
            Some(_) | None => start + text.chars().count(),
        }
    }

    /// Whether screen row `at` shows a virtual line, not the text.
    fn is_virtual_row(&self, at: ScreenRow) -> bool {
        if self.is_long_line(at.line) {
            return false;
        }
        let text = self.document.line_text(at.line).unwrap_or_default();
        self.layout(at.line, &text)
            .rows()
            .get(at.row)
            .is_some_and(|row| row.virtual_line.is_some())
    }

    /// The last screen row of the document.
    fn last_row(&self) -> ScreenRow {
        let line = last_line(self.document);
        ScreenRow {
            line,
            row: self.row_count(line) - 1,
        }
    }

    /// The screen row `count` rows below `from`, or `None` when the
    /// document ends first.
    fn row_below(&self, from: ScreenRow, count: usize) -> Option<ScreenRow> {
        let last_line = last_line(self.document);
        let mut at = from;
        for _ in 0..count {
            if at.row + 1 < self.row_count(at.line) {
                at.row += 1;
            } else if at.line < last_line {
                at = ScreenRow {
                    line: at.line + 1,
                    row: 0,
                };
            } else {
                return None;
            }
        }
        Some(at)
    }

    /// The number of screen rows line `line` takes.
    pub(crate) fn row_count(&self, line: usize) -> usize {
        if !self.options.wrap {
            let virtual_lines = self
                .decorations
                .virtual_lines_after(self.document, line)
                .len();
            return match virtual_lines > 0 && !self.is_long_line(line) {
                true => 1 + virtual_lines,
                false => 1,
            };
        }
        let text = self.document.line_text(line).unwrap_or_default();
        self.layout(line, &text).rows().len()
    }

    /// The screen row `count` rows above `from`, stopping at the first row
    /// of the document.
    fn rows_above(&self, from: ScreenRow, count: usize) -> ScreenRow {
        let mut at = from;
        for _ in 0..count {
            if at.row > 0 {
                at.row -= 1;
            } else if at.line > 0 {
                at.line -= 1;
                at.row = self.row_count(at.line) - 1;
            } else {
                break;
            }
        }
        at
    }

    /// How many of the `count` rows below `from` the document has.
    fn rows_below(&self, from: ScreenRow, count: usize) -> usize {
        let last_line = last_line(self.document);
        let mut at = from;
        for taken in 0..count {
            if at.row + 1 < self.row_count(at.line) {
                at.row += 1;
            } else if at.line < last_line {
                at = ScreenRow {
                    line: at.line + 1,
                    row: 0,
                };
            } else {
                return taken;
            }
        }
        count
    }
}

// ========================================================================
// View Implementations
// ========================================================================

impl View {
    /// A view of `document`, `cols` cells wide and `rows` high, in Normal
    /// mode, scrolled to the top, with a cursor at the start of the
    /// document; a pending key sequence times out as `options` say.
    pub(crate) fn new(document: DocumentId, cols: u16, rows: u16, options: &EditorOptions) -> Self {
        Self {
            auto_indented_line: None,
            awaiting_register: false,
            block_insert: None,
            change: None,
            completion: None,
            completion_wait: None,
            cols,
            count: None,
            document,
            extra_cursors: ExtraCursors::default(),
            goal_column: None,
            half_page: None,
            insert_start: None,
            jumps: JumpList::default(),
            last_visual: None,
            margin_waived_at: None,
            mode: Mode::Normal,
            options: ViewOptions::default(),
            mouse_anchor: None,
            operator: None,
            pending: Resolver::new(options.key_timeout).with_prefix_timeout(options.prefix_timeout),
            pending_char: None,
            preedit: None,
            prompt: None,
            recentered: None,
            recording: None,
            replace_repeat: None,
            replace_session: None,
            register: None,
            rows,
            scroll: ScrollOffset::default(),
            selection: Selection::point(0),
            changing_occurrences: false,
            growths: Vec::new(),
            grown: None,
            query_replace: None,
            snippet: None,
            surround: None,
            visual_anchor: None,
        }
    }

    /// The number of cells in a row, gutter included.
    pub fn cols(&self) -> u16 {
        self.cols
    }

    /// The open completion menu, for the host to paint as a popup near
    /// [`Completion::anchor`]; `None` when no menu is open.
    pub fn completion(&self) -> Option<&Completion> {
        self.completion.as_ref()
    }

    /// The count typed so far in Normal mode, waiting for its command.
    pub fn count(&self) -> Option<usize> {
        self.count
    }

    /// The char offset of the cursor: the head of the primary range.
    pub fn cursor(&self) -> usize {
        self.selection.primary().head
    }

    /// The char offsets of the view's cursors besides the main one, in
    /// the order they were added, for the host to draw (see
    /// [`Editor::set_cursors`](crate::Editor::set_cursors)); empty for one
    /// cursor.
    pub fn cursors(&self) -> &[usize] {
        self.extra_cursors.offsets()
    }

    /// The document the view shows.
    pub fn document(&self) -> DocumentId {
        self.document
    }

    /// The editing mode.
    pub fn mode(&self) -> Mode {
        self.mode
    }

    /// The keys typed so far toward a binding, for the host to show; empty
    /// when none are pending.
    pub fn pending_keys(&self) -> &[Key] {
        self.pending.pending()
    }

    /// The text an input method is composing at the cursor, as the host
    /// last set it with [`Editor::set_preedit`](crate::Editor::set_preedit);
    /// `None` when nothing is being composed.
    pub fn preedit(&self) -> Option<&str> {
        self.preedit.as_deref()
    }

    /// The open prompt, for the host to show; `None` outside command-line
    /// mode.
    pub fn prompt(&self) -> Option<&Prompt> {
        self.prompt.as_ref()
    }

    /// The register selected with `"` for the next command, for the host
    /// to show; `None` when none is.
    pub fn register(&self) -> Option<char> {
        self.register
    }

    /// The register keys are being recorded into with `q`, for the host
    /// to show; `None` when not recording.
    pub fn recording(&self) -> Option<char> {
        self.recording.as_ref().map(MacroRecording::register)
    }

    /// The number of rows.
    pub fn rows(&self) -> u16 {
        self.rows
    }

    /// Where the view is scrolled to.
    pub fn scroll(&self) -> ScrollOffset {
        self.scroll
    }

    /// The cursors and selected ranges.
    pub fn selection(&self) -> &Selection {
        &self.selection
    }

    /// The char offset where the visual selection was started, while the
    /// view is in a visual mode; `None` otherwise. The selection runs from
    /// it to [`View::cursor`], both ends included: as chars in
    /// [`Mode::Visual`], whole lines in [`Mode::VisualLine`], and the
    /// rectangle of display columns between them in [`Mode::VisualBlock`].
    pub fn visual_anchor(&self) -> Option<usize> {
        self.visual_anchor
    }

    /// The visual anchor of each extra cursor, by the order of
    /// [`View::cursors`], while the view is in Visual or VisualLine mode.
    pub(crate) fn extra_anchors(&self) -> &[Option<usize>] {
        self.extra_cursors.anchors()
    }

    /// The line holding only the indent Insert mode gave it, if any.
    pub(crate) fn auto_indented_line(&self) -> Option<usize> {
        self.auto_indented_line
    }

    /// Whether a `"` was typed and the register name is awaited.
    pub(crate) fn awaiting_register(&self) -> bool {
        self.awaiting_register
    }

    /// Closes the open prompt and returns to the mode it was opened from,
    /// the cursor and scroll back where they were if incremental search
    /// moved them; returns the prompt, or `None` when none was open.
    pub(crate) fn close_prompt(&mut self) -> Option<Prompt> {
        let prompt = self.prompt.take()?;
        if let Some((cursor, scroll)) = prompt.follow().origin {
            self.selection = Selection::point(cursor);
            self.scroll = scroll;
        }
        self.set_mode(prompt.return_mode());
        Some(prompt)
    }

    /// The change being recorded for `.`, if any.
    pub(crate) fn change(&self) -> Option<&ChangeCandidate> {
        self.change.as_ref()
    }

    /// The change being recorded for `.`, to add to.
    pub(crate) fn change_mut(&mut self) -> Option<&mut ChangeCandidate> {
        self.change.as_mut()
    }

    /// The count typed so far, for the editor to extend or take.
    pub(crate) fn count_mut(&mut self) -> &mut Option<usize> {
        &mut self.count
    }

    /// Whether nothing is pending: Normal mode, no keys, count, register,
    /// or command waiting. A change starts and ends here.
    pub(crate) fn is_idle(&self) -> bool {
        self.mode == Mode::Normal
            && self.pending.pending().is_empty()
            && self.count.is_none()
            && self.register.is_none()
            && !self.awaiting_register
            && self.pending_char.is_none()
    }

    /// The keys being recorded with `q`, to add to.
    pub(crate) fn recording_mut(&mut self) -> Option<&mut MacroRecording> {
        self.recording.as_mut()
    }

    /// The places jumps started from.
    pub(crate) fn jumps(&self) -> &JumpList {
        &self.jumps
    }

    /// The places jumps started from, to add to or walk.
    pub(crate) fn jumps_mut(&mut self) -> &mut JumpList {
        &mut self.jumps
    }

    /// The visual selection last left, if any.
    pub(crate) fn last_visual(&self) -> Option<VisualArea> {
        self.last_visual
    }

    /// Carries the visual anchor, the last visual selection, and the
    /// start of the insert session's text to where `map` takes their
    /// offsets, after the document changed.
    pub(crate) fn map_offsets(&mut self, map: impl Fn(usize) -> usize) {
        self.extra_cursors.map(&map);
        self.visual_anchor = self.visual_anchor.map(&map);
        self.insert_start = self.insert_start.map(&map);
        if let Some(area) = &mut self.last_visual {
            area.anchor = map(area.anchor);
            area.cursor = map(area.cursor);
        }
    }

    /// The display column up and down motions aim for, if one is set.
    pub(crate) fn goal_column(&self) -> Option<usize> {
        self.goal_column
    }

    /// Opens `prompt` and enters command-line mode, keeping a waiting
    /// operator and a visual selection for when it closes.
    pub(crate) fn open_prompt(&mut self, prompt: Prompt) {
        self.prompt = Some(prompt);
        self.set_mode(Mode::CommandLine);
    }

    /// Where the mouse button went down, if it is held.
    pub(crate) fn mouse_anchor(&self) -> Option<usize> {
        self.mouse_anchor
    }

    /// The operator waiting for its motion, if any.
    pub(crate) fn operator(&self) -> Option<PendingOperator> {
        self.operator
    }

    /// When the pending key sequence times out; `None` when no keys are
    /// pending.
    pub(crate) fn pending_deadline(&self) -> Option<Timestamp> {
        self.pending.deadline()
    }

    /// Makes the view `cols` cells wide and `rows` high, keeping its scroll
    /// position; call [`View::scroll_to_cursor`] after.
    pub(crate) fn resize(&mut self, cols: u16, rows: u16) {
        if rows != self.rows {
            self.half_page = None;
        }
        self.cols = cols;
        self.rows = rows;
    }

    /// The rows `C-d` and `C-u` scroll: those a count set, else half the
    /// view's rows, at least one.
    pub(crate) fn half_page(&self) -> usize {
        self.half_page.unwrap_or(usize::from(self.rows) / 2).max(1)
    }

    /// Makes `rows`, cut to the view's height, what `C-d` and `C-u` scroll
    /// until the number of rows changes.
    pub(crate) fn set_half_page(&mut self, rows: usize) {
        self.half_page = Some(rows.min(usize::from(self.rows)));
    }

    /// Records that the selection `from`, a visual one when `was_visual`,
    /// grew to `to`; a selection changed since the last growth starts the
    /// record over.
    pub(crate) fn push_growth(
        &mut self,
        from: ops::Range<usize>,
        was_visual: bool,
        to: ops::Range<usize>,
    ) {
        if self.grown.as_ref() != Some(&from) {
            self.growths.clear();
        }
        self.growths.push((from, was_visual));
        self.grown = Some(to);
    }

    /// The selection the last growth started from, and whether it was a
    /// visual one, when `current` is what that growth made; `None`, and
    /// the record dropped, when the selection changed since.
    pub(crate) fn pop_growth(
        &mut self,
        current: &ops::Range<usize>,
    ) -> Option<(ops::Range<usize>, bool)> {
        if self.grown.as_ref() != Some(current) {
            self.growths.clear();
            self.grown = None;
            return None;
        }
        let (previous, was_visual) = self.growths.pop()?;
        self.grown = was_visual.then(|| previous.clone());
        Some((previous, was_visual))
    }

    /// Whether the extra cursors change every occurrence of a word at once.
    pub(crate) fn is_changing_occurrences(&self) -> bool {
        self.changing_occurrences
    }

    /// Says whether the extra cursors change every occurrence of a word at
    /// once.
    pub(crate) fn set_changing_occurrences(&mut self, changing: bool) {
        self.changing_occurrences = changing;
    }

    /// Keeps `query`, a confirming substitution asking about a match, or
    /// with `None` drops it.
    pub(crate) fn set_query_replace(&mut self, query: Option<QueryReplace>) {
        self.query_replace = query;
    }

    /// Takes the confirming substitution asking about a match.
    pub(crate) fn take_query_replace(&mut self) -> Option<QueryReplace> {
        self.query_replace.take()
    }

    /// The snippet being filled in, if any.
    pub(crate) fn snippet(&self) -> Option<&SnippetSession> {
        self.snippet.as_ref()
    }

    /// The snippet being filled in, to change.
    pub(crate) fn snippet_mut(&mut self) -> Option<&mut SnippetSession> {
        self.snippet.as_mut()
    }

    /// Starts filling in `snippet`, or with `None` ends it.
    pub(crate) fn set_snippet(&mut self, snippet: Option<SnippetSession>) {
        self.snippet = snippet;
    }

    /// Keeps `pending`, a surround waiting for its next char.
    pub(crate) fn set_surround(&mut self, pending: PendingSurround) {
        self.surround = Some(pending);
    }

    /// Takes the surround waiting for its next char.
    pub(crate) fn take_surround(&mut self) -> Option<PendingSurround> {
        self.surround.take()
    }

    /// The sequence number of the last `C-l` and where it put the cursor's
    /// row.
    pub(crate) fn recentered(&self) -> Option<(u64, CursorPlace)> {
        self.recentered
    }

    /// Records that the command numbered `seq`, a `C-l`, put the cursor's
    /// row at `place`.
    pub(crate) fn set_recentered(&mut self, seq: u64, place: CursorPlace) {
        self.recentered = Some((seq, place));
    }

    /// Keeps the view from scrolling to give the cursor at `offset` its
    /// scroll margin, until the cursor moves.
    pub(crate) fn waive_margin_at(&mut self, offset: usize) {
        self.margin_waived_at = Some(offset);
    }

    /// The open prompt, for its commands to edit.
    /// The cursors besides the main one.
    pub(crate) fn extra_cursors(&self) -> &ExtraCursors {
        &self.extra_cursors
    }

    /// The cursors besides the main one, to change.
    pub(crate) fn extra_cursors_mut(&mut self) -> &mut ExtraCursors {
        &mut self.extra_cursors
    }

    /// Scrolls the view to `scroll`.
    pub(crate) fn set_scroll(&mut self, scroll: ScrollOffset) {
        self.scroll = scroll;
    }

    /// Sets the text an input method is composing; empty for none.
    pub(crate) fn set_preedit(&mut self, text: &str) {
        self.preedit = (!text.is_empty()).then(|| text.to_string());
    }

    pub(crate) fn prompt_mut(&mut self) -> Option<&mut Prompt> {
        self.prompt.as_mut()
    }

    /// The register selected for the next command, for the editor to set
    /// or take.
    pub(crate) fn register_mut(&mut self) -> &mut Option<char> {
        &mut self.register
    }

    /// The pending key sequence, for the editor to feed.
    pub(crate) fn resolver_mut(&mut self) -> &mut Resolver {
        &mut self.pending
    }

    /// The view made to show `document` in place of this one's: the same
    /// size, in Normal mode with the cursor at the start, keeping only the
    /// change `.` repeats, a macro being recorded, and the rows `C-d` and
    /// `C-u` scroll (a window setting in Vim).
    pub(crate) fn showing(&mut self, document: DocumentId, options: &EditorOptions) -> Self {
        let mut view = Self::new(document, self.cols, self.rows, options);
        view.half_page = self.half_page;
        view.change = self.change.take();
        view.recording = self.recording.take();
        view
    }

    /// Gives the view the key timeouts of `options`, dropping any keys
    /// pending toward a binding.
    pub(crate) fn set_key_timeouts(&mut self, options: &EditorOptions) {
        self.pending =
            Resolver::new(options.key_timeout).with_prefix_timeout(options.prefix_timeout);
    }

    /// The text area of `document` in the view, with its `decorations`,
    /// laid out with `options`: the concealed text of the cursor's line
    /// shows.
    pub(crate) fn area<'a>(
        &'a self,
        document: &'a Document,
        decorations: &'a Decorations,
        options: &'a EditorOptions,
    ) -> TextArea<'a> {
        self.area_with_preedit(document, decorations, options, self.preedit.as_deref())
    }

    /// As [`View::area`], with `preedit` as the text being composed, for a
    /// caller that changes the view while laying it out.
    fn area_with_preedit<'a>(
        &self,
        document: &'a Document,
        decorations: &'a Decorations,
        options: &'a EditorOptions,
        preedit: Option<&'a str>,
    ) -> TextArea<'a> {
        let cursor = self.cursor().min(document.len_chars());
        let line = document.char_to_position(cursor).map(|at| at.line);
        let preedit = preedit
            .filter(|_| matches!(self.mode, Mode::Insert | Mode::Replace))
            .map(|text| (cursor, text));
        TextArea {
            preedit,
            ..TextArea::new(document, decorations, options, self.cols).revealing(line)
        }
    }

    /// The char offset of `document` shown at cell `column` of row `row` of
    /// the view, laid out with `options`, when the cell shows text: `None`
    /// for a cell of the gutter, past the end of its line, or below the
    /// last row.
    pub(crate) fn text_offset_at_cell(
        &self,
        document: &Document,
        decorations: &Decorations,
        options: &EditorOptions,
        column: u16,
        row: u16,
    ) -> Option<usize> {
        let area = self.area(document, decorations, options);
        let x = usize::from(column).checked_sub(area.gutter_width)? + self.scroll.column;
        let at = area.row_below(self.top(), usize::from(row))?;
        let offset = area.offset_in_row(at, x);
        document
            .char_at(offset)
            .filter(|c| *c != '\n')
            .map(|_| offset)
    }

    /// What the view shows differently from the editor's options.
    pub(crate) fn options(&self) -> ViewOptions {
        self.options
    }

    /// Makes `options` what the view shows differently.
    pub(crate) fn set_options(&mut self, options: ViewOptions) {
        self.options = options;
    }

    /// The char offset of `document` shown at cell `column` of row `row` of
    /// the view, laid out with `options`: a cell in the gutter counts as
    /// the first text cell, a row past the end as the last row.
    pub(crate) fn offset_at_cell(
        &self,
        document: &Document,
        decorations: &Decorations,
        options: &EditorOptions,
        column: u16,
        row: u16,
    ) -> usize {
        let area = self.area(document, decorations, options);
        let x = usize::from(column).saturating_sub(area.gutter_width) + self.scroll.column;
        let at = area
            .row_below(self.top(), usize::from(row))
            .unwrap_or_else(|| area.last_row());
        area.offset_in_row(at, x)
    }

    /// Scrolls `count` screen rows down (or up when not `down`), no further
    /// than the first row at the top or the last row at the bottom, and
    /// returns where the cursor must go to stay in sight with the scroll
    /// margin, at its cell; `None` when it may stay.
    pub(crate) fn scroll_rows(
        &mut self,
        document: &Document,
        decorations: &Decorations,
        options: &EditorOptions,
        down: bool,
        count: usize,
    ) -> Option<usize> {
        let preedit = self.preedit.clone();
        let area = self.area_with_preedit(document, decorations, options, preedit.as_deref());
        let height = usize::from(self.rows).max(1);
        let top = self.top();
        let last = area.last_row();
        let new_top = if down {
            let lowest = area.rows_above(last, height - 1).max(top);
            area.row_below(top, count).unwrap_or(last).min(lowest)
        } else {
            area.rows_above(top, count)
        };
        self.scroll.line = new_top.line;
        self.scroll.row = new_top.row;
        let margin = options.scroll_margin.min((height - 1) / 2);
        let first = ScreenRow { line: 0, row: 0 };
        let highest = if new_top == first {
            first
        } else {
            area.row_below(new_top, margin).unwrap_or(last)
        };
        let lowest = match area.row_below(new_top, height - 1) {
            Some(bottom) if bottom != last => area.rows_above(bottom, margin),
            Some(_) | None => last,
        };
        let (line, cell) = area.locate(self.cursor());
        let cursor = ScreenRow {
            line,
            row: cell.row,
        };
        let target = cursor.max(highest).min(lowest);
        (target != cursor).then(|| area.offset_in_row(target, cell.x))
    }

    /// The char offset `count` screen rows below `offset` (above it when
    /// not `down`), at the same cell of its row or as near as the row
    /// allows; as far as the document goes, `None` when it cannot move.
    pub(crate) fn offset_rows_away(
        &self,
        document: &Document,
        decorations: &Decorations,
        options: &EditorOptions,
        offset: usize,
        down: bool,
        count: usize,
    ) -> Option<usize> {
        let area = self.area(document, decorations, options);
        let (line, cell) = area.locate(offset);
        let from = ScreenRow {
            line,
            row: cell.row,
        };
        let mut target = if down {
            let taken = area.rows_below(from, count);
            area.row_below(from, taken)?
        } else {
            area.rows_above(from, count)
        };
        while area.is_virtual_row(target) {
            let next = match down {
                true => area.row_below(target, 1),
                false => Some(area.rows_above(target, 1)).filter(|above| *above != target),
            };
            target = next?;
        }
        (target != from).then(|| area.offset_in_row(target, cell.x))
    }

    /// The char offset shown at `place` of the screen row char `offset` of
    /// `document`, laid out with `options`, is on: the cluster covering
    /// that cell, else the row's last cluster, or the line's end past the
    /// end of its last row.
    pub(crate) fn offset_at_row_place(
        &self,
        document: &Document,
        decorations: &Decorations,
        options: &EditorOptions,
        offset: usize,
        place: RowPlace,
    ) -> usize {
        let area = self.area(document, decorations, options);
        let (line, cell) = area.locate(offset);
        let width = area.width.max(1);
        let left = if options.wrap { 0 } else { self.scroll.column };
        let x = match place {
            RowPlace::End => left + width - 1,
            RowPlace::Middle => left + width / 2,
            RowPlace::Start => left,
        };
        area.offset_in_row(
            ScreenRow {
                line,
                row: cell.row,
            },
            x,
        )
    }

    /// The cell, gutter included, where char `offset` of `document` shows,
    /// laid out with `options`, as `(column, row)`; `None` when it is out
    /// of sight.
    pub(crate) fn cell_of(
        &self,
        document: &Document,
        decorations: &Decorations,
        options: &EditorOptions,
        offset: usize,
    ) -> Option<(u16, u16)> {
        let area = self.area(document, decorations, options);
        let (line, cell) = area.locate(offset);
        let target = ScreenRow {
            line,
            row: cell.row,
        };
        let top = self.top();
        if target < top {
            return None;
        }
        let row =
            (0..usize::from(self.rows)).find(|row| area.row_below(top, *row) == Some(target))?;
        let x = cell.x.checked_sub(self.scroll.column)?;
        if x >= area.width.max(1) {
            return None;
        }
        Some((
            u16::try_from(area.gutter_width + x).ok()?,
            u16::try_from(row).ok()?,
        ))
    }

    /// The lines shown on the rows of the view that have text, laid out
    /// with `options`.
    pub(crate) fn screen_lines(
        &self,
        document: &Document,
        decorations: &Decorations,
        options: &EditorOptions,
    ) -> ScreenLines {
        let area = self.area(document, decorations, options);
        let height = usize::from(self.rows).max(1);
        let top = self.top();
        let shown = area.rows_below(top, height - 1) + 1;
        let lines = (0..shown)
            .filter_map(|row| area.row_below(top, row))
            .map(|row| row.line)
            .collect();
        let bottom = area.row_below(top, shown - 1);
        ScreenLines {
            lines,
            shows_end: bottom == Some(area.last_row()),
            shows_start: top == ScreenRow { line: 0, row: 0 },
        }
    }

    /// Scrolls so the cursor's row of `document`, laid out with `options`,
    /// is at `place`, keeping the scroll margin where the document allows.
    pub(crate) fn scroll_cursor_to(
        &mut self,
        document: &Document,
        decorations: &Decorations,
        options: &EditorOptions,
        place: CursorPlace,
    ) {
        let area = self.area(document, decorations, options);
        let (line, cell) = area.locate(self.cursor());
        let cursor = ScreenRow {
            line,
            row: cell.row,
        };
        let height = usize::from(self.rows).max(1);
        let margin = options.scroll_margin.min((height - 1) / 2);
        let rows_above = match place {
            CursorPlace::Bottom => height - 1 - area.rows_below(cursor, margin),
            CursorPlace::Center => (height - 1) / 2,
            CursorPlace::Top => margin,
        };
        let top = area.rows_above(cursor, rows_above);
        self.scroll.line = top.line;
        self.scroll.row = top.row;
    }

    /// Scrolls the least that shows the cursor of `document`, laid out with
    /// `options`, with the scroll margin kept as the module documentation
    /// describes.
    pub(crate) fn scroll_to_cursor(
        &mut self,
        document: &Document,
        decorations: &Decorations,
        options: &EditorOptions,
    ) {
        let preedit = self.preedit.clone();
        let area = self.area_with_preedit(document, decorations, options, preedit.as_deref());
        let (line, cell) = area.locate(self.cursor());
        let cursor = ScreenRow {
            line,
            row: cell.row,
        };
        let height = usize::from(self.rows).max(1);
        if self.margin_waived_at != Some(self.cursor()) {
            self.margin_waived_at = None;
        }
        let margin = match self.margin_waived_at {
            Some(_) => 0,
            None => options.scroll_margin.min((height - 1) / 2),
        };
        let mut top = self.top().min(cursor);
        top.row = top.row.min(area.row_count(top.line) - 1);
        top = top.min(area.rows_above(cursor, margin));
        let below = area.rows_below(cursor, margin);
        top = top.max(area.rows_above(cursor, height - 1 - below));
        self.scroll.line = top.line;
        self.scroll.row = top.row;
        self.scroll.column = if options.wrap {
            0
        } else {
            let width = area.width.max(1);
            let column = self.scroll.column.min(cell.x);
            column.max((cell.x + 1).saturating_sub(width))
        };
    }

    /// Sets where the mouse button went down.
    pub(crate) fn set_mouse_anchor(&mut self, anchor: Option<usize>) {
        self.mouse_anchor = anchor;
    }

    /// Sets the change being recorded for `.`.
    pub(crate) fn set_change(&mut self, change: Option<ChangeCandidate>) {
        self.change = change;
    }

    /// Sets the command waiting for a character.
    pub(crate) fn set_pending_char(&mut self, pending: Option<PendingChar>) {
        self.pending_char = pending;
    }

    /// Sets the keys being recorded with `q`.
    pub(crate) fn set_recording(&mut self, recording: Option<MacroRecording>) {
        self.recording = recording;
    }

    /// Sets the line holding only the indent Insert mode gave it.
    pub(crate) fn set_auto_indented_line(&mut self, line: Option<usize>) {
        self.auto_indented_line = line;
    }

    /// Sets whether the register name is awaited after a `"`.
    pub(crate) fn set_awaiting_register(&mut self, awaiting: bool) {
        self.awaiting_register = awaiting;
    }

    /// Sets the display column up and down motions aim for.
    pub(crate) fn set_goal_column(&mut self, goal_column: usize) {
        self.goal_column = Some(goal_column);
    }

    /// Switches to `mode`, dropping any pending keys, count, and selected
    /// register, and the waiting operator unless `mode` is operator-pending
    /// or command-line. Leaving the visual modes remembers the selection
    /// for `gv`; switching between them, or to command-line mode, keeps it.
    pub(crate) fn set_mode(&mut self, mode: Mode) {
        if mode == Mode::VisualBlock {
            self.extra_cursors.clear();
        }
        if mode != Mode::Insert {
            self.snippet = None;
        }
        if !is_visual(mode)
            && mode != Mode::CommandLine
            && let (true, Some(anchor)) = (is_visual(self.mode), self.visual_anchor.take())
        {
            self.last_visual = Some(VisualArea {
                anchor,
                cursor: self.cursor(),
                mode: self.mode,
            });
        }
        self.mode = mode;
        self.block_insert = None;
        self.completion = None;
        self.completion_wait = None;
        self.insert_start = None;
        self.replace_repeat = None;
        self.replace_session = None;
        self.awaiting_register = false;
        self.count = None;
        self.pending_char = None;
        self.register = None;
        self.pending.reset();
        if !matches!(mode, Mode::CommandLine | Mode::OperatorPending) {
            self.operator = None;
        }
    }

    /// Sets the operator waiting for its motion; call
    /// [`View::set_mode`] with operator-pending mode after.
    pub(crate) fn set_operator(&mut self, operator: PendingOperator) {
        self.operator = Some(operator);
    }

    /// Replaces the cursors and selected ranges, forgetting the goal
    /// column; call [`View::scroll_to_cursor`] after to keep the cursor in
    /// sight.
    pub(crate) fn set_selection(&mut self, selection: Selection) {
        self.selection = selection;
        self.goal_column = None;
    }

    /// Takes the change being recorded for `.`.
    pub(crate) fn take_change(&mut self) -> Option<ChangeCandidate> {
        self.change.take()
    }

    /// Takes the command waiting for a character.
    pub(crate) fn take_pending_char(&mut self) -> Option<PendingChar> {
        self.pending_char.take()
    }

    /// Whether a command waits for a character.
    pub(crate) fn has_pending_char(&self) -> bool {
        self.pending_char.is_some()
    }

    /// Sets the text to repeat on a block's other lines; call after
    /// [`View::set_mode`], which drops it.
    pub(crate) fn set_block_insert(&mut self, insert: Option<BlockInsert>) {
        self.block_insert = insert;
    }

    /// Takes the text to repeat on a block's other lines.
    pub(crate) fn take_block_insert(&mut self) -> Option<BlockInsert> {
        self.block_insert.take()
    }

    /// Sets the open completion menu; call after [`View::set_mode`], which
    /// drops it.
    pub(crate) fn set_completion(&mut self, completion: Option<Completion>) {
        self.completion = completion;
    }

    /// The completion waiting for late answers, if any.
    pub(crate) fn completion_wait(&self) -> Option<&CompletionWait> {
        self.completion_wait.as_ref()
    }

    /// Sets the completion waiting for late answers, or with `None` stops
    /// waiting.
    pub(crate) fn set_completion_wait(&mut self, wait: Option<CompletionWait>) {
        self.completion_wait = wait;
    }

    /// Takes the open completion menu, closing it.
    pub(crate) fn take_completion(&mut self) -> Option<Completion> {
        self.completion.take()
    }

    /// Sets what typing in Replace mode replaced.
    pub(crate) fn set_replace_session(&mut self, session: Option<ReplaceSession>) {
        self.replace_session = session;
    }

    /// Takes what typing in Replace mode replaced.
    pub(crate) fn take_replace_session(&mut self) -> Option<ReplaceSession> {
        self.replace_session.take()
    }

    /// The count of `R` and the text typed over since, while in Replace
    /// mode.
    pub(crate) fn replace_repeat_mut(&mut self) -> Option<&mut ReplaceRepeat> {
        self.replace_repeat.as_mut()
    }

    /// Sets the count of `R` and the text typed over since.
    pub(crate) fn set_replace_repeat(&mut self, repeat: Option<ReplaceRepeat>) {
        self.replace_repeat = repeat;
    }

    /// Where the text typed in the insert session starts, if one is on.
    pub(crate) fn insert_start(&self) -> Option<usize> {
        self.insert_start
    }

    /// Sets where the text typed in the insert session starts.
    pub(crate) fn set_insert_start(&mut self, start: Option<usize>) {
        self.insert_start = start;
    }

    /// Takes the count of `R` and the text typed over since.
    pub(crate) fn take_replace_repeat(&mut self) -> Option<ReplaceRepeat> {
        self.replace_repeat.take()
    }

    /// Takes the keys being recorded with `q`, ending the recording.
    pub(crate) fn take_recording(&mut self) -> Option<MacroRecording> {
        self.recording.take()
    }

    /// Sets where the visual selection starts; call [`View::set_mode`]
    /// with a visual mode first.
    pub(crate) fn set_visual_anchor(&mut self, anchor: usize) {
        self.visual_anchor = Some(anchor);
    }

    /// The screen row at the top of the view.
    fn top(&self) -> ScreenRow {
        ScreenRow {
            line: self.scroll.line,
            row: self.scroll.row,
        }
    }
}

// ========================================================================
// Functions
// ========================================================================

/// The host's gutter lanes of `decorations` and then the sign column when
/// `options` show it, each as its name (`None` for the sign column) and
/// width.
fn lane_widths<'a>(
    decorations: &'a Decorations,
    options: &EditorOptions,
) -> impl Iterator<Item = (Option<&'a str>, usize)> + use<'a> {
    let sign_column = options.sign_column.then_some((None, SIGN_COLUMN_WIDTH));
    decorations
        .lanes()
        .iter()
        .map(|lane| (Some(lane.name.as_str()), usize::from(lane.width)))
        .chain(sign_column)
}

/// Whether `mode` is one of the visual modes.
pub(crate) fn is_visual(mode: Mode) -> bool {
    match mode {
        Mode::Visual | Mode::VisualBlock | Mode::VisualLine => true,
        Mode::CommandLine | Mode::Insert | Mode::Normal | Mode::OperatorPending | Mode::Replace => {
            false
        }
    }
}

// ========================================================================
// Tests
// ========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn view(cols: u16, rows: u16) -> View {
        View::new(DocumentId::default(), cols, rows, &EditorOptions::default())
    }

    fn options(scroll_margin: usize, wrap: bool) -> EditorOptions {
        EditorOptions {
            scroll_margin,
            wrap,
            ..EditorOptions::default()
        }
    }

    /// A document of `count` lines named `l0`, `l1`, and so on.
    fn numbered_lines(count: usize) -> Document {
        let lines: Vec<_> = (0..count).map(|line| format!("l{line}")).collect();
        Document::new(&lines.join("\n"))
    }

    /// Moves the cursor of `view` to the start of `line` and scrolls.
    fn go_to_line(view: &mut View, document: &Document, options: &EditorOptions, line: usize) {
        let offset = document
            .position_to_char(Position { column: 0, line })
            .unwrap();
        view.set_selection(Selection::point(offset));
        view.scroll_to_cursor(document, &Decorations::default(), options);
    }

    #[test]
    fn test_scrolling_down_keeps_the_margin_below_the_cursor() {
        let (document, options) = (numbered_lines(100), options(2, false));
        let mut view = view(20, 10);
        go_to_line(&mut view, &document, &options, 7);
        assert_eq!(view.scroll().line, 0, "line 7 is shown with 2 rows below");
        go_to_line(&mut view, &document, &options, 8);
        assert_eq!(view.scroll().line, 1);
        go_to_line(&mut view, &document, &options, 50);
        assert_eq!(view.scroll().line, 43);
    }

    #[test]
    fn test_scrolling_up_keeps_the_margin_above_the_cursor() {
        let (document, options) = (numbered_lines(100), options(2, false));
        let mut view = view(20, 10);
        go_to_line(&mut view, &document, &options, 50);
        go_to_line(&mut view, &document, &options, 45);
        assert_eq!(view.scroll().line, 43, "still inside the margins");
        go_to_line(&mut view, &document, &options, 44);
        assert_eq!(view.scroll().line, 42);
        go_to_line(&mut view, &document, &options, 1);
        assert_eq!(view.scroll().line, 0, "the margin stops at the top");
    }

    #[test]
    fn test_the_last_line_sits_on_the_bottom_row_without_blank_rows_below() {
        let (document, options) = (numbered_lines(100), options(3, false));
        let mut view = view(20, 10);
        go_to_line(&mut view, &document, &options, 99);
        assert_eq!(view.scroll().line, 90);
        go_to_line(&mut view, &document, &options, 98);
        assert_eq!(view.scroll().line, 90, "the margin below is cut short");
    }

    #[test]
    fn test_a_margin_larger_than_half_the_view_is_capped() {
        let (document, options) = (numbered_lines(100), options(50, false));
        let mut view = view(20, 5);
        go_to_line(&mut view, &document, &options, 10);
        assert_eq!(view.scroll().line, 8, "the cursor sits in the middle row");
    }

    #[test]
    fn test_wrapped_rows_count_when_scrolling() {
        let document = Document::new("aaaaaaaaaa\nb\nc\nd");
        let options = options(0, true);
        let mut view = view(4, 3);
        go_to_line(&mut view, &document, &options, 1);
        assert_eq!(
            (view.scroll().line, view.scroll().row),
            (0, 1),
            "line 0 wraps into rows 0 to 2, and the top falls inside it"
        );
        go_to_line(&mut view, &document, &options, 3);
        assert_eq!((view.scroll().line, view.scroll().row), (1, 0));
        go_to_line(&mut view, &document, &options, 0);
        assert_eq!((view.scroll().line, view.scroll().row), (0, 0));
    }

    #[test]
    fn test_without_wrap_the_view_scrolls_sideways_to_the_cursor() {
        let document = Document::new("0123456789abcdef\nx");
        let options = options(0, false);
        let mut view = view(5, 2);
        view.set_selection(Selection::point(12));
        view.scroll_to_cursor(&document, &Decorations::default(), &options);
        assert_eq!(view.scroll().column, 8);
        view.set_selection(Selection::point(10));
        view.scroll_to_cursor(&document, &Decorations::default(), &options);
        assert_eq!(view.scroll().column, 8, "column 10 is already in sight");
        view.set_selection(Selection::point(17));
        view.scroll_to_cursor(&document, &Decorations::default(), &options);
        assert_eq!(view.scroll().column, 0);
    }

    #[test]
    fn test_the_gutter_narrows_the_text_for_horizontal_scrolling() {
        let document = Document::new("0123456789");
        let options = EditorOptions {
            line_numbers: true,
            ..options(0, false)
        };
        let mut view = view(8, 1);
        view.set_selection(Selection::point(5));
        view.scroll_to_cursor(&document, &Decorations::default(), &options);
        assert_eq!(view.scroll().column, 2, "4 gutter cells leave 4 for text");
    }
}
