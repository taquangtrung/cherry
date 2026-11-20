# cherry-egui

A GUI host for Cherry Editor on [egui](https://github.com/emilk/egui), as an example; it is never published. It shows what a host on a toolkit other than misty does: it depends on `cherry` with default features off, so no misty is built, and turns egui's key, text, clipboard, and pointer events into Cherry keys and mouse events, ticks the editor when a half-typed key sequence times out, paints the surface the editor renders cell by cell in a monospace font, and acts on the editor's requests (save, quit, show a notice).

## Running

From the repository root:

```bash
cargo run -p cherry-egui -- notes.txt
# or
make gui FILE=notes.txt
```

It opens the file, or an empty buffer when the file does not exist yet. `CHERRY_STYLE` in the environment picks the editing style: `vim`, `emacs`, or `standard`; Cherry's own otherwise.
