//! Regenerate the protocol binary fixture catalog (BW-00-04-01).
//!
//! Usage: `scan-protocol-fixtures <reference-checkout> [output]`
//!
//! `<reference-checkout>` is the BigWorld repository root. The catalog is
//! written to `[output]` (default stdout), and the interface/message counts
//! are printed to stdout. Every message records its id, direction and raw
//! frame bytes.

use std::path::Path;
use std::path::PathBuf;
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 || args.len() > 3 {
        eprintln!("usage: scan-protocol-fixtures <reference-checkout> [output]");
        return ExitCode::from(2);
    }

    let root = PathBuf::from(&args[1]);
    let first = root.join("programming/bigworld/server/baseapp/baseapp_int_interface.hpp");
    if !first.is_file() {
        eprintln!(
            "error: {} is not a BigWorld checkout (no programming/bigworld/server/baseapp/baseapp_int_interface.hpp)",
            root.display()
        );
        return ExitCode::from(2);
    }

    let scan = test_support::protocol_fixtures::scan_reference(&root);
    let catalog = test_support::protocol_fixtures::render(&scan);

    if let Some(output) = args.get(2) {
        if let Err(e) = std::fs::write(output, &catalog) {
            eprintln!("error: cannot write {}: {e}", Path::new(output).display());
            return ExitCode::from(1);
        }
    } else {
        print!("{catalog}");
    }

    let interfaces = test_support::protocol_fixtures::interfaces();
    let messages: usize = interfaces.iter().map(|i| i.messages.len()).sum();
    println!(
        "{} interfaces, {} messages (ids in declaration order)",
        interfaces.len(),
        messages
    );
    for fact in &scan {
        if fact.item == "reference-commit" || fact.item.starts_with("interface.") {
            println!("  {}: {}", fact.item, fact.value);
        }
    }
    ExitCode::SUCCESS
}
