//! Prompts: the one-line input at the bottom of a view where a search
//! pattern is typed after `/` or `?`, or an ex command after `:`.
//!
//! While a prompt is open, its view is in [`Mode::CommandLine`] and keys
//! edit the prompt's line with the same Emacs line editing as Insert mode
//! (see [`super::line_edit`]), the kill ring included; `<up>` and `<down>`
//! (`C-p`, `C-n`) recall earlier entries of the prompt's history, the
//! line typed so far coming back past the newest. `C-r` and a register
//! name types that register's text; `C-r C-w` types the word under the
//! view's cursor. `<cr>` submits the line
//! and `<esc>` (`C-g`, `C-c`) cancels it; so does `<bs>` on an empty line.
//! Either way the view returns to the mode the prompt was opened from: an
//! operator waiting for its motion keeps waiting through a search prompt
//! (`d/foo<cr>`), and a visual selection stays selected. Cancelling an
//! operator's prompt cancels the operator too.
//!
//! A host shows the prompt with
//! [`render_prompt`](crate::render_prompt), or reads it from
//! [`View::prompt`](crate::View::prompt) to draw it its own way.

use std::mem;
use std::ops::Range;

use regex::Regex;

use super::command::{CommandContext, await_char};
use super::document::Document;
use super::editor::{Editor, ViewId};
use super::ex;
use super::line_edit::{LineChange, LineCommand, plan};
use super::register::{RegisterKind, Registers};
use super::search::{self, Direction};
use super::view::{Mode, ScrollOffset};

// ========================================================================
// Constants
// ========================================================================

/// The most entries a prompt history keeps; the oldest goes first.
const HISTORY_CAPACITY: usize = 100;

/// A line break, which a prompt's single line cannot hold.
const LINE_BREAK: char = '\n';

/// What a line break yanked or typed into a prompt becomes.
const LINE_BREAK_STAND_IN: char = ' ';

/// The char `C-w` gives after `C-r`, which types the word under the
/// view's cursor.
const WORD_UNDER_CURSOR: char = '\u{17}';

/// The prompt commands that are plain line editing, by id.
pub(crate) const PROMPT_LINE_COMMANDS: &[(&str, LineCommand)] = &[
    ("prompt-backward-char", LineCommand::BackwardChar),
    ("prompt-backward-kill-word", LineCommand::BackwardKillWord),
    ("prompt-backward-word", LineCommand::BackwardWord),
    ("prompt-capitalize-word", LineCommand::CapitalizeWord),
    ("prompt-delete-forward", LineCommand::DeleteChar),
    ("prompt-downcase-word", LineCommand::DowncaseWord),
    ("prompt-forward-char", LineCommand::ForwardChar),
    ("prompt-forward-word", LineCommand::ForwardWord),
    ("prompt-kill-line", LineCommand::KillLine),
    ("prompt-kill-to-line-start", LineCommand::KillToLineStart),
    ("prompt-kill-word", LineCommand::KillWord),
    ("prompt-line-end", LineCommand::LineEnd),
    ("prompt-line-start", LineCommand::LineStart),
    ("prompt-transpose-chars", LineCommand::TransposeChars),
    ("prompt-transpose-words", LineCommand::TransposeWords),
    ("prompt-unix-word-rubout", LineCommand::UnixWordRubout),
    ("prompt-upcase-word", LineCommand::UpcaseWord),
];

// ========================================================================
// Data Structures
// ========================================================================

/// An open prompt: what it is for, the line typed, and the cursor in it.
#[derive(Clone, Debug)]
pub struct Prompt {
    /// The count typed before the key that opened the prompt.
    count: Option<usize>,
    /// The char offset of the cursor in the line.
    cursor: usize,
    /// Where a search prompt's typing has taken the view.
    follow: SearchFollow,
    /// What the prompt is for.
    kind: PromptKind,
    /// The line typed, one line long.
    line: Document,
    /// Where recalling the prompt's history has got to.
    recall: Recall,
    /// The mode the view returns to when the prompt closes.
    return_mode: Mode,
}

