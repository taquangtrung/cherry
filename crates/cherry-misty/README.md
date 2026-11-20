# cherry-misty

Cherry Editor on the [misty](https://github.com/taquangtrung/misty) GUI framework, for an app built on both. The editor itself is the toolkit-free [`cherry-engine`](https://github.com/taquangtrung/cherry) crate, and misty knows nothing of it: misty owns its key values and resolves no key presses, and the engine paints rows of cells for any toolkit. This crate is the one place they meet:

- `keys`: converts misty's keys to Cherry's and back, without loss.
- `clock`: puts Cherry's timestamps on one clock with misty's `Instant`s.
- `keymap`: binds a misty `CommandRegistry` into a `cherry_engine::keys::Keymap`, and turns a pending prefix's continuations into which-key rows.
- `EditorView`: one view of a Cherry `Editor` as a misty widget, with mouse handling, an input method, decorations, signs, screen reader access, and Insert-mode completion. `EditorClipboard` hands the editor misty's system clipboard.

Most apps reach this crate through the [`cherry`](https://github.com/taquangtrung/cherry) crate, whose default `misty` feature re-exports it as `cherry::misty`.

An app that only resolves misty keys with Cherry's keymap uses the first three; an app that edits text uses `EditorView` too. The engine is re-exported as `cherry_misty::engine` and misty as `cherry_misty::misty`, so an app on misty needs no other dependency for either, and the engine's features are this crate's too.

## Features

| Feature | What it adds |
| --- | --- |
| `serde` | Serialize and Deserialize for the engine's saved undo histories and keymap files. |
| `syntax` | Syntax highlighting in `EditorView` through the engine's `syntax` module, colored by the theme's syntax colors, with all five bundled grammars; builds the `editor` example. |
| `syntax-json`, `syntax-markdown`, `syntax-python`, `syntax-rust`, `syntax-toml` | One bundled grammar each, with the highlighting. |
| `syntax-engine` | The highlighting without a bundled grammar, for an app that brings its own. |

## Examples

The `chrome`, `palette`, and `settings` examples resolve their keys with Cherry's keymap; `editor` is a small text editor. From the repository root:

```bash
cargo run -p cherry-misty --example palette
cargo run -p cherry-misty --features syntax --example editor -- notes.txt
```

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](https://github.com/taquangtrung/cherry/blob/main/LICENSE-APACHE))
- MIT license ([LICENSE-MIT](https://github.com/taquangtrung/cherry/blob/main/LICENSE-MIT))

at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in the work by you, as defined in the Apache-2.0 license, shall be dual licensed as above, without any additional terms or conditions.
