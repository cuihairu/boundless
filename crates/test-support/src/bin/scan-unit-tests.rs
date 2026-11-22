//! Regenerate the original unit-test run catalog (BW-00-02-02).
//!
//! Usage: `scan-unit-tests <reference-checkout> [output]`
//!
//! `<reference-checkout>` is the BigWorld repository root; the participating
//! unit-test list is read from
//! `<reference-checkout>/programming/bigworld/`. The catalog is written to
//! `[output]` (default stdout), and the record count plus the result census
//! are printed to stderr.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 || args.len() > 3 {
        eprintln!("usage: scan-unit-tests <reference-checkout> [output]");
        return ExitCode::from(2);
    }

    let root = PathBuf::from(&args[1]);
    let bw_source = root.join("programming").join("bigworld");

    if !bw_source.join("Makefile.rules").is_file() {
        eprintln!(
            "error: {} is not a BigWorld source dir (no programming/bigworld/Makefile.rules)",
            root.display()
        );
        return ExitCode::from(2);
    }

    let scan = test_support::unit_tests::scan_reference(&bw_source);
    let catalog = test_support::unit_tests::render(&scan);

    if let Some(output) = args.get(2) {
        if let Err(e) = std::fs::write(output, &catalog) {
            eprintln!("error: cannot write {}: {e}", Path::new(output).display());
            return ExitCode::from(1);
        }
    } else {
        print!("{catalog}");
    }

    println!("{} unit-test entries", scan.len());
    let mut results = std::collections::BTreeMap::new();
    for entry in &scan {
        *results.entry(entry.result.as_str()).or_insert(0usize) += 1;
    }
    for (result, count) in &results {
        println!("  {result}: {count}");
    }
    ExitCode::SUCCESS
}
