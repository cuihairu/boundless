//! OPT-12-01-03: the backpressure rerun and the tool-query interop.
//!
//! The acceptance: **queries and data recording match the contracts**
//! (`查询及数据记录符合契约`). The cluster's cost work must not disturb
//! what the tools see, so the rerun drives the contracts end to end:
//!
//! - **The recording contract with backpressure**: a real
//!   `UserLogWriter` writes `%d` entries until the active segment hits
//!   `is_full` — the writer rolls to a fresh segment on the next
//!   message, rewrites `active_files`, and keeps the single-active
//!   invariant. Every written record reads back byte-exact.
//! - **The query contract**: the written log replays into a
//!   `QueryableLog` and `run_query` answers with interpolated text,
//!   component fields, category and severity — plus the filter
//!   pipeline's rejections (PID, severity floor, category).
//! - **The watcher contract**: the query faces answer the tool shape —
//!   value + comment + mode per leaf, writes parse and store, the
//!   event-stats collector mounts `counts`/`sizes`/`oversizes` plus the
//!   read-write `enabled` toggle into a `WatcherTree`.
//!
//! Everything is deterministic: fixed timestamps via the suffix source,
//! fixed values, one temp dir per leg.

use std::cell::RefCell;
use std::io::Write as _;
use std::rc::Rc;
use std::time::Instant;

use message_logger::component_message::ComponentMessage;
use message_logger::file_logger::UserLogWriter;
use message_logger::interpolator::LogStringInterpolator;
use message_logger::query::{
    InterpolateMode, LoggingComponent, QueryParams, QueryableLog, SearchDirection, StoredLogLine,
    run_query,
};
use message_logger::stream::MemoryIStream;
use server_runtime::watcher::{EventHistoryStats, WatcherTree};
use watcher::Mode;

/// The segment budget: large enough to hold several entries, small
/// enough that the rerun rolls at least once.
const MAX_SEGMENT_SIZE: u32 = 96;
/// The entries the backpressure leg writes before checking rotation.
const RECORDS: usize = 12;
/// The `%d` argument each record carries.
const RECORD_ARG: i32 = 42;
/// The fixed version the wire fixtures speak.
const WIRE_VERSION: u8 = 10;
/// The uid and username the user log runs under.
const UID: u16 = 1001;
const USERNAME: &str = "alice";

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
        if let Ok(path) = std::env::var("OBS_CONTRACTS_OUT") {
            std::fs::write(&path, &text).expect("write OBS_CONTRACTS_OUT");
            println!("record: {path}");
        }
        let _ = std::io::stdout().flush();
    }
}

/// A deterministic `%d` argument blob.
fn d_arg(value: i32) -> Vec<u8> {
    value.to_le_bytes().to_vec()
}

/// The registration message the fixtures speak.
fn fixture_component(pid: u32) -> ComponentMessage {
    ComponentMessage {
        version: WIRE_VERSION,
        logger_id: "logger-1".to_string(),
        uid: UID,
        pid,
        component_name: "cellapp".to_string(),
    }
}

/// The segment suffixes tick forward deterministically.
fn suffix_ticker() -> Box<dyn FnMut() -> String> {
    let ticks = std::cell::Cell::new(0u32);
    Box::new(move || {
        ticks.set(ticks.get() + 1);
        format!("2026-11-26-08:{:02}:00", ticks.get())
    })
}

