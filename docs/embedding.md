# Embedding Cherry Editor

This guide is for authors of applications that embed Cherry Editor: a terminal program, a GUI toolkit widget, or a game console. Cherry owns the text, the cursor, the modes, and the keys; the host owns the window, the event loop, the file system, and the colors. Everything the two exchange is plain data, so no toolkit is involved on Cherry's side.

The terminal example in `crates/cherry-engine/examples/term.rs` is a complete host in about 800 lines; read it alongside this guide. `examples/cherry-egui` is a GUI host on egui in about 600, a crate of its own (`cargo run -p cherry-egui -- <file>`), showing a window toolkit's keys, text input, clipboard, and pointer turned into Cherry's.

## Setup

Add `cherry` with its default features off (`cargo add cherry --no-default-features`), and with its `syntax` feature for highlighting. (An app built on misty keeps the default `misty` feature instead, and draws the editor with `cherry::misty::EditorView`.) Make one `Editor` with your `EditorOptions`, open each document from its text, and make a view per window that shows one:

```rust
use cherry::{Editor, EditorOptions};

let options = EditorOptions {
    line_numbers: true,
    sign_column: true,
    auto_pairs: true,
    ..EditorOptions::default()
};
let mut editor = Editor::new(options);
let document = editor.open_document(&std::fs::read_to_string("notes.txt")?);
let view = editor.new_view(document).expect("the document is open");
editor.resize_view(view, cols, rows);
```

Call `resize_view` whenever the window's size in cells changes; a change in the number of rows also resets what `C-d` and `C-u` scroll, which a count before either sets per view (as Vim's `scroll` option), back to half the view. Several views may show one document, each with its own cursor, mode, and scroll position. Drop a view with `close_view` when its window closes. `show_document(view, document)` puts another document in a view (a tab switch, `:e`), the cursor back where a view last left that document; `close_document` drops a document no view shows. `set_options` changes the options at runtime, as a settings page does.

## Keys

Convert each key event of your toolkit into a `cherry::keys::Key` (a `KeyCode` and `Mods`) and feed it with the time it was typed, as a `cherry::keys::Timestamp` read from your own monotonic clock:

```rust
use cherry::keys::Timestamp;

// Once, at startup: the origin of every timestamp you pass.
let started = std::time::Instant::now();

let now = Timestamp::from_duration(started.elapsed());
let handled = editor.handle_key(view, key, now);
```

Cherry never reads a clock itself, so it runs wherever you can tell time. In a browser, where `std::time::Instant::now` is not available, pass `Timestamp::from_millis` of `performance.now()`. Pass the same clock to `tick` and `ClickCounter::count`, and compare `deadline()` with it: `deadline.saturating_duration_since(now)` is how long to wait.

`Key::new` normalizes chords, so Shift with `g` and a reported `G` are one key. Drop key releases; pass repeats as presses. `handle_key` returns `Handled::Ignored` for a key that ran nothing, which you may pass on to the rest of your application.

Keys a document needs alone, as a commit message's `C-c C-c` or a picker's own Enter, bind with `editor.bind_in_document(document, Mode::Normal, &["C-c C-c"], "my-commit")`: views of that document read them first, over every document's keys, and they go when the document is closed.

A half-typed sequence waits for its next key. An ambiguous one (a key that is bound and also starts a longer binding) times out after `EditorOptions::key_timeout`: when no key comes, call `editor.tick(now)` at `editor.deadline()`, so the sequence runs what it has. An incomplete prefix that runs nothing on its own (the `g` of `g v`, or a host's `C-x`) waits indefinitely, as in Emacs, and `editor.deadline()` is `None` while only such prefixes are pending; set `EditorOptions::prefix_timeout` to `PrefixTimeout::All` (from `cherry::keys`) to time every pending sequence out, as Vim does. A blocking loop waits on the deadline as a timeout; an event-driven toolkit sets a timer.

## Editing styles

`EditorOptions::editing_style` picks one of four `EditingStyle`s. A style is more than a keymap: it decides the default keys of every mode, whether views ever leave Insert mode, what typing does to a selection, how undo steps are grouped, and what a mouse selection is.

- `EditingStyle::Cherry`, the default, is Cherry's own: Vim's modes where you navigate, Emacs keys where you type (in Insert mode and in prompts), and a few keys of its own where Vim has none (`M-%` query replace, `C-;` to change every occurrence, `S-M-<right>` to grow the selection). It is the style Cherry is designed around, and it may change between releases.
- `EditingStyle::Vim` is Vim's stock keys. It drops Cherry's extras, and Insert mode and the command line take Vim's keys instead of Emacs's: `C-o` runs one Normal mode command, `C-t` and `C-d` indent, `C-n` and `C-p` complete, `C-a` and `C-@` type the last insert, and `C-b` goes to the start of the command line. It keeps the operators most Vim users add with plugins, on keys stock Vim leaves free (`gc` to comment, `ys`, `cs`, and `ds` to surround).
- `EditingStyle::Emacs` is Emacs's stock keys, with no modes. `C-SPC` sets the mark and the motions after it select the region; `C-w` kills it and `M-w` copies it, to the kill ring and the clipboard; `C-g` drops it and `C-x C-x` swaps its ends. Typed text goes in at the cursor, as in Emacs without `delete-selection-mode`. `C-u` gives the next command a prefix argument (`C-u 3 x` types `xxx`, `C-u 2 C-k` kills two lines), as `M-3` does, Escape then a key is the key with Meta, `M-x` runs a command by its name (Cherry's command ids, such as `kill-line`, which Tab completes), `M-%` replaces text, asking at each match, `C-x C-s` saves, `C-x k` and `C-x C-c` close the view, and `C-s` and `C-r` search as the text is typed, again for the next match and back for the one before, as Emacs's isearch does.
- `EditingStyle::Standard` is for users who know neither: the keys of a desktop text editor, with no modes. Shift with an arrow, Home, or End selects; Ctrl with an arrow moves by words; typing replaces the selection and Escape drops it. Ctrl+A, Ctrl+C, Ctrl+X, Ctrl+V, Ctrl+Z, and Ctrl+Y (or Ctrl+Shift+Z) select all, copy, cut, paste, undo, and redo; Ctrl+S asks you to save (`HostRequest::Save`) and Ctrl+Q to close the view (`HostRequest::Quit`, refused with a notice while its changes are unsaved). With nothing selected, Ctrl+C and Ctrl+X take the whole line and Ctrl+V puts it back above the cursor's line. Ctrl+D adds a cursor at the next match of the word, Alt+Up and Alt+Down move lines, Ctrl+/ comments them, Tab and Shift+Tab indent and outdent the selected lines, Ctrl+F searches, and Ctrl+H replaces, asking at each match (`y` or Space to replace, `n` to skip, `a` or `!` for all the rest, `q` to stop).

