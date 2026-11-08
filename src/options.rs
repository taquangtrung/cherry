//! Editor options: the settings a host chooses when it creates an editor,
//! read by the view to lay text out and keep the cursor in sight, by key
//! dispatch to time out a half-typed key sequence, by indenting, and by
//! the typing helpers of Insert mode.

use std::time::Duration;

use crate::keys::PrefixTimeout;

// ========================================================================
// Constants
// ========================================================================

/// How long a half-typed key sequence waits for its next key by default.
const DEFAULT_KEY_TIMEOUT: Duration = Duration::from_millis(1000);

/// The lines a highlighter prepares each tick by default: a few screens,
/// a small part of a frame for a TextMate grammar.
const DEFAULT_HIGHLIGHT_BUDGET: usize = 2_000;

/// The rows kept between the cursor and the top or bottom edge by default.
const DEFAULT_SCROLL_MARGIN: usize = 3;

/// The cells a line reflowed by `gq` holds at most, by default.
const DEFAULT_TEXT_WIDTH: usize = 79;

/// The cells between tab stops by default.
const DEFAULT_TAB_WIDTH: usize = 4;

// ========================================================================
// Data Structures
// ========================================================================

/// Whether text a host conceals ([`Editor::set_conceals`](crate::Editor::set_conceals))
/// hides, as Vim's `conceallevel` and `concealcursor` choose.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum ConcealMode {
    /// Hidden on every line, the cursor's too.
    Hide,
    /// Hidden but on the cursor's line, which shows it to edit.
    #[default]
    RevealCursorLine,
    /// Shown everywhere, as if nothing were concealed.
    Show,
}

/// How the editor lays text out, scrolls, indents, and waits for keys.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EditorOptions {
    /// Whether a line opened by Enter, `o`, or `O` starts with the indent
    /// of the line it was opened from; a line left holding only that
    /// indent is emptied again when Insert mode is left or Enter pressed.
    pub auto_indent: bool,
    /// Whether typing in Insert mode opens the completion menu by itself,
    /// none of its matches typed, once two word characters are before the
    /// cursor or a provider's trigger character is typed: the providers'
    /// matches, then the words of the open documents.
    pub auto_complete: bool,
    /// Whether typing an opening bracket or quote in Insert mode types its
    /// closer too, typing a closer just before the same closer steps over
    /// it, and Backspace between an empty pair deletes both.
    pub auto_pairs: bool,
    /// Whether, in a view that wraps, each row a line wraps onto starts at
    /// the line's indent, as Vim's `breakindent` does, keeping at least 20
    /// cells for text.
    pub break_indent: bool,
    /// Whether `H` and `L` go to the very top and bottom rows of the view
    /// (a count's row from there), inside the scroll margin, the view left
    /// where it is until the cursor moves on; otherwise they stop at the
    /// margin, as in Vim.
    pub edge_row_motions: bool,
    /// Whether Escape typed after a count or a register (`3`, `"a`) in a
    /// visual mode or after an operator only drops them, the mode kept,
    /// so a second Escape leaves it; in Normal mode Escape always only
    /// drops them.
    pub escape_drops_prefix: bool,
    /// Whether concealed text hides, and where.
    pub conceal: ConcealMode,
    /// Whether a line opened from a line comment starts with the same
    /// indent, comment token, and space.
    pub continue_comments: bool,
    /// Whether a line opened by Enter or `o` from a list item (`- `, `1. `)
    /// starts with the next item's marker, and Enter on an item holding
    /// only its marker ends the list.
    pub continue_lists: bool,
    /// Whether indenting writes spaces only; otherwise it fills with tabs
    /// as far as tab stops allow, then spaces.
    pub expand_tab: bool,
    /// Whether typing a search pattern after `/` or `?` moves the cursor
    /// to the match it would find and paints the pattern's matches as it
    /// is typed, the cursor going back when the prompt is cancelled.
    pub incremental_search: bool,
    /// Whether a pattern typed after `/` or `?` is looked for as the text
    /// it is, so `a.b` finds only `a.b`, not a regular expression. Case
    /// still follows smart case, and `:s` and `:g` still take regular
    /// expressions.
    pub literal_search: bool,
    /// Whether each blank cell at a level of its line's indentation, a
    /// level [`shift_width`](Self::shift_width) cells wide, carries
    /// [`Layers::INDENT_GUIDE`](crate::Layers::INDENT_GUIDE), for the host
    /// to draw a faint line in; a blank line takes the levels of the
    /// shallower of the lines with text above and below it, so a block's
    /// guides run on through it.
    pub indent_guides: bool,
    /// The most lines a highlighter prepares at each
    /// [`Editor::tick`](crate::Editor::tick) (see
    /// [`Highlighter::prepare`](crate::Highlighter::prepare)): more colors a
    /// long file sooner, fewer keeps each tick short.
    pub highlight_budget: usize,
    /// How long a half-typed key sequence that times out (see
    /// [`prefix_timeout`](Self::prefix_timeout)) waits for its next key
    /// before it runs what it has or is dropped. Counted from the last key
    /// typed.
    pub key_timeout: Duration,
    /// Whether the text of every whole-word occurrence in sight of the
    /// word at the cursor, at least two characters long, carries
    /// [`Layers::OCCURRENCE`](crate::Layers::OCCURRENCE), for the host to
    /// draw a faint band under; case counts.
    pub light_occurrences: bool,
    /// Whether a gutter of line numbers is painted left of the text.
    pub line_numbers: bool,
    /// Whether the line number gutter counts lines away from the cursor's
    /// instead, as Vim's `relativenumber`: the cursor's line shows 0, or
    /// with [`line_numbers`](Self::line_numbers) on too, its own number.
    pub relative_line_numbers: bool,
    /// Whether the bracket matching the one at the cursor (in Insert mode,
    /// the one just before it, when the cursor is not at one) is painted
    /// with it, as [`Style::MatchingBracket`](crate::Style::MatchingBracket),
    /// when both are in sight.
    pub match_brackets: bool,
    /// Which half-typed key sequences time out after
    /// [`key_timeout`](Self::key_timeout): by default only an ambiguous one
    /// (bound, and the start of a longer binding), while an incomplete
    /// prefix such as the `g` of `g v` waits for its next key;
    /// [`PrefixTimeout::All`] times every one out, as Vim does.
    pub prefix_timeout: PrefixTimeout,
    /// Whether, in a view that wraps, `j` and `k` (and the arrows) move by
    /// screen rows and `gj` and `gk` by lines, the other way round from
    /// Vim. After an operator, `j` and `k` still take whole lines.
    pub screen_row_motions: bool,
    /// The rows kept visible above and below the cursor when scrolling,
    /// where the document has them; capped at half the viewport.
    pub scroll_margin: usize,
    /// The most rows pinned at the top of a view (sticky scroll): the lines
    /// opening the blocks, by indentation, that the first line in sight
    /// below them is inside, outermost first, each line's first row, shown
    /// over the text scrolled under them with
    /// [`Layers::PINNED`](crate::Layers::PINNED) on every cell; zero pins
    /// none.
    pub sticky_scroll: usize,
    /// The cells one level of indent takes, for `>` and `<`; zero takes
    /// the tab width.
    pub shift_width: usize,
    /// Whether a sign column, two cells wide, is painted left of the line
    /// numbers for the signs a host sets with
    /// [`Editor::set_signs`](crate::Editor::set_signs). Kept apart from
    /// whether any sign is set, so the text does not shift sideways as
    /// signs come and go.
    pub sign_column: bool,
    /// The cells between tab stops; a tab fills to the next stop. Zero is
    /// taken as one.
    pub tab_width: usize,
    /// Whether the unnamed register is the system clipboard (register
    /// `+`), as Vim's `clipboard=unnamedplus` makes it: a yank or delete
    /// with no register selected goes on the clipboard too, and a put with
    /// none puts the clipboard's text. The registers `0` to `9` and `-`
    /// fill as they always do.
    pub unnamed_clipboard: bool,
    /// The cells a line refilled by `gq` holds at most; zero is taken as
    /// one.
    pub text_width: usize,
    /// Whether a line wider than the viewport continues on the next row
    /// (soft wrap) instead of running off the right edge, which then
    /// scrolls horizontally to follow the cursor.
    pub wrap: bool,
}

