//! Outlines, written for each format on its own: what a document of the
//! format holds, entry by entry, each with the line it starts on and how
//! deep it sits, for a picker or a side panel to show and jump through. A
//! format gets an outline only when one is written for it here; nothing is
//! guessed for others from their brackets or indentation, whose meaning a
//! format does not promise.
//!
//! - Markdown: its headings, `#` to `######` and the underlined (`===`,
//!   `---`) ones, outside fenced code and the front matter at its top.
//! - LaTeX: the sectioning commands, `\part` to `\subparagraph`, starred or
//!   not, outside comments and verbatim environments, the shallowest one the
//!   document uses at the top.
//! - INI: the section headers as written (`[Unit]`, `[remote "origin"]`).
//! - TOML: the section headers, `[name]` and `[[name]]`, a dotted name one
//!   level deeper per dot.
//! - JSON, YAML, KDL: what holds more than a setting: keys holding objects,
//!   arrays, or blocks, and an array's objects by their place and name.
//! - HTML, XML, reStructuredText, nginx, SSH's configuration, and Swift:
//!   each as its own module says.

mod html;
mod ini;
mod json;
mod kdl;
mod nginx;
mod rst;
mod ssh;
mod swift;
mod xml;
mod yaml;

// ========================================================================
// Constants
// ========================================================================

/// The fences that open and close a Markdown code block.
const FENCES: [&str; 2] = ["```", "~~~"];

/// The deepest Markdown heading.
const MAX_HEADING: usize = 6;

/// The fewest `-` a front matter fence has.
const FRONT_MATTER_DASHES: usize = 3;

/// LaTeX's sectioning commands, the shallowest first.
const LATEX_SECTIONS: [&str; 7] = [
    "part",
    "chapter",
    "section",
    "subsection",
    "subsubsection",
    "paragraph",
    "subparagraph",
];

/// The LaTeX environments whose text is shown as written, a sectioning
/// command in them being no section.
const LATEX_VERBATIM: [&str; 5] = ["comment", "lstlisting", "minted", "verbatim", "Verbatim"];

// ========================================================================
// Data Structures
// ========================================================================

/// A format an outline is written for.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[non_exhaustive]
pub enum Format {
    /// HTML.
    Html,
    /// INI files (`.ini`, `.cfg`, `.conf`).
    Ini,
    /// JSON.
    Json,
    /// KDL.
    Kdl,
    /// LaTeX.
    Latex,
    /// Markdown.
    Markdown,
    /// nginx's configuration.
    Nginx,
    /// reStructuredText.
    ReStructuredText,
    /// SSH's client or server configuration.
    SshConfig,
    /// Swift.
    Swift,
    /// TOML.
    Toml,
    /// XML.
    Xml,
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
            "htm" | "html" | "xhtml" => Self::Html,
            "json" => Self::Json,
            "kdl" => Self::Kdl,
            "latex" | "ltx" | "tex" => Self::Latex,
            "markdown" | "md" => Self::Markdown,
            "rst" => Self::ReStructuredText,
            "swift" => Self::Swift,
            "toml" => Self::Toml,
            "plist" | "svg" | "xml" | "xsd" | "xsl" => Self::Xml,
            "yaml" | "yml" => Self::Yaml,
            _ => return None,
        };
        Some(format)
    }

    /// The format of a file in the language named `language`, a language's
    /// id or its grammar's name (`"ssh-config"` or `"SSH Config"`, `"nginx"`),
    /// ignoring case and taking `-` and `_` for spaces; `None` for a language
    /// with no outline here. For a file whose name says nothing of its
    /// format, as `ssh_config`.
    pub fn from_language(language: &str) -> Option<Self> {
        let name = language.to_ascii_lowercase().replace(['-', '_'], " ");
        let format = match name.as_str() {
            "html" => Self::Html,
            "ini" => Self::Ini,
            "json" | "jsonc" | "json with comments" => Self::Json,
            "kdl" => Self::Kdl,
            "latex" | "tex" => Self::Latex,
            "markdown" => Self::Markdown,
            "nginx" => Self::Nginx,
            "restructuredtext" => Self::ReStructuredText,
            "ssh config" | "sshd config" => Self::SshConfig,
            "swift" => Self::Swift,
            "toml" => Self::Toml,
            "xml" => Self::Xml,
            "yaml" => Self::Yaml,
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
    let lines = || -> Vec<String> { text.split('\n').map(str::to_string).collect() };
    match format {
        Format::Html => html::outline(&lines()),
        Format::Ini => ini::outline(&lines()),
        Format::Json => json::outline(&lines()),
        Format::Kdl => kdl::outline(&lines()),
        Format::Latex => latex_sections(text),
        Format::Markdown => headings(text),
        Format::Nginx => nginx::outline(&lines()),
        Format::ReStructuredText => rst::outline(&lines()),
        Format::SshConfig => ssh::outline(&lines()),
        Format::Swift => swift::outline(&lines()),
        Format::Toml => sections(text),
        Format::Xml => xml::outline(&lines()),
        Format::Yaml => yaml::outline(&lines()),
    }
}

/// The headings of Markdown `text`.
fn headings(text: &str) -> Vec<OutlineEntry> {
    let lines: Vec<&str> = text.split('\n').collect();
    let mut entries = Vec::new();
    let mut in_fence = false;
    for (line, content) in lines.iter().enumerate().skip(front_matter_lines(&lines)) {
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
            entries.push(entry(line, hashes - 1, title));
            continue;
        }
        let underline = lines.get(line + 1).map(|next| next.trim());
        let level = match underline {
            Some(next) if !next.is_empty() && next.chars().all(|c| c == '=') => 1,
            Some(next) if next.len() >= 2 && next.chars().all(|c| c == '-') => 2,
            Some(_) | None => continue,
        };
        if !content.trim().is_empty() {
            entries.push(entry(line, level - 1, content.trim()));
        }
    }
    entries
}

