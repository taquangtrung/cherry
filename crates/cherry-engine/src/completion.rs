//! Completion in Insert mode: a menu of words or lines that could finish
//! what is being typed, kept on the view for the host to paint as a popup.
//!
//! - `C-x C-n` completes the word before the cursor from the words of the
//!   document, nearest after the cursor first (wrapping around), then the
//!   words of every other open document; `C-x C-p` the same, nearest
//!   before the cursor first.
//! - `C-x C-l` completes the whole line, from its first non-blank, from
//!   the other lines of the document, nearest above first.
//! - `C-x C-o` asks the document's [`CompletionProvider`]s, which a host
//!   sets with [`Editor::set_completion_providers`](crate::Editor::set_completion_providers)
//!   for richer sources, such as a language server or snippets.
//!
//! # Providers
//!
//! Each provider answers a [`CompletionRequest`] with its matches at once
//! ([`CompletionAnswer::Items`]) or later ([`CompletionAnswer::Later`]),
//! when the host hands them to
//! [`Editor::answer_completion`](crate::Editor::answer_completion) with
//! the request's [`CompletionTicket`]. The menu shows the matches of every
//! provider in the order the providers were given, each text once, a late
//! answer joining the menu (or opening it) as it comes. A late answer is
//! dropped once its ticket is stale: when another completion started in
//! the view, it left Insert mode, or the cursor left the completed text's
//! line. Each match may name its [`CompletionKind`] and its source, the
//! provider's when it names none.
//!
//! # Automatic completion
//!
//! With [`EditorOptions::auto_complete`](crate::EditorOptions::auto_complete),
//! typing in Insert mode opens the menu by itself, none of its matches
//! typed, once [`AUTO_MIN_CHARS`] word characters are before the cursor or
//! the char typed is one of a provider's
//! [`trigger_characters`](CompletionProvider::trigger_characters) (`.`
//! after a value): the providers' matches, then the words of the open
//! documents. `C-n` types the first, and any key but those that narrow the
//! menu closes it with nothing typed.
//!
//! The first match is typed at once in place of what was being completed,
//! and the menu opens ([`View::completion`](crate::View::completion)).
//! While it is open, `C-n` and `<down>` type the next match, `C-p` and
//! `<up>` the previous one, going through the text first typed between
//! the last match and the first; `C-g` puts that text back and closes the
//! menu. Typing a character or Backspace keeps the menu open, narrowed (or
//! widened again) to the matches first offered that start with the text
//! now between the anchor and the cursor and are longer, none of them
//! typed; when none is left, the menu closes. Any other key closes the
//! menu, keeping the match typed, and then does what it does (`<esc>`
//! leaves Insert mode). With no menu open,
//! `C-n` and `C-p` move down and up a line, as the Emacs keys of Insert
//! mode do; completion starts only from `C-x`.

use std::fmt;

use super::command::CommandContext;
use super::document::{Document, first_non_blank_in, is_word_char, line_end, line_of, line_start};
use super::editor::{Editor, ViewId};
use super::motion::{move_down, move_up};
use super::request::request_cancel;
use super::snippet::{Snippet, start_session};
use super::transaction::Edit;

// ========================================================================
// Constants
// ========================================================================

/// The word characters before the cursor automatic completion waits for:
/// fewer match too much to be worth a menu.
const AUTO_MIN_CHARS: usize = 2;

// ========================================================================
// Data Structures
// ========================================================================

/// An open completion menu: the matches, the one typed, and where the
/// completed text starts.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Completion {
    /// The char offset where the completed text starts.
    anchor: usize,
    /// Every match first offered, which typing narrows the menu from.
    candidates: Vec<CompletionItem>,
    /// The matches, in the order the menu shows them.
    items: Vec<CompletionItem>,
    /// The text typed between the anchor and the cursor before a match
    /// was, which cycling past the last match comes back to.
    original: String,
    /// The command sequence number the menu was last used at; any later
    /// command closes it.
    seq: u64,
    /// The match typed now, or `None` while the original text is back.
    selected: Option<usize>,
}

/// One match in a completion menu.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct CompletionItem {
    /// More about the match for the menu to show beside it (a type, a
    /// signature), if any.
    pub detail: Option<String>,
    /// What the match is for, in plain text (a function's doc comment),
    /// for the menu to show beside it while it is selected, if any.
    pub documentation: Option<String>,
    /// What kind of thing the match is, for the menu to show an icon or a
    /// letter by, if known.
    pub kind: Option<CompletionKind>,
    /// What the menu shows.
    pub label: String,
    /// Whether `text` is a snippet, typed without its syntax and filled in
    /// stop by stop with `<tab>`, as the docs of
    /// [`Editor::insert_snippet`](crate::Editor::insert_snippet) say.
    pub snippet: bool,
    /// Where the match came from (a language server's name, `"snippets"`),
    /// for the menu to show; the provider's
    /// [`source`](CompletionProvider::source) when the item names none.
    pub source: Option<String>,
    /// What is typed in place of the completed text.
    pub text: String,
}

