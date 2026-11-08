//! Rendering: paints a view of a document onto a [`Surface`] and says where
//! the cursor goes, drawn as a block in Normal mode and a bar in Insert
//! mode; and paints a view's prompt as one row.
//!
//! The view is painted as it is scrolled; rendering never scrolls, as the
//! editor already scrolled the view to its cursor after every key. Each screen row
//! shows one row of a line: the line number on its first row when line
//! numbers are on, then the clusters of the row, tabs as blank cells. A
//! cluster cut by the left or right edge shows as blank cells. Rows past the
//! end of the document stay blank. Matches of the last search pattern on
//! the lines in sight paint as [`Style::SearchMatch`], under a selection.
//! Host decorations paint as the [`decoration`](super::decoration) module
//! describes: highlights as [`Style::Highlight`] under both, other kinds as
//! the cell's underline; the sign column, when on, shows each line's sign.
//! The spans of the document's [`Highlighter`], if it has one, paint as
//! each cell's highlight, whatever its style.
//!
//! The prompt row shows the prompt's prefix (`/`, `?`) and line, scrolled
//! sideways just enough to show its cursor, a bar.

use std::ops::Range;

use regex::Regex;
use unicode_width::UnicodeWidthChar;

use super::decoration::{Decoration, DecorationKind, Decorations};
use super::document::{Document, Position, is_word_char, line_count, line_of, start_of_line};
use super::editor::{Editor, ViewId};
use super::highlight::{HighlightId, HighlightSpan, Highlighter};
use super::operator::Operand;
use super::options::EditorOptions;
use super::paint::{Cursor, CursorShape, Layers, Style, Surface};
use super::prompt::PromptKind;
use super::search::{LastSearch, line_matches};
use super::text_motion::bracket_match_within;
use super::text_object::{Reach, word_object};
use super::view::{Mode, ScrollOffset, TextArea, View};
use super::visual::selected_ranges;
use super::wrap::{Glyph, LineLayout, TAB};

// ========================================================================
// Constants
// ========================================================================

/// The lines either side of a blank line its indentation guides are read
/// from: enough to see past a long block, few enough to stay cheap.
const GUIDE_REACH: usize = 100;

/// The lines above a line its block headers are looked for in.
const HEADER_REACH: usize = 1_000;

/// The shortest word whose occurrences are lit: a lone letter is
/// everywhere, and lighting it says nothing.
const MIN_LIT_CHARS: usize = 2;

/// The symbol of a blank cell: padding, a tab, or a cut cluster.
const BLANK: &str = " ";

/// The symbol of the second cell of a wide character, skipped by hosts.
const CONTINUATION: &str = "";

// ========================================================================
// Data Structures
// ========================================================================

/// The sign a gutter lane shows on one line, as its text and kind, if any.
type LineSign<'a> = Option<(&'a str, DecorationKind)>;

/// What is painted over a view's text besides its selections.
#[derive(Clone, Copy, Debug, Default)]
struct Overlays<'a> {
    /// The host's decorations of the document.
    decorations: Option<&'a Decorations>,
    /// The highlighter of the document.
    highlighter: Option<&'a dyn Highlighter>,
    /// The phrases lit by hand, with their color numbers.
    phrases: &'a [(String, u8)],
    /// The pattern whose matches are marked.
    search: Option<&'a Regex>,
}

/// What a view lights over its text in sight, gathered once per paint
/// for every row to read.
struct Lit<'a> {
    /// The bracket at the cursor and the one matching it, as offsets.
    brackets: Option<(usize, usize)>,
    /// The decorations touching the chars in sight.
    decorated: Vec<&'a Decoration>,
    /// The char ranges matching the search pattern, as `(start, end)`.
    matched: Vec<(usize, usize)>,
    /// The char ranges of the lit occurrences of the word at the cursor,
    /// as `(start, end)`.
    occurrences: Vec<(usize, usize)>,
    /// The char ranges of the phrases lit by hand, as `(start, end)`,
    /// with their color numbers.
    phrases: Vec<(usize, usize, u8)>,
    /// The selected char ranges, as `(start, end)`.
    selected: Vec<(usize, usize)>,
    /// Each gutter lane, as `(start, width)` in cells, with the sign of
    /// each line in sight.
    signs: Vec<((usize, usize), Vec<LineSign<'a>>)>,
    /// The highlighter's spans over the chars in sight.
    spans: Vec<HighlightSpan>,
}

/// One screen row being painted: where it is and how its text is styled.
#[derive(Clone, Copy, Debug)]
struct Painter<'a> {
    /// The text area the row belongs to.
    area: TextArea<'a>,
    /// The style of the row's unselected cells.
    base: Style,
    /// The bracket at the cursor and the one matching it, as offsets.
    brackets: Option<(usize, usize)>,
    /// The decorations touching the lines in sight.
    decorated: &'a [&'a Decoration],
    /// The highlight of each char of the row's line from `highlights_from`.
    highlights: &'a [Option<HighlightId>],
    /// The char column of the line the first of `highlights` is for.
    highlights_from: usize,
    /// The char offset where the row's line starts.
    line_start: usize,
    /// The char ranges of the lit occurrences of the word at the cursor,
    /// as `(start, end)`.
    lit: &'a [(usize, usize)],
    /// The char ranges of the phrases lit by hand, as `(start, end)`, with
    /// their color numbers.
    phrases: &'a [(usize, usize, u8)],
    /// The char ranges matching the search pattern, as `(start, end)`.
    matched: &'a [(usize, usize)],
    /// The cell column of the text at the left edge.
    scroll_column: usize,
    /// The selected char ranges, as `(start, end)` offsets.
    selected: &'a [(usize, usize)],
    /// The surface row painted.
    y: usize,
}

// ========================================================================
// Lit Implementations
// ========================================================================

impl<'a> Lit<'a> {
    /// Gathers what `view` lights over the lines of `document` in sight,
    /// laid out in `area`.
    fn gather(
        document: &Document,
        view: &View,
        options: &EditorOptions,
        overlays: &Overlays<'a>,
        area: &TextArea<'_>,
    ) -> Self {
        let scroll = view.scroll();
        let rows = usize::from(view.rows());
        let shown_lines = scroll.line..scroll.line.saturating_add(rows);
        let visual = selected_ranges(document, options, view, shown_lines)
            .into_iter()
            .map(|range| (range.start, range.end));
        let selected: Vec<_> = view
            .selection()
            .ranges()
            .iter()
            .filter(|range| !range.is_empty())
            .map(|range| (range.start(), range.end()))
            .chain(visual)
            .collect();
        let matched = overlays.search.map_or_else(Vec::new, |regex| {
            visible_matches(document, regex, scroll.line, rows)
        });
        let phrases: Vec<(usize, usize, u8)> = overlays
            .phrases
            .iter()
            .filter_map(|(phrase, color)| {
                let regex = Regex::new(&regex::escape(phrase)).ok()?;
                let found = visible_matches(document, &regex, scroll.line, rows);
                Some(
                    found
                        .into_iter()
                        .map(move |(start, end)| (start, end, *color)),
                )
            })
            .flatten()
            .collect();
        let occurrences = match options.light_occurrences {
            true => lit_occurrences(document, view.cursor(), scroll.line, rows),
            false => Vec::new(),
        };
        let last_line = scroll.line.saturating_add(rows).min(line_count(document));
        let visible_chars = visible_chars(document, view);
        let spans = overlays.highlighter.map_or_else(Vec::new, |highlighter| {
            highlighter.highlights(document, visible_chars.clone())
        });
        let brackets = options
            .match_brackets
            .then(|| bracket_at_cursor(document, view, visible_chars.clone()))
            .flatten();
        let decorated = overlays
            .decorations
            .map_or_else(Vec::new, |decorations| decorations.touching(visible_chars));
        let signs: Vec<_> = area
            .lanes()
            .iter()
            .map(|(name, start, width)| {
                let shown = overlays.decorations.map_or_else(Vec::new, |decorations| {
                    decorations.signs_on_lines(document, scroll.line..last_line, *name)
                });
                ((*start, *width), shown)
            })
            .collect();
        Self {
            brackets,
            decorated,
            matched,
            occurrences,
            phrases,
            selected,
            signs,
            spans,
        }
    }
}

