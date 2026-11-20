//! The editor: the documents being edited, the views onto them, and the
//! keys that drive them. This is what a host embeds.
//!
//! # Embedding
//!
//! A host makes an [`Editor`] with its [`EditorOptions`], opens documents
//! from text with [`Editor::open_document`], and makes views onto them with
//! [`Editor::new_view`]. It then runs its own event loop:
//!
//! 1. Each key the user types goes to [`Editor::handle_key`], converted to a
//!    [`Key`] and passed with the view it was typed in and the current time.
//! 2. When no key comes, [`Editor::tick`] settles a half-typed sequence
//!    whose timeout has passed; [`Editor::deadline`] says when that is.
//! 3. After either, [`Editor::take_requests`] hands over what the editor
//!    asks of the host (save, quit, show a notice), and
//!    [`render_view`](crate::render_view) paints each view.
//!
//! # Dispatch
//!
//! Each view has its own [`Mode`], and the mode picks the key bindings in
//! force. In Normal and operator-pending mode, digits typed before a
//! command make its count (`3l`, `d2w`); a `0` that does not continue a
//! count is a key of its own. In Normal and visual modes, `"` and a register name
//! select the register of the next command (`"ayy`). In Insert mode, a
//! printable key that no binding takes is typed as text; in
//! operator-pending mode, a key no binding takes cancels the operator.
//!
//! # Host commands
//!
//! A host registers commands of its own with [`Editor::register_command`]
//! and binds keys to them with [`Editor::bind`]; a command runs with a
//! [`CommandContext`] (the editor, the view, the count), as the built-in
//! commands do, and acts through the editor: [`Editor::apply_edits`],
//! [`Editor::set_cursor`], [`Editor::error`], [`Editor::request`].
//! [`Editor::override_keys`] applies a user's rebinding of any command.

mod accessible;
mod decorations;
mod editing;
mod fields;
mod keymap_file;
mod keys;
mod providers;
mod saved;
mod shared;
mod views;

pub use self::accessible::{AccessibleLine, AccessibleRange, AccessibleView};
pub use self::keymap_file::{KeymapEntry, KeymapFile};
pub use self::saved::{HistoryError, SavedHistory};

use std::collections::{HashMap, VecDeque};
use std::mem;
use std::ops;

use crate::keys::{Key, Timestamp};
use slotmap::{SlotMap, new_key_type};

use super::change::{ChangeLog, DocumentChange};
use super::char_find::CharFind;
use super::command::{KeyBindings, Registry};
use super::completion::CompletionProvider;
use super::decoration::Decorations;
use super::document::Document;
use super::ex::{ExRegistry, LineAnchors};
use super::expand::SelectionRangeProvider;
use super::highlight::Highlighter;
use super::history::History;
use super::kill::KillRing;
use super::mark::{ChangeList, Marks};
use super::options::EditorOptions;
use super::phrase::Phrases;
use super::prompt::{PromptHistory, Recall};
use super::register::{MemoryClipboard, RegisterText, Registers};
use super::repeat::RecordedChange;
use super::request::{HostRequest, NoticeLevel, NoticeRequest};
use super::search::{CompiledPattern, LastSearch};
use super::substitute::Substitution;
use super::view::{Mode, View};

use self::fields::join_lines;

// ========================================================================
// Constants
// ========================================================================

/// The width, in cells, of a view until the host resizes it.
const DEFAULT_VIEW_COLS: u16 = 80;

/// The height, in rows, of a view until the host resizes it.
const DEFAULT_VIEW_ROWS: u16 = 24;

/// The notice an edit of a read-only document gives in place of the edit.
pub(crate) const READ_ONLY_NOTICE: &str = "Cannot make changes: the document is read-only";

/// The key that selects a register for the next command.
pub(crate) const REGISTER_PREFIX: char = '"';

// ========================================================================
// Data Structures
// ========================================================================

new_key_type! {
    /// Names a document held by an [`Editor`].
    pub struct DocumentId;

    /// Names a view held by an [`Editor`].
    pub struct ViewId;
}

/// Documents, views onto them, and the keys that drive them.
#[derive(Debug)]
pub struct Editor {
    /// Whether the key running acted at the main cursor alone, as undo
    /// does, so a round over several cursors stops there.
    acted_once: bool,
    /// The keys in force in each mode.
    bindings: KeyBindings,
    /// How many commands have run and characters been typed, for telling
    /// whether one command directly follows another (kills merge, `M-y`
    /// follows a yank).
    command_seq: u64,
    /// The completions asked of the providers so far, for their tickets.
    completion_requests: u64,
    /// The functions command ids run.
    commands: Registry,
    /// The patterns submitted to the ex command line.
    command_history: PromptHistory,
    /// The last pattern compiled, to compile again for free.
    compiled_pattern: Option<CompiledPattern>,
    /// The open documents, each with its undo history.
    documents: SlotMap<DocumentId, OpenDocument>,
    /// How many edits have been made, for telling whether keys changed
    /// the text.
    edit_count: u64,
    /// The functions ex command names run.
    ex_commands: ExRegistry,
    /// Whether the matches of the last search are painted.
    highlight_search: bool,
    /// The chars of each view its document's highlighter last said were
    /// ready, with the highlight version of the document they were ready
    /// for.
    highlighted: HashMap<ViewId, (u64, ops::Range<usize>)>,
    /// Text killed with Emacs keys, shared by every view.
    kill_ring: KillRing,
    /// The last change, for `.`, shared by every view.
    last_change: Option<RecordedChange>,
    /// The last ex command line run, for register `:` and `@:`.
    last_ex_command: Option<String>,
    /// The last find on a line, for `;` and `,`, shared by every view.
    last_find: Option<CharFind>,
    /// The text typed in the last insert session, for register `.`.
    last_inserted: Option<String>,
    /// The register `@@` replays.
    last_macro: Option<char>,
    /// How many keys completed the binding that ran last.
    last_matched_len: usize,
    /// When the last key was fed, for keys `:norm` types.
    last_key_at: Option<Timestamp>,
    /// The last search, shared by every view.
    last_search: Option<LastSearch>,
    /// The last `:s`, for `&`, shared by every view.
    last_substitution: Option<Substitution>,
    /// How text is laid out and keys time out.
    options: EditorOptions,
    /// Yanked and deleted text, shared by every view.
    registers: Registers,
    /// The command that last gave the read-only notice, so a command
    /// refused at several steps gives it once.
    read_only_notice_seq: Option<u64>,
    /// Keys to feed after the one being handled, as if typed.
    replay: VecDeque<Key>,
    /// What the editor asks of the host, oldest first.
    requests: Vec<HostRequest>,
    /// The patterns submitted to search prompts.
    search_history: PromptHistory,
    /// The views, each onto one document.
    views: SlotMap<ViewId, View>,
}

/// How the views of a single-line document are edited
/// ([`Editor::set_field_editing`]).
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
#[non_exhaustive]
pub enum FieldEditing {
    /// Emacs keys only, as in a text field of any GUI: the views stay in
    /// Insert mode, Escape and `C-g` cancel at once (an open completion
    /// menu closes first), and no Vim key leaves Insert mode. The default.
    #[default]
    Emacs,
    /// The Vim modes and the Emacs keys of Insert mode, as in any
    /// document: Escape goes from Insert to Normal mode, and Escape in
    /// Normal mode with nothing pending cancels.
    Modal,
}

/// What became of a key fed to [`Editor::handle_key`].
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Handled {
    /// The editor used the key: it ran a command, typed text, extended a
    /// count, or is waiting for the rest of a sequence.
    Consumed,
    /// The key ran nothing and was dropped; a host may pass it on or beep.
    Ignored,
}

/// Which way to walk an undo history.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum HistoryStep {
    /// Forward, redoing the next undone step.
    Redo,
    /// Back, undoing the newest step.
    Undo,
    /// One step along the tree toward the state of the revision, for
    /// `g-` and `g+`.
    Toward(u64),
}

/// What a round of a key over several cursors keeps as the main cursor
/// left it, put back after the extra cursors' turns.
#[derive(Debug)]
pub(crate) struct RoundState {
    /// The kill ring.
    kill_ring: KillRing,
    /// The change `.` makes again.
    last_change: Option<RecordedChange>,
    /// The text last inserted, register `.`.
    last_inserted: Option<String>,
    /// The text of the registers.
    registers: HashMap<char, RegisterText>,
    /// How many requests were queued.
    requests: usize,
}

// ========================================================================
// RoundState Implementations
// ========================================================================

impl RoundState {
    /// The text of the registers, as the main cursor left them.
    pub(crate) fn registers(&self) -> &HashMap<char, RegisterText> {
        &self.registers
    }
}

