//! Edits of a document's text and walks of its undo history, and the
//! lines `:g` and `:norm` run commands on.

use crate::document::last_line;
use crate::document::{Position, normalize_line_breaks, start_of_line};
use crate::ex::{LineAnchors, lines_insertion};
use crate::history::History;
use crate::line_diff::line_edits;
use crate::mark::{CHANGE_END_MARK, CHANGE_START_MARK, LAST_CHANGE_MARK};
use crate::motion::first_non_blank_of_line;
use crate::selection::{Range, Selection};
use crate::snippet;
use crate::transaction::{Assoc, ChangeSet, Edit};
use crate::view::Mode;

use super::fields::join_lines;
use super::{DocumentId, Editor, HistoryStep, ViewId};

// ========================================================================
// Editor Implementations
// ========================================================================

impl Editor {
    /// Makes `edits`, ranges of the document of `view` as it is now, as one
    /// undo step (or part of the open group), and carries the selection of
    /// every view onto that document across them. A failed edit, or any
    /// edit of a read-only document, changes nothing and queues a notice.
    /// Does nothing for a view the editor lacks.
    pub fn apply_edits(&mut self, view: ViewId, edits: Vec<Edit>) {
        if !self.views.contains_key(view) || self.refuse_if_read_only(view) {
            return;
        }
        self.commit_edits(view, edits);
    }

    /// Puts `text`, the file of the document `id` as just read again, in
    /// place of its text, as one undo step, and counts the document saved:
    /// what a host does when the file changes on disk. Only the lines that
    /// differ are edited, so the cursor, marks, and folds of every view stay
    /// with the text around them. Line breaks are normalized as
    /// [`Editor::open_document`] does; the document keeps the line ending it
    /// was opened with. A read-only document is reloaded too. Returns
    /// whether the text changed; nothing happens for a document the editor
    /// lacks or no view shows.
    pub fn reload_document(&mut self, id: DocumentId, text: &str) -> bool {
        let Some(view) = self
            .views
            .iter()
            .find_map(|(view, state)| (state.document() == id).then_some(view))
        else {
            return false;
        };
        let current = self.documents[id].document.to_string();
        let edits = line_edits(&current, &normalize_line_breaks(text));
        let changed = !edits.is_empty();
        if changed {
            self.commit_edits(view, edits);
        }
        self.mark_saved(id);
        changed
    }

    /// Makes `edits` as [`Editor::apply_edits`] does, read-only or not, for
    /// a view the editor has.
    fn commit_edits(&mut self, view: ViewId, edits: Vec<Edit>) {
        let document_id = self.views[view].document();
        let open = &mut self.documents[document_id];
        let edits = if open.field_editing.is_some() {
            edits
                .into_iter()
                .map(|edit| Edit::new(edit.range, join_lines(&edit.text)))
                .collect()
        } else {
            edits
        };
        let changes = match ChangeSet::from_edits(open.document.len_chars(), edits) {
            Ok(changes) => changes,
            Err(err) => return self.error(err.to_string()),
        };
        let before = self.views[view].selection().clone();
        let after = changes.map_selection(&before);
        let text_before = open.changes.is_tracking().then(|| open.document.clone());
        let committed = open
            .history
            .commit(&mut open.document, changes.clone(), before, after);
        if let Err(err) = committed {
            return self.error(err.to_string());
        }
        open.changes.record(&changes, text_before.as_ref());
        self.edit_count += 1;
        for state in self.views.values_mut() {
            if state.document() == document_id {
                state.set_region_mark(None);
                state
                    .folds_mut()
                    .map(|offset| changes.map_position(offset, Assoc::After));
            }
        }
        let map = |offset| changes.map_position(offset, Assoc::Before);
        open.marks.map(map);
        open.changelist.map(map);
        open.last_cursor = map(open.last_cursor);
        if let Some(range) = changes.changed_range() {
            let end = changes.map_position(range.end, Assoc::After);
            let last = match end > range.start {
                true => open.document.prev_grapheme_boundary(end),
                false => range.start,
            };
            open.marks.set(CHANGE_START_MARK, range.start);
            open.marks.set(CHANGE_END_MARK, last);
            open.marks.set(LAST_CHANGE_MARK, last);
            open.changelist.record(&open.document, last);
        }
        open.decorations.map(&changes);
        for anchors in &mut open.line_anchors {
            anchors.map(&changes);
        }
        if let Some(highlighter) = &mut open.highlighter {
            highlighter.edit(&changes, &open.document);
            open.highlight_version += 1;
        }
        for other in self.views.values_mut() {
            if other.document() == document_id {
                other.set_selection(changes.map_selection(other.selection()));
                other.map_offsets(map);
                other.jumps_mut().map(map);
                if let Some(snippet) = other.snippet_mut() {
                    snippet.map(&changes);
                }
            }
        }
    }

