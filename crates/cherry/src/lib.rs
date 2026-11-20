//! Cherry Editor, an embeddable text editor: the Vim grammar where you
//! navigate and Emacs editing where you type, or strict Vim, modeless
//! Emacs, or a desktop editor's keys. This crate gathers Cherry's parts
//! under one name:
//!
//! - The editor itself, toolkit-free, at the top: [`Editor`],
//!   [`EditorOptions`], [`keys`], [`markup`], and the rest, from the
//!   `cherry-engine` crate. A host feeds it keys and paints the rows it
//!   produces, in whatever toolkit it is built with.
//! - [`misty`], with the `misty` feature (on by default): the editor as a
//!   widget of the misty GUI framework, with misty's keys resolved by
//!   Cherry's keymap, from the `cherry-misty` crate.
//!
//! An app on another toolkit, or in a terminal, turns default features
//! off and builds no toolkit at all. The `serde` feature serializes saved
//! undo histories and keymap files. The [`syntax`] module, whose
//! highlights the misty widget draws in the theme's syntax colors, comes
//! with `syntax-textmate` (on by default: TextMate highlighting through
//! iris) and with `syntax` (or one grammar of it, such as `syntax-rust`:
//! tree-sitter highlighting and text objects).
//!
//! ```
//! use cherry::{Editor, EditorOptions};
//!
//! let mut editor = Editor::new(EditorOptions::default());
//! let document = editor.open_document("hello\n");
//! let view = editor.new_view(document).expect("the document is open");
//! // On misty, the app draws the view as a widget.
//! #[cfg(feature = "misty")]
//! let widget = cherry::misty::EditorView::new(view);
//! ```

pub use cherry_engine::*;
#[cfg(feature = "misty")]
pub use cherry_misty as misty;
