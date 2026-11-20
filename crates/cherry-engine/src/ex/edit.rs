//! Ex commands over whole lines, each one undo step (see
//! [`super::ex`] for ranges):
//!
//! - `:d [x] [count]` (`:delete`) deletes the range's lines into register
//!   `x` (the unnamed one when left out), `:y [x] [count]` (`:yank`)
//!   yanks them; a count takes that many lines from the range's last.
//! - `:pu [x]` (`:put`) puts register `x` as whole lines below the
//!   range's last line, `:pu!` above it; `:0pu` above the first line.
//! - `:m {address}` (`:move`) moves the range's lines below the address's
//!   line, `:t` (`:co`, `:copy`) copies them there; 0 is above the first
//!   line. Moving lines into themselves is refused.
//! - `:j [count]` (`:join`) joins the range's lines (a line alone with the
//!   next) as `J` does, `:j!` as `gJ` does.
//! - `:>` and `:<` shift the range's lines one indent level per `>` or
//!   `<` typed (`:>>`), as `>` and `<` do, with an optional count.
//! - `:sort [i] [u] [n]` (`:sor`) sorts the range's lines, every line when
//!   no range is given: `i` ignores case, `n` sorts by the first decimal
//!   number on each line (lines without one first, in their order), `u`
//!   keeps one of each run of equal lines; `:sort!` reverses the order.
//! - `:e [file]` (`:edit`) and `:r [file]` (`:read`) ask the host to open
//!   a file or read one into the document (see
//!   [`HostRequest::Open`](crate::HostRequest::Open) and
//!   [`HostRequest::ReadInto`](crate::HostRequest::ReadInto)).
//! - `:u` (`:undo`) and `:red` (`:redo`) undo and redo a step.

use std::cmp::Ordering;

use crate::command::CommandContext;
use crate::document::last_line;
use crate::document::{Document, line_end, line_of, start_of_line};
use crate::edit::join_edit;
use crate::editor::{Editor, ViewId};
use crate::motion::first_non_blank_of_line;
use crate::operator::Lines;
use crate::register::{RegisterKind, RegisterText, Registers};
use crate::request::{HostRequest, OpenRequest, ReadIntoRequest};
use crate::transaction::Edit;
use crate::undo;
use crate::visual::{Shift, shift_line_range};

use super::range::{AddressContext, AddressError, parse_address};
use super::{ExContext, LineRange, refuse_args};

// ========================================================================
// Constants
// ========================================================================

/// The line break between lines.
const LINE_BREAK: char = '\n';

/// The `:sort` flag that ignores case.
const SORT_IGNORE_CASE: char = 'i';

/// The `:sort` flag that sorts by number.
const SORT_NUMERIC: char = 'n';

/// The `:sort` flag that keeps one of equal lines.
const SORT_UNIQUE: char = 'u';

/// The sign before a negative number `:sort n` reads.
const MINUS: char = '-';

/// The name of `:<`, repeated for more levels.
const SHIFT_LEFT: char = '<';

/// The name of `:>`, repeated for more levels.
const SHIFT_RIGHT: char = '>';

/// The base line counts and sorted numbers are written in.
const RADIX: u32 = 10;

// ========================================================================
// Data Structures
// ========================================================================

/// How `:sort` compares lines.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct SortFlags {
    /// Whether case is ignored.
    ignore_case: bool,
    /// Whether lines compare by their first number.
    numeric: bool,
    /// Whether only one of each run of equal lines is kept.
    unique: bool,
}

// ========================================================================
// Functions
// ========================================================================

/// `:t` and `:co`: copies the range's lines below the address's line.
pub(crate) fn copy_lines(ex: &mut ExContext<'_>) {
    let lines = target_lines(ex);
    let Some(below) = destination(ex) else {
        return;
    };
    let document = ex.editor.document_of(ex.view);
    let text = lines_text(document, lines);
    let edit = lines_insertion(document, below, &text);
    ex.editor.apply_edits(ex.view, vec![edit]);
    cursor_to_line(ex.editor, ex.view, below + lines.last - lines.first);
}

