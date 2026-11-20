//! The view's scrollbar, drawn and driven as a terminal's is: a thin thumb
//! on a faint track down the view's right edge, shown while the text's
//! screen rows overflow the view, at least [`MIN_THUMB`] tall, lit in the
//! theme's hover color while the pointer is over it or drags it, and dimmed
//! toward the background while the view is out of focus. Where the theme
//! shows scrollbars only around scrolling ([`ScrollbarShow::Auto`]) it
//! shows when the view scrolls and while the pointer is over it or drags
//! it, then fades out (see [`ScrollbarFade`]). A press on the
//! thumb takes hold of it where it was pressed; a press on the track brings
//! the thumb's middle to the pointer at once; then the drag scrolls the
//! view a screen row at a time, through the engine's rows.

use cherry_engine::ViewRows;
use std::time::Instant;

use misty::{Frame, Input, Point, PointerButton, Rect, Rgb, ScrollbarFade, ScrollbarShow, Theme};

// ========================================================================
// Constants
// ========================================================================

/// How wide the strip the scrollbar takes at the view's right is, the part
/// the pointer hits, in logical pixels.
pub(super) const STRIP_WIDTH: f32 = 10.0;

/// How wide the thumb and its track are drawn, at the strip's right, in
/// logical pixels.
const BAR_WIDTH: f32 = 5.0;

/// The shortest the thumb gets, however long the text, in logical pixels.
const MIN_THUMB: f32 = 20.0;

/// How much of the border color the track is mixed with the background.
const TRACK_BORDER_SHARE: f32 = 0.3;

/// How far an unfocused view's thumb is dimmed toward the background.
const UNFOCUSED_DIM: f32 = 0.5;

// ========================================================================
// Data Structures
// ========================================================================

/// The scrollbar's state between frames.
#[derive(Debug, Default)]
pub(super) struct Scrollbar {
    /// When it shows while the theme shows scrollbars only around
    /// scrolling.
    fade: ScrollbarFade,
    /// How far down the thumb the pointer took hold of it, while it drags.
    grab: Option<f32>,
    /// Whether the pointer is over the strip.
    hovered: bool,
    /// Where it was laid out last, while it shows.
    layout: Option<BarLayout>,
    /// When scrollbars show, as the theme last drawn in said.
    show: ScrollbarShow,
}

/// Where the scrollbar was laid out, and the rows it was laid out for.
#[derive(Clone, Copy, Debug, PartialEq)]
struct BarLayout {
    /// The rows of the text and of the view.
    rows: ViewRows,
    /// The strip the pointer hits, the track's height.
    strip: Rect,
    /// How tall the thumb is.
    thumb_height: f32,
}

// ========================================================================
// Scrollbar Implementations
// ========================================================================

impl Scrollbar {
    /// Lays the scrollbar out in `strip` for `rows` and draws it in
    /// `theme`'s colors, dimmed unless `focused`; nothing while the rows
    /// fit the view.
    pub(super) fn draw(
        &mut self,
        frame: &mut Frame,
        strip: Rect,
        rows: Option<ViewRows>,
        theme: &Theme,
        focused: bool,
        now: Instant,
    ) {
        let top = self.layout.map(|layout| layout.rows.top);
        if rows.is_some_and(|rows| top.is_some_and(|top| top != rows.top)) {
            self.fade.wake(now);
        }
        self.show = theme.scrollbar_show;
        self.layout = rows.filter(|rows| rows.total > rows.shown).map(|rows| {
            let scale = frame.scale_factor() as f32;
            let share = rows.shown as f32 / rows.total as f32;
            let thumb_height = (strip.height * share)
                .max(MIN_THUMB * scale)
                .min(strip.height);
            BarLayout {
                rows,
                strip,
                thumb_height,
            }
        });
        let Some(layout) = self.layout else {
            return;
        };
        let lit = self.is_lit();
        let opacity = match (lit, self.show) {
            (true, ScrollbarShow::Auto) => 1.0,
            _ => self.fade.opacity(self.show, now),
        };
        if opacity <= 0.0 {
            return;
        }
        let faded = |color: Rgb| color.mix(theme.background, 1.0 - opacity);
        let scale = frame.scale_factor() as f32;
        let width = (BAR_WIDTH * scale).round();
        let x = (strip.x + strip.width - width).round();
        let track = theme.background.mix(theme.border, TRACK_BORDER_SHARE);
        frame.fill_rect(Rect::new(x, strip.y, width, strip.height), faded(track));
        let thumb: Rgb = match (lit, focused) {
            (true, _) => theme.scrollbar_hover,
            (false, true) => theme.scrollbar,
            (false, false) => theme.scrollbar.mix(theme.background, UNFOCUSED_DIM),
        };
        let top = layout.thumb_top();
        frame.fill_rect(Rect::new(x, top, width, layout.thumb_height), faded(thumb));
    }

