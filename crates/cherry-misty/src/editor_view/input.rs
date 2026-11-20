//! How an [`EditorView`](crate::EditorView) takes input: keys fed to the
//! editor, input method text composed and typed, and the pointer and
//! wheel turned into Cherry's mouse events in cells.

use std::time::Instant;

use cherry_engine::{Editor, LineChange, Mode, MouseEvent, MouseEventKind, ViewId};
use misty::{
    EventCx, ImeInput, Input, Key, KeyCode, Mods, Point, PointerButton, Preedit, PreeditChange,
    WheelDelta,
};

use super::{EditorResponse, EditorView, respond};
use crate::clock;
use crate::keys::{mods_to_cherry, to_cherry};

// ========================================================================
// Constants
// ========================================================================

/// The rows Cherry scrolls for one notch of the wheel, which a touchpad's
/// pixel scrolling is counted in.
const ROWS_PER_NOTCH: f32 = 3.0;

// ========================================================================
// Data Structures
// ========================================================================

/// What the pointer did, from a window event, for the view to turn into
/// Cherry's mouse events.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum PointerInput {
    /// The pointer moved to this point, in physical pixels.
    Moved(Point),
    /// The primary button went down.
    Pressed,
    /// The primary button went up.
    Released,
    /// The wheel or touchpad scrolled by this much.
    Wheel(WheelDelta),
}

// ========================================================================
// EditorView Implementations
// ========================================================================

impl EditorView {
    /// Feeds `key` to the view in `editor`, asks for a redraw
    /// when the editor used it, and hands back what the editor asked of
    /// the app.
    pub(crate) fn handle_key(
        &mut self,
        editor: &mut Editor,
        cx: &mut EventCx<'_>,
        key: Key,
    ) -> EditorResponse {
        let Some(key) = to_cherry(key) else {
            return EditorResponse::Ignored;
        };
        let handled = editor.handle_key(self.view, key, clock::now());
        respond(editor, cx, handled)
    }

    /// Reacts to a mouse `input` through the view in
    /// `editor`: a press over the view moves the cursor (Shift extends the
    /// selection), a drag selects, quick presses on one cell select a word
    /// then a line, and the wheel over the view scrolls it. Asks for a
    /// redraw when the editor used the event. Offer it every event, so it
    /// sees the pointer move and the modifiers change.
    /// Offers the pointer `input` to the scrollbar, which takes a press on
    /// it and the drag that follows, scrolling the view to the row its
    /// thumb comes to; `None` for input it leaves to the text. The wheel
    /// is always the text's.
    fn handle_scrollbar(
        &mut self,
        editor: &mut Editor,
        cx: &mut EventCx<'_>,
        input: &Input,
    ) -> Option<EditorResponse> {
        if self.scrollbar_hidden || matches!(input, Input::Wheel(_)) {
            return None;
        }
        let lit = self.scrollbar.is_lit();
        let (took, row) = self.scrollbar.handle(input);
        if let Some(row) = row {
            editor.scroll_view_to_row(self.view, row);
        }
        if took || row.is_some() || lit != self.scrollbar.is_lit() {
            cx.request_redraw();
        }
        took.then_some(EditorResponse::Handled)
    }

    pub(super) fn handle_mouse(
        &mut self,
        editor: &mut Editor,
        cx: &mut EventCx<'_>,
        input: &Input,
    ) -> EditorResponse {
        if let Some(response) = self.handle_scrollbar(editor, cx, input) {
            return response;
        }
        self.follow_sign(editor, cx, input);
        let mut response = EditorResponse::Ignored;
        for mouse in self.mouse_events(input, Instant::now()) {
            let handled = editor.handle_mouse(self.view, mouse);
            response = match (response, respond(editor, cx, handled)) {
                (EditorResponse::Requests(mut earlier), EditorResponse::Requests(later)) => {
                    earlier.extend(later);
                    EditorResponse::Requests(earlier)
                }
                (
                    EditorResponse::Requests(earlier),
                    EditorResponse::Handled | EditorResponse::Ignored,
                ) => EditorResponse::Requests(earlier),
                (EditorResponse::Handled | EditorResponse::Ignored, later) => later,
            };
        }
        response
    }

