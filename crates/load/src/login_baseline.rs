//! The login and resource-download baseline (OPT-06-01-01).
//!
//! Records a fixed-seed run's **peak** online count, **refusal rate** and
//! **download transfer latency** so the login/download rows that follow have a
//! number to compare against. It changes no scheduling or dispatch policy —
//! like the load hotspot baseline, it composes the faces that already exist
//! and saves the machine-checkable numbers.
//!
//! ## What is recorded and why
//!
//! - **Peak / refusal** come from the login hurricane (`run_login_hurricane`):
//!   the online-count series' maximum and the FD-budget refusals over the
//!   attempts the ramp issued. Both are deterministic from the fixed seed.
//! - **Download transfer latency** comes from the reference download face
//!   (`server_runtime::ServerDownloads::add_to_bundle`): a payload is streamed
//!   at most a bundle's worth (`BUNDLE_BYTES`) per mini-tick, so the transfer
//!   time of a payload is `ceil(payload / BUNDLE_BYTES)` mini-ticks — the
//!   deterministic floor the reference's chunking imposes, the same number the
//!   server-download tests pin. The queue memory is the bytes the in-flight
//!   download holds at its peak.
//!
//! The offline hurricane is simulated time, so there is no wall-clock login
//! *latency* to record — the recorded latency is the download transfer time,
//! which is deterministic and reproducible. This is stated on the report so a
//! reader never mistakes the tick floor for a measured round trip.

use crate::hurricane::{HurricaneLedger, run_login_hurricane};
use crate::metrics::{LoginAggregate, OnlineSummary, online_summary};
use crate::scenario::Scenario;

/// The most a bundle streams per mini-tick (`addToBundle`, the bundle's
/// payload budget). The reference caps a bundle's message content; the
/// download face chunks the payload to that budget, so this is the per-tick
/// transfer ceiling the baseline models.
pub const BUNDLE_BYTES: usize = 1024;

/// The resource-download transfer record of one run.
#[derive(Clone, Debug, PartialEq)]
pub struct DownloadWindow {
    /// The payload size streamed.
    pub payload_bytes: usize,
    /// The mini-ticks the payload takes at `BUNDLE_BYTES` per tick — the
    /// deterministic transfer latency.
    pub ticks: u64,
    /// The peak bytes held in the in-flight download queue at once.
    pub queue_bytes: usize,
}

impl DownloadWindow {
    /// The transfer window for one payload at the bundle's per-tick budget.
    pub fn new(payload_bytes: usize) -> Self {
        let ticks = if payload_bytes == 0 {
            0
        } else {
            payload_bytes.div_ceil(BUNDLE_BYTES) as u64
        };
        Self {
            payload_bytes,
            ticks,
            queue_bytes: payload_bytes,
        }
    }

    /// The report entries for the transfer window.
    pub fn report_entries(&self) -> Vec<crate::report::ReportEntry> {
        use crate::report::ReportEntry;
        vec![
            ReportEntry::new("download_payload_bytes", self.payload_bytes as i64),
            ReportEntry::new("download_ticks", self.ticks as i64),
            ReportEntry::new("download_queue_bytes", self.queue_bytes as i64),
        ]
    }
}

/// The login-and-download baseline of one run: the login aggregate, the
/// online summary and the download transfer window.
#[derive(Clone, Debug, PartialEq)]
pub struct LoginBaseline {
    pub login: LoginAggregate,
    pub online: OnlineSummary,
    pub download: DownloadWindow,
    /// Bots the FD budget refused over every bot the ramp tried to admit.
    /// The hurricane counts a refusal when an admission is attempted and the
    /// budget is spent (`ledger.refused`), so the denominator is admitted plus
    /// refused — the rate never claims a send was issued for a refused bot.
    pub refusal_rate: f64,
}

impl LoginBaseline {
    /// Record the baseline for a fixed-seed run: drive the hurricane, then
    /// the download window for the given payload.
    pub fn record(scenario: &Scenario, fd_limit: i32, seed: u64, payload_bytes: usize) -> Self {
        let ledger: HurricaneLedger = run_login_hurricane(scenario, fd_limit, seed);
        let admitted = ledger.names.len() as u64;
        let refused = ledger.refused as u64;
        Self {
            login: LoginAggregate::from_hurricane(&ledger),
            online: online_summary(&ledger.online),
            download: DownloadWindow::new(payload_bytes),
            refusal_rate: match admitted + refused {
                0 => 0.0,
                total => refused as f64 / total as f64,
            },
        }
    }

