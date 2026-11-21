//! Regenerate the release-script / RPM-target catalog (BW-00-01-03).
//!
//! Usage: `scan-release-artifacts <reference-checkout> [output]`
//!
//! `<reference-checkout>` is the BigWorld repository root; the scripts are
//! read from `<reference-checkout>/programming/bigworld/build/bw_internal/scripts/`.
//! The catalog is written to `[output]` (default stdout), and the artifact
//! count is printed to stdout.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 || args.len() > 3 {
        eprintln!("usage: scan-release-artifacts <reference-checkout> [output]");
        return ExitCode::from(2);
    }

    let root = PathBuf::from(&args[1]);
    let bw_source = root.join("programming").join("bigworld");
    let scripts_dir = bw_source.join(test_support::release_artifacts::scripts_dir_name());

    if !scripts_dir.is_dir() {
        eprintln!(
            "error: {} is not a BigWorld release-scripts dir (no programming/bigworld/build/bw_internal/scripts)",
            root.display()
        );
        return ExitCode::from(2);
    }

    let scan = test_support::release_artifacts::scan_reference(&bw_source);
    let catalog = test_support::release_artifacts::render(&scan);

    if let Some(output) = args.get(2) {
        if let Err(e) = std::fs::write(output, &catalog) {
            eprintln!("error: cannot write {}: {e}", Path::new(output).display());
            return ExitCode::from(1);
        }
    } else {
        print!("{catalog}");
    }

    println!("{} release artifacts", scan.len());
    for artifact in &scan {
        if artifact.artifact == "rpm" || artifact.artifact == "server-build" {
            println!("  {}: {}", artifact.artifact, artifact.path);
        }
    }
    ExitCode::SUCCESS
}