/// What kind of thing a [`CompletionItem`] is, as language servers sort
/// them, for a menu's icon.
///
/// New variants may come in a minor release, so a host's `match` needs a
/// fallback arm.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[non_exhaustive]
pub enum CompletionKind {
    /// A class or struct.
    Class,
    /// A constant.
    Constant,
    /// An enum, or one of its variants.
    Enum,
    /// A field of a struct or object.
    Field,
    /// A file or directory path.
    File,
    /// A free function.
    Function,
    /// A keyword of the language.
    Keyword,
    /// A method.
    Method,
    /// A module, package, or namespace.
    Module,
    /// A snippet of text with places to fill in.
    Snippet,
    /// Plain text, such as a word of the document.
    Text,
    /// A type, trait, or interface.
    Type,
    /// A variable or parameter.
    Variable,
}

/// A provider's answer to a [`CompletionRequest`].
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum CompletionAnswer {
    /// The matches, in the order to show them; empty when there are none.
    Items(Vec<CompletionItem>),
    /// The matches come later, handed to
    /// [`Editor::answer_completion`](crate::Editor::answer_completion) with
    /// the request's ticket: for a source that answers on another thread,
    /// such as a language server.
    Later,
}

/// Which request a late [`CompletionAnswer`] answers: one provider's part
/// of one completion.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct CompletionTicket {
    /// The index of the provider asked, in the order they were given.
    provider: usize,
    /// The completion asked for, counted by the editor.
    request: u64,
}

/// A completion waiting for late answers.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CompletionWait {
    /// The char offset where the completed text starts.
    anchor: usize,
    /// The text being completed when the providers were asked.
    prefix: String,
    /// The completion the tickets are for.
    request: u64,
    /// Whether the first match is typed as the menu opens.
    select_first: bool,
}

/// What a [`CompletionProvider`] is asked to complete.
#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
pub struct CompletionRequest<'a> {
    /// The document being typed in, as it reads at the request.
    pub document: &'a Document,
    /// The char offset of the cursor.
    pub offset: usize,
    /// The text being completed, from `start` to the cursor: the word
    /// characters (letters, digits, underscores) before the cursor.
    pub prefix: &'a str,
    /// The char offset where the completed text starts, which a match
    /// replaces up to the cursor.
    pub start: usize,
    /// What a late answer to this request is handed back with.
    pub ticket: CompletionTicket,
}

/// Where the matches of a completion come from.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Source {
    /// The document's [`CompletionProvider`].
    Host,
    /// Whole lines of the document.
    Lines,
    /// Words of the open documents, nearest after (`true`) or before the
    /// cursor first.
    Words(bool),
}

// ========================================================================
// Traits
// ========================================================================

/// A host's source of completions for a document, asked by `C-x C-o` and
/// by automatic completion.
pub trait CompletionProvider: fmt::Debug + Send {
    /// The matches for `request`, now or later.
    fn complete(&self, request: &CompletionRequest<'_>) -> CompletionAnswer;

    /// The source its matches show when they name none (a language
    /// server's name); none by default.
    fn source(&self) -> Option<&str> {
        None
    }

    /// The chars that, typed, ask for completion at once with automatic
    /// completion on, however short the word before them (`.`, `:`); none
    /// by default.
    fn trigger_characters(&self) -> &[char] {
        &[]
    }
}

// ========================================================================
// Completion Implementations
// ========================================================================

impl Completion {
    /// The char offset where the completed text starts, for placing the
    /// popup (see [`Editor::screen_cell`](crate::Editor::screen_cell)).
    pub fn anchor(&self) -> usize {
        self.anchor
    }

    /// The matches, in the order to show them.
    pub fn items(&self) -> &[CompletionItem] {
        &self.items
    }

    /// The text typed between the anchor and the cursor before any match
    /// was, the menu's matches narrowed to it: for the popup to mark in
    /// each label.
    pub fn typed(&self) -> &str {
        &self.original
    }

    /// The index of the match typed now, or `None` while the text first
    /// typed is back in place.
    pub fn selected(&self) -> Option<usize> {
        self.selected
    }

    /// Whether the menu was last used before the command numbered `seq`.
    pub(crate) fn is_stale(&self, seq: u64) -> bool {
        self.seq != seq
    }
}

// ========================================================================
// CompletionWait Implementations
// ========================================================================

impl CompletionWait {
    /// Whether the wait is for the completion numbered `request`.
    pub(crate) fn is_for(&self, request: u64) -> bool {
        self.request == request
    }
}

// ========================================================================
// CompletionItem Implementations
// ========================================================================

impl CompletionItem {
    /// A match typing and showing `text`, with no detail, kind, or
    /// source.
    pub fn new(text: impl Into<String>) -> Self {
        let text = text.into();
        Self {
            detail: None,
            documentation: None,
            kind: None,
            label: text.clone(),
            snippet: false,
            source: None,
            text,
        }
    }

    /// This match of kind `kind`.
    pub fn with_kind(self, kind: CompletionKind) -> Self {
        Self {
            kind: Some(kind),
            ..self
        }
    }

