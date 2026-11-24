//! Aggregated run statistics (BW-17-03-02).
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hurricane::run_login_hurricane;
    use crate::report::ScenarioReport;
    use crate::scenario::{AccountPool, Scenario};
    use std::time::Duration;

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