    /// Names the note of the gutter sign under the pointer to the tooltip
    /// after `input` moved it, asking for the redraw that shows it once it
    /// is due; puts it away for a press or a scroll, and when the pointer
    /// leaves the signs.
    fn follow_sign(&mut self, editor: &Editor, cx: &mut EventCx<'_>, input: &Input) {
        match input {
            Input::PointerMoved(point) if !self.pressed => {
                let layout = self.layout;
                let sign = layout.and_then(|layout| {
                    let (column, row) = layout.cell_at(*point, false)?;
                    let change = self.change_notes.as_ref().and_then(|notes| {
                        let change = editor.line_change_at(self.view, column, row)?;
                        Some(match change {
                            LineChange::Added => notes.added.clone(),
                            LineChange::Deleted => notes.deleted.clone(),
                            LineChange::Modified => notes.modified.clone(),
                        })
                    });
                    let notes: Vec<String> = editor
                        .sign_note_at(self.view, column, row)
                        .into_iter()
                        .chain(change)
                        .filter(|note| !note.is_empty())
                        .collect();
                    (!notes.is_empty()).then(|| (layout.cell_rect(column, row), notes.join("; ")))
                });
                match sign {
                    Some((cell, note)) => {
                        self.tooltip.hover(cell, note);
                        if let Some(due) = self.tooltip.due() {
                            cx.request_redraw_at(due);
                        }
                    }
                    None => {
                        if self.tooltip.leave() {
                            cx.request_redraw();
                        }
                    }
                }
            }
            Input::PointerLeft if self.tooltip.leave() => cx.request_redraw(),
            Input::PointerButton(_) | Input::Wheel(_) if self.tooltip.dismiss() => {
                cx.request_redraw();
            }
            _ => {}
        }
    }

    /// Reacts to an input method event for the view in `editor`: text
    /// being composed is kept to draw at the cursor, and committed text is
    /// typed into the view key by key while it takes text (Insert mode or
    /// a prompt); in other modes a commit is dropped, since its characters
    /// would run as commands. Asks for a redraw when anything changed.
    pub(super) fn handle_ime(
        &mut self,
        editor: &mut Editor,
        cx: &mut EventCx<'_>,
        ime: &ImeInput,
    ) -> EditorResponse {
        let text = match Preedit::apply(&mut self.preedit, ime) {
            PreeditChange::Changed => {
                // Cherry lays the text into the line where keys type it; a
                // prompt's is drawn over its row.
                if lays_preedit(editor, self.view) {
                    let text = self.preedit.as_ref().map_or("", |preedit| preedit.text());
                    editor.set_preedit(self.view, text);
                }
                cx.request_redraw();
                return EditorResponse::Handled;
            }
            PreeditChange::Commit(text) => text,
            PreeditChange::Unchanged => return EditorResponse::Ignored,
        };
        if !takes_text(editor, self.view) {
            return EditorResponse::Ignored;
        }
        let mut requests = Vec::new();
        let mut response = EditorResponse::Ignored;
        for key in text.chars().filter_map(typed_key) {
            match self.handle_key(editor, cx, key) {
                EditorResponse::Handled => response = EditorResponse::Handled,
                EditorResponse::Ignored => {}
                EditorResponse::Requests(asked) => requests.extend(asked),
            }
        }
        match requests.is_empty() {
            true => response,
            false => EditorResponse::Requests(requests),
        }
    }

    /// The Cherry mouse events `input`, at `now`, makes over the view as
    /// last drawn: none for an input that is not the pointer's or falls
    /// outside the view, several for a wheel turned several notches.
    fn mouse_events(&mut self, input: &Input, now: Instant) -> Vec<MouseEvent> {
        let pointer = match input {
            Input::PointerButton(button) if button.button == PointerButton::Left => {
                self.modifiers = button.mods;
                self.pointer = Some(button.position);
                match button.pressed {
                    true => PointerInput::Pressed,
                    false => PointerInput::Released,
                }
            }
            Input::PointerLeft => {
                self.pointer = None;
                return Vec::new();
            }
            Input::PointerMoved(position) => PointerInput::Moved(*position),
            Input::Wheel(wheel) => {
                self.modifiers = wheel.mods;
                self.pointer = Some(wheel.position);
                PointerInput::Wheel(wheel.delta)
            }
            Input::AppearanceChanged(_)
            | Input::CloseRequested
            | Input::FileDropped(_)
            | Input::FileHovered(_)
            | Input::FileHoverCancelled
            | Input::FocusGained
            | Input::FocusLost
            | Input::Ime(_)
            | Input::Key(_)
            | Input::KeyReleased(_)
            | Input::PointerButton(_) => return Vec::new(),
            // Input misty adds later is no pointer input the editor knows.
            _ => return Vec::new(),
        };
        self.pointer_events(pointer, now)
    }