The Vim and Emacs styles follow those editors as far as Cherry has the commands for it: a key either does what it does there or nothing. What is missing is missing on purpose rather than done differently: in the Vim style, digraphs (`C-k`); in the Emacs style, negative arguments (`M--`) and the commands Emacs names but Cherry lacks. Neither style runs Vimscript or Elisp, and neither reads the user's Vim or Emacs configuration; rebinding goes through `override_keys`.

In the two modeless styles, Emacs and standard, every view stays in Insert mode, as a single-line field edited with Emacs keys does in any style. A selection there is the range between an anchor and the cursor, so it sits between characters; a mouse drag, Shift click, or double or triple click selects without leaving Insert mode. Each command starts a new undo step, while a run of one command (Backspace held down) and the typing after it make one, so undo takes back what the user did a step at a time rather than the whole session. Prompts still open (search, `M-x`) and return to Insert mode when they close.

Each mode's default key table is built in layers: keys every style shares, with the style's own on top, where a later layer may add keys to a command an earlier one bound (the Emacs style adds `C-v` to page down). Keys you bind with `bind` sit over any style. Changing the style with `editor.set_options` swaps the default tables of every mode and drops the user's overrides from `override_keys`, while the keys you bound stay, so apply overrides after setting the style. `help_rows`, `keys_for`, and `key_hints` report the keys of the style in force.

## Input methods

An input method (Vietnamese Telex or VNI, Pinyin, Kana) composes text over several keys before it commits it. Show what it is composing with `editor.set_preedit(view, text)` on each preedit event, and end the composition with an empty text, as input methods do. Cherry lays the text into the line at the cursor, before the character under it, without touching the document: it pushes the rest of the line right, wraps with it, paints as `Style::Preedit` (underline it), and `render_view` puts the cursor after it, so anchor the input method's candidate window at that cursor. It shows only in Insert and Replace mode, where keys type text.

When the input method commits, feed the committed text as keys, a character at a time; any key ends the composition, so the preedit never shows twice:

```rust
match ime_event {
    Ime::Preedit(text) => editor.set_preedit(view, &text),
    Ime::Commit(text) => {
        for c in text.chars() {
            editor.handle_key(view, Key::new(KeyCode::Char(c), Mods::NONE), now);
        }
    }
    Ime::Disabled => editor.set_preedit(view, ""),
}
```

Typed this way, committed text works wherever a character does: in Insert mode, after `r` or `f`, and in prompts. A preedit does not show in a prompt yet, nor on a line long enough to be laid out only where it is in sight.

## Search

While `/` or `?` is open, the cursor follows the pattern as it is typed: it moves to the match the search would land on and the view scrolls to it, the pattern's matches paint as `Style::SearchMatch`, and Escape puts the cursor and the scroll back (Vim's `incsearch`). Turn it off with `EditorOptions::incremental_search` or `:set noincsearch`. Patterns are regular expressions; set `EditorOptions::literal_search` to look for the typed text as it is (`a.b` finds only `a.b`), with smart case still on, and `:s` and `:g` still taking regular expressions. For a status bar, `editor.search_count(view)` gives the match the cursor is on or last passed and how many there are (`3/17`), for the pattern being typed or else the last search while its matches show; it is `None` after `:noh`. It scans the document, so call it when the cursor or the pattern changes; it stops counting at 9999 and says so with `is_capped`.

