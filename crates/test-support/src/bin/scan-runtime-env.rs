//! Regenerate the original runtime-environment catalog (BW-00-02-01).
//!
//! Usage: `scan-runtime-env <reference-checkout> [output]`
//!
//! `<reference-checkout>` is the BigWorld repository root. The catalog is
//! written to `[output]` (default stdout), and the fact count is printed to
//! stdout.

use std::path::Path;
use std::path::PathBuf;
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 || args.len() > 3 {
        eprintln!("usage: scan-runtime-env <reference-checkout> [output]");
        return ExitCode::from(2);
    }

    let root = PathBuf::from(&args[1]);
    let bw_source = root.join("programming").join("bigworld");
    if !bw_source.join("lib/cstdmf/bwversion.hpp").is_file() {
        eprintln!(
            "error: {} is not a BigWorld checkout (no programming/bigworld/lib/cstdmf/bwversion.hpp)",
            root.display()
        );
        return ExitCode::from(2);
    }

    let scan = test_support::runtime_env::scan_reference(&root);
    let catalog = test_support::runtime_env::render(&scan);

    if let Some(output) = args.get(2) {
        if let Err(e) = std::fs::write(output, &catalog) {
            eprintln!("error: cannot write {}: {e}", Path::new(output).display());
            return ExitCode::from(1);
        }
    } else {
        print!("{catalog}");
    }

    println!("{} runtime facts", scan.len());
    for fact in &scan {
        if fact.item.starts_with("reference-commit") || fact.item == "prebuilt-server-binaries" {
            println!("  {}: {}", fact.item, fact.value);
        }
    }
    ExitCode::SUCCESS
}
