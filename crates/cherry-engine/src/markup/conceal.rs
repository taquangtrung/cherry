//! The markup markers of Markdown to conceal, for
//! [`Editor::set_conceals`](crate::Editor::set_conceals): the `**` and `__`
//! around strong text, the `*` and `_` around emphasis, the `~~` around
//! struck text, the backticks around code, a heading's `#`s and the blank
//! after them, and a link's `[` and `](url)` (an image's `![` too), so its
//! text shows alone; markers inside others hide too (`` [`code`](url) ``,
//! `**bold _it_**`).
//! Text inside code spans and fenced code blocks is left as it is, as is a
//! marker with no closer on its line (or in its link's text); a backslash
//! before a marker makes it the char itself.

use std::ops::Range;

// ========================================================================
// Constants
// ========================================================================

/// The fences that open and close a code block.
const FENCES: [&str; 2] = ["```", "~~~"];

/// The paired markers, longest first, so `**` is read before `*`.
const PAIRED: [&str; 6] = ["**", "__", "~~", "*", "_", "`"];

/// The marker around code, whose text is never marked up.
const CODE: &str = "`";

/// The char that makes the next one itself.
const ESCAPE: char = '\\';

// ========================================================================
// Functions
// ========================================================================

/// The char ranges of the markers of the Markdown `text` to conceal, as
/// the module docs say, in order.
pub fn markdown_conceals(text: &str) -> Vec<Range<usize>> {
    let mut conceals = Vec::new();
    let mut in_fence = false;
    let mut start = 0;
    for line in text.split('\n') {
        let chars: Vec<char> = line.chars().collect();
        let trimmed = line.trim_start();
        if FENCES.iter().any(|fence| trimmed.starts_with(fence)) {
            in_fence = !in_fence;
        } else if !in_fence {
            conceal_line(&chars, start, &mut conceals);
        }
        start += chars.len() + 1;
    }
    conceals
}

/// Adds the conceals of one line, `chars`, starting at char `start` of the
/// text.
fn conceal_line(chars: &[char], start: usize, conceals: &mut Vec<Range<usize>>) {
    let hashes = chars.iter().take_while(|c| **c == '#').count();
    let mut at = 0;
    if (1..=6).contains(&hashes) && chars.get(hashes) == Some(&' ') {
        conceals.push(start..start + hashes + 1);
        at = hashes + 1;
    }
    conceal_span(chars, at..chars.len(), start, conceals);
}

/// Adds the conceals of the chars of `chars` in `span`, a line or a link's
/// text, every marker paired inside it; `start` is where the line starts in
/// the text.
fn conceal_span(
    chars: &[char],
    span: Range<usize>,
    start: usize,
    conceals: &mut Vec<Range<usize>>,
) {
    let mut at = span.start;
    while at < span.end {
        if chars[at] == ESCAPE {
            at += 2;
            continue;
        }
        if let Some(found) = link_at(chars, at, span.end) {
            conceals.push(start + found.open.start..start + found.open.end);
            conceal_span(chars, found.open.end..found.close.start, start, conceals);
            conceals.push(start + found.close.start..start + found.close.end);
            at = found.close.end;
            continue;
        }
        let Some(marker) = PAIRED.iter().find(|marker| starts_with(chars, at, marker)) else {
            at += 1;
            continue;
        };
        let len = marker.chars().count();
        match closer(chars, at + len, span.end, marker) {
            Some(close) if close > at + len => {
                conceals.push(start + at..start + at + len);
                // What a code span holds is code; what the others hold
                // may be marked up itself.
                if *marker != CODE {
                    conceal_span(chars, at + len..close, start, conceals);
                }
                conceals.push(start + close..start + close + len);
                at = close + len;
            }
            Some(_) | None => at += len,
        }
    }
}

/// The parts of a link or image to hide.
struct Link {
    /// Its close, from `]` through `)`.
    close: Range<usize>,
    /// Its open, `[` or `![`.
    open: Range<usize>,
}

/// The link starting at `at` in `chars`, `[text](url)` or `![alt](url)`,
/// ending before `end`.
fn link_at(chars: &[char], at: usize, end: usize) -> Option<Link> {
    let open_len = match (chars.get(at), chars.get(at + 1)) {
        (Some('!'), Some('[')) => 2,
        (Some('['), _) => 1,
        _ => return None,
    };
    let text_end = (at + open_len..end).find(|index| chars[*index] == ']')?;
    if chars.get(text_end + 1) != Some(&'(') {
        return None;
    }
    let url_end = (text_end + 2..end).find(|index| chars[*index] == ')')?;
    Some(Link {
        close: text_end..url_end + 1,
        open: at..at + open_len,
    })
}

/// Where the closing `marker` after `from` and before `end` in `chars` is,
/// not escaped; a code span's backticks also hide what they hold from other
/// markers.
fn closer(chars: &[char], from: usize, end: usize, marker: &str) -> Option<usize> {
    let mut at = from;
    while at + marker.chars().count() <= end {
        if chars[at] == ESCAPE {
            at += 2;
            continue;
        }
        if starts_with(chars, at, marker) {
            return Some(at);
        }
        at += 1;
    }
    None
}

/// Whether `chars` holds `marker` at `at`.
fn starts_with(chars: &[char], at: usize, marker: &str) -> bool {
    marker
        .chars()
        .enumerate()
        .all(|(offset, c)| chars.get(at + offset) == Some(&c))
}

// ========================================================================
// Tests
// ========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// The text `text` shows once its conceals hide.
    fn shown(text: &str) -> String {
        let hidden = markdown_conceals(text);
        text.chars()
            .enumerate()
            .filter(|(at, _)| !hidden.iter().any(|range| range.contains(at)))
            .map(|(_, c)| c)
            .collect()
    }

    #[test]
    fn test_markers_hide_and_their_text_shows() {
        assert_eq!(shown("## A **bold** and _it_ `x`"), "A bold and it x");
        assert_eq!(
            shown("see [docs](https://a.b) or ![pic](p.png)"),
            "see docs or pic"
        );
        assert_eq!(
            shown("~~gone~~ **open"),
            "gone **open",
            "no closer, no conceal"
        );
        assert_eq!(shown("a \\*b\\* c"), "a \\*b\\* c", "escaped markers stay");
    }

    #[test]
    fn test_a_links_text_hides_its_own_markers() {
        assert_eq!(
            shown("in [`docs/guide/`](docs/guide/README.md), one page"),
            "in docs/guide/, one page"
        );
        assert_eq!(
            shown("**[bold link](u)** and [_it_](v)"),
            "bold link and it"
        );
        assert_eq!(
            shown("[a *b](u) c*"),
            "a *b c*",
            "a marker pairs inside its link's text, not across its edge"
        );
    }

    #[test]
    fn test_markers_inside_others_hide_but_code_keeps_its_own() {
        assert_eq!(shown("**bold _it_ end**"), "bold it end");
        assert_eq!(shown("`a **b** c`"), "a **b** c", "code is literal");
    }

    #[test]
    fn test_fenced_code_is_left_alone() {
        assert_eq!(shown("```\n**x**\n```\n**y**"), "```\n**x**\n```\ny");
    }
}
