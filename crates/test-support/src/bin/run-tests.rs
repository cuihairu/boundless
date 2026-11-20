//! `run-tests`: the shared local/CI test entry (BW-18-04-01/02).
//!
//! The same command drives both, so a failure reproduces identically:
//! - full suite: `cargo run -p test-support --bin run-tests -- all`
//! - one module: `cargo run -p test-support --bin run-tests -- server-runtime`
//! - one module inside a crate:
//!   `cargo run -p test-support --bin run-tests -- test-support timer_scenarios`
//! - compat diff between two run archives, naming every difference:
//!   `cargo run -p test-support --bin run-tests -- diff <baseline> <current>`
//! - coverage (cargo-llvm-cov 0.9.1), lcov + html mapping back to source:
//!   `cargo run -p test-support --bin run-tests -- cov [module]`
//!
//! Exits with the test run's status so CI gates on it directly; the diff
//! exits 2 when the runs are not compatible.

use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Command, ExitCode, Stdio};

use test_support::harness::{
    Plan, RunArchive, coverage_args, coverage_report_dir, diff_runs, lcov_source_files,
    parse_cargo_test_output, parse_outcomes, workspace_modules,
};
use test_support::layering::find_workspace_root;

fn usage() -> String {
    "usage: run-tests all\n       run-tests <module> [filter]\n       \
     run-tests diff <baseline-archive> <current-archive>\n       \
     run-tests cov [module]\n\
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

/// The `cov` mode: run cargo-llvm-cov for a module (or the workspace) and
/// print which source files the report maps to.
fn run_cov(module: Option<&str>, root: &Path) -> ExitCode {
    let target_dir = root.join("target");
    let report_dir = coverage_report_dir(&target_dir, module);
    if let Err(reason) = std::fs::create_dir_all(&report_dir) {
        eprintln!("run-tests: cannot create report dir: {reason}");
        return ExitCode::FAILURE;
    }

    let cov_args = coverage_args(module, &report_dir);
    println!("run-tests: cargo {}", cov_args.join(" "));
    let status = Command::new("cargo").args(&cov_args).status();
    match status {
        Ok(status) if !status.success() => ExitCode::FAILURE,
        Err(reason) => {
            eprintln!("run-tests: cannot start cargo: {reason}");
            ExitCode::FAILURE
        }
        Ok(_) => {
            let lcov = std::fs::read_to_string(report_dir.join("lcov.info")).unwrap_or_default();
            let sources = lcov_source_files(&lcov);
            println!(
                "run-tests: lcov maps {} source files ({} .. {})",
                sources.len(),
                sources.first().map(String::as_str).unwrap_or("-"),
                sources.last().map(String::as_str).unwrap_or("-"),
            );
            ExitCode::SUCCESS
        }
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
        ["cov"] => return run_cov(None, &root),
        ["cov", module] => {
            if let Err(message) = Plan::for_module(module, &modules, None) {
                eprintln!("run-tests: {message}");
                return ExitCode::FAILURE;
            }
            return run_cov(Some(module), &root);
        }
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

    // Stream the run's stdout through (so progress shows live) while
    // collecting per-test outcomes for the archive (BW-18-04-04).
    let mut child = match Command::new("cargo")
        .args(&cargo_args)
        .stdout(Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(reason) => {
            eprintln!("run-tests: cannot start cargo: {reason}");
            return ExitCode::FAILURE;
        }
    };
    let mut outcomes = Vec::new();
    if let Some(stdout) = child.stdout.take() {
        for line in BufReader::new(stdout).lines() {
            match line {
                Ok(line) => {
                    println!("{line}");
                    outcomes.extend(parse_cargo_test_output(&line));
                }
                Err(reason) => {
                    eprintln!("run-tests: cannot read test output: {reason}");
                    return ExitCode::FAILURE;
                }
            }
        }
    }
    let status = match child.wait() {
        Ok(status) => status,
        Err(reason) => {
            eprintln!("run-tests: cannot wait for cargo: {reason}");
            return ExitCode::FAILURE;
        }
    };

    let label = match &plan {
        Plan::All => "all".to_string(),
        Plan::Single { package, filter } => match filter {
            Some(filter) => format!("{package}-{filter}"),
            None => package.clone(),
        },
    };
    let archive = RunArchive {
        label,
        command: format!("cargo {}", cargo_args.join(" ")),
        commit: git_commit(&root),
        when: utc_now(),
        outcomes,
    };
    let archive_path = archive.dir(&root.join("target")).join("run.txt");
    if let Err(reason) = write_archive(&archive, &archive_path) {
        eprintln!("run-tests: cannot write archive: {reason}");
        // The test run itself still decides the exit status below.
    } else {
        println!("run-tests: archive {}", archive_path.display());
    }

    if status.success() {
        ExitCode::SUCCESS
    } else {
        match status.code() {
            Some(code) => ExitCode::from(code.clamp(1, 255) as u8),
            None => ExitCode::FAILURE,
        }
    }
}

/// Short HEAD sha, or None outside a git checkout.
fn git_commit(root: &Path) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .arg("rev-parse")
        .arg("--short")
        .arg("HEAD")
        .output()
        .ok()?;
    if output.status.success() {
        Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        None
    }
}

/// ISO-8601 UTC stamp via `date`, so no extra dependency.
fn utc_now() -> String {
    Command::new("date")
        .arg("-u")
        .arg("+%Y-%m-%dT%H:%M:%SZ")
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
        .unwrap_or_default()
}

fn write_archive(archive: &RunArchive, path: &Path) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, archive.render())
}
