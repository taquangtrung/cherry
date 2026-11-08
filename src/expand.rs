//! Growing and shrinking the selection by structure: `S-M-<right>` grows
//! it to the next piece of text holding it, starting Visual mode from
//! Normal, and `S-M-<left>` gives the last growth back, ending in Normal
//! mode where it started.
//!
//! The pieces come from the document's [`SelectionRangeProvider`] when the
//! host sets one ([`Editor::set_selection_range_provider`](crate::Editor::set_selection_range_provider)),
//! a language server's selection ranges, say; otherwise from the text
//! itself: the word at the cursor, the quoted text and then the quotes
//! with it, each pair of brackets around it, inside and then with the
//! brackets, outward, the line from its first non-blank, the paragraph,
//! and the whole document. Growing takes the smallest piece that holds the
//! selection and is larger; with none larger, it stays.

use std::fmt;
use std::ops::Range;

use super::command::CommandContext;
use super::document::{Document, first_non_blank_in, line_end, line_of, start_of_line};
use super::operator::Operand;
use super::text_object::{Reach, bracket_object, find_open, quote_object, word_object};
use super::view::Mode;
use super::visual::selection;

// ========================================================================
// Constants
// ========================================================================

/// The bracket pairs the selection grows through.
const BRACKETS: [(char, char); 4] = [('(', ')'), ('[', ']'), ('{', '}'), ('<', '>')];

/// The quotes the selection grows through.
const QUOTES: [char; 3] = ['"', '\'', '`'];

// ========================================================================
// Traits
// ========================================================================

/// A host's source of the pieces of text the selection grows through, such
/// as a language server's selection ranges.
pub trait SelectionRangeProvider: fmt::Debug + Send {
    /// The char ranges of `document` holding `offset`, innermost first.
    fn ranges(&self, document: &Document, offset: usize) -> Vec<Range<usize>>;
}

// ========================================================================
// Functions
// ========================================================================

/// `S-M-<right>`: grows the selection to the smallest larger piece holding
/// it, remembering the selection it grew from.
pub(crate) fn expand_selection(cx: &mut CommandContext<'_>) {
    let current = selected(cx);
    let document = cx.editor.document_of(cx.view);
    let cursor = cx.editor.view_ref(cx.view).cursor();
    let pieces = match cx.editor.selection_range_provider_of(cx.view) {
        Some(provider) => provider.ranges(document, cursor),
        None => text_pieces(document, cursor),
    };
    let Some(grown) = pieces
        .into_iter()
        .filter(|piece| piece.start <= current.start && piece.end >= current.end)
        .filter(|piece| piece.len() > current.len())
        .min_by_key(Range::len)
    else {
        return;
    };
    let was_visual = cx.editor.view_ref(cx.view).mode() == Mode::Visual;
    let state = cx.editor.view_mut(cx.view);
    state.push_growth(current, was_visual, grown.clone());
    select(cx, &grown);
}

/// `S-M-<left>`: gives the last growth back, to Normal mode where the
/// first one started; nothing when the selection changed since.
pub(crate) fn shrink_selection(cx: &mut CommandContext<'_>) {
    let current = selected(cx);
    let Some((previous, was_visual)) = cx.editor.view_mut(cx.view).pop_growth(&current) else {
        return;
    };
    if was_visual {
        return select(cx, &previous);
    }
    cx.editor.set_mode(cx.view, Mode::Normal);
    cx.editor.set_cursor(cx.view, previous.start);
}

/// The chars selected in a charwise visual selection, else the char at the
/// cursor.
fn selected(cx: &CommandContext<'_>) -> Range<usize> {
    let document = cx.editor.document_of(cx.view);
    let cursor = cx.editor.view_ref(cx.view).cursor();
    let anchor = match cx.editor.view_ref(cx.view).mode() {
        Mode::Visual => selection(cx.editor, cx.view).map_or(cursor, |(anchor, _)| anchor),
        Mode::CommandLine
        | Mode::Insert
        | Mode::Normal
        | Mode::OperatorPending
        | Mode::Replace
        | Mode::VisualBlock
        | Mode::VisualLine => cursor,
    };
    let (first, last) = (anchor.min(cursor), anchor.max(cursor));
    first..document.next_grapheme_boundary(last)
}

/// Selects `range` in Visual mode, the cursor on its last char.
fn select(cx: &mut CommandContext<'_>, range: &Range<usize>) {
    let last = cx
        .editor
        .document_of(cx.view)
        .prev_grapheme_boundary(range.end);
    cx.editor.set_mode(cx.view, Mode::Visual);
    cx.editor.set_visual_anchor(cx.view, range.start);
    cx.editor.set_cursor(cx.view, last.max(range.start));
}

