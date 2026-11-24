//! BW-17-03-01: the machine-readable scenario report envelope
//! (`docs/operations/load-testing.md`, Reports section).
//!
//! The report contract: a scenario run renders a machine-readable
//! document in which the app version, the topology the run targeted and
//! the fixed seed are traceable - the three fields a re-run is compared
//! against (the fixed seed is "what makes a second run comparable to the
//! first"). The measurement sections land in the same document as their
//! items are delivered: login success rate and online count
//! (BW-17-03-02), latency percentiles and timeouts (BW-17-03-03),
//! bandwidth and resource usage (BW-17-03-04).
//!
//! The reference bots have no report layer - their only out-of-process
//! face is the watcher tree (BW-17-01-06) - so this document is the port's
//! own deliverable, defined by the documentation item and rendered with
//! the workspace's JSON codec.

use serde_json::{json, Value};

use crate::executor::Executor;

/// The schema tag every report carries, so machine readers can pin the
/// field layout.
pub const REPORT_SCHEMA: &str = "boundless-load-report/1";

/// BW-17-03-02: the login success rate and online count of one run.
///
/// The aggregator consumes what the bot fleet records - one outcome per
/// log-on attempt and one bot-count sample per observation tick - and
/// reduces them to the two figures the report contract names. The
/// acceptance is verification against known inputs, so every rule here is
/// a fixed arithmetic definition: the rate is successes over attempts
/// (0.0 when nothing was attempted), the online peak is the maximum
/// sample and the online average is the mean of the samples (0.0 with no
/// samples).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LoginStats {
    attempts: u64,
    successes: u64,
    online_peak: u64,
    online_total: u64,
    online_samples: u64,
}

impl LoginStats {
    /// Reduce one run's recordings: a flag per log-on attempt and one
    /// bot-count sample per observation tick.
    pub fn from_run(attempts: &[bool], online: &[u64]) -> Self {
        let mut stats = Self {
            attempts: attempts.len() as u64,
            successes: attempts.iter().filter(|ok| **ok).count() as u64,
            ..Self::default()
        };
        for sample in online {
            stats.online_peak = stats.online_peak.max(*sample);
            stats.online_total += sample;
            stats.online_samples += 1;
        }
        stats
    }

    /// Attempted log-ons.
    pub fn attempts(&self) -> u64 {
        self.attempts
    }

    /// Log-ons that succeeded.
    pub fn successes(&self) -> u64 {
        self.successes
    }

    /// Successes over attempts; 0.0 when nothing was attempted.
    pub fn success_rate(&self) -> f64 {
        if self.attempts == 0 {
            0.0
        } else {
            self.successes as f64 / self.attempts as f64
        }
    }

    /// The largest simultaneous bot count observed.
    pub fn online_peak(&self) -> u64 {
        self.online_peak
    }

    /// The mean bot count over the observation ticks; 0.0 with no
    /// samples.
    pub fn online_average(&self) -> f64 {
        if self.online_samples == 0 {
            0.0
        } else {
            self.online_total as f64 / self.online_samples as f64
        }
    }

    /// The machine-readable section under the report's `login` key.
    pub fn to_json(&self) -> Value {
        json!({
            "attempts": self.attempts,
            "successes": self.successes,
            "success_rate": self.success_rate(),
            "online_peak": self.online_peak,
            "online_average": self.online_average(),
        })
    }
}

/// BW-17-03-03: the latency percentiles and tick timeouts of one run.
///
/// The aggregator sorts one latency sample per observation (milliseconds)
/// and answers the report contract's percentiles with the nearest-rank
/// definition: for `n` samples the p-th percentile is the
/// `ceil(p/100 * n)`-th smallest. The acceptance is verification against a
/// fixed sample, so the definition is pinned by that test. A timeout is
/// not a latency - it is counted, never averaged in.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LatencyStats {
    sorted: Vec<u64>,
    timeouts: u64,
}

impl LatencyStats {
    /// Reduce one run's recordings: a millisecond latency per observation
    /// and the count of ticks that timed out.
    pub fn from_run(samples: &[u64], timeouts: u64) -> Self {
        let mut sorted = samples.to_vec();
        sorted.sort_unstable();
        Self { sorted, timeouts }
    }

    /// The observed samples, smallest first.
    pub fn samples(&self) -> &[u64] {
        &self.sorted
    }

