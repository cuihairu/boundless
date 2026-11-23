//! The `process_defs` entry point: reference-compatible options, help text
//! and exit codes over the entitydef dump contract (BW-04-04-01).

fn main() {
    let code = entitydef::cli::run(
        std::env::args().skip(1).collect(),
        &mut std::io::stdout(),
        &mut std::io::stderr(),
    );
    std::process::exit(code);
}
