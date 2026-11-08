//! Change sets: an edit of the whole document described as data, so it can
//! be applied, inverted for undo, composed with the next edit, and used to
//! carry positions and selections across it.
//!
//! A [`ChangeSet`] walks the document from start to end as a list of
//! operations: keep the next chars, delete the next chars, or insert text.
//! It is made from [`Edit`]s (each a char range of the original text and
//! its replacement), all at once, so one keystroke at many cursors is one
//! change set. Inserted text has its line breaks normalized to `\n`, as the
//! [`Document`] stores them.
//!
//! # Mapping
//!
//! [`ChangeSet::map_position`] says where an offset of the old text lands in
//! the new one: unchanged before every edit, shifted after one, at the start
//! of a deletion it fell in. Text inserted exactly at the offset is the one
//! ambiguous case, settled by [`Assoc`]: the offset stays before the text, or
//! moves past it.
//!
//! [`ChangeSet::map_selection`] maps every range: a plain cursor moves past
//! text inserted at it, as typing does, while a range neither grows to take
//! in text inserted at its edges nor loses any inserted within it. Ranges a
//! deletion brings together merge, as [`Selection`] always does.

use std::ops;

use thiserror::Error;

use super::document::{Document, RangeError, normalize_line_breaks};
use super::selection::{Range, Selection};

// ========================================================================
// Data Structures
// ========================================================================

/// Which side of text inserted exactly at a position the position maps to.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Assoc {
    /// Past the inserted text, as a cursor moves when typing.
    After,
    /// Before the inserted text.
    Before,
}

/// Why a change set could not be made, applied, or composed.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
#[non_exhaustive]
pub enum ChangeError {
    /// A document or change set has another length than the change set
    /// expects.
    #[error(transparent)]
    LengthMismatch(LengthMismatch),
    /// An edit's range runs backward or past the end of the document.
    #[error(transparent)]
    Range(RangeError),
    /// An edit starts before the previous one ends.
    #[error(transparent)]
    Unordered(Unordered),
}

/// An edit of a whole document: what to keep, delete, and insert, from its
/// start to its end.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChangeSet {
    /// The length, in chars, of the text after the change.
    len_after: usize,
    /// The length, in chars, of the text the change applies to.
    len_before: usize,
    /// The operations, in canonical form: none empty, no two of a kind in a
    /// row, and an insert before a delete where they meet.
    operations: Vec<Operation>,
}

/// One replacement: a char range of the original text and the text that
/// takes its place. An empty range inserts; empty text deletes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Edit {
    /// The chars replaced, as offsets into the original text.
    pub range: ops::Range<usize>,
    /// The text put in their place.
    pub text: String,
}

/// A length that does not match what a change set expects.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
#[error("expected a text of {expected} chars, found {actual}")]
pub struct LengthMismatch {
    /// The length found.
    pub actual: usize,
    /// The length the change set expects.
    pub expected: usize,
}

/// An edit that starts before the previous edit ends.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
#[error("an edit starts at {start}, before the previous edit ends at {previous_end}")]
pub struct Unordered {
    /// Where the previous edit ends.
    pub previous_end: usize,
    /// Where the edit starts.
    pub start: usize,
}

/// One step of a change set's walk over the document.
#[derive(Clone, Debug, Eq, PartialEq)]
enum Operation {
    /// Drop the next chars.
    Delete(usize),
    /// Put this text in.
    Insert(String),
    /// Keep the next chars.
    Retain(usize),
}

// ========================================================================
// ChangeSet Implementations
// ========================================================================

impl ChangeSet {
    /// The change that makes `edits` to a text of `len` chars. The edits are
    /// given in order, as ranges of the original text that do not overlap;
    /// several may insert at one offset, in the order given.
    pub fn from_edits(
        len: usize,
        edits: impl IntoIterator<Item = Edit>,
    ) -> Result<Self, ChangeError> {
        let mut changes = Self::empty();
        let mut offset = 0;
        for Edit { range, text } in edits {
            if range.start > range.end || range.end > len {
                return Err(ChangeError::Range(RangeError {
                    end: range.end,
                    len,
                    start: range.start,
                }));
            }
            if range.start < offset {
                return Err(ChangeError::Unordered(Unordered {
                    previous_end: offset,
                    start: range.start,
                }));
            }
            changes.retain(range.start - offset);
            changes.delete(range.end - range.start);
            changes.insert(&normalize_line_breaks(&text));
            offset = range.end;
        }
        changes.retain(len - offset);
        Ok(changes)
    }

