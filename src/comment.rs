//! Comment toggling: `gc` is an operator that comments or uncomments the
//! lines a motion covers (`gcc` the cursor's line and the count less one
//! below, `gcip` a paragraph, `gc` in a visual mode the selected lines).
//!
//! # Toggling
//!
//! When every non-blank line of the lines is commented already, each loses
//! its comment token and the one space after it, if any; otherwise each
//! non-blank line gets the token and a space, all at the indent of the
//! least indented line, so the comment tokens line up. Blank lines are
//! left alone. The cursor goes to the first non-blank of the first line.
//!
//! # Comment tokens
//!
//! Each document has its own line comment token (`//`, `#`, `--`), which
//! the host sets with
//! [`Editor::set_comment_token`](crate::Editor::set_comment_token), often
//! from [`comment_token_for_extension`] and the file name it opened.
//!
//! A language with no line comments (CSS, HTML, XML) has block comment
//! tokens instead, an opener and a closer (`/*` and `*/`, `<!--` and
//! `-->`), set with
//! [`Editor::set_block_comment_tokens`](crate::Editor::set_block_comment_tokens)
//! and known by [`block_comment_tokens_for_extension`]. With no line token
//! set, `gc` uses them, wrapping each non-blank line on its own (`/* a */`)
//! at the shared indent, and unwrapping when every non-blank line is
//! wrapped already. A document with neither gives a notice instead.

use super::document::start_of_line;
use super::editor::{Editor, ViewId};
use super::motion::first_non_blank_of_line;
use super::transaction::Edit;

// ========================================================================
// Constants
// ========================================================================

/// The line comment token of each file extension the editor knows.
const COMMENT_TOKENS: &[(&str, &str)] = &[
    ("bash", "#"),
    ("c", "//"),
    ("cc", "//"),
    ("clj", ";;"),
    ("cpp", "//"),
    ("cs", "//"),
    ("dart", "//"),
    ("el", ";;"),
    ("elm", "--"),
    ("erl", "%"),
    ("go", "//"),
    ("h", "//"),
    ("hpp", "//"),
    ("hs", "--"),
    ("java", "//"),
    ("js", "//"),
    ("jsx", "//"),
    ("kt", "//"),
    ("lisp", ";;"),
    ("lua", "--"),
    ("nix", "#"),
    ("php", "//"),
    ("pl", "#"),
    ("py", "#"),
    ("r", "#"),
    ("rb", "#"),
    ("rs", "//"),
    ("scala", "//"),
    ("scm", ";;"),
    ("sh", "#"),
    ("sql", "--"),
    ("swift", "//"),
    ("tex", "%"),
    ("toml", "#"),
    ("ts", "//"),
    ("tsx", "//"),
    ("vim", "\""),
    ("yaml", "#"),
    ("yml", "#"),
    ("zig", "//"),
    ("zsh", "#"),
];

/// The block comment tokens, opener and closer, of each file extension the
/// editor knows that has no line comments.
const BLOCK_COMMENT_TOKENS: &[(&str, (&str, &str))] = &[
    ("css", ("/*", "*/")),
    ("htm", ("<!--", "-->")),
    ("html", ("<!--", "-->")),
    ("less", ("/*", "*/")),
    ("markdown", ("<!--", "-->")),
    ("md", ("<!--", "-->")),
    ("svg", ("<!--", "-->")),
    ("vue", ("<!--", "-->")),
    ("xhtml", ("<!--", "-->")),
    ("xml", ("<!--", "-->")),
];

/// What separates a comment token from the text it comments.
const SEPARATOR: char = ' ';

/// The notice for a document with no comment token.
const NO_TOKEN: &str = "No comment token for this document";

// ========================================================================
// Functions
// ========================================================================

/// The line comment token of files with `extension` (`"rs"`, `"py"`,
/// without the dot, any case), from a small built-in table; `None` for an
/// extension it does not know.
///
/// ```
/// assert_eq!(cherry::comment_token_for_extension("rs"), Some("//"));
/// assert_eq!(cherry::comment_token_for_extension("PY"), Some("#"));
/// assert_eq!(cherry::comment_token_for_extension("unknown"), None);
/// ```
pub fn comment_token_for_extension(extension: &str) -> Option<&'static str> {
    let extension = extension.to_ascii_lowercase();
    COMMENT_TOKENS
        .binary_search_by(|(known, _)| known.cmp(&extension.as_str()))
        .ok()
        .map(|index| COMMENT_TOKENS[index].1)
}

