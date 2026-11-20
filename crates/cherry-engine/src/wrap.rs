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
//! Wrapping at words, a row breaks before a piece of the line (the text
//! between two of the breaks UAX #14 allows) that does not fit, its blanks
//! left at the row's end, so the piece starts the next row whole; a piece
//! wider than a row breaks where the row ends, as when wrapping at chars.
//! With break indent, each row after the first starts at the cells the
//! line's indent takes, as Vim's `breakindent` does, or past what opens
//! the line (a list item's marker, a quote's `>`), keeping at least
//! [`MIN_BREAK_ROOM`] cells for text.
//!
//! Right-to-left text (Arabic, Hebrew) and text mixing directions show in
//! visual order: once a line is laid out in rows, the clusters of each row
//! are reordered by the Unicode Bidirectional Algorithm, the paragraph's
//! direction taken from its first strong character, so `abc ابت` shows
//! the Arabic letters right to left after `abc`. Offsets stay logical: the
//! cursor moves through the text in the order it is written and shows on
//! the cluster it is at, wherever that cluster is shown. A row still starts
//! at the left edge, whatever its direction.
//!
//! A very long line that is not wrapped is never laid out whole: only the
//! part in sight is ([`LineLayout::window`]), found by counting the cells of
//! the chars before it ([`char_cells`]), where a char of zero width joins
//! the cluster before it. That count matches the clusters of nearly all
//! text; on such a line a rare cluster built otherwise (an emoji sequence
//! joined by zero-width joiners) may be counted a cell or two wider than
//! its layout.

use std::ops::Range;

use unicode_bidi::{BidiClass, BidiInfo, bidi_class};
use unicode_linebreak::linebreaks;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use super::insert::list_marker;
use super::options::{BreakIndent, WrapMode};

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

/// LaTeX's list item command, which opens a line as a list marker does.
const LATEX_ITEM: &str = "\\item";

/// A Markdown task box after a list marker, as long as each of
/// [`TASK_BOX_MARKS`].
const TASK_BOX: &str = "[ ]";

/// The task boxes a list item may open with: to do, done, and cancelled.
const TASK_BOX_MARKS: [&str; 4] = ["[ ]", "[x]", "[X]", "[-]"];

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

/// How a line wraps into rows.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Wrap {
    /// Where each row after the first starts.
    pub break_indent: BreakIndent,
    /// Where the line breaks.
    pub mode: WrapMode,
    /// The cells a row holds.
    pub width: usize,
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
    /// Where the line breaks when wrapping.
    mode: WrapMode,
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
    /// The clusters on the row, in the order they are written; their
    /// cells run left to right but where right-to-left text reorders them.
    pub glyphs: Vec<Glyph<'a>>,
    /// The cells the clusters fill.
    pub width: usize,
    /// For the row of a virtual line after the text, its index among the
    /// line's virtual lines; `None` for a row of the text.
    pub virtual_line: Option<usize>,
}

// ========================================================================
// Row Implementations
// ========================================================================

impl Row<'_> {
    /// The glyph taking cell `x` of the row, or the leftmost glyph when `x`
    /// is left of them all (in a wrapped row's break indent); `None` past
    /// the row's last cell or for a row with no glyphs.
    pub fn glyph_at(&self, x: usize) -> Option<&Glyph<'_>> {
        self.glyphs
            .iter()
            .find(|glyph| glyph.covers(x))
            .or_else(|| {
                self.glyphs
                    .iter()
                    .min_by_key(|glyph| glyph.x)
                    .filter(|leftmost| x < leftmost.x)
            })
    }
}

// ========================================================================
// LineLayout Implementations
// ========================================================================

impl<'a> LineLayout<'a> {
    /// The layout of `text`, one line without its break, with tab stops
    /// every `tab_width` cells, wrapped into rows of `wrap_width` cells at
    /// chars when given.
    pub fn new(text: &'a str, wrap_width: Option<usize>, tab_width: usize) -> Self {
        let wrap = wrap_width.map(|width| Wrap {
            break_indent: BreakIndent::None,
            mode: WrapMode::Chars,
            width,
        });
        Self::decorated(text, &[], &[], wrap, tab_width)
    }

