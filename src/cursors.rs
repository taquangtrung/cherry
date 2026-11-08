//! Several cursors editing at once, as in VS Code or vim-visual-multi.
//!
//! A view has its main cursor, the head of its selection, and may have
//! extra cursors beside it, plain offsets that every edit carries along.
//! A key typed in a view with extra cursors makes a round: it runs at the
//! main cursor as it always does, then again at each extra cursor in turn,
//! the view put back each time to how it was before the key (its mode, its
//! count, its waiting operator) with that cursor as its own. So everything
//! one cursor's keys do works at all of them with no command of its own:
//! typing in Insert mode, `ciw`, `>>`, a count, `.`, a macro, a put. After
//! the round the view is as the main cursor left it, the registers and the
//! kill ring hold what the main cursor's keys put in them, the requests
//! the extra cursors' keys made are dropped, and the round's edits undo as
//! one step. Each extra cursor keeps an unnamed register of its own: what
//! its keys yank or delete there, which its keys put back (`yiw` at three
//! cursors, then `p` at three cursors, puts each one's own word), the main
//! cursor's until it has one.
//!
//! Some keys act once, at the main cursor: undo and redo (the cursors
//! follow the history they walk), and the keys typed in a prompt. In
//! Visual and VisualLine mode each cursor has a selection of its own,
//! from where it entered the mode (`viw` at three cursors selects three
//! words, and `d` deletes them all), painted as the main one is; a block
//! selection is the main cursor's alone, so `C-v` leaves only the main
//! cursor, as do a click and Escape in Normal mode with nothing typed. Commands run by id with
//! [`Editor::run_command`](crate::Editor::run_command) run at the main
//! cursor alone.
//!
//! The commands that add cursors have no default keys; a host binds them
//! with [`Editor::bind`](crate::Editor::bind):
//!
//! - `add-cursor-next-match` adds a cursor at the next occurrence of the
//!   word under the main cursor, as far into it as the main cursor is,
//!   after the cursor added last; from a visual selection on one line, at
//!   the next occurrence of the selected text, ending the selection.
//! - `add-cursor-every-match` adds one at every occurrence.
//! - `skip-cursor-next-match` moves the cursor added last on to the next
//!   occurrence after it.
//! - `add-cursor-below` and `add-cursor-above` add one in the main
//!   cursor's column on the next line out.
//! - `remove-cursor-last` drops the cursor added last, and `keep-main-cursor`
//!   every extra one.
//!
//! `C-;` changes every occurrence at once, as Emacs's iedit: a cursor at
//! every occurrence of the word under the cursor, as far into each as the
//! main cursor is, and Insert mode, so what is typed goes into each one;
//! Escape ends it, leaving the main cursor alone.

use std::mem;

use crate::keys::{Key, KeyCode, Mods, Timestamp};

use super::command::CommandContext;
use super::document::{Document, Position, line_count};
use super::editor::{Editor, Handled, RoundState, ViewId};
use super::insert::enter_insert;
use super::register::{RegisterText, UNNAMED};
use super::search::word_at;
use super::selection::Selection;
use super::view::{Mode, View, is_visual};
use super::visual::selected_ranges;

// ========================================================================
// Constants
// ========================================================================

/// The most extra cursors a view takes: every occurrence of a short word
/// in a long document would otherwise replay each key thousands of times.
const MAX_CURSORS: usize = 1000;

/// What a command adding at an occurrence says with no word to look for.
const NO_WORD: &str = "No word under the cursor";

/// What a command adding a cursor says when there is nowhere left.
const NO_MORE: &str = "Every occurrence has a cursor";

// ========================================================================
// Data Structures
// ========================================================================

/// A view's cursors besides the main one.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct ExtraCursors {
    /// What the occurrences cursors were added at are of, kept from the
    /// first so the next ones look for the same text.
    needle: Option<Needle>,
    /// Where they are, as char offsets, in the order they were added.
    offsets: Vec<usize>,
    /// The unnamed register of each, by the same index, `None` until its
    /// keys write one.
    registers: Vec<Option<RegisterText>>,
    /// The visual anchor of each, by the same index, while the view is in
    /// a visual mode.
    anchors: Vec<Option<usize>>,
}

/// The text cursors are added at the occurrences of.
#[derive(Clone, Debug, Eq, PartialEq)]
struct Needle {
    /// How far into an occurrence its cursor goes, in chars.
    into: usize,
    /// The text looked for.
    text: String,
    /// Whether only whole words count, as for the word under the cursor;
    /// a selection's text counts wherever it is.
    whole_word: bool,
}

