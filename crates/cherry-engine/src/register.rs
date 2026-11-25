//! Registers: the named places yanked and deleted text goes, for putting
//! it back, as in Vim.
//!
//! A register is named by a char, selected by typing `"` and the name
//! before a command (`"ayy`, `"_dd`):
//!
//! - `"`, the unnamed register: every yank and delete writes it (except to
//!   the black hole), and a put with no register reads it.
//! - `a` to `z`: written only when selected; the uppercase name appends to
//!   the lowercase register instead of replacing it.
//! - `0`: the last yank made with no register selected.
//! - `1` to `9`: deletes with no register selected that took whole lines
//!   or spanned a line break, the newest in `1`, older ones shifted down.
//! - `-`: the last delete with no register selected that stayed inside a
//!   line.
//! - `_`, the black hole: writing it keeps nothing and touches no other
//!   register; reading it gives nothing.
//! - `+` and `*`: the system clipboard and the primary selection, read and
//!   written through the host's [`Clipboard`]. An editor starts with a
//!   [`MemoryClipboard`], which keeps the text inside the editor. With
//!   [`EditorOptions::unnamed_clipboard`](crate::EditorOptions::unnamed_clipboard),
//!   the unnamed register is the system clipboard, as Vim's
//!   `clipboard=unnamedplus` makes it: every yank and delete with no
//!   register selected goes there too, and a put with none reads it.
//! - `.`, `:`, `/`, and `%`, read only: the text typed in the last insert
//!   session, the last ex command line run, the last search pattern, and
//!   the document's name when the host set one
//!   ([`Editor::set_document_name`](crate::Editor::set_document_name)).
//!   Writing one keeps nothing and touches no other register, as the
//!   black hole does.
//!
//! Text is kept with its [`RegisterKind`]: charwise text is put inside a
//! line, linewise text (always ending in a line break) as whole lines, and
//! blockwise text (a visual block's lines, joined by line breaks) as a
//! block, one line of it per line of the document.

use std::collections::HashMap;
use std::fmt;

// ========================================================================
// Constants
// ========================================================================

/// The black hole register.
const BLACK_HOLE: char = '_';

/// The read-only register of the document's name.
const DOCUMENT_NAME: char = '%';

/// The read-only register of the last ex command line.
const LAST_COMMAND: char = ':';

/// The read-only register of the text typed in the last insert session.
pub(crate) const LAST_INSERTED: char = '.';

/// The read-only register of the last search pattern.
const LAST_SEARCH: char = '/';

/// The register of the system clipboard.
pub(crate) const CLIPBOARD: char = '+';

/// The numbered delete registers, newest first.
const DELETE_REGISTERS: [char; 9] = ['1', '2', '3', '4', '5', '6', '7', '8', '9'];

/// The line break linewise text ends with.
const LINE_BREAK: char = '\n';

/// The register of the primary selection.
const PRIMARY: char = '*';

/// The small delete register.
const SMALL_DELETE: char = '-';

/// The unnamed register.
pub(crate) const UNNAMED: char = '"';

/// The register of the last yank made with no register selected.
const YANK: char = '0';

// ========================================================================
// Data Structures
// ========================================================================

/// Which of the two host selections a clipboard read or write is for.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ClipboardSelection {
    /// The system clipboard, register `+`.
    Clipboard,
    /// The primary selection (selected text, pasted with the middle
    /// button on X11 and Wayland), register `*`.
    Primary,
}

/// An in-memory clipboard: what is written is read back, and nothing
/// leaves the process. The clipboard an editor starts with, and a fake for
/// tests.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct MemoryClipboard {
    /// The text on the system clipboard.
    clipboard: Option<String>,
    /// The text on the primary selection.
    primary: Option<String>,
}

/// A register the editor fills from its own state, which writes never
/// change.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum ReadOnlyRegister {
    /// `%`: the name the host gave the document.
    DocumentName,
    /// `:`: the last ex command line run.
    LastCommand,
    /// `.`: the text typed in the last insert session.
    LastInserted,
    /// `/`: the last search pattern.
    LastSearch,
}

