//! BW-15-02-01: each reference role starts and shuts down on a quit
//! signal. The acceptance is the topology's start/close contract: a role
//! that starts is a live process and returns `0` when the server stops.
//!
//! `server_runtime::app` prints `[<role>] running` once it is waiting on
//! the quit signals and `[<role>] has shut down.` on the way out. The
//! signals go through `/bin/kill` so the test needs no `libc`
//! dev-dependency (which would churn `Cargo.lock`).

use std::io::{BufRead, BufReader};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

/// The reference roles (`server/*/`), in canonical order.
const ROLES: &[&str] = &[
    "baseapp",
    "baseappmgr",
    "cellapp",
    "cellappmgr",
    "dbapp",
    "dbappmgr",
    "loginapp",
    "reviver",
];

/// The compiled `boundless-server` binary, handed to integration tests by
/// Cargo as `CARGO_BIN_EXE_<bin name>`. The dash spelling is canonical;
/// the underscore spelling is a fallback for toolchains that fold it.
fn server_bin() -> std::path::PathBuf {
    if let Some(p) = option_env!("CARGO_BIN_EXE_boundless-server") {
        return std::path::PathBuf::from(p);
    }
    if let Some(p) = option_env!("CARGO_BIN_EXE_boundless_server") {
        return std::path::PathBuf::from(p);
    }
    panic!("CARGO_BIN_EXE_boundless-server is not set");
}

/// Spawn `role`, wait for the startup line, deliver `signal_name`
/// (`"TERM"`, `"INT"`, `"QUIT"`), and return the exit status plus the
/// full stderr.
fn start_and_stop(role: &str, signal_name: &str) -> (ExitStatus, String) {
    let mut child = Command::new(server_bin())
        .arg(role)
        .stderr(Stdio::piped())
        .spawn()
        .unwrap_or_else(|e| panic!("{role}: spawn: {e}"));
    let stderr = child.stderr.take().expect("stderr pipe");

    // Forward every stderr line to the test. The channel is unbounded,
    // so the reader never blocks on a send, and it exits on EOF -- which
    // arrives when the child closes the pipe at exit.
    let (tx, rx) = mpsc::channel::<String>();
    let reader = thread::spawn(move || {
        let mut reader = BufReader::new(stderr);
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line) {
                Ok(0) | Err(_) => break, // EOF or a closed pipe
                Ok(_) => {
                    if tx.send(line.clone()).is_err() {
                        break;
                    }
                }
            }
        }
    });

    // Wait for the startup line, bounded. A timeout keeps polling: the
    // child may simply be slow under load.
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut started = false;
    loop {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(line) => {
                if line.contains("] running") {
                    started = true;
                    break;
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if Instant::now() > deadline {
                    break;
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    if !started {
        let _ = child.kill();
        let _ = child.wait();
        panic!("{role}: never reached 'running'");
    }

    // Ask for shutdown. `/bin/kill` wraps `kill(2)`; avoiding a libc
    // dev-dependency keeps `Cargo.lock` untouched.
    let kill = Command::new("/bin/kill")
        .arg(format!("-{signal_name}"))
        .arg(child.id().to_string())
        .status()
        .unwrap_or_else(|e| panic!("{role}: /bin/kill -{signal_name}: {e}"));
    assert!(kill.success(), "{role}: /bin/kill -{signal_name} failed");

    // Reap with a bounded wait.
    let deadline = Instant::now() + Duration::from_secs(5);
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(e) => panic!("{role}: try_wait: {e}"),
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("{role}: did not exit after -{signal_name}");
        }
        thread::sleep(Duration::from_millis(50));
    };

    // The child has exited, so its stderr pipe is closed: the reader
    // drains to EOF and disconnects. Join it, then collect every line.
    let _ = reader.join();
    let stderr: String = rx.try_iter().collect();
    (status, stderr)
}

#[test]
fn every_role_starts_and_shuts_down_on_sigterm() {
    for role in ROLES {
        let (status, stderr) = start_and_stop(role, "TERM");
        assert!(
            status.success(),
            "{role}: non-zero exit after SIGTERM: {status:?}\n{stderr}"
        );
        assert!(
            stderr.contains("has shut down."),
            "{role}: missing the shutdown line:\n{stderr}"
        );
        assert!(
            stderr.contains("dispatchSignal: SIGTERM"),
            "{role}: missing the dispatch line:\n{stderr}"
        );
    }
}

#[test]
fn the_runners_lane_budget_stops_the_server_alone() {
    // `ci/runners.sh` starts each role with `CI_RUNNERS_TIMEOUT` and
    // sends nothing else: the process must come up and stop itself on
    // that budget, or the lane would hold the runner for the job
    // timeout. The stop goes through the same clean path an operator
    // signal takes. The wait is bounded, so a regression shows up as a
    // failure rather than a hung lane.
    let mut child = Command::new(server_bin())
        .arg("baseapp")
        .env("CI_RUNNERS_TIMEOUT", "1")
        .stderr(Stdio::piped())
        .spawn()
        .unwrap_or_else(|e| panic!("baseapp: spawn: {e}"));
    let stderr = child.stderr.take().expect("stderr pipe");

    let (tx, rx) = mpsc::channel::<String>();
    let reader = thread::spawn(move || {
        let mut reader = BufReader::new(stderr);
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line) {
                Ok(0) | Err(_) => break, // EOF or a closed pipe
                Ok(_) => {
                    if tx.send(line.clone()).is_err() {
                        break;
                    }
                }
            }
        }
    });

    let deadline = Instant::now() + Duration::from_secs(10);
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(e) => panic!("baseapp: try_wait: {e}"),
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("baseapp: did not stop on its own budget");
        }
        thread::sleep(Duration::from_millis(50));
    };

    let _ = reader.join();
    let stderr: String = rx.try_iter().collect();
    assert!(
        status.success(),
        "baseapp: non-zero exit on its own budget: {status:?}\n{stderr}"
    );
    assert!(stderr.contains("has shut down."), "{stderr}");
    assert!(stderr.contains("dispatchSignal: SIGTERM"), "{stderr}");
}

#[test]
fn sigint_and_sigquit_also_shut_down() {
    // SIGINT and SIGQUIT are held and consumed the same way. The base
    // reference re-raises SIGINT instead (ServerApp::onSignalled), so a
    // clean exit here is the minimal entry's documented deviation: a dev
    // server stops cleanly on Ctrl-C. The per-app SIGQUIT handler joins
    // with the full ServerApp wiring.
    for signal in ["INT", "QUIT"] {
        let (status, stderr) = start_and_stop("baseapp", signal);
        assert!(
            status.success(),
            "SIG{signal}: non-zero exit: {status:?}\n{stderr}"
        );
        assert!(
            stderr.contains(&format!("dispatchSignal: SIG{signal}")),
            "SIG{signal}: missing the dispatch line:\n{stderr}"
        );
    }
}
