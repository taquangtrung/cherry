//! Writing Markdown, as Emacs's markdown-mode helps with it, as commands
//! [`register_markdown_commands`] adds to an editor (with no keys; bind
//! the ones wanted with [`Editor::bind`]):
//!
//! - `markdown-next-heading` and `markdown-previous-heading` move to the
//!   start of the next or previous heading, a count at a time.
//! - `markdown-promote` and `markdown-demote` take a `#` from or give one
//!   to the heading on the cursor's line, or take two blanks from or give
//!   them to the indent of the list item there.
//! - `markdown-toggle-task` ticks the task (`- [ ]`) on the cursor's line,
//!   or unticks it.
//! - `markdown-bold`, `markdown-italic`, and `markdown-code` wrap the word
//!   under the cursor in `**`, `_`, or backticks, or unwrap it.
//! - `markdown-renumber` numbers the ordered list the cursor is in again,
//!   from its first item's number, items nested deeper left as they are.

use std::ops::Range;

use crate::{CommandContext, CommandError, Document, Edit, Editor, Position};

use super::outline::{Format, outline};

// ========================================================================
// Constants
// ========================================================================

/// The deepest heading Markdown has.
const MAX_LEVEL: usize = 6;

/// What a list item is indented by for each level it is demoted.
const LIST_INDENT: &str = "  ";

/// A task's box, unticked and ticked.
const TASK_OPEN: &str = "[ ] ";
const TASK_DONE: &str = "[x] ";

