//! Selections: one or more ranges of the document, one of them primary.
//!
//! A [`Range`] has an anchor, where it was started, and a head, where the
//! cursor is; the head may come before the anchor. An empty range (anchor
//! and head equal) is a plain cursor.
//!
//! A [`Selection`] is always normalized: its ranges are sorted by where they
//! start, and ranges that share a char, or are the same cursor, are merged
//! into one spanning them all. Ranges that only touch (one ends where the
//! next starts) stay apart, as does a cursor at the edge of a range, so two
//! adjacent words selected side by side remain two selections. The primary
//! range is followed through sorting and merging; a merged range takes the
//! direction of the primary when the primary is in it and is not a plain
//! cursor, else that of the first range in it that is not a plain cursor.

// ========================================================================
// Data Structures
// ========================================================================

/// A span of the document between an anchor and a head, both char offsets.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct Range {
    /// Where the range was started; it stays put as the head moves.
    pub anchor: usize,
    /// Where the cursor is.
    pub head: usize,
}

/// One or more ranges, sorted and with overlaps merged, one of them primary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Selection {
    /// The index of the primary range in `ranges`.
    primary: usize,
    /// The ranges, sorted by start, never sharing a char; never empty.
    ranges: Vec<Range>,
}

/// A range during normalization, with whether it is or holds the primary.
#[derive(Clone, Copy, Debug)]
struct Tagged {
    /// Whether the range is, or has absorbed, the primary range.
    is_primary: bool,
    /// The range.
    range: Range,
}

// ========================================================================
// Range Implementations
// ========================================================================

impl Range {
    /// The range from `anchor` to `head`.
    pub fn new(anchor: usize, head: usize) -> Self {
        Self { anchor, head }
    }

    /// A plain cursor at `offset`.
    pub fn point(offset: usize) -> Self {
        Self::new(offset, offset)
    }

    /// The offset just past the range's last char: the larger of anchor and
    /// head.
    pub fn end(self) -> usize {
        self.anchor.max(self.head)
    }

    /// Whether the range is a plain cursor, covering no char.
    pub fn is_empty(self) -> bool {
        self.anchor == self.head
    }

    /// The offset of the range's first char: the smaller of anchor and head.
    pub fn start(self) -> usize {
        self.anchor.min(self.head)
    }

    /// Whether the head comes before the anchor.
    fn is_backward(self) -> bool {
        self.head < self.anchor
    }

    /// Whether `next`, which starts no earlier than this range, shares a char
    /// with it or is the same cursor.
    fn overlaps(self, next: Self) -> bool {
        next.start() < self.end() || (self.is_empty() && self == next)
    }

    /// The range from `start` to `end`, running backward when `backward`.
    fn spanning(start: usize, end: usize, backward: bool) -> Self {
        if backward {
            Self::new(end, start)
        } else {
            Self::new(start, end)
        }
    }
}

// ========================================================================
// Selection Implementations
// ========================================================================

impl Selection {
    /// The selection of `primary` and `others`, normalized.
    pub fn new(primary: Range, others: impl IntoIterator<Item = Range>) -> Self {
        let tagged = others.into_iter().map(|range| Tagged {
            is_primary: false,
            range,
        });
        let primary = Tagged {
            is_primary: true,
            range: primary,
        };
        Self::normalized(tagged.chain([primary]).collect())
    }

    /// A single cursor at `offset`.
    pub fn point(offset: usize) -> Self {
        Self::single(Range::point(offset))
    }

    /// The selection of `range` alone.
    pub fn single(range: Range) -> Self {
        Self {
            primary: 0,
            ranges: vec![range],
        }
    }

    /// The primary range: the one the main cursor is in.
    pub fn primary(&self) -> Range {
        self.ranges[self.primary]
    }

    /// The index of the primary range in [`Selection::ranges`].
    pub fn primary_index(&self) -> usize {
        self.primary
    }

    /// The ranges, sorted by start, never sharing a char.
    pub fn ranges(&self) -> &[Range] {
        &self.ranges
    }