    /// Ticks that timed out; excluded from the percentile sample.
    pub fn timeouts(&self) -> u64 {
        self.timeouts
    }

    /// The `p`-th percentile (0.0-1.0) under the nearest-rank definition:
    /// the `ceil(p * n)`-th smallest sample. 0.0 with no samples; `p`
    /// outside 0.0-1.0 reads as its clamped value, so p95 is a convenient
    /// spelling of 0.95.
    pub fn percentile(&self, p: f64) -> f64 {
        if self.sorted.is_empty() {
            return 0.0;
        }
        let p = p.clamp(0.0, 1.0);
        let n = self.sorted.len();
        let rank = ((p * n as f64).ceil() as usize).max(1);
        self.sorted[rank - 1] as f64
    }

    /// The 95th percentile in milliseconds.
    pub fn p95(&self) -> f64 {
        self.percentile(0.95)
    }

    /// The 99th percentile in milliseconds.
    pub fn p99(&self) -> f64 {
        self.percentile(0.99)
    }

    /// The machine-readable section under the report's `latency` key.
    pub fn to_json(&self) -> Value {
        json!({
            "samples": self.sorted.len(),
            "p95_millis": self.p95(),
            "p99_millis": self.p99(),
            "max_millis": self.sorted.last().copied().unwrap_or(0),
            "timeouts": self.timeouts,
        })
    }
}

/// BW-17-03-04: the bandwidth and resource usage of one run.
///
/// The acceptance asks the report to state the metric definitions (the
/// 口径) and the sampling period (采样周期), so the section carries them
/// explicitly alongside the figures: bandwidth is total bytes over the
/// whole run (not a rate - the sampling period multiplies out), CPU is
/// the load generator's own process as permille of one core with one
/// sample per period, and memory is the peak resident set in bytes. The
/// reference bots have no resource recorder - this is the port's own
/// accounting of its generator, defined by the documentation item.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ResourceStats {
    sampling_period_millis: u64,
    bytes_sent: u64,
    bytes_received: u64,
    cpu_permille_sorted: Vec<u64>,
    peak_rss_bytes: u64,
}

/// The metric definitions the section states, in the order the JSON
/// carries them (`definitions` object).
const CPU_DEFINITION: &str =
    "load-generator process CPU, permille of one core, one sample per period";
const BANDWIDTH_DEFINITION: &str = "total bytes over the whole run, bot transport summed";
const RSS_DEFINITION: &str = "load-generator process peak resident set, bytes";

impl ResourceStats {
    /// Reduce one run's recordings: the observation period every sample
    /// was taken on, the transport totals, the CPU samples and the peak
    /// resident set.
    pub fn from_run(
        sampling_period_millis: u64,
        bytes_sent: u64,
        bytes_received: u64,
        cpu_permille: &[u64],
        peak_rss_bytes: u64,
    ) -> Self {
        let mut cpu_permille_sorted = cpu_permille.to_vec();
        cpu_permille_sorted.sort_unstable();
        Self {
            sampling_period_millis,
            bytes_sent,
            bytes_received,
            cpu_permille_sorted,
            peak_rss_bytes,
        }
    }

    /// The period every CPU sample was taken on, in milliseconds.
    pub fn sampling_period_millis(&self) -> u64 {
        self.sampling_period_millis
    }

    /// Mean CPU over the samples, permille of one core; 0 with no
    /// samples.
    pub fn cpu_mean_permille(&self) -> u64 {
        if self.cpu_permille_sorted.is_empty() {
            0
        } else {
            let total: u64 = self.cpu_permille_sorted.iter().sum();
            total / self.cpu_permille_sorted.len() as u64
        }
    }

    /// Peak CPU sample, permille of one core; 0 with no samples.
    pub fn cpu_peak_permille(&self) -> u64 {
        self.cpu_permille_sorted.last().copied().unwrap_or(0)
    }

    /// The machine-readable section under the report's `resource` key:
    /// the figures, the sampling period, and the metric definitions.
    pub fn to_json(&self) -> Value {
        json!({
            "sampling_period_millis": self.sampling_period_millis,
            "bandwidth": {
                "sent_bytes": self.bytes_sent,
                "received_bytes": self.bytes_received,
            },
            "cpu": {
                "mean_permille": self.cpu_mean_permille(),
                "peak_permille": self.cpu_peak_permille(),
            },
            "peak_rss_bytes": self.peak_rss_bytes,
            "definitions": {
                "bandwidth": BANDWIDTH_DEFINITION,
                "cpu": CPU_DEFINITION,
                "rss": RSS_DEFINITION,
            },
        })
    }
}

