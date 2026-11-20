//! Highlight names: the fixed list of what highlighted text can be, which
//! a host themes, and how a grammar's capture names map onto it.

use std::sync::OnceLock;

use crate::HighlightId;

// ========================================================================
// Constants
// ========================================================================

/// What highlighted text can be, from the general to the specific; a
/// span's [`HighlightId`] is the index of its name here, so a host themes
/// by name with [`highlight_name`]. A host lacking a color for a dotted
/// name (`function.method`) may fall back to its first part (`function`).
pub const HIGHLIGHT_NAMES: &[&str] = &[
    "attribute",
    "boolean",
    "comment",
    "comment.documentation",
    "constant",
    "constant.builtin",
    "constructor",
    "embedded",
    "escape",
    "function",
    "function.builtin",
    "function.macro",
    "function.method",
    "keyword",
    "label",
    "number",
    "operator",
    "property",
    "punctuation",
    "punctuation.bracket",
    "punctuation.delimiter",
    "punctuation.special",
    "string",
    "string.escape",
    "string.special",
    "string.special.key",
    "text.literal",
    "text.reference",
    "text.title",
    "text.uri",
    "type",
    "type.builtin",
    "variable",
    "variable.builtin",
    "variable.parameter",
];

// ========================================================================
// Functions
// ========================================================================

/// The name `id` stands for, or `None` for an id no highlighter of this
/// crate makes.
pub fn highlight_name(id: HighlightId) -> Option<&'static str> {
    HIGHLIGHT_NAMES.get(usize::from(id.index())).copied()
}

/// The id of the capture name `capture`: its own name when listed, else
/// its longest listed prefix of whole parts (`function.method.call` is
/// `function.method`); `None` when no prefix is listed.
#[cfg(any(test, feature = "syntax-textmate"))]
pub(crate) fn highlight_id(capture: &str) -> Option<HighlightId> {
    let index = resolver().resolve(capture)?;
    u16::try_from(index).ok().map(HighlightId::new)
}

/// The [`HIGHLIGHT_NAMES`] as iris resolves a capture's name to them, the
/// index of a name its [`HighlightId`].
pub(crate) fn resolver() -> &'static iris::Highlighter {
    static RESOLVER: OnceLock<iris::Highlighter> = OnceLock::new();
    RESOLVER.get_or_init(|| iris::Highlighter::new(HIGHLIGHT_NAMES))
}

// ========================================================================
// Tests
// ========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_a_capture_takes_its_longest_listed_prefix_of_whole_parts() {
        let name = |capture| highlight_id(capture).and_then(highlight_name);
        assert_eq!(name("function.method"), Some("function.method"));
        assert_eq!(name("function.method.call"), Some("function.method"));
        assert_eq!(name("keyword.control.flow"), Some("keyword"));
        assert_eq!(name("functional"), None, "a prefix of whole parts only");
        assert_eq!(name("none"), None);
    }
}
