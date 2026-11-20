//! `:s/pattern/replacement/flags`: replaces matches of a pattern on the
//! lines of a range, the cursor's line when none is given (`:%s` for every
//! line, `:2,5s`, `:.,$s`).
//!
//! The pattern is a search pattern (see [`super::search`]): a regular
//! expression with smart case, matched within each line; an empty pattern
//! is the last search's, and the pattern becomes the last search. The
//! delimiter is the first character after `s`, any but a letter, a digit,
//! a blank, or `\`; a backslash before it puts it in literally. The
//! replacement is written as in Vim: `&` or `\0` is the whole match, `\1`
//! to `\9` are groups, `\n` or `\r` breaks the line, `\t` is a tab, and a
//! backslash before any other character (`\&`, `\\`) puts it in
//! literally. Flags: `g` replaces every match on a line, not just the
//! first; `i` ignores case, `I` matches it exactly; `c` asks at each.
//!
//! # Confirming
//!
//! With `c`, each match in turn is selected in Visual mode, a notice asks
//! what to do, and the next key answers: `y` replaces it and goes on, `n`
//! leaves it and goes on, `a` replaces it and every match after it, `l`
//! replaces it and stops, and `q`, Escape, or any other key stops; Emacs's
//! Space, `!`, and `.` answer as `y`, `a`, and `l` do. A view that never
//! leaves Insert mode selects each match there instead. The
//! replacements made are one undo step, and a notice counts them. `M-%`
//! opens the command line with `.,$s/` typed, to replace from the cursor's
//! line to the end with the pattern and replacement typed next (and `gc`
//! to confirm each). In the Emacs and standard editing styles, `M-%` and
//! Ctrl+H ask for the text to replace and what to replace it with, as
//! they are, in two prompts, and then ask at each match from the cursor
//! on.
//!
//! All the replacements are one undo step; the cursor goes to the first
//! non-blank of the last line changed. A pattern that matches nothing
//! gives a notice, and so does a substitution over more than one line.
//!
//! `&` in Normal mode repeats the last substitution on the cursor's line,
//! with the same pattern and replacement but none of its flags.

use std::mem;
use std::ops::Range;

use regex::Regex;

use super::command::{CommandContext, PendingChar};
use super::document::{Document, Position, last_line, line_of, start_of_line};
use super::editor::{Editor, ViewId};
use super::ex::{ExContext, LineRange};
use super::motion::first_non_blank_of_line;
use super::prompt::{self, PromptKind};
use super::search::{CaseRule, Direction, LastSearch, NO_PREVIOUS_PATTERN};
use super::selection::{self, Selection};
use super::transaction::Edit;
use super::view::Mode;

// ========================================================================
// Constants
// ========================================================================

/// The char that escapes the next one.
const ESCAPE: char = '\\';

/// The replacement char that stands for the whole match.
const WHOLE_MATCH: char = '&';

/// The flag that replaces every match on a line.
const FLAG_GLOBAL: char = 'g';

/// The flag that ignores case.
const FLAG_IGNORE_CASE: char = 'i';

/// The flag that matches case exactly.
const FLAG_EXACT_CASE: char = 'I';

/// The flag that confirms each replacement.
const FLAG_CONFIRM: char = 'c';

/// What `M-%` types on the command line.
const QUERY_REPLACE_START: &str = ".,$s/";

/// The answers to a confirming substitution's question.
const ANSWER_ALL: char = 'a';
const ANSWER_LAST: char = 'l';
const ANSWER_NO: char = 'n';
const ANSWER_YES: char = 'y';

/// Emacs's answers to a query: replace, replace all, replace and stop.
const ANSWER_YES_EMACS: char = ' ';
const ANSWER_ALL_EMACS: char = '!';
const ANSWER_LAST_EMACS: char = '.';

/// The character that starts a group reference in a replacement of the
/// `regex` crate, and how a literal one is written there.
const REPLACEMENT_SIGIL: &str = "$";
const REPLACEMENT_SIGIL_ESCAPED: &str = "$$";

// ========================================================================
// Data Structures
// ========================================================================

