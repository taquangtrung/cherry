# cherry-engine

The engine of Cherry Editor, an embeddable text editor: the Vim grammar where you navigate and Emacs editing where you type, or strict Vim, modeless Emacs, or a desktop editor's keys. It owns the text, the cursor, the modes, and the keys, and depends on no window toolkit; the app that embeds it owns the window and paints the rows it produces.

Most apps add the [`cherry`](https://github.com/taquangtrung/cherry) crate instead, which re-exports this one at its top level (`cherry::Editor` is `cherry_engine::Editor`), and with its default `misty` feature adds the editor as a widget of the [misty](https://github.com/taquangtrung/misty) GUI framework. Depend on `cherry-engine` directly to pin the engine alone.

> Status: early. The API may change between versions.

## Usage

```bash
cargo add cherry-engine
# with tree-sitter highlighting and text objects:
cargo add cherry-engine --features syntax
```

Make an editor, open text, feed it keys, paint a view, and act on what it asks:

```rust
use cherry_engine::keys::{parse_sequence, Timestamp};
use cherry_engine::{render_view, Editor, EditorOptions, HostRequest, Surface};

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

The crate's [`term` example](https://github.com/taquangtrung/cherry/blob/main/crates/cherry-engine/examples/term.rs) is a complete terminal host (`cargo run -p cherry-engine --example term -- <file>`), and the [embedding guide](https://github.com/taquangtrung/cherry/blob/main/docs/embedding.md) walks through every part a host touches.

## Features

| Feature | What it adds |
| --- | --- |
| `serde` | Serialize and Deserialize for saved undo histories (`SavedHistory`) and keymap files (`KeymapFile`). |
| `syntax` | The `syntax` module: tree-sitter highlighting and structural text objects, with all five bundled grammars. |
| `syntax-json`, `syntax-markdown`, `syntax-python`, `syntax-rust`, `syntax-toml` | One bundled grammar each, with the `syntax` module. |
| `syntax-engine` | The `syntax` module without a bundled grammar, for one that names its own. |

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](https://github.com/taquangtrung/cherry/blob/main/LICENSE-APACHE))
- MIT license ([LICENSE-MIT](https://github.com/taquangtrung/cherry/blob/main/LICENSE-MIT))

at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in the work by you, as defined in the Apache-2.0 license, shall be dual licensed as above, without any additional terms or conditions.
