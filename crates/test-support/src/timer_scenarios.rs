//! Controlled-clock timer scenarios (BW-18-03-05).
//!
//! The reference timers run on the wall clock, so a repeat-timer case
//! fires a different number of times on every run and cancellation tests
//! race the deadline. Here [`ManualClock`] is injected as the
//! `DispatcherTimers` `ClockFn` (`EventDispatcher::clock_`): time only
//! moves when the script advances it, so a repeat timer fires at exactly
//! the scripted stamps and a cancellation lands deterministically between
//! passes. Every scenario is a pure function of its script and replays
//! identically.

use std::cell::RefCell;
use std::rc::Rc;

use server_runtime::timers::{DispatcherTimers, TimerHandle, TimerHandlerFn};

use crate::clock::{ManualClock, NANOS_PER_MS};

/// A scripted timer run: one manual clock, one dispatcher timer queue on
/// it, and the delivery log the handlers append to.
pub struct TimerScenario {
    clock: ManualClock,
    timers: DispatcherTimers,
    deliveries: Rc<RefCell<Vec<String>>>,
}

impl Default for TimerScenario {
    fn default() -> Self {
        Self::new()
    }
}

impl TimerScenario {
    /// A fresh scenario standing at stamp zero.
    pub fn new() -> Self {
        let clock = ManualClock::new();
        let timers = DispatcherTimers::with_clock(clock.clock_fn());
        Self {
            clock,
            timers,
            deliveries: Rc::new(RefCell::new(Vec::new())),
        }
    }

    /// Add a repeating timer of `period_ms`; each fire logs `name@<ms>`.
    /// The logged stamp is when the callback ran -- the pass it fired in --
    /// which can be later than the scheduled deadline when passes are
    /// coarse, exactly what a handler reading the clock would observe.
    pub fn every(&mut self, period_ms: u64, name: &'static str) -> TimerHandle<u64> {
        self.add_timer(period_ms * 1000, true, name)
    }

    /// Add a once-off timer firing after `delay_ms`; logs `name@<ms>` at
    /// the pass that processes it.
    pub fn once_after(&mut self, delay_ms: u64, name: &'static str) -> TimerHandle<u64> {
        self.add_timer(delay_ms * 1000, false, name)
    }

    /// Shared add path: microseconds for the reference-style call,
    /// recurrence chosen by the caller.
    fn add_timer(
        &mut self,
        microseconds: u64,
        recurrent: bool,
        name: &'static str,
    ) -> TimerHandle<u64> {
        let log = Rc::clone(&self.deliveries);
        let clock = self.clock.clone();
        let handler = TimerHandlerFn::new(
            move |_handle, _user| {
                log.borrow_mut()
                    .push(format!("{name}@{}", clock.now() / NANOS_PER_MS));
            },
            name,
        );
        if recurrent {
            self.timers
                .add_timer(microseconds as i64, Rc::new(handler), 0, name)
        } else {
            self.timers
                .add_once_off_timer(microseconds as i64, Rc::new(handler), 0, name)
        }
    }

    /// Move the virtual clock and run the queue: returns how many timers
    /// fired in this pass.
    pub fn advance_ms(&mut self, milliseconds: i64) -> usize {
        self.clock.advance_ms(milliseconds);
        self.timers.process_timers()
    }

    /// The delivery log so far, in fire order.
    pub fn deliveries(&self) -> Vec<String> {
        self.deliveries.borrow().clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The acceptance: the same advance script yields the same delivery
    /// sequence, on every run of the scenario.
    #[test]
    fn repeat_timers_deliver_the_same_sequence_every_run() {
        let run = || {
            let mut scenario = TimerScenario::new();
            let _tick = scenario.every(100, "tick");
            let mut fired_per_pass = Vec::new();
            for _ in 0..12 {
                fired_per_pass.push(scenario.advance_ms(50));
            }
            (scenario.deliveries(), fired_per_pass)
        };

        let (deliveries, fired) = run();
        // Twelve 50ms passes cover 600ms: fires at 100..600, none early,
        // none late.
        assert_eq!(
            deliveries,
            [
                "tick@100", "tick@200", "tick@300", "tick@400", "tick@500", "tick@600"
            ]
        );
        assert_eq!(fired.iter().sum::<usize>(), 6);

        // The replay is bit-for-bit identical -- no wall clock involved.
        assert_eq!(run(), (deliveries, fired));
    }

    /// Cancelling a repeating timer lands exactly between passes: fires up
    /// to the cancel point, never after, while other timers run on.
    #[test]
    fn cancelling_a_repeat_timer_stops_exactly_on_the_script() {
        let run = || {
            let mut scenario = TimerScenario::new();
            let mut fast = scenario.every(50, "fast");
            let _slow = scenario.every(70, "slow");
            for step in 0..7 {
                scenario.advance_ms(50);
                if step == 3 {
                    // Cancelled at the 200ms mark, after its @200 fire.
                    fast.cancel();
                }
            }
            scenario.deliveries()
        };

        let deliveries = run();
        // Slow's 70ms deadlines land in the 50ms passes that process them
        // (70 -> pass@100, 140 -> pass@150, ...); the log records pass
        // stamps. Fast stops after its @200 fire; slow runs on.
        assert_eq!(
            deliveries,
            [
                "fast@50", "slow@100", "fast@100", "slow@150", "fast@150", "fast@200", "slow@250",
                "slow@300", "slow@350"
            ]
        );
        assert_eq!(run(), deliveries, "the cancel script replays identically");
    }

    /// A once-off cancelled before its deadline never fires, no matter how
    /// far the clock runs past it.
    #[test]
    fn a_cancelled_once_off_never_fires() {
        let mut scenario = TimerScenario::new();
        let mut doomed = scenario.once_after(120, "doomed");
        let _kept = scenario.every(100, "kept");
        doomed.cancel();

        let mut total_fired = 0;
        for _ in 0..5 {
            total_fired += scenario.advance_ms(100);
        }
        assert_eq!(total_fired, 5, "only the repeat timer fires (100..500)");
        assert_eq!(
            scenario.deliveries(),
            ["kept@100", "kept@200", "kept@300", "kept@400", "kept@500"]
        );
    }

    /// Mixed deadlines interleave in stamp order, and the heap keeps that
    /// order stable across the whole script.
    #[test]
    fn mixed_deadlines_interleave_in_stamp_order() {
        let run = || {
            let mut scenario = TimerScenario::new();
            let _every = scenario.every(100, "every");
            let _once = scenario.once_after(350, "once");
            for _ in 0..5 {
                scenario.advance_ms(100);
            }
            scenario.deliveries()
        };

        // The 350ms once-off fires in the 400ms pass that reaches it.
        assert_eq!(
            run(),
            [
                "every@100",
                "every@200",
                "every@300",
                "once@400",
                "every@400",
                "every@500"
            ]
        );
        assert_eq!(run(), run());
    }
}
