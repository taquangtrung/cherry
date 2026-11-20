//! How an [`EditorView`](crate::EditorView) draws: Cherry's painted
//! surface turned into misty cells in the theme's colors, the marks drawn
//! over them as shapes (indentation guides, changed lines, problems, lit
//! occurrences), the text being composed, and the glide after a scroll.

use std::iter;
use std::mem;
use std::ops::Range;
use std::time::Instant;

use cherry_engine::{
    DecorationKind, Editor, Layers, LineChange, ScrollOffset, Surface, render_prompt, render_view,
};
use misty::{
    Attributes, CellBuffer, CellColor, CellMetrics, CellSource, CellWidth, Frame, Paint, Point,
    Preedit, Rect, Rgb, Rgba, Stroke, Theme, UnderlineStyle, VectorPath, char_width, text_width,
};

use super::completion::draw_completion;
use super::{EditorView, HighlightStyle, ViewLayout, scrollbar};

// ========================================================================
// Constants
// ========================================================================

/// What a blank cell at a level of indentation holds: a guide line, its
/// glyph hidden, which [`EditorView::draw_cells`] draws as a hairline down
/// the cell's left edge instead, the font's box line being as thick, as
/// long, and as far from the edge as the font makes it.
const INDENT_GUIDE: &str = "\u{2502}";

/// How wide an indentation guide is drawn, and how far in from its cell's
/// left edge, so it clears the glyph of the column before it, in logical
/// pixels.
const GUIDE_WIDTH: f32 = 1.0;
const GUIDE_INSET: f32 = 1.0;

/// How far an indentation guide's color is taken from the border color
/// toward the background, so the guides sit behind the text rather than
/// stand out as rules of their own.
const GUIDE_FADE: f32 = 0.6;

/// How thick the outline around a lit occurrence of a word is drawn, in
/// logical pixels.
const OUTLINE_WIDTH: f32 = 1.0;

/// How wide the bar down a changed line's sign cell is drawn, and how
/// thick the tick where lines were deleted, in logical pixels.
const CHANGE_BAR_WIDTH: f32 = 2.0;

/// What a sign shows to be drawn as a filled dot, and as a ring, in its
/// color: the host's mark for a problem, drawn as a shape rather than the
/// font's glyph so the ring's line is as thin as an underline.
const PROBLEM_DOT: &str = "●";
const PROBLEM_RING: &str = "○";

/// How wide a problem's dot or ring is, as a share of its cell's width.
const PROBLEM_MARK_SHARE: f32 = 0.6;

/// The palette colors changed lines are marked in: added green, modified
/// blue, and deleted red.
const RED: u8 = 1;
const GREEN: u8 = 2;
const BLUE: u8 = 4;

/// The palette colors phrases lit by hand take, in turn: the six named
/// colors from red to cyan.
pub(super) const PHRASE_COLORS: [usize; 6] = [1, 2, 3, 4, 5, 6];

// ========================================================================
// Data Structures
// ========================================================================

/// A frame from before a scroll still in sight while the text glides:
/// drawn where the text drawn since has not slid in yet.
#[derive(Debug)]
pub(super) struct Glided {
    /// Its cells, the cursor left out.
    pub(super) cells: CellBuffer,
    /// The marks drawn over them.
    pub(super) marks: Vec<Mark>,
    /// The lines the view has scrolled since it was drawn, positive
    /// toward the end of the document: how many rows ahead of the text
    /// drawn now it stands.
    pub(super) shift: isize,
}

/// A run of cells on one row drawn over with a shape after the text.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct Mark {
    /// The cells it covers.
    pub(super) cols: Range<usize>,
    /// What it marks, which says how it is drawn.
    pub(super) kind: MarkKind,
    /// The row it is on.
    pub(super) row: usize,
}

