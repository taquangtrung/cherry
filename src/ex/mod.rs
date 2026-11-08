//! The ex command line: `:` opens a prompt (see [`super::prompt`]) for a
//! command such as `:w`, `:%s/a/b/g`, or `:set ts=8`, run when submitted.
//!
//! # Syntax
//!
//! `[range]name[!] [args]`. A range is `%` (every line) or one address or
//! two joined by `,` or `;` (`3`, `.`, `$`, `2,$`, `.,+5`, `'a,'b`,
//! `/x/;/y/`), see [`range`] for every address; line numbers
//! count from 1, a number past the end means the last line, and a backward
//! range is turned around. `:` from a visual mode types `'<,'>`, the
//! selected lines, and after a count `.,.+n`, that many lines. The name is
//! a run of ASCII letters, typed whole (no abbreviations beyond the
//! built-in aliases), so `:s/a/b/` reads as `s` with the arguments
//! `/a/b/`; `<` and `>` are names of one char. A range with no name goes
//! to its last line. However many edits a command makes, it is one undo
//! step.
//!
//! # Built-in commands
//!
//! - `:w` (`:write`) asks the host to save; `:q` (`:quit`) asks it to
//!   close the view, `:q!` forcibly, discarding changes. A plain `:q` on
//!   the last view of a modified document (see
//!   [`Editor::is_modified`](crate::Editor::is_modified)) gives a notice
//!   instead. `:wq` asks to save and then to close; `:x` (`:xit`) the
//!   same, but saves only a modified document.
//! - `:s` (`:substitute`), see [`super::substitute`].
//! - `:d`, `:y`, `:pu`, `:m`, `:t`, `:j`, `:>`, `:<`, `:sort`, `:e`,
//!   `:r`, `:u`, and `:red`, see [`edit`].
//! - `:g`, `:v`, and `:norm`, see [`global`].
//! - `:noh` (`:nohlsearch`) stops painting the last search's matches until
//!   the next search.
//! - `:set` (`:se`) changes [`EditorOptions`]: `tabstop`/`ts`,
//!   `shiftwidth`/`sw`, `expandtab`/`et`, `wrap`, `number`/`nu`,
//!   `autoindent`/`ai`, and `autopairs`/`ap`. A
//!   switch is set by name, unset with `no`, toggled with `!` or `inv`;
//!   a number is set with `=`; `?` (or a bare number option) shows the
//!   value in a notice. Several may follow one `:set`.
//!
//! An unknown name, or a range or argument a command cannot take, gives a
//! notice. A host adds ex commands with
//! [`Editor::register_ex_command`](crate::Editor::register_ex_command).

mod edit;
mod global;
mod range;

use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

use super::command::{CommandContext, CommandError};
use super::editor::{Editor, ViewId};
use super::motion::goto_line;
use super::options::EditorOptions;
use super::prompt::{self, PromptKind};
use super::request;
use super::search::LastSearch;
use super::substitute;
use super::view::{Mode, is_visual};

use self::range::{AddressContext, AddressError, parse_range};

pub(crate) use self::edit::lines_insertion;
pub(crate) use self::global::LineAnchors;

// ========================================================================
// Constants
// ========================================================================

/// The ex commands the editor registers, by name.
const BUILTIN_EX_COMMANDS: &[(&str, BuiltinExFn)] = &[
    ("<", edit::shift_left),
    (">", edit::shift_right),
    ("co", edit::copy_lines),
    ("copy", edit::copy_lines),
    ("d", edit::delete_lines),
    ("delete", edit::delete_lines),
    ("e", edit::edit_file),
    ("edit", edit::edit_file),
    ("g", global::global),
    ("global", global::global),
    ("j", edit::join_lines),
    ("join", edit::join_lines),
    ("m", edit::move_lines),
    ("move", edit::move_lines),
    ("noh", nohlsearch),
    ("nohlsearch", nohlsearch),
    ("norm", global::normal),
    ("normal", global::normal),
    ("pu", edit::put_lines),
    ("put", edit::put_lines),
    ("q", quit),
    ("quit", quit),
    ("r", edit::read_file),
    ("read", edit::read_file),
    ("red", edit::redo_step),
    ("redo", edit::redo_step),
    ("s", substitute::substitute),
    ("se", set),
    ("set", set),
    ("sor", edit::sort_lines),
    ("sort", edit::sort_lines),
    ("substitute", substitute::substitute),
    ("t", edit::copy_lines),
    ("u", edit::undo_step),
    ("undo", edit::undo_step),
    ("v", global::vglobal),
    ("vglobal", global::vglobal),
    ("w", write),
    ("wq", write_quit),
    ("write", write),
    ("x", exit),
    ("xit", exit),
    ("y", edit::yank_lines),
    ("yank", edit::yank_lines),
];

