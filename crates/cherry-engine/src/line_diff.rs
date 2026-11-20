//! The line edits that turn one text into another, smallest first: what a
//! document reloaded from its file is changed by, so that only the lines
//! that differ move, and every cursor, mark, and fold elsewhere stays put.
//!
//! Lines the two texts share at the start and the end are left out first;
//! the rest is diffed line by line with Myers' algorithm. A middle that
//! would take more than [`MAX_STEPS`] lines added or removed is replaced
//! whole instead, so a file rewritten from top to bottom costs no more than
//! a plain replacement.

use crate::transaction::Edit;

// ========================================================================
// Constants
// ========================================================================

/// The most lines added and removed the diff traces before it replaces the
/// changed middle whole: its memory grows with the square of this.
const MAX_STEPS: usize = 1_000;

// ========================================================================
// Data Structures
// ========================================================================

/// One step of an edit script between two runs of lines.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Step {
    /// A line of the new text not in the old.
    Add,
    /// A line both texts have.
    Keep,
    /// A line of the old text not in the new.
    Remove,
}

// ========================================================================
// Functions
// ========================================================================

/// The edits, ranges of `old` in chars, that turn `old` into `new`: one for
/// each run of lines that differ, none when the texts are equal.
pub(crate) fn line_edits(old: &str, new: &str) -> Vec<Edit> {
    let old_lines: Vec<&str> = old.split_inclusive('\n').collect();
    let new_lines: Vec<&str> = new.split_inclusive('\n').collect();
    let prefix = old_lines
        .iter()
        .zip(&new_lines)
        .take_while(|(old, new)| old == new)
        .count();
    let suffix = old_lines[prefix..]
        .iter()
        .rev()
        .zip(new_lines[prefix..].iter().rev())
        .take_while(|(old, new)| old == new)
        .count();
    let old_middle = &old_lines[prefix..old_lines.len() - suffix];
    let new_middle = &new_lines[prefix..new_lines.len() - suffix];
    if old_middle.is_empty() && new_middle.is_empty() {
        return Vec::new();
    }
    let mut starts = Vec::with_capacity(old_lines.len() + 1);
    let mut offset = 0;
    starts.push(offset);
    for line in &old_lines {
        offset += line.chars().count();
        starts.push(offset);
    }
    let edit = |old_range: (usize, usize), new_range: (usize, usize)| {
        Edit::new(
            starts[prefix + old_range.0]..starts[prefix + old_range.1],
            new_middle[new_range.0..new_range.1].concat(),
        )
    };
    let Some(steps) = edit_script(old_middle, new_middle) else {
        return vec![edit((0, old_middle.len()), (0, new_middle.len()))];
    };
    let mut edits = Vec::new();
    let (mut old_at, mut new_at) = (0, 0);
    let mut run: Option<((usize, usize), (usize, usize))> = None;
    for step in steps {
        match step {
            Step::Keep => {
                if let Some((old_range, new_range)) = run.take() {
                    edits.push(edit(old_range, new_range));
                }
                old_at += 1;
                new_at += 1;
            }
            Step::Add | Step::Remove => {
                let (old_range, new_range) =
                    run.get_or_insert(((old_at, old_at), (new_at, new_at)));
                match step {
                    Step::Add => new_at += 1,
                    _ => old_at += 1,
                }
                old_range.1 = old_at;
                new_range.1 = new_at;
            }
        }
    }
    if let Some((old_range, new_range)) = run {
        edits.push(edit(old_range, new_range));
    }
    edits
}