/// A document, its undo history, its marks, its decorations, and its
/// highlighter.
#[derive(Debug)]
struct OpenDocument {
    /// The brackets and quotes typing an opener pairs, set by the host for
    /// the document's language; `None` for the editor's own.
    auto_pairs: Option<Vec<(char, char)>>,
    /// The block comment tokens `gc` uses without a line token, set by the
    /// host.
    block_comment_tokens: Option<(String, String)>,
    /// The version of the text and the changes queued for the host.
    changes: ChangeLog,
    /// The line comment token `gc` uses, set by the host.
    comment_token: Option<String>,
    /// The host's source of completions for `C-x C-o`.
    completion_providers: Vec<Box<dyn CompletionProvider>>,
    /// The host's source of the pieces the selection grows through.
    selection_range_provider: Option<Box<dyn SelectionRangeProvider>>,
    /// The host's decorations, by owner.
    decorations: Decorations,
    /// The text.
    document: Document,
    /// How a single-line document is edited, or `None` for a document of
    /// many lines.
    field_editing: Option<FieldEditing>,
    /// The runs of lines the host says may fold, first and last line
    /// each, which `zc` folds before it looks at the indent.
    fold_ranges: Vec<ops::RangeInclusive<usize>>,
    /// The earlier answers a single-line document recalls, oldest first.
    field_history: Vec<String>,
    /// Where recalling the field history has got to.
    field_recall: Recall,
    /// The host's highlighter, told of every change.
    highlighter: Option<Box<dyn Highlighter>>,
    /// Counts the changes the highlighter was told of, and new
    /// highlighters, so a view's prepared chars are known stale.
    highlight_version: u64,
    /// The changes that can be undone and redone.
    history: History,
    /// Where the cursor was when a view last left the document, for the
    /// next view shown it.
    last_cursor: usize,
    /// The lines `:g` and `:norm` are running commands on, one list per
    /// command running (`:g` running `:norm` over a range), each with the
    /// next line last.
    line_anchors: Vec<LineAnchors>,
    /// The places of the document's changes, for `g;` and `g,`.
    changelist: ChangeList,
    /// The marks set with `m`, and those the editor sets.
    marks: Marks,
    /// The phrases lit by hand.
    phrases: Phrases,
    /// The name the host gave the document, for register `%`.
    name: Option<String>,
    /// Whether every edit of the text is refused.
    read_only: bool,
    /// The revision of the history the host last saved, or opened.
    saved_revision: u64,
}

// ========================================================================
// Editor Implementations
// ========================================================================

impl Editor {
    /// An editor with no documents or views, laying text out and timing out
    /// keys with `options`.
    pub fn new(options: EditorOptions) -> Self {
        let mut registers = Registers::new(Box::new(MemoryClipboard::default()));
        registers.set_unnamed_clipboard(options.unnamed_clipboard);
        Self {
            acted_once: false,
            bindings: KeyBindings::new(options.editing_style),
            command_seq: 0,
            completion_requests: 0,
            commands: Registry::new(),
            command_history: PromptHistory::default(),
            compiled_pattern: None,
            documents: SlotMap::with_key(),
            edit_count: 0,
            ex_commands: ExRegistry::new(),
            highlight_search: false,
            highlighted: HashMap::new(),
            kill_ring: KillRing::default(),
            last_change: None,
            last_ex_command: None,
            last_find: None,
            last_inserted: None,
            last_macro: None,
            last_key_at: None,
            last_matched_len: 0,
            last_search: None,
            last_substitution: None,
            options,
            read_only_notice_seq: None,
            registers,
            replay: VecDeque::new(),
            requests: Vec::new(),
            search_history: PromptHistory::default(),
            views: SlotMap::with_key(),
        }
    }

    /// Drops `view`, as a host does when it closes the window showing it;
    /// its document stays open. Returns whether the editor had the view.
    pub fn close_view(&mut self, view: ViewId) -> bool {
        let Some(state) = self.views.remove(view) else {
            return false;
        };
        self.highlighted.remove(&view);
        if let Some(open) = self.documents.get_mut(state.document()) {
            open.last_cursor = state.cursor();
        }
        true
    }

    /// Drops the document `id`, its history, marks, and decorations, as a
    /// host does when the user closes a file. Refused while a view shows
    /// it (close the view, or show it another document, first). Returns
    /// whether the document was dropped.
    pub fn close_document(&mut self, id: DocumentId) -> bool {
        if self.views.values().any(|view| view.document() == id) {
            return false;
        }
        self.bindings.drop_document(id);
        self.documents.remove(id).is_some()
    }

    /// The document `id`, or `None` when the editor has no such document.
    pub fn document(&self, id: DocumentId) -> Option<&Document> {
        self.documents.get(id).map(|open| &open.document)
    }

    /// The name the host gave the document `id` with
    /// [`Editor::set_document_name`], if any.
    pub fn document_name(&self, id: DocumentId) -> Option<&str> {
        self.documents.get(id)?.name.as_deref()
    }

    /// Whether the document `id` changed since it was opened or last
    /// marked saved with [`Editor::mark_saved`]; undoing back to that text
    /// makes it unmodified again. False when the editor has no such
    /// document.
    pub fn is_modified(&self, id: DocumentId) -> bool {
        self.documents
            .get(id)
            .is_some_and(|open| open.history.revision() != open.saved_revision)
    }

    /// Records that the host saved the document `id` as it is now, so it is
    /// no longer modified. Does nothing for a document the editor lacks.
    pub fn mark_saved(&mut self, id: DocumentId) {
        if let Some(open) = self.documents.get_mut(id) {
            open.saved_revision = open.history.revision();
        }
    }

    /// The version of the document `id`: 0 when it was opened, raised by
    /// one by each change of its text, undo and redo included (see
    /// [`DocumentChange`]); `None` when the editor has no such document.
    pub fn document_version(&self, id: DocumentId) -> Option<u64> {
        Some(self.documents.get(id)?.changes.version())
    }

    /// Queues a notice of `message` for the host, saying why a command did
    /// nothing ([`NoticeLevel::Error`]).
    pub fn error(&mut self, message: String) {
        self.notice(NoticeLevel::Error, message);
    }

    /// Queues a notice of `message` for the host, reporting on what a
    /// command did ([`NoticeLevel::Info`]).
    pub fn info(&mut self, message: String) {
        self.notice(NoticeLevel::Info, message);
    }

    /// Queues a notice of `message` for the host, warning of something a
    /// command met as it did what was asked ([`NoticeLevel::Warning`]).
    pub fn warn(&mut self, message: String) {
        self.notice(NoticeLevel::Warning, message);
    }

    /// Queues a notice of `message` at `level` for the host.
    fn notice(&mut self, level: NoticeLevel, message: String) {
        self.request(HostRequest::Notice(NoticeRequest { level, message }));
    }

    /// Opens a document holding `text` and returns its id.
    pub fn open_document(&mut self, text: &str) -> DocumentId {
        self.open(Document::new(text), None)
    }

    /// Opens a single-line document holding `text`, each line break made a
    /// space, and returns its id: the text of a one-line field (a rename
    /// box, a search box). No edit breaks its line: a line break any
    /// command or host edit would insert (a put of lines, `:s` with `\r`,
    /// `C-o`) becomes a space. Its views paint no gutter, keep no scroll
    /// margin, and never wrap, whatever the options say.
    ///
    /// It is edited with Emacs keys only ([`FieldEditing::Emacs`]): its
    /// views stay in Insert mode, where Enter (and `C-j`, `C-m`) asks the
    /// host to submit ([`HostRequest::Submit`]) and Escape or `C-g` asks
    /// it to cancel ([`HostRequest::Cancel`]). [`Editor::set_field_editing`]
    /// with [`FieldEditing::Modal`] gives it the Vim modes instead.
    pub fn open_single_line_document(&mut self, text: &str) -> DocumentId {
        let editing = Some(FieldEditing::default());
        self.open(Document::new(&join_lines(text)), editing)
    }

    /// Opens `document`, single-line and edited as `field_editing` says
    /// when that is set, and returns its id.
    fn open(&mut self, document: Document, field_editing: Option<FieldEditing>) -> DocumentId {
        self.documents.insert(OpenDocument {
            auto_pairs: None,
            block_comment_tokens: None,
            changes: ChangeLog::default(),
            comment_token: None,
            completion_providers: Vec::new(),
            selection_range_provider: None,
            decorations: Decorations::default(),
            document,
            field_editing,
            fold_ranges: Vec::new(),
            field_history: Vec::new(),
            field_recall: Recall::default(),
            highlighter: None,
            highlight_version: 0,
            history: History::new(),
            last_cursor: 0,
            line_anchors: Vec::new(),
            changelist: ChangeList::default(),
            marks: Marks::default(),
            phrases: Phrases::default(),
            name: None,
            read_only: false,
            saved_revision: 0,
        })
    }

