//! Errors: why a highlighter could not be made.

use thiserror::Error;
use tree_sitter::{LanguageError, QueryError};

// ========================================================================
// Data Structures
// ========================================================================

/// Why a [`SyntaxHighlighter`](crate::syntax::SyntaxHighlighter) could not be
/// made: a grammar or query bundled with a build that does not fit it.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum SyntaxError {
    /// The grammar was generated for another tree-sitter version.
    #[error(transparent)]
    Grammar(#[from] LanguageError),
    /// The highlights query does not compile against the grammar.
    #[error(transparent)]
    Query(#[from] QueryError),
}
