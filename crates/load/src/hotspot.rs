//! The hotspot/fault baseline (OPT-05-01-01): the load process runs hot,
//! faults, and drains while it records — the allocation, recovery and
//! reclamation windows the acceptance asks to record.
//!
//! One run carries the section's three faces over the real [`load::Bot`]
//! state machine:
//!
//! - **allocation** — every bot is brought online and circle-walks the
//!   whole load window (the [`crate::steady`] movement, the
//!   [`crate::long_stability`] soak); the procfs sampler reads the
//!   process's resident size on the long-stability schedule, eight
//!   samples over the run, and the peak is the allocation answer.
//! - **recovery** — once mid-window a seeded subset of the fleet is
//!   dropped and the reconnect behaviour re-issues `logOn` (the
//!   [`crate::reconnect`] face's recovery tracks): the window the run
//!   records is how long each bot took to come back.
//! - **reclamation** — the drain window releases every session (the
//!   personality's destroy decision, `feed_logged_off(None)`: the
//!   entity and movement state clears); the online curve walks to zero
//!   and the sampling continues onto the drain tail, so the final
//!   resident size is the reclamation answer and the difference from
//!   the peak is what the run says the reclamation did.
//!
//! The reproducibility contract is the long-stability one: the online
//! curve, the drop schedule, the recovery tracks and the sample schedule
//! are simulated time and reproduce bit for bit; the RSS numbers are
//! wall-clock by nature and are excluded from equality — their schedule
//! (the period and the sample count) is part of the fingerprint. The
//! wall-clock numbers travel in the saved
//! [`ScenarioReport`](crate::report::ScenarioReport) through
//! [`HotspotBaselineLedger::resource_report_entries`], per the BW-17-03-04
//! report contract.

use std::time::Duration;

use crate::bot::{ConnState, FD_PER_BOT, TICK_PERIOD};
use crate::fleet::Fleet;
use crate::reconnect::{RECONNECT_KEEP_ALIVE, RecoveryTrack};

/// The reconnect face's answer latency, in ticks: the reply is queued
/// one tick after the attempt is issued, so the recovery is the drop
/// tick plus this latency (`client_app.cpp`'s offline floor).
const RELOGIN_LATENCY_TICKS: u32 = 1;
use crate::report::{ReportEntry, ReportValue};
use crate::resource::{CPU_METRIC_DEF, RSS_METRIC_DEF, ResourceSample, ResourceTracker};
use crate::scenario::Scenario;

/// The drain floor: the minimum tail ticks a release gets, so a short
/// scenario still shows the curve reaching zero before the ledger is
/// written. Every drop lands within one update, so a few ticks drain a
/// whole fleet; the floor keeps the shape honest on tiny scenarios.
const DRAIN_FLOOR_TICKS: u32 = 4;

/// The hotspot/fault baseline run's record: the load, recovery and
/// reclamation windows over one fleet.
#[derive(Clone, Debug)]
pub struct HotspotBaselineLedger {
    /// The wall duration each tick covers (the mini-tick).
    pub sample_period: f32,
    /// The load window ticks (the fleet online and faulted).
    pub window_ticks: u32,
    /// The drain window ticks (the fleet released).
    pub drain_ticks: u32,
    /// Online count per tick across load + drain, in tick order — the
    /// curve the windows read off: it rises to the fleet size, dents at
    /// each fault drop, recovers, and ends at zero.
    pub online: Vec<u32>,
    /// The recovery tracks (the reconnect face's shape) for every bot
    /// the fault window dropped.
    pub recovery: Vec<RecoveryTrack>,
    /// The resource sampling period the tracker ran at.
    pub resource_period: Duration,
    /// The procfs samples, in schedule order, spanning the load and
    /// drain windows.
    pub samples: Vec<ResourceSample>,
}

/// Equality covers the reproduced face only — see the module docs: the
/// resource series is wall-clock, so its numbers are not comparable
/// across passes; its schedule (the period and the sample count) is.
impl PartialEq for HotspotBaselineLedger {
    fn eq(&self, other: &Self) -> bool {
        self.sample_period == other.sample_period
            && self.window_ticks == other.window_ticks
            && self.drain_ticks == other.drain_ticks
            && self.online == other.online
            && self.recovery == other.recovery
            && self.resource_period == other.resource_period
            && self.samples.len() == other.samples.len()
    }
}