    /// Applies the change to `document`, which must have the length the
    /// change was made for; on a mismatch the document is left as it was.
    pub fn apply(&self, document: &mut Document) -> Result<(), ChangeError> {
        check_length(document.len_chars(), self.len_before)?;
        let rope = document.rope_mut();
        let mut offset = 0;
        for operation in &self.operations {
            match operation {
                Operation::Delete(len) => rope.remove(offset..offset + len),
                Operation::Insert(text) => {
                    rope.insert(offset, text);
                    offset += char_len(text);
                }
                Operation::Retain(len) => offset += len,
            }
        }
        Ok(())
    }

    /// This change followed by `next`, as one change: applying it equals
    /// applying the two in turn. `next` must apply to the text this change
    /// produces.
    pub fn compose(self, next: Self) -> Result<Self, ChangeError> {
        let mismatch = LengthMismatch {
            actual: next.len_before,
            expected: self.len_after,
        };
        check_length(mismatch.actual, mismatch.expected)?;
        let mut composed = Self::empty();
        let mut first = self.operations.into_iter();
        let mut second = next.operations.into_iter();
        let (mut head_first, mut head_second) = (first.next(), second.next());
        loop {
            match (head_first.take(), head_second.take()) {
                (None, None) => break,
                (Some(Operation::Delete(len)), pending) => {
                    composed.delete(len);
                    (head_first, head_second) = (first.next(), pending);
                }
                (pending, Some(Operation::Insert(text))) => {
                    composed.insert(&text);
                    (head_first, head_second) = (pending, second.next());
                }
                (Some(Operation::Retain(kept)), Some(Operation::Retain(also_kept))) => {
                    let len = kept.min(also_kept);
                    composed.retain(len);
                    head_first = rest_or_next(Operation::Retain, kept - len, &mut first);
                    head_second = rest_or_next(Operation::Retain, also_kept - len, &mut second);
                }
                (Some(Operation::Retain(kept)), Some(Operation::Delete(deleted))) => {
                    let len = kept.min(deleted);
                    composed.delete(len);
                    head_first = rest_or_next(Operation::Retain, kept - len, &mut first);
                    head_second = rest_or_next(Operation::Delete, deleted - len, &mut second);
                }
                (Some(Operation::Insert(text)), Some(Operation::Retain(kept))) => {
                    let (front, back) = split_chars(&text, kept);
                    composed.insert(front);
                    let len = char_len(front);
                    head_first = text_or_next(back, &mut first);
                    head_second = rest_or_next(Operation::Retain, kept - len, &mut second);
                }
                (Some(Operation::Insert(text)), Some(Operation::Delete(deleted))) => {
                    let (front, back) = split_chars(&text, deleted);
                    let len = char_len(front);
                    head_first = text_or_next(back, &mut first);
                    head_second = rest_or_next(Operation::Delete, deleted - len, &mut second);
                }
                (None, Some(Operation::Delete(_) | Operation::Retain(_)))
                | (Some(Operation::Insert(_) | Operation::Retain(_)), None) => {
                    return Err(ChangeError::LengthMismatch(mismatch));
                }
            }
        }
        Ok(composed)
    }

    /// The change that undoes this one, made from `original`, the document
    /// as it was before this change applied (the text it deletes is read
    /// from there).
    pub fn invert(&self, original: &Document) -> Result<Self, ChangeError> {
        check_length(original.len_chars(), self.len_before)?;
        let mut inverse = Self::empty();
        let mut offset = 0;
        for operation in &self.operations {
            match operation {
                Operation::Delete(len) => {
                    let deleted = original.rope().slice(offset..offset + len).to_string();
                    inverse.insert(&deleted);
                    offset += len;
                }
                Operation::Insert(text) => inverse.delete(char_len(text)),
                Operation::Retain(len) => {
                    inverse.retain(*len);
                    offset += len;
                }
            }
        }
        Ok(inverse)
    }

