//! The paint model: what a host draws for a view. A [`Surface`] is a grid
//! of [`Cell`]s, each a symbol and a [`Style`]; the style names what the
//! cell shows (text, the cursor line, a selection, a line number), and the
//! host maps each style to its own colors and fonts. The cursor comes apart
//! from the grid, as a [`Cursor`] with a [`CursorShape`], so a host draws it
//! with its own caret.
//!
//! A cell may also carry an [`underline`](Cell::underline), the kind of
//! decoration under it (a diagnostic, say), for the host to draw over the
//! style as it likes: a colored undercurl, a plain underline; and a
//! [`highlight`](Cell::highlight), what a syntax highlighter says its text
//! is, for the host to color the text by; and [`layers`](Cell::layers),
//! marks under the style that may come together with any of them (an
//! indentation guide, a lit occurrence of the word at the cursor), for the
//! host to draw beneath the text.
//!
//! A wide character takes two cells: the first holds its symbol, the second
//! an empty symbol the host skips, as the character already covers it.

use std::ops::BitOr;

use super::decoration::DecorationKind;
use super::highlight::HighlightId;

// ========================================================================
// Constants
// ========================================================================

/// The symbol of a blank cell.
const BLANK: &str = " ";

// ========================================================================
// Data Structures
// ========================================================================

/// One cell of a surface.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Cell {
    /// What the text in the cell is, by the document's
    /// [`Highlighter`](crate::Highlighter) (a keyword, a string), for the
    /// host to color the text by while the style gives the background;
    /// `None` for plain text and cells outside the text.
    pub highlight: Option<HighlightId>,
    /// The marks under the style: none for most cells.
    pub layers: Layers,
    /// What the cell shows.
    pub style: Style,
    /// The grapheme cluster drawn in the cell; empty for the second cell of
    /// a wide character, which the host skips.
    pub symbol: String,
    /// The most severe decoration under the cell, other than a highlight
    /// (which paints as [`Style::Highlight`]), for the host to draw as an
    /// underline over the style; `None` for most cells.
    pub underline: Option<DecorationKind>,
}

/// The marks under a cell's style, as a set: each may come with any style
/// and with the others, so a host draws them beneath the text (a faint
/// line for a guide, a faint band for an occurrence) and the style over
/// them.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub struct Layers(u8);

/// Where the cursor is drawn on a surface, and how.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Cursor {
    /// The cell column, from 0 at the left edge.
    pub column: u16,
    /// The cell row, from 0 at the top edge.
    pub row: u16,
    /// How the cursor is drawn.
    pub shape: CursorShape,
}

/// How the cursor is drawn.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[non_exhaustive]
pub enum CursorShape {
    /// A thin bar before the cell, for typing text in.
    Bar,
    /// A block over the cell, for moving around.
    Block,
    /// A line under the cell, for typing over text.
    Underline,
}

/// What a cell shows, for the host to give its own colors.
///
/// New variants may come in a minor release, so a host's `match` needs a
/// fallback arm.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
#[non_exhaustive]
pub enum Style {
    /// The marker after the first line of a closed fold, saying how many
    /// lines the fold hides.
    Folded,
    /// Text a host decoration gives a background of its own
    /// ([`DecorationKind::Background`]), such as a diff's added line; the
    /// id names it, for the host to color.
    Background(HighlightId),
    /// The cell an extra cursor is on ([`View::cursors`](crate::View::cursors)),
    /// for the host to draw as it draws its cursor; the main cursor comes
    /// apart, as a [`Cursor`].
    ExtraCursor,
    /// Text, or blank space, on the line the cursor is on.
    CursorLine,
    /// Text a host decoration highlights
    /// ([`DecorationKind::Highlight`]).
    Highlight,
    /// Text a host laid into a line that is not the document's own
    /// ([`Inlay`](crate::Inlay)), such as an inlay hint or ghost text;
    /// the cell's [`highlight`](Cell::highlight) is the inlay's.
    Inlay,
    /// The line number of the cursor's line.
    CursorLineNumber,
    /// The line number gutter.
    LineNumber,
    /// A bracket at the cursor and the one matching it
    /// ([`EditorOptions::match_brackets`](crate::EditorOptions::match_brackets)).
    MatchingBracket,
    /// Text an input method is composing at the cursor, not yet typed
    /// ([`Editor::set_preedit`](crate::Editor::set_preedit)); hosts
    /// usually underline it.
    Preedit,
    /// Text of a phrase lit by hand (`M-s h .`), with the phrase's color
    /// number, counting up as phrases are lit and wrapping, for the host
    /// to give each its own color.
    Phrase(u8),
    /// Text matching the last search pattern.
    SearchMatch,
    /// Selected text.
    Selection,
    /// A sign in the sign column, of the kind it marks; a sign column cell
    /// with no sign is [`Style::LineNumber`].
    Sign(DecorationKind),
    /// Text, or blank space; the default.
    #[default]
    Text,
}

/// A grid of cells, row by row, for a host to draw.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Surface {
    /// The cells, row after row.
    cells: Vec<Cell>,
    /// The number of cells in a row.
    cols: u16,
    /// The number of rows.
    rows: u16,
}

// ========================================================================
// Cell Implementations
// ========================================================================

impl Default for Cell {
    /// A blank cell of text.
    fn default() -> Self {
        Self {
            highlight: None,
            layers: Layers::NONE,
            style: Style::Text,
            symbol: BLANK.to_string(),
            underline: None,
        }
    }
}

