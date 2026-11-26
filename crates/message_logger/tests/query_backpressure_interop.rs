//! OPT-12-01-03: rerun backpressure and tool interoperability —
//! the log channel's side of the query/data-record contract.
//!
//! The record walk fills a bounded user segment and honors the shipped
//! backpressure face: `is_full(budget)` gates every record, and the
//! caller stops at the first refusal (the writer itself accepts
//! unconditionally — refusing is the component's contract, the
//! reference's `UserSegmentWriter::isFull` gate). Every landed record
//! reads back with its fields intact and its args re-rendering the
//! original message; the records are then loaded into the shipped
//! `QueryableLog` and queried through `run_query` with real tool
//! filters (time span, direction, severity, source, category,
//! include/exclude regex) — the tool's view answers the filtered set
//! in both walk directions, and the typed error paths
//! (unknown category / unknown component) surface on bad input.
//!
//! Wall-clock of the walks is printed (`BACKPRESSURE` lines) as probe
//! data only; the contracts are asserted.
//!
//! ```sh
//! cargo test -p message_logger --test query_backpressure_interop
//! ```

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use message_logger::entry::LogEntry;
use message_logger::file_logger::UserSegmentWriter;
use message_logger::interpolator::{LogStringInterpolator, stream_to_string};
use message_logger::message_priority;
use message_logger::query::{
    LoggingComponent, QueryError, QueryParams, QueryableLog, SearchDirection, StoredLogLine,
    run_query,
};
use message_logger::stream::MemoryIStream;
use message_logger::{MessageSource, query::InterpolateMode};

// ---------------------------------------------------------------------------
// The scoped temp dir (the channel-cost test's pattern).
// ---------------------------------------------------------------------------

static WALKS: AtomicUsize = AtomicUsize::new(0);

struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "ml-interop-{name}-{}-{}",
            std::process::id(),
            WALKS.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        Self(dir)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

// ---------------------------------------------------------------------------
// The record walk under backpressure.
// ---------------------------------------------------------------------------

/// Records offered to the segment.
const OFFERED: usize = 64;

/// The segment budget in packed-record bytes. Each record costs
/// 34 + 4 (the `%d` args blob), so the budget holds
/// 748 / 38 → 20 records; record 20 is the first refusal
/// (20 × 38 = 760 ≥ 748, 19 × 38 = 722 < 748).
const MAX_SEGMENT_SIZE: u32 = 22 * 34;
const REFUSAL_AT: usize = 20;

/// One landed record's contract fields (what the tool will query).
#[derive(Clone, Debug, PartialEq, Eq)]
struct Record {
    frame: usize,
    secs: i64,
    msecs: u16,
    priority: u8,
    source: MessageSource,
    rendered: String,
}

