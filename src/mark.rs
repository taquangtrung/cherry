//! Marks and the jumplist.
//!
//! # Marks
//!
//! `m` and a letter `a` to `z` marks the cursor's place in the document;
//! each document has its own marks, shared by its views, and edits carry
//! them along as they do cursors (text deleted around a mark takes it to
//! where the text was; an undo or redo keeps it inside the text). A
//! backquote and the letter goes back to the exact place, a quote to the
//! first non-blank of its line. Both are motions too: exclusive and
//! linewise after an operator (`` d`a ``, `y'a`), extending a visual
//! selection. The marks `` ` `` and `'` are the place before the latest
//! jump, so ``` `` ``` and `''` go back and forth between two places.
//!
//! The editor sets the other special marks itself: `.` at the last change
//! (its last char), `[` and `]` at the first and last char of the text
//! last changed, typed in an insert session, or yanked, `^` where Insert
//! mode was last left, and `<` and `>` at the start and end of the last
//! visual selection (whole lines for a linewise one).
//!
//! # The changelist
//!
//! Each document keeps the places of its changes, one per line for a run
//! of changes on the same line (the newest kept), at most
//! [`CHANGELIST_CAPACITY`], carried by edits. `g;` goes to an older one a
//! count at a time and `g,` to a newer one, as motions; a new change
//! starts over from the newest.
//!
//! # The jumplist
//!
//! A jump is a motion that may go far: `G`, `gg`, `:<n>`, the searches
//! (`/ ? n N * #`), and going to a mark. Each view remembers where its
//! jumps started, one place per line (the newest kept), at most
//! [`JUMPLIST_CAPACITY`]. `C-o` goes back through them (from the newest,
//! remembering where it started so `C-i` can come back) and `C-i` (also
//! `<tab>`, which terminals send for it) goes forward again, a count of
//! places at a time. Going back and forth is not a jump itself; edits
//! carry the places along as they carry marks.

use std::collections::HashMap;

use super::command::{CommandContext, await_char};
use super::document::{Document, line_end, line_of, line_start};
use super::insert::LAST_INSERT_MARK;
use super::motion::{finish, first_non_blank_of_line, last_cursor_offset};
use super::operator::{MotionKind, operate};
use super::view::Mode;

// ========================================================================
// Constants
// ========================================================================

/// The most places a changelist keeps; the oldest goes first.
const CHANGELIST_CAPACITY: usize = 100;

/// The mark at the first char of the text last changed or yanked.
pub(crate) const CHANGE_START_MARK: char = '[';

/// The mark at the last char of the text last changed or yanked.
pub(crate) const CHANGE_END_MARK: char = ']';

/// The mark at the last change.
pub(crate) const LAST_CHANGE_MARK: char = '.';

/// The notice for `g;` at the oldest change.
const CHANGELIST_START: &str = "At start of changelist";

/// The notice for `g,` at the newest change.
const CHANGELIST_END: &str = "At end of changelist";

/// The notice for `g;` or `g,` before any change.
const CHANGELIST_EMPTY: &str = "changelist is empty";

/// The most places a jumplist keeps; the oldest goes first.
const JUMPLIST_CAPACITY: usize = 100;

/// The marks the editor sets itself, read with `` ` `` and `'` as the
/// letters are.
const SPECIAL_MARKS: [char; 4] = [
    CHANGE_END_MARK,
    CHANGE_START_MARK,
    LAST_CHANGE_MARK,
    LAST_INSERT_MARK,
];

/// The marks of the last visual selection's start and end.
const VISUAL_START_MARK: char = '<';
const VISUAL_END_MARK: char = '>';

/// The mark names for the place before the latest jump.
const PREVIOUS_CONTEXT: [char; 2] = ['`', '\''];

// ========================================================================
// Data Structures
// ========================================================================

/// The places of a document's changes, oldest first, with where `g;` and
/// `g,` stand among them.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct ChangeList {
    /// The places, as char offsets, oldest first.
    entries: Vec<usize>,
    /// The entry `g;` and `g,` last went to; the length when none.
    index: usize,
}

/// The places a view jumped from, oldest first, with where `C-o` and
/// `C-i` stand among them.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct JumpList {
    /// The places, as char offsets, oldest first, one per line.
    entries: Vec<usize>,
    /// The entry `C-o` and `C-i` last went to; the length when none.
    index: usize,
    /// The place before the latest jump, for ``` `` ``` and `''`.
    previous: Option<usize>,
}

