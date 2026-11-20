//! The layered keymap: one [`KeyTrie`] per layer, with layers of a type the
//! host chooses (an editor mode, a buffer context, a popup), and the active
//! layers passed in precedence order with every lookup.
//!
//! # Resolution
//!
//! [`Keymap::active`] stacks layers, highest first, into a set of
//! [`Bindings`] the [`Resolver`](crate::keys::Resolver) can read. A sequence is
//! asked of each active layer in turn, and the first layer that binds it, or
//! binds a longer sequence starting with it, answers for it. So:
//!
//! - A sequence bound in a higher layer hides the same sequence in lower
//!   layers, and a command bound there hides every longer lower binding it
//!   starts: with `g` bound above, the `g g` below cannot be typed.
//! - A prefix in a higher layer shadows a command on that same sequence in a
//!   lower layer: the resolver waits on the prefix, and if it expires the
//!   keys go unmatched rather than running the lower command.
//! - Prefixes merge: with `C-c C-c` bound in a context layer and `C-c C-k` in
//!   the mode layer, both can be typed while both layers are active.
//!
//! # User bindings and conflicts
//!
//! Each layer holds defaults (the host's) and user bindings (from the user's
//! configuration). A user binding replaces the default on its sequence in its
//! layer, whichever is bound first; that is an override, not a conflict.
//!
//! The one conflict is a [`Duplicate`]: two different commands bound to one
//! sequence in one layer by the same party, two defaults or two user
//! bindings. The later binding wins, and the keymap records the pair for the
//! host to report.
//!
//! Prefix overlap is no conflict. Within a layer the resolver can reach both
//! `g` and `g g` (the shorter one runs on a key that does not extend it, or on
//! the timeout), and across layers hiding a lower binding is what precedence
//! is for; which layers are active together is the host's choice per lookup,
//! so the keymap cannot know ahead of time which hides which.
//!
//! # Help
//!
//! [`ActiveLayers`] also answers the questions help asks, under the same
//! rules the resolver follows: what can follow a pending prefix (for a
//! which-key popup), which command a sequence runs (describe-key), and which
//! sequences run a command (where-is). A binding hidden by a higher layer is
//! never reported.

use std::collections::{HashMap, HashSet};
use std::hash::Hash;

use super::key::Key;
use super::trie::{BindError, Bindings, BoundSequence, KeyTrie, Lookup};

// ========================================================================
// Data Structures
// ========================================================================

/// The active layers of a [`Keymap`], highest first, as one set of
/// [`Bindings`]. Made by [`Keymap::active`].
#[derive(Debug)]
pub struct ActiveLayers<'a, L, C> {
    /// The keymap the layers belong to.
    keymap: &'a Keymap<L, C>,
    /// The active layers, highest precedence first.
    layers: &'a [L],
}

/// A key that may follow a pending prefix, and what typing it leads to.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Continuation<'a, C> {
    /// The command the prefix and this key run, if they are bound.
    pub command: Option<&'a C>,
    /// Whether longer bindings continue past this key, so that typing it
    /// leaves a prefix pending.
    pub continues: bool,
    /// The key that may be typed next.
    pub key: Key,
}

/// Two different commands bound to one sequence in one layer by the same
/// party (both defaults, or both user bindings).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Duplicate<L, C> {
    /// The sequence bound twice.
    pub keys: Vec<Key>,
    /// The command bound later, which the sequence now runs.
    pub kept: C,
    /// The layer holding both bindings.
    pub layer: L,
    /// The command bound earlier, which was replaced.
    pub replaced: C,
}

/// Key sequences bound to commands of type `C` in layers of type `L`.
#[derive(Clone, Debug)]
pub struct Keymap<L, C> {
    /// The duplicate bindings found so far, in the order they were bound.
    duplicates: Vec<Duplicate<L, C>>,
    /// The bindings of each layer that has any.
    layers: HashMap<L, Layer<C>>,
}

/// The bindings of one layer.
#[derive(Clone, Debug)]
struct Layer<C> {
    /// Every binding of the layer, defaults and user bindings alike.
    trie: KeyTrie<C>,
    /// The sequences whose binding came from the user.
    user: HashSet<Vec<Key>>,
}

// ========================================================================
// ActiveLayers Implementations
// ========================================================================