/// The prefix that unsets a switch option.
const UNSET_PREFIX: &str = "no";

/// The prefix that toggles a switch option.
const TOGGLE_PREFIX: &str = "inv";

/// The suffix that toggles a switch option.
const TOGGLE_SUFFIX: char = '!';

/// The suffix that shows an option's value.
const QUERY_SUFFIX: char = '?';

/// What joins a number option's name and value.
const ASSIGN: char = '=';

/// What follows a command name to force it.
const BANG: char = '!';

/// What may come before the command, as Vim allows `::w`.
const COLON: char = ':';

/// The range `:` types from a visual mode: the lines of the selection.
const VISUAL_RANGE: &str = "'<,'>";

/// The address of the cursor's line, which `:` after a count types.
const CURRENT_LINE_RANGE: char = '.';

/// The names of one char, read apart from letter names; the name repeated
/// after it is its argument (`:>>` shifts right twice).
const SHIFT_NAMES: [char; 2] = ['<', '>'];

// ========================================================================
// Data Structures
// ========================================================================

/// The function a built-in ex command runs.
type BuiltinExFn = fn(&mut ExContext<'_>);

/// The function an ex command name runs. Shared, so it can be called while
/// the editor it is called with is borrowed mutably.
pub(crate) type ExFn = Arc<dyn Fn(&mut ExContext<'_>) + Send + Sync>;

/// What an ex command runs with: the editor, the view it was typed in, and
/// what was typed with its name.
#[derive(Debug)]
pub struct ExContext<'a> {
    /// What follows the name (and its `!`), leading blanks dropped:
    /// `"/a/b/g"` for `:s/a/b/g`, `"ts=8"` for `:set ts=8`.
    pub args: &'a str,
    /// Whether a `!` followed the name, forcing it.
    pub bang: bool,
    /// The editor the command acts on.
    pub editor: &'a mut Editor,
    /// Whether the range's last address was line 0, before the first
    /// line (`:0put`), which [`ExContext::lines`] gives as the first line.
    pub(crate) line_zero: bool,
    /// The lines the range before the name gave, if one was typed.
    pub lines: Option<LineRange>,
    /// The view the command was typed in.
    pub view: ViewId,
}

/// Ex command names and the functions they run.
#[derive(Clone)]
pub(crate) struct ExRegistry {
    /// The function of each registered name.
    commands: HashMap<String, ExFn>,
}

/// A run of whole lines given as an ex range, from the first to the last,
/// both counted from 0 and inside the document.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct LineRange {
    /// The first line.
    pub first: usize,
    /// The last line, never before the first.
    pub last: usize,
}

/// A command line taken apart.
#[derive(Debug)]
struct ParsedLine<'a> {
    /// What follows the name and its `!`, leading blanks dropped.
    args: &'a str,
    /// Whether a `!` followed the name.
    bang: bool,
    /// Whether the range's last address was line 0.
    line_zero: bool,
    /// The range, if one was typed.
    lines: Option<LineRange>,
    /// The name; empty when none was typed.
    name: &'a str,
    /// The search a pattern address made, to become the last search.
    searched: Option<LastSearch>,
}

