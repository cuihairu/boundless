//! BW-15-01-03: process control ends with the right exit statuses.
//!
//! The acceptance: **the three platforms shut the server down normally
//! and reap children** (`三端可正常关服并回收子进程`). The `process`
//! module's `spawn`/`wait`/`try_wait`/`signal` contract is exercised
//! end to end here; this host measures the Linux leg, and the same test
//! is the rerunnable fixture for the windows/macOS runners (the row
//! stays open for the three-end acceptance).
//!
//! Five faces:
//!
//! - **Exit code round-trip.** `sh -c "exit 7"`: `wait` decodes
//!   `code == Some(7)`, `signal == None`.
//! - **Zero exit.** `sh -c "true"`: `code == Some(0)` — the clean
//!   shutdown shape.
//! - **Reap while running.** `try_wait` returns `None` while the child
//!   sleeps, then `Some` once it ends — the polling half of the reaper.
//! - **Signal death** (unix — the windows `signal` arm is a stub).
//!   `sleep 30` child, `SIGTERM` from the parent: `wait` reports
//!   `signal == Some(SIGTERM)`, `code == None`, and the wait returns
//!   promptly rather than after the child's own sleep.
//! - **Reap many.** Sixteen children, each with its own exit code,
//!   waited in spawn order with every code matching — no lost or
//!   mismatched reaps.
//!
//! Wall-clock readings (spawn/wait cost) are recorded, never asserted.
//! The record sink writes `metric<TAB>value` rows to stdout and, when
//! `PROCESS_EXIT_OUT` is set, to that path:
//!
//! ```sh
//! PROCESS_EXIT_OUT=<abs path> cargo test -p platform \
//!     --test process_exit_baseline -- --nocapture
//! ```

use std::fs;
use std::io::Write as _;
use std::time::{Duration, Instant};

use platform::process::Child;

struct Record {
    rows: Vec<(String, String)>,
}

impl Record {
    fn new() -> Self {
        Self { rows: Vec::new() }
    }
    fn add(&mut self, key: &str, value: String) {
        println!("{}\t{}", key, value);
        self.rows.push((key.to_string(), value));
    }
    fn flush(self) {
        if let Ok(path) = std::env::var("PROCESS_EXIT_OUT") {
            if let Some(dir) = std::path::Path::new(&path).parent() {
                fs::create_dir_all(dir).expect("create output dir");
            }
            let mut f = fs::File::create(&path).expect("create output file");
            for (k, v) in &self.rows {
                writeln!(f, "{}\t{}", k, v).expect("write output file");
            }
        }
    }
}

/// The shell spelling of "exit with this code" per platform: the posix
/// shell on unix, `cmd` on windows. Both exit through the normal path so
/// the status word decodes as an exit code, not a signal.
#[cfg(unix)]
fn exit_argv(code: i32) -> [String; 3] {
    [
        "/bin/sh".to_string(),
        "-c".to_string(),
        format!("exit {code}"),
    ]
}

#[cfg(windows)]
fn exit_argv(code: i32) -> [String; 3] {
    [
        "cmd".to_string(),
        "/C".to_string(),
        format!("exit /b {code}"),
    ]
}

#[cfg(unix)]
fn sleep_argv(secs: u32) -> [String; 3] {
    [
        "/bin/sh".to_string(),
        "-c".to_string(),
        format!("sleep {secs}"),
    ]
}

#[cfg(windows)]
fn sleep_argv(secs: u32) -> [String; 3] {
    [
        "cmd".to_string(),
        "/C".to_string(),
        format!("timeout /t {secs} /nobreak >nul"),
    ]
}

fn spawn_exit(code: i32) -> Child {
    let argv = exit_argv(code);
    Child::spawn(&argv).expect("spawn the child")
}

#[test]
fn the_child_processes_end_with_the_right_statuses() {
    let mut rec = Record::new();

    // --- face 1: exit code round-trip -------------------------------------
    let start = Instant::now();
    let mut child = spawn_exit(7);
    let spawn_us = start.elapsed().as_micros() as u64;
    let start = Instant::now();
    let status = child.wait().expect("wait the child");
    let wait_us = start.elapsed().as_micros() as u64;
    assert_eq!(status.code, Some(7), "the exit code round-trips");
    assert_eq!(status.signal, None, "a normal exit is not a signal");
    rec.add("spawn_us", spawn_us.to_string());
    rec.add("wait_us", wait_us.to_string());

    // --- face 2: the clean (zero) exit ------------------------------------
    let mut child = spawn_exit(0);
    let status = child.wait().expect("wait the clean child");
    assert_eq!(status.code, Some(0), "the clean shutdown code is zero");
    assert_eq!(status.signal, None, "zero exit is not a signal");

    // --- face 3: reap while the child still runs --------------------------
    let mut child = Child::spawn(&sleep_argv(1)).expect("spawn the sleeper");
    let before = child.try_wait().expect("try_wait");
    assert_eq!(before, None, "a running child has no status yet");
    let poll_start = Instant::now();
    let deadline = poll_start + Duration::from_secs(10);
    let status = loop {
        if let Some(status) = child.try_wait().expect("try_wait") {
            break status;
        }
        assert!(
            Instant::now() < deadline,
            "the sleeper never ended (reap poll exhausted)"
        );
        std::thread::sleep(Duration::from_millis(20));
    };
    assert_eq!(status.code, Some(0), "the sleeper exited cleanly");
    assert_eq!(status.signal, None, "the sleeper was not signalled");
    rec.add("reap_poll_us", poll_start.elapsed().as_micros().to_string());

    // --- face 4: signal death (unix; the windows signal arm is a stub) ----
    #[cfg(unix)]
    {
        let mut child = Child::spawn(&sleep_argv(30)).expect("spawn the long sleeper");
        child
            .signal(libc::SIGTERM)
            .expect("signal the long sleeper");
        let start = Instant::now();
        let status = child.wait().expect("wait the signalled child");
        let sig_us = start.elapsed().as_micros() as u64;
        assert_eq!(
            status.signal,
            Some(libc::SIGTERM),
            "the status word names the signal"
        );
        assert_eq!(status.code, None, "a signal death has no code");
        assert!(
            sig_us < 5_000_000,
            "the wait ended on the signal, not the child's own sleep"
        );
        rec.add("signal_wait_us", sig_us.to_string());
    }

    // --- face 5: reap sixteen children, every code in place ---------------
    const MANY: usize = 16;
    let mut children: Vec<(usize, Child)> = (0..MANY)
        .map(|i| {
            let code = (i * 15 + 3) % 256; // varied, never a signal byte
            (code, spawn_exit(code as i32))
        })
        .collect();
    for (i, (code, child)) in children.iter_mut().enumerate() {
        let status = child.wait().expect("wait a many-child");
        assert_eq!(
            status.code,
            Some(*code as i32),
            "child {i} exits with its own code"
        );
        assert_eq!(status.signal, None, "child {i} was not signalled");
    }
    rec.add("reaped_many", MANY.to_string());

    rec.add(
        "host",
        format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH),
    );
    rec.flush();
}
