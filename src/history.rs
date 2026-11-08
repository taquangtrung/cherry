//! Undo history: the changes made to a document, so they can be undone and
//! redone, each with the selection to restore.
//!
//! The history is a tree. A commit records one undo step after the text as
//! it stands; after an undo, that makes a new branch beside the undone
//! steps, which stay. Undo walks back to the step's parent and redo
//! forward to the child redo last came from (the newest branch, after a
//! commit). Each walk reports a [`WalkedStep`]: where the step changed the
//! text and the selections before and after it, for the caller to place
//! the cursor.
//!
//! # Moving in time
//!
//! [`History::earlier`] and [`History::later`] name the state a count of
//! steps before or after the current one in the order the steps were
//! made, across branches, as Vim's `g-` and `g+` walk; [`History::step_toward`]
//! takes one undo or redo step along the tree to it, for the caller to
//! repeat until it is there.
//!
//! # Groups
//!
//! The host decides what one undo step is. Between [`History::begin_group`]
//! and [`History::end_group`], every commit merges into one step (the
//! changes composed, the selection before the first and after the last
//! kept), so an insert session undoes at once rather than key by key. Undo
//! and redo end an open group, so typing after them starts a new step.
//!
//! # Revisions
//!
//! [`History::revision`] names the text the history has walked to: every
//! commit makes a new revision, even one merged into a group's step, and
//! undo and redo walk back to the revisions they reach. Comparing it with
//! the revision a document was saved at tells whether it changed since.

use super::document::Document;
use super::selection::Selection;
use super::transaction::{ChangeError, ChangeSet};

// ========================================================================
// Data Structures
// ========================================================================

/// The undo and redo steps of one document.
#[derive(Clone, Debug, Default)]
pub struct History {
    /// The node whose step made the text as it stands; `None` for the text
    /// the history started with.
    current: Option<usize>,
    /// Whether a group is open, and whether it has a step yet.
    group: Group,
    /// The revision the last commit made; 0 for the text the history
    /// started with.
    last_revision: u64,
    /// Every step, in the order made, each knowing its parent.
    nodes: Vec<Node>,
    /// The first step redo takes from the text the history started with.
    root_child: Option<usize>,
}

/// An undo or redo step just walked: where it changed the text, and the
/// selections recorded with it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WalkedStep {
    /// The offset of the first char the step changed; the same offset in
    /// the text with and without the step.
    pub change_start: usize,
    /// The change just applied to the document: the step's change for a
    /// redo, its inverse for an undo; it maps places (marks, other cursors)
    /// across the walk.
    pub changes: ChangeSet,
    /// The selection after the step was made.
    pub selection_after: Selection,
    /// The selection before the step was made.
    pub selection_before: Selection,
}

/// Whether commits merge into the newest step.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum Group {
    /// No group is open: each commit is its own step.
    #[default]
    Closed,
    /// A group is open but has no step yet: the next commit starts one.
    Opened,
    /// A group is open and its step is the current one: commits merge
    /// into it.
    Started,
}

/// One step of the tree, and where it hangs.
#[derive(Clone, Debug)]
struct Node {
    /// The child redo last came from, or goes to first.
    child: Option<usize>,
    /// The step before it; `None` for one made on the starting text.
    parent: Option<usize>,
    /// The step.
    step: Step,
}

/// One undo step: a change, its inverse, and the selections around it.
#[derive(Clone, Debug)]
struct Step {
    /// The change, from the text before the step to the text after it.
    changes: ChangeSet,
    /// The change back, from the text after the step to the text before it.
    inverse: ChangeSet,
    /// The revision of the text after the step.
    revision: u64,
    /// The selection to restore on redo.
    selection_after: Selection,
    /// The selection to restore on undo.
    selection_before: Selection,
}

// ========================================================================
// History Implementations
// ========================================================================

impl History {
    /// An empty history, with nothing to undo or redo.
    pub fn new() -> Self {
        Self::default()
    }

    /// Opens a group: the commits until [`History::end_group`] make one undo
    /// step. Opening a group that is already open changes nothing.
    pub fn begin_group(&mut self) {
        if self.group == Group::Closed {
            self.group = Group::Opened;
        }
    }

