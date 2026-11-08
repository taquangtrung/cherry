//! The document: the text being edited, held in a rope so that edits and
//! lookups stay fast however long the text grows.
//!
//! # Offsets
//!
//! Every offset is a char offset (a Unicode scalar value, not a byte), from
//! 0 to [`Document::len_chars`] inclusive. A [`Position`] is a line and a
//! column, both counted from 0, the column in chars. A cursor moves by
//! grapheme cluster (what a reader sees as one character, such as `e` plus
//! a combining accent), never splitting one; see
//! [`Document::next_grapheme_boundary`].
//!
//! # Lines and line endings
//!
//! The text is stored with `\n` as its only line break: `\r\n` and a lone
//! `\r` both become `\n`, when the document is made and in every inserted
//! text, so that line and column arithmetic never meets a `\r`. The document
//! remembers which ending the text came with (the dominant one, `\n` when
//! there is none or a tie), and [`Document::text_with_line_endings`] writes
//! that ending back for saving. A file that mixed endings is saved with the
//! dominant one throughout.
//!
//! A line is the text between two breaks, without the break. Text ending in
//! `\n` therefore ends with an empty line, where a cursor can sit: `"a\n"`
//! has two lines, and the empty text has one.

use std::fmt;
use std::ops::Range;

use ropey::{Rope, RopeSlice};
use thiserror::Error;
use unicode_segmentation::{GraphemeCursor, GraphemeIncomplete};

// ========================================================================
// Constants
// ========================================================================

/// The line break the document stores.
const LF: &str = "\n";

/// The Windows line break.
const CRLF: &str = "\r\n";

/// A lone carriage return, the classic Mac line break.
const CR: &str = "\r";

// ========================================================================
// Data Structures
// ========================================================================

/// Text being edited, with `\n` line breaks, remembering the line ending it
/// came with.
#[derive(Clone, Debug)]
pub struct Document {
    /// The line ending the text came with, written back on save.
    line_ending: LineEnding,
    /// The text, with `\n` as its only line break.
    text: Rope,
}

/// The line ending a text uses.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum LineEnding {
    /// `\r\n`, as on Windows.
    Crlf,
    /// `\n`, as on Unix; the default.
    #[default]
    Lf,
}

/// A line and a column, both from 0; the column counts chars. The default
/// is the start of the text.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub struct Position {
    /// The column, in chars from the start of the line.
    pub column: usize,
    /// The line, from 0.
    pub line: usize,
}

/// A line and a column, both from 0, the column counting UTF-16 code
/// units: a char outside the Basic Multilingual Plane (most emoji) counts
/// two. This is how the Language Server Protocol counts positions by
/// default.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub struct Utf16Position {
    /// The column, in UTF-16 code units from the start of the line.
    pub column: usize,
    /// The line, from 0.
    pub line: usize,
}

/// A char range that does not fit the document (or an insert offset, given
/// as an empty range), or whose start is past its end.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
#[error("char range {start}..{end} does not fit a document of {len} chars")]
pub struct RangeError {
    /// The end of the range, exclusive.
    pub end: usize,
    /// The length of the document, in chars.
    pub len: usize,
    /// The start of the range.
    pub start: usize,
}

// ========================================================================
// Document Implementations
// ========================================================================

impl Document {
    /// A document holding `text`, its line breaks normalized to `\n` and its
    /// dominant line ending remembered.
    pub fn new(text: &str) -> Self {
        Self {
            line_ending: LineEnding::detect(text),
            text: Rope::from_str(&normalize_line_breaks(text)),
        }
    }

    /// The char offset of the char holding byte `byte` of the text as
    /// UTF-8, clamped to the end; for a parser that counts bytes.
    pub fn byte_to_char(&self, byte: usize) -> usize {
        self.text.byte_to_char(byte.min(self.len_bytes()))
    }

    /// The byte offset, in the text as UTF-8, of the char offset `offset`,
    /// clamped to the end; for a parser that counts bytes.
    pub fn char_to_byte(&self, offset: usize) -> usize {
        self.text.char_to_byte(offset.min(self.len_chars()))
    }

    /// The text from byte `byte` to the end of the stored piece holding
    /// it, empty at or past the end; a parser reads the whole text piece
    /// by piece this way without copying it.
    pub fn chunk_at_byte(&self, byte: usize) -> &str {
        if byte >= self.len_bytes() {
            return "";
        }
        let (chunk, chunk_start, _, _) = self.text.chunk_at_byte(byte);
        &chunk[byte - chunk_start..]
    }

