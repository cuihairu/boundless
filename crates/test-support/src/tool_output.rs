//! BW-27-03-01/02: the tool's stable machine-readable output.
//!
//! The CLI, the IDE and an independent protocol client read the tool's
//! answers through one contract. Two documents answer the two questions — the
//! running status, and the process selection list — and both are
//! deterministic: the same input renders the same bytes, every field appears
//! in a fixed order, and values are JSON-escaped, so a client parses either
//! with one field set and no guesses.
//!
//! The test and load runs report the same way: one line per result, each
//! carrying the source location or the produced artifact path as a
//! `path:line` / `path` token, so an editor's terminal links open it.

use crate::dap_session::Adapter;
use crate::dap_targets::DebugTarget;
use crate::process_status::ProcessStatus;

/// The output contract version, so a client tells a contract mismatch apart
/// from a bad answer.
pub const CONTRACT_VERSION: &str = "1";

/// Escape a string for a JSON string literal, mirroring the writer the trace
/// dump uses so the whole crate escapes one way.
fn escape(value: &str) -> String {
    let mut out = String::new();
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                use std::fmt::Write as _;
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out
}

/// The adapter's name, as a client keys on it.
fn adapter_name(adapter: Adapter) -> &'static str {
    match adapter {
        Adapter::Python => "python",
        Adapter::Rust => "rust",
    }
}

/// The status query answer: the cluster as the tool sees it at `now`.
///
/// ```json
/// {"tool":"status","version":"1","snapshot_at":1000,
///  "processes":[{"role":"cellapp01","pid":100,"ready":true}]}
/// ```
///
/// Processes are ordered by role, then pid. A process without a pid reports
/// `null`, and `ready` reflects its latest readiness announcement.
pub fn status_query(now: u64, processes: &[&ProcessStatus]) -> String {
    let mut sorted: Vec<&ProcessStatus> = processes.to_vec();
    sorted.sort_by(|a, b| a.role.cmp(&b.role).then_with(|| a.pid.cmp(&b.pid)));
    let mut out = String::new();
    out.push_str("{\"tool\":\"status\",\"version\":\"");
    out.push_str(CONTRACT_VERSION);
    out.push_str("\",\"snapshot_at\":");
    out.push_str(&now.to_string());
    out.push_str(",\"processes\":[");
    for (index, status) in sorted.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        out.push_str("{\"role\":\"");
        out.push_str(&escape(&status.role));
        out.push_str("\",\"pid\":");
        match status.pid {
            Some(pid) => out.push_str(&pid.to_string()),
            None => out.push_str("null"),
        }
        out.push_str(",\"ready\":");
        out.push_str(if status.is_ready() { "true" } else { "false" });
        out.push('}');
    }
    out.push_str("]}");
    out
}

/// The process selection answer: one target per line, so an editor or a
/// client picks a line and parses it on its own. Lines come in pid order, and
/// each is a self-contained JSON object.
pub fn targets_jsonl(targets: &[DebugTarget]) -> String {
    let mut sorted: Vec<&DebugTarget> = targets.iter().collect();
    sorted.sort_by_key(|target| target.pid);
    let mut out = String::new();
    for target in sorted {
        out.push_str("{\"id\":\"");
        out.push_str(&escape(&target.id));
        out.push_str("\",\"role\":\"");
        out.push_str(&escape(&target.role));
        out.push_str("\",\"pid\":");
        out.push_str(&target.pid.to_string());
        out.push_str(",\"adapter\":\"");
        out.push_str(&escape(adapter_name(target.adapter)));
        out.push_str("\",\"address\":\"");
        out.push_str(&escape(&target.address));
        out.push_str("\"}\n");
    }
    out
}

/// One test's outcome, for the stable test output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestResult {
    /// The test's full name.
    pub name: String,
    /// The source file the test is declared in.
    pub file: String,
    /// The line the test is declared on.
    pub line: usize,
    /// Whether the test passed.
    pub passed: bool,
}

/// The stable test output: one line per result, ordered by test name.
///
/// ```text
/// PASS crates/server/src/cellapp.rs:120 space::CellApp::update
/// FAIL crates/server/src/space.rs:88 space::CellApp::update
/// ```
///
/// The `file:line` token is a link the editor's terminal opens, so a failing
/// test jumps straight to its source.
pub fn test_output(results: &[TestResult]) -> String {
    let mut sorted: Vec<&TestResult> = results.iter().collect();
    sorted.sort_by(|a, b| a.name.cmp(&b.name));
    let mut out = String::new();
    for result in sorted {
        out.push_str(if result.passed { "PASS " } else { "FAIL " });
        out.push_str(&result.file);
        out.push(':');
        out.push_str(&result.line.to_string());
        out.push(' ');
        out.push_str(&result.name);
        out.push('\n');
    }
    out
}

/// One load run's artifact, for the stable load output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadReport {
    /// The scenario that ran.
    pub scenario: String,
    /// The artifact the run produced: the report file.
    pub path: String,
    /// How long the run took.
    pub duration_ms: u64,
}

