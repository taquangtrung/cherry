//! The numbers of a Markdown document's headings, `1.`, `1.1.`, `2.`, for a
//! host to show before each heading's text, where its hidden `#`s were
//! ([`markdown_conceals`](super::markdown_conceals)).
//!
//! Only `#` headings are numbered, outside fenced code and front matter.
//! The levels count from the top heading the document has: with no front
//! matter and a single `#` heading, that heading is the title, left
//! unnumbered, and `##` is the first level; with no `#` heading, the
//! shallowest level used is. A heading deeper than the one before it by
//! more than a level is left unnumbered, as it is out of the structure. A
//! document that numbers a heading by hand (`## 2. Setup`, `## B) Notes`)
//! is left unnumbered throughout, so no heading reads twice numbered.

use super::outline::front_matter_lines;

// ========================================================================
// Constants
// ========================================================================

/// The fences that open and close a code block.
const FENCES: [&str; 2] = ["```", "~~~"];

/// The deepest heading Markdown has.
const MAX_HEADING: usize = 6;

// ========================================================================
// Data Structures
// ========================================================================

/// The number a heading is shown with.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct HeadingNumber {
    /// How many `#`s the heading has.
    pub level: usize,
    /// The line it is on, from 0.
    pub line: usize,
    /// Its number, the numbers of its levels joined by `.` and ended by
    /// one: `1.2.`.
    pub number: String,
}

/// A `#` heading of the document.
struct Heading {
    /// How many `#`s it has.
    level: usize,
    /// The line it is on.
    line: usize,
}

// ========================================================================
// Functions
// ========================================================================

/// The numbers of the headings of Markdown `text`, as the module docs say,
/// top first; none when the document numbers its headings by hand.
pub fn heading_numbers(text: &str) -> Vec<HeadingNumber> {
    let lines: Vec<&str> = text.split('\n').collect();
    let front_matter = front_matter_lines(&lines);
    let mut headings = Vec::new();
    let mut in_fence = false;
    for (line, content) in lines.iter().enumerate().skip(front_matter) {
        let trimmed = content.trim_start();
        if FENCES.iter().any(|fence| trimmed.starts_with(fence)) {
            in_fence = !in_fence;
            continue;
        }
        let hashes = content.chars().take_while(|c| *c == '#').count();
        let title = content[hashes..].strip_prefix(' ');
        if in_fence || !(1..=MAX_HEADING).contains(&hashes) {
            continue;
        }
        let Some(title) = title else {
            continue;
        };
        if is_numbered_by_hand(title.trim_start()) {
            return Vec::new();
        }
        headings.push(Heading {
            level: hashes,
            line,
        });
    }
    let tops = headings.iter().filter(|heading| heading.level == 1).count();
    let shallowest = headings
        .iter()
        .map(|heading| heading.level)
        .min()
        .unwrap_or(1);
    let offset = match (front_matter, tops) {
        (0, 1) => 1,
        (_, 0) => shallowest - 1,
        _ => 0,
    };
    let mut counts: Vec<usize> = Vec::new();
    let mut numbers = Vec::new();
    for heading in headings {
        let Some(level) = heading.level.checked_sub(offset).filter(|level| *level > 0) else {
            continue;
        };
        let parents_seen = counts.len() >= level - 1 && counts[..level - 1].iter().all(|n| *n > 0);
        if !parents_seen {
            continue;
        }
        counts.resize(level, 0);
        counts[level - 1] += 1;
        let number: Vec<String> = counts.iter().map(usize::to_string).collect();
        numbers.push(HeadingNumber {
            level: heading.level,
            line: heading.line,
            number: format!("{}.", number.join(".")),
        });
    }
    numbers
}

/// Whether a heading's `title` starts with a number of its own: digits, a
/// letter, or a Roman numeral, then `.` or `)` and a blank.
fn is_numbered_by_hand(title: &str) -> bool {
    let Some(end) = title.find(['.', ')']) else {
        return false;
    };
    let (mark, rest) = (&title[..end], &title[end + 1..]);
    let is_mark = mark.chars().all(|c| c.is_ascii_digit())
        || (mark.chars().count() == 1 && mark.chars().all(|c| c.is_ascii_alphabetic()))
        || mark.chars().all(|c| "IVXLCDM".contains(c));
    !mark.is_empty() && is_mark && rest.starts_with([' ', '\t'])
}

// ========================================================================
// Tests
// ========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// The numbers of `text`'s headings, as `(line, number)`.
    fn numbered(text: &str) -> Vec<(usize, String)> {
        heading_numbers(text)
            .into_iter()
            .map(|heading| (heading.line, heading.number))
            .collect()
    }

    #[test]
    fn test_one_top_heading_is_the_title_and_the_rest_count_under_it() {
        let text = "# Title\n## A\n### A.1\n### A.2\n## B";
        assert_eq!(
            numbered(text),
            [
                (1, "1.".to_owned()),
                (2, "1.1.".to_owned()),
                (3, "1.2.".to_owned()),
                (4, "2.".to_owned()),
            ]
        );
    }

    #[test]
    fn test_front_matter_and_several_top_headings_number_from_the_top() {
        let text = "---\ntitle: x\n---\n# One\n## Sub\n# Two";
        let numbers: Vec<String> = numbered(text).into_iter().map(|(_, n)| n).collect();
        assert_eq!(numbers, ["1.", "1.1.", "2."]);
        let lone = numbered("---\ntitle: x\n---\n# Only");
        assert_eq!(lone, [(3, "1.".to_owned())], "front matter: # is no title");
    }

    #[test]
    fn test_skipped_levels_code_and_hand_numbering_are_left_alone() {
        assert_eq!(
            numbered("## A\n#### Deep\n## B").len(),
            2,
            "out of structure"
        );
        assert_eq!(numbered("## A\n```\n## not\n```"), [(0, "1.".to_owned())]);
        assert!(
            numbered("## 1. Setup\n## 2. Use").is_empty(),
            "numbered by hand"
        );
        assert!(numbered("## B) Notes").is_empty());
        assert_eq!(numbered("## I am here").len(), 1, "a word, not a numeral");
    }
}
