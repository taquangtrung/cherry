//! The highlighter: a tree-sitter parse of one document, an iris
//! [`SyntaxTree`], kept current by reparsing incrementally on every change
//! the editor reports, queried for the highlighted spans in sight and for
//! structural text objects. What a grammar embeds (a Markdown fence's
//! code, a Rust macro's body) is highlighted in the bundled grammar of its
//! language.

use std::fmt;
use std::iter;
use std::ops::Range;
use std::sync::{Arc, Mutex, PoisonError};

use iris::tree_sitter::{InputEdit, Node, Point};
use iris::{SyntaxTree, TreeSitterGrammar};

use crate::{ChangeSet, Document, HighlightId, HighlightSpan, Highlighter};

use super::error::SyntaxError;
use super::language::Language;
use super::names::resolver;

// ========================================================================
// Constants
// ========================================================================

/// The line break, which moves a point to the next row.
const LINE_BREAK: char = '\n';

/// The node kind that opens a braced body.
const OPEN_BRACE: &str = "{";

/// The node kind that closes a braced body.
const CLOSE_BRACE: &str = "}";

/// The field naming the body of a function or class node.
const BODY_FIELD: &str = "body";

/// The text object name of functions.
pub(crate) const FUNCTION_OBJECT: &str = "function";

/// The text object name of classes and types.
pub(crate) const CLASS_OBJECT: &str = "class";

// ========================================================================
// Data Structures
// ========================================================================

/// The syntax of one document in one [`Language`], for
/// [`Editor::set_highlighter`](crate::Editor::set_highlighter). Its
/// spans name the [`HIGHLIGHT_NAMES`](crate::syntax::HIGHLIGHT_NAMES); it knows
/// the text objects `"function"` and `"class"`.
pub struct SyntaxHighlighter {
    /// The language parsed.
    language: Language,
    /// The parse of `text`, with what it embeds.
    syntax: SyntaxTree,
    /// The text as last parsed, to find where the next change was made.
    text: Document,
}

/// Each bundled language's grammar with its queries, compiled the first
/// time a document in it, or embedding it, is highlighted.
static GRAMMARS: Mutex<Vec<(Language, Arc<TreeSitterGrammar>)>> = Mutex::new(Vec::new());

// ========================================================================
// SyntaxHighlighter Implementations
// ========================================================================

impl SyntaxHighlighter {
    /// A highlighter of `document` in `language`, parsed now. Fails only
    /// when the grammar does not load into this tree-sitter or its query
    /// does not compile, a build problem rather than one of the text.
    pub fn new(language: Language, document: &Document) -> Result<Self, SyntaxError> {
        let mut syntax = SyntaxTree::new(compiled(language)?, resolver())?;
        syntax.set_injections(|name| {
            let language = Language::from_injection_name(name)?;
            compiled(language).ok()
        });
        let mut highlighter = Self {
            language,
            syntax,
            text: document.clone(),
        };
        highlighter.reparse();
        Ok(highlighter)
    }

    /// The language parsed.
    pub fn language(&self) -> Language {
        self.language
    }

    /// The smallest node holding char `offset` of the text, if parsed.
    fn node_at(&self, offset: usize) -> Option<Node<'_>> {
        let byte = self.text.char_to_byte(offset);
        self.syntax
            .tree()?
            .root_node()
            .descendant_for_byte_range(byte, byte)
    }

    /// Parses the text again, reusing the old tree where it still holds.
    fn reparse(&mut self) {
        let text = &self.text;
        let mut read = |byte: usize, _: Point| text.chunk_at_byte(byte).as_bytes();
        self.syntax.parse_with(&mut read);
    }
}

impl Highlighter for SyntaxHighlighter {
    fn edit(&mut self, changes: &ChangeSet, document: &Document) {
        for edit in input_edits(&self.text, changes) {
            self.syntax.edit(&edit);
        }
        self.text = document.clone();
        self.reparse();
    }