// ========================================================================
// Painter Implementations
// ========================================================================

impl Painter<'_> {
    /// Paints `glyph` onto `surface`, blank where an edge cuts it.
    fn paint_glyph(&self, surface: &mut Surface, glyph: &Glyph<'_>) {
        if glyph.inlay.is_some_and(|index| self.area.is_preedit(index)) {
            self.paint_cells(surface, glyph, Style::Preedit, None, None);
            return;
        }
        if let Some(index) = glyph.inlay {
            let highlight = self
                .area
                .decorations
                .inlays_by_offset()
                .get(index)
                .and_then(|inlay| inlay.highlight);
            self.paint_cells(surface, glyph, Style::Inlay, highlight, None);
            return;
        }
        let offset = self.line_start + glyph.column;
        let covers = |ranges: &[(usize, usize)]| {
            ranges
                .iter()
                .any(|(start, end)| (*start..*end).contains(&offset))
        };
        let decorations = self
            .decorated
            .iter()
            .filter(|decoration| decoration.covers(offset));
        let mut highlighted = false;
        let mut underline: Option<DecorationKind> = None;
        for decoration in decorations {
            match decoration.kind {
                DecorationKind::Background(_) => {}
                DecorationKind::Highlight => highlighted = true,
                DecorationKind::Error
                | DecorationKind::Hint
                | DecorationKind::Info
                | DecorationKind::Underline
                | DecorationKind::Warning => {
                    let beats =
                        underline.is_none_or(|shown| decoration.kind.severity() > shown.severity());
                    if beats {
                        underline = Some(decoration.kind);
                    }
                }
            }
        }
        let style = if covers(self.selected) {
            Style::Selection
        } else if covers(self.matched) {
            Style::SearchMatch
        } else if self
            .brackets
            .is_some_and(|(at, other)| offset == at || offset == other)
        {
            Style::MatchingBracket
        } else if highlighted {
            Style::Highlight
        } else if let Some((_, _, color)) = self
            .phrases
            .iter()
            .find(|(start, end, _)| (*start..*end).contains(&offset))
        {
            Style::Phrase(*color)
        } else {
            self.background_at(offset)
        };
        let highlight = glyph
            .column
            .checked_sub(self.highlights_from)
            .and_then(|index| self.highlights.get(index).copied())
            .flatten();
        self.paint_cells(surface, glyph, style, highlight, underline);
        if covers(self.lit) {
            self.add_layers(surface, glyph, Layers::OCCURRENCE);
        }
    }

    /// Paints `text`, a virtual line, along the row from the text's left
    /// edge, colored as `highlight`, cut at the view's edges.
    fn paint_virtual_line(
        &self,
        surface: &mut Surface,
        text: &str,
        highlight: Option<HighlightId>,
    ) {
        let visible = self.scroll_column..self.scroll_column + self.area.width;
        let mut x = 0;
        for c in text.chars() {
            let width = c.width().unwrap_or(0).max(1);
            if visible.contains(&x) && x + width <= visible.end {
                let column = self.area.gutter_width + x - self.scroll_column;
                surface.put_layers(
                    column,
                    self.y,
                    &c.to_string(),
                    Style::Inlay,
                    highlight,
                    None,
                );
                for continuation in 1..width {
                    surface.put(column + continuation, self.y, CONTINUATION, Style::Inlay);
                }
            }
            x += width;
        }
    }

    /// Adds `layers` to the cells of `glyph` in sight.
    fn add_layers(&self, surface: &mut Surface, glyph: &Glyph<'_>, layers: Layers) {
        let visible = self.scroll_column..self.scroll_column + self.area.width;
        for x in (glyph.x..glyph.x + glyph.width).filter(|x| visible.contains(x)) {
            let column = self.area.gutter_width + x - self.scroll_column;
            surface.add_layers(column, self.y, layers);
        }
    }

    /// Marks the blank cells in sight at the cell columns `guides` of the
    /// row's text as indentation guides.
    fn paint_guides(&self, surface: &mut Surface, guides: &[usize]) {
        let visible = self.scroll_column..self.scroll_column + self.area.width;
        for x in guides.iter().filter(|x| visible.contains(x)) {
            let column = self.area.gutter_width + x - self.scroll_column;
            let blank = u16::try_from(column)
                .ok()
                .zip(u16::try_from(self.y).ok())
                .and_then(|(column, row)| surface.cell(column, row))
                .is_some_and(|cell| cell.symbol == BLANK);
            if blank {
                surface.add_layers(column, self.y, Layers::INDENT_GUIDE);
            }
        }
    }

    /// The style of an unselected, unmatched, unhighlighted cell showing
    /// char `offset`: the narrowest host background covering it, else the
    /// row's base style.
    fn background_at(&self, offset: usize) -> Style {
        self.decorated
            .iter()
            .filter(|decoration| decoration.covers(offset))
            .filter_map(|decoration| match decoration.kind {
                DecorationKind::Background(id) => Some((decoration.range.len(), id)),
                DecorationKind::Error
                | DecorationKind::Highlight
                | DecorationKind::Hint
                | DecorationKind::Info
                | DecorationKind::Underline
                | DecorationKind::Warning => None,
            })
            .min_by_key(|(len, _)| *len)
            .map_or(self.base, |(_, id)| Style::Background(id))
    }

    /// Paints the cells of `glyph` in sight onto `surface` with these
    /// layers, blank where an edge cuts it.
    fn paint_cells(
        &self,
        surface: &mut Surface,
        glyph: &Glyph<'_>,
        style: Style,
        highlight: Option<HighlightId>,
        underline: Option<DecorationKind>,
    ) {
        let visible = self.scroll_column..self.scroll_column + self.area.width;
        let fits = visible.contains(&glyph.x) && glyph.x + glyph.width <= visible.end;
        for cell in 0..glyph.width {
            let x = glyph.x + cell;
            if !visible.contains(&x) {
                continue;
            }
            let symbol = match (fits && glyph.text != TAB, cell) {
                (true, 0) => glyph.text,
                (true, _) => CONTINUATION,
                (false, _) => BLANK,
            };
            let column = self.area.gutter_width + x - self.scroll_column;
            surface.put_layers(column, self.y, symbol, style, highlight, underline);
        }
    }

    /// Paints the line number `number`, right aligned in the gutter right
    /// of the sign column, in `style`.
    fn paint_line_number(&self, surface: &mut Surface, number: Option<usize>, style: Style) {
        let width = self.area.gutter_width - self.area.sign_width;
        let digits = width.saturating_sub(1);
        let label = number.map_or_else(String::new, |number| number.to_string());
        let text = format!("{label:>digits$}{BLANK}");
        for (column, symbol) in text.chars().enumerate() {
            let column = self.area.sign_width + column;
            surface.put(column, self.y, &symbol.to_string(), style);
        }
    }

    /// Paints `sign` in the lane of `lane_width` cells from cell `start`,
    /// cut to its width, blank cells as [`Style::LineNumber`].
    fn paint_sign(
        &self,
        surface: &mut Surface,
        (start, lane_width): (usize, usize),
        sign: Option<(&str, DecorationKind)>,
    ) {
        for column in start..start + lane_width {
            surface.put(column, self.y, BLANK, Style::LineNumber);
        }
        let Some((text, kind)) = sign else {
            return;
        };
        let mut column = 0;
        for c in text.chars() {
            let width = c.width().unwrap_or(0).max(1);
            if column + width > lane_width {
                break;
            }
            surface.put(start + column, self.y, &c.to_string(), Style::Sign(kind));
            for continuation in 1..width {
                surface.put(
                    start + column + continuation,
                    self.y,
                    CONTINUATION,
                    Style::Sign(kind),
                );
            }
            column += width;
        }
    }
}