/// `:d`: deletes the range's lines into a register; refused, the register
/// kept, in a read-only document.
pub(crate) fn delete_lines(ex: &mut ExContext<'_>) {
    if ex.editor.refuse_if_read_only(ex.view) {
        return;
    }
    let Some((register, lines)) = register_and_lines(ex) else {
        return;
    };
    let document = ex.editor.document_of(ex.view);
    let text = RegisterText::new(RegisterKind::Linewise, lines_text(document, lines));
    let range = Lines {
        first: lines.first,
        last: lines.last,
    }
    .deleted(document);
    ex.editor.registers_mut().record_delete(register, text);
    ex.editor
        .apply_edits(ex.view, vec![Edit::new(range, String::new())]);
    cursor_to_line(ex.editor, ex.view, lines.first);
}

/// `:e`: asks the host to open a file in the view.
pub(crate) fn edit_file(ex: &mut ExContext<'_>) {
    let path = Some(ex.args.trim().to_string()).filter(|path| !path.is_empty());
    ex.editor.request(HostRequest::Open(OpenRequest {
        force: ex.bang,
        path,
        view: ex.view,
    }));
}

/// `:j`: joins the range's lines, or the line with the next.
pub(crate) fn join_lines(ex: &mut ExContext<'_>) {
    let Ok(count) = count_arg(ex.args) else {
        return ex
            .editor
            .error(format!("Trailing characters: {}", ex.args.trim()));
    };
    let mut lines = target_lines(ex);
    if let Some(count) = count {
        lines.first = lines.last;
        lines.last = lines.last.saturating_add(count.saturating_sub(1));
    }
    let document = ex.editor.document_of(ex.view);
    let last = last_line(document);
    let end = lines.last.max(lines.first + 1).min(last);
    let edits: Vec<Edit> = (lines.first..end)
        .map(|line| join_edit(document, line, !ex.bang))
        .collect();
    ex.editor.apply_edits(ex.view, edits);
    cursor_to_line(ex.editor, ex.view, lines.first);
}

/// The insertion that puts `text`, whole lines with or without a final
/// line break, below line `below` of `document` (counted from 1; 0 for
/// above the first line), with the line breaks it needs.
pub(crate) fn lines_insertion(document: &Document, below: usize, text: &str) -> Edit {
    let body = text.strip_suffix(LINE_BREAK).unwrap_or(text);
    let ends_in_break = document.len_lines() - 1 > last_line(document);
    if below <= last_line(document) {
        let at = start_of_line(document, below);
        return Edit::new(at..at, format!("{body}{LINE_BREAK}"));
    }
    let at = document.len_chars();
    if ends_in_break {
        Edit::new(at..at, format!("{body}{LINE_BREAK}"))
    } else {
        Edit::new(at..at, format!("{LINE_BREAK}{body}"))
    }
}

/// `:m`: moves the range's lines below the address's line.
pub(crate) fn move_lines(ex: &mut ExContext<'_>) {
    let lines = target_lines(ex);
    let Some(below) = destination(ex) else {
        return;
    };
    if below > lines.first && below <= lines.last {
        return ex
            .editor
            .error("Cannot move a range of lines into itself".to_string());
    }
    let size = lines.last - lines.first;
    if below == lines.first || below == lines.last + 1 {
        return cursor_to_line(ex.editor, ex.view, lines.first + size);
    }
    let document = ex.editor.document_of(ex.view);
    let text = lines_text(document, lines);
    let deletion = Edit::new(
        Lines {
            first: lines.first,
            last: lines.last,
        }
        .deleted(document),
        String::new(),
    );
    let insertion = lines_insertion(document, below, &text);
    let (edits, last) = if below > lines.last {
        (vec![deletion, insertion], below - 1)
    } else {
        (vec![insertion, deletion], below + size)
    };
    ex.editor.apply_edits(ex.view, edits);
    cursor_to_line(ex.editor, ex.view, last);
}

