//! Differences report between the two evidence tables (BW-00-04-06).
//!
//! Usage: `diff-stack <original.tsv> <new.tsv> [output]`
//!
//! Compares the original and new evidence tables and writes a differences
//! table (default stdout): one row per located difference, keyed by kind
//! (`field` / `timeout` / `order`) and role, so an injected difference is
//! locatable.

use std::path::Path;
use std::path::PathBuf;
use std::process::ExitCode;

fn read_side(path: &Path) -> Result<Vec<test_support::full_stack::Evidence>, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|err| format!("cannot read {}: {err}", path.display()))?;
    let rows = test_support::full_stack::parse_side(&text);
    if rows.is_empty() {
        return Err(format!("{} has no evidence rows", path.display()));
    }
    Ok(rows)
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 || args.len() > 4 {
        eprintln!("usage: diff-stack <original.tsv> <new.tsv> [output]");
        return ExitCode::from(2);
    }

    let original_path = PathBuf::from(&args[1]);
    let new_path = PathBuf::from(&args[2]);

    let original = match read_side(&original_path) {
        Ok(rows) => rows,
        Err(err) => {
            eprintln!("error: {err}");
            return ExitCode::from(2);
        }
    };
    let new = match read_side(&new_path) {
        Ok(rows) => rows,
        Err(err) => {
            eprintln!("error: {err}");
            return ExitCode::from(2);
        }
    };

    let diffs = test_support::full_stack::diff_reports(&original, &new);
    let table = test_support::full_stack::render_differences(&diffs);

    if let Some(output) = args.get(3) {
        if let Err(err) = std::fs::write(output, &table) {
            eprintln!("error: cannot write {}: {err}", Path::new(output).display());
            return ExitCode::from(1);
        }
    } else {
        print!("{table}");
    }

    let by_kind =
        |kind: test_support::full_stack::DiffKind| diffs.iter().filter(|d| d.kind == kind).count();
    println!(
        "{} differences located: {} field, {} timeout, {} order",
        diffs.len(),
        by_kind(test_support::full_stack::DiffKind::Field),
        by_kind(test_support::full_stack::DiffKind::Timeout),
        by_kind(test_support::full_stack::DiffKind::Order),
    );
    for diff in &diffs {
        println!("  {} [{}]: {}", diff.kind.label(), diff.role, diff.summary);
    }
    ExitCode::SUCCESS
}