// ========================================================================
// Functions
// ========================================================================

/// Paints `view` of `editor` onto `surface`, first sized to the view, and
/// returns where to draw the cursor; `None` when the cursor is out of sight
/// or the editor has no such view.
pub fn render_view(editor: &Editor, view: ViewId, surface: &mut Surface) -> Option<Cursor> {
    let id = view;
    let view = editor.view(id)?;
    let document = editor.document(view.document())?;
    let shape = match view.mode() {
        Mode::Insert => CursorShape::Bar,
        Mode::Replace => CursorShape::Underline,
        Mode::CommandLine
        | Mode::Normal
        | Mode::OperatorPending
        | Mode::Visual
        | Mode::VisualBlock
        | Mode::VisualLine => CursorShape::Block,
    };
    let overlays = Overlays {
        decorations: Some(editor.decorations_of(view.document())),
        highlighter: editor.highlighter(view.document()),
        phrases: editor.phrases_of(view.document()).lit(),
        search: typed_pattern(view).or_else(|| editor.highlighted_search().map(LastSearch::regex)),
    };
    let options = editor.layout_options(id);
    paint_view(document, view, &options, shape, overlays, surface)
}

/// The chars of `document` in sight in `view`: from the first line shown
/// to the end of the last.
pub(crate) fn visible_chars(document: &Document, view: &View) -> Range<usize> {
    let scroll = view.scroll();
    let rows = usize::from(view.rows());
    let last_line = scroll.line.saturating_add(rows).min(line_count(document));
    start_of_line(document, scroll.line)..start_of_line(document, last_line) + 1
}

/// The pattern being typed in a search prompt of `view` that incremental
/// search follows, if any.
fn typed_pattern(view: &View) -> Option<&Regex> {
    let prompt = view.prompt()?;
    match prompt.kind() {
        PromptKind::Command => None,
        PromptKind::SearchBackward | PromptKind::SearchForward => prompt.follow().regex.as_ref(),
    }
}

/// Paints the prompt of `view` onto `surface`, first sized to one row as
/// wide as the view, and returns where to draw its cursor; blank, and
/// `None`, when the view has no prompt open or the editor has no such
/// view.
pub fn render_prompt(editor: &Editor, view: ViewId, surface: &mut Surface) -> Option<Cursor> {
    let view = editor.view(view)?;
    if (surface.cols(), surface.rows()) == (view.cols(), 1) {
        surface.clear();
    } else {
        surface.resize(view.cols(), 1);
    }
    let prompt = view.prompt()?;
    let text = format!("{}{}", prompt.kind().prefix(), prompt.text());
    let layout = LineLayout::new(&text, None, editor.options().tab_stop());
    let cursor_x = layout.locate(prompt.cursor() + 1).x;
    let width = usize::from(view.cols()).max(1);
    let scroll = (cursor_x + 1).saturating_sub(width);
    let visible = scroll..scroll + width;
    for glyph in layout.rows().iter().flat_map(|row| &row.glyphs) {
        let fits = visible.contains(&glyph.x) && glyph.x + glyph.width <= visible.end;
        for cell in 0..glyph.width {
            let x = glyph.x + cell;
            if !visible.contains(&x) {
                continue;
            }
            let symbol = match (fits && glyph.text != TAB, cell) {
                (true, 0) => glyph.text,
                (true, _) => CONTINUATION,
                (false, _) => BLANK,
            };
            surface.put(x - scroll, 0, symbol, Style::Text);
        }
    }
    Some(Cursor {
        column: u16::try_from(cursor_x - scroll).ok()?,
        row: 0,
        shape: CursorShape::Bar,
    })
}