impl<'a, L: Eq + Hash, C: PartialEq> ActiveLayers<'a, L, C> {
    /// The command `keys` run in these layers, as the resolver would run it
    /// (at once, or when an ambiguous sequence settles); `None` when they run
    /// nothing, are only a prefix, or are hidden behind a shorter command in
    /// a higher layer. This is describe-key.
    pub fn command_for(&self, keys: &[Key]) -> Option<&'a C> {
        let (_, prefix) = keys.split_last()?;
        if !self.is_pending(prefix) {
            return None;
        }
        match self.find(keys) {
            Lookup::Ambiguous(command) | Lookup::Complete(command) => Some(command),
            Lookup::Prefix | Lookup::Unbound => None,
        }
    }

    /// The keys that may follow `prefix`, sorted by their notation, each with
    /// what it leads to; empty unless `prefix` leaves the resolver pending.
    /// The empty prefix lists the keys that start a binding. This is the data
    /// of a which-key popup.
    pub fn continuations(&self, prefix: &[Key]) -> Vec<Continuation<'a, C>> {
        if !self.is_pending(prefix) {
            return Vec::new();
        }
        let next_keys: HashSet<Key> = self
            .tries()
            .flat_map(|trie| trie.next_keys(prefix))
            .collect();
        let mut path = prefix.to_vec();
        let mut continuations: Vec<_> = next_keys
            .into_iter()
            .filter_map(|key| {
                path.push(key);
                let found = self.find(&path);
                path.pop();
                let (command, continues) = match found {
                    Lookup::Ambiguous(command) => (Some(command), true),
                    Lookup::Complete(command) => (Some(command), false),
                    Lookup::Prefix => (None, true),
                    Lookup::Unbound => return None,
                };
                Some(Continuation {
                    command,
                    continues,
                    key,
                })
            })
            .collect();
        continuations.sort_by_cached_key(|continuation| continuation.key.to_string());
        continuations
    }

    /// Every sequence that runs `command` in these layers, shortest first
    /// and then by notation; sequences a higher layer hides are left out.
    /// This is where-is.
    pub fn sequences_for(&self, command: &C) -> Vec<Vec<Key>> {
        let mut sequences: Vec<_> = self
            .tries()
            .flat_map(KeyTrie::bound_sequences)
            .filter(|bound| bound.command == command)
            .map(|BoundSequence { keys, .. }| keys)
            .filter(|keys| self.command_for(keys) == Some(command))
            .collect();
        sequences.sort_by_cached_key(|keys| (keys.len(), notation(keys)));
        sequences.dedup();
        sequences
    }

    /// What the first layer that knows `keys` holds for them.
    fn find(&self, keys: &[Key]) -> Lookup<'a, C> {
        for trie in self.tries() {
            match trie.lookup(keys) {
                Lookup::Unbound => {}
                found @ (Lookup::Ambiguous(_) | Lookup::Complete(_) | Lookup::Prefix) => {
                    return found;
                }
            }
        }
        Lookup::Unbound
    }

    /// Whether typing `keys` leaves the resolver pending: each of them, and
    /// every sequence they start with, is a prefix of a longer binding.
    fn is_pending(&self, keys: &[Key]) -> bool {
        (1..=keys.len()).all(|len| {
            matches!(
                self.find(&keys[..len]),
                Lookup::Ambiguous(_) | Lookup::Prefix
            )
        })
    }

    /// The tries of the active layers that have bindings, highest first.
    fn tries(&self) -> impl Iterator<Item = &'a KeyTrie<C>> + '_ {
        self.layers
            .iter()
            .filter_map(|layer| self.keymap.layers.get(layer))
            .map(|layer| &layer.trie)
    }
}

impl<L: Eq + Hash, C: PartialEq> Bindings<C> for ActiveLayers<'_, L, C> {
    fn lookup(&self, keys: &[Key]) -> Lookup<'_, C> {
        self.find(keys)
    }
}

// ========================================================================
// Keymap Implementations
// ========================================================================

impl<L: Copy + Eq + Hash, C: Clone + PartialEq> Keymap<L, C> {
    /// An empty keymap, with no layers and no bindings.
    pub fn new() -> Self {
        Self {
            duplicates: Vec::new(),
            layers: HashMap::new(),
        }
    }

    /// The bindings of `layers`, highest precedence first, for one lookup or
    /// one [`Resolver`](crate::keys::Resolver) call. Layers with no bindings are
    /// skipped.
    pub fn active<'a>(&'a self, layers: &'a [L]) -> ActiveLayers<'a, L, C> {
        ActiveLayers {
            keymap: self,
            layers,
        }
    }

    /// Binds `keys` to `command` in `layer` as a default. A user binding
    /// already on `keys` in `layer` stays; another default on it is replaced
    /// and, if it ran a different command, recorded as a [`Duplicate`].
    pub fn bind_default(&mut self, layer: L, keys: &[Key], command: C) -> Result<(), BindError> {
        let bindings = self.layers.entry(layer).or_insert_with(Layer::new);
        if bindings.user.contains(keys) {
            return Ok(());
        }
        let replaced = bindings.trie.bind(keys, command.clone())?;
        self.record_duplicate(layer, keys, command, replaced);
        Ok(())
    }

