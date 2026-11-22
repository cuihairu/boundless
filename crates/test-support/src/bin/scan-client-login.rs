//! Regenerate the minimal client-login baseline catalog (BW-00-02-04).
//!
//! Usage: `scan-client-login <reference-checkout> [output]`
//!
//! `<reference-checkout>` is the BigWorld repository root. The catalog is
//! written to `[output]` (default stdout), and the fact count is printed to
//! stdout. `login-messages` and `logon-statuses` are re-derived from the
//! checkout; the request/callback sequence, the named blockers and the real
//! client run are the frozen committed artifacts, re-emitted verbatim.

use std::path::Path;
use std::path::PathBuf;
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 || args.len() > 3 {
        eprintln!("usage: scan-client-login <reference-checkout> [output]");
        return ExitCode::from(2);
    }

    let root = PathBuf::from(&args[1]);
    let interface = root.join("programming/bigworld/lib/connection/login_interface.hpp");
    if !interface.is_file() {
        eprintln!(
            "error: {} is not a BigWorld checkout (no programming/bigworld/lib/connection/login_interface.hpp)",
            root.display()
        );
        return ExitCode::from(2);
    }

    let scan = test_support::client_login::scan_reference(&root);
    let catalog = test_support::client_login::render(&scan);

    if let Some(output) = args.get(2) {
        if let Err(e) = std::fs::write(output, &catalog) {
            eprintln!("error: cannot write {}: {e}", Path::new(output).display());
            return ExitCode::from(1);
        }
    } else {
        print!("{catalog}");
    }

    let requests = scan
        .iter()
        .filter(|f| f.item.starts_with("request."))
        .count();
    let callbacks = scan
        .iter()
        .filter(|f| f.item.starts_with("callback."))
        .count();
    let attempt = scan
        .iter()
        .filter(|f| f.item.starts_with("attempt."))
        .count();
    println!("{requests} request steps, {callbacks} callback steps, {attempt} attempt lines");
    for fact in &scan {
        if fact.item == "login-messages" {
            println!("  login-messages: {}", fact.value);
        }
        if fact.item == "reference-commit" {
            println!("  reference-commit: {}", fact.value);
        }
    }
    ExitCode::SUCCESS
}
