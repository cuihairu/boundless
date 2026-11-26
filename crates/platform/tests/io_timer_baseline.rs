//! OPT-15-01-01: 采集各平台 I/O 及定时精度 — the unified I/O and timer
//! workload, producing a per-platform measurement report.
//!
//! One fixed workload drives the shipped `platform` faces the engine's
//! event loop and storage actually sit on, so numbers from different
//! hosts answer the same questions:
//!
//! - [`clock`](platform::clock): read latency and the monotonic clock's
//!   observable granularity (the smallest nonzero step between adjacent
//!   reads) — the reference's `getSystemTime`/`Stamp` basis;
//! - `Instant` pair deltas — the timing floor every probe in this repo
//!   is measured with;
//! - `thread::sleep` granularity at 1/2/5/10 ms — the scheduler floor
//!   behind tick budgets (actual >= requested is the contract, asserted);
//! - file write/read at 4 KiB per op and the shipped
//!   [`file::atomic_replace`](platform::file::atomic_replace) (write +
//!   fsync + rename) — the durability path;
//! - the self-pipe wakeup round trip through the poller (trigger → a
//!   blocked wait reports the pipe) — the event-loop interrupt latency;
//! - poller timeout precision at 1 ms with no events.
//!
//! Structural contracts are asserted (monotonicity, read-back equality,
//! drain-to-empty, timeout-with-no-events); every wall-clock is printed
//! on `IO-TIMER` lines and written to the report named by
//! `PLATFORM_BASELINE_OUT` as probe data, never asserted — it moves with
//! the host. The committed three-platform record
//! (`tests/baseline/platform-baseline.tsv`) carries this host's measured
//! rows plus pending rows naming the BW-15-03/BW-15-04 blockers for the
//! platforms that cannot compile yet; the grammar of that record is
//! checked on every run.
//!
//! ```sh
//! cargo test -p platform --test io_timer_baseline
//! ```

use std::io::Write as _;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use platform::clock::monotonic_now_micros;

const CLOCK_READS: usize = 200_000;
const FILE_OPS: usize = 256;
const PAYLOAD: usize = 4096;
const WAKEUP_ROUNDS: usize = 200;
const TIMEOUT_WAITS: usize = 100;

/// One report row: the probe, its sample count and the distribution in
/// nanoseconds, plus a free-text note for the figures that are not a
/// percentile (requested duration, granularity, deltas).
struct Probe {
    name: &'static str,
    samples: usize,
    min_ns: u128,
    median_ns: u128,
    p99_ns: u128,
    max_ns: u128,
    note: String,
}

impl Probe {
    fn from_samples(name: &'static str, samples: Vec<u128>, note: String) -> Self {
        let mut sorted = samples.clone();
        sorted.sort_unstable();
        let min = sorted.first().copied().unwrap_or(0);
        let median = sorted[sorted.len() / 2];
        let p99 = sorted[sorted.len() * 99 / 100];
        let max = sorted.last().copied().unwrap_or(0);
        Self {
            name,
            samples: samples.len(),
            min_ns: min,
            median_ns: median,
            p99_ns: p99,
            max_ns: max,
            note,
        }
    }

    fn row(&self) -> String {
        format!(
            "{}\t{}\t{}\t{}\t{}\t{}\t{}",
            self.name,
            self.samples,
            self.min_ns,
            self.median_ns,
            self.p99_ns,
            self.max_ns,
            self.note
        )
    }
}

fn platform_name() -> String {
    format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH)
}

fn percentile(samples: &[u128], pct: usize) -> u128 {
    let mut sorted = samples.to_vec();
    sorted.sort_unstable();
    sorted[sorted.len() * pct / 100]
}

// --- the workload legs ------------------------------------------------------

