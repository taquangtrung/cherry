//! Keymap files: a user's rebinding written once, in a file the host
//! reads, and applied to the editor in one call.
//!
//! A [`KeymapFile`] is a list of entries, each a command, the mode its keys
//! work in, and the keys in Cherry's key notation (`C-x C-s`, `<f2>`).
//! An entry adds its keys to the command's, or with `replace` takes the
//! place of all its keys, as a user's configuration rebinds; a `replace`
//! with no keys unbinds the command. With the `serde` feature it reads from
//! any format serde knows; in TOML:
//!
//! ```toml
//! [[keys]]
//! mode = "normal"
//! command = "save"
//! keys = ["C-s"]
//!
//! [[keys]]
//! mode = "insert"
//! command = "kill-line"
//! keys = []
//! replace = true
//! ```
//!
//! Modes are written in kebab case: `normal`, `insert`, `visual`,
//! `visual-line`, `visual-block`, `operator-pending`, `replace`, and
//! `command-line`.

use crate::command::CommandError;
use crate::view::Mode;

use super::Editor;

// ========================================================================
// Data Structures
// ========================================================================

/// A user's key bindings, to apply with [`Editor::apply_keymap`].
#[derive(Clone, Debug, Default, Eq, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
pub struct KeymapFile {
    /// The bindings, applied in order.
    #[cfg_attr(feature = "serde", serde(default))]
    pub keys: Vec<KeymapEntry>,
}

/// One binding of a [`KeymapFile`].
#[derive(Clone, Debug, Eq, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
pub struct KeymapEntry {
    /// The id of the command the keys run: a built-in one (`save`,
    /// `kill-line`) or one the host registered.
    pub command: String,
    /// The key sequences, in Cherry's key notation.
    #[cfg_attr(feature = "serde", serde(default))]
    pub keys: Vec<String>,
    /// The mode the keys work in.
    pub mode: Mode,
    /// Whether the keys take the place of all the command's keys in the
    /// mode, rather than adding to them.
    #[cfg_attr(feature = "serde", serde(default))]
    pub replace: bool,
}

// ========================================================================
// Editor Implementations
// ========================================================================

impl Editor {
    /// Applies every entry of `keymap` in order, as [`Editor::bind`] or,
    /// for an entry that replaces, [`Editor::override_keys`] would; an entry
    /// that is rejected (an unknown command, a key that does not parse)
    /// changes nothing and the rest still apply. Returns why each rejected
    /// entry was, with its index in `keymap`, for the host to report.
    pub fn apply_keymap(&mut self, keymap: &KeymapFile) -> Vec<(usize, CommandError)> {
        keymap
            .keys
            .iter()
            .enumerate()
            .filter_map(|(index, entry)| {
                let specs: Vec<&str> = entry.keys.iter().map(String::as_str).collect();
                let applied = match entry.replace {
                    true => self.override_keys(entry.mode, &entry.command, &specs),
                    false => self.bind(entry.mode, &specs, &entry.command),
                };
                applied.err().map(|error| (index, error))
            })
            .collect()
    }
}

// ========================================================================
// Tests
// ========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::Harness;

    /// An entry binding `keys` to `command` in `mode`.
    fn entry(mode: Mode, command: &str, keys: &[&str], replace: bool) -> KeymapEntry {
        KeymapEntry {
            command: command.to_string(),
            keys: keys.iter().map(|key| key.to_string()).collect(),
            mode,
            replace,
        }
    }

    #[test]
    fn test_a_keymap_adds_and_replaces_keys_and_reports_each_bad_entry() {
        let mut harness = Harness::new("|one two");
        let keymap = KeymapFile {
            keys: vec![
                entry(Mode::Normal, "word-forward", &["<f2>"], false),
                entry(Mode::Normal, "no-such-command", &["<f3>"], false),
                entry(Mode::Normal, "delete-char", &[], true),
                entry(Mode::Normal, "word-forward", &["<<bad"], false),
            ],
        };
        let rejected = harness.editor.apply_keymap(&keymap);
        let indexes: Vec<usize> = rejected.iter().map(|(index, _)| *index).collect();
        assert_eq!(indexes, [1, 3]);
        harness.keys("<f2> x");
        assert_eq!(harness.marked(), "one |two", "F2 moves, x deletes nothing");
    }

    #[cfg(feature = "serde")]
    #[test]
    fn test_a_keymap_reads_from_json() {
        let json = r#"{"keys": [
            {"mode": "normal", "command": "word-forward", "keys": ["<f2>"]},
            {"mode": "visual-line", "command": "exit-visual", "keys": ["q"]}
        ]}"#;
        let keymap: KeymapFile = serde_json::from_str(json).expect("parses");
        assert_eq!(keymap.keys[1].mode, Mode::VisualLine);
        let mut harness = Harness::new("|one two");
        assert!(harness.editor.apply_keymap(&keymap).is_empty());
        harness.keys("<f2> V q");
        assert_eq!(harness.mode(), Mode::Normal);
        assert_eq!(harness.marked(), "one |two");
    }
}
