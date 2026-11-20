//! Passes that name a line's runs more closely than its grammar does, by
//! the scopes they put the text in, for a theme's rules to color:
//!
//! - A commit summary's conventional type (`feat:`, `fix(ui)!:`), and git's
//!   `#` comments in a commit message or a rebase plan as one comment each
//!   ([`commit`]).
//! - Rust's names the grammar leaves unscoped, scoped by what stands around
//!   them ([`words`]).
//! - A Markdown task box, and the host's todo keywords in comments and in
//!   Markdown's prose ([`todo`]).
//! - Markdown's wiki links, highlights, callout labels, and front matter's
//!   fences ([`markdown`]).
//!
//! The grammar's runs come in as bytes of the line with their scopes, and go
//! out as pieces of the line, each with the scopes it sits in.

use std::ops::Range;

mod commit;
mod markdown;
mod todo;
mod words;

// ========================================================================
// Data Structures
// ========================================================================

/// A run of a line as the grammar scoped it, or a piece of it once named
/// more closely: its bytes in the line, and the scopes it sits in,
/// outermost first.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Piece<'a> {
    /// Its bytes in the line.
    pub(crate) range: Range<usize>,
    /// The scopes it sits in, outermost first.
    pub(crate) scopes: Vec<&'a str>,
}

/// Which passes a line goes through, as its language and the host say.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Passes<'k> {
    /// The todo keywords the host colors, each a whole word in its case.
    pub(crate) keywords: &'k [String],
    /// Whether the line is Markdown's, for its marks beyond the grammar.
    pub(crate) markdown: bool,
    /// Whether the grammar is one whose unscoped names are scoped (Rust's).
    pub(crate) words: bool,
}

// ========================================================================
// Functions
// ========================================================================

/// The pieces of `line`, whose grammar scoped it as `runs`, named more
/// closely by the passes `passes` turns on; `colors` says whether the
/// host colors text in a scope stack, as the names pass scopes only words
/// left in the text's color.
pub(crate) fn refine<'a>(
    line: &str,
    runs: &[Piece<'a>],
    passes: Passes<'_>,
    colors: impl Fn(&[&str]) -> bool,
) -> Vec<Piece<'a>> {
    let named = match passes.words {
        true => {
            let views: Vec<words::Run> = runs
                .iter()
                .map(|run| words::Run {
                    plain: !colors(&run.scopes),
                    range: run.range.clone(),
                    scopes: &run.scopes,
                })
                .collect();
            words::scope_words(line, &views)
        }
        false => Vec::new(),
    };
    let mut pieces = Vec::new();
    for run in runs {
        let mut at = run.range.start;
        for word in named
            .iter()
            .filter(|word| run.range.contains(&word.range.start))
        {
            commit::split(&mut pieces, line, at..word.range.start, &run.scopes);
            let mut scopes = run.scopes.clone();
            scopes.push(word.scope);
            push(&mut pieces, word.range.clone(), scopes);
            at = word.range.end;
        }
        commit::split(&mut pieces, line, at..run.range.end, &run.scopes);
    }
    let mut overlays = todo::overlays(line, runs, passes.keywords);
    if passes.markdown {
        overlays.extend(markdown::overlays(line, runs));
    }
    for overlay in overlays {
        lay(&mut pieces, overlay);
    }
    pieces
}

/// Whether the names pass scopes the words of a grammar whose scope is
/// `scope`: Rust's.
pub(crate) fn names_words(scope: &str) -> bool {
    words::reads(scope)
}

/// Puts a piece of `range` in `scopes` after `pieces`, nothing for an
/// empty range.
fn push<'a>(pieces: &mut Vec<Piece<'a>>, range: Range<usize>, scopes: Vec<&'a str>) {
    if !range.is_empty() {
        pieces.push(Piece { range, scopes });
    }
}

