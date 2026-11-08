//! Host requests: what the editor asks of the application embedding it.
//!
//! The editor never touches the file system, the window, or the screen
//! outside the views it paints. When a command needs one of those (saving,
//! quitting, telling the user something), the editor queues a
//! [`HostRequest`], and the host takes the queue with
//! [`Editor::take_requests`](crate::Editor::take_requests) after feeding
//! keys, and acts on each request in order.

use super::command::{CommandContext, CommandId};
use super::editor::{DocumentId, Editor, ViewId};

// ========================================================================
// Constants
// ========================================================================

/// The notice `:q` gives in place of closing a modified document.
pub(crate) const UNSAVED_NOTICE: &str = "No write since last change (add ! to override)";

// ========================================================================
// Data Structures
// ========================================================================

/// Something the editor asks the host to do.
///
/// New variants may come in a minor release, so a host's `match` needs a
/// fallback arm.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum HostRequest {
    /// Escape or `C-g` was pressed in a view of a single-line document
    /// edited with Emacs keys, or Escape in Normal mode with nothing
    /// pending in one edited with the Vim modes: a host showing it as a
    /// field may close the field and drop what was typed.
    Cancel(CancelRequest),
    /// Run a command the host registered to run itself, with
    /// [`Editor::register_host_command`](crate::Editor::register_host_command).
    Command(CommandRequest),
    /// Show the user a message.
    Notice(NoticeRequest),
    /// Open a file in a view (`:e`), or load the view's document again.
    Open(OpenRequest),
    /// Close a view; the host decides what closing means for it (the
    /// window, the whole application).
    Quit(QuitRequest),
    /// Read a file into a view's document below a line (`:r`).
    ReadInto(ReadIntoRequest),
    /// Save a document wherever the host keeps it.
    Save(SaveRequest),
    /// Enter was pressed in a view of a single-line document: a host
    /// showing it as a field may act on the text (rename, search) and
    /// close the field.
    Submit(SubmitRequest),
}

/// A request to cancel a single-line field, from Escape or `C-g` (Escape
/// in Normal mode for [`FieldEditing::Modal`](crate::FieldEditing::Modal));
/// the view stays in its mode. See [`Editor::open_single_line_document`](crate::Editor::open_single_line_document).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CancelRequest {
    /// The view Escape was pressed in.
    pub view: ViewId,
}

/// A request to run a command the host runs with its own state (a
/// language server's hover, a git blame), from a key bound to it or a
/// palette. See
/// [`Editor::register_host_command`](crate::Editor::register_host_command).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommandRequest {
    /// The command, as the host registered it.
    pub command: CommandId,
    /// The count typed before its keys, if any.
    pub count: Option<usize>,
    /// The register selected with `"` before its keys, if any.
    pub register: Option<char>,
    /// The view the keys were typed in.
    pub view: ViewId,
}

/// A message for the user, such as why a command did nothing.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NoticeRequest {
    /// How it reads: a failure, a warning, or a report.
    pub level: NoticeLevel,
    /// The message, one line.
    pub message: String,
}

/// What kind of message a [`NoticeRequest`] is, for the host to show each
/// in its own color, or to keep reports out of a message log.
///
/// New variants may come in a minor release, so a host's `match` needs a
/// fallback arm.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[non_exhaustive]
pub enum NoticeLevel {
    /// A command could not do what was asked, and says why: Vim's error
    /// messages (`Pattern not found`, `No write since last change`).
    Error,
    /// A command did what was asked and reports on it: an option's value,
    /// how many lines matched.
    Info,
    /// A command did what was asked, with something to know: a search
    /// that wrapped around the end of the document.
    Warning,
}

/// A request to open a file in a view, from `:e {file}`. Cherry reads no
/// files: the host opens the document (with
/// [`Editor::open_document`](crate::Editor::open_document)) and shows it
/// where it sees fit: in the view
/// ([`Editor::show_document`](crate::Editor::show_document)) or beside it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OpenRequest {
    /// Whether to open it even if the view's document has unsaved
    /// changes (`:e!`), discarding them; otherwise a host should refuse to
    /// drop a modified document
    /// ([`Editor::is_modified`](crate::Editor::is_modified)).
    pub force: bool,
    /// The file as typed, or `None` for `:e` alone: load the view's own
    /// document again from where the host keeps it.
    pub path: Option<String>,
    /// The view the command was typed in.
    pub view: ViewId,
}