The bracket at the cursor and the one matching it paint as `Style::MatchingBracket` when both are in sight (in Insert mode, the bracket just before the cursor counts when the cursor is not at one), as Vim's matchparen does; `EditorOptions::match_brackets` or `:set nomatchbrackets` turns it off.

## Mouse

Convert mouse events into `MouseEvent`s in cells counted from the view's top left corner, gutter included, and pass them to `editor.handle_mouse(view, event)`. Kinds are `Press`, `Drag`, `Release`, `DoubleClick`, `TripleClick`, `ScrollUp`, and `ScrollDown`; Shift on a press extends the selection. A toolkit that reports click counts maps them to the click kinds directly; a host whose toolkit reports each press alone (a terminal) passes every event through a `ClickCounter` made with its double-click interval (`ClickCounter::new(Duration::from_millis(400))`), which turns quick presses on one cell into `DoubleClick` and `TripleClick`.

`S-M-<right>` grows the selection to the next piece of text holding it (the word, the quotes, each pair of brackets, the line, the paragraph, the document), and `S-M-<left>` gives the growth back, in Normal and Visual mode of the Cherry style; in the others, bind `expand-selection` and `shrink-selection` where you like. For pieces from a language server, implement `SelectionRangeProvider::ranges(document, offset)` (the ranges holding the offset, innermost first) and set it with `editor.set_selection_range_provider(document, Some(Box::new(provider)))`.

## Rendering

After feeding keys, render each view onto a `Surface` you keep between frames, and draw it:

```rust
let cursor = cherry::render_view(&editor, view, &mut surface);
for row in 0..surface.rows() {
    for column in 0..surface.cols() {
        let cell = surface.cell(column, row).expect("inside the surface");
        if cell.symbol.is_empty() {
            continue; // the second cell of a wide character
        }
        // draw cell.symbol with your colors for cell.style, cell.highlight,
        // and cell.underline
    }
}
```

Each `Cell` has layers:

1. `style`, a `Style` naming what the cell shows: `Text`, `CursorLine`, `Selection`, `SearchMatch`, `Highlight`, `Background(id)`, `MatchingBracket`, `Inlay`, `Preedit`, `ExtraCursor`, `LineNumber`, `CursorLineNumber`, `Phrase(n)` (a phrase lit with `M-s h .`, numbered so each gets its own color; cycle your palette by `n`), or `Sign(kind)`. Map it to a background (and a foreground for the gutter and inlays, and an underline for `Preedit`).

2. `highlight`, an optional `HighlightId` from the document's syntax highlighter. Map it to a foreground color; with the `syntax` module, `highlight_name(id)` gives its name (`keyword`, `string.escape`). A highlighter of your own implements `Highlighter`. One that colors each line from the state the line before left, as TextMate grammars such as syntect's do, scans from the top to reach the lines in sight: do that work in `Highlighter::prepare`, `EditorOptions::highlight_budget` lines at a time, and return `Preparation::Pending` until the lines asked for are done. The editor then wants a tick at once (`deadline()` has passed), prepares more each `tick`, and `tick` returns `true` so you repaint, until the lines in sight are colored; an edit or a scroll starts it again. A highlighter that keeps a tree up to date in `edit`, as the `syntax` module does, leaves `prepare` as it is.

3. `underline`, an optional `DecorationKind` of the most severe decoration under the cell. Draw an underline or undercurl in its color.

4. `layers`, the `Layers` under the style, any of them with any style: `Layers::INDENT_GUIDE` on a blank cell at a level of its line's indentation (`EditorOptions::indent_guides`; draw a faint line), and `Layers::OCCURRENCE` on the text of a whole-word occurrence of the word at the cursor (`EditorOptions::light_occurrences`; draw a faint band). `Layers::PINNED` marks the rows `EditorOptions::sticky_scroll` pins at the top (the lines opening the blocks, by indentation, that the text in sight is inside); give them a band of their own. Draw them under the style's background, so a selection or a search match still shows.

`render_view` returns where to draw the cursor and its `CursorShape` (a bar in Insert mode, an underline in Replace mode, a block elsewhere), or `None` when it is out of sight. While a prompt is open (`/`, `?`, `:`), `render_prompt` paints it as one row for you to show below the view, with its own cursor.

`Style`, `Mode`, `CursorShape`, `HostRequest`, `DecorationKind`, and `MouseEventKind` are `#[non_exhaustive]`: give every `match` on them a fallback arm, so new variants do not break your build.

## Requests

The editor never touches the file system or the window. After feeding keys or a tick, take its requests and act on them in order:

```rust
for request in editor.take_requests() {
    match request {
        HostRequest::Notice(notice) => show_message(&notice.message),
        HostRequest::Save(save) => {
            let text = editor.document(save.document).expect("open").text_with_line_endings();
            if std::fs::write(&path, text).is_ok() {
                editor.mark_saved(save.document);
            }
        }
        HostRequest::Quit(quit) => close_window(quit.view, quit.force),
        HostRequest::ReadInto(read) => {
            let text = std::fs::read_to_string(read.path.as_deref().unwrap_or(&path))?;
            editor.insert_lines(read.view, read.line, &text);
        }
        _ => {}
    }
}
```