/// What a [`Mark`] marks.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum MarkKind {
    /// A changed line, in the sign column: a bar down the cell's left
    /// edge, or a tick along its top for lines deleted above.
    Change(LineChange),
    /// A problem's sign, [`PROBLEM_DOT`] or [`PROBLEM_RING`]: a dot, or a
    /// ring one logical pixel thick, in the color of its kind.
    Problem {
        /// The color of the problem's kind.
        color: Rgb,
        /// Whether it is a dot rather than a ring.
        filled: bool,
    },
    /// An occurrence of a word, the word's other uses lit by Cherry or
    /// named by the language server: outlined in the muted color.
    Occurrence,
}

// ========================================================================
// EditorView Implementations
// ========================================================================

impl EditorView {
    /// The size of a cell of the text in `frame`.
    pub(super) fn metrics(&self, frame: &Frame) -> CellMetrics {
        match &self.font {
            Some(font) => frame.cell_metrics_of(font),
            None => frame.cell_metrics(),
        }
    }

    /// Draws `cells` at `origin` of `frame` in the text's font, each
    /// indentation guide a hairline the row's full height, so a block's
    /// rows join into one line, and `marks` over them.
    fn draw_cells(
        &self,
        frame: &mut Frame,
        cells: &CellBuffer,
        marks: &[Mark],
        origin: Point,
        theme: &Theme,
    ) {
        match &self.font {
            Some(font) => frame.draw_cells_in(cells, origin, font, theme),
            None => frame.draw_cells(cells, origin, theme),
        }
        let metrics = self.metrics(frame);
        let scale_factor = frame.scale_factor() as f32;
        let width = (GUIDE_WIDTH * scale_factor).round().max(1.0);
        let inset = (GUIDE_INSET * scale_factor).round();
        for row in 0..cells.rows() {
            for col in 0..cells.cols() {
                let Some(cell) = cells.cell(row, col).filter(|cell| is_guide(cell)) else {
                    continue;
                };
                let color = match cell.style.foreground {
                    CellColor::Rgb(color) => color,
                    _ => guide_color(theme),
                };
                let left = (origin.x + col as f32 * metrics.width + inset).round();
                let top = origin.y + row as f32 * metrics.height;
                frame.fill_rect(Rect::new(left, top, width, metrics.height), color);
            }
        }
        let thickness = (OUTLINE_WIDTH * scale_factor).round().max(1.0);
        let bar = (CHANGE_BAR_WIDTH * scale_factor).round().max(1.0);
        for mark in marks {
            let left = (origin.x + mark.cols.start as f32 * metrics.width).round();
            let right = (origin.x + mark.cols.end as f32 * metrics.width).round();
            let top = (origin.y + mark.row as f32 * metrics.height).round();
            let bottom = (origin.y + (mark.row + 1) as f32 * metrics.height).round();
            let (width, height) = (right - left, bottom - top);
            match mark.kind {
                MarkKind::Change(LineChange::Deleted) => {
                    let color = palette_color(theme, RED);
                    frame.fill_rect(Rect::new(left, top, width, bar), color);
                }
                MarkKind::Change(change) => {
                    let color = match change {
                        LineChange::Added => palette_color(theme, GREEN),
                        _ => palette_color(theme, BLUE),
                    };
                    frame.fill_rect(Rect::new(left, top, bar, height), color);
                }
                MarkKind::Problem { color, filled } => {
                    let center = Point::new((left + right) / 2.0, (top + bottom) / 2.0);
                    let radius = width.min(height) * PROBLEM_MARK_SHARE / 2.0;
                    let paint = Paint::Solid(Rgba::from(color));
                    match filled {
                        true => frame.fill_path(&VectorPath::circle(center, radius), paint),
                        false => {
                            let ring = VectorPath::circle(center, radius - thickness / 2.0);
                            frame.stroke_path(&ring, &Stroke::new(thickness), paint);
                        }
                    }
                }
                MarkKind::Occurrence => {
                    let color = occurrence_color(theme);
                    for edge in [
                        Rect::new(left, top, width, thickness),
                        Rect::new(left, bottom - thickness, width, thickness),
                        Rect::new(left, top, thickness, height),
                        Rect::new(right - thickness, top, thickness, height),
                    ] {
                        frame.fill_rect(edge, color);
                    }
                }
            }
        }
    }

