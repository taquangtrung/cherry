//! Decorations: marks a host lays over a document's text and gutter, such
//! as the diagnostics of a language server, the matches of a spell checker,
//! or the places a review comments on.
//!
//! # Owners
//!
//! Each set of decorations belongs to an owner, a name the host picks
//! (`"lsp"`, `"spell"`), so several sources decorate one document without
//! knowing of each other: setting an owner's decorations replaces only that
//! owner's, and clearing an owner leaves the others. See
//! [`Editor::set_decorations`](crate::Editor::set_decorations),
//! [`Editor::set_signs`](crate::Editor::set_signs),
//! [`Editor::set_line_changes`](crate::Editor::set_line_changes), and
//! [`Editor::clear_decorations`](crate::Editor::clear_decorations).
//!
//! # Through edits
//!
//! Edits carry decorations along as they carry marks. Text typed inside a
//! range widens it; text typed at either edge stays outside it; a range
//! whose text is all deleted is dropped. A sign stays on its line as lines
//! are added or removed above it, and moves to the line that takes the
//! place of its own when that is deleted. Undo and redo carry them the
//! same way, but never bring a dropped range back: a host refreshes its
//! decorations when its source (a language server) reports again.
//!
//! # Inlays
//!
//! An [`Inlay`] is text a host lays into a line that is not the
//! document's own: an inlay hint's type or parameter name, a suggestion
//! shown as ghost text, a placeholder in an empty field. It takes cells
//! before the character at its offset, wraps with the line, and moves
//! with the text, but no motion, edit, search, or yank sees it, and a
//! cell inside it maps a click to the character after it. Text typed at
//! its offset goes after it, where the cursor shows; it never goes away
//! with deleted text, as nothing of it is in the text, so a host replaces
//! inlays when their source answers again. A line not wrapped and long
//! enough to be laid out only where it is in sight shows none.
//!
//! # Conceals
//!
//! A conceal hides a range of a document's text where the host knows it
//! is markup, not content: the `**` around bold Markdown, a link's
//! `(url)`. Its chars stay in the text, so every motion, edit, search,
//! and yank sees them, but they take no cells, except on the line each
//! view's cursor is on, where they show again so the cursor can reach
//! them. Edits carry conceals along as they carry decorations, dropping
//! one whose text is all deleted.
//!
//! # Painting
//!
//! [`render_view`](crate::render_view) paints a [`DecorationKind::Highlight`]
//! range as [`Style::Highlight`](crate::Style::Highlight) under selections
//! and search matches, a [`DecorationKind::Background`] range as
//! [`Style::Background`](crate::Style::Background) under those, the
//! narrowest winning where several overlap (a range covering a line's
//! break fills the rest of the line's rows, for whole-line backgrounds),
//! and every other kind as the cell's
//! [`underline`](crate::Cell::underline), the most severe kind winning
//! where ranges overlap. An inlay paints as
//! [`Style::Inlay`](crate::Style::Inlay), with its highlight, if any, as
//! the cell's [`highlight`](crate::Cell::highlight). Signs show in the sign column, painted as
//! [`Style::Sign`](crate::Style::Sign), when
//! [`EditorOptions::sign_column`](crate::EditorOptions::sign_column) is on.

use std::collections::BTreeMap;
use std::ops::Range;

use super::document::{Document, last_line, line_of, start_of_line};
use super::highlight::HighlightId;
use super::transaction::{Assoc, ChangeSet};

// ========================================================================
// Data Structures
// ========================================================================

/// A range of a document's text marked with a kind.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct Decoration {
    /// What the range is marked as.
    pub kind: DecorationKind,
    /// The chars marked, as offsets into the document. An empty range
    /// marks the one character at its start.
    pub range: Range<usize>,
}

/// What a decoration marks, from the most severe diagnostic down; the host
/// gives each kind its own look.
///
/// New variants may come in a minor release, so a host's `match` needs a
/// fallback arm.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[non_exhaustive]
pub enum DecorationKind {
    /// Text shown on a background the host names, such as a diff's added
    /// or removed lines or a matching bracket; the host colors each id.
    Background(HighlightId),
    /// An error, such as code that does not compile.
    Error,
    /// A hint, such as a suggested rewrite.
    Hint,
    /// Text shown highlighted, such as the other uses of a name.
    Highlight,
    /// Information, such as a note on the code.
    Info,
    /// Text shown underlined, with no diagnostic meaning.
    Underline,
    /// A warning, such as code that is likely wrong.
    Warning,
}

