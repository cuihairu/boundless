//! Regenerate the coverage-ledger catalog (BW-00-05-01).
//!
//! Usage: `scan-coverage <reference-checkout> [output]`
//!
//! `<reference-checkout>` is the BigWorld repository root; the ledger rows
//! are derived from `<reference-checkout>/programming/bigworld/`'s Make build
//! catalog and gate machinery. The catalog is written to `[output]` (default
//! stdout), and the record count plus the enable-condition census are printed
//! to stderr.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 || args.len() > 3 {
        eprintln!("usage: scan-coverage <reference-checkout> [output]");
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

    let scan = test_support::coverage::scan_reference(&bw_source);
    let catalog = test_support::coverage::render(&scan);

    if let Some(output) = args.get(2) {
        if let Err(e) = std::fs::write(output, &catalog) {
            eprintln!("error: cannot write {}: {e}", Path::new(output).display());
            return ExitCode::from(1);
        }
    } else {
        print!("{catalog}");
    }

    println!("{} coverage rows", scan.len());
    let mut enables = std::collections::BTreeMap::new();
    for row in &scan {
        *enables.entry(row.enable.as_str()).or_insert(0usize) += 1;
    }
    for (enable, count) in &enables {
        println!("  {enable}: {count}");
    }
    ExitCode::SUCCESS
}
