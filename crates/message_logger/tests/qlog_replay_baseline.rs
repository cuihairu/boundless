//! OPT-26-01-01: replay a fixed event set into the in-memory queryable
//! log and record the import / query / memory baseline.
//!
//! The acceptance: **retain the import, query and memory data**
//! (`保留导入、查询及内存数据`). One deterministic event set — 20 000
//! entries across 4 components, 8 format strings and 4 categories, every
//! field derived from the entry index (no RNG) — is replayed through
//! `QueryableLog::push_entry`, then queried under eight parameter faces.
//!
//! Retention is asserted on data faces only: the entry count, field-exact
//! spot checks (first / middle / last) including the argument blob bytes
//! and metadata string, and per-query survivor counts computed from the
//! generator's index formulas. Wall-clock readings (import rate, per-query
//! latency) are recorded to the report, never asserted. Memory is the
//! counting allocator's live-bytes ledger: the import window's peak and
//! each query window's allocation count are recorded rows.
//!
//! The record sink writes `metric<TAB>value` rows to stdout and, when
//! `MESSAGELOG_QLOG_OUT` is set, to that path:
//!
//! ```sh
//! MESSAGELOG_QLOG_OUT=<abs path> cargo test -p message_logger \
//!     --test qlog_replay_baseline -- --nocapture
//! ```
//!
//! This baseline is the pre-optimization reference for OPT-26-01-02
//! (import batch size), 01-03 (query aggregation) and 01-04 (sample
//! report recalc).

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::collections::HashMap;
use std::fs;
use std::io::Write as _;
use std::time::Instant;

use message_logger::MessageSource;
use message_logger::entry::{LogEntry, LogTime};
use message_logger::query::{
    InterpolateMode, LoggingComponent, QueryParams, QueryableLog, SearchDirection, StoredLogLine,
};

// ---------------------------------------------------------------------------
// The counting allocator: a per-thread live-set ledger so each measured
// window reports allocation count, total bytes and a live-bytes high-water
// mark relative to the window's base (the mercury buffer_alloc shape).
// ---------------------------------------------------------------------------

thread_local! {
    /// The live-set ledger: every allocation this thread made, paired
    /// with its size, so a dealloc subtracts exactly what its alloc added.
    static LEDGER: std::cell::RefCell<HashMap<usize, usize>> =
        std::cell::RefCell::new(HashMap::new());
    static LEDGERING: Cell<bool> = const { Cell::new(false) };
    static TRACKING: Cell<bool> = const { Cell::new(false) };
    static LIVE: Cell<usize> = const { Cell::new(0) };
    static PEAK: Cell<usize> = const { Cell::new(0) };
    static ALLOCS: Cell<usize> = const { Cell::new(0) };
    static BYTES: Cell<usize> = const { Cell::new(0) };
}

struct Counting;

impl Counting {
    /// Bookkeeping that must survive teardown: once the thread locals are
    /// being destroyed, `try_with` fails and the allocator turns
    /// transparent — no ledger can run at that point either way. While the
    /// ledger's own map is being mutated its storage growth re-enters this
    /// allocator, so the mutation runs under the `LEDGERING` gate (the
    /// mercury buffer_alloc shape).
    fn record_alloc(&self, ptr: *mut u8, size: usize) {
        if LEDGERING.try_with(Cell::get).unwrap_or(true) {
            return;
        }
        let live = LIVE
            .try_with(|c| {
                c.set(c.get() + size);
                c.get()
            })
            .unwrap_or(0);
        if live == 0 {
            return; // TLS gone: nothing to tally into.
        }
        let _ = LEDGER.try_with(|m| {
            let _ = LEDGERING.try_with(|g| g.set(true));
            m.borrow_mut().insert(ptr as usize, size);
            let _ = LEDGERING.try_with(|g| g.set(false));
        });
        let _ = PEAK.try_with(|p| p.set(p.get().max(live)));
        if TRACKING.try_with(Cell::get).unwrap_or(false) {
            let _ = ALLOCS.try_with(|c| c.set(c.get() + 1));
            let _ = BYTES.try_with(|c| c.set(c.get() + size));
        }
    }