    /// Draws the view in `area`, resizing it to the whole cells that fit:
    /// text in the theme's foreground, the cursor line on the hover color,
    /// the selection on the selection color, search matches on the warning
    /// color, line numbers muted, and the cursor as the editor shapes it.
    /// An open prompt takes the bottom row and the cursor.
    pub(super) fn draw_view(
        &mut self,
        editor: &mut Editor,
        frame: &mut Frame,
        area: Rect,
        theme: &Theme,
    ) {
        // The scrollbar's strip comes off the right; the text has the rest.
        let strip_width = match self.scrollbar_hidden {
            true => 0.0,
            false => (scrollbar::STRIP_WIDTH * frame.scale_factor() as f32).round(),
        };
        let strip_x = area.x + (area.width - strip_width).max(0.0);
        let area = Rect::new(
            area.x,
            area.y,
            (area.width - strip_width).max(0.0),
            area.height,
        );
        let metrics = self.metrics(frame);
        let cols = u16::try_from(metrics.cols_in(area.width)).unwrap_or(u16::MAX);
        let rows = u16::try_from(metrics.rows_in(area.height)).unwrap_or(u16::MAX);
        let prompt_open = editor
            .view(self.view)
            .is_some_and(|view| view.prompt().is_some());
        let view_rows = match prompt_open {
            true => rows.saturating_sub(1),
            false => rows,
        };
        let resized = editor
            .view(self.view)
            .is_some_and(|view| (view.cols(), view.rows()) != (cols, view_rows));
        if resized {
            editor.resize_view(self.view, cols, view_rows);
        }
        self.layout = Some(ViewLayout {
            area: Rect::new(
                area.x,
                area.y,
                f32::from(cols) * metrics.width,
                f32::from(view_rows) * metrics.height,
            ),
            metrics,
        });
        if !self.scrollbar_hidden {
            let height = f32::from(view_rows) * metrics.height;
            let strip = Rect::new(strip_x, area.y, strip_width, height);
            let rows = editor.view_rows(self.view);
            self.scrollbar
                .draw(frame, strip, rows, theme, self.focused, Instant::now());
        }
        let cursor = render_view(editor, self.view, &mut self.surface);
        let cursor = cursor.filter(|_| !prompt_open);
        let highlights = match &self.highlights {
            Some(styles) => styles.clone(),
            None => highlight_colors(theme),
        };
        fill_cells(
            &self.surface,
            cursor,
            theme,
            &highlights,
            &mut self.next,
            &mut self.next_marks,
        );
        // Out of focus, the cursor is an outline where it stands.
        if !self.focused
            && let Some(cursor) = self.next.cursor()
        {
            let hollow = misty::Cursor::new(cursor.row, cursor.col, misty::CursorShape::Hollow);
            self.next.set_cursor(Some(hollow));
        }
        let scroll = editor.view(self.view).map(|view| view.scroll());
        let now = Instant::now();
        if let (Some(old), Some(new)) = (self.scroll, scroll)
            && old != new
        {
            self.follow_scroll(old, new, now);
        }
        self.scroll = scroll;
        mem::swap(&mut self.cells, &mut self.next);
        mem::swap(&mut self.marks, &mut self.next_marks);
        let prompt_top = area.y + f32::from(view_rows) * metrics.height;
        if prompt_open {
            let cursor = render_prompt(editor, self.view, &mut self.prompt_surface);
            fill_cells(
                &self.prompt_surface,
                cursor,
                theme,
                &highlights,
                &mut self.prompt_cells,
                &mut Vec::new(),
            );
        }
        // The cursor is in the prompt while one is open, in the view
        // otherwise; a prompt's preedit is drawn over it, the view's is in
        // its text.
        let (cursor_cells, cursor_origin) = match prompt_open {
            true => (&mut self.prompt_cells, Point::new(area.x, prompt_top)),
            false => (&mut self.cells, Point::new(area.x, area.y)),
        };
        self.cursor_area = cursor_cells.cursor().map(|cursor| {
            Rect::new(
                cursor_origin.x + cursor.col as f32 * metrics.width,
                cursor_origin.y + cursor.row as f32 * metrics.height,
                metrics.width,
                metrics.height,
            )
        });
        if let Some(preedit) = self.preedit.as_ref().filter(|_| prompt_open) {
            overlay_preedit(cursor_cells, preedit);
        }
        let offset = self.motion.position(now);
        self.drawn_offset = offset;
        if offset == 0.0 {
            self.trail.clear();
        }
        match offset == 0.0 {
            true => self.draw_cells(
                frame,
                &self.cells,
                &self.marks,
                Point::new(area.x, area.y),
                theme,
            ),
            false => {
                let height = f32::from(view_rows) * metrics.height;
                self.draw_glide(
                    frame,
                    Rect::new(area.x, area.y, area.width, height),
                    offset,
                    theme,
                );
            }
        }
        if prompt_open {
            self.draw_cells(
                frame,
                &self.prompt_cells,
                &[],
                Point::new(area.x, prompt_top),
                theme,
            );
            return;
        }
        let completion = editor.view(self.view).and_then(|view| view.completion());
        if let Some(completion) = completion
            && let Some(anchor) = self.offset_rect(editor, completion.anchor())
        {
            draw_completion(frame, completion, anchor, theme);
        }
        let bounds = frame.bounds();
        misty::Widget::draw(&mut self.tooltip, frame, bounds, theme);
    }