/// How text in a register is put back.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum RegisterKind {
    /// As a block: each of its lines at the cursor's column, on the
    /// cursor's line and those below.
    Blockwise,
    /// Inside a line, at the cursor.
    Charwise,
    /// As whole lines, above or below the cursor's line.
    Linewise,
}

/// Text in a register, with how it is put back.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RegisterText {
    /// How it is put back.
    pub kind: RegisterKind,
    /// The text; linewise text ends with a line break.
    pub text: String,
}

/// Every register, and the clipboard behind `+` and `*`.
pub(crate) struct Registers {
    /// Reads and writes `+` and `*`.
    clipboard: Box<dyn Clipboard + Send>,
    /// What was last put on each clipboard selection, so text read back
    /// unchanged is put back as it was taken (a block stays a block).
    put_on_clipboard: HashMap<ClipboardSelection, RegisterText>,
    /// The text of every register but `+`, `*`, and `_`, by lowercase name.
    slots: HashMap<char, RegisterText>,
    /// Whether the unnamed register is the system clipboard.
    unnamed_clipboard: bool,
}

// ========================================================================
// Traits
// ========================================================================

/// The host's clipboard, behind registers `+` and `*`. Cherry never
/// touches the system clipboard itself; a host implements this over its
/// toolkit's clipboard, or keeps the [`MemoryClipboard`] an editor starts
/// with.
pub trait Clipboard {
    /// The text on `selection`, or `None` when it is empty or cannot be
    /// read.
    fn read(&mut self, selection: ClipboardSelection) -> Option<String>;

    /// Puts `text` on `selection`.
    fn write(&mut self, selection: ClipboardSelection, text: &str);
}

// ========================================================================
// MemoryClipboard Implementations
// ========================================================================

impl MemoryClipboard {
    /// The slot of `selection`.
    fn slot(&mut self, selection: ClipboardSelection) -> &mut Option<String> {
        match selection {
            ClipboardSelection::Clipboard => &mut self.clipboard,
            ClipboardSelection::Primary => &mut self.primary,
        }
    }
}

impl Clipboard for MemoryClipboard {
    fn read(&mut self, selection: ClipboardSelection) -> Option<String> {
        self.slot(selection).clone()
    }

    fn write(&mut self, selection: ClipboardSelection, text: &str) {
        *self.slot(selection) = Some(text.to_string());
    }
}

// ========================================================================
// ReadOnlyRegister Implementations
// ========================================================================

impl ReadOnlyRegister {
    /// The read-only register `name` names, if it names one.
    pub(crate) fn named(name: char) -> Option<Self> {
        match name {
            DOCUMENT_NAME => Some(Self::DocumentName),
            LAST_COMMAND => Some(Self::LastCommand),
            LAST_INSERTED => Some(Self::LastInserted),
            LAST_SEARCH => Some(Self::LastSearch),
            _ => None,
        }
    }
}

// ========================================================================
// RegisterText Implementations
// ========================================================================

impl RegisterText {
    /// `text` put back as `kind`; linewise text is given its final line
    /// break when it lacks one.
    pub(crate) fn new(kind: RegisterKind, text: impl Into<String>) -> Self {
        let mut text = text.into();
        if kind == RegisterKind::Linewise && !text.ends_with(LINE_BREAK) {
            text.push(LINE_BREAK);
        }
        Self { kind, text }
    }

    /// This text with `more` after it: charwise or blockwise when both
    /// are (blocks stacked), else linewise.
    fn appended(self, more: Self) -> Self {
        match (self.kind, more.kind) {
            (RegisterKind::Charwise, RegisterKind::Charwise) => {
                Self::new(RegisterKind::Charwise, self.text + &more.text)
            }
            (RegisterKind::Blockwise, RegisterKind::Blockwise) => Self::new(
                RegisterKind::Blockwise,
                format!("{}{LINE_BREAK}{}", self.text, more.text),
            ),
            (RegisterKind::Blockwise | RegisterKind::Charwise | RegisterKind::Linewise, _) => {
                let first = Self::new(RegisterKind::Linewise, self.text);
                Self::new(RegisterKind::Linewise, first.text + &more.text)
            }
        }
    }
}

