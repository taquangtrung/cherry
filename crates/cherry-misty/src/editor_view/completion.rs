//! The completion popup of an [`EditorView`](crate::EditorView): Cherry's
//! open completion menu drawn as a raised menu under the text being
//! completed, a deeper shadow than a tooltip's lifting it off the text.
//! Each row, as roomy as a menu's, shows the match's kind as a letter on a
//! chip tinted its color, its label with the typed characters marked in the
//! accent color, and its detail muted at the row's right end, each cut with
//! an ellipsis past its width; the selected row is filled and lined with a
//! thin accent border, and a thumb at the right says how far down the list
//! is while more matches than fit. Beside the popup, a card holds the
//! selected match's signature over a line, its documentation, and where
//! the match came from.

use cherry_engine::{Completion, CompletionItem, CompletionKind};
use misty::{
    Border, BoxStyle, CellColor, Frame, Paragraph, Point, Popup, PopupSide, Rect, Rgb, Rgba, Span,
    StyledText, TextColor, Theme, ThemeToken, fuzzy_highlight, fuzzy_score, text_width,
};

// ========================================================================
// Constants
// ========================================================================

/// The most matches the popup shows at once; it scrolls to keep the
/// selected one in sight.
const MAX_ROWS: usize = 10;

/// The fewest cells of text a row is wide.
const MIN_TEXT_CELLS: usize = 12;

/// The most cells a label takes before it is cut.
const MAX_LABEL_CELLS: usize = 36;

/// The most cells a detail takes before it is cut.
const MAX_DETAIL_CELLS: usize = 30;

/// The cells between a match's label and its detail.
const DETAIL_GAP_CELLS: usize = 2;

/// The cells a kind's chip takes, the gap after it included.
const KIND_CELLS: usize = 3;

/// The chip's side as a share of the text's line height.
const CHIP_SHARE: f32 = 0.9;

/// How much of a kind's color tints its chip over the popup's fill.
const CHIP_TINT: f32 = 0.22;

/// How many times a tooltip's shadow the popup casts, so it reads as a
/// menu standing over the text rather than a hint beside it.
const SHADOW_DEPTH: f32 = 2.0;

/// The scroll thumb's width, in logical pixels.
const THUMB_WIDTH: f32 = 3.0;

/// The most cells wide a documentation card's text is.
const MAX_DOC_CELLS: usize = 52;

/// The fewest cells wide a documentation card beside the popup is; with
/// less room on both sides, it goes under the popup instead.
const MIN_DOC_CELLS: usize = 24;

/// The most rows tall a documentation card's text is; longer text is cut.
const MAX_DOC_ROWS: usize = 14;

/// The most rows tall the signature heading a documentation card is.
const MAX_SIGNATURE_ROWS: usize = 3;

/// The palette's named colors a kind's chip is tinted with.
const RED: u8 = 1;
const GREEN: u8 = 2;
const YELLOW: u8 = 3;
const BLUE: u8 = 4;
const MAGENTA: u8 = 5;
const CYAN: u8 = 6;

// ========================================================================
// Data Structures
// ========================================================================

/// How a popup's rows are laid out across, in cells.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Columns {
    /// The cells of the detail column; 0 when no match has a detail.
    detail: usize,
    /// The cells of the kind column; 0 when no match names its kind.
    kind: usize,
    /// The cells of the label column.
    label: usize,
}

// ========================================================================
// Columns Implementations
// ========================================================================

impl Columns {
    /// The columns fitting `items`, each as wide as its widest entry up to
    /// its cap, all of them together at least [`MIN_TEXT_CELLS`].
    fn of(items: &[CompletionItem]) -> Self {
        let widest = |cells: fn(&CompletionItem) -> usize, cap: usize| {
            items.iter().map(cells).max().unwrap_or(0).min(cap)
        };
        let kind = match items.iter().any(|item| item.kind.is_some()) {
            true => KIND_CELLS,
            false => 0,
        };
        let label = widest(|item| text_width(&item.label), MAX_LABEL_CELLS);
        let detail = widest(
            |item| item.detail.as_deref().map_or(0, text_width),
            MAX_DETAIL_CELLS,
        );
        let label = label.max(MIN_TEXT_CELLS.saturating_sub(kind + detail));
        Self {
            detail,
            kind,
            label,
        }
    }