/// An option `:set` knows.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum OptionName {
    /// `autoindent`: open lines with the indent of the line before.
    AutoIndent,
    /// `autopairs`: type brackets and quotes in pairs.
    AutoPairs,
    /// `expandtab`: indent with spaces only.
    ExpandTab,
    /// `incsearch`: follow a search pattern as it is typed.
    IncSearch,
    /// `matchbrackets`: paint the bracket matching the one at the cursor.
    MatchBrackets,
    /// `number`: show line numbers.
    Number,
    /// `relativenumber`: count line numbers from the cursor's line.
    RelativeNumber,
    /// `shiftwidth`: the cells of one indent level.
    ShiftWidth,
    /// `tabstop`: the cells between tab stops.
    TabStop,
    /// `textwidth`: the cells `gq` refills lines to.
    TextWidth,
    /// `wrap`: soft wrap long lines.
    Wrap,
}

// ========================================================================
// ExRegistry Implementations
// ========================================================================

impl ExRegistry {
    /// The registry of the editor's own ex commands.
    pub fn new() -> Self {
        let commands = BUILTIN_EX_COMMANDS
            .iter()
            .map(|(name, run)| (name.to_string(), Arc::new(*run) as ExFn))
            .collect();
        Self { commands }
    }

    /// The function `name` runs, or `None` when no ex command has it.
    pub fn get(&self, name: &str) -> Option<ExFn> {
        self.commands.get(name).cloned()
    }

    /// Registers `run` under `name`. Rejected when `name` is taken or is
    /// not a run of ASCII letters.
    pub fn register(&mut self, name: String, run: ExFn) -> Result<(), CommandError> {
        if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphabetic()) {
            return Err(CommandError::InvalidExName(name));
        }
        if self.commands.contains_key(&name) {
            return Err(CommandError::DuplicateId(name));
        }
        self.commands.insert(name, run);
        Ok(())
    }
}

impl fmt::Debug for ExRegistry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut names: Vec<_> = self.commands.keys().collect();
        names.sort();
        f.debug_struct("ExRegistry")
            .field("commands", &names)
            .finish()
    }
}

// ========================================================================
// OptionName Implementations
// ========================================================================

impl OptionName {
    /// The option named `name`, in full or short.
    fn parse(name: &str) -> Option<Self> {
        let option = match name {
            "autoindent" | "ai" => Self::AutoIndent,
            "autopairs" | "ap" => Self::AutoPairs,
            "expandtab" | "et" => Self::ExpandTab,
            "incsearch" | "is" => Self::IncSearch,
            "matchbrackets" | "mb" => Self::MatchBrackets,
            "number" | "nu" => Self::Number,
            "relativenumber" | "rnu" => Self::RelativeNumber,
            "shiftwidth" | "sw" => Self::ShiftWidth,
            "tabstop" | "ts" => Self::TabStop,
            "textwidth" | "tw" => Self::TextWidth,
            "wrap" => Self::Wrap,
            _ => return None,
        };
        Some(option)
    }

    /// The full name, for showing the value.
    fn full_name(self) -> &'static str {
        match self {
            Self::AutoIndent => "autoindent",
            Self::AutoPairs => "autopairs",
            Self::ExpandTab => "expandtab",
            Self::IncSearch => "incsearch",
            Self::MatchBrackets => "matchbrackets",
            Self::Number => "number",
            Self::RelativeNumber => "relativenumber",
            Self::ShiftWidth => "shiftwidth",
            Self::TabStop => "tabstop",
            Self::TextWidth => "textwidth",
            Self::Wrap => "wrap",
        }
    }

    /// The switch this option is in `options`, or `None` for a number.
    fn switch(self, options: &mut EditorOptions) -> Option<&mut bool> {
        match self {
            Self::AutoIndent => Some(&mut options.auto_indent),
            Self::AutoPairs => Some(&mut options.auto_pairs),
            Self::ExpandTab => Some(&mut options.expand_tab),
            Self::IncSearch => Some(&mut options.incremental_search),
            Self::MatchBrackets => Some(&mut options.match_brackets),
            Self::Number => Some(&mut options.line_numbers),
            Self::RelativeNumber => Some(&mut options.relative_line_numbers),
            Self::Wrap => Some(&mut options.wrap),
            Self::ShiftWidth | Self::TabStop | Self::TextWidth => None,
        }
    }

    /// The number this option is in `options`, or `None` for a switch.
    fn number(self, options: &mut EditorOptions) -> Option<&mut usize> {
        match self {
            Self::ShiftWidth => Some(&mut options.shift_width),
            Self::TabStop => Some(&mut options.tab_width),
            Self::TextWidth => Some(&mut options.text_width),
            Self::AutoIndent
            | Self::AutoPairs
            | Self::ExpandTab
            | Self::IncSearch
            | Self::MatchBrackets
            | Self::Number
            | Self::RelativeNumber
            | Self::Wrap => None,
        }
    }
}