// ========================================================================
// Registers Implementations
// ========================================================================

impl Registers {
    /// Empty registers over `clipboard`.
    pub(crate) fn new(clipboard: Box<dyn Clipboard + Send>) -> Self {
        Self {
            clipboard,
            put_on_clipboard: HashMap::new(),
            slots: HashMap::new(),
            unnamed_clipboard: false,
        }
    }

    /// Makes the unnamed register the system clipboard, or with `false`
    /// the editor's own register again.
    pub(crate) fn set_unnamed_clipboard(&mut self, unnamed_clipboard: bool) {
        self.unnamed_clipboard = unnamed_clipboard;
    }

    /// The text of every register but `+`, `*`, and `_`, to put back with
    /// [`Registers::restore`].
    pub(crate) fn stored(&self) -> HashMap<char, RegisterText> {
        self.slots.clone()
    }

    /// Puts back the text of every register but `+`, `*`, and `_` as
    /// [`Registers::stored`] read it.
    pub(crate) fn restore(&mut self, slots: HashMap<char, RegisterText>) {
        self.slots = slots;
    }

    /// Whether `name` names a register, read-only ones included.
    pub(crate) fn is_name(name: char) -> bool {
        name.is_ascii_alphanumeric()
            || [BLACK_HOLE, CLIPBOARD, PRIMARY, SMALL_DELETE, UNNAMED].contains(&name)
            || ReadOnlyRegister::named(name).is_some()
    }

    /// The text register `name` holds, the unnamed register when `None`;
    /// `None` when it holds nothing. Clipboard text is put back as it was
    /// taken when it is what the editor put there, and else is linewise
    /// when it ends in a line break. An unnamed register that is the
    /// clipboard falls back to the editor's own when the clipboard gives
    /// nothing.
    pub(crate) fn read(&mut self, name: Option<char>) -> Option<RegisterText> {
        let name = name.unwrap_or(UNNAMED);
        let selection = match name {
            UNNAMED if self.unnamed_clipboard => Some(ClipboardSelection::Clipboard),
            _ => clipboard_selection(name),
        };
        if let Some(selection) = selection {
            let text = self.read_clipboard(selection);
            if text.is_some() || name != UNNAMED {
                return text;
            }
        }
        self.slots.get(&name.to_ascii_lowercase()).cloned()
    }

    /// Records `text` deleted into register `name`: with none selected, a
    /// delete of lines or across a line break goes to `1` (older deletes
    /// shifted toward `9`), a smaller one to `-`.
    pub(crate) fn record_delete(&mut self, name: Option<char>, text: RegisterText) {
        if name.is_some() {
            return self.write(name, text);
        }
        let is_large = text.kind == RegisterKind::Linewise || text.text.contains(LINE_BREAK);
        if is_large {
            for pair in DELETE_REGISTERS.windows(2).rev() {
                if let Some(older) = self.slots.remove(&pair[0]) {
                    self.slots.insert(pair[1], older);
                }
            }
            self.slots.insert(DELETE_REGISTERS[0], text.clone());
        } else {
            self.slots.insert(SMALL_DELETE, text.clone());
        }
        self.set_unnamed(text);
    }

    /// Records `text` yanked into register `name`: with none selected, it
    /// also goes to `0`.
    pub(crate) fn record_yank(&mut self, name: Option<char>, text: RegisterText) {
        if name.is_none() {
            self.slots.insert(YANK, text.clone());
        }
        self.write(name, text);
    }

    /// Records the key notation `keys` of a macro into register `name`
    /// alone, appended (after a space) for an uppercase name; the unnamed
    /// register is left as it was.
    pub(crate) fn record_macro(&mut self, name: char, keys: String) {
        let lower = name.to_ascii_lowercase();
        let text = match (name.is_ascii_uppercase(), self.slots.remove(&lower)) {
            (true, Some(old)) if !old.text.is_empty() => format!("{} {keys}", old.text),
            (true, Some(_) | None) | (false, _) => keys,
        };
        self.slots
            .insert(lower, RegisterText::new(RegisterKind::Charwise, text));
    }