/// Offer OFFERED records to one bounded segment, honoring `is_full` at
/// every record. Returns the landed records, the suffix's dir, and the
/// walk's wall-clock.
fn record_walk() -> (TempDir, Vec<Record>, u128) {
    let dir = TempDir::new("backpressure");
    let handler = LogStringInterpolator::new("frame %d");
    let mut segment = UserSegmentWriter::new(dir.path(), Some("2026-11-26-13:00:00"));
    assert!(segment.init(), "the segment opens");
    let mut records = Vec::new();

    let started = Instant::now();
    for frame in 0..OFFERED {
        // the backpressure contract: the caller checks, the writer answers
        if segment.is_full(MAX_SEGMENT_SIZE) {
            break;
        }
        let args = (frame as i32).to_le_bytes();
        let secs = 1_700_000_000 + frame as i64;
        let msecs = ((frame * 7) % 1000) as u16;
        let priority = if frame % 2 == 0 {
            message_priority::INFO
        } else {
            message_priority::WARNING
        };
        let source = if frame % 3 == 0 {
            MessageSource::Script
        } else {
            MessageSource::Cpp
        };
        let mut entry = LogEntry::new(secs, msecs, 1, 1, source, priority, 0);
        let mut input = MemoryIStream::new(&args);
        assert!(
            segment.add_entry(&mut entry, &handler, &mut input, 10, None),
            "record {frame}: the writer accepted a record under the budget"
        );
        records.push(Record {
            frame,
            secs,
            msecs,
            priority,
            source,
            rendered: format!("frame {frame}"),
        });
    }
    let elapsed = started.elapsed().as_nanos();

    // the budget landed exactly where the arithmetic says: record N
    // costs 34 packed + 4 args, and the refusal is the first N where
    // the walk's is_full flips
    assert_eq!(records.len(), REFUSAL_AT, "the records before the refusal");
    assert_eq!(
        segment.num_entries(),
        REFUSAL_AT as u32,
        "the writer agrees"
    );
    assert!(segment.is_full(MAX_SEGMENT_SIZE), "the segment is full");
    assert!(
        38 * REFUSAL_AT >= MAX_SEGMENT_SIZE as usize
            && 38 * (REFUSAL_AT - 1) < MAX_SEGMENT_SIZE as usize,
        "the refusal sits on the budget edge"
    );

    // every landed record reads back with its fields intact and its
    // args re-rendering the original message
    let args_file =
        std::fs::read(dir.path().join("args.2026-11-26-13:00:00")).expect("args readable");
    for (index, expected) in records.iter().enumerate() {
        let stored = segment.read_entry(index as u32).expect("record readable");
        assert_eq!(stored.time.secs, expected.secs, "record {index}: secs");
        assert_eq!(stored.time.msecs, expected.msecs, "record {index}: msecs");
        assert_eq!(
            stored.message_priority, expected.priority,
            "record {index}: priority"
        );
        assert_eq!(
            stored.message_source.to_byte(),
            expected.source.to_byte(),
            "record {index}: source"
        );
        assert_eq!(stored.category_id, 1, "record {index}: category");
        let mut blob = MemoryIStream::new(
            &args_file[stored.args_offset as usize..][..stored.args_len as usize],
        );
        let mut rendered = String::new();
        assert!(
            stream_to_string(&handler, &mut blob, 10, &mut rendered),
            "record {index}: the args render"
        );
        assert_eq!(
            rendered, expected.rendered,
            "record {index}: the message reruns"
        );
    }
    (dir, records, elapsed)
}

/// Build the tool's query view from the landed records (the shipped
/// `QueryableLog` tables).
fn tool_log(records: &[Record]) -> QueryableLog {
    let mut log = QueryableLog::default();
    log.add_component(
        1,
        LoggingComponent {
            ip: 0x0a00_0001,
            host: "cellapp01".to_string(),
            pid: 4242,
            app_instance_id: 7,
            app_type_id: 1,
            username: "cuihairu".to_string(),
            name: "cellapp01".to_string(),
        },
    );
    log.add_format_string(0, "frame %d");
    assert_eq!(
        log.add_category("Messages"),
        1,
        "the first category ids at 1"
    );
    for record in records {
        let entry = LogEntry::new(
            record.secs,
            record.msecs,
            1,
            1,
            record.source,
            record.priority,
            0,
        );
        log.push_entry(StoredLogLine {
            entry,
            args: (record.frame as i32).to_le_bytes().to_vec(),
            metadata: String::new(),
        });
    }
    log
}

/// The tool's query: a time window, one direction, warning severity,
/// C++ source, the Messages category, include `frame (1[0-4])` and
/// exclude `frame 12`.
fn tool_query(log: &QueryableLog, backwards: bool) -> Vec<String> {
    let params = QueryParams {
        start: 1_700_000_000.0 + 5.0,
        end: 1_700_000_000.0 + 15.0,
        direction: if backwards {
            SearchDirection::Backwards
        } else {
            SearchDirection::Forwards
        },
        interpolate: InterpolateMode::PreInterpolate,
        category_names: vec!["Messages".to_string()],
        severities: 1 << message_priority::WARNING,
        log_source_type_flags: 1 << MessageSource::Cpp.to_byte(),
        ..QueryParams::default()
    };
    let mut params = params;
    params
        .set_regexes(Some("frame (1[0-4])"), Some("frame 12"))
        .expect("the regexes compile");
    let results = run_query(log, &params).expect("the query runs");
    results.into_iter().map(|r| r.message).collect()
}