// ========================================================================
// Functions
// ========================================================================

/// `:`: opens the ex command line. From a visual mode it leaves the mode
/// and types the range of the selected lines (`'<,'>`); after a count it
/// types the range of that many lines from the cursor's (`.,.+2`).
pub(crate) fn enter_command_line(cx: &mut CommandContext<'_>) {
    let range = if is_visual(cx.editor.view_ref(cx.view).mode()) {
        cx.editor.set_mode(cx.view, Mode::Normal);
        Some(VISUAL_RANGE.to_string())
    } else {
        cx.count.map(|count| match count {
            0 | 1 => CURRENT_LINE_RANGE.to_string(),
            count => format!("{CURRENT_LINE_RANGE},{CURRENT_LINE_RANGE}+{}", count - 1),
        })
    };
    prompt::open(cx, PromptKind::Command);
    if let Some(range) = range {
        prompt::type_text(cx.editor, cx.view, &range);
    }
}

/// Runs the command `line`, submitted on the ex command line of the view,
/// as one undo step however many edits it makes. An ex command is never a
/// change for `.` to repeat.
pub(crate) fn run_line(cx: &mut CommandContext<'_>, line: &str) {
    cx.editor.view_mut(cx.view).set_change(None);
    let parsed = match parse_line(line, cx.editor, cx.view) {
        Ok(parsed) => parsed,
        Err(message) => return cx.editor.error(message),
    };
    if let Some(search) = parsed.searched {
        cx.editor.set_last_search(search);
    }
    if parsed.name.is_empty() {
        if !parsed.args.trim().is_empty() {
            return cx
                .editor
                .error(format!("Not an editor command: {}", line.trim()));
        }
        if let Some(lines) = parsed.lines {
            goto_line(cx, lines.last);
        }
        return;
    }
    let Some(run) = cx.editor.ex_command(parsed.name) else {
        return cx
            .editor
            .error(format!("Not an editor command: {}", line.trim()));
    };
    let checkpoint = cx.editor.history_mut(cx.view).checkpoint();
    run(&mut ExContext {
        args: parsed.args,
        bang: parsed.bang,
        editor: cx.editor,
        line_zero: parsed.line_zero,
        lines: parsed.lines,
        view: cx.view,
    });
    if let Err(err) = cx.editor.history_mut(cx.view).merge_since(checkpoint) {
        cx.editor.error(err.to_string());
    }
    cx.editor.set_last_ex_command(line.trim().to_string());
}

/// `:x`: asks the host to save the view's document when it is modified,
/// then to close the view.
fn exit(ex: &mut ExContext<'_>) {
    if refuse_args(ex) {
        return;
    }
    request::save_if_modified_and_quit(ex.editor, ex.view);
}

/// `:noh`: stops painting the last search's matches.
fn nohlsearch(ex: &mut ExContext<'_>) {
    ex.editor.set_highlight_search(false);
}