/// Paints `view` of `document`, laid out with `options`, onto `surface`,
/// first sized to the view, with `overlays` painted over the text, and
/// returns where to draw the cursor in `shape`; `None` when the cursor is
/// scrolled out of sight.
fn paint_view(
    document: &Document,
    view: &View,
    options: &EditorOptions,
    shape: CursorShape,
    overlays: Overlays<'_>,
    surface: &mut Surface,
) -> Option<Cursor> {
    if (surface.cols(), surface.rows()) == (view.cols(), view.rows()) {
        surface.clear();
    } else {
        surface.resize(view.cols(), view.rows());
    }
    let undecorated = Decorations::default();
    let decorations = overlays.decorations.unwrap_or(&undecorated);
    let area = view.area(document, decorations, options);
    let (cursor_line, cursor_cell) = area.locate(view.cursor());
    let extra_cells: Vec<_> = view
        .cursors()
        .iter()
        .map(|offset| area.locate(*offset))
        .collect();
    let scroll = view.scroll();
    let mut guides = (usize::MAX, Vec::new());
    let mut line_highlights = (usize::MAX, Vec::new());
    let lit = Lit::gather(document, view, options, &overlays, &area);
    let (mut line, mut row) = (scroll.line, scroll.row);
    let pinned = pinned_lines(document, &area, scroll, options.sticky_scroll);
    let painted_lines = painted_lines(document, view);
    let mut cursor = None;
    for y in 0..usize::from(view.rows()) {
        if line >= painted_lines {
            break;
        }
        let (scrolled_line, scrolled_row) = (line, row);
        if let Some(pin) = pinned.get(y) {
            (line, row) = (*pin, 0);
        }
        let long = area.is_long_line(line);
        let (text, from_column, from_x) = if long {
            area.window(line, scroll.column, scroll.column + area.width)
        } else {
            (document.line_text(line).unwrap_or_default(), 0, 0)
        };
        let layout = if long {
            LineLayout::window(&text, from_column, from_x, options.tab_stop())
        } else {
            area.layout(line, &text)
        };
        if line_highlights.0 != line {
            let start = start_of_line(document, line) + from_column;
            line_highlights = (
                line,
                highlights_of_line(&lit.spans, start, text.chars().count()),
            );
        }
        let painter = Painter {
            area,
            base: if line == cursor_line && y >= pinned.len() {
                Style::CursorLine
            } else {
                Style::Text
            },
            brackets: lit.brackets,
            decorated: &lit.decorated,
            highlights: &line_highlights.1,
            highlights_from: from_column,
            line_start: start_of_line(document, line),
            lit: &lit.occurrences,
            matched: &lit.matched,
            phrases: &lit.phrases,
            scroll_column: scroll.column,
            selected: &lit.selected,
            y,
        };
        let line_end = document
            .position_to_char(Position {
                column: usize::MAX,
                line,
            })
            .unwrap_or_else(|| document.len_chars());
        let padding = painter.background_at(line_end);
        for x in area.gutter_width..usize::from(view.cols()) {
            surface.put(x, y, BLANK, padding);
        }
        for (lane, shown) in &lit.signs {
            let sign = line
                .checked_sub(scroll.line)
                .and_then(|index| shown.get(index))
                .copied()
                .flatten();
            painter.paint_sign(surface, *lane, sign.filter(|_| row == 0));
        }
        if options.line_numbers || options.relative_line_numbers {
            let number = (row == 0).then(|| line_number(options, line, cursor_line));
            let style = match line == cursor_line {
                true => Style::CursorLineNumber,
                false => Style::LineNumber,
            };
            painter.paint_line_number(surface, number, style);
        }
        if let Some(cells) = layout.rows().get(row) {
            for glyph in &cells.glyphs {
                painter.paint_glyph(surface, glyph);
            }
            if let Some(index) = cells.virtual_line {
                let virtual_lines = decorations.virtual_lines_after(document, line);
                if let Some((text, highlight)) = virtual_lines.get(index) {
                    painter.paint_virtual_line(surface, text, *highlight);
                }
            }
        }
        if options.indent_guides {
            if guides.0 != line {
                guides = (line, indent_guides(document, line, options));
            }
            painter.paint_guides(surface, &guides.1);
        }
        if y < pinned.len() {
            for x in 0..usize::from(view.cols()) {
                surface.add_layers(x, y, Layers::PINNED);
            }
            row = scrolled_row + 1;
            line = scrolled_line;
            if row >= area.row_count(line) {
                (line, row) = (line + 1, 0);
            }
            continue;
        }
        if (line, row) == (cursor_line, cursor_cell.row) {
            cursor = place_cursor(&area, cursor_cell.x, scroll.column, y, shape);
        }
        let extras_here = extra_cells
            .iter()
            .filter(|(at_line, cell)| (*at_line, cell.row) == (line, row));
        for (_, cell) in extras_here {
            if let Some(shown) = place_cursor(&area, cell.x, scroll.column, y, shape) {
                surface.restyle(usize::from(shown.column), y, Style::ExtraCursor);
            }
        }
        row += 1;
        if row >= layout.rows().len() {
            (line, row) = (line + 1, 0);
        }
    }
    cursor
}

/// The bracket at the cursor of `view` (in Insert and Replace mode, the
/// one before it when the cursor is not at one) and the bracket matching
/// it, both among the chars `visible`.
fn bracket_at_cursor(
    document: &Document,
    view: &View,
    visible: Range<usize>,
) -> Option<(usize, usize)> {
    let cursor = view.cursor();
    let typing = matches!(view.mode(), Mode::Insert | Mode::Replace);
    let before = cursor.checked_sub(1).filter(|_| typing);
    [Some(cursor), before]
        .into_iter()
        .flatten()
        .filter(|at| visible.contains(at))
        .find_map(|at| Some((at, bracket_match_within(document, at, visible.clone())?)))
}

/// The highlight of each of the `len` chars of the line starting at char
/// `start`, from `spans`, a later span winning where they overlap.
fn highlights_of_line(
    spans: &[HighlightSpan],
    start: usize,
    len: usize,
) -> Vec<Option<HighlightId>> {
    let mut highlights = vec![None; len];
    if spans.is_empty() {
        return highlights;
    }
    let end = start + len;
    for span in spans {
        let from = span.range.start.max(start);
        let to = span.range.end.min(end);
        for slot in highlights
            .iter_mut()
            .take(to.saturating_sub(start))
            .skip(from.saturating_sub(start))
        {
            *slot = Some(span.highlight);
        }
    }
    highlights
}

/// The cursor at cell `x` of its row, shown on surface row `y`, or `None`
/// when the view is scrolled sideways past it.
fn place_cursor(
    area: &TextArea<'_>,
    x: usize,
    scroll_column: usize,
    y: usize,
    shape: CursorShape,
) -> Option<Cursor> {
    if x < scroll_column || x - scroll_column >= area.width.max(1) {
        return None;
    }
    Some(Cursor {
        column: u16::try_from(area.gutter_width + x - scroll_column).ok()?,
        row: u16::try_from(y).ok()?,
        shape,
    })
}

/// The char ranges matching `regex` on the `count` lines from `first`, as
/// `(start, end)` offsets into `document`.
fn visible_matches(
    document: &Document,
    regex: &Regex,
    first: usize,
    count: usize,
) -> Vec<(usize, usize)> {
    let last = first.saturating_add(count).min(line_count(document));
    let mut matched = Vec::new();
    for line in first..last {
        let (Some(text), Some(start)) = (
            document.line_text(line),
            document.position_to_char(Position { column: 0, line }),
        ) else {
            continue;
        };
        let found = line_matches(regex, &text);
        matched.extend(found.into_iter().map(|(s, e)| (start + s, start + e)));
    }
    matched
}

/// The char ranges, as `(start, end)`, of the whole-word occurrences on the
/// `count` lines from line `first` of the word at `cursor`, when the
/// cursor is on a word at least [`MIN_LIT_CHARS`] long.
fn lit_occurrences(
    document: &Document,
    cursor: usize,
    first: usize,
    count: usize,
) -> Vec<(usize, usize)> {
    let Operand::Charwise(word) = word_object(document, cursor, 1, false, Reach::Inner) else {
        return Vec::new();
    };
    let text = document.slice(word);
    let is_word = text.chars().all(is_word_char);
    if !is_word || text.chars().count() < MIN_LIT_CHARS {
        return Vec::new();
    }
    let Ok(regex) = Regex::new(&format!(r"\b{}\b", regex::escape(&text))) else {
        return Vec::new();
    };
    visible_matches(document, &regex, first, count)
}

/// The cell columns of line `line`'s indentation guides: one at each
/// level, from the line's start, of the cells its leading blanks take, a
/// level being the indent width; for a blank line, those of the
/// shallower of the nearest lines with text above and below, within
/// [`GUIDE_REACH`] lines.
fn indent_guides(document: &Document, line: usize, options: &EditorOptions) -> Vec<usize> {
    let depth_of = |line: usize| {
        let text = document.line_text(line)?;
        if text.trim().is_empty() {
            return None;
        }
        let mut cells = 0;
        for c in text.chars().take_while(|c| *c == ' ' || *c == '\t') {
            cells += match c {
                '\t' => options.tab_stop() - cells % options.tab_stop(),
                _ => 1,
            };
        }
        Some(cells)
    };
    let depth = depth_of(line).unwrap_or_else(|| {
        let above = (line.saturating_sub(GUIDE_REACH)..line)
            .rev()
            .find_map(depth_of);
        let last = document.len_lines().min(line + 1 + GUIDE_REACH);
        let below = (line + 1..last).find_map(depth_of);
        above
            .zip(below)
            .map_or(0, |(above, below)| above.min(below))
    });
    (0..depth).step_by(options.indent_width()).collect()
}

