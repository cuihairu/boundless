//! OPT-23-01-01: the collection-off vs collection-on long-run baseline.
//!
//! The acceptance: **preserve CPU, RSS, tick tail and dropped-event
//! counts** (`留存 CPU、RSS、Tick 及丢事件数`). Two legs run the same
//! scripted 200-frame workload (20 zero-width sections per frame) over
//! fresh local profilers — one with collection explicitly disabled, one
//! with it enabled through the pending-activate tick:
//!
//! - **CPU** — the total wall time of the whole leg, decomposed into
//!   per-entry and per-tick averages (an observation, not a gate).
//! - **RSS** — `VmRSS` before and after each leg, in KiB, straight
//!   from `/proc/self/status` (an observation; `-1` when unavailable).
//! - **Tick tail** — per-tick wall samples: average, maximum and the
//!   p99 (observations).
//! - **Dropped events** — the structural half: with collection off,
//!   `add_entry` drops every submission (8000 of 8000), the frame
//!   counter never advances and the final buffer stays empty; with it
//!   on, every frame records its 40 entries, the high watermark pins
//!   at 40 of the 64-slot budget and nothing clamps.

use profiler::{CATEGORY_CPP, EventType, Profiler};
use std::io::Write as _;
use std::time::Instant;

const TICKS: usize = 200;
const SECTIONS_PER_FRAME: usize = 20;
const ENTRIES_PER_FRAME: usize = SECTIONS_PER_FRAME * 2;
const SUBMITTED: usize = TICKS * ENTRIES_PER_FRAME;
/// `max_entry_count = memory / (128*2 + 160/2)`: 64 slots, 32 display.
const MEMORY: i32 = 336 * 64;
const THREADS: i32 = 4;
const SECTION_NAMES: [&str; 4] = [
    "cellapp::tick",
    "cellapp::move",
    "cellapp::ai",
    "cellapp::vision",
];

#[derive(Default)]
struct Record(Vec<(String, String)>);

impl Record {
    fn put(&mut self, metric: impl AsRef<str>, value: impl std::fmt::Display) {
        self.0
            .push((metric.as_ref().to_string(), value.to_string()));
    }
}

/// `VmRSS` in KiB from `/proc/self/status`, or -1 when unavailable.
fn rss_kib() -> i64 {
    let Ok(status) = std::fs::read_to_string("/proc/self/status") else {
        return -1;
    };
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("VmRSS:") {
            return rest
                .trim()
                .trim_end_matches("kB")
                .trim()
                .parse()
                .unwrap_or(-1);
        }
    }
    -1
}

fn percentile(samples: &[u64], pct: usize) -> u64 {
    let mut sorted = samples.to_vec();
    sorted.sort_unstable();
    let index = (sorted.len() - 1) * pct / 100;
    sorted[index]
}

struct LegResult {
    total_ns: u64,
    tick_ns: Vec<u64>,
    dropped: usize,
    final_count: i32,
    frame_number: i32,
    watermark: i32,
    rss_before: i64,
    rss_after: i64,
}

/// One long run over a fresh local profiler.
fn run_leg(active: bool) -> LegResult {
    let p = Profiler::new();
    assert!(p.init(MEMORY, THREADS));
    p.enable(active);
    p.tick(); // the first tick activates recording (on) / early-returns (off)
    assert_eq!(p.watcher_enabled(), active, "the state self-reports");

    let rss_before = rss_kib();
    let leg_t0 = Instant::now();
    let mut tick_ns = Vec::with_capacity(TICKS);
    for _ in 0..TICKS {
        let t0 = Instant::now();
        for s in 0..SECTIONS_PER_FRAME {
            let name = SECTION_NAMES[s % SECTION_NAMES.len()];
            p.add_entry(name, EventType::Start, s as i32, CATEGORY_CPP);
            p.add_entry(name, EventType::End, 0, CATEGORY_CPP);
        }
        p.tick();
        tick_ns.push(t0.elapsed().as_nanos() as u64);
    }
    let total_ns = leg_t0.elapsed().as_nanos() as u64;
    let rss_after = rss_kib();

    let dropped = if active {
        assert_eq!(
            p.final_entry_count(),
            ENTRIES_PER_FRAME as i32,
            "the last frame recorded every section"
        );
        assert_eq!(
            p.profile_frame_number(),
            TICKS as i32,
            "every frame processed"
        );
        let watermark = p.high_watermark_of_entry_count();
        assert_eq!(watermark, ENTRIES_PER_FRAME as i32, "the watermark pins");
        assert!(
            watermark <= p.max_entry_count(),
            "nothing clamps inside the budget"
        );
        assert_eq!(p.max_entry_count(), 64);
        0
    } else {
        assert_eq!(
            p.final_entry_count(),
            0,
            "a disabled profiler records nothing"
        );
        assert_eq!(
            p.profile_frame_number(),
            0,
            "no frame processed while disabled"
        );
        SUBMITTED
    };

    LegResult {
        total_ns,
        tick_ns,
        dropped,
        final_count: p.final_entry_count(),
        frame_number: p.profile_frame_number(),
        watermark: p.high_watermark_of_entry_count(),
        rss_before,
        rss_after,
    }
}

