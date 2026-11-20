//! Git's messages: a commit summary's conventional type, the way `git log`
//! readers and the forges pick it out, and git's `#` comments as one
//! comment each.

use std::ops::Range;

use super::{Piece, names, push};

// ========================================================================
// Constants
// ========================================================================

/// The scope a commit message's summary sits in.
const SUBJECT: &str = "meta.scope.subject.git-commit";

/// The scope a summary's conventional type (`feat:`, `fix(ui)!:`) is put
/// in, which the grammar leaves as part of the summary.
const TYPE: &str = "keyword.other.type.git-commit";

/// The scopes git's `#` comment lines sit in, in a commit message and in a
/// rebase plan, which the grammars fill with scopes of their own (each
/// changed file and how it changed) a theme would color as code.
const COMMENTS: [&str; 2] = [
    "comment.line.number-sign.git-commit",
    "comment.line.number-sign.git-rebase",
];

// ========================================================================
// Functions
// ========================================================================

/// Puts the bytes `range` of `line`, which sit in `stack`, after `pieces`:
/// a summary's conventional type, at the line's start, apart in its own
/// scope; a git comment's text in the comment's scope alone, whatever the
/// grammar picked out inside it; anything else as it is.
pub(super) fn split<'a>(
    pieces: &mut Vec<Piece<'a>>,
    line: &str,
    range: Range<usize>,
    stack: &[&'a str],
) {
    if range.is_empty() {
        return;
    }
    let top = stack.last().copied().unwrap_or_default();
    if range.start == 0
        && names(SUBJECT, top)
        && let Some(end) = conventional_type_end(&line[range.clone()])
    {
        let mut typed = stack.to_vec();
        typed.push(TYPE);
        push(pieces, range.start..range.start + end, typed);
        push(pieces, range.start + end..range.end, stack.to_vec());
        return;
    }
    if let Some(comment) = stack.iter().position(|scope| COMMENTS.contains(scope)) {
        push(pieces, range, stack[..=comment].to_vec());
        return;
    }
    push(pieces, range, stack.to_vec());
}

/// Where a summary's conventional type ends, its colon included: a word, an
/// optional `(scope)`, an optional `!`, then `:`.
fn conventional_type_end(text: &str) -> Option<usize> {
    let word = text
        .find(|c: char| !c.is_ascii_alphabetic())
        .filter(|end| *end > 0)?;
    let mut end = word;
    if text[end..].starts_with('(') {
        end += text[end..].find(')')? + 1;
    }
    if text[end..].starts_with('!') {
        end += 1;
    }
    text[end..].starts_with(':').then_some(end + 1)
}

// ========================================================================
// Tests
// ========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_a_conventional_type_ends_at_its_colon() {
        assert_eq!(conventional_type_end("feat: x"), Some(5));
        assert_eq!(conventional_type_end("fix(ui)!: x"), Some(9));
        assert_eq!(conventional_type_end("Merge branch"), None);
        assert_eq!(conventional_type_end(": x"), None);
    }

    #[test]
    fn test_a_git_comment_is_one_comment() {
        let mut pieces = Vec::new();
        let stack = [
            "text.git-commit",
            "comment.line.number-sign.git-commit",
            "meta.diff.git-commit",
        ];
        split(&mut pieces, "#\tmodified: a.rs", 0..15, &stack);
        assert_eq!(pieces[0].scopes, &stack[..2]);
    }
}