/// How many lines of `document` `view` paints: the empty line after a final
/// line break only while the cursor is on it, as when it was just typed.
fn painted_lines(document: &Document, view: &View) -> usize {
    let count = line_count(document);
    match line_of(document, view.cursor()) >= count {
        true => document.len_lines(),
        false => count,
    }
}

/// The lines pinned at the top of a view of `area` scrolled to `scroll`,
/// at most `max`, outermost first, as
/// [`EditorOptions::sticky_scroll`] says: the lines opening the blocks
/// the first line in sight below them is inside, each above that line.
fn pinned_lines(
    document: &Document,
    area: &TextArea<'_>,
    scroll: ScrollOffset,
    max: usize,
) -> Vec<usize> {
    let line_at_row = |rows: usize| {
        let (mut line, mut row) = (scroll.line, scroll.row);
        for _ in 0..rows {
            row += 1;
            if row >= area.row_count(line) {
                (line, row) = (line + 1, 0);
            }
        }
        line
    };
    let mut pinned = Vec::new();
    for pins in 0..=max {
        let below = line_at_row(pins);
        if below >= document.len_lines() {
            break;
        }
        let openers: Vec<usize> = block_headers(document, below)
            .into_iter()
            .filter(|opener| *opener < scroll.line + pins)
            .take(max)
            .collect();
        if openers.len() <= pins {
            pinned = openers;
            break;
        }
        pinned = openers;
    }
    pinned
}

/// The lines opening the blocks line `line` is inside by indentation,
/// outermost first: going up, each line with text indented less than the
/// last one found, within [`HEADER_REACH`] lines. A blank line is inside
/// the blocks of the next line with text.
fn block_headers(document: &Document, line: usize) -> Vec<usize> {
    let indent = |line: usize| {
        let text = document.line_text(line)?;
        (!text.trim().is_empty())
            .then(|| text.chars().take_while(|c| *c == ' ' || *c == '\t').count())
    };
    let Some(mut depth) = (line..document.len_lines()).find_map(indent) else {
        return Vec::new();
    };
    let mut headers = Vec::new();
    for above in (line.saturating_sub(HEADER_REACH)..line).rev() {
        if depth == 0 {
            break;
        }
        if let Some(found) = indent(above).filter(|found| *found < depth) {
            headers.push(above);
            depth = found;
        }
    }
    headers.reverse();
    headers
}

/// The number the gutter shows by line `line` with the cursor on line
/// `cursor_line`, both from 0: the line's own number, or with relative
/// line numbers how many lines it is from the cursor's, the cursor's own
/// showing 0, or its own number when line numbers are on too.
fn line_number(options: &EditorOptions, line: usize, cursor_line: usize) -> usize {
    match (options.relative_line_numbers, line == cursor_line) {
        (true, true) if !options.line_numbers => 0,
        (true, false) => line.abs_diff(cursor_line),
        (true, true) | (false, _) => line + 1,
    }
}

// ========================================================================
// Tests
// ========================================================================

#[cfg(test)]
mod tests {

    use super::*;
    use crate::decoration::Inlay;
    use crate::decoration::{GutterLane, Sign, VirtualLine};
    use crate::editor::DocumentId;
    use crate::mouse::{MouseEvent, MouseEventKind};
    use crate::options::ConcealMode;
    use crate::selection::{Range, Selection};
    use crate::testing::Harness;

    fn view(cols: u16, rows: u16) -> View {
        View::new(DocumentId::default(), cols, rows, &EditorOptions::default())
    }

    /// Every row of `surface` as text.
    fn screen(surface: &Surface) -> Vec<String> {
        (0..surface.rows())
            .map(|row| surface.row_text(row))
            .collect()
    }

    fn style_at(surface: &Surface, column: u16, row: u16) -> Style {
        surface.cell(column, row).expect("in the surface").style
    }

    #[test]
    fn test_a_small_document_renders_with_line_numbers_and_the_cursor() {
        let document = Document::new("one\ntwo\n");
        let options = EditorOptions {
            line_numbers: true,
            ..EditorOptions::default()
        };
        let mut view = view(9, 4);
        view.set_selection(Selection::point(5));
        let mut surface = Surface::new(1, 1);
        let cursor = paint_view(
            &document,
            &view,
            &options,
            CursorShape::Block,
            Overlays::default(),
            &mut surface,
        );
        assert_eq!(
            screen(&surface),
            ["  1 one  ", "  2 two  ", "         ", "         "]
        );
        assert_eq!(
            cursor,
            Some(Cursor {
                column: 5,
                row: 1,
                shape: CursorShape::Block,
            })
        );
        assert_eq!(style_at(&surface, 0, 1), Style::CursorLineNumber);
        assert_eq!(style_at(&surface, 0, 0), Style::LineNumber);
        assert_eq!(style_at(&surface, 4, 1), Style::CursorLine);
        assert_eq!(style_at(&surface, 8, 1), Style::CursorLine, "padding too");
        assert_eq!(style_at(&surface, 4, 0), Style::Text);
        assert_eq!(style_at(&surface, 4, 3), Style::Text, "past the end");
    }

    #[test]
    fn test_selected_text_is_styled_as_a_selection() {
        let document = Document::new("abcdef");
        let mut view = view(6, 1);
        view.set_selection(Selection::single(Range::new(4, 1)));
        let mut surface = Surface::new(6, 1);
        paint_view(
            &document,
            &view,
            &EditorOptions::default(),
            CursorShape::Bar,
            Overlays::default(),
            &mut surface,
        );
        let styles: Vec<_> = (0..6).map(|x| style_at(&surface, x, 0)).collect();
        assert_eq!(
            styles,
            [
                Style::CursorLine,
                Style::Selection,
                Style::Selection,
                Style::Selection,
                Style::CursorLine,
                Style::CursorLine,
            ]
        );
    }

    #[test]
    fn test_wide_chars_take_two_cells_and_tabs_render_as_blanks() {
        let document = Document::new("日\tx");
        let options = EditorOptions {
            tab_width: 4,
            ..EditorOptions::default()
        };
        let mut view = view(6, 1);
        view.set_selection(Selection::point(2));
        let mut surface = Surface::new(6, 1);
        let cursor = paint_view(
            &document,
            &view,
            &options,
            CursorShape::Block,
            Overlays::default(),
            &mut surface,
        );
        assert_eq!(surface.row_text(0), "日  x ");
        assert_eq!(surface.cell(1, 0).unwrap().symbol, "", "the continuation");
        assert_eq!(cursor.map(|cursor| cursor.column), Some(4));
    }

    #[test]
    fn test_wrapped_lines_continue_on_the_next_row_without_a_line_number() {
        let document = Document::new("abcdefg\nh");
        let options = EditorOptions {
            line_numbers: true,
            wrap: true,
            ..EditorOptions::default()
        };
        let mut view = view(7, 4);
        view.set_selection(Selection::point(6));
        let mut surface = Surface::new(7, 4);
        let cursor = paint_view(
            &document,
            &view,
            &options,
            CursorShape::Block,
            Overlays::default(),
            &mut surface,
        );
        assert_eq!(
            screen(&surface),
            ["  1 abc", "    def", "    g  ", "  2 h  "]
        );
        assert_eq!(
            cursor.map(|cursor| (cursor.column, cursor.row)),
            Some((4, 2))
        );
    }

