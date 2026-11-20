//! Folds: runs of lines a view shows as their first line alone.
//!
//! Each view has its own folds, as each Vim window does. A fold covers its
//! first line to its last, and while it is closed the view shows only the
//! first, with a marker saying how many lines it hides
//! ([`Style::Folded`](crate::Style::Folded)); `j`, `k`, and every row
//! the view steps through pass over the rest, and any other motion that
//! lands inside a closed fold (a search, `G`, a mark) opens it, as Vim's
//! `foldopen` does. Folds are kept as char offsets, so edits carry them
//! along as they carry marks.
//!
//! # Where a fold comes from
//!
//! `zc` closes the innermost fold holding the cursor's line. When the line
//! is in none, it makes one: from the ranges the host gave the document
//! ([`Editor::set_fold_ranges`](crate::Editor::set_fold_ranges), from a
//! language server's folding ranges, say), else from the indent, as Vim's
//! `foldmethod=indent` does: a line and the lines after it indented more
//! deeply, blank lines among them included, or else the block the line is
//! indented inside. `zf` in a visual mode folds the selected lines.
//!
//! # Keys
//!
//! In the Cherry and Vim styles: `zc` closes, `zo` opens, `za` toggles, `zM`
//! closes every fold the document has room for and `zR` opens them all,
//! `zd` deletes the fold at the cursor and `zE` every fold. In the standard
//! style, Ctrl+{ folds and Ctrl+} unfolds.

use std::ops::RangeInclusive;

use super::command::CommandContext;
use super::document::{Document, last_line, line_of, start_of_line};
use super::editor::{Editor, ViewId};
use super::view::Mode;

// ========================================================================
// Constants
// ========================================================================

/// The marker shown after the first line of a closed fold, before the
/// count of lines it hides.
pub(crate) const FOLD_MARKER: &str = " \u{22ef} ";

// ========================================================================
// Data Structures
// ========================================================================

/// The folds of a view.
#[derive(Clone, Debug, Default)]
pub(crate) struct Folds {
    /// The folds, in the order they were made.
    folds: Vec<Fold>,
}

/// A fold: its first and last lines, as char offsets, and whether it is
/// closed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Fold {
    /// Whether the view shows its first line alone.
    closed: bool,
    /// The start of its first line.
    first: usize,
    /// The start of its last line.
    last: usize,
    /// Whether it was made with `zf`, so opening it keeps it; a fold made
    /// to close goes when it opens.
    manual: bool,
}

// ========================================================================
// Folds Implementations
// ========================================================================

impl Folds {
    /// No folds.
    pub(crate) const fn new() -> Self {
        Self { folds: Vec::new() }
    }

    /// The closed fold whose hidden lines hold `line`, or whose first line
    /// it is: the outermost, as lines.
    pub(crate) fn closed_at(
        &self,
        document: &Document,
        line: usize,
    ) -> Option<RangeInclusive<usize>> {
        self.folds
            .iter()
            .filter(|fold| fold.closed)
            .map(|fold| fold.lines(document))
            .filter(|lines| lines.start() < lines.end() && lines.contains(&line))
            .max_by_key(|lines| lines.end() - lines.start())
    }

    /// Whether the view has no folds.
    pub(crate) fn is_empty(&self) -> bool {
        self.folds.is_empty()
    }

    /// Carries the folds through an edit, `map` taking each old offset to
    /// its new one.
    pub(crate) fn map(&mut self, map: impl Fn(usize) -> usize) {
        for fold in &mut self.folds {
            fold.first = map(fold.first);
            fold.last = map(fold.last);
        }
    }

    /// The line after `line` that the view shows: past the closed fold
    /// `line` starts.
    pub(crate) fn next_line(&self, document: &Document, line: usize) -> usize {
        match self.closed_at(document, line) {
            Some(lines) => lines.end() + 1,
            None => line + 1,
        }
    }

