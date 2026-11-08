//! Commands: the registry that maps command ids to the functions that run
//! them, the default key tables that bind keys to those ids, one table per
//! mode, and the keys a host binds on top.
//!
//! A key table names every binding once (id, title, group, default keys),
//! so dispatch and help read the same source; the registry gives each id
//! its function. Every id a table binds must be registered.
//!
//! # Host commands
//!
//! A host adds commands of its own with
//! [`Editor::register_command`](crate::Editor::register_command) and binds
//! keys to them, or to built-in ids, with
//! [`Editor::bind`](crate::Editor::bind). Host commands and built-ins are
//! equals: both run with a [`CommandContext`], and a key the host binds is
//! a user binding, so it wins over a default on the same keys.
//! [`Editor::override_keys`](crate::Editor::override_keys) replaces all the
//! keys of an id in a mode, the way a user's configuration rebinds.

mod defaults;

use std::borrow::Borrow;
use std::collections::HashMap;
use std::fmt;
use std::mem;
use std::ops::Range;
use std::sync::Arc;

use crate::keys::{
    Binding, HelpRow, InvalidSpec, Key, KeyTable, Keymap, ParseError, TableError, parse_sequence,
};
use thiserror::Error;

use super::editor::{DocumentId, Editor, ViewId};
use super::operator::Operand;
use super::prompt::{self, PROMPT_LINE_COMMANDS};
use super::text_object;
use super::view::Mode;

use self::defaults::{
    BLOCK_BINDINGS, COMMANDS, INSERT_BINDINGS, MOTION_BINDINGS, NORMAL_BINDINGS,
    OPERATOR_PENDING_BINDINGS, PROMPT_BINDINGS, REPLACE_BINDINGS, SCROLL_BINDINGS,
    TEXT_OBJECT_BINDINGS, VISUAL_BINDINGS,
};

// ========================================================================
// Data Structures
// ========================================================================

/// The function a built-in command id runs.
type BuiltinFn = fn(&mut CommandContext<'_>);

/// What a command waiting for a character (`q`, `@`) runs with it.
pub(crate) type CharFn = fn(&mut CommandContext<'_>, char);

/// The function a command id runs. Shared, so it can be called while the
/// editor it is called with is borrowed mutably.
type CommandFn = Arc<dyn Fn(&mut CommandContext<'_>) + Send + Sync>;

/// The name a command is registered and bound by: `undo`, or one a host
/// builds at runtime, such as `openwith:firefox`. Cheap to clone (the text
/// is shared), made from a `&str` or a `String`.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct CommandId(Arc<str>);

/// What a command runs with: the editor, the view the keys were typed in,
/// the count typed before them, and the register selected with `"`.
#[derive(Debug)]
pub struct CommandContext<'a> {
    /// The count typed before the command, if any; after an operator, the
    /// counts typed before the operator and before the motion multiplied.
    pub count: Option<usize>,
    /// The editor the command acts on.
    pub editor: &'a mut Editor,
    /// The register selected with `"` before the command (or before its
    /// operator), if any: a char from `a` to `z`, `A` to `Z`, `0` to `9`,
    /// or one of `" - _ + *`.
    pub register: Option<char>,
    /// The view the keys were typed in.
    pub view: ViewId,
}

/// Why a command could not be registered or its keys bound.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
#[non_exhaustive]
pub enum CommandError {
    /// A command with this id is already registered; ids are never
    /// replaced, so a host cannot silently change what a built-in does.
    #[error("a command named {0:?} is already registered")]
    DuplicateId(String),
    /// An ex command name is not a run of ASCII letters, so it could
    /// never be typed apart from its arguments.
    #[error("{0:?} is not an ex command name: use ASCII letters only")]
    InvalidExName(String),
    /// A key spec does not parse, or no spec was given where one is needed.
    #[error("{:?} for {:?} is not a key sequence: {}", .0.spec, .0.id, .0.error)]
    InvalidSpec(InvalidSpec),
    /// No command is registered under this id.
    #[error("no command is named {0:?}")]
    UnknownId(String),
}

/// A key that may follow the keys pending in a view, for a which-key
/// popup (see [`Editor::key_hints`](crate::Editor::key_hints)).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KeyHint {
    /// The command the pending keys and this key run, if they are bound.
    pub command: Option<CommandId>,
    /// Whether longer bindings continue past this key, so that typing it
    /// may wait for more.
    pub continues: bool,
    /// The key to type next.
    pub key: Key,
    /// What the command does, for a built-in command; `None` for a host
    /// command or a key that only continues.
    pub title: Option<&'static str>,
}

/// The keys in force: the default key tables (with the user's overrides),
/// the keys the host bound, and the keymap made of both.
#[derive(Clone, Debug)]
pub(crate) struct KeyBindings {
    /// The keys the host bound, in the order bound.
    host: Vec<HostBinding>,
    /// The keymap dispatch reads, one layer per mode.
    keymap: Keymap<KeyLayer, CommandId>,
    /// The default key table of each mode.
    tables: HashMap<Mode, KeyTable>,
}

/// Command ids and the functions they run.
#[derive(Clone)]
pub(crate) struct Registry {
    /// The function of each registered id.
    commands: HashMap<CommandId, CommandFn>,
}

/// A command waiting for the next key, typed as a character, to run with:
/// the register of `q` and `@`.
#[derive(Clone, Copy, Debug)]
pub(crate) struct PendingChar {
    /// The count the command was typed with.
    pub count: Option<usize>,
    /// The register selected for the command.
    pub register: Option<char>,
    /// What runs with the character.
    pub run: CharFn,
}

/// A key sequence the host bound to a command in a mode.
#[derive(Clone, Debug)]
struct HostBinding {
    /// The document the keys are bound in alone, or `None` for every one.
    document: Option<DocumentId>,
    /// The command id.
    id: CommandId,
    /// The keys.
    keys: Vec<Key>,
    /// The mode the keys are bound in.
    mode: Mode,
}

/// A layer of the keymap: the keys of a mode in every document, or in one
/// document alone, which win over every document's.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum KeyLayer {
    /// One document's own keys in a mode.
    Document(DocumentId, Mode),
    /// A mode's keys in every document.
    Mode(Mode),
}