`HostRequest::Open` (`:e {file}`, `:e!`) asks you to open a file: open it with `open_document` and show it with `show_document`, refusing a modified document unless `force` is set. `text_with_line_endings` restores the line ending the text was opened with. Call `mark_saved` after every successful save: `is_modified` then reads false, a plain `:q` closes the last view of the document instead of refusing with "No write since last change", and `:x` and `Z Z` skip the save when nothing changed. When the file changes on disk, read it again and hand the text to `reload_document`: only the lines that differ are edited, as one undo step, so each view's cursor stays with its text, and the document counts as saved. Whether to reload a document that `is_modified` says has unsaved edits is the host's choice.

## Changes

Every document has a version, 0 when opened and raised by one by each change of its text, from any source: typing, operators, undo and redo, ex commands, `:g`, `insert_lines`, and your own `apply_edits`. Read it with `editor.document_version(document)`, for example to tell whether a cached parse is current.

For the changes themselves (a language server client syncing incrementally, a highlighter of your own), turn on tracking for the document and drain its queue after feeding keys:

```rust
editor.set_change_tracking(document, true);
// ... feed keys ...
for change in editor.take_changes(document) {
    for edit in &change.edits {
        // edit.start and edit.end are lines and UTF-16 columns, edit.range
        // the same in char offsets, edit.text what replaced it
        send_did_change(change.version, edit.start, edit.end, &edit.text);
    }
}
```

Each `DocumentChange` carries the version it made, its `ChangeSet` (compose several with `ChangeSet::compose` for one change since a version you remember), and its edits last in the text first: applied one after another in that order to the text of the previous version, each range is still valid, which is how an LSP `didChange` notification orders its content changes. `Document::utf16_position_to_char` and `char_to_utf16_position` convert positions the other way, for edits a server sends back. An untracked document queues nothing and computes no positions; a tracked one keeps its queue until you take it, and `set_change_tracking(document, false)` drops it.

## Single-line fields

A text field (a rename box, a search box, a form field) is a single-line document: open it with `editor.open_single_line_document(text)` and show it in a view as any other. Line breaks in `text` become spaces, and so does every line break an edit would insert, whether typed, put (a linewise put joins the line to the text), substituted, opened by `C-o`, or sent through `apply_edits`, so the document is always one line. Its views paint no gutter, keep no scroll margin, and scroll sideways instead of wrapping, whatever `EditorOptions` say; size the view one row high. `is_single_line(document)` tells such a document apart.

A field is edited with Emacs keys only, as text fields are everywhere else; Vim motions belong to the views around it. Its views start in Insert mode, show the bar cursor, and never leave Insert mode, so every Insert-mode key works (motions, kills and `C-y`, `C-r` and a register, `C-v`, completion) and every other key types itself, `v`, `:`, and `/` included. Nothing switches the mode: `run_command` with the id of a command that would enter another mode (`visual-charwise`, `delete-operator`, `enter-replace`, `search-forward`, `enter-command-line`) does nothing, while other commands run (`undo` undoes what was typed since the field was shown, or since the last undo); a mouse drag, Shift click, or double or triple click selects without leaving Insert mode; `show_document` puts a field in a view in Insert mode. `editor.set_field_editing(document, FieldEditing::Modal)` gives a field the Vim modes as in any document, and `FieldEditing::Emacs` takes them away again, ending a visual selection, a waiting operator, Replace mode, or an open prompt in every view of the field; `field_editing(document)` reads the style. A field never wraps with the editor's `wrap`; for one over several fixed rows (renaming in place, a message box), make its view that many rows high and give it `ViewOptions { wrap: Some(true), ..ViewOptions::default() }` with `set_view_options`, and its one line wraps over them.

A field can recall earlier answers, as a shell or a search box does. Give it the answers, oldest first, with `editor.set_field_history(document, answers)`: in Insert mode, with no completion menu open, `<up>` and `C-p` show the next older one, `<down>` and `C-n` the next newer, and past the newest the text typed before recalling comes back. Keeping the answers is yours, since you know which question a field asks: after a submit, add the answer and set them again (which also starts the next recall from the newest).

Two requests end the field's editing, and the view stays in its mode for you to decide what follows:

```rust
for request in editor.take_requests() {
    match request {
        HostRequest::Submit(submit) => {
            let text = editor.document(document).expect("open").to_string();
            rename_symbol(&text);
            close_field(submit.view);
        }
        HostRequest::Cancel(cancel) => close_field(cancel.view),
        _ => {}
    }
}
```

`HostRequest::Submit` comes from Enter (or `C-j`, `C-m`) in Insert mode, and with `FieldEditing::Modal` from Replace mode and from Enter in Normal mode (`+` only moves). `HostRequest::Cancel` comes from Escape or `C-g` at once; an open completion menu closes first, its typed text put back, or a selection is dropped first, and the next Escape cancels. With `FieldEditing::Modal`, it comes from Escape in Normal mode with nothing pending instead: the first Escape from Insert mode goes to Normal mode as usual, and an Escape after a count or `"x` only drops it, so the user presses Escape once more to cancel.