    fn highlights(&self, document: &Document, range: Range<usize>) -> Vec<HighlightSpan> {
        let bytes = document.char_to_byte(range.start)..document.char_to_byte(range.end);
        let source = |node: Node<'_>| {
            let chars =
                document.byte_to_char(node.start_byte())..document.byte_to_char(node.end_byte());
            iter::once(document.slice(chars).into_bytes())
        };
        // Outer spans first, inner ones after so they win, as iris orders
        // them.
        self.syntax
            .highlights(source, bytes)
            .into_iter()
            .filter_map(|span| {
                let id = HighlightId::new(u16::try_from(span.highlight).ok()?);
                let chars =
                    document.byte_to_char(span.range.start)..document.byte_to_char(span.range.end);
                Some(HighlightSpan::new(chars, id))
            })
            .collect()
    }

    fn text_object(
        &self,
        document: &Document,
        object: &str,
        around: bool,
        offset: usize,
        count: usize,
    ) -> Option<Range<usize>> {
        let structure = self.language.structure();
        let kinds = match object {
            CLASS_OBJECT => structure.classes,
            FUNCTION_OBJECT => structure.functions,
            _ => return None,
        };
        let mut node = self.node_at(offset);
        let mut left = count.max(1);
        let found = loop {
            let current = node?;
            if kinds.contains(&current.kind()) {
                left -= 1;
                if left == 0 {
                    break current;
                }
            }
            node = current.parent();
        };
        let bytes = if around {
            found.byte_range()
        } else {
            inside(found)?
        };
        let chars = document.byte_to_char(bytes.start)..document.byte_to_char(bytes.end);
        Some(trim_blanks(document, chars))
    }
}

impl fmt::Debug for SyntaxHighlighter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SyntaxHighlighter")
            .field("language", &self.language)
            .field("parsed", &self.syntax.tree().is_some())
            .finish()
    }
}

// ========================================================================
// Functions
// ========================================================================

/// `language`'s grammar with its highlights and injections queries,
/// compiled once and shared.
fn compiled(language: Language) -> Result<Arc<TreeSitterGrammar>, SyntaxError> {
    let mut grammars = GRAMMARS.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some((_, grammar)) = grammars.iter().find(|(known, _)| *known == language) {
        return Ok(grammar.clone());
    }
    let mut grammar = TreeSitterGrammar::new(language.grammar(), language.highlights_query())?;
    if let Some(injections) = language.injections_query() {
        grammar = grammar.with_injections(injections)?;
    }
    let grammar = Arc::new(grammar);
    grammars.push((language, grammar.clone()));
    Ok(grammar)
}

/// The bytes inside `node`: its body, without the braces around it.
fn inside(node: Node<'_>) -> Option<Range<usize>> {
    let body = node.child_by_field_name(BODY_FIELD)?;
    let count = body.child_count();
    let first = body.child(0)?;
    let last = body.child(count.checked_sub(1)?)?;
    if count >= 2 && first.kind() == OPEN_BRACE && last.kind() == CLOSE_BRACE {
        return Some(first.end_byte()..last.start_byte());
    }
    Some(body.byte_range())
}

/// The edits of `changes` to `text` as tree-sitter takes them, in bytes
/// and points, to tell a tree one after another: last edit first, so the
/// offsets of the edits before it, all counted in `text`, still hold when
/// each is told.
fn input_edits(text: &Document, changes: &ChangeSet) -> Vec<InputEdit> {
    changes
        .edits()
        .iter()
        .rev()
        .map(|edit| {
            let start_byte = text.char_to_byte(edit.range.start);
            let start_position = point_at(text, edit.range.start);
            InputEdit {
                new_end_byte: start_byte + edit.text.len(),
                new_end_position: point_after(start_position, &edit.text),
                old_end_byte: text.char_to_byte(edit.range.end),
                old_end_position: point_at(text, edit.range.end),
                start_byte,
                start_position,
            }
        })
        .collect()
}

/// The point `text` ends at when typed at `start`.
fn point_after(start: Point, text: &str) -> Point {
    match text.rfind(LINE_BREAK) {
        Some(last_break) => Point {
            column: text.len() - last_break - 1,
            row: start.row + text.matches(LINE_BREAK).count(),
        },
        None => Point {
            column: start.column + text.len(),
            row: start.row,
        },
    }
}

/// The tree-sitter point of char `offset` of `document`: its line and its
/// byte column.
fn point_at(document: &Document, offset: usize) -> Point {
    let position = document.char_to_position(offset).unwrap_or_default();
    let line_start = offset - position.column;
    Point {
        column: document.char_to_byte(offset) - document.char_to_byte(line_start),
        row: position.line,
    }
}

