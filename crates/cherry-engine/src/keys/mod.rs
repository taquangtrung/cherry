//! Keys for keyboard-driven apps, independent of any window toolkit: the
//! host converts its own key events into a key value, and everything here
//! works on that. Chords, multi-key sequences, and key tables that drive
//! dispatch, help, and rebinding from one place.
//!
//! - [`Key`], [`KeyCode`], and [`Mods`] model one chord, normalized so each
//!   chord has one value; [`parse_key`] and [`parse_sequence`] read the key
//!   notation (`C-x C-f`, `g g`, `S-<tab>`), and `Display` writes it back.
//! - [`KeyTrie`] binds key sequences to commands of the host's choosing, and
//!   [`Resolver`] turns keys typed one at a time into those commands, with a
//!   pending prefix and a timeout the host supplies; a [`PrefixTimeout`]
//!   policy says whether only ambiguous prefixes time out or all do.
//! - [`Timestamp`] is the time the host passes with each key, read from
//!   its own monotonic clock: nothing here reads a clock, so timeouts work
//!   in a browser too.
//! - [`Keymap`] holds one trie per layer of the host's choosing (a mode, a
//!   context) and resolves against the layers active at each key, highest
//!   first, with user bindings over defaults and duplicates reported.
//! - [`KeyTable`] writes each binding once (id, title, group, default keys)
//!   and from it fills a keymap layer, lists help rows, and applies the
//!   user's overrides.
//! - [`ActiveLayers`] answers help: the [`Continuation`]s of a pending prefix
//!   (which-key), the command a sequence runs (describe-key), and the
//!   sequences that run a command (where-is).

mod key;
mod keymap;
mod notation;
mod resolver;
mod table;
mod timestamp;
mod trie;

pub use key::{Key, KeyCode, Mods};
pub use keymap::{ActiveLayers, Continuation, Duplicate, Keymap};
pub use notation::{ParseError, parse_key, parse_sequence};
pub use resolver::{Match, PrefixTimeout, Resolution, Resolver};
pub use table::{Binding, HelpRow, InvalidSpec, KeyTable, TableError};
pub use timestamp::Timestamp;
pub use trie::{BindError, Bindings, KeyTrie, Lookup};
