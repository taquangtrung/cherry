//! Search: `/` and `?` search forward and backward for a pattern typed in
//! a prompt, `n` and `N` search again the same way and the other way, and
//! `*` and `#` search for the word under the cursor. All of them are
//! motions: they move the cursor, extend a visual selection, or after an
//! operator end the text it acts on, exclusively (`d/foo<cr>` deletes up
//! to the match). A count finds the count-th match.
//!
//! # Patterns
//!
//! Patterns are regular expressions, as in Vim, but written in the syntax
//! of the `regex` crate, not Vim's: `(a|b)` rather than `\(a\|b\)`. Vim's
//! word edges work as in Vim, since the `regex` crate reads them too: `\<`
//! matches at the start of a word and `\>` at its end, so `\<cat\>` finds
//! `cat` alone and `\bcat\b` does the same. A pattern matches within the
//! whole document, so `\n` matches a line break. Case follows smart case: a
//! pattern with no uppercase letter (escapes like `\W` aside) matches any
//! case, one with an uppercase letter matches exactly. `*` and `#` match
//! the word whole and exactly. An empty pattern searches for the last one
//! again. With
//! [`EditorOptions::literal_search`](crate::EditorOptions::literal_search),
//! a typed pattern is looked for as the text it is, escaped into the
//! regular expression the last search keeps (as `*` keeps its word), so
//! `:s//` with an empty pattern replaces that text.
//!
//! # Selecting a match
//!
//! `gn` selects the match of the last pattern under the cursor, or else
//! the next one, in Visual mode; after an operator it acts on that match
//! (`dgn`, and `cgn` repeated with `.` for each next match), and in a
//! visual mode it stretches the selection to the match's end. `gN` does
//! the same going back. Both wrap around the document.
//!
//! # Wrapping
//!
//! Searching past the end of the document continues at the start (and
//! back past the start, at the end), with a notice saying so. A pattern
//! that does not parse or matches nothing leaves the cursor where it is
//! and cancels a waiting operator, with a notice.
//!
//! # Highlighting
//!
//! The last pattern stays the editor's, shared by every view, and its
//! matches on the lines in sight paint as
//! [`Style::SearchMatch`](crate::Style::SearchMatch), until `:noh` hides
//! them; the next search shows them again.

use std::ops::Range;

use regex::{Regex, RegexBuilder};

use super::command::CommandContext;
use super::document::{Document, line_end};
use super::editor::{Editor, ViewId};
use super::mark::jump_to;
use super::motion::{CharClass, class_at};
use super::operator::{MotionKind, Operand, operate_on};
use super::prompt::{self, PromptKind, SearchFollow};
use super::view::{Mode, is_visual};

// ========================================================================
// Constants
// ========================================================================

/// The char that escapes the next one in a pattern.
const ESCAPE: char = '\\';

/// The most matches [`Editor::search_count`] counts before it stops.
const MAX_SEARCH_COUNT: usize = 9999;

/// The notice for searching again before any search.
pub(crate) const NO_PREVIOUS_PATTERN: &str = "No previous search pattern";

/// The notice for `*` or `#` with no word on the rest of the line.
const NO_WORD: &str = "No string under cursor";

/// The notice for a forward search that wrapped.
const WRAPPED_TO_TOP: &str = "search hit BOTTOM, continuing at TOP";

/// The notice for a backward search that wrapped.
const WRAPPED_TO_BOTTOM: &str = "search hit TOP, continuing at BOTTOM";

// ========================================================================
// Data Structures
// ========================================================================

/// Which way a search goes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Direction {
    /// Toward the start of the document.
    Backward,
    /// Toward the end of the document.
    Forward,
}

/// How a pattern treats case.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CaseRule {
    /// Case matches exactly.
    Exact,
    /// Any case.
    Ignore,
    /// Any case unless the pattern has an uppercase letter.
    Smart,
}

/// How many matches of a search pattern a document has, and which the
/// cursor is at, for a host's status bar (`3/17`); see
/// [`Editor::search_count`].
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub struct SearchCount {
    /// The match the cursor is on or last passed, counted from 1 at the
    /// start of the document; 0 before the first.
    pub current: usize,
    /// Whether counting stopped at the most it counts, 9999, so the
    /// document has more matches than [`total`](Self::total).
    pub is_capped: bool,
    /// The matches counted.
    pub total: usize,
}

