//! Cherry, the modal text editor, on the misty GUI framework. The editor
//! itself is the toolkit-free [`cherry-engine`](engine) crate, and misty
//! knows nothing of it; this crate is where they meet, for an app built on
//! both. Most apps reach it through the `cherry` crate, whose default
//! `misty` feature re-exports it as `cherry::misty`. Misty owns its key
//! values and resolves no key presses; this crate resolves them with
//! Cherry's keymap, and edits text with Cherry's modal editor:
//!
//! - [`keys`] converts misty's keys to Cherry's and back, without loss.
//! - [`clock`] gives Cherry timestamps on one clock with misty's
//!   `Instant`s.
//! - [`keymap`] binds a misty `CommandRegistry` into a
//!   `cherry_engine::keys::Keymap` and turns a pending prefix's
//!   continuations into which-key rows.
//! - [`EditorView`] shows one view of a Cherry
//!   [`Editor`](cherry_engine::Editor) as a misty widget, painted through
//!   the cell grid in the theme's colors.
//!
//! The engine is re-exported as [`engine`], and misty as [`misty`], so an
//! app built on both needs no other dependency for them, and its misty is
//! the one this crate was built against; an app on another toolkit depends
//! on `cherry-engine` alone. The engine's features are this crate's too:
//! `serde` for saved undo histories and keymap files, and `syntax` (or one
//! grammar of it, such as `syntax-rust`) for the engine's `syntax` module,
//! whose highlights are drawn in the theme's syntax colors.

pub mod clock;
mod editor_view;
pub mod keymap;
pub mod keys;

pub use cherry_engine as engine;
pub use editor_view::{
    ChangeNotes, EditorClipboard, EditorResponse, EditorView, EditorWait, HighlightStyle,
    LentEditor,
};
pub use misty;
