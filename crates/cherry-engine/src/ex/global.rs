//! Ex commands that run other commands line by line:
//!
//! - `:g/pattern/command` (`:global`) runs the ex command on every line of
//!   the range (every line when none is given) that matches the pattern;
//!   `:g!` and `:v` (`:vglobal`) on every line that does not. The
//!   delimiter is any char but a letter, a digit, a blank, `\`, or `"`;
//!   an empty pattern is the last search's, and the pattern becomes the
//!   last search. With no command, a notice counts the matching lines.
//! - `:norm {keys}` (`:normal`) types the keys as Normal mode commands,
//!   each char one key (`:norm A;`), on every line of the range or at the
//!   cursor; whatever the keys leave unfinished is ended as if Escape
//!   were typed, so `:norm ihello` inserts and returns to Normal mode.
//!
//! The lines are chosen before any command runs and followed through the
//! edits the commands make, the cursor put at the start of each in turn: a
//! line deleted or joined into another by an earlier command is skipped
//! (so `:g/x/d` and `:g/^/m0` work line by line as in Vim), and a line
//! whose start an earlier command typed at is skipped too. The whole
//! command is one undo step.

use crate::command::CommandContext;
use crate::document::last_line;
use crate::document::{Document, start_of_line};
use crate::editor::{Editor, ViewId};
use crate::keys::{Key, KeyCode, Mods};
use crate::search::{CaseRule, Direction, LastSearch, NO_PREVIOUS_PATTERN};
use crate::transaction::{Assoc, ChangeSet};

use super::range::take_pattern;
use super::{ExContext, LineRange, run_line};

// ========================================================================
// Constants
// ========================================================================

/// The chars that may not delimit a `:g` pattern, besides letters,
/// digits, and blanks.
const NOT_DELIMITERS: [char; 2] = ['\\', '"'];

/// The points [`LineAnchors`] follows per line: its start and the next's.
const POINTS_PER_LINE: usize = 2;

// ========================================================================
// Data Structures
// ========================================================================

/// The lines chosen for a command to run on, in document order, followed
/// through edits: where each starts and where the line after it starts.
/// An edit costs about the lines it touches, not all of them: the lines
/// past it move together, by an amount summed in a Fenwick tree.
#[derive(Clone, Debug)]
pub(crate) struct LineAnchors {
    /// Whether the last line chosen is the document's last, with no line
    /// after it, so that its second point means nothing.
    ends_at_last_line: bool,
    /// The index of the next line to hand out.
    next: usize,
    /// Two points per line, ascending: its start (text typed there goes
    /// before it) and the start of the line after it (text typed there
    /// stays after it), each less the shift summed at its index.
    points: Vec<isize>,
    /// The Fenwick tree of the amounts the points moved by from an index
    /// on: a point's shift is the sum up to its index.
    shifts: Vec<isize>,
}

// ========================================================================
// LineAnchors Implementations
// ========================================================================

impl LineAnchors {
    /// Follows `lines` of `document`, ascending line numbers from 0.
    pub fn new(document: &Document, lines: &[usize]) -> Self {
        let last = document.len_lines() - 1;
        let points: Vec<isize> = lines
            .iter()
            .flat_map(|line| {
                let start = start_of_line(document, *line);
                let next = if *line < last {
                    start_of_line(document, line + 1)
                } else {
                    document.len_chars()
                };
                [start as isize, next as isize]
            })
            .collect();
        Self {
            ends_at_last_line: lines.last().is_some_and(|line| *line >= last),
            next: 0,
            shifts: vec![0; points.len()],
            points,
        }
    }

    /// Carries the lines not yet handed out across `changes`: the points
    /// inside the changed range one by one, those past it all at once.
    pub fn map(&mut self, changes: &ChangeSet) {
        let Some(changed) = changes.changed_range() else {
            return;
        };
        let inside =
            self.first_point_from(POINTS_PER_LINE * self.next, |point| point >= changed.start);
        let past = self.first_point_from(inside, |point| point > changed.end);
        for index in inside..past {
            let assoc = if index % POINTS_PER_LINE == 0 {
                Assoc::After
            } else {
                Assoc::Before
            };
            let mapped = changes.map_position(self.point(index), assoc);
            self.points[index] = mapped as isize - self.shift_at(index);
        }
        let moved = changes.len_after() as isize - changes.len_before() as isize;
        if moved != 0 {
            self.add_shift_from(past, moved);
        }
    }

    /// Where the next line to run on starts in `document`, skipping lines
    /// deleted or joined into another and lines whose start text was
    /// typed at; `None` when none is left.
    pub fn next_line(&mut self, document: &Document) -> Option<usize> {
        let lines = self.points.len() / POINTS_PER_LINE;
        while self.next < lines {
            let line = self.next;
            self.next += 1;
            let start = self.point(POINTS_PER_LINE * line);
            let is_last = self.ends_at_last_line && self.next == lines;
            let deleted = !is_last && self.point(POINTS_PER_LINE * line + 1) == start;
            let at_line_start = document
                .char_to_position(start)
                .is_some_and(|position| position.column == 0);
            if at_line_start && !deleted {
                return Some(start);
            }
        }
        None
    }

