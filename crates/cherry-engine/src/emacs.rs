//! The Emacs editing style ([`EditingStyle::Emacs`]) beyond the region:
//! the prefix argument, Escape as Meta, and `M-x`.
//!
//! `C-u` gives the next command the argument 4, each `C-u` more
//! multiplies it by 4, and digits typed after it give the number itself:
//! `C-u 1 2 C-f` moves 12 characters, `C-u x` types `xxxx`. Escape
//! followed by a key is that key with Meta, as on a terminal (`ESC f` is
//! `M-f`); Escape twice does what Escape does alone in the other styles.
//! `M-x` opens a prompt for the name of a command to run, where Tab
//! completes the name. `M-5` (or `ESC 5`) gives an argument too, as a
//! digit after `C-u` does.
//!
//! [`EditingStyle::Emacs`]: crate::EditingStyle::Emacs

use crate::keys::{Key, KeyCode, Mods};

use super::command::CommandContext;
use super::editor::{Editor, ViewId};
use super::prompt::{self, PromptKind};
use super::view::UniversalArgument;

// ========================================================================
// Constants
// ========================================================================

/// The count `C-u` gives, and multiplies by for each `C-u` more.
const UNIVERSAL_FACTOR: usize = 4;

/// The radix of the digits of a prefix argument.
const DECIMAL: u32 = 10;

/// How many command names Tab at `M-x` lists, at most.
const LISTED_NAMES: usize = 8;

// ========================================================================
// Functions
// ========================================================================

/// `M-x`: opens a prompt for the name of a command to run.
pub(crate) fn execute_extended_command(cx: &mut CommandContext<'_>) {
    prompt::open(cx, PromptKind::ExtendedCommand);
}

/// Runs the command `name` typed at `M-x` with the prompt's `count`, or
/// says there is none by that name.
pub(crate) fn run_named_command(cx: &mut CommandContext<'_>, name: &str, count: Option<usize>) {
    if cx.editor.run_command(cx.view, name.trim(), count).is_err() {
        cx.editor
            .error(format!("No command is named {:?}", name.trim()));
    }
}

/// The key `key` stands for in `view` in the Emacs style, where Escape
/// gives the key after it Meta: `None` for an Escape that waits for that
/// key, the key with Meta after one, and Escape itself after one.
pub(crate) fn translate_escape(editor: &mut Editor, view: ViewId, key: Key) -> Option<Key> {
    let escape = Key::new(KeyCode::Esc, Mods::NONE);
    let state = editor.view_mut(view);
    if state.take_escape_meta() {
        return Some(match key == escape {
            true => escape,
            false => Key::new(key.code, key.mods | Mods::ALT),
        });
    }
    if key == escape && state.pending_keys().is_empty() {
        state.set_escape_meta(true);
        return None;
    }
    Some(key)
}

/// Takes `key` as a digit of the prefix argument in `view`, as `M-5` or
/// `ESC 5` gives one, when it is Meta and a digit with nothing half-typed;
/// returns whether it did.
pub(crate) fn take_digit_argument(editor: &mut Editor, view: ViewId, key: Key) -> bool {
    let KeyCode::Char(c) = key.code else {
        return false;
    };
    let state = editor.view_mut(view);
    let Some(digit) = c.to_digit(DECIMAL).filter(|_| key.mods == Mods::ALT) else {
        return false;
    };
    if !state.pending_keys().is_empty() {
        return false;
    }
    let count = match state.universal_argument() {
        Some(UniversalArgument::Digits) => state.count_mut().unwrap_or(0),
        Some(UniversalArgument::Fresh) | None => 0,
    };
    let digit = digit as usize;
    *state.count_mut() = Some(count.saturating_mul(DECIMAL as usize).saturating_add(digit));
    state.set_universal_argument(Some(UniversalArgument::Digits));
    true
}

/// Tab at `M-x`: completes the command name typed to the longest start
/// the commands it may name share, and lists them when there are several.
pub(crate) fn complete_command_name(cx: &mut CommandContext<'_>) {
    let Some(typed) = cx
        .editor
        .view_ref(cx.view)
        .prompt()
        .filter(|prompt| prompt.kind() == PromptKind::ExtendedCommand)
        .map(|prompt| prompt.text())
    else {
        return;
    };
    let mut names: Vec<String> = cx
        .editor
        .command_ids()
        .filter(|id| id.starts_with(&typed))
        .map(str::to_string)
        .collect();
    names.sort();
    let Some(first) = names.first() else {
        return cx.editor.error(format!("No command starts with {typed:?}"));
    };
    let shared = names.iter().fold(first.clone(), |shared, name| {
        shared
            .chars()
            .zip(name.chars())
            .take_while(|(a, b)| a == b)
            .map(|(c, _)| c)
            .collect()
    });
    if let Some(prompt) = cx.editor.view_mut(cx.view).prompt_mut() {
        prompt.replace_line(&shared);
    }
    if names.len() > 1 {
        let shown: Vec<&str> = names
            .iter()
            .take(LISTED_NAMES)
            .map(String::as_str)
            .collect();
        let more = names.len().saturating_sub(LISTED_NAMES);
        let tail = if more > 0 {
            format!(" and {more} more")
        } else {
            String::new()
        };
        cx.editor.info(format!("{}{tail}", shown.join(" ")));
    }
}

/// `C-u`: gives the next command an argument of 4, or 4 times the one
/// already given, and lets digits typed next give it instead.
pub(crate) fn universal_argument(cx: &mut CommandContext<'_>) {
    let count = cx.count.map_or(UNIVERSAL_FACTOR, |count| {
        count.saturating_mul(UNIVERSAL_FACTOR)
    });
    let state = cx.editor.view_mut(cx.view);
    *state.count_mut() = Some(count);
    state.set_universal_argument(Some(UniversalArgument::Fresh));
}