    /// The cells of a whole row's text.
    fn width(self) -> usize {
        let detail = match self.detail {
            0 => 0,
            cells => DETAIL_GAP_CELLS + cells,
        };
        self.kind + self.label + detail
    }
}

// ========================================================================
// Functions
// ========================================================================

/// Draws `completion` in the frame's overlay below `anchor` (the cell
/// where the completed text starts, in physical pixels), or above it when
/// there is no room below, in `theme`'s colors, the selected match's
/// documentation in a card beside it.
pub(crate) fn draw_completion(
    frame: &mut Frame,
    completion: &Completion,
    anchor: Rect,
    theme: &Theme,
) {
    let items = completion.items();
    if items.is_empty() {
        return;
    }
    let metrics = frame.cell_metrics();
    let scale_factor = frame.scale_factor();
    let inset = Popup::inset(&theme.shape, scale_factor);
    let padding = metrics.width.round();
    // A menu's room around the text, so a row is as easy to aim at.
    let row_padding = (f32::from(theme.shape.row_padding) * scale_factor as f32).round();
    let row_height = metrics.height + 2.0 * row_padding;
    let columns = Columns::of(items);
    let rows = items.len().min(MAX_ROWS);
    let width = columns.width() as f32 * metrics.width + 2.0 * (padding + inset);
    let height = rows as f32 * row_height + 2.0 * inset;
    let popup = Popup::new(anchor, width, height, PopupSide::Below, frame.bounds());
    let style = menu_box(theme, scale_factor);
    frame.overlay(|frame| frame.draw_box(popup.rect(), &style));
    let content = popup.content(&theme.shape, scale_factor);
    let first = first_shown(completion.selected(), items.len(), rows);
    frame.overlay(|frame| {
        for (slot, (index, item)) in items.iter().enumerate().skip(first).take(rows).enumerate() {
            let row = Rect::new(
                content.x,
                content.y + slot as f32 * row_height,
                content.width,
                row_height,
            );
            if completion.selected() == Some(index) {
                draw_selection(frame, row, theme);
            }
            let origin = Point::new(row.x + padding, row.y + row_padding);
            draw_row(frame, item, completion.typed(), columns, origin, row, theme);
        }
        if items.len() > rows {
            draw_thumb(frame, content, first, rows, items.len(), theme);
        }
    });
    let selected = completion.selected().and_then(|index| items.get(index));
    if let Some(item) = selected {
        // The card keeps clear of the text being completed as of the list.
        let list = popup.rect();
        let top = list.y.min(anchor.y);
        let bottom = (list.y + list.height).max(anchor.y + anchor.height);
        let clear = Rect::new(list.x, top, list.width, bottom - top);
        draw_documentation(frame, item, clear, theme);
    }
}

/// The box the popup and its card are drawn in: a floating box of the
/// raised color, its shadow [`SHADOW_DEPTH`] times a tooltip's.
fn menu_box(theme: &Theme, scale_factor: f64) -> BoxStyle {
    let mut style = theme
        .shape
        .floating_box(theme.raised, theme.border, scale_factor);
    if let Some(shadow) = &mut style.shadow {
        shadow.blur *= SHADOW_DEPTH;
        shadow.offset = Point::new(
            shadow.offset.x * SHADOW_DEPTH,
            shadow.offset.y * SHADOW_DEPTH,
        );
    }
    style
}