    /// The position of the char offset `offset`; `None` past the end.
    pub fn char_to_position(&self, offset: usize) -> Option<Position> {
        if offset > self.len_chars() {
            return None;
        }
        let line = self.text.char_to_line(offset);
        Some(Position {
            column: offset - self.text.line_to_char(line),
            line,
        })
    }

    /// The position of the char offset `offset`, its column counted in
    /// UTF-16 code units (see [`Utf16Position`]); `None` past the end.
    pub fn char_to_utf16_position(&self, offset: usize) -> Option<Utf16Position> {
        let position = self.char_to_position(offset)?;
        let line_start = offset - position.column;
        Some(Utf16Position {
            column: self.text.char_to_utf16_cu(offset) - self.text.char_to_utf16_cu(line_start),
            line: position.line,
        })
    }

    /// Deletes the chars in `range`.
    pub fn delete(&mut self, range: Range<usize>) -> Result<(), RangeError> {
        self.check_range(range.clone())?;
        self.text.remove(range);
        Ok(())
    }

    /// Inserts `text` at the char offset `offset`, its line breaks
    /// normalized to `\n`.
    pub fn insert(&mut self, offset: usize, text: &str) -> Result<(), RangeError> {
        self.check_range(offset..offset)?;
        self.text.insert(offset, &normalize_line_breaks(text));
        Ok(())
    }

    /// The length of the text in bytes, as UTF-8.
    pub fn len_bytes(&self) -> usize {
        self.text.len_bytes()
    }

    /// The length of the text in chars.
    pub fn len_chars(&self) -> usize {
        self.text.len_chars()
    }

    /// The number of lines of the raw text: one more than the number of
    /// line breaks, so `"a\nb\n"` has three (see [`Self::line_count`]).
    pub fn len_lines(&self) -> usize {
        self.text.len_lines()
    }

    /// The number of lines as the editor shows them: the empty line after
    /// a final line break is no line, so `"a\nb\n"` has two (as in Vim).
    pub fn line_count(&self) -> usize {
        line_count(self)
    }

    /// The line ending the text came with.
    pub fn line_ending(&self) -> LineEnding {
        self.line_ending
    }

    /// The text of line `line`, without its line break; `None` past the last
    /// line.
    pub fn line_text(&self, line: usize) -> Option<String> {
        let content = self.line_content(line)?;
        Some(self.text.slice(content).to_string())
    }

    /// The char offset just after the grapheme cluster at `offset`: where a
    /// cursor at `offset` moves one character right. A line break is a
    /// cluster of its own; at the end of the text, the end.
    pub fn next_grapheme_boundary(&self, offset: usize) -> usize {
        let offset = offset.min(self.len_chars());
        let content = self
            .line_content(self.text.char_to_line(offset))
            .unwrap_or(offset..offset);
        if offset >= content.end {
            return (offset + 1).min(self.len_chars());
        }
        let line = self.text.slice(content.clone());
        content.start + boundary_in(line, offset - content.start, true)
    }

    /// The char offset of `position`; a column past the end of its line is
    /// taken as the line's end. `None` past the last line.
    pub fn position_to_char(&self, position: Position) -> Option<usize> {
        let content = self.line_content(position.line)?;
        Some(
            content
                .start
                .saturating_add(position.column)
                .min(content.end),
        )
    }

    /// The char offset where the grapheme cluster before `offset` starts:
    /// where a cursor at `offset` moves one character left. At the start of
    /// the text, the start.
    pub fn prev_grapheme_boundary(&self, offset: usize) -> usize {
        let offset = offset.min(self.len_chars());
        let content = self
            .line_content(self.text.char_to_line(offset))
            .unwrap_or(offset..offset);
        if offset <= content.start || offset > content.end {
            return offset.saturating_sub(1);
        }
        let line = self.text.slice(content.clone());
        content.start + boundary_in(line, offset - content.start, false)
    }

    /// The text of the chars `range`, clamped to the document.
    pub fn slice(&self, range: Range<usize>) -> String {
        let end = range.end.min(self.len_chars());
        self.text.slice(range.start.min(end)..end).to_string()
    }

    /// The text with the line ending it came with, for saving.
    pub fn text_with_line_endings(&self) -> String {
        let text = self.text.to_string();
        match self.line_ending {
            LineEnding::Crlf => text.replace(LF, CRLF),
            LineEnding::Lf => text,
        }
    }

    /// The char offset of `position`, its column counted in UTF-16 code
    /// units; a column past the end of its line is taken as the line's
    /// end, and one inside a char's surrogate pair as that char. `None`
    /// past the last line.
    pub fn utf16_position_to_char(&self, position: Utf16Position) -> Option<usize> {
        let content = self.line_content(position.line)?;
        let start = self.text.char_to_utf16_cu(content.start);
        let end = self.text.char_to_utf16_cu(content.end);
        let unit = start.saturating_add(position.column).min(end);
        Some(self.text.utf16_cu_to_char(unit))
    }

