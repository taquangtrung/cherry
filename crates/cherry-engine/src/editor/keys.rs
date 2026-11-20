//! Key dispatch: the keys a host feeds, the bindings and commands they
//! resolve to, counts and registers typed before a command, and keys
//! replayed as if typed (`.`, macros, `:norm`).

use std::mem;
use std::sync::Arc;

use crate::command::{CommandContext, CommandError, CommandId, KeyHint, KeyLayer};
use crate::completion;
use crate::cursors;
use crate::emacs;
use crate::ex::{ExContext, ExFn};
use crate::fold;
use crate::insert;
use crate::keys::{HelpRow, Key, KeyCode, Mods, Resolution, Timestamp};
use crate::options::EditingStyle;
use crate::prompt;
use crate::region;
use crate::register::Registers;
use crate::repeat::{self, COUNT_RADIX, MAX_REPLAYED_KEYS};
use crate::replace;
use crate::request::{CommandRequest, HostRequest, request_cancel};
use crate::search;
use crate::snippet;
use crate::substitute;
use crate::view::{Mode, UniversalArgument, View, is_visual};
use crate::vim_insert;

use super::{DocumentId, Editor, Handled, REGISTER_PREFIX, RoundState, ViewId};

// ========================================================================
// Constants
// ========================================================================

/// The bit Ctrl clears from an ASCII character to make its control
/// character (`C-a` is `\x01`).
const CONTROL_BIT: u8 = 0x40;

/// The most Escapes `:norm` types to end what its keys left unfinished:
/// a prompt, then a visual mode or Insert mode.
const ESCAPES_TO_NORMAL: usize = 3;

// ========================================================================
// Editor Implementations
// ========================================================================

impl Editor {
    /// Binds each key sequence of `specs` (`"g x"`, `"C-c C-t"`) to the
    /// command `id` in `mode`, keeping the keys it already has. The keys
    /// are user bindings, so they win over a default on the same keys.
    /// Rejected, binding nothing, when no command has `id`, a spec does not
    /// parse, or no spec is given.
    pub fn bind(&mut self, mode: Mode, specs: &[&str], id: &str) -> Result<(), CommandError> {
        let id = self.commands.id(id)?;
        self.bindings.bind(mode, specs, id)
    }

    /// Binds each key sequence of `specs` to the command `id` in `mode` in
    /// the document `document` alone, as a commit message's `C-c C-c`: its
    /// views read these keys first, over the keys of every document. The
    /// keys go when the document is closed. Rejected as
    /// [`Editor::bind`] is.
    pub fn bind_in_document(
        &mut self,
        document: DocumentId,
        mode: Mode,
        specs: &[&str],
        id: &str,
    ) -> Result<(), CommandError> {
        let id = self.commands.id(id)?;
        self.bindings.bind_in_document(document, mode, specs, id)
    }

    /// When the host is next to call [`Editor::tick`]: when the first
    /// half-typed sequence of any view times out, or at once (the start of
    /// the host's clock, which has passed) while a highlighter has lines in
    /// sight still to prepare; `None` when nothing waits.
    pub fn deadline(&self) -> Option<Timestamp> {
        let highlighting = self
            .views
            .keys()
            .any(|view| self.unprepared(view).is_some())
            .then(|| Timestamp::from_millis(0));
        self.views
            .values()
            .filter_map(|view| view.pending_deadline())
            .chain(highlighting)
            .min()
    }

    /// The built-in commands bound in `mode`, in the order help lists them,
    /// each with its group, title, and the keys in force there: defaults,
    /// as the user overrode them, with the keys a host bound added. For a
    /// command palette or a key help page; host commands are the host's to
    /// list, with [`Editor::keys_for`] for their keys.
    pub fn help_rows(&self, mode: Mode) -> Vec<HelpRow> {
        let layers = mode_layers(mode);
        let active = self.bindings.keymap().active(&layers);
        let mut rows = self.bindings.help_rows(mode);
        for row in &mut rows {
            row.keys = active.sequences_for(&CommandId::from(row.id));
        }
        rows
    }