/// The shortest steps from `old` to `new`, by Myers' algorithm; `None` when
/// they would take more than [`MAX_STEPS`] lines added or removed.
fn edit_script(old: &[&str], new: &[&str]) -> Option<Vec<Step>> {
    let (old_len, new_len) = (old.len() as isize, new.len() as isize);
    let max = old_len + new_len;
    let limit = max.min(MAX_STEPS as isize);
    // The furthest old line each diagonal `k` (old line less new line)
    // reaches, at `k + max`; and for each number of steps `d`, its values
    // for the diagonals `-d..=d`, to walk back along.
    let mut furthest = vec![0isize; 2 * max as usize + 2];
    let mut trace: Vec<Vec<isize>> = Vec::new();
    for d in 0..=limit {
        for k in (-d..=d).step_by(2) {
            let at = (k + max) as usize;
            let mut x = match k == -d || (k != d && furthest[at - 1] < furthest[at + 1]) {
                true => furthest[at + 1],
                false => furthest[at - 1] + 1,
            };
            let mut y = x - k;
            while x < old_len && y < new_len && old[x as usize] == new[y as usize] {
                x += 1;
                y += 1;
            }
            furthest[at] = x;
            if x >= old_len && y >= new_len {
                trace.push(furthest[(max - d) as usize..=(max + d) as usize].to_vec());
                return Some(walk_back(&trace, old_len, new_len));
            }
        }
        trace.push(furthest[(max - d) as usize..=(max + d) as usize].to_vec());
    }
    None
}

/// The steps the furthest reaches of `trace` took to `(old_len, new_len)`,
/// first to last.
fn walk_back(trace: &[Vec<isize>], old_len: isize, new_len: isize) -> Vec<Step> {
    let mut steps = Vec::new();
    let (mut x, mut y) = (old_len, new_len);
    for d in (1..trace.len() as isize).rev() {
        let before = &trace[(d - 1) as usize];
        let reach = |k: isize| before[(k + d - 1) as usize];
        let k = x - y;
        let from = match k == -d || (k != d && reach(k - 1) < reach(k + 1)) {
            true => k + 1,
            false => k - 1,
        };
        let from_x = reach(from);
        let from_y = from_x - from;
        while x > from_x && y > from_y {
            steps.push(Step::Keep);
            x -= 1;
            y -= 1;
        }
        steps.push(match x == from_x {
            true => Step::Add,
            false => Step::Remove,
        });
        x = from_x;
        y = from_y;
    }
    while x > 0 && y > 0 {
        steps.push(Step::Keep);
        x -= 1;
        y -= 1;
    }
    steps.reverse();
    steps
}

// ========================================================================
// Tests
// ========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// `old` with `edits` made, each range in chars of `old`.
    fn applied(old: &str, edits: &[Edit]) -> String {
        let mut chars: Vec<char> = old.chars().collect();
        for edit in edits.iter().rev() {
            chars.splice(edit.range.clone(), edit.text.chars());
        }
        chars.into_iter().collect()
    }

    #[test]
    fn test_equal_texts_need_no_edit() {
        assert!(line_edits("a\nb\n", "a\nb\n").is_empty());
        assert!(line_edits("", "").is_empty());
    }

    #[test]
    fn test_only_the_lines_that_differ_are_edited() {
        let old = "one\ntwo\nthree\nfour\nfive\n";
        let new = "one\nTWO\nthree\nfour\nfive\nsix\n";
        let edits = line_edits(old, new);
        assert_eq!(
            edits,
            [Edit::new(4..8, "TWO\n"), Edit::new(24..24, "six\n")],
            "the changed line and the added one, the rest untouched"
        );
        assert_eq!(applied(old, &edits), new);
    }

    #[test]
    fn test_any_two_texts_are_reached_exactly() {
        let texts = [
            "",
            "a",
            "a\n",
            "a\nb\nc",
            "x\na\nb\nc\ny\n",
            "c\nb\na\n",
            "é\n日本\n",
            "a\n\n\nb\n",
        ];
        for old in texts {
            for new in texts {
                let edits = line_edits(old, new);
                assert_eq!(applied(old, &edits), new, "{old:?} to {new:?}");
            }
        }
    }

    #[test]
    fn test_a_middle_past_the_cap_is_replaced_whole_and_still_exact() {
        let old: String = (0..MAX_STEPS + 10)
            .map(|line| format!("old {line}\n"))
            .collect();
        let new: String = (0..MAX_STEPS + 10)
            .map(|line| format!("new {line}\n"))
            .collect();
        let edits = line_edits(&old, &new);
        assert_eq!(edits.len(), 1, "one replacement");
        assert_eq!(applied(&old, &edits), new);
    }
}