/// Clock read latency + observable granularity of the shipped monotonic
/// stamp. Monotonicity is the structural contract.
fn clock_probes() -> (Probe, Probe) {
    let started = Instant::now();
    let mut previous = monotonic_now_micros().micros();
    let mut deltas: Vec<u128> = Vec::with_capacity(CLOCK_READS);
    for _ in 0..CLOCK_READS {
        let now = monotonic_now_micros().micros();
        assert!(
            now >= previous,
            "the monotonic stamp went backwards: {previous} -> {now}"
        );
        deltas.push((now - previous) as u128);
        previous = now;
    }
    let per_call_ns = started.elapsed().as_nanos() / CLOCK_READS as u128;
    let nonzero: Vec<u128> = deltas.into_iter().filter(|d| *d > 0).collect();
    let granularity = nonzero.iter().copied().min().unwrap_or(0);
    let distinct_steps = {
        nonzero
            .iter()
            .copied()
            .collect::<std::collections::BTreeSet<_>>()
            .len()
    };
    (
        Probe::from_samples(
            "clock_read_latency",
            vec![per_call_ns],
            "per monotonic_now_micros() call".into(),
        ),
        Probe::from_samples(
            "clock_granularity",
            vec![granularity],
            format!(
                "min nonzero adjacent-read delta; {distinct_steps} distinct steps over {CLOCK_READS} reads"
            ),
        ),
    )
}

/// The `Instant` pair-delta floor and read cost — what every wall-clock
/// probe in this workspace measures with.
fn instant_probes() -> (Probe, Probe) {
    let started = Instant::now();
    let mut deltas = Vec::with_capacity(CLOCK_READS);
    let mut last = Instant::now();
    for _ in 0..CLOCK_READS {
        let now = Instant::now();
        deltas.push(now.duration_since(last).as_nanos());
        last = now;
    }
    let per_call_ns = started.elapsed().as_nanos() / CLOCK_READS as u128;
    let resolution = deltas.iter().copied().filter(|d| *d > 0).min().unwrap_or(0);
    (
        Probe::from_samples(
            "instant_read_latency",
            vec![per_call_ns],
            "per Instant::now() call".into(),
        ),
        Probe::from_samples(
            "instant_resolution",
            vec![resolution],
            "min nonzero Instant pair delta (the repo's timing floor)".into(),
        ),
    )
}

/// `thread::sleep` granularity: actual >= requested is the contract the
/// tick budgets rely on; the overshoot is the scheduler's floor.
fn sleep_probes() -> Vec<Probe> {
    let rounds = 50;
    let mut probes = Vec::new();
    for target_ms in [1u64, 2, 5, 10] {
        let mut samples = Vec::with_capacity(rounds);
        for _ in 0..rounds {
            let requested = Duration::from_millis(target_ms);
            let started = Instant::now();
            std::thread::sleep(requested);
            let actual = started.elapsed();
            assert!(
                actual >= requested,
                "sleep({target_ms}ms) returned early: {actual:?}"
            );
            samples.push(actual.as_nanos());
        }
        let median = percentile(&samples, 50);
        let overshoot_median = (median - target_ms as u128 * 1_000_000) / 1_000_000;
        probes.push(Probe::from_samples(
            Box::leak(format!("sleep_{target_ms}ms_actual").into_boxed_str()),
            samples,
            format!("requested={target_ms}ms overshoot_median={overshoot_median}us"),
        ));
    }
    probes
}

/// File write/read throughput at one 4 KiB payload per op: the durability
/// path's per-op latency. Read-back equality is the structural contract.
fn file_probes(dir: &std::path::Path) -> (Probe, Probe) {
    let payload: Vec<u8> = (0..PAYLOAD).map(|i| (i % 251) as u8).collect();
    let path = dir.join("io_timer_baseline.bin");
    let mut writes = Vec::with_capacity(FILE_OPS);
    for round in 0..FILE_OPS {
        let started = Instant::now();
        std::fs::write(&path, &payload).expect("file write");
        writes.push(started.elapsed().as_nanos());
        let _ = round;
    }
    let mut reads = Vec::with_capacity(FILE_OPS);
    for _ in 0..FILE_OPS {
        let started = Instant::now();
        let back = std::fs::read(&path).expect("file read");
        reads.push(started.elapsed().as_nanos());
        assert_eq!(back, payload, "the read-back payload is byte-identical");
    }
    (
        Probe::from_samples(
            "file_write_4k",
            writes,
            format!("{PAYLOAD}B payload, std::fs::write per op"),
        ),
        Probe::from_samples(
            "file_read_4k",
            reads,
            format!("{PAYLOAD}B payload, std::fs::read per op, byte-equal"),
        ),
    )
}

