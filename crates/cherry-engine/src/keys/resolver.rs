//! The resolver: turns keys typed one at a time into the commands a set of
//! [`Bindings`] binds them to, either one [`KeyTrie`](crate::keys::KeyTrie) or the
//! active layers of a [`Keymap`](crate::keys::Keymap).
//!
//! Each key fed either completes a binding ([`Resolution::Matched`]), extends
//! a prefix that may still complete one ([`Resolution::Pending`]), or leads
//! nowhere ([`Resolution::Unmatched`]). Matched and unmatched keys are
//! consumed; the resolver starts the next sequence from nothing.
//!
//! A sequence that is bound and is also the prefix of a longer binding, as
//! `g` is when both `g` and `g g` are bound, is ambiguous. The resolver waits
//! on it as pending: the next key decides. A key that extends it toward the
//! longer binding continues the sequence; any other key first runs the
//! shorter binding and is then resolved on its own, so `g x` runs `g` and
//! then resolves `x`. If no key comes within the timeout, the shorter binding
//! runs.
//!
//! A [`PrefixTimeout`] policy says which pending sequences time out. By
//! default only ambiguous ones do: an incomplete prefix that runs nothing on
//! its own, as Emacs's `C-x`, waits for its next key however long it takes.
//! [`PrefixTimeout::All`] times every pending sequence out, as Vim does.
//!
//! The resolver never reads a clock or a configuration. The host passes the
//! timeout when it creates the resolver and the current time with every call,
//! and calls [`Resolver::tick`] from its event loop so that a pending
//! sequence expires even when no further key arrives. The timeout counts from
//! the last key fed, not the first; [`Resolver::deadline`] says when it
//! elapses (never, for a sequence the policy keeps waiting), so a host can schedule a timer instead of polling, and
//! [`Resolver::reset`] drops a pending sequence the host no longer wants.

use std::mem;
use std::time::Duration;

use super::key::Key;
use super::timestamp::Timestamp;
use super::trie::{Bindings, Lookup};

// ========================================================================
// Data Structures
// ========================================================================

/// A completed binding: the command and the keys that ran it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Match<C> {
    /// The command the keys are bound to.
    pub command: C,
    /// The keys typed, in order.
    pub keys: Vec<Key>,
}

/// Which pending sequences time out.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum PrefixTimeout {
    /// Every pending sequence times out, an incomplete prefix unmatched
    /// and an ambiguous one running its shorter binding, as in Vim.
    All,
    /// Only an ambiguous sequence (bound, and the prefix of a longer
    /// binding) times out; an incomplete prefix that is not bound itself
    /// waits for its next key indefinitely, as in Emacs.
    #[default]
    AmbiguousOnly,
}

/// What a key, or the timeout, did to the sequence being typed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Resolution<C> {
    /// The keys completed a binding.
    Matched(Match<C>),
    /// The keys so far may still complete a binding; [`Resolver::pending`]
    /// lists them.
    Pending,
    /// The keys, in order, complete no binding and start none; they are
    /// dropped. A host may treat a single unmatched key as text to insert.
    Unmatched(Vec<Key>),
}

/// Resolves keys typed one at a time against a set of [`Bindings`], tracking
/// the prefix typed so far. One resolver serves one input stream; the
/// bindings are passed with each call, so the host may switch tables or
/// layers between keys.
#[derive(Clone, Debug)]
pub struct Resolver {
    /// When the last pending key was fed; set exactly when keys are pending
    /// and the policy lets them time out.
    last_key_at: Option<Timestamp>,
    /// The keys typed so far that may still complete a binding.
    pending: Vec<Key>,
    /// Which pending sequences time out.
    prefix_timeout: PrefixTimeout,
    /// How long a pending sequence waits for its next key.
    timeout: Duration,
}

// ========================================================================
// Resolver Implementations
// ========================================================================

impl Resolver {
    /// A resolver with nothing pending, whose ambiguous pending sequences
    /// wait `timeout` for their next key (see [`PrefixTimeout::AmbiguousOnly`]).
    pub fn new(timeout: Duration) -> Self {
        Self {
            last_key_at: None,
            pending: Vec::new(),
            prefix_timeout: PrefixTimeout::default(),
            timeout,
        }
    }

    /// This resolver with `policy` saying which pending sequences time out.
    pub fn with_prefix_timeout(self, policy: PrefixTimeout) -> Self {
        Self {
            prefix_timeout: policy,
            ..self
        }
    }

    /// When the pending sequence expires: one timeout after the last key fed,
    /// or `None` when nothing is pending or what is pending never times out. A host that would rather not poll
    /// schedules a timer for this moment and calls [`Resolver::tick`] when it
    /// fires.
    pub fn deadline(&self) -> Option<Timestamp> {
        self.last_key_at
            .map(|last_key_at| last_key_at + self.timeout)
    }

