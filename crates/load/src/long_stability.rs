//! The long-stability scenario (BW-17-02-08): the fleet soaks — stays
//! online and walks for the whole window — while the run samples the load
//! process's own resource use and saves it with the run's numbers.
//!
//! Reference face: `server/tools/bots/` long-run configs drive a steady
//! fleet for hours and the operators watch the generator's RSS/CPU; the
//! report contract (`docs/operations/load-testing.md`, "Reports", BW-17-03-04)
//! requires the sampling period and the metric definitions to travel with
//! the numbers. The offline model composes the real [`load::Bot`] soak
//! (every bot online and circle-walking the whole window — nothing drops,
//! nothing reconnects) with the real [`crate::resource::ResourceTracker`]
//! procfs sampler, driven on a deterministic tick schedule so the sample
//! count is part of the run's fingerprint.
//!
//! The reproducibility contract is shaped by what can be reproduced: the
//! load face (the online curve, the tick count, the sampling schedule) is
//! simulated time and reproduces bit for bit; the resource series is
//! wall-clock by nature — two passes of the same seed sample the same
//! schedule but not the same procfs numbers — so the smoke's metric lines
//! carry only the deterministic face and the ledger's equality covers the
//! schedule and the series' length. The numbers themselves are saved in
//! the (non-reproducible, by design) [`ScenarioReport`](crate::report::ScenarioReport)
//! via [`LongStabilityLedger::resource_report_entries`], traceable through
//! their own metric definitions and sampling period.

use std::time::Duration;

use crate::bot::{FD_PER_BOT, TICK_PERIOD};
use crate::fleet::Fleet;
use crate::report::{ReportEntry, ReportValue};
use crate::resource::{CPU_METRIC_DEF, RSS_METRIC_DEF, ResourceSample, ResourceTracker};
use crate::scenario::Scenario;

/// The long-stability run's record: the deterministic soak face plus the
/// wall-clock resource series.
#[derive(Clone, Debug)]
pub struct LongStabilityLedger {
    /// The wall duration each tick covers (the mini-tick).
    pub sample_period: f32,
    /// The ticks the soak window ran.
    pub window_ticks: u32,
    /// The online count at each window tick, in tick order — the soak's
    /// own curve: a bot that dropped would dent it.
    pub online: Vec<u32>,
    /// The resource sampling period the tracker ran at.
    pub resource_period: Duration,
    /// The procfs samples, in schedule order.
    pub samples: Vec<ResourceSample>,
}

/// Equality covers the reproduced face only — see the module docs: the
/// resource series is wall-clock, so its numbers are not comparable
/// across passes; its schedule (the period and the sample count) is.
impl PartialEq for LongStabilityLedger {
    fn eq(&self, other: &Self) -> bool {
        self.sample_period == other.sample_period
            && self.window_ticks == other.window_ticks
            && self.online == other.online
            && self.resource_period == other.resource_period
            && self.samples.len() == other.samples.len()
    }
}

impl LongStabilityLedger {
    /// The report entries for the saved resource series: the sampling
    /// period, the metric definitions, the CPU series and the peak
    /// resident size — the BW-17-03-04 contract's names, so a saved run is
    /// traceable to what was measured and how often. The resource numbers
    /// are wall-clock by nature; they live in the report (and in
    /// [`Self::samples`]), not in the smoke's reproducible metric lines.
    pub fn resource_report_entries(&self) -> Vec<ReportEntry> {
        let peak_rss = self.peak_rss_kib();
        let series = |pick: fn(&ResourceSample) -> f64| {
            ReportValue::List(
                self.samples
                    .iter()
                    .map(|s| ReportValue::Float(pick(s)))
                    .collect(),
            )
        };
        vec![
            ReportEntry::new(
                "sampling_period_secs",
                ReportValue::Float(self.resource_period.as_secs_f64()),
            ),
            ReportEntry::new("resource_sample_count", self.samples.len() as i64),
            ReportEntry::new("cpu_user_secs_series", series(|s| s.user_cpu_secs)),
            ReportEntry::new("cpu_system_secs_series", series(|s| s.system_cpu_secs)),
            ReportEntry::new("rss_kib_series", series(|s| s.rss_kib as f64)),
            ReportEntry::new("peak_rss_kib", peak_rss as i64),
            ReportEntry::new("cpu_metric_def", CPU_METRIC_DEF),
            ReportEntry::new("rss_metric_def", RSS_METRIC_DEF),
        ]
    }