/// Text laid into a line before the character at an offset, shown but
/// not part of the document: an inlay hint's type or parameter name, a
/// suggestion shown as ghost text, a placeholder in an empty field.
///
/// It takes cells before the character at its offset and wraps with the
/// line, and every view scrolls and takes clicks with it, a cell inside it
/// counting as the character after it. No motion, edit, search, or yank
/// sees it. Edits carry it along: text typed at its offset goes after it,
/// where the cursor shows, and deleting the text around it keeps it, so a
/// host replaces inlays when their source answers again. A line not
/// wrapped and long enough to be laid out only where it is in sight shows
/// none.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct Inlay {
    /// What the text is, for the host to color it by as it colors a
    /// syntax highlight; `None` paints it in the inlay style alone.
    pub highlight: Option<HighlightId>,
    /// The char offset of the character it shows before; the document's
    /// length for one after its last character.
    pub offset: usize,
    /// The text shown, one line: line breaks show as spaces.
    pub text: String,
}

/// A line of text a host shows after a line of the document, not part of
/// it: a suggestion of several lines shown as ghost text, a merge
/// conflict's band, an error explained under its line. It takes a screen
/// row of its own below the line's rows (with soft wrap or without), which
/// scrolling counts and screen row motions pass over; no motion, edit,
/// search, or yank sees it, and a click on it lands at the end of its
/// line. Edits carry it with its line, and it goes with its line deleted
/// whole. A line not wrapped and long enough to be laid out only where it
/// is in sight shows none.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct VirtualLine {
    /// What the text is, for the host to color it by as it colors a syntax
    /// highlight; `None` paints it in the inlay style alone.
    pub highlight: Option<HighlightId>,
    /// The line it shows after, from 0.
    pub line: usize,
    /// The text shown, one row: line breaks show as spaces, and what the
    /// view's width cannot hold is cut.
    pub text: String,
}

/// A virtual line held at the start of its line, so edits carry it along.
#[derive(Clone, Debug, Eq, PartialEq)]
struct AnchoredVirtualLine {
    /// The char offset of the start of its line.
    anchor: usize,
    /// What the text is.
    highlight: Option<HighlightId>,
    /// The text shown.
    text: String,
}

/// A lane of the gutter a host names, left of the sign column and the
/// line numbers, for marks of its own: a column of git change bars, of
/// breakpoints, of fold markers. Lanes show left to right in the order the
/// host gives them, each as wide as asked, whether or not a line has a mark
/// in it, so the text does not shift as marks come and go.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct GutterLane {
    /// The name signs give to show in it ([`Sign::in_lane`]).
    pub name: String,
    /// The cells it takes.
    pub width: u16,
}

/// What became of a line since a version the host compares the document
/// with, such as its last commit.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum LineChange {
    /// The line is new.
    Added,
    /// Lines were deleted just above it, or after it as the last line.
    Deleted,
    /// The line stands where the version's lines differed.
    Modified,
}

/// A line marked as changed, shown down the sign column's left edge
/// beside the line's sign ([`Editor::set_line_changes`](crate::Editor::set_line_changes)).
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ChangedLine {
    /// What became of it.
    pub change: LineChange,
    /// The line, from 0.
    pub line: usize,
}

/// A line change held at the start of its line, so edits carry it along.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct AnchoredChange {
    /// The char offset of the start of its line.
    anchor: usize,
    /// What became of the line.
    change: LineChange,
}

/// A mark in the sign column, or in a lane the host names, left of a line.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct Sign {
    /// What the sign marks; where several signs share a line and lane,
    /// the most severe shows.
    pub kind: DecorationKind,
    /// The [`GutterLane`] it shows in, by name; `None` for the sign column.
    /// A sign in a lane the document lacks shows nowhere.
    pub lane: Option<String>,
    /// The line it stands by, from 0.
    pub line: usize,
    /// What it means in words, for a tooltip when the pointer rests on it
    /// (a diagnostic's message); `None` for a sign its look says all of.
    pub note: Option<String>,
    /// What it shows: as many cells of text as its lane or the sign column
    /// has (`"E"`, `">>"`); more is cut.
    pub text: String,
}