/// A pattern compiled, kept so that compiling it again is free (`:g`
/// running `:s` on each line).
#[derive(Clone, Debug)]
pub(crate) struct CompiledPattern {
    /// Whether it was compiled ignoring case.
    ignore_case: bool,
    /// The pattern as typed.
    pattern: String,
    /// The pattern compiled.
    regex: Regex,
}

/// The last search: its pattern, compiled, and the way it went.
#[derive(Clone, Debug)]
pub(crate) struct LastSearch {
    /// The way `/` or `?` went; `n` goes the same way.
    direction: Direction,
    /// The pattern as typed.
    pattern: String,
    /// The pattern compiled.
    regex: Regex,
}

// ========================================================================
// Direction Implementations
// ========================================================================

impl Direction {
    /// The other way.
    fn reversed(self) -> Self {
        match self {
            Self::Backward => Self::Forward,
            Self::Forward => Self::Backward,
        }
    }
}

// ========================================================================
// LastSearch Implementations
// ========================================================================

impl LastSearch {
    /// The search for `pattern`, compiled as `regex`, going `direction`.
    pub(crate) fn new(direction: Direction, pattern: String, regex: Regex) -> Self {
        Self {
            direction,
            pattern,
            regex,
        }
    }

    /// The pattern as typed.
    pub(crate) fn pattern(&self) -> &str {
        &self.pattern
    }

    /// The pattern compiled, for painting its matches.
    pub(crate) fn regex(&self) -> &Regex {
        &self.regex
    }
}

// ========================================================================
// Functions
// ========================================================================

/// The char ranges of the non-empty matches of `regex` in `text`, a line.
pub(crate) fn line_matches(regex: &Regex, text: &str) -> Vec<(usize, usize)> {
    regex
        .find_iter(text)
        .filter(|found| !found.is_empty())
        .map(|found| {
            let start = text[..found.start()].chars().count();
            (start, start + found.as_str().chars().count())
        })
        .collect()
}

/// `?`: opens a prompt for a pattern to search backward for.
pub(crate) fn search_backward(cx: &mut CommandContext<'_>) {
    prompt::open(cx, PromptKind::SearchBackward);
}

/// `C-r` in a search prompt in the Emacs style: the next match backward,
/// as Emacs's isearch does; see [`step_search_prompt`].
pub(crate) fn isearch_backward(cx: &mut CommandContext<'_>) {
    step_search_prompt(cx, Direction::Backward);
}

/// `C-s` in a search prompt in the Emacs style: the next match forward,
/// as Emacs's isearch does; see [`step_search_prompt`].
pub(crate) fn isearch_forward(cx: &mut CommandContext<'_>) {
    step_search_prompt(cx, Direction::Forward);
}

/// Moves the match a search prompt is at one further `toward`: one match
/// more the way the prompt searches, or one fewer the other way, turning
/// the search around when it is at its first match. With nothing typed
/// yet, types the last search's pattern instead, as `C-s C-s` does.
fn step_search_prompt(cx: &mut CommandContext<'_>, toward: Direction) {
    let last = cx.editor.last_search().map(|last| last.pattern.clone());
    let Some(prompt) = cx.editor.view_mut(cx.view).prompt_mut() else {
        return;
    };
    let along = match prompt.kind() {
        PromptKind::SearchBackward => toward == Direction::Backward,
        PromptKind::SearchForward => toward == Direction::Forward,
        PromptKind::Command
        | PromptKind::ExtendedCommand
        | PromptKind::ReplaceFrom
        | PromptKind::ReplaceTo => return,
    };
    if prompt.text().is_empty() {
        if let Some(last) = last {
            prompt.replace_line(&last);
        }
    } else {
        let count = prompt.count().unwrap_or(1);
        match (along, count > 1) {
            (true, _) => prompt.set_search(prompt.kind(), Some(count + 1)),
            (false, true) => prompt.set_search(prompt.kind(), Some(count - 1)),
            (false, false) => {
                let kind = match toward {
                    Direction::Backward => PromptKind::SearchBackward,
                    Direction::Forward => PromptKind::SearchForward,
                };
                prompt.set_search(kind, Some(1));
            }
        }
    }
    follow_prompt(cx.editor, cx.view);
}

/// `/`: opens a prompt for a pattern to search forward for.
pub(crate) fn search_forward(cx: &mut CommandContext<'_>) {
    prompt::open(cx, PromptKind::SearchForward);
}

