//! Scriptable virtual clock for timer-driven tests (BW-18-03-01).
//!
//! Port of the reference `ManualClock`
//! (`lib/network/unit_test/manual_clock.hpp`): time stands still until
//! advanced, so a test fires timers by moving the clock between dispatcher
//! pumps instead of sleeping in wall time. It hands out exactly the
//! nanosecond stamps `real_clock()` produces, so it can be injected as a
//! `ClockFn` anywhere a `DispatcherTimers` or tick loop is built.
//!
//! Difference from the reference: it starts at stamp zero instead of the
//! wall clock, because the acceptance is that the same advance sequence
//! yields the same ticks on every run -- a wall-clock start would break
//! replay before any scenario runs.

use std::cell::Cell;
use std::rc::Rc;

/// Nanoseconds per millisecond, the stamp unit of this tree.
const NANOS_PER_MS: u64 = 1_000_000;

/// A clock that only moves when the test moves it.
#[derive(Debug, Clone, Default)]
pub struct ManualClock {
    now: Rc<Cell<u64>>,
}

impl ManualClock {
    /// A clock standing at stamp zero.
    pub fn new() -> Self {
        Self::default()
    }

    /// A clock standing at `start_ms` milliseconds.
    pub fn starting_at_ms(start_ms: u64) -> Self {
        Self {
            now: Rc::new(Cell::new(start_ms * NANOS_PER_MS)),
        }
    }

    /// The current stamp, in the same nanosecond units `real_clock()`
    /// produces.
    pub fn now(&self) -> u64 {
        self.now.get()
    }

    /// Move the clock by `milliseconds`, negative moves included.
    pub fn advance_ms(&self, milliseconds: i64) {
        let delta = (milliseconds as i128) * (NANOS_PER_MS as i128);
        self.now.set((self.now.get() as i128 + delta) as u64);
    }

    /// Move the clock backwards. Like the reference, subtraction happens on
    /// the unsigned stamp, so rewinding past zero wraps around instead of
    /// trapping -- the reference relies on this being well defined where a
    /// negative `advanceMs` would not be.
    pub fn rewind_ms(&self, milliseconds: i64) {
        self.now.set(
            self.now
                .get()
                .wrapping_sub((milliseconds as u64) * NANOS_PER_MS),
        );
    }

    /// The `ClockFn` to hand to `DispatcherTimers`/tick loops: every read
    /// observes the clock as it stands right now.
    pub fn clock_fn(&self) -> Box<dyn Fn() -> u64> {
        let now = Rc::clone(&self.now);
        Box::new(move || now.get())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The acceptance: the identical advance sequence produces the
    /// identical tick sequence, on every construction.
    #[test]
    fn the_same_advance_sequence_yields_the_same_ticks() {
        let run = || {
            let clock = ManualClock::new();
            let mut ticks = Vec::new();
            for step in [10_i64, 20, 5, 40, 0] {
                clock.advance_ms(step);
                ticks.push(clock.now());
            }
            ticks
        };
        assert_eq!(run(), run());
    }

    /// Time stands still until advanced.
    #[test]
    fn time_stands_still_until_advanced() {
        let clock = ManualClock::new();
        assert_eq!(clock.now(), 0);
        for _ in 0..10 {
            assert_eq!(clock.now(), 0, "reads do not move the clock");
        }
    }

    /// One millisecond is one million nanosecond stamps, matching
    /// `stamp_now()`.
    #[test]
    fn milliseconds_convert_to_nanosecond_stamps() {
        let clock = ManualClock::new();
        clock.advance_ms(100);
        assert_eq!(clock.now(), 100 * NANOS_PER_MS);

        clock.rewind_ms(40);
        assert_eq!(clock.now(), 60 * NANOS_PER_MS);
    }

    /// A start offset fixes scenarios at a known wall position without
    /// consulting the real clock.
    #[test]
    fn a_clock_can_start_at_a_known_time() {
        let clock = ManualClock::starting_at_ms(1_000);
        assert_eq!(clock.now(), 1_000 * NANOS_PER_MS);
    }

    /// Rewinding past zero wraps, like the reference's unsigned
    /// subtraction.
    #[test]
    fn rewinding_past_zero_wraps_like_the_reference() {
        let clock = ManualClock::new();
        clock.rewind_ms(1);
        assert_eq!(clock.now(), u64::MAX - NANOS_PER_MS + 1);
    }

    /// The injected closure always reads the clock as it stands now, so
    /// advancing between dispatcher pumps moves timer processing forward.
    #[test]
    fn the_injected_clock_fn_tracks_advances() {
        let clock = ManualClock::new();
        let injected = clock.clock_fn();

        assert_eq!(injected(), 0);
        clock.advance_ms(250);
        assert_eq!(injected(), 250 * NANOS_PER_MS);
        clock.rewind_ms(250);
        assert_eq!(injected(), 0);

        // The closure observes the shared clock, not a snapshot.
        let also_injected = clock.clock_fn();
        clock.advance_ms(5);
        assert_eq!(also_injected(), 5 * NANOS_PER_MS);
    }
}