/// A sign held at the start of its line, so edits carry it along.
#[derive(Clone, Debug, Eq, PartialEq)]
struct AnchoredSign {
    /// The char offset of the start of its line.
    anchor: usize,
    /// What the sign marks.
    kind: DecorationKind,
    /// The lane it shows in, or `None` for the sign column.
    lane: Option<String>,
    /// What it means in words, if anything.
    note: Option<String>,
    /// What it shows.
    text: String,
}

/// The decorations of one document, by owner.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct Decorations {
    /// Every owner's conceals, by start.
    conceals: Vec<Range<usize>>,
    /// Every owner's inlays, by offset, owners in name order where they
    /// share one.
    inlays: Vec<Inlay>,
    /// The host's gutter lanes, left to right.
    lanes: Vec<GutterLane>,
    /// Each owner's decorations, inlays, and signs.
    owners: BTreeMap<String, OwnedSet>,
}

/// The decorations and signs of one owner.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct OwnedSet {
    /// The changed lines, in the order the host gave them.
    changes: Vec<AnchoredChange>,
    /// The hidden ranges, in the order the host gave them.
    conceals: Vec<Range<usize>>,
    /// The ranges, in the order the host gave them.
    decorations: Vec<Decoration>,
    /// The inlays, in the order the host gave them.
    inlays: Vec<Inlay>,
    /// The signs, in the order the host gave them.
    signs: Vec<AnchoredSign>,
    /// The virtual lines, in the order the host gave them.
    virtual_lines: Vec<AnchoredVirtualLine>,
}

// ========================================================================
// Decoration Implementations
// ========================================================================

impl Decoration {
    /// A decoration marking `range` as `kind`.
    pub fn new(range: Range<usize>, kind: DecorationKind) -> Self {
        Self { kind, range }
    }

    /// Whether the decoration covers the char at `offset`.
    pub(crate) fn covers(&self, offset: usize) -> bool {
        self.range.contains(&offset) || (self.range.is_empty() && offset == self.range.start)
    }
}

// ========================================================================
// DecorationKind Implementations
// ========================================================================

impl DecorationKind {
    /// How severe the kind is, for the one to show where several overlap:
    /// higher wins.
    pub(crate) fn severity(self) -> u8 {
        match self {
            Self::Background(_) => 0,
            Self::Error => 5,
            Self::Highlight => 0,
            Self::Hint => 2,
            Self::Info => 3,
            Self::Underline => 1,
            Self::Warning => 4,
        }
    }
}

// ========================================================================
// Inlay Implementations
// ========================================================================

impl Inlay {
    /// An inlay showing `text` before the character at char `offset`.
    pub fn new(offset: usize, text: impl Into<String>) -> Self {
        Self {
            highlight: None,
            offset,
            text: text.into(),
        }
    }

    /// This inlay colored as `highlight`.
    pub fn with_highlight(self, highlight: HighlightId) -> Self {
        Self {
            highlight: Some(highlight),
            ..self
        }
    }
}

// ========================================================================
// Sign Implementations
// ========================================================================

impl Sign {
    /// A sign in the sign column showing `text` by line `line`, marking
    /// `kind`.
    pub fn new(line: usize, text: impl Into<String>, kind: DecorationKind) -> Self {
        Self {
            kind,
            lane: None,
            line,
            note: None,
            text: text.into(),
        }
    }

    /// This sign saying `note` in words when the pointer rests on it.
    pub fn with_note(self, note: impl Into<String>) -> Self {
        Self {
            note: Some(note.into()),
            ..self
        }
    }

    /// This sign in the gutter lane named `lane` instead.
    pub fn in_lane(self, lane: impl Into<String>) -> Self {
        Self {
            lane: Some(lane.into()),
            ..self
        }
    }
}

// ========================================================================
// VirtualLine Implementations
// ========================================================================

impl VirtualLine {
    /// A virtual line showing `text` after line `line`, in the inlay style.
    pub fn new(line: usize, text: impl Into<String>) -> Self {
        Self {
            highlight: None,
            line,
            text: text.into(),
        }
    }

    /// This virtual line colored as `highlight`.
    pub fn with_highlight(self, highlight: HighlightId) -> Self {
        Self {
            highlight: Some(highlight),
            ..self
        }
    }
}

// ========================================================================
// GutterLane Implementations
// ========================================================================

