//! Machine-readable scenario results (BW-17-03-01).
//!
//! The reference bots keep no results file — the report contract is
//! Boundless-side (`docs/operations/load-testing.md`, "Reports"): a run
//! produces a machine-readable document in which the generator version,
//! the topology it ran against and the driving seed are all traceable.
//! The report is plain JSON written by a small deterministic writer (the
//! workspace carries no JSON dependency), so two runs of the same fixed
//! seed produce byte-identical documents and a third party can diff them.

use std::fs;
use std::path::Path;

use crate::scenario::Scenario;

/// The report schema tag. Bumped when a field changes shape, so old
/// reports stay parseable by version.
pub const REPORT_SCHEMA: i64 = 1;

/// One named value in the report payload.
#[derive(Clone, Debug, PartialEq)]
pub struct ReportEntry {
    /// The key, unique per report.
    pub key: String,
    /// The value.
    pub value: ReportValue,
}

impl ReportEntry {
    /// A `(key, value)` pair.
    pub fn new(key: &str, value: impl Into<ReportValue>) -> Self {
        Self {
            key: key.to_string(),
            value: value.into(),
        }
    }
}

/// A JSON-shaped report value.
#[derive(Clone, Debug, PartialEq)]
pub enum ReportValue {
    /// A whole number.
    Int(i64),
    /// A float; non-finite values render as `null` (JSON has no NaN).
    Float(f64),
    /// A string.
    Str(String),
    /// An ordered list.
    List(Vec<ReportValue>),
}

impl From<i64> for ReportValue {
    fn from(value: i64) -> Self {
        ReportValue::Int(value)
    }
}

impl From<f64> for ReportValue {
    fn from(value: f64) -> Self {
        ReportValue::Float(value)
    }
}

impl From<&str> for ReportValue {
    fn from(value: &str) -> Self {
        ReportValue::Str(value.to_string())
    }
}

/// The machine-readable result of one scenario run: the traceability
/// envelope (scenario, generator version, topology, seed) plus the run's
/// recorded values.
pub struct ScenarioReport {
    /// The scenario's `name`.
    pub scenario: String,
    /// `"load <pkg version>"` — which generator build produced the run.
    pub generator: String,
    /// The run target, e.g. `"offline"` — every delivered scenario runs
    /// without a live server, and the field is what says so.
    pub topology: String,
    /// The scenario's fixed seed.
    pub seed: u64,
    /// The recorded run values.
    pub values: Vec<ReportEntry>,
}

impl ScenarioReport {
    /// Wrap a run's values with the traceability envelope.
    pub fn record(scenario: &Scenario, topology: &str, values: Vec<ReportEntry>) -> Self {
        Self {
            scenario: scenario.name.clone(),
            generator: format!("load {}", env!("CARGO_PKG_VERSION")),
            topology: topology.to_string(),
            seed: scenario.fixed_seed,
            values,
        }
    }

    /// The JSON document. Deterministic: field order is fixed, floats
    /// carry Rust's shortest round-trip form, non-finite floats collapse
    /// to `null`.
    pub fn to_json(&self) -> String {
        let mut out = String::with_capacity(256);
        out.push_str("{\n");
        out.push_str("  \"schema\": ");
        out.push_str(&REPORT_SCHEMA.to_string());
        out.push_str(",\n  \"scenario\": ");
        write_json_string(&mut out, &self.scenario);
        out.push_str(",\n  \"generator\": ");
        write_json_string(&mut out, &self.generator);
        out.push_str(",\n  \"topology\": ");
        write_json_string(&mut out, &self.topology);
        out.push_str(",\n  \"seed\": ");
        out.push_str(&self.seed.to_string());
        out.push_str(",\n  \"values\": {");
        for (index, entry) in self.values.iter().enumerate() {
            if index != 0 {
                out.push(',');
            }
            out.push('\n');
            out.push_str("    ");
            write_json_string(&mut out, &entry.key);
            out.push_str(": ");
            write_json_value(&mut out, &entry.value);
        }
        if !self.values.is_empty() {
            out.push('\n');
            out.push_str("  }");
        } else {
            out.push('}');
        }
        out.push_str("\n}\n");
        out
    }

    /// Write the document to `path`.
    pub fn write_json(&self, path: &Path) -> std::io::Result<()> {
        fs::write(path, self.to_json())
    }
}