/// The marks of a document, by letter, as char offsets.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct Marks {
    /// The place of each mark set.
    places: HashMap<char, usize>,
}

// ========================================================================
// ChangeList Implementations
// ========================================================================

impl ChangeList {
    /// Records a change at `place`: in place of the newest entry when that
    /// is on the same line, `g;` starting over from the newest end.
    pub fn record(&mut self, document: &Document, place: usize) {
        let line = line_of(document, place);
        if self
            .entries
            .last()
            .is_some_and(|last| line_of(document, *last) == line)
        {
            self.entries.pop();
        }
        if self.entries.len() == CHANGELIST_CAPACITY {
            self.entries.remove(0);
        }
        self.entries.push(place);
        self.index = self.entries.len();
    }

    /// Goes `count` changes older, as far as the oldest; `None` at the
    /// oldest already.
    pub fn older(&mut self, count: usize) -> Option<usize> {
        if self.index == 0 {
            return None;
        }
        self.index = self.index.saturating_sub(count);
        self.entries.get(self.index).copied()
    }

    /// Goes `count` changes newer, as far as the newest; `None` at the
    /// newest already.
    pub fn newer(&mut self, count: usize) -> Option<usize> {
        let newest = self.entries.len().checked_sub(1)?;
        if self.index >= newest {
            return None;
        }
        self.index = self.index.saturating_add(count).min(newest);
        self.entries.get(self.index).copied()
    }

    /// Whether no change was recorded.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Carries every place to where `map` takes it, after an edit.
    pub fn map(&mut self, map: impl Fn(usize) -> usize) {
        for entry in &mut self.entries {
            *entry = map(*entry);
        }
    }
}

// ========================================================================
// JumpList Implementations
// ========================================================================

impl JumpList {
    /// Goes `count` places back from `current`, remembering `current`
    /// first when at the newest end; `None` when there are not that many.
    pub fn back(&mut self, document: &Document, current: usize, count: usize) -> Option<usize> {
        if self.index >= self.entries.len() {
            self.insert(document, current);
            self.index = self.entries.len() - 1;
        }
        self.index = self.index.checked_sub(count)?;
        self.entries.get(self.index).copied()
    }

    /// Goes `count` places forward; `None` when there are not that many.
    pub fn forward(&mut self, count: usize) -> Option<usize> {
        let target = self.index.checked_add(count)?;
        let place = *self.entries.get(target)?;
        self.index = target;
        Some(place)
    }

    /// Carries every place to where `map` takes it, after an edit.
    pub fn map(&mut self, map: impl Fn(usize) -> usize) {
        for entry in &mut self.entries {
            *entry = map(*entry);
        }
        self.previous = self.previous.map(map);
    }

    /// The place before the latest jump, if any.
    pub fn previous(&self) -> Option<usize> {
        self.previous
    }

    /// Records a jump from `from`: the newest place, `C-o` starting over
    /// from the newest end.
    pub fn push(&mut self, document: &Document, from: usize) {
        self.insert(document, from);
        self.index = self.entries.len();
        self.previous = Some(from);
    }

    /// Adds `place` as the newest entry, dropping any other on its line
    /// and the oldest when full.
    fn insert(&mut self, document: &Document, place: usize) {
        let line = line_of(document, place);
        self.entries
            .retain(|entry| line_of(document, *entry) != line);
        if self.entries.len() == JUMPLIST_CAPACITY {
            self.entries.remove(0);
        }
        self.entries.push(place);
    }
}

// ========================================================================
// Marks Implementations
// ========================================================================

impl Marks {
    /// The place of mark `name`, if set.
    pub fn get(&self, name: char) -> Option<usize> {
        self.places.get(&name).copied()
    }

    /// Carries every mark to where `map` takes it, after an edit.
    pub fn map(&mut self, map: impl Fn(usize) -> usize) {
        for place in self.places.values_mut() {
            *place = map(*place);
        }
    }

    /// Sets mark `name` at `place`.
    pub fn set(&mut self, name: char, place: usize) {
        self.places.insert(name, place);
    }
}

// ========================================================================
// Functions
// ========================================================================