/// A substitution as typed: pattern, replacement, and flags; the last one
/// run is kept for `&`, its pattern filled in.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Substitution {
    /// How the pattern treats case.
    case: CaseRule,
    /// Whether each replacement is asked about first.
    confirm: bool,
    /// Whether every match on a line is replaced.
    global: bool,
    /// The pattern; empty for the last search's.
    pattern: String,
    /// The replacement, in the `regex` crate's syntax.
    replacement: String,
}

/// A substitution asking about each match: where it looks next, the
/// lines it covers, and how far it got.
#[derive(Clone, Debug)]
pub(crate) struct QueryReplace {
    /// Where the cursor goes when the query ends: where it started, or
    /// at the start of the last match replaced.
    cursor: usize,
    /// The char offset the next match is looked for from.
    from: usize,
    /// Whether the replacements' undo group is open.
    grouped: bool,
    /// The last line of the range, carried past line breaks replaced in.
    last_line: usize,
    /// The pattern, compiled.
    regex: Regex,
    /// How many matches were replaced.
    replaced: usize,
    /// The substitution typed.
    substitution: Substitution,
}

// ========================================================================
// Functions
// ========================================================================

/// `&`: repeats the last substitution on the cursor's line, without its
/// flags.
pub(crate) fn repeat_substitute(cx: &mut CommandContext<'_>) {
    let Some(last) = cx.editor.last_substitution() else {
        return cx
            .editor
            .error("No previous substitute regular expression".to_string());
    };
    let substitution = Substitution {
        case: CaseRule::Smart,
        confirm: false,
        global: false,
        ..last.clone()
    };
    run_substitution(cx.editor, cx.view, None, substitution);
}

/// `:s`: replaces matches on the range's lines, as the module
/// documentation describes.
pub(crate) fn substitute(ex: &mut ExContext<'_>) {
    let mut substitution = match parse_substitution(ex.args) {
        Ok(substitution) => substitution,
        Err(message) => return ex.editor.error(message),
    };
    if substitution.pattern.is_empty() {
        match ex.editor.last_search() {
            Some(last) => substitution.pattern = last.pattern().to_string(),
            None => return ex.editor.error(NO_PREVIOUS_PATTERN.to_string()),
        }
    }
    run_substitution(ex.editor, ex.view, ex.lines, substitution);
}

/// Runs `substitution`, its pattern filled in, on `lines` (the cursor's
/// line when `None`) of the document of `view`, and keeps it for `&`.
fn run_substitution(
    editor: &mut Editor,
    view: ViewId,
    lines: Option<LineRange>,
    substitution: Substitution,
) {
    let pattern = substitution.pattern.clone();
    let regex = match editor.compile_pattern(&pattern, substitution.case) {
        Ok(regex) => regex,
        Err(message) => return editor.error(message),
    };
    let document = editor.document_of(view);
    let lines = lines.unwrap_or_else(|| {
        let line = line_of(document, editor.view_ref(view).cursor());
        LineRange {
            first: line,
            last: line,
        }
    });
    if substitution.confirm {
        let query = QueryReplace {
            cursor: editor.view_ref(view).cursor(),
            from: start_of_line(document, lines.first),
            grouped: false,
            last_line: lines.last,
            regex: regex.clone(),
            replaced: 0,
            substitution: substitution.clone(),
        };
        editor.set_last_search(LastSearch::new(Direction::Forward, pattern.clone(), regex));
        editor.set_last_substitution(Substitution {
            confirm: false,
            ..substitution
        });
        return ask_next(editor, view, query);
    }
    let edits = replacements(document, &regex, &substitution, lines);
    editor.set_last_search(LastSearch::new(Direction::Forward, pattern.clone(), regex));
    editor.set_last_substitution(substitution);
    let Some(last_edit) = edits.last() else {
        return editor.error(format!("Pattern not found: {pattern}"));
    };
    let growth: isize = edits
        .iter()
        .take(edits.len() - 1)
        .map(|edit| edit.text.chars().count() as isize - edit.range.len() as isize)
        .sum();
    let last_start = last_edit.range.start.saturating_add_signed(growth);
    let changed = edits.len();
    editor.apply_edits(view, edits);
    let cursor = first_non_blank_of_line(editor, view, last_start);
    editor.set_cursor(view, cursor);
    if changed > 1 {
        editor.error(format!("Substituted on {changed} lines"));
    }
}

