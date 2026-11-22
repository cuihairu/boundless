//! BW-21-02-05: the Python SDK, delivered and driven by the simple example.
//!
//! The Python SDK is delivered as Python files under `cases/python_sdk/`: the
//! SDK itself ([`SDK_FILE`]), a scripted server ([`SERVER_FILE`]), the simple
//! example ([`EXAMPLE_FILE`]), the refusal example ([`REFUSAL_FILE`]) and the
//! pre-login guard probe ([`GUARD_FILE`]).  The simple example is the
//! acceptance: it logs in, receives the events the server sends in order, and
//! exits.  [`run_fixture`] runs it under the system Python interpreter and
//! [`EXPECTED_TRANSCRIPT`] is the transcript the run must print, line for
//! line.
//!
//! A runner without a Python interpreter records a gap rather than a
//! failure, the way the other tool-gated runs do: every probe here returns
//! `None` instead of an answer.

use std::path::{Path, PathBuf};
use std::process::Command;

/// The delivered SDK.
pub const SDK_FILE: &str = "bw_sdk.py";

/// The scripted server the example talks to.
pub const SERVER_FILE: &str = "simple_server.py";

/// The simple example: log in, receive the events, and exit.
pub const EXAMPLE_FILE: &str = "simple.py";

/// The example that runs against a server refusing the login.
pub const REFUSAL_FILE: &str = "fail.py";

/// The probe that checks the pre-login guards.
pub const GUARD_FILE: &str = "guards.py";

/// The files the delivery consists of.
pub const DELIVERED_FILES: &[&str] = &[
    SDK_FILE,
    SERVER_FILE,
    EXAMPLE_FILE,
    REFUSAL_FILE,
    GUARD_FILE,
];

/// The expected transcript of the simple example: the login, the four
/// lifecycle events in order, and the logout.
pub const EXPECTED_TRANSCRIPT: &[&str] = &[
    "LOGIN 1",
    "EVENT created 1",
    "EVENT entered 1",
    "EVENT left 1",
    "EVENT destroyed 1",
    "LOGOUT",
];

/// The expected transcript of the guard probe: both pre-login calls refuse.
pub const EXPECTED_GUARDS: &[&str] =
    &["GUARD recv : not logged in", "GUARD logout : not logged in"];

/// The fixture directory holding the delivered Python SDK.
pub fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("cases")
        .join("python_sdk")
}

/// The interpreter this module drives, or `None` when none is on the runner.
pub fn python_interpreter() -> Option<&'static str> {
    ["python3", "python"].iter().copied().find(|name| {
        Command::new(name)
            .arg("--version")
            .output()
            .is_ok_and(|out| out.status.success())
    })
}

/// What one fixture printed and how it ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FixtureRun {
    /// The process's exit status, or -1 when it did not report one.
    pub status: i32,
    /// What it printed, one entry per line.
    pub stdout: Vec<String>,
    /// What it reported, one entry per line.
    pub stderr: Vec<String>,
}

/// Run one fixture under the interpreter, with the fixture directory as its
/// import root and working directory. `None` when there is no interpreter.
pub fn run_fixture(name: &str) -> Option<FixtureRun> {
    let interpreter = python_interpreter()?;
    let dir = fixture_dir();
    let out = Command::new(interpreter)
        .arg(dir.join(name))
        .current_dir(&dir)
        .output()
        .ok()?;
    Some(FixtureRun {
        status: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(String::from)
            .collect(),
        stderr: String::from_utf8_lossy(&out.stderr)
            .lines()
            .map(String::from)
            .collect(),
    })
}

/// Whether the simple example logs in, receives the events in order, and
/// exits cleanly — the acceptance. `None` when there is no interpreter.
pub fn simple_example_passes() -> Option<bool> {
    let run = run_fixture(EXAMPLE_FILE)?;
    Some(
        run.status == 0
            && run.stdout.iter().map(String::as_str).collect::<Vec<_>>() == EXPECTED_TRANSCRIPT,
    )
}

/// Whether the refusal example exits non-zero and names the refusal.
pub fn refusal_is_reported() -> Option<bool> {
    let run = run_fixture(REFUSAL_FILE)?;
    Some(run.status != 0 && run.stdout.join("\n").contains("LOGIN-REFUSED"))
}

/// Whether the pre-login guard probe refuses both calls, in order.
pub fn guards_hold() -> Option<bool> {
    let run = run_fixture(GUARD_FILE)?;
    Some(
        run.status == 0
            && run.stdout.iter().map(String::as_str).collect::<Vec<_>>() == EXPECTED_GUARDS,
    )
}

/// Whether every delivered file parses as Python, so the delivery is valid.
pub fn delivered_files_parse() -> Option<bool> {
    let interpreter = python_interpreter()?;
    let dir = fixture_dir();
    Some(DELIVERED_FILES.iter().all(|name| {
        Command::new(interpreter)
            .arg("-c")
            .arg("import ast, sys; ast.parse(open(sys.argv[1]).read())")
            .arg(dir.join(name))
            .output()
            .is_ok_and(|out| out.status.success())
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gap(name: &str) {
        eprintln!("{name}: no python interpreter on this runner, recorded as a gap");
    }

    #[test]
    fn the_delivered_files_parse_as_python() {
        match delivered_files_parse() {
            Some(true) => {}
            Some(false) => panic!("a delivered file does not parse"),
            None => gap("delivered_files_parse"),
        }
    }

    #[test]
    fn the_simple_example_logs_in_receives_events_and_exits() {
        match run_fixture(EXAMPLE_FILE) {
            Some(run) => {
                assert_eq!(run.status, 0, "the example did not exit cleanly");
                assert!(
                    run.stderr.is_empty(),
                    "the example reported: {}",
                    run.stderr.join("\n"),
                );
                // The transcript is the acceptance: login, the events in
                // order, then the logout.
                assert_eq!(
                    run.stdout.iter().map(String::as_str).collect::<Vec<_>>(),
                    EXPECTED_TRANSCRIPT,
                );
                assert_eq!(simple_example_passes(), Some(true));
            }
            None => gap("simple_example"),
        }
    }

    #[test]
    fn the_simple_example_transcript_is_deterministic() {
        let first = match run_fixture(EXAMPLE_FILE) {
            Some(run) => run,
            None => return gap("simple_example_transcript"),
        };
        let second = match run_fixture(EXAMPLE_FILE) {
            Some(run) => run,
            None => return gap("simple_example_transcript"),
        };

        // Two runs, the same transcript: the example is reproducible.
        assert_eq!(first.stdout, second.stdout);
        assert_eq!(first.status, second.status);
        assert!(first.status == 0);
    }

    #[test]
    fn a_refused_login_exits_non_zero_and_names_it() {
        match run_fixture(REFUSAL_FILE) {
            Some(run) => {
                assert_ne!(run.status, 0, "a refused login must not look clean");
                assert!(
                    run.stdout.join("\n").contains("LOGIN-REFUSED"),
                    "the refusal is not named: {}",
                    run.stdout.join("\n"),
                );
                assert_eq!(refusal_is_reported(), Some(true));
            }
            None => gap("refusal"),
        }
    }

    #[test]
    fn the_sdk_refuses_to_receive_or_logout_before_login() {
        match run_fixture(GUARD_FILE) {
            Some(run) => {
                assert_eq!(
                    run.stdout.iter().map(String::as_str).collect::<Vec<_>>(),
                    EXPECTED_GUARDS,
                    "a pre-login call was accepted: {}",
                    run.stdout.join("\n"),
                );
                assert_eq!(guards_hold(), Some(true));
            }
            None => gap("guards"),
        }
    }
}
