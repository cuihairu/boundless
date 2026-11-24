//! Aggregated run statistics (BW-17-03-02, BW-17-03-03).
//!
//! The reference bots keep no statistics file — the aggregates are
//! Boundless-side (`docs/operations/load-testing.md`, "Reports"). The
//! face is pure math over recorded run inputs: a ledger in, hand-checkable
//! numbers out, so a fixed input pins the statistics exactly. The offline
//! runs convert every send they issue, so their success rate is 1.0 until
//! a live server starts feeding log-on failures — the field is what a
//! server-backed run will drag down.

use crate::hurricane::HurricaneLedger;
use crate::report::{ReportEntry, ReportValue};

/// The login statistics of one run.
#[derive(Clone, Debug, PartialEq)]
pub struct LoginAggregate {
    /// Log-on sends issued (`sum(per_second)`, retry re-log-ons included).
    pub attempts: u64,
    /// Bots admitted (`names.len()`).
    pub admitted: u64,
    /// Bots the FD budget refused before they ever sent.
    pub refused: u64,
    /// `attempts / admitted`; a run that issued no sends reports `0.0`
    /// rather than claiming a vacuous success.
    pub success_rate: f64,
}

impl LoginAggregate {
    /// Aggregate a hurricane ledger.
    pub fn from_hurricane(ledger: &HurricaneLedger) -> Self {
        let attempts = ledger.per_second.iter().map(|&n| n as u64).sum();
        let admitted = ledger.names.len() as u64;
        let refused = ledger.refused as u64;
        let success_rate = if attempts == 0 {
            0.0
        } else {
            admitted as f64 / attempts as f64
        };
        Self {
            attempts,
            admitted,
            refused,
            success_rate,
        }
    }

    /// The report entries for these statistics (BW-17-03-01 envelope).
    pub fn report_entries(&self) -> Vec<ReportEntry> {
        vec![
            ReportEntry::new("login_attempts", self.attempts as i64),
            ReportEntry::new("login_admitted", self.admitted as i64),
            ReportEntry::new("login_refused", self.refused as i64),
            ReportEntry::new("login_success_rate", self.success_rate),
        ]
    }
}

/// The shape of an online-count series: the peak the fleet reached and
/// where it ended.
#[derive(Clone, Debug, PartialEq)]
pub struct OnlineSummary {
    /// The largest sample.
    pub peak: u32,
    /// The last sample.
    pub final_count: u32,
}

/// Summarize an online-count series (`HurricaneLedger::online`).
pub fn online_summary(online: &[u32]) -> OnlineSummary {
    OnlineSummary {
        peak: online.iter().copied().max().unwrap_or(0),
        final_count: online.last().copied().unwrap_or(0),
    }
}

/// The report entries for the online series and its summary.
pub fn online_report_entries(online: &[u32]) -> Vec<ReportEntry> {
    let summary = online_summary(online);
    vec![
        ReportEntry::new(
            "online",
            ReportValue::List(online.iter().map(|&n| ReportValue::Int(n as i64)).collect()),
        ),
        ReportEntry::new("online_peak", summary.peak as i64),
        ReportEntry::new("online_final", summary.final_count as i64),
    ]
}

/// The nearest-rank percentile of a sample set (`ceil(p/100 * n)`,
/// 1-indexed, on the ascending sort): the order statistic a load report
/// quotes. Non-finite samples are dropped first — a NaN latency is not a
/// latency. `None` for an empty set; `p` clamps to `[0, 100]`.
pub fn nearest_rank_percentile(samples: &[f64], percentile: f64) -> Option<f64> {
    let mut finite: Vec<f64> = samples
        .iter()
        .copied()
        .filter(|sample| sample.is_finite())
        .collect();
    if finite.is_empty() {
        return None;
    }
    finite.sort_by(|a, b| a.total_cmp(b));
    let rank = ((percentile.clamp(0.0, 100.0) / 100.0) * finite.len() as f64).ceil();
    let index = (rank as usize).clamp(1, finite.len()) - 1;
    Some(finite[index])
}

