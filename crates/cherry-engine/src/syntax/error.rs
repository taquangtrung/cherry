//! Errors: why a highlighter could not be made.

use thiserror::Error;

// ========================================================================
// Data Structures
// ========================================================================

/// Why a [`SyntaxHighlighter`](crate::syntax::SyntaxHighlighter) could not be
/// made: a grammar or query bundled with a build that does not fit it.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum SyntaxError {
    /// The grammar was generated for another tree-sitter version, or one
    /// of its queries does not compile against it.
    #[error(transparent)]
    Grammar(#[from] iris::Error),
}