/// `n`: searches for the last pattern again, the way it went.
pub(crate) fn search_next(cx: &mut CommandContext<'_>) {
    search_again(cx, false);
}

/// `N`: searches for the last pattern again, the other way.
pub(crate) fn search_previous(cx: &mut CommandContext<'_>) {
    search_again(cx, true);
}

/// Searches `direction` for `pattern`, typed in a prompt opened with
/// `count`; an empty pattern searches for the last one again.
pub(crate) fn search_typed(
    cx: &mut CommandContext<'_>,
    pattern: &str,
    direction: Direction,
    count: Option<usize>,
) {
    let pattern = match (pattern.is_empty(), cx.editor.last_search()) {
        (true, Some(last)) => last.pattern.clone(),
        (true, None) => return abort(cx, NO_PREVIOUS_PATTERN.to_string()),
        (false, _) => typed_pattern(cx.editor, pattern),
    };
    let cursor = cx.editor.view_ref(cx.view).cursor();
    start_search(cx, pattern, CaseRule::Smart, direction, count, cursor);
}

/// `#`: searches backward for the word under the cursor, whole.
pub(crate) fn search_word_backward(cx: &mut CommandContext<'_>) {
    search_word(cx, Direction::Backward);
}

/// `*`: searches forward for the word under the cursor, whole.
pub(crate) fn search_word_forward(cx: &mut CommandContext<'_>) {
    search_word(cx, Direction::Forward);
}

/// Says `message` and cancels a waiting operator.
fn abort(cx: &mut CommandContext<'_>, message: String) {
    cx.editor.error(message);
    if cx.editor.view_ref(cx.view).mode() == Mode::OperatorPending {
        cx.editor.set_mode(cx.view, Mode::Normal);
    }
}

/// `pattern` compiled under `case`, or why it does not compile, in one
/// line.
pub(crate) fn compile(pattern: &str, case: CaseRule) -> Result<Regex, String> {
    build_regex(pattern, ignores_case(pattern, case))
}

/// `pattern` compiled under `case` as [`compile`] does, taken from `cache`
/// when it holds the same pattern compiled the same way, else kept there.
pub(crate) fn compile_cached(
    cache: &mut Option<CompiledPattern>,
    pattern: &str,
    case: CaseRule,
) -> Result<Regex, String> {
    let ignore_case = ignores_case(pattern, case);
    if let Some(cached) = cache
        .as_ref()
        .filter(|cached| cached.pattern == pattern && cached.ignore_case == ignore_case)
    {
        return Ok(cached.regex.clone());
    }
    let regex = build_regex(pattern, ignore_case)?;
    *cache = Some(CompiledPattern {
        ignore_case,
        pattern: pattern.to_string(),
        regex: regex.clone(),
    });
    Ok(regex)
}

/// `pattern` compiled, ignoring case or not, multi-line, or the notice
/// saying why it does not parse.
fn build_regex(pattern: &str, ignore_case: bool) -> Result<Regex, String> {
    RegexBuilder::new(pattern)
        .case_insensitive(ignore_case)
        .multi_line(true)
        .build()
        .map_err(|err| {
            let text = err.to_string();
            let reason = text.lines().last().unwrap_or_default();
            let reason = reason.trim_start_matches("error: ");
            format!("Invalid pattern {pattern}: {reason}")
        })
}

/// Whether `pattern` matches ignoring case under `case`.
fn ignores_case(pattern: &str, case: CaseRule) -> bool {
    match case {
        CaseRule::Exact => false,
        CaseRule::Ignore => true,
        CaseRule::Smart => !has_uppercase(pattern),
    }
}

/// Whether `pattern` has an uppercase letter that is not escaped.
fn has_uppercase(pattern: &str) -> bool {
    let mut escaped = false;
    for c in pattern.chars() {
        if !escaped && c.is_uppercase() {
            return true;
        }
        escaped = !escaped && c == ESCAPE;
    }
    false
}

/// Moves to the count-th match of the last pattern going `direction` from
/// `from`, wrapping around with a notice, as a jump and a motion.
fn jump(cx: &mut CommandContext<'_>, direction: Direction, count: Option<usize>, from: usize) {
    let Some(last) = cx.editor.last_search() else {
        return abort(cx, NO_PREVIOUS_PATTERN.to_string());
    };
    let document = cx.editor.document_of(cx.view);
    let starts = match_starts(document, &last.regex);
    if starts.is_empty() {
        let message = format!("Pattern not found: {}", last.pattern);
        return abort(cx, message);
    }
    let (at, wrapped) = nth_match(&starts, direction, count, from);
    cx.editor.set_highlight_search(true);
    if wrapped {
        let message = match direction {
            Direction::Backward => WRAPPED_TO_BOTTOM,
            Direction::Forward => WRAPPED_TO_TOP,
        };
        cx.editor.warn(message.to_string());
    }
    jump_to(cx, MotionKind::Exclusive, at);
}