    /// Feeds `key`, typed at `now`, and returns every resolution it caused,
    /// in order. Usually that is one; it is two when the key first settles
    /// what was pending, either because the timeout had already elapsed
    /// without a [`Resolver::tick`] or because the key ends an ambiguous
    /// sequence without extending it.
    pub fn feed<C: Clone>(
        &mut self,
        bindings: &impl Bindings<C>,
        key: Key,
        now: Timestamp,
    ) -> Vec<Resolution<C>> {
        let mut resolutions: Vec<_> = self.tick(bindings, now).into_iter().collect();
        resolutions.extend(self.settle_ambiguous(bindings, key));
        resolutions.push(self.extend(bindings, key, now));
        resolutions
    }

    /// The keys typed so far that may still complete a binding, for the host
    /// to show; empty when nothing is pending.
    pub fn pending(&self) -> &[Key] {
        &self.pending
    }

    /// Drops the pending keys without running anything, not even the shorter
    /// binding of an ambiguous sequence. The host calls this when the keys
    /// typed so far no longer apply: on a mode switch or when focus is lost.
    pub fn reset(&mut self) {
        self.take();
    }

    /// Settles a pending sequence whose timeout has elapsed at `now`: an
    /// ambiguous one runs its shorter binding, an incomplete one is
    /// unmatched. Returns `None` when nothing is pending, the timeout has
    /// not yet elapsed, or the policy keeps what is pending waiting. The host calls this from its event loop.
    pub fn tick<C: Clone>(
        &mut self,
        bindings: &impl Bindings<C>,
        now: Timestamp,
    ) -> Option<Resolution<C>> {
        let last_key_at = self.last_key_at?;
        if now.saturating_duration_since(last_key_at) < self.timeout {
            return None;
        }
        let keys = self.take();
        let resolution = match bindings.lookup(&keys) {
            Lookup::Ambiguous(command) | Lookup::Complete(command) => Resolution::Matched(Match {
                command: command.clone(),
                keys,
            }),
            Lookup::Prefix | Lookup::Unbound => Resolution::Unmatched(keys),
        };
        Some(resolution)
    }

    /// Adds `key`, typed at `now`, to the pending keys and resolves them.
    fn extend<C: Clone>(
        &mut self,
        bindings: &impl Bindings<C>,
        key: Key,
        now: Timestamp,
    ) -> Resolution<C> {
        self.pending.push(key);
        match bindings.lookup(&self.pending) {
            Lookup::Complete(command) => {
                let command = command.clone();
                Resolution::Matched(Match {
                    command,
                    keys: self.take(),
                })
            }
            Lookup::Ambiguous(_) => {
                self.last_key_at = Some(now);
                Resolution::Pending
            }
            Lookup::Prefix => {
                self.last_key_at = match self.prefix_timeout {
                    PrefixTimeout::All => Some(now),
                    PrefixTimeout::AmbiguousOnly => None,
                };
                Resolution::Pending
            }
            Lookup::Unbound => Resolution::Unmatched(self.take()),
        }
    }

    /// Runs the shorter binding of an ambiguous pending sequence when `key`
    /// does not extend it toward a longer one, leaving nothing pending.
    fn settle_ambiguous<C: Clone>(
        &mut self,
        bindings: &impl Bindings<C>,
        key: Key,
    ) -> Option<Resolution<C>> {
        let Lookup::Ambiguous(command) = bindings.lookup(&self.pending) else {
            return None;
        };
        self.pending.push(key);
        let extends = !matches!(bindings.lookup(&self.pending), Lookup::Unbound);
        self.pending.pop();
        if extends {
            return None;
        }
        Some(Resolution::Matched(Match {
            command: command.clone(),
            keys: self.take(),
        }))
    }

    /// Takes the pending keys, leaving nothing pending.
    fn take(&mut self) -> Vec<Key> {
        self.last_key_at = None;
        mem::take(&mut self.pending)
    }
}