    /// The Cherry mouse events `input`, at `now`, makes over the view as
    /// last drawn: a move is a drag while the button is held from a press
    /// over the view, a press over the view may count as a double or
    /// triple click, and a wheel turn over the view is a scroll per notch.
    pub(super) fn pointer_events(&mut self, input: PointerInput, now: Instant) -> Vec<MouseEvent> {
        let Some(layout) = self.layout else {
            return Vec::new();
        };
        let mods = mods_to_cherry(self.modifiers);
        let at = |point: Option<Point>, kind: MouseEventKind, clamp: bool| {
            point
                .and_then(|point| layout.cell_at(point, clamp))
                .map(|(column, row)| MouseEvent {
                    column,
                    kind,
                    mods,
                    row,
                })
        };
        let event = match input {
            PointerInput::Moved(point) => {
                self.pointer = Some(point);
                match self.pressed {
                    true => at(self.pointer, MouseEventKind::Drag, true),
                    false => None,
                }
            }
            PointerInput::Pressed => {
                let press = at(self.pointer, MouseEventKind::Press, false);
                self.pressed = press.is_some();
                press.map(|press| self.clicks.count(press, clock::stamp(now)))
            }
            PointerInput::Released if self.pressed => {
                self.pressed = false;
                at(self.pointer, MouseEventKind::Release, true)
            }
            PointerInput::Released => None,
            PointerInput::Wheel(delta) => {
                let Some(notch) = at(self.pointer, MouseEventKind::ScrollDown, false) else {
                    return Vec::new();
                };
                let notches = self.wheel_notches(delta, layout.metrics.height);
                let kind = match notches < 0 {
                    true => MouseEventKind::ScrollUp,
                    false => MouseEventKind::ScrollDown,
                };
                return vec![MouseEvent { kind, ..notch }; notches.unsigned_abs()];
            }
        };
        event.into_iter().collect()
    }

    /// The whole notches the wheel's `delta` turns, negative toward the
    /// start of the document; a touchpad's pixels count a notch per
    /// [`ROWS_PER_NOTCH`] rows of `row_height`, the rest kept for later.
    fn wheel_notches(&mut self, delta: WheelDelta, row_height: f32) -> isize {
        match delta {
            WheelDelta::Lines(lines) => -lines.y.round() as isize,
            WheelDelta::Pixels(pixels) => {
                self.wheel_rest -= pixels.y / row_height.max(1.0);
                let notches = (self.wheel_rest / ROWS_PER_NOTCH).trunc();
                self.wheel_rest -= notches * ROWS_PER_NOTCH;
                notches as isize
            }
            // A delta misty adds later scrolls nothing until it is known.
            _ => 0,
        }
    }
}

// ========================================================================
// Functions
// ========================================================================

/// Whether `view` of `editor` takes typed text: in Insert or Replace
/// mode, or with a prompt open.
pub(super) fn takes_text(editor: &Editor, view: ViewId) -> bool {
    editor.view(view).is_some_and(|view| match view.mode() {
        Mode::CommandLine | Mode::Insert | Mode::Replace => true,
        Mode::Normal
        | Mode::OperatorPending
        | Mode::Visual
        | Mode::VisualBlock
        | Mode::VisualLine => false,
        // A mode added after this was written: typed text could run as
        // commands, so the input method stays off.
        _ => false,
    })
}

/// Whether Cherry lays a preedit into the text of `view` of `editor`: in
/// Insert or Replace mode with no prompt open, where keys type text.
fn lays_preedit(editor: &Editor, view: ViewId) -> bool {
    editor.view(view).is_some_and(|view| {
        view.prompt().is_none() && matches!(view.mode(), Mode::Insert | Mode::Replace)
    })
}

/// The key typing `c` presses: Enter for a line break, Tab for a tab, the
/// character otherwise; `None` for other control characters.
pub(super) fn typed_key(c: char) -> Option<Key> {
    let code = match c {
        '\n' | '\r' => KeyCode::Enter,
        '\t' => KeyCode::Tab,
        c if c.is_control() => return None,
        c => KeyCode::Char(c),
    };
    Some(Key::new(code, Mods::NONE))
}