/// `M-%`: opens the command line with `.,$s/` typed.
pub(crate) fn query_replace(cx: &mut CommandContext<'_>) {
    prompt::open(cx, PromptKind::Command);
    prompt::type_text(cx.editor, cx.view, QUERY_REPLACE_START);
}

/// `M-%` in the Emacs style and Ctrl+H in the standard style: asks for
/// the text to replace and what to replace it with, then for each match
/// from the cursor on.
pub(crate) fn query_replace_text(cx: &mut CommandContext<'_>) {
    prompt::open(cx, PromptKind::ReplaceFrom);
}

/// Replaces `from`, as it is, with `to`, as it is, at each match from the
/// cursor of `view` to the end, asking at each as `:s` with `c` does.
pub(crate) fn query_replace_literal(editor: &mut Editor, view: ViewId, from: &str, to: &str) {
    if from.is_empty() {
        return;
    }
    let substitution = Substitution {
        case: CaseRule::Smart,
        confirm: true,
        global: true,
        pattern: regex::escape(from),
        replacement: to.replace(REPLACEMENT_SIGIL, REPLACEMENT_SIGIL_ESCAPED),
    };
    let regex = match editor.compile_pattern(&substitution.pattern, substitution.case) {
        Ok(regex) => regex,
        Err(message) => return editor.error(message),
    };
    let cursor = editor.view_ref(view).cursor();
    let query = QueryReplace {
        cursor,
        from: cursor,
        grouped: false,
        last_line: last_line(editor.document_of(view)),
        regex,
        replaced: 0,
        substitution,
    };
    ask_next(editor, view, query);
}

/// Selects the next match of `query` and asks about it, or finishes when
/// there is none. A view that never leaves Insert mode selects it there;
/// any other selects it in Visual mode.
fn ask_next(editor: &mut Editor, view: ViewId, query: QueryReplace) {
    let Some(found) = next_match(editor.document_of(view), &query) else {
        return finish_query(editor, view, &query);
    };
    if editor.is_insert_locked(view) {
        let selection = Selection::single(selection::Range::new(found.start, found.end));
        editor.view_mut(view).set_selection(selection);
    } else {
        let last = editor.document_of(view).prev_grapheme_boundary(found.end);
        editor.set_mode(view, Mode::Visual);
        editor.set_visual_anchor(view, found.start);
        editor.set_cursor(view, last.max(found.start));
    }
    editor.info("Replace? (y/n/a/q/l)".to_string());
    editor.view_mut(view).set_query_replace(Some(query));
    let pending = PendingChar {
        count: None,
        register: None,
        run: answer,
    };
    editor.view_mut(view).set_pending_char(Some(pending));
}

/// Acts on `key`, the answer to the match the query selects, as the
/// module docs say.
fn answer(cx: &mut CommandContext<'_>, key: char) {
    let Some(mut query) = cx.editor.view_mut(cx.view).take_query_replace() else {
        return;
    };
    let Some(found) = next_match(cx.editor.document_of(cx.view), &query) else {
        return finish_query(cx.editor, cx.view, &query);
    };
    match key {
        ANSWER_YES | ANSWER_YES_EMACS => {
            replace_one(cx.editor, cx.view, &mut query, found);
            ask_next(cx.editor, cx.view, query);
        }
        ANSWER_NO => {
            query.from = past(cx.editor.document_of(cx.view), &query, found.end);
            ask_next(cx.editor, cx.view, query);
        }
        ANSWER_ALL | ANSWER_ALL_EMACS => {
            let mut found = Some(found);
            while let Some(range) = found {
                replace_one(cx.editor, cx.view, &mut query, range);
                found = next_match(cx.editor.document_of(cx.view), &query);
            }
            finish_query(cx.editor, cx.view, &query);
        }
        ANSWER_LAST | ANSWER_LAST_EMACS => {
            replace_one(cx.editor, cx.view, &mut query, found);
            finish_query(cx.editor, cx.view, &query);
        }
        _ => finish_query(cx.editor, cx.view, &query),
    }
}