    fn record_free(&self, ptr: *mut u8) {
        if LEDGERING.try_with(Cell::get).unwrap_or(true) {
            return;
        }
        let freed = LEDGER.try_with(|m| {
            let _ = LEDGERING.try_with(|g| g.set(true));
            let freed = m.borrow_mut().remove(&(ptr as usize));
            let _ = LEDGERING.try_with(|g| g.set(false));
            freed
        });
        if let Ok(Some(size)) = freed {
            let _ = LIVE.try_with(|c| c.set(c.get().saturating_sub(size)));
        }
    }
}

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() {
            self.record_alloc(ptr, layout.size());
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        self.record_free(ptr);
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

/// One measured window: `base` live bytes when it opened, `peak` the
/// high-water mark inside it, plus the allocation count and total bytes.
/// Tracking on inside the allocator cannot recurse: the ledger inserts
/// run before `TRACKING` is consulted and never touch it.
struct Window {
    base: usize,
    peak: usize,
    allocs: usize,
    bytes: usize,
}

fn open_window() -> Window {
    // Base live bytes include everything already built (the log's own
    // tables); the window's footprint is peak above that base, so no
    // drain assertion here — persistent structures keep the ledger busy.
    TRACKING.with(|t| t.set(true));
    ALLOCS.with(|c| c.set(0));
    BYTES.with(|c| c.set(0));
    PEAK.with(|p| p.set(0));
    Window {
        base: LIVE.with(|c| c.get()),
        peak: 0,
        allocs: 0,
        bytes: 0,
    }
}

fn close_window(mut w: Window) -> Window {
    TRACKING.with(|t| t.set(false));
    w.peak = PEAK.with(|p| p.get());
    w.allocs = ALLOCS.with(|c| c.get());
    w.bytes = BYTES.with(|c| c.get());
    w
}

impl Window {
    /// Live bytes above the base — the window's own footprint.
    fn peak_delta(&self) -> usize {
        self.peak.saturating_sub(self.base)
    }
}

// ---------------------------------------------------------------------------
// The record sink.
// ---------------------------------------------------------------------------

struct Record {
    rows: Vec<(String, String)>,
}

impl Record {
    fn new() -> Self {
        Self { rows: Vec::new() }
    }
    fn add(&mut self, key: impl Into<String>, value: String) {
        let key = key.into();
        println!("{}\t{}", key, value);
        self.rows.push((key, value));
    }
    fn flush(self) {
        if let Ok(path) = std::env::var("MESSAGELOG_QLOG_OUT") {
            // cargo runs the binary with the package root as CWD, so a
            // relative out path only lands where the caller meant it when
            // it is absolute — create whatever parents the path carries.
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

// ---------------------------------------------------------------------------
// The fixed event set: every field derives from the entry index, so the
// expected survivor count of any filter face is a closed-form formula.
// ---------------------------------------------------------------------------

const N: usize = 20_000;
const BASE_SECS: i64 = 1_000_000;
/// Format strings, one per `string_offset` `100..=107`; literal texts so
/// the pre/post-interpolate regex faces and the raw-format faces agree.
const FORMATS: [&str; 8] = [
    "heartbeat steady",
    "movement delta small",
    "ai decision branch",
    "network flush batch",
    "database commit row",
    "script callback fire",
    "cell migration wave",
    "profile sample flush",
];
const CATEGORIES: [&str; 4] = ["general", "combat", "movement", "system"];

fn component(id: u32) -> LoggingComponent {
    LoggingComponent {
        ip: (0x0A00_0000_u32) | id,
        host: format!("host{id}.cluster"),
        pid: 1000 + id,
        app_instance_id: id,
        app_type_id: id,
        username: format!("user{id}"),
        name: format!("cellapp{id:02}"),
    }
}

fn entry(i: usize) -> StoredLogLine {
    let every_eighth_has_no_metadata = i % 8 == 5;
    let meta = if every_eighth_has_no_metadata {
        String::new()
    } else {
        format!("meta-{i}")
    };
    StoredLogLine {
        entry: LogEntry {
            time: LogTime {
                secs: BASE_SECS + (i as i64) / 1000,
                msecs: (i % 1000) as u16,
            },
            user_component_id: (i % 4) as u32 + 1,
            category_id: (i % 4) as u16 + 1,
            message_source: if i.is_multiple_of(2) {
                MessageSource::Cpp
            } else {
                MessageSource::Script
            },
            message_priority: (i % 4) as u8,
            string_offset: (i % 8) as u32 + 100,
            args_offset: (i * 16) as u32,
            args_len: 16,
            metadata_offset: (i * 24) as u32,
            metadata_len: meta.len() as u16,
        },
        args: (0..16)
            .map(|j| ((i + j * 7) & 0xFF) as u8)
            .collect::<Vec<u8>>(),
        metadata: meta,
    }
}

fn build_log() -> QueryableLog {
    let mut log = QueryableLog::default();
    for id in 1u32..=4 {
        log.add_component(id, component(id));
    }
    for (k, fmt) in FORMATS.iter().enumerate() {
        log.add_format_string(100 + k as u32, fmt);
    }
    for (k, name) in CATEGORIES.iter().enumerate() {
        let got = log.add_category(name);
        assert_eq!(got, k as u16 + 1, "category ids are registration order");
    }
    log
}

/// The sample report face: per-(severity, category) counts over a time
/// window — the aggregate OPT-26-01-04 must reproduce exactly.
fn sample_report(log: &QueryableLog, from: usize, to: usize) -> Vec<(u8, u16, usize)> {
    let mut counts = std::collections::BTreeMap::new();
    for line in &log.entries[from..to] {
        *counts
            .entry((line.entry.message_priority, line.entry.category_id))
            .or_insert(0usize) += 1;
    }
    counts.into_iter().map(|((s, c), n)| (s, c, n)).collect()
}

#[test]
fn the_fixed_replay_records_the_import_query_and_memory_baseline() {
    let mut rec = Record::new();

    // --- import: replay the whole fixed set through push_entry ------------
    let mut log = build_log();
    let win = open_window();
    let start = Instant::now();
    for i in 0..N {
        log.push_entry(entry(i));
    }
    let import_s = start.elapsed().as_secs_f64();
    let win = close_window(win);
    rec.add("entries", N.to_string());
    rec.add("import_us", format!("{:.0}", import_s * 1e6));
    rec.add(
        "import_entries_per_s",
        format!("{:.0}", N as f64 / import_s),
    );
    rec.add("import_allocs", win.allocs.to_string());
    rec.add("import_bytes_total", win.bytes.to_string());
    rec.add("import_peak_live_bytes", win.peak_delta().to_string());

    // Retention: the count and field-exact spot checks (first/mid/last),
    // including the argument blob round-trip and the metadata string.
    assert_eq!(log.entries.len(), N, "every replayed entry is stored");
    for idx in [0usize, N / 2, N - 1] {
        let want = entry(idx);
        let got = &log.entries[idx];
        assert_eq!(got.entry.time.secs, want.entry.time.secs, "secs @ {idx}");
        assert_eq!(got.entry.time.msecs, want.entry.time.msecs, "msecs @ {idx}");
        assert_eq!(
            got.entry.user_component_id, want.entry.user_component_id,
            "component @ {idx}"
        );
        assert_eq!(
            got.entry.category_id, want.entry.category_id,
            "category @ {idx}"
        );
        assert_eq!(
            got.entry.message_priority, want.entry.message_priority,
            "priority @ {idx}"
        );
        assert_eq!(
            got.entry.string_offset, want.entry.string_offset,
            "string offset @ {idx}"
        );
        assert_eq!(got.args, want.args, "argument blob round-trips @ {idx}");
        assert_eq!(got.metadata, want.metadata, "metadata round-trips @ {idx}");
    }
    // The empty-metadata face: every eighth entry (i % 8 == 5) carries none.
    assert!(log.entries[5].metadata.is_empty(), "i=5 has no metadata");
    rec.add("retention_spot_checks", "3/3 field-exact".to_string());

    // The pre-optimization sample report (severities x categories over two
    // windows) — recorded for OPT-26-01-04 to reproduce.
    for (tag, from, to) in [("first_quarter", 0, N / 4), ("last_quarter", 3 * N / 4, N)] {
        let report = sample_report(&log, from, to);
        let total: usize = report.iter().map(|(_, _, c)| c).sum();
        assert_eq!(total, to - from, "report {tag} covers the window");
        rec.add(format!("report_{tag}_cells"), report.len().to_string());
        rec.add(format!("report_{tag}_total"), total.to_string());
    }

    // --- query faces: latency recorded, survivor counts asserted ----------
    let query = |tag: &str, rec: &mut Record, p: QueryParams| {
        let win = open_window();
        let start = Instant::now();
        let got = message_logger::query::run_query(&log, &p).expect("query runs");
        let us = start.elapsed().as_secs_f64() * 1e6;
        let win = close_window(win);
        rec.add(format!("query_{tag}_us"), format!("{:.0}", us));
        rec.add(format!("query_{tag}_survivors"), got.len().to_string());
        rec.add(format!("query_{tag}_allocs"), win.allocs.to_string());
        rec.add(
            format!("query_{tag}_peak_live_bytes"),
            win.peak_delta().to_string(),
        );
        got
    };

    // Full unfiltered walk: every entry survives, log order.
    let got = query("all", &mut rec, QueryParams::default());
    assert_eq!(got.len(), N, "unfiltered query returns every entry");
    assert_eq!(
        got[0].time,
        entry(0).entry.time.as_double(),
        "first is oldest"
    );
    assert_eq!(
        got[N - 1].time,
        entry(N - 1).entry.time.as_double(),
        "last is newest"
    );
    assert_eq!(got[0].component, "cellapp01", "component name resolves");
    assert_eq!(got[0].category, "general", "category name resolves");
    assert_eq!(got[0].severity, 0, "severity byte rides the entry");
    assert_eq!(got[0].message, FORMATS[0], "literal format renders as-is");

    // Single severity bit: priority i%4==0 → N/4.
    let got = query(
        "severity0",
        &mut rec,
        QueryParams {
            severities: 1,
            ..QueryParams::default()
        },
    );
    assert_eq!(got.len(), N / 4, "one severity bit keeps a quarter");

    // PID filter: component 2's entries (i%4==1) → N/4.
    let got = query(
        "pid2",
        &mut rec,
        QueryParams {
            pid: 1002,
            ..QueryParams::default()
        },
    );
    assert_eq!(got.len(), N / 4, "the pid face keeps component 2 only");
    assert!(got.iter().all(|r| r.pid == 1002), "survivors all pid 1002");

    // App instance filter: component 3 (i%4==2) → N/4.
    let got = query(
        "appid3",
        &mut rec,
        QueryParams {
            appid: 3,
            ..QueryParams::default()
        },
    );
    assert_eq!(got.len(), N / 4, "the appid face keeps component 3 only");

    // Process-type mask (1<<2): app_type_id 2 (i%4==1) → N/4.
    let got = query(
        "procs2",
        &mut rec,
        QueryParams {
            procs: 1 << 2,
            ..QueryParams::default()
        },
    );
    assert_eq!(got.len(), N / 4, "the process-type mask keeps type 2 only");

    // Script source only (i%2==1) → N/2.
    let got = query(
        "source_script",
        &mut rec,
        QueryParams {
            log_source_type_flags: 1 << message_logger::MessageSource::Script.to_byte(),
            ..QueryParams::default()
        },
    );
    assert_eq!(got.len(), N / 2, "the script face keeps half");
    assert!(
        got.iter().all(|r| r.source == "SCRIPT"),
        "all survivors SCRIPT"
    );

    // Category "combat" (category_id 2, i%4==1) → N/4.
    let got = query(
        "category_combat",
        &mut rec,
        QueryParams {
            category_names: vec!["combat".to_string()],
            ..QueryParams::default()
        },
    );
    assert_eq!(got.len(), N / 4, "the category face keeps a quarter");
    assert!(got.iter().all(|r| r.category == "combat"), "all combat");

    // Include regex "flush": formats 3 and 7 → 2·N/8 = N/4. Deterministic
    // across two runs (the query is a pure function of the log).
    let mk_flush = || {
        let mut p = QueryParams::default();
        p.set_regexes(Some("flush"), None).expect("regex compiles");
        p
    };
    let a = query("include_flush", &mut rec, mk_flush());
    let b = query("include_flush_rerun", &mut rec, mk_flush());
    assert_eq!(a.len(), N / 4, "the include face keeps formats 3 and 7");
    assert_eq!(a.len(), b.len(), "repeated query is deterministic");
    assert!(
        a.iter().zip(b.iter()).all(|(x, y)| x.time == y.time),
        "repeated query returns the same rows in the same order"
    );

    // Exclude regex "delta": only format 1 carries it → N - N/8.
    let mut p = QueryParams::default();
    p.set_regexes(None, Some("delta")).expect("regex compiles");
    let got = query("exclude_delta", &mut rec, p);
    assert_eq!(got.len(), N - N / 8, "the exclude face drops format 1");

    // Backwards walk: same survivors, newest first.
    let got = query(
        "backwards",
        &mut rec,
        QueryParams {
            direction: SearchDirection::Backwards,
            ..QueryParams::default()
        },
    );
    assert_eq!(got.len(), N, "backwards keeps every entry");
    assert_eq!(
        got[0].time,
        entry(N - 1).entry.time.as_double(),
        "backwards starts at the newest"
    );

    // Post-interpolate with the include face: regexes match the raw format
    // string here (literal formats), so the count agrees with the raw face.
    let mut p = QueryParams {
        interpolate: InterpolateMode::PostInterpolate,
        ..QueryParams::default()
    };
    p.set_regexes(Some("flush"), None).expect("regex compiles");
    let got = query("post_interpolate_flush", &mut rec, p);
    assert_eq!(got.len(), N / 4, "post-interpolate agrees on literals");

    rec.add(
        "host",
        format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH),
    );
    rec.flush();
}