    /// Replaces the clipboard behind `+` and `*`.
    pub(crate) fn set_clipboard(&mut self, clipboard: Box<dyn Clipboard + Send>) {
        self.clipboard = clipboard;
    }

    /// Writes `text` to register `name` and the unnamed register; nothing
    /// for the black hole or a read-only register, an append for an
    /// uppercase name.
    fn write(&mut self, name: Option<char>, text: RegisterText) {
        let Some(name) = name.filter(|&name| name != UNNAMED) else {
            return self.set_unnamed(text);
        };
        if name == BLACK_HOLE || ReadOnlyRegister::named(name).is_some() {
            return;
        }
        if let Some(selection) = clipboard_selection(name) {
            self.write_clipboard(selection, &text);
        } else if name.is_ascii_uppercase() {
            let lower = name.to_ascii_lowercase();
            let text = match self.slots.remove(&lower) {
                Some(old) => old.appended(text),
                None => text,
            };
            self.slots.insert(lower, text.clone());
            self.slots.insert(UNNAMED, text);
            return;
        } else {
            self.slots.insert(name, text.clone());
        }
        self.slots.insert(UNNAMED, text);
    }

    /// Writes `text` to the unnamed register, and to the system clipboard
    /// when the unnamed register is it.
    fn set_unnamed(&mut self, text: RegisterText) {
        if self.unnamed_clipboard {
            self.write_clipboard(ClipboardSelection::Clipboard, &text);
        }
        self.slots.insert(UNNAMED, text);
    }

    /// Puts `text` on `selection`, remembering how it is put back.
    fn write_clipboard(&mut self, selection: ClipboardSelection, text: &RegisterText) {
        self.clipboard.write(selection, &text.text);
        self.put_on_clipboard.insert(selection, text.clone());
    }

    /// The text on `selection`: as it was taken when it is what the editor
    /// put there, else linewise when it ends in a line break.
    fn read_clipboard(&mut self, selection: ClipboardSelection) -> Option<RegisterText> {
        let text = self.clipboard.read(selection)?;
        if let Some(put) = self
            .put_on_clipboard
            .get(&selection)
            .filter(|put| put.text == text)
        {
            return Some(put.clone());
        }
        let kind = match text.ends_with(LINE_BREAK) {
            true => RegisterKind::Linewise,
            false => RegisterKind::Charwise,
        };
        Some(RegisterText::new(kind, text))
    }
}

impl fmt::Debug for Registers {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Registers")
            .field("slots", &self.slots)
            .finish_non_exhaustive()
    }
}

// ========================================================================
// Functions
// ========================================================================

/// The clipboard selection register `name` stands for, if it is `+` or `*`.
fn clipboard_selection(name: char) -> Option<ClipboardSelection> {
    match name {
        CLIPBOARD => Some(ClipboardSelection::Clipboard),
        PRIMARY => Some(ClipboardSelection::Primary),
        _ => None,
    }
}