/// What a prompt is for.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[non_exhaustive]
pub enum PromptKind {
    /// An ex command, opened with `:`.
    Command,
    /// A pattern to search backward for, opened with `?`.
    SearchBackward,
    /// A pattern to search forward for, opened with `/`.
    SearchForward,
}

/// What incremental search has done while a search pattern is typed:
/// where the view was before, to go back to, and the pattern followed.
#[derive(Clone, Debug, Default)]
pub(crate) struct SearchFollow {
    /// The pattern last followed, compiled; `None` when it is empty or does
    /// not compile.
    pub regex: Option<Regex>,
    /// The cursor and scroll of the view when the prompt opened.
    pub origin: Option<(usize, ScrollOffset)>,
    /// The line last followed, so an unchanged line is not searched again.
    pub pattern: Option<String>,
}

/// Where walking back through a history of earlier lines has got to, and
/// the line typed before the walk began, to come back to past the newest.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct Recall {
    /// The line typed before an entry was recalled.
    draft: String,
    /// The entry shown, or `None` for the line typed.
    recalled: Option<usize>,
}

/// The lines submitted to prompts of one kind, oldest first.
#[derive(Clone, Debug, Default)]
pub(crate) struct PromptHistory {
    /// The entries, oldest first, none repeated.
    entries: Vec<String>,
}

// ========================================================================
// Prompt Implementations
// ========================================================================

impl Prompt {
    /// The char offset of the cursor in the line.
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// What the prompt is for.
    pub fn kind(&self) -> PromptKind {
        self.kind
    }

    /// The line typed so far.
    pub fn text(&self) -> String {
        self.line.to_string()
    }

    /// An empty prompt of `kind`, opened with `count` typed before it from
    /// `return_mode`.
    pub(crate) fn new(kind: PromptKind, count: Option<usize>, return_mode: Mode) -> Self {
        Self {
            count,
            cursor: 0,
            follow: SearchFollow::default(),
            kind,
            line: Document::new(""),
            recall: Recall::default(),
            return_mode,
        }
    }

    /// The count typed before the key that opened the prompt.
    pub(crate) fn count(&self) -> Option<usize> {
        self.count
    }

    /// What incremental search has done while the line was typed.
    pub(crate) fn follow(&self) -> &SearchFollow {
        &self.follow
    }

    /// What incremental search has done, to change.
    pub(crate) fn follow_mut(&mut self) -> &mut SearchFollow {
        &mut self.follow
    }

    /// The mode the view returns to when the prompt closes.
    pub(crate) fn return_mode(&self) -> Mode {
        self.return_mode
    }

    /// Makes `change`, planned over the line, and moves the cursor.
    fn apply(&mut self, change: LineChange) {
        if let Some(edit) = change.edit {
            self.replace(edit.range, &edit.text);
        }
        self.cursor = change.cursor.min(self.line.len_chars());
    }

    /// Puts `text` in place of `range`, line breaks made blanks, with the
    /// cursor after it; returns the chars it took.
    fn replace(&mut self, range: Range<usize>, text: &str) -> Range<usize> {
        let text: String = text
            .chars()
            .map(|c| match c {
                LINE_BREAK => LINE_BREAK_STAND_IN,
                c => c,
            })
            .collect();
        let start = range.start.min(self.line.len_chars());
        let end = range.end.clamp(start, self.line.len_chars());
        let replaced =
            self.line.delete(start..end).is_ok() && self.line.insert(start, &text).is_ok();
        if !replaced {
            return start..start;
        }
        self.cursor = start + text.chars().count();
        start..self.cursor
    }

    /// Makes `text` the line, the cursor at its end.
    fn set_text(&mut self, text: &str) {
        self.line = Document::new("");
        self.cursor = 0;
        self.replace(0..0, text);
    }
}

// ========================================================================
// PromptKind Implementations
// ========================================================================

impl PromptKind {
    /// The character shown before the line: `:`, `/`, or `?`.
    pub fn prefix(self) -> char {
        match self {
            Self::Command => ':',
            Self::SearchBackward => '?',
            Self::SearchForward => '/',
        }
    }
}

// ========================================================================
// Recall Implementations
// ========================================================================

