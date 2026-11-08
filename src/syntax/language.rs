//! Languages: the grammars bundled behind features, how a file's extension
//! picks one, and what each knows of its structure for text objects.

use tree_sitter::Language as Grammar;

// ========================================================================
// Data Structures
// ========================================================================

/// A language with a bundled grammar. Each variant exists only when its
/// feature (`json`, `markdown`, `python`, `rust`, `toml`) is on, as all are
/// by default.
///
/// New variants may come in a minor release, so a host's `match` needs a
/// fallback arm.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[non_exhaustive]
pub enum Language {
    /// JSON.
    #[cfg(feature = "syntax-json")]
    Json,
    /// Markdown, its block structure (headings, lists, code blocks).
    #[cfg(feature = "syntax-markdown")]
    Markdown,
    /// Python.
    #[cfg(feature = "syntax-python")]
    Python,
    /// Rust.
    #[cfg(feature = "syntax-rust")]
    Rust,
    /// TOML.
    #[cfg(feature = "syntax-toml")]
    Toml,
}

/// The node kinds a language's structural text objects stand for.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Structure {
    /// The kinds of classes and types: `ac` and `ic`.
    pub classes: &'static [&'static str],
    /// The kinds of functions: `af` and `if`.
    pub functions: &'static [&'static str],
}

// ========================================================================
// Language Implementations
// ========================================================================

impl Language {
    /// Every language bundled in this build.
    pub const ALL: &'static [Self] = &[
        #[cfg(feature = "syntax-json")]
        Self::Json,
        #[cfg(feature = "syntax-markdown")]
        Self::Markdown,
        #[cfg(feature = "syntax-python")]
        Self::Python,
        #[cfg(feature = "syntax-rust")]
        Self::Rust,
        #[cfg(feature = "syntax-toml")]
        Self::Toml,
    ];

    /// The language of files with `extension` (`"rs"`, `"md"`, without
    /// the dot, any case); `None` for one no bundled language claims.
    pub fn from_extension(extension: &str) -> Option<Self> {
        let extension = extension.to_ascii_lowercase();
        Self::ALL
            .iter()
            .copied()
            .find(|language| language.extensions().contains(&extension.as_str()))
    }

    /// The file extensions the language claims, without the dot.
    pub fn extensions(self) -> &'static [&'static str] {
        match self {
            #[cfg(feature = "syntax-json")]
            Self::Json => &["json", "jsonc"],
            #[cfg(feature = "syntax-markdown")]
            Self::Markdown => &["markdown", "md"],
            #[cfg(feature = "syntax-python")]
            Self::Python => &["py", "pyi"],
            #[cfg(feature = "syntax-rust")]
            Self::Rust => &["rs"],
            #[cfg(feature = "syntax-toml")]
            Self::Toml => &["toml"],
        }
    }

    /// The language's name, lowercase: `"rust"`.
    pub fn name(self) -> &'static str {
        match self {
            #[cfg(feature = "syntax-json")]
            Self::Json => "json",
            #[cfg(feature = "syntax-markdown")]
            Self::Markdown => "markdown",
            #[cfg(feature = "syntax-python")]
            Self::Python => "python",
            #[cfg(feature = "syntax-rust")]
            Self::Rust => "rust",
            #[cfg(feature = "syntax-toml")]
            Self::Toml => "toml",
        }
    }

    /// The tree-sitter grammar.
    pub(crate) fn grammar(self) -> Grammar {
        match self {
            #[cfg(feature = "syntax-json")]
            Self::Json => tree_sitter_json::LANGUAGE.into(),
            #[cfg(feature = "syntax-markdown")]
            Self::Markdown => tree_sitter_md::LANGUAGE.into(),
            #[cfg(feature = "syntax-python")]
            Self::Python => tree_sitter_python::LANGUAGE.into(),
            #[cfg(feature = "syntax-rust")]
            Self::Rust => tree_sitter_rust::LANGUAGE.into(),
            #[cfg(feature = "syntax-toml")]
            Self::Toml => tree_sitter_toml_ng::LANGUAGE.into(),
        }
    }

    /// The highlights query, in the grammar's own capture names.
    pub(crate) fn highlights_query(self) -> &'static str {
        match self {
            #[cfg(feature = "syntax-json")]
            Self::Json => tree_sitter_json::HIGHLIGHTS_QUERY,
            #[cfg(feature = "syntax-markdown")]
            Self::Markdown => tree_sitter_md::HIGHLIGHT_QUERY_BLOCK,
            #[cfg(feature = "syntax-python")]
            Self::Python => tree_sitter_python::HIGHLIGHTS_QUERY,
            #[cfg(feature = "syntax-rust")]
            Self::Rust => tree_sitter_rust::HIGHLIGHTS_QUERY,
            #[cfg(feature = "syntax-toml")]
            Self::Toml => tree_sitter_toml_ng::HIGHLIGHTS_QUERY,
        }
    }

    /// The node kinds its text objects stand for; none for a data format.
    pub(crate) fn structure(self) -> Structure {
        match self {
            #[cfg(feature = "syntax-json")]
            Self::Json => Structure::NONE,
            #[cfg(feature = "syntax-markdown")]
            Self::Markdown => Structure::NONE,
            #[cfg(feature = "syntax-python")]
            Self::Python => Structure {
                classes: &["class_definition"],
                functions: &["function_definition", "lambda"],
            },
            #[cfg(feature = "syntax-rust")]
            Self::Rust => Structure {
                classes: &[
                    "enum_item",
                    "impl_item",
                    "struct_item",
                    "trait_item",
                    "union_item",
                ],
                functions: &["closure_expression", "function_item"],
            },
            #[cfg(feature = "syntax-toml")]
            Self::Toml => Structure::NONE,
        }
    }
}

// ========================================================================
// Structure Implementations
// ========================================================================

impl Structure {
    /// No text objects.
    #[cfg(any(
        feature = "syntax-json",
        feature = "syntax-markdown",
        feature = "syntax-toml"
    ))]
    const NONE: Self = Self {
        classes: &[],
        functions: &[],
    };
}

// ========================================================================
// Tests
// ========================================================================

// The tests open documents in every bundled grammar.
#[cfg(all(test, feature = "syntax"))]
mod tests {
    use super::*;

    #[test]
    fn test_extensions_pick_their_language_in_any_case_and_no_two_claim_one() {
        assert_eq!(Language::from_extension("RS"), Some(Language::Rust));
        assert_eq!(Language::from_extension("md"), Some(Language::Markdown));
        assert_eq!(Language::from_extension("txt"), None);
        let mut claimed: Vec<_> = Language::ALL
            .iter()
            .flat_map(|language| language.extensions())
            .collect();
        let total = claimed.len();
        claimed.sort();
        claimed.dedup();
        assert_eq!(claimed.len(), total);
    }
}