/// Replaces the match `found` of `query` in the document of `view`, as a
/// part of the query's one undo step, and moves past it.
fn replace_one(editor: &mut Editor, view: ViewId, query: &mut QueryReplace, found: Range<usize>) {
    let document = editor.document_of(view);
    let line = line_of(document, found.start);
    let start = start_of_line(document, line);
    let text = document.line_text(line).unwrap_or_default();
    let byte = |offset: usize| {
        text.char_indices()
            .nth(offset - start)
            .map_or(text.len(), |(byte, _)| byte)
    };
    let mut replaced = String::new();
    if let Some(captures) = query.regex.captures_at(&text, byte(found.start)) {
        captures.expand(&query.substitution.replacement, &mut replaced);
    }
    if !query.grouped {
        editor.history_mut(view).begin_group();
        query.grouped = true;
    }
    editor.apply_edits(view, vec![Edit::new(found.clone(), replaced.clone())]);
    query.replaced += 1;
    query.cursor = found.start;
    query.last_line += replaced.matches('\n').count();
    let end = found.start + replaced.chars().count();
    query.from = past(editor.document_of(view), query, end);
}

/// Where the next match of `query` is looked for, after a match ending at
/// `end`: there, or at the next line's start without the `g` flag.
fn past(document: &Document, query: &QueryReplace, end: usize) -> usize {
    match query.substitution.global {
        true => end,
        false => {
            let line = line_of(document, end) + 1;
            match line < document.len_lines() {
                true => start_of_line(document, line),
                false => document.len_chars(),
            }
        }
    }
}

/// The next match of `query` at or after its `from`, within its lines.
fn next_match(document: &Document, query: &QueryReplace) -> Option<Range<usize>> {
    let first = line_of(document, query.from);
    let last = query.last_line.min(last_line(document));
    for line in first..=last {
        let start = start_of_line(document, line);
        let text = document.line_text(line)?;
        let column = query.from.saturating_sub(start);
        let byte = text
            .char_indices()
            .nth(column)
            .map_or(text.len(), |(byte, _)| byte);
        let Some(found) = query
            .regex
            .find_at(&text, byte)
            .filter(|_| query.from <= start + text.chars().count())
        else {
            continue;
        };
        let chars = |bytes: usize| text[..bytes].chars().count();
        return Some(start + chars(found.start())..start + chars(found.end()));
    }
    None
}

/// Ends the confirming substitution of `view` asking about a match, if
/// any, as an answer that stops it does: for a key that is no char.
pub(crate) fn stop_query(editor: &mut Editor, view: ViewId) {
    if let Some(query) = editor.view_mut(view).take_query_replace() {
        finish_query(editor, view, &query);
    }
}

/// Ends `query`: back in Normal mode, the replacements one undo step, and
/// a notice of how many were made.
fn finish_query(editor: &mut Editor, view: ViewId, query: &QueryReplace) {
    if query.grouped {
        editor.history_mut(view).end_group();
    }
    if !editor.is_insert_locked(view) {
        editor.set_mode(view, Mode::Normal);
    }
    editor.set_cursor(view, query.cursor);
    let count = query.replaced;
    let noun = if count == 1 {
        "substitution"
    } else {
        "substitutions"
    };
    editor.info(format!("{count} {noun}"));
}

/// The replacement written as in Vim, in the `regex` crate's syntax.
fn expand_replacement(vim: &str) -> String {
    let mut expanded = String::new();
    let mut chars = vim.chars();
    while let Some(c) = chars.next() {
        match c {
            WHOLE_MATCH => expanded.push_str("${0}"),
            '$' => expanded.push_str("$$"),
            ESCAPE => match chars.next() {
                Some(digit @ '0'..='9') => expanded.push_str(&format!("${{{digit}}}")),
                Some('n' | 'r') => expanded.push('\n'),
                Some('t') => expanded.push('\t'),
                Some('$') => expanded.push_str("$$"),
                Some(other) => expanded.push(other),
                None => expanded.push(ESCAPE),
            },
            c => expanded.push(c),
        }
    }
    expanded
}