    /// Starts gliding from the last frame's cells to the new ones in
    /// `next` after the view scrolled from `old` to `new`, when the two
    /// line up: the same rows of text, moved by the lines scrolled, less
    /// than a screen away counting a glide already under way. Otherwise
    /// the text jumps. A scroll in the middle of a glide glides on from
    /// where the text is, the frames before it kept to fill the rows it
    /// has not reached.
    fn follow_scroll(&mut self, old: ScrollOffset, new: ScrollOffset, now: Instant) {
        let shift = new.line as isize - old.line as isize;
        let rows = self.next.rows();
        let start = self.motion.position(now) + shift as f32;
        let lines_up = old.row == 0
            && new.row == 0
            && old.column == new.column
            && shift != 0
            && start.abs() < rows as f32
            && rows_line_up(&self.cells, &self.next, shift);
        if !lines_up {
            self.trail.clear();
            self.motion.jump_to(0.0);
            return;
        }
        let mut cells = mem::replace(&mut self.cells, CellBuffer::new(0, 0));
        cells.set_cursor(None);
        let marks = mem::take(&mut self.marks);
        for glided in &mut self.trail {
            glided.shift += shift;
        }
        self.trail.insert(
            0,
            Glided {
                cells,
                marks,
                shift,
            },
        );
        // A frame a screen or more from its place is out of sight for
        // the rest of the glide.
        self.trail
            .retain(|glided| (start - glided.shift as f32).abs() < rows as f32);
        self.motion.jump_to(start);
        self.motion.scroll_to(0.0, now);
    }

    /// Draws the glide in `area` with the new cells `offset` rows from
    /// their place, and the frames from before each scroll, newest first,
    /// filling the rows the newer ones have left bare.
    fn draw_glide(&self, frame: &mut Frame, area: Rect, offset: f32, theme: &Theme) {
        let height = self.metrics(frame).height;
        let top = |rows: f32| (area.y + rows * height).round();
        let older = self.trail.iter().map(|glided| {
            let at = offset - glided.shift as f32;
            (&glided.cells, glided.marks.as_slice(), at)
        });
        // The rows no newer frame has covered, from the top down. A frame
        // a screen tall that overlaps the area covers its top or its
        // bottom, so what is left is always one run.
        let (mut bare_top, mut bare_bottom) = (area.y, area.y + area.height);
        let newest = iter::once((&self.cells, self.marks.as_slice(), offset));
        for (cells, marks, at) in newest.chain(older) {
            if bare_top >= bare_bottom {
                break;
            }
            let (from, to) = (top(at), top(at + cells.rows() as f32));
            let bare = Rect::new(area.x, bare_top, area.width, bare_bottom - bare_top);
            frame.clipped(bare, |frame| {
                self.draw_cells(frame, cells, marks, Point::new(area.x, from), theme);
            });
            match from <= bare_top {
                true => bare_top = bare_top.max(to),
                false => bare_bottom = bare_bottom.min(from),
            }
        }
    }
}

