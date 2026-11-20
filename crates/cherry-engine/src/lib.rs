//! Cherry Editor, an embeddable text editor. Its own editing style is the
//! Vim grammar (counts, operators, motions, text objects) where you
//! navigate and Emacs editing where you type; a host may pick strict Vim,
//! modeless Emacs, or a desktop editor's keys instead ([`EditingStyle`]).
//! The host feeds it keys and paints the rows it produces, in whatever
//! toolkit the host is built with.
//!
//! - [`Document`] holds the text in a rope, with `\n` line breaks and the
//!   line ending it came with remembered for saving; offsets are chars, and
//!   a cursor steps by grapheme cluster.
//! - [`Selection`] holds one or more [`Range`]s (anchor and head), kept
//!   sorted with overlaps merged, one of them primary.
//! - [`ChangeSet`] describes an edit of the whole document, made from
//!   [`Edit`]s; it applies, inverts for undo, composes, and maps positions
//!   and selections across the edit.
//! - [`History`] records committed changes for undo and redo, with groups
//!   the host opens and closes to make one undo step of many edits.
//! - [`Editor`] holds documents and views in arenas, named by [`DocumentId`]
//!   and [`ViewId`]; the host feeds it keys with [`Editor::handle_key`],
//!   settles timed out key sequences with [`Editor::tick`], and acts on the
//!   [`HostRequest`]s it takes with [`Editor::take_requests`].
//! - A host adds its own commands with [`Editor::register_command`], each
//!   run with a [`CommandContext`] as the built-in commands are and named
//!   by a [`CommandId`] it may build at runtime, binds keys
//!   with [`Editor::bind`], and applies a user's rebinding with
//!   [`Editor::override_keys`], or a whole keymap file with
//!   [`Editor::apply_keymap`] ([`KeymapFile`], read with the `serde`
//!   feature); mistakes come back as [`CommandError`]s.
//! - Yanked and deleted text goes to Vim's registers, selected with `"`;
//!   registers `+` and `*` go through the host's [`Clipboard`], set with
//!   [`Editor::set_clipboard`] (a [`MemoryClipboard`] until then), and
//!   with [`EditorOptions::unnamed_clipboard`] the unnamed register is
//!   the system clipboard.
//! - `m` sets marks that edits carry along, `` ` `` and `'` go to them,
//!   and each view keeps a jumplist walked with `C-o` and `C-i`.
//! - `.` repeats the last change, recorded as its keys; `q` records keys
//!   into a register as key notation and `@` replays them
//!   ([`View::recording`] says when a view is recording).
//! - [`View`] is a window onto a document: a cursor, its own [`Mode`] and
//!   pending keys, a size in cells, and a [`ScrollOffset`] that follows the
//!   cursor with a margin, laid out with [`EditorOptions`] (soft wrap, tab
//!   width, line numbers).
//! - `/` and `?` open a [`Prompt`] on the view, in [`Mode::CommandLine`],
//!   edited with the Emacs keys of Insert mode; search patterns are
//!   regular expressions with smart case. `:` opens the ex command line
//!   (`:w`, `:q`, `:s`, `:set`, ...), and a host adds ex commands with
//!   [`Editor::register_ex_command`], each run with an [`ExContext`].
//! - Insert mode keeps the indent of a line opened from another and, when
//!   asked, types brackets and quotes in pairs ([`EditorOptions`]); `gc`
//!   comments and uncomments lines with the token a host sets with
//!   [`Editor::set_comment_token`] (see [`comment_token_for_extension`]).
//! - Each document has a version raised by every change of its text; a
//!   host that tracks a document's changes with
//!   [`Editor::set_change_tracking`] takes them as [`DocumentChange`]s
//!   (edits as [`TextChange`]s with char offsets and [`Utf16Position`]s)
//!   with [`Editor::take_changes`], for language server sync.
//! - [`Editor::open_single_line_document`] opens the text of a one-line
//!   field that no edit may break, edited with Emacs keys in Insert mode
//!   it never leaves, or with the Vim modes ([`FieldEditing`]); Enter
//!   asks the host to submit it ([`SubmitRequest`]) and Escape to cancel
//!   ([`CancelRequest`]).
//! - [`EditingStyle`] picks the keys: Cherry's mix of the Vim modes and
//!   Emacs keys in Insert mode (the default), strict Vim, modeless Emacs
//!   with its mark and region, or a desktop text editor's keys, where
//!   Shift with a motion key selects and Ctrl+C, X, V, Z, and Y copy, cut,
//!   paste, undo, and redo.
//! - Right-to-left and mixed-direction lines paint in visual order, and
//!   [`Editor::accessible_view`] gives a screen reader the lines in sight,
//!   the cursor, and the selections ([`AccessibleView`]).
//! - [`Editor::saved_history`] takes a document's undo history out, and
//!   [`Editor::restore_history`] puts it back in a later session; with the
//!   `serde` feature, [`SavedHistory`] serializes.
//! - Each view folds lines: `zc` folds a block by its indent, or by the
//!   ranges a host gives with [`Editor::set_fold_ranges`], and a closed
//!   fold shows its first line with a marker ([`Style::Folded`]).
//! - [`Editor::set_read_only`] makes a document a preview: everything
//!   but editing works, and every edit is refused with a notice.
//! - A host lays [`Decoration`]s (diagnostics, highlights, underlines) over
//!   a document's text and [`Sign`]s in its gutter, each set under an owner
//!   name with [`Editor::set_decorations`] and [`Editor::set_signs`]; edits
//!   carry them along.
//! - [`Editor::set_inlays`] lays [`Inlay`]s into a document's lines: text
//!   that is not the document's own (inlay hints, ghost text), laid out,
//!   painted, and clicked through with the text, and carried through edits.
//!   [`Editor::set_conceals`] hides markup (Markdown's `**`) that stays in
//!   the text, shown again on the line a view's cursor is on.
//! - A host's [`Highlighter`], set per document with
//!   [`Editor::set_highlighter`], is told of every change and asked for
//!   the [`HighlightSpan`]s in sight; the `syntax` module is one, built on
//!   tree-sitter.
//! - A view may have several cursors ([`Editor::set_cursors`],
//!   [`View::cursors`], and the `add-cursor-*` commands): every key typed
//!   runs at each of them, as one undo step.
//! - [`Editor::set_preedit`] shows what an input method is composing at
//!   the cursor, laid into the line without touching the document.
//! - [`Editor::handle_mouse`] takes a host's [`MouseEvent`]s in cells:
//!   clicks move the cursor, drags select, double and triple clicks select
//!   a word or a line, the wheel scrolls.
//! - [`render_view`] paints a view onto a [`Surface`] of styled [`Cell`]s
//!   and says where to draw the [`Cursor`]; the host draws both.
//!   [`render_prompt`] paints the view's prompt as one row, for the host
//!   to show below the view.
//!
//! # Example
//!
//! A host makes an editor, opens text, feeds it keys, renders a view, and
//! acts on what the editor asks of it:
//!
//! ```
//! use cherry_engine::{render_view, Editor, EditorOptions, HostRequest, Mode, Surface};
//! use cherry_engine::keys::{parse_sequence, Timestamp};
//!
//! let mut editor = Editor::new(EditorOptions::default());
//! let document = editor.open_document("hello world\n");
//! let view = editor.new_view(document).expect("the document is open");
//! editor.resize_view(view, 20, 3);
//!
//! // `cw` changes the first word, Escape goes back to Normal mode, `:w`
//! // asks the host to save.
//! for key in parse_sequence("c w h o w d y <esc> : w <cr>").expect("valid notation") {
//!     editor.handle_key(view, key, Timestamp::default());
//! }
//! assert_eq!(editor.view(view).map(|view| view.mode()), Some(Mode::Normal));
//!
//! let mut surface = Surface::new(0, 0);
//! let cursor = render_view(&editor, view, &mut surface).expect("the cursor is in sight");
//! assert_eq!(surface.row_text(0), "howdy world         ");
//! assert_eq!((cursor.column, cursor.row), (4, 0));
//!
//! for request in editor.take_requests() {
//!     if let HostRequest::Save(save) = request {
//!         let text = editor.document(save.document).expect("open").text_with_line_endings();
//!         assert_eq!(text, "howdy world\n");
//!         // ... write `text` to disk, then:
//!         editor.mark_saved(save.document);
//!     }
//! }
//! assert!(!editor.is_modified(document));
//! ```
//!
//! See `docs/embedding.md` in the repository for a guide to every part a
//! host touches.