/// `C-i`: goes forward through the jumplist.
pub(crate) fn jump_forward(cx: &mut CommandContext<'_>) {
    let count = cx.count_or_one();
    let place = cx.editor.view_mut(cx.view).jumps_mut().forward(count);
    if let Some(place) = place {
        land(cx, place);
    }
}

/// `C-o`: goes back through the jumplist.
pub(crate) fn jump_back(cx: &mut CommandContext<'_>) {
    let count = cx.count_or_one();
    let cursor = cx.editor.view_ref(cx.view).cursor();
    let document = cx.editor.document_of(cx.view).clone();
    let place = cx
        .editor
        .view_mut(cx.view)
        .jumps_mut()
        .back(&document, cursor, count);
    if let Some(place) = place {
        land(cx, place);
    }
}

/// Moves to `target` as a jump: after an operator, operates on the text a
/// motion of `kind` covers instead; otherwise records where the cursor
/// was in the jumplist and moves it, kept onto the line's text.
pub(crate) fn jump_to(cx: &mut CommandContext<'_>, kind: MotionKind, target: usize) {
    if operate(cx, kind, target) {
        return;
    }
    let cursor = cx.editor.view_ref(cx.view).cursor();
    let document = cx.editor.document_of(cx.view).clone();
    cx.editor
        .view_mut(cx.view)
        .jumps_mut()
        .push(&document, cursor);
    land(cx, target);
}

/// `g;`: goes to an older place in the changelist, a count at a time.
pub(crate) fn change_older(cx: &mut CommandContext<'_>) {
    walk_changes(cx, false);
}

/// `g,`: goes to a newer place in the changelist, a count at a time.
pub(crate) fn change_newer(cx: &mut CommandContext<'_>) {
    walk_changes(cx, true);
}

/// Goes `count` places along the changelist, newer or older, saying so at
/// its ends and cancelling a waiting operator there.
fn walk_changes(cx: &mut CommandContext<'_>, newer: bool) {
    let count = cx.count_or_one();
    let changelist = cx.editor.changelist_mut(cx.view);
    let (place, notice) = match (changelist.is_empty(), newer) {
        (true, _) => (None, CHANGELIST_EMPTY),
        (false, true) => (changelist.newer(count), CHANGELIST_END),
        (false, false) => (changelist.older(count), CHANGELIST_START),
    };
    let Some(place) = place else {
        cx.editor.error(notice.to_string());
        if cx.editor.view_ref(cx.view).mode() == Mode::OperatorPending {
            cx.editor.set_mode(cx.view, Mode::Normal);
        }
        return;
    };
    let place = place.min(cx.editor.document_of(cx.view).len_chars());
    let target = place.min(last_cursor_offset(cx.editor, cx.view, place));
    finish(cx, MotionKind::Exclusive, target);
}

/// `` ` ``: waits for the mark to go to exactly.
pub(crate) fn goto_mark(cx: &mut CommandContext<'_>) {
    await_char(cx, goto_mark_exactly);
}

/// `'`: waits for the mark whose line to go to.
pub(crate) fn goto_mark_line(cx: &mut CommandContext<'_>) {
    await_char(cx, goto_mark_first_non_blank);
}

/// `m`: waits for the letter of the mark to set.
pub(crate) fn set_mark(cx: &mut CommandContext<'_>) {
    await_char(cx, set_mark_named);
}

/// Goes exactly to mark `name`, as an exclusive jump.
fn goto_mark_exactly(cx: &mut CommandContext<'_>, name: char) {
    if let Some(place) = mark_place(cx, name) {
        jump_to(cx, MotionKind::Exclusive, place);
    }
}

/// Goes to the first non-blank of the line of mark `name`, as a linewise
/// jump.
fn goto_mark_first_non_blank(cx: &mut CommandContext<'_>, name: char) {
    let Some(place) = mark_place(cx, name) else {
        return;
    };
    let start = line_start(cx.editor.document_of(cx.view), place);
    let target = first_non_blank_of_line(cx.editor, cx.view, start);
    jump_to(cx, MotionKind::Linewise, target);
}

/// Moves the cursor to `place`, kept onto the line's text in Normal mode.
fn land(cx: &mut CommandContext<'_>, place: usize) {
    let place = place.min(cx.editor.document_of(cx.view).len_chars());
    let target = place.min(last_cursor_offset(cx.editor, cx.view, place));
    cx.editor.set_cursor(cx.view, target);
}