    /// The edits the change makes, as ranges of the text before it and the
    /// text put in their place, in order: what [`ChangeSet::from_edits`]
    /// would make it from, with neighboring edits joined. A parser that
    /// reparses incrementally reads them to say where the text changed.
    pub fn edits(&self) -> Vec<Edit> {
        let mut edits: Vec<Edit> = Vec::new();
        let mut old = 0;
        for operation in &self.operations {
            let (deleted, inserted) = match operation {
                Operation::Retain(len) => {
                    old += len;
                    continue;
                }
                Operation::Delete(len) => (*len, ""),
                Operation::Insert(text) => (0, text.as_str()),
            };
            match edits.last_mut() {
                Some(last) if last.range.end == old => {
                    last.range.end += deleted;
                    last.text.push_str(inserted);
                }
                Some(_) | None => edits.push(Edit::new(old..old + deleted, inserted)),
            }
            old += deleted;
        }
        edits
    }

    /// The offset of the first char the change deletes or inserts at, the
    /// same in the text before and after it; `None` when it changes
    /// nothing.
    pub fn first_change(&self) -> Option<usize> {
        match self.operations.as_slice() {
            [Operation::Retain(len), _, ..] => Some(*len),
            [Operation::Retain(_)] | [] => None,
            [Operation::Delete(_) | Operation::Insert(_), ..] => Some(0),
        }
    }

    /// The chars of the text before the change from the first it deletes
    /// or inserts at to the end of the last: every position past it moves
    /// by the change in length, every position before it stays. `None`
    /// when it changes nothing.
    pub(crate) fn changed_range(&self) -> Option<ops::Range<usize>> {
        let start = self.first_change()?;
        let (mut old, mut end) = (0, start);
        for operation in &self.operations {
            match operation {
                Operation::Delete(len) => {
                    old += len;
                    end = old;
                }
                Operation::Insert(_) => end = old,
                Operation::Retain(len) => old += len,
            }
        }
        Some(start..end)
    }

    /// Whether the change keeps the text as it is.
    pub fn is_identity(&self) -> bool {
        self.operations
            .iter()
            .all(|operation| matches!(operation, Operation::Retain(_)))
    }

    /// The length, in chars, of the text after the change.
    pub fn len_after(&self) -> usize {
        self.len_after
    }

    /// The length, in chars, of the text the change applies to.
    pub fn len_before(&self) -> usize {
        self.len_before
    }

    /// Where `position`, an offset of the text before the change, lands in
    /// the text after it; `assoc` says which side of text inserted exactly
    /// there it takes. A position past the end maps to the new end.
    pub fn map_position(&self, position: usize, assoc: Assoc) -> usize {
        let (mut old, mut new) = (0, 0);
        for operation in &self.operations {
            match operation {
                Operation::Delete(len) => {
                    if position < old + len {
                        return new;
                    }
                    old += len;
                }
                Operation::Insert(text) => {
                    if position == old && assoc == Assoc::Before {
                        return new;
                    }
                    new += char_len(text);
                }
                Operation::Retain(len) => {
                    if position < old + len {
                        return new + (position - old);
                    }
                    old += len;
                    new += len;
                }
            }
        }
        (new + position.saturating_sub(old)).min(self.len_after)
    }

    /// `selection`, a selection of the text before the change, carried into
    /// the text after it as the module documentation describes.
    pub fn map_selection(&self, selection: &Selection) -> Selection {
        let primary = self.map_range(selection.primary());
        let others = selection
            .ranges()
            .iter()
            .enumerate()
            .filter(|(index, _)| *index != selection.primary_index())
            .map(|(_, range)| self.map_range(*range));
        Selection::new(primary, others)
    }

    /// The change of nothing, applying to the empty text, to build on.
    fn empty() -> Self {
        Self {
            len_after: 0,
            len_before: 0,
            operations: Vec::new(),
        }
    }

    /// Appends the deletion of the next `len` chars.
    fn delete(&mut self, len: usize) {
        if len == 0 {
            return;
        }
        self.len_before += len;
        match self.operations.last_mut() {
            Some(Operation::Delete(deleted)) => *deleted += len,
            Some(Operation::Insert(_) | Operation::Retain(_)) | None => {
                self.operations.push(Operation::Delete(len));
            }
        }
    }