    /// The selection of `tagged`, exactly one of them primary, sorted and
    /// with overlapping ranges merged.
    fn normalized(mut tagged: Vec<Tagged>) -> Self {
        tagged.sort_by_key(|tagged| (tagged.range.start(), tagged.range.end()));
        let mut merged: Vec<Tagged> = Vec::with_capacity(tagged.len());
        for next in tagged {
            match merged.last_mut() {
                Some(last) if last.range.overlaps(next.range) => *last = last.merge(next),
                Some(_) | None => merged.push(next),
            }
        }
        Self {
            primary: merged
                .iter()
                .position(|tagged| tagged.is_primary)
                .unwrap_or(0),
            ranges: merged.into_iter().map(|tagged| tagged.range).collect(),
        }
    }
}

// ========================================================================
// Tagged Implementations
// ========================================================================

impl Tagged {
    /// This range and `next`, which overlaps it, as one range spanning both,
    /// directed as the module documentation describes.
    fn merge(self, next: Self) -> Self {
        let next_directs = next.is_primary && !next.range.is_empty();
        let backward = if next_directs || self.range.is_empty() {
            next.range.is_backward()
        } else {
            self.range.is_backward()
        };
        let start = self.range.start().min(next.range.start());
        let end = self.range.end().max(next.range.end());
        Self {
            is_primary: self.is_primary || next.is_primary,
            range: Range::spanning(start, end, backward),
        }
    }
}

// ========================================================================
// Tests
// ========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn range(anchor: usize, head: usize) -> Range {
        Range::new(anchor, head)
    }

    #[test]
    fn test_ranges_are_sorted_and_the_primary_follows_its_range() {
        let selection = Selection::new(range(10, 12), [range(20, 25), range(0, 2)]);
        assert_eq!(
            selection.ranges(),
            [range(0, 2), range(10, 12), range(20, 25)]
        );
        assert_eq!(selection.primary_index(), 1);
        assert_eq!(selection.primary(), range(10, 12));
    }

    #[test]
    fn test_overlapping_ranges_merge_and_the_primary_moves_to_the_merged_range() {
        let selection = Selection::new(
            range(8, 4),
            [range(0, 2), range(1, 3), range(3, 6), range(20, 21)],
        );
        assert_eq!(
            selection.ranges(),
            [range(0, 3), range(8, 3), range(20, 21)]
        );
        assert_eq!(selection.primary_index(), 1);
        assert_eq!(
            selection.primary(),
            range(8, 3),
            "the primary's direction wins"
        );
    }

    #[test]
    fn test_a_merge_without_the_primary_keeps_the_first_ranges_direction() {
        let selection = Selection::new(range(30, 31), [range(5, 0), range(2, 9), range(12, 10)]);
        assert_eq!(
            selection.ranges(),
            [range(9, 0), range(12, 10), range(30, 31)]
        );
        assert_eq!(selection.primary_index(), 2);
    }

    #[test]
    fn test_touching_ranges_stay_apart_but_shared_chars_and_same_cursors_merge() {
        let touching = Selection::new(range(0, 3), [range(3, 6)]);
        assert_eq!(touching.ranges(), [range(0, 3), range(3, 6)]);
        let cursors = Selection::new(Range::point(4), [Range::point(4), Range::point(7)]);
        assert_eq!(cursors.ranges(), [Range::point(4), Range::point(7)]);
        let inside = Selection::new(Range::point(2), [range(0, 5)]);
        assert_eq!(inside.ranges(), [range(0, 5)]);
        assert_eq!(inside.primary_index(), 0);
        let inside_backward = Selection::new(Range::point(2), [range(5, 0)]);
        assert_eq!(
            inside_backward.ranges(),
            [range(5, 0)],
            "a plain cursor has no direction to give"
        );
        let at_edge = Selection::new(Range::point(5), [range(0, 5)]);
        assert_eq!(at_edge.ranges(), [range(0, 5), Range::point(5)]);
        assert_eq!(at_edge.primary_index(), 1);
    }

    #[test]
    fn test_a_backward_range_spans_from_its_head_to_its_anchor() {
        let backward = range(9, 4);
        assert_eq!((backward.start(), backward.end()), (4, 9));
        assert!(!backward.is_empty());
        assert!(Range::point(3).is_empty());
    }
}