/// How many of Markdown `lines`, from the first, are its front matter: a
/// first line of three or more `-`, through the next line of `-` (or
/// `...`); none when the first line opens none or nothing closes it.
pub(super) fn front_matter_lines(lines: &[&str]) -> usize {
    let opens = lines.first().is_some_and(|first| {
        let first = first.trim_end();
        first.len() >= FRONT_MATTER_DASHES && first.chars().all(|c| c == '-')
    });
    if !opens {
        return 0;
    }
    lines[1..]
        .iter()
        .position(|line| {
            let line = line.trim();
            line == "..." || (line.len() >= FRONT_MATTER_DASHES && line.chars().all(|c| c == '-'))
        })
        .map_or(0, |close| close + 2)
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
            Some(entry(line, level - 1, name))
        })
        .collect()
}

/// The sectioning commands of LaTeX `text`, levelled from the shallowest
/// the document uses.
fn latex_sections(text: &str) -> Vec<OutlineEntry> {
    let mut entries = Vec::new();
    let mut verbatim: Option<String> = None;
    for (line, content) in text.split('\n').enumerate() {
        let content = latex_uncommented(content);
        let trimmed = content.trim_start();
        if let Some(name) = &verbatim {
            if trimmed.starts_with(&format!("\\end{{{name}}}")) {
                verbatim = None;
            }
            continue;
        }
        if let Some(name) = latex_environment(trimmed, "\\begin{")
            && LATEX_VERBATIM.contains(&name)
        {
            verbatim = Some(name.to_string());
            continue;
        }
        let Some(command) = trimmed.strip_prefix('\\') else {
            continue;
        };
        let Some((depth, rest)) = LATEX_SECTIONS.iter().enumerate().find_map(|(depth, name)| {
            let rest = command.strip_prefix(name)?;
            rest.starts_with(['*', '[', '{', ' '])
                .then_some((depth, rest))
        }) else {
            continue;
        };
        let rest = rest.trim_start().trim_start_matches('*').trim_start();
        let rest = match rest.strip_prefix('[') {
            Some(short) => short
                .split_once(']')
                .map_or("", |(_, after)| after)
                .trim_start(),
            None => rest,
        };
        let Some(body) = rest.strip_prefix('{') else {
            continue;
        };
        // A title whose braces do not close on its line is shown as the
        // whole line, as written.
        let title = match braced(body) {
            Some(title) => title.split_whitespace().collect::<Vec<_>>().join(" "),
            None => trimmed.trim_end().to_string(),
        };
        entries.push(entry(line, depth, &title));
    }
    let top = entries.iter().map(|entry| entry.level).min().unwrap_or(0);
    for entry in &mut entries {
        entry.level = entry.level - top + 1;
    }
    entries
}

/// LaTeX `line` without its comment: what precedes the first `%` no
/// backslash escapes.
fn latex_uncommented(line: &str) -> &str {
    let mut escaped = false;
    for (at, c) in line.char_indices() {
        match c {
            '%' if !escaped => return &line[..at],
            '\\' => escaped = !escaped,
            _ => escaped = false,
        }
    }
    line
}

/// The environment `line` opens or closes, `prefix` being `\begin{` or
/// `\end{`.
fn latex_environment<'a>(line: &'a str, prefix: &str) -> Option<&'a str> {
    let rest = line.strip_prefix(prefix)?;
    rest.split_once('}').map(|(name, _)| name)
}

