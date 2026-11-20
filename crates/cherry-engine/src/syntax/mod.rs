//! Syntax for the Cherry editor: tree-sitter highlighting and structural
//! text objects, with grammars bundled behind features, and TextMate
//! highlighting of every other language through iris.
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
//! - With the `syntax-textmate` feature, [`TextMateGrammars`] makes a
//!   [`TextMateHighlighter`] for a file in any of the some ninety languages
//!   iris's TextMate grammars cover, its spans named with the same
//!   [`HIGHLIGHT_NAMES`].
//!
//! None of this is on in this crate by default; the `cherry` crate turns
//! on `syntax-textmate` by default. The `syntax` feature turns on the module
//! with all five bundled grammars; `syntax-json`, `syntax-markdown`,
//! `syntax-python`, `syntax-rust`, and `syntax-toml` turn on one grammar
//! each, and `syntax-engine` the module with no grammar, for a host that
//! brings its own; `syntax-textmate` the TextMate highlighter alone.
//!
//! ```
//! # #[cfg(feature = "syntax-rust")] {
//! use cherry_engine::{Editor, EditorOptions};
//! use cherry_engine::syntax::{Language, SyntaxHighlighter};
//!
//! let mut editor = Editor::new(EditorOptions::default());
//! let document = editor.open_document("fn main() {}\n");
//! let language = Language::from_extension("rs").expect("rust is bundled");
//! let text = editor.document(document).expect("just opened");
//! let highlighter = SyntaxHighlighter::new(language, text).expect("the grammar loads");
//! editor.set_highlighter(document, Some(Box::new(highlighter)));
//! cherry_engine::syntax::register_text_objects(&mut editor).expect("ids are free");
//! # }
//! ```

#[cfg(feature = "syntax-engine")]
mod error;
#[cfg(feature = "syntax-engine")]
mod highlighter;
#[cfg(feature = "syntax-engine")]
mod language;
mod names;
#[cfg(feature = "syntax-textmate")]
mod refine;
#[cfg(feature = "syntax-textmate")]
mod style;
#[cfg(feature = "syntax-engine")]
mod text_object;
#[cfg(feature = "syntax-textmate")]
mod textmate;

#[cfg(feature = "syntax-engine")]
pub use error::SyntaxError;
#[cfg(feature = "syntax-engine")]
pub use highlighter::SyntaxHighlighter;
#[cfg(feature = "syntax-engine")]
pub use language::Language;
pub use names::{HIGHLIGHT_NAMES, highlight_name};
#[cfg(feature = "syntax-textmate")]
pub use style::ScopeStyle;
#[cfg(feature = "syntax-engine")]
pub use text_object::register_text_objects;
#[cfg(feature = "syntax-textmate")]
pub use textmate::{LineHighlighter, LineStart, TextMateGrammars, TextMateHighlighter};