    /// The char at offset `offset`, a line break as `\n`; `None` at or past
    /// the end.
    pub(crate) fn char_at(&self, offset: usize) -> Option<char> {
        self.text.get_char(offset)
    }

    /// The rope, for change sets to read deleted text from.
    pub(crate) fn rope(&self) -> &Rope {
        &self.text
    }

    /// The rope, for change sets to edit. Text put in it must already have
    /// `\n` line breaks only.
    pub(crate) fn rope_mut(&mut self) -> &mut Rope {
        &mut self.text
    }

    /// Fails unless `range` runs forward and fits the text.
    fn check_range(&self, range: Range<usize>) -> Result<(), RangeError> {
        let len = self.len_chars();
        if range.start > range.end || range.end > len {
            return Err(RangeError {
                end: range.end,
                len,
                start: range.start,
            });
        }
        Ok(())
    }

    /// The char range of line `line` without its line break; `None` past the
    /// last line.
    fn line_content(&self, line: usize) -> Option<Range<usize>> {
        if line >= self.len_lines() {
            return None;
        }
        let start = self.text.line_to_char(line);
        let next_start = if line + 1 < self.len_lines() {
            self.text.line_to_char(line + 1)
        } else {
            self.len_chars()
        };
        let has_break = next_start > start && self.text.char(next_start - 1) == '\n';
        Some(start..next_start - usize::from(has_break))
    }
}

impl fmt::Display for Document {
    /// Writes the text as stored, with `\n` line breaks.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.text.chunks().try_for_each(|chunk| f.write_str(chunk))
    }
}

// ========================================================================
// LineEnding Implementations
// ========================================================================

impl LineEnding {
    /// The dominant line ending of `text`: `\r\n` when it ends more lines
    /// than a bare `\n` does, else `\n`.
    fn detect(text: &str) -> Self {
        let crlf = text.matches(CRLF).count();
        let lf = text.matches(LF).count() - crlf;
        if crlf > lf { Self::Crlf } else { Self::Lf }
    }
}

// ========================================================================
// Functions
// ========================================================================

/// The char offset in `line` of the grapheme boundary after char `offset`
/// (before it when not `forward`), read from the rope's chunks around it so
/// a long line is never copied or segmented whole.
fn boundary_in(line: RopeSlice<'_>, offset: usize, forward: bool) -> usize {
    let byte = line.char_to_byte(offset);
    let mut cursor = GraphemeCursor::new(byte, line.len_bytes(), true);
    let (mut chunk, mut chunk_byte, _, _) = line.chunk_at_byte(byte);
    loop {
        let found = if forward {
            cursor.next_boundary(chunk, chunk_byte)
        } else {
            cursor.prev_boundary(chunk, chunk_byte)
        };
        match found {
            Ok(Some(boundary)) => return line.byte_to_char(boundary),
            Ok(None) => return if forward { line.len_chars() } else { 0 },
            Err(GraphemeIncomplete::NextChunk) => {
                chunk_byte += chunk.len();
                chunk = line.chunk_at_byte(chunk_byte).0;
            }
            Err(GraphemeIncomplete::PrevChunk) => {
                let (previous, previous_byte, _, _) = line.chunk_at_byte(chunk_byte - 1);
                (chunk, chunk_byte) = (previous, previous_byte);
            }
            Err(GraphemeIncomplete::PreContext(end)) => {
                let (context, context_byte, _, _) = line.chunk_at_byte(end - 1);
                cursor.provide_context(context, context_byte);
            }
            Err(GraphemeIncomplete::InvalidOffset) => {
                return if forward { offset + 1 } else { offset - 1 };
            }
        }
    }
}

/// The offset of the first non-blank character on the line of `offset`,
/// the line end when it has none.
pub(crate) fn first_non_blank_in(document: &Document, offset: usize) -> usize {
    let end = line_end(document, offset);
    let mut target = line_start(document, offset);
    while target < end && matches!(document.char_at(target), Some(' ' | '\t')) {
        target += 1;
    }
    target
}

/// Whether `c` is a word character, as Vim's `iskeyword` has it by
/// default: a letter or digit of any script, or `_`.
pub(crate) fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// The last line of `document`, from 0. The empty line after a final line
/// break is no line, as in Vim: `"a\nb\n"` has two lines, `""` has one.
pub(crate) fn last_line(document: &Document) -> usize {
    let last = document.len_lines() - 1;
    let ends_in_break = last > 0 && document.line_text(last).is_some_and(|text| text.is_empty());
    if ends_in_break { last - 1 } else { last }
}