/// The place of mark `name`; when it is not set or no mark has the name,
/// `None`, saying so and cancelling a waiting operator.
fn mark_place(cx: &mut CommandContext<'_>, name: char) -> Option<usize> {
    let place = if PREVIOUS_CONTEXT.contains(&name) {
        cx.editor.view_ref(cx.view).jumps().previous()
    } else if name.is_ascii_lowercase() || SPECIAL_MARKS.contains(&name) {
        cx.editor.marks(cx.view).get(name)
    } else if [VISUAL_START_MARK, VISUAL_END_MARK].contains(&name) {
        visual_mark(cx, name == VISUAL_END_MARK)
    } else {
        None
    };
    if place.is_none() {
        cx.editor.error(format!("Mark not set: {name}"));
        if cx.editor.view_ref(cx.view).mode() == Mode::OperatorPending {
            cx.editor.set_mode(cx.view, Mode::Normal);
        }
    }
    place
}

/// The start, or with `end` the end, of the last visual selection of the
/// view: its first char, or last, or for a linewise one the start of its
/// first line, or the last char of its last.
fn visual_mark(cx: &CommandContext<'_>, end: bool) -> Option<usize> {
    let area = cx.editor.view_ref(cx.view).last_visual()?;
    let (first, last) = (area.anchor.min(area.cursor), area.anchor.max(area.cursor));
    let document = cx.editor.document_of(cx.view);
    let place = match (area.mode == Mode::VisualLine, end) {
        (true, false) => line_start(document, first),
        (true, true) => {
            let line_end = line_end(document, last);
            document
                .prev_grapheme_boundary(line_end)
                .max(line_start(document, last))
        }
        (false, false) => first,
        (false, true) => last,
    };
    Some(place)
}

/// Sets mark `name`, a letter `a` to `z`, at the cursor.
fn set_mark_named(cx: &mut CommandContext<'_>, name: char) {
    if !name.is_ascii_lowercase() {
        return cx.editor.error(format!("Invalid mark: {name}"));
    }
    let cursor = cx.editor.view_ref(cx.view).cursor();
    cx.editor.marks_mut(cx.view).set(name, cursor);
}

// ========================================================================
// Tests
// ========================================================================

#[cfg(test)]
mod tests {
    use crate::request::{HostRequest, NoticeRequest};
    use crate::testing::{Harness, assert_keys};

    /// The notices `harness` queued, oldest first.
    fn notices(harness: &mut Harness) -> Vec<String> {
        harness
            .requests()
            .into_iter()
            .filter_map(|request| match request {
                HostRequest::Notice(NoticeRequest { message, .. }) => Some(message),
                HostRequest::Cancel(_)
                | HostRequest::Command(_)
                | HostRequest::Open(_)
                | HostRequest::Quit(_)
                | HostRequest::ReadInto(_)
                | HostRequest::Save(_)
                | HostRequest::Submit(_) => None,
            })
            .collect()
    }

    #[test]
    fn test_a_backquote_goes_to_the_mark_exactly_and_a_quote_to_its_line() {
        assert_keys("one\n  tw|o\nthree", "m a g g ` a", "one\n  tw|o\nthree");
        assert_keys("one\n  tw|o\nthree", "m a G ' a", "one\n  |two\nthree");
    }

    #[test]
    fn test_marks_follow_edits_before_them_and_survive_undo() {
        assert_keys("ab|c\nd", "m a 0 i XY <esc> ` a", "XYab|c\nd");
        assert_keys("ab|c\nd", "m a 0 d $ j ` a", "|\nd");
        assert_keys("ab\nc|d", "m b g g d d u G 0 ` b", "ab\nc|d");
    }

    #[test]
    fn test_marks_are_motions_after_an_operator_and_in_visual_mode() {
        assert_keys("ab|cdef", "m a $ d ` a", "ab|f");
        assert_keys("a\nb|b\nc\nd", "m a j j d ' a", "|a");
        assert_keys("|abcdef", "l l m a 0 v ` a d", "|def");
    }

