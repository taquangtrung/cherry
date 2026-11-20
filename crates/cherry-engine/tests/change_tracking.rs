//! Change tracking as a language server client uses it: a host that applies
//! every tracked edit, in the order given, to its own copy of the text
//! keeps that copy equal to the document.

use cherry_engine::keys::{Timestamp, parse_sequence};
use cherry_engine::{DocumentChange, Editor, EditorOptions};

// ========================================================================
// Functions
// ========================================================================

/// `text` with the edits of `change` applied in the order given, each
/// range in chars of the text as it is when that edit applies.
fn replay(text: &str, change: &DocumentChange) -> String {
    let mut chars: Vec<char> = text.chars().collect();
    for edit in &change.edits {
        chars.splice(edit.range.clone(), edit.text.chars());
    }
    chars.into_iter().collect()
}

// ========================================================================
// Tests
// ========================================================================

#[test]
fn test_replaying_tracked_edits_rebuilds_the_text_through_undo_and_redo() {
    let original = "fn main() {\n    let x = 1;\n}\n";
    let mut editor = Editor::new(EditorOptions::default());
    let document = editor.open_document(original);
    let view = editor.new_view(document).expect("the document is open");
    editor.set_change_tracking(document, true);

    let notation = "j c i w y <esc> j o é ü <esc> k J . u u C-r : % s / x / z / g <cr>";
    let mut copy = original.to_owned();
    let mut version = editor.document_version(document).expect("open");
    for key in parse_sequence(notation).expect("valid notation") {
        editor.handle_key(view, key, Timestamp::default());
        for change in editor.take_changes(document) {
            assert!(change.version > version, "versions only rise");
            version = change.version;
            copy = replay(&copy, &change);
        }
        let text = editor.document(document).expect("open").to_string();
        assert_eq!(copy, text);
    }
}

#[test]
fn test_untracked_documents_queue_no_changes() {
    let mut editor = Editor::new(EditorOptions::default());
    let document = editor.open_document("one\n");
    let view = editor.new_view(document).expect("the document is open");

    for key in parse_sequence("d d").expect("valid notation") {
        editor.handle_key(view, key, Timestamp::default());
    }
    assert!(editor.take_changes(document).is_empty());
}
