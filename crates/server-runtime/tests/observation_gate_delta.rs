//! OPT-12-01-02: the observation cost when the gates are on and off.
//!
//! The acceptance: **the enabled/disabled deltas reported** (`报告启用
//! 和关闭时的差值`). The 01-01 baseline separated the three channels;
//! this harness measures each channel's own gate:
//!
//! - **The profiler's gate** (`profiler::enable`): recording off, an
//!   `add_entry` pays the mutex lock and the activity check but not the
//!   entry; a `tick` pays the lock and the state check but not the
//!   buffer swap or the inline processing.
//! - **The Watcher's gate** (`EventHistoryStats::set_enabled`): the
//!   event-stats collector starts disabled (the reference's default);
//!   a disabled `track_event` is a bool check, an enabled one formats a
//!   key and accumulates three maps.
//! - **The log's gate** (caller-side priority filter): the port has no
//!   sink-side switch — the reference filters by debug-message priority
//!   at the call site, so the disabled cost is the compare-and-skip.
//!
//! Everything is deterministic: fixed values, one run.

use std::io::Write as _;
use std::time::Instant;

use message_logger::MessageSource;
use message_logger::entry::LogEntry;
use profiler::{CATEGORY_CPP, EventType};
use server_runtime::watcher::EventHistoryStats;

/// The timed operations per leg.
const OPS: u32 = 2000;
/// The profiler frames per tick leg.
const FRAMES: u32 = 50;
/// The entries per profiled frame.
const ENTRIES_PER_FRAME: u32 = 100;
/// The profiler's buffer budget and thread slots (`init`).
const PROFILER_MEMORY: i32 = 1_048_576;
const PROFILER_THREADS: i32 = 4;
/// The log gate: entries at priority 2 against a minimum of 4.
const LOG_PRIORITY: u8 = 2;
const LOG_MIN_PRIORITY: u8 = 4;

/// The record sink (the OPT family's shape).
#[derive(Default)]
struct Record(Vec<(String, String)>);

impl Record {
    fn put(&mut self, metric: impl AsRef<str>, value: impl std::fmt::Display) {
        self.0
            .push((metric.as_ref().to_string(), value.to_string()));
    }

    fn flush(&self) {
        let mut text = String::new();
        for (k, v) in &self.0 {
            text.push_str(&format!("{k}\t{v}\n"));
        }
        print!("{text}");
        if let Ok(path) = std::env::var("OBS_GATE_OUT") {
            std::fs::write(&path, &text).expect("write OBS_GATE_OUT");
            println!("record: {path}");
        }
        let _ = std::io::stdout().flush();
    }
}

/// Averages a timed loop into ns per op.
fn avg_ns(total: u64, ops: u32) -> f64 {
    total as f64 / f64::from(ops)
}

/// Times one entry write.
fn time_op<T>(ops: u32, mut op: impl FnMut(u32) -> T) -> f64 {
    let t0 = Instant::now();
    for i in 0..ops {
        let _ = op(i);
    }
    avg_ns(t0.elapsed().as_nanos() as u64, ops)
}

/// Reports the enabled/disabled deltas of the observation gates.
#[test]
fn the_observation_gates_report_their_deltas() {
    let started = Instant::now();
    let mut record = Record::default();

    // --- The profiler's gate: recording on vs off.
    let p = profiler::global();
    assert!(p.init(PROFILER_MEMORY, PROFILER_THREADS));
    p.enable(true);
    p.tick(); // the first tick activates recording
    let entry_on = time_op(OPS, |i| {
        p.add_entry(
            "cellapp::updateEntities",
            EventType::Counter,
            i as i32,
            CATEGORY_CPP,
        )
    });
    let mut tick_on_total = 0u64;
    for frame in 0..FRAMES {
        for i in 0..ENTRIES_PER_FRAME {
            p.add_entry(
                "cellapp::movement",
                EventType::Counter,
                frame as i32 * 10 + i as i32,
                CATEGORY_CPP,
            );
        }
        let t0 = Instant::now();
        p.tick();
        tick_on_total += t0.elapsed().as_nanos() as u64;
    }
    let tick_on = avg_ns(tick_on_total, FRAMES);
    p.enable(false);
    let entry_off = time_op(OPS, |i| {
        p.add_entry(
            "cellapp::updateEntities",
            EventType::Counter,
            i as i32,
            CATEGORY_CPP,
        )
    });
    let tick_off = time_op(FRAMES, |_| p.tick());
    assert!(!p.watcher_enabled(), "the disabled gate reports itself");
    record.put("profiler_entry_on_ns", format!("{entry_on:.0}"));
    record.put("profiler_entry_off_ns", format!("{entry_off:.0}"));
    record.put(
        "profiler_entry_delta_ratio",
        format!("{:.3}", entry_on / entry_off),
    );
    record.put("profiler_tick_on_ns", format!("{tick_on:.0}"));
    record.put("profiler_tick_off_ns", format!("{tick_off:.0}"));
    record.put(
        "profiler_tick_delta_ratio",
        format!("{:.3}", tick_on / tick_off),
    );
    record.put("profiler_gate_reports_disabled", 1);
    p.fini();

    // --- The Watcher's gate: event stats on vs off.
    let mut stats = EventHistoryStats::new();
    assert!(!stats.is_enabled(), "the collector starts disabled");
    stats.set_enabled(true);
    let track_on = time_op(OPS, |i| {
        stats.track_event("Avatar", "position", 64 + i % 8, -1);
    });
    assert_eq!(stats.counts().len(), 1, "the enabled collector accumulates");
    assert!(*stats.counts().get("Avatar.position").unwrap_or(&0) == OPS);
    stats.set_enabled(false);
    let track_off = time_op(OPS, |i| {
        stats.track_event("Avatar", "position", 64 + i % 8, -1);
    });
    assert_eq!(stats.counts().len(), 1, "the disabled collector holds");
    record.put("eventstats_track_on_ns", format!("{track_on:.0}"));
    record.put("eventstats_track_off_ns", format!("{track_off:.0}"));
    record.put(
        "eventstats_delta_ratio",
        format!("{:.3}", track_on / track_off),
    );
    record.put("eventstats_gate_reports_disabled", 1);

    // --- The log's gate: the caller-side priority filter.
    let write_on = time_op(OPS, |i| {
        let mut entry = LogEntry::new(
            1_800_000_000,
            500,
            7,
            3,
            MessageSource::Cpp,
            LOG_PRIORITY + (i % 2) as u8,
            128,
        );
        entry.set_args(4096, 64);
        entry.to_disk_bytes()
    });
    let write_off = time_op(OPS, |i| {
        let priority = LOG_PRIORITY + (i % 2) as u8;
        if priority >= LOG_MIN_PRIORITY {
            let mut entry =
                LogEntry::new(1_800_000_000, 500, 7, 3, MessageSource::Cpp, priority, 128);
            entry.set_args(4096, 64);
            entry.to_disk_bytes()
        } else {
            [0u8; 48]
        }
    });
    record.put("log_write_on_ns", format!("{write_on:.0}"));
    record.put("log_write_gated_ns", format!("{write_off:.0}"));
    record.put(
        "log_gate_delta_ratio",
        format!("{:.3}", write_on / write_off),
    );
    record.put("log_gate_priority_floor", LOG_MIN_PRIORITY);

    record.put(
        "eval_wall_ms",
        format!("{:.3}", started.elapsed().as_secs_f64() * 1000.0),
    );
    record.flush();
}
