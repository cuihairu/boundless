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
        };
        let value = report.to_json();
        assert_eq!(value["version"], "9.9.9");
        assert_eq!(value["topology"], "single-cell");
        assert_eq!(value["seed"], 7);
        assert_eq!(value["scenario"], "login-surge");
    }
}