    /// The line before `line` that the view shows: the first line of the
    /// closed fold the line before it is in; `None` at the first line.
    pub(crate) fn previous_line(&self, document: &Document, line: usize) -> Option<usize> {
        let before = line.checked_sub(1)?;
        Some(
            self.closed_at(document, before)
                .map_or(before, |lines| *lines.start()),
        )
    }

    /// The innermost fold holding `line`, as an index.
    fn innermost(&self, document: &Document, line: usize) -> Option<usize> {
        self.folds
            .iter()
            .enumerate()
            .map(|(index, fold)| (index, fold.lines(document)))
            .filter(|(_, lines)| lines.start() < lines.end() && lines.contains(&line))
            .min_by_key(|(_, lines)| lines.end() - lines.start())
            .map(|(index, _)| index)
    }
}

// ========================================================================
// Fold Implementations
// ========================================================================

impl Fold {
    /// A fold of the lines `lines` of `document`.
    fn new(document: &Document, lines: RangeInclusive<usize>, closed: bool, manual: bool) -> Self {
        Self {
            closed,
            first: start_of_line(document, *lines.start()),
            last: start_of_line(document, *lines.end()),
            manual,
        }
    }

    /// The fold's first and last lines.
    fn lines(&self, document: &Document) -> RangeInclusive<usize> {
        line_of(document, self.first)..=line_of(document, self.last)
    }
}

// ========================================================================
// Functions
// ========================================================================

/// `zc`: closes the innermost fold at the cursor's line, making one as
/// the module docs say when there is none.
pub(crate) fn close_fold(cx: &mut CommandContext<'_>) {
    cx.editor.act_once();
    let line = cursor_line(cx.editor, cx.view);
    let document = cx.editor.document_of(cx.view);
    let open = cx.editor.view_ref(cx.view).folds().folds.clone();
    let reopened = open
        .iter()
        .enumerate()
        .filter(|(_, fold)| !fold.closed)
        .map(|(index, fold)| (index, fold.lines(document)))
        .filter(|(_, lines)| lines.start() < lines.end() && lines.contains(&line))
        .min_by_key(|(_, lines)| lines.end() - lines.start())
        .map(|(index, _)| index);
    if let Some(index) = reopened {
        cx.editor.view_mut(cx.view).folds_mut().folds[index].closed = true;
        return go_to_line(cx.editor, cx.view, line);
    }
    let ranges = cx.editor.fold_ranges_of(cx.view);
    let Some(lines) = fold_candidate(document, &ranges, line, cx.editor.view_ref(cx.view).folds())
    else {
        return cx.editor.error("No fold found".to_string());
    };
    let fold = Fold::new(document, lines, true, false);
    cx.editor.view_mut(cx.view).folds_mut().folds.push(fold);
    go_to_line(cx.editor, cx.view, line);
}

/// `zM`: closes every fold, and folds every block of the document not in
/// one yet, outermost first.
pub(crate) fn close_all_folds(cx: &mut CommandContext<'_>) {
    cx.editor.act_once();
    let ranges = cx.editor.fold_ranges_of(cx.view);
    let document = cx.editor.document_of(cx.view);
    let mut made = Vec::new();
    let mut line = 0;
    while line < document.len_lines() {
        let starts_here = block_at(document, &ranges, line).filter(|lines| *lines.start() == line);
        match starts_here {
            Some(lines) => {
                made.push(Fold::new(document, lines.clone(), true, false));
                line = lines.end() + 1;
            }
            None => line += 1,
        }
    }
    let folds = cx.editor.view_mut(cx.view).folds_mut();
    for fold in &mut folds.folds {
        fold.closed = true;
    }
    folds.folds.extend(made);
    let line = cursor_line(cx.editor, cx.view);
    go_to_line(cx.editor, cx.view, line);
}

/// `zE`: deletes every fold of the view.
pub(crate) fn delete_all_folds(cx: &mut CommandContext<'_>) {
    cx.editor.act_once();
    cx.editor.view_mut(cx.view).folds_mut().folds.clear();
}

