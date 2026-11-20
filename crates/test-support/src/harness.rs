//! Shared test entry core (BW-18-04-01).
//!
//! Local and CI drive the same entry (`cargo run -p test-support --bin
//! run-tests -- ...`), and this module holds the decision logic the entry
//! is built on: which workspace crates exist and which `cargo test`
//! invocation a requested module maps to. Keeping it as library code makes
//! the mapping unit-testable without spawning anything; the binary only
//! wires arguments, runs the plan, and propagates the exit status.

use std::path::{Path, PathBuf};

/// Workspace crate names parsed from the root manifest's `members` array.
/// `apps/server` contributes `server` -- the path tail is the package name.
pub fn workspace_modules(root_text: &str) -> Vec<String> {
    let mut modules = Vec::new();
    let mut in_members = false;
    for line in root_text.lines() {
        let trimmed = line.trim();
        if in_members {
            if trimmed.starts_with('[') {
                // A new table header closes the array.
                in_members = false;
            } else {
                let entries = trimmed.split(']').next().unwrap_or(trimmed);
                push_entries(entries, &mut modules);
                if trimmed.contains(']') {
                    in_members = false;
                }
                continue;
            }
        }
        if trimmed.starts_with("members")
            && trimmed.contains('[')
            && let Some(entries) = trimmed.split('[').nth(1)
        {
            push_entries(entries.split(']').next().unwrap_or(entries), &mut modules);
            in_members = !trimmed.contains(']');
        }
    }
    modules
}

/// Split one run of comma-separated quoted paths into package names.
fn push_entries(entries: &str, modules: &mut Vec<String>) {
    for part in entries.split(',') {
        let candidate = part.trim().trim_matches('"');
        if candidate.is_empty() {
            continue;
        }
        let name = candidate.rsplit('/').next().unwrap_or(candidate);
        modules.push(name.to_string());
    }
}

/// Which slice of the suite a run executes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Plan {
    /// One crate, optionally narrowed to one test module by name filter.
    Single {
        package: String,
        filter: Option<String>,
    },
    /// The whole workspace.
    All,
}

impl Plan {
    /// The plan for a named module: it must be a workspace crate, and the
    /// error names the known ones so a typo is fixable in one read.
    pub fn for_module(
        module: &str,
        modules: &[String],
        filter: Option<&str>,
    ) -> Result<Self, String> {
        if modules.iter().any(|m| m == module) {
            Ok(Plan::Single {
                package: module.to_string(),
                filter: filter.map(str::to_string),
            })
        } else {
            Err(format!(
                "unknown module {module}; workspace crates are: {}",
                modules.join(", ")
            ))
        }
    }

    /// The cargo arguments (everything after `cargo`) this plan runs.
    pub fn cargo_args(&self) -> Vec<String> {
        match self {
            Plan::Single { package, filter } => {
                let mut args = vec!["test".to_string(), "-p".to_string(), package.clone()];
                args.extend(filter.clone());
                args
            }
            Plan::All => vec!["test".to_string(), "--workspace".to_string()],
        }
    }
}

/// The outcome of one test inside a run archive (BW-18-04-02).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestOutcome {
    pub name: String,
    pub status: TestStatus,
}

/// What became of one test.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TestStatus {
    Passed,
    Failed,
    Skipped,
}

impl TestStatus {
    /// The one-word archive token, also the prefix `parse_outcomes` reads.
    pub fn token(self) -> &'static str {
        match self {
            TestStatus::Passed => "PASS",
            TestStatus::Failed => "FAIL",
            TestStatus::Skipped => "SKIP",
        }
    }
}

/// Parse an archive's outcome lines (`PASS <name>` / `FAIL <name>` /
/// `SKIP <name>`); header, comment and blank lines are ignored, so the
/// same files feed the diff locally and from CI artifacts.
pub fn parse_outcomes(archive_text: &str) -> Vec<TestOutcome> {
    let mut outcomes = Vec::new();
    for line in archive_text.lines() {
        let trimmed = line.trim();
        let Some((token, name)) = trimmed.split_once(' ') else {
            continue;
        };
        let status = match token {
            "PASS" => TestStatus::Passed,
            "FAIL" => TestStatus::Failed,
            "SKIP" => TestStatus::Skipped,
            _ => continue,
        };
        let name = name.trim();
        if !name.is_empty() {
            outcomes.push(TestOutcome {
                name: name.to_string(),
                status,
            });
        }
    }
    outcomes
}

