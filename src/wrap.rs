//! Line layout: where each grapheme cluster of one line lands in screen
//! cells, and how the line breaks into rows when it is soft wrapped.
//!
//! A cluster takes the cells its display width says: one for most, two for
//! a wide character such as `日`, and at least one even for a cluster the
//! width tables count as zero, so the cursor always has a cell to sit on. A
//! tab fills to the next tab stop, counted from the start of its row.
//!
//! An inlay, text a host lays into a line that is not the line's own (a
//! type hint, a parameter name), takes cells before the cluster at its
//! column, or after the last cluster for one at the line's end, and wraps
//! with the text. Its cells belong to no char: a cell inside one maps to
//! the char after it, and the cursor on that char shows after it.
//!
//! A hidden cluster, one whose first char a host conceals (the `**`
//! around bold text), takes no cells: its offsets show on the next cluster
//! shown, as the offsets inside a cluster show on it.
//!
//! When wrapping, a cluster that does not fit the rest of a row starts the
//! next row, leaving the cells it did not fit in blank; a tab that does not
//! fit instead fills the row to its end. A row always takes at least one
//! cluster, so even a viewport narrower than one cluster makes progress.
//! With break indent, each row after the first starts at the cells the
//! line's indent takes, as Vim's `breakindent` does, keeping at least
//! [`MIN_BREAK_ROOM`] cells for text.
//!
//! A very long line that is not wrapped is never laid out whole: only the
//! part in sight is ([`LineLayout::window`]), found by counting the cells of
//! the chars before it ([`char_cells`]), where a char of zero width joins
//! the cluster before it. That count matches the clusters of nearly all
//! text; on such a line a rare cluster built otherwise (an emoji sequence
//! joined by zero-width joiners) may be counted a cell or two wider than
//! its layout.

use std::ops::Range;

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

// ========================================================================
// Constants
// ========================================================================

/// The tab character, laid out as blank cells to the next tab stop.
pub(crate) const TAB: &str = "\t";

/// The widest a cluster is laid out, in cells.
const MAX_GLYPH_WIDTH: usize = 2;

/// The fewest cells a wrapped row keeps for text after a break indent,
/// Vim's `breakindentopt` minimum.
const MIN_BREAK_ROOM: usize = 20;

// ========================================================================
// Data Structures
// ========================================================================

/// One grapheme cluster placed on a row.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Glyph<'a> {
    /// The char offset of the cluster in its line; for a cluster of an
    /// inlay, the offset of the cluster it comes before.
    pub column: usize,
    /// The [`index`](LineInlay::index) of the inlay the cluster is part
    /// of; `None` for the line's own text.
    pub inlay: Option<usize>,
    /// The cluster as written; a tab is [`TAB`].
    pub text: &'a str,
    /// The cells it takes.
    pub width: usize,
    /// The cell it starts at, counted from the start of its row.
    pub x: usize,
}

/// Text laid into a line before the cluster at its column.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct LineInlay<'a> {
    /// The char offset in the line it comes before; the line's length for
    /// one at its end.
    pub column: usize,
    /// What the caller knows it by, given back in [`Glyph::inlay`].
    pub index: usize,
    /// The text, one line.
    pub text: &'a str,
}

/// One line laid out in rows of cells.
#[derive(Clone, Debug)]
pub(crate) struct LineLayout<'a> {
    /// The char offset in the line just past the text laid out: the
    /// line's length, unless only a part of it is laid out.
    end: usize,
    /// The cells each row after the first starts at: the break indent.
    indent: usize,
    /// The rows, top first; never empty, as an empty line has one row.
    /// The rows of virtual lines, if any, come after the text's.
    rows: Vec<Row<'a>>,
    /// How many of the rows are the text's.
    text_rows: usize,
    /// The cells a row holds when wrapping, or `None` for one long row.
    wrap_width: Option<usize>,
}