/// `zd`: deletes the innermost fold at the cursor's line.
pub(crate) fn delete_fold(cx: &mut CommandContext<'_>) {
    cx.editor.act_once();
    let line = cursor_line(cx.editor, cx.view);
    let document = cx.editor.document_of(cx.view);
    match cx
        .editor
        .view_ref(cx.view)
        .folds()
        .innermost(document, line)
    {
        Some(index) => {
            cx.editor.view_mut(cx.view).folds_mut().folds.remove(index);
        }
        None => cx.editor.error("No fold found".to_string()),
    }
}

/// `zf` in a visual mode: folds the selected lines, closed, and goes back
/// to Normal mode on the fold's first line.
pub(crate) fn fold_selection(cx: &mut CommandContext<'_>) {
    cx.editor.act_once();
    let state = cx.editor.view_ref(cx.view);
    let anchor = state.visual_anchor().unwrap_or_else(|| state.cursor());
    let cursor = state.cursor();
    let document = cx.editor.document_of(cx.view);
    let first = line_of(document, anchor.min(cursor));
    let last = line_of(document, anchor.max(cursor));
    if first < last {
        let fold = Fold::new(document, first..=last, true, true);
        cx.editor.view_mut(cx.view).folds_mut().folds.push(fold);
    }
    if cx.editor.view_ref(cx.view).mode() != Mode::Insert {
        cx.editor.set_mode(cx.view, Mode::Normal);
    }
    let start = start_of_line(cx.editor.document_of(cx.view), first);
    cx.editor.set_cursor(cx.view, start);
}

/// `zR`: opens every fold.
pub(crate) fn open_all_folds(cx: &mut CommandContext<'_>) {
    cx.editor.act_once();
    let folds = cx.editor.view_mut(cx.view).folds_mut();
    folds.folds.retain(|fold| fold.manual);
    for fold in &mut folds.folds {
        fold.closed = false;
    }
}

/// `zo`: opens the closed folds at the cursor's line.
pub(crate) fn open_fold(cx: &mut CommandContext<'_>) {
    cx.editor.act_once();
    let line = cursor_line(cx.editor, cx.view);
    open_folds_at(cx.editor, cx.view, line);
}

/// After a command ran in `view`, opens the closed folds the cursor is
/// inside, past their first line, where a motion other than `j` and `k`
/// took it.
pub(crate) fn reveal_cursor(editor: &mut Editor, view: ViewId) {
    if editor.view_ref(view).folds().is_empty() {
        return;
    }
    let line = cursor_line(editor, view);
    let document = editor.document_of(view);
    let hides = editor
        .view_ref(view)
        .folds()
        .closed_at(document, line)
        .is_some_and(|lines| *lines.start() != line);
    if hides {
        open_folds_at(editor, view, line);
    }
}

/// The line `count` lines from `from` down (`down`) or up that `view`
/// shows, a closed fold counting as one line, stopping at the document's
/// edge.
pub(crate) fn step_lines(
    editor: &Editor,
    view: ViewId,
    from: usize,
    count: usize,
    down: bool,
) -> usize {
    let document = editor.document_of(view);
    let folds = editor.view_ref(view).folds();
    let last = last_line(document);
    let mut line = from;
    for _ in 0..count {
        let next = match down {
            true => Some(folds.next_line(document, line)).filter(|next| *next <= last),
            false => folds.previous_line(document, line),
        };
        match next {
            Some(next) => line = next,
            None => break,
        }
    }
    line
}

/// `za`: closes the fold at the cursor's line when none there is closed,
/// else opens it.
pub(crate) fn toggle_fold(cx: &mut CommandContext<'_>) {
    let line = cursor_line(cx.editor, cx.view);
    let document = cx.editor.document_of(cx.view);
    match cx
        .editor
        .view_ref(cx.view)
        .folds()
        .closed_at(document, line)
    {
        Some(_) => open_fold(cx),
        None => close_fold(cx),
    }
}

