//! BW-04 differential evidence: the Rust dump must be byte-equal to the JSON
//! captured from the stock `process_defs` binary on the same res tree
//! (`docs/review/reference/*.json`). The fixture res roots live in the
//! reference checkout, so a run only happens when its env var points at one.

use entitydef::diagnostics::Diagnostics;
use entitydef::dump::load_and_dump;

const ORACLE_DIR: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../docs/review/reference"
);

fn compare(env_var: &str, oracle_file: &str) {
    let Some(root) = std::env::var_os(env_var) else {
        eprintln!("skipping: set {env_var} to the fixture res root");
        return;
    };

    let mut diag = Diagnostics::new();
    let actual = load_and_dump(std::path::Path::new(&root), &mut diag)
        .expect("parse and dump");

    let oracle_path = format!("{ORACLE_DIR}/{oracle_file}");
    let expected =
        std::fs::read_to_string(&oracle_path).expect("oracle file readable");
    assert!(
        actual == expected,
        "dump differs from the process_defs oracle {oracle_path}"
    );
}

#[test]
fn fantasydemo_dump_matches_the_process_defs_oracle() {
    compare(
        "BW04_FD_RES",
        "fantasydemo_entitydef.json",
    );
}

#[test]
fn client_integration_dump_matches_the_process_defs_oracle() {
    compare(
        "BW04_CI_RES",
        "client_integration_entitydef.json",
    );
}
