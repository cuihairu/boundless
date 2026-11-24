//! Load-generator process resource sampling (BW-17-03-04).
//!
//! The report contract (`docs/operations/load-testing.md`, "Reports")
//! requires the metric definitions and the sampling period to travel with
//! the numbers. The sampler reads the generator's own procfs face —
//! `/proc/self/stat` CPU ticks and `/proc/self/status` resident size —
//! and every report it produces names its period and its metric
//! definitions, so a sample is traceable to what was measured and how
//! often. Bandwidth rides the connection layer's byte counters, which
//! only exist once a live server feeds the run; its definition is a
//! documented constant until that face wires up.

use std::fs;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::report::{ReportEntry, ReportValue};

/// `CPU_METRIC_DEF`: what `user_cpu_secs`/`system_cpu_secs` measure.
pub const CPU_METRIC_DEF: &str =
    "utime+stime from /proc/self/stat, clock ticks scaled by _SC_CLK_TCK";

/// `RSS_METRIC_DEF`: what `rss_kib` measures.
pub const RSS_METRIC_DEF: &str = "VmRSS from /proc/self/status, KiB";

/// `BANDWIDTH_METRIC_DEF`: the wire-usage metric the report reserves.
/// The counters live in the connection layer and only move against a
/// live server; the definition documents the metric until then.
pub const BANDWIDTH_METRIC_DEF: &str =
    "bytes_sent/bytes_received deltas per sampling period from the connection layer byte counters";

/// One sample of the generator process's resource use.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ResourceSample {
    /// User-mode CPU seconds since process start.
    pub user_cpu_secs: f64,
    /// System-mode CPU seconds since process start.
    pub system_cpu_secs: f64,
    /// Resident set size, KiB.
    pub rss_kib: u64,
    /// The wall-clock stamp of the sample, seconds since the Unix epoch.
    pub wall_stamp_secs: f64,
}

impl ResourceSample {
    /// Sample the current process.
    pub fn take() -> std::io::Result<Self> {
        let stat = fs::read_to_string("/proc/self/stat")?;
        let status = fs::read_to_string("/proc/self/status")?;
        Self::from_parts(&stat, &status)
    }

    /// Build a sample from the two procfs bodies, the injectable shape
    /// of [`take`].
    pub fn from_parts(stat: &str, status: &str) -> std::io::Result<Self> {
        let (user_ticks, system_ticks) = parse_stat_ticks(stat).ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "unparsable /proc/self/stat",
            )
        })?;
        let rss_kib = parse_status_rss_kib(status).ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "unparsable /proc/self/status",
            )
        })?;
        let ticks_per_sec = unsafe { libc::sysconf(libc::_SC_CLK_TCK) }.max(1) as f64;
        let since_epoch = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        Ok(Self {
            user_cpu_secs: user_ticks as f64 / ticks_per_sec,
            system_cpu_secs: system_ticks as f64 / ticks_per_sec,
            rss_kib,
            wall_stamp_secs: since_epoch.as_secs_f64(),
        })
    }
}

/// Split the `/proc/self/stat` fields after the `comm` parentheses — the
/// comm can hold spaces and digits, so everything before the **last**
/// `)` is the comm — and return `(utime, stime)` in clock ticks.
pub fn parse_stat_ticks(stat: &str) -> Option<(u64, u64)> {
    let after_comm = stat.rsplit_once(')')?.1.trim_start();
    // With the pid and comm consumed, `state` is field 3 — the first
    // token here — so utime (field 14) and stime (15) sit at 11 and 12.
    let fields: Vec<&str> = after_comm.split_whitespace().collect();
    let utime = fields.get(11)?.parse().ok()?;
    let stime = fields.get(12)?.parse().ok()?;
    Some((utime, stime))
}

/// The `VmRSS:` value of a `/proc/self/status` body, in KiB.
pub fn parse_status_rss_kib(status: &str) -> Option<u64> {
    for line in status.lines() {
        if let Some(value) = line.strip_prefix("VmRSS:") {
            // "    1234 kB" — the unit is always kB; take the number.
            return value.split_whitespace().next()?.parse().ok();
        }
    }
    None
}

/// The periodic sampler a run drives: `sample()` once per period, and
/// the report renders the series with the period and metric definitions
/// attached.
pub struct ResourceTracker {
    period: Duration,
    samples: Vec<ResourceSample>,
}

impl ResourceTracker {
    /// A tracker sampling every `period`.
    pub fn new(period: Duration) -> Self {
        Self {
            period,
            samples: Vec::new(),
        }
    }

    /// The sampling period, as the report quotes it.
    pub fn period(&self) -> Duration {
        self.period
    }

    /// Take and record one sample.
    pub fn sample(&mut self) -> std::io::Result<()> {
        self.samples.push(ResourceSample::take()?);
        Ok(())
    }

    /// The recorded samples.
    pub fn samples(&self) -> &[ResourceSample] {
        &self.samples
    }