// ========================================================================
// ExtraCursors Implementations
// ========================================================================

impl ExtraCursors {
    /// Drops every extra cursor and what they were added at.
    pub fn clear(&mut self) {
        *self = Self::default();
    }

    /// Whether there is no extra cursor.
    pub fn is_empty(&self) -> bool {
        self.offsets.is_empty()
    }

    /// Carries every cursor across an edit with `map`.
    pub fn map(&mut self, map: impl Fn(usize) -> usize) {
        for offset in &mut self.offsets {
            *offset = map(*offset);
        }
        for anchor in self.anchors.iter_mut().flatten() {
            *anchor = map(*anchor);
        }
    }

    /// The visual anchor of each cursor, by the order of
    /// [`ExtraCursors::offsets`], while the view is in a visual mode.
    pub fn anchors(&self) -> &[Option<usize>] {
        &self.anchors
    }

    /// Where the cursors are, in the order they were added.
    pub fn offsets(&self) -> &[usize] {
        &self.offsets
    }

    /// Makes `offsets` the cursors, in that order, dropping `main` and
    /// repeats and keeping at most [`MAX_CURSORS`].
    pub fn set(&mut self, offsets: Vec<usize>, main: usize) {
        let mut kept: Vec<usize> = Vec::with_capacity(offsets.len());
        let mut registers = Vec::with_capacity(offsets.len());
        let mut anchors = Vec::with_capacity(offsets.len());
        for (index, offset) in offsets.into_iter().enumerate() {
            if offset != main && !kept.contains(&offset) && kept.len() < MAX_CURSORS {
                kept.push(offset);
                registers.push(self.registers.get(index).cloned().flatten());
                anchors.push(self.anchors.get(index).copied().flatten());
            }
        }
        self.offsets = kept;
        self.registers = registers;
        self.anchors = anchors;
        if self.offsets.is_empty() {
            self.needle = None;
        }
    }
}

// ========================================================================
// Functions
// ========================================================================

/// Feeds `key`, typed at `now`, to `view`, which has extra cursors, as a
/// round over every cursor, as the module documentation describes.
pub(crate) fn run_round(editor: &mut Editor, view: ViewId, key: Key, now: Timestamp) -> Handled {
    let before = editor.view_ref(view).clone();
    if before.mode() == Mode::CommandLine {
        return editor.run_key(view, key, now);
    }
    if before.is_idle() && key == Key::new(KeyCode::Esc, Mods::NONE) {
        editor.view_mut(view).extra_cursors_mut().clear();
        return Handled::Consumed;
    }
    let checkpoint = editor.history_mut(view).checkpoint();
    editor.take_acted_once();
    let handled = editor.run_key(view, key, now);
    let mode = editor.view_ref(view).mode();
    if editor.take_acted_once() || mode == Mode::CommandLine || mode == Mode::VisualBlock {
        return handled;
    }
    let kept = editor.round_state();
    let main = editor.view_ref(view).clone();
    let count = main.extra_cursors().offsets().len();
    let shadow = editor.insert_view_state(main);
    for index in 0..count {
        let at = editor.view_ref(shadow).extra_cursors().offsets()[index];
        let own = editor.view_ref(shadow).extra_cursors().registers[index].clone();
        let anchor = editor.view_ref(shadow).extra_cursors().anchors[index];
        let mut live = before.clone();
        live.extra_cursors_mut().clear();
        live.set_selection(Selection::point(at));
        if is_visual(before.mode()) {
            live.set_visual_anchor(anchor.unwrap_or(at));
        }
        editor.replace_view_state(view, live);
        let unnamed = with_unnamed(editor, &kept, own);
        editor.run_key(view, key, now);
        let landed = editor.view_ref(view).cursor();
        let landed_anchor = Some(editor.view_ref(view))
            .filter(|state| is_visual(state.mode()))
            .and_then(View::visual_anchor);
        let written = editor.registers_mut().stored().remove(&UNNAMED);
        let cursors = editor.view_mut(shadow).extra_cursors_mut();
        cursors.offsets[index] = landed;
        cursors.anchors[index] = landed_anchor;
        if written != unnamed {
            cursors.registers[index] = written;
        }
    }
    let main = editor.remove_view_state(shadow);
    editor.replace_view_state(view, main);
    let state = editor.view_mut(view);
    let (offsets, cursor) = (
        mem::take(&mut state.extra_cursors_mut().offsets),
        state.cursor(),
    );
    state.extra_cursors_mut().set(offsets, cursor);
    editor.restore_round_state(kept);
    editor.merge_history_since(view, checkpoint);
    handled
}