/// The text of a group whose `{` came just before `body`, up to its
/// matching `}`; `None` where it does not close on the line.
fn braced(body: &str) -> Option<&str> {
    let mut depth = 0_usize;
    let mut escaped = false;
    for (at, c) in body.char_indices() {
        match c {
            '\\' => {
                escaped = !escaped;
                continue;
            }
            '{' if !escaped => depth += 1,
            '}' if !escaped && depth == 0 => return Some(&body[..at]),
            '}' if !escaped => depth -= 1,
            _ => {}
        }
        escaped = false;
    }
    None
}

/// An entry on line `line`, `depth` below the top (from 0), called `title`.
pub(super) fn entry(line: usize, depth: usize, title: &str) -> OutlineEntry {
    OutlineEntry {
        level: depth + 1,
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

    /// `text` split into its lines, as the formats' outlines take them.
    pub(super) fn lines(text: &str) -> Vec<String> {
        text.lines().map(str::to_string).collect()
    }

    /// Each of `entries` as a list shows it: its line number from 1, then
    /// its title indented by its depth.
    pub(super) fn labels(entries: &[OutlineEntry]) -> Vec<String> {
        entries
            .iter()
            .map(|entry| {
                let indent = "  ".repeat(entry.level - 1);
                format!("{:>5}  {indent}{}", entry.line + 1, entry.title)
            })
            .collect()
    }

    #[test]
    fn test_toml_sections_nest_by_their_dots() {
        let toml = outline(
            Format::Toml,
            "a = 1\n[server]\nx = 2\n[server.tls]\n[[bin]]",
        );
        assert_eq!(
            labels(&toml),
            ["    2  server", "    4    server.tls", "    5  bin"]
        );
    }

    #[test]
    fn test_a_latex_title_not_closed_on_its_line_is_the_whole_line() {
        let found = outline(
            Format::Latex,
            "\\section{Intro}\n\\subsection*{The {nested} part}\n\\section{End",
        );
        assert_eq!(
            labels(&found),
            [
                "    1  Intro",
                "    2    The {nested} part",
                "    3  \\section{End"
            ]
        );
    }

    /// The outline of `text` in `format`, as `(level, line, title)`.
    fn entries(format: Format, text: &str) -> Vec<(usize, usize, String)> {
        outline(format, text)
            .into_iter()
            .map(|entry| (entry.level, entry.line, entry.title))
            .collect()
    }

    #[test]
    fn test_front_matter_is_not_read_for_headings() {
        let text = "---\ntitle: Notes\n---\n# One";
        let entries = outline(Format::Markdown, text);
        let lines: Vec<usize> = entries.iter().map(|entry| entry.line).collect();
        assert_eq!(lines, [3], "the closing fence underlines no heading");
        let unclosed = outline(Format::Markdown, "---\nTitle\n---");
        assert_eq!(unclosed.len(), 0, "closed after all: front matter");
        assert_eq!(
            outline(Format::Markdown, "Intro\n---").len(),
            1,
            "a heading"
        );
    }

    #[test]
    fn test_markdown_headings_both_kinds_outside_code() {
        let text = "# One #\ntext\n```\n# not\n```\nTwo\n---\n### Three";
        let found = entries(Format::Markdown, text);
        let expected = [(1, 0, "One"), (2, 5, "Two"), (3, 7, "Three")];
        assert_eq!(found, expected.map(|(l, n, t)| (l, n, t.to_string())));
    }

    #[test]
    fn test_latex_sections_outside_comments_and_verbatim_from_the_shallowest_used() {
        let text = "\\documentclass{article}\n\\section{Intro}\n% \\section{Gone}\n\\begin{verbatim}\n\\section{Code}\n\\end{verbatim}\n\\subsection*[Short]{Long {\\em title}} text\n\\section{100\\% sure} % note";
        let found = entries(Format::Latex, text);
        let expected = [
            (1, 1, "Intro"),
            (2, 6, "Long {\\em title}"),
            (1, 7, "100\\% sure"),
        ];
        assert_eq!(found, expected.map(|(l, n, t)| (l, n, t.to_string())));
    }

    #[test]
    fn test_formats_are_known_by_extension() {
        assert_eq!(Format::from_extension("MD"), Some(Format::Markdown));
        assert_eq!(Format::from_extension("tex"), Some(Format::Latex));
        assert_eq!(Format::from_extension("kdl"), Some(Format::Kdl));
        assert_eq!(Format::from_language("SSH Config"), Some(Format::SshConfig));
        assert_eq!(
            Format::from_language("sshd-config"),
            Some(Format::SshConfig)
        );
        assert_eq!(
            Format::from_language("rust"),
            None,
            "code has no outline here"
        );
        assert_eq!(
            Format::from_extension("rs"),
            None,
            "no outline written for it"
        );
    }
}
