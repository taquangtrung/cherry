//! Key tables: each binding written once, with the id it dispatches to, the
//! title and group help shows it under, and its default keys. The same table
//! fills a [`Keymap`] layer for dispatch, lists the help rows, and takes the
//! user's overrides, so what a key does and what the help says it does
//! cannot disagree.
//!
//! An override gives an id new keys in place of all its defaults; no keys
//! unbinds it. Overridden keys are bound as user bindings, so one that takes
//! a key another id has by default wins it, and the other id keeps the rest
//! of its keys (see [`Keymap`] for user bindings over defaults).
//!
//! A table checks its defaults when it is made and every override as it is
//! applied: a spec that does not parse or an id that is repeated would
//! otherwise silently never match.

use std::collections::{HashMap, HashSet};
use std::hash::Hash;

use thiserror::Error;

use super::key::Key;
use super::keymap::Keymap;
use super::notation::{ParseError, parse_sequence};
use super::trie::BindError;

// ========================================================================
// Data Structures
// ========================================================================

/// One binding of a key table, written by the host, usually in a `const`
/// table.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Binding {
    /// The heading help lists the binding under: `Files`.
    pub group: &'static str,
    /// The name the binding dispatches to and the user overrides it by,
    /// unique within its table: `save`.
    pub id: &'static str,
    /// Every key sequence that runs it by default, in the key notation
    /// (`C-x C-s`, `<f2>`), in the order help lists them.
    pub keys: &'static [&'static str],
    /// What it does, as help shows it: `Save the file`.
    pub title: &'static str,
}

/// One row of help: a binding and the keys in force for it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HelpRow {
    /// The heading the row is listed under.
    pub group: &'static str,
    /// The id the row's keys dispatch to.
    pub id: &'static str,
    /// The key sequences in force: the user's where they overrode the id,
    /// else the defaults. Empty when the user unbound it.
    pub keys: Vec<Vec<Key>>,
    /// What the binding does.
    pub title: &'static str,
}

/// A spec that does not parse, and the binding it was written for.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InvalidSpec {
    /// Why it does not parse.
    pub error: ParseError,
    /// The id of the binding it was written for.
    pub id: String,
    /// The spec as written.
    pub spec: String,
}

/// Bindings written once, driving dispatch, help, and rebinding.
#[derive(Clone, Debug)]
pub struct KeyTable {
    /// The bindings, in table order, with their defaults parsed.
    entries: Vec<Entry>,
    /// The user's keys, by the id they override.
    overrides: HashMap<&'static str, Vec<Vec<Key>>>,
}

/// Why a table or an override was rejected.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum TableError {
    /// A key spec does not parse.
    #[error("{:?} for {:?} is not a key sequence: {}", .0.spec, .0.id, .0.error)]
    InvalidSpec(InvalidSpec),
    /// Two bindings of the table share an id, so one could not be
    /// overridden without the other.
    #[error("the id {0:?} is used by more than one binding")]
    RepeatedId(String),
    /// An override names an id the table has no binding for.
    #[error("no binding has the id {0:?}")]
    UnknownId(String),
}

/// One binding with its default keys parsed.
#[derive(Clone, Debug)]
struct Entry {
    /// The binding as written.
    binding: &'static Binding,
    /// Its default key sequences, in the order written.
    defaults: Vec<Vec<Key>>,
}

// ========================================================================
// KeyTable Implementations
// ========================================================================

impl KeyTable {
    /// A table of `bindings`, with no overrides: one `const` table, or
    /// several chained (`NORMAL.iter().chain(MOTIONS)`) when modes share
    /// bindings. Rejected when a default spec does not parse or an id is
    /// repeated.
    pub fn new(bindings: impl IntoIterator<Item = &'static Binding>) -> Result<Self, TableError> {
        let mut ids = HashSet::new();
        let mut entries = Vec::new();
        for binding in bindings {
            if !ids.insert(binding.id) {
                return Err(TableError::RepeatedId(binding.id.to_string()));
            }
            entries.push(Entry {
                binding,
                defaults: parse_specs(binding.id, binding.keys)?,
            });
        }
        Ok(Self {
            entries,
            overrides: HashMap::new(),
        })
    }

