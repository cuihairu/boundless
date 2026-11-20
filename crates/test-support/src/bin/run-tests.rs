//! `run-tests`: the shared local/CI test entry (BW-18-04-01/02).
//!
//! The same command drives both, so a failure reproduces identically:
//! - full suite: `cargo run -p test-support --bin run-tests -- all`
//! - one module: `cargo run -p test-support --bin run-tests -- server-runtime`
//! - one module inside a crate:
//!   `cargo run -p test-support --bin run-tests -- test-support timer_scenarios`
//! - compat diff between two run archives, naming every difference:
//!   `cargo run -p test-support --bin run-tests -- diff <baseline> <current>`
//!
//! Exits with the test run's status so CI gates on it directly; the diff
//! exits 2 when the runs are not compatible.

use std::path::Path;
use std::process::{Command, ExitCode};

use test_support::harness::{Plan, diff_runs, parse_outcomes, workspace_modules};
use test_support::layering::find_workspace_root;

fn usage() -> String {
    "usage: run-tests all\n       run-tests <module> [filter]\n       \
     run-tests diff <baseline-archive> <current-archive>\n\
     modules: foundation platform resources mercury contracts entitydef \
     python-host server-runtime test-support server"
        .to_string()
}

/// The `diff` mode: locate every difference between two run archives.
fn run_diff(baseline_path: &str, current_path: &str) -> ExitCode {
    let read = |path: &str, label: &str| -> Result<String, String> {
        std::fs::read_to_string(path).map_err(|reason| format!("cannot read {label}: {reason}"))
    };
    let (baseline_text, current_text) = match (
        read(baseline_path, "baseline"),
        read(current_path, "current"),
    ) {
        (Ok(baseline), Ok(current)) => (baseline, current),
        (Err(message), _) | (_, Err(message)) => {
            eprintln!("run-tests: {message}");
            return ExitCode::FAILURE;
        }
    };

    let report = diff_runs(
        &parse_outcomes(&baseline_text),
        &parse_outcomes(&current_text),
    );
    print!("{}", report.render());
    if report.compatible() {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(2)
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let words: Vec<&str> = args.iter().map(String::as_str).collect();
    if let ["diff", baseline, current] = words.as_slice() {
        return run_diff(baseline, current);
    }

    let Some(root) = find_workspace_root(Path::new(".")) else {
        eprintln!("run-tests: no workspace manifest found above .");
        return ExitCode::FAILURE;
    };
    let root_text = match std::fs::read_to_string(root.join("Cargo.toml")) {
        Ok(text) => text,
        Err(reason) => {
            eprintln!("run-tests: cannot read root manifest: {reason}");
            return ExitCode::FAILURE;
        }
    };
    let modules = workspace_modules(&root_text);

    let plan = match words.as_slice() {
        ["all"] => Ok(Plan::All),
        [module] => Plan::for_module(module, &modules, None),
        [module, filter] => Plan::for_module(module, &modules, Some(filter)),
        _ => Err(usage()),
    };
    let plan = match plan {
        Ok(plan) => plan,
        Err(message) => {
            eprintln!("run-tests: {message}");
            return ExitCode::FAILURE;
        }
    };

    let cargo_args = plan.cargo_args();
    println!("run-tests: cargo {}", cargo_args.join(" "));
    match Command::new("cargo").args(&cargo_args).status() {
        Ok(status) if status.success() => ExitCode::SUCCESS,
        Ok(status) => match status.code() {
            Some(code) => ExitCode::from(code.clamp(1, 255) as u8),
            None => ExitCode::FAILURE,
        },
        Err(reason) => {
            eprintln!("run-tests: cannot start cargo: {reason}");
            ExitCode::FAILURE
        }
    }
}