/// Lays `overlay` over `pieces`, which cover the line in order: the bytes
/// it covers sit in its scopes instead.
fn lay<'a>(pieces: &mut Vec<Piece<'a>>, overlay: Piece<'a>) {
    let range = overlay.range.clone();
    let mut laid = Vec::with_capacity(pieces.len() + 2);
    let mut placed = false;
    for piece in pieces.drain(..) {
        if piece.range.end <= range.start || piece.range.start >= range.end {
            if piece.range.start >= range.end && !placed {
                laid.push(overlay.clone());
                placed = true;
            }
            laid.push(piece);
            continue;
        }
        if piece.range.start < range.start {
            laid.push(Piece {
                range: piece.range.start..range.start,
                scopes: piece.scopes.clone(),
            });
        }
        if !placed {
            laid.push(overlay.clone());
            placed = true;
        }
        if piece.range.end > range.end {
            laid.push(Piece {
                range: range.end..piece.range.end,
                scopes: piece.scopes,
            });
        }
    }
    if !placed {
        laid.push(overlay);
    }
    *pieces = laid;
}

/// Whether a scope of `stack` is one of `wanted` or inside one.
fn within(stack: &[&str], wanted: &[&str]) -> bool {
    stack
        .iter()
        .any(|scope| wanted.iter().any(|name| names(name, scope)))
}

/// Whether `wanted` names `scope` or a scope it holds: the whole name, or a
/// prefix of it ending at a dot.
fn names(wanted: &str, scope: &str) -> bool {
    scope
        .strip_prefix(wanted)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with('.'))
}

/// The piece of `runs` holding byte `at` of the line.
fn run_at<'r, 'a>(runs: &'r [Piece<'a>], at: usize) -> Option<&'r Piece<'a>> {
    runs.iter().find(|run| run.range.contains(&at))
}

/// An overlay of `range` in `stack` with `scopes` inside it.
fn overlay<'a>(range: Range<usize>, stack: &[&'a str], scopes: &[&'a str]) -> Piece<'a> {
    let mut stack = stack.to_vec();
    stack.extend_from_slice(scopes);
    Piece {
        range,
        scopes: stack,
    }
}

// ========================================================================
// Tests
// ========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// No pass but the ones every line goes through.
    const NONE: Passes<'static> = Passes {
        keywords: &[],
        markdown: false,
        words: false,
    };

    /// `line` refined from one run per `(bytes, scopes)`, as `(text, top
    /// scope)` for each piece.
    fn refined<'a>(
        line: &'a str,
        runs: &[(Range<usize>, &[&'a str])],
        passes: Passes<'_>,
    ) -> Vec<(&'a str, &'a str)> {
        let runs: Vec<Piece> = runs
            .iter()
            .map(|(range, scopes)| Piece {
                range: range.clone(),
                scopes: scopes.to_vec(),
            })
            .collect();
        refine(line, &runs, passes, |_| false)
            .into_iter()
            .map(|piece| (&line[piece.range], *piece.scopes.last().expect("scoped")))
            .collect()
    }

    #[test]
    fn test_an_overlay_splits_the_pieces_it_falls_in() {
        let mut pieces = vec![
            Piece {
                range: 0..4,
                scopes: vec!["a"],
            },
            Piece {
                range: 4..8,
                scopes: vec!["b"],
            },
        ];
        lay(
            &mut pieces,
            Piece {
                range: 2..6,
                scopes: vec!["c"],
            },
        );
        let laid: Vec<(Range<usize>, &str)> = pieces
            .into_iter()
            .map(|piece| (piece.range, piece.scopes[0]))
            .collect();
        assert_eq!(laid, [(0..2, "a"), (2..6, "c"), (6..8, "b")]);
    }

    #[test]
    fn test_a_commit_summary_s_type_and_a_todo_keyword_are_named() {
        let summary = "fix(ui)!: a TODO";
        let found = refined(
            summary,
            &[(
                0..summary.len(),
                &["text.git-commit", "meta.scope.subject.git-commit"],
            )],
            NONE,
        );
        assert_eq!(
            found,
            [
                ("fix(ui)!:", "keyword.other.type.git-commit"),
                (" a TODO", "meta.scope.subject.git-commit"),
            ]
        );
        let keywords = ["TODO".to_string()];
        let comment = "x // TODO now";
        let found = refined(
            comment,
            &[
                (0..2, &["source.rust"]),
                (2..comment.len(), &["source.rust", "comment.line.rust"]),
            ],
            Passes {
                keywords: &keywords,
                ..NONE
            },
        );
        assert_eq!(
            found,
            [
                ("x ", "source.rust"),
                ("// ", "comment.line.rust"),
                ("TODO", "keyword.todo"),
                (" now", "comment.line.rust"),
            ]
        );
    }
}