/// `line` taken apart as the module documentation describes, its range
/// read in `view`.
fn parse_line<'a>(line: &'a str, editor: &Editor, view: ViewId) -> Result<ParsedLine<'a>, String> {
    let mut rest = line.trim_start().trim_start_matches(COLON).trim_start();
    let mut addresses = AddressContext::new(editor, view);
    let range = parse_range(&mut rest, &mut addresses).map_err(|error| match error {
        AddressError::InvalidRange => format!("Invalid range: {}", line.trim()),
        AddressError::Message(message) => message,
    })?;
    let lines = range.map(|(first, last)| LineRange {
        first: first.max(1) - 1,
        last: last.max(1) - 1,
    });
    let rest = rest.trim_start();
    let name_len = match rest.chars().next() {
        Some(shift) if SHIFT_NAMES.contains(&shift) => shift.len_utf8(),
        Some(_) | None => rest
            .find(|c: char| !c.is_ascii_alphabetic())
            .unwrap_or(rest.len()),
    };
    let (name, rest) = rest.split_at(name_len);
    let (bang, rest) = match rest.strip_prefix(BANG) {
        Some(after) => (true, after),
        None => (false, rest),
    };
    Ok(ParsedLine {
        args: rest.trim_start(),
        bang,
        line_zero: range.is_some_and(|(_, last)| last == 0),
        lines,
        name,
        searched: addresses.searched,
    })
}

/// `:q`: asks the host to close the view, forcibly with `!`; refused with
/// a notice when that would leave a modified document in no view.
fn quit(ex: &mut ExContext<'_>) {
    if refuse_args(ex) {
        return;
    }
    request::quit_unless_unsaved(ex.editor, ex.view, ex.bang);
}

/// Says so and returns true when the command was given arguments it does
/// not take.
pub(crate) fn refuse_args(ex: &mut ExContext<'_>) -> bool {
    let args = ex.args.trim();
    if args.is_empty() {
        return false;
    }
    let message = format!("Trailing characters: {args}");
    ex.editor.error(message);
    true
}

/// `:set`: sets, unsets, toggles, or shows each option named in the
/// arguments, as the module documentation describes.
fn set(ex: &mut ExContext<'_>) {
    let args = ex.args.to_string();
    for arg in args.split_whitespace() {
        match set_option(ex.editor.options_mut(), arg) {
            Ok(Some(value)) => ex.editor.info(value),
            Ok(None) => {}
            Err(message) => return ex.editor.error(message),
        }
    }
}

/// Applies one `:set` argument `arg` to `options`; returns the value to
/// show for a query, or why `arg` is refused.
fn set_option(options: &mut EditorOptions, arg: &str) -> Result<Option<String>, String> {
    let unknown = || format!("Unknown option: {arg}");
    let invalid = || format!("Invalid argument: {arg}");
    let (body, query) = match arg.strip_suffix(QUERY_SUFFIX) {
        Some(body) => (body, true),
        None => (arg, false),
    };
    if let Some((name, value)) = body.split_once(ASSIGN) {
        let option = OptionName::parse(name).ok_or_else(unknown)?;
        let number = option.number(options).ok_or_else(invalid)?;
        *number = value.parse().map_err(|_| invalid())?;
        return Ok(None);
    }
    if let Some(option) = OptionName::parse(body) {
        let name = option.full_name();
        if let Some(number) = option.number(options) {
            return Ok(Some(format!("{name}={number}")));
        }
        let switch = option.switch(options).ok_or_else(invalid)?;
        if query {
            let prefix = if *switch { "" } else { UNSET_PREFIX };
            return Ok(Some(format!("{prefix}{name}")));
        }
        *switch = true;
        return Ok(None);
    }
    let (name, change): (&str, fn(bool) -> bool) =
        if let Some(name) = body.strip_suffix(TOGGLE_SUFFIX) {
            (name, |on| !on)
        } else if let Some(name) = body.strip_prefix(TOGGLE_PREFIX) {
            (name, |on| !on)
        } else if let Some(name) = body.strip_prefix(UNSET_PREFIX) {
            (name, |_| false)
        } else {
            return Err(unknown());
        };
    let option = OptionName::parse(name).ok_or_else(unknown)?;
    let switch = option.switch(options).ok_or_else(invalid)?;
    *switch = change(*switch);
    Ok(None)
}

