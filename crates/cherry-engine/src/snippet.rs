//! Snippets: text with places to fill in, in the syntax language servers
//! use. `$1`, `$2` are tab stops, `${1:name}` a tab stop holding a
//! placeholder, `$0` the place the cursor ends at, and `\$`, `\}`, and
//! `\\` the chars themselves; a placeholder may hold tab stops of its own.
//! A `$` that starts none of these is the char itself.
//!
//! Inserting a snippet ([`Editor::insert_snippet`](crate::Editor::insert_snippet))
//! types its text without the syntax and goes to its first stop; a
//! completion item marked as one ([`CompletionItem::with_snippet`](crate::CompletionItem::with_snippet))
//! types it the same way and leaves the cursor at its end, for `<tab>` to
//! go to the first stop. The stops are visited in number order, `$0`
//! last (or the snippet's end when it has none). In Insert mode `<tab>` goes to the
//! next stop and `S-<tab>` back to the one before; the first char typed at
//! a stop with a placeholder, or Backspace, replaces the placeholder
//! whole. Edits carry the stops along, text typed at a stop's end growing
//! it. The snippet ends at its last stop, when Insert mode is left, or
//! when an undo or redo changes the text. A number used twice is a stop
//! only where it first appears.

use std::ops::Range;

use super::command::CommandContext;
use super::editor::{Editor, ViewId};
use super::insert::insert_text;
use super::transaction::{Assoc, ChangeSet, Edit};

// ========================================================================
// Constants
// ========================================================================

/// The char that starts a tab stop.
const DOLLAR: char = '$';

/// The char that escapes the next one.
const ESCAPE: char = '\\';

/// The chars that open and close a tab stop with a placeholder.
const OPEN: char = '{';
const CLOSE: char = '}';

/// The char between a tab stop's number and its placeholder.
const PLACEHOLDER: char = ':';

/// What `<tab>` types with no snippet to fill in.
const TAB: &str = "\t";

// ========================================================================
// Data Structures
// ========================================================================

/// A snippet's text without its syntax, and its tab stops.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct Snippet {
    /// The stops in the order they are visited, as char ranges of `text`:
    /// their placeholders, or empty where they have none.
    pub stops: Vec<Range<usize>>,
    /// The text typed.
    pub text: String,
}

/// A snippet being filled in: its stops in the document, carried by
/// edits, and the one the cursor went to last.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SnippetSession {
    /// Whether the current stop's placeholder is still as inserted, for
    /// the next char typed there to replace.
    fresh: bool,
    /// The stop the cursor went to last; `None` before the first, for a
    /// snippet a completion typed.
    index: Option<usize>,
    /// The stops, as char ranges of the document, in visiting order.
    stops: Vec<Range<usize>>,
}

/// The parse of a snippet, gathering stops by number as it goes.
struct Parser<'a> {
    /// The chars left to read.
    chars: std::iter::Peekable<std::str::Chars<'a>>,
    /// The stops found, by number and range in `text`.
    stops: Vec<(u32, Range<usize>)>,
    /// The text so far.
    text: String,
    /// The chars in `text`.
    len: usize,
}

// ========================================================================
// Snippet Implementations
// ========================================================================

impl Snippet {
    /// The snippet `source` writes, as the module docs say.
    pub(crate) fn parse(source: &str) -> Self {
        let mut parser = Parser {
            chars: source.chars().peekable(),
            len: 0,
            stops: Vec::new(),
            text: String::new(),
        };
        parser.read(false);
        let mut numbered = parser.stops;
        let has_end = numbered.iter().any(|(number, _)| *number == 0);
        numbered.sort_by_key(|(number, _)| match number {
            0 => u32::MAX,
            number => *number,
        });
        let mut stops: Vec<Range<usize>> = numbered.into_iter().map(|(_, range)| range).collect();
        if !has_end {
            stops.push(parser.len..parser.len);
        }
        Self {
            stops,
            text: parser.text,
        }
    }
}

// ========================================================================
// Parser Implementations
// ========================================================================

impl Parser<'_> {
    /// Reads text and stops until the end, or until an unescaped `}` when
    /// `in_placeholder`, which it consumes.
    fn read(&mut self, in_placeholder: bool) {
        while let Some(c) = self.chars.next() {
            match c {
                ESCAPE => match self
                    .chars
                    .next_if(|next| [DOLLAR, CLOSE, ESCAPE].contains(next))
                {
                    Some(escaped) => self.push(escaped),
                    None => self.push(ESCAPE),
                },
                CLOSE if in_placeholder => return,
                DOLLAR => self.read_stop(),
                c => self.push(c),
            }
        }
    }

    /// Reads what follows a `$`: a stop's number, or `{`, its number, and
    /// maybe a placeholder; anything else leaves the `$` as text.
    fn read_stop(&mut self) {
        let braced = self.chars.next_if_eq(&OPEN).is_some();
        let mut digits = String::new();
        while let Some(digit) = self.chars.next_if(char::is_ascii_digit) {
            digits.push(digit);
        }
        let Ok(number) = digits.parse::<u32>() else {
            self.push(DOLLAR);
            if braced {
                self.push(OPEN);
            }
            return;
        };
        let start = self.len;
        if braced {
            if self.chars.next_if_eq(&PLACEHOLDER).is_some() {
                self.read(true);
            } else {
                self.chars.next_if_eq(&CLOSE);
            }
        }
        if !self.stops.iter().any(|(seen, _)| *seen == number) {
            self.stops.push((number, start..self.len));
        }
    }

    /// Adds `c` to the text.
    fn push(&mut self, c: char) {
        self.text.push(c);
        self.len += 1;
    }
}

// ========================================================================
// SnippetSession Implementations
// ========================================================================