/// Draws the scroll thumb at the right of `content`: as tall a share of it
/// as `rows` are of `count` matches, as far down as `first` is.
fn draw_thumb(
    frame: &mut Frame,
    content: Rect,
    first: usize,
    rows: usize,
    count: usize,
    theme: &Theme,
) {
    let scale_factor = frame.scale_factor() as f32;
    let width = (THUMB_WIDTH * scale_factor).round().max(1.0);
    let (height, top) = thumb_span(content.height, first, rows, count);
    let thumb = Rect::new(
        content.x + content.width - width,
        content.y + top,
        width,
        height,
    );
    let style = BoxStyle {
        border: None,
        corner_radius: width / 2.0,
        fill: Rgba::from(theme.muted),
        shadow: None,
    };
    frame.draw_box(thumb, &style);
}

/// Fills `row` as the selected one, lined with a thin accent border.
fn draw_selection(frame: &mut Frame, row: Rect, theme: &Theme) {
    let scale_factor = frame.scale_factor();
    let band = theme.shape.highlight_band(row, scale_factor);
    let mut style = theme.shape.highlight(theme.selected_row, scale_factor);
    style.border = Some(Border {
        color: Rgba::from(theme.accent),
        width: (scale_factor as f32).round().max(1.0),
    });
    frame.draw_box(band, &style);
}

/// Draws `item`'s kind chip, label, and detail across `columns` from
/// `origin`, clipped to `row`, the characters of its label `typed` matched
/// marked and its detail at the row's right end.
fn draw_row(
    frame: &mut Frame,
    item: &CompletionItem,
    typed: &str,
    columns: Columns,
    origin: Point,
    row: Rect,
    theme: &Theme,
) {
    let metrics = frame.cell_metrics();
    let cell = metrics.width;
    if let Some((letter, tone)) = item.kind.map(kind_mark) {
        let (color, rgb) = tone_colors(tone, theme);
        let side = (metrics.height * CHIP_SHARE).round();
        let chip = Rect::new(
            origin.x,
            (row.y + (row.height - side) / 2.0).round(),
            side,
            side,
        );
        let style = BoxStyle {
            border: None,
            corner_radius: f32::from(theme.shape.control_radius) * frame.scale_factor() as f32,
            fill: Rgba::from(theme.raised.mix(rgb, CHIP_TINT)),
            shadow: None,
        };
        frame.draw_box(chip, &style);
        let mark = StyledText::from(Span::new(letter.to_string(), color).bold());
        let letter_x = chip.x + ((side - mark.width() as f32 * cell) / 2.0).round();
        frame.draw_text(&mark, Point::new(letter_x, origin.y), row, theme);
    }
    let label_x = origin.x + columns.kind as f32 * cell;
    let label = marked_label(&item.label, typed)
        .with_ellipsis()
        .fit(columns.label);
    frame.draw_text(&label, Point::new(label_x, origin.y), row, theme);
    if let Some(detail) = &item.detail {
        let detail = muted_cut(detail, columns.detail);
        let end = origin.x + columns.width() as f32 * cell;
        let detail_x = end - detail.width() as f32 * cell;
        frame.draw_text(&detail, Point::new(detail_x, origin.y), row, theme);
    }
}

/// `label` with the characters `typed` matches in it bold in the accent
/// color, the rest in the foreground.
fn marked_label(label: &str, typed: &str) -> StyledText {
    let positions = fuzzy_score(typed, label).map_or_else(Vec::new, |found| found.positions);
    fuzzy_highlight(
        label,
        &positions,
        ThemeToken::Foreground.into(),
        ThemeToken::Accent.into(),
    )
}