    /// This match with its text a snippet (`fn ${1:name}($2)`), the label
    /// showing it as typed when the item sets none of its own.
    pub fn with_snippet(self) -> Self {
        let label = match self.label == self.text {
            true => Snippet::parse(&self.text).text,
            false => self.label,
        };
        Self {
            label,
            snippet: true,
            ..self
        }
    }

    /// This match from `source`.
    pub fn with_source(self, source: impl Into<String>) -> Self {
        Self {
            source: Some(source.into()),
            ..self
        }
    }

    /// This match with `detail` shown beside it.
    pub fn with_detail(self, detail: impl Into<String>) -> Self {
        Self {
            detail: Some(detail.into()),
            ..self
        }
    }

    /// This match with `documentation` shown beside it while selected.
    pub fn with_documentation(self, documentation: impl Into<String>) -> Self {
        Self {
            documentation: Some(documentation.into()),
            ..self
        }
    }

    /// This match showing `label` in the menu instead of its text.
    pub fn with_label(self, label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            ..self
        }
    }
}

// ========================================================================
// Functions
// ========================================================================

/// `C-g` in Insert mode: puts back the text first typed and closes the
/// menu. When none is open, in a view that never leaves Insert mode, drops
/// the selection (and the Emacs region), or else asks the host to cancel
/// a single-line field; elsewhere, nothing.
pub(crate) fn cancel_completion(cx: &mut CommandContext<'_>) {
    let Some(completion) = cx.editor.view_mut(cx.view).take_completion() else {
        if !cx.editor.is_insert_locked(cx.view) {
            return;
        }
        cx.editor.view_mut(cx.view).set_region_mark(None);
        let range = cx.editor.view_ref(cx.view).selection().primary();
        if !range.is_empty() {
            cx.editor.set_cursor(cx.view, range.head);
        } else if cx.editor.shows_single_line(cx.view) {
            request_cancel(cx.editor, cx.view);
        }
        return;
    };
    let cursor = cx.editor.view_ref(cx.view).cursor();
    let original = completion.original.clone();
    replace_completed(cx.editor, cx.view, completion.anchor, cursor, &original);
    cx.editor.view_mut(cx.view).set_snippet(None);
}

/// `C-n` in the Vim style: the next match while a menu is open, else
/// completes the word, nearest after the cursor first.
pub(crate) fn complete_or_next(cx: &mut CommandContext<'_>) {
    if !step(cx, true) {
        start(cx, Source::Words(true));
    }
}

/// `C-p` in the Vim style: the previous match while a menu is open, else
/// completes the word, nearest before the cursor first.
pub(crate) fn complete_or_previous(cx: &mut CommandContext<'_>) {
    if !step(cx, false) {
        start(cx, Source::Words(false));
    }
}

/// `C-x C-o`: completes from the document's provider.
pub(crate) fn complete_from_host(cx: &mut CommandContext<'_>) {
    start(cx, Source::Host);
}

/// `C-x C-l`: completes the whole line.
pub(crate) fn complete_line(cx: &mut CommandContext<'_>) {
    start(cx, Source::Lines);
}

/// `C-x C-n`: completes the word, nearest after the cursor first.
pub(crate) fn complete_word_next(cx: &mut CommandContext<'_>) {
    start(cx, Source::Words(true));
}

/// `C-x C-p`: completes the word, nearest before the cursor first.
pub(crate) fn complete_word_previous(cx: &mut CommandContext<'_>) {
    start(cx, Source::Words(false));
}

/// Narrows the open menu of `view` to the matches first offered that
/// start with the text from its anchor to the cursor and are longer, none
/// selected, after a character was typed or deleted; closes it when none
/// is left or the cursor left the completed text's line.
pub(crate) fn narrow_completion(editor: &mut Editor, view: ViewId) {
    let Some(mut completion) = editor.view_mut(view).take_completion() else {
        return;
    };
    let document = editor.document_of(view);
    let cursor = editor.view_ref(view).cursor();
    let same_line = line_of(document, cursor) == line_of(document, completion.anchor);
    if cursor < completion.anchor || !same_line {
        return;
    }
    let typed = document.slice(completion.anchor..cursor);
    let items: Vec<CompletionItem> = completion
        .candidates
        .iter()
        .filter(|item| item.text.len() > typed.len() && item.text.starts_with(&typed))
        .cloned()
        .collect();
    if items.is_empty() {
        return;
    }
    completion.items = items;
    completion.original = typed;
    completion.selected = None;
    completion.seq = editor.command_seq();
    editor.view_mut(view).set_completion(Some(completion));
}

/// `C-n` and `<down>` in Insert mode: the next match while a menu is
/// open, else the next newer answer in a field with a history, else down
/// a line.
pub(crate) fn next_completion_or_down(cx: &mut CommandContext<'_>) {
    if !step(cx, true) && !cx.editor.recall_field_entry(cx.view, false) {
        move_down(cx);
    }
}

/// `C-p` and `<up>` in Insert mode: the previous match while a menu is
/// open, else the next older answer in a field with a history, else up a
/// line.
pub(crate) fn previous_completion_or_up(cx: &mut CommandContext<'_>) {
    if !step(cx, false) && !cx.editor.recall_field_entry(cx.view, true) {
        move_up(cx);
    }
}