impl SnippetSession {
    /// Carries the stops along `changes`, a stop's start staying before
    /// text typed at it and its end going after, so typing grows it.
    pub(crate) fn map(&mut self, changes: &ChangeSet) {
        for stop in &mut self.stops {
            let start = changes.map_position(stop.start, Assoc::Before);
            let end = changes.map_position(stop.end, Assoc::After);
            *stop = start..end.max(start);
        }
    }
}

// ========================================================================
// Functions
// ========================================================================

/// Types `snippet` at the cursor of `view` and goes to its first stop.
pub(crate) fn insert(editor: &mut Editor, view: ViewId, snippet: &str) {
    let start = editor.view_ref(view).cursor();
    let parsed = Snippet::parse(snippet);
    insert_text(editor, view, &parsed.text);
    start_session(editor, view, start, parsed);
    go_to(editor, view, 0);
}

/// Starts filling in `snippet`, typed already at `start` in `view`, the
/// cursor left where it is.
pub(crate) fn start_session(editor: &mut Editor, view: ViewId, start: usize, snippet: Snippet) {
    let stops = snippet
        .stops
        .into_iter()
        .map(|stop| start + stop.start..start + stop.end)
        .collect();
    let session = SnippetSession {
        fresh: false,
        index: None,
        stops,
    };
    editor.view_mut(view).set_snippet(Some(session));
}

/// `<tab>` in Insert mode: the next stop of the snippet being filled in,
/// ending it at the last; a tab when none is.
pub(crate) fn next_stop_or_tab(cx: &mut CommandContext<'_>) {
    let Some(session) = cx.editor.view_ref(cx.view).snippet() else {
        return insert_text(cx.editor, cx.view, TAB);
    };
    let next = session.index.map_or(0, |index| index + 1);
    go_to(cx.editor, cx.view, next);
}

/// `S-<tab>` in Insert mode: the stop before, in the snippet being filled
/// in.
pub(crate) fn previous_stop(cx: &mut CommandContext<'_>) {
    let Some(session) = cx.editor.view_ref(cx.view).snippet() else {
        return;
    };
    let previous = session.index.map_or(0, |index| index.saturating_sub(1));
    go_to(cx.editor, cx.view, previous);
}

/// Before a char is typed or deleted at the cursor of `view`: deletes the
/// placeholder of the stop the cursor went to, while it is as inserted,
/// leaving the cursor where it started; returns whether it did.
pub(crate) fn take_placeholder(editor: &mut Editor, view: ViewId) -> bool {
    let cursor = editor.view_ref(view).cursor();
    let Some(session) = editor.view_mut(view).snippet_mut() else {
        return false;
    };
    let Some(index) = session.index else {
        return false;
    };
    let stop = session.stops[index].clone();
    if !session.fresh || stop.is_empty() || cursor != stop.end {
        return false;
    }
    session.fresh = false;
    editor.apply_edits(view, vec![Edit::new(stop.clone(), "")]);
    editor.set_cursor(view, stop.start);
    true
}

/// Goes to stop `index` of the snippet of `view`, its placeholder left
/// for the next char typed to replace; at or past the last stop, there,
/// and the snippet ends.
fn go_to(editor: &mut Editor, view: ViewId, index: usize) {
    let Some(session) = editor.view_mut(view).snippet_mut() else {
        return;
    };
    let last = session.stops.len() - 1;
    let index = index.min(last);
    let stop = session.stops[index].clone();
    session.index = Some(index);
    session.fresh = !stop.is_empty();
    if index == last {
        editor.view_mut(view).set_snippet(None);
    }
    editor.set_cursor(view, stop.end);
}

// ========================================================================
// Tests
// ========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::Harness;

    #[test]
    fn test_parse_takes_stops_placeholders_and_escapes_in_number_order_zero_last() {
        let snippet = Snippet::parse("fn ${1:name}($2) {\n\t$0\n} \\$x ${3:a ${4:b}}");
        assert_eq!(snippet.text, "fn name() {\n\t\n} $x a b");
        let texts: Vec<&str> = snippet
            .stops
            .iter()
            .map(|stop| &snippet.text[stop.clone()])
            .collect();
        assert_eq!(texts, ["name", "", "a b", "b", ""]);
        assert_eq!(snippet.stops[1], 8..8, "inside the parentheses");
        assert_eq!(snippet.stops[4], 13..13, "$0 last");
    }

    #[test]
    fn test_a_snippet_without_zero_ends_at_its_end_and_a_bare_dollar_is_text() {
        let snippet = Snippet::parse("a$1 b $x");
        assert_eq!(snippet.text, "a b $x");
        assert_eq!(snippet.stops, [1..1, 6..6]);
    }

    /// The text after inserting `fn ${1:name}($2) {}` in Insert mode and
    /// typing `keys`.
    fn filled(keys: &str) -> String {
        let mut harness = Harness::new("|");
        harness.keys("i");
        harness
            .editor
            .insert_snippet(harness.view, "fn ${1:name}($2) {}");
        harness.keys(keys);
        harness.marked()
    }

    #[test]
    fn test_tab_walks_the_stops_and_typing_replaces_a_placeholder() {
        assert_eq!(filled(""), "fn name|() {}", "at the first stop");
        assert_eq!(filled("x"), "fn x|() {}", "the placeholder replaced");
        assert_eq!(filled("<tab> a <tab>"), "fn name(a) {}|", "the end last");
        assert_eq!(
            filled("<tab> S-<tab> <bs> y"),
            "fn y|() {}",
            "back, and Backspace"
        );
        assert_eq!(
            filled("<tab> <tab> <tab>"),
            "fn name() {}\t|",
            "a tab once done"
        );
        assert_eq!(
            filled("x y <esc> i <tab>"),
            "fn x\t|y() {}",
            "Escape ended it"
        );
    }
}