/// The block comment tokens, opener and closer, of files with `extension`
/// (without the dot, any case) whose language has no line comments, from a
/// small built-in table; `None` for an extension it does not know.
///
/// ```
/// assert_eq!(cherry::block_comment_tokens_for_extension("css"), Some(("/*", "*/")));
/// assert_eq!(cherry::block_comment_tokens_for_extension("HTML"), Some(("<!--", "-->")));
/// assert_eq!(cherry::block_comment_tokens_for_extension("rs"), None);
/// ```
pub fn block_comment_tokens_for_extension(extension: &str) -> Option<(&'static str, &'static str)> {
    let extension = extension.to_ascii_lowercase();
    BLOCK_COMMENT_TOKENS
        .binary_search_by(|(known, _)| known.cmp(&extension.as_str()))
        .ok()
        .map(|index| BLOCK_COMMENT_TOKENS[index].1)
}

/// Comments or uncomments the lines `first` to `last` of the document of
/// `view`, as the module documentation describes, as one undo step.
pub(crate) fn toggle_comment(editor: &mut Editor, view: ViewId, first: usize, last: usize) {
    let Some(token) = editor.comment_token_of(view).map(str::to_string) else {
        let Some((open, close)) = editor
            .block_comment_tokens_of(view)
            .map(|(open, close)| (open.to_string(), close.to_string()))
        else {
            return editor.error(NO_TOKEN.to_string());
        };
        return toggle_block_comment(editor, view, (first, last), (&open, &close));
    };
    let document = editor.document_of(view);
    let lines: Vec<(usize, String)> = (first..=last)
        .filter_map(|line| Some((line, document.line_text(line)?)))
        .filter(|(_, text)| !text.trim().is_empty())
        .collect();
    let indent_len = |text: &str| text.chars().take_while(|c| matches!(c, ' ' | '\t')).count();
    let is_commented = |text: &str| text.trim_start_matches([' ', '\t']).starts_with(&token);
    let uncomment = !lines.is_empty() && lines.iter().all(|(_, text)| is_commented(text));
    let shared_indent = lines
        .iter()
        .map(|(_, text)| indent_len(text))
        .min()
        .unwrap_or(0);
    let token_len = token.chars().count();
    let edits = lines
        .iter()
        .map(|(line, text)| {
            let start = start_of_line(document, *line);
            if uncomment {
                let at = start + indent_len(text);
                let after_token = text.chars().nth(indent_len(text) + token_len);
                let len = token_len + usize::from(after_token == Some(SEPARATOR));
                Edit::new(at..at + len, "")
            } else {
                let at = start + shared_indent;
                Edit::new(at..at, format!("{token}{SEPARATOR}"))
            }
        })
        .collect();
    editor.apply_edits(view, edits);
    let line_start = start_of_line(editor.document_of(view), first);
    let target = first_non_blank_of_line(editor, view, line_start);
    editor.set_cursor(view, target);
}

/// Wraps or unwraps each non-blank line of `first` to `last` in the block
/// comment tokens `open` and `close`, as the module documentation
/// describes, as one undo step.
fn toggle_block_comment(
    editor: &mut Editor,
    view: ViewId,
    (first, last): (usize, usize),
    (open, close): (&str, &str),
) {
    let document = editor.document_of(view);
    let lines: Vec<(usize, String)> = (first..=last)
        .filter_map(|line| Some((line, document.line_text(line)?)))
        .filter(|(_, text)| !text.trim().is_empty())
        .collect();
    let indent_len = |text: &str| text.chars().take_while(|c| matches!(c, ' ' | '\t')).count();
    let is_wrapped = |text: &str| {
        let body = text.trim_matches([' ', '\t']);
        body.len() >= open.len() + close.len() && body.starts_with(open) && body.ends_with(close)
    };
    let unwrap = !lines.is_empty() && lines.iter().all(|(_, text)| is_wrapped(text));
    let shared_indent = lines
        .iter()
        .map(|(_, text)| indent_len(text))
        .min()
        .unwrap_or(0);
    let (open_len, close_len) = (open.chars().count(), close.chars().count());
    let mut edits = Vec::new();
    for (line, text) in &lines {
        let start = start_of_line(document, *line);
        let chars: Vec<char> = text.chars().collect();
        if unwrap {
            let body_start = indent_len(text);
            let body_end = chars.len()
                - chars
                    .iter()
                    .rev()
                    .take_while(|c| matches!(c, ' ' | '\t'))
                    .count();
            let after_open = body_start + open_len;
            let open_end = after_open + usize::from(chars.get(after_open) == Some(&SEPARATOR));
            let before_close = body_end - close_len;
            let close_start = before_close
                - usize::from(
                    before_close > open_end && chars.get(before_close - 1) == Some(&SEPARATOR),
                );
            edits.push(Edit::new(start + body_start..start + open_end, ""));
            edits.push(Edit::new(start + close_start..start + body_end, ""));
        } else {
            let at = start + shared_indent;
            let end = start + chars.len();
            edits.push(Edit::new(at..at, format!("{open}{SEPARATOR}")));
            edits.push(Edit::new(end..end, format!("{SEPARATOR}{close}")));
        }
    }
    editor.apply_edits(view, edits);
    let line_start = start_of_line(editor.document_of(view), first);
    let target = first_non_blank_of_line(editor, view, line_start);
    editor.set_cursor(view, target);
}