// ========================================================================
// Tests
// ========================================================================

#[cfg(test)]
mod tests {
    use crate::options::{EditingStyle, EditorOptions};
    use crate::request::HostRequest;
    use crate::testing::Harness;
    use crate::view::Mode;

    /// A harness editing `marked` in the Emacs style.
    fn emacs(marked: &str) -> Harness {
        let options = EditorOptions {
            editing_style: EditingStyle::Emacs,
            ..EditorOptions::default()
        };
        Harness::with_options(marked, options)
    }

    #[test]
    fn test_typing_inserts_at_the_cursor_and_leaves_the_region_alone() {
        let mut harness = emacs("|one");
        harness.keys("C-SPC C-f X C-f");
        assert_eq!(harness.marked(), "oXn|e", "typing dropped the mark");
    }

    #[test]
    fn test_c_u_gives_an_argument_of_four_or_the_digits_typed_after_it() {
        let mut harness = emacs("|abcdefghijklmnopqrstuvwxyz");
        harness.keys("C-u C-f");
        assert_eq!(harness.marked(), "abcd|efghijklmnopqrstuvwxyz");
        harness.keys("C-u 1 2 C-f");
        assert_eq!(harness.marked(), "abcdefghijklmnop|qrstuvwxyz");
        harness.keys("C-u C-u C-b C-u 3 x");
        assert_eq!(harness.marked(), "xxx|abcdefghijklmnopqrstuvwxyz");
        harness.keys("C-a 5");
        assert_eq!(
            harness.marked(),
            "5|xxxabcdefghijklmnopqrstuvwxyz",
            "a digit types"
        );
    }

    #[test]
    fn test_c_u_c_k_kills_whole_lines() {
        let mut harness = emacs("o|ne\ntwo\nthree");
        harness.keys("C-u 2 C-k");
        assert_eq!(harness.marked(), "o|three");
    }

    #[test]
    fn test_escape_then_a_key_is_the_key_with_meta() {
        let mut harness = emacs("|one two");
        harness.keys("<esc> f");
        assert_eq!(harness.marked(), "one| two");
        harness.keys("C-SPC C-e <esc> <esc> X");
        assert_eq!(
            harness.marked(),
            "one twoX|",
            "Escape twice drops the region"
        );
    }

    #[test]
    fn test_m_x_runs_a_command_by_name() {
        let mut harness = emacs("one |two");
        harness.keys("M-x");
        assert_eq!(harness.mode(), Mode::CommandLine);
        harness.keys("kill-line <cr>");
        assert_eq!(harness.mode(), Mode::Insert);
        assert_eq!(harness.marked(), "one |");
        harness.keys("M-x no-such-command <cr>");
        let requests = harness.requests();
        assert!(
            matches!(requests.as_slice(), [HostRequest::Notice(_)]),
            "{requests:?}"
        );
    }

    #[test]
    fn test_m_percent_asks_at_each_match_of_the_text_typed() {
        let mut harness = emacs("a.b a.b axb |a.b");
        harness.keys("C-a M-% a.b <cr> c <cr> SPC n !");
        assert_eq!(harness.mode(), Mode::Insert);
        assert_eq!(
            harness.marked().replace('|', ""),
            "c a.b axb c",
            "literal text"
        );
    }

    #[test]
    fn test_c_s_and_c_r_step_through_the_matches_while_searching() {
        let mut harness = emacs("|a1 a2 a3 a4");
        harness.keys("C-s a C-s C-s <cr>");
        assert_eq!(harness.marked(), "a1 a2 a3 |a4");
        harness.keys("C-a C-s a C-s C-s C-r <cr>");
        assert_eq!(harness.marked(), "a1 a2 |a3 a4", "C-r stepped back one");
        harness.keys("C-a C-s a C-s C-g");
        assert_eq!(harness.marked(), "|a1 a2 a3 a4", "C-g goes back");
        harness.keys("C-s a3 <cr> C-a C-s C-s <cr>");
        assert_eq!(harness.marked(), "a1 a2 |a3 a4", "C-s C-s searches again");
    }

    #[test]
    fn test_meta_digits_give_the_argument() {
        let mut harness = emacs("|abcdefghijklmnop");
        harness.keys("M-1 M-2 C-f");
        assert_eq!(harness.marked(), "abcdefghijkl|mnop");
        harness.keys("<esc> 3 x");
        assert_eq!(harness.marked(), "abcdefghijklxxx|mnop", "ESC 3 is M-3");
    }

    #[test]
    fn test_tab_at_m_x_completes_the_command_name() {
        let mut harness = emacs("one |two");
        harness.keys("M-x kill-li <tab>");
        let typed = |harness: &Harness| {
            let view = harness.editor.view(harness.view).expect("open");
            view.prompt().map(|prompt| prompt.text())
        };
        assert_eq!(typed(&harness).as_deref(), Some("kill-line"));
        harness.keys("<cr> M-x kill <tab>");
        assert_eq!(
            typed(&harness).as_deref(),
            Some("kill-"),
            "the shared start"
        );
        let requests = harness.requests();
        assert!(
            matches!(requests.as_slice(), [HostRequest::Notice(_)]),
            "the names listed: {requests:?}"
        );
        harness.keys("<esc>");
        assert_eq!(harness.marked(), "one |");
    }

    #[test]
    fn test_c_x_c_x_swaps_the_cursor_and_the_mark() {
        let mut harness = emacs("|one two");
        harness.keys("C-SPC M-f C-x C-x");
        assert_eq!(harness.marked(), "|one two");
        harness.keys("C-w");
        assert_eq!(harness.marked(), "| two", "the region stays active");
    }
}
