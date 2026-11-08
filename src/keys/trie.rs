//! The key trie: key sequences bound to commands, stored so that each key
//! typed narrows the bindings still reachable.
//!
//! A sequence may be bound and also be the prefix of a longer bound sequence,
//! as `g` and `g g` may both be bound; the trie records both, and the
//! [`Resolver`](crate::keys::Resolver) decides which one a typed sequence means.
//!
//! [`Bindings`] is what the resolver asks: a trie answers it on its own, and
//! a [`Keymap`](crate::keys::Keymap) answers it for a stack of active layers.

use std::collections::HashMap;

use thiserror::Error;

use super::key::Key;

// ========================================================================
// Data Structures
// ========================================================================

/// Why a sequence could not be bound.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum BindError {
    /// The sequence has no keys.
    #[error("cannot bind an empty key sequence")]
    EmptySequence,
}

/// Key sequences bound to commands of a host-chosen type `C`, such as a
/// command id string or an enum of the host's actions.
#[derive(Clone, Debug)]
pub struct KeyTrie<C> {
    /// The node of the empty sequence, which never holds a command.
    root: Node<C>,
}

/// One binding of a trie: a sequence and the command bound to it.
#[derive(Debug)]
pub(crate) struct BoundSequence<'a, C> {
    /// The command bound to the sequence.
    pub(crate) command: &'a C,
    /// The sequence.
    pub(crate) keys: Vec<Key>,
}

/// What a set of bindings holds for a sequence.
#[derive(Debug, Eq, PartialEq)]
pub enum Lookup<'a, C> {
    /// The sequence is bound, and is also the prefix of a longer binding.
    Ambiguous(&'a C),
    /// The sequence is bound, and no longer binding starts with it.
    Complete(&'a C),
    /// The sequence is not bound, but a longer binding starts with it.
    Prefix,
    /// Neither the sequence nor any longer sequence starting with it is bound.
    Unbound,
}

/// One sequence in the trie: the command bound to it, and the sequences one
/// key longer.
#[derive(Clone, Debug)]
struct Node<C> {
    /// The sequences one key longer, by the key that extends them.
    children: HashMap<Key, Node<C>>,
    /// The command bound to this sequence, if any.
    command: Option<C>,
}

// ========================================================================
// Traits
// ========================================================================

/// Key sequences bound to commands of type `C`, as the
/// [`Resolver`](crate::keys::Resolver) sees them.
pub trait Bindings<C> {
    /// What is bound to `keys`, and whether longer bindings start with them.
    fn lookup(&self, keys: &[Key]) -> Lookup<'_, C>;
}

// ========================================================================
// KeyTrie Implementations
// ========================================================================

impl<C> KeyTrie<C> {
    /// An empty trie, with no bindings.
    pub fn new() -> Self {
        Self { root: Node::new() }
    }

    /// Binds `keys` to `command`, returning the command it was bound to
    /// before, if any, so the caller can tell a rebinding from a new binding.
    pub fn bind(&mut self, keys: &[Key], command: C) -> Result<Option<C>, BindError> {
        if keys.is_empty() {
            return Err(BindError::EmptySequence);
        }
        let node = keys.iter().fold(&mut self.root, |node, key| {
            node.children.entry(*key).or_insert_with(Node::new)
        });
        Ok(node.command.replace(command))
    }

    /// Every binding of the trie, in no set order.
    pub(crate) fn bound_sequences(&self) -> Vec<BoundSequence<'_, C>> {
        let mut found = Vec::new();
        self.root.collect_bound(&mut Vec::new(), &mut found);
        found
    }

    /// The keys that extend `keys` toward a longer binding, in no set order.
    pub(crate) fn next_keys(&self, keys: &[Key]) -> Vec<Key> {
        self.node(keys)
            .map(|node| node.children.keys().copied().collect())
            .unwrap_or_default()
    }

    /// The node of `keys`, if any binding is or starts with them.
    fn node(&self, keys: &[Key]) -> Option<&Node<C>> {
        keys.iter()
            .try_fold(&self.root, |node, key| node.children.get(key))
    }
}

impl<C> Bindings<C> for KeyTrie<C> {
    fn lookup(&self, keys: &[Key]) -> Lookup<'_, C> {
        let Some(node) = self.node(keys) else {
            return Lookup::Unbound;
        };
        match (&node.command, node.children.is_empty()) {
            (Some(command), true) => Lookup::Complete(command),
            (Some(command), false) => Lookup::Ambiguous(command),
            (None, false) => Lookup::Prefix,
            (None, true) => Lookup::Unbound,
        }
    }
}

impl<C> Default for KeyTrie<C> {
    fn default() -> Self {
        Self::new()
    }
}

// ========================================================================
// Node Implementations
// ========================================================================

impl<C> Node<C> {
    /// A node with no command and no longer sequences.
    fn new() -> Self {
        Self {
            children: HashMap::new(),
            command: None,
        }
    }

    /// Adds the binding of this node, reached by `path`, and of every node
    /// below it to `found`.
    fn collect_bound<'a>(&'a self, path: &mut Vec<Key>, found: &mut Vec<BoundSequence<'a, C>>) {
        if let Some(command) = &self.command {
            found.push(BoundSequence {
                command,
                keys: path.clone(),
            });
        }
        for (key, child) in &self.children {
            path.push(*key);
            child.collect_bound(path, found);
            path.pop();
        }
    }
}

// ========================================================================
// Tests
// ========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::parse_sequence;

    fn keys(spec: &str) -> Vec<Key> {
        parse_sequence(spec).unwrap_or_else(|err| panic!("{spec:?}: {err}"))
    }

    #[test]
    fn test_lookup_tells_complete_ambiguous_prefix_and_unbound_apart() {
        let mut trie = KeyTrie::new();
        trie.bind(&keys("g"), "goto").unwrap();
        trie.bind(&keys("g g"), "top").unwrap();
        trie.bind(&keys("C-x C-f"), "open").unwrap();

        assert_eq!(trie.lookup(&keys("g")), Lookup::Ambiguous(&"goto"));
        assert_eq!(trie.lookup(&keys("g g")), Lookup::Complete(&"top"));
        assert_eq!(trie.lookup(&keys("C-x")), Lookup::Prefix);
        assert_eq!(trie.lookup(&keys("C-x C-f")), Lookup::Complete(&"open"));
        assert_eq!(trie.lookup(&keys("C-x f")), Lookup::Unbound);
        assert_eq!(trie.lookup(&keys("g g g")), Lookup::Unbound);
    }

    #[test]
    fn test_rebinding_a_sequence_returns_the_command_it_replaced() {
        let mut trie = KeyTrie::new();
        assert_eq!(trie.bind(&keys("C-x C-s"), "save"), Ok(None));
        assert_eq!(trie.bind(&keys("C-x C-s"), "save-all"), Ok(Some("save")));
        assert_eq!(trie.lookup(&keys("C-x C-s")), Lookup::Complete(&"save-all"));
    }

    #[test]
    fn test_binding_a_prefix_of_a_binding_replaces_nothing() {
        let mut trie = KeyTrie::new();
        trie.bind(&keys("C-x C-s"), "save").unwrap();
        assert_eq!(trie.bind(&keys("C-x"), "prefix"), Ok(None));
    }

    #[test]
    fn test_an_empty_sequence_cannot_be_bound() {
        let mut trie = KeyTrie::new();
        assert_eq!(trie.bind(&[], "nothing"), Err(BindError::EmptySequence));
        assert_eq!(trie.lookup(&[]), Lookup::Unbound);
    }
}