impl HotspotBaselineLedger {
    /// The peak resident size over the run (KiB) — the allocation
    /// answer. A run that sampled nothing has no peak to report (`0`).
    pub fn peak_rss_kib(&self) -> u64 {
        self.samples.iter().map(|s| s.rss_kib).max().unwrap_or(0)
    }

    /// The final resident size over the run (KiB) — the reclamation
    /// answer: the process after the fleet was released. A run that
    /// sampled nothing reports the floor (`0`).
    pub fn final_rss_kib(&self) -> u64 {
        self.samples.last().map(|s| s.rss_kib).unwrap_or(0)
    }

    /// What the run says the reclamation did: the resident size the peak
    /// came back by after the drain, in KiB. Recorded, never asserted —
    /// the allocator is under no obligation to return the memory, and a
    /// negative result (the floor above the peak) is the honest answer
    /// on this machine.
    pub fn reclaimed_kib(&self) -> i64 {
        self.peak_rss_kib() as i64 - self.final_rss_kib() as i64
    }

    /// How many bots came back online inside the window, out of those
    /// the fault window dropped.
    pub fn recoveries_completed(&self) -> usize {
        self.recovery
            .iter()
            .filter(|t| t.back_online_at_tick.is_some())
            .count()
    }

    /// The mean recovery length in ticks over the bots that came back —
    /// the recovery window's statistic. A run with no completed recovery
    /// has no length to report (`0.0`).
    pub fn mean_recovery_ticks(&self) -> f64 {
        let mut total = 0.0f64;
        let mut done = 0usize;
        for track in &self.recovery {
            if let Some(back) = track.back_online_at_tick {
                total += (back - track.dropped_at_tick) as f64;
                done += 1;
            }
        }
        if done == 0 { 0.0 } else { total / done as f64 }
    }

    /// The longest recovery in ticks over the bots that came back — the
    /// recovery window's tail. A run with no completed recovery has no
    /// tail (`0`).
    pub fn max_recovery_ticks(&self) -> u32 {
        self.recovery
            .iter()
            .filter_map(|t| t.back_online_at_tick.map(|back| back - t.dropped_at_tick))
            .max()
            .unwrap_or(0)
    }

    /// The load window's holding rate: the smallest share of the fleet
    /// that stayed online across the load ticks (1.0 = nothing dented).
    /// A run with no load ticks has nothing to hold (`0.0`).
    pub fn load_window_min_online_ratio(&self) -> f64 {
        if self.window_ticks == 0 {
            return 0.0;
        }
        let peak = *self.online.iter().max().unwrap_or(&0) as f64;
        if peak == 0.0 {
            return 0.0;
        }
        self.online[..self.window_ticks as usize]
            .iter()
            .map(|&n| n as f64 / peak)
            .fold(1.0, f64::min)
    }

    /// The report entries for the saved run: the sampling period, the
    /// metric definitions, the CPU series, the resident series and the
    /// peak resident size — the same names the long-stability face
    /// uses, so a saved hotspot run is traceable to what was measured
    /// and how often. The wall-clock numbers are wall-clock by nature;
    /// they live in the report, not in the reproducible stdout metrics.
    pub fn resource_report_entries(&self) -> Vec<ReportEntry> {
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
            ReportEntry::new("peak_rss_kib", self.peak_rss_kib() as i64),
            ReportEntry::new("final_rss_kib", self.final_rss_kib() as i64),
            ReportEntry::new("reclaimed_rss_kib", self.reclaimed_kib()),
            ReportEntry::new("cpu_metric_def", CPU_METRIC_DEF),
            ReportEntry::new("rss_metric_def", RSS_METRIC_DEF),
        ]
    }
}