/// What a command runs.
type Run = fn(&mut CommandContext<'_>);

/// Each command: its id and what it runs.
const COMMANDS: [(&str, Run); 9] = [
    ("markdown-bold", bold),
    ("markdown-code", code),
    ("markdown-demote", demote),
    ("markdown-italic", italic),
    ("markdown-next-heading", next_heading),
    ("markdown-previous-heading", previous_heading),
    ("markdown-promote", promote),
    ("markdown-renumber", renumber),
    ("markdown-toggle-task", toggle_task),
];

// ========================================================================
// Functions
// ========================================================================

/// Registers the Markdown commands the module docs list in `editor`, with
/// no keys. Fails when one of their ids is taken, as when called twice.
pub fn register_markdown_commands(editor: &mut Editor) -> Result<(), CommandError> {
    for (id, run) in COMMANDS {
        editor.register_command(id, run)?;
    }
    Ok(())
}

/// `markdown-next-heading`.
fn next_heading(cx: &mut CommandContext<'_>) {
    step_heading(cx, true);
}

/// `markdown-previous-heading`.
fn previous_heading(cx: &mut CommandContext<'_>) {
    step_heading(cx, false);
}

/// `markdown-promote`: a heading one level up, or a list item one less
/// indented.
fn promote(cx: &mut CommandContext<'_>) {
    edit_line(cx, |text| {
        let hashes = heading_level(text);
        if hashes > 1 {
            return Some(text[1..].to_string());
        }
        (list_marker_at(text).is_some() && text.starts_with(LIST_INDENT))
            .then(|| text[LIST_INDENT.len()..].to_string())
    });
}

/// `markdown-demote`: a heading one level down, or a list item one more
/// indented.
fn demote(cx: &mut CommandContext<'_>) {
    edit_line(cx, |text| {
        let hashes = heading_level(text);
        if hashes > 0 {
            return (hashes < MAX_LEVEL).then(|| format!("#{text}"));
        }
        list_marker_at(text).map(|_| format!("{LIST_INDENT}{text}"))
    });
}

/// `markdown-toggle-task`.
fn toggle_task(cx: &mut CommandContext<'_>) {
    edit_line(cx, |text| {
        let (_, marker_end) = list_marker_at(text)?;
        let rest = &text[marker_end..];
        let toggled = match (rest.strip_prefix(TASK_OPEN), rest.strip_prefix(TASK_DONE)) {
            (Some(after), _) => format!("{TASK_DONE}{after}"),
            (_, Some(after)) => format!("{TASK_OPEN}{after}"),
            (None, None) => return None,
        };
        Some(format!("{}{toggled}", &text[..marker_end]))
    });
}

/// `markdown-bold`.
fn bold(cx: &mut CommandContext<'_>) {
    wrap_word(cx, "**");
}

/// `markdown-italic`.
fn italic(cx: &mut CommandContext<'_>) {
    wrap_word(cx, "_");
}

/// `markdown-code`.
fn code(cx: &mut CommandContext<'_>) {
    wrap_word(cx, "`");
}

/// `markdown-renumber`: the ordered list around the cursor's line numbered
/// again from its first item's number.
fn renumber(cx: &mut CommandContext<'_>) {
    let Some((document, cursor)) = state(cx) else {
        return;
    };
    let line = position(document, cursor).line;
    let indent_of = |line: usize| {
        let text = document.line_text(line)?;
        let indent = text.len() - text.trim_start().len();
        ordered_number(text.trim_start()).map(|_| indent)
    };
    let Some(indent) = (0..=line).rev().find_map(indent_of) else {
        return;
    };
    let in_list = |line: usize| {
        document.line_text(line).is_some_and(|text| {
            let deeper = text.len() - text.trim_start().len() > indent;
            deeper || indent_of(line) == Some(indent)
        })
    };
    let mut first = line;
    while first > 0 && in_list(first - 1) {
        first -= 1;
    }
    let mut edits = Vec::new();
    let mut number = None;
    let mut at = first;
    while at < document.len_lines() && in_list(at) {
        let text = document.line_text(at).unwrap_or_default();
        if indent_of(at) == Some(indent) {
            let trimmed = text.trim_start();
            let (old, digits) = ordered_number(trimmed).unwrap_or((1, 0));
            let next = number.map_or(old, |number: usize| number + 1);
            number = Some(next);
            let start = line_start(document, at) + indent;
            edits.push(Edit::new(start..start + digits, next.to_string()));
        }
        at += 1;
    }
    cx.editor.apply_edits(cx.view, edits);
}

/// Moves the cursor to the start of the count-th heading after the cursor's
/// line, or before it.
fn step_heading(cx: &mut CommandContext<'_>, forward: bool) {
    let Some((document, cursor)) = state(cx) else {
        return;
    };
    let line = position(document, cursor).line;
    let text = document.slice(0..document.len_chars());
    let headings: Vec<usize> = outline(Format::Markdown, &text)
        .into_iter()
        .map(|entry| entry.line)
        .collect();
    let candidates: Vec<usize> = match forward {
        true => headings.into_iter().filter(|at| *at > line).collect(),
        false => headings.into_iter().rev().filter(|at| *at < line).collect(),
    };
    let count = cx.count_or_one();
    let Some(target) = candidates.get(count - 1).or(candidates.last()).copied() else {
        return;
    };
    let offset = line_start(document, target);
    cx.editor.set_cursor(cx.view, offset);
}

/// Replaces the cursor's line with what `change` makes of its text, when
/// it makes anything, the cursor kept as far from the line's end.
fn edit_line(cx: &mut CommandContext<'_>, change: impl Fn(&str) -> Option<String>) {
    let Some((document, cursor)) = state(cx) else {
        return;
    };
    let line = position(document, cursor).line;
    let text = document.line_text(line).unwrap_or_default();
    let Some(changed) = change(&text) else {
        return;
    };
    let start = line_start(document, line);
    let end = start + text.chars().count();
    let from_end = end - cursor;
    let new_end = start + changed.chars().count();
    cx.editor
        .apply_edits(cx.view, vec![Edit::new(start..end, changed)]);
    cx.editor
        .set_cursor(cx.view, new_end.saturating_sub(from_end).max(start));
}

/// Wraps the word under the cursor in `marker` on both sides, or unwraps
/// it when it is wrapped so already.
fn wrap_word(cx: &mut CommandContext<'_>, marker: &str) {
    let Some((document, cursor)) = state(cx) else {
        return;
    };
    let Some(word) = word_at(document, cursor) else {
        return;
    };
    let len = marker.chars().count();
    let before = word
        .start
        .checked_sub(len)
        .map(|start| document.slice(start..word.start));
    let after =
        (word.end + len <= document.len_chars()).then(|| document.slice(word.end..word.end + len));
    let wrapped = before.as_deref() == Some(marker) && after.as_deref() == Some(marker);
    let edits = match wrapped {
        true => vec![
            Edit::new(word.start - len..word.start, ""),
            Edit::new(word.end..word.end + len, ""),
        ],
        false => vec![
            Edit::new(word.start..word.start, marker),
            Edit::new(word.end..word.end, marker),
        ],
    };
    cx.editor.apply_edits(cx.view, edits);
}

/// The document of the view `cx` runs in and its cursor.
fn state<'a>(cx: &'a CommandContext<'_>) -> Option<(&'a Document, usize)> {
    let view = cx.editor.view(cx.view)?;
    Some((cx.editor.document(view.document())?, view.cursor()))
}

