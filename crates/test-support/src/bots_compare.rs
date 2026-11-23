//! BW-23-03-05: comparing runs under load.
//!
//! A load run — the thing Bots drive against the server — leaves a record:
//! the version and scenario that ran, how long each tick took, and how much
//! memory the run held. Comparing two runs reports each version's p95/p99
//! tick duration (the latency tail that matters) and how the memory readings
//! moved between them, so a regression is readable against the version it
//! happened on.

use crate::stats_report::report;
use foundation::profiler::Snapshot;

/// One run under load.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunRecord {
    /// The version that ran.
    pub version: String,
    /// The scenario loaded.
    pub scenario: String,
    /// How long each tick took, in clock stamps.
    pub tick_cycles: Vec<u64>,
    /// The memory the run held, in bytes.
    pub rss_bytes: u64,
    /// The peak memory the run reached, in bytes.
    pub peak_bytes: u64,
}

impl RunRecord {
    /// A run for the given version and scenario, with no ticks yet.
    pub fn new(version: &str, scenario: &str) -> Self {
        Self {
            version: version.to_string(),
            scenario: scenario.to_string(),
            tick_cycles: Vec::new(),
            rss_bytes: 0,
            peak_bytes: 0,
        }
    }

    /// Set the run's per-tick durations.
    pub fn ticks(mut self, tick_cycles: &[u64]) -> Self {
        self.tick_cycles = tick_cycles.to_vec();
        self
    }

    /// Set the run's memory readings.
    pub fn memory(mut self, rss_bytes: u64, peak_bytes: u64) -> Self {
        self.rss_bytes = rss_bytes;
        self.peak_bytes = peak_bytes;
        self
    }

    /// A run seeded from a profiler capture: how long each tick actually took,
    /// extracted by the tick statistics.
    pub fn from_capture(snapshot: &Snapshot, version: &str, scenario: &str) -> Self {
        let ticks = report(snapshot).ticks;
        Self::new(version, scenario).ticks(
            &ticks
                .iter()
                .map(|tick| tick.inclusive_cycles)
                .collect::<Vec<_>>(),
        )
    }
}

/// One version's summary of its run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunStats {
    /// The version that ran.
    pub version: String,
    /// The scenario loaded.
    pub scenario: String,
    /// How many ticks were measured.
    pub samples: usize,
    /// The median tick duration.
    pub p50: u64,
    /// The 95th percentile tick duration.
    pub p95: u64,
    /// The 99th percentile tick duration.
    pub p99: u64,
    /// The slowest tick.
    pub max: u64,
    /// The memory the run held, in bytes.
    pub rss_bytes: u64,
    /// The peak memory the run reached, in bytes.
    pub peak_bytes: u64,
}

/// Two or more runs compared: each version's summary, plus how the memory
/// readings moved from the first run to the last (negative means the later
/// run used less).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComparisonReport {
    /// One summary per run, in the order given.
    pub runs: Vec<RunStats>,
    /// Last minus first run's held memory, in bytes.
    pub rss_delta_bytes: i64,
    /// Last minus first run's peak memory, in bytes.
    pub peak_delta_bytes: i64,
}

/// The nearest-rank percentile of `per_cent` over the values: sort ascending
/// and take the value at rank `ceil(per_cent/100 * count)`. Returns nothing
/// for an empty sample.
pub fn percentile(values: &[u64], per_cent: u32) -> Option<u64> {
    let count = values.len();
    if count == 0 {
        return None;
    }
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    // Nearest rank, rounded up so a percentile never under-counts: p95 of 20
    // samples lands on the 19th value.
    let rank = (per_cent as u64 * count as u64).div_ceil(100);
    let rank = rank.max(1).min(count as u64) as usize;
    Some(sorted[rank - 1])
}

/// Summarize one run: its percentiles and its memory readings.
pub fn stats(run: &RunRecord) -> RunStats {
    RunStats {
        version: run.version.clone(),
        scenario: run.scenario.clone(),
        samples: run.tick_cycles.len(),
        p50: percentile(&run.tick_cycles, 50).unwrap_or(0),
        p95: percentile(&run.tick_cycles, 95).unwrap_or(0),
        p99: percentile(&run.tick_cycles, 99).unwrap_or(0),
        max: run.tick_cycles.iter().copied().max().unwrap_or(0),
        rss_bytes: run.rss_bytes,
        peak_bytes: run.peak_bytes,
    }
}