impl GutterLane {
    /// A lane named `name`, `width` cells wide.
    pub fn new(name: impl Into<String>, width: u16) -> Self {
        Self {
            name: name.into(),
            width,
        }
    }
}

// ========================================================================
// Decorations Implementations
// ========================================================================

impl Decorations {
    /// Drops every conceal, decoration, inlay, and sign of `owner`.
    pub(crate) fn clear(&mut self, owner: &str) {
        self.owners.remove(owner);
        self.merge_conceals();
        self.merge_inlays();
    }

    /// The conceals of `owner`, in the order given, as edits left them.
    pub(crate) fn conceals(&self, owner: &str) -> &[Range<usize>] {
        self.owners
            .get(owner)
            .map_or(&[], |set| set.conceals.as_slice())
    }

    /// Every owner's conceals, by start.
    pub(crate) fn conceals_by_offset(&self) -> &[Range<usize>] {
        &self.conceals
    }

    /// The decorations of `owner`, in the order given, as edits left them.
    pub(crate) fn decorations(&self, owner: &str) -> &[Decoration] {
        self.owners
            .get(owner)
            .map_or(&[], |set| set.decorations.as_slice())
    }

    /// Carries every decoration and sign across `changes`, as the module
    /// documentation describes.
    pub(crate) fn map(&mut self, changes: &ChangeSet) {
        for set in self.owners.values_mut() {
            set.conceals.retain_mut(|range| map_range(range, changes));
            set.decorations.retain_mut(|decoration| {
                let Range { start, end } = decoration.range;
                let new_start = changes.map_position(start, Assoc::After);
                let new_end = changes.map_position(end, Assoc::Before).max(new_start);
                decoration.range = new_start..new_end;
                start == end || new_start < new_end
            });
            for inlay in &mut set.inlays {
                inlay.offset = changes.map_position(inlay.offset, Assoc::Before);
            }
            for sign in &mut set.signs {
                sign.anchor = changes.map_position(sign.anchor, Assoc::After);
            }
            for changed in &mut set.changes {
                changed.anchor = changes.map_position(changed.anchor, Assoc::After);
            }
            for line in &mut set.virtual_lines {
                line.anchor = changes.map_position(line.anchor, Assoc::After);
            }
        }
        for inlay in &mut self.inlays {
            inlay.offset = changes.map_position(inlay.offset, Assoc::Before);
        }
        self.merge_conceals();
    }

    /// Makes `conceals` the ones of `owner` in `document`, in place of any
    /// it had; ranges are clamped to the text and turned forward, and an
    /// empty one is dropped.
    pub(crate) fn set_conceals(&mut self, document: &Document, owner: &str, conceals: Vec<Range<usize>>) {
        let len = document.len_chars();
        let clamped = conceals
            .into_iter()
            .map(|range| {
                let (a, b) = (range.start.min(len), range.end.min(len));
                a.min(b)..a.max(b)
            })
            .filter(|range| !range.is_empty())
            .collect();
        self.owners.entry(owner.to_string()).or_default().conceals = clamped;
        self.merge_conceals();
    }

    /// Makes `decorations` the ones of `owner` in `document`, in place of
    /// any it had; ranges are clamped to the text and turned forward.
    pub(crate) fn set_decorations(
        &mut self,
        document: &Document,
        owner: &str,
        decorations: Vec<Decoration>,
    ) {
        let len = document.len_chars();
        let clamped = decorations
            .into_iter()
            .map(|decoration| {
                let (a, b) = (
                    decoration.range.start.min(len),
                    decoration.range.end.min(len),
                );
                Decoration::new(a.min(b)..a.max(b), decoration.kind)
            })
            .collect();
        self.owners
            .entry(owner.to_string())
            .or_default()
            .decorations = clamped;
    }

    /// The inlays of `owner`, in the order given, as edits moved them.
    pub(crate) fn inlays(&self, owner: &str) -> &[Inlay] {
        self.owners
            .get(owner)
            .map_or(&[], |set| set.inlays.as_slice())
    }

    /// Every owner's inlays, by offset.
    pub(crate) fn inlays_by_offset(&self) -> &[Inlay] {
        &self.inlays
    }