// ========================================================================
// Functions
// ========================================================================

/// How each highlight name is drawn under `theme`, by highlight id: the
/// names of the engine's `syntax` module, whose ids index them, in the
/// theme's syntax colors. Tree-sitter's highlighter and the TextMate one
/// number their highlights by the same names.
#[cfg(any(feature = "syntax-engine", feature = "syntax-textmate"))]
pub(super) fn highlight_colors(theme: &Theme) -> Vec<HighlightStyle> {
    cherry_engine::syntax::HIGHLIGHT_NAMES
        .iter()
        .map(|name| HighlightStyle {
            foreground: theme.syntax.color(name),
            ..HighlightStyle::default()
        })
        .collect()
}

/// No highlight styles: without a syntax feature no highlight id has a
/// name to color it by.
#[cfg(not(any(feature = "syntax-engine", feature = "syntax-textmate")))]
pub(super) fn highlight_colors(_theme: &Theme) -> Vec<HighlightStyle> {
    Vec::new()
}

/// Whether `new` shows the rows of `old` moved up by `shift` rows (down
/// for a negative shift) wherever both have them, comparing text only,
/// since the cursor line and selection move with the cursor.
pub(super) fn rows_line_up(old: &CellBuffer, new: &CellBuffer, shift: isize) -> bool {
    if (old.cols(), old.rows()) != (new.cols(), new.rows()) {
        return false;
    }
    let rows = new.rows() as isize;
    (0..rows)
        .filter_map(|row| {
            let from = row + shift;
            (0..rows)
                .contains(&from)
                .then_some((row as usize, from as usize))
        })
        .all(|(row, from)| {
            (0..new.cols()).all(|col| match (new.cell(row, col), old.cell(from, col)) {
                (Some(new), Some(old)) => new.ch == old.ch && new.tail == old.tail,
                (None, None) => true,
                (Some(_), None) | (None, Some(_)) => false,
            })
        })
}

/// Whether `cell` is an indentation guide, its glyph hidden for the line
/// drawn in its place.
pub(super) fn is_guide(cell: &misty::Cell) -> bool {
    cell.style.attributes.contains(Attributes::CONCEAL) && cell.ch.to_string() == INDENT_GUIDE
}

/// Writes `surface` and `cursor` into `cells`, sized to match, Cherry's
/// styles mapped to `theme`'s colors and highlighted text drawn as
/// `highlights` says, indexed by highlight id, and the marks drawn over
/// them into `marks`: the runs of cells that are occurrences of a word,
/// and the sign column's cells by changed lines.
pub(super) fn fill_cells(
    surface: &Surface,
    cursor: Option<cherry_engine::Cursor>,
    theme: &Theme,
    highlights: &[HighlightStyle],
    cells: &mut CellBuffer,
    marks: &mut Vec<Mark>,
) {
    let (cols, rows) = (surface.cols(), surface.rows());
    cells.resize(usize::from(cols), usize::from(rows));
    marks.clear();
    for row in 0..rows {
        let mut after_wide = false;
        for col in 0..cols {
            let Some(painted) = surface.cell(col, row) else {
                continue;
            };
            let mark = mark_kind(painted, theme);
            if let Some(kind) = mark {
                let (row, col) = (usize::from(row), usize::from(col));
                match marks.last_mut() {
                    Some(run) if run.row == row && run.cols.end == col && run.kind == kind => {
                        run.cols.end += 1;
                    }
                    _ => marks.push(Mark {
                        cols: col..col + 1,
                        kind,
                        row,
                    }),
                }
            }
            let guide = painted.layers.contains(Layers::INDENT_GUIDE) && painted.symbol == " ";
            let symbol = if guide {
                INDENT_GUIDE
            } else if matches!(mark, Some(MarkKind::Problem { .. })) {
                // Drawn as a shape over the blank cell.
                " "
            } else {
                painted.symbol.as_str()
            };
            let mut chars = symbol.chars();
            let mut style = cell_style(painted, theme, highlights);
            if guide {
                style.attributes = style.attributes | Attributes::CONCEAL;
            }
            let cell = match chars.next() {
                None if after_wide => misty::Cell {
                    width: CellWidth::Continuation,
                    ..misty::Cell::new(' ', style)
                },
                None => misty::Cell::new(' ', style),
                Some(first) => {
                    let rest = chars.as_str();
                    misty::Cell {
                        tail: (!rest.is_empty()).then(|| rest.into()),
                        width: match text_width(&painted.symbol) > 1 {
                            true => CellWidth::Wide,
                            false => CellWidth::Single,
                        },
                        ..misty::Cell::new(first, style)
                    }
                }
            };
            after_wide = cell.width == CellWidth::Wide;
            cells.set(usize::from(row), usize::from(col), cell);
        }
    }
    cells.set_cursor(cursor.map(|cursor| {
        let shape = match cursor.shape {
            cherry_engine::CursorShape::Bar => misty::CursorShape::Bar,
            cherry_engine::CursorShape::Block => misty::CursorShape::Block,
            cherry_engine::CursorShape::Underline => misty::CursorShape::Underline,
            // A shape added after this was written: the block shows the
            // cursor's place whatever it means.
            _ => misty::CursorShape::Block,
        };
        misty::Cursor::new(usize::from(cursor.row), usize::from(cursor.column), shape)
    }));
}

