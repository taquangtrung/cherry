//! Commands a host adds through the public API: registering, binding,
//! rebinding, and the typed errors for each mistake.

use cherry_engine::keys::{Timestamp, parse_sequence};
use cherry_engine::{CommandError, DocumentId, Edit, Editor, EditorOptions, Mode, ViewId};

// ========================================================================
// Constants
// ========================================================================

/// The id the tests register their command under.
const PREFIX_ID: &str = "host-prefix-line";

// ========================================================================
// Functions
// ========================================================================

/// An editor with `text` open in one view and the command [`PREFIX_ID`]
/// registered: it puts one `>` per count at the start of the document.
fn open(text: &str) -> (Editor, DocumentId, ViewId) {
    let mut editor = Editor::new(EditorOptions::default());
    editor
        .register_command(PREFIX_ID, |cx| {
            let prefix = ">".repeat(cx.count_or_one());
            cx.editor
                .apply_edits(cx.view, vec![Edit::new(0..0, prefix)]);
        })
        .expect("the id is free");
    let document = editor.open_document(text);
    let view = editor.new_view(document).expect("the document is open");
    (editor, document, view)
}

/// Feeds the keys of `notation` to `view`, as a host would one by one.
fn type_keys(editor: &mut Editor, view: ViewId, notation: &str) {
    for key in parse_sequence(notation).expect("valid notation") {
        editor.handle_key(view, key, Timestamp::default());
    }
}

/// The text of `document`.
fn text(editor: &Editor, document: DocumentId) -> String {
    editor.document(document).expect("open").to_string()
}

// ========================================================================
// Tests
// ========================================================================

#[test]
fn test_a_bound_host_command_runs_with_its_count_and_undoes_in_one_step() {
    let (mut editor, document, view) = open("line\n");
    editor
        .bind(Mode::Normal, &["g p"], PREFIX_ID)
        .expect("valid binding");

    type_keys(&mut editor, view, "3 g p");
    assert_eq!(text(&editor, document), ">>>line\n");
    type_keys(&mut editor, view, "u");
    assert_eq!(text(&editor, document), "line\n");
}

#[test]
fn test_overriding_keys_moves_a_command_off_its_old_keys() {
    let (mut editor, document, view) = open("line\n");
    editor
        .bind(Mode::Normal, &["g p"], PREFIX_ID)
        .expect("valid binding");
    editor
        .override_keys(Mode::Normal, PREFIX_ID, &["g P"])
        .expect("valid override");

    type_keys(&mut editor, view, "g p");
    assert_eq!(text(&editor, document), "line\n");
    type_keys(&mut editor, view, "g P");
    assert_eq!(text(&editor, document), ">line\n");
}

#[test]
fn test_each_mistake_is_its_own_error_variant() {
    let (mut editor, _, _) = open("");

    let duplicate = editor.register_command(PREFIX_ID, |_| {});
    assert!(matches!(duplicate, Err(CommandError::DuplicateId(_))));
    let built_in = editor.register_command("undo", |_| {});
    assert!(matches!(built_in, Err(CommandError::DuplicateId(_))));

    let unknown = editor.bind(Mode::Normal, &["g p"], "no-such-command");
    assert!(matches!(unknown, Err(CommandError::UnknownId(_))));
    let bad_spec = editor.bind(Mode::Normal, &["<nope>"], PREFIX_ID);
    assert!(matches!(bad_spec, Err(CommandError::InvalidSpec(_))));
}