    /// When its next frame is due at `now`, while it fades; `None` once it
    /// is still.
    pub(super) fn next_frame(&self, now: Instant) -> Option<Instant> {
        self.fade.next_frame(self.show, now)
    }

    /// Reacts to the pointer `input`: a press on the strip takes hold of
    /// the thumb (or brings it to the pointer), a move drags it, a release
    /// lets it go. Returns whether the scrollbar took the input, and the
    /// screen row the view's top is to go to, if the thumb moved.
    pub(super) fn handle(&mut self, input: &Input) -> (bool, Option<usize>) {
        match input {
            Input::PointerButton(button) if button.button == PointerButton::Left => {
                if !button.pressed {
                    return (self.grab.take().is_some(), None);
                }
                let Some(layout) = self
                    .layout
                    .filter(|layout| layout.strip.contains(button.position))
                else {
                    return (false, None);
                };
                let top = layout.thumb_top();
                let y = button.position.y;
                let on_thumb = (top..=top + layout.thumb_height).contains(&y);
                let grab = match on_thumb {
                    true => y - top,
                    false => layout.thumb_height / 2.0,
                };
                self.grab = Some(grab);
                let row = (!on_thumb).then(|| layout.row_at(y - grab));
                (true, row)
            }
            Input::PointerMoved(point) => {
                let hovered = self.is_over(*point);
                if hovered != self.hovered {
                    // Over it, it shows; left, it holds a moment, then fades.
                    self.fade.wake(Instant::now());
                }
                self.hovered = hovered;
                match (self.grab, self.layout) {
                    (Some(grab), Some(layout)) => (true, Some(layout.row_at(point.y - grab))),
                    _ => (false, None),
                }
            }
            Input::PointerLeft => {
                if self.hovered {
                    self.fade.wake(Instant::now());
                }
                self.hovered = false;
                (false, None)
            }
            Input::FocusLost => {
                self.grab = None;
                (false, None)
            }
            _ => (false, None),
        }
    }

    /// Whether the thumb is lit: the pointer over the strip, or dragging.
    pub(super) fn is_lit(&self) -> bool {
        self.hovered || self.grab.is_some()
    }

    /// Whether `point` is over the strip, while the scrollbar shows.
    fn is_over(&self, point: Point) -> bool {
        self.layout
            .is_some_and(|layout| layout.strip.contains(point))
    }
}

impl BarLayout {
    /// How far the thumb can travel down the track.
    fn travel(&self) -> f32 {
        (self.strip.height - self.thumb_height).max(0.0)
    }

    /// The most rows the view's top can be down the text.
    fn max_top(&self) -> usize {
        self.rows.total.saturating_sub(self.rows.shown)
    }

    /// Where the thumb's top is, for the view's top row.
    fn thumb_top(&self) -> f32 {
        let share = match self.max_top() {
            0 => 0.0,
            max => self.rows.top.min(max) as f32 / max as f32,
        };
        self.strip.y + share * self.travel()
    }

    /// The row the view's top goes to with the thumb's top at `top`.
    fn row_at(&self, top: f32) -> usize {
        let travel = self.travel();
        let share = match travel > 0.0 {
            true => ((top - self.strip.y) / travel).clamp(0.0, 1.0),
            false => 0.0,
        };
        (share * self.max_top() as f32).round() as usize
    }
}

// ========================================================================
// Tests
// ========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn layout(top: usize) -> BarLayout {
        BarLayout {
            rows: ViewRows {
                shown: 10,
                top,
                total: 110,
            },
            strip: Rect::new(0.0, 0.0, 10.0, 200.0),
            thumb_height: 20.0,
        }
    }

    #[test]
    fn test_the_thumb_runs_the_track_as_the_top_runs_the_text_and_back() {
        assert_eq!(layout(0).thumb_top(), 0.0);
        assert_eq!(layout(100).thumb_top(), 180.0, "the last row at the bottom");
        assert_eq!(layout(50).thumb_top(), 90.0);
        let middle = layout(0);
        assert_eq!(middle.row_at(90.0), 50);
        assert_eq!(middle.row_at(-5.0), 0, "held to the track");
        assert_eq!(middle.row_at(500.0), 100);
    }
}
