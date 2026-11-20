//! TextMate highlighting through iris, behind the `syntax-textmate`
//! feature: the grammars VS Code ships (some eighty languages, embedded
//! ones included), tokenized as VS Code does, for any file the bundled
//! tree-sitter grammars do not cover. A TextMate grammar colors a line
//! from the state the line before left, so the highlighter tokenizes from
//! the top, a budget of lines at a time in [`Highlighter::prepare`], and
//! after an edit again from the first line it changed, until a line ends
//! in the state it ended in before: the lines after it are kept as they
//! were.
//!
//! The highlighter knows its language's configuration too, so it answers
//! what Enter does ([`Highlighter::enter`]), which typed lines close a
//! block ([`Highlighter::outdents`]), and how the document folds
//! ([`Highlighter::fold_ranges`]); [`TextMateGrammars::configure`] also
//! gives a document its language's comment tokens and auto-paired
//! brackets and quotes.

use std::collections::HashMap;
use std::fmt;
use std::ops::{Range, RangeInclusive};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError, RwLock};

use iris::{
    Bracket, Brackets, Folding, Grammar, Indent, Language, LanguageConfig, Languages, LineState,
    Registry, Token, Tokenizer, fold_by_brackets, fold_by_indentation,
};

use crate::{
    ChangeSet, Document, DocumentId, Editor, EnterIndent, HighlightId, HighlightSpan, Highlighter,
    IndentChange, Position, Preparation,
};

use crate::markup::OutlineEntry;

use super::refine::{Passes, Piece, names_words, refine};
use super::style::{NamedStyle, ScopeStyle};

// ========================================================================
// Constants
// ========================================================================

/// iris's id for Markdown, whose lines go through its passes.
const MARKDOWN: &str = "markdown";

/// The longest line, in bytes, read through its grammar: a grammar takes
/// most of a second over a few hundred kilobytes of one line, and would
/// take it on every paint. A longer line is left plain, and the next begins
/// where it did. VS Code stops at the same length.
const LONGEST_LINE: usize = 20_000;

/// The scopes a grammar names a definition with, rather than a use: what an
/// outline of code lists. A call is `variable.function`, a definition
/// `entity.name.function`, in VS Code's grammars and Sublime's alike.
const DEFINITIONS: [&str; 15] = [
    "entity.name.class",
    "entity.name.constant",
    "entity.name.enum",
    "entity.name.function",
    "entity.name.impl",
    "entity.name.interface",
    "entity.name.macro",
    "entity.name.module",
    "entity.name.namespace",
    "entity.name.section",
    "entity.name.struct",
    "entity.name.table",
    "entity.name.trait",
    "entity.name.type",
    "entity.name.union",
];

/// The scopes a call sits in, whose name some grammars (VS Code's Rust,
/// Python, JavaScript) scope as a definition's: a definition scope inside
/// one names a use.
const CALLS: [&str; 4] = [
    "meta.function-call",
    "meta.function.call",
    "meta.method-call",
    "meta.method.call",
];

/// What follows a definition's scope where a grammar names a use with it
/// instead (Go's `entity.name.function.support` for a call).
const SUPPORT: &str = ".support";

/// The columns a tab counts for, when a line's indent says how deep it is.
const TAB_COLUMNS: usize = 4;

/// How many columns a tab takes when folding by indentation.
const FOLD_TAB_WIDTH: usize = 4;

// ========================================================================
// Data Structures
// ========================================================================

/// VS Code's TextMate grammars, each compiled the first time a document in
/// its language is opened. Make one and share it: a highlighter it makes
/// shares its grammar with every other in the same language.
pub struct TextMateGrammars {
    /// Each language's grammar, by grammar scope; `None` for a scope with
    /// no grammar.
    grammars: Mutex<HashMap<String, Option<Arc<Grammar>>>>,
    /// The todo keywords every highlighter it makes colors.
    keywords: Arc<Keywords>,
    /// The languages, to find a file's.
    languages: Languages,
    /// The grammars as written.
    registry: Registry,
    /// How every highlighter it makes styles text by its scopes.
    style: Arc<dyn ScopeStyle>,
}

/// The todo keywords a host colors, and how many times they changed, so a
/// line colored before the last change is colored again.
#[derive(Default)]
struct Keywords {
    changes: AtomicU64,
    words: RwLock<Vec<String>>,
}

/// The TextMate highlighting of one document, for
/// [`Editor::set_highlighter`](crate::Editor::set_highlighter). Its spans
/// name the [`HIGHLIGHT_NAMES`](crate::syntax::HIGHLIGHT_NAMES).
pub struct TextMateHighlighter {
    /// Finds the language's brackets in a line's code.
    brackets: Brackets,
    /// What the language's configuration says: its comments, brackets,
    /// Enter's indent, how it folds.
    config: LanguageConfig,
    /// How it colors a tokenized line.
    colorer: Colorer,
    /// The keywords' changes it colored its lines at.
    keywords_seen: u64,
    /// The lines tokenized so far, from the first.
    lines: Vec<TokenizedLine>,
    /// The lines after the last edit's lines as they were tokenized before
    /// it, to keep once a line ends in the state the first of them began
    /// in.
    kept: Kept,
    /// The text as last told, to find the lines the next change touches.
    text: Document,
    /// The tokenizer, with the document's grammar.
    tokenizer: Tokenizer,
}

