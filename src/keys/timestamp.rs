//! Time as the host tells it: a [`Timestamp`] is a moment on a monotonic
//! clock the host reads, counted from an origin the host picks, such as
//! when its event loop started. Nothing here reads a clock, so key timeouts
//! work wherever the host can tell time, a browser included, where
//! `std::time::Instant::now` is not available.

use std::ops::Add;
use std::time::Duration;

// ========================================================================
// Data Structures
// ========================================================================

/// A moment on the host's monotonic clock: the time since an origin the
/// host picks, the same origin for every timestamp it passes. A native
/// host keeps an `Instant` from startup and passes
/// `Timestamp::from_duration(started.elapsed())`; a browser host passes
/// `performance.now()` in milliseconds.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Timestamp(Duration);

// ========================================================================
// Timestamp Implementations
// ========================================================================

impl Timestamp {
    /// The moment `since_origin` after the host's origin.
    pub const fn from_duration(since_origin: Duration) -> Self {
        Self(since_origin)
    }

    /// The moment `millis` milliseconds after the host's origin.
    pub const fn from_millis(millis: u64) -> Self {
        Self(Duration::from_millis(millis))
    }

    /// The time from the host's origin to this moment.
    pub const fn as_duration(self) -> Duration {
        self.0
    }

    /// The time from `earlier` to this moment, or zero when `earlier` is
    /// not earlier, as when a host's clock reports two keys out of order.
    pub fn saturating_duration_since(self, earlier: Self) -> Duration {
        self.0.saturating_sub(earlier.0)
    }
}

impl Add<Duration> for Timestamp {
    type Output = Self;

    /// The moment `duration` after this one.
    fn add(self, duration: Duration) -> Self {
        Self(self.0 + duration)
    }
}

// ========================================================================
// Tests
// ========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_the_time_since_a_later_moment_is_zero_rather_than_a_panic() {
        let (earlier, later) = (Timestamp::from_millis(5), Timestamp::from_millis(9));
        assert_eq!(
            later.saturating_duration_since(earlier),
            Duration::from_millis(4)
        );
        assert_eq!(earlier.saturating_duration_since(later), Duration::ZERO);
    }
}
