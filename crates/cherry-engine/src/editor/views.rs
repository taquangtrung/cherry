//! A view's size, cursor, and selection, the cells its text is laid
//! out in, scrolling, and the mouse.

use std::ops;

use crate::decoration::LineChange;
use crate::insert;
use crate::mouse::{self, MouseEvent};
use crate::operator::PendingOperator;
use crate::options::{EditorOptions, ViewOptions};
use crate::selection::Selection;
use crate::view::{CursorPlace, Mode, RowPlace, ScreenLines, ViewRows};

use super::{Editor, Handled, ViewId};

// ========================================================================
// Editor Implementations
// ========================================================================

impl Editor {
    /// Acts on the mouse `event` over `view`: a press moves the cursor, a
    /// drag selects, a double or triple click selects a word or a line,
    /// the wheel scrolls (see [`MouseEvent`]). An event for a view the
    /// editor does not have, or while its prompt is open, is
    /// [`Handled::Ignored`].
    pub fn handle_mouse(&mut self, view: ViewId, event: MouseEvent) -> Handled {
        if !self.views.contains_key(view) {
            return Handled::Ignored;
        }
        let handled = mouse::handle_mouse(self, view, event);
        self.scroll_to_cursor(view);
        handled
    }

    /// How far down its text `view` is, in screen rows: what a scrollbar
    /// is drawn from. `None` for a view the editor does not have.
    pub fn view_rows(&self, view: ViewId) -> Option<ViewRows> {
        let state = self.views.get(view)?;
        let options = self.layout_options(view);
        let document = &self.documents[state.document()].document;
        let decorations = &self.documents[state.document()].decorations;
        Some(state.rows_of(document, decorations, &options))
    }

    /// Scrolls `view` so screen row `row` of its text is at its top, or as
    /// near as the end of the text lets it, moving the cursor into sight as
    /// the wheel does: what a scrollbar dragged to that row asks. Does
    /// nothing for a view the editor does not have.
    pub fn scroll_view_to_row(&mut self, view: ViewId, row: usize) {
        let Some(rows) = self.view_rows(view) else {
            return;
        };
        if row == rows.top {
            return;
        }
        let down = row > rows.top;
        if let Some(offset) = self.scroll_view_rows(view, down, row.abs_diff(rows.top)) {
            mouse::place_cursor(self, view, offset);
        }
    }

    /// Makes `view` `cols` cells wide and `rows` high, scrolling to keep
    /// its cursor in sight. A change in the number of rows sets the rows
    /// `C-d` and `C-u` scroll back to half the view, dropping those a
    /// count set, as Vim resets its `scroll` option. Does nothing for a
    /// view the editor lacks.
    pub fn resize_view(&mut self, view: ViewId, cols: u16, rows: u16) {
        let Some(state) = self.views.get_mut(view) else {
            return;
        };
        state.resize(cols, rows);
        self.scroll_to_cursor(view);
    }

    /// Shows `text`, what an input method is composing (Vietnamese Telex,
    /// Pinyin, Kana), at the cursor of `view` without typing it: laid out
    /// in the line before the character under the cursor, painted as
    /// [`Style::Preedit`](crate::Style::Preedit), the cursor after it, and
    /// scrolled into sight. Empty `text` ends the composition, as input
    /// methods send it. It shows only in Insert and Replace mode, where
    /// keys type text. When the input method commits, feed the committed
    /// text to [`Editor::handle_key`] a character at a time: any key ends
    /// the composition. Does nothing for a view the editor lacks.
    pub fn set_preedit(&mut self, view: ViewId, text: &str) {
        let Some(state) = self.views.get_mut(view) else {
            return;
        };
        state.set_preedit(text);
        self.scroll_to_cursor(view);
    }

    /// Makes `offsets` the char offsets of the cursors of `view` besides its
    /// main one, in place of any it had: every key typed in the view then
    /// runs at each of them as at the main cursor (see [`View::cursors`](crate::View::cursors)
    /// and the commands `add-cursor-next-match`, `add-cursor-below`, and
    /// their kin). Offsets are clamped to the text; the main cursor's and
    /// repeats are dropped, and an empty list leaves one cursor. Does
    /// nothing for a view the editor lacks.
    pub fn set_cursors(&mut self, view: ViewId, offsets: Vec<usize>) {
        let Some(state) = self.views.get_mut(view) else {
            return;
        };
        let len = self.documents[state.document()].document.len_chars();
        let offsets = offsets.into_iter().map(|offset| offset.min(len)).collect();
        let cursor = state.cursor();
        state.extra_cursors_mut().set(offsets, cursor);
    }