/// How a highlighter colors a line it tokenized: the passes that name its
/// runs more closely, then the style.
#[derive(Clone)]
struct Colorer {
    /// The todo keywords colored, shared with the grammars it came from.
    keywords: Arc<Keywords>,
    /// Whether its lines are Markdown's.
    markdown: bool,
    /// How it styles text by its scopes.
    style: Arc<dyn ScopeStyle>,
    /// Whether its grammar's unscoped names are scoped (Rust's).
    words: bool,
}

/// Where a line begins for a [`LineHighlighter`]: what the lines above it
/// left open. Kept to go back to with [`LineHighlighter::resume`].
#[derive(Clone, Debug, PartialEq)]
pub struct LineStart(Option<LineState>);

/// Highlights lines one after another, with no document: for text shown
/// elsewhere than an editor, a diff's side or a preview, styled as a
/// [`TextMateHighlighter`] from the same grammars would style it.
pub struct LineHighlighter {
    /// How it colors a line.
    colorer: Colorer,
    /// Where the next line begins.
    start: LineStart,
    /// The tokenizer, with the text's grammar.
    tokenizer: Tokenizer,
}

/// One line's highlighting and the state it ends in.
struct TokenizedLine {
    /// Its brackets in code, for folding.
    brackets: Vec<Bracket>,
    /// Its highlighted spans, as char columns into the line, so the line
    /// keeps them when the text before it changes.
    spans: Vec<(Range<usize>, HighlightId)>,
    /// The state the line ends in, the next line's start.
    state: LineState,
}

/// The lines after an edit as they were, waiting to be kept.
#[derive(Default)]
struct Kept {
    /// The lines, in order.
    lines: Vec<TokenizedLine>,
    /// The state the first of them began in.
    start: Option<LineState>,
    /// The number the first of them has now.
    first: usize,
}

// ========================================================================
// TextMateGrammars Implementations
// ========================================================================

impl TextMateGrammars {
    /// VS Code's grammars, bundled.
    pub fn new() -> Self {
        Self {
            grammars: Mutex::new(HashMap::new()),
            keywords: Arc::default(),
            languages: Languages::builtin(),
            registry: Registry::bundled(),
            style: Arc::new(NamedStyle::new()),
        }
    }

    /// The same grammars, every highlighter they make styling text with
    /// `style` rather than Cherry's names.
    pub fn with_style(mut self, style: impl ScopeStyle + 'static) -> Self {
        self.style = Arc::new(style);
        self
    }

