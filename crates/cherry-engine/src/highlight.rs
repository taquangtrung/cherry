//! Highlighting: the trait a host's syntax highlighter implements so the
//! editor paints the spans it finds, and asks it for structural text
//! objects, without Cherry depending on any parser.
//!
//! A host gives a document a [`Highlighter`] with
//! [`Editor::set_highlighter`](crate::Editor::set_highlighter). The editor
//! then tells it of every change to the document, undo and redo included,
//! with [`Highlighter::edit`], so it can reparse incrementally, and asks
//! it for the [`HighlightSpan`]s of the lines in sight each time it
//! renders. A highlighter that colors a line from the state the line
//! before left, as TextMate grammars do, must scan from the top to reach
//! the lines in sight; it does that work in [`Highlighter::prepare`], a
//! budget of lines at a time, as the editor's [`Editor::tick`] asks, and
//! says when it is done, so a long file colors over a few frames instead
//! of stalling one. Each span names a [`HighlightId`], an id the
//! highlighter chooses (the `syntax` module numbers its highlight
//! names; a host whose styles combine a color and a weight numbers each
//! combination it meets, as interning does); it
//! paints as the cell's [`highlight`](crate::Cell::highlight), a layer of
//! its own, so the host colors the text by it and keeps the background
//! the cell's [`Style`](crate::Style) gives (the cursor line, a selection).

use std::fmt;
use std::ops::{Range, RangeInclusive};

use super::document::Document;
use super::transaction::ChangeSet;

// ========================================================================
// Data Structures
// ========================================================================

/// Names what a highlighted span is (a keyword, a string), for the host to
/// color; the highlighter that made it says which name each id stands for.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct HighlightId(u16);

/// A run of highlighted text.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct HighlightSpan {
    /// What the text is.
    pub highlight: HighlightId,
    /// The chars highlighted, as offsets into the document.
    pub range: Range<usize>,
}

/// What Enter does to the line it opens, as a highlighter that knows the
/// document's language says ([`Highlighter::enter`]).
#[derive(Clone, Debug, Default, Eq, Hash, PartialEq)]
pub struct EnterIndent {
    /// What the new line starts with after its indent: a comment's leader
    /// going on (`/// `, ` * `), or nothing.
    pub append: String,
    /// How the new line's indent differs from the line's.
    pub indent: IndentChange,
    /// How many chars of that indent the new line drops.
    pub remove: usize,
    /// Whether the text after the cursor moves to a line of its own after
    /// the new one, at the line's indent: Enter between `{` and `}`.
    pub split: bool,
}

/// How a line opened by Enter is indented against the line it came from.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum IndentChange {
    /// One level deeper ([`EditorOptions::shift_width`](crate::EditorOptions::shift_width)).
    Indent,
    /// The same.
    #[default]
    Keep,
    /// One level shallower.
    Outdent,
}

/// Whether a highlighter has done all the work the spans asked for need.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Preparation {
    /// Work is left: [`Highlighter::highlights`] gives what is done, the
    /// rest painting plain, and the editor asks to be ticked again at once.
    Pending,
    /// Done: the spans of the chars asked for are complete.
    Ready,
}

// ========================================================================
// Traits
// ========================================================================

/// Finds the highlighted spans of a document, and its structure, as the
/// text changes. One highlighter serves one document.
pub trait Highlighter: fmt::Debug + Send {
    /// Takes note that `changes` were made to the document, which is now
    /// `document`: the moment to reparse.
    fn edit(&mut self, changes: &ChangeSet, document: &Document);

    /// Does up to `lines` lines of the work the spans of the chars `range`
    /// of `document` need, keeping it for [`Highlighter::highlights`], and
    /// says whether any is left. The editor calls it from
    /// [`Editor::tick`](crate::Editor::tick) for the lines in sight of each
    /// view, with [`EditorOptions::highlight_budget`](crate::EditorOptions::highlight_budget)
    /// lines, until it is [`Preparation::Ready`], and again after the text
    /// changes or the view scrolls. The default has nothing to prepare: a
    /// highlighter whose spans come from a tree it keeps up to date in
    /// [`Highlighter::edit`].
    fn prepare(&mut self, document: &Document, range: Range<usize>, lines: usize) -> Preparation {
        let _ = (document, range, lines);
        Preparation::Ready
    }

    /// The spans touching the chars `range` of `document`. Where spans
    /// overlap, a later one wins, so an inner span (an escape in a string)
    /// comes after the one around it.
    fn highlights(&self, document: &Document, range: Range<usize>) -> Vec<HighlightSpan>;

    /// What Enter at char `offset` of `document` does to the line it opens,
    /// as the document's language has it: an indent after an opening
    /// bracket or a `:`, a comment going on, a closer moved to its own
    /// line. `None`, the default, leaves the line its plain auto-indent.
    fn enter(&mut self, document: &Document, offset: usize) -> Option<EnterIndent> {
        let _ = (document, offset);
        None
    }

