//! Syntax for the Cherry editor: tree-sitter highlighting and structural
//! text objects, with grammars bundled behind features.
//!
//! - [`SyntaxHighlighter`] parses one document in one [`Language`] and is
//!   a [`crate::Highlighter`]: the editor tells it of every change, which
//!   it reparses incrementally, and paints the spans it finds. Each span's
//!   [`HighlightId`](crate::HighlightId) is the index of one of the
//!   [`HIGHLIGHT_NAMES`], which a host themes ([`highlight_name`]).
//! - [`Language::from_extension`] picks the language of a file.
//! - [`register_text_objects`] adds `af`/`if` (functions) and `ac`/`ic`
//!   (classes and types) to an editor, answered by each document's
//!   highlighter.
//!
//! None of this is on by default. The `syntax` feature turns on the module
//! with all five bundled grammars; `syntax-json`, `syntax-markdown`,
//! `syntax-python`, `syntax-rust`, and `syntax-toml` turn on one grammar
//! each, and `syntax-engine` the module with no grammar, for a host that
//! brings its own.
//!
//! ```
//! use cherry::{Editor, EditorOptions};
//! use cherry::syntax::{Language, SyntaxHighlighter};
//!
//! # #[cfg(feature = "syntax-rust")] {
//! let mut editor = Editor::new(EditorOptions::default());
//! let document = editor.open_document("fn main() {}\n");
//! let language = Language::from_extension("rs").expect("rust is bundled");
//! let text = editor.document(document).expect("just opened");
//! let highlighter = SyntaxHighlighter::new(language, text).expect("the grammar loads");
//! editor.set_highlighter(document, Some(Box::new(highlighter)));
//! cherry::syntax::register_text_objects(&mut editor).expect("ids are free");
//! # }
//! ```

mod error;
mod highlighter;
mod language;
mod names;
mod text_object;

pub use error::SyntaxError;
pub use highlighter::SyntaxHighlighter;
pub use language::Language;
pub use names::{HIGHLIGHT_NAMES, highlight_name};
pub use text_object::register_text_objects;