    /// Colors `keywords` (`TODO`, `FIXME`) in every highlighter these
    /// grammars made or make, each as a whole word in its case, in comments
    /// and in Markdown's prose; the same list again changes nothing.
    pub fn set_todo_keywords(&self, keywords: &[String]) {
        let mut words = self
            .keywords
            .words
            .write()
            .unwrap_or_else(PoisonError::into_inner);
        if words.as_slice() != keywords {
            *words = keywords.to_vec();
            self.keywords.changes.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// A highlighter of `document` in `language`'s `grammar`, styled and
    /// keyworded as these grammars are.
    fn highlighter_for(
        &self,
        language: &Language,
        grammar: Arc<Grammar>,
        document: &Document,
    ) -> TextMateHighlighter {
        let mut highlighter = TextMateHighlighter::new(grammar, language.config.clone(), document);
        highlighter.colorer = self.colorer(language);
        highlighter
    }

    /// How a highlighter of a text in `language` colors its lines.
    fn colorer(&self, language: &Language) -> Colorer {
        Colorer {
            keywords: Arc::clone(&self.keywords),
            markdown: language.id == MARKDOWN,
            style: Arc::clone(&self.style),
            words: language.scope.as_deref().is_some_and(names_words),
        }
    }

    /// A highlighter of lines with no document, of a text in the file at
    /// `path` whose first line is `first_line` (its name and extension, a
    /// modeline or `#!` line, tell its language); `None` for a text in no
    /// language with a grammar.
    pub fn line_highlighter(
        &self,
        path: impl AsRef<Path>,
        first_line: &str,
    ) -> Option<LineHighlighter> {
        let language = self.languages.detect(path, first_line)?;
        self.line_highlighter_of(language)
    }

    /// A highlighter of lines in the language `name` names: its id, an
    /// alias, or an extension (`"rust"`, `"py"`), as a Markdown fence names
    /// its code's; `None` for a name of no language with a grammar.
    pub fn line_highlighter_named(&self, name: &str) -> Option<LineHighlighter> {
        let language = self
            .languages
            .get(name)
            .or_else(|| self.languages.detect(format!("fence.{name}"), ""))?;
        self.line_highlighter_of(language)
    }

    /// The id of the language a file at `path`, its first line
    /// `first_line`, is in; `None` for none iris knows.
    pub fn language_id(&self, path: impl AsRef<Path>, first_line: &str) -> Option<&str> {
        self.languages
            .detect(path, first_line)
            .map(|language| language.id.as_str())
    }

    /// The definitions of `text`, the file at `path`, as its grammar names
    /// them (a function, a type, a module): each a line that names one, by
    /// its text trimmed, as deep as it is indented over the file's least
    /// indent. What an outline of code lists where nothing better knows its
    /// shape; `None` for a text in no language with a grammar.
    pub fn definitions(&self, path: impl AsRef<Path>, text: &str) -> Option<Vec<OutlineEntry>> {
        let rows: Vec<&str> = text.split('\n').collect();
        let mut lines = self.line_highlighter(path, rows.first().copied().unwrap_or_default())?;
        let indents: Vec<usize> = rows.iter().map(|row| indent_of(row)).collect();
        let step = indents
            .iter()
            .copied()
            .filter(|width| *width > 0)
            .min()
            .unwrap_or(1);
        let definitions = rows
            .iter()
            .enumerate()
            .filter(|(_, row)| lines.defines(row))
            .map(|(line, row)| OutlineEntry {
                level: indents[line] / step + 1,
                line,
                title: row.trim().to_string(),
            })
            .collect();
        Some(definitions)
    }

    /// A highlighter of lines in `language`, if it has a grammar.
    fn line_highlighter_of(&self, language: &Language) -> Option<LineHighlighter> {
        let grammar = self.grammar_of(language)?;
        Some(LineHighlighter {
            colorer: self.colorer(language),
            start: LineStart(None),
            tokenizer: tokenizer(grammar),
        })
    }

    /// A highlighter of `document`, the file at `path` (its name and
    /// extension, a modeline or `#!` line in the document, tell its
    /// language); `None` for a file in no language with a grammar.
    pub fn highlighter(
        &self,
        path: impl AsRef<Path>,
        document: &Document,
    ) -> Option<TextMateHighlighter> {
        let (language, grammar) = self.language(path, document)?;
        Some(self.highlighter_for(language, grammar, document))
    }

    /// Gives the document `id`, the file at `path`, its language's
    /// highlighter and what its configuration says: the comment tokens `gc`
    /// uses, and the brackets and quotes typing pairs. Returns whether the
    /// file is in a language with a grammar; the document is left as it
    /// was when it is not.
    pub fn configure(&self, editor: &mut Editor, id: DocumentId, path: impl AsRef<Path>) -> bool {
        let Some(document) = editor.document(id) else {
            return false;
        };
        let Some((language, grammar)) = self.language(path, document) else {
            return false;
        };
        let config = &language.config;
        let highlighter = self.highlighter_for(language, grammar, document);
        editor.set_comment_token(id, config.line_comment.as_deref());
        let block = config.block_comment.as_ref();
        editor.set_block_comment_tokens(
            id,
            block.map(|pair| (pair.open.as_str(), pair.close.as_str())),
        );
        let pairs = config
            .auto_closing_pairs
            .iter()
            .filter_map(|pair| Some((single_char(&pair.open)?, single_char(&pair.close)?)))
            .collect();
        editor.set_auto_pairs(id, Some(pairs));
        editor.set_highlighter(id, Some(Box::new(highlighter)));
        true
    }

    /// The language of `document`, the file at `path`, with its grammar,
    /// compiled the first time it is asked for.
    fn language(
        &self,
        path: impl AsRef<Path>,
        document: &Document,
    ) -> Option<(&Language, Arc<Grammar>)> {
        let head = document.line_text(0).unwrap_or_default();
        let language = self.languages.detect(path, &head)?;
        let grammar = self.grammar_of(language)?;
        Some((language, grammar))
    }

    /// `language`'s grammar, compiled the first time it is asked for.
    fn grammar_of(&self, language: &Language) -> Option<Arc<Grammar>> {
        let scope = language.scope.as_deref()?;
        let mut grammars = self.grammars.lock().unwrap_or_else(PoisonError::into_inner);
        grammars
            .entry(scope.to_owned())
            .or_insert_with(|| self.registry.grammar(scope).ok())
            .clone()
    }
}

impl Default for TextMateGrammars {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for TextMateGrammars {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TextMateGrammars").finish_non_exhaustive()
    }
}

// ========================================================================
// TextMateHighlighter Implementations
// ========================================================================

impl TextMateHighlighter {
    /// A highlighter of `document` in `grammar`, its language configured
    /// as `config` says, nothing tokenized yet.
    pub fn new(grammar: Arc<Grammar>, config: LanguageConfig, document: &Document) -> Self {
        Self {
            brackets: Brackets::new(&config.brackets),
            colorer: Colorer {
                keywords: Arc::default(),
                markdown: false,
                style: Arc::new(NamedStyle::new()),
                words: false,
            },
            config,
            keywords_seen: 0,
            lines: Vec::new(),
            kept: Kept::default(),
            text: document.clone(),
            tokenizer: tokenizer(grammar),
        }
    }

    /// Tokenizes the next line of `document`, which must exist, then keeps
    /// the lines after an edit if it ends in the state the first of them
    /// began in.
    fn tokenize_next(&mut self, document: &Document) {
        let line = self.lines.len();
        let text = document.line_text(line).unwrap_or_default();
        let state = self.lines.last().map(|line| &line.state);
        let tokenized = self.tokenizer.tokenize_line(&text, state);
        let spans = self.colorer.color(&text, &tokenized.tokens);
        self.lines.push(TokenizedLine {
            brackets: self.brackets.find(&text, &tokenized.tokens),
            spans,
            state: tokenized.state,
        });
        self.keep_if_unchanged();
    }

    /// Line `line` of `document` with its tokens, tokenized again from the
    /// state the line before ended in; `None` until that line is.
    fn tokens_of(&mut self, document: &Document, line: usize) -> Option<(String, Vec<Token>)> {
        let state = match line.checked_sub(1) {
            Some(before) => Some(self.lines.get(before)?.state.clone()),
            None => None,
        };
        let text = document.line_text(line)?;
        let tokens = self.tokenizer.tokenize_line(&text, state.as_ref()).tokens;
        Some((text, tokens))
    }

    /// Keeps the lines after the last edit when the line just tokenized
    /// ends in the state the first of them, or one past it, began in: from
    /// there on the text and the states are what they were.
    fn keep_if_unchanged(&mut self) {
        let next = self.lines.len();
        let Some(skipped) = next.checked_sub(self.kept.first) else {
            return;
        };
        if skipped > self.kept.lines.len() {
            self.kept = Kept::default();
            return;
        }
        let began = match skipped {
            0 => self.kept.start.as_ref(),
            _ => Some(&self.kept.lines[skipped - 1].state),
        };
        let ended = self.lines.last().map(|line| &line.state);
        if began.is_some() && began == ended {
            let kept = std::mem::take(&mut self.kept);
            self.lines.extend(kept.lines.into_iter().skip(skipped));
        }
    }
}

// ========================================================================
// LineHighlighter Implementations
// ========================================================================

impl LineHighlighter {
    /// The highlighted spans of `line`, the next line of the text, as char
    /// columns into it, each with its highlight; then on to the line after.
    pub fn line(&mut self, line: &str) -> Vec<(Range<usize>, HighlightId)> {
        let tokenized = self.tokenizer.tokenize_line(line, self.start.0.as_ref());
        let spans = self.colorer.color(line, &tokenized.tokens);
        self.start = LineStart(Some(tokenized.state));
        spans
    }

    /// Past `line`, the next line of the text, without highlighting it: on
    /// to where the line after begins.
    pub fn skip(&mut self, line: &str) {
        let tokenized = self.tokenizer.tokenize_line(line, self.start.0.as_ref());
        self.start = LineStart(Some(tokenized.state));
    }

    /// Whether `line`, the next line of the text, names something it
    /// defines (a function, a type, a module, a section), its grammar says;
    /// then on to the line after.
    pub fn defines(&mut self, line: &str) -> bool {
        let tokenized = self.tokenizer.tokenize_line(line, self.start.0.as_ref());
        let names = |wanted: &str, scope: &str| {
            scope
                .strip_prefix(wanted)
                .is_some_and(|rest| rest.is_empty() || rest.starts_with('.'))
        };
        let defines = tokenized.tokens.iter().any(|token| {
            // Go's grammar names a call `entity.name.function.support`.
            let defined = token.scopes.iter_inner_first().any(|scope| {
                DEFINITIONS.iter().any(|definition| {
                    names(definition, scope) && !scope[definition.len()..].starts_with(SUPPORT)
                })
            });
            let called = token
                .scopes
                .iter_inner_first()
                .any(|scope| CALLS.iter().any(|call| names(call, scope)));
            defined && !called
        });
        self.start = LineStart(Some(tokenized.state));
        defines
    }

    /// Where the next line begins, to [`resume`](Self::resume) from.
    pub fn start(&self) -> LineStart {
        self.start.clone()
    }

    /// Goes back, or on, to a line beginning where `start` says.
    pub fn resume(&mut self, start: LineStart) {
        self.start = start;
    }
}

impl fmt::Debug for LineHighlighter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LineHighlighter")
            .field("start", &self.start)
            .finish_non_exhaustive()
    }
}