/// `:pu`: puts a register's text as whole lines below the range's last
/// line, or above it with `!`.
pub(crate) fn put_lines(ex: &mut ExContext<'_>) {
    let args = ex.args.trim();
    let mut chars = args.chars();
    let register = chars.next().filter(|name| Registers::is_name(*name));
    if register.is_none() && !args.is_empty() || !chars.as_str().trim().is_empty() {
        return ex.editor.error(format!("Trailing characters: {args}"));
    }
    let Some(text) = ex.editor.read_register(ex.view, register) else {
        let name = register.unwrap_or('"');
        return ex.editor.error(format!("Nothing in register {name}"));
    };
    let lines = target_lines(ex);
    let below = match (ex.line_zero, ex.bang) {
        (true, _) => 0,
        (false, true) => lines.last,
        (false, false) => lines.last + 1,
    };
    let document = ex.editor.document_of(ex.view);
    let body = text.text.strip_suffix(LINE_BREAK).unwrap_or(&text.text);
    let put_lines = body.split(LINE_BREAK).count();
    let edit = lines_insertion(document, below, &text.text);
    ex.editor.apply_edits(ex.view, vec![edit]);
    cursor_to_line(ex.editor, ex.view, below + put_lines - 1);
}

/// `:r`: asks the host to read a file into the document below the
/// range's last line.
pub(crate) fn read_file(ex: &mut ExContext<'_>) {
    let path = Some(ex.args.trim().to_string()).filter(|path| !path.is_empty());
    let line = if ex.line_zero {
        0
    } else {
        target_lines(ex).last + 1
    };
    ex.editor.request(HostRequest::ReadInto(ReadIntoRequest {
        line,
        path,
        view: ex.view,
    }));
}

/// `:red`: redoes a step.
pub(crate) fn redo_step(ex: &mut ExContext<'_>) {
    if refuse_args(ex) {
        return;
    }
    undo::redo(&mut command_context(ex));
}

/// `:<`: shifts the range's lines left an indent level, and one more per
/// `<` repeated (`:<<`).
pub(crate) fn shift_left(ex: &mut ExContext<'_>) {
    shift_lines(ex, Shift::Left);
}

/// `:>`: shifts the range's lines right an indent level, and one more per
/// `>` repeated (`:>>`).
pub(crate) fn shift_right(ex: &mut ExContext<'_>) {
    shift_lines(ex, Shift::Right);
}

/// `:sort`: sorts the range's lines, every line without a range.
pub(crate) fn sort_lines(ex: &mut ExContext<'_>) {
    let mut flags = SortFlags::default();
    for c in ex.args.chars().filter(|c| !c.is_whitespace()) {
        match c {
            SORT_IGNORE_CASE => flags.ignore_case = true,
            SORT_NUMERIC => flags.numeric = true,
            SORT_UNIQUE => flags.unique = true,
            _ => {
                return ex
                    .editor
                    .error(format!("Invalid argument: {}", ex.args.trim()));
            }
        }
    }
    let document = ex.editor.document_of(ex.view);
    let lines = ex.lines.unwrap_or(LineRange {
        first: 0,
        last: last_line(document),
    });
    let old: Vec<String> = (lines.first..=lines.last)
        .map(|line| document.line_text(line).unwrap_or_default())
        .collect();
    let mut sorted = old.clone();
    sorted.sort_by(|a, b| compare_lines(a, b, flags));
    if ex.bang {
        sorted.reverse();
    }
    if flags.unique {
        sorted.dedup_by(|b, a| compare_lines(a, b, flags) == Ordering::Equal);
    }
    if sorted == old {
        return;
    }
    let start = start_of_line(document, lines.first);
    let end = line_end(document, start_of_line(document, lines.last));
    let text = sorted.join(&LINE_BREAK.to_string());
    ex.editor
        .apply_edits(ex.view, vec![Edit::new(start..end, text)]);
    cursor_to_line(ex.editor, ex.view, lines.first);
}

/// `:u`: undoes a step.
pub(crate) fn undo_step(ex: &mut ExContext<'_>) {
    if refuse_args(ex) {
        return;
    }
    undo::undo(&mut command_context(ex));
}