    /// Binds the keys in force for every binding into `layer` of `keymap`,
    /// each to its id: defaults as defaults, overridden keys as user
    /// bindings. The keymap's command type is any made from an id: the id
    /// itself (`&'static str`), or an owned id type of the host's, so a
    /// host may bind ids it builds at runtime into the same keymap.
    pub fn bind_into<L, C>(&self, keymap: &mut Keymap<L, C>, layer: L) -> Result<(), BindError>
    where
        L: Copy + Eq + Hash,
        C: Clone + From<&'static str> + PartialEq,
    {
        for entry in &self.entries {
            let id = entry.binding.id;
            match self.overrides.get(id) {
                Some(keys) => {
                    for sequence in keys {
                        keymap.bind_user(layer, sequence, C::from(id))?;
                    }
                }
                None => {
                    for sequence in &entry.defaults {
                        keymap.bind_default(layer, sequence, C::from(id))?;
                    }
                }
            }
        }
        Ok(())
    }

    /// One help row per binding, in table order, with the keys in force.
    pub fn help_rows(&self) -> Vec<HelpRow> {
        self.entries
            .iter()
            .map(|entry| HelpRow {
                group: entry.binding.group,
                id: entry.binding.id,
                keys: self.keys_in_force(entry).to_vec(),
                title: entry.binding.title,
            })
            .collect()
    }

    /// Gives `id` the keys `specs` in place of its defaults and of any
    /// earlier override; no specs unbinds it. Rejected, changing nothing,
    /// when no binding has `id` or a spec does not parse.
    pub fn override_keys(&mut self, id: &str, specs: &[&str]) -> Result<(), TableError> {
        let entry = self
            .entry(id)
            .ok_or_else(|| TableError::UnknownId(id.to_string()))?;
        let (id, keys) = (entry.binding.id, parse_specs(id, specs)?);
        self.overrides.insert(id, keys);
        Ok(())
    }

    /// The title of the binding with `id`, for showing a dispatched id, a
    /// which-key [`Continuation`](crate::keys::Continuation), or a describe-key
    /// answer by name.
    pub fn title(&self, id: &str) -> Option<&'static str> {
        self.entry(id).map(|entry| entry.binding.title)
    }

    /// The entry of the binding with `id`.
    fn entry(&self, id: &str) -> Option<&Entry> {
        self.entries.iter().find(|entry| entry.binding.id == id)
    }

    /// The key sequences in force for `entry`.
    fn keys_in_force<'a>(&'a self, entry: &'a Entry) -> &'a [Vec<Key>] {
        self.overrides
            .get(entry.binding.id)
            .unwrap_or(&entry.defaults)
    }
}

// ========================================================================
// Functions
// ========================================================================

/// The key sequences `specs` name, or the first spec that does not parse,
/// reported against the binding `id`.
fn parse_specs(id: &str, specs: &[&str]) -> Result<Vec<Vec<Key>>, TableError> {
    specs
        .iter()
        .map(|spec| {
            parse_sequence(spec).map_err(|error| {
                TableError::InvalidSpec(InvalidSpec {
                    error,
                    id: id.to_string(),
                    spec: spec.to_string(),
                })
            })
        })
        .collect()
}