    /// Applies `changes` to `document` and records them as an undo step, or
    /// merges them into the open group's step. `before` is the selection
    /// undo restores, `after` the one redo restores. A change that changes
    /// nothing is applied but not recorded.
    pub fn commit(
        &mut self,
        document: &mut Document,
        changes: ChangeSet,
        before: Selection,
        after: Selection,
    ) -> Result<(), ChangeError> {
        let inverse = changes.invert(document)?;
        changes.apply(document)?;
        if changes.is_identity() {
            return Ok(());
        }
        self.last_revision += 1;
        let step = Step {
            changes,
            inverse,
            revision: self.last_revision,
            selection_after: after,
            selection_before: before,
        };
        let merging = self.current.filter(|current| {
            let node = &self.nodes[*current];
            self.group == Group::Started && node.child.is_none() && node.step.is_followed_by(&step)
        });
        match merging {
            Some(current) => {
                let newest = self.nodes[current].step.clone();
                self.nodes[current].step = newest.merge(step)?;
            }
            None => self.push(step),
        }
        if self.group == Group::Opened {
            self.group = Group::Started;
        }
        Ok(())
    }

    /// A mark of where the history stands, for [`History::merge_since`].
    pub(crate) fn checkpoint(&self) -> usize {
        self.nodes.len()
    }

    /// Merges every step committed since `checkpoint` into one, so a
    /// command that made many edits (an ex command over many lines)
    /// undoes at once; steps that do not follow one another stay apart.
    /// Neighbors merge pairwise, round by round, so that n steps compose
    /// in about n log n rather than n squared.
    pub(crate) fn merge_since(&mut self, checkpoint: usize) -> Result<(), ChangeError> {
        let Some(current) = self.current else {
            return Ok(());
        };
        if current + 1 != self.nodes.len() || self.nodes.len() <= checkpoint + 1 {
            return Ok(());
        }
        let mut first = current;
        while first > checkpoint
            && self.nodes[first].parent == Some(first - 1)
            && self.nodes[first - 1]
                .step
                .is_followed_by(&self.nodes[first].step)
        {
            first -= 1;
        }
        let parent = self.nodes[first].parent;
        let mut steps: Vec<Step> = self
            .nodes
            .split_off(first)
            .into_iter()
            .map(|node| node.step)
            .collect();
        while steps.len() > 1 {
            let mut pairs = steps.into_iter();
            let mut merged = Vec::new();
            while let Some(step) = pairs.next() {
                merged.push(match pairs.next() {
                    Some(next) => step.merge(next)?,
                    None => step,
                });
            }
            steps = merged;
        }
        self.current = parent;
        for step in steps {
            self.push(step);
        }
        Ok(())
    }

    /// Closes the open group, if any: the next commit is a step of its own.
    pub fn end_group(&mut self) {
        self.group = Group::Closed;
    }

    /// The revision `count` states before the current one, in the order
    /// the steps were made across branches, as far as the start; `None`
    /// when at the start already.
    pub fn earlier(&self, count: usize) -> Option<u64> {
        let states = self.revisions();
        let at = states.binary_search(&self.revision()).ok()?;
        (at > 0).then(|| states[at.saturating_sub(count)])
    }

    /// The revision `count` states after the current one, in the order the
    /// steps were made across branches, as far as the newest; `None` when
    /// at the newest already.
    pub fn later(&self, count: usize) -> Option<u64> {
        let states = self.revisions();
        let at = states.binary_search(&self.revision()).ok()?;
        (at + 1 < states.len()).then(|| states[(at + count).min(states.len() - 1)])
    }

    /// Redoes the next undone step on `document` and reports it, or `None`
    /// when nothing is undone. A document whose length no longer matches
    /// the step is refused, and the step kept.
    pub fn redo(&mut self, document: &mut Document) -> Result<Option<WalkedStep>, ChangeError> {
        self.end_group();
        let child = match self.current {
            Some(current) => self.nodes[current].child,
            None => self.root_child,
        };
        let Some(child) = child else {
            return Ok(None);
        };
        self.walk_down(document, child).map(Some)
    }

    /// The revision of the text the history has reached: 0 before any
    /// commit or after undoing every step, a new number after each commit,
    /// and the number of the step reached after undo or redo.
    pub fn revision(&self) -> u64 {
        self.current
            .map_or(0, |current| self.nodes[current].step.revision)
    }