/// `operator<<( std::string )` for JSON: quote and escape. Control
/// characters use the shortest legal form — the named escapes for the
/// JSON specials, `\u00xx` for the rest.
fn write_json_string(out: &mut String, value: &str) {
    out.push('"');
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

fn write_json_value(out: &mut String, value: &ReportValue) {
    match value {
        ReportValue::Int(v) => out.push_str(&v.to_string()),
        ReportValue::Float(v) => {
            if v.is_finite() {
                out.push_str(&format!("{}", v));
            } else {
                out.push_str("null");
            }
        }
        ReportValue::Str(s) => write_json_string(out, s),
        ReportValue::List(items) => {
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index != 0 {
                    out.push_str(", ");
                }
                write_json_value(out, item);
            }
            out.push(']');
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hurricane::run_login_hurricane;
    use crate::scenario::{AccountPool, Scenario};
    use std::time::Duration;

    /// A small login-hurricane scenario, the shape the scenario suite
    /// saves as `tests/load/scenarios/login-hurricane.xml`.
    fn hurricane_scenario() -> Scenario {
        Scenario {
            name: "login-hurricane".to_string(),
            fixed_seed: 161803,
            account_pool: AccountPool::Generated {
                prefix: "Bot".to_string(),
                count: 12,
                password: String::new(),
            },
            concurrency: 12,
            ramp_rate: 5,
            warmup: Duration::from_secs(1),
            duration: Duration::from_secs(9),
        }
    }

    /// The acceptance face: a sample scenario run produces a report in
    /// which version, topology and seed are traceable — the exact bytes
    /// of a hurricane run's report, written out in full.
    #[test]
    fn a_sample_scenario_run_produces_a_traceable_report() {
        let scenario = hurricane_scenario();
        let ledger = run_login_hurricane(&scenario, 512, scenario.fixed_seed);
        let report = ScenarioReport::record(
            &scenario,
            "offline",
            vec![
                ReportEntry::new(
                    "per_second",
                    ReportValue::List(
                        ledger
                            .per_second
                            .iter()
                            .map(|&n| ReportValue::Int(n as i64))
                            .collect(),
                    ),
                ),
                ReportEntry::new("refused", ledger.refused as i64),
            ],
        );

        let per_second = ledger
            .per_second
            .iter()
            .map(|&n| n.to_string())
            .collect::<Vec<_>>()
            .join(", ");
        let expected = format!(
            "{{\n  \"schema\": 1,\n  \"scenario\": \"login-hurricane\",\n  \
             \"generator\": \"load {version}\",\n  \"topology\": \"offline\",\n  \
             \"seed\": 161803,\n  \"values\": {{\n    \"per_second\": [{ps}],\n    \
             \"refused\": {refused}\n  }}\n}}\n",
            version = env!("CARGO_PKG_VERSION"),
            ps = per_second,
            refused = ledger.refused
        );
        assert_eq!(report.to_json(), expected);

        // The traceability triple, machine-readably: every field is a
        // plain JSON scalar a consumer can grep out.
        assert!(report.to_json().contains("\"seed\": 161803"));
        assert!(report.to_json().contains("\"topology\": \"offline\""));
        assert!(report.to_json().contains(&format!(
            "\"generator\": \"load {}\"",
            env!("CARGO_PKG_VERSION")
        )));
    }

    /// Two runs of the same fixed seed produce byte-identical documents;
    /// the envelope follows the scenario, not the call site.
    #[test]
    fn the_report_is_deterministic_across_runs() {
        let scenario = hurricane_scenario();
        let mut reports = Vec::new();
        for _ in 0..2 {
            let ledger = run_login_hurricane(&scenario, 512, scenario.fixed_seed);
            reports.push(ScenarioReport::record(
                &scenario,
                "offline",
                vec![ReportEntry::new(
                    "per_second",
                    ReportValue::List(
                        ledger
                            .per_second
                            .iter()
                            .map(|&n| ReportValue::Int(n as i64))
                            .collect(),
                    ),
                )],
            ));
        }
        assert_eq!(reports[0].to_json(), reports[1].to_json());
    }

    /// Strings escape the JSON specials; control characters take the
    /// `\u00xx` form so the document stays one line per record.
    #[test]
    fn strings_escape_the_json_specials() {
        let mut out = String::new();
        write_json_string(&mut out, "quote\" back\\ line\n return\r tab\t bell\u{7}");
        assert_eq!(
            out,
            "\"quote\\\" back\\\\ line\\n return\\r tab\\t bell\\u0007\""
        );
    }

    /// Floats carry Rust's shortest round-trip form; non-finite floats
    /// collapse to `null` because JSON has no NaN.
    #[test]
    fn floats_render_shortest_and_non_finite_becomes_null() {
        let mut out = String::new();
        write_json_value(&mut out, &ReportValue::Float(2.5));
        assert_eq!(out, "2.5");
        out.clear();
        write_json_value(&mut out, &ReportValue::Float(1.0));
        assert_eq!(out, "1");
        out.clear();
        write_json_value(&mut out, &ReportValue::Float(f64::NAN));
        assert_eq!(out, "null");
        out.clear();
        write_json_value(&mut out, &ReportValue::Float(f64::NEG_INFINITY));
        assert_eq!(out, "null");
    }

    /// Lists render inline, empty payloads keep the `values` object
    /// empty, and the document ends with a newline.
    #[test]
    fn lists_and_empty_payloads_render_compactly() {
        let scenario = hurricane_scenario();
        let report = ScenarioReport::record(
            &scenario,
            "offline",
            vec![
                ReportEntry::new("empty", ReportValue::List(Vec::new())),
                ReportEntry::new(
                    "mixed",
                    ReportValue::List(vec![
                        ReportValue::Int(1),
                        ReportValue::Str("two".to_string()),
                    ]),
                ),
            ],
        );
        let json = report.to_json();
        assert!(json.contains("\"empty\": [],\n    \"mixed\": [1, \"two\"]"));
        assert!(json.ends_with("\n}\n"));

        let empty = ScenarioReport::record(&scenario, "offline", Vec::new());
        assert!(empty.to_json().contains("\"values\": {}"));
    }

    /// `write_json` round-trips through the file system byte for byte.
    #[test]
    fn write_json_round_trips_through_a_file() {
        let scenario = hurricane_scenario();
        let report = ScenarioReport::record(
            &scenario,
            "offline",
            vec![
                ReportEntry::new("refused", 0),
                ReportEntry::new("duration", 9.0f64),
                ReportEntry::new("kind", "login-hurricane"),
            ],
        );
        let path = std::env::temp_dir().join(format!("load-report-{}.json", std::process::id()));
        report.write_json(&path).expect("write");
        let on_disk = fs::read_to_string(&path).expect("read back");
        assert_eq!(on_disk, report.to_json());
        std::fs::remove_file(&path).expect("clean up");
    }
}
