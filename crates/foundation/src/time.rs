//! BW-01-01-02 game time and monotonic timestamps.
//!
//! Number of game ticks per second is a configured value of the running
//! server; this module only carries the time primitives.

use crate::ids::GameTime;

/// Monotonic clock timestamp used for ordering and for the event timers.
///
/// Stored in microseconds since an unspecified process-local origin. The
/// origin is not observable across processes; only differences are.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Stamp(u64);

impl Stamp {
    /// The zero timestamp, meaning "not yet delivered".
    pub const ZERO: Self = Self(0);

    pub const fn zero() -> Self {
        Self(0)
    }

    /// Build a timestamp from raw microseconds.
    pub const fn from_micros(micros: u64) -> Self {
        Self(micros)
    }

    /// Whole seconds represented by this timestamp.
    pub const fn as_secs(&self) -> u64 {
        self.0 / 1_000_000
    }

    /// Microseconds within the current second.
    pub const fn subsec_micros(&self) -> u32 {
        (self.0 % 1_000_000) as u32
    }

    /// Saturating difference to `other`, zero when `other` is not earlier.
    pub const fn saturating_duration_since(&self, other: Self) -> Self {
        Self(self.0.saturating_sub(other.0))
    }

    pub const fn micros(&self) -> u64 {
        self.0
    }

    /// Add `micros`, saturating at the representable maximum.
    pub const fn plus_micros(self, micros: u64) -> Self {
        Self(self.0.saturating_add(micros))
    }

    /// Subtract `micros`, saturating at zero.
    pub const fn minus_micros(self, micros: u64) -> Self {
        Self(self.0.saturating_sub(micros))
    }
}

/// Fixed-rate game clock converting a monotonic timestamp into the
/// integral tick count that every role shares.
#[derive(Debug, Clone)]
pub struct GameClock {
    origin: Stamp,
    /// Whole microseconds per tick.
    micros_per_tick: u64,
    total: GameTime,
}

impl GameClock {
    /// Start a clock with `ticks_per_second` as the configured tick rate.
    ///
    /// Sub-fractional rates are rounded down, as the reference implementation
    /// derives its interval from the configured rate.
    pub fn new(origin: Stamp, ticks_per_second: f64) -> Self {
        assert!(ticks_per_second > 0.0, "tick rate must be positive");
        Self {
            origin,
            micros_per_tick: (1_000_000.0 / ticks_per_second).floor() as u64,
            total: 0,
        }
    }

    /// Ticks elapsed since the origin for `now`.
    pub fn ticks_since_origin(&self, now: Stamp) -> GameTime {
        if now < self.origin {
            0
        } else {
            let elapsed = now.saturating_duration_since(self.origin).micros();
            (elapsed / self.micros_per_tick) as GameTime
        }
    }

    /// Re-compute the running tick count, allowing the wall clock to be
    /// re-synced after a rebase.
    pub fn total(&mut self, now: Stamp) -> GameTime {
        self.total = self.ticks_since_origin(now);
        self.total
    }
}

/// The reference's `timestamp()`: monotonic nanosecond stamps with a
/// process-local origin frozen at the first call. This is the real clock
/// behind the dispatcher time queue (`realClock`); only differences are
/// meaningful, like every stamp in this crate.
pub fn stamp_now() -> u64 {
    use std::sync::OnceLock;
    use std::time::Instant;
    static EPOCH: OnceLock<Instant> = OnceLock::new();
    let epoch = *EPOCH.get_or_init(Instant::now);
    epoch.elapsed().as_nanos() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stamp(micros: u64) -> Stamp {
        // Construct via arithmetic rather than exposing the inner field.
        Stamp::ZERO.plus_micros(micros)
    }

    #[test]
    fn stamp_components_roundtrip() {
        let s = stamp(1_000_005);
        assert_eq!(s.as_secs(), 1);
        assert_eq!(s.subsec_micros(), 5);
    }

    #[test]
    fn from_micros_roundtrips() {
        assert_eq!(Stamp::from_micros(1_000_005), stamp(1_000_005));
        assert_eq!(Stamp::from_micros(0), Stamp::ZERO);
        assert_eq!(Stamp::from_micros(7).micros(), 7);
    }

    #[test]
    fn zero_matches_the_zero_constant() {
        assert_eq!(Stamp::zero(), Stamp::ZERO);
        assert_eq!(Stamp::zero().micros(), 0);
    }

    #[test]
    fn saturating_difference_never_underflows() {
        assert_eq!(stamp(10).saturating_duration_since(stamp(20)), Stamp::ZERO);
        assert_eq!(stamp(20).saturating_duration_since(stamp(10)), stamp(10));
    }

    #[test]
    fn arithmetic_saturates() {
        assert_eq!(stamp(10).minus_micros(20), Stamp::ZERO);
        assert_eq!(stamp(0).plus_micros(10), stamp(10));
    }

    #[test]
    fn clock_counts_whole_ticks() {
        let clock = GameClock::new(stamp(0), 100.0);
        assert_eq!(clock.ticks_since_origin(stamp(9_999)), 0);
        assert_eq!(clock.ticks_since_origin(stamp(10_000)), 1);
        assert_eq!(clock.ticks_since_origin(stamp(19_999)), 1);
        assert_eq!(clock.ticks_since_origin(stamp(20_000)), 2);
    }

    #[test]
    #[should_panic(expected = "tick rate must be positive")]
    fn clock_rejects_non_positive_rate() {
        GameClock::new(Stamp::ZERO, 0.0);
    }

    #[test]
    fn clock_ignores_backwards_clock() {
        let mut clock = GameClock::new(stamp(1_000_000), 200.0);
        assert_eq!(clock.total(stamp(0)), 0);
        assert_eq!(clock.total(stamp(1_000_000)), 0);
        assert_eq!(clock.total(stamp(1_005_000)), 1);
    }
}