/// The count-th of `starts`, which is not empty, going `direction` from
/// `from`, and whether finding it wrapped past an end of the document.
fn nth_match(
    starts: &[usize],
    direction: Direction,
    count: Option<usize>,
    from: usize,
) -> (usize, bool) {
    let mut at = from;
    let mut wrapped = false;
    for _ in 0..count.unwrap_or(1).max(1) {
        let next = match direction {
            Direction::Backward => starts.iter().rev().find(|start| **start < at),
            Direction::Forward => starts.iter().find(|start| **start > at),
        };
        at = match (next, direction) {
            (Some(next), _) => *next,
            (None, Direction::Backward) => {
                wrapped = true;
                starts[starts.len() - 1]
            }
            (None, Direction::Forward) => {
                wrapped = true;
                starts[0]
            }
        };
    }
    (at, wrapped)
}

/// Follows the pattern typed in the search prompt of `view`, if one is
/// open and incremental search is on: moves the cursor to the match the
/// search would find from where the prompt opened, or back there when the
/// pattern finds none, and keeps the pattern for painting its matches.
pub(crate) fn follow_prompt(editor: &mut Editor, view: ViewId) {
    let state = editor.view_ref(view);
    let Some(prompt) = state.prompt() else {
        return;
    };
    let direction = match prompt.kind() {
        PromptKind::Command
        | PromptKind::ExtendedCommand
        | PromptKind::ReplaceFrom
        | PromptKind::ReplaceTo => return,
        PromptKind::SearchBackward => Direction::Backward,
        PromptKind::SearchForward => Direction::Forward,
    };
    let text = prompt.text();
    if !editor.options().incremental_search || prompt.follow().pattern.as_ref() == Some(&text) {
        return;
    }
    let origin = prompt
        .follow()
        .origin
        .unwrap_or((state.cursor(), state.scroll()));
    let count = prompt.count();
    let regex = if text.is_empty() {
        None
    } else {
        let pattern = typed_pattern(editor, &text);
        editor.compile_pattern(&pattern, CaseRule::Smart).ok()
    };
    let starts = regex.as_ref().map_or_else(Vec::new, |regex| {
        match_starts(editor.document_of(view), regex)
    });
    let target = (!starts.is_empty()).then(|| nth_match(&starts, direction, count, origin.0).0);
    let state = editor.view_mut(view);
    if let Some(prompt) = state.prompt_mut() {
        *prompt.follow_mut() = SearchFollow {
            origin: Some(origin),
            pattern: Some(text),
            regex,
        };
    }
    match target {
        Some(at) => editor.set_cursor(view, at),
        None => {
            editor.set_cursor(view, origin.0);
            editor.view_mut(view).set_scroll(origin.1);
        }
    }
}

/// How many matches of the pattern being typed in a search prompt of
/// `view`, else of the last search while its matches are painted, are in
/// its document, and which the cursor is at; see [`Editor::search_count`].
pub(crate) fn count_matches(editor: &Editor, view: ViewId) -> Option<SearchCount> {
    let state = editor.view_ref(view);
    let typed = state.prompt().and_then(|prompt| match prompt.kind() {
        PromptKind::Command
        | PromptKind::ExtendedCommand
        | PromptKind::ReplaceFrom
        | PromptKind::ReplaceTo => None,
        PromptKind::SearchBackward | PromptKind::SearchForward => Some(prompt.follow()),
    });
    let regex = match typed {
        Some(follow) if follow.pattern.is_some() => match &follow.regex {
            Some(regex) => regex,
            None => return Some(SearchCount::default()),
        },
        Some(_) | None => editor.highlighted_search()?.regex(),
    };
    let document = editor.document_of(view);
    let text = document.to_string();
    let rope = document.rope();
    let cursor = rope.char_to_byte(state.cursor().min(document.len_chars()));
    let mut count = SearchCount::default();
    for found in regex.find_iter(&text) {
        if count.total == MAX_SEARCH_COUNT {
            count.is_capped = true;
            break;
        }
        count.total += 1;
        if found.start() <= cursor {
            count.current = count.total;
        }
    }
    Some(count)
}