/// The shipped durability face: `atomic_replace` is write + fsync +
/// rename in one call; its per-op cost is what a save path pays.
fn atomic_replace_probe(dir: &std::path::Path) -> Probe {
    let payload: Vec<u8> = (0..PAYLOAD).map(|i| (i % 241) as u8).collect();
    let path = dir.join("io_timer_atomic.bin");
    let mut samples = Vec::with_capacity(FILE_OPS);
    for _ in 0..FILE_OPS {
        let started = Instant::now();
        platform::file::atomic_replace(path.to_str().expect("a plain temp path"), &payload)
            .expect("atomic replace");
        samples.push(started.elapsed().as_nanos());
    }
    let back = std::fs::read(&path).expect("read-back");
    assert_eq!(back, payload, "atomic replace read-back is byte-identical");
    Probe::from_samples(
        "atomic_replace_4k",
        samples,
        "platform::file::atomic_replace per op (write+fsync+rename), byte-equal".to_string(),
    )
}

/// The self-pipe wakeup round trip: trigger a poller-registered pipe and
/// time how long a blocked `epoll_wait` takes to report it. Drain-to-empty
/// and the event identity are the structural contracts.
#[cfg(unix)]
fn wakeup_probe() -> Probe {
    use platform::poller::{CTL_ADD, EPOLLIN, PollEvent, ctl, wait};

    let wakeup = platform::wakeup::Wakeup::new().expect("self-pipe wakeup");
    let epfd = platform::poller::create(1);
    assert!(epfd >= 0, "poller created");
    assert_eq!(
        ctl(epfd, CTL_ADD, wakeup.read_fd(), EPOLLIN),
        0,
        "wakeup read end registered"
    );

    let mut samples = Vec::with_capacity(WAKEUP_ROUNDS);
    for _ in 0..WAKEUP_ROUNDS {
        wakeup.trigger();
        let started = Instant::now();
        let mut events = [PollEvent { events: 0, fd: 0 }; 4];
        let count = wait(epfd, &mut events, 1_000);
        samples.push(started.elapsed().as_nanos());
        assert_eq!(count, 1, "the triggered wakeup is the one reported event");
        assert_eq!(events[0].fd, wakeup.read_fd(), "the pipe read end");
        assert!(events[0].events & EPOLLIN != 0, "read-readiness reported");
        wakeup.consume();
        // Re-armed: a zero wait sees no leftover byte.
        let mut idle = [PollEvent { events: 0, fd: 0 }; 4];
        assert_eq!(
            wait(epfd, &mut idle, 0),
            0,
            "consume drains the pipe completely"
        );
    }
    unsafe { libc::close(epfd) };
    Probe::from_samples(
        "wakeup_roundtrip",
        samples,
        "trigger -> blocked wait reports the self-pipe (per round)".to_string(),
    )
}

/// Poller timeout precision: a registered-but-quiet pipe, a 1 ms budget.
/// Returning zero events is the structural contract; how close the wait
/// lands to the budget is probe data.
#[cfg(target_os = "linux")]
fn poller_timeout_probe() -> Probe {
    use platform::poller::{CTL_ADD, EPOLLIN, PollEvent, ctl, wait};

    let wakeup = platform::wakeup::Wakeup::new().expect("self-pipe wakeup");
    let epfd = platform::poller::create(1);
    assert!(epfd >= 0, "poller created");
    assert_eq!(ctl(epfd, CTL_ADD, wakeup.read_fd(), EPOLLIN), 0);

    let mut samples = Vec::with_capacity(TIMEOUT_WAITS);
    for _ in 0..TIMEOUT_WAITS {
        let started = Instant::now();
        let mut events = [PollEvent { events: 0, fd: 0 }; 4];
        let count = wait(epfd, &mut events, 1);
        samples.push(started.elapsed().as_nanos());
        assert_eq!(count, 0, "no trigger means no events inside the budget");
    }
    unsafe { libc::close(epfd) };
    Probe::from_samples(
        "poller_timeout_1ms",
        samples,
        "epoll_wait(1ms) with no events, actual wait per call".to_string(),
    )
}

// --- the committed three-platform record ------------------------------------