    /// Whether line `line` of `document`, as typed so far, closes the
    /// block it is in (`else:`, `end`), so a line Enter auto-indented goes
    /// one level shallower. `false`, the default, for none.
    fn outdents(&mut self, document: &Document, line: usize) -> bool {
        let _ = (document, line);
        false
    }

    /// The runs of lines of `document` that may fold, each its first and
    /// last line, as the document's language folds (by brackets or by
    /// indentation), for a document the host gave none
    /// ([`Editor::set_fold_ranges`](crate::Editor::set_fold_ranges)).
    /// `None`, the default, folds indented blocks.
    fn fold_ranges(&self, document: &Document) -> Option<Vec<RangeInclusive<usize>>> {
        let _ = document;
        None
    }

    /// The chars of the structural text object `object` (`"function"`,
    /// `"class"`, as the highlighter names them) around char `offset` of
    /// `document`: the `count`-th enclosing one, whole when `around`, else
    /// its inside. `None` when there is none or the highlighter knows no
    /// such object, which is the default.
    fn text_object(
        &self,
        document: &Document,
        object: &str,
        around: bool,
        offset: usize,
        count: usize,
    ) -> Option<Range<usize>> {
        let _ = (document, object, around, offset, count);
        None
    }
}

// ========================================================================
// HighlightId Implementations
// ========================================================================

impl HighlightId {
    /// The id numbered `index`.
    pub fn new(index: u16) -> Self {
        Self(index)
    }

    /// The number of the id.
    pub fn index(self) -> u16 {
        self.0
    }
}

// ========================================================================
// HighlightSpan Implementations
// ========================================================================

impl HighlightSpan {
    /// A span of `highlight` over the chars `range`.
    pub fn new(range: Range<usize>, highlight: HighlightId) -> Self {
        Self { highlight, range }
    }
}