/// The number of lines of `document`, the empty line after a final line
/// break not counted (see [`last_line`]).
pub(crate) fn line_count(document: &Document) -> usize {
    last_line(document) + 1
}

/// The char offset where the line holding `offset` ends, before its break.
pub(crate) fn line_end(document: &Document, offset: usize) -> usize {
    let line = line_of(document, offset);
    document
        .position_to_char(Position {
            column: usize::MAX,
            line,
        })
        .unwrap_or(offset)
}

/// The line holding `offset`, the last line for an offset past the end.
pub(crate) fn line_of(document: &Document, offset: usize) -> usize {
    document
        .char_to_position(offset.min(document.len_chars()))
        .map_or(0, |position| position.line)
}

/// The char offset where the line holding `offset` starts.
pub(crate) fn line_start(document: &Document, offset: usize) -> usize {
    start_of_line(document, line_of(document, offset))
}

/// `text` with every `\r\n` and lone `\r` turned into `\n`.
pub(crate) fn normalize_line_breaks(text: &str) -> String {
    text.replace(CRLF, LF).replace(CR, LF)
}

/// The char offset where line `line` starts, the document end for a line
/// past the last.
pub(crate) fn start_of_line(document: &Document, line: usize) -> usize {
    document
        .position_to_char(Position { column: 0, line })
        .unwrap_or_else(|| document.len_chars())
}

