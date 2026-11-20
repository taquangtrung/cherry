# cherry

Cherry Editor is an embeddable text editor for Rust applications: the Vim grammar where you navigate and Emacs editing where you type, or strict Vim, modeless Emacs, or a desktop editor's keys. This is the crate an app adds. It gathers Cherry's parts under one name:

- The editor itself, toolkit-free, at the top: `cherry::Editor`, `cherry::keys`, `cherry::markup`, and the rest, from [`cherry-engine`](https://github.com/taquangtrung/cherry/tree/main/crates/cherry-engine).
- `cherry::misty`, with the default `misty` feature: the editor as a widget of the [misty](https://github.com/taquangtrung/misty) GUI framework, from [`cherry-misty`](https://github.com/taquangtrung/cherry/tree/main/crates/cherry-misty).

> Status: early. The API may change between versions.

## Usage

```bash
# Cherry on the misty GUI framework, with TextMate highlighting, the default:
cargo add cherry
# Cherry alone, for an app on any other toolkit or in a terminal:
cargo add cherry --no-default-features
```

On misty, the app owns one `Editor` and draws each of its views with a `cherry::misty::EditorView`:

```rust
use cherry::misty::EditorView;
use cherry::{Editor, EditorOptions};

let mut editor = Editor::new(EditorOptions::default());
let document = editor.open_document("hello world\n");
let view = editor.new_view(document).expect("the document is open");
let mut editor_view = EditorView::new(view);
// Each frame: `editor_view.lend(&mut editor)` is the widget to hand
// inputs and draw; act on the `HostRequest`s it hands back.
```

Any other host feeds the editor keys and paints the rows it produces itself; the [repository's README](https://github.com/taquangtrung/cherry) shows how, and the [embedding guide](https://github.com/taquangtrung/cherry/blob/main/docs/embedding.md) walks through every part a host touches.

## Features

| Feature | What it adds |
| --- | --- |
| `misty` (default) | `cherry::misty`: the editor as a misty widget, misty's keys resolved by Cherry's keymap. |
| `serde` | Serialize and Deserialize for saved undo histories and keymap files. |
| `syntax-textmate` (default) | TextMate highlighting of every language iris knows (`syntax::TextMateGrammars`), drawn in the theme's syntax colors on misty. Pure Rust. |
| `syntax` | The `syntax` module: tree-sitter highlighting and text objects with all five bundled grammars, drawn in the theme's syntax colors on misty. |
| `syntax-json`, `syntax-markdown`, `syntax-python`, `syntax-rust`, `syntax-toml` | One bundled grammar each, with the `syntax` module. |
| `syntax-engine` | The `syntax` module without a bundled grammar, for an app that brings its own. |

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](https://github.com/taquangtrung/cherry/blob/main/LICENSE-APACHE))
- MIT license ([LICENSE-MIT](https://github.com/taquangtrung/cherry/blob/main/LICENSE-MIT))

at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in the work by you, as defined in the Apache-2.0 license, shall be dual licensed as above, without any additional terms or conditions.