fn put_leg(record: &mut Record, prefix: &str, leg: &LegResult) {
    let ticks = leg.tick_ns.len() as f64;
    let tick_total: u64 = leg.tick_ns.iter().sum();
    let tick_avg = tick_total as f64 / ticks;
    record.put(format!("{prefix}_total_ns"), leg.total_ns);
    record.put(
        format!("{prefix}_entry_avg_ns"),
        leg.total_ns as f64 / SUBMITTED as f64,
    );
    record.put(format!("{prefix}_tick_avg_ns"), format!("{tick_avg:.0}"));
    record.put(
        format!("{prefix}_tick_max_ns"),
        leg.tick_ns.iter().max().copied().unwrap_or(0),
    );
    record.put(
        format!("{prefix}_tick_p99_ns"),
        percentile(&leg.tick_ns, 99),
    );
    record.put(format!("{prefix}_rss_before_kib"), leg.rss_before);
    record.put(format!("{prefix}_rss_after_kib"), leg.rss_after);
    record.put(
        format!("{prefix}_rss_delta_kib"),
        leg.rss_after - leg.rss_before,
    );
    record.put(format!("{prefix}_dropped"), leg.dropped);
    record.put(format!("{prefix}_final_count"), leg.final_count);
    record.put(format!("{prefix}_frame_number"), leg.frame_number);
    record.put(format!("{prefix}_watermark"), leg.watermark);
}

/// The off-vs-on long-run collection baseline.
#[test]
fn the_collection_off_and_on_long_runs_preserve_their_costs() {
    let mut record = Record::default();
    record.put("ticks", TICKS);
    record.put("entries_per_frame", ENTRIES_PER_FRAME);
    record.put("submitted", SUBMITTED);
    record.put("max_entry_budget", 64);

    // The off leg first: the disabled state is the baseline cost of
    // running the workload with the gates closed.
    let off = run_leg(false);
    put_leg(&mut record, "off", &off);
    // The on leg: same workload, collection live.
    let on = run_leg(true);
    put_leg(&mut record, "on", &on);

    // The overhead shape: per-tick and per-entry ratios between the
    // legs. Observations, not gates — the absolute numbers move with
    // machine load, the ratio is the reportable face.
    let off_tick: f64 = off.tick_ns.iter().sum::<u64>() as f64 / TICKS as f64;
    let on_tick: f64 = on.tick_ns.iter().sum::<u64>() as f64 / TICKS as f64;
    record.put("tick_overhead_ratio", format!("{:.2}", on_tick / off_tick));
    record.put(
        "entry_overhead_ratio",
        format!("{:.2}", on.total_ns as f64 / off.total_ns as f64),
    );

    let mut text = String::new();
    for (k, v) in &record.0 {
        text.push_str(&format!("{k}\t{v}\n"));
    }
    print!("{text}");
    if let Ok(path) = std::env::var("LONG_STABILITY_OUT") {
        std::fs::write(&path, &text).expect("write LONG_STABILITY_OUT");
        println!("record: {path}");
    }
    let _ = std::io::stdout().flush();
}