/// The letter a kind of match is marked with, and the palette color its
/// chip is tinted with (`None` for the muted color): a palette color, so a
/// theme change recolors it.
fn kind_mark(kind: CompletionKind) -> (char, Option<u8>) {
    match kind {
        CompletionKind::Class => ('c', Some(YELLOW)),
        CompletionKind::Constant => ('#', Some(RED)),
        CompletionKind::Enum => ('e', Some(YELLOW)),
        CompletionKind::Field => ('p', Some(CYAN)),
        CompletionKind::File => ('/', None),
        CompletionKind::Function => ('f', Some(MAGENTA)),
        CompletionKind::Keyword => ('k', Some(BLUE)),
        CompletionKind::Method => ('m', Some(MAGENTA)),
        CompletionKind::Module => ('n', Some(BLUE)),
        CompletionKind::Snippet => ('s', Some(GREEN)),
        CompletionKind::Text => ('w', None),
        CompletionKind::Type => ('t', Some(YELLOW)),
        CompletionKind::Variable => ('v', Some(CYAN)),
        _ => ('?', None),
    }
}

/// The color a kind's letter is drawn in and the one its chip is tinted
/// with, for palette color `tone` (`None` for the muted color) in `theme`.
fn tone_colors(tone: Option<u8>, theme: &Theme) -> (TextColor, Rgb) {
    match tone {
        Some(index) => (
            TextColor::Palette(CellColor::Indexed(index)),
            theme.palette.indexed(index),
        ),
        None => (ThemeToken::Muted.into(), theme.muted),
    }
}

/// Draws `item`'s documentation in a card by `list` (the popup's box and
/// the text it completes), never over it: its signature (the match's
/// detail) over a thin line, the documentation, and under it where the
/// match came from. Nothing when the match has no documentation or no room
/// for a row of it.
fn draw_documentation(frame: &mut Frame, item: &CompletionItem, list: Rect, theme: &Theme) {
    let Some(documentation) = item.documentation.as_deref().map(str::trim) else {
        return;
    };
    if documentation.is_empty() {
        return;
    }
    let metrics = frame.cell_metrics();
    let scale_factor = frame.scale_factor();
    let inset = Popup::inset(&theme.shape, scale_factor);
    let padding = metrics.width.round();
    let frame_width = 2.0 * (padding + inset);
    let paragraph = Paragraph::plain(documentation, ThemeToken::Foreground.into());
    let signature = item
        .detail
        .as_deref()
        .map(str::trim)
        .filter(|detail| !detail.is_empty())
        .map(|detail| Paragraph::plain(detail, ThemeToken::Foreground.into()));
    let max_width = MAX_DOC_CELLS as f32 * metrics.width;
    let widest = |paragraph: &Paragraph, frame: &mut Frame| {
        frame.paragraph_width(paragraph).ceil().min(max_width)
    };
    let mut text_width = widest(&paragraph, frame);
    if let Some(signature) = &signature {
        text_width = text_width.max(widest(signature, frame));
    }
    let source = item
        .source
        .as_deref()
        .map(|source| muted_cut(source, MAX_DOC_CELLS));
    let source_width = source
        .as_ref()
        .map_or(0.0, |source| source.width() as f32 * metrics.width);
    let wanted = text_width.max(source_width) + frame_width;
    let narrowest = MIN_DOC_CELLS as f32 * metrics.width + frame_width;
    let bounds = frame.bounds();
    let (beside, width) = card_width(list, bounds, wanted, narrowest);
    let text_width = width - frame_width;
    // The signature takes at most a few rows, and a gap with the line in it.
    let heading = signature.as_ref().map_or(0.0, |signature| {
        let rows = frame.measure_paragraph(signature, text_width);
        rows.min(MAX_SIGNATURE_ROWS as f32 * metrics.height) + metrics.height
    });
    let max_height = MAX_DOC_ROWS as f32 * metrics.height;
    let text_height = frame
        .measure_paragraph(&paragraph, text_width)
        .min(max_height);
    let footer = match source {
        Some(_) => metrics.height * 1.5,
        None => 0.0,
    };
    let edge = 2.0 * (padding / 2.0 + inset);
    let wanted_height = heading + text_height + footer + edge;
    let rect = card_rect(list, bounds, width, wanted_height, beside);
    let text_height = text_height.min(rect.height - heading - footer - edge);
    if text_height < metrics.height {
        return;
    }
    let style = menu_box(theme, scale_factor);
    frame.overlay(|frame| frame.draw_box(rect, &style));
    let content = Popup::with_rect(rect).content(&theme.shape, scale_factor);
    let top = content.y + padding / 2.0;
    let text = Rect::new(content.x + padding, top + heading, text_width, text_height);
    frame.overlay(|frame| {
        if let Some(signature) = &signature {
            let room = Rect::new(text.x, top, text_width, heading - metrics.height);
            frame.clipped(room, |frame| {
                frame.draw_paragraph(signature, room, theme);
            });
            let stroke = (scale_factor as f32).round().max(1.0);
            let y = (top + heading - metrics.height / 2.0).round();
            let line = Rect::new(content.x, y, content.width, stroke);
            frame.fill_rect_over_text(line, theme.border);
        }
        frame.draw_paragraph(&paragraph, text, theme);
        if let Some(source) = &source {
            let y = text.y + text_height + metrics.height / 2.0;
            frame.draw_text(source, Point::new(text.x, y), content, theme);
        }
    });
}