/// `gn`: selects, or acts on, the match under the cursor or the next.
pub(crate) fn select_next_match(cx: &mut CommandContext<'_>) {
    select_match(cx, Direction::Forward);
}

/// `gN`: selects, or acts on, the match under the cursor or the one
/// before.
pub(crate) fn select_previous_match(cx: &mut CommandContext<'_>) {
    select_match(cx, Direction::Backward);
}

/// Selects, or acts on after an operator, the match of the last pattern
/// at the cursor or the nearest going `direction`, wrapping around, as
/// the module docs say.
fn select_match(cx: &mut CommandContext<'_>, direction: Direction) {
    let Some(last) = cx.editor.last_search() else {
        return abort(cx, NO_PREVIOUS_PATTERN.to_string());
    };
    let (pattern, regex) = (last.pattern.clone(), last.regex.clone());
    let document = cx.editor.document_of(cx.view);
    let cursor = cx.editor.view_ref(cx.view).cursor();
    let ranges = match_ranges(document, &regex);
    let mode = cx.editor.view_ref(cx.view).mode();
    // A selection already reaching a match's end goes on to the next one.
    let past = usize::from(is_visual(mode));
    let found = match direction {
        Direction::Backward => ranges
            .iter()
            .rev()
            .find(|range| range.start + past <= cursor)
            .or(ranges.last()),
        Direction::Forward => ranges
            .iter()
            .find(|range| range.end > cursor + past)
            .or(ranges.first()),
    };
    let Some(found) = found.cloned() else {
        return abort(cx, format!("Pattern not found: {pattern}"));
    };
    let last_char = document.prev_grapheme_boundary(found.end);
    if is_visual(mode) {
        let target = match direction {
            Direction::Backward => found.start,
            Direction::Forward => last_char,
        };
        return cx.editor.set_cursor(cx.view, target);
    }
    if operate_on(cx, Operand::Charwise(found.clone())) {
        return;
    }
    cx.editor.set_mode(cx.view, Mode::Visual);
    cx.editor.set_visual_anchor(cx.view, found.start);
    cx.editor.set_cursor(cx.view, last_char);
}

/// The char ranges of the non-empty matches of `regex` in `document`.
fn match_ranges(document: &Document, regex: &Regex) -> Vec<Range<usize>> {
    let text = document.to_string();
    let rope = document.rope();
    regex
        .find_iter(&text)
        .filter(|found| !found.is_empty())
        .map(|found| rope.byte_to_char(found.start())..rope.byte_to_char(found.end()))
        .collect()
}

/// The char offsets where the matches of `regex` in `document` start.
fn match_starts(document: &Document, regex: &Regex) -> Vec<usize> {
    let text = document.to_string();
    let rope = document.rope();
    regex
        .find_iter(&text)
        .map(|found| rope.byte_to_char(found.start()))
        .collect()
}

/// Searches for the last pattern again, the other way when `reverse`.
fn search_again(cx: &mut CommandContext<'_>, reverse: bool) {
    let Some(last) = cx.editor.last_search() else {
        return abort(cx, NO_PREVIOUS_PATTERN.to_string());
    };
    let direction = if reverse {
        last.direction.reversed()
    } else {
        last.direction
    };
    let cursor = cx.editor.view_ref(cx.view).cursor();
    jump(cx, direction, cx.count, cursor);
}

/// Searches `direction` for the word at or after the cursor on its line,
/// whole and exactly, from the word's start (an operator still acts from
/// the cursor).
fn search_word(cx: &mut CommandContext<'_>, direction: Direction) {
    let document = cx.editor.document_of(cx.view);
    let Some(word) = word_at(document, cx.editor.view_ref(cx.view).cursor()) else {
        return abort(cx, NO_WORD.to_string());
    };
    let pattern = format!(r"\b{}\b", regex::escape(&document.slice(word.clone())));
    cx.editor.search_history_mut().push(&pattern);
    start_search(
        cx,
        pattern,
        CaseRule::Exact,
        direction,
        cx.count,
        word.start,
    );
}

