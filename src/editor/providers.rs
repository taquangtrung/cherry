//! What a host supplies a document: the clipboard, comment tokens,
//! and the providers of completions and selection ranges.

use crate::completion::{self, CompletionItem, CompletionProvider, CompletionTicket};
use crate::document::Document;
use crate::expand::SelectionRangeProvider;
use crate::register::Clipboard;

use super::{DocumentId, Editor, ViewId};

// ========================================================================
// Editor Implementations
// ========================================================================

impl Editor {
    /// Makes `clipboard` the one behind registers `+` and `*`, in place of
    /// the [`MemoryClipboard`](crate::MemoryClipboard) an editor starts with.
    pub fn set_clipboard(&mut self, clipboard: impl Clipboard + Send + 'static) {
        self.registers.set_clipboard(Box::new(clipboard));
    }

    /// Makes `token` (`"//"`, `"#"`) the line comment token `gc` uses in
    /// the document `id`; `None` leaves it with none, and `gc` then gives a
    /// notice. [`comment_token_for_extension`](crate::comment_token_for_extension)
    /// knows the token of many file extensions. Does nothing for a document
    /// the editor lacks.
    pub fn set_comment_token(&mut self, id: DocumentId, token: Option<&str>) {
        if let Some(open) = self.documents.get_mut(id) {
            open.comment_token = token.map(str::to_string);
        }
    }

    /// Makes `tokens` (an opener and a closer, `("/*", "*/")`) the block
    /// comment tokens `gc` wraps lines in in the document `id` when it has
    /// no line comment token; `None` leaves it with none.
    /// [`block_comment_tokens_for_extension`](crate::block_comment_tokens_for_extension)
    /// knows those of languages without line comments. Does nothing for a
    /// document the editor lacks.
    pub fn set_block_comment_tokens(&mut self, id: DocumentId, tokens: Option<(&str, &str)>) {
        if let Some(open) = self.documents.get_mut(id) {
            open.block_comment_tokens =
                tokens.map(|(opener, closer)| (opener.to_string(), closer.to_string()));
        }
    }

    /// Makes `providers` the sources of the completions `C-x C-o` offers in
    /// Insert mode in the document `id`, their matches shown in this order,
    /// in place of any it had; an empty list drops them. Does nothing for a
    /// document the editor lacks.
    pub fn set_completion_providers(
        &mut self,
        id: DocumentId,
        providers: Vec<Box<dyn CompletionProvider>>,
    ) {
        if let Some(open) = self.documents.get_mut(id) {
            open.completion_providers = providers;
        }
    }

    /// Makes `provider` the source of the pieces of text `S-M-<right>`
    /// grows the selection through in the document `id`, in place of the
    /// text's own words, brackets, lines, and paragraphs; `None` goes back
    /// to those. Does nothing for a document the editor lacks.
    pub fn set_selection_range_provider(
        &mut self,
        id: DocumentId,
        provider: Option<Box<dyn SelectionRangeProvider>>,
    ) {
        if let Some(open) = self.documents.get_mut(id) {
            open.selection_range_provider = provider;
        }
    }

    /// Hands `items`, a provider's late answer
    /// ([`CompletionAnswer::Later`](crate::CompletionAnswer::Later)) to the
    /// request of `ticket`, to the view that asked: they join its menu, or
    /// open one, unless the ticket is stale, when nothing happens. Repaint
    /// after.
    pub fn answer_completion(&mut self, ticket: CompletionTicket, items: Vec<CompletionItem>) {
        completion::receive_answer(self, ticket, items);
    }

    /// The block comment tokens of the document `view` shows, if set.
    pub(crate) fn block_comment_tokens_of(&self, view: ViewId) -> Option<(&str, &str)> {
        self.documents[self.views[view].document()]
            .block_comment_tokens
            .as_ref()
            .map(|(open, close)| (open.as_str(), close.as_str()))
    }

    /// The line comment token of the document `view` shows, if set.
    pub(crate) fn comment_token_of(&self, view: ViewId) -> Option<&str> {
        self.documents[self.views[view].document()]
            .comment_token
            .as_deref()
    }

    /// The completion provider of the document `view` shows, if set.
    pub(crate) fn completion_providers_of(&self, view: ViewId) -> &[Box<dyn CompletionProvider>] {
        &self.documents[self.views[view].document()].completion_providers
    }

    /// The source of the pieces the selection of `view` grows through, if
    /// the host set one.
    pub(crate) fn selection_range_provider_of(
        &self,
        view: ViewId,
    ) -> Option<&dyn SelectionRangeProvider> {
        self.documents[self.views[view].document()]
            .selection_range_provider
            .as_deref()
    }

    /// A new number for a completion asked of the providers.
    pub(crate) fn next_completion_request(&mut self) -> u64 {
        self.completion_requests += 1;
        self.completion_requests
    }

    /// The view waiting for late answers to the completion numbered
    /// `request`, if any.
    pub(crate) fn view_waiting_for(&self, request: u64) -> Option<ViewId> {
        self.views.iter().find_map(|(id, view)| {
            view.completion_wait()
                .is_some_and(|wait| wait.is_for(request))
                .then_some(id)
        })
    }

    /// The open documents other than the one `view` shows.
    pub(crate) fn other_documents(&self, view: ViewId) -> impl Iterator<Item = &Document> {
        let shown = self.views[view].document();
        self.documents
            .iter()
            .filter(move |(id, _)| *id != shown)
            .map(|(_, open)| &open.document)
    }
}