/// `:w`: asks the host to save the view's document.
fn write(ex: &mut ExContext<'_>) {
    if refuse_args(ex) {
        return;
    }
    request::request_save(ex.editor, ex.view);
}

/// `:wq`: asks the host to save the view's document, then to close the
/// view, forcibly with `!`.
fn write_quit(ex: &mut ExContext<'_>) {
    if refuse_args(ex) {
        return;
    }
    request::request_save(ex.editor, ex.view);
    request::request_quit(ex.editor, ex.view, ex.bang);
}

// ========================================================================
// Tests
// ========================================================================

#[cfg(test)]
mod tests {
    use std::slice;

    use super::*;
    use crate::paint::{Style, Surface};
    use crate::render::render_view;
    use crate::request::{HostRequest, NoticeRequest, QuitRequest, SaveRequest, UNSAVED_NOTICE};
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
    fn test_write_and_quit_commands_become_host_requests_in_order() {
        let mut harness = Harness::new("|text");
        let document = harness.editor.view(harness.view).unwrap().document();
        let view = harness.view;
        let save = HostRequest::Save(SaveRequest { document });
        let quit = |force| HostRequest::Quit(QuitRequest { force, view });
        harness.keys(": w <cr>");
        assert_eq!(harness.requests(), slice::from_ref(&save));
        harness.keys(": q <cr> : q! <cr> : quit! <cr>");
        assert_eq!(harness.requests(), [quit(false), quit(true), quit(true)]);
        harness.keys(": wq <cr> : write <cr> : x <cr>");
        let both = [save.clone(), quit(false), save, quit(false)];
        assert_eq!(harness.requests(), both, ":x saves only a modified text");
        harness.keys(": w SPC other.txt <cr>");
        let refused = notices(harness.requests());
        assert_eq!(refused, ["Trailing characters: other.txt"]);
    }

    #[test]
    fn test_quit_refuses_a_modified_document_until_it_is_saved_or_forced() {
        let mut harness = Harness::new("|text");
        let document = harness.editor.view(harness.view).unwrap().document();
        let view = harness.view;
        let save = HostRequest::Save(SaveRequest { document });
        let quit = |force| HostRequest::Quit(QuitRequest { force, view });
        harness.keys("x");
        assert!(harness.editor.is_modified(document));
        harness.keys(": q <cr>");
        assert_eq!(notices(harness.requests()), [UNSAVED_NOTICE]);
        harness.keys(": x <cr>");
        assert_eq!(harness.requests(), [save, quit(false)]);
        harness.editor.mark_saved(document);
        assert!(!harness.editor.is_modified(document));
        harness.keys(": q <cr>");
        assert_eq!(harness.requests(), [quit(false)]);
        harness.keys("x : q <cr>");
        assert_eq!(notices(harness.requests()), [UNSAVED_NOTICE]);
        harness.keys("u : q <cr>");
        assert_eq!(
            harness.requests(),
            [quit(false)],
            "undone to the saved text"
        );
        harness.keys("x");
        let other = harness.editor.new_view(document).unwrap();
        harness.keys(": q <cr>");
        assert_eq!(harness.requests(), [quit(false)], "another view keeps it");
        assert!(harness.editor.close_view(other));
        harness.keys(": q <cr>");
        assert_eq!(
            notices(harness.requests()),
            [UNSAVED_NOTICE],
            "the last view"
        );
    }

    #[test]
    fn test_a_range_alone_goes_to_its_line() {
        assert_keys("|a\n  b\nc", ": 2 <cr>", "a\n  |b\nc");
        assert_keys("|a\nb\nc", ": $ <cr>", "a\nb\n|c");
        assert_keys("|a\nb\nc", ": 99 <cr>", "a\nb\n|c");
        assert_keys("a\n|b\nc", ": . <cr>", "a\n|b\nc");
        assert_keys("|a\nb\nc", ": 3,1 <cr>", "a\nb\n|c");
    }