// ========================================================================
// Tests
// ========================================================================

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::keys::{Duplicate, Match, Resolution, Resolver, Timestamp, parse_sequence};

    const LAYER: () = ();
    const TIMEOUT: Duration = Duration::from_millis(1000);

    const BINDINGS: &[Binding] = &[
        Binding {
            group: "Files",
            id: "save",
            keys: &["C-x C-s", "C-s"],
            title: "Save the file",
        },
        Binding {
            group: "Files",
            id: "open",
            keys: &["C-x C-f"],
            title: "Open a file",
        },
        Binding {
            group: "Editing",
            id: "undo",
            keys: &["C-/", "C-_"],
            title: "Undo",
        },
    ];

    fn keys(spec: &str) -> Vec<Key> {
        parse_sequence(spec).unwrap_or_else(|err| panic!("{spec:?}: {err}"))
    }

    fn table() -> KeyTable {
        KeyTable::new(BINDINGS).expect("the test table is valid")
    }

    fn keymap(table: &KeyTable) -> Keymap<(), &'static str> {
        let mut keymap = Keymap::new();
        table.bind_into(&mut keymap, LAYER).unwrap();
        keymap
    }

    /// The id `spec` runs in `keymap`, if it completes a binding.
    fn dispatch<C: Clone + PartialEq>(keymap: &Keymap<(), C>, spec: &str) -> Option<C> {
        let (mut resolver, now) = (Resolver::new(TIMEOUT), Timestamp::default());
        let active = keymap.active(&[LAYER]);
        let mut resolutions: Vec<_> = keys(spec)
            .into_iter()
            .flat_map(|key| resolver.feed(&active, key, now))
            .collect();
        resolutions.extend(resolver.tick(&active, now + TIMEOUT));
        match resolutions.pop()? {
            Resolution::Matched(Match { command, .. }) => Some(command),
            Resolution::Pending | Resolution::Unmatched(_) => None,
        }
    }

    #[test]
    fn test_every_default_spec_of_a_binding_dispatches_to_its_id() {
        let keymap = keymap(&table());
        assert_eq!(dispatch(&keymap, "C-x C-s"), Some("save"));
        assert_eq!(dispatch(&keymap, "C-s"), Some("save"));
        assert_eq!(dispatch(&keymap, "C-x C-f"), Some("open"));
        assert_eq!(dispatch(&keymap, "C-_"), Some("undo"));
        assert_eq!(dispatch(&keymap, "C-x C-c"), None);
    }

    #[test]
    fn test_help_rows_follow_the_table_order_with_the_keys_in_force() {
        let mut table = table();
        table.override_keys("open", &["C-o"]).unwrap();
        let rows = table.help_rows();
        let summary: Vec<_> = rows
            .iter()
            .map(|row| (row.group, row.id, row.title, row.keys.clone()))
            .collect();
        assert_eq!(
            summary,
            vec![
                (
                    "Files",
                    "save",
                    "Save the file",
                    vec![keys("C-x C-s"), keys("C-s")]
                ),
                ("Files", "open", "Open a file", vec![keys("C-o")]),
                ("Editing", "undo", "Undo", vec![keys("C-/"), keys("C-_")]),
            ]
        );
    }

    #[test]
    fn test_an_override_replaces_every_default_spec_of_its_id() {
        let mut table = table();
        table.override_keys("save", &["C-x s", "<f2>"]).unwrap();
        let keymap = keymap(&table);
        assert_eq!(dispatch(&keymap, "C-x s"), Some("save"));
        assert_eq!(dispatch(&keymap, "<f2>"), Some("save"));
        assert_eq!(dispatch(&keymap, "C-s"), None);
        assert_eq!(dispatch(&keymap, "C-x C-s"), None);
    }

    #[test]
    fn test_an_override_with_no_specs_unbinds_the_id() {
        let mut table = table();
        table.override_keys("undo", &[]).unwrap();
        assert_eq!(dispatch(&keymap(&table), "C-/"), None);
        assert!(table.help_rows()[2].keys.is_empty());
    }

    #[test]
    fn test_an_override_onto_another_ids_default_key_takes_it_over() {
        let mut table = table();
        table.override_keys("open", &["C-s"]).unwrap();
        let keymap = keymap(&table);
        assert_eq!(dispatch(&keymap, "C-s"), Some("open"));
        assert_eq!(dispatch(&keymap, "C-x C-s"), Some("save"));
        assert!(keymap.duplicates().is_empty());
    }

    #[test]
    fn test_a_table_binds_into_an_owned_id_keymap_beside_runtime_built_ids() {
        let mut table = table();
        table.override_keys("open", &["C-o"]).unwrap();
        let mut keymap: Keymap<(), String> = Keymap::new();
        table.bind_into(&mut keymap, LAYER).unwrap();
        let tag = format!("settag:{}", "red");
        keymap
            .bind_user(LAYER, &keys("C-x C-s"), tag.clone())
            .unwrap();
        assert_eq!(dispatch(&keymap, "C-x C-s"), Some(tag), "user over default");
        assert_eq!(dispatch(&keymap, "C-s"), Some("save".to_string()));
        assert_eq!(dispatch(&keymap, "C-o"), Some("open".to_string()));
        assert_eq!(dispatch(&keymap, "C-x C-f"), None, "overridden away");
    }

    #[test]
    fn test_a_bad_override_is_rejected_and_keeps_the_keys_in_force() {
        let mut table = table();
        table.override_keys("save", &["<f2>"]).unwrap();
        assert_eq!(
            table.override_keys("save", &["C-x", "C-<nope>"]),
            Err(TableError::InvalidSpec(InvalidSpec {
                error: ParseError::UnknownName("nope".into()),
                id: "save".into(),
                spec: "C-<nope>".into(),
            }))
        );
        assert_eq!(
            table.override_keys("sav", &["C-s"]),
            Err(TableError::UnknownId("sav".into()))
        );
        assert_eq!(table.help_rows()[0].keys, vec![keys("<f2>")]);
    }

    #[test]
    fn test_a_table_with_a_misspelled_default_spec_is_rejected() {
        const BAD: &[Binding] = &[Binding {
            group: "Files",
            id: "save",
            keys: &["C-s", "C-x  C-ss"],
            title: "Save the file",
        }];
        assert_eq!(
            KeyTable::new(BAD).err(),
            Some(TableError::InvalidSpec(InvalidSpec {
                error: ParseError::MultipleChars("ss".into()),
                id: "save".into(),
                spec: "C-x  C-ss".into(),
            }))
        );
    }

    #[test]
    fn test_a_table_with_a_repeated_id_is_rejected() {
        const REPEATED: &[Binding] = &[
            Binding {
                group: "Files",
                id: "save",
                keys: &["C-s"],
                title: "Save the file",
            },
            Binding {
                group: "Files",
                id: "save",
                keys: &["C-x C-s"],
                title: "Save the file again",
            },
        ];
        assert_eq!(
            KeyTable::new(REPEATED).err(),
            Some(TableError::RepeatedId("save".into()))
        );
    }

    #[test]
    fn test_chained_tables_bind_both_and_reject_an_id_repeated_across_them() {
        const SHARED: &[Binding] = &[Binding {
            group: "Motion",
            id: "down",
            keys: &["j"],
            title: "Move down",
        }];
        const OWN: &[Binding] = &[Binding {
            group: "Editing",
            id: "delete",
            keys: &["d"],
            title: "Delete",
        }];
        let table = KeyTable::new(OWN.iter().chain(SHARED)).unwrap();
        let ids: Vec<_> = table.help_rows().iter().map(|row| row.id).collect();
        assert_eq!(ids, ["delete", "down"]);
        assert_eq!(
            KeyTable::new(SHARED.iter().chain(SHARED)).err(),
            Some(TableError::RepeatedId("down".into()))
        );
    }

    #[test]
    fn test_two_ids_on_one_default_key_surface_as_a_duplicate() {
        const CLASH: &[Binding] = &[
            Binding {
                group: "Files",
                id: "save",
                keys: &["C-s"],
                title: "Save the file",
            },
            Binding {
                group: "Search",
                id: "search",
                keys: &["C-s"],
                title: "Search forward",
            },
        ];
        let keymap = keymap(&KeyTable::new(CLASH).unwrap());
        assert_eq!(
            keymap.duplicates(),
            [Duplicate {
                keys: keys("C-s"),
                kept: "search",
                layer: LAYER,
                replaced: "save",
            }]
        );
    }

    #[test]
    fn test_which_key_rows_pair_each_continuation_with_its_title() {
        let table = table();
        let keymap = keymap(&table);
        let active = keymap.active(&[LAYER]);
        let rows: Vec<_> = active
            .continuations(&keys("C-x"))
            .into_iter()
            .map(|next| {
                (
                    next.key.to_string(),
                    next.command.and_then(|id| table.title(id)),
                )
            })
            .collect();
        assert_eq!(
            rows,
            vec![
                ("C-f".to_string(), Some("Open a file")),
                ("C-s".to_string(), Some("Save the file")),
            ]
        );
        assert_eq!(table.title("missing"), None);
    }
}