    /// Makes a view onto `document`, in Normal mode (Insert mode in a
    /// modeless editing style, and for a single-line document edited with
    /// Emacs keys) with the cursor at the
    /// start, 80 by 24 cells until [`Editor::resize_view`]; `None` when the
    /// editor has no such document.
    pub fn new_view(&mut self, document: DocumentId) -> Option<ViewId> {
        self.documents.get(document)?;
        let view = View::new(
            document,
            DEFAULT_VIEW_COLS,
            DEFAULT_VIEW_ROWS,
            &self.options,
        );
        let view = self.views.insert(view);
        self.lock_in_insert(view);
        Some(view)
    }

    /// The options in force: those the editor was made with, as `:set` and
    /// [`Editor::set_options`] changed them.
    pub fn options(&self) -> &EditorOptions {
        &self.options
    }

    /// Queues `request` for the host.
    pub fn request(&mut self, request: HostRequest) {
        self.requests.push(request);
    }

    /// Replaces the options, as a host's settings page does, for every
    /// view: each takes the new key timeouts (dropping a half-typed key
    /// sequence) and is scrolled to keep its cursor in sight. A new
    /// [`EditingStyle`](crate::EditingStyle) puts its own default keys in
    /// place of the old style's, dropping the user's overrides
    /// ([`Editor::override_keys`]) while the host's keys stay; a modeless style (Emacs, standard)
    /// puts every view in Insert mode, and the others leave each where it
    /// is.
    pub fn set_options(&mut self, options: EditorOptions) {
        let restyled = options.editing_style != self.options.editing_style;
        self.options = options;
        self.registers
            .set_unnamed_clipboard(options.unnamed_clipboard);
        if restyled {
            self.bindings.set_editing_style(options.editing_style);
        }
        let views: Vec<ViewId> = self.views.keys().collect();
        for view in views {
            self.views[view].set_key_timeouts(&self.options);
            self.lock_in_insert(view);
            self.scroll_to_cursor(view);
        }
    }

    /// Starts or stops queueing a [`DocumentChange`] for each change of the
    /// document `id`, for the host to take with [`Editor::take_changes`];
    /// stopping drops the changes not taken. Untracked, the default, a
    /// document queues nothing. Does nothing for a document the editor
    /// lacks.
    pub fn set_change_tracking(&mut self, id: DocumentId, tracking: bool) {
        if let Some(open) = self.documents.get_mut(id) {
            open.changes.set_tracking(tracking);
        }
    }

    /// Makes `name` (a path, say) the name of the document `id`, which
    /// register `%` reads (`"%p`, `C-r %`); `None` leaves it with none.
    /// Cherry uses it for nothing else. Does nothing for a document the
    /// editor lacks.
    pub fn set_document_name(&mut self, id: DocumentId, name: Option<&str>) {
        if let Some(open) = self.documents.get_mut(id) {
            open.name = name.map(str::to_string);
        }
    }

    /// Puts the document `document` in `view` in place of the one it shows,
    /// as `:e` or a tab switch does: the view keeps its id and size, goes
    /// back to Normal mode (stays in Insert mode in a modeless editing
    /// style, and for a single-line document edited with Emacs keys), and
    /// drops what belonged to the other document
    /// (selection, jumps, `gv`), keeping the change `.` repeats and a macro
    /// being recorded; the cursor goes where a view last left `document`.
    /// Returns false, changing nothing, for a view or document the editor
    /// lacks.
    pub fn show_document(&mut self, view: ViewId, document: DocumentId) -> bool {
        if !self.documents.contains_key(document) || !self.views.contains_key(view) {
            return false;
        }
        if self.is_insert_locked(view) {
            self.history_mut(view).end_group();
        } else {
            self.set_mode(view, Mode::Normal);
        }
        let state = &mut self.views[view];
        if let Some(left) = self.documents.get_mut(state.document()) {
            left.last_cursor = state.cursor();
        }
        *state = state.showing(document, &self.options);
        let open = &self.documents[document];
        let cursor = open.last_cursor.min(open.document.len_chars());
        self.set_cursor(view, cursor);
        self.lock_in_insert(view);
        self.scroll_to_cursor(view);
        true
    }

    /// Takes the changes queued for the document `id` since they were
    /// last taken, oldest first, each with the version it made; empty
    /// unless the host tracks the document's changes
    /// ([`Editor::set_change_tracking`]) or when the editor has no such
    /// document.
    pub fn take_changes(&mut self, id: DocumentId) -> Vec<DocumentChange> {
        self.documents
            .get_mut(id)
            .map(|open| open.changes.take())
            .unwrap_or_default()
    }

    /// Takes what the editor has asked of the host since the last call,
    /// oldest first, for the host to act on in order.
    pub fn take_requests(&mut self) -> Vec<HostRequest> {
        mem::take(&mut self.requests)
    }

    /// The view `id`, or `None` when the editor has no such view.
    pub fn view(&self, id: ViewId) -> Option<&View> {
        self.views.get(id)
    }

    /// The document `view` shows.
    pub(crate) fn document_of(&self, view: ViewId) -> &Document {
        &self.documents[self.views[view].document()].document
    }

    /// The options, to change with `:set`.
    pub(crate) fn options_mut(&mut self) -> &mut EditorOptions {
        &mut self.options
    }

    /// Whether a view other than `view` shows the document `view` shows.
    pub(crate) fn is_shown_elsewhere(&self, view: ViewId) -> bool {
        let document = self.views[view].document();
        self.views
            .iter()
            .any(|(id, other)| id != view && other.document() == document)
    }

    /// The view `id`, which the caller knows the editor has.
    pub(crate) fn view_ref(&self, id: ViewId) -> &View {
        &self.views[id]
    }

    /// The view `id`, which the caller knows the editor has, to change.
    pub(crate) fn view_mut(&mut self, id: ViewId) -> &mut View {
        &mut self.views[id]
    }

    /// Adds `state` as a view of its own, for a round over several cursors
    /// to keep the main cursor's view carried through the others' edits.
    pub(crate) fn insert_view_state(&mut self, state: View) -> ViewId {
        self.views.insert(state)
    }

    /// Drops the view `id` added with [`Editor::insert_view_state`] and
    /// returns it.
    pub(crate) fn remove_view_state(&mut self, id: ViewId) -> View {
        self.views.remove(id).expect("the round added the view")
    }