    /// The peak resident size over the series (KiB) — the soak's memory
    /// answer. A run that sampled nothing has no peak to report (`0`).
    pub fn peak_rss_kib(&self) -> u64 {
        self.samples.iter().map(|s| s.rss_kib).max().unwrap_or(0)
    }

    /// The final cumulative CPU seconds — `(user, system)`. A run that
    /// sampled nothing reports `(0.0, 0.0)`.
    pub fn final_cpu_secs(&self) -> (f64, f64) {
        match self.samples.last() {
            Some(last) => (last.user_cpu_secs, last.system_cpu_secs),
            None => (0.0, 0.0),
        }
    }

    /// The soak's holding rate: the smallest share of the fleet that was
    /// online across the window (1.0 = nobody dropped). An empty run has
    /// nothing to hold (`0.0`).
    pub fn min_online_ratio(&self) -> f64 {
        let peak = self.online.iter().copied().max().unwrap_or(0) as f64;
        if peak == 0.0 {
            return 0.0;
        }
        self.online
            .iter()
            .map(|&n| n as f64 / peak)
            .fold(1.0, f64::min)
    }
}

/// Run the long-stability scenario: bring `concurrency` bots online and
/// circle-walk them for the whole load window (the soak), sampling the
/// process's procfs face every `stride` ticks — eight samples a window,
/// whatever its length.
///
/// The fd budget is sized like the other faces' so an offline run stays
/// refusal-free; a refused admission never joins the soak.
pub fn run_long_stability(scenario: &Scenario, fd_limit: i32, seed: u64) -> LongStabilityLedger {
    let mut fleet = Fleet::with_seed("long", fd_limit, seed);
    let peak = scenario.concurrency as usize;
    let mut fd_open = 0i32;
    for _ in 0..peak {
        if fleet.add_bot() {
            fd_open += FD_PER_BOT;
            fleet.set_fd_open(fd_open);
        }
    }

    // A settled fleet: the server answers every log-on, each bot gets its
    // entity and space.
    let entity_ids: Vec<u32> = (1..=fleet.len() as u32).collect();
    for (index, bot) in fleet.bots_mut().iter_mut().enumerate() {
        bot.feed_log_on_result(entity_ids[index], 1);
    }

    let warmup_ticks = (scenario.warmup.as_secs_f32() / TICK_PERIOD) as u32;
    let window_ticks = (scenario.duration.as_secs_f32() / TICK_PERIOD) as u32;

    for _ in 0..warmup_ticks {
        for bot in fleet.bots_mut() {
            bot.tick(TICK_PERIOD);
        }
    }

    // Eight samples a window, whatever its length: the sampling schedule
    // is part of the run's fingerprint, its count deterministic even when
    // the window does not divide evenly (the eighth stride samples at the
    // last tick).
    let stride = (window_ticks / 8).max(1);
    let resource_period = Duration::from_secs_f32(TICK_PERIOD * stride as f32);
    let targets: Vec<u32> = (0..window_ticks)
        .filter(|&tick| tick % stride == 0)
        .take(8)
        .collect();
    let mut tracker = ResourceTracker::new(resource_period);

    let mut online: Vec<u32> = Vec::with_capacity(window_ticks as usize);
    for tick in 0..window_ticks {
        for bot in fleet.bots_mut() {
            bot.tick(TICK_PERIOD);
        }
        let online_now = fleet
            .bots()
            .iter()
            .filter(|bot| bot.conn_state() == crate::bot::ConnState::Online)
            .count() as u32;
        online.push(online_now);
        if targets.contains(&tick) {
            // The live procfs read; a run that cannot read its own
            // process cannot report resources — fail loudly rather than
            // save a hollow series.
            tracker.sample().expect("procfs resource sample");
        }
    }

    LongStabilityLedger {
        sample_period: TICK_PERIOD,
        window_ticks,
        online,
        resource_period,
        samples: tracker.samples().to_vec(),
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::scenario::AccountPool;

    fn scenario(concurrency: u64, duration: Duration, seed: u64) -> Scenario {
        Scenario {
            name: "long-stability".to_string(),
            fixed_seed: seed,
            account_pool: AccountPool::Generated {
                prefix: "Long".to_string(),
                count: concurrency.max(1),
                password: String::new(),
            },
            concurrency,
            ramp_rate: 0,
            warmup: Duration::from_secs(0),
            duration,
        }
    }

    /// The acceptance face: the run saves the load process's resource use
    /// — a real procfs series with a positive resident size and
    /// cumulative CPU, at the schedule's period.
    #[test]
    fn the_ledger_saves_the_process_resource_use() {
        let scenario = scenario(4, Duration::from_secs(60), 173205);
        let ledger = run_long_stability(&scenario, 500 * FD_PER_BOT, scenario.fixed_seed);
        assert_eq!(ledger.window_ticks, 600);
        // Eight samples a window, whatever its length.
        assert_eq!(ledger.samples.len(), 8);
        assert_eq!(ledger.resource_period, Duration::from_secs_f32(7.5));
        assert!(ledger.peak_rss_kib() > 0, "the process has a resident size");
        let (user, system) = ledger.final_cpu_secs();
        assert!(user >= 0.0 && system >= 0.0);
    }

    /// A fixed seed generates the same ledger: the soak face reproduces
    /// bit for bit, and the resource series' schedule (period, count) is
    /// part of the fingerprint — its wall-clock numbers are not, by the
    /// ledger's own equality contract.
    #[test]
    fn the_fixed_seed_generates_the_same_ledger() {
        let scenario = scenario(4, Duration::from_secs(60), 173205);
        let a = run_long_stability(&scenario, 500 * FD_PER_BOT, scenario.fixed_seed);
        let b = run_long_stability(&scenario, 500 * FD_PER_BOT, scenario.fixed_seed);
        assert_eq!(a, b);
        assert_eq!(a.samples.len(), b.samples.len());
        assert_eq!(a.online, b.online);
    }

    /// The soak holds: every bot stays online the whole window — the
    /// holding rate is 1.0 and no tick of the curve dents.
    #[test]
    fn the_soak_holds_the_fleet_online() {
        let scenario = scenario(4, Duration::from_secs(60), 173205);
        let ledger = run_long_stability(&scenario, 500 * FD_PER_BOT, scenario.fixed_seed);
        assert_eq!(ledger.min_online_ratio(), 1.0);
        assert!(ledger.online.iter().all(|&n| n == 4));
    }

    /// The ledger's shape follows the scenario: one curve entry per
    /// window tick, in tick order.
    #[test]
    fn the_ledger_shape_follows_the_scenario() {
        let scenario = scenario(3, Duration::from_secs(30), 99);
        let ledger = run_long_stability(&scenario, 500 * FD_PER_BOT, scenario.fixed_seed);
        assert_eq!(ledger.sample_period, TICK_PERIOD);
        assert_eq!(ledger.window_ticks, 300);
        assert_eq!(ledger.online.len(), 300);
    }

    /// A window too short to sample anything saves no series: the peak
    /// has no answer and the CPU totals stay at the floor.
    #[test]
    fn an_empty_window_saves_no_series() {
        let scenario = scenario(4, Duration::from_millis(1), 173205);
        let ledger = run_long_stability(&scenario, 500 * FD_PER_BOT, scenario.fixed_seed);
        assert_eq!(ledger.window_ticks, 0);
        assert_eq!(ledger.samples.len(), 0);
        assert_eq!(ledger.peak_rss_kib(), 0);
        assert_eq!(ledger.final_cpu_secs(), (0.0, 0.0));
        assert_eq!(ledger.min_online_ratio(), 0.0);
    }

    /// The FD budget bounds the fleet: a refused admission never joins
    /// the soak.
    #[test]
    fn a_refused_admission_shrinks_the_fleet() {
        let scenario = scenario(4, Duration::from_secs(60), 173205);
        let ledger = run_long_stability(&scenario, FD_PER_BOT, scenario.fixed_seed);
        assert!(ledger.online.iter().all(|&n| n == 1));
    }
}
