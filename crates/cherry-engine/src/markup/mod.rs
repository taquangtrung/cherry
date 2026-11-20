//! Optional modules for the Cherry editor, for markup and plain-text
//! formats, built on Cherry's public API alone:
//!
//! - [`markdown_conceals`] finds the Markdown markers to hide (`**`, `_`,
//!   backticks, a heading's `#`s, a link's brackets and URL), for
//!   [`Editor::set_conceals`](crate::Editor::set_conceals).
//! - [`heading_numbers`] numbers a Markdown document's headings (`1.`,
//!   `1.2.`), for a host to show where their hidden `#`s were.
//! - [`outline`] gives the outline of a document in one of the [`Format`]s
//!   an outline is written for: Markdown headings, INI and TOML sections,
//!   YAML and JSON keys, LaTeX sections, and KDL nodes.
//! - [`register_markdown_commands`] adds commands for writing Markdown:
//!   heading steps, promoting and demoting, task boxes, emphasis, and
//!   renumbering a list.
//!
//! ```
//! use cherry_engine::{Editor, EditorOptions};
//! use cherry_engine::markup::{markdown_conceals, outline, Format};
//!
//! let text = "# Notes\nSome **bold** text.\n";
//! let mut editor = Editor::new(EditorOptions::default());
//! let document = editor.open_document(text);
//! editor.set_conceals(document, "markdown", markdown_conceals(text));
//! let headings = outline(Format::Markdown, text);
//! assert_eq!(headings[0].title, "Notes");
//! cherry_engine::markup::register_markdown_commands(&mut editor).expect("ids are free");
//! ```

mod conceal;
mod markdown;
mod numbering;
mod outline;

pub use conceal::markdown_conceals;
pub use markdown::register_markdown_commands;
pub use numbering::{HeadingNumber, heading_numbers};
pub use outline::{Format, OutlineEntry, outline};