/// Puts back the registers of `kept`, as the main cursor left them, with
/// `own` as the unnamed register when an extra cursor has one; returns the
/// unnamed register's text now.
fn with_unnamed(
    editor: &mut Editor,
    kept: &RoundState,
    own: Option<RegisterText>,
) -> Option<RegisterText> {
    let mut slots = kept.registers().clone();
    if let Some(own) = own {
        slots.insert(UNNAMED, own);
    }
    let unnamed = slots.get(&UNNAMED).cloned();
    editor.registers_mut().restore(slots);
    unnamed
}

/// `add-cursor-above`: a cursor on the line above the topmost cursor.
pub(crate) fn add_above(cx: &mut CommandContext<'_>) {
    add_on_next_line(cx, false);
}

/// `add-cursor-below`: a cursor on the line below the lowest cursor.
pub(crate) fn add_below(cx: &mut CommandContext<'_>) {
    add_on_next_line(cx, true);
}

/// `add-cursor-every-match`: a cursor at every occurrence.
pub(crate) fn add_every_match(cx: &mut CommandContext<'_>) {
    let Some(needle) = needle_of(cx) else {
        return cx.editor.error(NO_WORD.to_string());
    };
    let found = occurrences(cx.editor.document_of(cx.view), &needle);
    let state = cx.editor.view_mut(cx.view);
    let mut offsets = state.extra_cursors().offsets().to_vec();
    offsets.extend(found.into_iter().map(|start| start + needle.into));
    let cursor = state.cursor();
    state.extra_cursors_mut().set(offsets, cursor);
    state.extra_cursors_mut().needle = Some(needle);
}

/// `C-;`: a cursor at every occurrence of the word under the cursor and
/// Insert mode, for Escape to end with the main cursor alone.
pub(crate) fn change_occurrences(cx: &mut CommandContext<'_>) {
    add_every_match(cx);
    if cx
        .editor
        .view_ref(cx.view)
        .extra_cursors()
        .offsets()
        .is_empty()
    {
        return;
    }
    enter_insert(cx);
    cx.editor.view_mut(cx.view).set_changing_occurrences(true);
}

/// Ends the change of every occurrence `view` is making, if it is making
/// one: every extra cursor dropped.
pub(crate) fn end_occurrence_change(editor: &mut Editor, view: ViewId) {
    let state = editor.view_mut(view);
    if state.is_changing_occurrences() {
        state.set_changing_occurrences(false);
        state.extra_cursors_mut().clear();
    }
}

/// `add-cursor-next-match`: a cursor at the next occurrence after the
/// cursor added last.
pub(crate) fn add_next_match(cx: &mut CommandContext<'_>) {
    let Some(needle) = needle_of(cx) else {
        return cx.editor.error(NO_WORD.to_string());
    };
    let state = cx.editor.view_ref(cx.view);
    let from = state
        .extra_cursors()
        .offsets()
        .last()
        .copied()
        .unwrap_or_else(|| state.cursor());
    let Some(at) = next_free_occurrence(cx.editor, cx.view, &needle, from) else {
        return cx.editor.error(NO_MORE.to_string());
    };
    let state = cx.editor.view_mut(cx.view);
    let mut offsets = state.extra_cursors().offsets().to_vec();
    offsets.push(at);
    let cursor = state.cursor();
    state.extra_cursors_mut().set(offsets, cursor);
    state.extra_cursors_mut().needle = Some(needle);
}

/// `keep-main-cursor`: drops every extra cursor.
pub(crate) fn keep_main(cx: &mut CommandContext<'_>) {
    cx.editor.view_mut(cx.view).extra_cursors_mut().clear();
}

/// `remove-cursor-last`: drops the cursor added last.
pub(crate) fn remove_last(cx: &mut CommandContext<'_>) {
    let state = cx.editor.view_mut(cx.view);
    let mut offsets = state.extra_cursors().offsets().to_vec();
    offsets.pop();
    let cursor = state.cursor();
    state.extra_cursors_mut().set(offsets, cursor);
}