/// The pieces of `document` holding `at`, from the text, as the module
/// docs say.
fn text_pieces(document: &Document, at: usize) -> Vec<Range<usize>> {
    let mut pieces = Vec::new();
    let mut charwise = |operand: Option<Operand>| {
        if let Some(Operand::Charwise(range)) = operand {
            pieces.push(range);
        }
    };
    charwise(Some(word_object(document, at, 1, false, Reach::Inner)));
    for quote in QUOTES {
        charwise(quote_object(document, at, 1, quote, Reach::Inner));
        charwise(quote_object(document, at, 2, quote, Reach::Inner));
    }
    for brackets in BRACKETS {
        let mut from = at;
        while let Some(Operand::Charwise(around)) =
            bracket_object(document, from, 1, brackets, Reach::Around)
        {
            pieces.push(around.start + 1..around.end - 1);
            pieces.push(around.clone());
            match find_open(document, around.start, brackets) {
                Some(outer) => from = outer,
                None => break,
            }
        }
    }
    let line = line_of(document, at);
    pieces.push(first_non_blank_in(document, at)..line_end(document, at));
    let paragraph = paragraph_around(document, line);
    pieces.push(paragraph);
    pieces.push(0..document.len_chars());
    pieces
}

/// The chars of the run of non-blank lines holding line `line`, without
/// the break after the last.
fn paragraph_around(document: &Document, line: usize) -> Range<usize> {
    let is_blank = |line: usize| {
        document
            .line_text(line)
            .is_none_or(|text| text.trim().is_empty())
    };
    let (mut first, mut last) = (line, line);
    while first > 0 && !is_blank(first - 1) {
        first -= 1;
    }
    while last + 1 < document.len_lines() && !is_blank(last + 1) {
        last += 1;
    }
    let start = start_of_line(document, first);
    start..line_end(document, start_of_line(document, last))
}

// ========================================================================
// Tests
// ========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::Harness;

    /// The text `harness` has selected.
    fn selected_text(harness: &Harness) -> String {
        let view = harness.editor.view(harness.view).unwrap();
        let anchor = view.visual_anchor().unwrap_or(view.cursor());
        let (first, last) = (anchor.min(view.cursor()), anchor.max(view.cursor()));
        harness
            .editor
            .document_of(harness.view)
            .slice(first..last + 1)
    }

    #[test]
    fn test_the_selection_grows_through_words_quotes_brackets_lines_and_back() {
        let mut harness = Harness::new("x\nf(a, [\"h|i\"])\ny");
        let mut grown = Vec::new();
        for _ in 0..8 {
            harness.keys("S-M-<right>");
            grown.push(selected_text(&harness));
        }
        assert_eq!(
            grown,
            [
                "hi",
                "\"hi\"",
                "[\"hi\"]",
                "a, [\"hi\"]",
                "(a, [\"hi\"])",
                "f(a, [\"hi\"])",
                "x\nf(a, [\"hi\"])\ny",
                "x\nf(a, [\"hi\"])\ny",
            ],
            "the bracket's inside equals the quotes, so it is skipped; the line is the paragraph"
        );
        for _ in 0..6 {
            harness.keys("S-M-<left>");
        }
        assert_eq!(selected_text(&harness), "hi");
        harness.keys("S-M-<left>");
        assert_eq!(harness.mode(), Mode::Normal);
        assert_eq!(
            harness.marked(),
            "x\nf(a, [\"h|i\"])\ny",
            "back where it started"
        );
    }

    /// A provider that offers two fixed ranges.
    #[derive(Debug)]
    struct Fixed;

    impl SelectionRangeProvider for Fixed {
        fn ranges(&self, _: &Document, _: usize) -> Vec<Range<usize>> {
            vec![1..3, 0..6]
        }
    }

    #[test]
    fn test_a_provider_gives_the_pieces_in_place_of_the_text() {
        let mut harness = Harness::new("a|bcdef");
        let document = harness.editor.view(harness.view).unwrap().document();
        harness
            .editor
            .set_selection_range_provider(document, Some(Box::new(Fixed)));
        harness.keys("S-M-<right>");
        assert_eq!(selected_text(&harness), "bc");
        harness.keys("S-M-<right> S-M-<right>");
        assert_eq!(selected_text(&harness), "abcdef", "none larger: it stays");
    }
}