    /// Makes `inlays` the ones of `owner` in `document`, in place of any
    /// it had; one past the end of the text is dropped, and line breaks in
    /// the text become spaces.
    pub(crate) fn set_inlays(&mut self, document: &Document, owner: &str, inlays: Vec<Inlay>) {
        let len = document.len_chars();
        let one_line = inlays
            .into_iter()
            .filter(|inlay| inlay.offset <= len)
            .map(|inlay| Inlay {
                text: inlay
                    .text
                    .chars()
                    .map(|c| if matches!(c, '\n' | '\r') { ' ' } else { c })
                    .collect(),
                ..inlay
            })
            .collect();
        self.owners.entry(owner.to_string()).or_default().inlays = one_line;
        self.merge_inlays();
    }

    /// Makes `signs` the ones of `owner` in `document`, in place of any it
    /// had; a line past the end stands by the last line.
    pub(crate) fn set_signs(&mut self, document: &Document, owner: &str, signs: Vec<Sign>) {
        let last_line = last_line(document);
        let anchored = signs
            .into_iter()
            .map(|sign| AnchoredSign {
                anchor: start_of_line(document, sign.line.min(last_line)),
                kind: sign.kind,
                lane: sign.lane,
                note: sign.note,
                text: sign.text,
            })
            .collect();
        self.owners.entry(owner.to_string()).or_default().signs = anchored;
    }

    /// Makes `changes` the changed lines of `owner` in `document`, in
    /// place of any it had; a line past the end stands for the last line.
    pub(crate) fn set_line_changes(
        &mut self,
        document: &Document,
        owner: &str,
        changes: Vec<ChangedLine>,
    ) {
        let last_line = last_line(document);
        let anchored = changes
            .into_iter()
            .map(|changed| AnchoredChange {
                anchor: start_of_line(document, changed.line.min(last_line)),
                change: changed.change,
            })
            .collect();
        self.owners.entry(owner.to_string()).or_default().changes = anchored;
    }

    /// The changed lines of `owner`, in the order given, on the lines edits
    /// left them on in `document`.
    pub(crate) fn line_changes(&self, document: &Document, owner: &str) -> Vec<ChangedLine> {
        self.owners.get(owner).map_or_else(Vec::new, |set| {
            set.changes
                .iter()
                .map(|changed| ChangedLine {
                    change: changed.change,
                    line: line_of(document, changed.anchor),
                })
                .collect()
        })
    }

    /// What became of each of the lines `lines` of `document`, as every
    /// owner marked them; where owners differ on a line, a modified line
    /// wins over an added one, and either over a deletion above it.
    pub(crate) fn changes_on_lines(
        &self,
        document: &Document,
        lines: Range<usize>,
    ) -> Vec<Option<LineChange>> {
        let rank = |change: LineChange| match change {
            LineChange::Deleted => 0,
            LineChange::Added => 1,
            LineChange::Modified => 2,
        };
        let mut shown: Vec<Option<LineChange>> = vec![None; lines.len()];
        for changed in self.owners.values().flat_map(|set| &set.changes) {
            let line = line_of(document, changed.anchor);
            let Some(slot) = line
                .checked_sub(lines.start)
                .and_then(|index| shown.get_mut(index))
            else {
                continue;
            };
            if slot.is_none_or(|change| rank(changed.change) > rank(change)) {
                *slot = Some(changed.change);
            }
        }
        shown
    }

    /// The signs of `owner`, in the order given, on the lines edits left
    /// them on in `document`.
    pub(crate) fn signs(&self, document: &Document, owner: &str) -> Vec<Sign> {
        self.owners.get(owner).map_or_else(Vec::new, |set| {
            set.signs
                .iter()
                .map(|sign| Sign {
                    kind: sign.kind,
                    lane: sign.lane.clone(),
                    line: line_of(document, sign.anchor),
                    note: sign.note.clone(),
                    text: sign.text.clone(),
                })
                .collect()
        })
    }

    /// What the signs by line `line` of `document` say in words, in any
    /// lane, the most severe first, joined a line apart; `None` when none
    /// there has a note.
    pub(crate) fn sign_note(&self, document: &Document, line: usize) -> Option<String> {
        let mut noted: Vec<(&AnchoredSign, &str)> = self
            .owners
            .values()
            .flat_map(|set| &set.signs)
            .filter(|sign| line_of(document, sign.anchor) == line)
            .filter_map(|sign| sign.note.as_deref().map(|note| (sign, note)))
            .collect();
        noted.sort_by_key(|(sign, _)| std::cmp::Reverse(sign.kind.severity()));
        let notes: Vec<&str> = noted.into_iter().map(|(_, note)| note).collect();
        (!notes.is_empty()).then(|| notes.join("; "))
    }

