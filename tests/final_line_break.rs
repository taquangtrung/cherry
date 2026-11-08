//! A text ending in a line break has no empty line after it, as in Vim:
//! no motion reaches one, no operator acts on one, and deleting the last
//! line keeps the text ending in a line break.

use cherry::keys::{Timestamp, parse_sequence};
use cherry::{Editor, EditorOptions, MouseEvent, MouseEventKind, Surface, render_view};

// ========================================================================
// Constants
// ========================================================================

/// Three lines, the last ending in a line break: `two` starts at 4,
/// `three` at 8, and the final break is at 13.
const TEXT: &str = "one\ntwo\nthree\n";

// ========================================================================
// Functions
// ========================================================================

/// The text and cursor after typing `notation` into a view of `text`.
fn after(text: &str, notation: &str) -> (String, usize) {
    let mut editor = Editor::new(EditorOptions::default());
    let document = editor.open_document(text);
    let view = editor.new_view(document).expect("the document is open");
    editor.resize_view(view, 20, 10);
    for key in parse_sequence(notation).expect("valid notation") {
        editor.handle_key(view, key, Timestamp::default());
    }
    let text = editor.document(document).expect("open").to_string();
    let cursor = editor.view(view).expect("open").cursor();
    (text, cursor)
}

// ========================================================================
// Tests
// ========================================================================

#[test]
fn test_line_motions_stop_on_the_last_line() {
    for notation in ["G", "j j j", "5 j", "j j +", "L"] {
        assert_eq!(after(TEXT, notation), (TEXT.to_owned(), 8), "{notation}");
    }
    assert_eq!(after(TEXT, "G $"), (TEXT.to_owned(), 12));
}

#[test]
fn test_deleting_the_last_line_keeps_the_final_line_break() {
    let two_lines = ("one\ntwo\n".to_owned(), 4);
    assert_eq!(after(TEXT, "G d d"), two_lines);
    assert_eq!(after(TEXT, "j j d d"), two_lines);
    assert_eq!(after(TEXT, "G V d"), two_lines);
    assert_eq!(after(TEXT, "j j d k"), ("one\n".to_owned(), 0));
    assert_eq!(after(TEXT, "j j d d d d"), ("one\n".to_owned(), 0));
    assert_eq!(after(TEXT, "d G"), (String::new(), 0));
}

#[test]
fn test_an_operator_past_the_last_line_does_nothing() {
    assert_eq!(after(TEXT, "G d j"), (TEXT.to_owned(), 8));
    assert_eq!(after(TEXT, "G J"), (TEXT.to_owned(), 8));
}

#[test]
fn test_appending_and_putting_at_the_last_line_keep_one_final_break() {
    assert_eq!(
        after(TEXT, "G A ! <esc>"),
        ("one\ntwo\nthree!\n".to_owned(), 13)
    );
    assert_eq!(
        after(TEXT, "y y G p"),
        ("one\ntwo\nthree\none\n".to_owned(), 14)
    );
    assert_eq!(
        after(TEXT, "G o x <esc>"),
        ("one\ntwo\nthree\nx\n".to_owned(), 14)
    );
}

#[test]
fn test_a_click_below_the_text_lands_on_the_last_line() {
    let mut editor = Editor::new(EditorOptions::default());
    let document = editor.open_document(TEXT);
    let view = editor.new_view(document).expect("the document is open");
    editor.resize_view(view, 20, 10);

    editor.handle_mouse(view, MouseEvent::new(MouseEventKind::Press, 0, 5));
    let cursor = editor.view(view).expect("open").cursor();
    assert!((8..13).contains(&cursor), "on `three`, at {cursor}");
}

#[test]
fn test_no_row_is_painted_for_a_line_after_the_final_break() {
    let options = EditorOptions {
        line_numbers: true,
        ..EditorOptions::default()
    };
    let mut editor = Editor::new(options);
    let document = editor.open_document(TEXT);
    let view = editor.new_view(document).expect("the document is open");
    editor.resize_view(view, 20, 5);

    let mut surface = Surface::new(0, 0);
    render_view(&editor, view, &mut surface);
    assert!(surface.row_text(2).contains("three"));
    assert!(
        !surface.row_text(3).contains('4'),
        "{:?}",
        surface.row_text(3)
    );

    let typed = editor.open_document("ab");
    editor.show_document(view, typed);
    for key in parse_sequence("A <cr>").expect("valid notation") {
        editor.handle_key(view, key, Timestamp::default());
    }
    render_view(&editor, view, &mut surface);
    assert!(
        surface.row_text(1).contains('2'),
        "the line just typed shows"
    );
}