/// Whether a documentation card `wanted` pixels wide goes beside `list`,
/// the popup's box, inside `bounds`, and how wide it is: beside it,
/// narrowed to the room on the roomier side down to `narrowest`, when a
/// side has that room; else at the width it wants, cut to `bounds`.
fn card_width(list: Rect, bounds: Rect, wanted: f32, narrowest: f32) -> (bool, f32) {
    let right = bounds.x + bounds.width - (list.x + list.width);
    let left = list.x - bounds.x;
    let room = right.max(left);
    match room >= wanted.min(narrowest) {
        true => (true, wanted.min(room)),
        false => (false, wanted.min(bounds.width)),
    }
}

/// Where a documentation card `width` by `height` goes by `list` inside
/// `bounds`, never over the list: `beside` it, on the right where it fits
/// and else on the left, its top at the list's as far as `bounds` allows;
/// otherwise under it or over it, whichever has more room, cut to that
/// room.
fn card_rect(list: Rect, bounds: Rect, width: f32, height: f32, beside: bool) -> Rect {
    let bottom = bounds.y + bounds.height;
    let right = bounds.x + bounds.width;
    if beside {
        let x = match list.x + list.width + width <= right {
            true => list.x + list.width,
            false => list.x - width,
        };
        let height = height.min(bounds.height);
        let y = list.y.min(bottom - height).max(bounds.y);
        return Rect::new(x, y, width, height);
    }
    let x = list.x.min(right - width).max(bounds.x);
    let below = bottom - (list.y + list.height);
    let above = list.y - bounds.y;
    match below >= above {
        true => Rect::new(x, list.y + list.height, width, height.min(below)),
        false => {
            let height = height.min(above);
            Rect::new(x, list.y - height, width, height)
        }
    }
}

/// `text` muted, cut with an ellipsis past `cells` cells.
fn muted_cut(text: &str, cells: usize) -> StyledText {
    StyledText::plain(text, ThemeToken::Muted.into())
        .with_ellipsis()
        .fit(cells)
}

/// How tall the scroll thumb is down a track `track` pixels tall, and how
/// far down it, when `rows` of `count` matches show from `first`: never
/// shorter than a sixth of the track, so it stays easy to see.
fn thumb_span(track: f32, first: usize, rows: usize, count: usize) -> (f32, f32) {
    let count = count.max(1) as f32;
    let height = (track * rows as f32 / count).max(track / 6.0).min(track);
    let hidden = (count - rows as f32).max(1.0);
    let top = (track - height) * first as f32 / hidden;
    (height.round(), top.round())
}

/// The index of the first match shown when `rows` of `count` matches fit
/// and `selected` is typed: from the top until the selected one would
/// fall below the last row, then with it on the last row.
fn first_shown(selected: Option<usize>, count: usize, rows: usize) -> usize {
    let last_first = count.saturating_sub(rows);
    selected
        .map_or(0, |selected| (selected + 1).saturating_sub(rows))
        .min(last_first)
}