#[test]
fn the_backpressured_record_answers_the_tool_query() {
    let (_dir, records, ns) = record_walk();
    let log = tool_log(&records);

    // the filters' expected set, by hand: the window 5..=15, warning
    // severity (odd frames), C++ source (not divisible by three), the
    // include regex (frames 10..14), the exclude regex (frame 12)
    let expected = ["frame 11".to_string(), "frame 13".to_string()];

    let forward = tool_query(&log, false);
    assert_eq!(
        forward, expected,
        "the forward query answers the filtered set"
    );
    let backward = tool_query(&log, true);
    let mut mirrored = expected.clone();
    mirrored.reverse();
    assert_eq!(backward, mirrored, "the backward query is the mirror");

    // the query's contract carries the record fields through
    let params = QueryParams {
        start: 1_700_000_000.0 + 5.0,
        end: 1_700_000_000.0 + 16.0,
        ..QueryParams::default()
    };
    let results = run_query(&log, &params).expect("the unfiltered query runs");
    assert_eq!(results.len(), 11, "the window holds frames 5..=15");
    for (index, result) in results.iter().enumerate() {
        assert_eq!(result.message, format!("frame {}", 5 + index));
        assert_eq!(result.component, "cellapp01");
        assert_eq!(result.pid, 4242);
        assert_eq!(result.username, "cuihairu");
        assert_eq!(
            result.source,
            if records[5 + index].source.to_byte() == 0 {
                "CPP"
            } else {
                "SCRIPT"
            },
            "the source name maps"
        );
        assert_eq!(result.category, "Messages", "the category resolves");
        assert_eq!(result.severity, records[5 + index].priority);
    }

    println!(
        "BACKPRESSURE offered={OFFERED} landed={} budget={MAX_SEGMENT_SIZE} walk_ns={ns}",
        records.len()
    );
    println!("TOOL-INTEROP forward={forward:?} backward={backward:?}");
}

#[test]
fn the_query_contract_surfaces_its_errors() {
    let (_dir, records, _) = record_walk();
    let mut log = tool_log(&records);

    // unknown category
    let params = QueryParams {
        category_names: vec!["NoSuchCategory".to_string()],
        ..QueryParams::default()
    };
    assert!(
        matches!(
            run_query(&log, &params),
            Err(QueryError::UnknownCategory(name)) if name == "NoSuchCategory"
        ),
        "the unknown category surfaces"
    );

    // unknown component
    log.components.clear();
    let params = QueryParams::default();
    assert!(
        matches!(
            run_query(&log, &params),
            Err(QueryError::UnknownComponentId(1))
        ),
        "the unknown component surfaces"
    );

    // unknown string offset
    let mut log = tool_log(&records);
    log.format_strings.clear();
    let params = QueryParams::default();
    assert!(
        matches!(
            run_query(&log, &params),
            Err(QueryError::UnknownStringOffset(0))
        ),
        "the unknown string offset surfaces"
    );

    // a window outside the log's span answers empty, not an error
    let params = QueryParams {
        start: 2_000_000_000.0,
        end: 2_000_000_100.0,
        ..QueryParams::default()
    };
    assert!(
        run_query(&log, &params)
            .expect("the empty span runs")
            .is_empty(),
        "the empty span"
    );

    // a backwards query over an inverted window answers empty
    let params = QueryParams {
        start: 1_700_000_010.0,
        end: 1_700_000_005.0,
        direction: SearchDirection::Backwards,
        ..QueryParams::default()
    };
    assert!(
        run_query(&log, &params)
            .expect("the inverted window runs")
            .is_empty(),
        "the inverted window"
    );
}