mod block;
mod change;
mod char_find;
mod command;
mod comment;
mod completion;
mod cursors;
mod decoration;
mod document;
mod edit;
mod editor;
mod emacs;
mod ex;
mod expand;
mod fold;
mod format;
mod highlight;
mod history;
mod insert;
pub mod keys;
mod kill;
mod line_diff;
mod line_edit;
mod mark;
pub mod markup;
mod motion;
mod mouse;
mod operator;
mod options;
mod paint;
mod phrase;
mod prompt;
mod region;
mod register;
mod render;
mod repeat;
mod replace;
mod request;
mod screen;
mod search;
mod selection;
mod sexp;
mod snippet;
mod standard;
mod structure;
mod substitute;
mod surround;
#[cfg(any(feature = "syntax-engine", feature = "syntax-textmate"))]
pub mod syntax;
#[cfg(test)]
mod testing;
mod text_motion;
mod text_object;
mod transaction;
mod undo;
mod view;
mod vim_insert;
mod visual;
mod wrap;

pub use change::{DocumentChange, TextChange};
pub use command::{CommandContext, CommandError, CommandId, KeyHint};
pub use comment::{block_comment_tokens_for_extension, comment_token_for_extension};
pub use completion::{
    Completion, CompletionAnswer, CompletionItem, CompletionKind, CompletionProvider,
    CompletionRequest, CompletionTicket,
};
pub use decoration::{
    ChangedLine, Decoration, DecorationKind, GutterLane, Inlay, LineChange, Sign, VirtualLine,
};
pub use document::{Document, LineEnding, Position, RangeError, Utf16Position};
pub use editor::{
    AccessibleLine, AccessibleRange, AccessibleView, DocumentId, Editor, FieldEditing, Handled,
    HistoryError, KeymapEntry, KeymapFile, SavedHistory, ViewId,
};
pub use ex::{ExContext, LineRange};
pub use expand::SelectionRangeProvider;
pub use highlight::{
    EnterIndent, HighlightId, HighlightSpan, Highlighter, IndentChange, Preparation,
};
pub use history::{History, WalkedStep};
pub use mouse::{ClickCounter, MouseEvent, MouseEventKind};
pub use options::{BreakIndent, ConcealMode, EditingStyle, EditorOptions, ViewOptions, WrapMode};
pub use paint::{Cell, Cursor, CursorShape, Layers, Style, Surface};
pub use prompt::{Prompt, PromptKind};
pub use register::{Clipboard, ClipboardSelection, MemoryClipboard};
pub use render::{render_prompt, render_view};
pub use request::{
    CancelRequest, CommandRequest, HostRequest, NoticeLevel, NoticeRequest, OpenRequest,
    QuitRequest, ReadIntoRequest, SaveRequest, SubmitRequest,
};
pub use search::SearchCount;
pub use selection::{Range, Selection};
pub use transaction::{Assoc, ChangeError, ChangeSet, Edit, LengthMismatch, Unordered};
pub use view::{Mode, ScrollOffset, View, ViewRows};