/// Compare runs: each version's summary, plus the memory movement from the
/// first run to the last.
pub fn compare(runs: &[RunRecord]) -> ComparisonReport {
    let summaries = runs.iter().map(stats).collect();
    let (first, last) = match (runs.first(), runs.last()) {
        (Some(first), Some(last)) => (first, last),
        _ => {
            return ComparisonReport {
                runs: summaries,
                rss_delta_bytes: 0,
                peak_delta_bytes: 0,
            };
        }
    };
    ComparisonReport {
        runs: summaries,
        rss_delta_bytes: signed_delta(first.rss_bytes, last.rss_bytes),
        peak_delta_bytes: signed_delta(first.peak_bytes, last.peak_bytes),
    }
}

/// The signed movement from one byte reading to another, saturating at the
/// edges of `i64` (readings never approach it in practice).
fn signed_delta(from: u64, to: u64) -> i64 {
    if to >= from {
        i64::try_from(to - from).unwrap_or(i64::MAX)
    } else {
        -i64::try_from(from - to).unwrap_or(i64::MAX)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use foundation::profiler::Profiler;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};

    type Pinned = Arc<AtomicU64>;

    fn pinned() -> Pinned {
        Arc::new(AtomicU64::new(0))
    }

    fn profiler_with(source: &Pinned) -> Profiler {
        let source = source.clone();
        Profiler::with_clock(move || source.load(Ordering::SeqCst))
    }

    fn advance(source: &Pinned, by: u64) {
        source.fetch_add(by, Ordering::SeqCst);
    }

    #[test]
    fn two_runs_report_their_percentiles_and_memory_movement() {
        // A runs the baseline build over twenty ticks; B runs the candidate.
        let baseline = RunRecord::new("0.1.0", "login-storm")
            .ticks(&(1..=20).collect::<Vec<_>>())
            .memory(100_000_000, 150_000_000);
        let candidate = RunRecord::new("0.1.1", "login-storm")
            .ticks(&(2..=21).collect::<Vec<_>>())
            .memory(120_000_000, 140_000_000);

        let report = compare(&[baseline, candidate]);
        assert_eq!(report.runs.len(), 2, "both versions are reported");
        assert_eq!(report.runs[0].version, "0.1.0");
        assert_eq!(report.runs[1].version, "0.1.1");
        assert_eq!(report.runs[0].samples, 20);

        // Nearest-rank percentiles: p95 of twenty values is the 19th, p99 is
        // the 20th.
        assert_eq!(report.runs[0].p50, 10);
        assert_eq!(report.runs[0].p95, 19);
        assert_eq!(report.runs[0].p99, 20);
        assert_eq!(report.runs[0].max, 20);
        assert_eq!(report.runs[1].p95, 20);
        assert_eq!(report.runs[1].p99, 21);

        // The memory movement between the runs.
        assert_eq!(report.rss_delta_bytes, 20_000_000);
        assert_eq!(report.peak_delta_bytes, -10_000_000);
    }

    #[test]
    fn a_run_seeded_from_a_capture_carries_its_tick_durations() {
        let source = pinned();
        let prof = profiler_with(&source);
        prof.set_identity("cellapp01", 100);

        // Three ticks of 10, 20 and 30 stamps.
        for cycles in [10, 20, 30] {
            {
                let _tick = prof.scoped("ServerApp::tick");
                advance(&source, cycles);
            }
            prof.tick();
        }

        let run = RunRecord::from_capture(&prof.snapshot(), "0.1.0", "movement");
        assert_eq!(run.version, "0.1.0");
        assert_eq!(run.scenario, "movement");
        assert_eq!(run.tick_cycles, vec![10, 20, 30]);
        assert_eq!(stats(&run).p99, 30);
        assert_eq!(percentile(&run.tick_cycles, 50), Some(20));
    }

    #[test]
    fn an_empty_sample_reports_no_percentile_and_a_single_run_no_delta() {
        assert_eq!(percentile(&[], 95), None);
        let run = RunRecord::new("0.1.0", "idle");
        assert_eq!(stats(&run).samples, 0);
        let report = compare(&[run]);
        assert_eq!(report.runs.len(), 1);
        assert_eq!(report.rss_delta_bytes, 0);
        assert_eq!(report.peak_delta_bytes, 0);
    }
}