    /// Makes `lines` the virtual lines of `owner` in `document`, in place
    /// of any it had; one after a line past the end shows after the last.
    pub(crate) fn set_virtual_lines(&mut self, document: &Document, owner: &str, lines: Vec<VirtualLine>) {
        let last_line = last_line(document);
        let anchored = lines
            .into_iter()
            .map(|line| AnchoredVirtualLine {
                anchor: start_of_line(document, line.line.min(last_line)),
                highlight: line.highlight,
                text: line.text.replace('\n', " "),
            })
            .collect();
        self.owners
            .entry(owner.to_string())
            .or_default()
            .virtual_lines = anchored;
    }

    /// The virtual lines shown after line `line` of `document`, every
    /// owner's, in owner order and then the order given, as their text and
    /// highlight.
    pub(crate) fn virtual_lines_after(
        &self,
        document: &Document,
        line: usize,
    ) -> Vec<(&str, Option<HighlightId>)> {
        if self.owners.values().all(|set| set.virtual_lines.is_empty()) {
            return Vec::new();
        }
        let start = start_of_line(document, line);
        self.owners
            .values()
            .flat_map(|set| &set.virtual_lines)
            .filter(|virtual_line| virtual_line.anchor == start)
            .map(|virtual_line| (virtual_line.text.as_str(), virtual_line.highlight))
            .collect()
    }

    /// The host's gutter lanes, left to right.
    pub(crate) fn lanes(&self) -> &[GutterLane] {
        &self.lanes
    }

    /// Makes `lanes` the host's gutter lanes, left to right.
    pub(crate) fn set_lanes(&mut self, lanes: Vec<GutterLane>) {
        self.lanes = lanes;
    }

    /// The sign to show in `lane` (`None` for the sign column) by each of
    /// the lines `lines` of `document`, the most severe where several share
    /// a line, as `(text, kind)`.
    pub(crate) fn signs_on_lines(
        &self,
        document: &Document,
        lines: Range<usize>,
        lane: Option<&str>,
    ) -> Vec<Option<(&str, DecorationKind)>> {
        let mut shown: Vec<Option<(&str, DecorationKind)>> = vec![None; lines.len()];
        let signs = self
            .owners
            .values()
            .flat_map(|set| &set.signs)
            .filter(|sign| sign.lane.as_deref() == lane);
        for sign in signs {
            let line = line_of(document, sign.anchor);
            let Some(slot) = line
                .checked_sub(lines.start)
                .and_then(|index| shown.get_mut(index))
            else {
                continue;
            };
            let beats = slot.is_none_or(|(_, kind)| sign.kind.severity() > kind.severity());
            if beats {
                *slot = Some((&sign.text, sign.kind));
            }
        }
        shown
    }

    /// Every owner's decorations that touch the chars `range`.
    pub(crate) fn touching(&self, range: Range<usize>) -> Vec<&Decoration> {
        self.owners
            .values()
            .flat_map(|set| &set.decorations)
            .filter(|decoration| {
                decoration.range.start < range.end && decoration.range.end >= range.start
            })
            .collect()
    }

    /// Gathers every owner's conceals, by start.
    fn merge_conceals(&mut self) {
        self.conceals = self
            .owners
            .values()
            .flat_map(|set| set.conceals.iter().cloned())
            .collect();
        self.conceals.sort_by_key(|range| range.start);
    }

    /// Gathers every owner's inlays, by offset.
    fn merge_inlays(&mut self) {
        self.inlays = self
            .owners
            .values()
            .flat_map(|set| set.inlays.iter().cloned())
            .collect();
        self.inlays.sort_by_key(|inlay| inlay.offset);
    }
}

// ========================================================================
// Functions
// ========================================================================

/// Carries `range` across `changes`, text typed at either edge staying
/// outside it; returns whether any of its text is left.
fn map_range(range: &mut Range<usize>, changes: &ChangeSet) -> bool {
    let start = changes.map_position(range.start, Assoc::After);
    let end = changes.map_position(range.end, Assoc::Before).max(start);
    *range = start..end;
    start < end
}