/// The lines of `document` other than `line` that start, past their
/// indent, with `prefix` and are longer, without their indent, nearest
/// above first (wrapping around), each once.
fn line_matches(document: &Document, line: usize, prefix: &str) -> Vec<String> {
    let count = document.len_lines();
    let mut matches: Vec<String> = Vec::new();
    for distance in 1..count {
        let other = (line + count - distance) % count;
        let text = document.line_text(other).unwrap_or_default();
        let trimmed = text.trim_start_matches([' ', '\t']);
        if trimmed.len() > prefix.len()
            && trimmed.starts_with(prefix)
            && !matches.iter().any(|seen| seen == trimmed)
        {
            matches.push(trimmed.to_string());
        }
    }
    matches
}

/// Types `item` in place of the chars from `anchor` to `cursor` of the
/// document of `view`, the cursor after it: a snippet without its syntax,
/// its stops then filled in with `<tab>`.
fn type_item(
    editor: &mut Editor,
    view: ViewId,
    anchor: usize,
    cursor: usize,
    item: &CompletionItem,
) {
    if !item.snippet {
        replace_completed(editor, view, anchor, cursor, &item.text);
        return editor.view_mut(view).set_snippet(None);
    }
    let snippet = Snippet::parse(&item.text);
    replace_completed(editor, view, anchor, cursor, &snippet.text);
    start_session(editor, view, anchor, snippet);
}

/// Types `text` in place of the chars from `anchor` to `cursor` of the
/// document of `view`, the cursor after it.
fn replace_completed(editor: &mut Editor, view: ViewId, anchor: usize, cursor: usize, text: &str) {
    editor.apply_edits(view, vec![Edit::new(anchor..cursor, text)]);
    editor.set_cursor(view, anchor + text.chars().count());
}

/// Opens a menu of the matches `source` gives for the text before the
/// cursor and types the first; a notice when there are none, or when the
/// document is read-only.
fn start(cx: &mut CommandContext<'_>, source: Source) {
    if cx.editor.refuse_if_read_only(cx.view) {
        return;
    }
    let document = cx.editor.document_of(cx.view);
    let cursor = cx.editor.view_ref(cx.view).cursor();
    let anchor = match source {
        Source::Lines => first_non_blank_in(document, cursor).min(cursor),
        Source::Host | Source::Words(_) => word_start(document, cursor),
    };
    let prefix = document.slice(anchor..cursor);
    let items: Vec<CompletionItem> = match source {
        Source::Host => {
            if cx.editor.completion_providers_of(cx.view).is_empty() {
                return cx.editor.error("No completion provider".to_string());
            }
            let (items, later) = ask_providers(cx.editor, cx.view, anchor, &prefix, true);
            if later && items.is_empty() {
                return;
            }
            items
        }
        Source::Lines => line_matches(document, line_of(document, cursor), &prefix)
            .into_iter()
            .map(CompletionItem::new)
            .collect(),
        Source::Words(forward) => open_words(cx.editor, cx.view, anchor, &prefix, forward),
    };
    if items.is_empty() {
        return cx.editor.error(format!("No completion for {prefix:?}"));
    }
    open_menu(cx.editor, cx.view, anchor, items, prefix, true);
}

/// After `typed` was typed in Insert mode in `view`, opens the menu with
/// none of its matches typed, as the module docs say, when automatic
/// completion is on and no menu is open.
pub(crate) fn auto_complete(editor: &mut Editor, view: ViewId, typed: char) {
    let state = editor.view_ref(view);
    if !editor.options().auto_complete || state.completion().is_some() {
        return;
    }
    let document = editor.document_of(view);
    let cursor = state.cursor();
    let anchor = word_start(document, cursor);
    let triggered = editor
        .completion_providers_of(view)
        .iter()
        .any(|provider| provider.trigger_characters().contains(&typed));
    if cursor - anchor < AUTO_MIN_CHARS && !triggered {
        return;
    }
    let prefix = document.slice(anchor..cursor);
    let (mut items, _) = ask_providers(editor, view, anchor, &prefix, false);
    if !prefix.is_empty() {
        let words = open_words(editor, view, anchor, &prefix, false);
        merge(&mut items, words, None);
    }
    if !items.is_empty() {
        open_menu(editor, view, anchor, items, prefix, false);
    }
}

/// Asks every provider of the document of `view` to complete `prefix`,
/// from `anchor` to the cursor: the matches answered at once, merged in
/// provider order, and whether any answers later, for which the view
/// waits, typing the first match when the menu opens if `select_first`.
fn ask_providers(
    editor: &mut Editor,
    view: ViewId,
    anchor: usize,
    prefix: &str,
    select_first: bool,
) -> (Vec<CompletionItem>, bool) {
    let request = editor.next_completion_request();
    let document = editor.document_of(view);
    let cursor = editor.view_ref(view).cursor();
    let mut items = Vec::new();
    let mut later = false;
    for (index, provider) in editor.completion_providers_of(view).iter().enumerate() {
        let answer = provider.complete(&CompletionRequest {
            document,
            offset: cursor,
            prefix,
            start: anchor,
            ticket: CompletionTicket {
                provider: index,
                request,
            },
        });
        match answer {
            CompletionAnswer::Items(found) => merge(&mut items, found, provider.source()),
            CompletionAnswer::Later => later = true,
        }
    }
    if later {
        let wait = CompletionWait {
            anchor,
            prefix: prefix.to_string(),
            request,
            select_first,
        };
        editor.view_mut(view).set_completion_wait(Some(wait));
    }
    (items, later)
}

