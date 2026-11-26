//! OPT-12-01-01: the observation channels' cost baseline.
//!
//! The acceptance: **logging, Watcher and Profiler separated** (`分离
//! 日志、Watcher 和 Profiler`). The three channels a running server pays
//! for observation live in three crates; the baseline drives each in
//! isolation so the per-op cost belongs to exactly one channel:
//!
//! - **The log channel** (`message_logger`): one `LogEntry` built the
//!   way `UserSegmentWriter` does, serialized to the 48-byte disk image
//!   — the per-message cost the logging channel pays.
//! - **The Watcher channel** (`watcher`): the tool-facing path registry
//!   — a read through a path formats the leaf's value; a write through a
//!   path parses and stores. Both descend the directory tree per call.
//! - **The Profiler channel** (`profiler`): the global profiler's
//!   `add_entry` (the per-event tax under its mutex) and the per-frame
//!   `tick` (buffer swap plus inline entry processing).
//!
//! Everything is deterministic: fixed timestamps, fixed values, one run.

use std::cell::RefCell;
use std::io::Write as _;
use std::rc::Rc;
use std::time::Instant;

use message_logger::MessageSource;
use message_logger::entry::LogEntry;
use profiler::{CATEGORY_CPP, EventType};
use watcher::Mode;

/// The timed operations per channel leg.
const OPS: u32 = 2000;
/// The profiler frames in the tick leg.
const FRAMES: u32 = 50;
/// The entries per profiled frame.
const ENTRIES_PER_FRAME: u32 = 100;
/// The profiler's buffer budget and thread slots (`init`).
const PROFILER_MEMORY: i32 = 1_048_576;
const PROFILER_THREADS: i32 = 4;
/// The fixed timestamp the log leg replays.
const LOG_SECS: i64 = 1_800_000_000;
const LOG_MSECS: u16 = 500;

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
        if let Ok(path) = std::env::var("OBS_CHANNELS_OUT") {
            std::fs::write(&path, &text).expect("write OBS_CHANNELS_OUT");
            println!("record: {path}");
        }
        let _ = std::io::stdout().flush();
    }
}

/// Averages a timed loop into ns per op.
fn avg_ns(total: u64, ops: u32) -> f64 {
    total as f64 / f64::from(ops)
}

/// Builds one log entry the way the user-segment writer does.
fn build_entry() -> LogEntry {
    let mut entry = LogEntry::new(
        LOG_SECS,
        LOG_MSECS,
        7,                  // user_component_id
        3,                  // category_id
        MessageSource::Cpp, // message_source
        2,                  // message_priority
        128,                // string_offset
    );
    entry.set_args(4096, 64);
    entry
}