    /// Moves the cursor of `view` to char `offset`, or to the end of the
    /// text when `offset` is past it, dropping any selection. Does nothing
    /// for a view the editor lacks.
    pub fn set_cursor(&mut self, view: ViewId, offset: usize) {
        let Some(state) = self.views.get_mut(view) else {
            return;
        };
        let len = self.documents[state.document()].document.len_chars();
        state.set_selection(Selection::point(offset.min(len)));
    }

    /// The cell of `view`, gutter included, as `(column, row)`, where char
    /// `offset` of its document shows, for placing a popup such as the
    /// completion menu at its [`anchor`](crate::Completion::anchor); `None`
    /// when it is out of sight or the editor has no such view.
    pub fn screen_cell(&self, view: ViewId, offset: usize) -> Option<(u16, u16)> {
        let state = self.views.get(view)?;
        let document = &self.documents[state.document()].document;
        let decorations = &self.documents[state.document()].decorations;
        let options = self.layout_options(view);
        state.cell_of(
            document,
            decorations,
            &options,
            offset.min(document.len_chars()),
        )
    }

    /// What became of the line whose gutter holds cell `column` of row
    /// `row` of `view`, as the host marked it with
    /// [`Editor::set_line_changes`]: for a tooltip on its change bar; `None`
    /// off the gutter, by an unchanged line, or for a view the editor lacks.
    pub fn line_change_at(&self, view: ViewId, column: u16, row: u16) -> Option<LineChange> {
        let state = self.views.get(view)?;
        let open = &self.documents[state.document()];
        let options = self.layout_options(view);
        let line =
            state.gutter_line_at_cell(&open.document, &open.decorations, &options, column, row)?;
        open.decorations
            .changes_on_lines(&open.document, line..line + 1)
            .into_iter()
            .next()
            .flatten()
    }

    /// What the signs by the line whose gutter holds cell `column` of row
    /// `row` of `view` say in words ([`Sign::with_note`](crate::Sign::with_note)), for a tooltip;
    /// `None` off the gutter, by a line no noted sign marks, or for a view
    /// the editor lacks.
    pub fn sign_note_at(&self, view: ViewId, column: u16, row: u16) -> Option<String> {
        let state = self.views.get(view)?;
        let open = &self.documents[state.document()];
        let options = self.layout_options(view);
        let line =
            state.gutter_line_at_cell(&open.document, &open.decorations, &options, column, row)?;
        open.decorations.sign_note(&open.document, line)
    }

    /// The char offset `view` shows at cell `column` of row `row`, gutter
    /// included, for what the pointer rests on (a hover card, a link):
    /// `None` for a cell of the gutter, past the end of its line, below the
    /// last row, or a view the editor lacks. The inverse of
    /// [`Editor::screen_cell`].
    pub fn offset_at_cell(&self, view: ViewId, column: u16, row: u16) -> Option<usize> {
        let state = self.views.get(view)?;
        let open = &self.documents[state.document()];
        let options = self.layout_options(view);
        state.text_offset_at_cell(&open.document, &open.decorations, &options, column, row)
    }

    /// Selects the chars `range` of the document `view` shows in Visual
    /// mode, the cursor on the last of them, as `v` and a motion would: for
    /// a host's expand-selection, or text a search or a server found. An
    /// empty range puts the cursor at its start in Normal mode instead.
    /// Ends past the document are cut to it; does nothing for a view the
    /// editor lacks.
    pub fn select(&mut self, view: ViewId, range: ops::Range<usize>) {
        let Some(state) = self.views.get(view) else {
            return;
        };
        let document = &self.documents[state.document()].document;
        let len = document.len_chars();
        let (start, end) = (range.start.min(len), range.end.min(len));
        if start >= end {
            self.set_mode(view, Mode::Normal);
            return self.set_cursor(view, start);
        }
        let last = document.prev_grapheme_boundary(end);
        self.set_mode(view, Mode::Visual);
        self.set_visual_anchor(view, start);
        self.set_cursor(view, last);
    }

    /// Makes `options` what `view` shows differently from the editor's
    /// options (wrap, line numbers), and scrolls to keep its cursor in
    /// sight; does nothing for a view the editor lacks.
    pub fn set_view_options(&mut self, view: ViewId, options: ViewOptions) {
        let Some(state) = self.views.get_mut(view) else {
            return;
        };
        state.set_options(options);
        self.scroll_to_cursor(view);
    }

    /// The options `view` is laid out with: the editor's, without a
    /// gutter or a scroll margin for a single-line document, which wraps
    /// only when the view's own options ask for it.
    pub(crate) fn layout_options(&self, view: ViewId) -> EditorOptions {
        let single_line = self.shows_single_line(view);
        let own = self.views[view].options();
        let line_numbers = own.line_numbers.unwrap_or(self.options.line_numbers);
        let wrap = own.wrap.unwrap_or(self.options.wrap);
        EditorOptions {
            line_numbers: line_numbers && !single_line,
            relative_line_numbers: self.options.relative_line_numbers && !single_line,
            scroll_margin: if single_line {
                0
            } else {
                self.options.scroll_margin
            },
            sign_column: self.options.sign_column && !single_line,
            wrap: match single_line {
                true => own.wrap == Some(true),
                false => wrap,
            },
            ..self.options
        }
    }