impl Recall {
    /// The line to show for the next newer of `entries`, oldest first: the
    /// entry, or the line typed before recalling past the newest; `None`
    /// when no entry is shown.
    pub fn newer(&mut self, entries: &[String]) -> Option<String> {
        let index = self.recalled? + 1;
        match entries.get(index) {
            Some(entry) => {
                self.recalled = Some(index);
                Some(entry.clone())
            }
            None => {
                self.recalled = None;
                Some(mem::take(&mut self.draft))
            }
        }
    }

    /// The line to show for the next older of `entries`, oldest first,
    /// keeping `typed`, the line shown, to come back to when no entry is
    /// shown yet; `None` at the oldest.
    pub fn older(&mut self, entries: &[String], typed: &str) -> Option<String> {
        let index = match self.recalled {
            Some(index) => index.checked_sub(1)?,
            None => entries.len().checked_sub(1)?,
        };
        let entry = entries.get(index)?.clone();
        if self.recalled.is_none() {
            self.draft = typed.to_string();
        }
        self.recalled = Some(index);
        Some(entry)
    }
}

// ========================================================================
// PromptHistory Implementations
// ========================================================================

impl PromptHistory {
    /// The entries, oldest first.
    pub fn entries(&self) -> &[String] {
        &self.entries
    }

    /// Adds `entry` as the newest, moving it there if it was already kept
    /// and dropping the oldest when full. An empty entry is not kept.
    pub fn push(&mut self, entry: &str) {
        if entry.is_empty() {
            return;
        }
        self.entries.retain(|kept| kept != entry);
        if self.entries.len() == HISTORY_CAPACITY {
            self.entries.remove(0);
        }
        self.entries.push(entry.to_string());
    }
}

// ========================================================================
// Functions
// ========================================================================

/// Cancels the prompt, back to the mode it was opened from, cancelling a
/// waiting operator.
pub(crate) fn cancel(cx: &mut CommandContext<'_>) {
    cx.editor.view_mut(cx.view).close_prompt();
    if cx.editor.view_ref(cx.view).mode() == Mode::OperatorPending {
        cx.editor.set_mode(cx.view, Mode::Normal);
    }
}

/// Deletes the character before the cursor; on an empty line, cancels.
pub(crate) fn delete_backward(cx: &mut CommandContext<'_>) {
    let is_empty = prompt_ref(cx.editor, cx.view).is_some_and(|p| p.line.len_chars() == 0);
    if is_empty {
        return cancel(cx);
    }
    run_line_command(cx, LineCommand::DeleteBackwardChar);
}

/// Shows the next newer history entry, or the line typed before recalling
/// past the newest.
pub(crate) fn history_next(cx: &mut CommandContext<'_>) {
    recall_history(cx, false);
}

/// Shows the next older history entry, keeping the line typed so far to
/// come back to; at the oldest, nothing.
pub(crate) fn history_previous(cx: &mut CommandContext<'_>) {
    recall_history(cx, true);
}

/// `C-r` in a prompt: waits for a register name and types the text of
/// that register at the cursor, a linewise register without its final
/// line break; `C-w` after it types the word under the view's cursor.
pub(crate) fn insert_register(cx: &mut CommandContext<'_>) {
    await_char(cx, |cx, name| {
        let text = if name == WORD_UNDER_CURSOR {
            let document = cx.editor.document_of(cx.view);
            let cursor = cx.editor.view_ref(cx.view).cursor();
            search::word_at(document, cursor).map(|word| document.slice(word))
        } else if Registers::is_name(name) {
            cx.editor.read_register(cx.view, Some(name)).map(|text| {
                match (text.kind, text.text.strip_suffix(LINE_BREAK)) {
                    (RegisterKind::Linewise, Some(line)) => line.to_string(),
                    (
                        RegisterKind::Blockwise | RegisterKind::Charwise | RegisterKind::Linewise,
                        _,
                    ) => text.text,
                }
            })
        } else {
            return cx.editor.error(format!("{name:?} names no register"));
        };
        if let Some(text) = text {
            type_text(cx.editor, cx.view, &text);
        }
    });
}

