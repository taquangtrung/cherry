//! How a TextMate highlighter turns the scopes text sits in into a
//! [`HighlightId`]: Cherry's [`HIGHLIGHT_NAMES`](crate::syntax::HIGHLIGHT_NAMES)
//! by default, or a host's own, such as the rules of a VS Code theme matched
//! with `iris::ScopeRules`.

use iris::HIGHLIGHT_NAMES;

use crate::HighlightId;

use super::names::highlight_id;

// ========================================================================
// Constants
// ========================================================================

/// iris's highlight names Cherry's names call otherwise; every other name
/// maps to Cherry's by its longest listed prefix of whole parts.
const RENAMED: &[(&str, &str)] = &[
    ("markup.heading", "text.title"),
    ("markup.link", "text.uri"),
    ("markup.raw", "text.literal"),
    ("tag", "label"),
];

// ========================================================================
// Traits
// ========================================================================

/// Styles text by the scopes it sits in, for a
/// [`TextMateHighlighter`](crate::syntax::TextMateHighlighter): a host
/// gives its own with
/// [`TextMateGrammars::with_style`](crate::syntax::TextMateGrammars::with_style)
/// to draw text by its theme's rules rather than by Cherry's names. The
/// host then says which highlight each id stands for.
pub trait ScopeStyle: Send + Sync {
    /// The highlight of text sitting in `scopes`, outermost first; `None`
    /// leaves it in the text's color.
    fn highlight(&self, scopes: &[&str]) -> Option<HighlightId>;

    /// Whether text sitting in `scopes` has a color of its own, which a
    /// pass naming words the grammar left plain asks: by default, whether
    /// it has a highlight.
    fn colors(&self, scopes: &[&str]) -> bool {
        self.highlight(scopes).is_some()
    }
}

// ========================================================================
// Data Structures
// ========================================================================

/// Cherry's own style: scopes resolved to iris's highlight names, each
/// named as Cherry's [`HIGHLIGHT_NAMES`](crate::syntax::HIGHLIGHT_NAMES)
/// call it.
pub(crate) struct NamedStyle {
    /// The Cherry id of each of iris's highlight names, by index.
    ids: Vec<Option<HighlightId>>,
    /// Resolves scopes to iris's highlight names.
    names: iris::Highlighter,
}

// ========================================================================
// NamedStyle Implementations
// ========================================================================

impl NamedStyle {
    /// Cherry's style.
    pub(crate) fn new() -> Self {
        let ids = HIGHLIGHT_NAMES
            .iter()
            .map(|&name| {
                let renamed = RENAMED.iter().find(|(from, _)| *from == name);
                highlight_id(renamed.map_or(name, |(_, to)| to))
            })
            .collect();
        Self {
            ids,
            names: iris::Highlighter::new(HIGHLIGHT_NAMES),
        }
    }
}

impl ScopeStyle for NamedStyle {
    fn highlight(&self, scopes: &[&str]) -> Option<HighlightId> {
        self.names
            .highlight_stack(scopes)
            .and_then(|index| self.ids.get(index).copied().flatten())
    }
}
