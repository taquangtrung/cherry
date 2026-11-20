//! Task boxes and todo keywords. A Markdown list item's task box (`- [ ]`,
//! `- [x]`) has its brackets put in `markup.checkbox` and its mark in
//! `markup.checkbox.checked`, which the grammar leaves as plain list text.
//! The host's todo keywords (`TODO`, `FIXME`) are put in `keyword.todo`,
//! each as a whole word in its own case: in a comment in code, and anywhere
//! in Markdown's prose but its code.

use super::markdown::{CODE, MARKDOWN};
use super::{Piece, overlay, run_at, within};

// ========================================================================
// Constants
// ========================================================================

/// The scope a list item sits in, its task box with it.
const LIST: &str = "markup.list";

/// The scope every comment of every grammar sits in.
const COMMENT: &str = "comment";

/// The scopes a task box's brackets, and its mark, are put in.
const CHECKBOX: &str = "markup.checkbox";
const CHECKBOX_MARK: &str = "markup.checkbox.checked";

/// The scope a todo keyword is put in.
const KEYWORD: &str = "keyword.todo";

/// The marks a task box holds: to do, done, done, and cancelled.
const TASK_MARKS: [u8; 4] = *b" xX-";

// ========================================================================
// Functions
// ========================================================================

/// The overlays of `line`'s task box and its `keywords`, from `runs`, the
/// scopes the grammar gave its bytes.
pub(super) fn overlays<'a>(line: &str, runs: &[Piece<'a>], keywords: &[String]) -> Vec<Piece<'a>> {
    let mut overlays = Vec::new();
    let markdown = runs
        .iter()
        .filter_map(|run| run.scopes.first())
        .any(|scope| *scope == MARKDOWN);
    if markdown {
        task_box(&mut overlays, line, runs);
    }
    for keyword in keywords.iter().filter(|keyword| !keyword.is_empty()) {
        for (start, _) in line.match_indices(keyword.as_str()) {
            let end = start + keyword.len();
            if !is_whole_word(line, start, end) {
                continue;
            }
            let Some(run) = run_at(runs, start) else {
                continue;
            };
            let counts = match markdown {
                true => !within(&run.scopes, &CODE),
                false => within(&run.scopes, &[COMMENT]),
            };
            if counts {
                overlays.push(overlay(start..end, &run.scopes, &[KEYWORD]));
            }
        }
    }
    overlays
}

/// The overlays of the task box opening `line`, a list item's in Markdown:
/// its brackets in [`CHECKBOX`] and its mark in [`CHECKBOX_MARK`].
fn task_box<'a>(overlays: &mut Vec<Piece<'a>>, line: &str, runs: &[Piece<'a>]) {
    let Some(open) = task_box_at(line) else {
        return;
    };
    let Some(run) = run_at(runs, open).filter(|run| within(&run.scopes, &[LIST])) else {
        return;
    };
    overlays.push(overlay(open..open + 1, &run.scopes, &[CHECKBOX]));
    overlays.push(overlay(open + 1..open + 2, &run.scopes, &[CHECKBOX_MARK]));
    overlays.push(overlay(open + 2..open + 3, &run.scopes, &[CHECKBOX]));
}

/// The byte of the `[` of the task box opening `line`, a list item's:
/// past the indent, a marker (`-`, `*`, `+`, or a number and `.` or `)`),
/// and a blank, a `[`, one of [`TASK_MARKS`], and `]`, then a blank or the
/// line's end.
fn task_box_at(line: &str) -> Option<usize> {
    let bytes = line.as_bytes();
    let mut at = bytes
        .iter()
        .take_while(|byte| matches!(byte, b' ' | b'\t'))
        .count();
    let digits = bytes[at..]
        .iter()
        .take_while(|byte| byte.is_ascii_digit())
        .count();
    match (digits, bytes.get(at + digits)) {
        (0, Some(b'-' | b'*' | b'+')) => at += 1,
        (1.., Some(b'.' | b')')) => at += digits + 1,
        _ => return None,
    }
    let blanks = bytes[at..]
        .iter()
        .take_while(|byte| matches!(byte, b' ' | b'\t'))
        .count();
    if blanks == 0 {
        return None;
    }
    at += blanks;
    let is_box = bytes.get(at) == Some(&b'[')
        && bytes
            .get(at + 1)
            .is_some_and(|mark| TASK_MARKS.contains(mark))
        && bytes.get(at + 2) == Some(&b']')
        && bytes
            .get(at + 3)
            .is_none_or(|after| matches!(after, b' ' | b'\t'));
    is_box.then_some(at)
}

/// Whether the bytes `start..end` of `line` are a word of their own: no
/// letter, digit, or `_` just before or after them.
fn is_whole_word(line: &str, start: usize, end: usize) -> bool {
    let is_word = |c: char| c.is_alphanumeric() || c == '_';
    !line[..start].chars().next_back().is_some_and(is_word)
        && !line[end..].chars().next().is_some_and(is_word)
}

// ========================================================================
// Tests
// ========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_a_task_box_is_found_past_a_list_marker_only() {
        assert_eq!(task_box_at("- [ ] task"), Some(2));
        assert_eq!(task_box_at("  12. [x] done"), Some(6));
        assert_eq!(task_box_at("* [-]"), Some(2), "at the line's end");
        assert_eq!(task_box_at("[ ] no marker"), None);
        assert_eq!(task_box_at("-[ ] no blank"), None);
        assert_eq!(task_box_at("- [y] not a mark"), None);
        assert_eq!(task_box_at("- [ ]x"), None, "a link, not a box");
    }

    #[test]
    fn test_a_keyword_is_a_whole_word() {
        let whole = |line: &str| {
            let start = line.find("TODO").expect("in the line");
            is_whole_word(line, start, start + 4)
        };
        assert!(whole("// TODO: x"));
        assert!(whole("TODO(me)"));
        assert!(!whole("TODOS"));
        assert!(!whole("MY_TODO"));
    }
}
