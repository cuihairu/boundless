//! Regenerate the CMake/Windows target catalog (BW-00-01-02).
//!
//! Usage: `scan-cmake-targets <reference-checkout> [output]`
//!
//! `<reference-checkout>` is the BigWorld repository root; the configuration
//! files are read from `<reference-checkout>/programming/bigworld/build/cmake/`.
//! The catalog is written to `[output]` (default stdout), and the record count
//! plus the per-configuration condition guards are printed to stderr.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 || args.len() > 3 {
        eprintln!("usage: scan-cmake-targets <reference-checkout> [output]");
        return ExitCode::from(2);
    }

    let root = PathBuf::from(&args[1]);
    let bw_source = root.join("programming").join("bigworld");
    let config_dir = bw_source.join(test_support::cmake_targets::config_dir_name());

    if !config_dir.is_dir() {
        eprintln!(
            "error: {} is not a BigWorld CMake config dir (no programming/bigworld/build/cmake)",
            root.display()
        );
        return ExitCode::from(2);
    }

    let scan = test_support::cmake_targets::scan_reference(&bw_source);
    let catalog = test_support::cmake_targets::render(&scan);

    if let Some(output) = args.get(2) {
        if let Err(e) = std::fs::write(output, &catalog) {
            eprintln!("error: cannot write {}: {e}", Path::new(output).display());
            return ExitCode::from(1);
        }
    } else {
        print!("{catalog}");
    }

    println!("{} CMake configurations", scan.len());
    for target in &scan {
        if !target.conditions.is_empty() {
            println!("  {}: {}", target.name, target.conditions.join(", "));
        }
    }
    ExitCode::SUCCESS
}