/// The block the document has room to fold at `line` when the view folds
/// nothing there: the innermost of the host's `ranges` holding it, else
/// its indented block.
fn fold_candidate(
    document: &Document,
    ranges: &[RangeInclusive<usize>],
    line: usize,
    folds: &Folds,
) -> Option<RangeInclusive<usize>> {
    let lines = block_at(document, ranges, line)?;
    let taken = folds.folds.iter().any(|fold| fold.lines(document) == lines);
    (!taken).then_some(lines)
}

/// The innermost of `ranges` holding `line`, else the indented block it
/// opens or sits in, as the module docs say; `None` when there is none.
fn block_at(
    document: &Document,
    ranges: &[RangeInclusive<usize>],
    line: usize,
) -> Option<RangeInclusive<usize>> {
    let hosted = ranges
        .iter()
        .filter(|lines| lines.start() < lines.end() && lines.contains(&line))
        .min_by_key(|lines| lines.end() - lines.start())
        .cloned();
    if !ranges.is_empty() {
        return hosted;
    }
    indent_block_from(document, line).or_else(|| {
        let indent = indent_of(document, line)?;
        let opener = (0..line)
            .rev()
            .find(|above| indent_of(document, *above).is_some_and(|depth| depth < indent))?;
        indent_block_from(document, opener)
    })
}

/// The block line `line` opens: it and the lines after it indented more
/// deeply than it, blank lines among them included; `None` when the next
/// line with text is not indented more deeply.
fn indent_block_from(document: &Document, line: usize) -> Option<RangeInclusive<usize>> {
    let indent = indent_of(document, line)?;
    let mut last = None;
    for below in line + 1..document.len_lines() {
        match indent_of(document, below) {
            Some(depth) if depth > indent => last = Some(below),
            Some(_) => break,
            None => {}
        }
    }
    last.map(|last| line..=last)
}

/// The line `view`'s cursor is on.
fn cursor_line(editor: &Editor, view: ViewId) -> usize {
    line_of(editor.document_of(view), editor.view_ref(view).cursor())
}

/// Moves the cursor of `view` to the start of `line` when a fold closed
/// over it, so it rests on the fold's first line.
fn go_to_line(editor: &mut Editor, view: ViewId, line: usize) {
    let document = editor.document_of(view);
    let Some(lines) = editor.view_ref(view).folds().closed_at(document, line) else {
        return;
    };
    if *lines.start() != line {
        let start = start_of_line(document, *lines.start());
        editor.set_cursor(view, start);
    }
}

/// The indent of line `line` in columns, a tab counting as one; `None`
/// for a blank line.
fn indent_of(document: &Document, line: usize) -> Option<usize> {
    let text = document.line_text(line)?;
    let trimmed = text.trim_start_matches([' ', '\t']);
    (!trimmed.is_empty()).then(|| text.len() - trimmed.len())
}

/// Opens every closed fold holding `line` in `view`, dropping those made
/// only to close.
fn open_folds_at(editor: &mut Editor, view: ViewId, line: usize) {
    let document = editor.document_of(view);
    let holding: Vec<bool> = editor
        .view_ref(view)
        .folds()
        .folds
        .iter()
        .map(|fold| fold.lines(document).contains(&line))
        .collect();
    let folds = editor.view_mut(view).folds_mut();
    let mut index = 0;
    folds.folds.retain_mut(|fold| {
        let holds = holding[index];
        index += 1;
        if holds {
            fold.closed = false;
        }
        !holds || fold.manual
    });
}

// ========================================================================
// Tests
// ========================================================================

#[cfg(test)]
mod tests {
    use std::iter;

    use crate::options::{EditingStyle, EditorOptions};
    use crate::paint::Surface;
    use crate::render::render_view;
    use crate::testing::Harness;