/// Draws `preedit` over `cells` from the cursor on, underlined, cut at the
/// row's end, with a bar cursor at the input method's caret (none when it
/// hides it). Covered cells are hidden until the text is committed.
pub(super) fn overlay_preedit(cells: &mut CellBuffer, preedit: &Preedit) {
    let Some(cursor) = cells.cursor() else {
        return;
    };
    let style = misty::CellStyle {
        underline: UnderlineStyle::Single,
        ..misty::CellStyle::default()
    };
    let mut col = cursor.col;
    for c in preedit.text().chars() {
        let width = char_width(c);
        // Zero-width marks have no cell of their own.
        if width == 0 {
            continue;
        }
        if col + width > cells.cols() {
            break;
        }
        let wide = width > 1;
        let width = match wide {
            true => CellWidth::Wide,
            false => CellWidth::Single,
        };
        cells.set(
            cursor.row,
            col,
            misty::Cell {
                width,
                ..misty::Cell::new(c, style)
            },
        );
        if wide {
            let continuation = misty::Cell {
                width: CellWidth::Continuation,
                ..misty::Cell::new(' ', style)
            };
            cells.set(cursor.row, col + 1, continuation);
            col += 1;
        }
        col += 1;
    }
    let caret = preedit
        .caret_cells()
        .map(|cells_before| (cursor.col + cells_before).min(cells.cols().saturating_sub(1)));
    cells.set_cursor(caret.map(|col| misty::Cursor::new(cursor.row, col, misty::CursorShape::Bar)));
}