    /// Puts `text`, whole lines (a final line break is optional), into
    /// the document of `view` below line `below`, counted from 1 (0 puts
    /// them above the first line), as one undo step, with the cursor on
    /// the first non-blank of the first line put: what a host does with
    /// the text of a [`ReadIntoRequest`](crate::ReadIntoRequest). Does
    /// nothing for a view the editor lacks.
    pub fn insert_lines(&mut self, view: ViewId, below: usize, text: &str) {
        let Some(state) = self.views.get(view) else {
            return;
        };
        let document = &self.documents[state.document()].document;
        let below = below.min(last_line(document) + 1);
        let edit = lines_insertion(document, below, text);
        self.apply_edits(view, vec![edit]);
        let start = start_of_line(self.document_of(view), below);
        let target = first_non_blank_of_line(self, view, start);
        self.set_cursor(view, target);
    }

    /// Types `snippet`, in the syntax language servers use (`$1`,
    /// `${2:default}`, `$0`), at the cursor of `view` and goes to its first
    /// tab stop, for `<tab>` and `S-<tab>` to walk, as the docs of snippets
    /// say. Does nothing outside Insert mode.
    pub fn insert_snippet(&mut self, view: ViewId, snippet: &str) {
        if self
            .views
            .get(view)
            .is_some_and(|state| state.mode() == Mode::Insert)
        {
            snippet::insert(self, view, snippet);
        }
    }

    /// The undo history of the document `view` shows.
    pub(crate) fn history_mut(&mut self, view: ViewId) -> &mut History {
        &mut self.documents[self.views[view].document()].history
    }

    /// How many edits have been made.
    pub(crate) fn edit_count(&self) -> u64 {
        self.edit_count
    }

    /// Takes one `step` through the history of the document `view` shows,
    /// placing the cursor of `view` where Vim does (see
    /// [`cursor_after_walk`]) and keeping every other view onto the
    /// document inside the text. Returns whether a step was taken; when
    /// none was, a notice says why.
    pub(crate) fn step_history(&mut self, view: ViewId, step: HistoryStep) -> bool {
        if self.refuse_if_read_only(view) {
            return false;
        }
        let locked = self.is_insert_locked(view);
        if locked {
            self.history_mut(view).end_group();
        }
        let stepped = self.walk_history(view, step);
        if locked {
            self.history_mut(view).begin_group();
        }
        stepped
    }