/// Baselines the three observation channels in isolation.
#[test]
fn the_observation_channels_separate() {
    let started = Instant::now();
    let mut record = Record::default();

    // --- The log channel: one entry = build + args + disk serialize.
    let entry = build_entry();
    let disk = entry.to_disk_bytes();
    let back = LogEntry::from_disk_bytes(&disk).expect("the disk image decodes");
    assert_eq!(back.category_id, 3);
    assert_eq!(back.args_offset, 4096);
    assert_eq!(back.args_len, 64);
    assert_eq!(back.message_priority, 2);
    record.put("log_roundtrip_match", 1);

    let t0 = Instant::now();
    let mut checksum = 0u64;
    for i in 0..OPS {
        let mut e = build_entry();
        e.set_args(i, 64);
        let bytes = e.to_disk_bytes();
        checksum += u64::from(bytes[24]);
        checksum += u64::from(bytes[36]);
    }
    let log_write_ns = avg_ns(t0.elapsed().as_nanos() as u64, OPS);
    assert!(checksum > 0, "the log entries serialize");
    record.put("log_write_ns", format!("{log_write_ns:.0}"));

    let t0 = Instant::now();
    let mut disk_sum = 0usize;
    for _ in 0..OPS {
        disk_sum += entry.to_disk_bytes().len();
    }
    let log_disk_ns = avg_ns(t0.elapsed().as_nanos() as u64, OPS);
    assert_eq!(disk_sum, OPS as usize * LogEntry::DISK_SIZE);
    record.put("log_disk_ns", format!("{log_disk_ns:.0}"));
    record.put("log_disk_bytes", LogEntry::DISK_SIZE);
    record.put("log_ops", OPS);

    // --- The Watcher channel: the tool-facing path registry.
    watcher::fini();
    let health = Rc::new(RefCell::new(100i32));
    let speed = Rc::new(RefCell::new(40i32));
    let load = Rc::new(RefCell::new(0i32));
    let t0 = Instant::now();
    assert!(
        watcher::add_watcher_value("app/health", &health, Mode::ReadWrite, "hit points").is_some()
    );
    assert!(
        watcher::add_watcher_value("app/speed", &speed, Mode::ReadWrite, "movement speed")
            .is_some()
    );
    assert!(
        watcher::add_watcher_value("app/load", &load, Mode::ReadWrite, "load factor").is_some()
    );
    let watcher_setup_ns = t0.elapsed().as_nanos() as u64;
    record.put("watcher_setup_ns", watcher_setup_ns);
    record.put("watcher_leaves", 3);

    let mut desc = String::new();
    let mut mode = Mode::Invalid;
    let t0 = Instant::now();
    let mut last = String::new();
    for _ in 0..OPS {
        if let Some(value) = watcher::get_watcher_string("app/health", &mut desc, &mut mode) {
            last = value;
        }
    }
    let watcher_read_ns = avg_ns(t0.elapsed().as_nanos() as u64, OPS);
    assert_eq!(last, "100", "the leaf answers its live value");
    record.put("watcher_read_ns", format!("{watcher_read_ns:.0}"));
    record.put("watcher_read_value_match", 1);

    let t0 = Instant::now();
    let mut accepted = 0u32;
    for i in 0..OPS {
        let value = if i % 2 == 0 { "41" } else { "40" };
        if watcher::set_watcher_string("app/speed", value) {
            accepted += 1;
        }
    }
    let watcher_write_ns = avg_ns(t0.elapsed().as_nanos() as u64, OPS);
    assert_eq!(accepted, OPS, "every parseable write lands");
    assert_eq!(*speed.borrow(), 40);
    assert!(!watcher::set_watcher_string("app/missing", "1"));
    record.put("watcher_write_ns", format!("{watcher_write_ns:.0}"));
    record.put("watcher_write_rejects_unknown", 1);
    record.put("watcher_ops", OPS);

    // --- The Profiler channel: the global profiler's entry and tick.
    let p = profiler::global();
    let t0 = Instant::now();
    assert!(p.init(PROFILER_MEMORY, PROFILER_THREADS));
    let profiler_init_ns = t0.elapsed().as_nanos() as u64;
    p.enable(true);
    p.tick(); // the first tick activates recording
    record.put("profiler_init_ns", profiler_init_ns);

    let t0 = Instant::now();
    for i in 0..OPS {
        p.add_entry(
            "cellapp::updateEntities",
            EventType::Counter,
            i as i32,
            CATEGORY_CPP,
        );
    }
    let profiler_entry_ns = avg_ns(t0.elapsed().as_nanos() as u64, OPS);
    record.put("profiler_entry_ns", format!("{profiler_entry_ns:.0}"));
    record.put("profiler_ops", OPS);

    let frame_t0 = Instant::now();
    let mut tick_total = 0u64;
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
        tick_total += t0.elapsed().as_nanos() as u64;
    }
    // The tick's own cost, separate from the entries the frame feeds it.
    let profiler_tick_ns = avg_ns(tick_total, FRAMES);
    record.put("profiler_tick_ns", format!("{profiler_tick_ns:.0}"));
    record.put("profiler_frame_entries", ENTRIES_PER_FRAME);
    let profiler_frame_ns = avg_ns(frame_t0.elapsed().as_nanos() as u64, FRAMES);
    record.put("profiler_frame_ns", format!("{profiler_frame_ns:.0}"));
    record.put("profiler_frames", FRAMES);
    p.fini();

    record.put(
        "baseline_wall_ms",
        format!("{:.3}", started.elapsed().as_secs_f64() * 1000.0),
    );
    record.flush();
}