/// Locate every difference between the baseline and current runs. Each
/// list names the test, so a report line points straight at the case to
/// look at.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DiffReport {
    /// Passed in the baseline, failed now.
    pub regressed: Vec<String>,
    /// Failed in the baseline, passed now.
    pub healed: Vec<String>,
    /// Present in the baseline, absent from the current run.
    pub missing: Vec<String>,
    /// Absent from the baseline, present now.
    pub added: Vec<String>,
}

impl DiffReport {
    /// True when nothing regressed, went missing, or was added -- the two
    /// runs are compatible from the diff entry's point of view. Heals are
    /// reported but not fatal.
    pub fn compatible(&self) -> bool {
        self.regressed.is_empty() && self.missing.is_empty() && self.added.is_empty()
    }

    /// Human-readable report; every line names the differing test.
    pub fn render(&self) -> String {
        let mut out = String::new();
        for name in &self.regressed {
            out.push_str(&format!(
                "REGRESSED {name}: passed in baseline, failed now\n"
            ));
        }
        for name in &self.healed {
            out.push_str(&format!("healed {name}: failed in baseline, passed now\n"));
        }
        for name in &self.missing {
            out.push_str(&format!("MISSING {name}: in baseline, absent now\n"));
        }
        for name in &self.added {
            out.push_str(&format!("ADDED {name}: not in baseline, present now\n"));
        }
        if out.is_empty() {
            out.push_str("no differences\n");
        }
        out
    }
}

/// Diff two runs, matching tests by name. Repeated names (a crate run
/// twice with different filters) diff on the worst outcome seen.
pub fn diff_runs(baseline: &[TestOutcome], current: &[TestOutcome]) -> DiffReport {
    fn worst(statuses: impl Iterator<Item = TestStatus>) -> Option<TestStatus> {
        statuses.max_by_key(|status| match status {
            TestStatus::Passed => 0,
            TestStatus::Skipped => 1,
            TestStatus::Failed => 2,
        })
    }

    fn by_name(outcomes: &[TestOutcome]) -> std::collections::BTreeMap<&str, TestStatus> {
        let mut grouped: std::collections::BTreeMap<&str, Vec<TestStatus>> =
            std::collections::BTreeMap::new();
        for outcome in outcomes {
            grouped
                .entry(outcome.name.as_str())
                .or_default()
                .push(outcome.status);
        }
        grouped
            .into_iter()
            .filter_map(|(name, statuses)| worst(statuses.into_iter()).map(|s| (name, s)))
            .collect()
    }

    let baseline = by_name(baseline);
    let current = by_name(current);
    let mut report = DiffReport::default();
    for (name, status) in &baseline {
        match current.get(name) {
            None => report.missing.push((*name).to_string()),
            Some(now) => match (status, now) {
                (TestStatus::Passed, TestStatus::Failed) => {
                    report.regressed.push((*name).to_string())
                }
                (TestStatus::Failed, TestStatus::Passed) => report.healed.push((*name).to_string()),
                _ => {}
            },
        }
    }
    for name in current.keys() {
        if !baseline.contains_key(name) {
            report.added.push(name.to_string());
        }
    }
    report
}

/// Where the coverage entry writes its reports (BW-18-04-03): under the
/// shared run-results tree, so local and CI pick the same paths.
pub fn coverage_report_dir(target_dir: &Path, module: Option<&str>) -> PathBuf {
    match module {
        Some(module) => target_dir
            .join("test-results")
            .join(format!("cov-{module}")),
        None => target_dir.join("test-results").join("cov-all"),
    }
}

