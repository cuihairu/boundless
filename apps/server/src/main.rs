//! BW-00-03-05 minimal server entry point.
//!
//! Starts the selected server role, keeps it running until a stop signal,
//! writes a startup log and exits cleanly.

use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    let role = args.get(1).map(String::as_str).unwrap_or("");

    match server_runtime::app::run(role) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("boundless-server: {err}");
            ExitCode::FAILURE
        }
    }
}