/// `range` without the blanks and line breaks at its ends, so the inside
/// of a braced body is its text alone.
fn trim_blanks(document: &Document, range: Range<usize>) -> Range<usize> {
    let text: Vec<char> = document.slice(range.clone()).chars().collect();
    let leading = text.iter().take_while(|c| c.is_whitespace()).count();
    let trailing = text[leading..]
        .iter()
        .rev()
        .take_while(|c| c.is_whitespace())
        .count();
    range.start + leading..range.end - trailing
}

// ========================================================================
// Tests
// ========================================================================

// The tests open documents in every bundled grammar.
#[cfg(all(test, feature = "syntax"))]
mod tests {
    use crate::keys::{Timestamp, parse_key};
    use crate::{Edit, Editor, EditorOptions, Mode, ViewId};

    use super::*;
    use crate::syntax::names::highlight_name;
    use crate::syntax::text_object::register_text_objects;

    /// The highlighted text of `document` and its highlight names, in order.
    fn named_spans(highlighter: &SyntaxHighlighter, document: &Document) -> Vec<(String, String)> {
        highlighter
            .highlights(document, 0..document.len_chars())
            .into_iter()
            .map(|span| {
                let name = highlight_name(span.highlight).unwrap_or_default();
                (document.slice(span.range), name.to_string())
            })
            .collect()
    }

    /// An editor with one view onto `text` highlighted as `language`, the
    /// cursor at char `cursor`, with the syntax text objects registered.
    fn open_editor(language: Language, text: &str, cursor: usize) -> (Editor, ViewId) {
        let mut editor = Editor::new(EditorOptions::default());
        let document = editor.open_document(text);
        let highlighter = SyntaxHighlighter::new(language, editor.document(document).unwrap());
        editor.set_highlighter(document, Some(Box::new(highlighter.unwrap())));
        register_text_objects(&mut editor).unwrap();
        let view = editor.new_view(document).unwrap();
        editor.set_cursor(view, cursor);
        (editor, view)
    }

    /// Feeds `keys`, written as space-separated key notation.
    fn keys(editor: &mut Editor, view: ViewId, keys: &str) {
        for token in keys.split_whitespace() {
            editor.handle_key(view, parse_key(token).unwrap(), Timestamp::default());
        }
    }

    fn text(editor: &Editor, view: ViewId) -> String {
        let document = editor.view(view).unwrap().document();
        editor.document(document).unwrap().to_string()
    }

    #[test]
    fn test_a_markdown_fence_highlights_in_its_language() {
        let mut document = Document::new("# Title\n\n```rust\nfn main() {}\n```\n");
        let mut highlighter = SyntaxHighlighter::new(Language::Markdown, &document).unwrap();
        let keyword = ("fn".to_string(), "keyword".to_string());
        assert!(named_spans(&highlighter, &document).contains(&keyword));
        // An edit in the fence highlights the fence again.
        let start = document.to_string().find("fn main").unwrap();
        let edit = Edit::new(start..start + "fn main() {}".len(), "let x = 1;");
        let changes = ChangeSet::from_edits(document.len_chars(), vec![edit]).unwrap();
        changes.apply(&mut document).unwrap();
        highlighter.edit(&changes, &document);
        let spans = named_spans(&highlighter, &document);
        assert!(
            spans.contains(&("let".to_string(), "keyword".to_string())),
            "{spans:?}"
        );
    }

    #[test]
    fn test_edits_become_byte_and_point_edits_last_first_across_wide_chars() {
        let text = Document::new("é\n日x\nz");
        let edits = vec![Edit::new(1..3, "ab\nc"), Edit::new(4..5, "")];
        let changes = ChangeSet::from_edits(text.len_chars(), edits).unwrap();
        let point = |row, column| Point { column, row };
        assert_eq!(
            input_edits(&text, &changes),
            [
                InputEdit {
                    new_end_byte: 7,
                    new_end_position: point(1, 4),
                    old_end_byte: 8,
                    old_end_position: point(2, 0),
                    start_byte: 7,
                    start_position: point(1, 4),
                },
                InputEdit {
                    new_end_byte: 6,
                    new_end_position: point(1, 1),
                    old_end_byte: 6,
                    old_end_position: point(1, 3),
                    start_byte: 2,
                    start_position: point(0, 2),
                },
            ]
        );
    }