/// The cell style `painted` is drawn in under `theme`: its Cherry style,
/// its highlighted text as `highlights` draws its highlight id (the text
/// color unless the style sets one, an inlay's always; the background
/// unless the style sets one), a wash (its text too, where its id sets a
/// text color) or a sign of a host's own kind in its id's colors, and the
/// underline of its decoration.
pub(super) fn cell_style(
    painted: &cherry_engine::Cell,
    theme: &Theme,
    highlights: &[HighlightStyle],
) -> misty::CellStyle {
    let host = |id: cherry_engine::HighlightId| highlights.get(usize::from(id.index())).copied();
    let mut style = style_of(painted.style, theme);
    match painted.style {
        cherry_engine::Style::Background(id) => {
            if let Some(drawn) = host(id) {
                if let Some(background) = drawn.background {
                    style.background = CellColor::Rgb(background);
                }
                if let Some(foreground) = drawn.foreground {
                    style.foreground = CellColor::Rgb(foreground);
                }
            }
        }
        cherry_engine::Style::Sign(DecorationKind::Background(id)) => {
            if let Some(foreground) = host(id).and_then(|drawn| drawn.foreground) {
                style.foreground = CellColor::Rgb(foreground);
            }
        }
        _ => {}
    }
    if let Some(drawn) = painted.highlight.and_then(host) {
        let inlay = painted.style == cherry_engine::Style::Inlay;
        if let Some(color) = drawn
            .foreground
            .filter(|_| inlay || style.foreground == CellColor::Default)
        {
            style.foreground = CellColor::Rgb(color);
        }
        if let Some(color) = drawn
            .background
            .filter(|_| style.background == CellColor::Default)
        {
            style.background = CellColor::Rgb(color);
        }
        // An inlay the host colors is the host's to style whole: its slant
        // is for text that only says it is not the document's.
        style.attributes = match inlay && drawn.foreground.is_some() {
            true => drawn.attributes,
            false => style.attributes | drawn.attributes,
        };
    }
    if let Some(kind) = painted.underline {
        (style.underline, style.underline_color) = underline_of(kind, theme);
    }
    let layers = painted.layers;
    let plain_background = style.background == CellColor::Default;
    // A guide's cell is blank, so its text color is the guide's alone: a
    // highlight over the indent (a list item, a code block) would draw the
    // guide as loud as the text.
    if layers.contains(Layers::INDENT_GUIDE) {
        style.foreground = CellColor::Rgb(guide_color(theme));
    }
    if layers.contains(Layers::PINNED) && plain_background {
        style.background = CellColor::Rgb(theme.surface);
    }
    style
}

/// The mark drawn over `painted` under `theme`, if any: the change of its
/// line, for the sign column's cell by a changed line; a problem's dot or
/// ring, for a sign showing one; or an occurrence, outlined rather than
/// filled, for text lit as the word under the cursor or highlighted by a
/// decoration (the language server's other uses of a name).
fn mark_kind(painted: &cherry_engine::Cell, theme: &Theme) -> Option<MarkKind> {
    let layers = painted.layers;
    let change = [
        (Layers::CHANGE_ADDED, LineChange::Added),
        (Layers::CHANGE_DELETED, LineChange::Deleted),
        (Layers::CHANGE_MODIFIED, LineChange::Modified),
    ]
    .into_iter()
    .find(|(layer, _)| layers.contains(*layer));
    if let Some((_, change)) = change {
        return Some(MarkKind::Change(change));
    }
    if let cherry_engine::Style::Sign(kind) = painted.style {
        let filled = match painted.symbol.as_str() {
            PROBLEM_DOT => Some(true),
            PROBLEM_RING => Some(false),
            _ => None,
        };
        if let Some(filled) = filled {
            let color = decoration_color(kind, theme);
            return Some(MarkKind::Problem { color, filled });
        }
    }
    let occurrence =
        layers.contains(Layers::OCCURRENCE) || painted.style == cherry_engine::Style::Highlight;
    occurrence.then_some(MarkKind::Occurrence)
}

/// Color `index` of `theme`'s terminal palette (red, green, blue, …).
fn palette_color(theme: &Theme, index: u8) -> Rgb {
    theme.palette.colors[usize::from(index)]
}

/// The color indentation guides are drawn in under `theme`: its border
/// color, faded toward the background.
pub(super) fn guide_color(theme: &Theme) -> Rgb {
    theme.border.mix(theme.background, GUIDE_FADE)
}

/// The color occurrences of a word are outlined in under `theme`: its
/// muted color, so the other uses of a word show without pulling the eye
/// from the cursor.
fn occurrence_color(theme: &Theme) -> Rgb {
    theme.muted
}

/// The color decorations and signs of `kind` are drawn in under `theme`:
/// errors in the danger color, warnings in the warning color, information
/// and highlights in the accent color, hints and plain underlines muted.
fn decoration_color(kind: DecorationKind, theme: &Theme) -> Rgb {
    match kind {
        DecorationKind::Error => theme.danger,
        DecorationKind::Highlight | DecorationKind::Info => theme.accent,
        DecorationKind::Hint | DecorationKind::Underline => theme.muted,
        DecorationKind::Warning => theme.warning,
        // A kind added after this was written: drawn as quietly as a hint.
        _ => theme.muted,
    }
}