/// The traceable envelope of one scenario run: who ran, against what, and
/// with which fixed seed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScenarioReport {
    /// The app version string the run executed.
    pub version: String,
    /// The topology descriptor: the roles the run targeted.
    pub topology: String,
    /// The fixed seed the scenario ran with (`Scenario::fixed_seed`).
    pub seed: u64,
    /// The scenario name (`Scenario::name`).
    pub scenario: String,
    /// The concurrency the run drove (`Scenario::concurrency`).
    pub concurrency: u64,
    /// The stage layout the executor derives for the scenario, in
    /// seconds.
    pub warmup_seconds: u64,
    pub ramp_seconds: u64,
    pub steady_seconds: u64,
    pub total_seconds: u64,
    /// The login/online section (BW-17-03-02); `None` while a run has
    /// recorded nothing.
    pub login: Option<LoginStats>,
    /// The latency/timeout section (BW-17-03-03); `None` while a run has
    /// recorded nothing.
    pub latency: Option<LatencyStats>,
    /// The bandwidth/resource section (BW-17-03-04); `None` while a run
    /// has recorded nothing.
    pub resource: Option<ResourceStats>,
}

impl ScenarioReport {
    /// The envelope of a run of `executor`'s scenario: the version and
    /// topology come from the caller (they describe the deployment under
    /// test, not the scenario file).
    pub fn for_run(executor: &Executor, version: &str, topology: &str) -> Self {
        let scenario = executor.scenario();
        let stages = executor.stages();
        Self {
            version: version.to_string(),
            topology: topology.to_string(),
            seed: scenario.fixed_seed,
            scenario: scenario.name.clone(),
            concurrency: scenario.concurrency,
            warmup_seconds: stages.warmup().as_secs(),
            ramp_seconds: stages.ramp().as_secs(),
            steady_seconds: stages.steady().as_secs(),
            total_seconds: stages.total().as_secs(),
            login: None,
            latency: None,
            resource: None,
        }
    }

    /// The machine-readable form: a JSON object under the pinned
    /// [`REPORT_SCHEMA`] tag.
    pub fn to_json(&self) -> Value {
        json!({
            "schema": REPORT_SCHEMA,
            "version": self.version,
            "topology": self.topology,
            "seed": self.seed,
            "scenario": self.scenario,
            "concurrency": self.concurrency,
            "stages": {
                "warmup_seconds": self.warmup_seconds,
                "ramp_seconds": self.ramp_seconds,
                "steady_seconds": self.steady_seconds,
                "total_seconds": self.total_seconds,
            },
            "login": self.login.as_ref().map(LoginStats::to_json),
            "latency": self.latency.as_ref().map(LatencyStats::to_json),
            "resource": self.resource.as_ref().map(ResourceStats::to_json),
        })
    }