// ========================================================================
// Tests
// ========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::{KeyTrie, parse_key, parse_sequence};

    const TIMEOUT: Duration = Duration::from_millis(1000);

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

    fn unmatched(spec: &str) -> Resolution<&'static str> {
        Resolution::Unmatched(keys(spec))
    }

    fn timing_out_all() -> Resolver {
        Resolver::new(TIMEOUT).with_prefix_timeout(PrefixTimeout::All)
    }

    fn trie() -> KeyTrie<&'static str> {
        let mut trie = KeyTrie::new();
        for (spec, command) in [
            ("x", "delete-char"),
            ("C-x C-f", "open"),
            ("C-x r t", "string-rectangle"),
            ("g", "goto"),
            ("g g", "top"),
        ] {
            trie.bind(&keys(spec), command).unwrap();
        }
        trie
    }

    #[test]
    fn test_a_single_key_binding_matches_at_once() {
        let (trie, mut resolver, now) = (trie(), Resolver::new(TIMEOUT), Timestamp::default());
        assert_eq!(
            resolver.feed(&trie, key("x"), now),
            vec![matched("delete-char", "x")]
        );
        assert!(resolver.pending().is_empty());
    }

    #[test]
    fn test_a_multi_key_binding_is_pending_until_its_last_key() {
        let (trie, mut resolver, now) = (trie(), Resolver::new(TIMEOUT), Timestamp::default());
        assert_eq!(
            resolver.feed(&trie, key("C-x"), now),
            vec![Resolution::Pending]
        );
        assert_eq!(resolver.pending(), keys("C-x"));
        assert_eq!(
            resolver.feed(&trie, key("C-f"), now),
            vec![matched("open", "C-x C-f")]
        );
        assert!(resolver.pending().is_empty());
    }

    #[test]
    fn test_an_unrelated_key_resets_a_pending_prefix() {
        let (trie, mut resolver, now) = (trie(), Resolver::new(TIMEOUT), Timestamp::default());
        resolver.feed(&trie, key("C-x"), now);
        assert_eq!(
            resolver.feed(&trie, key("z"), now),
            vec![unmatched("C-x z")]
        );
        assert!(resolver.pending().is_empty());
        assert_eq!(
            resolver.feed(&trie, key("x"), now),
            vec![matched("delete-char", "x")]
        );
    }

    #[test]
    fn test_an_unbound_key_is_unmatched() {
        let (trie, mut resolver, now) = (trie(), Resolver::new(TIMEOUT), Timestamp::default());
        assert_eq!(resolver.feed(&trie, key("q"), now), vec![unmatched("q")]);
    }

    #[test]
    fn test_an_ambiguous_prefix_waits_then_resolves_to_the_shorter_binding() {
        let (trie, mut resolver, start) = (trie(), Resolver::new(TIMEOUT), Timestamp::default());
        assert_eq!(
            resolver.feed(&trie, key("g"), start),
            vec![Resolution::Pending]
        );
        assert_eq!(resolver.tick(&trie, start + TIMEOUT / 2), None);
        assert_eq!(resolver.pending(), keys("g"));
        assert_eq!(
            resolver.tick(&trie, start + TIMEOUT),
            Some(matched("goto", "g"))
        );
        assert!(resolver.pending().is_empty());
        assert_eq!(resolver.tick(&trie, start + TIMEOUT * 2), None);
    }

    #[test]
    fn test_an_ambiguous_prefix_continues_to_the_longer_binding() {
        let (trie, mut resolver, now) = (trie(), Resolver::new(TIMEOUT), Timestamp::default());
        resolver.feed(&trie, key("g"), now);
        assert_eq!(
            resolver.feed(&trie, key("g"), now),
            vec![matched("top", "g g")]
        );
    }

    #[test]
    fn test_a_key_that_ends_an_ambiguous_prefix_runs_the_shorter_binding_first() {
        let (trie, mut resolver, now) = (trie(), Resolver::new(TIMEOUT), Timestamp::default());
        resolver.feed(&trie, key("g"), now);
        assert_eq!(
            resolver.feed(&trie, key("x"), now),
            vec![matched("goto", "g"), matched("delete-char", "x")]
        );
        resolver.feed(&trie, key("g"), now);
        assert_eq!(
            resolver.feed(&trie, key("C-x"), now),
            vec![matched("goto", "g"), Resolution::Pending]
        );
        assert_eq!(resolver.pending(), keys("C-x"));
    }

    #[test]
    fn test_an_incomplete_prefix_expires_unmatched_when_all_prefixes_time_out() {
        let (trie, mut resolver, start) = (trie(), timing_out_all(), Timestamp::default());
        resolver.feed(&trie, key("C-x"), start);
        assert_eq!(
            resolver.tick(&trie, start + TIMEOUT),
            Some(unmatched("C-x"))
        );
        assert!(resolver.pending().is_empty());
    }

    #[test]
    fn test_the_timeout_counts_from_the_last_key() {
        let (trie, mut resolver, start) = (trie(), timing_out_all(), Timestamp::default());
        resolver.feed(&trie, key("C-x"), start);
        resolver.feed(&trie, key("r"), start + TIMEOUT / 2);
        assert_eq!(resolver.tick(&trie, start + TIMEOUT), None);
        assert_eq!(
            resolver.feed(&trie, key("t"), start + TIMEOUT),
            vec![matched("string-rectangle", "C-x r t")]
        );
    }

    #[test]
    fn test_a_key_after_an_unticked_timeout_settles_the_pending_prefix_first() {
        let (trie, mut resolver, start) = (trie(), timing_out_all(), Timestamp::default());
        resolver.feed(&trie, key("C-x"), start);
        assert_eq!(
            resolver.feed(&trie, key("C-f"), start + TIMEOUT),
            vec![unmatched("C-x"), unmatched("C-f")]
        );
        resolver.feed(&trie, key("g"), start + TIMEOUT);
        assert_eq!(
            resolver.feed(&trie, key("g"), start + TIMEOUT * 2),
            vec![matched("goto", "g"), Resolution::Pending]
        );
    }

    #[test]
    fn test_reset_drops_the_pending_keys_so_the_next_key_starts_fresh() {
        let (trie, mut resolver, now) = (trie(), Resolver::new(TIMEOUT), Timestamp::default());
        resolver.feed(&trie, key("C-x"), now);
        resolver.reset();
        assert!(resolver.pending().is_empty());
        assert_eq!(resolver.deadline(), None);
        assert_eq!(resolver.tick(&trie, now + TIMEOUT), None);
        assert_eq!(
            resolver.feed(&trie, key("C-f"), now),
            vec![unmatched("C-f")]
        );
    }

    #[test]
    fn test_reset_of_an_ambiguous_prefix_runs_nothing() {
        let (trie, mut resolver, now) = (trie(), Resolver::new(TIMEOUT), Timestamp::default());
        resolver.feed(&trie, key("g"), now);
        resolver.reset();
        assert_eq!(
            resolver.feed(&trie, key("x"), now),
            vec![matched("delete-char", "x")]
        );
    }

    #[test]
    fn test_the_deadline_is_one_timeout_after_the_last_pending_key() {
        let (trie, mut resolver, start) = (trie(), timing_out_all(), Timestamp::default());
        assert_eq!(resolver.deadline(), None);
        resolver.feed(&trie, key("C-x"), start);
        assert_eq!(resolver.deadline(), Some(start + TIMEOUT));
        let later = start + TIMEOUT / 2;
        resolver.feed(&trie, key("r"), later);
        let deadline = resolver.deadline().expect("C-x r is pending");
        assert_eq!(deadline, later + TIMEOUT);
        let just_before = later + (TIMEOUT - Duration::from_millis(1));
        assert_eq!(resolver.tick(&trie, just_before), None);
        assert_eq!(resolver.tick(&trie, deadline), Some(unmatched("C-x r")));
        assert_eq!(resolver.deadline(), None);
    }

    #[test]
    fn test_a_matched_key_leaves_no_deadline() {
        let (trie, mut resolver, now) = (trie(), Resolver::new(TIMEOUT), Timestamp::default());
        resolver.feed(&trie, key("C-x"), now);
        resolver.feed(&trie, key("C-f"), now);
        assert_eq!(resolver.deadline(), None);
    }

    #[test]
    fn test_an_incomplete_prefix_waits_indefinitely_by_default() {
        let (trie, mut resolver, start) = (trie(), Resolver::new(TIMEOUT), Timestamp::default());
        resolver.feed(&trie, key("C-x"), start);
        assert_eq!(resolver.deadline(), None);
        let much_later = start + TIMEOUT * 100;
        assert_eq!(resolver.tick(&trie, much_later), None);
        assert_eq!(resolver.pending(), keys("C-x"));
        assert_eq!(
            resolver.feed(&trie, key("C-f"), much_later),
            vec![matched("open", "C-x C-f")]
        );
    }

    #[test]
    fn test_an_ambiguous_prefix_still_times_out_by_default() {
        let (trie, mut resolver, start) = (trie(), Resolver::new(TIMEOUT), Timestamp::default());
        resolver.feed(&trie, key("g"), start);
        assert_eq!(resolver.deadline(), Some(start + TIMEOUT));
        assert_eq!(
            resolver.tick(&trie, start + TIMEOUT),
            Some(matched("goto", "g"))
        );
    }

    #[test]
    fn test_a_waiting_prefix_that_becomes_ambiguous_gains_a_deadline() {
        let mut trie = trie();
        trie.bind(&keys("C-x r"), "registers").unwrap();
        let (mut resolver, start) = (Resolver::new(TIMEOUT), Timestamp::default());
        resolver.feed(&trie, key("C-x"), start);
        assert_eq!(resolver.deadline(), None);
        resolver.feed(&trie, key("r"), start + TIMEOUT);
        assert_eq!(resolver.deadline(), Some(start + TIMEOUT * 2));
        assert_eq!(
            resolver.tick(&trie, start + TIMEOUT * 2),
            Some(matched("registers", "C-x r"))
        );
    }
}
