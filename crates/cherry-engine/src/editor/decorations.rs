//! What a host lays over a document's text: decorations, signs,
//! inlays, virtual lines, conceals, and the highlighter.

use std::ops;

use crate::decoration::{
    ChangedLine, Decoration, Decorations, GutterLane, Inlay, Sign, VirtualLine,
};
use crate::highlight::{Highlighter, Preparation};
use crate::render::visible_chars;

use super::{DocumentId, Editor, ViewId};

// ========================================================================
// Editor Implementations
// ========================================================================

impl Editor {
    /// Drops every conceal, decoration, inlay, and sign `owner` set on the
    /// document `id`; other owners' stay. Does nothing for a document the editor lacks.
    pub fn clear_decorations(&mut self, id: DocumentId, owner: &str) {
        if let Some(open) = self.documents.get_mut(id) {
            open.decorations.clear(owner);
        }
    }

    /// The ranges `owner` conceals in the document `id`, in the order it
    /// gave them, as edits since have moved them (dropped ones left out);
    /// empty when the editor has no such document or owner.
    pub fn conceals(&self, id: DocumentId, owner: &str) -> &[ops::Range<usize>] {
        self.documents
            .get(id)
            .map_or(&[], |open| open.decorations.conceals(owner))
    }

    /// The chars in sight in `view`, and the highlight version of its
    /// document, when its highlighter has not said they are ready; `None`
    /// when they are, or the document has no highlighter.
    pub(super) fn unprepared(&self, view: ViewId) -> Option<(u64, ops::Range<usize>)> {
        let state = self.views.get(view)?;
        let open = self.documents.get(state.document())?;
        open.highlighter.as_ref()?;
        let wanted = (open.highlight_version, visible_chars(&open.document, state));
        (self.highlighted.get(&view) != Some(&wanted)).then_some(wanted)
    }

    /// Has each view's highlighter prepare a budget of the lines in sight
    /// it has not said are ready; whether any did, so the host repaints.
    pub(super) fn prepare_highlights(&mut self) -> bool {
        let budget = self.options.highlight_budget;
        let view_ids: Vec<_> = self.views.keys().collect();
        let mut prepared = false;
        for view in view_ids {
            let Some((version, range)) = self.unprepared(view) else {
                continue;
            };
            let open = &mut self.documents[self.views[view].document()];
            let Some(highlighter) = open.highlighter.as_mut() else {
                continue;
            };
            let done = highlighter.prepare(&open.document, range.clone(), budget);
            if done == Preparation::Ready {
                self.highlighted.insert(view, (version, range));
            }
            prepared = true;
        }
        prepared
    }

    /// The decorations `owner` set on the document `id`, in the order it
    /// gave them, as edits since have moved them (dropped ones left out);
    /// empty when the editor has no such document or owner.
    pub fn decorations(&self, id: DocumentId, owner: &str) -> &[Decoration] {
        self.documents
            .get(id)
            .map_or(&[], |open| open.decorations.decorations(owner))
    }

    /// The inlays `owner` set on the document `id`, in the order it gave
    /// them, as edits since have moved them; empty when the editor has no
    /// such document or owner.
    pub fn inlays(&self, id: DocumentId, owner: &str) -> &[Inlay] {
        self.documents
            .get(id)
            .map_or(&[], |open| open.decorations.inlays(owner))
    }

    /// The highlighter of the document `id`, if the host set one; for a
    /// command to ask it for structure with
    /// [`Highlighter::text_object`](crate::Highlighter::text_object).
    pub fn highlighter(&self, id: DocumentId) -> Option<&dyn Highlighter> {
        self.documents.get(id)?.highlighter.as_deref()
    }

    /// Makes `conceals` the char ranges `owner` hides in the document
    /// `id`, in place of any it set before; other owners' stay. Concealed
    /// text takes no cells, except on the line a view's cursor is on,
    /// where that view shows it; every motion, edit, search, and yank
    /// still sees it. Ranges are clamped to the text and turned forward,
    /// an empty one is dropped, and edits carry them along as they carry
    /// decorations. Does nothing for a document the editor lacks.
    pub fn set_conceals(&mut self, id: DocumentId, owner: &str, conceals: Vec<ops::Range<usize>>) {
        if let Some(open) = self.documents.get_mut(id) {
            open.decorations
                .set_conceals(&open.document, owner, conceals);
        }
    }

    /// Makes `decorations` the ones `owner` (a name the host picks, such as
    /// `"lsp"`) has on the document `id`, in place of any it set before;
    /// other owners' stay. Ranges past the end are cut to it. Edits then
    /// carry them along (see [`Decoration`]); [`render_view`](crate::render_view)
    /// paints them. Does nothing for a document the editor lacks.
    pub fn set_decorations(&mut self, id: DocumentId, owner: &str, decorations: Vec<Decoration>) {
        if let Some(open) = self.documents.get_mut(id) {
            open.decorations
                .set_decorations(&open.document, owner, decorations);
        }
    }