## Read-only documents

For a preview (a file in a picker, a diff, documentation), make the document read-only with `editor.set_read_only(document, true)`; `false` makes it editable again, and `is_read_only` reads it. Motions, search, yanks, visual selections, marks, and the jumplist all work. Every edit is refused with the notice "Cannot make changes: the document is read-only", once per command, and changes nothing, so it queues no change event, raises no version, and leaves the registers as they were: typing (Insert and Replace mode are not entered, and a view already in either goes back to Normal mode when you set the flag), operators other than yank, puts, undo and redo, ex commands such as `:s`, `:d`, and `:g`, and your own `apply_edits` and `insert_lines`. To change the text of a read-only document yourself, clear the flag, edit, and set it again. A view that never leaves Insert mode (a field edited with Emacs keys, or any view in the Emacs or standard style) stays in it: the cursor moves, and each key that would edit (typing, `C-k`, `C-d`, `C-y`, completion) gives the notice, kills nothing, and moves nothing.

## Commands and keys

Built-in and host commands are equals. Register a command under an id, which may be built at runtime, and bind keys to it in a mode:

```rust
editor.register_command("show-position", |cx: &mut cherry::CommandContext<'_>| {
    let line = cx.editor.view(cx.view).map(|view| view.cursor());
    cx.editor.info(format!("cursor at {line:?}"));
})?;
editor.bind(cherry::Mode::Normal, &["C-g"], "show-position")?;
```

A command runs with a `CommandContext`: the editor, the view, the count, and the register. It acts through `apply_edits` (one undo step, every view and mark carried along), `set_cursor`, `error` (why it did nothing), `info` (what it did), `warn`, and `request`. A host text object is a command that finds a range and calls `cx.act_on_text_object(found)`, bound in `OperatorPending`, `Visual`, and `VisualLine`. `override_keys` applies a user's rebinding of any command, built-in or not. Ex commands (`:name`) register with `register_ex_command` and run with an `ExContext` holding the range, the `!`, and the arguments. A command that needs your own state (a language server client, a version control tool), which a registered closure cannot borrow, registers with `register_host_command(id)` instead: binding and running it work the same, and running it queues a `HostRequest::Command` with the id, the view, the count, and the register, for you to act on when you take the requests.

A user's rebinding usually lives in a file. Build a `KeymapFile` (a list of `KeymapEntry`s, each a command id, a `Mode`, key specs, and whether they `replace` the command's keys) and apply it with `editor.apply_keymap(&keymap)`, which applies each entry and returns the ones it rejected, with their index and the `CommandError`, for you to report. With the `serde` feature it reads from TOML, JSON, or any format serde knows, modes written in kebab case (`normal`, `visual-line`); apply it after setting the editing style, since changing the style drops the user's overrides.

Menus and a command palette run any command by id with `run_command(view, id, count)`. `help_rows(mode)` lists the built-in commands of a mode with their group, title, and keys in force (overrides and host bindings included); `keys_for(mode, id)` gives the keys of any command, a host one too. For a which-key popup, `key_hints(view)` lists the keys that may follow what the user has typed so far, each with its command, title, and whether more keys may follow; show it while `deadline()` or `view.pending_keys()` says a sequence is half-typed.

## Multiple cursors