// ========================================================================
// Colorer Implementations
// ========================================================================

impl Colorer {
    /// The spans of `line`, tokenized as `tokens`, as char columns into it:
    /// its runs named more closely by the passes its language turns on,
    /// then styled, a run of one highlight one span.
    fn color(&self, line: &str, tokens: &[Token]) -> Vec<(Range<usize>, HighlightId)> {
        let runs: Vec<Piece> = tokens
            .iter()
            .map(|token| {
                let mut scopes: Vec<&str> = token.scopes.iter_inner_first().collect();
                scopes.reverse();
                Piece {
                    range: token.start..token.end,
                    scopes,
                }
            })
            .collect();
        let keywords = self
            .keywords
            .words
            .read()
            .unwrap_or_else(PoisonError::into_inner);
        let passes = Passes {
            keywords: &keywords,
            markdown: self.markdown,
            words: self.words,
        };
        let pieces = refine(line, &runs, passes, |scopes| self.style.colors(scopes));
        let mut spans: Vec<(Range<usize>, HighlightId)> = Vec::new();
        let (mut byte, mut char) = (0, 0);
        for piece in &pieces {
            let from = char + line[byte..piece.range.start].chars().count();
            let to = from + line[piece.range.clone()].chars().count();
            (byte, char) = (piece.range.end, to);
            let Some(id) = self.style.highlight(&piece.scopes) else {
                continue;
            };
            match spans.last_mut() {
                Some((last, last_id)) if *last_id == id && last.end == from => last.end = to,
                _ => spans.push((from..to, id)),
            }
        }
        spans
    }
}