    /// Makes `highlighter` the one that finds the highlighted spans of the
    /// document `id`, in place of any it had; `None` drops it, and the
    /// text paints plain. The editor tells it of every change from now on,
    /// so it must be made for the document as it is now. Does nothing for
    /// a document the editor lacks.
    pub fn set_highlighter(&mut self, id: DocumentId, highlighter: Option<Box<dyn Highlighter>>) {
        if let Some(open) = self.documents.get_mut(id) {
            open.highlighter = highlighter;
            open.highlight_version += 1;
        }
    }

    /// Makes `inlays` the ones `owner` lays into the lines of the document
    /// `id`, in place of any it set before; other owners' stay. An inlay
    /// past the end of the text is dropped, and line breaks in its text
    /// show as spaces. Edits carry them along, and every view lays them
    /// out, paints, scrolls, and takes clicks with them (see [`Inlay`]).
    /// Does nothing for a document the editor lacks.
    pub fn set_inlays(&mut self, id: DocumentId, owner: &str, inlays: Vec<Inlay>) {
        if let Some(open) = self.documents.get_mut(id) {
            open.decorations.set_inlays(&open.document, owner, inlays);
        }
    }

    /// Makes `changes` the lines `owner` marks as changed in the document
    /// `id` (since its last commit, say), in place of any it set before;
    /// other owners' stay. Each paints its change as a layer
    /// ([`Layers::CHANGE_ADDED`](crate::Layers::CHANGE_ADDED) and its kin)
    /// on the sign column's first cell, beside the line's sign: every row
    /// of an added or modified line, the first of one below a deletion.
    /// They show when [`EditorOptions::sign_column`](crate::EditorOptions::sign_column)
    /// is on, and move with their lines through edits as signs do. Does
    /// nothing for a document the editor lacks.
    pub fn set_line_changes(&mut self, id: DocumentId, owner: &str, changes: Vec<ChangedLine>) {
        if let Some(open) = self.documents.get_mut(id) {
            open.decorations
                .set_line_changes(&open.document, owner, changes);
        }
    }

    /// Makes `signs` the ones `owner` has in the sign column of the
    /// document `id`, in place of any it set before; other owners' stay.
    /// They show when [`EditorOptions::sign_column`](crate::EditorOptions::sign_column) is on. Does nothing
    /// for a document the editor lacks.
    pub fn set_signs(&mut self, id: DocumentId, owner: &str, signs: Vec<Sign>) {
        if let Some(open) = self.documents.get_mut(id) {
            open.decorations.set_signs(&open.document, owner, signs);
        }
    }

    /// Makes `lines` the virtual lines `owner` shows in the document `id`,
    /// in place of any it set before; other owners' stay. Each takes a
    /// screen row after its line, as [`VirtualLine`] describes. Does
    /// nothing for a document the editor lacks.
    pub fn set_virtual_lines(&mut self, id: DocumentId, owner: &str, lines: Vec<VirtualLine>) {
        if let Some(open) = self.documents.get_mut(id) {
            open.decorations
                .set_virtual_lines(&open.document, owner, lines);
        }
    }

    /// Makes `lanes` the gutter lanes of the document `id`, left to right
    /// before the sign column, in place of any it had; signs show in one
    /// by naming it ([`Sign::in_lane`]). Does nothing for a document the
    /// editor lacks.
    pub fn set_gutter_lanes(&mut self, id: DocumentId, lanes: Vec<GutterLane>) {
        if let Some(open) = self.documents.get_mut(id) {
            open.decorations.set_lanes(lanes);
        }
    }

    /// The lines `owner` marked as changed in the document `id`, in the
    /// order it gave them, on the lines edits since have moved them to;
    /// empty when the editor has no such document or owner.
    pub fn line_changes(&self, id: DocumentId, owner: &str) -> Vec<ChangedLine> {
        self.documents.get(id).map_or_else(Vec::new, |open| {
            open.decorations.line_changes(&open.document, owner)
        })
    }

    /// The signs `owner` set on the document `id`, in the order it gave
    /// them, on the lines edits since have moved them to; empty when the
    /// editor has no such document or owner.
    pub fn signs(&self, id: DocumentId, owner: &str) -> Vec<Sign> {
        self.documents.get(id).map_or_else(Vec::new, |open| {
            open.decorations.signs(&open.document, owner)
        })
    }

    /// The decorations of the document `id`, which the caller knows the
    /// editor has.
    pub(crate) fn decorations_of(&self, id: DocumentId) -> &Decorations {
        &self.documents[id].decorations
    }
}
