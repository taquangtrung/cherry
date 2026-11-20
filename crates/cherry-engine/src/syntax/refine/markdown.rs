//! Markdown beyond VS Code's grammar. The grammar reads front matter and
//! math itself; outside code, this puts:
//!
//! - A wiki link, `[[page]]` or `[[page|label]]`, in a link's scope.
//! - A highlight, `==text==`, in a highlight's.
//! - A callout's label, the `[!NOTE]` opening a quote (GitHub's alerts,
//!   Obsidian's callouts), in a keyword's.
//! - Front matter's fences in a comment's, which the grammar scopes as
//!   punctuation a theme leaves in the text's color.

use super::{Piece, overlay, within};

// ========================================================================
// Constants
// ========================================================================

/// The scope a Markdown file's every line sits in.
pub(super) const MARKDOWN: &str = "text.html.markdown";

/// The scopes of Markdown's code and math, inline and in blocks, whose
/// text is not prose.
pub(super) const CODE: [&str; 4] = [
    "markup.inline.raw",
    "markup.fenced_code",
    "markup.math",
    "meta.embedded",
];

/// The scopes front matter's fences sit in.
const FENCES: [&str; 2] = [
    "punctuation.definition.begin.frontmatter",
    "punctuation.definition.end.frontmatter",
];

/// The scopes the marks the grammar misses are put in.
const FENCE: &str = "comment.block.frontmatter.markdown";
const WIKI_LINK: &str = "markup.underline.link.wiki.markdown";
const HIGHLIGHT: &str = "markup.highlight.markdown";
const CALLOUT: &str = "keyword.other.callout.markdown";

// ========================================================================
// Functions
// ========================================================================

/// The overlays of the marks inside `line` of a Markdown file, from `runs`,
/// the scopes the grammar gave its bytes.
pub(super) fn overlays<'a>(line: &str, runs: &[Piece<'a>]) -> Vec<Piece<'a>> {
    let mut overlays = Vec::new();
    for run in runs {
        if run.scopes.last().is_some_and(|top| FENCES.contains(top)) {
            overlays.push(overlay(run.range.clone(), &run.scopes, &[FENCE]));
        }
    }
    let prose = |at: usize| {
        runs.iter()
            .find(|run| run.range.contains(&at))
            .filter(|run| !within(&run.scopes, &CODE))
    };
    for (start, end, scope) in paired(line, "[[", "]]", WIKI_LINK)
        .chain(paired(line, "==", "==", HIGHLIGHT))
        .chain(callout(line))
    {
        if let Some(run) = prose(start) {
            overlays.push(overlay(start..end, &run.scopes, &[scope]));
        }
    }
    overlays
}

/// Each span of `line` from `open` to the next `close`, both included,
/// with something between that does not start or end in a blank, tagged
/// with `scope`.
fn paired<'a>(
    line: &'a str,
    open: &'static str,
    close: &'static str,
    scope: &'static str,
) -> impl Iterator<Item = (usize, usize, &'static str)> + 'a {
    let mut from = 0;
    std::iter::from_fn(move || {
        while let Some(offset) = line.get(from..)?.find(open) {
            let start = from + offset;
            let inner = start + open.len();
            let length = line[inner..].find(close)?;
            let text = &line[inner..inner + length];
            let end = inner + length + close.len();
            if !text.is_empty() && text.trim() == text {
                from = end;
                return Some((start, end, scope));
            }
            from = inner;
        }
        None
    })
}

/// The label opening `line`'s quote, if it is a callout's: `>` and blanks,
/// then `[!`, a word, and `]`.
fn callout(line: &str) -> Option<(usize, usize, &'static str)> {
    let quote = line.len() - line.trim_start().len();
    let rest = line[quote..].strip_prefix('>')?;
    let start = line.len() - rest.trim_start().len();
    let label = line[start..].strip_prefix("[!")?;
    let word = label.find(']')?;
    let is_word = word > 0
        && label[..word]
            .chars()
            .all(|c| c.is_alphanumeric() || c == '-');
    is_word.then_some((start, start + 2 + word + 1, CALLOUT))
}

// ========================================================================
// Tests
// ========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_wiki_links_highlights_and_callout_labels_are_found() {
        let spans = |line| paired(line, "[[", "]]", WIKI_LINK).collect::<Vec<_>>();
        assert_eq!(spans("see [[Page|label]] x"), [(4, 18, WIKI_LINK)]);
        assert!(spans("[[ ]]").is_empty(), "nothing in it");
        let marks: Vec<_> = paired("a ==b== c == d", "==", "==", HIGHLIGHT).collect();
        assert_eq!(marks, [(2, 7, HIGHLIGHT)]);
        assert_eq!(callout("> [!NOTE] Title"), Some((2, 9, CALLOUT)));
        assert_eq!(callout("> [!note]"), Some((2, 9, CALLOUT)));
        assert_eq!(callout("[!NOTE]"), None, "only opening a quote");
    }

    #[test]
    fn test_a_mark_in_code_is_left_alone() {
        let line = "`[[x]]` and [[y]]";
        let runs = [
            Piece {
                range: 0..7,
                scopes: vec![MARKDOWN, "markup.inline.raw.string.markdown"],
            },
            Piece {
                range: 7..line.len(),
                scopes: vec![MARKDOWN, "meta.paragraph.markdown"],
            },
        ];
        let found: Vec<_> = overlays(line, &runs)
            .into_iter()
            .map(|piece| &line[piece.range])
            .collect();
        assert_eq!(found, ["[[y]]"]);
    }
}
