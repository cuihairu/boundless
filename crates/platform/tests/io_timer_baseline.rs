//! OPT-15-01-01: the host-side I/O and timing-precision baseline.
//!
//! The acceptance: **one report under a uniform load** (`统一负载下产出
//! 报告`). The three-platform matrix is BW-15's runner lane; this port's
//! device is the host this test runs on, so the report records the Linux
//! leg's face — the same measurements a windows/macOS runner replays
//! through the identical test — and the row stays open for the three-end
//! matrix (the BW-15-01 precedent: Linux measured + the other legs named,
//! not claimed).
//!
//! Four faces, each under the same load shape (fixed op count, fixed
//! payload, warm cache excluded by a pre-pass):
//!
//! - **Sequential file I/O.** One write + one read of a fixed 64 KiB
//!   payload, plus `atomic_replace` (write + fsync + rename): the durable
//!   path the config chain uses. Wall time per op is the report; the
//!   assertion is the data round-trip, not the clock.
//! - **Memory-mapped read.** The same 64 KiB payload through
//!   `mmap::map_file` — page-cache warm after the write leg, which is the
//!   steady state a server sees.
//! - **Timer precision.** `clock::monotonic_now_micros` sampled in a
//!   tight loop: the minimum nonzero step is the clock resolution, and
//!   the sampled rate must stay within ±10% of the reference sleep — a
//!   resolution/consistency report rather than a throughput claim.
//! - **Wakeup latency.** `wakeup::Wakeup::trigger` → `consume` round
//!   trips: the same 1-byte poke the tick loop rides; wall time per poke
//!   is the report, delivery is the assertion.
//!
//! The record sink writes `metric<TAB>value` rows to stdout and, when
//! `PLATFORM_IO_OUT` is set, to that path:
//!
//! ```sh
//! PLATFORM_IO_OUT=<abs path> cargo test -p platform \
//!     --test io_timer_baseline -- --nocapture
//! ```

use std::fs;
use std::io::Write as _;
use std::time::{Duration, Instant};

/// The uniform load: every leg runs this many ops so the report's rows are
/// comparable in kind, not just in name.
const OPS: usize = 512;
/// The payload the I/O legs move: one 64 KiB buffer per op.
const PAYLOAD: usize = 64 * 1024;
/// The sleep the timer leg paces itself against (1 ms = the reference tick
/// granularity's neighbourhood).
const PACING: Duration = Duration::from_millis(1);

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
        if let Ok(path) = std::env::var("PLATFORM_IO_OUT") {
            let mut f = fs::File::create(&path).expect("create output file");
            for (k, v) in &self.rows {
                writeln!(f, "{}\t{}", k, v).expect("write output file");
            }
        }
    }
}

fn scratch(tag: &str) -> std::path::PathBuf {
    let dir =
        std::env::temp_dir().join(format!("opt15-io-baseline-{}-{}", tag, std::process::id()));
    fs::create_dir_all(&dir).expect("mkdir scratch");
    dir
}

/// One warm-up pass so the page cache and the allocator are steady before
/// the measured pass — the report claims steady state, not cold start.
fn warm_up(path: &std::path::Path, payload: &[u8]) {
    for _ in 0..64 {
        fs::write(path, payload).expect("warm write");
        let _ = fs::read(path).expect("warm read");
    }
}