/// Compiles `pattern` under `case`, makes it the last search, and jumps to
/// its count-th match going `direction`.
fn start_search(
    cx: &mut CommandContext<'_>,
    pattern: String,
    case: CaseRule,
    direction: Direction,
    count: Option<usize>,
    from: usize,
) {
    let regex = match cx.editor.compile_pattern(&pattern, case) {
        Ok(regex) => regex,
        Err(message) => return abort(cx, message),
    };
    cx.editor
        .set_last_search(LastSearch::new(direction, pattern, regex));
    jump(cx, direction, count, from);
}

/// The regular expression a search typed as `text` looks for: `text`
/// itself, or with literal search, `text` matched as it is.
fn typed_pattern(editor: &Editor, text: &str) -> String {
    match editor.options().literal_search {
        true => regex::escape(text),
        false => text.to_string(),
    }
}

/// The word at `cursor`, or the first one after it on its line.
pub(crate) fn word_at(document: &Document, cursor: usize) -> Option<Range<usize>> {
    let end_of_line = line_end(document, cursor);
    let is_word = |at: usize| class_at(document, at) == Some(CharClass::Word);
    let mut start = (cursor..end_of_line).find(|at| is_word(*at))?;
    while start > 0 && is_word(start - 1) {
        start -= 1;
    }
    let mut end = start;
    while is_word(end) {
        end += 1;
    }
    Some(start..end)
}

