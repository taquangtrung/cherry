//! Outlines, written for each format on its own: what a document of the
//! format holds, entry by entry, each with the line it starts on and how
//! deep it sits, for a picker or a side panel to show and jump through. A
//! format gets an outline only when one is written for it here; nothing is
//! guessed for others from their brackets or indentation.
//!
//! - Markdown: its headings, `#` to `######` and the underlined (`===`,
//!   `---`) ones, outside fenced code.
//! - INI and TOML: the section headers, `[name]` and `[[name]]`, a dotted
//!   TOML name one level deeper per dot.
//! - YAML: the mapping keys, as deep as they are indented, list items and
//!   comments left out.
//! - JSON: the object keys whose values are objects or arrays, and every
//!   key at the top, as deep as they are nested.

// ========================================================================
// Constants
// ========================================================================

/// The fences that open and close a Markdown code block.
const FENCES: [&str; 2] = ["```", "~~~"];

/// The deepest Markdown heading.
const MAX_HEADING: usize = 6;

// ========================================================================
// Data Structures
// ========================================================================

/// A format an outline is written for.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[non_exhaustive]
pub enum Format {
    /// INI files (`.ini`, `.cfg`, `.conf`).
    Ini,
    /// JSON.
    Json,
    /// Markdown.
    Markdown,
    /// TOML.
    Toml,
    /// YAML.
    Yaml,
}

/// One entry of an outline.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct OutlineEntry {
    /// How deep it sits, from 1 at the top.
    pub level: usize,
    /// The line it starts on, from 0.
    pub line: usize,
    /// What it is called: a heading's text, a section's or key's name.
    pub title: String,
}

// ========================================================================
// Format Implementations
// ========================================================================

impl Format {
    /// The format of files with extension `extension` (`"md"`, without the
    /// dot), ignoring case; `None` for a format with no outline here.
    pub fn from_extension(extension: &str) -> Option<Self> {
        let format = match extension.to_ascii_lowercase().as_str() {
            "cfg" | "conf" | "ini" => Self::Ini,
            "json" => Self::Json,
            "markdown" | "md" => Self::Markdown,
            "toml" => Self::Toml,
            "yaml" | "yml" => Self::Yaml,
            _ => return None,
        };
        Some(format)
    }
}

// ========================================================================
// Functions
// ========================================================================

/// The outline of `text`, a document in `format`, in document order.
pub fn outline(format: Format, text: &str) -> Vec<OutlineEntry> {
    match format {
        Format::Ini | Format::Toml => sections(text),
        Format::Json => json_keys(text),
        Format::Markdown => headings(text),
        Format::Yaml => yaml_keys(text),
    }
}

/// The headings of Markdown `text`.
fn headings(text: &str) -> Vec<OutlineEntry> {
    let lines: Vec<&str> = text.split('\n').collect();
    let mut entries = Vec::new();
    let mut in_fence = false;
    for (line, content) in lines.iter().enumerate() {
        let trimmed = content.trim_start();
        if FENCES.iter().any(|fence| trimmed.starts_with(fence)) {
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            continue;
        }
        let hashes = content.chars().take_while(|c| *c == '#').count();
        if (1..=MAX_HEADING).contains(&hashes) && content[hashes..].starts_with(' ') {
            let title = content[hashes..].trim().trim_end_matches('#').trim_end();
            entries.push(entry(hashes, line, title));
            continue;
        }
        let underline = lines.get(line + 1).map(|next| next.trim());
        let level = match underline {
            Some(next) if !next.is_empty() && next.chars().all(|c| c == '=') => 1,
            Some(next) if next.len() >= 2 && next.chars().all(|c| c == '-') => 2,
            Some(_) | None => continue,
        };
        if !content.trim().is_empty() {
            entries.push(entry(level, line, content.trim()));
        }
    }
    entries
}

/// The section headers of INI or TOML `text`.
fn sections(text: &str) -> Vec<OutlineEntry> {
    text.split('\n')
        .enumerate()
        .filter_map(|(line, content)| {
            let trimmed = content.trim();
            let name = trimmed
                .strip_prefix("[[")
                .and_then(|rest| rest.strip_suffix("]]"))
                .or_else(|| trimmed.strip_prefix('[')?.strip_suffix(']'))?
                .trim();
            let level = name.matches('.').count() + 1;
            Some(entry(level, line, name))
        })
        .collect()
}