    /// Adds `amount` to the shift of every point from `index` on.
    fn add_shift_from(&mut self, index: usize, amount: isize) {
        let mut node = index + 1;
        while node <= self.shifts.len() {
            self.shifts[node - 1] += amount;
            node += node.isolate_lowest_one();
        }
    }

    /// The index of the first point from `from` on for which `reached`
    /// holds, `reached` holding for every point after one it holds for;
    /// the number of points when it holds for none.
    fn first_point_from(&self, from: usize, reached: impl Fn(usize) -> bool) -> usize {
        let (mut low, mut high) = (from, self.points.len());
        while low < high {
            let middle = low + (high - low) / 2;
            if reached(self.point(middle)) {
                high = middle;
            } else {
                low = middle + 1;
            }
        }
        low
    }

    /// The offset of point `index` now.
    fn point(&self, index: usize) -> usize {
        usize::try_from(self.points[index] + self.shift_at(index)).unwrap_or_default()
    }

    /// The sum of the shifts added at or before `index`.
    fn shift_at(&self, index: usize) -> isize {
        let (mut node, mut sum) = (index + 1, 0);
        while node > 0 {
            sum += self.shifts[node - 1];
            node -= node.isolate_lowest_one();
        }
        sum
    }
}

// ========================================================================
// Functions
// ========================================================================

/// `:g`: runs a command on the lines matching a pattern, or with `!` on
/// the others.
pub(crate) fn global(ex: &mut ExContext<'_>) {
    run_global(ex, ex.bang);
}

/// `:norm`: types keys as Normal mode commands on the range's lines, or
/// at the cursor.
pub(crate) fn normal(ex: &mut ExContext<'_>) {
    let keys: Vec<Key> = ex
        .args
        .chars()
        .map(|c| Key::new(KeyCode::Char(c), Mods::NONE))
        .collect();
    if keys.is_empty() {
        return ex.editor.error("Argument required".to_string());
    }
    match ex.lines {
        Some(lines) => {
            let lines: Vec<usize> = (lines.first..=lines.last).collect();
            run_on_lines(ex.editor, ex.view, &lines, |cx| {
                cx.editor.feed_keys_now(cx.view, keys.clone());
            });
        }
        None => ex.editor.feed_keys_now(ex.view, keys),
    }
}

/// `:v`: runs a command on the lines not matching a pattern.
pub(crate) fn vglobal(ex: &mut ExContext<'_>) {
    run_global(ex, true);
}

/// Runs the command after the pattern of `:g` on each line of the range
/// that matches the pattern, or that does not when `invert`.
fn run_global(ex: &mut ExContext<'_>, invert: bool) {
    let args = ex.args.trim_start();
    let delimiter = args
        .chars()
        .next()
        .filter(|c| !c.is_alphanumeric() && !c.is_whitespace() && !NOT_DELIMITERS.contains(c));
    let Some(delimiter) = delimiter else {
        return ex
            .editor
            .error("Regular expression missing from :global".to_string());
    };
    let mut rest = args;
    let typed = take_pattern(&mut rest, delimiter);
    let command = rest.trim_start().to_string();
    let pattern = if typed.is_empty() {
        let last = ex
            .editor
            .last_search()
            .map(|last| last.pattern().to_string());
        let Some(last) = last else {
            return ex.editor.error(NO_PREVIOUS_PATTERN.to_string());
        };
        last
    } else {
        typed
    };
    let regex = match ex.editor.compile_pattern(&pattern, CaseRule::Smart) {
        Ok(regex) => regex,
        Err(message) => return ex.editor.error(message),
    };
    let document = ex.editor.document_of(ex.view);
    let range = ex.lines.unwrap_or(LineRange {
        first: 0,
        last: last_line(document),
    });
    let lines: Vec<usize> = (range.first..=range.last)
        .filter(|line| {
            let text = document.line_text(*line).unwrap_or_default();
            regex.is_match(&text) != invert
        })
        .collect();
    ex.editor
        .set_last_search(LastSearch::new(Direction::Forward, pattern, regex));
    if command.is_empty() {
        let message = format!("{} matching lines", lines.len());
        return ex.editor.info(message);
    }
    run_on_lines(ex.editor, ex.view, &lines, |cx| run_line(cx, &command));
}