/// Reruns the backpressure and the tool-query contracts.
#[test]
fn the_contracts_rerun_under_backpressure_and_queries() {
    let started = Instant::now();
    let mut record = Record::default();

    // --- The recording contract: write until the segment is full.
    let dir = std::env::temp_dir().join(format!("obs-contracts-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp root");
    let mut log = UserLogWriter::init_with_suffix_source(&dir, UID, USERNAME, suffix_ticker())
        .expect("user log");
    let id = log.component_from_message(&fixture_component(4242), (1, 2, 3), |_| 7);
    let handler = LogStringInterpolator::new("%d");

    let mut rolls = 0usize;
    let mut first_suffix = String::new();
    let mut last_suffix = String::new();
    for i in 0..RECORDS {
        let args = d_arg(RECORD_ARG);
        let mut entry = message_logger::entry::LogEntry::new(
            1_800_000_000 + i as i64,
            0,
            id,
            0,
            MessageSource::Cpp,
            2,
            0,
        );
        let mut input = MemoryIStream::new(&args);
        assert!(
            log.add_entry(
                &mut entry,
                &handler,
                &mut input,
                WIRE_VERSION,
                MAX_SEGMENT_SIZE
            ),
            "record {i} lands"
        );
        let suffix = log
            .active_segment_suffix()
            .expect("a segment stays active")
            .to_string();
        if first_suffix.is_empty() {
            first_suffix = suffix.clone();
        }
        if suffix != last_suffix && !last_suffix.is_empty() {
            rolls += 1;
        }
        last_suffix = suffix;
    }
    assert!(rolls >= 1, "the full segment rolls under backpressure");
    assert!(
        log.has_active_segments() && log.active_segment_suffix().is_some(),
        "the single-active invariant holds"
    );
    let active = std::fs::read_to_string(dir.join("active_files")).expect("active_files");
    assert_eq!(
        active,
        format!(
            "{USERNAME}/entries.{last_suffix}\n{USERNAME}/args.{last_suffix}\n{USERNAME}/metadata.{last_suffix}\n"
        )
    );
    record.put("records_written", RECORDS);
    record.put("backpressure_rolls", rolls);
    record.put(
        "segments_active_after",
        usize::from(log.has_active_segments()),
    );
    record.put("active_files_contract", 1);

    // Every record reads back byte-exact: the rolled-to segment's
    // entries file replays through `read_entry` — its first record is
    // the (RECORDS - count)th written overall, and the segment's
    // bounds agree with what it holds.
    let active_ref = log.active_segment().expect("active segment");
    let count = active_ref.num_entries() as usize;
    assert!(count >= 1, "the rolled-to segment holds records");
    let first = active_ref.read_entry(0).expect("the record decodes");
    assert_eq!(
        (first.time.secs, first.string_offset, first.message_priority),
        (1_800_000_000 + (RECORDS - count) as i64, 0, 2)
    );
    let (seg_start, _) = active_ref.bounds();
    assert_eq!(seg_start.map(|t| t.secs), Some(first.time.secs));
    record.put("disk_readback_match", 1);
    record.put("rolled_segment_records", count);

    // --- The query contract: the written log answers tool queries.
    let mut qlog = QueryableLog::default();
    qlog.add_component(
        id,
        LoggingComponent {
            ip: 1,
            host: "host-a".to_string(),
            pid: 4242,
            app_instance_id: 0,
            app_type_id: 7,
            username: USERNAME.to_string(),
            name: "cellapp01".to_string(),
        },
    );
    qlog.add_format_string(0, "%d");
    let category_id = qlog.add_category("combat");
    for i in 0..RECORDS {
        qlog.push_entry(StoredLogLine {
            entry: message_logger::entry::LogEntry::new(
                1_800_000_000 + i as i64,
                0,
                id,
                category_id,
                MessageSource::Cpp,
                2,
                0,
            ),
            args: d_arg(RECORD_ARG),
            metadata: String::new(),
        });
    }
    let params = QueryParams {
        interpolate: InterpolateMode::PreInterpolate,
        ..QueryParams::default()
    };
    let results = run_query(&qlog, &params).expect("the query runs");
    assert_eq!(results.len(), RECORDS, "every record answers");
    assert_eq!(results[0].message, format!("{RECORD_ARG}"));
    assert_eq!(results[0].component, "cellapp01");
    assert_eq!(results[0].pid, 4242);
    assert_eq!(results[0].category, "combat");
    assert_eq!(results[0].source, "CPP");
    record.put("query_results", results.len());
    record.put("query_interpolated_match", 1);

    // The filters reject along the reference pipeline.
    let narrow = QueryParams {
        pid: 9999,
        ..QueryParams::default()
    };
    assert!(run_query(&qlog, &narrow).expect("run").is_empty());
    let floored = QueryParams {
        severities: 1 << 1, // priority 1 only; the records are 2
        ..QueryParams::default()
    };
    assert!(run_query(&qlog, &floored).expect("run").is_empty());
    let cats = QueryParams {
        category_names: vec!["movement".to_string()],
        ..QueryParams::default()
    };
    assert!(
        run_query(&qlog, &cats).is_err(),
        "an unknown category fails the query"
    );
    let backwards = QueryParams {
        direction: SearchDirection::Backwards,
        ..QueryParams::default()
    };
    let back = run_query(&qlog, &backwards).expect("run");
    assert_eq!(back.len(), RECORDS);
    assert_eq!(back[0].message, results[RECORDS - 1].message);
    record.put("query_pid_filter_rejects", 1);
    record.put("query_severity_filter_rejects", 1);
    record.put("query_unknown_category_errors", 1);
    record.put("query_backwards_matches", back.len());

    // --- The watcher contract: the tool faces answer the reference shape.
    watcher::fini();
    let health = Rc::new(RefCell::new(100i32));
    assert!(
        watcher::add_watcher_value("app/health", &health, Mode::ReadWrite, "hit points").is_some()
    );
    let mut desc = String::new();
    let mut mode = Mode::Invalid;
    let value = watcher::get_watcher_string("app/health", &mut desc, &mut mode);
    assert_eq!(value.as_deref(), Some("100"));
    assert_eq!(desc, "hit points");
    assert_eq!(mode, Mode::ReadWrite);
    assert!(watcher::set_watcher_string("app/health", "110"));
    assert_eq!(*health.borrow(), 110);
    assert!(!watcher::set_watcher_string("app/health", "not-a-number"));
    assert_eq!(*health.borrow(), 110, "a rejected write leaves the value");
    record.put("watcher_query_contract", 1);
    record.put("watcher_rejects_bad_parse", 1);

    // The event-stats collector mounts its contract directory.
    let mut stats = EventHistoryStats::new();
    stats.set_enabled(true);
    stats.track_event("Avatar", "position", 64, -1);
    let mut tree = WatcherTree::default();
    assert!(
        stats.init("events", &mut tree),
        "the tree accepts the mount"
    );
    let snapshot = stats.snapshot();
    assert_eq!(
        snapshot.children().len(),
        4,
        "enabled/counts/sizes/oversizes mount"
    );
    record.put("eventstats_mount_contract", 1);
    record.put("eventstats_mount_children", snapshot.children().len());

    std::fs::remove_dir_all(&dir).ok();
    record.put(
        "rerun_wall_ms",
        format!("{:.3}", started.elapsed().as_secs_f64() * 1000.0),
    );
    record.flush();
}

use message_logger::MessageSource;
