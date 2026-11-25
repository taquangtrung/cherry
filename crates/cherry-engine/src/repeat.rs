//! Repeating: `.` repeats the last change, `q{reg}` records keys into a
//! register until the next `q`, and `@{reg}` replays them.
//!
//! # The last change
//!
//! A change is recorded as the keys that made it: every key from one that
//! starts in Normal mode, with nothing pending, to the next time the view
//! is back in Normal mode with nothing pending, kept when the document
//! changed in between. So `3dw`, `ci(text<esc>`, `x`, `p`, `d/foo<cr>`,
//! and `vjd` are each one change, while motions, yanks, undo, and ex
//! commands are none. The count is kept apart from the keys: the count
//! typed before the change, or after an operator the counts on both sides
//! multiplied (`2d3w` keeps `6`), so a count typed before `.` replaces it
//! (`3.` after `dw` deletes three words), and it carries on to the next
//! `.`. The register selected for the change is selected again. A visual
//! change replays its keys from the cursor, so it covers the same motions,
//! not the same size of text.
//!
//! # Macros
//!
//! `q` and a register (`a` to `z`, `0` to `9`; `A` to `Z` append) records
//! every key typed until `q` again, into that register as key notation
//! (`d w j`), so `"ap` puts a macro in the text to edit and `"ayy` takes it
//! back. `@` and a register replays it, a count times; `@@` replays the
//! last register replayed; `@:` runs the last ex command line again. Replayed keys run as if typed, after the key
//! that replayed them; a macro that replays itself stops after
//! [`MAX_REPLAYED_KEYS`] keys.

use crate::keys::{Key, KeyCode, Mods, parse_sequence};

use super::command::{CommandContext, await_char};
use super::editor::{Editor, REGISTER_PREFIX, ViewId};
use super::ex;
use super::view::Mode;

// ========================================================================
// Constants
// ========================================================================

/// The most keys replayed for one key typed, so a macro that replays
/// itself ends.
pub(crate) const MAX_REPLAYED_KEYS: usize = 100_000;

/// The register name `@` takes for the last register replayed.
const LAST_MACRO: char = '@';

/// The register name `@` takes for the last ex command line.
const LAST_EX_COMMAND: char = ':';

/// What separates the keys of a macro in its register.
const KEY_SEPARATOR: &str = " ";

/// The base counts are typed and written in.
pub(crate) const COUNT_RADIX: u32 = 10;

// ========================================================================
// Data Structures
// ========================================================================

/// The keys of a change being made, from its first key on.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ChangeCandidate {
    /// The count the change runs with, when one was typed.
    count: Option<usize>,
    /// How many edits the editor had made before the first key.
    edits_before: u64,
    /// Whether a command has run since the first key.
    has_command: bool,
    /// The keys, counts and register selection left out.
    keys: Vec<Key>,
    /// The register selected for the change.
    register: Option<char>,
}

/// Keys being recorded into a register.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct MacroRecording {
    /// The keys typed so far.
    keys: Vec<Key>,
    /// The register they go to.
    register: char,
}

/// The last change, for `.` to make again.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RecordedChange {
    /// The count it ran with, if any.
    count: Option<usize>,
    /// Its keys, counts and register selection left out.
    keys: Vec<Key>,
    /// The register it was made with, if any.
    register: Option<char>,
}

// ========================================================================
// ChangeCandidate Implementations
// ========================================================================

impl ChangeCandidate {
    /// Whether a count digit typed now belongs to the keys: one typed
    /// after the first command and not after an operator (those are kept
    /// as the count instead), as in `v3jd`.
    pub(crate) fn takes_count_digit(&self, mode: Mode) -> bool {
        self.has_command && mode != Mode::OperatorPending
    }
}

// ========================================================================
// MacroRecording Implementations
// ========================================================================

impl MacroRecording {
    /// The register the keys go to.
    pub(crate) fn register(&self) -> char {
        self.register
    }

    /// Adds `key`, just typed.
    pub(crate) fn push(&mut self, key: Key) {
        self.keys.push(key);
    }
}

// ========================================================================
// Functions
// ========================================================================

/// Starts recording a change in `view` when it is idle and none is being
/// recorded, before a key is dispatched.
pub(crate) fn begin_change(editor: &mut Editor, view: ViewId) {
    let edits_before = editor.edit_count();
    let state = editor.view_mut(view);
    if state.is_idle() && state.change().is_none() {
        state.set_change(Some(ChangeCandidate {
            count: None,
            edits_before,
            has_command: false,
            keys: Vec::new(),
            register: None,
        }));
    }
}

