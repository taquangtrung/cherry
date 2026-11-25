//! The kill ring: text killed with Emacs keys, to be yanked back, kept
//! apart from Vim's registers.
//!
//! # Merging kills
//!
//! As in Emacs, kills made by consecutive commands make one entry: a kill
//! forward (`C-k`, `M-d`) appends to the newest entry, a kill backward
//! (`C-u`, `C-w`, `M-<bs>`) prepends to it, so `C-k C-k` yanks back as the
//! line and its break. Any other command in between (a motion, typed
//! text) starts a new entry. "Consecutive" is judged by the editor's
//! command sequence number, which goes up by one for every command run
//! and every character typed.
//!
//! # Yanking
//!
//! `C-y` types the entry the yank pointer is at, the newest after any kill.
//! Right after a yank, `M-y` replaces the yanked text with the next older
//! entry and moves the pointer there, wrapping from the oldest back to the
//! newest; anywhere else it does nothing and says so. The ring keeps
//! [`KILL_RING_CAPACITY`] entries, dropping the oldest.

use std::collections::VecDeque;
use std::ops::Range;

use super::command::CommandContext;
use super::transaction::Edit;

// ========================================================================
// Constants
// ========================================================================

/// The most kills the ring keeps; the oldest goes first.
const KILL_RING_CAPACITY: usize = 60;

/// The notice for a yank with nothing killed yet.
const EMPTY_RING: &str = "The kill ring is empty";

/// The notice for `M-y` not right after a yank.
const NOT_AFTER_YANK: &str = "Previous command was not a yank";

// ========================================================================
// Data Structures
// ========================================================================

/// Which way a kill went from the cursor, deciding how it merges with the
/// kill before it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum KillDirection {
    /// Text before the cursor; prepended to the newest entry.
    Backward,
    /// Text after the cursor; appended to the newest entry.
    Forward,
}

/// Text a command killed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Kill {
    /// Which way it went from the cursor.
    pub direction: KillDirection,
    /// The killed text.
    pub text: String,
}

/// Killed text, the newest last, with the yank pointer and what the last
/// kill and yank were, for merging and `M-y`.
#[derive(Clone, Debug, Default)]
pub(crate) struct KillRing {
    /// The kills, oldest first.
    kills: VecDeque<String>,
    /// The command sequence number of the last kill.
    last_kill: Option<u64>,
    /// The last yank, for `M-y` to replace.
    last_yank: Option<Yank>,
    /// How many entries back from the newest `C-y` yanks.
    pointer: usize,
}

/// Where the last yank put its text.
#[derive(Clone, Debug, Eq, PartialEq)]
struct Yank {
    /// The chars the yanked text took.
    range: Range<usize>,
    /// The command sequence number of the yank.
    seq: u64,
}

// ========================================================================
// KillRing Implementations
// ========================================================================

impl KillRing {
    /// The entry `C-y` yanks, if any.
    pub(crate) fn current(&self) -> Option<&str> {
        let index = self.kills.len().checked_sub(1 + self.pointer)?;
        self.kills.get(index).map(String::as_str)
    }

    /// Records `kill`, made by the command numbered `seq`: merged into the
    /// newest entry when the last kill was made by the command just
    /// before, else pushed as the newest, dropping the oldest when full.
    /// Empty text adds nothing but keeps a run of kills going. Moves the
    /// yank pointer to the newest entry.
    pub(crate) fn kill(&mut self, kill: Kill, seq: u64) {
        let chained = self.last_kill.is_some_and(|last| last + 1 == seq);
        self.last_kill = Some(seq);
        self.pointer = 0;
        match (chained, self.kills.back_mut()) {
            (true, Some(newest)) => match kill.direction {
                KillDirection::Backward => newest.insert_str(0, &kill.text),
                KillDirection::Forward => newest.push_str(&kill.text),
            },
            (true, None) | (false, _) => {
                if kill.text.is_empty() {
                    return;
                }
                if self.kills.len() == KILL_RING_CAPACITY {
                    self.kills.pop_front();
                }
                self.kills.push_back(kill.text);
            }
        }
    }

    /// Records that the command numbered `seq` yanked text into `range`.
    pub(crate) fn record_yank(&mut self, range: Range<usize>, seq: u64) {
        self.last_yank = Some(Yank { range, seq });
    }

    /// For `M-y` run as the command numbered `seq`: when the command just
    /// before was a yank, moves the yank pointer one entry older (from the
    /// oldest back to the newest) and returns the range that yank took
    /// with the entry to put there instead; `None` otherwise.
    pub(crate) fn rotate(&mut self, seq: u64) -> Option<(Range<usize>, String)> {
        let yank = self.last_yank.take()?;
        if yank.seq + 1 != seq || self.kills.is_empty() {
            return None;
        }
        self.pointer = (self.pointer + 1) % self.kills.len();
        let text = self.current()?.to_string();
        Some((yank.range, text))
    }
}

// ========================================================================
// Functions
// ========================================================================

/// Types the entry at the yank pointer at the cursor: the Emacs yank.
/// Refused in a read-only document, the cursor left where it is.
pub(crate) fn yank(cx: &mut CommandContext<'_>) {
    if cx.editor.refuse_if_read_only(cx.view) {
        return;
    }
    let Some(text) = cx.editor.kill_ring_mut().current().map(str::to_string) else {
        cx.editor.error(EMPTY_RING.to_string());
        return;
    };
    let cursor = cx.editor.view_ref(cx.view).cursor();
    put_yanked(cx, cursor..cursor, text);
}

