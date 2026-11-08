//! The Vim state every view shares: registers, marks, the kill ring,
//! the last change, find, search, and substitution, and prompt histories.

use crate::char_find::CharFind;
use crate::kill::KillRing;
use crate::mark::{ChangeList, Marks};
use crate::phrase::Phrases;
use crate::prompt::PromptHistory;
use crate::register::{ReadOnlyRegister, RegisterKind, RegisterText, Registers};
use crate::repeat::RecordedChange;
use crate::search::{self, CaseRule, LastSearch, SearchCount, compile_cached};
use crate::substitute::Substitution;
use regex::Regex;

use super::{DocumentId, Editor, ViewId};

// ========================================================================
// Editor Implementations
// ========================================================================

impl Editor {
    /// How many matches the search pattern has in the document `view`
    /// shows, and which the cursor is on or last passed, for a status bar
    /// (`3/17`): the pattern being typed in a search prompt, else the last
    /// search while its matches are painted. `None` when there is neither,
    /// as after `:noh`, or for a view the editor lacks. Counting scans the
    /// document, so call it when the cursor or the pattern changes rather
    /// than on every frame.
    pub fn search_count(&self, view: ViewId) -> Option<SearchCount> {
        self.views.get(view)?;
        search::count_matches(self, view)
    }

    /// The history of the ex command line.
    pub(crate) fn command_history_mut(&mut self) -> &mut PromptHistory {
        &mut self.command_history
    }

    /// The kill ring.
    pub(crate) fn kill_ring_mut(&mut self) -> &mut KillRing {
        &mut self.kill_ring
    }

    /// The last change, for `.`.
    pub(crate) fn last_change(&self) -> Option<&RecordedChange> {
        self.last_change.as_ref()
    }

    /// The last ex command line run, for `@:`.
    pub(crate) fn last_ex_command(&self) -> Option<&str> {
        self.last_ex_command.as_deref()
    }

    /// The last find on a line, for `;` and `,`.
    pub(crate) fn last_find(&self) -> Option<CharFind> {
        self.last_find
    }

    /// The register `@@` replays.
    pub(crate) fn last_macro(&self) -> Option<char> {
        self.last_macro
    }

    /// The last search, if any.
    pub(crate) fn last_search(&self) -> Option<&LastSearch> {
        self.last_search.as_ref()
    }

    /// The last `:s`, for `&`.
    pub(crate) fn last_substitution(&self) -> Option<&Substitution> {
        self.last_substitution.as_ref()
    }

    /// Makes `substitution` the one `&` repeats.
    pub(crate) fn set_last_substitution(&mut self, substitution: Substitution) {
        self.last_substitution = Some(substitution);
    }

    /// The marks of the document `view` shows.
    pub(crate) fn marks(&self, view: ViewId) -> &Marks {
        &self.documents[self.views[view].document()].marks
    }

    /// The phrases the document `document` lights.
    pub(crate) fn phrases_of(&self, document: DocumentId) -> &Phrases {
        &self.documents[document].phrases
    }

    /// The phrases the document of `view` lights, to change.
    pub(crate) fn phrases_mut(&mut self, view: ViewId) -> &mut Phrases {
        &mut self.documents[self.views[view].document()].phrases
    }

    /// The changelist of the document `view` shows.
    pub(crate) fn changelist_mut(&mut self, view: ViewId) -> &mut ChangeList {
        &mut self.documents[self.views[view].document()].changelist
    }

    /// The marks of the document `view` shows, to set.
    pub(crate) fn marks_mut(&mut self, view: ViewId) -> &mut Marks {
        &mut self.documents[self.views[view].document()].marks
    }

    /// The text register `name` holds as `view` reads it (the unnamed
    /// register when `None`): a read-only register from the editor's own
    /// state, charwise, any other from the registers.
    pub(crate) fn read_register(
        &mut self,
        view: ViewId,
        name: Option<char>,
    ) -> Option<RegisterText> {
        let Some(read_only) = name.and_then(ReadOnlyRegister::named) else {
            return self.registers.read(name);
        };
        let text = match read_only {
            ReadOnlyRegister::DocumentName => {
                self.documents[self.views[view].document()].name.clone()
            }
            ReadOnlyRegister::LastCommand => self.last_ex_command.clone(),
            ReadOnlyRegister::LastInserted => self.last_inserted.clone(),
            ReadOnlyRegister::LastSearch => self
                .last_search
                .as_ref()
                .map(|search| search.pattern().to_string()),
        }?;
        Some(RegisterText::new(RegisterKind::Charwise, text))
    }

    /// The registers.
    pub(crate) fn registers_mut(&mut self) -> &mut Registers {
        &mut self.registers
    }

    /// The last search, when its matches are to be painted.
    pub(crate) fn highlighted_search(&self) -> Option<&LastSearch> {
        self.last_search.as_ref().filter(|_| self.highlight_search)
    }

    /// The history of search prompts.
    pub(crate) fn search_history_mut(&mut self) -> &mut PromptHistory {
        &mut self.search_history
    }

    /// Makes `change` the one `.` repeats.
    pub(crate) fn set_last_change(&mut self, change: RecordedChange) {
        self.last_change = Some(change);
    }

    /// Makes `line` the last ex command line run.
    pub(crate) fn set_last_ex_command(&mut self, line: String) {
        self.last_ex_command = Some(line);
    }

    /// Makes `text` the text typed in the last insert session.
    pub(crate) fn set_last_inserted(&mut self, text: String) {
        self.last_inserted = Some(text);
    }

    /// Makes `find` the one `;` and `,` repeat.
    pub(crate) fn set_last_find(&mut self, find: CharFind) {
        self.last_find = Some(find);
    }

    /// Makes `name` the register `@@` replays.
    pub(crate) fn set_last_macro(&mut self, name: char) {
        self.last_macro = Some(name);
    }

    /// Sets whether the matches of the last search are painted.
    pub(crate) fn set_highlight_search(&mut self, highlight: bool) {
        self.highlight_search = highlight;
    }

    /// `pattern` compiled under `case`, the last pattern compiled kept so
    /// that compiling it again is free.
    pub(crate) fn compile_pattern(
        &mut self,
        pattern: &str,
        case: CaseRule,
    ) -> Result<Regex, String> {
        compile_cached(&mut self.compiled_pattern, pattern, case)
    }

    /// Makes `search` the last search, its matches painted.
    pub(crate) fn set_last_search(&mut self, search: LastSearch) {
        self.last_search = Some(search);
        self.highlight_search = true;
    }
}