impl Cell {
    /// Makes the cell show `symbol` with these layers, reusing its symbol's
    /// buffer.
    fn write(
        &mut self,
        symbol: &str,
        style: Style,
        highlight: Option<HighlightId>,
        underline: Option<DecorationKind>,
    ) {
        self.symbol.clear();
        self.symbol.push_str(symbol);
        self.highlight = highlight;
        self.layers = Layers::NONE;
        self.style = style;
        self.underline = underline;
    }
}

// ========================================================================
// Layers Implementations
// ========================================================================

impl Layers {
    /// The sign column's first cell by a line added
    /// ([`Editor::set_line_changes`](crate::Editor::set_line_changes)).
    pub const CHANGE_ADDED: Self = Self(1 << 3);
    /// The sign column's first cell by a line lines were deleted above.
    pub const CHANGE_DELETED: Self = Self(1 << 4);
    /// The sign column's first cell by a line modified.
    pub const CHANGE_MODIFIED: Self = Self(1 << 5);
    /// A blank cell at a level of its line's indentation
    /// ([`EditorOptions::indent_guides`](crate::EditorOptions::indent_guides)).
    pub const INDENT_GUIDE: Self = Self(1 << 0);
    /// No mark.
    pub const NONE: Self = Self(0);
    /// A row pinned at the top of the view
    /// ([`EditorOptions::sticky_scroll`](crate::EditorOptions::sticky_scroll)):
    /// a line opening a block the text in sight is inside, shown over the
    /// text scrolled under it.
    pub const PINNED: Self = Self(1 << 2);
    /// Text of a whole-word occurrence of the word at the cursor
    /// ([`EditorOptions::light_occurrences`](crate::EditorOptions::light_occurrences)),
    /// the one at the cursor included.
    pub const OCCURRENCE: Self = Self(1 << 1);

    /// Whether every mark in `other` is in this set.
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }
}

impl BitOr for Layers {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

// ========================================================================
// Surface Implementations
// ========================================================================

impl Surface {
    /// A surface of `cols` by `rows` blank cells.
    pub fn new(cols: u16, rows: u16) -> Self {
        Self {
            cells: vec![Cell::default(); usize::from(cols) * usize::from(rows)],
            cols,
            rows,
        }
    }

    /// The cell at `column` and `row`, or `None` outside the surface.
    pub fn cell(&self, column: u16, row: u16) -> Option<&Cell> {
        self.index(column, row).map(|index| &self.cells[index])
    }

    /// The number of cells in a row.
    pub fn cols(&self) -> u16 {
        self.cols
    }

    /// Makes the surface `cols` by `rows` blank cells, whatever it held.
    pub fn resize(&mut self, cols: u16, rows: u16) {
        *self = Self::new(cols, rows);
    }

    /// The symbols of row `row` joined, a wide character once; empty
    /// outside the surface. Handy for tests and for hosts that draw a row
    /// as one string.
    pub fn row_text(&self, row: u16) -> String {
        (0..self.cols)
            .filter_map(|column| self.cell(column, row))
            .map(|cell| cell.symbol.as_str())
            .collect()
    }

    /// The number of rows.
    pub fn rows(&self) -> u16 {
        self.rows
    }

    /// Gives the cell at `column` and `row` the style `style`, keeping its
    /// symbol and other layers; outside the surface, nothing.
    pub(crate) fn restyle(&mut self, column: usize, row: usize, style: Style) {
        let (Ok(column), Ok(row)) = (u16::try_from(column), u16::try_from(row)) else {
            return;
        };
        if let Some(index) = self.index(column, row) {
            self.cells[index].style = style;
        }
    }

    /// Adds `layers` to the cell at `column` and `row`; outside the
    /// surface, nothing.
    pub(crate) fn add_layers(&mut self, column: usize, row: usize, layers: Layers) {
        let (Ok(column), Ok(row)) = (u16::try_from(column), u16::try_from(row)) else {
            return;
        };
        if let Some(index) = self.index(column, row) {
            let cell = &mut self.cells[index];
            cell.layers = cell.layers | layers;
        }
    }

    /// Makes every cell blank text, keeping each symbol's buffer so a
    /// repaint does not allocate.
    pub(crate) fn clear(&mut self) {
        for cell in &mut self.cells {
            cell.write(BLANK, Style::Text, None, None);
        }
    }

    /// Puts `symbol` in `style` at `column` and `row`; outside the surface,
    /// nothing.
    pub(crate) fn put(&mut self, column: usize, row: usize, symbol: &str, style: Style) {
        self.put_layers(column, row, symbol, style, None, None);
    }

    /// Puts `symbol` with every layer (style, highlight, underline) at
    /// `column` and `row`, reusing the cell's symbol buffer; outside the
    /// surface, nothing.
    pub(crate) fn put_layers(
        &mut self,
        column: usize,
        row: usize,
        symbol: &str,
        style: Style,
        highlight: Option<HighlightId>,
        underline: Option<DecorationKind>,
    ) {
        let (Ok(column), Ok(row)) = (u16::try_from(column), u16::try_from(row)) else {
            return;
        };
        if let Some(index) = self.index(column, row) {
            self.cells[index].write(symbol, style, highlight, underline);
        }
    }

    /// The index in `cells` of `column` and `row`, or `None` outside.
    fn index(&self, column: u16, row: u16) -> Option<usize> {
        if column >= self.cols || row >= self.rows {
            return None;
        }
        Some(usize::from(row) * usize::from(self.cols) + usize::from(column))
    }
}