/// The fields of `args` split on its first char, the delimiter: at most
/// three, a backslash before the delimiter dropped.
fn split_fields(args: &str) -> Result<Vec<String>, String> {
    let mut chars = args.chars();
    let delimiter = chars
        .next()
        .ok_or_else(|| "Usage: :s/pattern/replacement/flags".to_string())?;
    if delimiter.is_alphanumeric() || delimiter.is_whitespace() || delimiter == ESCAPE {
        return Err(format!("Not a delimiter: {delimiter}"));
    }
    let mut fields = Vec::new();
    let mut field = String::new();
    while let Some(c) = chars.next() {
        match c {
            ESCAPE => match chars.next() {
                Some(next) if next == delimiter => field.push(next),
                Some(next) => {
                    field.push(ESCAPE);
                    field.push(next);
                }
                None => field.push(ESCAPE),
            },
            c if c == delimiter && fields.len() < 2 => fields.push(mem::take(&mut field)),
            c => field.push(c),
        }
    }
    fields.push(field);
    Ok(fields)
}

/// `args`, what follows `:s`, as a substitution.
fn parse_substitution(args: &str) -> Result<Substitution, String> {
    let mut fields = split_fields(args)?.into_iter();
    let pattern = fields.next().unwrap_or_default();
    let replacement = expand_replacement(&fields.next().unwrap_or_default());
    let mut substitution = Substitution {
        case: CaseRule::Smart,
        confirm: false,
        global: false,
        pattern,
        replacement,
    };
    for flag in fields.next().unwrap_or_default().trim_end().chars() {
        match flag {
            FLAG_CONFIRM => substitution.confirm = true,
            FLAG_EXACT_CASE => substitution.case = CaseRule::Exact,
            FLAG_GLOBAL => substitution.global = true,
            FLAG_IGNORE_CASE => substitution.case = CaseRule::Ignore,
            other => return Err(format!("Trailing characters: {other}")),
        }
    }
    Ok(substitution)
}

/// The edits replacing the matches of `regex` on `lines`, one per line
/// changed, in order.
fn replacements(
    document: &Document,
    regex: &Regex,
    substitution: &Substitution,
    lines: LineRange,
) -> Vec<Edit> {
    let mut edits = Vec::new();
    for line in lines.first..=lines.last {
        let (Some(text), Some(start)) = (
            document.line_text(line),
            document.position_to_char(Position { column: 0, line }),
        ) else {
            continue;
        };
        if !regex.is_match(&text) {
            continue;
        }
        let replaced = if substitution.global {
            regex.replace_all(&text, substitution.replacement.as_str())
        } else {
            regex.replace(&text, substitution.replacement.as_str())
        };
        let end = start + text.chars().count();
        edits.push(Edit::new(start..end, replaced.into_owned()));
    }
    edits
}

// ========================================================================
// Tests
// ========================================================================

#[cfg(test)]
mod tests {
    use crate::request::{HostRequest, NoticeRequest};
    use crate::testing::{Harness, assert_keys};

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
    fn test_the_c_flag_asks_at_each_match_and_the_answers_steer_it() {
        assert_keys("|a a\na", ": %s/a/b/gc <cr> y n y", "b a\n|b");
        assert_keys("|a a\na", ": %s/a/b/gc <cr> n a", "a b\n|b");
        assert_keys("|a a\na", ": %s/a/b/gc <cr> l", "|b a\na");
        assert_keys("|a a\na", ": %s/a/b/c <cr> y y", "b a\n|b");
        assert_keys("|a a\na", ": %s/a/b/gc <cr> y <esc>", "|b a\na");
        assert_keys("|a a\na", ": %s/a/b/gc <cr> a u", "|a a\na");
    }