/// The mapping keys of YAML `text`, as deep as they are indented.
fn yaml_keys(text: &str) -> Vec<OutlineEntry> {
    let mut indents: Vec<usize> = Vec::new();
    let mut entries = Vec::new();
    for (line, content) in text.split('\n').enumerate() {
        let trimmed = content.trim_start();
        if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with('-') {
            continue;
        }
        let Some((key, _)) = trimmed.split_once(':') else {
            continue;
        };
        if key.is_empty() || key.contains(' ') && !key.starts_with(['"', '\'']) {
            continue;
        }
        let indent = content.len() - trimmed.len();
        while indents.last().is_some_and(|last| *last >= indent) {
            indents.pop();
        }
        indents.push(indent);
        entries.push(entry(indents.len(), line, key.trim_matches(['"', '\''])));
    }
    entries
}

/// The keys of JSON `text` whose values are objects or arrays, and every
/// key at the top, as deep as they are nested.
fn json_keys(text: &str) -> Vec<OutlineEntry> {
    let chars: Vec<char> = text.chars().collect();
    let (mut entries, mut depth, mut line, mut at) = (Vec::new(), 0_usize, 0, 0);
    while at < chars.len() {
        match chars[at] {
            '\n' => line += 1,
            '{' | '[' => depth += 1,
            '}' | ']' => depth = depth.saturating_sub(1),
            '"' => {
                let start = at + 1;
                at = start;
                while at < chars.len() && chars[at] != '"' {
                    at += if chars[at] == '\\' { 2 } else { 1 };
                }
                let key: String = chars[start..at.min(chars.len())].iter().collect();
                let rest = chars[(at + 1).min(chars.len())..]
                    .iter()
                    .skip_while(|c| c.is_whitespace());
                let mut rest = rest.copied();
                if rest.next() == Some(':') {
                    let value = rest.find(|c| !c.is_whitespace());
                    let container = matches!(value, Some('{' | '['));
                    if container || depth == 1 {
                        entries.push(entry(depth, line, &key));
                    }
                }
            }
            _ => {}
        }
        at += 1;
    }
    entries
}

/// An entry at `level` on `line` called `title`.
fn entry(level: usize, line: usize, title: &str) -> OutlineEntry {
    OutlineEntry {
        level,
        line,
        title: title.to_string(),
    }
}

// ========================================================================
// Tests
// ========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// The outline of `text` in `format`, as `(level, line, title)`.
    fn entries(format: Format, text: &str) -> Vec<(usize, usize, String)> {
        outline(format, text)
            .into_iter()
            .map(|entry| (entry.level, entry.line, entry.title))
            .collect()
    }

    #[test]
    fn test_markdown_headings_both_kinds_outside_code() {
        let text = "# One #\ntext\n```\n# not\n```\nTwo\n---\n### Three";
        let found = entries(Format::Markdown, text);
        let expected = [(1, 0, "One"), (2, 5, "Two"), (3, 7, "Three")];
        assert_eq!(found, expected.map(|(l, n, t)| (l, n, t.to_string())));
    }

    #[test]
    fn test_sections_yaml_and_json_keys_by_depth() {
        let toml = entries(
            Format::Toml,
            "a = 1\n[server]\nx = 2\n[server.tls]\n[[bin]]",
        );
        assert_eq!(
            toml.iter()
                .map(|(level, _, title)| (*level, title.as_str()))
                .collect::<Vec<_>>(),
            [(1, "server"), (2, "server.tls"), (1, "bin")]
        );
        let yaml = entries(
            Format::Yaml,
            "name: x\nspec:\n  # c\n  ports:\n    - 80\n  host: h",
        );
        assert_eq!(
            yaml.iter()
                .map(|(level, _, title)| (*level, title.as_str()))
                .collect::<Vec<_>>(),
            [(1, "name"), (1, "spec"), (2, "ports"), (2, "host")]
        );
        let json = entries(
            Format::Json,
            "{\n  \"a\": 1,\n  \"b\": {\n    \"c\": 2,\n    \"d\": [3]\n  }\n}",
        );
        assert_eq!(
            json.iter()
                .map(|(level, line, title)| (*level, *line, title.as_str()))
                .collect::<Vec<_>>(),
            [(1, 1, "a"), (1, 2, "b"), (2, 4, "d")]
        );
    }

    #[test]
    fn test_formats_are_known_by_extension() {
        assert_eq!(Format::from_extension("MD"), Some(Format::Markdown));
        assert_eq!(
            Format::from_extension("rs"),
            None,
            "no outline written for it"
        );
    }
}