/// Run the hotspot/fault baseline: hold `concurrency` bots online over
/// the load window with a seeded fault subset dropped mid-window, then
/// release the whole fleet over the drain window, sampling the process
/// resource face on the long-stability schedule throughout.
///
/// The fd budget is sized like the other faces' so an offline run stays
/// refusal-free; a refused admission never joins the fleet (it has no
/// session to fault or release).
pub fn run_hotspot_baseline(
    scenario: &Scenario,
    fd_limit: i32,
    seed: u64,
) -> HotspotBaselineLedger {
    let mut fleet = Fleet::with_seed("hotspot", fd_limit, seed);
    let peak = scenario.concurrency as usize;
    let mut fd_open = 0i32;
    for _ in 0..peak {
        if fleet.add_bot() {
            fd_open += FD_PER_BOT;
            fleet.set_fd_open(fd_open);
        }
    }

    // A settled fleet for the load window: the server answers every
    // log-on, each bot gets its entity and space.
    let entity_ids: Vec<u32> = (1..=fleet.len() as u32).collect();
    for (index, bot) in fleet.bots_mut().iter_mut().enumerate() {
        bot.feed_log_on_result(entity_ids[index], 1);
    }

    let warmup_ticks = (scenario.warmup.as_secs_f32() / TICK_PERIOD) as u32;
    let window_ticks = (scenario.duration.as_secs_f32() / TICK_PERIOD) as u32;
    let drain_ticks = (window_ticks / 4).max(DRAIN_FLOOR_TICKS);

    // The fault window's seeded drop schedule, spread over the first
    // half of the load window so a recovery has a full half to land in
    // (the reconnect face's own rule).
    let span = (window_ticks / 2).max(1);
    let schedule: Vec<u32> = (0..fleet.len() as u32)
        .map(|index| (index * 37) % span)
        .collect();

    for _ in 0..warmup_ticks {
        for bot in fleet.bots_mut() {
            bot.tick(TICK_PERIOD);
        }
    }

    // The per-bot reconnect bookkeeping: whether the re-login has been
    // issued, and when the bot was online again.
    const NOT_ISSUED: u32 = u32::MAX;
    let mut relogin_at: Vec<u32> = vec![NOT_ISSUED; fleet.len()];
    let mut back_online_at: Vec<Option<u32>> = vec![None; fleet.len()];

    // The sampling schedule: eight samples over load + drain, whatever
    // their length (the long-stability stride, its eighth sample at the
    // last tick so a window that does not divide evenly still samples).
    // A run with no load window samples nothing — there is no series to
    // save for a fleet that never held.
    let total_ticks = window_ticks + drain_ticks;
    let stride = (total_ticks / 8).max(1);
    let resource_period = Duration::from_secs_f32(TICK_PERIOD * stride as f32);
    let targets: Vec<u32> = if window_ticks == 0 {
        Vec::new()
    } else {
        (0..total_ticks)
            .filter(|&tick| tick % stride == 0)
            .take(8)
            .collect()
    };
    let mut tracker = ResourceTracker::new(resource_period);

    let mut online: Vec<u32> = Vec::with_capacity(total_ticks as usize);
    let online_count = |fleet: &Fleet| {
        fleet
            .bots()
            .iter()
            .filter(|bot| bot.conn_state() == ConnState::Online)
            .count() as u32
    };

    // The load window: walk, drop the scheduled faults, re-issue logOn
    // for them, land the replies one latency later.
    for tick in 0..window_ticks {
        for (index, when) in schedule.iter().enumerate() {
            if *when == tick {
                fleet.bots_mut()[index].feed_logged_off(RECONNECT_KEEP_ALIVE);
            }
        }
        for bot in fleet.bots_mut() {
            bot.tick(TICK_PERIOD);
        }
        for index in 0..fleet.len() {
            let bot = &fleet.bots()[index];
            if back_online_at[index].is_none()
                && relogin_at[index] != NOT_ISSUED
                && bot.conn_state() == ConnState::Online
            {
                back_online_at[index] = Some(tick);
            }
            if relogin_at[index] == NOT_ISSUED
                && bot.conn_state() == ConnState::Idle
                && schedule[index] <= tick
            {
                fleet.bots_mut()[index].log_on();
                relogin_at[index] = tick;
            }
            if relogin_at[index] != NOT_ISSUED
                && relogin_at[index] + RELOGIN_LATENCY_TICKS <= tick
                && fleet.bots()[index].conn_state() == ConnState::LoggingIn
            {
                fleet.bots_mut()[index].feed_log_on_result(entity_ids[index], 1);
            }
        }
        online.push(online_count(&fleet));
        if targets.contains(&tick) {
            // The live procfs read; a run that cannot read its own
            // process cannot report resources — fail loudly rather than
            // save a hollow series.
            tracker.sample().expect("procfs resource sample");
        }
    }

    // The drain window: the destroy decision releases every session
    // (`feed_logged_off(None)` — the entity and movement state clears);
    // the sampling continues onto the drain tail so the reclamation face
    // is measured after the fleet is gone.
    for bot in fleet.bots_mut() {
        bot.feed_logged_off(None);
    }
    for tick in window_ticks..total_ticks {
        for bot in fleet.bots_mut() {
            bot.tick(TICK_PERIOD);
        }
        online.push(online_count(&fleet));
        if targets.contains(&tick) {
            tracker.sample().expect("procfs resource sample");
        }
    }

    // Compose the tracks from what actually happened: a drop whose tick
    // fell outside the window is not recorded as one.
    let recovery = fleet
        .bots()
        .iter()
        .enumerate()
        .map(|(index, bot)| RecoveryTrack {
            name: bot.user_name().to_owned(),
            dropped_at_tick: schedule[index].min(window_ticks.saturating_sub(1)),
            relogin_at_tick: relogin_at[index].min(window_ticks.saturating_sub(1)),
            back_online_at_tick: back_online_at[index],
        })
        .collect();

    HotspotBaselineLedger {
        sample_period: TICK_PERIOD,
        window_ticks,
        drain_ticks,
        online,
        recovery,
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
            name: "hotspot-baseline".to_string(),
            fixed_seed: seed,
            account_pool: AccountPool::Generated {
                prefix: "Hotspot".to_string(),
                count: concurrency.max(1),
                password: String::new(),
            },
            concurrency,
            ramp_rate: 0,
            warmup: Duration::from_secs(0),
            duration,
        }
    }

    /// The acceptance faces: the run holds the fleet over the load
    /// window (every bot online at its start), saves a real procfs
    /// series with a positive resident size across load and drain, and
    /// ends the online curve at zero — the reclamation window.
    #[test]
    fn the_run_records_allocation_and_reclamation_windows() {
        let scenario = scenario(4, Duration::from_secs(60), 173205);
        let ledger = run_hotspot_baseline(&scenario, 500 * FD_PER_BOT, scenario.fixed_seed);
        assert_eq!(ledger.window_ticks, 600);
        // The drain is a quarter of the load window, at the floor.
        assert_eq!(ledger.drain_ticks, 150);
        // Eight samples over load + drain, whatever their length; the
        // period is the stride the schedule picked (750 ticks / 8).
        assert_eq!(ledger.samples.len(), 8);
        assert_eq!(
            ledger.resource_period,
            Duration::from_secs_f32(TICK_PERIOD * 93.0)
        );
        assert!(ledger.peak_rss_kib() > 0, "the process has a resident size");
        assert_eq!(
            *ledger.online.last().unwrap(),
            0,
            "the drain releases the fleet"
        );
        // The curve is load + drain long, and starts at the full fleet
        // (the first load tick brings every settled log-on home except
        // the fault drops).
        assert_eq!(ledger.online.len(), 750);
        assert_eq!(ledger.online.iter().copied().max().unwrap(), 4);
    }

    /// The recovery window: the seeded drops land mid-window, every
    /// dropped bot comes back, and the recovery length is the offline
    /// floor — the drop tick plus the re-login and its one-tick reply
    /// (the reconnect face's own pinned floor).
    #[test]
    fn the_run_records_the_recovery_window() {
        let scenario = scenario(4, Duration::from_secs(60), 173205);
        let ledger = run_hotspot_baseline(&scenario, 500 * FD_PER_BOT, scenario.fixed_seed);
        assert_eq!(ledger.recovery.len(), 4);
        assert_eq!(ledger.recoveries_completed(), 4);
        assert_eq!(ledger.mean_recovery_ticks(), 2.0);
        assert_eq!(ledger.max_recovery_ticks(), 2);
        for track in &ledger.recovery {
            let back = track
                .back_online_at_tick
                .expect("a recovery inside the window");
            assert_eq!(track.relogin_at_tick, track.dropped_at_tick);
            assert_eq!(back, track.dropped_at_tick + 2);
        }
    }

    /// A fixed seed generates the same ledger: the load curve, the
    /// recovery tracks and the sample schedule reproduce bit for bit, and
    /// the resource series' schedule (period, count) is part of the
    /// fingerprint — its wall-clock numbers are not, by the ledger's own
    /// equality contract.
    #[test]
    fn the_fixed_seed_generates_the_same_ledger() {
        let scenario = scenario(4, Duration::from_secs(60), 173205);
        let a = run_hotspot_baseline(&scenario, 500 * FD_PER_BOT, scenario.fixed_seed);
        let b = run_hotspot_baseline(&scenario, 500 * FD_PER_BOT, scenario.fixed_seed);
        assert_eq!(a, b);
        assert_eq!(a.online, b.online);
        assert_eq!(a.recovery, b.recovery);
        assert_eq!(a.samples.len(), b.samples.len());
    }

    /// The ledger's shape follows the scenario: one curve entry per load
    /// and drain tick, in tick order, and the report quotes the same
    /// series lengths it saved.
    #[test]
    fn the_ledger_shape_follows_the_scenario() {
        let scenario = scenario(3, Duration::from_secs(30), 99);
        let ledger = run_hotspot_baseline(&scenario, 500 * FD_PER_BOT, scenario.fixed_seed);
        assert_eq!(ledger.sample_period, TICK_PERIOD);
        assert_eq!(ledger.window_ticks, 300);
        assert_eq!(ledger.online.len(), 300 + ledger.drain_ticks as usize);
        let entries = ledger.resource_report_entries();
        assert_eq!(entries.len(), 10);
        // The reported series and the saved samples agree in length.
        let rss_series = entries
            .iter()
            .find(|e| e.key == "rss_kib_series")
            .expect("the rss series");
        match &rss_series.value {
            ReportValue::List(values) => assert_eq!(values.len(), ledger.samples.len()),
            _ => panic!("the rss series is a list"),
        }
    }

    /// A window too short to sample anything saves no series: the peaks
    /// and the reclamation answer stay at the floor, and no recovery
    /// completes (the drop never landed).
    #[test]
    fn an_empty_window_saves_no_series() {
        let scenario = scenario(4, Duration::from_millis(1), 173205);
        let ledger = run_hotspot_baseline(&scenario, 500 * FD_PER_BOT, scenario.fixed_seed);
        assert_eq!(ledger.window_ticks, 0);
        assert_eq!(ledger.samples.len(), 0);
        assert_eq!(ledger.peak_rss_kib(), 0);
        assert_eq!(ledger.final_rss_kib(), 0);
        assert_eq!(ledger.reclaimed_kib(), 0);
        assert_eq!(ledger.recoveries_completed(), 0);
        assert_eq!(ledger.mean_recovery_ticks(), 0.0);
        assert_eq!(ledger.max_recovery_ticks(), 0);
        assert_eq!(ledger.load_window_min_online_ratio(), 0.0);
    }

    /// The FD budget bounds the fleet: a refused admission never joins
    /// the run — it has no session to fault or release.
    #[test]
    fn a_refused_admission_shrinks_the_fleet() {
        let scenario = scenario(4, Duration::from_secs(60), 173205);
        let ledger = run_hotspot_baseline(&scenario, FD_PER_BOT, scenario.fixed_seed);
        assert_eq!(ledger.recovery.len(), 1);
        // One bot on the fault schedule (dropped at tick 0, back at 2).
        assert_eq!(ledger.recoveries_completed(), 1);
        assert_eq!(ledger.online.iter().copied().max().unwrap(), 1);
        assert_eq!(*ledger.online.last().unwrap(), 0, "the drain releases it");
    }

    /// The allocation face holds: the fleet is fully online at the load
    /// window's start and nothing but the fault drops dents it — the
    /// minimum online share stays above zero.
    #[test]
    fn the_load_window_holds_the_fleet_online() {
        let scenario = scenario(4, Duration::from_secs(60), 173205);
        let ledger = run_hotspot_baseline(&scenario, 500 * FD_PER_BOT, scenario.fixed_seed);
        assert!(ledger.load_window_min_online_ratio() > 0.0);
        // Every bot is home for most of the window: the fault drops dent
        // it, the recoveries restore it.
        assert!(
            ledger.online[..ledger.window_ticks as usize].contains(&(ledger.recovery.len() as u32))
        );
    }
}
