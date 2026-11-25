//! The test harness: an editor with one view, fed keys in the key notation
//! and read back as text with the cursor marked, so a test reads as what a
//! user types and sees.
//!
//! Text is written with a `|` where the cursor is: `"he|llo"` is the cursor
//! on the `l`. Keys are written as the key notation, separated by spaces,
//! where a word that is not one key is typed one character at a time:
//! `"i hello <esc>"` presses `i`, types `hello`, and presses Escape.

use std::time::Duration;

use crate::keys::{Key, KeyCode, Mods, Timestamp, parse_key};

use super::editor::{DocumentId, Editor, Handled, ViewId};
use super::options::EditorOptions;
use super::request::HostRequest;
use super::view::Mode;

// ========================================================================
// Constants
// ========================================================================

/// The cursor marker in harness text.
const CURSOR: char = '|';

/// The time that passes between two keys fed.
const KEY_INTERVAL: Duration = Duration::from_millis(1);

// ========================================================================
// Data Structures
// ========================================================================

/// An editor with one view, driven by key notation.
#[derive(Debug)]
pub(crate) struct Harness {
    /// The editor under test.
    pub editor: Editor,
    /// What the last key fed came to.
    pub last_handled: Option<Handled>,
    /// The time the next key is fed at.
    now: Timestamp,
    /// The one view.
    pub view: ViewId,
}

// ========================================================================
// Harness Implementations
// ========================================================================

impl Harness {
    /// A harness editing `marked`, text with one `|` for the cursor, with
    /// default options.
    pub(crate) fn new(marked: &str) -> Self {
        Self::with_options(marked, EditorOptions::default())
    }

    /// A harness editing `marked` as a single-line document, with
    /// `options`.
    pub(crate) fn single_line(marked: &str, options: EditorOptions) -> Self {
        Self::opened(marked, options, Editor::open_single_line_document)
    }

    /// A harness editing `marked` with `options`.
    pub(crate) fn with_options(marked: &str, options: EditorOptions) -> Self {
        Self::opened(marked, options, Editor::open_document)
    }

    /// A harness editing `marked`, opened with `open`, with `options`.
    fn opened(
        marked: &str,
        options: EditorOptions,
        open: fn(&mut Editor, &str) -> DocumentId,
    ) -> Self {
        let cursor = marked
            .chars()
            .position(|c| c == CURSOR)
            .expect("the text marks the cursor with |");
        let text: String = marked.chars().filter(|c| *c != CURSOR).collect();
        let mut editor = Editor::new(options);
        let document = open(&mut editor, &text);
        let view = editor.new_view(document).expect("the document is open");
        editor.set_cursor(view, cursor);
        Self {
            editor,
            last_handled: None,
            now: Timestamp::default(),
            view,
        }
    }

    /// Feeds the keys `notation` writes, one key interval apart.
    pub(crate) fn keys(&mut self, notation: &str) -> &mut Self {
        for key in parse_keys(notation) {
            self.last_handled = Some(self.editor.handle_key(self.view, key, self.now));
            self.now = self.now + KEY_INTERVAL;
        }
        self
    }

    /// The text with `|` at the cursor.
    pub(crate) fn marked(&self) -> String {
        let view = self.editor.view(self.view).expect("the view is open");
        let document = self.editor.document(view.document()).expect("open");
        let mut text: Vec<char> = document.to_string().chars().collect();
        text.insert(view.cursor(), CURSOR);
        text.into_iter().collect()
    }

    /// The mode of the view.
    pub(crate) fn mode(&self) -> Mode {
        self.editor
            .view(self.view)
            .expect("the view is open")
            .mode()
    }

    /// Takes the requests the editor queued.
    pub(crate) fn requests(&mut self) -> Vec<HostRequest> {
        self.editor.take_requests()
    }

    /// Lets the key timeout pass with no key, and ticks the editor.
    pub(crate) fn wait(&mut self) -> bool {
        self.now = self.now + self.editor.options().key_timeout;
        self.editor.tick(self.now)
    }
}

// ========================================================================
// Functions
// ========================================================================

/// Feeds `keys` to a harness editing `before` and asserts the text and
/// cursor are `after`.
#[track_caller]
pub(crate) fn assert_keys(before: &str, keys: &str, after: &str) {
    let mut harness = Harness::new(before);
    harness.keys(keys);
    assert_eq!(harness.marked(), after, "{before:?} after {keys:?}");
}

/// The keys `notation` writes: each word one key when it parses as one,
/// else its characters typed one by one.
pub(crate) fn parse_keys(notation: &str) -> Vec<Key> {
    notation
        .split_whitespace()
        .flat_map(|word| match parse_key(word) {
            Ok(key) => vec![key],
            Err(_) => word
                .chars()
                .map(|c| Key::new(KeyCode::Char(c), Mods::NONE))
                .collect(),
        })
        .collect()
}