/// The chars of the word (letters, digits, underscores) at `offset`.
fn word_at(document: &Document, offset: usize) -> Option<Range<usize>> {
    let len = document.len_chars();
    let is_word = |at: usize| {
        at < len
            && document
                .slice(at..at + 1)
                .chars()
                .all(|c| c.is_alphanumeric() || c == '_')
    };
    if !is_word(offset) {
        return None;
    }
    let mut start = offset;
    while start > 0 && is_word(start - 1) {
        start -= 1;
    }
    let mut end = offset;
    while is_word(end) {
        end += 1;
    }
    Some(start..end)
}

/// How deep the heading `text` is, or 0 when it is none.
fn heading_level(text: &str) -> usize {
    let hashes = text.chars().take_while(|c| *c == '#').count();
    match (1..=MAX_LEVEL).contains(&hashes) && text[hashes..].starts_with(' ') {
        true => hashes,
        false => 0,
    }
}

/// Where the list marker of `text` (a line) starts and where its text
/// starts after it and its blank; `None` for a line that is no list item.
fn list_marker_at(text: &str) -> Option<(usize, usize)> {
    let trimmed = text.trim_start();
    let indent = text.len() - trimmed.len();
    let marker = match ordered_number(trimmed) {
        Some((_, digits)) => digits + 1,
        None if trimmed.starts_with(['-', '*', '+']) => 1,
        None => return None,
    };
    trimmed[marker..]
        .starts_with(' ')
        .then_some((indent, indent + marker + 1))
}

/// The number of the ordered list item `text` (past its indent) starts
/// with, and its digits; `None` when it starts with none.
fn ordered_number(text: &str) -> Option<(usize, usize)> {
    let digits = text.chars().take_while(char::is_ascii_digit).count();
    let closes = matches!(text[digits..].chars().next(), Some('.' | ')'));
    (digits > 0 && closes).then(|| (text[..digits].parse().unwrap_or(1), digits))
}

/// The char offset where line `line` of `document` starts.
fn line_start(document: &Document, line: usize) -> usize {
    document
        .position_to_char(Position { column: 0, line })
        .unwrap_or_else(|| document.len_chars())
}

/// The line and column of char `offset` of `document`.
fn position(document: &Document, offset: usize) -> Position {
    document.char_to_position(offset).unwrap_or_default()
}

// ========================================================================
// Tests
// ========================================================================

#[cfg(test)]
mod tests {
    use crate::{Editor, EditorOptions};

    use super::*;

    /// The text of `text` after running the command `id` with the cursor
    /// at char `cursor`, and where the cursor ends.
    fn ran(text: &str, cursor: usize, id: &str) -> (String, usize) {
        let mut editor = Editor::new(EditorOptions::default());
        register_markdown_commands(&mut editor).unwrap();
        let document = editor.open_document(text);
        let view = editor.new_view(document).unwrap();
        editor.set_cursor(view, cursor);
        editor.run_command(view, id, None).unwrap();
        let shown = editor.document(document).unwrap().to_string();
        (shown, editor.view(view).unwrap().cursor())
    }

    #[test]
    fn test_promote_demote_and_tasks_change_the_cursors_line() {
        assert_eq!(ran("## A", 3, "markdown-promote").0, "# A");
        assert_eq!(
            ran("# A", 0, "markdown-promote").0,
            "# A",
            "the top already"
        );
        assert_eq!(ran("- a", 2, "markdown-demote").0, "  - a");
        assert_eq!(ran("  1. a", 2, "markdown-promote").0, "1. a");
        assert_eq!(ran("- [ ] a", 6, "markdown-toggle-task").0, "- [x] a");
        assert_eq!(ran("- [x] a", 6, "markdown-toggle-task").0, "- [ ] a");
    }

    #[test]
    fn test_words_wrap_and_unwrap_and_lists_renumber() {
        assert_eq!(ran("a word b", 3, "markdown-bold").0, "a **word** b");
        assert_eq!(ran("a **word** b", 5, "markdown-bold").0, "a word b");
        assert_eq!(ran("x `y`", 3, "markdown-code").0, "x y");
        let list = "3. a\n7. b\n   - c\n1. d\ntext";
        assert_eq!(
            ran(list, 0, "markdown-renumber").0,
            "3. a\n4. b\n   - c\n5. d\ntext"
        );
    }

    #[test]
    fn test_headings_are_stepped_through_a_count_at_a_time() {
        let text = "# a\nx\n## b\ny\n# c";
        assert_eq!(ran(text, 0, "markdown-next-heading").1, 6);
        assert_eq!(ran(text, 14, "markdown-previous-heading").1, 6);
        let mut editor = Editor::new(EditorOptions::default());
        register_markdown_commands(&mut editor).unwrap();
        assert!(
            register_markdown_commands(&mut editor).is_err(),
            "the ids are taken"
        );
    }
}
