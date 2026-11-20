//! `check-deps`: print the workspace layering report.
//!
//! Exits non-zero when any dependency edge breaks the direction rule
//! (BW-16-02-05), so the same check can run locally and in CI:
//! `cargo run -p test-support --bin check-deps`.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use test_support::layering::{check_workspace, find_workspace_root, report};

fn main() -> ExitCode {
    let start = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));

    let Some(root) = find_workspace_root(Path::new(&start)) else {
        eprintln!(
            "check-deps: no workspace manifest found above {}",
            start.display()
        );
        return ExitCode::FAILURE;
    };

    let violations = check_workspace(&root);
    print!("{}", report(&violations));

    if violations.is_empty() {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(2)
    }
}