/// `:y`: yanks the range's lines into a register.
pub(crate) fn yank_lines(ex: &mut ExContext<'_>) {
    let Some((register, lines)) = register_and_lines(ex) else {
        return;
    };
    let document = ex.editor.document_of(ex.view);
    let text = RegisterText::new(RegisterKind::Linewise, lines_text(document, lines));
    ex.editor.registers_mut().record_yank(register, text);
}

/// The context a Normal command runs with from the ex command `ex`.
fn command_context<'a>(ex: &'a mut ExContext<'_>) -> CommandContext<'a> {
    CommandContext {
        count: None,
        editor: ex.editor,
        register: None,
        view: ex.view,
    }
}

/// `a` against `b` as `:sort` with `flags` orders them.
fn compare_lines(a: &str, b: &str, flags: SortFlags) -> Ordering {
    if flags.numeric {
        return first_number(a).cmp(&first_number(b));
    }
    if flags.ignore_case {
        return a.to_lowercase().cmp(&b.to_lowercase());
    }
    a.cmp(b)
}

/// The count typed as the whole of `args`, if any; an error when `args`
/// holds anything else.
fn count_arg(args: &str) -> Result<Option<usize>, ()> {
    let args = args.trim();
    if args.is_empty() {
        return Ok(None);
    }
    if !args.chars().all(|c| c.is_digit(RADIX)) {
        return Err(());
    }
    Ok(Some(args.parse().unwrap_or(usize::MAX)))
}

/// Moves the cursor to the first non-blank of line `line`, kept inside
/// the document.
fn cursor_to_line(editor: &mut Editor, view: ViewId, line: usize) {
    let document = editor.document_of(view);
    let start = start_of_line(document, line.min(last_line(document)));
    let target = first_non_blank_of_line(editor, view, start);
    editor.set_cursor(view, target);
}

/// The line, counted from 1 (0 above the first), the address `:m` and `:t`
/// take as their argument names; `None`, with a notice, when it is
/// missing or malformed.
fn destination(ex: &mut ExContext<'_>) -> Option<usize> {
    let mut rest = ex.args.trim();
    let mut addresses = AddressContext::new(ex.editor, ex.view);
    let parsed = parse_address(&mut rest, &mut addresses);
    let searched = addresses.searched;
    let message = match parsed {
        Ok(Some(line)) if rest.trim().is_empty() => {
            if let Some(search) = searched {
                ex.editor.set_last_search(search);
            }
            return Some(line);
        }
        Ok(Some(_)) => format!("Trailing characters: {}", rest.trim()),
        Ok(None) | Err(AddressError::InvalidRange) => {
            format!("Invalid address: {}", ex.args.trim())
        }
        Err(AddressError::Message(message)) => message,
    };
    ex.editor.error(message);
    None
}

/// The first decimal number on `line`, with a `-` right before it, for
/// `:sort n`; `None` when it has none, which sorts first.
fn first_number(line: &str) -> Option<i64> {
    let start = line.find(|c: char| c.is_digit(RADIX))?;
    let digits = line[start..]
        .find(|c: char| !c.is_digit(RADIX))
        .map_or(&line[start..], |end| &line[start..start + end]);
    let negative = line[..start].ends_with(MINUS);
    let value: i64 = digits.parse().unwrap_or(i64::MAX);
    Some(if negative { -value } else { value })
}

/// The text of `lines` of `document`, each with its line break but the
/// last.
fn lines_text(document: &Document, lines: LineRange) -> String {
    let start = start_of_line(document, lines.first);
    let end = line_end(document, start_of_line(document, lines.last));
    document.slice(start..end)
}

/// The register and lines `:d` and `:y` act on: a register name, then a
/// count taking that many lines from the range's last; `None`, with a
/// notice, when the arguments are neither.
fn register_and_lines(ex: &mut ExContext<'_>) -> Option<(Option<char>, LineRange)> {
    let args = ex.args.trim();
    let mut chars = args.chars();
    let (register, rest) = match chars.next() {
        Some(name) if Registers::is_name(name) && !name.is_ascii_digit() => {
            (Some(name), chars.as_str())
        }
        Some(_) | None => (None, args),
    };
    let Ok(count) = count_arg(rest) else {
        ex.editor.error(format!("Trailing characters: {args}"));
        return None;
    };
    let lines = with_count(ex, target_lines(ex), count);
    Some((register, lines))
}