    /// Feeds `key`, typed at `now` in `view`, running whatever it completes,
    /// then any keys that replays (`.`, `@a`). A key for a view the editor
    /// does not have is [`Handled::Ignored`].
    pub fn handle_key(&mut self, view: ViewId, key: Key, now: Timestamp) -> Handled {
        let Some(state) = self.views.get_mut(view) else {
            return Handled::Ignored;
        };
        state.set_preedit("");
        if let Some(recording) = state.recording_mut() {
            recording.push(key);
        }
        self.last_key_at = Some(now);
        if self.views[view].extra_cursors().is_empty() {
            self.run_key(view, key, now)
        } else {
            cursors::run_round(self, view, key, now)
        }
    }

    /// The keys that may follow the keys pending in `view` (the keys that
    /// start a binding when none are pending), in its mode, sorted by
    /// notation, for a which-key popup; empty for a view the editor lacks.
    pub fn key_hints(&self, view: ViewId) -> Vec<KeyHint> {
        let Some(state) = self.views.get(view) else {
            return Vec::new();
        };
        let layers = key_layers(state.document(), state.mode());
        let active = self.bindings.keymap().active(&layers);
        active
            .continuations(state.pending_keys())
            .into_iter()
            .map(|continuation| KeyHint {
                command: continuation.command.cloned(),
                continues: continuation.continues,
                key: continuation.key,
                title: continuation.command.and_then(|id| {
                    layers_of(state.mode())
                        .iter()
                        .find_map(|mode| self.bindings.title(*mode, id.as_str()))
                }),
            })
            .collect()
    }

    /// The key sequences that run the command `id` in `mode`, shortest
    /// first (where-is); empty when none does.
    pub fn keys_for(&self, mode: Mode, id: &str) -> Vec<Vec<Key>> {
        let layers = mode_layers(mode);
        let active = self.bindings.keymap().active(&layers);
        active.sequences_for(&CommandId::from(id))
    }

    /// Gives the command `id` the keys `specs` in `mode` in place of all
    /// it had there, defaults and bound keys alike, as a user's
    /// configuration rebinds; no specs unbinds it. Rejected, changing
    /// nothing, when no command has `id` or a spec does not parse.
    pub fn override_keys(
        &mut self,
        mode: Mode,
        id: &str,
        specs: &[&str],
    ) -> Result<(), CommandError> {
        let id = self.commands.id(id)?;
        self.bindings.override_keys(mode, id, specs)
    }