/// `skip-cursor-next-match`: moves the cursor added last on to the next
/// occurrence after it.
pub(crate) fn skip_next_match(cx: &mut CommandContext<'_>) {
    let state = cx.editor.view_ref(cx.view);
    let (Some(needle), Some(&last)) = (
        state.extra_cursors().needle.clone(),
        state.extra_cursors().offsets().last(),
    ) else {
        return add_next_match(cx);
    };
    let Some(at) = next_free_occurrence(cx.editor, cx.view, &needle, last) else {
        return cx.editor.error(NO_MORE.to_string());
    };
    let state = cx.editor.view_mut(cx.view);
    let mut offsets = state.extra_cursors().offsets().to_vec();
    offsets.pop();
    offsets.push(at);
    let cursor = state.cursor();
    state.extra_cursors_mut().set(offsets, cursor);
}

/// Adds a cursor in the main cursor's column on the line past the cursor
/// furthest `down` (else up), when there is one.
fn add_on_next_line(cx: &mut CommandContext<'_>, down: bool) {
    let document = cx.editor.document_of(cx.view);
    let state = cx.editor.view_ref(cx.view);
    let line_of = |offset: usize| document.char_to_position(offset).map_or(0, |at| at.line);
    let cursor = state.cursor();
    let lines = state
        .extra_cursors()
        .offsets()
        .iter()
        .map(|offset| line_of(*offset));
    let outermost = lines
        .chain([line_of(cursor)])
        .reduce(|a, b| if down { a.max(b) } else { a.min(b) });
    let Some(outermost) = outermost else {
        return;
    };
    let line = if down {
        outermost + 1
    } else {
        match outermost.checked_sub(1) {
            Some(line) => line,
            None => return,
        }
    };
    if line >= line_count(document) {
        return;
    }
    let column = document.char_to_position(cursor).map_or(0, |at| at.column);
    let Some(at) = cursor_in_line(document, line, column, state.mode()) else {
        return;
    };
    let state = cx.editor.view_mut(cx.view);
    let mut offsets = state.extra_cursors().offsets().to_vec();
    offsets.push(at);
    state.extra_cursors_mut().set(offsets, cursor);
}

/// The offset of char `column` of `line`, kept on a character in a mode
/// whose cursor sits on one.
fn cursor_in_line(document: &Document, line: usize, column: usize, mode: Mode) -> Option<usize> {
    let start = document.position_to_char(Position { column: 0, line })?;
    let end = document.position_to_char(Position {
        column: usize::MAX,
        line,
    })?;
    let last = match mode {
        Mode::Insert | Mode::Replace => end,
        Mode::CommandLine
        | Mode::Normal
        | Mode::OperatorPending
        | Mode::Visual
        | Mode::VisualBlock
        | Mode::VisualLine => end.saturating_sub(1).max(start),
    };
    Some((start + column).min(last))
}

/// What the cursors of `cx`'s view are added at: the needle kept, else
/// the selected text of a visual selection on one line (ending it), else
/// the word under the main cursor; `None` with no word there.
fn needle_of(cx: &mut CommandContext<'_>) -> Option<Needle> {
    let state = cx.editor.view_ref(cx.view);
    if let Some(needle) = state.extra_cursors().needle.clone() {
        return Some(needle);
    }
    let document = cx.editor.document_of(cx.view);
    let cursor = state.cursor();
    if is_visual(state.mode()) {
        let options = cx.editor.options();
        let selected = selected_ranges(document, options, state, 0..document.len_lines());
        let range = selected
            .first()
            .filter(|_| state.mode() == Mode::Visual)?
            .clone();
        let start = range.start;
        let text = document.slice(range);
        if text.is_empty() || text.contains('\n') {
            return None;
        }
        cx.editor.set_mode(cx.view, Mode::Normal);
        cx.editor.set_cursor(cx.view, start);
        return Some(Needle {
            into: 0,
            text,
            whole_word: false,
        });
    }
    let word = word_at(document, cursor)?;
    if !word.contains(&cursor) {
        return None;
    }
    Some(Needle {
        into: cursor - word.start,
        text: document.slice(word.clone()),
        whole_word: true,
    })
}

/// The cursor offset of the first occurrence of `needle` after the one the
/// cursor at `from` is in, wrapping around, whose offset no cursor of
/// `view` has yet.
fn next_free_occurrence(
    editor: &Editor,
    view: ViewId,
    needle: &Needle,
    from: usize,
) -> Option<usize> {
    let found = occurrences(editor.document_of(view), needle);
    let state = editor.view_ref(view);
    let taken = |at: usize| at == state.cursor() || state.extra_cursors().offsets().contains(&at);
    let start = from.saturating_sub(needle.into);
    let after = found
        .iter()
        .position(|occurrence| *occurrence > start)
        .unwrap_or(0);
    found[after..]
        .iter()
        .chain(&found[..after])
        .map(|occurrence| occurrence + needle.into)
        .find(|at| !taken(*at))
}

