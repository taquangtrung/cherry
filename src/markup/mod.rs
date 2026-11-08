//! Optional modules for the Cherry editor, for markup and plain-text
//! formats, built on Cherry's public API alone:
//!
//! - [`markdown_conceals`] finds the Markdown markers to hide (`**`, `_`,
//!   backticks, a heading's `#`s, a link's brackets and URL), for
//!   [`Editor::set_conceals`](crate::Editor::set_conceals).
//! - [`outline`] gives the outline of a document in one of the [`Format`]s
//!   an outline is written for: Markdown headings, INI and TOML sections,
//!   YAML and JSON keys.
//! - [`register_markdown_commands`] adds commands for writing Markdown:
//!   heading steps, promoting and demoting, task boxes, emphasis, and
//!   renumbering a list.
//!
//! ```
//! use cherry::{Editor, EditorOptions};
//! use cherry::markup::{markdown_conceals, outline, Format};
//!
//! let text = "# Notes\nSome **bold** text.\n";
//! let mut editor = Editor::new(EditorOptions::default());
//! let document = editor.open_document(text);
//! editor.set_conceals(document, "markdown", markdown_conceals(text));
//! let headings = outline(Format::Markdown, text);
//! assert_eq!(headings[0].title, "Notes");
//! cherry::markup::register_markdown_commands(&mut editor).expect("ids are free");
//! ```

mod conceal;
mod markdown;
mod outline;

pub use conceal::markdown_conceals;
pub use markdown::register_markdown_commands;
pub use outline::{Format, OutlineEntry, outline};
