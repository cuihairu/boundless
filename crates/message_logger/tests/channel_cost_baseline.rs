//! OPT-12-01-01 (the log channel): the logging channel's cost,
//! recorded separately from the Watcher's and the Profiler's —
//! one message recorded, per entry, and one record read back, per
//! query, through the shipped faces (`UserSegmentWriter` +
//! `LogStringInterpolator` + `LogEntry`).
//!
//! The per-entry and per-read wall-clock times and allocation counts
//! are the channel's baseline: times are probe data, printed for the
//! review doc, never asserted; the allocation ledger and the record
//! contract (every entry readable, every read renders the original
//! message) are asserted and rerun identically.
//!
//! ```sh
//! cargo test -p message_logger --test channel_cost_baseline
//! ```

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use message_logger::MessageSource;
use message_logger::entry::LogEntry;
use message_logger::file_logger::UserSegmentWriter;
use message_logger::interpolator::{LogStringInterpolator, stream_to_string};
use message_logger::stream::MemoryIStream;

// ---------------------------------------------------------------------------
// The counting allocator (the OPT-01-01-01 pattern).
// ---------------------------------------------------------------------------

thread_local! {
    static TRACKING: Cell<bool> = const { Cell::new(false) };
    static ALLOCS: Cell<usize> = const { Cell::new(0) };
    static BYTES: Cell<usize> = const { Cell::new(0) };
}

struct Counting;

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if TRACKING.with(Cell::get) {
            ALLOCS.with(|c| c.set(c.get() + 1));
            BYTES.with(|b| b.set(b.get() + layout.size()));
        }
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

/// Allocations + bytes counted during the region, on the calling thread.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Count {
    allocs: usize,
    bytes: usize,
}

fn tracked<T>(body: impl FnOnce() -> T) -> (T, Count) {
    TRACKING.with(|t| t.set(true));
    ALLOCS.with(|c| c.set(0));
    BYTES.with(|b| b.set(0));
    let value = body();
    TRACKING.with(|t| t.set(false));
    let count = Count {
        allocs: ALLOCS.with(Cell::get),
        bytes: BYTES.with(Cell::get),
    };
    (value, count)
}

// ---------------------------------------------------------------------------
// The log channel.
// ---------------------------------------------------------------------------

/// Entries in one channel walk.
const ENTRIES: usize = 64;

/// Walks created in this process (temp dirs are per walk, and the
/// binary's tests run concurrently under one pid).
static WALKS: AtomicUsize = AtomicUsize::new(0);

/// A scoped temp directory that cleans itself up.
struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "ml-channel-{name}-{}-{}",
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

/// The args blob for the walk's message `"{name} frame {n}"`: the string
/// packed, the int at its conversion width.
fn args_blob(name: &str, frame: i32) -> Vec<u8> {
    let mut out = vec![name.len() as u8];
    out.extend_from_slice(name.as_bytes());
    out.extend_from_slice(&frame.to_le_bytes());
    out
}

/// One recorded entry's ledger row: the record cost and the render
/// query cost, allocations counted per region.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct EntryLedger {
    record_ns: u128,
    record_allocs: usize,
    query_ns: u128,
    query_allocs: usize,
}

/// One log-channel walk: ENTRIES messages recorded into a fresh
/// segment through the shipped faces, then every record read back and
/// re-rendered through the query face.
fn channel_walk() -> (TempDir, Vec<EntryLedger>) {
    let dir = TempDir::new("cost");
    let handler = LogStringInterpolator::new("%s frame %d");
    let mut segment = UserSegmentWriter::new(dir.path(), Some("2026-11-26-12:00:00"));
    assert!(segment.init(), "the segment opens");

    let mut ledgers = Vec::with_capacity(ENTRIES);
    for frame in 0..ENTRIES {
        let name = "baseapp";
        let blob = args_blob(name, frame as i32);
        let mut entry = LogEntry::new(
            1_700_000_000 + frame as i64,
            (frame * 15 % 1000) as u16,
            1,
            0,
            MessageSource::Cpp,
            2,
            0,
        );
        let started = Instant::now();
        let (recorded, count) = tracked(|| {
            let mut input = MemoryIStream::new(&blob);
            segment.add_entry(&mut entry, &handler, &mut input, 10, None)
        });
        let record_ns = started.elapsed().as_nanos();
        assert!(recorded, "entry {frame} records");

        let started = Instant::now();
        let (rendered, qcount) = tracked(|| {
            let stored = segment.read_entry(frame as u32).expect("entry readable");
            let args =
                std::fs::read(dir.path().join("args.2026-11-26-12:00:00")).expect("args readable");
            let mut blob = MemoryIStream::new(
                &args[stored.args_offset as usize..][..stored.args_len as usize],
            );
            let mut rendered = String::new();
            assert!(stream_to_string(&handler, &mut blob, 10, &mut rendered));
            rendered
        });
        let query_ns = started.elapsed().as_nanos();
        assert_eq!(
            rendered,
            format!("baseapp frame {frame}"),
            "entry {frame} renders the original message"
        );
        ledgers.push(EntryLedger {
            record_ns,
            record_allocs: count.allocs,
            query_ns,
            query_allocs: qcount.allocs,
        });
    }
    assert_eq!(segment.num_entries(), ENTRIES as u32, "every entry landed");
    (dir, ledgers)
}

#[test]
fn the_log_channel_records_and_reads_per_entry() {
    let (_dir, ledgers) = channel_walk();
    assert_eq!(ledgers.len(), ENTRIES);

    // the record contract: the ledger is flat — every entry costs the
    // same allocations, so the channel has no per-entry drift
    let first = ledgers[0];
    for (index, ledger) in ledgers.iter().enumerate() {
        assert_eq!(
            ledger.record_allocs, first.record_allocs,
            "entry {index}: the record cost is flat"
        );
        assert_eq!(
            ledger.query_allocs, first.query_allocs,
            "entry {index}: the query cost is flat"
        );
    }

    for (index, ledger) in ledgers.iter().enumerate() {
        println!(
            "LOG-CHANNEL entry={} record_ns={} record_allocs={} query_ns={} query_allocs={}",
            index, ledger.record_ns, ledger.record_allocs, ledger.query_ns, ledger.query_allocs
        );
    }
    let record_total: usize = ledgers.iter().map(|l| l.record_allocs).sum();
    let query_total: usize = ledgers.iter().map(|l| l.query_allocs).sum();
    println!(
        "LOG-CHANNEL-TOTAL entries={ENTRIES} record_allocs={record_total} query_allocs={query_total}"
    );
}

#[test]
fn the_log_channel_reruns_to_the_same_ledger() {
    let (_, first) = channel_walk();
    let (_, second) = channel_walk();
    assert_eq!(first.len(), second.len());
    for (index, (a, b)) in first.iter().zip(second.iter()).enumerate() {
        assert_eq!(
            a.record_allocs, b.record_allocs,
            "entry {index}: the record ledger reruns"
        );
        assert_eq!(
            a.query_allocs, b.query_allocs,
            "entry {index}: the query ledger reruns"
        );
    }
    println!(
        "LOG-RERUN entries={ENTRIES} record_allocs={} per_entry",
        first[0].record_allocs
    );
}