    #[test]
    fn test_a_wide_char_cut_by_the_left_edge_renders_blank() {
        let document = Document::new("a日bcdefgh");
        let options = EditorOptions::default();
        let mut view = view(3, 1);
        view.set_selection(Selection::point(3));
        view.scroll_to_cursor(&document, &Decorations::default(), &options);
        assert_eq!(view.scroll().column, 2, "the cursor is at cell 4");
        let mut surface = Surface::new(3, 1);
        paint_view(
            &document,
            &view,
            &options,
            CursorShape::Block,
            Overlays::default(),
            &mut surface,
        );
        assert_eq!(surface.row_text(0), " bc");
    }

    #[test]
    fn test_rendering_starts_inside_a_wrapped_line_scrolled_off_the_top() {
        let document = Document::new("abcdef\nx\ny");
        let options = EditorOptions {
            scroll_margin: 0,
            wrap: true,
            ..EditorOptions::default()
        };
        let mut view = view(3, 2);
        view.set_selection(Selection::point(7));
        view.scroll_to_cursor(&document, &Decorations::default(), &options);
        let mut surface = Surface::new(3, 2);
        let cursor = paint_view(
            &document,
            &view,
            &options,
            CursorShape::Block,
            Overlays::default(),
            &mut surface,
        );
        assert_eq!(screen(&surface), ["def", "x  "]);
        assert_eq!(cursor.map(|cursor| cursor.row), Some(1));
    }

    #[test]
    fn test_decorations_paint_as_highlights_under_selections_and_as_underlines() {
        let document = Document::new("abcdef");
        let mut decorations = Decorations::default();
        let marked = vec![
            Decoration::new(0..3, DecorationKind::Highlight),
            Decoration::new(1..5, DecorationKind::Warning),
            Decoration::new(2..3, DecorationKind::Error),
            Decoration::new(5..5, DecorationKind::Hint),
        ];
        decorations.set_decorations(&document, "lsp", marked);
        let mut view = view(7, 1);
        view.set_selection(Selection::single(Range::new(0, 1)));
        let mut surface = Surface::new(7, 1);
        let overlays = Overlays {
            decorations: Some(&decorations),
            highlighter: None,
            phrases: &[],
            search: None,
        };
        let options = EditorOptions::default();
        paint_view(
            &document,
            &view,
            &options,
            CursorShape::Bar,
            overlays,
            &mut surface,
        );
        let cells: Vec<_> = (0..7)
            .map(|x| {
                let cell = surface.cell(x, 0).unwrap();
                (cell.style, cell.underline)
            })
            .collect();
        assert_eq!(
            cells,
            [
                (Style::Selection, None),
                (Style::Highlight, Some(DecorationKind::Warning)),
                (Style::Highlight, Some(DecorationKind::Error)),
                (Style::CursorLine, Some(DecorationKind::Warning)),
                (Style::CursorLine, Some(DecorationKind::Warning)),
                (Style::CursorLine, Some(DecorationKind::Hint)),
                (Style::CursorLine, None),
            ]
        );
    }

    #[test]
    fn test_relative_line_numbers_count_from_the_cursor_and_hybrid_keeps_its_own() {
        let document = Document::new("a\nb\nc\nd");
        let mut view = view(6, 4);
        view.set_selection(Selection::point(4));
        let paint = |options: EditorOptions| {
            let mut surface = Surface::new(1, 1);
            paint_view(
                &document,
                &view,
                &options,
                CursorShape::Block,
                Overlays::default(),
                &mut surface,
            );
            screen(&surface)
        };
        let relative = EditorOptions {
            relative_line_numbers: true,
            ..EditorOptions::default()
        };
        assert_eq!(paint(relative), ["  2 a ", "  1 b ", "  0 c ", "  1 d "]);
        let hybrid = EditorOptions {
            line_numbers: true,
            ..relative
        };
        assert_eq!(paint(hybrid), ["  2 a ", "  1 b ", "  3 c ", "  1 d "]);
    }

    /// The columns of row `row` of `surface` whose cells carry `layers`.
    fn layered(surface: &Surface, row: u16, layers: Layers) -> Vec<u16> {
        (0..surface.cols())
            .filter(|column| {
                surface
                    .cell(*column, row)
                    .is_some_and(|cell| cell.layers.contains(layers))
            })
            .collect()
    }

    fn paint_plain(document: &Document, view: &View, options: &EditorOptions) -> Surface {
        let mut surface = Surface::new(1, 1);
        paint_view(
            document,
            view,
            options,
            CursorShape::Block,
            Overlays::default(),
            &mut surface,
        );
        surface
    }

    #[test]
    fn test_indent_guides_mark_blank_cells_at_each_level_and_run_through_blank_lines() {
        let document = Document::new("a\n        b\n\n    \tc\n  d");
        let options = EditorOptions {
            indent_guides: true,
            ..EditorOptions::default()
        };
        let surface = paint_plain(&document, &view(12, 5), &options);
        let guides = |row| layered(&surface, row, Layers::INDENT_GUIDE);
        assert!(guides(0).is_empty(), "no indent");
        assert_eq!(guides(1), [0, 4], "two levels of four");
        assert_eq!(guides(2), [0, 4], "the blank line takes the shallower side");
        assert_eq!(guides(3), [0, 4], "a tab fills to the next stop");
        assert_eq!(guides(4), [0], "half a level is one guide");
    }

    #[test]
    fn test_occurrences_of_the_word_at_the_cursor_are_lit_as_whole_words() {
        let document = Document::new("foo bar foo\nfoo2 foo");
        let options = EditorOptions {
            light_occurrences: true,
            ..EditorOptions::default()
        };
        let surface = paint_plain(&document, &view(12, 2), &options);
        assert_eq!(
            layered(&surface, 0, Layers::OCCURRENCE),
            [0, 1, 2, 8, 9, 10]
        );
        assert_eq!(
            layered(&surface, 1, Layers::OCCURRENCE),
            [5, 6, 7],
            "not foo2"
        );
        let mut on_blank = view(12, 2);
        on_blank.set_selection(Selection::point(3));
        let surface = paint_plain(&document, &on_blank, &options);
        assert!(layered(&surface, 0, Layers::OCCURRENCE).is_empty());
    }

    #[test]
    fn test_sticky_scroll_pins_the_lines_opening_the_blocks_in_sight() {
        let text = "|fn a() {\n    if x {\n        one\n        two\n        three\n    }\n}";
        let options = EditorOptions {
            sticky_scroll: 3,
            ..EditorOptions::default()
        };
        let mut harness = Harness::with_options(text, options);
        harness.editor.resize_view(harness.view, 16, 3);
        harness.keys("G");
        let mut surface = Surface::new(0, 0);
        let top = ScrollOffset {
            column: 0,
            line: 2,
            row: 0,
        };
        harness.editor.view_mut(harness.view).set_scroll(top);
        render_view(&harness.editor, harness.view, &mut surface);
        assert_eq!(
            screen(&surface),
            ["fn a() {        ", "    if x {      ", "        three   "],
            "two lines pinned over one and two"
        );
        assert!(layered(&surface, 0, Layers::PINNED).len() == 16);
        assert!(layered(&surface, 2, Layers::PINNED).is_empty());
    }

