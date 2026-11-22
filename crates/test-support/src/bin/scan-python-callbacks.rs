//! Regenerate the Python callback fixture catalog (BW-00-04-02).
//!
//! Usage: `scan-python-callbacks <reference-checkout> [output]`
//!
//! `<reference-checkout>` is the BigWorld repository root. The catalog is
//! written to `[output]` (default stdout), and the callback count is printed
//! to stdout. Every callback records its owning type, order, component(s),
//! parameter list and optionality, as documented by the reference.

use std::path::Path;
use std::path::PathBuf;
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 || args.len() > 3 {
        eprintln!("usage: scan-python-callbacks <reference-checkout> [output]");
        return ExitCode::from(2);
    }

    let root = PathBuf::from(&args[1]);
    let first = root.join("programming/bigworld/server/baseapp/base.cpp");
    if !first.is_file() {
        eprintln!(
            "error: {} is not a BigWorld checkout (no programming/bigworld/server/baseapp/base.cpp)",
            root.display()
        );
        return ExitCode::from(2);
    }

    let scan = test_support::python_callbacks::scan_reference(&root);
    let catalog = test_support::python_callbacks::render(&scan);

    if let Some(output) = args.get(2) {
        if let Err(e) = std::fs::write(output, &catalog) {
            eprintln!("error: cannot write {}: {e}", Path::new(output).display());
            return ExitCode::from(1);
        }
    } else {
        print!("{catalog}");
    }

    // Count from the scan, not the committed catalog: this binary is what
    // regenerates that catalog, so it must report what it just wrote.
    let names = scan
        .iter()
        .filter(|f| f.item.ends_with(".name") && f.item.starts_with("cb."))
        .count();
    let by_type = |t: &str| {
        scan.iter()
            .filter(|f| f.item.ends_with(".type") && f.value == t)
            .count()
    };
    println!(
        "{} callbacks ({} Base, {} Proxy, {} Entity)",
        names,
        by_type("Base"),
        by_type("Proxy"),
        by_type("Entity")
    );
    for fact in &scan {
        if fact.item == "reference-commit" || fact.item.starts_with("blocker.") {
            println!("  {}: {}", fact.item, fact.value);
        }
    }
    ExitCode::SUCCESS
}