    /// The rows the view of `harness` paints, their trailing blanks cut,
    /// down to the last with text.
    fn rows(harness: &Harness) -> Vec<String> {
        let mut surface = Surface::new(0, 0);
        render_view(&harness.editor, harness.view, &mut surface);
        let mut rows: Vec<String> = (0..surface.rows())
            .map(|y| surface.row_text(y).trim_end().to_string())
            .collect();
        while rows.last().is_some_and(String::is_empty) {
            rows.pop();
        }
        rows
    }

    #[test]
    fn test_zc_folds_the_indented_block_and_j_and_k_step_over_it() {
        let mut harness = Harness::new("|fn a() {\n    one\n    two\n}\nend");
        harness.keys("z c");
        assert_eq!(rows(&harness), ["fn a() { \u{22ef} 2 lines", "}", "end"]);
        harness.keys("j");
        assert_eq!(harness.marked(), "fn a() {\n    one\n    two\n|}\nend");
        harness.keys("k z o");
        assert_eq!(rows(&harness).len(), 5, "open again");
    }

    #[test]
    fn test_zc_inside_a_block_folds_the_block_and_a_search_into_it_opens_it() {
        let mut harness = Harness::new("fn a() {\n    o|ne\n    two\n}");
        harness.keys("z c");
        assert_eq!(harness.marked(), "|fn a() {\n    one\n    two\n}");
        assert_eq!(rows(&harness), ["fn a() { \u{22ef} 2 lines", "}"]);
        harness.keys("/ two <cr>");
        assert_eq!(rows(&harness).len(), 4, "the search opened it");
    }

    #[test]
    fn test_zm_folds_every_block_and_zr_opens_them() {
        let mut harness = Harness::new("|fn a() {\n    one\n}\nfn b() {\n    two\n}");
        harness.keys("z M");
        let folded = [
            "fn a() { \u{22ef} 1 line",
            "}",
            "fn b() { \u{22ef} 1 line",
            "}",
        ];
        assert_eq!(rows(&harness), folded);
        harness.keys("z R");
        assert_eq!(rows(&harness).len(), 6);
    }

    #[test]
    fn test_zf_folds_the_selection_and_zd_deletes_the_fold() {
        let mut harness = Harness::new("|a\nb\nc\nd");
        harness.keys("V j j z f");
        assert_eq!(rows(&harness), ["a \u{22ef} 2 lines", "d"]);
        harness.keys("z o");
        assert_eq!(rows(&harness).len(), 4, "a fold made with zf stays open");
        harness.keys("z c");
        assert_eq!(rows(&harness).len(), 2, "and closes again");
        harness.keys("z d");
        assert_eq!(rows(&harness).len(), 4);
    }

    #[test]
    fn test_an_edit_above_a_fold_carries_it_along() {
        let mut harness = Harness::new("x\nfn a() {\n|    one\n}");
        harness.keys("z c g g O new <esc>");
        assert_eq!(
            rows(&harness),
            ["new", "x", "fn a() { \u{22ef} 1 line", "}"]
        );
    }

    #[test]
    fn test_the_hosts_ranges_fold_before_the_indent() {
        let mut harness = Harness::new("a\n|b\nc\nd\ne");
        let document = harness.editor.view(harness.view).expect("open").document();
        harness
            .editor
            .set_fold_ranges(document, iter::once(0..=3).collect());
        harness.keys("z c");
        assert_eq!(rows(&harness), ["a \u{22ef} 3 lines", "e"]);
    }

    #[test]
    fn test_ctrl_braces_fold_and_unfold_in_the_standard_style() {
        let options = EditorOptions {
            editing_style: EditingStyle::Standard,
            ..EditorOptions::default()
        };
        let mut harness = Harness::with_options("|if x:\n    y\nz", options);
        harness.keys("C-{");
        assert_eq!(rows(&harness), ["if x: \u{22ef} 1 line", "z"]);
        harness.keys("C-}");
        assert_eq!(rows(&harness).len(), 3);
    }
}