    /// The layout of `text`, as [`LineLayout::new`] lays it out, with
    /// `inlays`, sorted by column, each before the cluster at its column,
    /// and without the clusters whose first char is in one of the `hidden`
    /// char ranges, wrapped as `wrap` says when given.
    pub fn decorated(
        text: &'a str,
        inlays: &[LineInlay<'a>],
        hidden: &[Range<usize>],
        wrap: Option<Wrap>,
        tab_width: usize,
    ) -> Self {
        let indent = wrap.map_or(0, |wrap| {
            let chars = match wrap.break_indent {
                BreakIndent::Indent => indent_len(text),
                BreakIndent::None => 0,
                BreakIndent::Text => opener_len(text),
            };
            shown_cells(text, chars, hidden, tab_width)
                .min(wrap.width.saturating_sub(MIN_BREAK_ROOM))
        });
        let mode = wrap.map_or(WrapMode::Chars, |wrap| wrap.mode);
        let mut layout = Self {
            end: 0,
            indent,
            mode,
            rows: vec![Row::default()],
            text_rows: 1,
            wrap_width: wrap.map(|wrap| wrap.width),
        };
        let clusters: Vec<(usize, &str)> = text.grapheme_indices(true).collect();
        let breaks: Vec<usize> = match (wrap, mode) {
            (Some(_), WrapMode::Words) => linebreaks(text).map(|(byte, _)| byte).collect(),
            (Some(_), WrapMode::Chars) | (None, _) => Vec::new(),
        };
        let mut next_break = breaks.iter().peekable();
        let mut pending = inlays.iter().peekable();
        let mut column = 0;
        for (index, &(byte, cluster)) in clusters.iter().enumerate() {
            let chars = cluster.chars().count();
            while let Some(inlay) = pending.next_if(|inlay| inlay.column < column + chars) {
                layout.place_text(inlay.text, column, Some(inlay.index), tab_width);
            }
            while next_break.next_if(|&&at| at < byte).is_some() {}
            if next_break.next_if(|&&at| at == byte).is_some() {
                let end = next_break.peek().map_or(text.len(), |&&at| at);
                let piece = piece_cells(&clusters[index..], column, end, hidden);
                layout.break_before(piece);
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
        layout.order_visually(text);
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
            mode: WrapMode::Chars,
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
            .glyph_at(x)
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

    /// Reorders the glyphs of each text row into visual order by the
    /// Unicode Bidirectional Algorithm, when `text`, the line, has any
    /// right-to-left character; each glyph keeps its width and offset and
    /// takes its place along the row.
    fn order_visually(&mut self, text: &str) {
        let has_rtl = text
            .chars()
            .any(|c| matches!(bidi_class(c), BidiClass::R | BidiClass::AL));
        if !has_rtl {
            return;
        }
        let info = BidiInfo::new(text, None);
        let levels: Vec<u8> = text
            .char_indices()
            .map(|(byte, _)| info.levels[byte].number())
            .collect();
        let base = levels.first().copied().unwrap_or(0);
        for row in self.rows.iter_mut().take(self.text_rows) {
            let start = row.glyphs.iter().map(|glyph| glyph.x).min().unwrap_or(0);
            let row_levels: Vec<u8> = row
                .glyphs
                .iter()
                .map(|glyph| levels.get(glyph.column).copied().unwrap_or(base))
                .collect();
            let order = visual_order(&row_levels);
            let mut x = start;
            for index in order {
                row.glyphs[index].x = x;
                x += row.glyphs[index].width;
            }
        }
    }

    /// Starts a row before a piece of the line needing `piece` cells when
    /// it does not fit the rest of the row but would fit a row of its own.
    fn break_before(&mut self, piece: usize) {
        let Some(wrap_width) = self.wrap_width else {
            return;
        };
        let row = self.rows.last().expect("a layout has a row");
        let fits_alone = self.indent + piece <= wrap_width;
        if !row.glyphs.is_empty() && row.width + piece > wrap_width && fits_alone {
            self.start_row();
        }
    }

    /// Places `cluster`, of char offset `column` (or before it, for a
    /// cluster of inlay `inlay`), after the clusters placed so far,
    /// starting a row when it does not fit a wrapped one. A tab that does
    /// not fit fills the row instead, as does a blank wrapping at words,
    /// so a row never starts with the blank it broke at.
    fn place(&mut self, cluster: &'a str, column: usize, inlay: Option<usize>, tab_width: usize) {
        let row = self.rows.last_mut().expect("a layout has a row");
        let mut width = glyph_width(cluster, row.width, tab_width);
        if let Some(wrap_width) = self.wrap_width {
            let overflows = row.width + width > wrap_width && !row.glyphs.is_empty();
            let fills = cluster == TAB || (self.mode == WrapMode::Words && is_blank(cluster));
            if overflows && fills && row.width < wrap_width {
                width = wrap_width - row.width;
            } else if overflows {
                width = glyph_width(cluster, self.indent, tab_width);
                self.start_row();
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

    /// Starts a new row of the text, at the break indent.
    fn start_row(&mut self) {
        self.rows.push(Row {
            glyphs: Vec::new(),
            virtual_line: None,
            width: self.indent,
        });
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
    /// Whether the glyph takes cell `x` of its row.
    pub fn covers(&self, x: usize) -> bool {
        (self.x..self.x + self.width).contains(&x)
    }

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

/// The chars of the blanks and tabs at the start of `text`.
fn indent_len(text: &str) -> usize {
    text.len() - text.trim_start_matches([' ', '\t']).len()
}

/// The chars of what opens `text`, a line, each part with the blanks
/// after it: the indent, any quote `>`s and line comment leaders or a
/// heading's `#`s, then a list item's marker and its task box, or LaTeX's
/// `\item`. Every part is ASCII, so the chars are its bytes.
fn opener_len(text: &str) -> usize {
    let mut at = indent_len(text);
    while let Some(leader) = leader_len(&text[at..]) {
        at += leader;
        at += indent_len(&text[at..]);
    }
    let rest = &text[at..];
    if let Some((marker, item)) = list_marker(rest) {
        at += marker.len();
        at += indent_len(&text[at..]);
        let task = item.len() > TASK_BOX.len()
            && item.as_bytes()[TASK_BOX.len()] == b' '
            && TASK_BOX_MARKS.iter().any(|mark| item.starts_with(mark));
        if task {
            at += TASK_BOX.len();
            at += indent_len(&text[at..]);
        }
    } else if rest.starts_with(LATEX_ITEM) && rest[LATEX_ITEM.len()..].starts_with([' ', '\t']) {
        at += LATEX_ITEM.len();
        at += indent_len(&text[at..]);
    }
    at
}

/// The bytes of the quote `>` or the line comment leader or heading `#`s
/// `text` starts with: a `>`, or a run of `#`, `;`, or `%`, a `//` and any
/// `/` or `!` after it, or `--`, each followed by a blank.
fn leader_len(text: &str) -> Option<usize> {
    let bytes = text.as_bytes();
    let first = *bytes.first()?;
    if first == b'>' {
        return Some(1);
    }
    let len = match first {
        b'#' | b';' | b'%' => bytes.iter().take_while(|&&byte| byte == first).count(),
        b'/' if bytes.get(1) == Some(&b'/') => {
            2 + bytes[2..]
                .iter()
                .take_while(|&&byte| matches!(byte, b'/' | b'!'))
                .count()
        }
        b'-' if bytes.get(1) == Some(&b'-') => 2,
        _ => return None,
    };
    matches!(bytes.get(len), Some(b' ' | b'\t')).then_some(len)
}

/// The cells the first `chars` chars of `text` take, starting a row, the
/// clusters in the `hidden` char ranges left out.
fn shown_cells(text: &str, chars: usize, hidden: &[Range<usize>], tab_width: usize) -> usize {
    let mut column = 0;
    let mut x = 0;
    for cluster in text.graphemes(true) {
        if column >= chars {
            break;
        }
        if !hidden.iter().any(|range| range.contains(&column)) {
            x += glyph_width(cluster, x, tab_width);
        }
        column += cluster.chars().count();
    }
    x
}

/// The cells the piece of a line made of the `clusters` before byte `end`
/// needs, the first at char `column`: its text, the clusters in the
/// `hidden` char ranges left out, and one cell for the blanks it ends
/// with, so the blank a row breaks at stays inside the row. Tabs end a
/// piece, so each cluster counts its own width.
fn piece_cells(
    clusters: &[(usize, &str)],
    column: usize,
    end: usize,
    hidden: &[Range<usize>],
) -> usize {
    let mut column = column;
    let mut width = 0;
    let mut shown = 0;
    for &(_, cluster) in clusters.iter().take_while(|&&(byte, _)| byte < end) {
        if !hidden.iter().any(|range| range.contains(&column)) {
            width += cluster.width().clamp(1, MAX_GLYPH_WIDTH);
            if !is_blank(cluster) {
                shown = width;
            }
        }
        column += cluster.chars().count();
    }
    shown + usize::from(width > shown)
}

/// Whether `cluster` is white space a row may end with: a blank or a tab.
fn is_blank(cluster: &str) -> bool {
    cluster.chars().all(char::is_whitespace)
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

/// The visual order of glyphs whose embedding levels are `levels`, as
/// indexes: rule L2 of the Unicode Bidirectional Algorithm, reversing each
/// run at or above every level from the highest down to the lowest odd one.
fn visual_order(levels: &[u8]) -> Vec<usize> {
    let mut order: Vec<usize> = (0..levels.len()).collect();
    let highest = levels.iter().copied().max().unwrap_or(0);
    let lowest_odd = levels
        .iter()
        .copied()
        .filter(|level| level % 2 == 1)
        .min()
        .unwrap_or(highest + 1);
    for level in (lowest_odd..=highest).rev() {
        let mut index = 0;
        while index < order.len() {
            if levels[order[index]] < level {
                index += 1;
                continue;
            }
            let start = index;
            while index < order.len() && levels[order[index]] >= level {
                index += 1;
            }
            order[start..index].reverse();
        }
    }
    order
}

// ========================================================================
// Tests
// ========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// Wrapping into rows of `width` cells at chars.
    fn chars(width: usize) -> Option<Wrap> {
        wrapping(BreakIndent::None, WrapMode::Chars, width)
    }

    /// Wrapping into rows of `width` cells at `mode`, with `break_indent`.
    fn wrapping(break_indent: BreakIndent, mode: WrapMode, width: usize) -> Option<Wrap> {
        Some(Wrap {
            break_indent,
            mode,
            width,
        })
    }

    /// Each text row of `layout` as the text it shows.
    fn texts(layout: &LineLayout<'_>) -> Vec<String> {
        layout
            .rows()
            .iter()
            .map(|row| row.glyphs.iter().map(|glyph| glyph.text).collect())
            .collect()
    }

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
        let layout = LineLayout::decorated("a\tb", &inlays, &[], None, 4);
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
        let layout = LineLayout::decorated("abc", &inlays, &[], chars(4), 4);
        assert_eq!(
            cells(&layout),
            vec![vec![(0, "a"), (1, "b"), (2, "c"), (3, "X")], vec![(0, "Y")]]
        );
        assert_eq!(layout.locate(3), RowCell { row: 1, x: 1 });
    }

    #[test]
    fn test_hidden_clusters_take_no_cells_and_their_offsets_show_on_the_next_one() {
        let hidden = [0..2, 3..5];
        let layout = LineLayout::decorated("**b**\tc", &[], &hidden, None, 4);
        assert_eq!(cells(&layout), vec![vec![(0, "b"), (1, "\t"), (4, "c")]]);
        assert_eq!(layout.locate(0), RowCell { row: 0, x: 0 }, "on the b");
        assert_eq!(layout.locate(3), RowCell { row: 0, x: 1 }, "on the tab");
        assert_eq!(layout.column_at(0), 2);
        assert_eq!(layout.column_at(99), 7, "past the end is the line end");
    }

    #[test]
    fn test_break_indent_starts_wrapped_rows_at_the_indent_leaving_room_for_text() {
        let text = format!("\t  {}", "x".repeat(60));
        let indent = |width| wrapping(BreakIndent::Indent, WrapMode::Chars, width);
        let layout = LineLayout::decorated(&text, &[], &[], indent(30), 4);
        let starts: Vec<usize> = layout.rows().iter().map(|row| row.glyphs[0].x).collect();
        assert_eq!(starts, [0, 6, 6], "the tab and two blanks");
        assert!(layout.rows().iter().all(|row| row.width <= 30));
        assert_eq!(
            layout.locate(30),
            RowCell { row: 1, x: 9 },
            "char 27 starts row 1"
        );
        let narrow = LineLayout::decorated(&text, &[], &[], indent(24), 4);
        assert_eq!(narrow.rows()[1].glyphs[0].x, 4, "20 cells kept for text");
        let off = LineLayout::decorated(&text, &[], &[], chars(30), 4);
        assert_eq!(off.rows()[1].glyphs[0].x, 0);
    }

    #[test]
    fn test_wrapping_at_words_moves_a_word_that_does_not_fit_down_whole() {
        let words = wrapping(BreakIndent::None, WrapMode::Words, 8);
        let layout = LineLayout::decorated("one two three", &[], &[], words, 4);
        assert_eq!(texts(&layout), ["one two ", "three"]);
        let layout = LineLayout::decorated("abcdefghij ok", &[], &[], words, 4);
        assert_eq!(texts(&layout), ["abcdefgh", "ij ok"], "too long for a row");
        let layout = LineLayout::decorated("one two three", &[], &[], chars(8), 4);
        assert_eq!(texts(&layout), ["one two ", "three"]);
        let layout = LineLayout::decorated("one twothree", &[], &[], chars(8), 4);
        assert_eq!(texts(&layout), ["one twot", "hree"], "chars break mid-word");
        let layout = LineLayout::decorated("one twothree", &[], &[], words, 4);
        assert_eq!(texts(&layout), ["one ", "twothree"]);
    }

    #[test]
    fn test_a_blank_a_word_wrap_breaks_at_ends_its_row_and_never_starts_one() {
        let words = wrapping(BreakIndent::None, WrapMode::Words, 7);
        let layout = LineLayout::decorated("abc def ghi", &[], &[], words, 4);
        assert_eq!(texts(&layout), ["abc ", "def ghi"], "room for def's blank");
        assert!(layout.rows().iter().all(|row| row.width <= 7));
    }

    #[test]
    fn test_wrapping_at_words_breaks_between_cjk_characters_and_skips_hidden_text() {
        let words = wrapping(BreakIndent::None, WrapMode::Words, 5);
        let layout = LineLayout::decorated("ab日本語", &[], &[], words, 4);
        assert_eq!(texts(&layout), ["ab日", "本語"]);
        let hidden = [4..6, 9..11];
        let words = wrapping(BreakIndent::None, WrapMode::Words, 8);
        let layout = LineLayout::decorated("one **two** x", &[], &hidden, words, 4);
        assert_eq!(texts(&layout), ["one two ", "x"], "the stars take no room");
    }

    #[test]
    fn test_text_break_indent_hangs_rows_under_a_list_item_s_text() {
        let text = |line: &str| {
            let wrap = wrapping(BreakIndent::Text, WrapMode::Words, 30);
            let long = format!("{line}{}", " word".repeat(12));
            LineLayout::decorated(&long, &[], &[], wrap, 4).rows()[1].glyphs[0].x
        };
        assert_eq!(text("- item"), 2);
        assert_eq!(text("  - [ ] task"), 8, "past the task box");
        assert_eq!(text("- [x] done"), 6);
        assert_eq!(text("12. item"), 4);
        assert_eq!(text("> - quoted"), 4, "a quote and its list");
        assert_eq!(text("## Heading"), 3);
        assert_eq!(text("    // comment"), 7);
        assert_eq!(text("\\item one"), 6);
        assert_eq!(text("  plain"), 2, "else the indent");
        assert_eq!(text("-dash"), 0, "a marker needs its blank");
        assert_eq!(text("#tag"), 0);
    }

    #[test]
    fn test_a_viewport_narrower_than_a_cluster_still_lays_out_every_cluster() {
        let layout = LineLayout::new("日本", Some(1), 4);
        assert_eq!(cells(&layout), vec![vec![(0, "日")], vec![(0, "本")]]);
        let zero = LineLayout::new("ab", Some(0), 4);
        assert_eq!(zero.rows().len(), 2);
    }

    #[test]
    fn test_visual_order_reverses_each_run_from_the_highest_level_down() {
        assert_eq!(visual_order(&[0, 0, 1, 1, 1]), [0, 1, 4, 3, 2]);
        assert_eq!(visual_order(&[1, 1, 2, 2, 1]), [4, 2, 3, 1, 0]);
        assert_eq!(visual_order(&[0, 0]), [0, 1]);
    }

    #[test]
    fn test_right_to_left_text_shows_in_visual_order_with_logical_offsets() {
        let layout = LineLayout::new("ab \u{5d0}\u{5d1}\u{5d2}", None, 4);
        let shown: Vec<(usize, usize)> = layout.rows()[0]
            .glyphs
            .iter()
            .map(|glyph| (glyph.column, glyph.x))
            .collect();
        assert_eq!(shown, [(0, 0), (1, 1), (2, 2), (3, 5), (4, 4), (5, 3)]);
        assert_eq!(layout.locate(3).x, 5, "the cursor shows on its letter");
        assert_eq!(layout.column_at(3), 5, "the cell maps back to its letter");
    }
}