/// The char offsets where `needle` occurs in `document`, in order, not
/// overlapping; for a whole-word needle, only where it is a whole word.
fn occurrences(document: &Document, needle: &Needle) -> Vec<usize> {
    let text = document.to_string();
    let len = needle.text.chars().count();
    let mut found = Vec::new();
    let mut offset = 0;
    let mut rest = text.as_str();
    while let Some(index) = rest.find(&needle.text) {
        let start = offset + rest[..index].chars().count();
        let is_whole = !needle.whole_word || word_at(document, start) == Some(start..start + len);
        if is_whole {
            found.push(start);
        }
        let skip = index + needle.text.len();
        offset = start + len;
        rest = &rest[skip..];
    }
    found
}

// ========================================================================
// Tests
// ========================================================================

#[cfg(test)]
mod tests {
    use crate::register::RegisterKind;
    use crate::testing::Harness;
    use crate::view::Mode;

    /// A harness editing `marked` with extra cursors at `extra`.
    fn harness(marked: &str, extra: &[usize]) -> Harness {
        let mut harness = Harness::new(marked);
        harness.editor.set_cursors(harness.view, extra.to_vec());
        harness
    }

    fn text(harness: &Harness) -> String {
        harness.editor.document_of(harness.view).to_string()
    }

    fn extra(harness: &Harness) -> Vec<usize> {
        harness
            .editor
            .view(harness.view)
            .unwrap()
            .cursors()
            .to_vec()
    }

    fn run(harness: &mut Harness, id: &str) {
        harness.editor.run_command(harness.view, id, None).unwrap();
    }

    #[test]
    fn test_each_cursor_selects_its_own_text_and_acts_on_it_in_visual_mode() {
        let mut harness = harness("|one two\nthree four", &[8]);
        harness.keys("v i w");
        assert_eq!(harness.mode(), Mode::Visual);
        let view = harness.editor.view(harness.view).unwrap();
        assert_eq!(view.cursors(), [12], "the extra cursor at its word's end");
        harness.keys("d");
        assert_eq!(text(&harness), " two\n four", "each selection deleted");
    }

    #[test]
    fn test_each_cursor_puts_back_what_it_yanked_itself() {
        let mut harness = Harness::new("|ab cd\nef gh");
        run(&mut harness, "add-cursor-below");
        harness.keys("y i w w P");
        let document = harness.editor.document_of(harness.view);
        assert_eq!(
            document.to_string(),
            "ab abcd\nef efgh",
            "each its own word"
        );
    }

    #[test]
    fn test_c_semicolon_changes_every_occurrence_and_escape_ends_with_one_cursor() {
        let mut harness = Harness::new("fo|o bar foo\nfoo");
        harness.keys("C-; x");
        assert_eq!(harness.marked(), "fox|o bar foxo\nfoxo");
        harness.keys("<esc>");
        assert_eq!(harness.mode(), Mode::Normal);
        let view = harness.editor.view(harness.view).unwrap();
        assert!(view.cursors().is_empty(), "the main cursor alone");
        harness.keys("x");
        assert_eq!(
            harness.marked(),
            "fo|o bar foxo\nfoxo",
            "one cursor edits once"
        );
    }

    #[test]
    fn test_typing_goes_in_at_every_cursor_and_one_undo_takes_it_all_back() {
        let mut harness = harness("a|b\nab\nab", &[4, 7]);
        harness.keys("i X Y <esc>");
        assert_eq!(text(&harness), "aXYb\naXYb\naXYb");
        assert_eq!(harness.marked(), "aX|Yb\naXYb\naXYb");
        assert_eq!(extra(&harness), [7, 12], "on each Y, as the main one");
        harness.keys("u");
        assert_eq!(text(&harness), "ab\nab\nab", "one undo, not one per cursor");
    }

    #[test]
    fn test_an_operator_with_a_count_and_dot_apply_at_every_cursor() {
        let mut harness = harness("|one two three\none two three", &[14]);
        harness.keys("d w");
        assert_eq!(text(&harness), "two three\ntwo three");
        harness.keys(".");
        assert_eq!(text(&harness), "three\nthree");
        harness.keys("u u");
        assert_eq!(text(&harness), "one two three\none two three");
    }