// ========================================================================
// Tests
// ========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::options::EditorOptions;
    use crate::paint::{Style, Surface};
    use crate::render::{render_prompt, render_view};
    use crate::request::{HostRequest, NoticeLevel, NoticeRequest};
    use crate::testing::{Harness, assert_keys};

    #[test]
    fn test_a_wrapped_search_warns_a_set_value_reports_and_a_missing_match_errs() {
        let mut harness = Harness::new("a |b a");
        let levels = |harness: &mut Harness| -> Vec<NoticeLevel> {
            harness
                .requests()
                .into_iter()
                .filter_map(|request| match request {
                    HostRequest::Notice(NoticeRequest { level, .. }) => Some(level),
                    _ => None,
                })
                .collect()
        };
        harness.keys("/ a <cr> n");
        assert_eq!(
            levels(&mut harness),
            [NoticeLevel::Warning],
            "wrapped to the top"
        );
        harness.keys(": s e t SPC t s ? <cr>");
        assert_eq!(levels(&mut harness), [NoticeLevel::Info]);
        harness.keys("/ z z <cr>");
        assert_eq!(levels(&mut harness), [NoticeLevel::Error]);
    }

    /// The notices `harness` queued, oldest first.
    fn notices(harness: &mut Harness) -> Vec<String> {
        harness
            .requests()
            .into_iter()
            .filter_map(|request| match request {
                HostRequest::Notice(NoticeRequest { message, .. }) => Some(message),
                HostRequest::Cancel(_)
                | HostRequest::Command(_)
                | HostRequest::Open(_)
                | HostRequest::Quit(_)
                | HostRequest::ReadInto(_)
                | HostRequest::Save(_)
                | HostRequest::Submit(_) => None,
            })
            .collect()
    }

    #[test]
    fn test_slash_and_n_search_forward_and_wrap_with_a_notice() {
        let mut harness = Harness::new("|foo x foo\nfoo");
        harness.keys("/ foo <cr>");
        assert_eq!(harness.mode(), Mode::Normal);
        assert_eq!(harness.marked(), "foo x |foo\nfoo");
        harness.keys("n");
        assert_eq!(harness.marked(), "foo x foo\n|foo");
        assert!(notices(&mut harness).is_empty());
        harness.keys("n");
        assert_eq!(harness.marked(), "|foo x foo\nfoo");
        assert_eq!(notices(&mut harness), [WRAPPED_TO_TOP]);
        harness.keys("N");
        assert_eq!(harness.marked(), "foo x foo\n|foo");
        assert_eq!(notices(&mut harness), [WRAPPED_TO_BOTTOM]);
        harness.keys("2n");
        assert_eq!(harness.marked(), "foo x |foo\nfoo", "a count wraps too");
    }

    #[test]
    fn test_question_mark_searches_backward_and_n_keeps_going_backward() {
        let mut harness = Harness::new("ab ab ab|");
        harness.keys("? ab <cr>");
        assert_eq!(harness.marked(), "ab ab |ab");
        harness.keys("n");
        assert_eq!(harness.marked(), "ab |ab ab");
        harness.keys("N");
        assert_eq!(harness.marked(), "ab ab |ab");
    }

    #[test]
    fn test_a_lowercase_pattern_ignores_case_and_an_uppercase_one_does_not() {
        assert_keys("|x Foo foo", "/ foo <cr>", "x |Foo foo");
        assert_keys("|x foo Foo", "/ Foo <cr>", "x foo |Foo");
        assert_keys("|x Foo 9", r"/ \d <cr>", "x Foo |9");
        assert_keys("|a-b", r"/ \W <cr>", "a|-b");
    }

    #[test]
    fn test_patterns_are_regular_expressions_and_an_empty_one_repeats_the_last() {
        assert_keys("|cat cot cut", "/ c[ou]t <cr>", "cat |cot cut");
        assert_keys("|cat cot cut", "/ c[ou]t <cr> / <cr>", "cat cot |cut");
        assert_keys("|a\nb", r"/ ^b <cr>", "a\n|b");
    }

    #[test]
    fn test_vim_word_boundaries_match_only_at_word_edges() {
        assert_keys("|cat concat cat", r"/ \<cat\> <cr>", "cat concat |cat");
        assert_keys("|cats concat", r"/ cat\> <cr>", "cats con|cat");
        assert_keys("|concat cats", r"/ \<cat <cr>", "concat |cats");
    }

    #[test]
    fn test_a_bad_or_unmatched_pattern_stays_put_and_cancels_the_operator() {
        let mut harness = Harness::new("a|bc");
        harness.keys("d / ( <cr>");
        assert_eq!(harness.mode(), Mode::Normal);
        assert_eq!(harness.marked(), "a|bc");
        let messages = notices(&mut harness);
        assert!(messages[0].starts_with("Invalid pattern ("), "{messages:?}");
        harness.keys("/ zz <cr>");
        assert_eq!(notices(&mut harness), ["Pattern not found: zz"]);
        harness.keys("x");
        assert_eq!(harness.marked(), "a|c");
    }

    #[test]
    fn test_star_and_hash_search_the_whole_word_under_the_cursor_exactly() {
        assert_keys("f|oo food Foo foo", "*", "foo food Foo |foo");
        assert_keys("foo food Foo f|oo", "#", "|foo food Foo foo");
        assert_keys("|  bar x bar", "*", "  bar x |bar");
        assert_keys("|foo bar foo", "* n", "|foo bar foo");
        let mut harness = Harness::new("ab |..");
        harness.keys("*");
        assert_eq!(notices(&mut harness), [NO_WORD]);
    }

    #[test]
    fn test_search_is_a_motion_after_an_operator_and_in_visual_mode() {
        assert_keys("|one two three", "d / th <cr>", "|three");
        assert_keys("|one two three", "c / two <cr> X <esc>", "|Xtwo three");
        assert_keys("|a x a x a", "2d / x <cr>", "|x a");
        assert_keys("|one two two", "d * ", "|one two two");
        let mut harness = Harness::new("|one two three");
        harness.keys("v / th <cr> d");
        assert_eq!(harness.marked(), "|hree");
    }

    #[test]
    fn test_matches_paint_in_the_search_style_and_the_prompt_paints_its_line() {
        let mut harness = Harness::new("|ab xab\nab");
        harness.editor.resize_view(harness.view, 8, 2);
        harness.keys("/ ab");
        let mut prompt = Surface::new(0, 0);
        let cursor = render_prompt(&harness.editor, harness.view, &mut prompt).unwrap();
        assert_eq!(prompt.row_text(0), "/ab     ");
        assert_eq!((cursor.column, cursor.row), (3, 0));
        harness.keys("<cr>");
        let mut surface = Surface::new(0, 0);
        render_view(&harness.editor, harness.view, &mut surface);
        let styles: Vec<_> = (0..7)
            .map(|column| surface.cell(column, 0).unwrap().style)
            .collect();
        let (hit, line) = (Style::SearchMatch, Style::CursorLine);
        assert_eq!(styles, [hit, hit, line, line, hit, hit, line]);
        assert_eq!(surface.cell(0, 1).unwrap().style, Style::SearchMatch);
        render_prompt(&harness.editor, harness.view, &mut prompt);
        assert_eq!(prompt.row_text(0), "        ", "closed prompts paint blank");
    }

    #[test]
    fn test_a_cached_pattern_is_reused_only_under_the_same_case_rule() {
        let mut cache = None;
        let smart = compile_cached(&mut cache, "ab", CaseRule::Smart).unwrap();
        assert!(smart.is_match("AB"));
        let again = compile_cached(&mut cache, "ab", CaseRule::Ignore).unwrap();
        assert!(again.is_match("AB"), "the same pattern ignoring case");
        let exact = compile_cached(&mut cache, "ab", CaseRule::Exact).unwrap();
        assert!(!exact.is_match("AB"));
        let other = compile_cached(&mut cache, "cd", CaseRule::Exact).unwrap();
        assert!(other.is_match("cd") && !other.is_match("ab"));
        assert!(compile_cached(&mut cache, "(", CaseRule::Exact).is_err());
    }

    #[test]
    fn test_typing_a_search_moves_to_its_match_and_escape_goes_back() {
        let mut harness = Harness::new("|one two\nthree two");
        harness.keys("/ t w");
        assert_eq!(harness.marked(), "one |two\nthree two", "while typing");
        harness.keys("o x");
        assert_eq!(harness.marked(), "|one two\nthree two", "no match, back");
        harness.keys("<bs> <esc>");
        assert_eq!(harness.marked(), "|one two\nthree two", "Escape goes back");
        harness.keys("? t w o <cr>");
        assert_eq!(harness.marked(), "one two\nthree |two", "one jump, not two");
        harness.keys("g g d / t w <cr>");
        assert_eq!(
            harness.marked(),
            "|two\nthree two",
            "the operator starts at the cursor"
        );
    }

    #[test]
    fn test_incremental_search_can_be_turned_off() {
        let options = EditorOptions {
            incremental_search: false,
            ..EditorOptions::default()
        };
        let mut harness = Harness::with_options("|one two", options);
        harness.keys("/ t w");
        assert_eq!(harness.marked(), "|one two");
    }

    #[test]
    fn test_literal_search_finds_the_text_as_typed_and_colon_s_reuses_it() {
        let options = EditorOptions {
            literal_search: true,
            ..EditorOptions::default()
        };
        let mut harness = Harness::with_options("|abc a.c (A.C)", options);
        harness.keys("/ a . c");
        assert_eq!(harness.marked(), "abc |a.c (A.C)", "followed as typed");
        harness.keys("<cr> n");
        assert_eq!(harness.marked(), "abc a.c (|A.C)", "smart case");
        harness.keys(": s//x/g <cr>");
        assert_eq!(harness.marked(), "|abc x (x)");
        harness.keys("/ ( <cr>");
        assert_eq!(harness.marked(), "abc x |(x)", "no notice of a bad pattern");
    }

    #[test]
    fn test_gn_selects_or_acts_on_the_next_match_and_dot_repeats_cgn() {
        let mut harness = Harness::new("|ab x ab y ab");
        harness.keys("/ a b <cr> g g g n");
        assert_eq!(harness.mode(), Mode::Visual);
        harness.keys("<esc> 0 c g n X <esc>");
        assert_eq!(
            harness.marked(),
            "|X x ab y ab",
            "the match under the cursor"
        );
        harness.keys(". .");
        assert_eq!(harness.marked(), "X x X y |X", "each next match");
        let mut harness = Harness::new("ab x a|b");
        harness.keys("/ a b <cr> $ d g N");
        assert_eq!(
            harness.marked(),
            "ab x| ",
            "back to the match under the cursor"
        );
        let mut harness = Harness::new("|ab x ab");
        harness.keys("/ a b <cr> g g v g n g n d");
        assert_eq!(
            harness.marked(),
            "|",
            "the selection stretched to the next match"
        );
    }

    #[test]
    fn test_the_search_count_is_the_match_at_or_before_the_cursor_of_all() {
        let mut harness = Harness::new("|ab ab ab");
        let count = |harness: &Harness| {
            let count = harness.editor.search_count(harness.view)?;
            Some((count.current, count.total))
        };
        assert_eq!(count(&harness), None, "nothing searched yet");
        harness.keys("/ a b <cr>");
        assert_eq!(count(&harness), Some((2, 3)));
        harness.keys("n n");
        assert_eq!(count(&harness), Some((1, 3)), "wrapped to the first");
        harness.keys("/ b");
        assert_eq!(count(&harness), Some((1, 3)), "the pattern being typed");
        harness.keys("x");
        assert_eq!(count(&harness), Some((0, 0)), "bx matches nothing");
        harness.keys("<esc> : n o h <cr>");
        assert_eq!(count(&harness), None, "hidden matches are not counted");
    }
}