    /// The report entries: the sampling period, the metric definitions,
    /// the CPU series, and the peak resident size.
    pub fn report_entries(&self) -> Vec<ReportEntry> {
        let peak_rss = self.samples.iter().map(|s| s.rss_kib).max().unwrap_or(0);
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
                ReportValue::Float(self.period.as_secs_f64()),
            ),
            ReportEntry::new("resource_sample_count", self.samples.len() as i64),
            ReportEntry::new("cpu_user_secs_series", series(|s| s.user_cpu_secs)),
            ReportEntry::new("cpu_system_secs_series", series(|s| s.system_cpu_secs)),
            ReportEntry::new("peak_rss_kib", peak_rss as i64),
            ReportEntry::new("cpu_metric_def", CPU_METRIC_DEF),
            ReportEntry::new("rss_metric_def", RSS_METRIC_DEF),
            ReportEntry::new("bandwidth_metric_def", BANDWIDTH_METRIC_DEF),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The acceptance face: the report states the metric definitions and
    /// the sampling period next to the numbers.
    #[test]
    fn the_report_states_the_metric_definitions_and_period() {
        let mut tracker = ResourceTracker::new(Duration::from_secs(5));
        tracker.sample().expect("sample");
        tracker.sample().expect("sample");

        let entries = tracker.report_entries();
        let find = |key: &str| {
            entries
                .iter()
                .find(|entry| entry.key == key)
                .map(|entry| &entry.value)
                .expect(key)
        };
        assert_eq!(find("sampling_period_secs"), &ReportValue::Float(5.0));
        assert_eq!(find("resource_sample_count"), &ReportValue::Int(2));
        assert_eq!(
            find("cpu_metric_def"),
            &ReportValue::Str(CPU_METRIC_DEF.to_string())
        );
        assert_eq!(
            find("rss_metric_def"),
            &ReportValue::Str(RSS_METRIC_DEF.to_string())
        );
        assert_eq!(
            find("bandwidth_metric_def"),
            &ReportValue::Str(BANDWIDTH_METRIC_DEF.to_string())
        );
        assert_eq!(series_len(find("cpu_user_secs_series")), 2);
        assert_eq!(tracker.samples().len(), 2);
        assert_eq!(tracker.period(), Duration::from_secs(5));

        let empty = ResourceTracker::new(Duration::from_millis(250));
        // A scalar entry has no series, and a sampleless run has none.
        assert_eq!(
            series_len(find_in(&empty.report_entries(), "sampling_period_secs")),
            0
        );
        assert_eq!(
            series_len(find_in(&empty.report_entries(), "cpu_user_secs_series")),
            0
        );
        assert_eq!(
            find_in(&empty.report_entries(), "resource_sample_count"),
            &ReportValue::Int(0)
        );
        assert_eq!(
            find_in(&empty.report_entries(), "peak_rss_kib"),
            &ReportValue::Int(0)
        );
        assert_eq!(empty.period(), Duration::from_millis(250));
    }

    fn find_in<'a>(entries: &'a [ReportEntry], key: &str) -> &'a ReportValue {
        &entries
            .iter()
            .find(|entry| entry.key == key)
            .expect(key)
            .value
    }

    /// The length of a list entry; a scalar entry has no series.
    fn series_len(value: &ReportValue) -> usize {
        match value {
            ReportValue::List(items) => items.len(),
            _ => 0,
        }
    }

    /// `take()` reads the live process: non-negative CPU, a non-zero
    /// resident size, and a plausible Unix stamp.
    #[test]
    fn take_samples_the_live_process() {
        let sample = ResourceSample::take().expect("procfs present");
        assert!(sample.user_cpu_secs >= 0.0);
        assert!(sample.system_cpu_secs >= 0.0);
        assert!(sample.rss_kib > 0);
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64();
        assert!(sample.wall_stamp_secs > 1_700_000_000.0);
        assert!(sample.wall_stamp_secs <= now);
    }

    /// The stat parser walks past a comm holding spaces, digits and
    /// parens, landing utime/stime at fields 14/15 (the classic trap:
    /// splitting from the *first* paren reads the comm as fields).
    #[test]
    fn the_stat_parser_walks_past_a_tricky_comm() {
        // Token index i after the comm is field i+3 (state = field 3),
        // so utime (field 14) sits at token 11: 52, and stime at 77.
        let stat = "123 ((bot) 1) R 42 43 44 45 46 47 48 49 50 51 52 77 88 99";
        assert_eq!(parse_stat_ticks(stat), Some((52, 77)));
        // A plain comm parses the same way.
        let plain = "9999 (load) S 1 2 3 4 5 6 7 8 9 10 11 5 6 7";
        assert_eq!(parse_stat_ticks(plain), Some((11, 5)));
        // Too few fields, or no comm parens: refused.
        assert_eq!(parse_stat_ticks("1 (x) S 1 2 3"), None);
        assert_eq!(parse_stat_ticks("1 2 3 4 5 6 7 8 9"), None);
        assert_eq!(parse_stat_ticks(""), None);
    }

    /// A body that fails either parser lands the InvalidData error, and
    /// a valid pair computes the CPU seconds against `_SC_CLK_TCK`.
    #[test]
    fn from_parts_refuses_garbage_and_scales_the_ticks() {
        let stat = "123 (load) R 1 2 3 4 5 6 7 8 9 10 40 60";
        let status = "VmRSS:\t 2048 kB\n";
        let sample = ResourceSample::from_parts(stat, status).expect("valid");
        let ticks_per_sec = unsafe { libc::sysconf(libc::_SC_CLK_TCK) }.max(1) as f64;
        assert_eq!(sample.user_cpu_secs, 40.0 / ticks_per_sec);
        assert_eq!(sample.system_cpu_secs, 60.0 / ticks_per_sec);
        assert_eq!(sample.rss_kib, 2048);

        let broken_stat = "no parens here";
        let error = ResourceSample::from_parts(broken_stat, status).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);

        let broken_status = "Name:\tload\n";
        let error = ResourceSample::from_parts(stat, broken_status).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    }

    /// The status parser reads `VmRSS:` in KiB and refuses a body
    /// without it.
    #[test]
    fn the_status_parser_reads_vm_rss() {
        let status = "Name:\tload\nVmPeak:\t  10000 kB\nVmRSS:\t  2048 kB\nThreads:\t3\n";
        assert_eq!(parse_status_rss_kib(status), Some(2048));
        assert_eq!(parse_status_rss_kib("Name:\tload\n"), None);
        assert_eq!(parse_status_rss_kib("VmRSS:\tkB\n"), None);
    }
}
