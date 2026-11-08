//! Benchmarks of what a user waits on: opening a large document, and one
//! keystroke followed by the render a host makes after it, on a document
//! of 100,000 lines and on one single line of a megabyte, and `:g` on
//! 10,000 lines. Run with
//! `cargo bench -p cherry`.

use std::hint::black_box;

use cherry::keys::{Key, KeyCode, Mods, Timestamp, parse_key};
use cherry::{Editor, EditorOptions, Surface, ViewId, render_view};
use criterion::{BatchSize, Criterion};

// ========================================================================
// Constants
// ========================================================================

/// The lines of the benchmarked document.
const LINES: usize = 100_000;

/// The bytes of the benchmarked single long line, about a megabyte.
const LONG_LINE_BYTES: usize = 1 << 20;

/// The lines of the document the `:g` benchmarks run on.
const GLOBAL_LINES: usize = 10_000;

/// The size of the view, a common terminal.
const VIEW_SIZE: (u16, u16) = (120, 40);

// ========================================================================
// Functions
// ========================================================================

/// A document of [`LINES`] lines of code-like text.
fn large_text() -> String {
    (0..LINES)
        .map(|line| format!("    let value_{line} = compute(\"item {line}\", {line}); // note\n"))
        .collect()
}

/// One line of code-like text, [`LONG_LINE_BYTES`] long, as minified
/// files have.
fn long_line() -> String {
    let piece = "let value = compute(\"item\", 42); ";
    piece.repeat(LONG_LINE_BYTES / piece.len())
}

/// An editor with one view onto `text`, line numbers on, the cursor at
/// its start.
fn open_at_start(text: &str) -> (Editor, ViewId) {
    let options = EditorOptions {
        line_numbers: true,
        ..EditorOptions::default()
    };
    let mut editor = Editor::new(options);
    let document = editor.open_document(text);
    let view = editor.new_view(document).expect("the document is open");
    editor.resize_view(view, VIEW_SIZE.0, VIEW_SIZE.1);
    (editor, view)
}

/// An editor with one view onto `text`, line numbers on, the cursor in
/// the middle of the document.
fn open_editor(text: &str) -> (Editor, ViewId) {
    let options = EditorOptions {
        line_numbers: true,
        ..EditorOptions::default()
    };
    let mut editor = Editor::new(options);
    let document = editor.open_document(text);
    let view = editor.new_view(document).expect("the document is open");
    editor.resize_view(view, VIEW_SIZE.0, VIEW_SIZE.1);
    let middle = format!("{}G", LINES / 2);
    for c in middle.chars() {
        editor.handle_key(view, key(&c.to_string()), Timestamp::default());
    }
    (editor, view)
}

/// The key `notation` writes.
fn key(notation: &str) -> Key {
    parse_key(notation).expect("valid key notation")
}

/// Opening a document of [`LINES`] lines and making a view onto it.
fn bench_open(c: &mut Criterion) {
    let text = large_text();
    c.bench_function("open 100k lines", |b| {
        b.iter(|| {
            let mut editor = Editor::new(EditorOptions::default());
            let document = editor.open_document(black_box(&text));
            black_box(editor.new_view(document))
        });
    });
}

/// One key then a render, for moving, deleting and undoing, and typing,
/// and a render alone; typing last, as it grows the cursor's line.
fn bench_keystroke(c: &mut Criterion) {
    let text = large_text();
    let mut surface = Surface::new(0, 0);
    let (mut editor, view) = open_editor(&text);
    c.bench_function("render only, 100k lines", |b| {
        b.iter(|| black_box(render_view(&editor, view, &mut surface)));
    });
    let (down, up) = (key("j"), key("k"));
    let mut toggle = false;
    c.bench_function("j or k then render, 100k lines", |b| {
        b.iter(|| {
            toggle = !toggle;
            let pressed = if toggle { down } else { up };
            editor.handle_key(view, pressed, Timestamp::default());
            black_box(render_view(&editor, view, &mut surface))
        });
    });
    c.bench_function("dd then u then render, 100k lines", |b| {
        b.iter(|| {
            for notation in ["d", "d", "u"] {
                editor.handle_key(view, key(notation), Timestamp::default());
            }
            black_box(render_view(&editor, view, &mut surface))
        });
    });
    editor.handle_key(view, key("A"), Timestamp::default());
    let typed = key("x");
    c.bench_function("type a char then render, 100k lines", |b| {
        b.iter(|| {
            editor.handle_key(view, typed, Timestamp::default());
            black_box(render_view(&editor, view, &mut surface))
        });
    });
}

/// A render and a keystroke on a single line of a megabyte, with the
/// cursor near its start and at its end.
fn bench_long_line(c: &mut Criterion) {
    let text = long_line();
    let mut surface = Surface::new(0, 0);
    let (mut editor, view) = open_at_start(&text);
    c.bench_function("render only, 1 MB line, cursor at start", |b| {
        b.iter(|| black_box(render_view(&editor, view, &mut surface)));
    });
    let (right, left) = (key("l"), key("h"));
    let mut toggle = false;
    c.bench_function("l or h then render, 1 MB line", |b| {
        b.iter(|| {
            toggle = !toggle;
            let pressed = if toggle { right } else { left };
            editor.handle_key(view, pressed, Timestamp::default());
            black_box(render_view(&editor, view, &mut surface))
        });
    });
    editor.handle_key(view, key("$"), Timestamp::default());
    c.bench_function("render only, 1 MB line, cursor at end", |b| {
        b.iter(|| black_box(render_view(&editor, view, &mut surface)));
    });
    c.bench_function("h or l then render, 1 MB line, at end", |b| {
        b.iter(|| {
            toggle = !toggle;
            let pressed = if toggle { left } else { right };
            editor.handle_key(view, pressed, Timestamp::default());
            black_box(render_view(&editor, view, &mut surface))
        });
    });
}

/// `:g` over every line or every other line of a document of
/// [`GLOBAL_LINES`] lines, each run on a fresh editor.
fn bench_global(c: &mut Criterion) {
    let text: String = (0..GLOBAL_LINES)
        .map(|line| {
            format!(
                "{} line {line}\n",
                if line % 2 == 0 { "even" } else { "odd" }
            )
        })
        .collect();
    for command in [":g/even/d", ":g/even/s/line/row/", ":g/^/m0"] {
        let name = format!("{command} then render, 10k lines");
        c.bench_function(&name, |b| {
            b.iter_batched(
                || open_at_start(&text),
                |(mut editor, view)| {
                    let mut surface = Surface::new(0, 0);
                    for c in command.chars() {
                        editor.handle_key(
                            view,
                            Key::new(KeyCode::Char(c), Mods::NONE),
                            Timestamp::default(),
                        );
                    }
                    editor.handle_key(view, key("<cr>"), Timestamp::default());
                    black_box(render_view(&editor, view, &mut surface))
                },
                BatchSize::LargeInput,
            );
        });
    }
}

/// Runs every benchmark with the options given on the command line.
fn main() {
    let mut criterion = Criterion::default().configure_from_args();
    bench_open(&mut criterion);
    bench_keystroke(&mut criterion);
    bench_long_line(&mut criterion);
    bench_global(&mut criterion);
    criterion.final_summary();
}