/// The latency statistics of one run: the nearest-rank percentiles the
/// report quotes and the timeout counts at the run's threshold.
#[derive(Clone, Debug, PartialEq)]
pub struct LatencyAggregate {
    /// Samples the statistics cover (non-finite entries dropped).
    pub samples: u64,
    /// The nearest-rank p50, or `None` with no samples.
    pub p50: Option<f64>,
    /// The nearest-rank p95.
    pub p95: Option<f64>,
    /// The nearest-rank p99.
    pub p99: Option<f64>,
    /// The timeout threshold the counts use.
    pub timeout_threshold: f64,
    /// Samples at or beyond the threshold.
    pub timeouts: u64,
    /// `timeouts / samples`; a run with no samples reports `0.0`.
    pub timeout_rate: f64,
}

impl LatencyAggregate {
    /// Aggregate latency samples (milliseconds, say — the unit is the
    /// feed's) against a timeout threshold.
    pub fn new(samples: &[f64], timeout_threshold: f64) -> Self {
        let finite: Vec<f64> = samples
            .iter()
            .copied()
            .filter(|sample| sample.is_finite())
            .collect();
        let total = finite.len() as u64;
        let timeouts = finite.iter().filter(|&&s| s >= timeout_threshold).count() as u64;
        Self {
            samples: total,
            p50: nearest_rank_percentile(&finite, 50.0),
            p95: nearest_rank_percentile(&finite, 95.0),
            p99: nearest_rank_percentile(&finite, 99.0),
            timeout_threshold,
            timeouts,
            timeout_rate: if total == 0 {
                0.0
            } else {
                timeouts as f64 / total as f64
            },
        }
    }