    /// Binds `keys` to `command` in `layer` as a user binding, replacing the
    /// default on it. Another user binding on `keys` in `layer` is replaced
    /// and, if it ran a different command, recorded as a [`Duplicate`].
    pub fn bind_user(&mut self, layer: L, keys: &[Key], command: C) -> Result<(), BindError> {
        let bindings = self.layers.entry(layer).or_insert_with(Layer::new);
        let replaced = bindings.trie.bind(keys, command.clone())?;
        if bindings.user.insert(keys.to_vec()) {
            return Ok(());
        }
        self.record_duplicate(layer, keys, command, replaced);
        Ok(())
    }

    /// The duplicate bindings found so far, in the order they were bound.
    pub fn duplicates(&self) -> &[Duplicate<L, C>] {
        &self.duplicates
    }

    /// Records a [`Duplicate`] when `command`, just bound to `keys` in
    /// `layer` by the party that bound `replaced`, differs from it.
    fn record_duplicate(&mut self, layer: L, keys: &[Key], command: C, replaced: Option<C>) {
        let Some(replaced) = replaced.filter(|replaced| *replaced != command) else {
            return;
        };
        self.duplicates.push(Duplicate {
            keys: keys.to_vec(),
            kept: command,
            layer,
            replaced,
        });
    }
}

impl<L: Copy + Eq + Hash, C: Clone + PartialEq> Default for Keymap<L, C> {
    fn default() -> Self {
        Self::new()
    }
}

// ========================================================================
// Layer Implementations
// ========================================================================

impl<C> Layer<C> {
    /// A layer with no bindings.
    fn new() -> Self {
        Self {
            trie: KeyTrie::new(),
            user: HashSet::new(),
        }
    }
}

// ========================================================================
// Functions
// ========================================================================

/// The notation of `keys`, keys separated by spaces: `C-x C-f`.
fn notation(keys: &[Key]) -> String {
    keys.iter()
        .map(Key::to_string)
        .collect::<Vec<_>>()
        .join(" ")
}