/// Ends the change being recorded in `view` once it is idle again: kept as
/// the last change when the document changed, else dropped.
pub(crate) fn finish_change(editor: &mut Editor, view: ViewId) {
    let edits = editor.edit_count();
    let state = editor.view_mut(view);
    if !state.is_idle() {
        return;
    }
    let Some(change) = state.take_change() else {
        return;
    };
    if change.edits_before != edits {
        editor.set_last_change(RecordedChange {
            count: change.count,
            keys: change.keys,
            register: change.register,
        });
    }
}

/// Notes in the change being recorded in `view` that a command runs with
/// `count` and `register`, `after_operator` when it is an operator's
/// motion.
pub(crate) fn note_command(
    editor: &mut Editor,
    view: ViewId,
    count: Option<usize>,
    register: Option<char>,
    after_operator: bool,
) {
    let Some(change) = editor.view_mut(view).change_mut() else {
        return;
    };
    if after_operator {
        change.count = count;
    } else if !change.has_command {
        change.count = count;
        change.register = register;
    }
    change.has_command = true;
}

/// Adds `key` to the change being recorded in `view`, if any.
pub(crate) fn note_key(editor: &mut Editor, view: ViewId, key: Key) {
    if let Some(change) = editor.view_mut(view).change_mut() {
        change.keys.push(key);
    }
}

/// `q`: stops recording when recording, else waits for the register to
/// record into.
pub(crate) fn record_macro(cx: &mut CommandContext<'_>) {
    let Some(mut recording) = cx.editor.view_mut(cx.view).take_recording() else {
        return await_char(cx, start_recording);
    };
    let stop_keys = cx.editor.last_matched_len().min(recording.keys.len());
    recording.keys.truncate(recording.keys.len() - stop_keys);
    let keys: Vec<_> = recording.keys.iter().map(Key::to_string).collect();
    cx.editor
        .registers_mut()
        .record_macro(recording.register, keys.join(KEY_SEPARATOR));
}

/// `.`: makes the last change again, with the count typed in place of its
/// own.
pub(crate) fn repeat_change(cx: &mut CommandContext<'_>) {
    let Some(change) = cx.editor.last_change().cloned() else {
        return;
    };
    let mut keys = Vec::new();
    if let Some(register) = change.register {
        keys.extend([char_key(REGISTER_PREFIX), char_key(register)]);
    }
    if let Some(count) = cx.count.or(change.count) {
        keys.extend(count_keys(count));
    }
    keys.extend(change.keys);
    cx.editor.queue_keys(keys);
}

/// `@`: waits for the register to replay.
pub(crate) fn replay_macro(cx: &mut CommandContext<'_>) {
    await_char(cx, replay_register);
}

/// The key that types `c`.
fn char_key(c: char) -> Key {
    Key::new(KeyCode::Char(c), Mods::NONE)
}

/// The keys that type `count`.
fn count_keys(count: usize) -> Vec<Key> {
    let digits = count.to_string();
    let digits = digits.chars().filter(|c| c.is_digit(COUNT_RADIX));
    digits.map(char_key).collect()
}

/// Replays the keys in register `name` (`@` for the last one replayed), a
/// count times.
fn replay_register(cx: &mut CommandContext<'_>, name: char) {
    let name = match (name, cx.editor.last_macro()) {
        (LAST_MACRO, Some(last)) => last,
        (LAST_MACRO, None) => return cx.editor.error("No previous macro".to_string()),
        (name, _) => name,
    };
    if name == LAST_EX_COMMAND {
        return replay_ex_command(cx);
    }
    let Some(text) = cx.editor.registers_mut().read(Some(name)) else {
        return cx.editor.error(format!("Register {name} is empty"));
    };
    let Ok(keys) = parse_sequence(text.text.trim()) else {
        return cx
            .editor
            .error(format!("Register {name} does not hold keys"));
    };
    cx.editor.set_last_macro(name);
    let replayed = keys.repeat(cx.count_or_one());
    cx.editor.queue_keys(replayed);
}

/// `@:`: runs the last ex command line again, a count times, and makes
/// `@@` run it too.
fn replay_ex_command(cx: &mut CommandContext<'_>) {
    let Some(line) = cx.editor.last_ex_command().map(str::to_string) else {
        return cx.editor.error("No previous command line".to_string());
    };
    cx.editor.set_last_macro(LAST_EX_COMMAND);
    for _ in 0..cx.count_or_one() {
        ex::run_line(cx, &line);
    }
}