/// Where in a line's layout a cell is: a row of the line and a cell of it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct RowCell {
    /// The row, counted from the line's first row.
    pub row: usize,
    /// The cell, counted from the start of the row.
    pub x: usize,
}

/// One screen row of a line.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct Row<'a> {
    /// The clusters on the row, left to right.
    pub glyphs: Vec<Glyph<'a>>,
    /// The cells the clusters fill.
    pub width: usize,
    /// For the row of a virtual line after the text, its index among the
    /// line's virtual lines; `None` for a row of the text.
    pub virtual_line: Option<usize>,
}

// ========================================================================
// LineLayout Implementations
// ========================================================================

impl<'a> LineLayout<'a> {
    /// The layout of `text`, one line without its break, with tab stops
    /// every `tab_width` cells, wrapped into rows of `wrap_width` cells when
    /// given.
    pub fn new(text: &'a str, wrap_width: Option<usize>, tab_width: usize) -> Self {
        Self::decorated(text, &[], &[], wrap_width, tab_width, false)
    }

    /// The layout of `text`, as [`LineLayout::new`] lays it out, with
    /// `inlays`, sorted by column, each before the cluster at its column,
    /// and without the clusters whose first char is in one of the `hidden`
    /// char ranges; with `break_indent`, a wrapped row after the first
    /// starts at the line's indent.
    pub fn decorated(
        text: &'a str,
        inlays: &[LineInlay<'a>],
        hidden: &[Range<usize>],
        wrap_width: Option<usize>,
        tab_width: usize,
        break_indent: bool,
    ) -> Self {
        let indent = match (break_indent, wrap_width) {
            (true, Some(wrap_width)) => {
                indent_cells(text, tab_width).min(wrap_width.saturating_sub(MIN_BREAK_ROOM))
            }
            (false, _) | (true, None) => 0,
        };
        let mut layout = Self {
            end: 0,
            indent,
            rows: vec![Row::default()],
            text_rows: 1,
            wrap_width,
        };
        let mut pending = inlays.iter().peekable();
        let mut column = 0;
        for cluster in text.graphemes(true) {
            let chars = cluster.chars().count();
            while let Some(inlay) = pending.next_if(|inlay| inlay.column < column + chars) {
                layout.place_text(inlay.text, column, Some(inlay.index), tab_width);
            }
            if !hidden.iter().any(|range| range.contains(&column)) {
                layout.place(cluster, column, None, tab_width);
            }
            column += chars;
        }
        for inlay in pending {
            layout.place_text(inlay.text, column, Some(inlay.index), tab_width);
        }
        layout.end = column;
        layout.text_rows = layout.rows.len();
        layout
    }

    /// Adds `count` rows of virtual lines after the text's.
    pub fn push_virtual_rows(&mut self, count: usize) {
        for index in 0..count {
            self.rows.push(Row {
                virtual_line: Some(index),
                ..Row::default()
            });
        }
    }

    /// The layout of `text`, a part of a line that is not wrapped, whose
    /// first cluster is char `column` of the line and starts at cell `x`:
    /// one row, its glyphs placed as in the whole line's layout.
    pub fn window(text: &'a str, column: usize, x: usize, tab_width: usize) -> Self {
        let mut row = Row {
            glyphs: Vec::new(),
            virtual_line: None,
            width: x,
        };
        let mut column = column;
        for cluster in text.graphemes(true) {
            let width = glyph_width(cluster, row.width, tab_width);
            row.glyphs.push(Glyph {
                column,
                inlay: None,
                text: cluster,
                width,
                x: row.width,
            });
            row.width += width;
            column += cluster.chars().count();
        }
        Self {
            end: column,
            indent: 0,
            rows: vec![row],
            text_rows: 1,
            wrap_width: None,
        }
    }

    /// The char offset in the line of the cluster covering cell `x` of the
    /// first row, or the end of the text laid out when `x` is past its last
    /// cluster. Meant for a layout made without wrapping, whose one row is
    /// the line.
    pub fn column_at(&self, x: usize) -> usize {
        self.rows[0]
            .glyphs
            .iter()
            .find(|glyph| x < glyph.x + glyph.width)
            .map_or(self.end, |glyph| glyph.column)
    }

    /// The cell a cursor at char offset `column` of the line is shown in.
    /// An offset inside a cluster shows on the cluster; one at or past the
    /// end of the line shows just after the last cluster, kept on the last
    /// cell of a wrapped row that is full.
    pub fn locate(&self, column: usize) -> RowCell {
        for (row, cells) in self.rows.iter().enumerate() {
            let glyph = cells
                .glyphs
                .iter()
                .find(|glyph| column < glyph.column + glyph.char_len());
            if let Some(glyph) = glyph {
                return RowCell { row, x: glyph.x };
            }
        }
        let row = self.text_rows.max(1) - 1;
        let end = self.rows[row].width;
        let x = match self.wrap_width {
            Some(wrap_width) => end.min(wrap_width.saturating_sub(1)),
            None => end,
        };
        RowCell { row, x }
    }

    /// The rows, top first.
    pub fn rows(&self) -> &[Row<'a>] {
        &self.rows
    }

    /// Places `cluster`, of char offset `column` (or before it, for a
    /// cluster of inlay `inlay`), after the clusters placed so far,
    /// starting a row when it does not fit a wrapped one.
    fn place(&mut self, cluster: &'a str, column: usize, inlay: Option<usize>, tab_width: usize) {
        let row = self.rows.last_mut().expect("a layout has a row");
        let mut width = glyph_width(cluster, row.width, tab_width);
        if let Some(wrap_width) = self.wrap_width {
            let overflows = row.width + width > wrap_width && !row.glyphs.is_empty();
            if overflows && cluster == TAB && row.width < wrap_width {
                width = wrap_width - row.width;
            } else if overflows {
                width = glyph_width(cluster, self.indent, tab_width);
                self.rows.push(Row {
                    glyphs: Vec::new(),
                    virtual_line: None,
                    width: self.indent,
                });
            }
        }
        let row = self.rows.last_mut().expect("a layout has a row");
        row.glyphs.push(Glyph {
            column,
            inlay,
            text: cluster,
            width,
            x: row.width,
        });
        row.width += width;
    }

    /// Places each cluster of `text`, inlay `inlay`, before char offset
    /// `column`.
    fn place_text(&mut self, text: &'a str, column: usize, inlay: Option<usize>, tab_width: usize) {
        for cluster in text.graphemes(true) {
            self.place(cluster, column, inlay, tab_width);
        }
    }
}

// ========================================================================
// Glyph Implementations
// ========================================================================

impl Glyph<'_> {
    /// The chars of the line the cluster stands for: none for a cluster of
    /// an inlay.
    pub fn char_len(&self) -> usize {
        match self.inlay {
            Some(_) => 0,
            None => self.text.chars().count(),
        }
    }
}

