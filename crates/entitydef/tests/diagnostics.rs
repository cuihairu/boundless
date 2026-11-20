//! BW-04-04-02 differential: a broken `.def` must produce the reference's own
//! diagnostic chain, naming the file and the field, in the reference's order.
//! The oracle is the stock `process_defs` binary's stderr on the committed
//! fixture (`docs/review/fixtures/bad_def/res`), captured live on this
//! machine into `docs/review/reference/bad_def_diagnostics.txt`.

use entitydef::diagnostics::Diagnostics;
use entitydef::dump::load_and_dump;

/// Strip the console decoration the reference's debug filter puts in front of
/// every message: `LEVEL: [Component] message` -> `message`. A bare console
/// line (no component) is kept whole.
fn strip_console_prefix(console_line: &str) -> String {
    let Some((_, rest)) = console_line.split_once(": ") else {
        return console_line.to_string();
    };
    match rest.strip_prefix('[') {
        Some(after) => match after.split_once("] ") {
            Some((_, message)) => message.to_string(),
            None => rest.to_string(),
        },
        None => rest.to_string(),
    }
}

#[test]
fn a_broken_def_reports_the_reference_file_and_field_diagnostics() {
    let root = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../docs/review/fixtures/bad_def/res"
    );
    let mut diag = Diagnostics::new();
    let outcome = load_and_dump(std::path::Path::new(root), &mut diag);

    // The stock binary's final console line, exit code 1.
    assert_eq!(outcome.unwrap_err(), "Failed to parse .def files");

    let oracle = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../docs/review/reference/bad_def_diagnostics.txt"
    ))
    .expect("oracle readable");
    let mut console: Vec<String> = oracle.lines().map(|line| format!("{line}\n")).collect();
    let summary = console.pop().expect("oracle non-empty");
    assert_eq!(
        strip_console_prefix(&summary).trim_end(),
        "Failed to parse .def files"
    );

    let expected: Vec<String> = console
        .iter()
        .map(|line| strip_console_prefix(line))
        .collect();
    assert_eq!(diag.messages(), expected);
}
