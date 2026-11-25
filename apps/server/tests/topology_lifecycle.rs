//! BW-15-02-01: the full topology starts and closes under one signal.
//! The acceptance is the topology's start/close contract: every role
//! runs as a child of one supervisor, and a single stop closes the whole
//! topology with every role exiting `0`.
//!
//! `server_runtime::topology::Topology::run` is the supervisor wiring
//! (BW-15-01-03: signal -> kill children -> wait/reap); the report it
//! returns carries each role's exit code, which is the observable the
//! acceptance names ("拓扑能够启动和关闭"). The stop it commands is the
//! controlled shutdown (SIGUSR1, `machine_guard.hpp:714`
//! `setControlledShutdown`), so each role ends through its own
//! `app::run` arm and exits `0`.
//!
//! Linux only: the stop readiness is polled from each child's
//! `/proc/<pid>/status` `SigCgt` mask (a role has its SIGUSR1 handler
//! caught before it prints the running line), and the supervisor's stop
//! signal is thread-directed so it cannot take a parallel test thread's
//! default disposition. Other platforms run the in-crate unit faces
//! instead.

#![cfg(target_os = "linux")]

use server_runtime::topology::Topology;
use std::ffi::OsString;
use std::thread;
use std::time::{Duration, Instant};

use platform::signals::{SIGTERM, SIGUSR1, thread_kill, thread_self};
use server_runtime::signals::{Blocker, Set};

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

#[test]
fn the_full_topology_starts_and_closes_on_one_signal() {
    // One topology entry per role: the server binary with the role as
    // its argv, the way `ci/runners.sh` starts each role.
    let entries: Vec<(String, Vec<OsString>)> = ROLES
        .iter()
        .map(|role| {
            (
                role.to_string(),
                vec![server_bin().into_os_string(), OsString::from(*role)],
            )
        })
        .collect();

    let topology = Topology::start(&entries).expect("start the topology");
    assert_eq!(topology.len(), ROLES.len());

    // Every role is a distinct live child, in start order.
    let pids = topology.pids();
    assert_eq!(pids.len(), ROLES.len());
    for (i, (name, pid)) in pids.iter().enumerate() {
        assert_eq!(name, ROLES[i], "the report keeps the start order");
        assert!(*pid > 0, "a live child pid");
    }
    assert!(
        pids.windows(2).all(|w| w[0].1 != w[1].1),
        "every role is its own process"
    );

    // The supervisor waits in its own thread; the watchdog only fires
    // once every child has caught the controlled-stop signal, so the
    // stop is never a default disposition racing the child's startup.
    let handle = thread::spawn(move || {
        let _early = Blocker::new(&Set::quit());
        let target = thread_self();
        let watch = thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                let ready = pids.iter().all(|(_, pid)| catches_the_stop_signal(*pid));
                if ready || Instant::now() > deadline {
                    break;
                }
                thread::sleep(Duration::from_millis(20));
            }
            let _ = thread_kill(target, SIGTERM);
        });
        let report = topology.run().expect("the topology runs");
        watch.join().expect("the watchdog joins");
        report
    });

    let report = handle.join().expect("the supervisor joins");
    assert_eq!(
        report.len(),
        ROLES.len(),
        "every role is in the shutdown report"
    );
    for (i, (name, status)) in report.iter().enumerate() {
        assert_eq!(name, ROLES[i], "the report keeps the start order");
        assert_eq!(status.code, Some(0), "{name} did not exit 0");
        assert_eq!(status.signal, None, "{name} died on a signal");
        assert!(status.success(), "{name} must exit successfully");
    }
}

/// Whether `pid` can take the controlled stop through its clean-stop
/// path: its `SigCgt` mask carries the `SIGUSR1` bit, meaning the role's
/// `app::run` has installed the controlled-stop handler. That install
/// happens before the running line prints, so a stop from here on walks
/// the handler arm instead of the default disposition.
fn catches_the_stop_signal(pid: i32) -> bool {
    let Ok(status) = std::fs::read_to_string(format!("/proc/{pid}/status")) else {
        return false;
    };
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("SigCgt:") {
            let Ok(mask) = u64::from_str_radix(rest.trim(), 16) else {
                return false;
            };
            return mask & (1 << (SIGUSR1 - 1)) != 0;
        }
    }
    false
}