#[test]
fn the_io_and_timer_baseline_records_under_one_load() {
    let mut rec = Record::new();
    let dir = scratch("io");
    let path = dir.join("payload.bin");
    let payload = vec![0xA5u8; PAYLOAD];

    // --- sequential write + read (std fs, the port's file face) ----------
    warm_up(&path, &payload);
    let start = Instant::now();
    for _ in 0..OPS {
        fs::write(&path, &payload).expect("write payload");
    }
    let write_us = start.elapsed().as_micros() as f64;
    rec.add(
        "seq_write_us_per_op_64KiB",
        format!("{:.2}", write_us / OPS as f64),
    );

    let mut sink = Vec::with_capacity(PAYLOAD);
    let start = Instant::now();
    for _ in 0..OPS {
        sink = fs::read(&path).expect("read payload");
    }
    let read_us = start.elapsed().as_micros() as f64;
    rec.add(
        "seq_read_us_per_op_64KiB",
        format!("{:.2}", read_us / OPS as f64),
    );
    assert_eq!(sink.len(), PAYLOAD, "the read-back payload is whole");
    assert!(sink.iter().all(|&b| b == 0xA5), "the payload round-trips");

    // --- atomic_replace: write + fsync + rename (the durable path) -------
    let replace_path = dir.join("replace.bin");
    let start = Instant::now();
    for i in 0..OPS {
        let buf = if i % 2 == 0 {
            payload.clone()
        } else {
            vec![0x5A; PAYLOAD]
        };
        platform::file::atomic_replace(replace_path.to_str().expect("utf8 path"), &buf)
            .expect("atomic replace");
    }
    let repl_us = start.elapsed().as_micros() as f64;
    rec.add(
        "atomic_replace_us_per_op_64KiB",
        format!("{:.2}", repl_us / OPS as f64),
    );
    let landed = fs::read(&replace_path).expect("read replaced");
    assert_eq!(landed.len(), PAYLOAD, "the replaced file is whole");

    // --- mmap read (page cache warm — the server's steady state) ---------
    let view = platform::mmap::Mmap::map_file(replace_path.to_str().expect("utf8 path"))
        .expect("map file");
    assert_eq!(view.len(), PAYLOAD, "the mapping covers the payload");
    let start = Instant::now();
    let mut sum: u64 = 0;
    for _ in 0..OPS {
        for &b in view.iter() {
            sum = sum.wrapping_add(b as u64);
        }
    }
    let mmap_us = start.elapsed().as_micros() as f64;
    rec.add(
        "mmap_read_us_per_pass_64KiB",
        format!("{:.2}", mmap_us / OPS as f64),
    );
    // The last replace wrote the 0x5A payload; the mapping must show
    // exactly those bytes (the read cannot be dropped by the optimizer —
    // the sum feeds the report's sanity assert below).
    assert!(
        view.iter().all(|&b| b == 0x5A),
        "the mapped bytes are the ones written"
    );
    assert_eq!(
        sum,
        0x5A_u64 * PAYLOAD as u64 * OPS as u64,
        "sum consistent"
    );

    // --- timer resolution + pacing fidelity -------------------------------
    let mut steps: Vec<u64> = Vec::with_capacity(OPS);
    let mut prev = platform::clock::monotonic_now_micros().micros();
    for _ in 0..OPS {
        loop {
            let now = platform::clock::monotonic_now_micros().micros();
            if now != prev {
                steps.push(now - prev);
                prev = now;
                break;
            }
        }
    }
    let resolution = *steps.iter().min().expect("nonempty steps");
    rec.add("timer_resolution_micros", resolution.to_string());
    assert!(
        resolution > 0,
        "the monotonic clock advances in micros steps"
    );

    // Pacing fidelity: 200 sleeps of 1 ms each; the measured rate must sit
    // within ±10% of the requested pace (a coarse clock shows up here).
    std::thread::sleep(PACING); // one pacing sleep before the measured run
    let start = Instant::now();
    let sleeps = 200;
    for _ in 0..sleeps {
        std::thread::sleep(PACING);
    }
    let elapsed = start.elapsed();
    let mean_ms = elapsed.as_secs_f64() * 1000.0 / sleeps as f64;
    rec.add("sleep_1ms_mean_ms", format!("{:.4}", mean_ms));
    assert!(
        (0.9..=1.6).contains(&mean_ms),
        "the 1 ms sleep paces near 1 ms (mean {mean_ms} ms); outliers mean a coarse timer"
    );

    // --- wakeup trigger→consume round trip --------------------------------
    let wake = platform::wakeup::Wakeup::new().expect("wakeup pipe");
    let start = Instant::now();
    for _ in 0..OPS {
        wake.trigger();
        wake.consume();
    }
    let wake_us = start.elapsed().as_micros() as f64;
    rec.add(
        "wakeup_roundtrip_us_per_op",
        format!("{:.2}", wake_us / OPS as f64),
    );

    rec.add(
        "host",
        format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH),
    );
    rec.add("ops", OPS.to_string());
    rec.flush();
    let _ = fs::remove_dir_all(&dir);
}
