//! Regenerate the Windows client and tool baseline catalog (BW-00-02-05).
//!
//! Usage: `scan-windows-baseline <reference-checkout> [output]`
//!
//! `<reference-checkout>` is the BigWorld repository root. The catalog is
//! written to `[output]` (default stdout), and the target count is printed to
//! stdout. Every target records its re-test result or gap.

use std::path::Path;
use std::path::PathBuf;
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 || args.len() > 3 {
        eprintln!("usage: scan-windows-baseline <reference-checkout> [output]");
        return ExitCode::from(2);
    }

    let root = PathBuf::from(&args[1]);
    let tools = root.join("programming/bigworld/tools");
    if !tools.is_dir() {
        eprintln!(
            "error: {} is not a BigWorld checkout (no programming/bigworld/tools)",
            root.display()
        );
        return ExitCode::from(2);
    }

    let scan = test_support::windows_baseline::scan_reference(&root);
    let catalog = test_support::windows_baseline::render(&scan);

    if let Some(output) = args.get(2) {
        if let Err(e) = std::fs::write(output, &catalog) {
            eprintln!("error: cannot write {}: {e}", Path::new(output).display());
            return ExitCode::from(1);
        }
    } else {
        print!("{catalog}");
    }

    let targets = scan
        .iter()
        .filter(|f| f.item.ends_with(".name") && f.item.starts_with("target."))
        .count();
    let gaps = scan
        .iter()
        .filter(|f| f.item.ends_with(".result") && f.item.starts_with("target."))
        .filter(|f| f.value == "GAP")
        .count();
    println!("{targets} Windows targets, {gaps} gaps (no Windows binary built)");
    for fact in &scan {
        if fact.item == "runner.wine" {
            println!("  {}: {}", fact.item, fact.value);
        }
        if fact.item == "reference-commit" {
            println!("  {}: {}", fact.item, fact.value);
        }
    }
    ExitCode::SUCCESS
}