    /// The report entries for these statistics.
    pub fn report_entries(&self) -> Vec<ReportEntry> {
        let percentile_entry = |key: &str, value: Option<f64>| {
            ReportEntry::new(
                key,
                match value {
                    Some(v) => ReportValue::Float(v),
                    None => ReportValue::Str("n/a".to_string()),
                },
            )
        };
        vec![
            ReportEntry::new("latency_samples", self.samples as i64),
            percentile_entry("latency_p50", self.p50),
            percentile_entry("latency_p95", self.p95),
            percentile_entry("latency_p99", self.p99),
            ReportEntry::new("latency_timeout_threshold", self.timeout_threshold),
            ReportEntry::new("latency_timeouts", self.timeouts as i64),
            ReportEntry::new("latency_timeout_rate", self.timeout_rate),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hurricane::run_login_hurricane;
    use crate::report::ScenarioReport;
    use crate::scenario::{AccountPool, Scenario};
    use std::time::Duration;

    /// The acceptance face: fixed samples pin the percentiles — 100
    /// samples 1..=100 give nearest-rank p50=50, p95=95, p99=99.
    #[test]
    fn fixed_samples_verify_the_percentiles() {
        let samples: Vec<f64> = (1..=100).map(f64::from).collect();
        assert_eq!(nearest_rank_percentile(&samples, 50.0), Some(50.0));
        assert_eq!(nearest_rank_percentile(&samples, 95.0), Some(95.0));
        assert_eq!(nearest_rank_percentile(&samples, 99.0), Some(99.0));
        assert_eq!(nearest_rank_percentile(&samples, 0.0), Some(1.0));
        assert_eq!(nearest_rank_percentile(&samples, 100.0), Some(100.0));
        assert_eq!(nearest_rank_percentile(&samples, 500.0), Some(100.0));

        let aggregate = LatencyAggregate::new(&samples, 100.0);
        assert_eq!(aggregate.samples, 100);
        assert_eq!(aggregate.p50, Some(50.0));
        assert_eq!(aggregate.p95, Some(95.0));
        assert_eq!(aggregate.p99, Some(99.0));
    }

    /// A small odd-sized set walks the ceil: 7 samples rank p95 at the
    /// 7th sample and p50 at the 4th.
    #[test]
    fn an_odd_sized_set_walks_the_ceil_rank() {
        let samples = [10.0, 20.0, 30.0, 40.0, 50.0, 60.0, 70.0];
        assert_eq!(nearest_rank_percentile(&samples, 50.0), Some(40.0));
        assert_eq!(nearest_rank_percentile(&samples, 95.0), Some(70.0));
        assert_eq!(nearest_rank_percentile(&samples, 99.0), Some(70.0));
    }

    /// One sample pins every percentile; an empty set answers `None`.
    #[test]
    fn one_sample_pins_every_percentile_and_empty_answers_none() {
        assert_eq!(nearest_rank_percentile(&[7.5], 95.0), Some(7.5));
        assert_eq!(nearest_rank_percentile(&[], 95.0), None);
        let aggregate = LatencyAggregate::new(&[], 100.0);
        assert_eq!(aggregate.samples, 0);
        assert_eq!(aggregate.p50, None);
        assert_eq!(aggregate.timeouts, 0);
        assert_eq!(aggregate.timeout_rate, 0.0);
    }

    /// Unordered input is fine — the rank walks the sorted copy — and
    /// non-finite samples drop out before ranking or counting.
    #[test]
    fn unordered_and_non_finite_samples_are_handled() {
        let samples = [50.0, 10.0, 30.0, 20.0, 40.0];
        let aggregate = LatencyAggregate::new(&samples, 45.0);
        assert_eq!(aggregate.samples, 5);
        assert_eq!(aggregate.p50, Some(30.0));
        // At-or-beyond the threshold: 50 only.
        assert_eq!(aggregate.timeouts, 1);
        assert!((aggregate.timeout_rate - 0.2).abs() < 1e-12);

        let with_nan = [10.0, f64::NAN, 30.0];
        let aggregate = LatencyAggregate::new(&with_nan, 100.0);
        assert_eq!(aggregate.samples, 2);
        assert_eq!(aggregate.p50, Some(10.0));
    }

    /// The caller's buffer is not reordered by aggregation.
    #[test]
    fn aggregation_does_not_reorder_the_caller_samples() {
        let samples = vec![3.0, 1.0, 2.0];
        let _ = LatencyAggregate::new(&samples, 100.0);
        assert_eq!(samples, vec![3.0, 1.0, 2.0]);
    }

    /// The latency statistics render into the report envelope, `n/a`
    /// standing in for the percentiles of a sampleless run.
    #[test]
    fn the_latency_statistics_render_into_the_report_envelope() {
        let samples = [10.0, 200.0, 50.0, 150.0, 20.0];
        let aggregate = LatencyAggregate::new(&samples, 100.0);
        let mut values = aggregate.report_entries();
        values.extend(
            LoginAggregate::from_hurricane(&HurricaneLedger {
                per_second: Vec::new(),
                names: Vec::new(),
                speeds: Vec::new(),
                refused: 0,
                online: Vec::new(),
            })
            .report_entries(),
        );
        let scenario = Scenario {
            name: "latency-check".to_string(),
            fixed_seed: 1,
            account_pool: AccountPool::Generated {
                prefix: "Bot".to_string(),
                count: 1,
                password: String::new(),
            },
            concurrency: 1,
            ramp_rate: 0,
            warmup: Duration::from_secs(0),
            duration: Duration::from_secs(1),
        };
        let json = ScenarioReport::record(&scenario, "offline", values).to_json();
        assert!(json.contains("\"latency_samples\": 5"));
        assert!(json.contains("\"latency_p50\": 50"));
        // ceil(0.95 * 5) = 5 and ceil(0.99 * 5) = 5: both ranks land on
        // the largest of the five samples.
        assert!(json.contains("\"latency_p95\": 200"));
        assert!(json.contains("\"latency_p99\": 200"));
        assert!(json.contains("\"latency_timeouts\": 2"));
        assert!(json.contains("\"latency_timeout_rate\": 0.4"));
        assert!(json.contains("\"login_success_rate\": 0"));

        // A sampleless run stands its percentiles in as "n/a".
        let empty = LatencyAggregate::new(&[], 100.0);
        let json = ScenarioReport::record(&scenario, "offline", empty.report_entries()).to_json();
        assert!(json.contains("\"latency_p50\": \"n/a\""));
        assert!(json.contains("\"latency_timeout_rate\": 0"));
    }

    /// A hand-built ledger with numbers a reader can check by eye.
    fn known_ledger() -> HurricaneLedger {
        HurricaneLedger {
            per_second: vec![3, 2, 4],
            names: vec![
                "Bot_1".to_string(),
                "Bot_2".to_string(),
                "Bot_3".to_string(),
                "Bot_4".to_string(),
                "Bot_5".to_string(),
            ],
            speeds: vec![6.0, 6.5, 7.0, 7.5, 6.25],
            refused: 7,
            online: vec![3, 5, 5],
        }
    }

    /// The acceptance face: known inputs produce hand-computed
    /// statistics — 5 admitted out of 9 issued sends is a 5/9 rate, the
    /// 7 budget refusals sit outside the denominator because those bots
    /// never sent.
    #[test]
    fn known_inputs_produce_hand_computed_login_statistics() {
        let aggregate = LoginAggregate::from_hurricane(&known_ledger());
        assert_eq!(aggregate.attempts, 9);
        assert_eq!(aggregate.admitted, 5);
        assert_eq!(aggregate.refused, 7);
        assert!((aggregate.success_rate - 5.0 / 9.0).abs() < 1e-12);
    }

    /// A run that issued no sends reports `0.0`, not a vacuous 100%.
    #[test]
    fn a_sendless_run_reports_zero_rate() {
        let ledger = HurricaneLedger {
            per_second: Vec::new(),
            names: Vec::new(),
            speeds: Vec::new(),
            refused: 0,
            online: Vec::new(),
        };
        assert_eq!(LoginAggregate::from_hurricane(&ledger).success_rate, 0.0);
    }

    /// An offline run converts every send it issues: the rate is 1.0 and
    /// the online series climbs to the concurrency cap in ramp steps
    /// (scenario 5/s ramp to 12, warmup 1s + 9 load seconds).
    #[test]
    fn an_offline_run_converts_every_send_and_tracks_the_online_series() {
        let scenario = Scenario {
            name: "login-hurricane".to_string(),
            fixed_seed: 7,
            account_pool: AccountPool::Generated {
                prefix: "Bot".to_string(),
                count: 12,
                password: String::new(),
            },
            concurrency: 12,
            ramp_rate: 5,
            warmup: Duration::from_secs(1),
            duration: Duration::from_secs(9),
        };
        let ledger = run_login_hurricane(&scenario, 512 * 40, scenario.fixed_seed);

        let aggregate = LoginAggregate::from_hurricane(&ledger);
        assert_eq!(aggregate.attempts, 12);
        assert_eq!(aggregate.admitted, 12);
        assert_eq!(aggregate.refused, 0);
        assert_eq!(aggregate.success_rate, 1.0);

        assert_eq!(ledger.online, vec![0, 0, 5, 10, 12, 12, 12, 12, 12, 12]);
        let summary = online_summary(&ledger.online);
        assert_eq!(summary.peak, 12);
        assert_eq!(summary.final_count, 12);
    }

    /// An empty online series summarizes to zeros.
    #[test]
    fn an_empty_online_series_sums_to_zeroes() {
        let summary = online_summary(&[]);
        assert_eq!(summary.peak, 0);
        assert_eq!(summary.final_count, 0);
    }

    /// The report entries render into the BW-17-03-01 envelope with the
    /// numbers the ledger recorded.
    #[test]
    fn the_aggregates_render_into_the_report_envelope() {
        let ledger = known_ledger();
        let scenario = Scenario {
            name: "login-hurricane".to_string(),
            fixed_seed: 141421,
            account_pool: AccountPool::Generated {
                prefix: "Bot".to_string(),
                count: 5,
                password: String::new(),
            },
            concurrency: 5,
            ramp_rate: 3,
            warmup: Duration::from_secs(0),
            duration: Duration::from_secs(3),
        };
        let mut values = LoginAggregate::from_hurricane(&ledger).report_entries();
        values.extend(online_report_entries(&ledger.online));
        let report = ScenarioReport::record(&scenario, "offline", values);

        let json = report.to_json();
        assert!(json.contains("\"login_attempts\": 9"));
        assert!(json.contains("\"login_admitted\": 5"));
        assert!(json.contains("\"login_refused\": 7"));
        assert!(json.contains("\"login_success_rate\": 0.5555555555555556"));
        assert!(json.contains("\"online\": [3, 5, 5]"));
        assert!(json.contains("\"online_peak\": 5"));
        assert!(json.contains("\"online_final\": 5"));
    }
}
