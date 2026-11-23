//! Regenerate the third-party dependency / resource-provenance catalog (BW-00-01-04).
//!
//! Usage: `scan-third-party-deps <reference-checkout> [output]`
//!
//! `<reference-checkout>` is the BigWorld repository root; the vendored
//! dependencies are read from `<reference-checkout>/programming/bigworld/third_party/`.
//! The catalog is written to `[output]` (default stdout), and the dependency
//! count is printed to stdout.

use std::path::Path;
use std::path::PathBuf;
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 || args.len() > 3 {
        eprintln!("usage: scan-third-party-deps <reference-checkout> [output]");
        return ExitCode::from(2);
    }

    let root = PathBuf::from(&args[1]);
    let bw_source = root.join("programming").join("bigworld");
    let deps_dir = bw_source.join(test_support::third_party_deps::deps_dir_name());

    if !deps_dir.is_dir() {
        eprintln!(
            "error: {} is not a BigWorld third-party dir (no programming/bigworld/third_party)",
            root.display()
        );
        return ExitCode::from(2);
    }

    let scan = test_support::third_party_deps::scan_reference(&bw_source);
    let catalog = test_support::third_party_deps::render(&scan);

    if let Some(output) = args.get(2) {
        if let Err(e) = std::fs::write(output, &catalog) {
            eprintln!("error: cannot write {}: {e}", Path::new(output).display());
            return ExitCode::from(1);
        }
    } else {
        print!("{catalog}");
    }

    println!("{} third party dependencies", scan.len());
    for dep in &scan {
        if dep.license.starts_with("unidentified") || dep.notice == "yes" {
            println!(
                "  {} (notice: {}, reuse: {})",
                dep.dep, dep.notice, dep.reuse_decision
            );
        }
    }
    ExitCode::SUCCESS
}
