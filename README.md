# Cherry

An embeddable modal text editor for Rust applications: the Vim grammar where you navigate, Emacs editing where you type. Cherry owns the text, the cursor, the modes, and the keys; the application that embeds it owns the window and paints the rows Cherry produces, in whatever toolkit it uses.

> Status: early. The API may change between versions.

## Usage

```bash
cargo add cherry
# with tree-sitter highlighting and text objects:
cargo add cherry --features syntax
```

Make an editor, open text, feed it keys, paint a view, and act on what it asks:

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

[`examples/term.rs`](https://github.com/taquangtrung/cherry/blob/main/examples/term.rs) is a complete terminal host, and the [embedding guide](https://github.com/taquangtrung/cherry/blob/main/docs/embedding.md) walks through every part a host touches.

## What it has

- The Vim grammar: counts, operators, motions, text objects, registers, marks, jumps, dot repeat, macros, visual modes, surround, and an ex command line with `:s`.
- Emacs editing in Insert mode and in prompts: kills and the kill ring, transposes, balanced expressions.
- Search with highlighting and a match count, as the pattern is typed.
- Completion from open documents and from the host (a language server, snippets), with tab-stop snippets.
- Multiple cursors, input methods, and a toolkit-free mouse API.
- What a host lays on the text: decorations, gutter signs, inlays, virtual lines, and conceals, carried through edits.
- Document versions and change queues for language server sync, single-line documents for fields, and read-only documents for previews.
- One command registry for host commands, key bindings, and user rebinding.

## Modules

| Module | Holds |
| --- | --- |
| `cherry` | The editor: documents, views, modes, commands, search, and the paint model a host draws. |
| `cherry::keys` | Keys, multi-key sequences, and key tables, independent of any window toolkit. |
| `cherry::markup` | Markdown conceals, per-format outlines, and Markdown writing commands. |
| `cherry::syntax` | Tree-sitter highlighting and text objects for Rust, Python, JSON, TOML, and Markdown, behind the `syntax` feature; `syntax-json`, `syntax-markdown`, `syntax-python`, `syntax-rust`, and `syntax-toml` pick grammars one by one. |

## License

MIT. See [LICENSE](https://github.com/taquangtrung/cherry/blob/main/LICENSE).