    #[test]
    fn test_virtual_lines_take_rows_of_their_own_that_row_motions_pass_over() {
        let mut harness = Harness::new("|one\ntwo");
        let document = harness.editor.view(harness.view).unwrap().document();
        let ghost = vec![
            VirtualLine::new(0, "ghost a"),
            VirtualLine::new(0, "ghost b"),
        ];
        harness.editor.set_virtual_lines(document, "ai", ghost);
        harness.editor.resize_view(harness.view, 8, 4);
        let mut surface = Surface::new(0, 0);
        render_view(&harness.editor, harness.view, &mut surface);
        assert_eq!(
            screen(&surface),
            ["one     ", "ghost a ", "ghost b ", "two     "]
        );
        assert_eq!(style_at(&surface, 0, 1), Style::Inlay);
        harness.keys("g j");
        assert_eq!(harness.marked(), "one\n|two", "past both virtual rows");
        harness.keys("g k");
        assert_eq!(harness.marked(), "|one\ntwo");
        harness.keys("A x <esc>");
        render_view(&harness.editor, harness.view, &mut surface);
        assert_eq!(
            surface.row_text(1),
            "ghost a ",
            "carried along with its line"
        );
    }

    #[test]
    fn test_host_lanes_show_their_own_signs_left_of_the_sign_column() {
        let document = Document::new("ab\ncd");
        let mut decorations = Decorations::default();
        decorations.set_lanes(vec![GutterLane::new("git", 1), GutterLane::new("fold", 1)]);
        let signs = vec![
            Sign::new(0, "+", DecorationKind::Info).in_lane("git"),
            Sign::new(1, "v", DecorationKind::Hint).in_lane("fold"),
            Sign::new(1, "E", DecorationKind::Error),
            Sign::new(0, "?", DecorationKind::Error).in_lane("nowhere"),
        ];
        decorations.set_signs(&document, "host", signs);
        let options = EditorOptions {
            sign_column: true,
            ..EditorOptions::default()
        };
        let mut surface = Surface::new(1, 1);
        let overlays = Overlays {
            decorations: Some(&decorations),
            highlighter: None,
            phrases: &[],
            search: None,
        };
        paint_view(
            &document,
            &view(7, 2),
            &options,
            CursorShape::Block,
            overlays,
            &mut surface,
        );
        assert_eq!(screen(&surface), ["+   ab ", " vE cd "]);
        assert_eq!(style_at(&surface, 0, 0), Style::Sign(DecorationKind::Info));
        assert_eq!(
            style_at(&surface, 0, 1),
            Style::LineNumber,
            "a blank lane cell"
        );
    }

    #[test]
    fn test_the_sign_column_shows_each_lines_sign_left_of_the_line_numbers() {
        let document = Document::new(
            "abcdef
x
y",
        );
        let mut decorations = Decorations::default();
        let signs = vec![
            Sign::new(0, "W", DecorationKind::Warning),
            Sign::new(2, "日本", DecorationKind::Error),
        ];
        decorations.set_signs(&document, "lsp", signs);
        let options = EditorOptions {
            line_numbers: true,
            sign_column: true,
            wrap: true,
            ..EditorOptions::default()
        };
        let mut view = view(9, 4);
        view.set_selection(Selection::point(7));
        let mut surface = Surface::new(9, 4);
        let overlays = Overlays {
            decorations: Some(&decorations),
            highlighter: None,
            phrases: &[],
            search: None,
        };
        let cursor = paint_view(
            &document,
            &view,
            &options,
            CursorShape::Block,
            overlays,
            &mut surface,
        );
        assert_eq!(
            screen(&surface),
            ["W   1 abc", "      def", "    2 x  ", "日  3 y  "]
        );
        assert_eq!(
            style_at(&surface, 0, 0),
            Style::Sign(DecorationKind::Warning)
        );
        assert_eq!(style_at(&surface, 1, 0), Style::LineNumber);
        assert_eq!(style_at(&surface, 0, 1), Style::LineNumber, "not on a wrap");
        assert_eq!(style_at(&surface, 1, 3), Style::Sign(DecorationKind::Error));
        assert_eq!(cursor.map(|cursor| cursor.column), Some(6));
    }

    #[test]
    fn test_a_long_line_paints_only_its_part_in_sight_as_a_short_one_would() {
        use crate::mouse::{MouseEvent, MouseEventKind};
        use crate::testing::Harness;
        let line = format!("{}\t日z{}", "ab".repeat(3000), "c".repeat(3000));
        let mut harness = Harness::new(&format!("|{line}\nnext"));
        harness.editor.resize_view(harness.view, 12, 2);
        harness.keys("6002l");
        let mut surface = Surface::new(0, 0);
        let cursor = render_view(&harness.editor, harness.view, &mut surface).unwrap();
        let view = harness.editor.view(harness.view).unwrap();
        assert_eq!(
            view.scroll().column,
            5995,
            "the z at cell 6006 is the last in sight"
        );
        assert_eq!(surface.row_text(0).trim_end(), "babab    日z");
        assert_eq!((cursor.column, cursor.row), (11, 0));
        harness.keys("h");
        let cursor = render_view(&harness.editor, harness.view, &mut surface).unwrap();
        assert_eq!(cursor.column, 9, "on the wide char after the tab");
        let click = MouseEvent::new(MouseEventKind::Press, 3, 0);
        harness.editor.handle_mouse(harness.view, click);
        assert_eq!(harness.editor.view(harness.view).unwrap().cursor(), 5998);
    }

    #[test]
    fn test_an_inlay_paints_before_its_char_and_the_cursor_and_clicks_pass_it() {
        let mut harness = Harness::new("let x |= 1;");
        let (editor, view) = (&mut harness.editor, harness.view);
        let document = editor.view(view).unwrap().document();
        let hint = Inlay::new(5, ": i32").with_highlight(HighlightId::new(7));
        editor.set_inlays(document, "lsp", vec![hint.clone()]);
        assert_eq!(editor.inlays(document, "lsp"), [hint]);
        editor.resize_view(view, 16, 1);
        let mut surface = Surface::new(0, 0);
        let cursor = render_view(editor, view, &mut surface).unwrap();
        assert_eq!(surface.row_text(0), "let x: i32 = 1; ");
        assert_eq!(cursor.column, 11, "on the = after the inlay");
        let cell = surface.cell(6, 0).unwrap();
        assert_eq!(
            (cell.style, cell.highlight),
            (Style::Inlay, Some(HighlightId::new(7)))
        );
        editor.handle_mouse(view, MouseEvent::new(MouseEventKind::Press, 8, 0));
        assert_eq!(harness.marked(), "let x| = 1;", "the char after the inlay");
    }

    #[test]
    fn test_an_inlay_wraps_with_its_line_and_pushes_the_lines_below_down() {
        let options = EditorOptions {
            wrap: true,
            ..EditorOptions::default()
        };
        let mut harness = Harness::with_options("|ab\ncd", options);
        let (editor, view) = (&mut harness.editor, harness.view);
        let document = editor.view(view).unwrap().document();
        editor.set_inlays(document, "ai", vec![Inlay::new(2, "123456789")]);
        editor.resize_view(view, 8, 3);
        let mut surface = Surface::new(0, 0);
        render_view(editor, view, &mut surface);
        assert_eq!(screen(&surface), ["ab123456", "789     ", "cd      "]);
        harness.keys("j");
        assert_eq!(harness.marked(), "ab\n|cd");
    }

