//! Regenerate the asset fixture catalog (BW-00-04-03).
//!
//! Usage: `scan-asset-fixtures <reference-checkout> [output]`
//!
//! `<reference-checkout>` is the BigWorld repository root. The catalog is
//! written to `[output]` (default stdout), and the fixture summary is printed
//! to stdout. Every fixture records its reference input path, byte size and
//! content digest, and the fantasydemo tree is pinned to the committed
//! generator that reproduces it byte-for-byte.

use std::path::Path;
use std::path::PathBuf;
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 || args.len() > 3 {
        eprintln!("usage: scan-asset-fixtures <reference-checkout> [output]");
        return ExitCode::from(2);
    }

    let root = PathBuf::from(&args[1]);
    let first = root.join("programming/fantasydemo/res/scripts/entities.xml");
    if !first.is_file() {
        eprintln!(
            "error: {} is not a BigWorld checkout (no programming/fantasydemo/res/scripts/entities.xml)",
            root.display()
        );
        return ExitCode::from(2);
    }

    let scan = test_support::asset_fixtures::scan_reference(&root);
    let catalog = test_support::asset_fixtures::render(&scan);

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
    for tree in test_support::asset_fixtures::trees() {
        let files = scan
            .iter()
            .filter(|f| {
                f.item.starts_with(&format!("file.{}.", tree.id)) && f.item.ends_with(".input")
            })
            .count();
        let entities = scan
            .iter()
            .filter(|f| {
                f.item.starts_with(&format!("entity.{}.", tree.id)) && f.item.ends_with(".def")
            })
            .count();
        let bytes = scan
            .iter()
            .find(|f| f.item == format!("tree.{}.bytes", tree.id))
            .map(|f| f.value.clone())
            .unwrap_or_default();
        println!(
            "{} files under {}, {} entities, {} bytes",
            files, tree.id, entities, bytes
        );
    }
    let generator = scan
        .iter()
        .find(|f| f.item == "gen.generator")
        .map(|f| f.value.clone())
        .unwrap_or_default();
    println!("generator: {generator}");
    for fact in &scan {
        if fact.item == "reference-commit" || fact.item.starts_with("blocker.") {
            println!("  {}: {}", fact.item, fact.value);
        }
    }
    ExitCode::SUCCESS
}
