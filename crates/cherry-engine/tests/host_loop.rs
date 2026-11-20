//! The loop every host runs, through the public API alone: feed keys, take
//! the requests they make, save, and keep the text's line endings.

use cherry_engine::keys::{Timestamp, parse_sequence};
use cherry_engine::{DocumentId, Editor, EditorOptions, HostRequest, ViewId};

// ========================================================================
// Functions
// ========================================================================

/// An editor with `text` open in one view.
fn open(text: &str) -> (Editor, DocumentId, ViewId) {
    let mut editor = Editor::new(EditorOptions::default());
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

/// The documents the requests taken from `editor` ask to save.
fn saves(editor: &mut Editor) -> Vec<DocumentId> {
    editor
        .take_requests()
        .into_iter()
        .filter_map(|request| match request {
            HostRequest::Save(save) => Some(save.document),
            _ => None,
        })
        .collect()
}

// ========================================================================
// Tests
// ========================================================================

#[test]
fn test_saving_clears_modified_and_undo_past_the_save_sets_it_again() {
    let (mut editor, document, view) = open("one\n");
    type_keys(&mut editor, view, "A ! <esc>");
    assert!(editor.is_modified(document));

    type_keys(&mut editor, view, ": w <cr>");
    assert_eq!(saves(&mut editor), vec![document]);
    editor.mark_saved(document);
    assert!(!editor.is_modified(document));

    type_keys(&mut editor, view, "u");
    assert!(editor.is_modified(document));
    type_keys(&mut editor, view, "C-r");
    assert!(!editor.is_modified(document));
}

#[test]
fn test_crlf_text_is_edited_with_lf_and_saved_with_crlf() {
    let (mut editor, document, view) = open("one\r\ntwo\r\n");
    type_keys(&mut editor, view, "o t h r e e <esc> j d d");

    let text = editor.document(document).expect("open");
    assert_eq!(text.to_string(), "one\nthree\n");
    assert_eq!(text.text_with_line_endings(), "one\r\nthree\r\n");
}

#[test]
fn test_requests_are_taken_once_and_only_from_the_editor_that_made_them() {
    let (mut editor, document, view) = open("one\n");
    let (mut other, _, _) = open("two\n");
    type_keys(&mut editor, view, ": w <cr>");

    assert!(other.take_requests().is_empty());
    assert_eq!(saves(&mut editor), vec![document]);
    assert!(editor.take_requests().is_empty());
}
