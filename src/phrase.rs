//! Phrases highlighted by hand, as Emacs's hi-lock does: `M-s h .` lights
//! every occurrence of the word under the cursor, or of the text a visual
//! selection on one line holds, as [`Style::Phrase`](crate::Style::Phrase)
//! with a number of its own, for the host to give each phrase its own
//! color (the numbers count up as phrases are added, wrapping); on a
//! phrase lit already, it takes the light off. `M-s h u` takes it off
//! every phrase. Each document keeps its own phrases, matched as written
//! (case counts) within each line, under a selection, a search match, and
//! a matching bracket.

use super::command::CommandContext;
use super::operator::Operand;
use super::text_object::{Reach, word_object};
use super::view::Mode;
use super::visual::selection;

// ========================================================================
// Data Structures
// ========================================================================

/// The phrases a document lights, each with its color number.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct Phrases {
    /// The number the next phrase lit takes.
    next: u8,
    /// The phrases and their numbers, in the order lit.
    lit: Vec<(String, u8)>,
}

// ========================================================================
// Phrases Implementations
// ========================================================================

impl Phrases {
    /// The phrases and their color numbers, in the order lit.
    pub(crate) fn lit(&self) -> &[(String, u8)] {
        &self.lit
    }

    /// Lights `phrase` in the next color, or takes the light off it when
    /// lit already.
    fn toggle(&mut self, phrase: String) {
        if let Some(index) = self.lit.iter().position(|(lit, _)| *lit == phrase) {
            self.lit.remove(index);
            return;
        }
        self.lit.push((phrase, self.next));
        self.next = self.next.wrapping_add(1);
    }
}

// ========================================================================
// Functions
// ========================================================================

/// `M-s h .`: lights the word under the cursor or the selected text, or
/// takes the light off it.
pub(crate) fn highlight_phrase(cx: &mut CommandContext<'_>) {
    let document = cx.editor.document_of(cx.view);
    let cursor = cx.editor.view_ref(cx.view).cursor();
    let mode = cx.editor.view_ref(cx.view).mode();
    let phrase = match (mode, selection(cx.editor, cx.view)) {
        (Mode::Visual, Some((anchor, _))) => {
            let (first, last) = (anchor.min(cursor), anchor.max(cursor));
            document.slice(first..document.next_grapheme_boundary(last))
        }
        _ => match word_object(document, cursor, 1, false, Reach::Inner) {
            Operand::Charwise(range) => document.slice(range),
            Operand::Linewise(_) => String::new(),
        },
    };
    if is_visual_mode(mode) {
        cx.editor.set_mode(cx.view, Mode::Normal);
    }
    if phrase.trim().is_empty() || phrase.contains('\n') {
        return cx.editor.error("No phrase under the cursor".to_string());
    }
    cx.editor.phrases_mut(cx.view).toggle(phrase);
}

/// `M-s h u`: takes the light off every phrase.
pub(crate) fn unhighlight_phrases(cx: &mut CommandContext<'_>) {
    *cx.editor.phrases_mut(cx.view) = Phrases::default();
}

/// Whether `mode` is a visual one, which lighting a phrase leaves.
fn is_visual_mode(mode: Mode) -> bool {
    matches!(mode, Mode::Visual | Mode::VisualBlock | Mode::VisualLine)
}

// ========================================================================
// Tests
// ========================================================================

#[cfg(test)]
mod tests {
    use crate::paint::{Style, Surface};
    use crate::render::render_view;
    use crate::testing::Harness;

    /// The styles of the first row of `harness`'s view.
    fn styles(harness: &Harness) -> Vec<Style> {
        let mut surface = Surface::new(0, 0);
        render_view(&harness.editor, harness.view, &mut surface);
        (0..surface.cols())
            .map(|column| surface.cell(column, 0).unwrap().style)
            .collect()
    }

    #[test]
    fn test_phrases_light_every_occurrence_in_a_color_of_their_own_and_toggle_off() {
        let mut harness = Harness::new("|ab cd ab cd");
        harness.editor.resize_view(harness.view, 11, 1);
        harness.keys("M-s h . w M-s h .");
        let (text, a, c) = (Style::CursorLine, Style::Phrase(0), Style::Phrase(1));
        assert_eq!(styles(&harness), [a, a, text, c, c, text, a, a, text, c, c]);
        harness.keys("M-s h .");
        assert_eq!(styles(&harness)[3], text, "lit again, the light goes");
        harness.keys("0 l v l l M-s h .");
        let lit = styles(&harness);
        assert_eq!(
            (lit[1], lit[2]),
            (a, Style::Phrase(2)),
            "the selected text, its own color"
        );
        harness.keys("M-s h u");
        assert!(styles(&harness).iter().all(|style| *style == text));
    }
}