    /// Appends the insertion of `text`, which has `\n` line breaks only,
    /// keeping an insert before a delete where they meet.
    fn insert(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        self.len_after += char_len(text);
        let count = self.operations.len();
        let (before_last, last) = match self.operations.as_mut_slice() {
            [.., before_last, last] => (Some(before_last), Some(last)),
            [last] => (None, Some(last)),
            [] => (None, None),
        };
        match (before_last, last) {
            (_, Some(Operation::Insert(inserted)))
            | (Some(Operation::Insert(inserted)), Some(Operation::Delete(_))) => {
                inserted.push_str(text);
            }
            (_, Some(Operation::Delete(_))) => {
                self.operations
                    .insert(count - 1, Operation::Insert(text.to_string()));
            }
            (_, Some(Operation::Retain(_)) | None) => {
                self.operations.push(Operation::Insert(text.to_string()));
            }
        }
    }

    /// `range` carried through the change: a plain cursor moves past text
    /// inserted at it; a range keeps its edges outside text inserted there.
    fn map_range(&self, range: Range) -> Range {
        if range.is_empty() {
            return Range::point(self.map_position(range.head, Assoc::After));
        }
        let start = self.map_position(range.start(), Assoc::After);
        let end = self.map_position(range.end(), Assoc::Before).max(start);
        if range.head < range.anchor {
            Range::new(end, start)
        } else {
            Range::new(start, end)
        }
    }

    /// Appends keeping the next `len` chars.
    fn retain(&mut self, len: usize) {
        if len == 0 {
            return;
        }
        self.len_before += len;
        self.len_after += len;
        match self.operations.last_mut() {
            Some(Operation::Retain(kept)) => *kept += len,
            Some(Operation::Delete(_) | Operation::Insert(_)) | None => {
                self.operations.push(Operation::Retain(len));
            }
        }
    }
}

// ========================================================================
// Edit Implementations
// ========================================================================

impl Edit {
    /// The edit replacing the chars in `range` with `text`.
    pub fn new(range: ops::Range<usize>, text: impl Into<String>) -> Self {
        Self {
            range,
            text: text.into(),
        }
    }
}

// ========================================================================
// Functions
// ========================================================================

/// The length of `text` in chars.
fn char_len(text: &str) -> usize {
    text.chars().count()
}

/// Fails unless `actual` is the `expected` length.
fn check_length(actual: usize, expected: usize) -> Result<(), ChangeError> {
    if actual != expected {
        return Err(ChangeError::LengthMismatch(LengthMismatch {
            actual,
            expected,
        }));
    }
    Ok(())
}

/// The operation `make` of the `len` left over, or the next operation of
/// `rest` when nothing is left.
fn rest_or_next(
    make: fn(usize) -> Operation,
    len: usize,
    rest: &mut impl Iterator<Item = Operation>,
) -> Option<Operation> {
    if len == 0 {
        return rest.next();
    }
    Some(make(len))
}

/// `text` split after its first `len` chars (all of it when shorter).
fn split_chars(text: &str, len: usize) -> (&str, &str) {
    let at = text
        .char_indices()
        .nth(len)
        .map_or(text.len(), |(index, _)| index);
    text.split_at(at)
}

/// The insertion of the `text` left over, or the next operation of `rest`
/// when none is left.
fn text_or_next(text: &str, rest: &mut impl Iterator<Item = Operation>) -> Option<Operation> {
    if text.is_empty() {
        return rest.next();
    }
    Some(Operation::Insert(text.to_string()))
}