/// What one view shows differently from the editor's [`EditorOptions`], as
/// a host toggles wrap or line numbers for one window; `None` takes the
/// editor's. Set with [`Editor::set_view_options`](crate::Editor::set_view_options);
/// start from `ViewOptions::default()` and set what differs.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub struct ViewOptions {
    /// Whether a gutter of line numbers is painted left of the text.
    pub line_numbers: Option<bool>,
    /// Whether a line wider than the view continues on the next row.
    pub wrap: Option<bool>,
}

// ========================================================================
// EditorOptions Implementations
// ========================================================================

impl EditorOptions {
    /// The cells one level of indent takes, at least one.
    pub(crate) fn indent_width(&self) -> usize {
        match self.shift_width {
            0 => self.tab_stop(),
            width => width,
        }
    }

    /// The tab width, at least one cell.
    pub(crate) fn tab_stop(&self) -> usize {
        self.tab_width.max(1)
    }
}

impl Default for EditorOptions {
    /// Auto-indent, incremental search, and matching brackets on and
    /// auto-pairs, automatic completion, break indent, comment and list continuation, edge row motions, Escape dropping a prefix first, indent guides, lit occurrences, literal search, and screen row motions off, a key timeout of one second for
    /// ambiguous sequences only,
    /// line numbers and the sign column off,
    /// a scroll margin of 3, tab stops every 4 cells, a text width of 79, indenting by the tab width with tabs,
    /// a highlight budget of 2,000 lines a tick, the unnamed register
    /// kept apart from the clipboard, and no wrap.
    fn default() -> Self {
        Self {
            auto_complete: false,
            auto_indent: true,
            auto_pairs: false,
            break_indent: false,
            conceal: ConcealMode::RevealCursorLine,
            continue_comments: false,
            continue_lists: false,
            edge_row_motions: false,
            escape_drops_prefix: false,
            expand_tab: false,
            highlight_budget: DEFAULT_HIGHLIGHT_BUDGET,
            incremental_search: true,
            indent_guides: false,
            key_timeout: DEFAULT_KEY_TIMEOUT,
            light_occurrences: false,
            line_numbers: false,
            literal_search: false,
            match_brackets: true,
            prefix_timeout: PrefixTimeout::AmbiguousOnly,
            relative_line_numbers: false,
            screen_row_motions: false,
            scroll_margin: DEFAULT_SCROLL_MARGIN,
            shift_width: 0,
            sign_column: false,
            sticky_scroll: 0,
            tab_width: DEFAULT_TAB_WIDTH,
            text_width: DEFAULT_TEXT_WIDTH,
            unnamed_clipboard: false,
            wrap: false,
        }
    }
}
