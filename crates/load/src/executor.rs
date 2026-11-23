//! Executor phase model (BW-17-02-02): ramp, warmup and duration.
//!
//! An [`Executor`] holds a loaded [`Scenario`] and reports the stage times it
//! drives the run through — warmup (no load), ramp (bots added until the peak
//! is reached), and steady (peak concurrency counted down from `duration`).
//!
//! This module is the *timing* half of the executor: it computes the stage
//! schedule and the ramp curve. Driving real bots through those stages is
//! BW-17-01 (the bot runtime) and needs a live server.

use std::time::Duration;

use crate::scenario::Scenario;

/// The stage times the executor drives a run through.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StageTimeline {
    warmup: Duration,
    ramp: Duration,
    steady: Duration,
    /// Concurrent bots the ramp is driving up to.
    concurrency: u64,
    /// Bots added per second during the ramp (`0` = instant).
    ramp_rate: u64,
    /// Load seconds driven at peak (`ramp` + `steady`).
    load: Duration,
}

impl StageTimeline {
    /// Build the stage schedule for a scenario.
    pub fn for_scenario(scenario: &Scenario) -> Self {
        let warmup = scenario.warmup;
        let ramp_secs = ramp_seconds(scenario.concurrency, scenario.ramp_rate);
        let steady = scenario
            .duration
            .saturating_sub(Duration::from_secs(ramp_secs));
        Self {
            warmup,
            ramp: Duration::from_secs(ramp_seconds(scenario.concurrency, scenario.ramp_rate)),
            steady,
            concurrency: scenario.concurrency,
            ramp_rate: scenario.ramp_rate,
            load: scenario.duration,
        }
    }

    /// Time with no load before the run starts generating it.
    pub fn warmup(&self) -> Duration {
        self.warmup
    }

    /// Time to add bots from zero to the peak.
    pub fn ramp(&self) -> Duration {
        self.ramp
    }

    /// Time held at peak concurrency after the ramp.
    pub fn steady(&self) -> Duration {
        self.steady
    }

    /// Whole run: warmup plus the load window.
    pub fn total(&self) -> Duration {
        self.warmup + self.load
    }

    /// Bots active at `elapsed` seconds into the run.
    pub fn active_bots_at(&self, elapsed: Duration) -> u64 {
        if elapsed <= self.warmup {
            return 0;
        }
        let into_ramp = elapsed - self.warmup;
        if self.ramp_rate == 0 || into_ramp >= self.ramp {
            return self.concurrency;
        }
        // `ramp_seconds` uses a ceiling, so a fractional tail step is what
        // reaches the peak; floor the count for everything before the end.
        into_ramp
            .as_secs()
            .saturating_mul(self.ramp_rate)
            .min(self.concurrency)
    }
}

/// Seconds to add `concurrency` bots at `ramp_rate` per second (ceiling),
/// `0` for an instant ramp or an empty peak.
fn ramp_seconds(concurrency: u64, ramp_rate: u64) -> u64 {
    if ramp_rate == 0 || concurrency == 0 {
        return 0;
    }
    concurrency.div_ceil(ramp_rate)
}

/// The executor: a loaded scenario plus the stage times it reports.
#[derive(Debug, Clone)]
pub struct Executor {
    scenario: Scenario,
}

impl Executor {
    pub fn new(scenario: Scenario) -> Self {
        Self { scenario }
    }

    pub fn scenario(&self) -> &Scenario {
        &self.scenario
    }

    /// The executor's stage times for its scenario.
    pub fn stages(&self) -> StageTimeline {
        StageTimeline::for_scenario(&self.scenario)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scenario::AccountPool;

    fn scenario(concurrency: u64, ramp_rate: u64, warmup: u64, duration: u64) -> Scenario {
        Scenario {
            name: "unit".to_string(),
            fixed_seed: 1,
            account_pool: AccountPool::Generated {
                prefix: "Bot".to_string(),
                count: 10,
                password: String::new(),
            },
            concurrency,
            ramp_rate,
            warmup: Duration::from_secs(warmup),
            duration: Duration::from_secs(duration),
        }
    }

    #[test]
    fn a_ramped_scenario_reports_each_stage_time() {
        let stages = StageTimeline::for_scenario(&scenario(100, 10, 5, 30));
        assert_eq!(stages.warmup(), Duration::from_secs(5));
        assert_eq!(stages.ramp(), Duration::from_secs(10));
        assert_eq!(stages.steady(), Duration::from_secs(20));
        assert_eq!(stages.total(), Duration::from_secs(35));
    }

    #[test]
    fn the_stage_times_cover_the_whole_run() {
        let stages = StageTimeline::for_scenario(&scenario(250, 25, 3, 60));
        assert_eq!(
            stages.ramp() + stages.steady(),
            stages.total() - stages.warmup()
        );
        assert_eq!(stages.total(), Duration::from_secs(63));
    }

    #[test]
    fn an_instant_ramp_keeps_the_whole_load_window() {
        let stages = StageTimeline::for_scenario(&scenario(100, 0, 5, 30));
        assert_eq!(stages.ramp(), Duration::ZERO);
        assert_eq!(stages.steady(), Duration::from_secs(30));
        assert_eq!(stages.active_bots_at(Duration::from_secs(5)), 0);
        assert_eq!(stages.active_bots_at(Duration::from_secs(6)), 100);
    }

    #[test]
    fn the_ramp_curve_reaches_the_peak_at_the_ramp_end() {
        let stages = StageTimeline::for_scenario(&scenario(100, 10, 5, 30));
        assert_eq!(stages.active_bots_at(Duration::ZERO), 0);
        assert_eq!(stages.active_bots_at(Duration::from_secs(5)), 0);
        assert_eq!(stages.active_bots_at(Duration::from_secs(8)), 30);
        assert_eq!(stages.active_bots_at(Duration::from_secs(15)), 100);
        assert_eq!(stages.active_bots_at(Duration::from_secs(40)), 100);
    }

    #[test]
    fn a_ramp_longer_than_the_duration_saturates_the_steady_stage() {
        // 10 bots at 1/s needs 10s; the load window is only 5s.
        let stages = StageTimeline::for_scenario(&scenario(10, 1, 0, 5));
        assert_eq!(stages.ramp(), Duration::from_secs(10));
        assert_eq!(stages.steady(), Duration::ZERO);
        assert_eq!(stages.active_bots_at(Duration::from_secs(5)), 5);
    }

    #[test]
    fn a_non_integer_ramp_rounds_up_so_the_peak_is_reached() {
        // 10 bots at 3/s = 3.33s → the ramp is 4s.
        let stages = StageTimeline::for_scenario(&scenario(10, 3, 0, 60));
        assert_eq!(stages.ramp(), Duration::from_secs(4));
        assert_eq!(stages.active_bots_at(Duration::from_secs(4)), 10);
    }

    #[test]
    fn an_empty_peak_needs_no_ramp() {
        let stages = StageTimeline::for_scenario(&scenario(0, 10, 2, 20));
        assert_eq!(stages.ramp(), Duration::ZERO);
        assert_eq!(stages.steady(), Duration::from_secs(20));
        assert_eq!(stages.active_bots_at(Duration::from_secs(9)), 0);
    }

    #[test]
    fn the_executor_reports_its_scenario_stage_times() {
        let executor = Executor::new(scenario(100, 10, 5, 30));
        assert_eq!(executor.scenario().fixed_seed, 1);
        assert_eq!(
            executor.stages(),
            StageTimeline::for_scenario(executor.scenario())
        );
    }
}
