//! Reflowing text, as Vim's `gq`: an operator that refills the lines a
//! motion covers (`gqip` a paragraph, `gqq` the cursor's line, `gq` in a
//! visual mode the selected lines) so each holds as many words as fit in
//! [`EditorOptions::text_width`](crate::EditorOptions::text_width) cells.
//!
//! Blank lines split the lines into paragraphs, each refilled on its own
//! and the blank lines kept. A paragraph keeps the indent of its first
//! line, and a comment prefix its lines all share (the document's comment
//! token and the space after it, as `// `) goes on every line it makes. A
//! word longer than the width takes a line of its own. The cursor goes to
//! the first non-blank of the last line made.

use super::document::{last_line, line_end, start_of_line};
use super::editor::{Editor, ViewId};
use super::motion::first_non_blank_of_line;
use super::transaction::Edit;

// ========================================================================
// Functions
// ========================================================================

/// Refills lines `first` to `last` of the document of `view`, as the
/// module docs say, as one undo step.
pub(crate) fn reflow_lines(editor: &mut Editor, view: ViewId, first: usize, last: usize) {
    let document = editor.document_of(view);
    let last = last.min(last_line(document));
    let lines: Vec<String> = (first..=last)
        .map(|line| document.line_text(line).unwrap_or_default())
        .collect();
    let width = editor.options().text_width.max(1);
    let token = editor.comment_token_of(view).map(str::to_string);
    let mut made = Vec::new();
    let mut paragraph: Vec<&str> = Vec::new();
    for line in &lines {
        if line.trim().is_empty() {
            made.extend(refill(&paragraph, width, token.as_deref()));
            paragraph.clear();
            made.push(line.clone());
        } else {
            paragraph.push(line);
        }
    }
    made.extend(refill(&paragraph, width, token.as_deref()));
    let start = start_of_line(document, first);
    let end = line_end(document, start_of_line(document, last));
    let text = made.join("\n");
    if document.slice(start..end) != text {
        editor.apply_edits(view, vec![Edit::new(start..end, text)]);
    }
    let document = editor.document_of(view);
    let last_made = start_of_line(document, first + made.len().saturating_sub(1));
    let cursor = first_non_blank_of_line(editor, view, last_made);
    editor.set_cursor(view, cursor);
}

/// The lines `paragraph` refills into at `width` cells: its first line's
/// indent, then the comment prefix all its lines share, then the words.
fn refill(paragraph: &[&str], width: usize, token: Option<&str>) -> Vec<String> {
    let Some(first) = paragraph.first() else {
        return Vec::new();
    };
    let indent: String = first
        .chars()
        .take_while(|c| *c == ' ' || *c == '\t')
        .collect();
    let prefix = token.map(|token| format!("{token} ")).filter(|prefix| {
        paragraph
            .iter()
            .all(|line| line.trim_start().starts_with(prefix.trim_end()))
    });
    let lead = format!("{indent}{}", prefix.as_deref().unwrap_or_default());
    let words: Vec<&str> = paragraph
        .iter()
        .map(|line| {
            let text = line.trim_start();
            match &prefix {
                Some(prefix) => text.strip_prefix(prefix.trim_end()).unwrap_or(text),
                None => text,
            }
        })
        .flat_map(str::split_whitespace)
        .collect();
    let mut made: Vec<String> = Vec::new();
    let mut line = lead.clone();
    for word in words {
        let fits = line.chars().count() + 1 + word.chars().count() <= width;
        if line.len() > lead.len() && !fits {
            made.push(line);
            line = lead.clone();
        }
        if line.len() > lead.len() {
            line.push(' ');
        }
        line.push_str(word);
    }
    made.push(line);
    made
}

// ========================================================================
// Tests
// ========================================================================

#[cfg(test)]
mod tests {
    use crate::options::EditorOptions;
    use crate::testing::Harness;

    /// The text of `marked` after `keys`, with a text width of 12.
    fn reflowed(marked: &str, keys: &str) -> String {
        let options = EditorOptions {
            text_width: 12,
            ..EditorOptions::default()
        };
        let mut harness = Harness::with_options(marked, options);
        let document = harness.editor.view(harness.view).unwrap().document();
        harness.editor.set_comment_token(document, Some("//"));
        harness.keys(keys);
        harness.marked()
    }

    #[test]
    fn test_gq_refills_each_paragraph_to_the_width_keeping_indent_and_comments() {
        assert_eq!(
            reflowed("|one two three four\nfive\n\n  six seven eight", "g q G"),
            "one two\nthree four\nfive\n\n  six seven\n  |eight"
        );
        assert_eq!(
            reflowed("  // |alpha beta\n  // gamma delta", "g q j"),
            "  // alpha\n  // beta\n  // gamma\n  |// delta"
        );
        assert_eq!(
            reflowed("|a b\nc", "g q q"),
            "|a b\nc",
            "one line, already fitting"
        );
        assert_eq!(
            reflowed("|abcdefghijklmnop q", "g q q"),
            "abcdefghijklmnop\n|q"
        );
        assert_eq!(
            reflowed("|a\nb\nc", "V j g q"),
            "|a b\nc",
            "the selected lines"
        );
    }
}