    /// Scrolls `view` `count` screen rows down, or up when not `down`, and
    /// returns where its cursor must go to stay in sight, if anywhere.
    pub(crate) fn scroll_view_rows(
        &mut self,
        view: ViewId,
        down: bool,
        count: usize,
    ) -> Option<usize> {
        let options = self.layout_options(view);
        let state = &mut self.views[view];
        let document = &self.documents[state.document()].document;
        let decorations = &self.documents[state.document()].decorations;
        state.scroll_rows(document, decorations, &options, down, count)
    }

    /// The char offset `count` screen rows below `offset` in `view` (above
    /// when not `down`), `None` when it cannot move.
    pub(crate) fn offset_rows_away(
        &self,
        view: ViewId,
        offset: usize,
        down: bool,
        count: usize,
    ) -> Option<usize> {
        let state = &self.views[view];
        let document = &self.documents[state.document()].document;
        let decorations = &self.documents[state.document()].decorations;
        state.offset_rows_away(
            document,
            decorations,
            &self.layout_options(view),
            offset,
            down,
            count,
        )
    }

    /// The char offset shown at `place` of the screen row of `view` char
    /// `offset` is on.
    pub(crate) fn offset_at_row_place(
        &self,
        view: ViewId,
        offset: usize,
        place: RowPlace,
    ) -> usize {
        let state = &self.views[view];
        let document = &self.documents[state.document()].document;
        let decorations = &self.documents[state.document()].decorations;
        state.offset_at_row_place(
            document,
            decorations,
            &self.layout_options(view),
            offset,
            place,
        )
    }

    /// The lines `view` shows, a row at a time.
    pub(crate) fn screen_lines(&self, view: ViewId) -> ScreenLines {
        let state = &self.views[view];
        let document = &self.documents[state.document()].document;
        let decorations = &self.documents[state.document()].decorations;
        state.screen_lines(document, decorations, &self.layout_options(view))
    }

    /// Scrolls `view` so its cursor's row is at `place`.
    pub(crate) fn scroll_cursor_to(&mut self, view: ViewId, place: CursorPlace) {
        let options = self.layout_options(view);
        let state = &mut self.views[view];
        let document = &self.documents[state.document()].document;
        let decorations = &self.documents[state.document()].decorations;
        state.scroll_cursor_to(document, decorations, &options, place);
    }

    /// Sets the display column up and down motions in `view` aim for.
    pub(crate) fn set_goal_column(&mut self, view: ViewId, goal_column: usize) {
        self.views[view].set_goal_column(goal_column);
    }

    /// Switches `view` to `mode`; Insert and Replace mode in a read-only
    /// document are refused, closing an undo group opened for them, back
    /// in Normal mode. A view that never leaves Insert mode
    /// ([`Editor::is_insert_locked`]) goes to Insert mode whatever `mode`
    /// is, starting an insert session when the last one ended.
    pub(crate) fn set_mode(&mut self, view: ViewId, mode: Mode) {
        if self.is_insert_locked(view) {
            self.views[view].set_mode(Mode::Insert);
            self.history_mut(view).begin_group();
            return insert::mark_insert_start(self, view);
        }
        let edits_text = matches!(mode, Mode::Insert | Mode::Replace);
        if edits_text && self.refuse_if_read_only(view) {
            self.history_mut(view).end_group();
            return self.views[view].set_mode(Mode::Normal);
        }
        self.views[view].set_mode(mode);
    }

    /// Makes `operator` the one `view` waits on a motion for.
    pub(crate) fn set_operator(&mut self, view: ViewId, operator: PendingOperator) {
        if self.is_insert_locked(view) {
            return;
        }
        self.views[view].set_operator(operator);
    }

    /// Sets where the visual selection of `view` starts.
    pub(crate) fn set_visual_anchor(&mut self, view: ViewId, anchor: usize) {
        if self.is_insert_locked(view) {
            return;
        }
        self.views[view].set_visual_anchor(anchor);
    }

    /// Scrolls `view` to keep its cursor in sight.
    pub(super) fn scroll_to_cursor(&mut self, view: ViewId) {
        if !self.views.contains_key(view) {
            return;
        }
        let options = self.layout_options(view);
        let state = &mut self.views[view];
        let document = &self.documents[state.document()].document;
        let decorations = &self.documents[state.document()].decorations;
        state.scroll_to_cursor(document, decorations, &options);
    }
}
