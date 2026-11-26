//! BW-15-02-01: each reference role starts and stops under the signal
//! faces the reference's own init chains register. The acceptance is the
//! topology's start/close contract: a role that starts is a live process,
//! the controlled stop returns `0`, and the other signals take the same
//! dispositions the reference gives them.
//!
//! The matrix (`server_runtime::app::run`, the generic `bwMainT` body):
//!
//! - SIGUSR1 is the controlled stop (`machine_guard.hpp:714`
//!   `setControlledShutdown`): the handler logs
//!   `ServerApp::shutDown: shutting down` (`server_app.cpp:542-546`),
//!   breaks the loop, and the role exits `0` after the shutdown tail.
//! - SIGINT is registered by `ServerApp::init` (`server_app.cpp:240`);
//!   the base arm resets the disposition and re-raises
//!   (`server_app.cpp:556-570`), so the process dies *on* SIGINT and the
//!   shutdown tail never runs.
//! - SIGQUIT is registered by `EntityApp::init` (`entity_app.cpp:91`)
//!   into the immediate slot (`signal_processor.cpp:306-308`): the
//!   CRITICAL text prints straight from signal context and the abort
//!   surfaces as SIGABRT.
//! - SIGTERM has no face -- like the reference, the default disposition
//!   takes the process.
//!
//! The signals go through `/bin/kill` so the test needs no `libc`
//! dev-dependency (which would churn `Cargo.lock`).