// ========================================================================
// Tests
// ========================================================================

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::command::CommandContext;
    use crate::options::EditorOptions;
    use crate::paint::Surface;
    use crate::render::render_view;
    use crate::testing::Harness;
    use crate::view::Mode;

    /// Highlights every digit as id 1 and every run of digits as the
    /// object `"number"`, counting the changes it is told of.
    #[derive(Debug)]
    struct Digits {
        /// The changes told, shared with the test.
        edits: Arc<AtomicUsize>,
        /// The text as last told.
        text: String,
    }

    impl Highlighter for Digits {
        fn edit(&mut self, changes: &ChangeSet, document: &Document) {
            self.edits
                .fetch_add(changes.edits().len(), Ordering::Relaxed);
            self.text = document.slice(0..document.len_chars());
        }

        fn highlights(&self, _: &Document, range: Range<usize>) -> Vec<HighlightSpan> {
            self.text
                .chars()
                .enumerate()
                .filter(|(offset, c)| range.contains(offset) && c.is_ascii_digit())
                .map(|(offset, _)| HighlightSpan::new(offset..offset + 1, HighlightId::new(1)))
                .collect()
        }

        fn text_object(
            &self,
            _: &Document,
            object: &str,
            _: bool,
            offset: usize,
            _: usize,
        ) -> Option<Range<usize>> {
            let chars: Vec<char> = self.text.chars().collect();
            if object != "number" || !chars.get(offset)?.is_ascii_digit() {
                return None;
            }
            let start = (0..offset)
                .rev()
                .take_while(|at| chars[*at].is_ascii_digit())
                .last()
                .unwrap_or(offset);
            let end = (offset..chars.len())
                .find(|at| !chars[*at].is_ascii_digit())
                .unwrap_or(chars.len());
            Some(start..end)
        }
    }

    /// A harness editing `marked` with a [`Digits`] highlighter, and its
    /// count of changes told.
    fn harness(marked: &str) -> (Harness, Arc<AtomicUsize>) {
        let mut harness = Harness::new(marked);
        let document = harness.editor.view(harness.view).unwrap().document();
        let edits = Arc::new(AtomicUsize::new(0));
        let text = harness.editor.document(document).unwrap().to_string();
        let digits = Digits {
            edits: Arc::clone(&edits),
            text,
        };
        harness
            .editor
            .set_highlighter(document, Some(Box::new(digits)));
        (harness, edits)
    }

    /// The highlight of each cell of the first row.
    fn painted(harness: &Harness) -> Vec<Option<u16>> {
        let mut surface = Surface::new(0, 0);
        render_view(&harness.editor, harness.view, &mut surface);
        (0..surface.cols())
            .map(|column| {
                let cell = surface.cell(column, 0).unwrap();
                cell.highlight.map(HighlightId::index)
            })
            .collect()
    }

    #[test]
    fn test_the_highlighter_hears_every_change_and_its_spans_paint_under_any_style() {
        let (mut harness, edits) = harness("|a1 b");
        harness.editor.resize_view(harness.view, 6, 1);
        assert_eq!(painted(&harness), [None, Some(1), None, None, None, None]);
        harness.keys("A 2 <esc>");
        let typed = [None, Some(1), None, None, Some(1), None];
        assert_eq!(painted(&harness), typed);
        harness.keys("u");
        assert_eq!(edits.load(Ordering::Relaxed), 2, "the typed 2 and its undo");
        assert_eq!(painted(&harness), [None, Some(1), None, None, None, None]);
        harness.keys("0 v l");
        assert_eq!(painted(&harness)[1], Some(1), "under the selection too");
    }

    /// Colors the digits of only the lines it has scanned, scanning from
    /// the top a budget of lines at a time, as a TextMate grammar does,
    /// and starting over on any change.
    #[derive(Debug, Default)]
    struct LineByLine {
        /// The lines scanned, from the top.
        scanned: usize,
    }

    impl Highlighter for LineByLine {
        fn edit(&mut self, _: &ChangeSet, _: &Document) {
            self.scanned = 0;
        }

        fn prepare(
            &mut self,
            document: &Document,
            range: Range<usize>,
            lines: usize,
        ) -> Preparation {
            let end = range.end.min(document.len_chars());
            let last = document.char_to_position(end).map_or(0, |at| at.line);
            self.scanned = (self.scanned + lines).min(last + 1);
            match self.scanned > last {
                true => Preparation::Ready,
                false => Preparation::Pending,
            }
        }

        fn highlights(&self, document: &Document, range: Range<usize>) -> Vec<HighlightSpan> {
            let text = document.to_string();
            text.chars()
                .enumerate()
                .filter(|(offset, c)| {
                    range.contains(offset)
                        && c.is_ascii_digit()
                        && document
                            .char_to_position(*offset)
                            .is_some_and(|at| at.line < self.scanned)
                })
                .map(|(offset, _)| HighlightSpan::new(offset..offset + 1, HighlightId::new(1)))
                .collect()
        }
    }

    #[test]
    fn test_a_line_by_line_highlighter_is_ticked_until_the_lines_in_sight_are_ready() {
        let options = EditorOptions {
            highlight_budget: 2,
            ..EditorOptions::default()
        };
        let mut harness = Harness::with_options("|1\n2\n3\n4\n5", options);
        harness.editor.resize_view(harness.view, 2, 5);
        let document = harness.editor.view(harness.view).unwrap().document();
        let highlighter = Box::new(LineByLine::default());
        harness.editor.set_highlighter(document, Some(highlighter));
        let colored = |harness: &Harness| {
            let mut surface = Surface::new(0, 0);
            render_view(&harness.editor, harness.view, &mut surface);
            (0..5)
                .filter(|row| surface.cell(0, *row).unwrap().highlight.is_some())
                .count()
        };
        assert_eq!(colored(&harness), 0, "nothing scanned yet");
        assert!(
            harness.editor.deadline().is_some(),
            "a tick is wanted at once"
        );
        assert!(harness.wait(), "the tick colored lines: repaint");
        assert_eq!(colored(&harness), 2, "the first budget");
        assert!(harness.editor.deadline().is_some(), "work is left");
        harness.wait();
        harness.wait();
        assert_eq!(colored(&harness), 5);
        assert_eq!(harness.editor.deadline(), None, "ready: no more ticks");
        assert!(!harness.wait(), "nothing left to repaint for");
        harness.keys("r 9");
        assert!(
            harness.editor.deadline().is_some(),
            "an edit wants the lines again"
        );
    }

    #[test]
    fn test_a_host_text_object_acts_after_an_operator_and_selects_in_visual_mode() {
        let (mut harness, _) = harness("ab 12|34 cd");
        let number = |cx: &mut CommandContext<'_>| {
            let view = cx.editor.view(cx.view).unwrap();
            let (document, cursor) = (view.document(), view.cursor());
            let range = cx.editor.highlighter(document).and_then(|highlighter| {
                let text = cx.editor.document(document).unwrap();
                highlighter.text_object(text, "number", true, cursor, 1)
            });
            cx.act_on_text_object(range);
        };
        harness.editor.register_command("number", number).unwrap();
        for mode in [Mode::OperatorPending, Mode::Visual] {
            harness.editor.bind(mode, &["a n"], "number").unwrap();
        }
        harness.keys("v a n y");
        assert_eq!(harness.marked(), "ab |1234 cd");
        harness.keys("l d a n");
        assert_eq!(harness.marked(), "ab | cd");
        harness.keys("d a n");
        assert_eq!(
            harness.mode(),
            Mode::Normal,
            "no number, operator cancelled"
        );
    }
}