// ========================================================================
// CommandId Implementations
// ========================================================================

impl CommandId {
    /// The id as text.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Borrow<str> for CommandId {
    fn borrow(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for CommandId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<&str> for CommandId {
    fn from(id: &str) -> Self {
        Self(Arc::from(id))
    }
}

impl From<String> for CommandId {
    fn from(id: String) -> Self {
        Self(Arc::from(id))
    }
}

// ========================================================================
// CommandContext Implementations
// ========================================================================

impl CommandContext<'_> {
    /// Acts on the chars `found` as a text object bound in operator-pending
    /// and visual modes does: after an operator, runs it over them (`daf`);
    /// in a visual mode, selects them, from their first char to their last
    /// (`vaf`). When nothing was found (`None`, or an empty range), a
    /// waiting operator is cancelled and a selection kept. A host
    /// registers its own text objects this way: a command that finds the
    /// range, bound with [`Editor::bind`](crate::Editor::bind) in
    /// [`Mode::OperatorPending`], [`Mode::Visual`], and [`Mode::VisualLine`].
    pub fn act_on_text_object(&mut self, found: Option<Range<usize>>) {
        match found.filter(|range| !range.is_empty()) {
            Some(range) => text_object::act_on_operand(self, &Operand::Charwise(range)),
            None if self.editor.view_ref(self.view).mode() == Mode::OperatorPending => {
                self.editor.set_mode(self.view, Mode::Normal);
            }
            None => {}
        }
    }

    /// The count typed, or one when none was.
    pub fn count_or_one(&self) -> usize {
        self.count.unwrap_or(1)
    }
}

// ========================================================================
// KeyBindings Implementations
// ========================================================================

impl KeyBindings {
    /// The default key tables, with no overrides and no host keys.
    pub fn new() -> Self {
        let tables = [
            (Mode::CommandLine, KeyTable::new(PROMPT_BINDINGS)),
            (Mode::Insert, KeyTable::new(INSERT_BINDINGS)),
            (Mode::Replace, KeyTable::new(REPLACE_BINDINGS)),
            (
                Mode::Normal,
                KeyTable::new(
                    NORMAL_BINDINGS
                        .iter()
                        .chain(MOTION_BINDINGS)
                        .chain(SCROLL_BINDINGS),
                ),
            ),
            (
                Mode::OperatorPending,
                KeyTable::new(with_objects(OPERATOR_PENDING_BINDINGS)),
            ),
            (
                Mode::Visual,
                KeyTable::new(with_objects(VISUAL_BINDINGS).chain(SCROLL_BINDINGS)),
            ),
            (
                Mode::VisualBlock,
                KeyTable::new(
                    with_objects(VISUAL_BINDINGS)
                        .chain(SCROLL_BINDINGS)
                        .chain(BLOCK_BINDINGS),
                ),
            ),
            (
                Mode::VisualLine,
                KeyTable::new(with_objects(VISUAL_BINDINGS).chain(SCROLL_BINDINGS)),
            ),
        ]
        .into_iter()
        .map(|(mode, table)| (mode, table.expect("the default key tables parse")))
        .collect();
        let mut bindings = Self {
            host: Vec::new(),
            keymap: Keymap::new(),
            tables,
        };
        bindings.rebuild_keymap();
        bindings
    }