// ========================================================================
// Tests
// ========================================================================

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::keys::{
        Match, PrefixTimeout, Resolution, Resolver, Timestamp, parse_key, parse_sequence,
    };

    const TIMEOUT: Duration = Duration::from_millis(1000);

    #[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
    enum Layer {
        Dired,
        Insert,
        Normal,
    }

    fn key(token: &str) -> Key {
        parse_key(token).unwrap_or_else(|err| panic!("{token:?}: {err}"))
    }

    fn keys(spec: &str) -> Vec<Key> {
        parse_sequence(spec).unwrap_or_else(|err| panic!("{spec:?}: {err}"))
    }

    fn matched(command: &'static str, spec: &str) -> Resolution<&'static str> {
        Resolution::Matched(Match {
            command,
            keys: keys(spec),
        })
    }

    fn keymap(defaults: &[(Layer, &str, &'static str)]) -> Keymap<Layer, &'static str> {
        let mut keymap = Keymap::new();
        for (layer, spec, command) in defaults {
            keymap.bind_default(*layer, &keys(spec), *command).unwrap();
        }
        keymap
    }

    /// Feeds every key of `spec` at `now` and returns the resolutions.
    fn type_keys(
        keymap: &Keymap<Layer, &'static str>,
        layers: &[Layer],
        spec: &str,
    ) -> Vec<Resolution<&'static str>> {
        let (mut resolver, now) = (Resolver::new(TIMEOUT), Timestamp::default());
        let active = keymap.active(layers);
        keys(spec)
            .into_iter()
            .flat_map(|key| resolver.feed(&active, key, now))
            .collect()
    }

    #[test]
    fn test_the_highest_active_layer_binding_a_sequence_wins() {
        let keymap = keymap(&[
            (Layer::Normal, "d", "delete"),
            (Layer::Dired, "d", "flag-file"),
        ]);
        assert_eq!(
            type_keys(&keymap, &[Layer::Dired, Layer::Normal], "d"),
            vec![matched("flag-file", "d")]
        );
        assert_eq!(
            type_keys(&keymap, &[Layer::Normal, Layer::Dired], "d"),
            vec![matched("delete", "d")]
        );
    }

    #[test]
    fn test_a_sequence_unbound_in_higher_layers_falls_through_to_a_lower_one() {
        let keymap = keymap(&[
            (Layer::Normal, "x", "delete-char"),
            (Layer::Dired, "d", "flag-file"),
        ]);
        assert_eq!(
            type_keys(&keymap, &[Layer::Dired, Layer::Normal], "x"),
            vec![matched("delete-char", "x")]
        );
    }

    #[test]
    fn test_inactive_layers_are_ignored() {
        let keymap = keymap(&[(Layer::Insert, "x", "insert-x")]);
        assert_eq!(
            type_keys(&keymap, &[Layer::Normal], "x"),
            vec![Resolution::Unmatched(keys("x"))]
        );
    }

    #[test]
    fn test_a_prefix_in_a_higher_layer_shadows_a_lower_command_on_that_sequence() {
        let keymap = keymap(&[
            (Layer::Normal, "g", "goto"),
            (Layer::Dired, "g r", "refresh"),
        ]);
        let active = keymap.active(&[Layer::Dired, Layer::Normal]);
        let (mut resolver, start) = (
            Resolver::new(TIMEOUT).with_prefix_timeout(PrefixTimeout::All),
            Timestamp::default(),
        );
        assert_eq!(
            resolver.feed(&active, key("g"), start),
            vec![Resolution::Pending]
        );
        assert_eq!(
            resolver.tick(&active, start + TIMEOUT),
            Some(Resolution::Unmatched(keys("g"))),
            "the lower layer's g must not run"
        );
        assert_eq!(
            type_keys(&keymap, &[Layer::Dired, Layer::Normal], "g r"),
            vec![Resolution::Pending, matched("refresh", "g r")]
        );
    }

    #[test]
    fn test_prefixes_shared_by_several_layers_reach_the_bindings_of_each() {
        let keymap = keymap(&[
            (Layer::Normal, "C-c C-k", "kill"),
            (Layer::Dired, "C-c C-c", "commit"),
        ]);
        let layers = [Layer::Dired, Layer::Normal];
        assert_eq!(
            type_keys(&keymap, &layers, "C-c C-c"),
            vec![Resolution::Pending, matched("commit", "C-c C-c")]
        );
        assert_eq!(
            type_keys(&keymap, &layers, "C-c C-k"),
            vec![Resolution::Pending, matched("kill", "C-c C-k")]
        );
    }

    #[test]
    fn test_a_command_in_a_higher_layer_hides_longer_lower_bindings_it_starts() {
        let keymap = keymap(&[
            (Layer::Normal, "g g", "top"),
            (Layer::Dired, "g", "refresh"),
        ]);
        assert_eq!(
            type_keys(&keymap, &[Layer::Dired, Layer::Normal], "g g"),
            vec![matched("refresh", "g"), matched("refresh", "g")]
        );
    }

    #[test]
    fn test_a_user_binding_beats_the_default_whichever_is_bound_first() {
        let mut keymap = keymap(&[(Layer::Normal, "x", "delete-char")]);
        keymap.bind_user(Layer::Normal, &keys("x"), "cut").unwrap();
        keymap
            .bind_user(Layer::Normal, &keys("C-s"), "search")
            .unwrap();
        keymap
            .bind_default(Layer::Normal, &keys("C-s"), "save")
            .unwrap();
        let layers = [Layer::Normal];
        assert_eq!(type_keys(&keymap, &layers, "x"), vec![matched("cut", "x")]);
        assert_eq!(
            type_keys(&keymap, &layers, "C-s"),
            vec![matched("search", "C-s")]
        );
        assert!(keymap.duplicates().is_empty());
    }

    #[test]
    fn test_two_defaults_on_one_sequence_in_one_layer_are_a_duplicate() {
        let keymap = keymap(&[
            (Layer::Normal, "C-x C-s", "save"),
            (Layer::Dired, "C-x C-s", "save-all"),
            (Layer::Normal, "C-x C-s", "save-all"),
            (Layer::Normal, "q", "quit"),
            (Layer::Normal, "q", "quit"),
        ]);
        assert_eq!(
            keymap.duplicates(),
            [Duplicate {
                keys: keys("C-x C-s"),
                kept: "save-all",
                layer: Layer::Normal,
                replaced: "save",
            }]
        );
        assert_eq!(
            type_keys(&keymap, &[Layer::Normal], "C-x C-s"),
            vec![Resolution::Pending, matched("save-all", "C-x C-s")]
        );
    }

    #[test]
    fn test_two_user_bindings_on_one_sequence_in_one_layer_are_a_duplicate() {
        let mut keymap = keymap(&[(Layer::Normal, "x", "delete-char")]);
        keymap.bind_user(Layer::Normal, &keys("x"), "cut").unwrap();
        keymap.bind_user(Layer::Normal, &keys("x"), "copy").unwrap();
        assert_eq!(
            keymap.duplicates(),
            [Duplicate {
                keys: keys("x"),
                kept: "copy",
                layer: Layer::Normal,
                replaced: "cut",
            }]
        );
    }

    #[test]
    fn test_an_empty_sequence_cannot_be_bound_in_a_layer() {
        let mut keymap: Keymap<Layer, &str> = Keymap::new();
        assert_eq!(
            keymap.bind_default(Layer::Normal, &[], "nothing"),
            Err(BindError::EmptySequence)
        );
        assert_eq!(
            keymap.bind_user(Layer::Normal, &[], "nothing"),
            Err(BindError::EmptySequence)
        );
    }

    fn help_keymap() -> Keymap<Layer, &'static str> {
        keymap(&[
            (Layer::Normal, "C-x C-f", "open"),
            (Layer::Normal, "C-x r", "redo"),
            (Layer::Normal, "C-x k", "kill-buffer"),
            (Layer::Dired, "C-x r t", "rename-tree"),
            (Layer::Dired, "C-x d", "dired"),
            (Layer::Dired, "C-x d d", "dired-other"),
            (Layer::Normal, "g", "goto"),
            (Layer::Dired, "g", "refresh"),
        ])
    }

    fn continuation(
        key_token: &str,
        command: Option<&'static str>,
        continues: bool,
    ) -> (Key, Option<&'static str>, bool) {
        (key(key_token), command, continues)
    }

    #[test]
    fn test_continuations_merge_the_layers_sorted_by_key_as_resolution_sees_them() {
        let keymap = help_keymap();
        let active = keymap.active(&[Layer::Dired, Layer::Normal]);
        let found: Vec<_> = active
            .continuations(&keys("C-x"))
            .into_iter()
            .map(|next| (next.key, next.command.copied(), next.continues))
            .collect();
        assert_eq!(
            found,
            vec![
                continuation("C-f", Some("open"), false),
                continuation("d", Some("dired"), true),
                continuation("k", Some("kill-buffer"), false),
                continuation("r", None, true),
            ]
        );
    }

    #[test]
    fn test_only_a_pending_prefix_has_continuations() {
        let keymap = help_keymap();
        let active = keymap.active(&[Layer::Dired, Layer::Normal]);
        assert!(active.continuations(&keys("C-x C-f")).is_empty());
        assert!(active.continuations(&keys("q")).is_empty());
        assert!(active.continuations(&keys("g")).is_empty());
        let top: Vec<_> = active
            .continuations(&[])
            .into_iter()
            .map(|next| next.key)
            .collect();
        assert_eq!(top, vec![key("C-x"), key("g")]);
    }

    #[test]
    fn test_describe_key_names_the_command_a_sequence_runs_in_the_active_layers() {
        let keymap = help_keymap();
        let dired = keymap.active(&[Layer::Dired, Layer::Normal]);
        assert_eq!(dired.command_for(&keys("g")), Some(&"refresh"));
        assert_eq!(dired.command_for(&keys("C-x d")), Some(&"dired"));
        assert_eq!(dired.command_for(&keys("C-x r")), None);
        assert_eq!(dired.command_for(&keys("C-x")), None);
        let normal = keymap.active(&[Layer::Normal]);
        assert_eq!(normal.command_for(&keys("C-x r")), Some(&"redo"));
    }

    #[test]
    fn test_describe_key_ignores_a_binding_hidden_behind_a_higher_command() {
        let keymap = keymap(&[
            (Layer::Normal, "g g", "top"),
            (Layer::Dired, "g", "refresh"),
        ]);
        let active = keymap.active(&[Layer::Dired, Layer::Normal]);
        assert_eq!(active.command_for(&keys("g g")), None);
        assert!(active.sequences_for(&"top").is_empty());
    }

    #[test]
    fn test_where_is_lists_the_reachable_sequences_of_a_command_shortest_first() {
        let keymap = keymap(&[
            (Layer::Normal, "C-x C-s", "save"),
            (Layer::Normal, "C-s", "save"),
            (Layer::Normal, "<f2>", "save"),
            (Layer::Dired, "C-s", "search"),
            (Layer::Dired, "C-x C-s", "save"),
            (Layer::Insert, "C-c s", "save"),
        ]);
        let active = keymap.active(&[Layer::Dired, Layer::Normal]);
        assert_eq!(
            active.sequences_for(&"save"),
            vec![keys("<f2>"), keys("C-x C-s")]
        );
        assert_eq!(active.sequences_for(&"search"), vec![keys("C-s")]);
        assert!(active.sequences_for(&"missing").is_empty());
    }
}