    #[test]
    fn test_two_cursors_on_one_line_each_type_where_they_stand() {
        let mut harness = harness("|ab ab", &[3]);
        harness.keys("i - <esc>");
        assert_eq!(text(&harness), "-ab -ab");
        assert_eq!(extra(&harness), [4]);
    }

    #[test]
    fn test_the_registers_hold_the_main_cursors_text_and_each_puts_back_its_own() {
        let mut harness = harness("|one\ntwo", &[4]);
        harness.keys("d i w");
        assert_eq!(text(&harness), "\n");
        let unnamed = harness.editor.registers_mut().read(None).unwrap();
        assert_eq!(
            (unnamed.text.as_str(), unnamed.kind),
            ("one", RegisterKind::Charwise)
        );
        harness.keys("P");
        assert_eq!(text(&harness), "one\ntwo", "each cursor's own text");
    }

    #[test]
    fn test_a_prompt_and_undo_act_once_at_the_main_cursor() {
        let mut harness = harness("|ab ab\nab", &[7]);
        harness.keys("/ b <cr>");
        assert_eq!(
            harness.marked(),
            "a|b ab\nab",
            "one search, the main cursor's"
        );
        assert_eq!(extra(&harness), [7], "the extra cursor stays");
        harness.keys("x");
        assert_eq!(text(&harness), "a ab\na");
        harness.keys("u");
        assert_eq!(text(&harness), "ab ab\nab");
        assert_eq!(extra(&harness), [6], "carried with its a");
    }

    #[test]
    fn test_escape_a_block_selection_or_a_click_leave_only_the_main_cursor() {
        let mut harness = harness("|ab\nab", &[3]);
        harness.keys("<esc>");
        assert!(extra(&harness).is_empty(), "Escape in Normal mode");
        harness.editor.set_cursors(harness.view, vec![3]);
        harness.keys("v");
        assert_eq!(extra(&harness), [3], "a selection at each cursor");
        harness.keys("<esc> C-v");
        assert!(extra(&harness).is_empty(), "a block selection");
        harness.keys("<esc>");
        harness.editor.set_cursors(harness.view, vec![3]);
        let press = crate::MouseEvent::new(crate::MouseEventKind::Press, 1, 0);
        harness.editor.handle_mouse(harness.view, press);
        assert!(extra(&harness).is_empty(), "a click");
    }

    #[test]
    fn test_cursors_are_added_at_the_next_occurrences_of_the_word_and_skipped_on() {
        let mut harness = Harness::new("f|oo bar foo foobar foo");
        run(&mut harness, "add-cursor-next-match");
        assert_eq!(extra(&harness), [9], "as far into it as the main cursor");
        run(&mut harness, "add-cursor-next-match");
        assert_eq!(extra(&harness), [9, 20], "foobar is not the word");
        run(&mut harness, "add-cursor-next-match");
        let notices = harness.requests();
        assert_eq!(extra(&harness), [9, 20]);
        assert_eq!(notices.len(), 1, "every occurrence has a cursor");
        run(&mut harness, "remove-cursor-last");
        run(&mut harness, "skip-cursor-next-match");
        assert_eq!(extra(&harness), [20], "the last moved on");
        run(&mut harness, "keep-main-cursor");
        run(&mut harness, "add-cursor-every-match");
        assert_eq!(extra(&harness), [9, 20]);
        harness.keys("c i w x <esc>");
        assert_eq!(text(&harness), "x bar x foobar x");
    }

    #[test]
    fn test_a_selection_on_one_line_adds_cursors_at_its_text_anywhere() {
        let mut harness = Harness::new("|a.b xa.b a.b");
        harness.keys("v l l");
        run(&mut harness, "add-cursor-next-match");
        assert_eq!(harness.marked(), "|a.b xa.b a.b", "the selection ends");
        assert_eq!(extra(&harness), [5]);
    }

    #[test]
    fn test_cursors_are_added_in_the_column_on_the_lines_out() {
        let mut harness = Harness::new("ab|c\nde\nfghi");
        run(&mut harness, "add-cursor-below");
        assert_eq!(extra(&harness), [5], "kept on the last char of de");
        run(&mut harness, "add-cursor-below");
        run(&mut harness, "add-cursor-below");
        assert_eq!(extra(&harness), [5, 9], "no line below the last");
        run(&mut harness, "add-cursor-above");
        assert_eq!(extra(&harness), [5, 9], "no line above the first");
    }
}