    /// The report entries the baseline saves (login + online + download).
    pub fn report_entries(&self) -> Vec<crate::report::ReportEntry> {
        let mut entries = self.login.report_entries();
        entries.push(crate::report::ReportEntry::new(
            "online_peak",
            self.online.peak as i64,
        ));
        entries.push(crate::report::ReportEntry::new(
            "online_final",
            self.online.final_count as i64,
        ));
        entries.push(crate::report::ReportEntry::new(
            "login_refusal_rate",
            self.refusal_rate,
        ));
        entries.extend(self.download.report_entries());
        entries
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scenario::AccountPool;
    use std::time::Duration;

    fn scenario(concurrency: u64, ramp_rate: u64, seed: u64) -> Scenario {
        Scenario {
            name: "login-hurricane".to_string(),
            fixed_seed: seed,
            account_pool: AccountPool::Generated {
                prefix: "Bot".to_string(),
                count: concurrency.max(1),
                password: String::new(),
            },
            concurrency,
            ramp_rate,
            warmup: Duration::from_secs(1),
            duration: Duration::from_secs(9),
        }
    }

    /// A download window is the ceil of the payload over the per-tick budget.
    #[test]
    fn the_download_window_is_the_ceil_over_the_bundle_budget() {
        assert_eq!(DownloadWindow::new(0).ticks, 0);
        assert_eq!(DownloadWindow::new(1).ticks, 1);
        assert_eq!(DownloadWindow::new(BUNDLE_BYTES).ticks, 1);
        assert_eq!(DownloadWindow::new(BUNDLE_BYTES + 1).ticks, 2);
        assert_eq!(DownloadWindow::new(3 * BUNDLE_BYTES + 1).ticks, 4);
    }

    /// The queue memory is the payload size the in-flight download holds.
    #[test]
    fn the_queue_bytes_are_the_payload_size() {
        let window = DownloadWindow::new(2 * BUNDLE_BYTES + 7);
        assert_eq!(window.queue_bytes, 2 * BUNDLE_BYTES + 7);
    }

    /// The baseline records the hurricane's peak, refusals and the download
    /// window for a fixed seed.
    #[test]
    fn the_baseline_records_peak_refusal_and_download() {
        let scenario = scenario(4, 2, 12345);
        let baseline = LoginBaseline::record(
            &scenario,
            500 * crate::bot::FD_PER_BOT,
            scenario.fixed_seed,
            5000,
        );
        assert_eq!(baseline.online.peak, baseline.online.final_count);
        assert!(baseline.login.attempts >= baseline.login.admitted);
        assert_eq!(baseline.download.ticks, 5);
        assert_eq!(baseline.download.queue_bytes, 5000);
    }

    /// A fixed seed reproduces the baseline bit for bit.
    #[test]
    fn the_baseline_is_reproducible_from_the_fixed_seed() {
        let a = LoginBaseline::record(
            &scenario(4, 2, 999),
            500 * crate::bot::FD_PER_BOT,
            999,
            4096,
        );
        let b = LoginBaseline::record(
            &scenario(4, 2, 999),
            500 * crate::bot::FD_PER_BOT,
            999,
            4096,
        );
        assert_eq!(a, b);
    }

    /// The refusal rate is refused over the bots the ramp tried to admit; an
    /// all-admitted run has none.
    #[test]
    fn the_refusal_rate_reflects_the_fd_budget() {
        // A tiny budget refuses bots the ramp still tries to admit.
        let tight = LoginBaseline::record(&scenario(6, 3, 7), 3 * crate::bot::FD_PER_BOT, 7, 1024);
        assert!(tight.login.refused > 0);
        assert!(tight.refusal_rate > 0.0);
        let wide = LoginBaseline::record(&scenario(4, 2, 7), 500 * crate::bot::FD_PER_BOT, 7, 1024);
        assert_eq!(wide.login.refused, 0);
        assert_eq!(wide.refusal_rate, 0.0);
    }
}

#[cfg(test)]
mod fixtures {
    use super::*;
    use crate::report::ScenarioReport;

    /// The saved login-hurricane scenario at the harness envelope.
    fn saved_scenario() -> Option<crate::scenario::Scenario> {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/load/scenarios/login-hurricane.xml");
        let raw = std::fs::read(&path).ok()?;
        crate::scenario::Scenario::load_xml(&raw).ok()
    }

    /// Print the recorded baseline for the saved scenario — the fixture the
    /// `docs/review/opt-06-01-01-login-baseline.md` page saves. A fixed
    /// seed reproduces this output byte for byte.
    #[test]
    fn print_the_login_baseline_fixture() {
        let Some(scenario) = saved_scenario() else {
            return;
        };
        let baseline = LoginBaseline::record(
            &scenario,
            500 * crate::bot::FD_PER_BOT,
            scenario.fixed_seed,
            4096,
        );
        let report = ScenarioReport::record(&scenario, "offline", baseline.report_entries());
        println!(
            "LOGIN_BASELINE_JSON\n{}\nLOGIN_BASELINE_JSON",
            report.to_json()
        );
    }
}