// ========================================================================
// Tests
// ========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::request::{HostRequest, NoticeLevel, NoticeRequest};
    use crate::testing::Harness;

    /// A harness editing `marked` with `//` comments.
    fn harness(marked: &str) -> Harness {
        let mut harness = Harness::new(marked);
        let document = harness.editor.view(harness.view).unwrap().document();
        harness.editor.set_comment_token(document, Some("//"));
        harness
    }

    #[track_caller]
    fn assert_comment_keys(before: &str, keys: &str, after: &str) {
        let mut harness = harness(before);
        harness.keys(keys);
        assert_eq!(harness.marked(), after, "{before:?} after {keys:?}");
    }

    #[test]
    fn test_the_extension_tables_are_sorted_for_their_binary_search() {
        assert!(COMMENT_TOKENS.windows(2).all(|pair| pair[0].0 < pair[1].0));
        assert!(
            BLOCK_COMMENT_TOKENS
                .windows(2)
                .all(|pair| pair[0].0 < pair[1].0)
        );
    }

    #[test]
    fn test_gc_wraps_each_line_in_block_tokens_when_there_is_no_line_token() {
        let block = |marked: &str, keys: &str| {
            let mut harness = Harness::new(marked);
            let document = harness.editor.view(harness.view).unwrap().document();
            harness
                .editor
                .set_block_comment_tokens(document, Some(("<!--", "-->")));
            harness.keys(keys);
            harness.marked()
        };
        assert_eq!(
            block("  a|b\n\n c", "g c G"),
            " |<!--  ab -->\n\n <!-- c -->"
        );
        assert_eq!(block("  <!-- a|b -->\n<!--c-->", "g c j"), "  |ab\nc");
        assert_eq!(
            block("<!-- a -->\n|b", "g c k"),
            "|<!-- <!-- a --> -->\n<!-- b -->"
        );
        assert_eq!(block("|a", "g c c u"), "|a");
        let mut both = harness("|a");
        let document = both.editor.view(both.view).unwrap().document();
        both.editor
            .set_block_comment_tokens(document, Some(("/*", "*/")));
        both.keys("g c c");
        assert_eq!(both.marked(), "|// a", "a line token wins");
    }

    #[test]
    fn test_gcc_comments_a_line_at_its_indent_and_again_uncomments_it() {
        assert_comment_keys("  a|b\nc", "g c c", "  |// ab\nc");
        assert_comment_keys("  // a|b\nc", "g c c", "  |ab\nc");
        assert_comment_keys("  //a|b", "g c c", "  |ab");
        assert_comment_keys("|a\nb\nc", "2 g c c", "|// a\n// b\nc");
        assert_comment_keys("|a\nb\nc", "g c g c", "|// a\nb\nc");
    }

    #[test]
    fn test_gc_with_a_motion_lines_up_tokens_and_skips_blank_lines() {
        assert_comment_keys("|    a\n\n  b\nc", "g c 2 j", "  |//   a\n\n  // b\nc");
        assert_comment_keys("|// a\nb\n", "g c j", "|// // a\n// b\n");
        assert_comment_keys("|// a\n// b", "g c j", "|a\nb");
    }

    #[test]
    fn test_visual_gc_toggles_the_selected_lines_and_undo_restores_them() {
        assert_comment_keys("a\n|b\nc", "V j g c", "a\n|// b\n// c");
        assert_comment_keys("a\n|b\nc", "v j g c u", "a\n|b\nc");
    }

    #[test]
    fn test_gcc_repeats_with_dot_and_without_a_token_says_so() {
        assert_comment_keys("|a\nb", "g c c j .", "// a\n|// b");
        let mut harness = Harness::new("|a");
        harness.keys("g c c");
        assert_eq!(harness.marked(), "|a");
        let requests = harness.requests();
        let notice = NoticeRequest {
            level: NoticeLevel::Error,
            message: NO_TOKEN.to_string(),
        };
        assert_eq!(requests, [HostRequest::Notice(notice)]);
    }
}
