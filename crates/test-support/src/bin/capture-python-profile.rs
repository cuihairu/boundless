//! Capture Python call timings for a script (BW-23-01-02).
//!
//! Usage: `capture-python-profile <output.tsv> [script-file]`
//!
//! Runs the known script (or the script source in `[script-file]`) inside the
//! embedded interpreter under the profile shim and writes the per-function TSV
//! to `[output.tsv]`, printing the captured function count.  This runs in its
//! own process because python-host allows one interpreter per process.

use std::path::PathBuf;
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 || args.len() > 3 {
        eprintln!("usage: capture-python-profile <output.tsv> [script-file]");
        return ExitCode::from(2);
    }

    let output = PathBuf::from(&args[1]);
    let script = if let Some(script_file) = args.get(2) {
        match std::fs::read_to_string(script_file) {
            Ok(source) => source,
            Err(error) => {
                eprintln!("error: cannot read {script_file}: {error}");
                return ExitCode::from(1);
            }
        }
    } else {
        test_support::python_profile::KNOWN_SCRIPT.to_string()
    };

    if !test_support::python_profile::capture_script(&script, &output) {
        eprintln!("error: the embedded interpreter could not run the script");
        return ExitCode::from(1);
    }

    // Read back through the parser so the printed count is what the caller's
    // test will see.
    let text = std::fs::read_to_string(&output).unwrap_or_default();
    let rows = test_support::python_profile::parse_rows(&text);
    println!("{} functions", rows.len());
    for row in &rows {
        println!(
            "  {} calls={} total={}ns",
            row.name, row.calls, row.total_nanos
        );
    }
    ExitCode::SUCCESS
}
