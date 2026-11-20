# Cherry Editor

Cherry Editor is an embeddable text editor for Rust applications. Its own editing style is the Vim grammar where you navigate and Emacs editing where you type; a host can instead give its users strict Vim, modeless Emacs, or the keys of a desktop text editor. Cherry owns the text, the cursor, the modes, and the keys; the application that embeds it owns the window and paints the rows Cherry produces, in whatever toolkit it uses.

> Status: early. The API may change between versions.

## Usage

```bash
# Cherry on the misty GUI framework, with TextMate highlighting, the default:
cargo add cherry
# Cherry alone, for an app on any other toolkit or in a terminal:
cargo add cherry --no-default-features
# either, with tree-sitter highlighting and text objects:
cargo add cherry --features syntax
```

An app built on the [misty](https://github.com/taquangtrung/misty) GUI framework needs no host code of its own: `cherry::misty` resolves misty's keys with Cherry's keymap and shows the editor as a misty widget, `cherry::misty::EditorView`. It powers the editors of [winter-term](https://github.com/taquangtrung/winter-term).

Any other host drives the editor itself. Make an editor, open text, feed it keys, paint a view, and act on what it asks:

```rust
use cherry::keys::{parse_sequence, Timestamp};
use cherry::{render_view, Editor, EditorOptions, HostRequest, Surface};

let mut editor = Editor::new(EditorOptions::default());
let document = editor.open_document("hello world\n");
let view = editor.new_view(document).expect("the document is open");
editor.resize_view(view, 20, 3);

// `cw` changes the first word, Escape leaves Insert mode, `:w` asks to save.
for key in parse_sequence("c w h o w d y <esc> : w <cr>").expect("valid notation") {
    editor.handle_key(view, key, Timestamp::default());
}

let mut surface = Surface::new(0, 0);
render_view(&editor, view, &mut surface);
assert_eq!(surface.row_text(0), "howdy world         ");

for request in editor.take_requests() {
    if let HostRequest::Save(save) = request {
        // Write the text to disk, then:
        editor.mark_saved(save.document);
    }
}
```

[`crates/cherry-engine/examples/term.rs`](https://github.com/taquangtrung/cherry/blob/main/crates/cherry-engine/examples/term.rs) is a complete terminal host, [`examples/cherry-egui`](https://github.com/taquangtrung/cherry/blob/main/examples/cherry-egui/src/main.rs) a complete GUI host on egui (`make gui`), and the [embedding guide](https://github.com/taquangtrung/cherry/blob/main/docs/embedding.md) walks through every part a host touches.

## Crates

`cherry` re-exports two crates, which an app may also depend on alone:

- [`cherry-engine`](https://github.com/taquangtrung/cherry/tree/main/crates/cherry-engine): the editor itself, toolkit-free; everything at the top of `cherry`.
- [`cherry-misty`](https://github.com/taquangtrung/cherry/tree/main/crates/cherry-misty): the editor on misty; `cherry::misty`, with the default `misty` feature.

## What it has

- The Vim grammar: counts, operators, motions, text objects, registers, marks, jumps, dot repeat, macros, visual modes, surround, and an ex command line with `:s`.
- Emacs editing in Insert mode and in prompts: kills and the kill ring, transposes, balanced expressions.
- Four editing styles, picked with one option: Cherry's mix of the two above (the default), Vim's stock keys, Emacs's stock keys with no modes and the mark and region, and a desktop editor's keys (Shift selects; Ctrl+C, X, V, Z copy, cut, paste, undo) for users who know neither.
- Undo history that can be saved and restored across sessions (with the `serde` feature).
- Folding by indent or by the ranges a language server gives, per view.
- Search with highlighting and a match count, as the pattern is typed.
- Completion from open documents and from the host (a language server, snippets), with tab-stop snippets.
- Multiple cursors, input methods, and a toolkit-free mouse API.
- Right-to-left and mixed-direction text in visual order, and a plain-data view of the text for screen readers.
- What a host lays on the text: decorations, gutter signs, inlays, virtual lines, and conceals, carried through edits.
- Document versions and change queues for language server sync, single-line documents for fields, and read-only documents for previews.
- One command registry for host commands, key bindings, and user rebinding, with keymap files read through serde.

## Modules

| Module | Holds |
| --- | --- |
| `cherry` | The editor: documents, views, modes, commands, search, and the paint model a host draws. |
| `cherry::keys` | Keys, multi-key sequences, and key tables, independent of any window toolkit. |
| `cherry::markup` | Markdown conceals, per-format outlines, and Markdown writing commands. |
| `cherry::misty` | The editor as a misty widget, misty's keys resolved by Cherry's keymap, behind the default `misty` feature. |
| `cherry::syntax` | Tree-sitter highlighting and text objects for Rust, Python, JSON, TOML, and Markdown, and TextMate highlighting of some ninety languages through iris (`TextMateGrammars`), behind the `syntax` feature; `syntax-json`, `syntax-markdown`, `syntax-python`, `syntax-rust`, and `syntax-toml` pick tree-sitter grammars one by one, `syntax-textmate`, on by default, the TextMate grammars. |

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](https://github.com/taquangtrung/cherry/blob/main/LICENSE-APACHE))
- MIT license ([LICENSE-MIT](https://github.com/taquangtrung/cherry/blob/main/LICENSE-MIT))

at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in the work by you, as defined in the Apache-2.0 license, shall be dual licensed as above, without any additional terms or conditions.
