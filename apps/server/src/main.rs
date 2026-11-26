//! BW-00-03-05 minimal server entry point.
//!
//! Starts the selected server role, keeps it running until a stop signal,
//! writes a startup log and exits cleanly.
//!
//! BW-15-02-01 (the bw.xml half of its remaining work): as in the
//! reference's `BIGWORLD_MAIN`, the resource roots and the `server/bw.xml`
//! chain are initialised from the command line before the role runs. The
//! return values are dropped exactly as the reference does
//! (BW-01-04-03): a missing `bw.xml` is not fatal -- the options fall back
//! to their defaults and the failure is only logged. The chain the loop
//! resolves is observable on stderr either way: a located config logs
//! nothing, an unlocated one logs the reference's "Could not find" /
//! "No configuration section" lines. The config tree is then handed to
//! the role loop, whose `bwMainT` body reads the `internalInterface`
//! chain from it (`bwservice.hpp:120-121`).

use resources::bwresource::BwResource;
use server_runtime::config::BwConfig;
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    let role = args.get(1).map(String::as_str).unwrap_or("");

    let mut resource = BwResource::new();
    let _ = resource.init_from_args(&args);
    let mut config = BwConfig::new();
    let _ = config.init_from_args(&resource, &args);

    match server_runtime::app::run(role, &config) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("boundless-server: {err}");
            ExitCode::FAILURE
        }
    }
}
