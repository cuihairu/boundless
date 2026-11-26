//! BW-15-01-01: an idle event loop stops on demand.
//!
//! The acceptance: **the three platforms can stop an idle loop**
//! (`三端可停止空闲循环`). The loop face here rides the poller legs that
//! exist — Linux epoll (measured on this host) and macOS kqueue (the
//! same `create`/`ctl`/`wait` shape) — and is a rerunnable fixture for
//! the windows leg once its poller backend lands (the windows `Wakeup`
//! socketpair exists, the windows poller does not). The row stays open
//! for the three-end acceptance.
//!
//! Four faces:
//!
//! - **Wakeup interrupt.** The loop is blocked in `wait` with a one-minute
//!   budget when a controller thread triggers the wakeup: the blocked
//!   wait returns with the wakeup descriptor readable, not by timeout —
//!   a timeout return would report zero events, so `wakeups == 1` with
//!   the loop exiting is the interrupt proof. The trigger→wake latency
//!   is recorded, never asserted.
//! - **Stop flag before entry.** The flag preset, zero budget: the loop
//!   exits on its first iteration without any wakeup.
//! - **Re-arm.** Two trigger rounds with `consume` between: the loop
//!   services both interruptions in one session, which is what a server
//!   loop does across ticks.
//! - **Budget expiry.** No trigger at all, a 10 ms budget: five idle
//!   iterations then a stop — the loop also ends without a wakeup.
//!
//! The record sink writes `metric<TAB>value` rows to stdout and, when
//! `PLATFORM_IDLE_OUT` is set, to that path:
//!
//! ```sh
//! PLATFORM_IDLE_OUT=<abs path> cargo test -p platform \
//!     --test idle_loop_stop -- --nocapture
//! ```

#![cfg(unix)]