// ========================================================================
// Functions
// ========================================================================

/// The cells char `c` adds at cell `x` of a row that is not wrapped, for
/// counting the cells of a long line without laying it out: to the next
/// tab stop for a tab, none for a char of zero width (it joins the cluster
/// before it), else its display width up to [`MAX_GLYPH_WIDTH`].
pub(crate) fn char_cells(c: char, x: usize, tab_width: usize) -> usize {
    if c == '\t' {
        return tab_width - x % tab_width;
    }
    c.width().unwrap_or(0).min(MAX_GLYPH_WIDTH)
}

/// The cells the blanks and tabs at the start of `text` take.
fn indent_cells(text: &str, tab_width: usize) -> usize {
    text.chars()
        .take_while(|&c| c == ' ' || c == '\t')
        .fold(0, |x, c| x + char_cells(c, x, tab_width))
}

/// The cells `cluster` takes when it starts at cell `x` of its row: to the
/// next tab stop for a tab, else its display width kept within one and
/// [`MAX_GLYPH_WIDTH`].
fn glyph_width(cluster: &str, x: usize, tab_width: usize) -> usize {
    if cluster == TAB {
        return tab_width - x % tab_width;
    }
    cluster.width().clamp(1, MAX_GLYPH_WIDTH)
}

// ========================================================================
// Tests
// ========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// Each row of `layout` as its glyphs' `(x, text)` pairs.
    fn cells<'a>(layout: &LineLayout<'a>) -> Vec<Vec<(usize, &'a str)>> {
        layout
            .rows()
            .iter()
            .map(|row| {
                row.glyphs
                    .iter()
                    .map(|glyph| (glyph.x, glyph.text))
                    .collect()
            })
            .collect()
    }

    #[test]
    fn test_tabs_fill_to_the_next_tab_stop() {
        let layout = LineLayout::new("a\tbc\td", None, 4);
        assert_eq!(
            cells(&layout),
            vec![vec![
                (0, "a"),
                (1, "\t"),
                (4, "b"),
                (5, "c"),
                (6, "\t"),
                (8, "d")
            ]]
        );
        assert_eq!(layout.rows()[0].width, 9);
    }

    #[test]
    fn test_a_long_line_wraps_at_the_wrap_width() {
        let layout = LineLayout::new("abcdefg", Some(3), 4);
        assert_eq!(
            cells(&layout),
            vec![
                vec![(0, "a"), (1, "b"), (2, "c")],
                vec![(0, "d"), (1, "e"), (2, "f")],
                vec![(0, "g")],
            ]
        );
    }

    #[test]
    fn test_a_wide_char_that_does_not_fit_starts_the_next_row() {
        let layout = LineLayout::new("ab日本", Some(3), 4);
        assert_eq!(
            cells(&layout),
            vec![vec![(0, "a"), (1, "b")], vec![(0, "日")], vec![(0, "本")]]
        );
        assert_eq!(layout.rows()[1].width, 2);
        assert_eq!(layout.locate(3), RowCell { row: 2, x: 0 });
    }

    #[test]
    fn test_a_tab_that_does_not_fit_fills_the_row_and_stops_restart_per_row() {
        let layout = LineLayout::new("abcde\tf\tg", Some(7), 4);
        let rows = layout.rows();
        assert_eq!(rows[0].glyphs[5].width, 2, "the tab is cut at the row end");
        assert_eq!(rows[0].width, 7);
        assert_eq!(cells(&layout)[1], vec![(0, "f"), (1, "\t"), (4, "g")]);
    }

    #[test]
    fn test_clusters_are_located_by_any_char_offset_inside_them() {
        let layout = LineLayout::new("e\u{301}日x", None, 4);
        assert_eq!(layout.locate(1), RowCell { row: 0, x: 0 });
        assert_eq!(layout.locate(2), RowCell { row: 0, x: 1 });
        assert_eq!(layout.locate(3), RowCell { row: 0, x: 3 });
        assert_eq!(layout.locate(4), RowCell { row: 0, x: 4 }, "end of line");
    }

    #[test]
    fn test_a_cell_inside_a_wide_char_or_tab_maps_back_to_its_cluster() {
        let layout = LineLayout::new("a日	b", None, 4);
        assert_eq!(layout.column_at(0), 0);
        assert_eq!(layout.column_at(2), 1, "the second cell of 日");
        assert_eq!(layout.column_at(3), 2, "the tab spans cell 3 alone");
        assert_eq!(layout.column_at(4), 3);
        assert_eq!(layout.column_at(99), 4, "past the end is the line end");
        assert_eq!(LineLayout::new("", None, 4).column_at(5), 0);
    }

    #[test]
    fn test_the_end_of_a_full_wrapped_row_stays_on_its_last_cell() {
        let layout = LineLayout::new("abcdef", Some(3), 4);
        assert_eq!(layout.locate(6), RowCell { row: 1, x: 2 });
        assert_eq!(
            LineLayout::new("", Some(3), 4).locate(0),
            RowCell { row: 0, x: 0 }
        );
    }

    #[test]
    fn test_an_inlay_takes_cells_before_its_char_and_belongs_to_no_char() {
        let inlays = [LineInlay {
            column: 1,
            index: 4,
            text: ": T",
        }];
        let layout = LineLayout::decorated("a\tb", &inlays, &[], None, 4, false);
        assert_eq!(
            cells(&layout),
            vec![vec![
                (0, "a"),
                (1, ":"),
                (2, " "),
                (3, "T"),
                (4, "\t"),
                (8, "b")
            ]],
            "the tab fills to the stop after the inlay's cells"
        );
        assert_eq!(
            layout.locate(1),
            RowCell { row: 0, x: 4 },
            "after the inlay"
        );
        assert_eq!(layout.locate(3), RowCell { row: 0, x: 9 }, "end of line");
        assert_eq!(
            layout.column_at(2),
            1,
            "an inlay cell maps to the char after it"
        );
        assert_eq!(layout.rows()[0].glyphs[2].inlay, Some(4));
        assert_eq!(layout.rows()[0].glyphs[0].inlay, None);
    }

    #[test]
    fn test_an_inlay_at_the_end_of_a_line_wraps_with_it() {
        let inlays = [LineInlay {
            column: 3,
            index: 0,
            text: "XY",
        }];
        let layout = LineLayout::decorated("abc", &inlays, &[], Some(4), 4, false);
        assert_eq!(
            cells(&layout),
            vec![vec![(0, "a"), (1, "b"), (2, "c"), (3, "X")], vec![(0, "Y")]]
        );
        assert_eq!(layout.locate(3), RowCell { row: 1, x: 1 });
    }

    #[test]
    fn test_hidden_clusters_take_no_cells_and_their_offsets_show_on_the_next_one() {
        let hidden = [0..2, 3..5];
        let layout = LineLayout::decorated("**b**\tc", &[], &hidden, None, 4, false);
        assert_eq!(cells(&layout), vec![vec![(0, "b"), (1, "\t"), (4, "c")]]);
        assert_eq!(layout.locate(0), RowCell { row: 0, x: 0 }, "on the b");
        assert_eq!(layout.locate(3), RowCell { row: 0, x: 1 }, "on the tab");
        assert_eq!(layout.column_at(0), 2);
        assert_eq!(layout.column_at(99), 7, "past the end is the line end");
    }

    #[test]
    fn test_break_indent_starts_wrapped_rows_at_the_indent_leaving_room_for_text() {
        let text = format!("\t  {}", "x".repeat(60));
        let layout = LineLayout::decorated(&text, &[], &[], Some(30), 4, true);
        let starts: Vec<usize> = layout.rows().iter().map(|row| row.glyphs[0].x).collect();
        assert_eq!(starts, [0, 6, 6], "the tab and two blanks");
        assert!(layout.rows().iter().all(|row| row.width <= 30));
        assert_eq!(
            layout.locate(30),
            RowCell { row: 1, x: 9 },
            "char 27 starts row 1"
        );
        let narrow = LineLayout::decorated(&text, &[], &[], Some(24), 4, true);
        assert_eq!(narrow.rows()[1].glyphs[0].x, 4, "20 cells kept for text");
        let off = LineLayout::decorated(&text, &[], &[], Some(30), 4, false);
        assert_eq!(off.rows()[1].glyphs[0].x, 0);
    }

    #[test]
    fn test_a_viewport_narrower_than_a_cluster_still_lays_out_every_cluster() {
        let layout = LineLayout::new("日本", Some(1), 4);
        assert_eq!(cells(&layout), vec![vec![(0, "日")], vec![(0, "本")]]);
        let zero = LineLayout::new("ab", Some(0), 4);
        assert_eq!(zero.rows().len(), 2);
    }
}