A view may have cursors besides its main one, as in VS Code or vim-visual-multi. Every key typed in the view then makes a round: it runs at the main cursor, then again at each extra cursor, each time from the view as it was before the key, so typing in Insert mode, operators with their motions and counts (`ciw`, `3dw`, `>>`), `.`, macros, and puts all work at every cursor with nothing written for them. After the round the view is as the main cursor left it; the registers and the kill ring hold what the main cursor put there (a put puts the main cursor's text at each), requests the extra cursors' keys made are dropped, and the round undoes as one step.

Undo and redo act once (the extra cursors follow the text they walk), and so do the keys typed in a prompt; in Visual and VisualLine mode each cursor has its own selection, painted as `Style::Selection` like the main one, so `viw` then `d` deletes a word at each; a block selection (`C-v`), a click, and Escape in Normal mode with nothing typed leave only the main cursor. Each extra cursor also keeps its own unnamed register, so a yank and a put at several cursors put back each one's own text. A command run by id with `run_command` runs at the main cursor alone.

The commands that add cursors have no default keys; bind them where you like:

```rust
editor.bind(Mode::Normal, &["M-n"], "add-cursor-next-match")?;
editor.bind(Mode::Visual, &["M-n"], "add-cursor-next-match")?;
editor.bind(Mode::Normal, &["M-j"], "add-cursor-below")?;
```

| Command | What it does |
| --- | --- |
| `add-cursor-next-match` | A cursor at the next occurrence of the word under the main cursor (whole words only), as far into it as the main cursor is, after the cursor added last; from a visual selection on one line, at the next occurrence of the selected text anywhere, ending the selection |
| `add-cursor-every-match` | A cursor at every occurrence |
| `skip-cursor-next-match` | Moves the cursor added last on to the next occurrence after it |
| `add-cursor-below`, `add-cursor-above` | A cursor in the main cursor's column on the line past the outermost cursor |
| `remove-cursor-last` | Drops the cursor added last |
| `keep-main-cursor` | Drops every extra cursor |

`set_cursors(view, offsets)` sets the extra cursors directly, and `view.cursors()` reads them, in the order they were added. `render_view` paints the cell each extra cursor is on as `Style::ExtraCursor` (the blank cell past a line's end when it is there); the main cursor still comes back as the `Cursor` to draw. A view takes at most a thousand extra cursors. Registers `+` and `*` write to your clipboard at each cursor, so the last cursor's text is what the clipboard holds.

## Clipboard

Registers `+` and `*` go through the `Clipboard` trait. Implement `read` and `write` for your system clipboard (and the primary selection, where there is one) and install it with `editor.set_clipboard(clipboard)`. Until then a `MemoryClipboard` keeps the text inside the editor. Set `EditorOptions::unnamed_clipboard` to make the unnamed register the system clipboard, as Vim's `clipboard=unnamedplus` does: a yank or delete with no register selected goes to your clipboard too, and `p` with none puts what your clipboard holds (text the editor put there comes back as it was taken, so a block stays a block). The numbered registers and `-` fill as before. The read-only registers `.` (the last inserted text), `:` (the last ex command), and `/` (the last search) need nothing from the host; `%` reads the name you give a document with `editor.set_document_name(document, Some("src/main.rs"))`. `C-r` and a register name types a register in Insert mode and in prompts, where `C-r C-w` types the word under the cursor. In the standard style, Ctrl+C, Ctrl+X, and Ctrl+V copy, cut, and paste through register `+`, and in the Emacs style `C-w` and `M-w` put the region there as well as on the kill ring, so install your clipboard when you offer either style.

## Comments and typing helpers

`gc{motion}`, `gcc`, and visual `gc` toggle line comments with the token of each document. Set it with `set_comment_token(document, Some("//"))`, or look it up from the file's extension with `cherry::comment_token_for_extension("rs")`. For a language without line comments (CSS, HTML, XML) set block tokens with `set_block_comment_tokens(document, Some(("/*", "*/")))`, or from `cherry::block_comment_tokens_for_extension("css")`; with no line token, `gc` wraps each non-blank line in them. Auto-indent and auto-pairing are `EditorOptions` fields, also switched with `:set autoindent` and `:set autopairs`.

## Completion

In Insert mode `C-x C-n` and `C-x C-p` complete the word before the cursor from the words of the document and then every other open document, `C-x C-l` completes the whole line, and `C-x C-o` asks the document's `CompletionProvider`s. Implement the trait's `complete(&CompletionRequest)` (the document, the cursor offset, the prefix, where it starts, and a ticket) to answer `CompletionAnswer::Items` with `CompletionItem`s (`CompletionItem::new(text)`, with `with_label`, `with_detail`, `with_documentation`, `with_kind`, and `with_source`), and `source()` to name the provider for items that name none. Install them with `editor.set_completion_providers(document, vec![Box::new(lsp), Box::new(snippets)])`; their matches show in that order, each text once. A provider that answers on another thread, as a language server does, answers `CompletionAnswer::Later`, keeps `request.ticket`, and when the answer comes you call `editor.answer_completion(ticket, items)` and repaint: the items join the open menu or open it, narrowed to what was typed since. An answer whose ticket is stale (another completion started, Insert mode left, the cursor off the line) is dropped. Set `EditorOptions::auto_complete` to open the menu as the user types, none of its matches typed: after two word characters, or at once on a provider's `trigger_characters()` (`.` for a language server's members); it offers the providers' matches and then the open documents' words. `C-n`, `C-p`, `<down>`, and `<up>` move its highlight without typing, `accept-completion` types the match highlighted, and Enter or any other key closes it, typing nothing.

Snippets use the syntax language servers send: `$1`, `${2:default}`, `$0`, with `\$`, `\}`, and `\\` for the chars themselves. Mark a completion item as one with `CompletionItem::with_snippet()`, or type one yourself in Insert mode with `editor.insert_snippet(view, "fn ${1:name}($2) {\n\t$0\n}")`. It types without the syntax, and `<tab>` and `S-<tab>` walk its stops in number order, `$0` last; the first key typed at a stop with a placeholder, or Backspace, replaces the placeholder. The snippet ends at its last stop, on leaving Insert mode, or on an undo; `<tab>` types a tab again after.

While a menu is open, `view.completion()` returns it: `items()`, the `selected()` index (`None` while none is chosen; `highlighted()` is the one to mark, the first then), the `typed()` text the matches were narrowed to (to mark in each label), and the `anchor()` offset where the completed text starts. Paint it as a popup at `editor.screen_cell(view, completion.anchor())`, one row below (the term example does). In a menu `C-x` opened, `C-n` and `<down>` type the next match and `C-p` and `<up>` the previous; with no menu open they move down and up a line, as the Emacs keys of Insert mode do, so completion only starts from `C-x`. `C-g` puts the typed text back; any other key keeps the match and closes the menu.

## Decorations

Lay diagnostics, highlights, and underlines over a document's text with `set_decorations(document, owner, decorations)`, and signs by its lines with `set_signs(document, owner, signs)`. The owner is a name you pick (`"lsp"`, `"spell"`); setting an owner's decorations replaces only that owner's, and `clear_decorations` drops them. Edits carry decorations along, dropping a range whose text was deleted; read the current ranges back with `decorations(document, owner)`, in the order you gave them, to tell which diagnostic the cursor is on. Turn on `EditorOptions::sign_column` to reserve the two gutter cells signs show in. Mark the lines changed since a version you compare with (the last commit) with `set_line_changes(document, owner, changes)`, each a `ChangedLine { line, change: LineChange::Added }` (or `Modified`, or `Deleted` for lines removed just above it): they share the sign column with signs, painting `Layers::CHANGE_ADDED`, `CHANGE_MODIFIED`, or `CHANGE_DELETED` on its first cell, so a line shows its change and its sign at once. Draw the layer as a bar down the cell's edge (`cherry::misty` does) and the sign's text beside it. For marks of your own (breakpoints, fold markers), give the document gutter lanes with `set_gutter_lanes(document, vec![GutterLane::new("git", 1)])`, left to right before the sign column, and put a sign in one with `Sign::new(line, "+", kind).in_lane("git")`; a lane keeps its width whether or not a line has a mark, so the text never shifts. Line numbers are absolute with `line_numbers`, relative to the cursor's line with `relative_line_numbers`, and hybrid with both; the cursor's line number paints as `Style::CursorLineNumber`.

For backgrounds of your own (a diff's added and removed lines, the bracket matching the one under the cursor, other uses of a name), use `DecorationKind::Background(id)` with a `HighlightId` you pick and color: it paints as `Style::Background(id)`, under selections, search matches, and `Highlight`, and where several overlap the narrowest range wins. A range that covers a line's break (`line_start..next_line_start`) also fills the rest of that line's rows, so a whole-line background reaches the right edge; the last line, which has no break, fills only its text. Text colors stay with the highlighter: bold or italic text is a `HighlightId` your theme draws bold.

## Inlays

Inlays are text you lay into a line that is not the document's own: a language server's inlay hints (a binding's type, a call's parameter names), a suggestion shown as ghost text, a placeholder in an empty field. Set them per owner, as decorations are:

```rust
use cherry::Inlay;

// `let x = 1;` shows as `let x: i32 = 1;`
let hint = Inlay::new(5, ": i32").with_highlight(type_hint_id);
editor.set_inlays(document, "lsp", vec![hint]);
```

An inlay takes cells before the character at its offset (after the last character for one at the document's end) and wraps with its line; every view paints, scrolls, and takes clicks with it, and a click inside it lands on the character after it. It paints as `Style::Inlay`, with its `HighlightId`, if you gave one, as the cell's `highlight`, so hints and ghost text can look different. No motion, edit, search, or yank sees it.

Edits carry inlays along: text typed at an inlay's offset goes after it, where the cursor shows, and deleting the text around one keeps it, so replace an owner's inlays when their source answers again (`set_inlays` with the new list, or `clear_decorations` to drop them with the owner's decorations and signs). `inlays(document, owner)` reads them back as edits moved them. Line breaks in an inlay's text show as spaces, and an inlay past the end of the text is dropped. A line long enough to be laid out only where it is in sight (see Long lines) shows no inlays.

## Conceals

A conceal hides text the host knows is markup rather than content: the `**` around bold Markdown, the backticks around code, a link's `(url)`. Set the hidden char ranges per owner:

```rust
// `**bold** text` shows as `bold text`
editor.set_conceals(document, "markdown", vec![0..2, 6..8]);
```

Concealed text stays in the document, so every motion, edit, search, and yank sees it; it only takes no cells. On the line a view's cursor is on, that view shows it again, so the cursor can reach every character, as Vim's `conceallevel` does with an empty `concealcursor`. `EditorOptions::conceal` chooses otherwise: `ConcealMode::Hide` hides it on the cursor's line too (a reading view), and `ConcealMode::Show` shows it everywhere. A click lands on the character shown in the cell. Edits carry conceals along as they carry decorations, dropping one whose text was all deleted; `conceals(document, owner)` reads them back, and `clear_decorations` drops them with the owner's other marks. To draw what the markup means (the bold), color the text between with your highlighter.

For text of several lines that is not the document's (a suggestion shown as ghost text, a merge conflict's band, an error explained under its line), set virtual lines: `editor.set_virtual_lines(document, "ai", vec![VirtualLine::new(4, "let x = 1;").with_highlight(ghost)])`. Each takes a screen row of its own after its line, painted as `Style::Inlay` with its highlight; scrolling counts it, `gj` and `gk` pass over it, no edit or search sees it, and edits carry it with its line.

## Syntax

With the `syntax` module, pick the language from the file name, make a highlighter for the document as it is, and give it to the editor:

```rust
use cherry::syntax::{Language, SyntaxHighlighter};

if let Some(language) = Language::from_extension("rs") {
    let text = editor.document(document).expect("open");
    let highlighter = SyntaxHighlighter::new(language, text)?;
    editor.set_highlighter(document, Some(Box::new(highlighter)));
}
cherry::syntax::register_text_objects(&mut editor)?;
```

The editor tells the highlighter of every change, undo and redo included, and it reparses incrementally. `register_text_objects` adds `af` and `if` (functions) and `ac` and `ic` (classes and types). Grammars are the features `json`, `markdown`, `python`, `rust`, and `toml`, all on by default; turn off default features to bundle only the ones you need. Any other highlighter, such as one fed by a language server's semantic tokens, implements the `cherry::Highlighter` trait the same way.

## Saved undo history

To keep undo across sessions, as Vim's `undofile` does, take a document's history with `editor.saved_history(document)` when the file closes or saves, and put it back with `editor.restore_history(document, saved)` after the file opens again; undo then walks back through the earlier session's steps, and the text counts as saved where it stands. With the `serde` feature, `SavedHistory` is `Serialize` and `Deserialize`, so write it in any format serde knows (JSON, bincode) next to the file or in your own store. Putting it back is refused with `HistoryError::TextChanged` when the text is not the one the history was saved with (the file changed elsewhere), and with `HistoryError::Damaged` when the history does not hold together, so a stale or broken file never corrupts the undo steps; open the file with a fresh history then.

## Folds

Each view folds lines on its own, as each Vim window does. A closed fold shows its first line alone, followed by a marker counting the lines it hides, painted as `Style::Folded`; `j`, `k`, scrolling, and the mouse pass over the hidden lines, and any other motion that lands inside a closed fold (a search, `G`, a mark) opens it. Edits carry folds along. In the Cherry and Vim styles, `zc` closes a fold, `zo` opens it, `za` toggles it, `zM` folds every block and `zR` opens them all, `zd` and `zE` delete folds, and `zf` in a visual mode folds the selected lines; in the standard style, Ctrl+{ folds and Ctrl+} unfolds. With nothing more from you, `zc` folds by indent: a line and the more deeply indented lines below it. To fold by structure instead, give a document its folding ranges, each a first and last line, from a language server or a tree-sitter grammar, with `editor.set_fold_ranges(document, vec![3..=10, 12..=20])`, and set them again after edits.

## Right-to-left text

Lines holding right-to-left text (Arabic, Hebrew) or mixing directions paint in visual order: each row's clusters are reordered by the Unicode Bidirectional Algorithm, the line's direction taken from its first strong character, so the cells `render_view` fills show `abc` and then the Hebrew letters right to left. The cursor still moves through the text in the order it is written and shows on the cluster it is at, and a mouse click lands on the cluster drawn in the cell. Three things are left to do: a right-to-left line still starts at the left edge instead of the right; Arabic letters arrive one per cell, so their joined forms need a host that shapes text runs; and Left and Right move in logical order, not visual.

## Accessibility

Cherry knows nothing of the platform's accessibility interface, so a screen reader reaches it through you. After each key, take `editor.accessible_view(view)`: the lines in sight with their text (a closed fold as its first line and how many lines it hides), the cursor and the selections as lines and chars, and the mode. Hand it to your accessibility layer (with AccessKit, a multi-line text field with these lines, this caret, and these selections), and pass the editor's notices on as announcements, since they say what a command did ("3 substitutions", "Pattern not found"). Cherry does not yet say what changed between two snapshots; compare them yourself to announce an edit.

## Long lines

With `EditorOptions::wrap` on, a line wider than its view continues on the rows below. `wrap_mode` picks where it breaks: `WrapMode::Words` (the default) at the last break the Unicode line breaking rules allow that fits, a word moving down whole with its blank left at the row's end, or `WrapMode::Chars` after the last character that fits. `break_indent` picks where the rows after the first start: `BreakIndent::None` at the left edge, `BreakIndent::Indent` under the line's indent, as Vim's `breakindent`, or `BreakIndent::Text` under the line's text, past what opens it (a list item's marker and task box, a quote's `>`s, a heading's `#`s, a line comment's leader, LaTeX's `\item`). Either indent is cut back so a row keeps twenty cells for text.

A line longer than a few thousand characters that is not soft wrapped is laid out only where it is in sight: rendering, scrolling, and moving on a one-megabyte line take microseconds near its start and a few milliseconds at its end. On such a line the cells before the part in sight are counted per character, where a character of zero width joins the one before it; that matches the grapheme clusters of nearly all text, though a rare cluster such as an emoji sequence joined by zero-width joiners may be counted a cell or two off. Soft wrapping a very long line still lays it out whole.

## Threads

`Editor` holds no global state and runs no threads. Commands, clipboards, and highlighters are `Send`, so the editor may live on whichever thread your event loop runs on. Long work (formatting, language servers) belongs to the host; feed its results back through `apply_edits`, decorations, and notices.