/// Shifts the range's lines the way `shift` says, a level for the name
/// and one per name char repeated at the start of the arguments, which
/// may end in a count.
fn shift_lines(ex: &mut ExContext<'_>, shift: Shift) {
    let name = match shift {
        Shift::Left => SHIFT_LEFT,
        Shift::Right => SHIFT_RIGHT,
    };
    let args = ex.args.trim_start();
    let repeated = args.chars().take_while(|c| *c == name).count();
    let Ok(count) = count_arg(&args[repeated * name.len_utf8()..]) else {
        return ex.editor.error(format!("Trailing characters: {args}"));
    };
    let lines = with_count(ex, target_lines(ex), count);
    shift_line_range(
        ex.editor,
        ex.view,
        (lines.first, lines.last),
        repeated + 1,
        shift,
    );
    cursor_to_line(ex.editor, ex.view, lines.last);
}

/// The lines an ex command acts on: its range, or the cursor's line.
fn target_lines(ex: &ExContext<'_>) -> LineRange {
    ex.lines.unwrap_or_else(|| {
        let document = ex.editor.document_of(ex.view);
        let line = line_of(document, ex.editor.view_ref(ex.view).cursor());
        let line = line.min(last_line(document));
        LineRange {
            first: line,
            last: line,
        }
    })
}

/// `lines`, or with a count the count of lines from their last, kept
/// inside the document.
fn with_count(ex: &ExContext<'_>, lines: LineRange, count: Option<usize>) -> LineRange {
    let Some(count) = count else {
        return lines;
    };
    let last = last_line(ex.editor.document_of(ex.view));
    LineRange {
        first: lines.last,
        last: lines.last.saturating_add(count.saturating_sub(1)).min(last),
    }
}

// ========================================================================
// Tests
// ========================================================================

#[cfg(test)]
mod tests {
    use crate::request::{HostRequest, NoticeRequest, OpenRequest, ReadIntoRequest};
    use crate::testing::{Harness, assert_keys};