/// The `cargo llvm-cov` arguments (everything after `cargo`) for one
/// coverage run: an lcov report whose `SF:`/`DA:` records map every
/// measured line back to its source file and line. Requires
/// cargo-llvm-cov 0.9.1 (which does not allow `--html` alongside
/// `--lcov`, so the lcov report is the one output).
pub fn coverage_args(module: Option<&str>, report_dir: &Path) -> Vec<String> {
    let mut args = vec!["llvm-cov".to_string()];
    if let Some(module) = module {
        args.push("-p".to_string());
        args.push(module.to_string());
    }
    args.extend([
        "--lcov".to_string(),
        "--output-path".to_string(),
        report_dir.join("lcov.info").to_string_lossy().into_owned(),
    ]);
    args
}

/// The source files an lcov report maps to, from its `SF:` records. This
/// is the acceptance check: a report that maps to source names real
/// files.
pub fn lcov_source_files(lcov_text: &str) -> Vec<String> {
    lcov_text
        .lines()
        .filter_map(|line| line.strip_prefix("SF:"))
        .map(str::to_string)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROOT_MANIFEST: &str = r#"
[workspace]
resolver = "2"
members = [
    "crates/foundation",
    "crates/platform",
    "crates/resources",
    "crates/mercury",
    "crates/contracts",
    "crates/entitydef",
    "crates/python-host",
    "crates/server-runtime",
    "crates/test-support",
    "apps/server",
]

[workspace.package]
version = "0.1.0"
"#;

    #[test]
    fn modules_list_every_workspace_crate() {
        assert_eq!(
            workspace_modules(ROOT_MANIFEST),
            [
                "foundation",
                "platform",
                "resources",
                "mercury",
                "contracts",
                "entitydef",
                "python-host",
                "server-runtime",
                "test-support",
                "server",
            ]
        );
    }

    #[test]
    fn a_table_after_members_closes_the_array() {
        let text = "[workspace]\nmembers = [\"a\"]\n\n[profile.release]\nfoo = \"b\"\n";
        assert_eq!(workspace_modules(text), ["a"]);
    }

    #[test]
    fn a_known_module_maps_to_a_single_crate_run() {
        let modules = workspace_modules(ROOT_MANIFEST);
        assert_eq!(
            Plan::for_module("server-runtime", &modules, None),
            Ok(Plan::Single {
                package: "server-runtime".to_string(),
                filter: None,
            })
        );
    }

    #[test]
    fn an_unknown_module_error_names_the_known_ones() {
        let modules = workspace_modules(ROOT_MANIFEST);
        let error = Plan::for_module("clock", &modules, None).unwrap_err();
        assert!(error.contains("unknown module clock"));
        assert!(error.contains("test-support"), "lists known crates");
    }

    #[test]
    fn single_module_args_target_only_that_crate() {
        let plan = Plan::for_module("server-runtime", &["server-runtime".to_string()], None)
            .expect("known");
        assert_eq!(plan.cargo_args(), ["test", "-p", "server-runtime"]);
    }

    #[test]
    fn a_filter_narrows_to_one_test_module_inside_the_crate() {
        let plan = Plan::for_module("test-support", &["test-support".to_string()], Some("clock"))
            .expect("known");
        assert_eq!(plan.cargo_args(), ["test", "-p", "test-support", "clock"]);
    }

    #[test]
    fn the_full_plan_runs_the_whole_workspace() {
        assert_eq!(Plan::All.cargo_args(), ["test", "--workspace"]);
    }

    fn outcomes(pairs: &[(&str, TestStatus)]) -> Vec<TestOutcome> {
        pairs
            .iter()
            .map(|(name, status)| TestOutcome {
                name: (*name).to_string(),
                status: *status,
            })
            .collect()
    }

    #[test]
    fn parsing_reads_outcome_lines_and_skips_the_rest() {
        let archive = "# run archive\nrun: all\ncrate: server-runtime\n\n\
                       PASS server_runtime::config::chain_resolves\n\
                       FAIL server_runtime::timers::fires\n  SKIP \n";
        assert_eq!(
            parse_outcomes(archive),
            outcomes(&[
                ("server_runtime::config::chain_resolves", TestStatus::Passed),
                ("server_runtime::timers::fires", TestStatus::Failed),
            ])
        );
    }

    #[test]
    fn identical_runs_diff_clean() {
        let run = outcomes(&[
            ("a", TestStatus::Passed),
            ("b", TestStatus::Failed),
            ("c", TestStatus::Skipped),
        ]);
        let report = diff_runs(&run, &run);
        assert!(report.compatible());
        assert_eq!(report.render(), "no differences\n");
    }

    #[test]
    fn a_baseline_pass_failing_now_is_a_named_regression() {
        let baseline = outcomes(&[("timer_fires", TestStatus::Passed)]);
        let current = outcomes(&[("timer_fires", TestStatus::Failed)]);
        let report = diff_runs(&baseline, &current);
        assert!(!report.compatible());
        assert_eq!(
            report.render(),
            "REGRESSED timer_fires: passed in baseline, failed now\n"
        );
    }

    #[test]
    fn heals_are_reported_but_not_fatal() {
        let baseline = outcomes(&[("timer_fires", TestStatus::Failed)]);
        let current = outcomes(&[("timer_fires", TestStatus::Passed)]);
        let report = diff_runs(&baseline, &current);
        assert!(report.compatible());
        assert_eq!(
            report.render(),
            "healed timer_fires: failed in baseline, passed now\n"
        );
    }

    #[test]
    fn vanished_and_new_tests_are_located_by_name() {
        let baseline = outcomes(&[("dropped_case", TestStatus::Passed)]);
        let current = outcomes(&[("fresh_case", TestStatus::Passed)]);
        let report = diff_runs(&baseline, &current);
        assert_eq!(report.missing, ["dropped_case"]);
        assert_eq!(report.added, ["fresh_case"]);
        assert_eq!(
            report.render(),
            "MISSING dropped_case: in baseline, absent now\n\
             ADDED fresh_case: not in baseline, present now\n"
        );
    }

    #[test]
    fn a_case_run_twice_diffs_on_its_worst_outcome() {
        // A crate run with two filters: the name appears twice, once
        // passing, once failing -- the diff must not read the pass.
        let run = outcomes(&[
            ("same_case", TestStatus::Failed),
            ("same_case", TestStatus::Passed),
        ]);
        let baseline = outcomes(&[("same_case", TestStatus::Passed)]);
        let report = diff_runs(&baseline, &run);
        assert_eq!(report.regressed, ["same_case"]);
    }

    #[test]
    fn every_combo_of_status_changes_is_classified() {
        let baseline = outcomes(&[
            ("regressed", TestStatus::Passed),
            ("stable_pass", TestStatus::Passed),
            ("stable_fail", TestStatus::Failed),
        ]);
        let current = outcomes(&[
            ("regressed", TestStatus::Failed),
            ("stable_pass", TestStatus::Passed),
            ("stable_fail", TestStatus::Failed),
        ]);
        let report = diff_runs(&baseline, &current);
        assert_eq!(report.regressed, ["regressed"]);
        assert!(report.healed.is_empty());
        assert!(report.missing.is_empty());
        assert!(report.added.is_empty());
    }

    #[test]
    fn coverage_args_target_the_module_and_its_lcov_report() {
        let report_dir = coverage_report_dir(Path::new("target"), Some("foundation"));
        assert_eq!(report_dir, Path::new("target/test-results/cov-foundation"));
        assert_eq!(
            coverage_args(Some("foundation"), &report_dir),
            [
                "llvm-cov",
                "-p",
                "foundation",
                "--lcov",
                "--output-path",
                "target/test-results/cov-foundation/lcov.info",
            ]
        );
    }

    #[test]
    fn coverage_without_a_module_spans_the_workspace() {
        let report_dir = coverage_report_dir(Path::new("target"), None);
        assert_eq!(report_dir, Path::new("target/test-results/cov-all"));
        assert_eq!(
            coverage_args(None, &report_dir),
            [
                "llvm-cov",
                "--lcov",
                "--output-path",
                "target/test-results/cov-all/lcov.info",
            ]
        );
    }

    #[test]
    fn lcov_source_files_read_the_sf_records() {
        let lcov = "TN:\nSF:crates/foundation/src/lib.rs\nDA:1,2\nDA:5,0\nend_of_record\n\
                    SF:crates/foundation/src/allocator.rs\nDA:1,1\nend_of_record\n";
        assert_eq!(
            lcov_source_files(lcov),
            [
                "crates/foundation/src/lib.rs",
                "crates/foundation/src/allocator.rs"
            ]
        );
    }
}