/// How text under a decoration of `kind` is underlined under `theme`: an
/// undercurl for a diagnostic, a dotted line for a hint, a plain line in
/// the text's color for an underline.
fn underline_of(kind: DecorationKind, theme: &Theme) -> (UnderlineStyle, CellColor) {
    let color = CellColor::Rgb(decoration_color(kind, theme));
    match kind {
        DecorationKind::Error | DecorationKind::Info | DecorationKind::Warning => {
            (UnderlineStyle::Curly, color)
        }
        // Cherry paints a highlight as a style, not an underline.
        DecorationKind::Highlight => (UnderlineStyle::None, CellColor::Default),
        DecorationKind::Hint => (UnderlineStyle::Dotted, color),
        DecorationKind::Underline => (UnderlineStyle::Single, CellColor::Default),
        _ => (UnderlineStyle::Single, CellColor::Default),
    }
}

/// The cell style Cherry's `style` is drawn in under `theme`.
fn style_of(style: cherry_engine::Style, theme: &Theme) -> misty::CellStyle {
    match style {
        // A wash of the host's own takes its color from the host's styles;
        // without one, the raised color.
        cherry_engine::Style::Background(_) => misty::CellStyle {
            background: CellColor::Rgb(theme.raised),
            ..misty::CellStyle::default()
        },
        cherry_engine::Style::CursorLine => misty::CellStyle {
            background: CellColor::Rgb(theme.cursor_line),
            ..misty::CellStyle::default()
        },
        // Text a decoration highlights (the other uses of a name) is
        // outlined rather than filled (see `mark_kind`).
        cherry_engine::Style::Highlight => misty::CellStyle::default(),
        cherry_engine::Style::CursorLineNumber => misty::CellStyle::default(),
        // An extra cursor as the main one is drawn: a block in the cursor
        // color over the text.
        cherry_engine::Style::ExtraCursor => misty::CellStyle {
            background: CellColor::Rgb(theme.cursor),
            foreground: CellColor::Rgb(theme.background),
            ..misty::CellStyle::default()
        },
        // Text that is not the document's own reads apart from it: muted
        // and slanted, unless its highlight colors it.
        cherry_engine::Style::Inlay => misty::CellStyle {
            attributes: Attributes::ITALIC,
            foreground: CellColor::Rgb(theme.muted),
            ..misty::CellStyle::default()
        },
        cherry_engine::Style::LineNumber => misty::CellStyle {
            foreground: CellColor::Rgb(theme.muted),
            ..misty::CellStyle::default()
        },
        cherry_engine::Style::MatchingBracket => misty::CellStyle {
            attributes: Attributes::BOLD,
            background: CellColor::Rgb(theme.border),
            ..misty::CellStyle::default()
        },
        cherry_engine::Style::Preedit => misty::CellStyle {
            underline: UnderlineStyle::Single,
            ..misty::CellStyle::default()
        },
        // Each phrase on a palette color of its own, cycling, its text dark.
        cherry_engine::Style::Phrase(number) => misty::CellStyle {
            background: CellColor::Rgb(
                theme.palette.colors[PHRASE_COLORS[usize::from(number) % PHRASE_COLORS.len()]],
            ),
            foreground: CellColor::Rgb(theme.background),
            ..misty::CellStyle::default()
        },
        // Unlike a selection, a match stands out wherever it is: dark text
        // on the warning color.
        cherry_engine::Style::SearchMatch => misty::CellStyle {
            background: CellColor::Rgb(theme.warning),
            foreground: CellColor::Rgb(theme.background),
            ..misty::CellStyle::default()
        },
        cherry_engine::Style::Selection => misty::CellStyle {
            background: CellColor::Rgb(theme.selection),
            ..misty::CellStyle::default()
        },
        cherry_engine::Style::Sign(kind) => misty::CellStyle {
            foreground: CellColor::Rgb(decoration_color(kind, theme)),
            ..misty::CellStyle::default()
        },
        cherry_engine::Style::Text => misty::CellStyle::default(),
        // A style added after this was written: drawn as plain text.
        _ => misty::CellStyle::default(),
    }
}