    /// Binds each of `specs` to `id` in `mode`, keeping the keys `id`
    /// already has. Rejected, binding nothing, when a spec does not parse
    /// or none is given.
    pub fn bind(&mut self, mode: Mode, specs: &[&str], id: CommandId) -> Result<(), CommandError> {
        let sequences = parse_specs(id.as_str(), specs)?;
        if sequences.is_empty() {
            return Err(empty_spec(id.as_str()));
        }
        for keys in sequences {
            self.bind_host(HostBinding {
                document: None,
                id: id.clone(),
                keys,
                mode,
            });
        }
        Ok(())
    }

    /// Binds each of `specs` to `id` in `mode` in `document` alone, where
    /// they win over every document's keys. Rejected, binding nothing,
    /// when a spec does not parse or none is given.
    pub fn bind_in_document(
        &mut self,
        document: DocumentId,
        mode: Mode,
        specs: &[&str],
        id: CommandId,
    ) -> Result<(), CommandError> {
        let sequences = parse_specs(id.as_str(), specs)?;
        if sequences.is_empty() {
            return Err(empty_spec(id.as_str()));
        }
        for keys in sequences {
            self.bind_host(HostBinding {
                document: Some(document),
                id: id.clone(),
                keys,
                mode,
            });
        }
        Ok(())
    }

    /// Drops every key bound in `document` alone.
    pub fn drop_document(&mut self, document: DocumentId) {
        let before = self.host.len();
        self.host
            .retain(|binding| binding.document != Some(document));
        if self.host.len() != before {
            self.rebuild_keymap();
        }
    }

    /// The bindings of the default key table of `mode`, in table order,
    /// with the table's keys in force.
    pub fn help_rows(&self, mode: Mode) -> Vec<HelpRow> {
        self.tables
            .get(&mode)
            .map_or_else(Vec::new, KeyTable::help_rows)
    }

    /// The keymap dispatch reads.
    pub fn keymap(&self) -> &Keymap<KeyLayer, CommandId> {
        &self.keymap
    }

    /// What the built-in command `id` does, as the key table of `mode`
    /// titles it.
    pub fn title(&self, mode: Mode, id: &str) -> Option<&'static str> {
        self.tables.get(&mode)?.title(id)
    }

    /// Gives `id` the keys `specs` in `mode` in place of all it had there,
    /// its defaults and the keys the host bound; no specs unbinds it.
    /// Rejected, changing nothing, when a spec does not parse.
    pub fn override_keys(
        &mut self,
        mode: Mode,
        id: CommandId,
        specs: &[&str],
    ) -> Result<(), CommandError> {
        let sequences = parse_specs(id.as_str(), specs)?;
        self.host.retain(|binding| {
            binding.mode != mode || binding.id != id || binding.document.is_some()
        });
        match self.tables.get_mut(&mode) {
            Some(table) if table.title(id.as_str()).is_some() => {
                table
                    .override_keys(id.as_str(), specs)
                    .map_err(table_error)?;
            }
            Some(_) | None => {
                let added = sequences.into_iter().map(|keys| HostBinding {
                    document: None,
                    id: id.clone(),
                    keys,
                    mode,
                });
                self.host.extend(added);
            }
        }
        self.rebuild_keymap();
        Ok(())
    }

    /// Records `binding` and binds it into the keymap as a user binding.
    fn bind_host(&mut self, binding: HostBinding) {
        let layer = match binding.document {
            Some(document) => KeyLayer::Document(document, binding.mode),
            None => KeyLayer::Mode(binding.mode),
        };
        self.keymap
            .bind_user(layer, &binding.keys, binding.id.clone())
            .expect("a parsed sequence is never empty");
        self.host.push(binding);
    }

    /// Makes the keymap afresh: the tables first, then the host's keys.
    fn rebuild_keymap(&mut self) {
        self.keymap = Keymap::new();
        for (mode, table) in &self.tables {
            table
                .bind_into(&mut self.keymap, KeyLayer::Mode(*mode))
                .expect("the key tables bind no empty sequence");
        }
        for binding in mem::take(&mut self.host) {
            self.bind_host(binding);
        }
    }
}