const CATALOG_HEADER: &str = "platform\tprobe\tsamples\tmin_ns\tmedian_ns\tp99_ns\tmax_ns\tnote";
const MEASURED_PROBES: &[&str] = &[
    "clock_read_latency",
    "clock_granularity",
    "instant_read_latency",
    "instant_resolution",
    "sleep_1ms_actual",
    "sleep_2ms_actual",
    "sleep_5ms_actual",
    "sleep_10ms_actual",
    "file_write_4k",
    "file_read_4k",
    "atomic_replace_4k",
    "wakeup_roundtrip",
    "poller_timeout_1ms",
];

/// The committed record at `tests/baseline/platform-baseline.tsv` carries
/// this host's measured rows plus one pending row per platform that cannot
/// compile yet (named BW-15 blocker, no invented figures). Grammar only:
/// numbers are never re-verified on another host.
fn check_catalog() {
    let catalog = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/baseline/platform-baseline.tsv");
    let text = std::fs::read_to_string(&catalog)
        .unwrap_or_else(|e| panic!("the committed catalog is readable at {catalog:?}: {e}"));
    let mut lines = text.lines();
    assert_eq!(
        lines.next(),
        Some(CATALOG_HEADER),
        "the catalog header pins the grammar"
    );
    let mut rows = std::collections::BTreeSet::new();
    for (index, line) in lines.enumerate() {
        let fields: Vec<&str> = line.split('\t').collect();
        assert_eq!(
            fields.len(),
            8,
            "catalog line {} carries 8 fields: {line:?}",
            index + 2
        );
        let platform = fields[0];
        assert!(
            rows.insert((platform, fields[1])),
            "platform {platform} probe {} appears twice",
            fields[1]
        );
        if fields[1] == "-" {
            assert!(
                fields[2..7].iter().all(|f| *f == "-"),
                "a pending row carries no measurements: {platform}"
            );
            assert!(
                fields[7].starts_with("pending: BW-15-0"),
                "a pending row names its BW-15 blocker: {platform}"
            );
        } else {
            assert!(
                MEASURED_PROBES.contains(&fields[1]),
                "unknown probe name {} for {platform}",
                fields[1]
            );
            for (field, name) in fields[2..7]
                .iter()
                .zip(["samples", "min", "median", "p99", "max"])
            {
                assert!(
                    field.parse::<u128>().is_ok(),
                    "{platform} {} {name} is numeric: {field:?}",
                    fields[1]
                );
            }
        }
    }
    // A pending platform is one summary row; the measuring platform covers
    // every probe in the workload exactly once.
    for probe in MEASURED_PROBES {
        assert!(
            rows.contains(&(platform_name().as_str(), *probe)),
            "the measuring platform {} has a {probe} row",
            platform_name()
        );
    }
}

fn write_report(probes: &[Probe]) {
    let Some(path) = std::env::var_os("PLATFORM_BASELINE_OUT") else {
        return;
    };
    let mut text = String::from(CATALOG_HEADER);
    text.push('\n');
    for probe in probes {
        text.push_str(&format!("{}\t{}\n", platform_name(), probe.row()));
    }
    std::fs::write(&path, text).expect("report written");
    eprintln!(
        "IO-TIMER report written to {}",
        PathBuf::from(&path).display()
    );
}

#[test]
fn the_unified_io_and_timer_workload_produces_a_report() {
    let dir = std::env::temp_dir().join(format!(
        "platform-io-timer-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).expect("scratch dir");

    let mut probes = Vec::new();
    let (read, granularity) = clock_probes();
    probes.push(read);
    probes.push(granularity);
    let (instant_cost, resolution) = instant_probes();
    probes.push(instant_cost);
    probes.push(resolution);
    probes.extend(sleep_probes());
    let (write, read_back) = file_probes(&dir);
    probes.push(write);
    probes.push(read_back);
    probes.push(atomic_replace_probe(&dir));
    #[cfg(unix)]
    probes.push(wakeup_probe());
    #[cfg(target_os = "linux")]
    probes.push(poller_timeout_probe());

    for probe in &probes {
        eprintln!(
            "IO-TIMER {} samples={} min={} median={} p99={} max={} ns {}",
            probe.name,
            probe.samples,
            probe.min_ns,
            probe.median_ns,
            probe.p99_ns,
            probe.max_ns,
            probe.note
        );
    }
    write_report(&probes);
    check_catalog();
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::io::stdout().flush();
}