/// Starts recording keys into register `name` in the view.
fn start_recording(cx: &mut CommandContext<'_>, name: char) {
    if !name.is_ascii_alphanumeric() {
        return cx
            .editor
            .error(format!("Cannot record into register {name}"));
    }
    let recording = MacroRecording {
        keys: Vec::new(),
        register: name,
    };
    cx.editor.view_mut(cx.view).set_recording(Some(recording));
}

// ========================================================================
// Tests
// ========================================================================

#[cfg(test)]
mod tests {
    use super::MAX_REPLAYED_KEYS;
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
    fn test_dot_repeats_an_operator_with_its_motion_and_a_new_count_replaces_the_old() {
        assert_keys("|a b c d e f", "d w .", "|c d e f");
        assert_keys("|a b c d e f g h", "2d w .", "|e f g h");
        assert_keys("|a b c d e f g h", "d 2w 3.", "|f g h");
        assert_keys("|a b c d e f g h i", "2d 2w .", "|i");
        assert_keys("|a b c d e f", "d w 2. .", "|f");
        assert_keys("|a\nb\nc\nd", "d d .", "|c\nd");
    }

    #[test]
    fn test_dot_repeats_insert_sessions_with_their_typed_text() {
        assert_keys("|x\ny", "i ab <esc> j .", "abx\na|by");
        assert_keys("|x\ny", "A ! <esc> j .", "x!\ny|!");
        assert_keys("|one\ntwo", "c w 1 <esc> j 0 .", "1\n|1");
        assert_keys("|a\nb", "o new <esc> .", "a\nnew\nne|w\nb");
    }

    #[test]
    fn test_dot_repeats_x_put_registers_and_search_motions_but_not_motions_or_yanks() {
        assert_keys("|abcdef", "x l .", "b|def");
        assert_keys("|abcdef", "2x .", "|ef");
        assert_keys("|ab", "y l p .", "aa|ab");
        assert_keys("|ab", "\" a y l l \" b y l \" a p .", "aba|a");
        assert_keys("|a x b x c", "d / x <cr> l .", "x|x c");
        assert_keys("|abc", "x u l .", "a|c");
    }

    #[test]
    fn test_dot_repeats_a_visual_change_from_the_cursor() {
        assert_keys("|abcdef", "v l d .", "|ef");
        assert_keys("|a\nb\nc\nd\ne", "V j d .", "|e");
    }

    #[test]
    fn test_ex_commands_are_not_changes_dot_repeats() {
        assert_keys("|aaaa", "x : s/a/b/ <cr> .", "|aa");
    }

    #[test]
    fn test_q_records_keys_and_at_replays_them_with_a_count() {
        let mut harness = Harness::new("|a1\na2\na3\na4\na5");
        harness.keys("q a A ! <esc> j 0 q");
        assert_eq!(harness.marked(), "a1!\n|a2\na3\na4\na5");
        let view = harness.editor.view(harness.view).unwrap();
        assert_eq!(view.recording(), None);
        harness.keys("@ a");
        assert_eq!(harness.marked(), "a1!\na2!\n|a3\na4\na5");
        harness.keys("2@ a");
        assert_eq!(harness.marked(), "a1!\na2!\na3!\na4!\n|a5");
        harness.keys("@ @");
        assert_eq!(harness.marked(), "a1!\na2!\na3!\na4!\n|a5!");
    }

    #[test]
    fn test_a_macro_is_key_notation_in_its_register_and_uppercase_appends() {
        let mut harness = Harness::new("|abc");
        harness.keys("q b x q q B l q");
        assert_eq!(harness.marked(), "b|c");
        harness.keys("o <esc> \" b p");
        assert_eq!(harness.marked(), "bc\nx |l");
        harness.keys("0 \" c d $ k 0 @ c");
        assert_eq!(harness.marked(), "|c\n", "the edited text replays as keys");
    }

    #[test]
    fn test_replaying_a_bad_register_says_so_and_a_self_replaying_macro_ends() {
        let mut harness = Harness::new("|abc");
        harness.keys("@ z @ @ q ! ");
        assert_eq!(
            notices(&mut harness),
            [
                "Register z is empty",
                "No previous macro",
                "Cannot record into register !",
            ]
        );
        harness.keys("i a SPC b <esc> \" d y y @ d");
        assert_eq!(notices(&mut harness), ["Register d does not hold keys"]);
        harness.keys("q e l @ e q @ e");
        let stopped = format!("Macro stopped after {MAX_REPLAYED_KEYS} keys");
        assert_eq!(
            notices(&mut harness),
            ["Register e is empty".to_string(), stopped]
        );
    }
}