/// Runs `run` with the cursor at the start of each of `lines` of the
/// document of `view` in turn, following them through the edits `run`
/// makes and skipping those deleted or joined away (see the module docs).
fn run_on_lines(
    editor: &mut Editor,
    view: ViewId,
    lines: &[usize],
    mut run: impl FnMut(&mut CommandContext<'_>),
) {
    let anchors = LineAnchors::new(editor.document_of(view), lines);
    editor.push_line_anchors(view, anchors);
    while let Some(start) = editor.next_line_anchor(view) {
        editor.set_cursor(view, start);
        run(&mut CommandContext {
            count: None,
            editor,
            register: None,
            view,
        });
    }
    editor.pop_line_anchors(view);
}

// ========================================================================
// Tests
// ========================================================================

#[cfg(test)]
mod tests {
    use std::ops::Range;

    use super::LineAnchors;
    use crate::document::Document;
    use crate::request::{HostRequest, NoticeRequest};
    use crate::testing::{Harness, assert_keys};
    use crate::transaction::{ChangeSet, Edit};
    use crate::view::Mode;

    /// Applies the edit of `range` to `text` in `document` and carries
    /// `anchors` across it.
    fn edit(document: &mut Document, anchors: &mut LineAnchors, range: Range<usize>, text: &str) {
        let changes = ChangeSet::from_edits(document.len_chars(), [Edit::new(range, text)]);
        let changes = changes.unwrap();
        changes.apply(document).unwrap();
        anchors.map(&changes);
    }

    #[test]
    fn test_line_anchors_skip_deleted_and_typed_at_lines_through_shifts() {
        let mut document = Document::new("a\nbb\nccc\ndd\ne");
        let mut anchors = LineAnchors::new(&document, &[0, 1, 2, 3, 4]);
        assert_eq!(anchors.next_line(&document), Some(0));
        edit(&mut document, &mut anchors, 2..5, "");
        edit(&mut document, &mut anchors, 0..0, "Z\n");
        edit(&mut document, &mut anchors, 8..8, "Y");
        edit(&mut document, &mut anchors, 12..13, "");
        assert_eq!(document.to_string(), "Z\na\nccc\nYdd\n");
        assert_eq!(anchors.next_line(&document), Some(4), "bb was deleted");
        let last = anchors.next_line(&document);
        assert_eq!(
            last,
            Some(12),
            "dd was typed at; e was emptied, not deleted"
        );
        assert_eq!(anchors.next_line(&document), None);
    }

    /// The notices among `requests`, oldest first.
    fn notices(requests: Vec<HostRequest>) -> Vec<String> {
        requests
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
    fn test_global_deletes_matching_lines_and_undoes_at_once() {
        let mut harness = Harness::new("|x1\na\nx2\nx3\nb");
        harness.keys(": g/x/d <cr>");
        assert_eq!(harness.marked(), "a\n|b");
        harness.keys("u");
        assert_eq!(harness.marked(), "|x1\na\nx2\nx3\nb");
        assert_keys("|x1\na\nx2\nb", ": v/x/d <cr>", "x1\n|x2");
        assert_keys("|x1\na\nx2\nb", ": g!/x/d <cr>", "x1\n|x2");
        assert_keys("|x1\na\nx2\nb", ": 2,$g/x/d <cr>", "x1\na\n|b");
    }

    #[test]
    fn test_global_follows_lines_through_moves_and_joins() {
        assert_keys("|a\nb\nc", ": g/^/m0 <cr>", "|c\nb\na");
        assert_keys("|a\nb\nc\nd", ": g/^/j <cr>", "a b\n|c d");
        assert_keys("|a1\nb\na2", ": g#a#s/a/z/ <cr>", "z1\nb\n|z2");
        assert_keys("|a\nb", ": g/^/t. <cr>", "a\na\nb\n|b");
    }

    #[test]
    fn test_global_patterns_default_to_the_last_search_and_count_alone() {
        let mut harness = Harness::new("|ab\nb\nab");
        harness.keys("/ ab <cr> : g//d <cr>");
        assert_eq!(harness.marked(), "|b");
        let mut harness = Harness::new("|ab\nb\nab");
        harness.keys(": g/b/ <cr> : g <cr> : g/(/d <cr>");
        let messages = notices(harness.requests());
        assert_eq!(messages[0], "3 matching lines");
        assert_eq!(messages[1], "Regular expression missing from :global");
        assert_eq!(messages.len(), 3, "{messages:?}");
        harness.keys("n");
        assert_eq!(harness.marked(), "a|b\nb\nab", ":g sets the last search");
    }

    #[test]
    fn test_normal_types_keys_on_each_line_and_ends_in_normal_mode() {
        assert_keys("|a\nb\nc", ": %norm SPC A; <cr>", "a;\nb;\nc|;");
        assert_keys("|a\nb", ": norm SPC ihi <cr>", "h|ia\nb");
        assert_keys("|x1\na\nx2", ": g/x/norm SPC dd <cr>", "|a");
        let mut harness = Harness::new("|a\nb");
        harness.keys(": 2norm SPC v <cr>");
        assert_eq!(harness.mode(), Mode::Normal);
        harness.keys(": %norm SPC Ax <cr> u");
        assert_eq!(harness.marked(), "|a\nb", "one undo step");
        harness.keys(": norm <cr>");
        assert_eq!(notices(harness.requests()), ["Argument required"]);
    }
}
