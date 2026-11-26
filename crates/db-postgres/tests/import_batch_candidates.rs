//! OPT-26-01-02: import batch-size evaluation.
//!
//! The OPT-26-01-01 baseline replays the fixed event stream through
//! the migration import and the storage contract; this row evaluates
//! the import's commit batching — the same 512-row corpus lands on
//! fresh tables under per-row, per-16, per-64, per-256 and all-in-one
//! transaction shapes, and each shape reports its throughput (rows/s)
//! and its counting-allocator peak memory. The verdict feeds the
//! replay face OPT-26-01-03 optimizes; the shapes mirror the
//! OPT-09-01-03 archive-period result (per-row commit is the shape to
//! avoid, a batch near the archive period is the shape to keep).
//!
//! Timings are recorded, not asserted; the data (row counts and
//! read-back equality per shape) is.
//!
//! ```sh
//! BOUNDLESS_PG_URL='host=... dbname=postgres user=...' \
//! IMPORT_BATCH_OUT=<abs path> \
//! cargo test -p db-postgres --test import_batch_candidates -- --nocapture
//! ```

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::io::Write as _;
use std::time::Instant;

use db::{EntityKey, EntityStorage};
use db_postgres::import_mysql::{MySqlEntityRow, import_entities};
use db_postgres::{EntitySchema, PostgresStorage};
use entitydef::value::PropertyValue;

const ROWS: usize = 512;
const TYPE_NAME: &str = "BatchBaseline";
/// The shapes: transactions of 1, 16, 64, 256 rows, and the whole
/// corpus in one transaction.
const BATCHES: [usize; 5] = [1, 16, 64, 256, 512];

fn entity_name(i: u64) -> String {
    format!("avatar-{i:04}")
}

fn base_values(i: u64) -> Vec<PropertyValue> {
    vec![
        PropertyValue::String(entity_name(i)),
        PropertyValue::Int((i % 200) as i64),
        PropertyValue::Float(f64::from_bits(0x3FF0_0000_0000_0000 + i)),
        PropertyValue::Blob(vec![0x00, 0xff, i as u8, 0x0a]),
    ]
}

// ---------------------------------------------------------------------------
// The counting allocator (the event_query_baseline.rs pattern)
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Count {
    allocs: usize,
    bytes: usize,
    base: usize,
    peak: usize,
}

impl Count {
    fn peak_delta(&self) -> usize {
        self.peak - self.base
    }
}

thread_local! {
    static TRACKING: Cell<bool> = const { Cell::new(false) };
    static ALLOCS: Cell<usize> = const { Cell::new(0) };
    static BYTES: Cell<usize> = const { Cell::new(0) };
    static LEDGER: RefCell<HashMap<usize, usize>> = RefCell::new(HashMap::new());
    static LEDGERING: Cell<bool> = const { Cell::new(false) };
    static LIVE: Cell<usize> = const { Cell::new(0) };
    static PEAK: Cell<usize> = const { Cell::new(0) };
}

fn ledger_insert(ptr: *mut u8, size: usize) {
    let recorded = LEDGER.try_with(|m| {
        LEDGERING.with(|g| g.set(true));
        m.borrow_mut().insert(ptr as usize, size);
        LEDGERING.with(|g| g.set(false));
    });
    if recorded.is_err() {
        return;
    }
    LIVE.with(|c| {
        let live = c.get() + size;
        c.set(live);
        if TRACKING.with(Cell::get) {
            PEAK.with(|p| p.set(p.get().max(live)));
        }
    });
}

fn ledger_remove(ptr: *mut u8) {
    let freed = LEDGER.try_with(|m| {
        LEDGERING.with(|g| g.set(true));
        let freed = m.borrow_mut().remove(&(ptr as usize));
        LEDGERING.with(|g| g.set(false));
        freed
    });
    if let Ok(Some(size)) = freed {
        LIVE.with(|c| c.set(c.get() - size));
    }
}