    #[test]
    fn test_unknown_commands_and_bad_ranges_say_so() {
        let mut harness = Harness::new("|a");
        harness.keys(": frob <cr> : 1, <cr> : 2# <cr>");
        let messages = notices(harness.requests());
        assert_eq!(
            messages,
            [
                "Not an editor command: frob",
                "Invalid range: 1,",
                "Not an editor command: 2#",
            ]
        );
    }

    #[test]
    fn test_noh_hides_the_search_matches_until_the_next_search() {
        let mut harness = Harness::new("|ab ab");
        harness.editor.resize_view(harness.view, 5, 1);
        let mut surface = Surface::new(0, 0);
        let first_style = |harness: &Harness, surface: &mut Surface| {
            render_view(&harness.editor, harness.view, surface);
            surface.cell(0, 0).unwrap().style
        };
        harness.keys("/ ab <cr>");
        assert_eq!(first_style(&harness, &mut surface), Style::SearchMatch);
        harness.keys(": noh <cr>");
        assert_eq!(first_style(&harness, &mut surface), Style::CursorLine);
        harness.keys("n");
        assert_eq!(first_style(&harness, &mut surface), Style::SearchMatch);
    }

    #[test]
    fn test_set_switches_numbers_toggles_and_queries_options() {
        let mut harness = Harness::new("|a");
        harness.keys(": set SPC ts=8 SPC sw=2 SPC et SPC nu SPC wrap <cr>");
        let options = *harness.editor.options();
        assert_eq!((options.tab_width, options.shift_width), (8, 2));
        assert!(options.expand_tab && options.line_numbers && options.wrap);
        harness.keys(": se SPC noet SPC wrap! SPC invnumber SPC noai SPC ap <cr>");
        let options = *harness.editor.options();
        assert!(!options.expand_tab && !options.wrap && !options.line_numbers);
        assert!(!options.auto_indent && options.auto_pairs);
        harness.keys(": set SPC ts? SPC et? SPC tabstop <cr>");
        let shown = notices(harness.requests());
        assert_eq!(shown, ["tabstop=8", "noexpandtab", "tabstop=8"]);
        harness.keys(": set SPC bogus <cr> : set SPC ts=x <cr> : set SPC et=1 <cr>");
        let refused = notices(harness.requests());
        assert_eq!(
            refused,
            [
                "Unknown option: bogus",
                "Invalid argument: ts=x",
                "Invalid argument: et=1",
            ]
        );
    }

    #[test]
    fn test_a_host_ex_command_gets_its_range_bang_and_arguments() {
        let mut harness = Harness::new("a\n|b\nc\nd");
        let echo = |ex: &mut ExContext<'_>| {
            let message = format!("{:?} {} {:?}", ex.lines, ex.bang, ex.args);
            ex.editor.error(message);
        };
        harness.editor.register_ex_command("echo", echo).unwrap();
        harness.keys(": .,$echo! SPC hi SPC there <cr> : echo <cr>");
        let messages = notices(harness.requests());
        assert_eq!(
            messages,
            [
                "Some(LineRange { first: 1, last: 3 }) true \"hi there\"",
                "None false \"\"",
            ]
        );
        let noop = |_: &mut ExContext<'_>| {};
        assert_eq!(
            harness.editor.register_ex_command("w", noop),
            Err(CommandError::DuplicateId("w".to_string()))
        );
        assert_eq!(
            harness.editor.register_ex_command("go-to", noop),
            Err(CommandError::InvalidExName("go-to".to_string()))
        );
    }

    #[test]
    fn test_the_command_line_keeps_a_history_apart_from_searches() {
        let mut harness = Harness::new("|a a");
        harness.keys("/ a <cr> : set SPC ts=2 <cr> : <up>");
        let prompt = harness.editor.view(harness.view).unwrap().prompt().unwrap();
        assert_eq!(prompt.text(), "set ts=2");
        harness.keys("<esc> / <up>");
        let prompt = harness.editor.view(harness.view).unwrap().prompt().unwrap();
        assert_eq!(prompt.text(), "a");
    }
}
