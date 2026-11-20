//! Moving by the structure of code: to the start and end of the function
//! around the cursor, and between the blocks indentation makes.
//!
//! `[m` (in Insert mode `C-M-a`) goes to the start of the function around
//! the cursor, the count-th one out, or when already at its start to the
//! one around that; `]M` goes to its last char. The function is the
//! highlighter's `"function"` text object
//! ([`Highlighter::text_object`](crate::Highlighter::text_object)), so
//! these find nothing in a document whose highlighter knows none.
//!
//! `zj` goes down to the next line that opens a block, one whose next line
//! with text is indented deeper, and `zk` back up to the one before, as
//! Vim steps between folds; blank lines are passed over. Each is a jump
//! and a motion, linewise after an operator.

use std::ops::Range;

use super::command::CommandContext;
use super::document::{Document, line_of, start_of_line};
use super::mark::jump_to;
use super::motion::first_non_blank_of_line;
use super::operator::MotionKind;

// ========================================================================
// Constants
// ========================================================================

/// The highlighter's name for the text object of a function.
const FUNCTION: &str = "function";

/// The notice when no block lies further in the way gone.
const NO_BLOCK: &str = "No block further on";

// ========================================================================
// Functions
// ========================================================================

/// `[m`: the start of the function around the cursor.
pub(crate) fn definition_start(cx: &mut CommandContext<'_>) {
    let cursor = cx.editor.view_ref(cx.view).cursor();
    let count = cx.count_or_one();
    let Some(mut found) = function_around(cx, cursor, count) else {
        return;
    };
    if found.start == cursor {
        match function_around(cx, cursor, count + 1) {
            Some(outer) => found = outer,
            None => return,
        }
    }
    jump_to(cx, MotionKind::Exclusive, found.start);
}

/// `]M`: the last char of the function around the cursor.
pub(crate) fn definition_end(cx: &mut CommandContext<'_>) {
    let cursor = cx.editor.view_ref(cx.view).cursor();
    let Some(found) = function_around(cx, cursor, cx.count_or_one()) else {
        return;
    };
    let last = cx
        .editor
        .document_of(cx.view)
        .prev_grapheme_boundary(found.end);
    jump_to(cx, MotionKind::Inclusive, last.max(found.start));
}

/// `zj`: down to the next line opening a block.
pub(crate) fn next_block(cx: &mut CommandContext<'_>) {
    step_block(cx, true);
}

/// `zk`: up to the line opening the block before.
pub(crate) fn previous_block(cx: &mut CommandContext<'_>) {
    step_block(cx, false);
}

/// The chars of the `count`-th function around `offset`, by the
/// highlighter of the cursor's document.
fn function_around(cx: &CommandContext<'_>, offset: usize, count: usize) -> Option<Range<usize>> {
    let document = cx.editor.view_ref(cx.view).document();
    let highlighter = cx.editor.highlighter(document)?;
    highlighter.text_object(
        cx.editor.document_of(cx.view),
        FUNCTION,
        true,
        offset,
        count,
    )
}

/// Goes to the first non-blank of the count-th line opening a block, down
/// or up; a notice when there is none.
fn step_block(cx: &mut CommandContext<'_>, down: bool) {
    let document = cx.editor.document_of(cx.view);
    let line = line_of(document, cx.editor.view_ref(cx.view).cursor());
    let openers = block_openers(document);
    let mut target = None;
    let mut from = line;
    for _ in 0..cx.count_or_one() {
        let next = match down {
            true => openers.iter().find(|opener| **opener > from),
            false => openers.iter().rev().find(|opener| **opener < from),
        };
        match next {
            Some(next) => {
                from = *next;
                target = Some(*next);
            }
            None => break,
        }
    }
    let Some(target) = target else {
        return cx.editor.error(NO_BLOCK.to_string());
    };
    let start = start_of_line(cx.editor.document_of(cx.view), target);
    let at = first_non_blank_of_line(cx.editor, cx.view, start);
    jump_to(cx, MotionKind::Linewise, at);
}

/// The lines of `document` that open a block: a line with text whose next
/// line with text is indented deeper.
fn block_openers(document: &Document) -> Vec<usize> {
    let indent = |line: usize| {
        let text = document.line_text(line)?;
        if text.trim().is_empty() {
            return None;
        }
        Some(text.chars().take_while(|c| *c == ' ' || *c == '\t').count())
    };
    let with_text: Vec<(usize, usize)> = (0..document.len_lines())
        .filter_map(|line| Some((line, indent(line)?)))
        .collect();
    with_text
        .windows(2)
        .filter(|pair| pair[1].1 > pair[0].1)
        .map(|pair| pair[0].0)
        .collect()
}

// ========================================================================
// Tests
// ========================================================================

#[cfg(test)]
mod tests {
    use std::ops::Range;

    use crate::document::Document;
    use crate::highlight::{HighlightSpan, Highlighter};
    use crate::testing::{Harness, assert_keys};
    use crate::transaction::ChangeSet;

    /// A highlighter whose functions are the brace groups starting at a
    /// line's start, fixed for the text it was made over.
    #[derive(Debug)]
    struct Braces(Vec<Range<usize>>);

    impl Highlighter for Braces {
        fn edit(&mut self, _: &ChangeSet, _: &Document) {}

        fn highlights(&self, _: &Document, _: Range<usize>) -> Vec<HighlightSpan> {
            Vec::new()
        }

        fn text_object(
            &self,
            _: &Document,
            object: &str,
            _: bool,
            offset: usize,
            count: usize,
        ) -> Option<Range<usize>> {
            let mut around = self
                .0
                .iter()
                .filter(|range| object == "function" && range.contains(&offset))
                .collect::<Vec<_>>();
            around.sort_by_key(|range| range.len());
            around.get(count - 1).map(|range| (*range).clone())
        }
    }

    #[test]
    fn test_m_brackets_go_to_the_start_and_end_of_the_function_around() {
        let mut harness = Harness::new("{a {b |c} d}");
        let document = harness.editor.view(harness.view).unwrap().document();
        harness
            .editor
            .set_highlighter(document, Some(Box::new(Braces(vec![0..12, 3..8]))));
        harness.keys("[ m");
        assert_eq!(harness.marked(), "{a |{b c} d}");
        harness.keys("[ m");
        assert_eq!(
            harness.marked(),
            "|{a {b c} d}",
            "at a start, the one around"
        );
        harness.keys("] M");
        assert_eq!(harness.marked(), "{a {b c} d|}");
    }

    #[test]
    fn test_zj_and_zk_step_between_the_lines_that_open_blocks() {
        let text = "|a\n  b\n\n  c\n    d\ne\n  f";
        assert_keys(text, "z j", "a\n  b\n\n  |c\n    d\ne\n  f");
        assert_keys(text, "z j z j", "a\n  b\n\n  c\n    d\n|e\n  f");
        assert_keys(text, "G z k", "a\n  b\n\n  c\n    d\n|e\n  f");
        assert_keys(text, "2 z j", "a\n  b\n\n  c\n    d\n|e\n  f");
        assert_keys(text, "z j d z j", "a\n  b\n\n  |f");
    }
}