    #[test]
    fn test_rust_keywords_strings_and_escapes_get_their_names_inner_spans_last() {
        let document = Document::new("fn main() { let s = \"a\\n\"; }");
        let highlighter = SyntaxHighlighter::new(Language::Rust, &document).unwrap();
        let spans = named_spans(&highlighter, &document);
        assert!(
            spans.contains(&("fn".to_string(), "keyword".to_string())),
            "{spans:?}"
        );
        assert!(
            spans.contains(&("main".to_string(), "function".to_string())),
            "{spans:?}"
        );
        let string = spans.iter().position(|span| span.1 == "string").unwrap();
        let escape = spans.iter().position(|span| span.1 == "escape").unwrap();
        assert_eq!(spans[escape].0, "\\n");
        assert!(string < escape, "the escape inside the string wins");
        let tail = highlighter.highlights(&document, 20..document.len_chars());
        assert!(
            tail.iter().all(|span| span.range.end > 20),
            "only spans in range"
        );
    }

    #[test]
    fn test_incremental_reparses_match_a_fresh_parse_after_edits_undo_and_wide_chars() {
        let source = "fn a() { let x = 1; }\n// é日本\nfn b() { let y = 2; }\n";
        let (mut editor, view) = open_editor(Language::Rust, source, 0);
        let document_id = editor.view(view).unwrap().document();
        let steps = [
            "j A SPC 語 <esc>",
            ": % s / l e t / c o n s t / g <cr>",
            "G O f n SPC c ( ) SPC { } <esc>",
            "u",
            "g g d d",
            "u C-r",
        ];
        for step in steps {
            keys(&mut editor, view, step);
            let document = editor.document(document_id).unwrap();
            let fresh = SyntaxHighlighter::new(Language::Rust, document).unwrap();
            let kept = editor.highlighter(document_id).unwrap();
            assert_eq!(
                kept.highlights(document, 0..document.len_chars()),
                fresh.highlights(document, 0..document.len_chars()),
                "after {step:?}: {:?}",
                document.to_string()
            );
        }
        assert!(text(&editor, view).contains("const y"));
    }

    #[test]
    fn test_function_and_class_objects_act_after_operators_and_select_in_visual() {
        let source = "struct S;\nimpl S {\n    fn f(&self) {\n        let c = |x| x;\n    }\n}\n";
        let inside_closure = source.find("x;").unwrap();
        let (mut editor, view) = open_editor(Language::Rust, source, inside_closure);
        keys(&mut editor, view, "d a f");
        assert!(
            text(&editor, view).contains("let c = ;"),
            "{}",
            text(&editor, view)
        );
        keys(&mut editor, view, "u 2 d a f");
        assert_eq!(text(&editor, view), "struct S;\nimpl S {\n    \n}\n");
        keys(&mut editor, view, "u d i c");
        assert_eq!(text(&editor, view), "struct S;\nimpl S {\n    \n}\n");
        keys(&mut editor, view, "u v a c d");
        assert_eq!(text(&editor, view), "struct S;\n\n");
    }

    #[test]
    fn test_python_bodies_are_inner_objects_and_data_formats_have_none() {
        let source = "class A:\n    def f(self):\n        return 1\n";
        let body = source.find("return").unwrap();
        let (mut editor, view) = open_editor(Language::Python, source, body);
        keys(&mut editor, view, "d i f");
        assert_eq!(
            text(&editor, view),
            "class A:\n    def f(self):\n        \n"
        );
        let (mut editor, view) = open_editor(Language::Json, "{\"a\": [1]}", 2);
        keys(&mut editor, view, "d a f");
        assert_eq!(text(&editor, view), "{\"a\": [1]}");
        assert_eq!(editor.view(view).unwrap().mode(), Mode::Normal);
    }

    #[test]
    fn test_every_bundled_language_parses_and_highlights_a_sample() {
        let samples = [
            (Language::Json, "{\"key\": [1, true, null]}"),
            (Language::Markdown, "# Title\n\n- item\n\n```\ncode\n```\n"),
            (Language::Python, "def f(x):\n    return x  # note\n"),
            (Language::Rust, "fn f() -> u8 { 1 }"),
            (Language::Toml, "[table]\nkey = \"value\"\n"),
        ];
        for (language, sample) in samples {
            let document = Document::new(sample);
            let highlighter = SyntaxHighlighter::new(language, &document).unwrap();
            let tree = highlighter.syntax.tree().unwrap();
            assert!(!tree.root_node().has_error(), "{language:?} parses");
            let spans = named_spans(&highlighter, &document);
            assert!(!spans.is_empty(), "{language:?} highlights");
        }
    }
}