/// Replaces the text just yanked with the next older kill: the Emacs
/// yank-pop. Anywhere but right after a yank, says so and does nothing;
/// refused in a read-only document, the ring left as it was.
pub(crate) fn yank_pop(cx: &mut CommandContext<'_>) {
    if cx.editor.refuse_if_read_only(cx.view) {
        return;
    }
    let seq = cx.editor.command_seq();
    let Some((range, text)) = cx.editor.kill_ring_mut().rotate(seq) else {
        cx.editor.error(NOT_AFTER_YANK.to_string());
        return;
    };
    put_yanked(cx, range, text);
}

/// Puts `text` in place of `range`, the cursor after it, and records the
/// yank for `M-y`.
fn put_yanked(cx: &mut CommandContext<'_>, range: Range<usize>, text: String) {
    let start = range.start;
    let end = start + text.chars().count();
    cx.editor.apply_edits(cx.view, vec![Edit::new(range, text)]);
    cx.editor.set_cursor(cx.view, end);
    let seq = cx.editor.command_seq();
    cx.editor.kill_ring_mut().record_yank(start..end, seq);
}

// ========================================================================
// Tests
// ========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::request::{HostRequest, NoticeRequest};
    use crate::testing::{Harness, assert_keys};

    /// The notices `harness` queued, oldest first.
    fn notices(harness: &mut Harness) -> Vec<String> {
        harness
            .requests()
            .into_iter()
            .filter_map(|request| match request {
                HostRequest::Notice(NoticeRequest { message, .. }) => Some(message),
                HostRequest::Cancel(_)
                | HostRequest::Command(_)
                | HostRequest::Open(_)
                | HostRequest::Quit(_)
                | HostRequest::ReadInto(_)
                | HostRequest::Save(_)
                | HostRequest::Submit(_) => None,
            })
            .collect()
    }

    #[test]
    fn test_c_k_kills_to_the_line_end_then_the_line_break() {
        assert_keys("ab|cd\nef", "i C-k", "ab|\nef");
        assert_keys("ab|\nef", "i C-k", "ab|ef");
        assert_keys("ab|", "i C-k", "ab|");
    }

    #[test]
    fn test_c_y_yanks_the_newest_kill_back_at_the_cursor() {
        assert_keys("a|日本\nx", "i C-k C-f C-e C-y", "a\nx日本|");
    }

    #[test]
    fn test_consecutive_kills_merge_forward_and_backward_until_another_command() {
        assert_keys("a|bc\nxy", "i C-k C-k C-y C-y", "abc\nbc\n|xy");
        assert_keys("one two three|", "i M-<bs> M-<bs> C-y", "one two three|");
        assert_keys("|one two three", "i M-d M-d C-e C-y", " threeone two|");
        let mut harness = Harness::new("a|bc\nxy");
        harness.keys("i C-k C-f C-k C-y");
        assert_eq!(harness.marked(), "a\nxy|", "C-f broke the run");
        harness.keys("M-y");
        assert_eq!(harness.marked(), "a\nbc|", "the older entry is bc alone");
    }

    #[test]
    fn test_m_y_after_a_yank_cycles_through_older_kills_and_wraps() {
        let mut harness = Harness::new("|one two three");
        harness.keys("i M-d C-f M-d C-f M-d");
        assert_eq!(harness.marked(), "  |");
        harness.keys("C-y");
        assert_eq!(harness.marked(), "  three|");
        harness.keys("M-y");
        assert_eq!(harness.marked(), "  two|");
        harness.keys("M-y");
        assert_eq!(harness.marked(), "  one|");
        harness.keys("M-y");
        assert_eq!(harness.marked(), "  three|", "wrapped to the newest");
        harness.keys("M-y C-y");
        assert_eq!(
            harness.marked(),
            "  twotwo|",
            "C-y yanks where M-y left the pointer"
        );
        harness.keys("<esc> u");
        assert_eq!(harness.marked(), "|one two three", "one undo step");
    }

    #[test]
    fn test_m_y_anywhere_but_after_a_yank_does_nothing_and_says_so() {
        let mut harness = Harness::new("|ab");
        harness.keys("i C-k C-y C-b M-y");
        assert_eq!(harness.marked(), "a|b");
        assert_eq!(notices(&mut harness), [NOT_AFTER_YANK]);
    }

    #[test]
    fn test_yanking_an_empty_ring_types_nothing_and_says_so() {
        let mut harness = Harness::new("ab|");
        harness.keys("i C-y");
        assert_eq!(harness.marked(), "ab|");
        assert_eq!(notices(&mut harness), [EMPTY_RING]);
    }

    #[test]
    fn test_the_ring_drops_its_oldest_kill_when_full_and_skips_empty_kills() {
        let mut ring = KillRing::default();
        let forward = |text: String| Kill {
            direction: KillDirection::Forward,
            text,
        };
        for kill in 0..=KILL_RING_CAPACITY {
            ring.kill(forward(kill.to_string()), 2 * kill as u64);
        }
        ring.kill(forward(String::new()), 1000);
        assert_eq!(ring.kills.len(), KILL_RING_CAPACITY);
        assert_eq!(ring.kills[0], "1");
        assert_eq!(
            ring.current(),
            Some(KILL_RING_CAPACITY.to_string().as_str())
        );
    }
}