use std::io::{BufRead, BufReader};
use std::os::unix::process::ExitStatusExt;
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
/// (`"USR1"`, `"INT"`, `"QUIT"`, `"TERM"`), and return the exit status
/// plus the full stderr.
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

    // Wait for the running line, bounded. It prints after every signal
    // face is installed and the Mercury interface is up, so a signal
    // delivered from here on is the face's to answer. The startup pays
    // the machined query's two-second timeout on a machined-less host
    // (`bwservice.hpp:113-119`), and the bound keeps polling: the child
    // may simply be slow under load. Every line seen on the way -- the
    // startup lines carry the interface observables -- is kept, not
    // consumed by the wait.
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut started = false;
    let mut startup_lines = Vec::new();
    loop {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(line) => {
                startup_lines.push(line.clone());
                if line.contains("is running") {
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
        panic!("{role}: never reached 'is running'");
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
    // drains to EOF and disconnects. Join it, then collect every line --
    // the startup lines the wait loop read first, then the rest.
    let _ = reader.join();
    let stderr: String = startup_lines.into_iter().chain(rx.try_iter()).collect();
    (status, stderr)
}

#[test]
fn every_role_starts_and_shuts_down_on_the_controlled_signal() {
    for role in ROLES {
        let (status, stderr) = start_and_stop(role, "USR1");
        assert!(
            status.success(),
            "{role}: non-zero exit on the controlled stop: {status:?}\n{stderr}"
        );
        assert!(
            stderr.contains("ServerApp::shutDown: shutting down"),
            "{role}: missing the shutDown log:\n{stderr}"
        );
        assert!(
            stderr.contains("dispatchSignal: SIGUSR1"),
            "{role}: missing the dispatch line:\n{stderr}"
        );
        // The interface joined the loop (bwservice.hpp:113-125): the
        // machined query failed on this machined-less host, and the
        // default interface name went to the socket. The reference logs
        // both lines verbatim.
        assert!(
            stderr
                .contains("bwMainT: Unable to determine internal interface via BWMachineD query."),
            "{role}: missing the machined-query warning:\n{stderr}"
        );
        assert!(
            stderr.contains("Couldn't parse interface spec 'bwmachined' so using all interfaces"),
            "{role}: the interface did not come up on the default name:\n{stderr}"
        );
        assert!(
            stderr.contains("has shut down."),
            "{role}: missing the shutdown line:\n{stderr}"
        );
    }
}

#[test]
fn the_runners_lane_budget_stops_the_server_alone() {
    // `ci/runners.sh` starts each role with `CI_RUNNERS_TIMEOUT` and
    // sends nothing else: the process must come up and stop itself on
    // that budget, or the lane would hold the runner for the job
    // timeout. The budget targets the main thread with the controlled
    // stop (SIGUSR1), so the stop goes through the same drain the
    // operator's signal takes. The wait is bounded, so a regression
    // shows up as a failure rather than a hung lane.
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
    assert!(
        stderr.contains("dispatchSignal: SIGUSR1"),
        "the budget stop drains through the loop:\n{stderr}"
    );
    assert!(stderr.contains("has shut down."), "{stderr}");
}

#[test]
fn sigint_dies_through_the_base_reraise() {
    // `ServerApp::onSignalled`'s base arm resets the disposition and
    // re-raises (server_app.cpp:556-570), so the process dies on the
    // signal itself and the shutdown tail never runs.
    let (status, stderr) = start_and_stop("baseapp", "INT");
    assert_eq!(
        status.signal(),
        Some(2), // SIGINT
        "SIGINT must kill the process as the reference re-raise does:\n{stderr}"
    );
    assert!(
        stderr.contains("dispatchSignal: SIGINT"),
        "the drain logs the signal before the arm:\n{stderr}"
    );
    assert!(
        !stderr.contains("has shut down."),
        "SIGINT must not reach the shutdown tail:\n{stderr}"
    );
}

#[test]
fn sigquit_takes_the_critical_slot() {
    // `EntityApp::onSignalled`'s SIGQUIT arm prints the CRITICAL text
    // (the suspected manager for a `BaseApp` is the `BaseAppMgr`) and
    // aborts, which surfaces as SIGABRT. The immediate slot runs before
    // the drain, so no dispatch line and no shutdown tail.
    let (status, stderr) = start_and_stop("baseapp", "QUIT");
    assert_eq!(
        status.signal(),
        Some(6), // SIGABRT
        "SIGQUIT must end in the CRITICAL abort:\n{stderr}"
    );
    assert!(
        stderr.contains("Received QUIT signal."),
        "missing the CRITICAL text:\n{stderr}"
    );
    assert!(
        stderr.contains("BaseAppMgr killing this BaseApp"),
        "the CRITICAL text names the suspected manager:\n{stderr}"
    );
    assert!(
        !stderr.contains("has shut down."),
        "SIGQUIT must not reach the shutdown tail:\n{stderr}"
    );
}

#[test]
fn sigterm_takes_the_default_disposition() {
    // The reference registers no SIGTERM face: the default disposition
    // takes the process, with neither a dispatch line nor a shutdown
    // tail.
    let (status, stderr) = start_and_stop("baseapp", "TERM");
    assert_eq!(
        status.signal(),
        Some(15), // SIGTERM
        "SIGTERM must kill the process at the default disposition:\n{stderr}"
    );
    assert!(
        !stderr.contains("dispatchSignal"),
        "SIGTERM has no face, so the drain never sees it:\n{stderr}"
    );
    assert!(
        !stderr.contains("has shut down."),
        "SIGTERM must not reach the shutdown tail:\n{stderr}"
    );
}

/// BW-14-04-03: on its first start the role initialises the resource roots
/// and the `server/bw.xml` chain from the command line (the reference's
/// `BIGWORLD_MAIN`). The chain logs nothing when it locates `bw.xml` and
/// the reference's failure lines when it does not (BW-01-04-02/03), so the
/// located-vs-unlocated difference is observable on the process's own
/// stderr — no new output had to be invented.
///
/// `CI_RUNNERS_TIMEOUT` stops the role on its own, which keeps this a
/// start-and-stop observation rather than a hang.
fn first_start_stderr(res_path: Option<&str>) -> String {
    let mut cmd = Command::new(server_bin());
    cmd.arg("baseapp").env("CI_RUNNERS_TIMEOUT", "1");
    match res_path {
        Some(res) => {
            cmd.env("BW_RES_PATH", res);
        }
        None => {
            cmd.env_remove("BW_RES_PATH");
        }
    }
    let output = cmd
        .output()
        .unwrap_or_else(|e| panic!("baseapp: spawn: {e}"));
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(
        output.status.success(),
        "baseapp: non-zero exit on its first start: {:?}\n{stderr}",
        output.status
    );
    stderr
}

/// The tracked fantasydemo res tree (BW-14-01): `server/bw.xml` plus the
/// sample entity definitions, as an install would lay them down.
fn fixture_res() -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs/review/fixtures/fantasydemo_def/res");
    assert!(
        path.join("server/bw.xml").is_file(),
        "fixture res tree missing at {}",
        path.display()
    );
    // An absolute path: `BW_RES_PATH` parts are used as given, and a
    // relative one would be anchored at the executable's directory.
    path.canonicalize()
        .expect("canonicalise the fixture res root")
        .to_string_lossy()
        .into_owned()
}

#[test]
fn first_start_locates_the_installed_style_res_tree() {
    let stderr = first_start_stderr(Some(&fixture_res()));
    assert!(
        !stderr.contains("Could not find"),
        "the located chain still complained:\n{stderr}"
    );
    assert!(
        !stderr.contains("No configuration section."),
        "the chain resolved no config:\n{stderr}"
    );
}

#[test]
fn first_start_without_a_res_tree_logs_the_reference_failure() {
    // The last res-path fallback is `~/.bwmachined.conf` of the real uid;
    // neither the dev machines nor the CI runners carry one, so with
    // `BW_RES_PATH` removed nothing resolves and the reference's
    // "Could not find" line is the expected observable.
    let stderr = first_start_stderr(None);
    assert!(
        stderr.contains("Could not find"),
        "the unlocated first start did not log the reference failure:\n{stderr}"
    );
}

#[test]
fn a_configured_internal_interface_takes_the_deprecation_warning() {
    // `bwMainT` warns before the interface comes up when the option is
    // set at all (bwservice.hpp:134-140): the setting is deprecated, and
    // the default rides the broadcast route instead. The role still
    // starts, and the socket follows the configured name -- here a name
    // no such device answers to, so the fallback line proves the value
    // was read from the role's own config section.
    let res = std::env::temp_dir().join(format!("role-ii-{}", std::process::id()));
    std::fs::create_dir_all(res.join("server")).expect("mkdir the res tree");
    std::fs::write(
        res.join("server/bw.xml"),
        "<root>\n\t<baseApp>\n\t\t<internalInterface>eth9</internalInterface>\n\t</baseApp>\n</root>\n",
    )
    .expect("write the fixture bw.xml");

    let stderr = first_start_stderr(Some(&res.to_string_lossy()));
    let _ = std::fs::remove_dir_all(&res);

    assert!(
        stderr.contains("internalInterface set to 'eth9' in bw.xml."),
        "the configured name takes the deprecation warning:\n{stderr}"
    );
    assert!(
        stderr.contains("Couldn't parse interface spec 'eth9' so using all interfaces"),
        "the socket went to the configured interface:\n{stderr}"
    );
    assert!(
        stderr.contains("has shut down."),
        "the role still starts and stops cleanly:\n{stderr}"
    );
}