impl Highlighter for TextMateHighlighter {
    fn enter(&mut self, document: &Document, offset: usize) -> Option<EnterIndent> {
        let position = document.char_to_position(offset)?;
        let (text, tokens) = self.tokens_of(document, position.line)?;
        let brackets = self.brackets.find(&text, &tokens);
        let cursor = text
            .char_indices()
            .nth(position.column)
            .map_or(text.len(), |(byte, _)| byte);
        let previous = position
            .line
            .checked_sub(1)
            .and_then(|line| document.line_text(line));
        let action =
            self.config
                .enter_action(&text, &tokens, &brackets, cursor, previous.as_deref());
        Some(EnterIndent {
            append: action.append,
            indent: match action.indent {
                Indent::Indent => IndentChange::Indent,
                Indent::Keep => IndentChange::Keep,
                Indent::Outdent => IndentChange::Outdent,
            },
            remove: action.remove,
            split: action.split,
        })
    }

    fn outdents(&mut self, document: &Document, line: usize) -> bool {
        self.tokens_of(document, line)
            .is_some_and(|(text, tokens)| self.config.outdents(&text, &tokens))
    }

    fn fold_ranges(&self, document: &Document) -> Option<Vec<RangeInclusive<usize>>> {
        let folds = match self.config.folding {
            Folding::Brackets => {
                let lines: Vec<&[Bracket]> =
                    self.lines.iter().map(|line| &*line.brackets).collect();
                fold_by_brackets(&lines)
            }
            Folding::Indentation => {
                let lines: Vec<String> = (0..document.len_lines())
                    .filter_map(|line| document.line_text(line))
                    .collect();
                fold_by_indentation(&lines, FOLD_TAB_WIDTH)
            }
        };
        Some(
            folds
                .into_iter()
                .map(|fold| fold.start..=fold.end)
                .collect(),
        )
    }

    fn edit(&mut self, changes: &ChangeSet, document: &Document) {
        let edits = changes.edits();
        let lines_of = |offset: usize| self.text.char_to_position(offset).map(|at| at.line);
        let first = edits
            .iter()
            .filter_map(|edit| lines_of(edit.range.start))
            .min();
        let last = edits
            .iter()
            .filter_map(|edit| lines_of(edit.range.end))
            .max();
        if let (Some(first), Some(last)) = (first, last) {
            // The lines after the edited ones, numbered anew by the lines
            // the edit added or took away.
            let added = document.len_lines() as isize - self.text.len_lines() as isize;
            let after = last + 1;
            self.kept = match after <= self.lines.len() {
                true => Kept {
                    start: after
                        .checked_sub(1)
                        .map(|line| self.lines[line].state.clone()),
                    first: after.saturating_add_signed(added),
                    lines: self.lines.split_off(after),
                },
                false => Kept::default(),
            };
            self.lines.truncate(first);
        }
        self.text = document.clone();
    }

    fn prepare(&mut self, document: &Document, range: Range<usize>, lines: usize) -> Preparation {
        // The todo keywords changed: every line is colored again, drawn as
        // it was meanwhile.
        let changes = self.colorer.keywords.changes.load(Ordering::Relaxed);
        if changes != self.keywords_seen {
            self.keywords_seen = changes;
            self.kept = Kept {
                start: None,
                first: 0,
                lines: std::mem::take(&mut self.lines),
            };
        }
        let last = document
            .char_to_position(range.end.min(document.len_chars()))
            .map_or(0, |position| position.line);
        let end = (last + 1).min(document.len_lines());
        for _ in 0..lines {
            if self.lines.len() >= end {
                break;
            }
            self.tokenize_next(document);
        }
        match self.lines.len() >= end {
            true => Preparation::Ready,
            false => Preparation::Pending,
        }
    }

    fn highlights(&self, document: &Document, range: Range<usize>) -> Vec<HighlightSpan> {
        let first = document
            .char_to_position(range.start.min(document.len_chars()))
            .map_or(0, |position| position.line);
        let mut spans = Vec::new();
        for (line, tokenized) in self.lines.iter().enumerate().skip(first) {
            let Some(start) = document.position_to_char(Position { column: 0, line }) else {
                break;
            };
            if start >= range.end {
                break;
            }
            for (columns, id) in &tokenized.spans {
                let chars = start + columns.start..start + columns.end;
                if chars.end > range.start && chars.start < range.end {
                    spans.push(HighlightSpan::new(chars, *id));
                }
            }
        }
        // The lines after an edit not tokenized again yet are drawn as they
        // were, moved with their text, rather than plain for a frame at
        // each key typed.
        let tokenized = self.lines.len();
        for (offset, kept) in self.kept.lines.iter().enumerate() {
            let line = self.kept.first + offset;
            if line < tokenized || line < first {
                continue;
            }
            let Some(start) = document.position_to_char(Position { column: 0, line }) else {
                break;
            };
            if start >= range.end {
                break;
            }
            // A kept run may reach past its line, which the edit shortened.
            let end = document
                .position_to_char(Position {
                    column: 0,
                    line: line + 1,
                })
                .unwrap_or_else(|| document.len_chars());
            for (columns, id) in &kept.spans {
                let chars = start + columns.start..(start + columns.end).min(end);
                if chars.start < chars.end && chars.end > range.start && chars.start < range.end {
                    spans.push(HighlightSpan::new(chars, *id));
                }
            }
        }
        spans
    }
}

impl fmt::Debug for TextMateHighlighter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TextMateHighlighter")
            .field("lines", &self.lines.len())
            .finish_non_exhaustive()
    }
}

// ========================================================================
// Functions
// ========================================================================