    #[test]
    fn test_g_semicolon_and_comma_walk_the_changes_one_per_line() {
        let mut harness = Harness::new("|l0\nl1\nl2\nl3");
        harness.keys("x j j x x G A ! <esc> g g");
        harness.keys("g ;");
        assert_eq!(harness.marked(), "0\nl1\n\nl3|!", "the newest");
        harness.keys("g ;");
        assert_eq!(
            harness.marked(),
            "0\nl1\n|\nl3!",
            "two x on a line, one place"
        );
        harness.keys("g ; g ;");
        assert_eq!(harness.marked(), "|0\nl1\n\nl3!", "the oldest");
        assert_eq!(notices(&mut harness), ["At start of changelist"]);
        harness.keys("2 g ,");
        assert_eq!(harness.marked(), "0\nl1\n\nl3|!");
        harness.keys("g ,");
        assert_eq!(notices(&mut harness), ["At end of changelist"]);
        let mut fresh = Harness::new("|ab");
        fresh.keys("g ;");
        assert_eq!(notices(&mut fresh), ["changelist is empty"]);
    }

    #[test]
    fn test_the_editor_sets_the_change_insert_yank_and_visual_marks() {
        assert_keys("one |two three", "c w 2 <esc> g g ` .", "one |2 three");
        assert_keys("|one two", "A SPC three <esc> 0 ` [", "one two| three");
        assert_keys("|one two", "A SPC three <esc> 0 ` ]", "one two thre|e");
        assert_keys("one |two three", "y e 0 ` ]", "one tw|o three");
        assert_keys("one |two three", "a x <esc> 0 ` ^", "one tx|wo three");
        assert_keys("o|ne two three", "v w e <esc> 0 ` >", "one tw|o three");
        assert_keys("one\nt|wo", "V k <esc> G ` <", "|one\ntwo");
    }

    #[test]
    fn test_an_unset_or_invalid_mark_says_so_and_cancels_the_operator() {
        let mut harness = Harness::new("|abc");
        harness.keys("` q d ' z m 1");
        assert_eq!(
            notices(&mut harness),
            ["Mark not set: q", "Mark not set: z", "Invalid mark: 1"]
        );
        harness.keys("l x");
        assert_eq!(harness.marked(), "a|c", "the d was cancelled");
    }

    #[test]
    fn test_c_o_and_c_i_walk_the_jumps_of_g_gg_and_search() {
        let mut harness = Harness::new("|l0\nl1\nl2 x\nl3\nl4");
        harness.keys("G / x <cr> g g");
        assert_eq!(harness.marked(), "|l0\nl1\nl2 x\nl3\nl4");
        harness.keys("C-o");
        assert_eq!(harness.marked(), "l0\nl1\nl2 |x\nl3\nl4");
        harness.keys("C-o");
        assert_eq!(harness.marked(), "l0\nl1\nl2 x\nl3\n|l4");
        harness.keys("C-o C-o");
        assert_eq!(harness.marked(), "l0\nl1\nl2 x\nl3\n|l4", "no older place");
        harness.keys("<tab>");
        assert_eq!(harness.marked(), "l0\nl1\nl2 |x\nl3\nl4");
        harness.keys("C-i");
        assert_eq!(
            harness.marked(),
            "|l0\nl1\nl2 x\nl3\nl4",
            "back where C-o began"
        );
        harness.keys("C-i");
        assert_eq!(harness.marked(), "|l0\nl1\nl2 x\nl3\nl4", "no newer place");
        harness.keys("2 C-o");
        assert_eq!(harness.marked(), "l0\nl1\nl2 x\nl3\n|l4");
    }

    #[test]
    fn test_quote_quote_toggles_between_the_last_two_places_and_goto_line_jumps() {
        assert_keys("|a\nb\n  c", "G ' '", "|a\nb\n  c");
        assert_keys("|a\nb\n  c", "G ' ' ' '", "a\nb\n  |c");
        assert_keys("a|b\nc", "j ` ` ", "ab\n|c");
        assert_keys("a|b\nc", "G ` `", "a|b\nc");
        assert_keys("a\n|b\nc", ": 3 <cr> C-o", "a\n|b\nc");
    }

    #[test]
    fn test_jump_places_follow_edits() {
        assert_keys(
            "ab\n|cd\nef",
            "G g g O new <esc> C-o C-o",
            "new\nab\n|cd\nef",
        );
    }
}