    /// The notices among `requests`, oldest first.
    fn notices(requests: Vec<HostRequest>) -> Vec<String> {
        requests
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
    fn test_delete_and_yank_take_a_register_and_a_count() {
        assert_keys("|a\nb\nc\nd", ": 2d <cr> p", "a\nc\n|b\nd");
        assert_keys(
            "|a\nb\nc\nd",
            ": d SPC x SPC 2 <cr> j \" x p",
            "c\nd\n|a\nb",
        );
        assert_keys("a\nb\n|c", ": d <cr>", "a\n|b");
        assert_keys("|a\nb\nc", ": 2,3y <cr> P", "|b\nc\na\nb\nc");
        let mut harness = Harness::new("|a");
        harness.keys(": d SPC x! <cr>");
        assert_eq!(notices(harness.requests()), ["Trailing characters: x!"]);
    }

    #[test]
    fn test_put_puts_whole_lines_below_above_or_before_the_first() {
        assert_keys("|a\nb", "y w : pu <cr>", "a\n|a\nb");
        assert_keys("a\n|b", "y w : pu! <cr>", "a\n|b\nb");
        assert_keys("a\n|b", "y w : 0pu <cr>", "|b\na\nb");
        assert_keys("|a\nb", "\" x y y : $pu SPC x <cr>", "a\nb\n|a");
        let mut harness = Harness::new("|a");
        harness.keys(": pu SPC q <cr>");
        assert_eq!(notices(harness.requests()), ["Nothing in register q"]);
    }

    #[test]
    fn test_move_and_copy_put_lines_below_an_address() {
        let text = "|a\nb\nc\nd";
        assert_keys(text, ": m$ <cr>", "b\nc\nd\n|a");
        assert_keys(text, ": 3,4m0 <cr>", "c\n|d\na\nb");
        assert_keys(text, ": 1,2m3 <cr>", "c\na\n|b\nd");
        assert_keys(text, ": t. <cr>", "a\n|a\nb\nc\nd");
        assert_keys(text, ": 1,2co$ <cr>", "a\nb\nc\nd\na\n|b");
        assert_keys(text, ": m/c/ <cr>", "b\nc\n|a\nd");
        let mut harness = Harness::new(text);
        harness.keys(": 1,3m2 <cr> : m <cr> : m1x <cr>");
        assert_eq!(
            notices(harness.requests()),
            [
                "Cannot move a range of lines into itself",
                "Invalid address: ",
                "Trailing characters: x",
            ]
        );
        assert_eq!(harness.marked(), text);
    }

    #[test]
    fn test_join_and_shift_act_on_the_range() {
        assert_keys("|a\n  b\nc", ": j <cr>", "|a b\nc");
        assert_keys("|a\n  b\nc", ": %j! <cr>", "|a  bc");
        assert_keys("|a\nb\nc\nd", ": j SPC 3 <cr>", "|a b c\nd");
        assert_keys("|a\nb", ": %> <cr>", "\ta\n\t|b");
        assert_keys("|a\nb", ": >> SPC 2 <cr>", "\t\ta\n\t\t|b");
        assert_keys("\t\t|a", ": < <cr>", "\t|a");
    }

    #[test]
    fn test_sort_orders_lines_by_text_case_or_number() {
        assert_keys("|c\na\nB\nb", ": sort <cr>", "|B\na\nb\nc");
        assert_keys("|c\na\nB\nb", ": sort! <cr>", "|c\nb\na\nB");
        assert_keys("|c\na\nB\nb", ": sort SPC i <cr>", "|a\nB\nb\nc");
        assert_keys("|b\na\nb\na", ": sort SPC u <cr>", "|a\nb");
        assert_keys(
            "|x10\nx9\nnone\nx-2",
            ": sort SPC n <cr>",
            "|none\nx-2\nx9\nx10",
        );
        assert_keys("|z\nc\na\nb", ": 2,$sort <cr>", "z\n|a\nb\nc");
        let mut harness = Harness::new("|a");
        harness.keys(": sort SPC q <cr>");
        assert_eq!(notices(harness.requests()), ["Invalid argument: q"]);
    }

    #[test]
    fn test_edit_and_read_become_host_requests_and_insert_lines_takes_text() {
        let mut harness = Harness::new("a\n|b");
        let view = harness.view;
        harness.keys(": e SPC x.txt <cr> : e! <cr> : r SPC y.txt <cr> : 0r <cr>");
        assert_eq!(
            harness.requests(),
            [
                HostRequest::Open(OpenRequest {
                    force: false,
                    path: Some("x.txt".to_string()),
                    view,
                }),
                HostRequest::Open(OpenRequest {
                    force: true,
                    path: None,
                    view,
                }),
                HostRequest::ReadInto(ReadIntoRequest {
                    line: 2,
                    path: Some("y.txt".to_string()),
                    view,
                }),
                HostRequest::ReadInto(ReadIntoRequest {
                    line: 0,
                    path: None,
                    view,
                }),
            ]
        );
        harness.editor.insert_lines(view, 1, "  x\ny\n");
        assert_eq!(harness.marked(), "a\n  |x\ny\nb");
        harness.editor.insert_lines(view, 99, "z");
        assert_eq!(harness.marked(), "a\n  x\ny\nb\n|z");
        harness.keys("u u");
        assert_eq!(harness.marked(), "a\n|b");
    }

    #[test]
    fn test_undo_and_redo_commands_step_through_the_history() {
        assert_keys("|a b", "x x : u <cr>", "| b");
        assert_keys("|a b", "x x : u <cr> : undo <cr> : red <cr>", "| b");
        let mut harness = Harness::new("|a");
        harness.keys(": u SPC 2 <cr>");
        assert_eq!(notices(harness.requests()), ["Trailing characters: 2"]);
    }
}