// ========================================================================
// Tests
// ========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// How many random change sets the randomized tests try.
    const RANDOM_ROUNDS: usize = 500;

    fn changes(len: usize, edits: &[(ops::Range<usize>, &str)]) -> ChangeSet {
        let edits = edits
            .iter()
            .map(|(range, text)| Edit::new(range.clone(), *text));
        ChangeSet::from_edits(len, edits).expect("the test edits are valid")
    }

    fn applied(text: &str, changes: &ChangeSet) -> String {
        let mut document = Document::new(text);
        changes.apply(&mut document).expect("the lengths match");
        document.to_string()
    }

    fn range(anchor: usize, head: usize) -> Range {
        Range::new(anchor, head)
    }

    /// A small deterministic pseudo-random generator (xorshift), so the
    /// randomized tests need no dependency and fail the same way every run.
    struct Rng(u64);

    impl Rng {
        fn below(&mut self, bound: usize) -> usize {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            (self.0 % bound as u64) as usize
        }

        fn change_set(&mut self, len: usize) -> ChangeSet {
            const PIECES: [&str; 4] = ["", "x", "日本", "a\nb"];
            let mut edits = Vec::new();
            let mut offset = 0;
            while offset <= len && self.below(3) != 0 {
                let start = offset + self.below(len - offset + 1);
                let end = start + self.below(len - start + 1);
                edits.push(Edit::new(start..end, PIECES[self.below(PIECES.len())]));
                offset = end;
                if offset == len {
                    break;
                }
            }
            ChangeSet::from_edits(len, edits).expect("generated edits are ordered")
        }
    }

    #[test]
    fn test_edits_give_back_what_the_change_set_was_made_from() {
        let edits = vec![
            Edit::new(1..1, "x"),
            Edit::new(2..4, ""),
            Edit::new(4..5, "yz"),
            Edit::new(6..6, "!"),
        ];
        let changes = ChangeSet::from_edits(6, edits).unwrap();
        assert_eq!(
            changes.edits(),
            [
                Edit::new(1..1, "x"),
                Edit::new(2..5, "yz"),
                Edit::new(6..6, "!"),
            ],
            "the touching delete and replace joined"
        );
        let rebuilt = ChangeSet::from_edits(6, changes.edits()).unwrap();
        assert_eq!(rebuilt, changes);
    }

    #[test]
    fn test_edits_apply_in_one_pass_with_offsets_of_the_original_text() {
        let changes = changes(11, &[(0..0, ">> "), (6..11, "wörld\r\n"), (11..11, "!")]);
        assert_eq!(changes.len_before(), 11);
        assert_eq!(changes.len_after(), 16);
        assert_eq!(applied("hello world", &changes), ">> hello wörld\n!");
    }

    #[test]
    fn test_edits_out_of_order_overlapping_or_out_of_range_are_rejected() {
        let overlapping = [Edit::new(2..5, "a"), Edit::new(4..6, "b")];
        assert_eq!(
            ChangeSet::from_edits(10, overlapping).err(),
            Some(ChangeError::Unordered(Unordered {
                previous_end: 5,
                start: 4,
            }))
        );
        assert_eq!(
            ChangeSet::from_edits(3, [Edit::new(2..4, "")]).err(),
            Some(ChangeError::Range(RangeError {
                end: 4,
                len: 3,
                start: 2,
            }))
        );
    }

    #[test]
    fn test_changes_refuse_a_document_of_another_length() {
        let changes = changes(3, &[(1..2, "x")]);
        let mut document = Document::new("four");
        assert_eq!(
            changes.apply(&mut document),
            Err(ChangeError::LengthMismatch(LengthMismatch {
                actual: 4,
                expected: 3,
            }))
        );
        assert_eq!(document.to_string(), "four");
    }

    #[test]
    fn test_the_inverse_restores_the_original_text() {
        let original = Document::new("héllo\nwörld");
        let changes = changes(11, &[(1..3, "E"), (5..7, " "), (11..11, "!")]);
        let inverse = changes.invert(&original).unwrap();
        let mut document = original.clone();
        changes.apply(&mut document).unwrap();
        assert_eq!(document.to_string(), "hElo örld!");
        inverse.apply(&mut document).unwrap();
        assert_eq!(document.to_string(), "héllo\nwörld");
    }

    #[test]
    fn test_the_first_change_is_where_the_first_edit_starts_and_its_inverse_too() {
        let original = Document::new("abcdef");
        let changes = changes(6, &[(2..3, ""), (5..5, "X")]);
        assert_eq!(changes.first_change(), Some(2));
        assert_eq!(changes.invert(&original).unwrap().first_change(), Some(2));
        assert_eq!(self::changes(6, &[(0..0, "X")]).first_change(), Some(0));
        assert_eq!(self::changes(6, &[]).first_change(), None);
    }

    #[test]
    fn test_the_changed_range_spans_the_first_to_the_last_edit_of_the_original() {
        let changes = changes(10, &[(2..3, ""), (5..5, "XY"), (7..9, "Q")]);
        assert_eq!(changes.changed_range(), Some(2..9));
        assert_eq!(self::changes(6, &[(6..6, "X")]).changed_range(), Some(6..6));
        assert_eq!(self::changes(6, &[]).changed_range(), None);
    }

    #[test]
    fn test_composed_changes_equal_applying_them_in_turn() {
        let first = changes(5, &[(0..0, "abc"), (2..4, "")]);
        let second = changes(6, &[(1..4, "Z"), (6..6, "?")]);
        let composed = first.clone().compose(second.clone()).unwrap();
        assert_eq!(applied("01234", &composed), "aZ14?");
        assert_eq!(applied(&applied("01234", &first), &second), "aZ14?");
        assert_eq!(
            first.compose(changes(4, &[])).err(),
            Some(ChangeError::LengthMismatch(LengthMismatch {
                actual: 4,
                expected: 6,
            }))
        );
    }

    #[test]
    fn test_random_changes_compose_and_invert_like_applying_them_in_turn() {
        let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
        for _ in 0..RANDOM_ROUNDS {
            let original = Document::new("ab\ncdé日f\n");
            let first = rng.change_set(original.len_chars());
            let second = rng.change_set(first.len_after());
            let mut stepwise = original.clone();
            first.apply(&mut stepwise).unwrap();
            let middle = stepwise.clone();
            second.apply(&mut stepwise).unwrap();
            let composed = first.clone().compose(second.clone()).unwrap();
            let mut at_once = original.clone();
            composed.apply(&mut at_once).unwrap();
            assert_eq!(
                at_once.to_string(),
                stepwise.to_string(),
                "{first:?} then {second:?}"
            );
            let undo = second
                .invert(&middle)
                .unwrap()
                .compose(first.invert(&original).unwrap());
            undo.unwrap().apply(&mut stepwise).unwrap();
            assert_eq!(stepwise.to_string(), original.to_string());
        }
    }

    #[test]
    fn test_positions_map_through_the_edits_around_them() {
        let changes = changes(10, &[(2..4, ""), (6..6, "xyz"), (8..9, "Q")]);
        assert_eq!(changes.map_position(1, Assoc::After), 1);
        assert_eq!(
            changes.map_position(3, Assoc::After),
            2,
            "inside a deletion"
        );
        assert_eq!(changes.map_position(5, Assoc::After), 3);
        assert_eq!(changes.map_position(6, Assoc::Before), 4);
        assert_eq!(changes.map_position(6, Assoc::After), 7);
        assert_eq!(changes.map_position(7, Assoc::Before), 8);
        assert_eq!(changes.map_position(10, Assoc::After), 11);
    }

    #[test]
    fn test_a_replacement_maps_a_position_at_its_start_by_assoc() {
        let changes = changes(6, &[(2..4, "new")]);
        assert_eq!(changes.map_position(2, Assoc::Before), 2);
        assert_eq!(changes.map_position(2, Assoc::After), 5);
        assert_eq!(changes.map_position(3, Assoc::After), 5);
        assert_eq!(changes.map_position(4, Assoc::Before), 5);
    }

    #[test]
    fn test_a_multi_range_selection_maps_through_edits_before_inside_and_after_each_range() {
        // Ranges 3..6 and 12..9 (backward); text of 20 chars.
        let selection = Selection::new(range(3, 6), [range(12, 9)]);
        let changes = changes(
            20,
            &[
                (1..1, "AB"),   // before the first range: shifts both
                (4..5, ""),     // inside the first range: shrinks it
                (7..8, "cdef"), // between the ranges: shifts the second
                (10..10, "+"),  // inside the second range: grows it
                (15..20, ""),   // after both: changes neither
            ],
        );
        let mapped = changes.map_selection(&selection);
        assert_eq!(mapped.ranges(), [range(5, 7), range(17, 13)]);
        assert_eq!(mapped.primary_index(), 0);
    }

    #[test]
    fn test_text_typed_at_a_cursor_moves_it_but_a_range_does_not_grow_at_its_edges() {
        let selection = Selection::new(Range::point(2), [range(5, 8)]);
        let changes = changes(10, &[(2..2, "ab"), (5..5, "<"), (8..8, ">")]);
        let mapped = changes.map_selection(&selection);
        assert_eq!(mapped.ranges(), [Range::point(4), range(8, 11)]);
    }

    #[test]
    fn test_cursors_brought_together_by_a_deletion_merge() {
        let selection = Selection::new(Range::point(5), [Range::point(3), range(7, 9)]);
        let changes = changes(10, &[(2..7, "")]);
        let mapped = changes.map_selection(&selection);
        assert_eq!(mapped.ranges(), [Range::point(2), range(2, 4)]);
        assert_eq!(mapped.primary_index(), 0);
    }
}