/// The stable load output: one line per report, ordered by scenario.
///
/// ```text
/// REPORT load_scenario 1200ms target/load/scenario.json
/// ```
///
/// The trailing path is the produced artifact; the editor's terminal opens
/// it, so a run's report is one click away.
pub fn load_output(reports: &[LoadReport]) -> String {
    let mut sorted: Vec<&LoadReport> = reports.iter().collect();
    sorted.sort_by(|a, b| a.scenario.cmp(&b.scenario));
    let mut out = String::new();
    for report in sorted {
        out.push_str("REPORT ");
        out.push_str(&report.scenario);
        out.push(' ');
        out.push_str(&report.duration_ms.to_string());
        out.push_str("ms ");
        out.push_str(&report.path);
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn process(role: &str, pid: Option<u32>, ready: bool) -> ProcessStatus {
        let mut status = ProcessStatus {
            role: role.to_string(),
            pid,
            ..ProcessStatus::default()
        };
        if ready {
            status.readings.push(crate::process_status::StatusReading {
                item: "ready".to_string(),
                value: "true".to_string(),
                source: "readiness announcement".to_string(),
                collected_at: 1_000,
            });
        }
        status
    }

    fn target(id: &str, pid: u32, role: &str, adapter: Adapter, address: &str) -> DebugTarget {
        DebugTarget {
            id: id.to_string(),
            pid,
            role: role.to_string(),
            adapter,
            address: address.to_string(),
        }
    }

    #[test]
    fn the_status_query_answer_is_one_fixed_document() {
        let cellapp = process("cellapp01", Some(100), true);
        let baseapp = process("baseapp01", Some(200), false);

        // One field set, one order: the answer an independent client parses.
        let doc = status_query(1_000, &[&cellapp, &baseapp]);
        assert_eq!(
            doc,
            "{\"tool\":\"status\",\"version\":\"1\",\"snapshot_at\":1000,\
             \"processes\":[{\"role\":\"baseapp01\",\"pid\":200,\"ready\":false},\
             {\"role\":\"cellapp01\",\"pid\":100,\"ready\":true}]}"
        );
    }

    #[test]
    fn the_status_query_answer_orders_processes_by_role() {
        let zebra = process("zebraapp", None, false);
        let alpha = process("alphaapp", Some(1), true);

        // Input order is not the contract: the answer sorts by role.
        let doc = status_query(0, &[&zebra, &alpha]);
        assert!(doc.find("alphaapp").unwrap() < doc.find("zebraapp").unwrap());
    }

    #[test]
    fn a_process_without_a_pid_reports_null_and_not_ready() {
        let ghost = process("ghostapp", None, false);
        let doc = status_query(5, &[&ghost]);
        assert!(doc.contains("\"pid\":null"));
        assert!(doc.contains("\"ready\":false"));
        assert!(doc.contains("\"snapshot_at\":5"));
    }

    #[test]
    fn the_status_query_answer_escapes_roles() {
        let weird = process("app\"\\one", Some(1), true);
        let doc = status_query(0, &[&weird]);
        // The quote and backslash are escaped, so the document stays parseable.
        assert!(doc.contains("app\\\"\\\\one"));
        assert!(!doc.contains("app\"\\one"));
    }

    #[test]
    fn the_process_selection_answer_is_one_object_per_line() {
        let targets = vec![
            target(
                "cellapp-1102",
                1102,
                "cellapp",
                Adapter::Rust,
                "127.0.0.1:12346",
            ),
            target(
                "baseapp-2201",
                2201,
                "baseapp",
                Adapter::Python,
                "127.0.0.1:5678",
            ),
        ];

        // One self-contained JSON object per line, in pid order.
        let jsonl = targets_jsonl(&targets);
        assert_eq!(
            jsonl,
            "{\"id\":\"cellapp-1102\",\"role\":\"cellapp\",\"pid\":1102,\
             \"adapter\":\"rust\",\"address\":\"127.0.0.1:12346\"}\n\
             {\"id\":\"baseapp-2201\",\"role\":\"baseapp\",\"pid\":2201,\
             \"adapter\":\"python\",\"address\":\"127.0.0.1:5678\"}\n"
        );
    }

    #[test]
    fn an_independent_client_parses_the_selection_line_by_line() {
        let targets = vec![
            target(
                "cellapp-1101",
                1101,
                "cellapp",
                Adapter::Rust,
                "127.0.0.1:12345",
            ),
            target(
                "baseapp-2202",
                2202,
                "baseapp",
                Adapter::Python,
                "127.0.0.1:5679",
            ),
        ];
        let jsonl = targets_jsonl(&targets);

        // A client splits on newline and parses each object on its own.
        let lines: Vec<&str> = jsonl.lines().filter(|line| !line.is_empty()).collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(
            lines[0],
            "{\"id\":\"cellapp-1101\",\"role\":\"cellapp\",\"pid\":1101,\
                    \"adapter\":\"rust\",\"address\":\"127.0.0.1:12345\"}"
        );
        assert!(lines[0].starts_with("{\"id\":") && lines[0].ends_with('}'));
        assert!(lines[1].starts_with("{\"id\":") && lines[1].ends_with('}'));
    }

    #[test]
    fn the_same_input_renders_the_same_bytes() {
        let cellapp = process("cellapp01", Some(100), true);
        let targets = vec![target(
            "cellapp-1101",
            1101,
            "cellapp",
            Adapter::Rust,
            "127.0.0.1:12345",
        )];

        // Deterministic output: the contract is byte-for-byte stable.
        assert_eq!(status_query(1, &[&cellapp]), status_query(1, &[&cellapp]));
        assert_eq!(targets_jsonl(&targets), targets_jsonl(&targets));
    }

    #[test]
    fn an_empty_selection_lists_nothing() {
        assert_eq!(targets_jsonl(&[]), "");
    }

    fn test_result(name: &str, file: &str, line: usize, passed: bool) -> TestResult {
        TestResult {
            name: name.to_string(),
            file: file.to_string(),
            line,
            passed,
        }
    }

    #[test]
    fn the_test_output_lists_each_result_with_its_location() {
        let results = vec![
            test_result(
                "space::CellApp::update",
                "crates/server/src/space.rs",
                88,
                false,
            ),
            test_result(
                "space::CellApp::tick",
                "crates/server/src/cellapp.rs",
                120,
                true,
            ),
        ];

        // One line per result: the outcome, the clickable location, the name.
        assert_eq!(
            test_output(&results),
            "PASS crates/server/src/cellapp.rs:120 space::CellApp::tick\n\
             FAIL crates/server/src/space.rs:88 space::CellApp::update\n"
        );
    }

    #[test]
    fn a_test_location_is_one_clickable_token() {
        let results = vec![test_result(
            "space::CellApp::update",
            "crates/server/src/space.rs",
            88,
            false,
        )];
        let output = test_output(&results);

        // The editor's terminal link regex finds `file:line` as one token.
        let line = output.trim_end();
        let location = line.split(' ').nth(1).unwrap();
        assert_eq!(location, "crates/server/src/space.rs:88");
        assert_eq!(line.split(' ').count(), 3);
    }

    #[test]
    fn the_test_output_orders_results_by_name() {
        let results = vec![
            test_result("zebra", "a.rs", 1, true),
            test_result("alpha", "b.rs", 2, true),
        ];
        let output = test_output(&results);
        assert!(output.find("alpha").unwrap() < output.find("zebra").unwrap());
    }

    #[test]
    fn the_load_output_reports_each_artifact() {
        let reports = vec![
            LoadReport {
                scenario: "load_scenario".to_string(),
                path: "target/load/scenario.json".to_string(),
                duration_ms: 1200,
            },
            LoadReport {
                scenario: "bots_compare".to_string(),
                path: "target/load/bots.json".to_string(),
                duration_ms: 450,
            },
        ];

        // The trailing path is the artifact the editor opens.
        assert_eq!(
            load_output(&reports),
            "REPORT bots_compare 450ms target/load/bots.json\n\
             REPORT load_scenario 1200ms target/load/scenario.json\n"
        );
    }

    #[test]
    fn a_load_artifact_is_the_trailing_token() {
        let reports = vec![LoadReport {
            scenario: "load_scenario".to_string(),
            path: "target/load/scenario.json".to_string(),
            duration_ms: 1200,
        }];
        let output = load_output(&reports);

        // The last token on the line is the produced report path.
        let line = output.trim_end();
        let tokens: Vec<&str> = line.split(' ').collect();
        assert_eq!(tokens.last().unwrap(), &"target/load/scenario.json");
        assert_eq!(&tokens[..2], &["REPORT", "load_scenario"]);
    }

    #[test]
    fn the_load_output_orders_reports_by_scenario() {
        let reports = vec![
            LoadReport {
                scenario: "zulu".to_string(),
                path: "z.json".to_string(),
                duration_ms: 1,
            },
            LoadReport {
                scenario: "alpha".to_string(),
                path: "a.json".to_string(),
                duration_ms: 1,
            },
        ];
        let output = load_output(&reports);
        assert!(output.find("alpha").unwrap() < output.find("zulu").unwrap());
    }

    #[test]
    fn the_same_test_and_load_input_renders_the_same_bytes() {
        let results = vec![test_result(
            "space::CellApp::tick",
            "crates/server/src/cellapp.rs",
            120,
            true,
        )];
        let reports = vec![LoadReport {
            scenario: "load_scenario".to_string(),
            path: "target/load/scenario.json".to_string(),
            duration_ms: 1200,
        }];

        // Deterministic output: the contract is byte-for-byte stable.
        assert_eq!(test_output(&results), test_output(&results));
        assert_eq!(load_output(&reports), load_output(&reports));
    }

    #[test]
    fn an_empty_run_outputs_nothing() {
        assert_eq!(test_output(&[]), "");
        assert_eq!(load_output(&[]), "");
    }
}