    /// The same document as the string a report file would carry.
    pub fn to_json_string(&self) -> String {
        self.to_json().to_string()
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use crate::executor::Executor;
    use crate::scenario::{AccountPool, Scenario};

    use super::*;

    fn scenario(name: &str, seed: u64, concurrency: u64) -> Scenario {
        Scenario {
            name: name.to_string(),
            fixed_seed: seed,
            account_pool: AccountPool::Generated {
                prefix: "Bot".to_string(),
                count: 10,
                password: String::new(),
            },
            concurrency,
            ramp_rate: 2,
            warmup: Duration::from_secs(30),
            duration: Duration::from_secs(300),
        }
    }

    fn report() -> ScenarioReport {
        let executor = Executor::new(scenario("steady", 42, 10));
        ScenarioReport::for_run(&executor, "0.1.0-nightly", "loginapp+base+cell via bots")
    }

    #[test]
    fn the_traceability_fields_survive_the_json_round_trip() {
        // The acceptance: version, topology and seed are traceable - the
        // rendered document carries each one exactly.
        let value = report().to_json();
        assert_eq!(value["version"], "0.1.0-nightly");
        assert_eq!(value["topology"], "loginapp+base+cell via bots");
        assert_eq!(value["seed"], 42);
        assert_eq!(value["scenario"], "steady");
        assert_eq!(value["concurrency"], 10);

        // And the string form re-parses to the same document.
        let text = report().to_json_string();
        let reparsed: Value = serde_json::from_str(&text).expect("report re-parses");
        assert_eq!(reparsed, value);
    }

    #[test]
    fn the_stage_layout_is_copied_from_the_executor() {
        // 30s warmup, 10 bots / 2 per second ramp (5s), and the scenario
        // duration covers ramp+steady: 300 - 5 = 295 steady.
        let value = report().to_json();
        assert_eq!(value["stages"]["warmup_seconds"], 30);
        assert_eq!(value["stages"]["ramp_seconds"], 5);
        assert_eq!(value["stages"]["steady_seconds"], 295);
        assert_eq!(value["stages"]["total_seconds"], 330);
    }

    #[test]
    fn the_schema_tag_is_stable() {
        // Machine readers pin the field layout through this tag.
        assert_eq!(report().to_json()["schema"], REPORT_SCHEMA);
        assert_eq!(REPORT_SCHEMA, "boundless-load-report/1");
    }

    #[test]
    fn known_inputs_verify_the_statistics() {
        // The acceptance: aggregation against known inputs. Three of four
        // log-ons succeed; the bot counts run 1, 5, 2.
        let stats = LoginStats::from_run(&[true, false, true, true], &[1, 5, 2]);
        assert_eq!(stats.attempts(), 4);
        assert_eq!(stats.successes(), 3);
        assert!((stats.success_rate() - 0.75).abs() < 1e-9);
        assert_eq!(stats.online_peak(), 5);
        assert!((stats.online_average() - 8.0 / 3.0).abs() < 1e-9);

        // The section renders the same figures.
        let section = stats.to_json();
        assert_eq!(section["attempts"], 4);
        assert_eq!(section["successes"], 3);
        assert_eq!(section["success_rate"], 0.75);
        assert_eq!(section["online_peak"], 5);
    }

    #[test]
    fn empty_recordings_aggregate_to_zero() {
        // No attempts -> rate 0.0; no samples -> peak 0, average 0.0.
        let stats = LoginStats::from_run(&[], &[]);
        assert_eq!(stats.success_rate(), 0.0);
        assert_eq!(stats.online_peak(), 0);
        assert_eq!(stats.online_average(), 0.0);
        assert_eq!(stats.to_json()["success_rate"], 0.0);
    }

    #[test]
    fn all_failure_and_all_success_bounds() {
        let failed = LoginStats::from_run(&[false, false], &[0]);
        assert_eq!(failed.success_rate(), 0.0);
        let succeeded = LoginStats::from_run(&[true, true], &[2]);
        assert_eq!(succeeded.success_rate(), 1.0);
        assert_eq!(succeeded.online_peak(), 2);
        assert_eq!(succeeded.online_average(), 2.0);
    }

    #[test]
    fn the_report_carries_the_login_section_when_recorded() {
        // A completed run's envelope renders the login section; a bare
        // one leaves it absent (null), not zero - an unrecorded run and a
        // failed run are different documents.
        let mut completed = report();
        completed.login = Some(LoginStats::from_run(&[true, true], &[2, 2]));
        let value = completed.to_json();
        assert_eq!(value["login"]["success_rate"], 1.0);
        assert_eq!(value["login"]["online_peak"], 2);

        let bare = report().to_json();
        assert!(bare["login"].is_null());
    }

    #[test]
    fn the_fixed_sample_pins_the_percentiles() {
        // The acceptance: 100 samples of i*10 ms - p95 is the 95th
        // smallest (950), p99 the 99th (990) under the nearest-rank
        // definition.
        let samples: Vec<u64> = (1..=100).map(|i| i * 10).collect();
        let stats = LatencyStats::from_run(&samples, 0);
        assert_eq!(stats.p95(), 950.0);
        assert_eq!(stats.p99(), 990.0);
        assert_eq!(stats.to_json()["p95_millis"], 950.0);
        assert_eq!(stats.to_json()["p99_millis"], 990.0);
        assert_eq!(stats.to_json()["max_millis"], 1000);
        assert_eq!(stats.to_json()["samples"], 100);
    }

    #[test]
    fn the_nearest_rank_rounds_up_to_the_smallest_covers() {
        // Three samples [10,20,30]: ceil(0.95*3) = 3 -> the 30ms tail
        // covers the 95th percentile; the median rank is exact.
        let stats = LatencyStats::from_run(&[30, 10, 20], 0);
        assert_eq!(stats.percentile(0.95), 30.0);
        assert_eq!(stats.percentile(0.5), 20.0);
        // The input order is irrelevant - the sample sorts.
        assert_eq!(stats.samples(), &[10, 20, 30]);
    }

    #[test]
    fn timeouts_are_counted_not_averaged_in() {
        // A timeout never joins the latency sample; it only counts.
        let stats = LatencyStats::from_run(&[10, 20], 7);
        assert_eq!(stats.timeouts(), 7);
        assert_eq!(stats.samples().len(), 2);
        assert_eq!(stats.to_json()["timeouts"], 7);
        assert_eq!(stats.to_json()["samples"], 2);
    }

    #[test]
    fn an_empty_latency_section_reads_zero() {
        let stats = LatencyStats::from_run(&[], 0);
        assert_eq!(stats.p95(), 0.0);
        assert_eq!(stats.p99(), 0.0);
        assert_eq!(stats.to_json()["max_millis"], 0);
    }

    #[test]
    fn the_report_carries_the_latency_section_when_recorded() {
        let mut completed = report();
        completed.latency = Some(LatencyStats::from_run(&[10, 20], 1));
        let value = completed.to_json();
        assert_eq!(value["latency"]["p95_millis"], 20.0);
        assert_eq!(value["latency"]["timeouts"], 1);

        let bare = report().to_json();
        assert!(bare["latency"].is_null());
    }

    #[test]
    fn the_section_states_the_definitions_and_the_period() {
        // The acceptance: the metric definitions and the sampling period
        // are part of the document, not folklore.
        let stats = ResourceStats::from_run(1000, 4096, 8192, &[500, 700], 1 << 20);
        assert_eq!(stats.sampling_period_millis(), 1000);
        let section = stats.to_json();
        assert_eq!(section["sampling_period_millis"], 1000);
        assert_eq!(section["definitions"]["cpu"], CPU_DEFINITION);
        assert_eq!(section["definitions"]["bandwidth"], BANDWIDTH_DEFINITION);
        assert_eq!(section["definitions"]["rss"], RSS_DEFINITION);
        assert_eq!(section["cpu"]["mean_permille"], 600);
        assert_eq!(section["cpu"]["peak_permille"], 700);
        assert_eq!(section["bandwidth"]["sent_bytes"], 4096);
        assert_eq!(section["bandwidth"]["received_bytes"], 8192);
        assert_eq!(section["peak_rss_bytes"], 1 << 20);
    }

    #[test]
    fn cpu_samples_sort_for_the_peak_and_mean() {
        // Input order is irrelevant; an empty sample set reads zero.
        let stats = ResourceStats::from_run(500, 0, 0, &[900, 100], 0);
        assert_eq!(stats.cpu_mean_permille(), 500);
        assert_eq!(stats.cpu_peak_permille(), 900);
        let empty = ResourceStats::from_run(500, 0, 0, &[], 0);
        assert_eq!(empty.cpu_mean_permille(), 0);
        assert_eq!(empty.cpu_peak_permille(), 0);
    }

    #[test]
    fn the_report_carries_the_resource_section_when_recorded() {
        let mut completed = report();
        completed.resource = Some(ResourceStats::from_run(1000, 1, 2, &[1], 3));
        let value = completed.to_json();
        assert_eq!(value["resource"]["sampling_period_millis"], 1000);
        assert_eq!(value["resource"]["peak_rss_bytes"], 3);

        let bare = report().to_json();
        assert!(bare["resource"].is_null());
    }

    #[test]
    fn a_report_can_be_built_directly() {
        // A caller that recorded its own run states the envelope fields
        // itself; the rendering is the same document.
        let report = ScenarioReport {
            version: "9.9.9".to_string(),
            topology: "single-cell".to_string(),
            seed: 7,
            scenario: "login-surge".to_string(),
            concurrency: 3,
            warmup_seconds: 0,
            ramp_seconds: 0,
            steady_seconds: 0,
            total_seconds: 0,
            login: None,
            latency: None,
            resource: None,
        };
        let value = report.to_json();
        assert_eq!(value["version"], "9.9.9");
        assert_eq!(value["topology"], "single-cell");
        assert_eq!(value["seed"], 7);
        assert_eq!(value["scenario"], "login-surge");
    }
}