// ========================================================================
// Tests
// ========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn position(line: usize, column: usize) -> Position {
        Position { column, line }
    }

    #[test]
    fn test_crlf_text_is_stored_with_lf_and_saved_with_crlf() {
        let document = Document::new("one\r\ntwo\r\nthree");
        assert_eq!(document.line_ending(), LineEnding::Crlf);
        assert_eq!(document.to_string(), "one\ntwo\nthree");
        assert_eq!(document.len_lines(), 3);
        assert_eq!(document.line_text(0).as_deref(), Some("one"));
        assert_eq!(document.text_with_line_endings(), "one\r\ntwo\r\nthree");
    }

    #[test]
    fn test_the_dominant_line_ending_wins_and_lf_is_the_default() {
        assert_eq!(
            Document::new("a\r\nb\r\nc\nd").line_ending(),
            LineEnding::Crlf
        );
        assert_eq!(Document::new("a\r\nb\nc\n").line_ending(), LineEnding::Lf);
        assert_eq!(Document::new("no breaks").line_ending(), LineEnding::Lf);
        let mixed = Document::new("a\r\nb\nc\r\n");
        assert_eq!(mixed.text_with_line_endings(), "a\r\nb\r\nc\r\n");
    }

    #[test]
    fn test_a_lone_carriage_return_breaks_the_line() {
        let document = Document::new("a\rb\r\nc");
        assert_eq!(document.to_string(), "a\nb\nc");
        assert_eq!(document.len_lines(), 3);
    }

    #[test]
    fn test_lengths_count_chars_and_a_trailing_newline_opens_an_empty_line() {
        let document = Document::new("héllo\n日本語\n");
        assert_eq!(document.len_chars(), 10);
        assert_eq!(document.len_lines(), 3);
        assert_eq!(document.line_text(1).as_deref(), Some("日本語"));
        assert_eq!(document.line_text(2).as_deref(), Some(""));
        assert_eq!(document.line_text(3), None);
        assert_eq!(Document::new("").len_lines(), 1);
    }

    #[test]
    fn test_char_offsets_and_positions_convert_both_ways_over_multibyte_text() {
        let document = Document::new("héllo\n日本語\nend");
        assert_eq!(document.char_to_position(0), Some(position(0, 0)));
        assert_eq!(document.char_to_position(5), Some(position(0, 5)));
        assert_eq!(document.char_to_position(6), Some(position(1, 0)));
        assert_eq!(document.char_to_position(8), Some(position(1, 2)));
        assert_eq!(document.char_to_position(13), Some(position(2, 3)));
        assert_eq!(document.char_to_position(14), None);
        assert_eq!(document.position_to_char(position(1, 2)), Some(8));
        assert_eq!(document.position_to_char(position(2, 3)), Some(13));
        assert_eq!(document.position_to_char(position(3, 0)), None);
    }

    #[test]
    fn test_utf16_positions_count_surrogate_pairs_and_clamp_to_the_line_end() {
        let document = Document::new("a😀b\n日x\nend");
        let utf16 = |line, column| Utf16Position { column, line };
        assert_eq!(document.char_to_utf16_position(2), Some(utf16(0, 3)));
        assert_eq!(document.char_to_utf16_position(3), Some(utf16(0, 4)));
        assert_eq!(document.char_to_utf16_position(5), Some(utf16(1, 1)));
        assert_eq!(document.char_to_utf16_position(10), Some(utf16(2, 3)));
        assert_eq!(document.char_to_utf16_position(11), None);
        assert_eq!(document.utf16_position_to_char(utf16(0, 3)), Some(2));
        assert_eq!(document.utf16_position_to_char(utf16(0, 4)), Some(3));
        assert_eq!(
            document.utf16_position_to_char(utf16(0, 99)),
            Some(3),
            "past the end of the line is its end, not the next line"
        );
        assert_eq!(document.utf16_position_to_char(utf16(1, 2)), Some(6));
        assert_eq!(document.utf16_position_to_char(utf16(3, 0)), None);
    }

    #[test]
    fn test_a_column_past_the_line_end_clamps_to_it() {
        let document = Document::new("long line\nab\nlast");
        assert_eq!(document.position_to_char(position(1, 7)), Some(12));
        assert_eq!(document.position_to_char(position(2, 99)), Some(17));
        assert_eq!(document.position_to_char(position(1, usize::MAX)), Some(12));
    }

    #[test]
    fn test_grapheme_steps_never_split_a_cluster() {
        // "e" + combining acute, then a family emoji of five chars, then "x".
        let document = Document::new("e\u{301}\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}x");
        assert_eq!(document.next_grapheme_boundary(0), 2);
        assert_eq!(document.next_grapheme_boundary(1), 2);
        assert_eq!(document.next_grapheme_boundary(2), 7);
        assert_eq!(document.next_grapheme_boundary(7), 8);
        assert_eq!(document.next_grapheme_boundary(8), 8);
        assert_eq!(document.prev_grapheme_boundary(8), 7);
        assert_eq!(document.prev_grapheme_boundary(7), 2);
        assert_eq!(document.prev_grapheme_boundary(4), 2);
        assert_eq!(document.prev_grapheme_boundary(2), 0);
        assert_eq!(document.prev_grapheme_boundary(0), 0);
    }

    #[test]
    fn test_grapheme_steps_cross_line_breaks_one_char_at_a_time() {
        let document = Document::new("ab\r\ncd");
        assert_eq!(document.next_grapheme_boundary(1), 2);
        assert_eq!(document.next_grapheme_boundary(2), 3);
        assert_eq!(document.prev_grapheme_boundary(3), 2);
        assert_eq!(document.prev_grapheme_boundary(2), 1);
    }

    #[test]
    fn test_inserted_text_is_normalized_and_moves_the_lines() {
        let mut document = Document::new("one\ntwo");
        document.insert(3, " and a\r\nhalf").unwrap();
        assert_eq!(document.to_string(), "one and a\nhalf\ntwo");
        assert_eq!(document.len_lines(), 3);
        assert_eq!(document.line_text(1).as_deref(), Some("half"));
        assert_eq!(
            document.insert(99, "x"),
            Err(RangeError {
                end: 99,
                len: 18,
                start: 99,
            })
        );
    }

    #[test]
    fn test_deleting_a_range_across_a_line_break_joins_the_lines() {
        let mut document = Document::new("héllo\r\nwörld");
        document.delete(3..7).unwrap();
        assert_eq!(document.to_string(), "hélörld");
        assert_eq!(document.len_lines(), 1);
        assert_eq!(document.line_ending(), LineEnding::Crlf);
        assert_eq!(
            document.delete(Range { start: 5, end: 3 }),
            Err(RangeError {
                end: 3,
                len: 7,
                start: 5,
            })
        );
        assert_eq!(
            document.delete(5..8),
            Err(RangeError {
                end: 8,
                len: 7,
                start: 5,
            })
        );
    }

    #[test]
    fn test_grapheme_steps_hold_across_rope_chunks_on_a_long_line() {
        let cluster = "e\u{301}";
        let document = Document::new(&format!("x{}\ny", cluster.repeat(2000)));
        let end = 1 + 2 * 2000;
        let mut at = 1;
        while at < end {
            let next = document.next_grapheme_boundary(at);
            assert_eq!(next, at + 2, "a cluster at {at}");
            assert_eq!(document.prev_grapheme_boundary(next), at);
            at = next;
        }
        assert_eq!(document.next_grapheme_boundary(end), end + 1, "the break");
        assert_eq!(document.prev_grapheme_boundary(end + 1), end);
    }
}