/// The words of the open documents completing `prefix`, which starts at
/// `anchor` in the document of `view`: its own first, nearest after the
/// cursor first when `forward`, else nearest before, then every other
/// document's, each once.
fn open_words(
    editor: &Editor,
    view: ViewId,
    anchor: usize,
    prefix: &str,
    forward: bool,
) -> Vec<CompletionItem> {
    let mut words = word_matches(editor.document_of(view), anchor, prefix, forward);
    for other in editor.other_documents(view) {
        for word in word_matches(other, usize::MAX, prefix, true) {
            if !words.contains(&word) {
                words.push(word);
            }
        }
    }
    words.into_iter().map(CompletionItem::new).collect()
}

/// Opens a menu of `items` on the text `typed` from `anchor` to the cursor
/// of `view`, typing the first when `select_first`.
fn open_menu(
    editor: &mut Editor,
    view: ViewId,
    anchor: usize,
    items: Vec<CompletionItem>,
    typed: String,
    select_first: bool,
) {
    let selected = select_first.then_some(0);
    if select_first {
        let cursor = editor.view_ref(view).cursor();
        type_item(editor, view, anchor, cursor, &items[0]);
    }
    let completion = Completion {
        anchor,
        candidates: items.clone(),
        items,
        original: typed,
        seq: editor.command_seq(),
        selected,
    };
    editor.view_mut(view).set_completion(Some(completion));
}

/// Adds the matches of `found` whose text none of `items` has, with
/// `source` as theirs when they name none.
fn merge(items: &mut Vec<CompletionItem>, found: Vec<CompletionItem>, source: Option<&str>) {
    for mut item in found {
        if items.iter().any(|seen| seen.text == item.text) {
            continue;
        }
        if item.source.is_none() {
            item.source = source.map(str::to_string);
        }
        items.push(item);
    }
}

/// Takes `found`, a provider's late answer to `ticket`, into the menu of
/// the view waiting for it, or opens one; dropped when the ticket is stale
/// (see the module docs). The matches are those that start with the text
/// the user typed since the request (an open menu's own record of it, not
/// the match it typed) and are longer, or all of them when nothing was
/// typed since.
pub(crate) fn receive_answer(
    editor: &mut Editor,
    ticket: CompletionTicket,
    found: Vec<CompletionItem>,
) {
    let Some(view) = editor.view_waiting_for(ticket.request) else {
        return;
    };
    let Some(wait) = editor.view_ref(view).completion_wait().cloned() else {
        return;
    };
    let document = editor.document_of(view);
    let cursor = editor.view_ref(view).cursor();
    let same_line = line_of(document, cursor) == line_of(document, wait.anchor);
    if cursor < wait.anchor || !same_line {
        editor.view_mut(view).set_completion_wait(None);
        return;
    }
    let typed = document.slice(wait.anchor..cursor);
    let fits = |typed: &str, item: &CompletionItem| {
        typed == wait.prefix || (item.text.len() > typed.len() && item.text.starts_with(typed))
    };
    let source = editor
        .completion_providers_of(view)
        .get(ticket.provider)
        .and_then(|provider| provider.source())
        .map(str::to_string);
    let mut fresh = Vec::new();
    merge(&mut fresh, found, source.as_deref());
    match editor.view_mut(view).take_completion() {
        Some(mut menu) if menu.anchor == wait.anchor => {
            let new: Vec<CompletionItem> = fresh
                .into_iter()
                .filter(|item| !menu.candidates.iter().any(|seen| seen.text == item.text))
                .collect();
            let original = menu.original.clone();
            let fitting = new.iter().filter(|item| fits(&original, item)).cloned();
            menu.items.extend(fitting);
            menu.candidates.extend(new);
            menu.seq = editor.command_seq();
            editor.view_mut(view).set_completion(Some(menu));
        }
        Some(other) => editor.view_mut(view).set_completion(Some(other)),
        None => {
            let items: Vec<CompletionItem> = fresh
                .into_iter()
                .filter(|item| fits(&typed, item))
                .collect();
            if !items.is_empty() {
                open_menu(editor, view, wait.anchor, items, typed, wait.select_first);
            }
        }
    }
}