    /// Takes one `step` through the history of the document `view` shows,
    /// as [`Editor::step_history`] describes, with no read-only check.
    fn walk_history(&mut self, view: ViewId, step: HistoryStep) -> bool {
        let document_id = self.views[view].document();
        let open = &mut self.documents[document_id];
        let may_redo = step != HistoryStep::Undo;
        let before = (may_redo || open.changes.is_tracking()).then(|| open.document.clone());
        let walked = match step {
            HistoryStep::Redo => open
                .history
                .redo(&mut open.document)
                .map(|walked| walked.map(|walked| (walked, true))),
            HistoryStep::Toward(revision) => open.history.step_toward(revision, &mut open.document),
            HistoryStep::Undo => open
                .history
                .undo(&mut open.document)
                .map(|walked| walked.map(|walked| (walked, false))),
        };
        let (walked, is_redo) = match walked {
            Ok(Some(walked)) => walked,
            Ok(None) => {
                let message = match step {
                    HistoryStep::Redo => "Already at newest change",
                    HistoryStep::Toward(_) => return false,
                    HistoryStep::Undo => "Already at oldest change",
                };
                self.error(message.to_string());
                return false;
            }
            Err(err) => {
                self.error(err.to_string());
                return false;
            }
        };
        open.changes.record(&walked.changes, before.as_ref());
        for other in self.views.values_mut() {
            if other.document() == document_id {
                other.set_snippet(None);
                other.set_region_mark(None);
                other
                    .folds_mut()
                    .map(|offset| walked.changes.map_position(offset, Assoc::After));
            }
        }
        let open = &mut self.documents[document_id];
        let map = |offset| walked.changes.map_position(offset, Assoc::Before);
        open.marks.map(map);
        open.changelist.map(map);
        open.last_cursor = map(open.last_cursor);
        open.decorations.map(&walked.changes);
        for anchors in &mut open.line_anchors {
            anchors.map(&walked.changes);
        }
        if let Some(highlighter) = &mut open.highlighter {
            highlighter.edit(&walked.changes, &open.document);
            open.highlight_version += 1;
        }
        let text_before_step = before
            .as_ref()
            .filter(|_| is_redo)
            .unwrap_or(&open.document);
        let cursor_before = text_before_step
            .char_to_position(walked.selection_before.primary().head)
            .unwrap_or_default();
        let len = open.document.len_chars();
        for (id, other) in &mut self.views {
            if other.document() != document_id {
                continue;
            }
            other.extra_cursors_mut().map(map);
            other.map_offsets(|offset| offset.min(len));
            other.jumps_mut().map(map);
            if id != view {
                other.set_selection(clamp_selection(other.selection(), len));
            }
        }
        let cursor = cursor_after_walk(self, view, cursor_before, walked.change_start);
        self.set_cursor(view, cursor);
        true
    }

    /// Starts following `anchors`, lines a command is to run on, through
    /// edits to the document of `view`.
    pub(crate) fn push_line_anchors(&mut self, view: ViewId, anchors: LineAnchors) {
        self.documents[self.views[view].document()]
            .line_anchors
            .push(anchors);
    }

    /// Where the next line of the newest anchors pushed in the document of
    /// `view` starts, skipping lines deleted or joined away; `None` when
    /// none is left.
    pub(crate) fn next_line_anchor(&mut self, view: ViewId) -> Option<usize> {
        let open = &mut self.documents[self.views[view].document()];
        open.line_anchors.last_mut()?.next_line(&open.document)
    }

    /// Stops following the newest anchors pushed in the document of `view`.
    pub(crate) fn pop_line_anchors(&mut self, view: ViewId) {
        self.documents[self.views[view].document()]
            .line_anchors
            .pop();
    }

    /// Merges the steps of the history of the document `view` shows since
    /// `checkpoint` into one undo step.
    pub(crate) fn merge_history_since(&mut self, view: ViewId, checkpoint: usize) {
        if let Err(err) = self.history_mut(view).merge_since(checkpoint) {
            self.error(err.to_string());
        }
    }
}

// ========================================================================
// Functions
// ========================================================================

/// `selection` with every offset past `len` brought back to `len`.
fn clamp_selection(selection: &Selection, len: usize) -> Selection {
    let clamp = |range: Range| Range::new(range.anchor.min(len), range.head.min(len));
    let others = selection
        .ranges()
        .iter()
        .enumerate()
        .filter(|(index, _)| *index != selection.primary_index())
        .map(|(_, range)| clamp(*range));
    Selection::new(clamp(selection.primary()), others)
}

/// Where the cursor of `view` goes after an undo or redo that changed the
/// text from `change_start` on, given where the cursor was before the step
/// was first made: Vim's rule, the same for undo and redo. On the line of
/// the first change (or the line above it, which is where `o` was typed),
/// the cursor takes the column it had; on any other line it goes to the
/// first non-blank of the first changed line. Either way, it lands at the
/// start of the change in the usual case of a change made at the cursor.
fn cursor_after_walk(
    editor: &Editor,
    view: ViewId,
    cursor_before: Position,
    change_start: usize,
) -> usize {
    let document = editor.document_of(view);
    let changed_line = document
        .char_to_position(change_start.min(document.len_chars()))
        .map_or(0, |position| position.line);
    let on_line_above = cursor_before.line + 1 == changed_line;
    if cursor_before.line == changed_line || on_line_above {
        return document
            .position_to_char(cursor_before)
            .unwrap_or(change_start);
    }
    let line_start = document
        .position_to_char(Position {
            column: 0,
            line: changed_line,
        })
        .unwrap_or(change_start);
    first_non_blank_of_line(editor, view, line_start)
}