// ========================================================================
// Tests
// ========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transaction::Edit;

    /// The change of `edits` on `document`, applied to it.
    fn edit(document: &mut Document, edits: Vec<Edit>) -> ChangeSet {
        let changes = ChangeSet::from_edits(document.len_chars(), edits).unwrap();
        changes.apply(document).unwrap();
        changes
    }

    fn ranges(decorations: &Decorations) -> Vec<Range<usize>> {
        decorations
            .decorations("lsp")
            .iter()
            .map(|decoration| decoration.range.clone())
            .collect()
    }

    #[test]
    fn test_ranges_shift_widen_inside_and_stay_put_at_their_edges() {
        let mut document = Document::new("ab cd ef");
        let mut decorations = Decorations::default();
        let marked = vec![
            Decoration::new(3..5, DecorationKind::Error),
            Decoration::new(6..8, DecorationKind::Warning),
        ];
        decorations.set_decorations(&document, "lsp", marked);
        let changes = edit(&mut document, vec![Edit::new(0..0, "__")]);
        decorations.map(&changes);
        assert_eq!(ranges(&decorations), [5..7, 8..10], "both shifted");
        let changes = edit(
            &mut document,
            vec![
                Edit::new(5..5, "<"),
                Edit::new(6..6, "x"),
                Edit::new(7..7, ">"),
            ],
        );
        decorations.map(&changes);
        assert_eq!(document.to_string(), "__ab <cxd> ef");
        assert_eq!(ranges(&decorations)[0], 6..9, "typed inside, not at edges");
    }

    #[test]
    fn test_a_range_whose_text_is_deleted_is_dropped_but_an_empty_one_is_kept() {
        let mut document = Document::new("one two three");
        let mut decorations = Decorations::default();
        let marked = vec![
            Decoration::new(4..7, DecorationKind::Error),
            Decoration::new(13..13, DecorationKind::Hint),
            Decoration::new(Range { start: 20, end: 1 }, DecorationKind::Info),
        ];
        decorations.set_decorations(&document, "lsp", marked);
        assert_eq!(ranges(&decorations), [4..7, 13..13, 1..13], "clamped");
        let changes = edit(&mut document, vec![Edit::new(3..7, "")]);
        decorations.map(&changes);
        assert_eq!(ranges(&decorations), [9..9, 1..9]);
    }

    #[test]
    fn test_a_line_s_sign_notes_read_the_most_severe_first_and_follow_edits() {
        let mut document = Document::new("a\nb\nc");
        let mut decorations = Decorations::default();
        let signs = vec![
            Sign::new(1, "W", DecorationKind::Warning).with_note("unused"),
            Sign::new(1, "E", DecorationKind::Error).with_note("no such name"),
            Sign::new(2, "+", DecorationKind::Info),
        ];
        decorations.set_signs(&document, "lsp", signs);
        assert_eq!(
            decorations.sign_note(&document, 1).as_deref(),
            Some("no such name; unused")
        );
        assert_eq!(
            decorations.sign_note(&document, 2),
            None,
            "a sign with no note"
        );
        let changes = edit(&mut document, vec![Edit::new(0..0, "new\n")]);
        decorations.map(&changes);
        assert!(
            decorations.sign_note(&document, 2).is_some(),
            "moved down with b"
        );
    }

    #[test]
    fn test_signs_follow_their_lines_through_edits_above_and_deletions() {
        let mut document = Document::new("a\nb\nc\nd");
        let mut decorations = Decorations::default();
        let signs = vec![
            Sign::new(1, "E", DecorationKind::Error),
            Sign::new(2, "W", DecorationKind::Warning),
            Sign::new(9, "I", DecorationKind::Info),
        ];
        decorations.set_signs(&document, "lsp", signs);
        let lines = |decorations: &Decorations, document: &Document| -> Vec<usize> {
            let signs = decorations.signs(document, "lsp");
            signs.iter().map(|sign| sign.line).collect()
        };
        assert_eq!(lines(&decorations, &document), [1, 2, 3], "9 clamped");
        let changes = edit(&mut document, vec![Edit::new(2..2, "new\n")]);
        decorations.map(&changes);
        assert_eq!(lines(&decorations, &document), [2, 3, 4], "a line above b");
        let changes = edit(&mut document, vec![Edit::new(6..8, "")]);
        decorations.map(&changes);
        assert_eq!(document.to_string(), "a\nnew\nc\nd");
        assert_eq!(
            lines(&decorations, &document),
            [2, 2, 3],
            "b went, c took it"
        );
        let shown = decorations.signs_on_lines(&document, 1..4, None);
        assert_eq!(
            shown,
            [
                None,
                Some(("E", DecorationKind::Error)),
                Some(("I", DecorationKind::Info)),
            ],
            "the error beats the warning on its line"
        );
    }

    #[test]
    fn test_line_changes_follow_their_lines_through_an_edit_above() {
        let mut document = Document::new("a\nb\nc");
        let mut decorations = Decorations::default();
        let changed = vec![ChangedLine {
            change: LineChange::Modified,
            line: 1,
        }];
        decorations.set_line_changes(&document, "git", changed);
        let changes = edit(&mut document, vec![Edit::new(0..0, "new\n")]);
        decorations.map(&changes);
        assert_eq!(
            decorations.changes_on_lines(&document, 0..4),
            [None, None, Some(LineChange::Modified), None],
            "b moved down a line"
        );
    }

    #[test]
    fn test_owners_are_set_and_cleared_apart() {
        let document = Document::new("text");
        let mut decorations = Decorations::default();
        let error = Decoration::new(0..1, DecorationKind::Error);
        let spell = Decoration::new(1..3, DecorationKind::Underline);
        decorations.set_decorations(&document, "lsp", vec![error.clone()]);
        decorations.set_decorations(&document, "spell", vec![spell.clone()]);
        assert_eq!(decorations.touching(0..4), [&error, &spell]);
        decorations.set_decorations(&document, "lsp", Vec::new());
        assert_eq!(decorations.touching(0..4), [&spell]);
        decorations.clear("spell");
        assert!(decorations.touching(0..4).is_empty());
    }

    #[test]
    fn test_an_inlay_stays_before_text_typed_at_it_and_moves_with_text_before_it() {
        let mut document = Document::new("ab");
        let mut decorations = Decorations::default();
        decorations.set_inlays(&document, "lsp", vec![Inlay::new(1, ": T")]);
        let changes = edit(&mut document, vec![Edit::new(1..1, "x")]);
        decorations.map(&changes);
        assert_eq!(decorations.inlays("lsp")[0].offset, 1, "typed after it");
        let changes = edit(&mut document, vec![Edit::new(0..0, "__")]);
        decorations.map(&changes);
        assert_eq!(decorations.inlays("lsp")[0].offset, 3);
        assert_eq!(
            decorations.inlays_by_offset()[0].offset,
            3,
            "the merged one too"
        );
    }

    #[test]
    fn test_inlays_merge_in_offset_order_one_line_each_and_inside_the_text() {
        let document = Document::new("hello");
        let mut decorations = Decorations::default();
        let given = vec![
            Inlay::new(5, "x\ny"),
            Inlay::new(1, "b"),
            Inlay::new(9, "gone"),
        ];
        decorations.set_inlays(&document, "b", given);
        decorations.set_inlays(&document, "a", vec![Inlay::new(3, "a")]);
        let merged: Vec<_> = decorations
            .inlays_by_offset()
            .iter()
            .map(|inlay| (inlay.offset, inlay.text.as_str()))
            .collect();
        assert_eq!(merged, [(1, "b"), (3, "a"), (5, "x y")]);
        decorations.clear("b");
        assert_eq!(
            decorations.inlays_by_offset().len(),
            1,
            "cleared with its owner"
        );
    }

    #[test]
    fn test_conceals_move_with_the_text_and_go_with_it() {
        let mut document = Document::new("**a** _b_");
        let mut decorations = Decorations::default();
        let given = vec![Range { start: 7, end: 6 }, 3..5, 0..2, 8..8, 20..30];
        decorations.set_conceals(&document, "md", given);
        assert_eq!(
            decorations.conceals("md"),
            [6..7, 3..5, 0..2],
            "turned, empty ones dropped"
        );
        let changes = edit(
            &mut document,
            vec![Edit::new(0..0, "x"), Edit::new(3..5, "")],
        );
        decorations.map(&changes);
        assert_eq!(document.to_string(), "x**a _b_");
        assert_eq!(
            decorations.conceals("md"),
            [5..6, 1..3],
            "the deleted one went"
        );
        assert_eq!(decorations.conceals_by_offset(), [1..3, 5..6]);
    }
}