/// Types the next (or previous) match of the open menu, going through the
/// text first typed; returns whether a menu was open.
fn step(cx: &mut CommandContext<'_>, forward: bool) -> bool {
    let Some(mut completion) = cx.editor.view_mut(cx.view).take_completion() else {
        return false;
    };
    let count = completion.items.len();
    completion.selected = match (completion.selected, forward) {
        (None, true) => Some(0),
        (None, false) => Some(count - 1),
        (Some(index), true) => (index + 1 < count).then_some(index + 1),
        (Some(index), false) => index.checked_sub(1),
    };
    let cursor = cx.editor.view_ref(cx.view).cursor();
    match completion.selected {
        Some(index) => {
            let item = completion.items[index].clone();
            type_item(cx.editor, cx.view, completion.anchor, cursor, &item);
        }
        None => {
            let original = completion.original.clone();
            replace_completed(cx.editor, cx.view, completion.anchor, cursor, &original);
            cx.editor.view_mut(cx.view).set_snippet(None);
        }
    }
    completion.seq = cx.editor.command_seq();
    cx.editor.view_mut(cx.view).set_completion(Some(completion));
    true
}

/// The words of `document` that start with `prefix` and are longer, each
/// once, nearest after `from` first (wrapping around) when `forward`, else
/// nearest before it first; the word starting at `from` is left out (a
/// `from` past the end takes every word, in order).
fn word_matches(document: &Document, from: usize, prefix: &str, forward: bool) -> Vec<String> {
    let text = document.slice(0..document.len_chars());
    let mut words: Vec<(usize, &str)> = Vec::new();
    let mut start = None;
    for (offset, (byte, c)) in text.char_indices().enumerate() {
        match (start, is_word_char(c)) {
            (None, true) => start = Some((offset, byte)),
            (Some((at, from_byte)), false) => {
                words.push((at, &text[from_byte..byte]));
                start = None;
            }
            (None, false) | (Some(_), true) => {}
        }
    }
    if let Some((at, from_byte)) = start {
        words.push((at, &text[from_byte..]));
    }
    let split = words.partition_point(|(at, _)| *at < from);
    let (before, after) = words.split_at(split);
    let after = after.iter().filter(|(at, _)| *at != from);
    let ordered: Vec<&str> = if forward {
        after.chain(before).map(|(_, word)| *word).collect()
    } else {
        let after: Vec<_> = after.collect();
        before
            .iter()
            .rev()
            .chain(after.into_iter().rev())
            .map(|(_, word)| *word)
            .collect()
    };
    let mut matches: Vec<String> = Vec::new();
    for word in ordered {
        if word.len() > prefix.len()
            && word.starts_with(prefix)
            && !matches.iter().any(|seen| seen == word)
        {
            matches.push(word.to_string());
        }
    }
    matches
}

/// Where the word ending at `cursor` starts, on its line.
fn word_start(document: &Document, cursor: usize) -> usize {
    let start = line_start(document, cursor);
    let mut at = cursor.min(line_end(document, cursor));
    while at > start && document.char_at(at - 1).is_some_and(is_word_char) {
        at -= 1;
    }
    at
}

