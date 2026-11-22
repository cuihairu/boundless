//! Regenerate the standalone-cluster startup baseline catalog (BW-00-02-03).
//!
//! Usage: `scan-startup-baseline <reference-checkout> [output]`
//!
//! `<reference-checkout>` is the BigWorld repository root. The catalog is
//! written to `[output]` (default stdout), and the fact count is printed to
//! stdout. Topology and config are re-derived from the checkout; the
//! startup-attempt log is the frozen committed artifact, re-emitted verbatim.

use std::path::Path;
use std::path::PathBuf;
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 || args.len() > 3 {
        eprintln!("usage: scan-startup-baseline <reference-checkout> [output]");
        return ExitCode::from(2);
    }

    let root = PathBuf::from(&args[1]);
    let bw_xml = root.join("programming/fantasydemo/res/server/bw.xml");
    if !bw_xml.is_file() {
        eprintln!(
            "error: {} is not a BigWorld checkout (no programming/fantasydemo/res/server/bw.xml)",
            root.display()
        );
        return ExitCode::from(2);
    }

    let scan = test_support::startup_baseline::scan_reference(&root);
    let catalog = test_support::startup_baseline::render(&scan);

    if let Some(output) = args.get(2) {
        if let Err(e) = std::fs::write(output, &catalog) {
            eprintln!("error: cannot write {}: {e}", Path::new(output).display());
            return ExitCode::from(1);
        }
    } else {
        print!("{catalog}");
    }

    let components = scan
        .iter()
        .filter(|f| f.item.ends_with(".role") && f.item.starts_with("topology."))
        .count();
    let log_lines = scan.iter().filter(|f| f.item.starts_with("log.")).count();
    println!("{components} components, {} log lines", log_lines);
    for fact in &scan {
        if fact.item == "reference-commit" {
            println!("  {}: {}", fact.item, fact.value);
        }
    }
    ExitCode::SUCCESS
}