// ========================================================================
// Tests
// ========================================================================

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::testing::Harness;

    fn registers() -> Registers {
        Registers::new(Box::new(MemoryClipboard::default()))
    }

    fn chars(text: &str) -> RegisterText {
        RegisterText::new(RegisterKind::Charwise, text)
    }

    fn lines(text: &str) -> RegisterText {
        RegisterText::new(RegisterKind::Linewise, text)
    }

    fn text_of(registers: &mut Registers, name: char) -> Option<String> {
        registers.read(Some(name)).map(|text| text.text)
    }

    #[test]
    fn test_a_plain_yank_fills_zero_and_unnamed_but_a_named_yank_spares_zero() {
        let mut registers = registers();
        registers.record_yank(None, chars("one"));
        registers.record_yank(Some('a'), chars("two"));
        assert_eq!(text_of(&mut registers, '0').as_deref(), Some("one"));
        assert_eq!(text_of(&mut registers, 'a').as_deref(), Some("two"));
        assert_eq!(registers.read(None), Some(chars("two")));
    }

    #[test]
    fn test_large_deletes_shift_through_the_numbered_registers_and_small_ones_go_to_minus() {
        let mut registers = registers();
        for n in 1..=10 {
            registers.record_delete(None, lines(&format!("line {n}")));
        }
        registers.record_delete(None, chars("word"));
        assert_eq!(text_of(&mut registers, '1').as_deref(), Some("line 10\n"));
        assert_eq!(text_of(&mut registers, '9').as_deref(), Some("line 2\n"));
        assert_eq!(text_of(&mut registers, '-').as_deref(), Some("word"));
        assert_eq!(registers.read(None), Some(chars("word")));
        registers.record_delete(None, chars("a\nb"));
        assert_eq!(text_of(&mut registers, '1').as_deref(), Some("a\nb"));
        assert_eq!(text_of(&mut registers, '-').as_deref(), Some("word"));
    }

    #[test]
    fn test_uppercase_appends_and_turns_linewise_when_either_part_is() {
        let mut registers = registers();
        registers.record_yank(Some('a'), chars("foo"));
        registers.record_yank(Some('A'), chars("bar"));
        assert_eq!(registers.read(Some('a')), Some(chars("foobar")));
        registers.record_yank(Some('A'), lines("line"));
        assert_eq!(registers.read(Some('a')), Some(lines("foobar\nline\n")));
        registers.record_yank(Some('B'), chars("new"));
        assert_eq!(registers.read(Some('b')), Some(chars("new")));
    }

    #[test]
    fn test_the_black_hole_keeps_nothing_and_touches_no_other_register() {
        let mut registers = registers();
        registers.record_yank(None, chars("kept"));
        registers.record_delete(Some('_'), lines("gone"));
        registers.record_yank(Some('_'), chars("gone"));
        assert_eq!(registers.read(None), Some(chars("kept")));
        assert_eq!(registers.read(Some('_')), None);
        assert_eq!(registers.read(Some('1')), None);
    }

    #[test]
    fn test_plus_and_star_go_through_the_clipboard_with_linewise_text_ending_in_a_break() {
        let mut registers = registers();
        registers.record_yank(Some('+'), lines("whole"));
        registers.record_yank(Some('*'), chars("part"));
        assert_eq!(registers.read(Some('+')), Some(lines("whole")));
        assert_eq!(registers.read(Some('*')), Some(chars("part")));
        assert_eq!(registers.read(None), Some(chars("part")));
        assert_eq!(registers.read(Some('0')), None, "a named yank spares 0");
    }

    /// A host clipboard a test can change behind the editor's back.
    #[derive(Clone, Default)]
    struct Shared(Arc<Mutex<Option<String>>>);

    impl Clipboard for Shared {
        fn read(&mut self, _: ClipboardSelection) -> Option<String> {
            self.0.lock().unwrap().clone()
        }
        fn write(&mut self, _: ClipboardSelection, text: &str) {
            *self.0.lock().unwrap() = Some(text.to_string());
        }
    }

    fn on_clipboard(clipboard: Shared) -> Registers {
        let mut registers = Registers::new(Box::new(clipboard));
        registers.set_unnamed_clipboard(true);
        registers
    }

    #[test]
    fn test_an_unnamed_clipboard_takes_plain_yanks_and_deletes_but_not_named_ones() {
        let host = Shared::default();
        let mut registers = on_clipboard(host.clone());
        registers.record_yank(None, chars("yanked"));
        assert_eq!(host.0.lock().unwrap().as_deref(), Some("yanked"));
        registers.record_delete(None, lines("deleted"));
        assert_eq!(host.0.lock().unwrap().as_deref(), Some("deleted\n"));
        assert_eq!(
            text_of(&mut registers, '1').as_deref(),
            Some("deleted\n"),
            "1 still fills"
        );
        assert_eq!(
            text_of(&mut registers, '0').as_deref(),
            Some("yanked"),
            "0 still fills"
        );
        registers.record_yank(Some('a'), chars("named"));
        registers.record_delete(Some('_'), chars("gone"));
        assert_eq!(
            host.0.lock().unwrap().as_deref(),
            Some("deleted\n"),
            "untouched"
        );
        *host.0.lock().unwrap() = Some("from another app".to_string());
        assert_eq!(registers.read(None), Some(chars("from another app")));
        assert_eq!(registers.read(Some('"')), Some(chars("from another app")));
    }

    #[test]
    fn test_clipboard_text_the_editor_put_there_comes_back_as_it_was_taken() {
        let host = Shared::default();
        let mut registers = on_clipboard(host.clone());
        let block = RegisterText::new(RegisterKind::Blockwise, "ab\ncd");
        registers.record_yank(None, block.clone());
        assert_eq!(registers.read(None), Some(block.clone()), "still a block");
        assert_eq!(registers.read(Some('+')), Some(block));
        *host.0.lock().unwrap() = Some("ab\ncd\n".to_string());
        assert_eq!(
            registers.read(None),
            Some(lines("ab\ncd")),
            "changed outside"
        );
    }

    #[test]
    fn test_an_unnamed_clipboard_that_gives_nothing_falls_back_to_the_unnamed_register() {
        struct Unreadable;
        impl Clipboard for Unreadable {
            fn read(&mut self, _: ClipboardSelection) -> Option<String> {
                None
            }
            fn write(&mut self, _: ClipboardSelection, _: &str) {}
        }
        let mut registers = Registers::new(Box::new(Unreadable));
        registers.set_unnamed_clipboard(true);
        registers.record_yank(None, chars("kept"));
        assert_eq!(registers.read(None), Some(chars("kept")));
        assert_eq!(registers.read(Some('+')), None, "+ itself has nothing");
    }

    #[test]
    fn test_read_only_registers_hold_the_last_insert_command_search_and_name() {
        let mut harness = Harness::new("|one two");
        let view = harness.view;
        let document = harness.editor.view(view).unwrap().document();
        let read = |harness: &mut Harness, name| {
            harness
                .editor
                .read_register(view, Some(name))
                .map(|text| text.text)
        };
        assert_eq!(read(&mut harness, '%'), None, "no name set yet");
        harness
            .editor
            .set_document_name(document, Some("src/main.rs"));
        assert_eq!(read(&mut harness, '%').as_deref(), Some("src/main.rs"));
        harness.keys("o new SPC line <esc> cw x <esc>");
        assert_eq!(read(&mut harness, '.').as_deref(), Some("x"));
        harness.keys("O ab <bs> c <esc>");
        assert_eq!(
            read(&mut harness, '.').as_deref(),
            Some("ac"),
            "o's break left out"
        );
        harness.keys("/ t.o <cr>");
        assert_eq!(read(&mut harness, '/').as_deref(), Some("t.o"));
        harness.keys(": s/two/2/ <cr>");
        assert_eq!(read(&mut harness, ':').as_deref(), Some("s/two/2/"));
        assert_eq!(
            read(&mut harness, '/').as_deref(),
            Some("two"),
            ":s searches"
        );
        harness.keys("y y \". y w");
        assert_eq!(
            read(&mut harness, '"').as_deref(),
            Some("one 2\n"),
            "a yank into a read-only register keeps nothing"
        );
        harness.keys("g g \". P");
        let text = harness.editor.document(document).unwrap().to_string();
        assert_eq!(text, "acone 2\nac\nnew linx");
    }

    #[test]
    fn test_at_colon_repeats_the_last_ex_command_and_at_at_repeats_it_again() {
        let mut harness = Harness::new("|a a a a");
        let document = harness.editor.view(harness.view).unwrap().document();
        harness.keys(": s/a/b/ <cr> @ : @ @");
        let text = harness.editor.document(document).unwrap().to_string();
        assert_eq!(text, "b b b a");
    }
}
