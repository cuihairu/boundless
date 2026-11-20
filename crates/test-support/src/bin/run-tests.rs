//! `run-tests`: the shared local/CI test entry (BW-18-04-01).
//!
//! The same command drives both, so a failure reproduces identically:
//! - full suite: `cargo run -p test-support --bin run-tests -- all`
//! - one module: `cargo run -p test-support --bin run-tests -- server-runtime`
//! - one module inside a crate:
//!   `cargo run -p test-support --bin run-tests -- test-support timer_scenarios`
//!
//! Exits with the test run's status so CI gates on it directly.

use std::path::Path;
use std::process::{Command, ExitCode};

use test_support::harness::{Plan, workspace_modules};
use test_support::layering::find_workspace_root;

fn usage() -> String {
    "usage: run-tests all\n       run-tests <module> [filter]\n\
     modules: foundation platform resources mercury contracts entitydef \
     python-host server-runtime test-support server"
        .to_string()
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
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

    let words: Vec<&str> = args.iter().map(String::as_str).collect();
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