    #[test]
    fn test_concealed_text_hides_except_on_the_cursor_line() {
        let mut harness = Harness::new("**bold** x\n|next");
        let (editor, view) = (&mut harness.editor, harness.view);
        let document = editor.view(view).unwrap().document();
        editor.set_conceals(document, "md", vec![0..2, 6..8]);
        editor.resize_view(view, 12, 2);
        let mut surface = Surface::new(0, 0);
        render_view(editor, view, &mut surface);
        assert_eq!(screen(&surface), ["bold x      ", "next        "]);
        editor.handle_mouse(view, MouseEvent::new(MouseEventKind::Press, 5, 0));
        assert_eq!(
            harness.marked(),
            "**bold** |x\nnext",
            "the x past the hidden **"
        );
        render_view(&harness.editor, harness.view, &mut surface);
        assert_eq!(
            screen(&surface),
            ["**bold** x  ", "next        "],
            "shown again"
        );
    }

    #[test]
    fn test_conceal_modes_hide_everywhere_or_nowhere() {
        let rendered = |conceal: ConcealMode| {
            let options = EditorOptions {
                conceal,
                ..EditorOptions::default()
            };
            let mut harness = Harness::with_options("**b**|\n**c**", options);
            let document = harness.editor.view(harness.view).unwrap().document();
            let conceals = vec![0..2, 3..5, 6..8, 9..11];
            harness.editor.set_conceals(document, "md", conceals);
            harness.editor.resize_view(harness.view, 6, 2);
            let mut surface = Surface::new(0, 0);
            render_view(&harness.editor, harness.view, &mut surface);
            screen(&surface)
        };
        assert_eq!(
            rendered(ConcealMode::RevealCursorLine),
            ["**b** ", "c     "]
        );
        assert_eq!(
            rendered(ConcealMode::Hide),
            ["b     ", "c     "],
            "the cursor's line too"
        );
        assert_eq!(rendered(ConcealMode::Show), ["**b** ", "**c** "]);
    }

    #[test]
    fn test_host_backgrounds_paint_under_highlights_the_narrowest_winning() {
        let document = Document::new("abc\nde");
        let mut decorations = Decorations::default();
        let (line, word) = (HighlightId::new(1), HighlightId::new(2));
        let marked = vec![
            Decoration::new(0..4, DecorationKind::Background(line)),
            Decoration::new(1..2, DecorationKind::Background(word)),
            Decoration::new(0..1, DecorationKind::Highlight),
            Decoration::new(4..5, DecorationKind::Background(line)),
        ];
        decorations.set_decorations(&document, "diff", marked);
        let mut view = view(6, 2);
        view.set_selection(Selection::point(6));
        let mut surface = Surface::new(6, 2);
        let overlays = Overlays {
            decorations: Some(&decorations),
            highlighter: None,
            phrases: &[],
            search: None,
        };
        let options = EditorOptions::default();
        paint_view(
            &document,
            &view,
            &options,
            CursorShape::Block,
            overlays,
            &mut surface,
        );
        let row = |y| -> Vec<Style> { (0..6).map(|x| style_at(&surface, x, y)).collect() };
        let (on_line, on_word) = (Style::Background(line), Style::Background(word));
        assert_eq!(
            row(0),
            [
                Style::Highlight,
                on_word,
                on_line,
                on_line,
                on_line,
                on_line
            ],
            "a break covered fills the row"
        );
        let cursor_line = Style::CursorLine;
        assert_eq!(
            row(1),
            [
                on_line,
                cursor_line,
                cursor_line,
                cursor_line,
                cursor_line,
                cursor_line
            ],
            "no break covered on the last line"
        );
    }

    #[test]
    fn test_a_preedit_shows_at_the_cursor_while_typing_and_a_key_ends_it() {
        let mut harness = Harness::new("ab|c");
        let view = harness.view;
        harness.editor.resize_view(view, 9, 1);
        harness.editor.set_preedit(view, "vi\u{1ec7}t");
        let mut surface = Surface::new(0, 0);
        render_view(&harness.editor, view, &mut surface);
        assert_eq!(surface.row_text(0), "abc      ", "not in Normal mode");
        harness.keys("i");
        harness.editor.set_preedit(view, "vi\u{1ec7}t");
        let cursor = render_view(&harness.editor, view, &mut surface).unwrap();
        assert_eq!(surface.row_text(0), "abvi\u{1ec7}tc  ");
        assert_eq!(style_at(&surface, 3, 0), Style::Preedit);
        assert_eq!(cursor.column, 6, "after the text being composed");
        assert_eq!(harness.marked(), "ab|c", "the document is untouched");
        harness.keys("vi\u{1ec7}t");
        let cursor = render_view(&harness.editor, view, &mut surface).unwrap();
        assert_eq!(
            surface.row_text(0),
            "abvi\u{1ec7}tc  ",
            "typed once, not twice"
        );
        assert_eq!(style_at(&surface, 3, 0), Style::CursorLine);
        assert_eq!(cursor.column, 6);
        harness.editor.set_preedit(view, "x");
        harness.editor.set_preedit(view, "");
        render_view(&harness.editor, view, &mut surface);
        assert_eq!(surface.row_text(0), "abvi\u{1ec7}tc  ", "empty is none");
    }

    #[test]
    fn test_extra_cursors_paint_on_their_cells_even_past_a_line_end() {
        let mut harness = Harness::new("|ab\ncd");
        let view = harness.view;
        harness.editor.set_cursors(view, vec![2, 4]);
        harness.editor.resize_view(view, 4, 2);
        let mut surface = Surface::new(0, 0);
        let cursor = render_view(&harness.editor, view, &mut surface).unwrap();
        assert_eq!((cursor.column, cursor.row), (0, 0), "the main cursor");
        let cells: Vec<_> = [(2, 0), (1, 1), (0, 1)]
            .iter()
            .map(|&(x, y)| style_at(&surface, x, y))
            .collect();
        assert_eq!(cells, [Style::ExtraCursor, Style::ExtraCursor, Style::Text]);
        assert_eq!(surface.cell(1, 1).unwrap().symbol, "d", "the text stays");
    }

    #[test]
    fn test_the_bracket_matching_the_one_at_the_cursor_is_marked_with_it() {
        let mut harness = Harness::new("f(a|(b)c)");
        let view = harness.view;
        harness.editor.resize_view(view, 9, 1);
        let mut surface = Surface::new(0, 0);
        let marked = |surface: &Surface| -> Vec<u16> {
            (0..9)
                .filter(|x| style_at(surface, *x, 0) == Style::MatchingBracket)
                .collect()
        };
        render_view(&harness.editor, view, &mut surface);
        assert_eq!(marked(&surface), [3, 5]);
        harness.keys("l l l i");
        render_view(&harness.editor, view, &mut surface);
        assert_eq!(
            marked(&surface),
            [3, 5],
            "in Insert mode, the one before the cursor"
        );
        harness.keys("<esc> 0");
        render_view(&harness.editor, view, &mut surface);
        assert!(marked(&surface).is_empty(), "no bracket at the cursor");
        let options = EditorOptions {
            match_brackets: false,
            ..EditorOptions::default()
        };
        harness.editor.set_options(options);
        harness.keys("f (");
        render_view(&harness.editor, view, &mut surface);
        assert!(marked(&surface).is_empty(), "turned off");
    }
}