struct Counting;

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if LEDGERING.with(Cell::get) {
            return unsafe { System.alloc(layout) };
        }
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() {
            ledger_insert(ptr, layout.size());
            if TRACKING.with(Cell::get) {
                ALLOCS.with(|c| c.set(c.get() + 1));
                BYTES.with(|c| c.set(c.get() + layout.size()));
            }
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        if !LEDGERING.with(Cell::get) {
            ledger_remove(ptr);
        }
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

fn measure<T>(body: impl FnOnce() -> T) -> (Count, T) {
    let base = LIVE.with(Cell::get);
    TRACKING.with(|t| {
        ALLOCS.with(|c| c.set(0));
        BYTES.with(|c| c.set(0));
        PEAK.with(|p| p.set(base));
        t.set(true);
    });
    let value = body();
    TRACKING.with(|t| t.set(false));
    let count = TRACKING.with(|_| Count {
        allocs: ALLOCS.with(Cell::get),
        bytes: BYTES.with(Cell::get),
        base,
        peak: PEAK.with(Cell::get),
    });
    (count, value)
}

// ---------------------------------------------------------------------------
// Server plumbing
// ---------------------------------------------------------------------------

fn pg_admin_conninfo() -> String {
    if let Ok(url) = std::env::var("BOUNDLESS_PG_URL") {
        return url;
    }
    if let (Ok(host), Ok(port)) = (
        std::env::var("CI_STORE_PG_HOST"),
        std::env::var("CI_STORE_PG_PORT"),
    ) {
        let password = std::env::var("CI_STORE_PG_PASSWORD").unwrap_or_default();
        return format!(
            "host={host} port={port} dbname=postgres user=boundless password={password}"
        );
    }
    let user = std::env::var("USER")
        .or_else(|_| std::env::var("LOGNAME"))
        .unwrap_or_else(|_| "postgres".to_string());
    format!("host=/var/run/postgresql dbname=postgres user={user}")
}

fn pg_fresh_db(admin: &mut PostgresStorage, base: &str, tag: &str) -> Option<String> {
    let database = format!("boundless_bench_{base}_{tag}_{}", std::process::id());
    let quoted = database.replace('"', "\"\"");
    if admin
        .client_mut()
        .execute(format!("CREATE DATABASE \"{quoted}\"").as_str(), &[])
        .is_err()
    {
        return None;
    }
    let conninfo = pg_admin_conninfo();
    let stripped: Vec<&str> = conninfo
        .split_whitespace()
        .filter(|part| !part.starts_with("dbname="))
        .collect();
    Some(format!("{} dbname={database}", stripped.join(" ")))
}

fn db_name(conninfo: &str) -> &str {
    conninfo.rsplit("dbname=").next().unwrap_or("")
}

fn pg_drop_db(admin: &mut PostgresStorage, conninfo: &str) {
    let name = db_name(conninfo).replace('"', "\"\"");
    let _ = admin.client_mut().execute(
        format!("DROP DATABASE \"{name}\" WITH (FORCE)").as_str(),
        &[],
    );
}

fn dataset_schema() -> EntitySchema {
    let names = ["name", "level", "ratio", "avatar"];
    EntitySchema::for_record(TYPE_NAME, &names, &base_values(0)).expect("the dataset schema")
}

fn setup_tables(storage: &mut PostgresStorage) {
    let plan = db_postgres::schema_init::plan();
    db_postgres::schema_init::apply(&mut storage.client_mut(), &plan).unwrap();
    let schema = dataset_schema();
    storage.create_entity_table(&schema).unwrap();
    storage.register_entity_type(1, &schema);
    storage.add_entity_type(1, TYPE_NAME).unwrap();
}

/// The import corpus: `MySqlEntityRow`s with explicit ids.
fn corpus() -> Vec<MySqlEntityRow> {
    (0..ROWS as u64)
        .map(|i| MySqlEntityRow {
            db_id: (i + 1) as i64,
            values: base_values(i),
        })
        .collect()
}

/// Imports `rows` under one transaction per `batch` rows. The
/// migration face runs inside the transaction; its get-then-put per
/// row is the shape the shapes share.
fn import_batched(
    storage: &mut PostgresStorage,
    rows: &[MySqlEntityRow],
    batch: usize,
) -> (f64, usize, usize, usize) {
    let started = Instant::now();
    let mut written = 0usize;
    let (count, ()) = measure(|| {
        for slice in rows.chunks(batch) {
            storage.begin_transaction().unwrap();
            let report = import_entities(storage, 1, &dataset_schema(), slice).unwrap();
            written += report.written;
            storage.commit_transaction().unwrap();
        }
    });
    (
        started.elapsed().as_secs_f64(),
        written,
        count.allocs,
        count.peak_delta() / 1024,
    )
}

/// A record sink like event_query_baseline's, under its own env var.
#[derive(Default)]
struct Record(Vec<(String, String)>);

impl Record {
    fn put(&mut self, metric: &str, value: impl std::fmt::Display) {
        self.0.push((metric.to_string(), value.to_string()));
    }

    fn flush(&self) {
        let mut text = String::new();
        for (k, v) in &self.0 {
            text.push_str(&format!("{k}\t{v}\n"));
        }
        print!("{text}");
        if let Ok(path) = std::env::var("IMPORT_BATCH_OUT") {
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .and_then(|mut f| f.write_all(text.as_bytes()))
                .expect("write IMPORT_BATCH_OUT");
            println!("record: {path}");
        }
        let _ = std::io::stdout().flush();
    }
}

#[test]
fn the_import_batch_shapes_report_throughput_and_peak() {
    let mut record = Record::default();
    let Some(mut admin) = PostgresStorage::connect(&pg_admin_conninfo()).ok() else {
        eprintln!("skipping: no reachable postgres server");
        return;
    };
    let Some(conninfo) = pg_fresh_db(&mut admin, "import", "batch") else {
        eprintln!("skipping: cannot create the test database");
        return;
    };
    let mut storage = PostgresStorage::connect(&conninfo).unwrap();
    setup_tables(&mut storage);
    let rows = corpus();

    println!(
        "{:<10} {:>10} {:>12} {:>10} {:>10}",
        "batch", "wall_ms", "rows_per_s", "allocs", "peak_kib"
    );
    for batch in BATCHES {
        let (wall, written, allocs, peak_kib) = import_batched(&mut storage, &rows, batch);
        assert_eq!(written, ROWS, "batch {batch}: every row imported");
        record.put(&format!("batch_{batch}_rows"), ROWS);
        record.put(&format!("batch_{batch}_commits"), ROWS.div_ceil(batch));
        record.put(
            &format!("batch_{batch}_wall_ms"),
            format!("{:.2}", wall * 1_000.0),
        );
        record.put(
            &format!("batch_{batch}_rows_per_s"),
            format!("{:.0}", ROWS as f64 / wall),
        );
        record.put(&format!("batch_{batch}_allocs"), allocs);
        record.put(&format!("batch_{batch}_peak_kib"), peak_kib);
        println!(
            "{batch:<10} {:>10.2} {:>12.0} {allocs:>10} {peak_kib:>10}",
            wall * 1_000.0,
            ROWS as f64 / wall,
        );

        // The shape's data: the corpus reads back equal.
        for i in 0..ROWS as u64 {
            let read = EntityStorage::get_entity(
                &storage,
                &EntityKey {
                    type_id: 1,
                    db_id: (i + 1) as i64,
                },
            )
            .unwrap()
            .unwrap_or_else(|| panic!("batch {batch}: entity {i} landed"));
            assert_eq!(read.values, base_values(i), "batch {batch}: row {i} equal");
        }

        // The next shape starts from an empty table: wipe and rearm.
        storage
            .client_mut()
            .execute(&format!("TRUNCATE \"tbl_{TYPE_NAME}\""), &[])
            .unwrap();
    }

    record.flush();
    drop(storage);
    pg_drop_db(&mut admin, &conninfo);
}

#[test]
fn the_batch_shapes_partition_the_corpus() {
    let rows = corpus();
    assert_eq!(rows.len(), ROWS);
    for batch in BATCHES {
        let commits = ROWS.div_ceil(batch);
        assert!(commits >= 1);
        let covered: usize = (0..commits).map(|_| batch).sum::<usize>().min(ROWS);
        assert_eq!(covered, ROWS, "batch {batch} covers the corpus");
    }
    // The shapes are ordered: each batch size is a divisor-consistent
    // chunking, so the commit count shrinks as the batch grows.
    let commits: Vec<usize> = BATCHES.iter().map(|b| ROWS.div_ceil(*b)).collect();
    assert!(
        commits.windows(2).all(|w| w[0] > w[1]),
        "commit counts shrink: {commits:?}"
    );
}

#[test]
fn the_corpus_is_deterministic() {
    assert_eq!(base_values(7), base_values(7));
    assert_eq!(corpus().len(), ROWS);
    assert_eq!(corpus()[0].db_id, 1);
    assert_eq!(corpus()[ROWS - 1].db_id, ROWS as i64);
}