// ========================================================================
// Registry Implementations
// ========================================================================

impl Registry {
    /// The registry of the editor's own commands.
    pub fn new() -> Self {
        let builtins = COMMANDS
            .iter()
            .map(|(id, run)| (CommandId::from(*id), Arc::new(*run) as CommandFn));
        let line_commands = PROMPT_LINE_COMMANDS.iter().map(|(id, command)| {
            let command = *command;
            let run = move |cx: &mut CommandContext<'_>| prompt::run_line_command(cx, command);
            (CommandId::from(*id), Arc::new(run) as CommandFn)
        });
        let commands = builtins.chain(line_commands).collect();
        Self { commands }
    }

    /// The function `id` runs, or `None` when no command has the id.
    pub fn get(&self, id: &str) -> Option<CommandFn> {
        self.commands.get(id).cloned()
    }

    /// The id as registered, for binding keys to it.
    pub fn id(&self, id: &str) -> Result<CommandId, CommandError> {
        self.commands
            .get_key_value(id)
            .map(|(id, _)| id.clone())
            .ok_or_else(|| CommandError::UnknownId(id.to_string()))
    }

    /// Registers `run` under `id`. Rejected when `id` is taken.
    pub fn register(&mut self, id: CommandId, run: CommandFn) -> Result<(), CommandError> {
        if self.commands.contains_key(&id) {
            return Err(CommandError::DuplicateId(id.to_string()));
        }
        self.commands.insert(id, run);
        Ok(())
    }
}

impl fmt::Debug for Registry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut ids: Vec<_> = self.commands.keys().collect();
        ids.sort();
        f.debug_struct("Registry").field("commands", &ids).finish()
    }
}

// ========================================================================
// Functions
// ========================================================================

/// Makes the view wait for the next key, typed as a character, to call
/// `run` with, keeping the command's count and register; any other key
/// cancels it (and a waiting operator).
pub(crate) fn await_char(cx: &mut CommandContext<'_>, run: CharFn) {
    let pending = PendingChar {
        count: cx.count,
        register: cx.register,
        run,
    };
    cx.editor.view_mut(cx.view).set_pending_char(Some(pending));
}

/// The error for binding `id` to no keys.
fn empty_spec(id: &str) -> CommandError {
    CommandError::InvalidSpec(InvalidSpec {
        error: ParseError::Empty,
        id: id.to_string(),
        spec: String::new(),
    })
}

/// The key sequences `specs` write, for binding them to `id`.
fn parse_specs(id: &str, specs: &[&str]) -> Result<Vec<Vec<Key>>, CommandError> {
    specs
        .iter()
        .map(|spec| {
            parse_sequence(spec).map_err(|error| {
                CommandError::InvalidSpec(InvalidSpec {
                    error,
                    id: id.to_string(),
                    spec: spec.to_string(),
                })
            })
        })
        .collect()
}

/// `bindings` followed by the motions and the text objects, for the
/// modes that take both.
fn with_objects(bindings: &'static [Binding]) -> impl Iterator<Item = &'static Binding> {
    bindings
        .iter()
        .chain(MOTION_BINDINGS)
        .chain(TEXT_OBJECT_BINDINGS)
}

/// A key table's error as a command error.
fn table_error(error: TableError) -> CommandError {
    match error {
        TableError::InvalidSpec(invalid) => CommandError::InvalidSpec(invalid),
        TableError::RepeatedId(id) => CommandError::DuplicateId(id),
        TableError::UnknownId(id) => CommandError::UnknownId(id),
    }
}

// ========================================================================
// Tests
// ========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_every_bound_id_is_registered_and_bound_once() {
        let registry = Registry::new();
        let bindings = KeyBindings::new();
        let duplicates = bindings.keymap().duplicates();
        assert!(duplicates.is_empty(), "{duplicates:?}");
        let tables = [
            BLOCK_BINDINGS,
            INSERT_BINDINGS,
            MOTION_BINDINGS,
            NORMAL_BINDINGS,
            OPERATOR_PENDING_BINDINGS,
            PROMPT_BINDINGS,
            REPLACE_BINDINGS,
            SCROLL_BINDINGS,
            TEXT_OBJECT_BINDINGS,
            VISUAL_BINDINGS,
        ];
        for bindings in tables {
            for binding in bindings {
                assert!(registry.get(binding.id).is_some(), "{}", binding.id);
            }
        }
    }
}