// ========================================================================
// Tests
// ========================================================================

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::options::EditorOptions;
    use crate::testing::{Harness, assert_keys};
    use crate::view::Mode;

    /// A provider that offers the prefix with `!` and then `?` after it,
    /// checking the request's prefix is the text from its start.
    #[derive(Debug)]
    struct Exclaim;

    impl CompletionProvider for Exclaim {
        fn complete(&self, request: &CompletionRequest<'_>) -> CompletionAnswer {
            let start = request.document.slice(request.start..request.offset);
            assert_eq!(start, request.prefix);
            CompletionAnswer::Items(vec![
                CompletionItem::new(format!("{}!", request.prefix)).with_detail("loud"),
                CompletionItem::new(format!("{}?", request.prefix)),
            ])
        }

        fn source(&self) -> Option<&str> {
            Some("exclaim")
        }
    }

    /// A provider that answers later, keeping each request's ticket and
    /// prefix for the test to answer with.
    #[derive(Debug, Default)]
    struct Slow(Arc<Mutex<Vec<(CompletionTicket, String)>>>);

    impl CompletionProvider for Slow {
        fn complete(&self, request: &CompletionRequest<'_>) -> CompletionAnswer {
            let asked = (request.ticket, request.prefix.to_string());
            self.0.lock().unwrap().push(asked);
            CompletionAnswer::Later
        }
    }

    /// The completion menu of the harness's view.
    fn menu(harness: &Harness) -> Option<Completion> {
        harness
            .editor
            .view(harness.view)
            .unwrap()
            .completion()
            .cloned()
    }

    #[test]
    fn test_ctrl_x_ctrl_n_types_the_first_word_and_ctrl_n_cycles_through_the_original() {
        let mut harness = Harness::new("foobar foobaz\n|fo");
        harness.keys("A C-x C-n");
        assert_eq!(harness.marked(), "foobar foobaz\nfoobar|");
        let completion = menu(&harness).expect("the menu is open");
        assert_eq!(completion.anchor(), 14);
        assert_eq!(completion.selected(), Some(0));
        let labels: Vec<_> = completion
            .items()
            .iter()
            .map(|item| item.label.as_str())
            .collect();
        assert_eq!(labels, ["foobar", "foobaz"]);
        harness.keys("C-n");
        assert_eq!(harness.marked(), "foobar foobaz\nfoobaz|");
        harness.keys("C-n");
        assert_eq!(harness.marked(), "foobar foobaz\nfo|");
        assert_eq!(menu(&harness).unwrap().selected(), None);
        harness.keys("<down>");
        assert_eq!(harness.marked(), "foobar foobaz\nfoobar|");
        harness.keys("C-p C-p");
        assert_eq!(harness.marked(), "foobar foobaz\nfoobaz|");
    }

    #[test]
    fn test_ctrl_x_ctrl_p_starts_with_the_nearest_word_before() {
        assert_keys("alpha alps\nzz |al", "A C-x C-p", "alpha alps\nzz alps|");
        assert_keys("|al alpha\nalps", "e a C-x C-n", "alpha| alpha\nalps");
    }

    #[test]
    fn test_ctrl_g_restores_the_typed_text_and_other_keys_keep_the_match() {
        assert_keys("foobar\n|fo", "A C-x C-n C-g", "foobar\nfo|");
        let mut harness = Harness::new("foobar\n|fo");
        harness.keys("A C-x C-n x");
        assert_eq!(harness.marked(), "foobar\nfoobarx|");
        assert_eq!(menu(&harness), None);
        harness.keys("C-x C-n <esc>");
        assert_eq!(harness.mode(), Mode::Normal);
        assert_eq!(menu(&harness), None);
    }

    #[test]
    fn test_ctrl_n_moves_down_without_a_menu_and_no_match_says_so() {
        assert_keys("a|b\ncd", "i C-n", "ab\nc|d");
        let mut harness = Harness::new("|xyz");
        harness.keys("i q C-x C-n");
        assert_eq!(harness.marked(), "q|xyz");
        assert_eq!(menu(&harness), None);
        assert_eq!(harness.requests().len(), 1, "a notice of no match");
    }

    #[test]
    fn test_words_of_other_open_documents_come_after_the_documents_own() {
        let mut harness = Harness::new("food\n|f");
        harness.editor.open_document("fork food fable");
        harness.keys("A C-x C-n");
        let labels: Vec<_> = menu(&harness)
            .unwrap()
            .items()
            .iter()
            .map(|item| item.label.clone())
            .collect();
        assert_eq!(labels, ["food", "fork", "fable"]);
    }

    #[test]
    fn test_ctrl_x_ctrl_l_completes_whole_lines_nearest_above() {
        assert_keys(
            "let a = 1;\nlet b = 2;\n  |l",
            "A C-x C-l",
            "let a = 1;\nlet b = 2;\n  let b = 2;|",
        );
    }

    #[test]
    fn test_ctrl_x_ctrl_o_asks_the_host_provider() {
        let mut harness = Harness::new("a\n|wo");
        let document = harness.editor.view(harness.view).unwrap().document();
        harness.keys("A C-x C-o");
        assert_eq!(harness.marked(), "a\nwo|", "no provider yet");
        harness
            .editor
            .set_completion_providers(document, vec![Box::new(Exclaim)]);
        harness.keys("C-x C-o");
        assert_eq!(harness.marked(), "a\nwo!|");
        let completion = menu(&harness).unwrap();
        assert_eq!(completion.items()[0].detail.as_deref(), Some("loud"));
        assert_eq!(completion.items()[0].source.as_deref(), Some("exclaim"));
        harness.keys("C-n");
        assert_eq!(harness.marked(), "a\nwo?|");
    }

    #[test]
    fn test_late_answers_join_the_menu_in_provider_order_and_stale_ones_are_dropped() {
        let mut harness = Harness::new("|");
        let document = harness.editor.view(harness.view).unwrap().document();
        let slow = Slow::default();
        let asked = Arc::clone(&slow.0);
        let providers: Vec<Box<dyn CompletionProvider>> = vec![Box::new(slow), Box::new(Exclaim)];
        harness.editor.set_completion_providers(document, providers);
        harness.keys("i fo C-x C-o");
        assert_eq!(harness.marked(), "fo!|", "the quick one at once");
        let (ticket, prefix) = asked.lock().unwrap()[0].clone();
        assert_eq!(prefix, "fo");
        let items = vec![
            CompletionItem::new("food").with_kind(CompletionKind::Variable),
            CompletionItem::new("fo!"),
        ];
        harness.editor.answer_completion(ticket, items);
        let labels: Vec<String> = menu(&harness)
            .unwrap()
            .items()
            .iter()
            .map(|item| item.label.clone())
            .collect();
        assert_eq!(labels, ["fo!", "fo?", "food"], "joined, each text once");
        harness.keys("<esc>");
        harness
            .editor
            .answer_completion(ticket, vec![CompletionItem::new("late")]);
        assert_eq!(
            menu(&harness),
            None,
            "out of Insert mode, the ticket is stale"
        );
    }

    #[test]
    fn test_a_late_answer_alone_opens_the_menu_narrowed_to_what_was_typed_since() {
        let mut harness = Harness::new("|");
        let document = harness.editor.view(harness.view).unwrap().document();
        let slow = Slow::default();
        let asked = Arc::clone(&slow.0);
        harness
            .editor
            .set_completion_providers(document, vec![Box::new(slow)]);
        harness.keys("i f C-x C-o");
        assert_eq!(menu(&harness), None, "nothing yet, and no notice");
        harness.keys("o");
        let (ticket, _) = asked.lock().unwrap()[0].clone();
        let items = vec![CompletionItem::new("fable"), CompletionItem::new("food")];
        harness.editor.answer_completion(ticket, items);
        assert_eq!(harness.marked(), "food|", "fable no longer fits");
    }

    /// A provider of `.` methods, triggered by `.`.
    #[derive(Debug)]
    struct Methods;

    impl CompletionProvider for Methods {
        fn complete(&self, request: &CompletionRequest<'_>) -> CompletionAnswer {
            let methods = ["len", "lines"]
                .into_iter()
                .filter(|name| name.starts_with(request.prefix))
                .map(|name| CompletionItem::new(name).with_kind(CompletionKind::Method))
                .collect();
            CompletionAnswer::Items(methods)
        }

        fn trigger_characters(&self) -> &[char] {
            &['.']
        }
    }

    #[test]
    fn test_automatic_completion_opens_with_nothing_typed_after_two_chars_or_a_trigger() {
        let options = EditorOptions {
            auto_complete: true,
            ..EditorOptions::default()
        };
        let mut harness = Harness::with_options("lemon lever\n|", options);
        let document = harness.editor.view(harness.view).unwrap().document();
        harness
            .editor
            .set_completion_providers(document, vec![Box::new(Methods)]);
        harness.keys("i l");
        assert_eq!(menu(&harness), None, "one char is not enough");
        harness.keys("e");
        let completion = menu(&harness).expect("two chars open the menu");
        let labels: Vec<&str> = completion
            .items()
            .iter()
            .map(|item| item.label.as_str())
            .collect();
        assert_eq!(
            labels,
            ["len", "lever", "lemon"],
            "the provider first, then the nearest words"
        );
        assert_eq!(completion.selected(), None);
        assert_eq!(harness.marked(), "lemon lever\nle|", "nothing typed");
        harness.keys("C-n");
        assert_eq!(harness.marked(), "lemon lever\nlen|");
        harness.keys("<esc> o s .");
        let completion = menu(&harness).expect("the trigger opens it");
        assert_eq!(completion.items()[0].kind, Some(CompletionKind::Method));
        harness.keys("<cr>");
        assert_eq!(menu(&harness), None, "Enter closes it, typing nothing");
        assert_eq!(harness.marked(), "lemon lever\nlen\ns.\n|");
    }

    /// A provider of one snippet.
    #[derive(Debug)]
    struct Functions;

    impl CompletionProvider for Functions {
        fn complete(&self, _: &CompletionRequest<'_>) -> CompletionAnswer {
            let item = CompletionItem::new("fn ${1:name}($2)").with_snippet();
            CompletionAnswer::Items(vec![item])
        }
    }

    #[test]
    fn test_a_snippet_item_types_its_text_and_tab_fills_in_its_stops() {
        let mut harness = Harness::new("|");
        let document = harness.editor.view(harness.view).unwrap().document();
        harness
            .editor
            .set_completion_providers(document, vec![Box::new(Functions)]);
        harness.keys("i f C-x C-o");
        assert_eq!(harness.marked(), "fn name()|");
        assert_eq!(menu(&harness).unwrap().items()[0].label, "fn name()");
        harness.keys("<tab> go <tab> x");
        assert_eq!(harness.marked(), "fn go(x|)");
    }

    #[test]
    fn test_the_anchor_maps_to_a_screen_cell_for_the_popup() {
        let mut harness = Harness::new("foobar\n  |fo");
        harness.keys("A C-x C-n");
        let anchor = menu(&harness).unwrap().anchor();
        assert_eq!(
            harness.editor.screen_cell(harness.view, anchor),
            Some((2, 1))
        );
    }

    #[test]
    fn test_typing_narrows_the_menu_backspace_widens_it_and_no_match_closes_it() {
        let mut harness = Harness::new("foobar foobaz fooqux\n|f");
        harness.keys("A C-x C-n C-p");
        assert_eq!(harness.marked(), "foobar foobaz fooqux\nf|");
        harness.keys("o o b");
        let labels = |harness: &Harness| -> Vec<String> {
            menu(harness).map_or_else(Vec::new, |completion| {
                completion
                    .items()
                    .iter()
                    .map(|item| item.label.clone())
                    .collect()
            })
        };
        assert_eq!(labels(&harness), ["foobar", "foobaz"]);
        assert_eq!(menu(&harness).unwrap().selected(), None);
        assert_eq!(menu(&harness).unwrap().typed(), "foob", "what narrowed it");
        harness.keys("<bs>");
        assert_eq!(labels(&harness), ["foobar", "foobaz", "fooqux"]);
        harness.keys("q C-n");
        assert_eq!(harness.marked(), "foobar foobaz fooqux\nfooqux|");
        harness.keys("C-n");
        assert_eq!(
            harness.marked(),
            "foobar foobaz fooqux\nfooq|",
            "back to the typed text"
        );
        harness.keys("z");
        assert_eq!(menu(&harness), None, "nothing starts with fooqz");
        assert_eq!(harness.marked(), "foobar foobaz fooqux\nfooqz|");
    }
}