/// Opens an empty prompt of `kind` in the view, with the command's count,
/// returning to the view's mode when it closes; does nothing in a view
/// that never leaves Insert mode.
pub(crate) fn open(cx: &mut CommandContext<'_>, kind: PromptKind) {
    if cx.editor.is_insert_locked(cx.view) {
        return;
    }
    let return_mode = cx.editor.view_ref(cx.view).mode();
    let prompt = Prompt::new(kind, cx.count, return_mode);
    cx.editor.view_mut(cx.view).open_prompt(prompt);
}

/// Runs the line editing `command` on the prompt's line, recording what
/// it kills in the kill ring.
pub(crate) fn run_line_command(cx: &mut CommandContext<'_>, command: LineCommand) {
    let seq = cx.editor.command_seq();
    let Some(prompt) = cx.editor.view_mut(cx.view).prompt_mut() else {
        return;
    };
    let change = plan(&prompt.line, prompt.cursor, command);
    let kill = change.kill.clone();
    prompt.apply(change);
    if let Some(kill) = kill {
        cx.editor.kill_ring_mut().kill(kill, seq);
    }
}

/// Submits the prompt's line: closes the prompt, back to the mode it was
/// opened from, keeps the line in the history, and acts on it.
pub(crate) fn submit(cx: &mut CommandContext<'_>) {
    let Some(prompt) = cx.editor.view_mut(cx.view).close_prompt() else {
        return;
    };
    let text = prompt.text();
    history_mut(cx.editor, prompt.kind).push(&text);
    match prompt.kind {
        PromptKind::Command => ex::run_line(cx, &text),
        PromptKind::SearchBackward => {
            search::search_typed(cx, &text, Direction::Backward, prompt.count());
        }
        PromptKind::SearchForward => {
            search::search_typed(cx, &text, Direction::Forward, prompt.count());
        }
    }
}

/// Types `text` at the prompt's cursor, for a key no prompt binding takes.
pub(crate) fn type_text(editor: &mut Editor, view: ViewId, text: &str) {
    if let Some(prompt) = editor.view_mut(view).prompt_mut() {
        let cursor = prompt.cursor;
        prompt.replace(cursor..cursor, text);
    }
}

/// Types the entry at the kill ring's yank pointer at the prompt's cursor.
pub(crate) fn yank(cx: &mut CommandContext<'_>) {
    let Some(text) = cx.editor.kill_ring_mut().current().map(str::to_string) else {
        return;
    };
    let seq = cx.editor.command_seq();
    let Some(prompt) = cx.editor.view_mut(cx.view).prompt_mut() else {
        return;
    };
    let cursor = prompt.cursor;
    let range = prompt.replace(cursor..cursor, &text);
    cx.editor.kill_ring_mut().record_yank(range, seq);
}

/// Replaces the text just yanked into the prompt with the next older kill.
pub(crate) fn yank_pop(cx: &mut CommandContext<'_>) {
    let seq = cx.editor.command_seq();
    let Some((range, text)) = cx.editor.kill_ring_mut().rotate(seq) else {
        return;
    };
    let Some(prompt) = cx.editor.view_mut(cx.view).prompt_mut() else {
        return;
    };
    let range = prompt.replace(range, &text);
    cx.editor.kill_ring_mut().record_yank(range, seq);
}

/// Shows the next `older` (else newer) entry of the history of the prompt
/// of `cx`'s view, if any.
fn recall_history(cx: &mut CommandContext<'_>, older: bool) {
    let Some(kind) = prompt_ref(cx.editor, cx.view).map(|prompt| prompt.kind) else {
        return;
    };
    let entries = history_mut(cx.editor, kind).entries().to_vec();
    let Some(prompt) = cx.editor.view_mut(cx.view).prompt_mut() else {
        return;
    };
    let typed = prompt.text();
    let shown = if older {
        prompt.recall.older(&entries, &typed)
    } else {
        prompt.recall.newer(&entries)
    };
    if let Some(line) = shown {
        prompt.set_text(&line);
    }
}

/// The history prompts of `kind` share.
fn history_mut(editor: &mut Editor, kind: PromptKind) -> &mut PromptHistory {
    match kind {
        PromptKind::Command => editor.command_history_mut(),
        PromptKind::SearchBackward | PromptKind::SearchForward => editor.search_history_mut(),
    }
}