    /// Registers `run` as the command `id`, for [`Editor::bind`] to bind
    /// keys to. It runs with a [`CommandContext`], as the built-in commands
    /// do. The id may be a literal (`"shout"`) or built at runtime
    /// (`format!("openwith:{app}")`). Rejected when a command, built-in or
    /// not, already has `id`: ids are never replaced; to change what a key
    /// does, bind it to another id.
    pub fn register_command(
        &mut self,
        id: impl Into<CommandId>,
        run: impl Fn(&mut CommandContext<'_>) + Send + Sync + 'static,
    ) -> Result<(), CommandError> {
        self.commands.register(id.into(), Arc::new(run))
    }

    /// Registers `id` as a command the host runs itself, with its own state
    /// (a language server, a version control tool), which a command
    /// registered with [`Editor::register_command`] cannot borrow. Running
    /// it, from a key bound with [`Editor::bind`] or with
    /// [`Editor::run_command`], queues a
    /// [`HostRequest::Command`] naming it, with the view, the count, and
    /// the register, for the host to act on once it takes the requests.
    /// Rejected as [`Editor::register_command`] is.
    pub fn register_host_command(&mut self, id: impl Into<CommandId>) -> Result<(), CommandError> {
        let id = id.into();
        let command = id.clone();
        self.register_command(id, move |cx| {
            let request = CommandRequest {
                command: command.clone(),
                count: cx.count,
                register: cx.register,
                view: cx.view,
            };
            cx.editor.request(HostRequest::Command(request));
        })
    }

    /// Registers `run` as the ex command `name`, run by typing `:name` with
    /// an optional range before it (`:%name`, `:2,5name`), an optional `!`
    /// after it, and arguments; it runs with an [`ExContext`]. The name is
    /// ASCII letters, typed whole (no abbreviations). Rejected when an ex
    /// command, built-in (`w`, `q`, `s`, `set`, ...) or not, already has
    /// the name, or the name is not letters.
    pub fn register_ex_command(
        &mut self,
        name: impl Into<String>,
        run: impl Fn(&mut ExContext<'_>) + Send + Sync + 'static,
    ) -> Result<(), CommandError> {
        self.ex_commands.register(name.into(), Arc::new(run))
    }

    /// Runs the command `id`, built-in or registered by the host, in
    /// `view` with `count`, as its keys would: for a menu item or a command
    /// palette. Keys half-typed in the view are dropped, and the view is
    /// scrolled to its cursor after. A command run this way is not a change
    /// `.` repeats. Rejected with [`CommandError::UnknownId`] when no
    /// command has `id`; does nothing for a view the editor lacks.
    pub fn run_command(
        &mut self,
        view: ViewId,
        id: &str,
        count: Option<usize>,
    ) -> Result<(), CommandError> {
        let command = self
            .commands
            .get(id)
            .ok_or_else(|| CommandError::UnknownId(id.to_string()))?;
        let Some(state) = self.views.get_mut(view) else {
            return Ok(());
        };
        state.resolver_mut().reset();
        *state.count_mut() = None;
        *state.register_mut() = None;
        self.command_seq += 1;
        command(&mut CommandContext {
            count,
            editor: self,
            register: None,
            view,
        });
        self.scroll_to_cursor(view);
        Ok(())
    }

    /// Settles every half-typed sequence whose timeout has passed at `now`,
    /// running the shorter binding it completes, if any, and has each
    /// highlighter prepare the lines in sight it still lacks (see
    /// [`Highlighter::prepare`](crate::Highlighter::prepare)). Returns whether any sequence settled or
    /// highlight was prepared, so the host knows to repaint.
    pub fn tick(&mut self, now: Timestamp) -> bool {
        let highlighted = self.prepare_highlights();
        let view_ids: Vec<_> = self.views.keys().collect();
        let mut settled = false;
        for view in view_ids {
            let state = &mut self.views[view];
            let layers = key_layers(state.document(), state.mode());
            let resolution = state
                .resolver_mut()
                .tick(&self.bindings.keymap().active(&layers), now);
            if let Some(resolution) = resolution {
                self.run_resolution(view, resolution);
                self.scroll_to_cursor(view);
                settled = true;
            }
        }
        settled || highlighted
    }

    /// The sequence number of the command running now (or of the last one
    /// run): one more than the command or typed character before it.
    pub(crate) fn command_seq(&self) -> u64 {
        self.command_seq
    }

    /// How many keys completed the binding running now.
    pub(crate) fn last_matched_len(&self) -> usize {
        self.last_matched_len
    }

    /// Queues `keys` to feed, as if typed, after the key being handled and
    /// before any keys queued already.
    pub(crate) fn queue_keys(&mut self, keys: Vec<Key>) {
        for key in keys.into_iter().rev() {
            self.replay.push_front(key);
        }
    }

    /// The function the ex command `name` runs, if one is registered.
    pub(crate) fn ex_command(&self, name: &str) -> Option<ExFn> {
        self.ex_commands.get(name)
    }

    /// Feeds `keys` to `view` now, as if typed, before any keys queued
    /// already (`:norm`), with the keys they replay (`@a`); then runs
    /// what an ambiguous prefix left pending completes and ends whatever
    /// the keys left unfinished as Escape does, back in Normal mode.
    pub(crate) fn feed_keys_now(&mut self, view: ViewId, keys: Vec<Key>) {
        let now = self.last_key_at.unwrap_or_default();
        let queued = mem::replace(&mut self.replay, keys.into());
        let mut fed = 0;
        while let Some(key) = self.replay.pop_front() {
            fed += 1;
            if fed > MAX_REPLAYED_KEYS {
                self.replay.clear();
                self.error(format!("Macro stopped after {MAX_REPLAYED_KEYS} keys"));
                break;
            }
            self.dispatch_key(view, key, now);
        }
        self.replay = queued;
        if let Some(deadline) = self.views[view].pending_deadline() {
            let state = &mut self.views[view];
            let layers = key_layers(state.document(), state.mode());
            let resolution = state
                .resolver_mut()
                .tick(&self.bindings.keymap().active(&layers), deadline);
            if let Some(resolution) = resolution {
                self.run_resolution(view, resolution);
            }
        }
        let escape = Key::new(KeyCode::Esc, Mods::NONE);
        let escapes = if self.is_insert_locked(view) {
            0
        } else {
            ESCAPES_TO_NORMAL
        };
        for _ in 0..escapes {
            if self.views[view].mode() == Mode::Normal {
                break;
            }
            self.dispatch_key(view, escape, now);
        }
        if !self.views[view].is_idle() {
            self.set_mode(view, Mode::Normal);
        }
    }

    /// Notes that the command running acts at the main cursor alone, so a
    /// round over several cursors runs it once.
    pub(crate) fn act_once(&mut self) {
        self.acted_once = true;
    }

    /// Whether a command acted at the main cursor alone since last asked.
    pub(crate) fn take_acted_once(&mut self) -> bool {
        mem::take(&mut self.acted_once)
    }

    /// Puts back what [`Editor::round_state`] kept, dropping the requests
    /// queued since.
    pub(crate) fn restore_round_state(&mut self, state: RoundState) {
        self.kill_ring = state.kill_ring;
        self.last_change = state.last_change;
        self.last_inserted = state.last_inserted;
        self.registers.restore(state.registers);
        self.requests.truncate(state.requests);
    }

    /// What a round over several cursors keeps as the main cursor left it.
    pub(crate) fn round_state(&self) -> RoundState {
        RoundState {
            kill_ring: self.kill_ring.clone(),
            last_change: self.last_change.clone(),
            last_inserted: self.last_inserted.clone(),
            registers: self.registers.stored(),
            requests: self.requests.len(),
        }
    }

    /// Feeds `key`, typed at `now`, to `view` at its main cursor, running
    /// whatever it completes, then any keys that replays (`.`, `@a`).
    pub(crate) fn run_key(&mut self, view: ViewId, key: Key, now: Timestamp) -> Handled {
        let handled = self.dispatch_key(view, key, now);
        let mut replayed = 0;
        while let Some(key) = self.replay.pop_front() {
            replayed += 1;
            if replayed > MAX_REPLAYED_KEYS {
                self.replay.clear();
                self.error(format!("Macro stopped after {MAX_REPLAYED_KEYS} keys"));
                break;
            }
            self.dispatch_key(view, key, now);
        }
        handled
    }

    /// Feeds one `key` to `view` at `now`, typed or replayed, recording it
    /// in the change being made for `.`.
    fn dispatch_key(&mut self, view: ViewId, key: Key, now: Timestamp) -> Handled {
        repeat::begin_change(self, view);
        let handled = self.feed_key(view, key, now);
        vim_insert::return_to_insert(self, view);
        repeat::finish_change(self, view);
        search::follow_prompt(self, view);
        let seq = self.command_seq;
        if let Some(state) = self.views.get_mut(view)
            && state
                .completion()
                .is_some_and(|completion| completion.is_stale(seq))
        {
            state.take_completion();
        }
        self.scroll_to_cursor(view);
        handled
    }

    /// Feeds one `key` to `view` at `now`: a register name after `"`, a
    /// character a command waits for, a count digit, or a key toward a
    /// binding.
    fn feed_key(&mut self, view: ViewId, key: Key, now: Timestamp) -> Handled {
        let is_emacs_insert = self.options.editing_style == EditingStyle::Emacs
            && self.views[view].mode() == Mode::Insert;
        let key = match is_emacs_insert {
            true => match emacs::translate_escape(self, view, key) {
                Some(key) => key,
                None => return Handled::Consumed,
            },
            false => key,
        };
        if is_emacs_insert && emacs::take_digit_argument(self, view, key) {
            return Handled::Consumed;
        }
        let state = &mut self.views[view];
        if state.awaiting_register() {
            state.set_awaiting_register(false);
            match typed_char(key).filter(|name| Registers::is_name(*name)) {
                Some(name) => *state.register_mut() = Some(name),
                None => {
                    *state.count_mut() = None;
                    self.error(format!("{key} names no register"));
                }
            }
            return Handled::Consumed;
        }
        if is_register_prefix(state, key) {
            state.set_awaiting_register(true);
            return Handled::Consumed;
        }
        if let Some(pending) = state.take_pending_char() {
            repeat::note_key(self, view, key);
            match literal_char(key) {
                Some(c) => {
                    let mut cx = CommandContext {
                        count: pending.count,
                        editor: self,
                        register: pending.register,
                        view,
                    };
                    (pending.run)(&mut cx, c);
                }
                None if self.views[view].mode() == Mode::OperatorPending => {
                    self.set_mode(view, Mode::Normal);
                }
                None => substitute::stop_query(self, view),
            }
            return Handled::Consumed;
        }
        if let Some(digit) = count_digit(state, key) {
            if state.universal_argument() == Some(UniversalArgument::Fresh) {
                *state.count_mut() = None;
                state.set_universal_argument(Some(UniversalArgument::Digits));
            }
            let mode = state.mode();
            let count = state.count_mut();
            *count = Some(
                count
                    .unwrap_or(0)
                    .saturating_mul(COUNT_RADIX as usize)
                    .saturating_add(digit),
            );
            if state
                .change()
                .is_some_and(|change| change.takes_count_digit(mode))
            {
                repeat::note_key(self, view, key);
            }
            return Handled::Consumed;
        }
        repeat::note_key(self, view, key);
        let state = &mut self.views[view];
        let layers = key_layers(state.document(), state.mode());
        let resolutions =
            state
                .resolver_mut()
                .feed(&self.bindings.keymap().active(&layers), key, now);
        let mut handled = Handled::Ignored;
        for resolution in resolutions {
            handled = self.run_resolution(view, resolution);
        }
        handled
    }

    /// Runs what `resolution` settled in `view`: the matched command, or an
    /// unmatched printable key typed as text in Insert mode.
    fn run_resolution(&mut self, view: ViewId, resolution: Resolution<CommandId>) -> Handled {
        if !matches!(resolution, Resolution::Pending) {
            self.command_seq += 1;
            self.views[view].set_universal_argument(None);
        }
        match resolution {
            Resolution::Matched(matched) => {
                let state = &mut self.views[view];
                let typed = state.count_mut().take();
                let selected = state.register_mut().take();
                let (count, register) = match state.operator() {
                    Some(pending) => (multiply_counts(pending.count, typed), pending.register),
                    None => (typed, selected),
                };
                let after_operator = state.operator().is_some();
                let escape = [Key::new(KeyCode::Esc, Mods::NONE)];
                let had_prefix = typed.is_some() || selected.is_some();
                if self.options.escape_drops_prefix && had_prefix && matched.keys == escape {
                    return Handled::Consumed;
                }
                repeat::note_command(self, view, count, register, after_operator);
                self.close_typed_step(view, &matched.command);
                self.last_matched_len = matched.keys.len();
                let Some(command) = self.commands.get(matched.command.as_str()) else {
                    self.error(format!("no command is named {:?}", matched.command));
                    return Handled::Ignored;
                };
                command(&mut CommandContext {
                    count,
                    editor: self,
                    register,
                    view,
                });
                region::follow_mark(self, view);
                fold::reveal_cursor(self, view);
                Handled::Consumed
            }
            Resolution::Pending => Handled::Consumed,
            Resolution::Unmatched(keys) => {
                let single_line = self.shows_single_line(view);
                let state = &mut self.views[view];
                let typed_count = state.count_mut().take();
                let had_prefix = typed_count.is_some();
                let had_prefix = state.register_mut().take().is_some() || had_prefix;
                let mode = state.mode();
                let escape = Key::new(KeyCode::Esc, Mods::NONE);
                if mode == Mode::Normal && keys == [escape] && !had_prefix && single_line {
                    request_cancel(self, view);
                    return Handled::Consumed;
                }
                let typed = match (mode, keys.as_slice()) {
                    (Mode::CommandLine | Mode::Insert | Mode::Replace, [key]) => typed_char(*key),
                    (
                        Mode::CommandLine
                        | Mode::Insert
                        | Mode::Replace
                        | Mode::Normal
                        | Mode::Visual
                        | Mode::VisualBlock
                        | Mode::VisualLine,
                        _,
                    ) => None,
                    (Mode::OperatorPending, _) => {
                        state.set_mode(Mode::Normal);
                        None
                    }
                };
                let Some(typed) = typed else {
                    return Handled::Ignored;
                };
                let mut buffer = [0; 4];
                let text = typed.encode_utf8(&mut buffer);
                match mode {
                    Mode::CommandLine => prompt::type_text(self, view, text),
                    Mode::Replace => replace::type_over(self, view, typed),
                    Mode::Insert => {
                        self.views[view].set_undo_step_command(None);
                        region::drop_region_for_typing(self, view);
                        snippet::take_placeholder(self, view);
                        for _ in 0..typed_count.unwrap_or(1) {
                            insert::type_char(self, view, typed);
                        }
                        completion::narrow_completion(self, view);
                        completion::auto_complete(self, view, typed);
                    }
                    Mode::Normal
                    | Mode::OperatorPending
                    | Mode::Visual
                    | Mode::VisualBlock
                    | Mode::VisualLine => insert::type_char(self, view, typed),
                }
                Handled::Consumed
            }
        }
    }

    /// Every registered command id, built-in or the host's, in no order.
    pub(crate) fn command_ids(&self) -> impl Iterator<Item = &str> {
        self.commands.ids()
    }

    /// In a modeless editing style, closes the undo step of `view` before
    /// the command `id` runs, so undo takes back typing and commands one
    /// at a time; a run of one command, such as Backspace held down, stays
    /// one step with the typing after it.
    fn close_typed_step(&mut self, view: ViewId, id: &CommandId) {
        let state = &mut self.views[view];
        if !self.options.editing_style.is_modeless() || state.undo_step_command() == Some(id) {
            return;
        }
        state.set_undo_step_command(Some(id.clone()));
        let history = self.history_mut(view);
        history.end_group();
        history.begin_group();
    }

    /// Puts `view`, when it never leaves Insert mode
    /// ([`Editor::is_insert_locked`]), in Insert mode with an insert
    /// session open, closing a prompt and dropping whatever it was in.
    pub(super) fn lock_in_insert(&mut self, view: ViewId) {
        if !self.is_insert_locked(view) {
            return;
        }
        let state = &mut self.views[view];
        state.close_prompt();
        if state.mode() != Mode::Insert {
            self.set_mode(view, Mode::Insert);
        }
    }
}

// ========================================================================
// Functions
// ========================================================================

/// The digit `key` adds to the count of `view`, when it is one: a digit
/// typed outside Insert and command-line mode, or after `C-u` in the
/// Emacs style, with no keys pending; `0` only after another digit.
fn count_digit(view: &View, key: Key) -> Option<usize> {
    let KeyCode::Char(c) = key.code else {
        return None;
    };
    let digit = c.to_digit(COUNT_RADIX)?;
    let starts_or_continues = digit != 0 || view.count().is_some();
    let takes_digits = view.universal_argument().is_some()
        || !matches!(
            view.mode(),
            Mode::CommandLine | Mode::Insert | Mode::Replace
        );
    let is_count = takes_digits
        && key.mods == Mods::NONE
        && view.pending_keys().is_empty()
        && starts_or_continues;
    is_count.then_some(digit as usize)
}

/// Whether `key` is the `"` that selects a register in `view`: typed in
/// Normal or a visual mode with no keys pending.
fn is_register_prefix(view: &View, key: Key) -> bool {
    (view.mode() == Mode::Normal || is_visual(view.mode()))
        && view.pending_keys().is_empty()
        && !view.has_pending_char()
        && key == Key::new(KeyCode::Char(REGISTER_PREFIX), Mods::NONE)
}

/// The keymap layers in force in `mode` in the document `document`,
/// highest first: the document's own keys, then every document's, each by
/// [`layers_of`].
fn key_layers(document: DocumentId, mode: Mode) -> Vec<KeyLayer> {
    let modes = layers_of(mode);
    let own = modes.iter().map(|mode| KeyLayer::Document(document, *mode));
    own.chain(modes.iter().map(|mode| KeyLayer::Mode(*mode)))
        .collect()
}

/// The keymap layers of every document in force in `mode`, highest first.
fn mode_layers(mode: Mode) -> Vec<KeyLayer> {
    layers_of(mode)
        .iter()
        .map(|mode| KeyLayer::Mode(*mode))
        .collect()
}

/// The modes whose keys are in force in `mode`, highest first: Replace mode's own
/// keys over Insert mode's, any other mode's alone.
fn layers_of(mode: Mode) -> &'static [Mode] {
    match mode {
        Mode::CommandLine => &[Mode::CommandLine],
        Mode::Insert => &[Mode::Insert],
        Mode::Normal => &[Mode::Normal],
        Mode::OperatorPending => &[Mode::OperatorPending],
        Mode::Replace => &[Mode::Replace, Mode::Insert],
        Mode::Visual => &[Mode::Visual],
        Mode::VisualBlock => &[Mode::VisualBlock],
        Mode::VisualLine => &[Mode::VisualLine],
    }
}

/// The count of a motion typed after an operator: the counts typed before
/// the operator and before the motion multiplied, one standing for a
/// missing one; `None` when neither was typed.
fn multiply_counts(operator: Option<usize>, motion: Option<usize>) -> Option<usize> {
    match (operator, motion) {
        (None, None) => None,
        (operator, motion) => Some(operator.unwrap_or(1).saturating_mul(motion.unwrap_or(1))),
    }
}

/// The character `key` stands for when a command waits for one (`r`,
/// `f`, `C-v`): the character it types, `\n` for Enter, `\t` for Tab, or
/// the control character of Ctrl and a letter (or one of `@ [ \ ] ^ _`).
fn literal_char(key: Key) -> Option<char> {
    if key == Key::new(KeyCode::Enter, Mods::NONE) {
        return Some('\n');
    }
    if key == Key::new(KeyCode::Tab, Mods::NONE) {
        return Some('\t');
    }
    if let (KeyCode::Char(c), Mods::CTRL) = (key.code, key.mods) {
        let upper = c.to_ascii_uppercase();
        return ('@'..='_')
            .contains(&upper)
            .then(|| char::from(upper as u8 ^ CONTROL_BIT));
    }
    typed_char(key)
}

/// The character `key` types as text: a character with no Ctrl, Alt, or
/// Super held.
fn typed_char(key: Key) -> Option<char> {
    let KeyCode::Char(c) = key.code else {
        return None;
    };
    let chorded = key.mods.intersects(Mods::CTRL | Mods::ALT | Mods::SUPER);
    (!chorded).then_some(c)
}