    /// Takes one step along the tree toward the state of `revision`: down
    /// toward it when it is after the current state on its branch, else
    /// back up. Reports the step walked and whether it was a redo; `None`
    /// once there, or when no state has the revision.
    pub fn step_toward(
        &mut self,
        revision: u64,
        document: &mut Document,
    ) -> Result<Option<(WalkedStep, bool)>, ChangeError> {
        self.end_group();
        let target = match revision {
            0 => None,
            revision => {
                let found = self
                    .nodes
                    .iter()
                    .position(|node| node.step.revision == revision);
                match found {
                    Some(found) => Some(found),
                    None => return Ok(None),
                }
            }
        };
        if target == self.current {
            return Ok(None);
        }
        let path = self.path_to(target);
        let next = match self.current {
            None => path.first().copied(),
            Some(current) => path
                .iter()
                .position(|node| *node == current)
                .map(|index| path[index + 1]),
        };
        match next {
            Some(next) => self
                .walk_down(document, next)
                .map(|walked| Some((walked, true))),
            None => self
                .undo(document)
                .map(|walked| walked.map(|walked| (walked, false))),
        }
    }

    /// Undoes the current step on `document` and reports it, or `None` when
    /// there is nothing to undo. A document whose length no longer matches
    /// the step is refused, and the step kept.
    pub fn undo(&mut self, document: &mut Document) -> Result<Option<WalkedStep>, ChangeError> {
        self.end_group();
        let Some(current) = self.current else {
            return Ok(None);
        };
        let node = &self.nodes[current];
        node.step.inverse.apply(document)?;
        let walked = node.step.walked(&node.step.inverse);
        let parent = node.parent;
        match parent {
            Some(parent) => self.nodes[parent].child = Some(current),
            None => self.root_child = Some(current),
        }
        self.current = parent;
        Ok(Some(walked))
    }

    /// The nodes from the first step after the starting text down to
    /// `target`, in order; empty for the starting text.
    fn path_to(&self, target: Option<usize>) -> Vec<usize> {
        let mut path = Vec::new();
        let mut at = target;
        while let Some(node) = at {
            path.push(node);
            at = self.nodes[node].parent;
        }
        path.reverse();
        path
    }

    /// Adds `step` after the current one, as the branch redo takes next,
    /// and makes it current.
    fn push(&mut self, step: Step) {
        let index = self.nodes.len();
        self.nodes.push(Node {
            child: None,
            parent: self.current,
            step,
        });
        match self.current {
            Some(current) => self.nodes[current].child = Some(index),
            None => self.root_child = Some(index),
        }
        self.current = Some(index);
    }

    /// The revisions of every state, the starting text's 0 among them,
    /// in the order made.
    fn revisions(&self) -> Vec<u64> {
        let mut revisions: Vec<u64> = self.nodes.iter().map(|node| node.step.revision).collect();
        revisions.push(0);
        revisions.sort_unstable();
        revisions
    }

    /// Redoes `child`, a child of the current step, on `document` and
    /// makes it current.
    fn walk_down(
        &mut self,
        document: &mut Document,
        child: usize,
    ) -> Result<WalkedStep, ChangeError> {
        let step = &self.nodes[child].step;
        step.changes.apply(document)?;
        let walked = step.walked(&step.changes);
        match self.nodes[child].parent {
            Some(parent) => self.nodes[parent].child = Some(child),
            None => self.root_child = Some(child),
        }
        self.current = Some(child);
        Ok(walked)
    }
}

// ========================================================================
// Step Implementations
// ========================================================================

impl Step {
    /// Whether `next` applies to the text this step leaves, so the two can
    /// merge; not so when the document was edited outside the history in
    /// between, and then `next` stays a step of its own.
    fn is_followed_by(&self, next: &Self) -> bool {
        self.changes.len_after() == next.changes.len_before()
    }

    /// The report of walking this step by `applied`, its change or its
    /// inverse.
    fn walked(&self, applied: &ChangeSet) -> WalkedStep {
        WalkedStep {
            change_start: applied.first_change().unwrap_or_default(),
            changes: applied.clone(),
            selection_after: self.selection_after.clone(),
            selection_before: self.selection_before.clone(),
        }
    }

    /// This step followed by `next`, as one step: the changes composed, the
    /// selection before this step and after `next`.
    fn merge(self, next: Self) -> Result<Self, ChangeError> {
        Ok(Self {
            changes: self.changes.compose(next.changes)?,
            inverse: next.inverse.compose(self.inverse)?,
            revision: next.revision,
            selection_after: next.selection_after,
            selection_before: self.selection_before,
        })
    }
}