use std::fs;
use std::io::Write as _;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use platform::poller;
use platform::wakeup::Wakeup;

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
        if let Ok(path) = std::env::var("PLATFORM_IDLE_OUT") {
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

/// What one idle-loop session did. `wake_at` carries the instant each
/// wakeup event was serviced, so the caller can pair it with the
/// controller's trigger instants and report the interrupt latency.
struct LoopResult {
    iterations: usize,
    wakeups: usize,
    exit_reason: &'static str,
    wake_at: Vec<Instant>,
}

/// The idle loop under test: register the wakeup read side, then wait on
/// the poller budget after budget until the stop condition. Exit when the
/// stop flag is set (after `stop_after_wakeups` wakeup services), or when
/// `max_iterations` idle iterations ran.
fn run_idle_loop(
    wake: &Wakeup,
    stop: &AtomicBool,
    entered: &AtomicBool,
    budget_ms: i32,
    stop_after_wakeups: usize,
    max_iterations: usize,
) -> LoopResult {
    let epfd = poller::create(4);
    assert!(epfd >= 0, "poller create");
    assert_eq!(
        poller::ctl(epfd, poller::CTL_ADD, wake.read_fd(), poller::EPOLLIN),
        0,
        "register the wakeup read side"
    );

    let mut events = [poller::PollEvent { events: 0, fd: -1 }; 4];
    let mut result = LoopResult {
        iterations: 0,
        wakeups: 0,
        exit_reason: "max-iterations",
        wake_at: Vec::new(),
    };

    entered.store(true, Ordering::SeqCst);
    loop {
        result.iterations += 1;
        let n = poller::wait(epfd, &mut events, budget_ms);
        assert!(n >= 0, "poller wait failed");
        for event in &events[..n as usize] {
            if event.fd == wake.read_fd() {
                result.wake_at.push(Instant::now());
                result.wakeups += 1;
                wake.consume(); // drain and re-arm
            }
        }
        if stop.load(Ordering::SeqCst) && result.wakeups >= stop_after_wakeups {
            result.exit_reason = if result.wakeups > 0 {
                "wakeup-stop"
            } else {
                "stop-flag"
            };
            break;
        }
        if result.iterations >= max_iterations {
            break;
        }
    }

    assert_eq!(platform::socket::close(epfd), 0, "close the poller");
    result
}

/// A wakeup with its controller: the controller waits for the loop to
/// enter its first wait, sleeps so the wait is genuinely blocked, then
/// fires the schedule (stop before each trigger as given).
fn controller(
    wake: Arc<Wakeup>,
    entered: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    delays_ms: &[u64],
) -> Vec<Instant> {
    while !entered.load(Ordering::SeqCst) {
        thread::yield_now();
    }
    thread::sleep(Duration::from_millis(20)); // let the wait block for real
    let mut fired = Vec::new();
    for &delay in delays_ms {
        thread::sleep(Duration::from_millis(delay));
        fired.push(Instant::now());
        wake.trigger();
        stop.store(true, Ordering::SeqCst);
    }
    fired
}

#[test]
fn the_idle_loop_stops_on_demand() {
    let mut rec = Record::new();

    // --- face 1: a blocked wait is interrupted by the wakeup --------------
    let wake = Arc::new(Wakeup::new().expect("wakeup pipe"));
    let stop = Arc::new(AtomicBool::new(false));
    let entered = Arc::new(AtomicBool::new(false));
    let runner = {
        let (wake, stop, entered) = (Arc::clone(&wake), Arc::clone(&stop), Arc::clone(&entered));
        thread::spawn(move || run_idle_loop(&wake, &stop, &entered, 60_000, 1, usize::MAX))
    };
    let fired = controller(
        Arc::clone(&wake),
        Arc::clone(&entered),
        Arc::clone(&stop),
        &[0],
    );
    let r1 = runner.join().expect("loop thread");
    assert_eq!(r1.wakeups, 1, "one wakeup serviced");
    assert_eq!(r1.exit_reason, "wakeup-stop", "the wakeup ended the loop");
    // A timeout return carries no events; the serviced event proves the
    // blocked wait was interrupted rather than expired.
    assert!(
        r1.iterations < 100,
        "the loop ended on the interrupt, not on timeouts"
    );
    let us1 = r1.wake_at[0].duration_since(fired[0]).as_micros() as u64;
    rec.add("interrupt1_us", us1.to_string());
    rec.add("interrupt1_iterations", r1.iterations.to_string());

    // --- face 2: the stop flag set before entry --------------------------
    let wake = Wakeup::new().expect("wakeup pipe");
    let r2 = run_idle_loop(
        &wake,
        &AtomicBool::new(true),
        &AtomicBool::new(false),
        0,
        0,
        100,
    );
    assert_eq!(r2.wakeups, 0, "no wakeup on the preset face");
    assert_eq!(r2.exit_reason, "stop-flag", "the preset flag ends it");
    assert_eq!(r2.iterations, 1, "one idle iteration, then out");
    rec.add("preset_iterations", r2.iterations.to_string());

    // --- face 3: trigger, consume, trigger again --------------------------
    let wake = Arc::new(Wakeup::new().expect("wakeup pipe"));
    let stop = Arc::new(AtomicBool::new(false));
    let entered = Arc::new(AtomicBool::new(false));
    let runner = {
        let (wake, stop, entered) = (Arc::clone(&wake), Arc::clone(&stop), Arc::clone(&entered));
        thread::spawn(move || run_idle_loop(&wake, &stop, &entered, 60_000, 2, usize::MAX))
    };
    let fired = controller(
        Arc::clone(&wake),
        Arc::clone(&entered),
        Arc::clone(&stop),
        &[10, 10],
    );
    let r3 = runner.join().expect("loop thread");
    assert_eq!(r3.wakeups, 2, "both interruptions serviced");
    assert_eq!(r3.exit_reason, "wakeup-stop", "the second wakeup ended it");
    for (i, (fired_at, woke_at)) in fired.iter().zip(&r3.wake_at).enumerate() {
        rec.add(
            &format!("rearm{}_us", i + 1),
            woke_at.duration_since(*fired_at).as_micros().to_string(),
        );
    }

    // --- face 4: budget expiry with no wakeup -----------------------------
    let wake = Wakeup::new().expect("wakeup pipe");
    let r4 = run_idle_loop(
        &wake,
        &AtomicBool::new(false),
        &AtomicBool::new(false),
        10,
        0,
        5,
    );
    assert_eq!(r4.wakeups, 0, "the budget face has no wakeup");
    assert_eq!(r4.iterations, 5, "five idle iterations, then out");
    assert_eq!(r4.exit_reason, "max-iterations", "the budget ended it");
    rec.add("budget_iterations", r4.iterations.to_string());

    rec.add(
        "host",
        format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH),
    );
    rec.flush();
}
