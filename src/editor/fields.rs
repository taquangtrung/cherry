//! Single-line and read-only documents: how a field is edited, the
//! history it recalls, and the edits a read-only document refuses.

use crate::document::normalize_line_breaks;
use crate::insert;
use crate::prompt::Recall;
use crate::transaction::Edit;
use crate::view::Mode;

use super::{DocumentId, Editor, FieldEditing, READ_ONLY_NOTICE, ViewId};

// ========================================================================
// Editor Implementations
// ========================================================================

impl Editor {
    /// How the single-line document `id` is edited
    /// ([`Editor::set_field_editing`]); `None` for a document of many
    /// lines, or when the editor has no such document.
    pub fn field_editing(&self, id: DocumentId) -> Option<FieldEditing> {
        self.documents.get(id)?.field_editing
    }

    /// Whether the document `id` is read-only ([`Editor::set_read_only`]);
    /// false when the editor has no such document.
    pub fn is_read_only(&self, id: DocumentId) -> bool {
        self.documents.get(id).is_some_and(|open| open.read_only)
    }

    /// Whether the document `id` was opened as a single-line document
    /// ([`Editor::open_single_line_document`]); false when the editor has
    /// no such document.
    pub fn is_single_line(&self, id: DocumentId) -> bool {
        self.documents
            .get(id)
            .is_some_and(|open| open.field_editing.is_some())
    }

    /// Makes `entries`, oldest first, the earlier answers the single-line
    /// document `id` recalls: in Insert mode with no completion menu open,
    /// `<up>` and `C-p` show the next older one, `<down>` and `C-n` the
    /// next newer, and past the newest the text typed before recalling
    /// comes back. Each recall is an edit of the whole text, the cursor at
    /// its end. Keeping the answers is the host's: add the one submitted
    /// and set them again, which also starts recalling from the newest.
    /// Does nothing for a document of many lines or one the editor lacks.
    pub fn set_field_history(&mut self, id: DocumentId, entries: Vec<String>) {
        let Some(open) = self.documents.get_mut(id) else {
            return;
        };
        if open.field_editing.is_some() {
            open.field_history = entries;
            open.field_recall = Recall::default();
        }
    }

    /// Shows the next `older` (else newer) entry of the field history of
    /// the single-line document `view` shows, in place of its text;
    /// returns whether the document has a field history to recall.
    pub(crate) fn recall_field_entry(&mut self, view: ViewId, older: bool) -> bool {
        let id = self.views[view].document();
        let open = &mut self.documents[id];
        if open.field_history.is_empty() {
            return false;
        }
        let typed = open.document.to_string();
        let shown = if older {
            open.field_recall.older(&open.field_history, &typed)
        } else {
            open.field_recall.newer(&open.field_history)
        };
        if let Some(text) = shown {
            let end = text.chars().count();
            self.apply_edits(view, vec![Edit::new(0..typed.chars().count(), text)]);
            self.set_cursor(view, end);
        }
        true
    }

    /// Edits the single-line document `id` as `editing` says from now on
    /// ([`FieldEditing`]). A view of it outside Insert mode (in Normal or a
    /// visual mode, an operator waiting, Replace mode, or a prompt open)
    /// goes to Insert mode for [`FieldEditing::Emacs`], dropping what was
    /// pending; for [`FieldEditing::Modal`] every view stays as it is.
    /// Does nothing for a document of many lines, or one the editor lacks.
    pub fn set_field_editing(&mut self, id: DocumentId, editing: FieldEditing) {
        let Some(open) = self.documents.get_mut(id) else {
            return;
        };
        if open.field_editing.is_none() {
            return;
        }
        open.field_editing = Some(editing);
        let views: Vec<ViewId> = self
            .views
            .iter()
            .filter(|(_, state)| state.document() == id)
            .map(|(view, _)| view)
            .collect();
        for view in views {
            self.lock_field_in_insert(view);
        }
    }

    /// Makes the document `id` read-only, or editable again, as for a
    /// preview: motions, search, yanks, visual selections, and marks still
    /// work, while every edit (typed, an operator other than yank, undo
    /// and redo, an ex command, and the host's own [`Editor::apply_edits`]
    /// and [`Editor::insert_lines`]) is refused with a notice, once per
    /// command, and makes no change; Insert and Replace mode are not
    /// entered, and a view of it in either goes back to Normal mode. A
    /// single-line document edited with Emacs keys ([`FieldEditing::Emacs`])
    /// is the exception: its views stay in Insert mode, where the cursor
    /// moves and every key that would edit gives the notice. Does nothing
    /// for a document the editor lacks.
    pub fn set_read_only(&mut self, id: DocumentId, read_only: bool) {
        if !self.documents.contains_key(id) {
            return;
        }
        if read_only {
            let inserting: Vec<ViewId> = self
                .views
                .iter()
                .filter(|(_, state)| state.document() == id)
                .filter(|(_, state)| matches!(state.mode(), Mode::Insert | Mode::Replace))
                .map(|(view, _)| view)
                .filter(|view| !self.is_insert_locked(*view))
                .collect();
            for view in inserting {
                insert::exit_insert_session(self, view);
            }
        }
        self.documents[id].read_only = read_only;
    }

    /// Whether `view` shows a single-line document edited with Emacs keys
    /// ([`FieldEditing::Emacs`]), which never leaves Insert mode.
    pub(crate) fn is_insert_locked(&self, view: ViewId) -> bool {
        self.documents[self.views[view].document()].field_editing == Some(FieldEditing::Emacs)
    }

    /// Whether `view` shows a single-line document.
    pub(crate) fn shows_single_line(&self, view: ViewId) -> bool {
        self.documents[self.views[view].document()]
            .field_editing
            .is_some()
    }

    /// Whether the document of `view` is read-only, in which case the
    /// command running gives the read-only notice, unless it gave it
    /// already.
    pub(crate) fn refuse_if_read_only(&mut self, view: ViewId) -> bool {
        if !self.documents[self.views[view].document()].read_only {
            return false;
        }
        if self.read_only_notice_seq != Some(self.command_seq) {
            self.read_only_notice_seq = Some(self.command_seq);
            self.error(READ_ONLY_NOTICE.to_string());
        }
        true
    }
}

// ========================================================================
// Functions
// ========================================================================

/// `text` with each line break (`\n`, `\r\n`, or a lone `\r`) made a
/// space, for a single-line document.
pub(super) fn join_lines(text: &str) -> String {
    normalize_line_breaks(text).replace('\n', " ")
}