/// How far `line` is indented, in columns, a tab counting for
/// [`TAB_COLUMNS`].
fn indent_of(line: &str) -> usize {
    line.chars()
        .take_while(|c| c.is_whitespace())
        .map(|c| if c == '\t' { TAB_COLUMNS } else { 1 })
        .sum()
}

/// A tokenizer for `grammar`, reading no line longer than [`LONGEST_LINE`].
fn tokenizer(grammar: Arc<Grammar>) -> Tokenizer {
    Tokenizer::new(grammar).with_max_line_length(LONGEST_LINE)
}

/// The one char `text` is, if it is one.
fn single_char(text: &str) -> Option<char> {
    let mut chars = text.chars();
    let c = chars.next()?;
    chars.next().is_none().then_some(c)
}

// ========================================================================
// Tests
// ========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::{Timestamp, parse_key};
    use crate::{EditorOptions, ViewId};

    /// An editor with one view onto `text`, the file `path`, configured by
    /// its language and prepared, the cursor at char `cursor`.
    fn open_editor(path: &str, text: &str, cursor: usize) -> (Editor, ViewId) {
        let options = EditorOptions {
            auto_indent: true,
            auto_pairs: true,
            expand_tab: true,
            shift_width: 4,
            ..EditorOptions::default()
        };
        let mut editor = Editor::new(options);
        let document = editor.open_document(text);
        assert!(TextMateGrammars::new().configure(&mut editor, document, path));
        let view = editor.new_view(document).unwrap();
        editor.set_cursor(view, cursor);
        editor.tick(Timestamp::default());
        (editor, view)
    }

    /// Feeds `keys`, written as space-separated key notation.
    fn keys(editor: &mut Editor, view: ViewId, keys: &str) {
        for token in keys.split_whitespace() {
            editor.handle_key(view, parse_key(token).unwrap(), Timestamp::default());
            editor.tick(Timestamp::default());
        }
    }

    fn text(editor: &Editor, view: ViewId) -> String {
        let document = editor.view(view).unwrap().document();
        editor.document(document).unwrap().to_string()
    }

    #[test]
    fn test_enter_indents_as_the_language_says() {
        // A Python block opens after `:`.
        let (mut editor, view) = open_editor("x.py", "if ready:", 9);
        keys(&mut editor, view, "A <cr> x");
        assert_eq!(text(&editor, view), "if ready:\n    x");
        // Between a pair of braces, the closer moves to its own line.
        let (mut editor, view) = open_editor("x.rs", "fn f() {}", 8);
        keys(&mut editor, view, "i <cr> x");
        assert_eq!(text(&editor, view), "fn f() {\n    x\n}");
        // A Rust doc comment goes on.
        let (mut editor, view) = open_editor("x.rs", "/// Docs.", 9);
        keys(&mut editor, view, "A <cr> x");
        assert_eq!(text(&editor, view), "/// Docs.\n/// x");
    }

    #[test]
    fn test_a_typed_line_closing_its_block_outdents_once() {
        let (mut editor, view) = open_editor("x.py", "if a:\n    b", 11);
        keys(&mut editor, view, "A <cr> e l s e :");
        assert_eq!(text(&editor, view), "if a:\n    b\nelse:");
    }

    #[test]
    fn test_the_language_s_comments_pairs_and_folds_are_used() {
        let (mut editor, view) = open_editor("x.lua", "f()\nx = 1", 4);
        keys(&mut editor, view, "g c c");
        assert_eq!(text(&editor, view), "f()\n-- x = 1", "Lua's line comment");
        let (editor, view) = open_editor("x.rs", "fn f() {\n    a();\n}\n", 0);
        let document = editor.view(view).unwrap().document();
        let folds = editor
            .highlighter(document)
            .unwrap()
            .fold_ranges(editor.document(document).unwrap());
        let braces: Vec<RangeInclusive<usize>> = std::iter::once(0..=1).collect();
        assert_eq!(folds, Some(braces), "the braces' lines");
    }
    use crate::syntax::names::highlight_name;
    use crate::{ChangeSet, Edit};

    /// The highlighted text of `document` with its names, in order.
    fn named(highlighter: &TextMateHighlighter, document: &Document) -> Vec<(String, String)> {
        highlighter
            .highlights(document, 0..document.len_chars())
            .into_iter()
            .map(|span| {
                let name = highlight_name(span.highlight).unwrap_or_default();
                (document.slice(span.range), name.to_owned())
            })
            .collect()
    }

    #[test]
    fn test_a_file_is_highlighted_in_the_language_its_name_says() {
        let grammars = TextMateGrammars::new();
        let document = Document::new("fn main() {\n    let s = \"hé\";\n}\n");
        let mut highlighter = grammars.highlighter("main.rs", &document).expect("rust");
        let all = 0..document.len_chars();
        assert_eq!(highlighter.prepare(&document, all, 100), Preparation::Ready);
        let spans = named(&highlighter, &document);
        assert!(
            spans.contains(&("fn".into(), "keyword".into())),
            "{spans:?}"
        );
        assert!(
            spans.contains(&("main".into(), "function".into())),
            "{spans:?}"
        );
        // Char offsets, past a two-byte char.
        assert!(
            spans.contains(&("\"hé\"".into(), "string".into())),
            "{spans:?}"
        );
        assert!(grammars.highlighter("notes.unknown", &document).is_none());
    }

    #[test]
    fn test_preparing_takes_a_budget_of_lines_at_a_time() {
        let grammars = TextMateGrammars::new();
        let document = Document::new("a = 1\nb = 2\nc = 3\n");
        let mut highlighter = grammars.highlighter("x.py", &document).expect("python");
        let all = 0..document.len_chars();
        assert_eq!(
            highlighter.prepare(&document, all.clone(), 2),
            Preparation::Pending
        );
        assert_eq!(highlighter.prepare(&document, all, 2), Preparation::Ready);
    }

    /// A host's style: every scope stack its innermost scope names, given
    /// ids in the order they are first asked for.
    struct Innermost(Mutex<Vec<String>>);

    impl ScopeStyle for Innermost {
        fn highlight(&self, scopes: &[&str]) -> Option<HighlightId> {
            let top = scopes.last()?.to_string();
            let mut seen = self.0.lock().unwrap_or_else(PoisonError::into_inner);
            let index = seen
                .iter()
                .position(|name| *name == top)
                .unwrap_or_else(|| {
                    seen.push(top);
                    seen.len() - 1
                });
            Some(HighlightId::new(index as u16))
        }
    }

    #[test]
    fn test_a_host_s_style_names_the_spans_and_todo_keywords_are_scoped() {
        let style = Arc::new(Innermost(Mutex::default()));
        struct Shared(Arc<Innermost>);
        impl ScopeStyle for Shared {
            fn highlight(&self, scopes: &[&str]) -> Option<HighlightId> {
                self.0.highlight(scopes)
            }
        }
        let grammars = TextMateGrammars::new().with_style(Shared(Arc::clone(&style)));
        grammars.set_todo_keywords(&["TODO".to_string()]);
        let document = Document::new("let x = 1; // TODO: y\n");
        let mut highlighter = grammars.highlighter("main.rs", &document).expect("rust");
        highlighter.prepare(&document, 0..document.len_chars(), 10);
        let spans = highlighter.highlights(&document, 0..document.len_chars());
        let seen = style
            .0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        let todo = spans
            .iter()
            .find(|span| document.slice(span.range.clone()) == "TODO")
            .expect("the keyword is a span of its own");
        assert_eq!(seen[usize::from(todo.highlight.index())], "keyword.todo");
        let x = spans
            .iter()
            .find(|span| document.slice(span.range.clone()) == "x")
            .expect("the grammar names the binding");
        assert_eq!(
            seen[usize::from(x.highlight.index())],
            "variable.other.rust",
            "the names pass never overrules a scope the grammar gave"
        );
        // Keywords changed: the lines are colored again.
        grammars.set_todo_keywords(&[]);
        highlighter.prepare(&document, 0..document.len_chars(), 10);
        let spans = highlighter.highlights(&document, 0..document.len_chars());
        assert!(
            !spans
                .iter()
                .any(|span| document.slice(span.range.clone()) == "TODO"),
            "no longer apart from its comment"
        );
    }

    #[test]
    fn test_lines_below_an_edit_keep_their_colors_until_tokenized_again() {
        let grammars = TextMateGrammars::new();
        let text: String = (0..50).map(|n| format!("x{n} = {n}\n")).collect();
        let mut document = Document::new(&text);
        let mut highlighter = grammars.highlighter("x.py", &document).expect("python");
        highlighter.prepare(&document, 0..document.len_chars(), 1_000);
        // A string opened at the top changes every state after it.
        let changes = ChangeSet::from_edits(document.len_chars(), [Edit::new(0..0, "\"\"\"\n")])
            .expect("in range");
        changes.apply(&mut document).expect("applies");
        highlighter.edit(&changes, &document);
        let spans = named(&highlighter, &document);
        assert!(
            spans.contains(&("49".into(), "number".into())),
            "the last line drawn as it was, before any of it is tokenized again"
        );
    }

    #[test]
    fn test_a_line_highlighter_colors_lines_as_a_document_s_highlighter_does() {
        let grammars = TextMateGrammars::new();
        let text = "/* a\n b */ fn f() {}\nlet s = \"x\";";
        let document = Document::new(text);
        let mut highlighter = grammars.highlighter("x.rs", &document).expect("rust");
        highlighter.prepare(&document, 0..document.len_chars(), 10);
        let whole = named(&highlighter, &document);
        let mut lines = grammars.line_highlighter("x.rs", "/* a").expect("rust");
        let mut by_line = Vec::new();
        for line in text.lines() {
            for (columns, id) in lines.line(line) {
                let piece: String = line
                    .chars()
                    .skip(columns.start)
                    .take(columns.len())
                    .collect();
                by_line.push((piece, highlight_name(id).unwrap_or_default().to_owned()));
            }
        }
        assert_eq!(by_line, whole, "the comment carried from line to line");
    }

    #[test]
    fn test_a_line_highlighter_skips_lines_and_goes_back_to_where_one_began() {
        let grammars = TextMateGrammars::new();
        let mut lines = grammars.line_highlighter_named("py").expect("by extension");
        lines.skip("s = \"\"\"");
        let inside = lines.start();
        let in_string = lines.line("not code");
        lines.resume(inside.clone());
        assert_eq!(
            lines.line("not code"),
            in_string,
            "the same, from the same start"
        );
        assert!(grammars.line_highlighter_named("rust").is_some(), "by id");
        assert!(
            grammars
                .line_highlighter_named("no-such-language")
                .is_none()
        );
    }

    #[test]
    fn test_definitions_are_the_lines_a_grammar_names_one_on_nested_by_indent() {
        let grammars = TextMateGrammars::new();
        let text = "struct S;\n\nimpl S {\n    fn new() -> Self { S }\n    // fn not_one()\n}\nfn main() { S::new(); }\n    run();";
        let found: Vec<(usize, usize, String)> = grammars
            .definitions("a.rs", text)
            .expect("rust has a grammar")
            .into_iter()
            .map(|entry| (entry.level, entry.line, entry.title))
            .collect();
        assert_eq!(
            found,
            [
                (1, 0, "struct S;".to_string()),
                (1, 2, "impl S {".to_string()),
                (2, 3, "fn new() -> Self { S }".to_string()),
                (1, 6, "fn main() { S::new(); }".to_string()),
            ],
            "a call and a comment are no definitions"
        );
        assert!(grammars.definitions("notes.unknown-ext", "x").is_none());
    }

    #[test]
    fn test_definitions_leave_calls_out_in_each_language() {
        let grammars = TextMateGrammars::new();
        let lines_of = |path: &str, text: &str| -> Vec<usize> {
            grammars
                .definitions(path, text)
                .expect("a grammar")
                .into_iter()
                .map(|entry| entry.line)
                .collect()
        };
        assert_eq!(
            lines_of(
                "a.py",
                "def f():\n    g()\nclass C:\n    def m(self):\n        self.n()"
            ),
            [0, 2, 3],
            "Python"
        );
        assert_eq!(
            lines_of(
                "a.js",
                "function a() {\n  b();\n}\nclass K {}\nobj.call(1);"
            ),
            [0, 3],
            "JavaScript"
        );
        assert_eq!(
            lines_of(
                "a.go",
                "package p\nfunc F() {\n\tfmt.Println(1)\n}\ntype T struct{}"
            ),
            [0, 1, 4],
            "Go: its package, a function, a type, not a call"
        );
    }

    #[test]
    fn test_lines_an_edit_leaves_in_their_state_are_kept() {
        let grammars = TextMateGrammars::new();
        let text: String = (0..200).map(|n| format!("x{n} = {n}\n")).collect();
        let mut document = Document::new(&text);
        let mut highlighter = grammars.highlighter("x.py", &document).expect("python");
        let all = 0..document.len_chars();
        highlighter.prepare(&document, all, 1_000);
        // A line added at the top changes no state after it.
        let changes = ChangeSet::from_edits(document.len_chars(), [Edit::new(0..0, "y = 1\n")])
            .expect("in range");
        changes.apply(&mut document).expect("applies");
        highlighter.edit(&changes, &document);
        let all = 0..document.len_chars();
        assert_eq!(
            highlighter.prepare(&document, all, 3),
            Preparation::Ready,
            "the lines after the first two were kept"
        );
        let last = document.len_lines() - 2;
        let start = document
            .position_to_char(Position {
                column: 0,
                line: last,
            })
            .unwrap();
        let spans = named(&highlighter, &document);
        assert_eq!(spans.last(), Some(&("199".into(), "number".into())));
        let on_last = highlighter.highlights(&document, start..document.len_chars());
        let texts: Vec<String> = on_last
            .iter()
            .map(|span| document.slice(span.range.clone()))
            .collect();
        assert!(
            texts.contains(&"199".to_owned()),
            "at its new place: {texts:?}"
        );
    }

    #[test]
    fn test_lines_an_edit_changes_the_state_of_are_tokenized_again() {
        let grammars = TextMateGrammars::new();
        let mut document = Document::new("a = 1\nb = 2\nc = 3\n");
        let mut highlighter = grammars.highlighter("x.py", &document).expect("python");
        highlighter.prepare(&document, 0..document.len_chars(), 100);
        // A string opened on the first line runs over the lines after.
        let changes = ChangeSet::from_edits(document.len_chars(), [Edit::new(4..4, "\"\"\"")])
            .expect("in range");
        changes.apply(&mut document).expect("applies");
        highlighter.edit(&changes, &document);
        highlighter.prepare(&document, 0..document.len_chars(), 100);
        let spans = named(&highlighter, &document);
        assert!(!spans.contains(&("3".into(), "number".into())), "{spans:?}");
        assert!(
            spans
                .iter()
                .any(|(text, name)| text.contains("c = 3") && name == "string"),
            "{spans:?}"
        );
    }

    #[test]
    fn test_an_edit_tokenizes_again_from_the_line_it_changed() {
        let grammars = TextMateGrammars::new();
        let mut document = Document::new("x = 1\ny = 2\n");
        let mut highlighter = grammars.highlighter("x.py", &document).expect("python");
        let all = 0..document.len_chars();
        highlighter.prepare(&document, all, 100);
        // Line 2 opens a string that runs on: `y = "2`.
        let changes = ChangeSet::from_edits(document.len_chars(), [Edit::new(10..10, "\"")])
            .expect("in range");
        changes.apply(&mut document).expect("applies");
        highlighter.edit(&changes, &document);
        highlighter.prepare(&document, 0..document.len_chars(), 100);
        let spans = named(&highlighter, &document);
        assert!(
            spans
                .iter()
                .any(|(text, name)| text.starts_with('"') && name == "string"),
            "{spans:?}"
        );
        assert!(
            spans.contains(&("1".into(), "number".into())),
            "the line before kept"
        );
    }
}