/// The open prompt of `view`, if any.
fn prompt_ref(editor: &Editor, view: ViewId) -> Option<&Prompt> {
    editor.view_ref(view).prompt()
}

// ========================================================================
// Tests
// ========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::Harness;

    /// The prompt line of `harness` with `|` at its cursor, or `None` when
    /// no prompt is open.
    fn prompt_line(harness: &Harness) -> Option<String> {
        let prompt = harness.editor.view(harness.view)?.prompt()?;
        let mut text: Vec<char> = prompt.text().chars().collect();
        text.insert(prompt.cursor(), '|');
        Some(format!(
            "{}{}",
            prompt.kind().prefix(),
            text.into_iter().collect::<String>()
        ))
    }

    #[test]
    fn test_a_prompt_edits_its_line_with_the_emacs_keys_and_the_kill_ring() {
        let mut harness = Harness::new("|text");
        harness.keys("/ foo SPC bar.baz");
        assert_eq!(harness.mode(), Mode::CommandLine);
        assert_eq!(prompt_line(&harness).unwrap(), "/foo bar.baz|");
        harness.keys("M-b M-b C-t");
        assert_eq!(prompt_line(&harness).unwrap(), "/foob |ar.baz");
        harness.keys("C-a M-d C-e C-y");
        assert_eq!(prompt_line(&harness).unwrap(), "/ ar.bazfoob|");
        harness.keys("C-w C-u");
        assert_eq!(prompt_line(&harness).unwrap(), "/|");
        harness.keys("C-y");
        assert_eq!(
            prompt_line(&harness).unwrap(),
            "/ ar.bazfoob|",
            "merged kills"
        );
        assert_eq!(harness.marked(), "|text", "the document is untouched");
    }

    #[test]
    fn test_escape_or_backspace_on_an_empty_line_cancels_back_to_the_mode_it_came_from() {
        let mut harness = Harness::new("|abc");
        harness.keys("/ x <esc>");
        assert_eq!(harness.mode(), Mode::Normal);
        assert_eq!(prompt_line(&harness), None);
        harness.keys("v l / x <bs>");
        assert_eq!(prompt_line(&harness).unwrap(), "/|");
        harness.keys("<bs>");
        assert_eq!(harness.mode(), Mode::Visual, "the selection stays");
        harness.keys("<esc> d / C-g");
        assert_eq!(harness.mode(), Mode::Normal, "the operator is cancelled");
        harness.keys("l");
        assert_eq!(harness.marked(), "ab|c");
    }

    #[test]
    fn test_up_and_down_recall_history_and_come_back_to_the_line_typed() {
        let mut harness = Harness::new("|a b c a b c");
        harness.keys("/ a <cr> / b <cr> / a <cr>");
        harness.keys("/ x <up>");
        assert_eq!(prompt_line(&harness).unwrap(), "/a|", "a moved to newest");
        harness.keys("C-p");
        assert_eq!(prompt_line(&harness).unwrap(), "/b|");
        harness.keys("<up>");
        assert_eq!(prompt_line(&harness).unwrap(), "/b|", "b is the oldest");
        harness.keys("<down> C-n");
        assert_eq!(prompt_line(&harness).unwrap(), "/x|");
        harness.keys("<down>");
        assert_eq!(prompt_line(&harness).unwrap(), "/x|");
    }

    #[test]
    fn test_ctrl_r_types_a_register_and_ctrl_r_ctrl_w_the_word_under_the_cursor() {
        let mut harness = Harness::new("foo |bar_1 baz");
        harness.keys("/ x C-r C-w");
        assert_eq!(prompt_line(&harness).unwrap(), "/xbar_1|");
        harness.keys("C-a C-d <cr> y y : C-r \"");
        assert_eq!(
            prompt_line(&harness).unwrap(),
            ":foo bar_1 baz|",
            "no line break"
        );
        harness.keys("C-u C-r /");
        assert_eq!(prompt_line(&harness).unwrap(), ":bar_1|");
        harness.keys("C-r <f1>");
        assert_eq!(prompt_line(&harness).unwrap(), ":bar_1|", "no register");
    }
}