// ========================================================================
// Tests
// ========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Edit, LengthMismatch};

    /// Types `text` at `offset` of `document` as one commit, moving a cursor
    /// from `offset` to just past the text.
    fn type_at(history: &mut History, document: &mut Document, offset: usize, text: &str) {
        let changes =
            ChangeSet::from_edits(document.len_chars(), [Edit::new(offset..offset, text)])
                .expect("the edit fits");
        let after = Selection::point(offset + text.chars().count());
        history
            .commit(document, changes, Selection::point(offset), after)
            .expect("the change fits the document");
    }

    /// Undoes a step, returning the selection from before it.
    fn undone(history: &mut History, document: &mut Document) -> Option<Selection> {
        let walked = history.undo(document).expect("the step fits");
        walked.map(|walked| walked.selection_before)
    }

    /// Redoes a step, returning the selection from after it.
    fn redone(history: &mut History, document: &mut Document) -> Option<Selection> {
        let walked = history.redo(document).expect("the step fits");
        walked.map(|walked| walked.selection_after)
    }

    #[test]
    fn test_undo_and_redo_step_through_the_commits_with_their_selections() {
        let (mut history, mut document) = (History::new(), Document::new("ac"));
        type_at(&mut history, &mut document, 1, "b");
        type_at(&mut history, &mut document, 3, "d");
        assert_eq!(document.to_string(), "abcd");

        assert_eq!(
            undone(&mut history, &mut document),
            Some(Selection::point(3))
        );
        assert_eq!(document.to_string(), "abc");
        assert_eq!(
            undone(&mut history, &mut document),
            Some(Selection::point(1))
        );
        assert_eq!(document.to_string(), "ac");
        assert_eq!(undone(&mut history, &mut document), None);

        assert_eq!(
            redone(&mut history, &mut document),
            Some(Selection::point(2))
        );
        assert_eq!(document.to_string(), "abc");
        assert_eq!(
            redone(&mut history, &mut document),
            Some(Selection::point(4))
        );
        assert_eq!(document.to_string(), "abcd");
        assert_eq!(redone(&mut history, &mut document), None);
    }

    #[test]
    fn test_a_commit_after_undo_branches_and_redo_takes_the_new_branch() {
        let (mut history, mut document) = (History::new(), Document::new(""));
        type_at(&mut history, &mut document, 0, "one");
        type_at(&mut history, &mut document, 3, " two");
        history.undo(&mut document).unwrap();
        type_at(&mut history, &mut document, 3, " three");
        assert_eq!(redone(&mut history, &mut document), None);
        assert_eq!(document.to_string(), "one three");
        history.undo(&mut document).unwrap();
        assert_eq!(document.to_string(), "one");
    }

    #[test]
    fn test_earlier_and_later_walk_every_state_in_the_order_made_across_branches() {
        let (mut history, mut document) = (History::new(), Document::new(""));
        type_at(&mut history, &mut document, 0, "one");
        type_at(&mut history, &mut document, 3, " two");
        history.undo(&mut document).unwrap();
        type_at(&mut history, &mut document, 3, " three");
        let walk_to = |history: &mut History, document: &mut Document, revision| {
            while history.step_toward(revision, document).unwrap().is_some() {}
            document.to_string()
        };
        let earlier = history.earlier(1).unwrap();
        assert_eq!(
            walk_to(&mut history, &mut document, earlier),
            "one two",
            "the other branch"
        );
        let earlier = history.earlier(1).unwrap();
        assert_eq!(walk_to(&mut history, &mut document, earlier), "one");
        assert_eq!(history.earlier(5), Some(0), "as far as the start");
        let later = history.later(2).unwrap();
        assert_eq!(walk_to(&mut history, &mut document, later), "one three");
        assert_eq!(history.later(1), None, "the newest already");
    }

    #[test]
    fn test_commits_in_a_group_undo_as_one_step() {
        let (mut history, mut document) = (History::new(), Document::new("x"));
        type_at(&mut history, &mut document, 0, "<");
        history.begin_group();
        type_at(&mut history, &mut document, 2, "h");
        type_at(&mut history, &mut document, 3, "i");
        type_at(&mut history, &mut document, 4, "!");
        history.end_group();
        type_at(&mut history, &mut document, 5, ">");
        assert_eq!(document.to_string(), "<xhi!>");

        history.undo(&mut document).unwrap();
        assert_eq!(
            undone(&mut history, &mut document),
            Some(Selection::point(2))
        );
        assert_eq!(document.to_string(), "<x");
        assert_eq!(
            redone(&mut history, &mut document),
            Some(Selection::point(5))
        );
        assert_eq!(document.to_string(), "<xhi!");
    }

    #[test]
    fn test_undo_ends_an_open_group() {
        let (mut history, mut document) = (History::new(), Document::new(""));
        history.begin_group();
        type_at(&mut history, &mut document, 0, "a");
        type_at(&mut history, &mut document, 1, "b");
        history.undo(&mut document).unwrap();
        type_at(&mut history, &mut document, 0, "c");
        type_at(&mut history, &mut document, 1, "d");
        history.undo(&mut document).unwrap();
        assert_eq!(document.to_string(), "c");
    }

    #[test]
    fn test_a_change_that_changes_nothing_is_no_undo_step() {
        let (mut history, mut document) = (History::new(), Document::new("text"));
        type_at(&mut history, &mut document, 4, "!");
        history.undo(&mut document).unwrap();
        let nothing = ChangeSet::from_edits(4, []).unwrap();
        history
            .commit(
                &mut document,
                nothing,
                Selection::point(0),
                Selection::point(0),
            )
            .unwrap();
        assert_eq!(
            redone(&mut history, &mut document),
            Some(Selection::point(5))
        );
        assert_eq!(document.to_string(), "text!");
    }

    #[test]
    fn test_undo_refuses_a_document_changed_behind_its_back_and_keeps_the_step() {
        let (mut history, mut document) = (History::new(), Document::new("ab"));
        type_at(&mut history, &mut document, 2, "c");
        let mut other = Document::new("unrelated");
        assert_eq!(
            history.undo(&mut other),
            Err(ChangeError::LengthMismatch(LengthMismatch {
                actual: 9,
                expected: 3,
            }))
        );
        assert_eq!(other.to_string(), "unrelated");
        assert_eq!(
            undone(&mut history, &mut document),
            Some(Selection::point(2))
        );
        assert_eq!(document.to_string(), "ab");
    }

    #[test]
    fn test_steps_since_a_checkpoint_merge_into_one_with_their_outer_selections() {
        let (mut history, mut document) = (History::new(), Document::new(""));
        type_at(&mut history, &mut document, 0, "a");
        let checkpoint = history.checkpoint();
        for (offset, text) in [(1, "b"), (2, "c"), (0, "x"), (4, "d"), (5, "e")] {
            type_at(&mut history, &mut document, offset, text);
        }
        history.merge_since(checkpoint).unwrap();
        assert_eq!(document.to_string(), "xabcde");
        assert_eq!(
            undone(&mut history, &mut document),
            Some(Selection::point(1))
        );
        assert_eq!(document.to_string(), "a");
        assert_eq!(
            redone(&mut history, &mut document),
            Some(Selection::point(6))
        );
        assert_eq!(document.to_string(), "xabcde");
        undone(&mut history, &mut document);
        undone(&mut history, &mut document);
        assert_eq!(document.to_string(), "", "the step before stays apart");
    }

    #[test]
    fn test_revisions_follow_undo_and_redo_and_change_inside_a_group() {
        let (mut history, mut document) = (History::new(), Document::new(""));
        assert_eq!(history.revision(), 0);
        type_at(&mut history, &mut document, 0, "a");
        let first = history.revision();
        history.begin_group();
        type_at(&mut history, &mut document, 1, "b");
        let grouped = history.revision();
        type_at(&mut history, &mut document, 2, "c");
        assert_ne!(history.revision(), grouped, "a merge is a new revision");
        let newest = history.revision();
        history.undo(&mut document).unwrap();
        assert_eq!(history.revision(), first);
        history.redo(&mut document).unwrap();
        assert_eq!(history.revision(), newest);
        history.undo(&mut document).unwrap();
        history.undo(&mut document).unwrap();
        assert_eq!(history.revision(), 0);
        type_at(&mut history, &mut document, 0, "z");
        assert!(![0, first, newest].contains(&history.revision()));
    }
}