// ========================================================================
// Tests
// ========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_the_shown_rows_scroll_only_to_keep_the_selected_match_in_sight() {
        assert_eq!(first_shown(None, 20, 8), 0);
        assert_eq!(first_shown(Some(7), 20, 8), 0, "the last row");
        assert_eq!(first_shown(Some(8), 20, 8), 1);
        assert_eq!(first_shown(Some(19), 20, 8), 12);
        assert_eq!(first_shown(Some(2), 3, 8), 0, "all fit");
    }

    #[test]
    fn test_the_thumb_is_as_tall_as_the_share_shown_and_as_far_down_as_the_list() {
        assert_eq!(thumb_span(100.0, 0, 10, 20), (50.0, 0.0), "the top half");
        assert_eq!(thumb_span(100.0, 10, 10, 20), (50.0, 50.0), "at the end");
        assert_eq!(
            thumb_span(120.0, 0, 10, 1000),
            (20.0, 0.0),
            "never shorter than a sixth of the track"
        );
    }

    #[test]
    fn test_the_columns_cap_long_labels_and_details_and_drop_empty_ones() {
        let long = "x".repeat(MAX_LABEL_CELLS + 10);
        let items = [
            CompletionItem::new(long.as_str()).with_detail("y".repeat(MAX_DETAIL_CELLS + 5)),
            CompletionItem::new("ab").with_kind(CompletionKind::Function),
        ];
        let columns = Columns::of(&items);
        assert_eq!(
            columns,
            Columns {
                detail: MAX_DETAIL_CELLS,
                kind: KIND_CELLS,
                label: MAX_LABEL_CELLS,
            }
        );
        let words = [CompletionItem::new("ab")];
        assert_eq!(
            Columns::of(&words),
            Columns {
                detail: 0,
                kind: 0,
                label: MIN_TEXT_CELLS,
            },
            "plain words take no kind or detail column, and a row stays readable"
        );
    }

    #[test]
    fn test_a_documentation_card_goes_beside_the_list_with_room_else_under_it() {
        let bounds = Rect::new(0.0, 0.0, 1000.0, 600.0);
        let at = |x: f32| Rect::new(x, 100.0, 200.0, 150.0);
        assert_eq!(card_width(at(100.0), bounds, 400.0, 150.0), (true, 400.0));
        assert_eq!(
            card_width(at(350.0), bounds, 600.0, 150.0),
            (true, 450.0),
            "narrowed to the roomier side"
        );
        let narrow = Rect::new(0.0, 0.0, 500.0, 600.0);
        assert_eq!(card_width(at(150.0), narrow, 400.0, 200.0), (false, 400.0));
        assert_eq!(
            card_rect(at(500.0), bounds, 400.0, 300.0, true),
            Rect::new(100.0, 100.0, 400.0, 300.0),
            "on the left where only there it fits"
        );
        assert_eq!(
            card_rect(at(100.0), bounds, 400.0, 560.0, true),
            Rect::new(300.0, 40.0, 400.0, 560.0),
            "lifted to stay inside the window"
        );
        let low = Rect::new(100.0, 400.0, 200.0, 150.0);
        assert_eq!(
            card_rect(low, narrow, 400.0, 500.0, false),
            Rect::new(100.0, 0.0, 400.0, 400.0),
            "over the list, the roomier way, cut to the room"
        );
    }

    #[test]
    fn test_the_typed_characters_of_a_label_are_marked_in_the_accent_color() {
        let label = marked_label("into_iter", "it");
        let marked: Vec<&str> = label
            .spans()
            .iter()
            .filter(|span| span.color == ThemeToken::Accent.into())
            .map(|span| span.text.as_str())
            .collect();
        assert!(!marked.is_empty(), "{label:?}");
        assert_eq!(marked.concat().len(), 2, "two chars marked: {label:?}");
        assert_eq!(
            marked_label("iter", "").width(),
            4,
            "nothing typed, the whole label"
        );
    }
}