    /// Makes `state` the view `id`, returning what it was.
    pub(crate) fn replace_view_state(&mut self, id: ViewId, state: View) -> View {
        mem::replace(&mut self.views[id], state)
    }
}
// ========================================================================
// Tests
// ========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::{CommandContext, CommandError, CommandId};
    use crate::decoration::{Decoration, DecorationKind, Sign};
    use crate::keys::PrefixTimeout;
    use crate::mouse::{MouseEvent, MouseEventKind};
    use crate::options::ViewOptions;
    use crate::paint::{CursorShape, Style, Surface};
    use crate::render::render_view;
    use crate::request::{CancelRequest, QuitRequest, SaveRequest, SubmitRequest};
    use crate::testing::{Harness, assert_keys, parse_keys};
    use crate::transaction::Edit;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn test_a_reload_edits_only_what_changed_and_counts_the_document_saved() {
        let mut editor = Editor::new(EditorOptions::default());
        let document = editor.open_document("one\ntwo\nthree\nfour\n");
        let view = editor.new_view(document).expect("a view");
        let line_of = |editor: &Editor| {
            let cursor = editor.view(view).expect("the view").cursor();
            editor
                .document(document)
                .and_then(|text| text.char_to_position(cursor))
                .map(|position| (position.line, position.column))
        };
        // On "four", past the line the file changes.
        editor.set_cursor(view, 15);
        assert_eq!(line_of(&editor), Some((3, 1)));
        assert!(editor.reload_document(document, "one\nTWO, longer\r\nthree\nfour\n"));
        let text = editor.document(document).expect("open").to_string();
        assert_eq!(
            text, "one\nTWO, longer\nthree\nfour\n",
            "line breaks normalized"
        );
        assert_eq!(
            line_of(&editor),
            Some((3, 1)),
            "the cursor stays on its text"
        );
        assert!(!editor.is_modified(document), "as on disk");
        assert!(
            !editor.reload_document(document, "one\nTWO, longer\nthree\nfour\n"),
            "the same text changes nothing"
        );
        editor.run_command(view, "undo", None).expect("undoes");
        assert_eq!(
            editor.document(document).expect("open").to_string(),
            "one\ntwo\nthree\nfour\n",
            "one undo step back"
        );
        assert!(editor.is_modified(document));
    }

    #[test]
    fn test_a_view_counts_its_rows_and_scrolls_to_a_row_with_the_cursor_in_sight() {
        let text: String = (0..100).map(|line| format!("line {line}\n")).collect();
        let mut editor = Editor::new(EditorOptions::default());
        let document = editor.open_document(&text);
        let view = editor.new_view(document).expect("a view");
        editor.resize_view(view, 20, 10);
        let rows = editor.view_rows(view).expect("the view");
        assert_eq!((rows.top, rows.shown), (0, 10));
        assert!(rows.total >= 100, "{rows:?}");
        editor.scroll_view_to_row(view, 40);
        let shown = editor.view(view).expect("the view");
        assert_eq!(shown.scroll().line, 40);
        let cursor_line = editor
            .document(document)
            .expect("the document")
            .char_to_position(shown.cursor())
            .expect("a position")
            .line;
        assert!((40..50).contains(&cursor_line), "in sight: {cursor_line}");
        editor.scroll_view_to_row(view, 1000);
        let bottom = editor.view_rows(view).expect("the view");
        assert_eq!(
            bottom.top,
            bottom.total - bottom.shown,
            "the last row at the bottom"
        );
    }

    #[test]
    fn test_a_wrapped_line_counts_as_the_rows_it_takes() {
        let options = EditorOptions {
            wrap: true,
            ..EditorOptions::default()
        };
        let mut editor = Editor::new(options);
        let text = format!("{}\nshort\nshort\n", "x".repeat(100));
        let document = editor.open_document(&text);
        let view = editor.new_view(document).expect("a view");
        editor.resize_view(view, 20, 2);
        let rows = editor.view_rows(view).expect("the view");
        assert!(
            rows.total > 4,
            "the long line wraps onto several rows: {rows:?}"
        );
        editor.scroll_view_to_row(view, 3);
        assert_eq!(editor.view_rows(view).map(|rows| rows.top), Some(3));
    }

    #[test]
    fn test_select_makes_a_visual_selection_and_an_empty_range_a_cursor() {
        let mut harness = Harness::new("|hello world");
        harness.editor.select(harness.view, 6..11);
        assert_eq!(harness.mode(), Mode::Visual);
        harness.keys("d");
        assert_eq!(
            harness.marked(),
            "hello| ",
            "the cursor on the last char left"
        );
        harness.editor.select(harness.view, 2..2);
        assert_eq!(
            (harness.mode(), harness.marked()),
            (Mode::Normal, "he|llo ".to_owned())
        );
        harness.editor.select(harness.view, 4..99);
        harness.keys("y");
        assert_eq!(
            harness.mode(),
            Mode::Normal,
            "cut to the document, still selected"
        );
    }

    #[test]
    fn test_set_cursor_clamps_and_a_closed_view_takes_no_cursor_or_edit() {
        let mut harness = Harness::new("|hello");
        harness.editor.set_cursor(harness.view, 99);
        assert_eq!(harness.marked(), "hello|");

        let document = harness.editor.view(harness.view).expect("open").document();
        let closed = harness.editor.new_view(document).expect("open");
        assert!(harness.editor.close_view(closed));
        harness.editor.set_cursor(closed, 2);
        harness
            .editor
            .apply_edits(closed, vec![Edit::new(0..0, "x")]);
        assert_eq!(harness.marked(), "hello|", "the open view is untouched");
    }

    #[test]
    fn test_a_cell_names_its_char_only_over_text_and_inverts_screen_cell() {
        let options = EditorOptions {
            line_numbers: true,
            ..EditorOptions::default()
        };
        let mut harness = Harness::with_options("|ab\ncd", options);
        harness.editor.resize_view(harness.view, 10, 4);
        let view = harness.view;
        let (column, row) = harness.editor.screen_cell(view, 4).unwrap();
        assert_eq!(harness.editor.offset_at_cell(view, column, row), Some(4));
        assert_eq!(
            harness.editor.offset_at_cell(view, 0, 0),
            None,
            "the gutter"
        );
        assert_eq!(
            harness.editor.offset_at_cell(view, 9, 0),
            None,
            "past the line"
        );
        assert_eq!(
            harness.editor.offset_at_cell(view, column, 3),
            None,
            "below the text"
        );
    }

    #[test]
    fn test_view_options_override_the_editor_s_for_one_view_only() {
        let mut harness = Harness::new("|ab");
        harness.editor.resize_view(harness.view, 10, 1);
        let other = harness
            .editor
            .new_view(harness.editor.view(harness.view).unwrap().document())
            .unwrap();
        harness.editor.resize_view(other, 10, 1);
        let numbered = ViewOptions {
            line_numbers: Some(true),
            ..ViewOptions::default()
        };
        harness.editor.set_view_options(harness.view, numbered);
        let first_cell = |editor: &Editor, view: ViewId| editor.screen_cell(view, 0).unwrap().0;
        assert!(
            first_cell(&harness.editor, harness.view) > 0,
            "a gutter of numbers"
        );
        assert_eq!(
            first_cell(&harness.editor, other),
            0,
            "the other view keeps the editor's"
        );
    }

    #[test]
    fn test_typing_in_insert_mode_and_escaping_back_to_normal() {
        let mut harness = Harness::new("|");
        harness.keys("i hello");
        assert_eq!(harness.mode(), Mode::Insert);
        assert_eq!(harness.marked(), "hello|");
        harness.keys("<esc>");
        assert_eq!(harness.mode(), Mode::Normal);
        assert_eq!(harness.marked(), "hell|o", "escape steps back onto the o");
    }

    #[test]
    fn test_multibyte_text_is_typed_at_the_cursor() {
        let mut harness = Harness::new("ab|c");
        harness.keys("i 日本 é");
        assert_eq!(harness.marked(), "ab日本é|c");
    }

    #[test]
    fn test_a_count_repeats_the_command_and_zero_continues_it() {
        let mut harness = Harness::new("|abcdefghijklmnopqrstuvwxyz");
        harness.keys("3l");
        assert_eq!(harness.marked(), "abc|defghijklmnopqrstuvwxyz");
        harness.keys("10l");
        assert_eq!(harness.marked(), "abcdefghijklm|nopqrstuvwxyz");
        harness.keys("l");
        assert_eq!(harness.marked(), "abcdefghijklmn|opqrstuvwxyz", "spent");
    }

    #[test]
    fn test_a_count_stops_at_the_last_character_of_the_line() {
        let mut harness = Harness::new("a|bc\nnext");
        harness.keys("99l");
        assert_eq!(harness.marked(), "ab|c\nnext");
    }

    #[test]
    fn test_escape_drops_a_prefix_first_when_asked_and_leaves_the_mode_after() {
        let options = EditorOptions {
            escape_drops_prefix: true,
            ..EditorOptions::default()
        };
        let mut harness = Harness::with_options("|one two three", options);
        harness.keys("v 3 <esc>");
        assert_eq!(harness.mode(), Mode::Visual, "the count is dropped");
        harness.keys("w");
        assert_eq!(harness.marked(), "one |two three", "one word, not three");
        harness.keys("\" a <esc> <esc>");
        assert_eq!(harness.mode(), Mode::Normal, "then Escape leaves");
        harness.keys("d 2 <esc> w");
        assert_eq!(harness.marked(), "one |three", "the operator still waits");
        let mut vim = Harness::new("|one two");
        vim.keys("v 3 <esc>");
        assert_eq!(vim.mode(), Mode::Normal, "Vim leaves at once");
    }

    #[test]
    fn test_a_double_quote_is_the_char_a_command_waits_for_not_a_register() {
        assert_keys("|a\"b", "f \"", "a|\"b");
        assert_keys("|ab", "r \"", "|\"b");
        assert_keys("|ab", "v r \"", "|\"b");
    }

    #[test]
    fn test_a_lone_zero_is_not_a_count_and_an_unbound_key_drops_the_count() {
        let mut harness = Harness::new("|abcdef");
        harness.keys("0");
        assert_eq!(harness.editor.view(harness.view).unwrap().count(), None);
        harness.keys("3 Q");
        assert_eq!(harness.last_handled, Some(Handled::Ignored));
        harness.keys("l");
        assert_eq!(harness.marked(), "a|bcdef");
    }

    #[test]
    fn test_a_digit_in_insert_mode_is_typed() {
        let mut harness = Harness::new("|");
        harness.keys("i 30");
        assert_eq!(harness.marked(), "30|");
    }

    #[test]
    fn test_a_chorded_key_unbound_in_insert_mode_is_not_typed() {
        let mut harness = Harness::new("|");
        harness.keys("i C-z M-x");
        assert_eq!(harness.marked(), "|");
        assert_eq!(harness.last_handled, Some(Handled::Ignored));
    }

    #[test]
    fn test_a_sequence_waits_for_its_last_key_then_requests_save_and_quit() {
        let mut harness = Harness::new("|text");
        harness.keys("x Z");
        let view = harness.editor.view(harness.view).unwrap();
        assert_eq!(view.pending_keys(), parse_keys("Z"));
        assert!(harness.requests().is_empty());
        harness.keys("Z");
        let document = harness.editor.view(harness.view).unwrap().document();
        assert_eq!(
            harness.requests(),
            [
                HostRequest::Save(SaveRequest { document }),
                HostRequest::Quit(QuitRequest {
                    force: false,
                    view: harness.view,
                }),
            ]
        );
    }

    #[test]
    fn test_an_incomplete_prefix_waits_for_its_next_key_by_default() {
        let mut harness = Harness::new("|text");
        harness.keys("Z");
        assert_eq!(harness.editor.deadline(), None);
        assert!(!harness.wait(), "nothing times out");
        harness.keys("Z");
        let quit = HostRequest::Quit(QuitRequest {
            force: false,
            view: harness.view,
        });
        assert_eq!(harness.requests(), [quit], "Z Z quit");
    }

    #[test]
    fn test_a_pending_sequence_times_out_on_tick_and_runs_nothing() {
        let options = EditorOptions {
            prefix_timeout: PrefixTimeout::All,
            ..EditorOptions::default()
        };
        let mut harness = Harness::with_options("|text", options);
        harness.keys("Z");
        assert!(harness.editor.deadline().is_some());
        assert!(harness.wait(), "the pending Z settled");
        assert!(
            harness
                .editor
                .view(harness.view)
                .unwrap()
                .pending_keys()
                .is_empty()
        );
        assert_eq!(harness.editor.deadline(), None);
        assert!(harness.requests().is_empty());
        assert!(!harness.wait(), "nothing left to settle");
    }

    #[test]
    fn test_each_view_has_its_own_mode_and_follows_edits_to_the_shared_document() {
        let mut harness = Harness::new("ab|cd");
        let document = harness.editor.view(harness.view).unwrap().document();
        let other = harness.editor.new_view(document).unwrap();
        harness.editor.set_cursor(other, 3);
        harness.keys("i XY");
        let other_view = harness.editor.view(other).unwrap();
        assert_eq!(other_view.mode(), Mode::Normal);
        assert_eq!(other_view.cursor(), 5, "the d moved two chars right");
        assert_eq!(harness.marked(), "abXY|cd");
    }

    #[test]
    fn test_keys_for_an_unknown_view_or_document_are_refused() {
        let mut editor = Editor::new(EditorOptions::default());
        let key = parse_keys("i")[0];
        let unknown = ViewId::default();
        assert_eq!(
            editor.handle_key(unknown, key, Timestamp::default()),
            Handled::Ignored
        );
        assert_eq!(editor.new_view(DocumentId::default()), None);
    }

    /// Registers `shout`, which types one `!` per count at the cursor.
    fn register_shout(editor: &mut Editor) {
        let shout = |cx: &mut CommandContext<'_>| {
            let cursor = cx.editor.view(cx.view).unwrap().cursor();
            let text = "!".repeat(cx.count_or_one());
            cx.editor
                .apply_edits(cx.view, vec![Edit::new(cursor..cursor, text)]);
        };
        editor.register_command("shout", shout).unwrap();
    }

    #[test]
    fn test_a_host_command_bound_to_keys_runs_with_the_count_and_undoes() {
        let mut harness = Harness::new("ab|c");
        register_shout(&mut harness.editor);
        harness
            .editor
            .bind(Mode::Normal, &["g !", "Q"], "shout")
            .unwrap();
        harness.keys("3 g !");
        assert_eq!(harness.marked(), "ab!!!|c");
        harness.keys("Q u");
        assert_eq!(harness.marked(), "ab!!!|c");
    }

    #[test]
    fn test_a_host_command_closure_keeps_its_own_state() {
        let runs = Arc::new(AtomicUsize::new(0));
        let mut harness = Harness::new("|x");
        let counter = Arc::clone(&runs);
        let count_runs = move |cx: &mut CommandContext<'_>| {
            let total = counter.fetch_add(1, Ordering::Relaxed) + 1;
            cx.editor.error(format!("run {total}"));
        };
        harness
            .editor
            .register_command("count-runs", count_runs)
            .unwrap();
        harness
            .editor
            .bind(Mode::Insert, &["C-t"], "count-runs")
            .unwrap();
        harness.keys("i C-t C-t");
        assert_eq!(runs.load(Ordering::Relaxed), 2);
        let last = harness.requests().pop();
        assert_eq!(
            last,
            Some(HostRequest::Notice(NoticeRequest {
                level: NoticeLevel::Error,
                message: "run 2".to_string()
            }))
        );
    }

    #[test]
    fn test_a_bound_key_wins_over_a_default_and_an_override_moves_a_built_in() {
        let mut harness = Harness::new("a|bc");
        register_shout(&mut harness.editor);
        harness.editor.bind(Mode::Normal, &["l"], "shout").unwrap();
        harness.keys("l");
        assert_eq!(harness.marked(), "a!|bc", "l now shouts");
        harness
            .editor
            .override_keys(Mode::Normal, "undo", &["U"])
            .unwrap();
        harness.keys("u");
        assert_eq!(harness.marked(), "a!|bc", "u no longer undoes");
        harness.keys("U");
        assert_eq!(harness.marked(), "a|bc");
        harness.keys("<right>");
        assert_eq!(harness.marked(), "ab|c", "move-right keeps its other key");
    }

    #[test]
    fn test_overriding_a_host_command_replaces_its_bound_keys_and_survives_rebuilds() {
        let mut harness = Harness::new("|x");
        register_shout(&mut harness.editor);
        harness.editor.bind(Mode::Normal, &["Q"], "shout").unwrap();
        harness
            .editor
            .override_keys(Mode::Normal, "shout", &["g s"])
            .unwrap();
        harness
            .editor
            .override_keys(Mode::Normal, "undo", &["U"])
            .unwrap();
        harness.keys("Q");
        assert_eq!(harness.marked(), "|x", "Q was replaced");
        harness.keys("g s");
        assert_eq!(harness.marked(), "!|x");
    }

    #[test]
    fn test_commands_registered_under_runtime_built_ids_run_from_their_keys() {
        let mut harness = Harness::new("|x");
        for (tag, key) in [("red", "g r"), ("blue", "g b")] {
            let id = format!("settag:{tag}");
            let set_tag = move |cx: &mut CommandContext<'_>| {
                cx.editor.error(format!("tagged {tag}"));
            };
            harness
                .editor
                .register_command(id.clone(), set_tag)
                .unwrap();
            harness.editor.bind(Mode::Normal, &[key], &id).unwrap();
        }
        harness.keys("g b g r");
        let notices: Vec<_> = harness
            .requests()
            .into_iter()
            .filter_map(|request| match request {
                HostRequest::Notice(notice) => Some(notice.message),
                HostRequest::Cancel(_)
                | HostRequest::Command(_)
                | HostRequest::Open(_)
                | HostRequest::Quit(_)
                | HostRequest::ReadInto(_)
                | HostRequest::Save(_)
                | HostRequest::Submit(_) => None,
            })
            .collect();
        assert_eq!(notices, ["tagged blue", "tagged red"]);
        let taken = harness
            .editor
            .register_command(format!("settag:{}", "red"), |_: &mut CommandContext<'_>| {});
        assert_eq!(
            taken,
            Err(CommandError::DuplicateId("settag:red".to_string()))
        );
    }

    #[test]
    fn test_registering_or_binding_badly_is_rejected_with_a_typed_error() {
        let mut editor = Editor::new(EditorOptions::default());
        register_shout(&mut editor);
        let noop = |_: &mut CommandContext<'_>| {};
        assert_eq!(
            editor.register_command("undo", noop),
            Err(CommandError::DuplicateId("undo".to_string()))
        );
        assert_eq!(
            editor.bind(Mode::Normal, &["Q"], "nope"),
            Err(CommandError::UnknownId("nope".to_string()))
        );
        let invalid = editor.bind(Mode::Normal, &["Q", "<nope>"], "shout");
        assert!(matches!(invalid, Err(CommandError::InvalidSpec(_))));
        let none = editor.bind(Mode::Normal, &[], "shout");
        assert!(matches!(none, Err(CommandError::InvalidSpec(_))));
        let bad_override = editor.override_keys(Mode::Normal, "undo", &["C-"]);
        assert!(matches!(bad_override, Err(CommandError::InvalidSpec(_))));
    }

    #[test]
    fn test_the_cursor_is_a_bar_in_insert_mode_and_a_block_in_normal_mode() {
        let mut harness = Harness::new("|ab");
        let mut surface = Surface::new(0, 0);
        harness.editor.resize_view(harness.view, 4, 1);
        let shape = |harness: &Harness, surface: &mut Surface| {
            render_view(&harness.editor, harness.view, surface).map(|cursor| cursor.shape)
        };
        assert_eq!(shape(&harness, &mut surface), Some(CursorShape::Block));
        harness.keys("i");
        assert_eq!(shape(&harness, &mut surface), Some(CursorShape::Bar));
        assert_eq!(surface.row_text(0), "ab  ");
    }

    #[test]
    fn test_decorations_and_signs_follow_edits_and_undo_per_owner() {
        let mut harness = Harness::new(
            "|one two
three",
        );
        let document = harness.editor.view(harness.view).unwrap().document();
        let error = Decoration::new(4..7, DecorationKind::Error);
        harness.editor.set_decorations(document, "lsp", vec![error]);
        harness.editor.set_signs(
            document,
            "lsp",
            vec![Sign::new(1, "E", DecorationKind::Error)],
        );
        let spell = Decoration::new(8..13, DecorationKind::Underline);
        harness
            .editor
            .set_decorations(document, "spell", vec![spell]);
        let ranges = |harness: &Harness, owner: &str| -> Vec<(usize, usize)> {
            let decorations = harness.editor.decorations(document, owner);
            decorations
                .iter()
                .map(|d| (d.range.start, d.range.end))
                .collect()
        };
        harness.keys("O new <esc>");
        assert_eq!(ranges(&harness, "lsp"), [(8, 11)]);
        assert_eq!(ranges(&harness, "spell"), [(12, 17)]);
        assert_eq!(harness.editor.signs(document, "lsp")[0].line, 2);
        harness.keys("u");
        assert_eq!(ranges(&harness, "lsp"), [(4, 7)], "undo carries them back");
        assert_eq!(harness.editor.signs(document, "lsp")[0].line, 1);
        harness.keys("w d w");
        assert!(ranges(&harness, "lsp").is_empty(), "its text was deleted");
        harness.editor.clear_decorations(document, "spell");
        assert!(ranges(&harness, "spell").is_empty());
        assert_eq!(harness.editor.signs(document, "lsp").len(), 1);
    }

    #[test]
    fn test_show_document_switches_a_view_and_restores_the_last_cursor() {
        let mut harness = Harness::new("ab|c");
        let first = harness.editor.view(harness.view).unwrap().document();
        let second = harness.editor.open_document("xyz\nw");
        harness.keys("x V");
        assert!(harness.editor.show_document(harness.view, second));
        let view = harness.editor.view(harness.view).unwrap();
        assert_eq!((view.document(), view.cursor()), (second, 0));
        assert_eq!(harness.mode(), Mode::Normal);
        harness.keys("j .");
        assert_eq!(
            harness.marked(),
            "xyz\n|",
            ". repeats x in the new document"
        );
        assert!(harness.editor.show_document(harness.view, first));
        assert_eq!(harness.marked(), "a|b", "back where it was left");
        harness.keys("u");
        assert_eq!(harness.marked(), "ab|c", "each document keeps its history");
        assert!(harness.editor.show_document(harness.view, second));
        assert_eq!(harness.marked(), "xyz\n|");
        assert!(
            !harness
                .editor
                .show_document(harness.view, DocumentId::default())
        );
        assert!(!harness.editor.show_document(ViewId::default(), first));
    }

    #[test]
    fn test_close_document_refuses_while_a_view_shows_it() {
        let mut harness = Harness::new("|a");
        let first = harness.editor.view(harness.view).unwrap().document();
        let second = harness.editor.open_document("b");
        assert!(!harness.editor.close_document(first), "the view shows it");
        assert!(harness.editor.close_document(second));
        assert!(harness.editor.document(second).is_none());
        assert!(!harness.editor.close_document(second), "already closed");
        assert!(harness.editor.close_view(harness.view));
        assert!(harness.editor.close_document(first));
    }

    #[test]
    fn test_a_field_wraps_over_several_rows_when_its_view_asks() {
        let mut editor = Editor::new(EditorOptions {
            wrap: true,
            ..EditorOptions::default()
        });
        let field = editor.open_single_line_document("one two three");
        let view = editor.new_view(field).unwrap();
        editor.resize_view(view, 6, 3);
        let rows = |editor: &Editor| {
            let mut surface = Surface::new(0, 0);
            render_view(editor, view, &mut surface);
            (0..3).map(|row| surface.row_text(row)).collect::<Vec<_>>()
        };
        assert_eq!(
            rows(&editor)[1],
            "      ",
            "the editor's wrap leaves a field alone"
        );
        let wrapped = ViewOptions {
            wrap: Some(true),
            ..ViewOptions::default()
        };
        editor.set_view_options(view, wrapped);
        assert_eq!(
            rows(&editor),
            ["one   ", "two   ", "three "],
            "wrapped at words as any line is"
        );
    }

    #[test]
    fn test_keys_bound_in_one_document_win_there_alone_and_go_with_it() {
        let mut harness = Harness::new("|ab");
        let first = harness.editor.view(harness.view).unwrap().document();
        let second = harness.editor.open_document("cd");
        harness
            .editor
            .bind_in_document(first, Mode::Normal, &["x"], "delete-to-line-end")
            .unwrap();
        harness.keys("x");
        assert_eq!(harness.marked(), "|", "x deletes to the line's end here");
        harness.editor.show_document(harness.view, second);
        harness.keys("x");
        assert_eq!(harness.marked(), "|d", "x deletes a char elsewhere");
        let keys = harness.editor.keys_for(Mode::Normal, "delete-to-line-end");
        assert!(!keys.contains(&parse_keys("x")), "not every document's key");
        assert!(harness.editor.close_document(first));
        let error = harness
            .editor
            .bind_in_document(first, Mode::Normal, &[], "delete-char");
        assert!(error.is_err(), "no keys: rejected");
    }

    #[test]
    fn test_set_options_applies_to_open_views_and_their_key_timeouts() {
        let mut harness = Harness::new("|a\tb");
        harness.editor.resize_view(harness.view, 4, 1);
        harness.keys("Z");
        assert_eq!(harness.editor.deadline(), None, "a prefix waits by default");
        let options = EditorOptions {
            line_numbers: true,
            prefix_timeout: PrefixTimeout::All,
            ..*harness.editor.options()
        };
        harness.editor.set_options(options);
        assert!(harness.editor.options().line_numbers);
        harness.keys("Z");
        assert!(
            harness.editor.deadline().is_some(),
            "the view times out now"
        );
        let mut surface = Surface::new(0, 0);
        render_view(&harness.editor, harness.view, &mut surface);
        assert_eq!(surface.cell(0, 0).unwrap().style, Style::CursorLineNumber);
    }

    #[test]
    fn test_run_command_runs_built_in_and_host_commands_with_a_count() {
        let mut harness = Harness::new("|abcd");
        register_shout(&mut harness.editor);
        harness.keys("g");
        let view = harness.view;
        harness
            .editor
            .run_command(view, "delete-char", Some(2))
            .unwrap();
        assert_eq!(harness.marked(), "|cd");
        assert!(harness.editor.view(view).unwrap().pending_keys().is_empty());
        harness.editor.run_command(view, "shout", Some(3)).unwrap();
        assert_eq!(harness.marked(), "!!!|cd");
        harness.keys("u u");
        assert_eq!(harness.marked(), "|abcd");
        assert_eq!(
            harness.editor.run_command(view, "frob", None),
            Err(CommandError::UnknownId("frob".to_string()))
        );
    }

    #[test]
    fn test_help_rows_and_keys_for_report_the_keys_in_force() {
        let mut editor = Editor::new(EditorOptions::default());
        register_shout(&mut editor);
        editor.bind(Mode::Normal, &["C-d"], "delete-char").unwrap();
        editor.bind(Mode::Normal, &["g s"], "shout").unwrap();
        editor.override_keys(Mode::Normal, "undo", &["U"]).unwrap();
        let rows = editor.help_rows(Mode::Normal);
        let row = |id: &str| rows.iter().find(|row| row.id == id).unwrap();
        assert_eq!(
            row("delete-char").title,
            "Delete the character under the cursor"
        );
        assert_eq!(
            row("delete-char").keys,
            [parse_keys("C-d"), parse_keys("x")]
        );
        assert_eq!(row("undo").keys, [parse_keys("U")]);
        assert!(rows.iter().all(|row| row.id != "shout"), "built-ins only");
        assert_eq!(editor.keys_for(Mode::Normal, "shout"), [parse_keys("g s")]);
        assert!(editor.keys_for(Mode::Insert, "shout").is_empty());
        assert!(
            editor
                .help_rows(Mode::Insert)
                .iter()
                .all(|row| row.id != "undo")
        );
    }

    #[test]
    fn test_key_hints_list_what_may_follow_the_pending_keys() {
        let mut harness = Harness::new("|a");
        harness.keys("Z");
        let hints = harness.editor.key_hints(harness.view);
        let summary: Vec<(String, Option<&str>, Option<&str>, bool)> = hints
            .iter()
            .map(|hint| {
                let command = hint.command.as_ref().map(CommandId::as_str);
                (hint.key.to_string(), command, hint.title, hint.continues)
            })
            .collect();
        assert_eq!(
            summary,
            [
                (
                    "Q".to_string(),
                    Some("quit"),
                    Some("Quit without saving"),
                    false
                ),
                (
                    "Z".to_string(),
                    Some("save-and-quit"),
                    Some("Save and quit"),
                    false
                ),
            ]
        );
        harness.keys("<esc>");
        let first_keys = harness.editor.key_hints(harness.view);
        let g = first_keys
            .iter()
            .find(|hint| hint.key.to_string() == "g")
            .unwrap();
        assert!(g.continues, "g starts longer bindings");
        assert!(harness.editor.key_hints(ViewId::default()).is_empty());
    }

    #[test]
    fn test_tracked_changes_replay_every_edit_path_onto_a_copy_of_the_text() {
        let original = "|😀 one x\ntwo x\nthree\n";
        let mut harness = Harness::new(original);
        let document = harness.editor.view(harness.view).unwrap().document();
        harness.keys("x");
        assert!(
            harness.editor.take_changes(document).is_empty(),
            "untracked"
        );
        assert_eq!(harness.editor.document_version(document), Some(1));
        harness.editor.set_change_tracking(document, true);
        let mut mirror = harness.editor.document(document).unwrap().clone();
        let mut version = 1;
        let mut replay = |harness: &mut Harness, what: &str| {
            let changes = harness.editor.take_changes(document);
            assert!(!changes.is_empty(), "{what} queued a change");
            for change in changes {
                version += 1;
                assert_eq!(change.version, version, "{what}");
                for edit in &change.edits {
                    let start = mirror.utf16_position_to_char(edit.start).unwrap();
                    let end = mirror.utf16_position_to_char(edit.end).unwrap();
                    assert_eq!(start..end, edit.range, "{what}");
                    mirror.delete(start..end).unwrap();
                    mirror.insert(start, &edit.text).unwrap();
                }
            }
            let text = harness.editor.document(document).unwrap().to_string();
            assert_eq!(mirror.to_string(), text, "{what}");
        };
        harness
            .editor
            .apply_edits(harness.view, vec![Edit::new(0..2, "😀😀")]);
        replay(&mut harness, "a host edit");
        harness.keys("A ab <bs> <esc>");
        replay(&mut harness, "typing");
        harness.keys("0 d w j .");
        replay(&mut harness, "operators and dot");
        harness.keys("u u C-r");
        replay(&mut harness, "undo and redo");
        harness.keys(": % s/x/yy/g <cr>");
        replay(&mut harness, "an ex command");
        harness.keys(": g/yy/ m0 <cr>");
        replay(&mut harness, ":g");
        harness.editor.insert_lines(harness.view, 1, "read\nin\n");
        replay(&mut harness, "insert_lines");
        harness.keys("l");
        assert!(harness.editor.take_changes(document).is_empty());
        assert_eq!(harness.editor.document_version(document), Some(version));
        harness.editor.set_change_tracking(document, false);
        harness.keys("x");
        assert!(harness.editor.take_changes(document).is_empty());
        assert_eq!(harness.editor.document_version(document), Some(version + 1));
        assert_eq!(harness.editor.document_version(DocumentId::default()), None);
    }

    #[test]
    fn test_a_modal_single_line_document_joins_lines_and_asks_to_submit_or_cancel() {
        let mut harness = Harness::single_line("|one\ntwo", EditorOptions::default());
        let view = harness.view;
        let document = harness.editor.view(view).unwrap().document();
        assert!(harness.editor.is_single_line(document));
        harness
            .editor
            .set_field_editing(document, FieldEditing::Modal);
        harness.keys("<esc>");
        assert_eq!(harness.mode(), Mode::Normal, "Escape leaves Insert mode");
        assert_eq!(harness.marked(), "|one two", "line breaks became spaces");
        let submit = HostRequest::Submit(SubmitRequest { view });
        let cancel = HostRequest::Cancel(CancelRequest { view });
        harness.keys("A <cr> C-j");
        assert_eq!(harness.requests(), [submit.clone(), submit.clone()]);
        assert_eq!(
            harness.mode(),
            Mode::Insert,
            "the host decides what comes next"
        );
        harness.keys("<esc>");
        assert!(harness.requests().is_empty());
        harness.keys("3 <esc>");
        assert!(
            harness.requests().is_empty(),
            "Escape first drops the count"
        );
        harness.keys("<esc>");
        assert_eq!(harness.requests(), [cancel]);
        harness.keys("<cr>");
        assert_eq!(harness.requests(), [submit]);
        harness.keys("+");
        assert!(harness.requests().is_empty(), "+ only moves");
        harness.keys("0 y y P");
        assert_eq!(harness.marked(), "|one two one two", "a linewise put joins");
        harness.keys("j d j o ! <esc> O < <esc>");
        assert_eq!(harness.marked(), "|<one two one two!");
        harness
            .editor
            .apply_edits(view, vec![Edit::new(0..1, "a\r\nb\n")]);
        assert_eq!(harness.marked(), "a b |one two one two!");
        harness.keys(": s/o/\\r/g <cr>");
        let document = harness.editor.document(document).unwrap();
        assert_eq!(document.len_lines(), 1);
        assert!(!harness.editor.is_single_line(DocumentId::default()));
    }

    #[test]
    fn test_up_and_down_in_a_field_recall_its_history_and_come_back_to_the_draft() {
        let mut harness = Harness::single_line("dra|ft", EditorOptions::default());
        let document = harness.editor.view(harness.view).unwrap().document();
        let history = vec!["one".to_string(), "two".to_string()];
        harness.editor.set_field_history(document, history);
        harness.keys("<up>");
        assert_eq!(harness.marked(), "two|", "the newest first");
        harness.keys("C-p <up>");
        assert_eq!(harness.marked(), "one|", "the oldest stays");
        harness.keys("<down> C-n");
        assert_eq!(harness.marked(), "draft|", "back to what was typed");
        harness.keys("<down> <up> x <down> <up>");
        assert_eq!(harness.marked(), "two|", "an edited entry is not kept");
        let mut plain = Harness::new("a\n|b");
        plain.keys("i <up>");
        assert_eq!(plain.marked(), "|a\nb", "a document of lines moves up");
    }

    #[test]
    fn test_an_emacs_field_stays_in_insert_mode_and_submits_or_cancels_at_once() {
        let mut harness = Harness::single_line("|one", EditorOptions::default());
        let view = harness.view;
        let document = harness.editor.view(view).unwrap().document();
        let field_editing = harness.editor.field_editing(document);
        assert_eq!(field_editing, Some(FieldEditing::Emacs), "the default");
        assert_eq!(harness.mode(), Mode::Insert, "a field starts typing");
        let submit = HostRequest::Submit(SubmitRequest { view });
        let cancel = HostRequest::Cancel(CancelRequest { view });
        harness.keys("C-e x <esc>");
        assert_eq!(
            harness.requests(),
            vec![cancel.clone()],
            "one Escape cancels"
        );
        assert_eq!(harness.marked(), "onex|");
        harness.keys("C-g <cr> C-j C-m");
        assert_eq!(
            harness.requests(),
            [cancel.clone(), submit.clone(), submit.clone(), submit]
        );
        harness.keys("C-a C-k C-y C-y M-b C-d C-f C-f M-f");
        assert_eq!(harness.marked(), "nexonex|", "Emacs keys edit");
        harness.keys("v : / ? C-o");
        assert_eq!(harness.marked(), "nexonexv:/?| ", "C-o opens a space");
        assert_eq!(harness.mode(), Mode::Insert);
        let mut surface = Surface::new(0, 0);
        harness.editor.resize_view(view, 20, 1);
        let cursor = render_view(&harness.editor, view, &mut surface).unwrap();
        assert_eq!(cursor.shape, CursorShape::Bar);
        for id in [
            "visual-charwise",
            "visual-blockwise",
            "delete-operator",
            "enter-replace",
            "enter-command-line",
            "search-forward",
            "exit-insert",
            "exit-visual",
        ] {
            harness.editor.run_command(view, id, None).unwrap();
            assert_eq!(harness.mode(), Mode::Insert, "{id} keeps Insert mode");
            assert!(harness.editor.view(view).unwrap().prompt().is_none());
        }
        harness.keys("x");
        assert_eq!(harness.marked(), "nexonexv:/?x| ", "still typing");
        harness.requests();
        harness
            .editor
            .run_command(view, "line-start", None)
            .unwrap();
        harness
            .editor
            .run_command(view, "delete-char", None)
            .unwrap();
        assert_eq!(harness.marked(), "|exonexv:/?x ", "other commands run");
        for kind in [MouseEventKind::Press, MouseEventKind::DoubleClick] {
            harness
                .editor
                .handle_mouse(view, MouseEvent::new(kind, 2, 0));
        }
        harness
            .editor
            .handle_mouse(view, MouseEvent::new(MouseEventKind::Drag, 5, 0));
        assert_eq!(harness.mode(), Mode::Insert, "the mouse selects nothing");
        assert_eq!(harness.marked(), "exone|xv:/?x ");
        let other = harness.editor.open_document("text");
        harness.editor.show_document(view, other);
        assert_eq!(harness.mode(), Mode::Normal);
        harness.editor.show_document(view, document);
        assert_eq!(harness.mode(), Mode::Insert, "shown again, typing");
        assert!(harness.requests().is_empty());
        let mut completing = Harness::single_line("open |o", EditorOptions::default());
        completing.keys("C-e C-x C-p");
        assert_eq!(completing.marked(), "open open|", "completion works");
        completing.keys("<esc>");
        assert!(
            completing.requests().is_empty(),
            "Escape closes the menu first"
        );
        assert_eq!(completing.marked(), "open o|");
        completing.keys("C-g");
        let cancel = HostRequest::Cancel(CancelRequest {
            view: completing.view,
        });
        assert_eq!(completing.requests(), [cancel]);
        let mut undoing = Harness::single_line("|", EditorOptions::default());
        undoing.keys("a b");
        undoing
            .editor
            .run_command(undoing.view, "undo", None)
            .unwrap();
        assert_eq!(undoing.marked(), "|", "the session undoes at once");
        undoing.keys("c");
        undoing
            .editor
            .run_command(undoing.view, "undo", None)
            .unwrap();
        undoing
            .editor
            .run_command(undoing.view, "redo", None)
            .unwrap();
        assert_eq!(undoing.marked(), "|c", "typing after undo is a new step");
        assert_eq!(undoing.mode(), Mode::Insert);
    }

    #[test]
    fn test_a_read_only_emacs_field_keeps_typing_mode_and_refuses_edits() {
        let mut harness = Harness::single_line("one |two", EditorOptions::default());
        let view = harness.view;
        let document = harness.editor.view(view).unwrap().document();
        harness.keys("C-k C-y");
        harness.editor.set_read_only(document, true);
        assert_eq!(harness.mode(), Mode::Insert, "a field keeps typing mode");
        harness.keys("C-a z C-k C-d C-y C-x C-n M-f");
        assert_eq!(harness.marked(), "one| two", "moving works, edits do not");
        let notices = harness
            .requests()
            .into_iter()
            .filter(|request| matches!(request, HostRequest::Notice(_)))
            .count();
        assert_eq!(notices, 5, "one notice per refused key");
        harness.keys("<esc>");
        let cancel = HostRequest::Cancel(CancelRequest { view });
        assert_eq!(harness.requests(), [cancel]);
        harness.editor.set_read_only(document, false);
        harness.keys("C-e C-y");
        assert_eq!(
            harness.marked(),
            "one twotwo|",
            "the kill ring kept its kill"
        );
    }

    #[test]
    fn test_switching_field_editing_leaves_every_view_in_a_valid_mode() {
        let mut harness = Harness::single_line("|one two", EditorOptions::default());
        let view = harness.view;
        let document = harness.editor.view(view).unwrap().document();
        let second = harness.editor.new_view(document).unwrap();
        harness
            .editor
            .set_field_editing(document, FieldEditing::Modal);
        assert_eq!(harness.mode(), Mode::Insert, "Insert mode is valid in both");
        harness.keys("<esc> v e");
        assert_eq!(harness.mode(), Mode::Visual);
        harness
            .editor
            .run_command(second, "search-forward", None)
            .unwrap();
        assert!(harness.editor.view(second).unwrap().prompt().is_some());
        harness
            .editor
            .set_field_editing(document, FieldEditing::Emacs);
        assert_eq!(harness.mode(), Mode::Insert);
        let second_view = harness.editor.view(second).unwrap();
        assert_eq!(second_view.mode(), Mode::Insert);
        assert!(second_view.prompt().is_none(), "the prompt closed");
        harness.keys("x");
        assert_eq!(harness.marked(), "onx|e two");
        let other = harness.editor.open_document("text");
        harness.editor.set_field_editing(other, FieldEditing::Emacs);
        assert_eq!(harness.editor.field_editing(other), None);
        assert!(!harness.editor.is_single_line(other));
    }

    #[test]
    fn test_a_single_line_view_paints_no_gutter_and_never_wraps() {
        let options = EditorOptions {
            line_numbers: true,
            sign_column: true,
            wrap: true,
            ..EditorOptions::default()
        };
        let mut harness = Harness::single_line("|abcdefghij", options);
        harness.editor.resize_view(harness.view, 4, 2);
        harness.keys("C-e");
        let mut surface = Surface::new(0, 0);
        let cursor = render_view(&harness.editor, harness.view, &mut surface).unwrap();
        assert_eq!(surface.row_text(0), "hij ", "scrolled sideways, no gutter");
        assert_eq!(
            surface.row_text(1),
            "    ",
            "never wrapped onto a second row"
        );
        assert_eq!((cursor.column, cursor.row), (3, 0));
        let mut lines = Harness::with_options("|abc", options);
        lines.editor.resize_view(lines.view, 8, 1);
        render_view(&lines.editor, lines.view, &mut surface).unwrap();
        assert!(
            !surface.row_text(0).starts_with("abc"),
            "other documents keep it"
        );
    }

    #[test]
    fn test_a_read_only_document_refuses_every_edit_but_moves_searches_and_yanks() {
        let mut harness = Harness::new("|one two\nthree four\n");
        let view = harness.view;
        let document = harness.editor.view(view).unwrap().document();
        harness.editor.set_change_tracking(document, true);
        harness.editor.set_read_only(document, true);
        assert!(harness.editor.is_read_only(document));
        let notices = |harness: &mut Harness| -> Vec<String> {
            harness
                .requests()
                .into_iter()
                .filter_map(|request| match request {
                    HostRequest::Notice(notice) => Some(notice.message),
                    HostRequest::Cancel(_)
                    | HostRequest::Command(_)
                    | HostRequest::Open(_)
                    | HostRequest::Quit(_)
                    | HostRequest::ReadInto(_)
                    | HostRequest::Save(_)
                    | HostRequest::Submit(_) => None,
                })
                .collect()
        };
        harness.keys("w m a / fou <cr> v e y ` a");
        assert_eq!(harness.marked(), "one |two\nthree four\n");
        assert!(notices(&mut harness).is_empty(), "moving and yanking work");
        harness.keys("x");
        assert_eq!(notices(&mut harness), [READ_ONLY_NOTICE]);
        harness.keys("c w");
        assert_eq!(notices(&mut harness), [READ_ONLY_NOTICE], "one per command");
        harness.keys("i");
        assert_eq!(harness.mode(), Mode::Normal, "Insert mode is refused");
        harness.keys("R z <esc> d d p u C-r > > J ~ r x o y <esc> C-v j d . C-a");
        harness.keys(": s/o/0/g <cr> : g/t/d <cr>");
        harness
            .editor
            .apply_edits(view, vec![Edit::new(0..0, "host")]);
        harness.editor.insert_lines(view, 1, "read\n");
        assert_eq!(harness.mode(), Mode::Normal);
        let text = harness.editor.document(document).unwrap().to_string();
        assert_eq!(text, "one two\nthree four\n");
        assert!(harness.editor.take_changes(document).is_empty());
        assert_eq!(harness.editor.document_version(document), Some(0));
        assert!(!harness.editor.is_modified(document));
        harness.editor.set_read_only(document, false);
        harness.keys("g g P");
        assert_eq!(
            harness.marked(),
            "fou|rone two\nthree four\n",
            "the yank was kept"
        );
        assert_eq!(harness.editor.take_changes(document).len(), 1);
        harness.keys("u");
        assert_eq!(
            harness.marked(),
            "|one two\nthree four\n",
            "one step to undo"
        );
        harness.keys("i");
        harness.editor.set_read_only(document, true);
        assert_eq!(harness.mode(), Mode::Normal, "an insert session ends");
        assert!(!harness.editor.is_read_only(DocumentId::default()));
    }

    #[test]
    fn test_an_editor_moves_to_another_thread() {
        fn assert_send<T: Send>() {}
        assert_send::<Editor>();
    }
}