/// A request to read a file into a document, from `:r {file}`. The host
/// reads it and puts its text in with
/// [`Editor::insert_lines`](crate::Editor::insert_lines), passing `line`
/// on.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReadIntoRequest {
    /// The line the text goes below, counted from 1; 0 puts it above the
    /// first line (`:0r`).
    pub line: usize,
    /// The file as typed, or `None` for `:r` alone: the view's own
    /// document as the host keeps it.
    pub path: Option<String>,
    /// The view whose document the text goes into.
    pub view: ViewId,
}

/// A request to close a view.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct QuitRequest {
    /// Whether to close it even if its document has unsaved changes
    /// (`:q!`, `Z Q`), discarding them; otherwise a host may refuse.
    pub force: bool,
    /// The view to close.
    pub view: ViewId,
}

/// A request to save a document. The host reads the text to write from
/// [`Document::text_with_line_endings`](crate::Document::text_with_line_endings),
/// which restores the line ending the document was opened with.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SaveRequest {
    /// The document to save.
    pub document: DocumentId,
}

/// A request to submit a single-line field, from Enter (or `C-j`, `C-m`)
/// in Insert or Replace mode, or Enter in Normal mode; the view stays in
/// its mode. See
/// [`Editor::open_single_line_document`](crate::Editor::open_single_line_document).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SubmitRequest {
    /// The view Enter was pressed in, whose document holds the text.
    pub view: ViewId,
}

// ========================================================================
// Functions
// ========================================================================

/// Asks the host to close the view, discarding changes: `Z Q`.
pub(crate) fn quit(cx: &mut CommandContext<'_>) {
    request_quit(cx.editor, cx.view, true);
}

/// Asks the host to close `view`, unless `force` is false and closing it
/// would leave its modified document in no view, which gives a notice
/// instead (`:q` on a modified document).
pub(crate) fn quit_unless_unsaved(editor: &mut Editor, view: ViewId, force: bool) {
    let document = editor.view_ref(view).document();
    let loses_changes = editor.is_modified(document) && !editor.is_shown_elsewhere(view);
    if !force && loses_changes {
        return editor.error(UNSAVED_NOTICE.to_string());
    }
    request_quit(editor, view, force);
}

/// Asks the host to close `view`, forcibly when `force`.
pub(crate) fn request_quit(editor: &mut Editor, view: ViewId, force: bool) {
    let request = QuitRequest { force, view };
    editor.request(HostRequest::Quit(request));
}

/// Asks the host to save the document `view` shows.
pub(crate) fn request_save(editor: &mut Editor, view: ViewId) {
    let document = editor.view_ref(view).document();
    editor.request(HostRequest::Save(SaveRequest { document }));
}

/// Asks the host to cancel the single-line field `view` shows.
pub(crate) fn request_cancel(editor: &mut Editor, view: ViewId) {
    editor.request(HostRequest::Cancel(CancelRequest { view }));
}

/// Asks the host to submit the single-line field `view` shows.
pub(crate) fn request_submit(editor: &mut Editor, view: ViewId) {
    editor.request(HostRequest::Submit(SubmitRequest { view }));
}

/// Asks the host to save the view's document when it is modified, then to
/// close the view: `Z Z`, as `:x`.
pub(crate) fn save_and_quit(cx: &mut CommandContext<'_>) {
    save_if_modified_and_quit(cx.editor, cx.view);
}

/// Asks the host to save the document `view` shows when it is modified,
/// then to close `view`.
pub(crate) fn save_if_modified_and_quit(editor: &mut Editor, view: ViewId) {
    let document = editor.view_ref(view).document();
    if editor.is_modified(document) {
        request_save(editor, view);
    }
    request_quit(editor, view, false);
}

// ========================================================================
// Tests
// ========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::Harness;
    use crate::view::Mode;

    #[test]
    fn test_a_host_command_comes_back_as_a_request_with_its_view_count_and_register() {
        let mut harness = Harness::new("|a");
        harness.editor.register_host_command("lsp.hover").unwrap();
        harness
            .editor
            .bind(Mode::Normal, &["g h"], "lsp.hover")
            .unwrap();
        harness.keys("\" a 3 g h");
        let request = CommandRequest {
            command: CommandId::from("lsp.hover"),
            count: Some(3),
            register: Some('a'),
            view: harness.view,
        };
        assert_eq!(harness.requests(), [HostRequest::Command(request)]);
        assert!(
            harness.editor.register_host_command("lsp.hover").is_err(),
            "an id is never replaced"
        );
    }
}