    #[test]
    fn test_s_replaces_the_first_match_on_the_line_or_every_one_with_g() {
        assert_keys("a a\n|a a\na a", ": s/a/b/ <cr>", "a a\n|b a\na a");
        assert_keys("a a\n|a a\na a", ": s/a/b/g <cr>", "a a\n|b b\na a");
        assert_keys("|  a a\na", ": s/a/b <cr>", "  |b a\na");
    }

    #[test]
    fn test_ranges_pick_the_lines_and_the_cursor_ends_on_the_last_changed() {
        assert_keys("|x\nx\nx\nx", ": %s/x/y/ <cr>", "y\ny\ny\n|y");
        assert_keys("|x\nx\nx\nx", ": 2,3s/x/y/ <cr>", "x\ny\n|y\nx");
        assert_keys("x\n|x\nx\nz", ": .,$s/x/y/ <cr>", "x\ny\n|y\nz");
    }

    #[test]
    fn test_the_replacement_takes_vim_escapes_groups_and_line_breaks() {
        assert_keys(
            "|foo bar",
            r": s/(\w+) SPC (\w+)/\2 SPC \1/ <cr>",
            "|bar foo",
        );
        assert_keys("|ab", r": s/b/[&]/ <cr>", "|a[b]");
        assert_keys("|ab", r": s/b/\&$1/ <cr>", "|a&$1");
        assert_keys("|a,b", r": s/,/\r/ <cr>", "|a\nb");
        assert_keys("|a/b", r": s#/#\## <cr>", "|a#b");
        assert_keys("|a/b", r": s/\//-/ <cr>", "|a-b");
    }

    #[test]
    fn test_case_follows_smart_case_and_the_i_and_capital_i_flags() {
        assert_keys("|Foo foo", ": s/foo/x/ <cr>", "|x foo");
        assert_keys("|foo Foo", ": s/Foo/x/ <cr>", "|foo x");
        assert_keys("|Foo foo", ": s/foo/x/I <cr>", "|Foo x");
        assert_keys("|foo FOO", ": s/FOO/x/i <cr>", "|x FOO");
    }

    #[test]
    fn test_an_empty_pattern_is_the_last_search_and_s_becomes_the_last_search() {
        assert_keys("|ab ab\nab", "/ b <cr> : %s//X/g <cr>", "aX aX\n|aX");
        assert_keys("|xa ya\nya", ": s/y/Z/ <cr> n", "xa Za\n|ya");
    }

    #[test]
    fn test_a_substitution_is_one_undo_step_and_failures_say_why() {
        let mut harness = Harness::new("|a\na\na");
        harness.keys(": %s/a/bb/ <cr>");
        assert_eq!(harness.marked(), "bb\nbb\n|bb");
        assert_eq!(notices(&mut harness), ["Substituted on 3 lines"]);
        harness.keys("u");
        assert_eq!(harness.marked(), "|a\na\na");
        harness.keys(": s/q/r/ <cr> : s/a/b/z <cr> : s <cr>");
        assert_eq!(
            notices(&mut harness),
            [
                "Pattern not found: q",
                "Trailing characters: z",
                "Usage: :s/pattern/replacement/flags",
            ]
        );
        assert_eq!(harness.marked(), "|a\na\na");
    }

    #[test]
    fn test_ampersand_repeats_the_last_substitute_on_the_line_without_flags() {
        let mut harness = Harness::new("|a a\na a\nb");
        harness.keys(": s/a/x/g <cr> j &");
        assert_eq!(harness.marked(), "x x\n|x a\nb");
        harness.keys("j &");
        let notices = harness.requests();
        assert!(
            notices
                .iter()
                .any(|request| matches!(request, HostRequest::Notice(notice) if notice.message.starts_with("Pattern not found"))),
            "{notices:?}"
        );
        let mut fresh = Harness::new("|a");
        fresh.keys("&");
        assert_eq!(fresh.marked(), "|a");
        assert!(matches!(
            fresh.requests().as_slice(),
            [HostRequest::Notice(NoticeRequest { message, .. })] if message.contains("No previous")
        ));
    }
}
