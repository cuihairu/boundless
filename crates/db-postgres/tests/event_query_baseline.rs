//! OPT-26-01-01: fixed event replay and query baseline.
//!
//! One deterministic event stream replays through the real storage and
//! lookup faces over a 512-entity corpus, and every leg's figures are
//! recorded — the `import` rate (the literal migration face), the
//! replay event rate, per-op `get`/name-lookup latency, the full-scan
//! `lookup_entities` shapes, and a counting allocator's per-window
//! allocation/byte/peak figures. The pinned sample report — counts per
//! level window and per query shape over the final state — is what
//! OPT-26-01-04 recomputes, so the report counts (not the timings)
//! are asserted exact: only the data (read-back equality, the online
//! split) and the determinism of the corpus are.
//!
//! The faces: `db_postgres::import_mysql::import_entities` (the
//! migration import), the `EntityStorage` contract (replay writes,
//! checkouts), and `db::lookup` (`lookup_entities` scans
//! `all_entities()` and joins `logon_of` per row for the online
//! filter — the shape a later row aggregates).
//!
//! ```sh
//! BOUNDLESS_PG_URL='host=... dbname=postgres user=...' \
//! EVENT_QUERY_OUT=<abs path> \
//! cargo test -p db-postgres --test event_query_baseline -- --nocapture
//! ```

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::io::Write as _;
use std::time::Instant;

use db::lookup::{
    EntityDefIndex, LookUpEntitiesCriteria, LookUpEntitiesFilter, LookupDefs,
    get_database_id_from_name, lookup_entities, lookup_entity_by_name,
};
use db::{BaseMailbox, CheckOut, EntityKey, EntityStorage};
use db_postgres::import_mysql::{MySqlEntityRow, import_entities};
use db_postgres::{EntitySchema, PostgresStorage};
use entitydef::value::PropertyValue;
use mercury::endpoint::Address;

const ROWS: usize = 512;
/// Deterministic replay generations over the corpus (update events).
const REPLAY_GENS: u64 = 3;
/// The level step each generation applies — the replay's evolution.
const LEVEL_STEP: u64 = 7;
const TYPE_NAME: &str = "ReplayBaseline";
/// Every eighth entity checks out a logon record for the online
/// filter legs.
const LOGON_STRIDE: u64 = 8;

// ---------------------------------------------------------------------------
// The deterministic corpus and event stream
// ---------------------------------------------------------------------------

fn entity_name(i: u64) -> String {
    format!("avatar-{i:04}")
}

/// The corpus's base values: the identifier string, a level, a ratio
/// and a blob — scalar properties, so the lookup faces cover the
/// string match, the numeric match, the sort and the identifier legs.
fn base_values(i: u64) -> Vec<PropertyValue> {
    vec![
        PropertyValue::String(entity_name(i)),
        PropertyValue::Int((i % 200) as i64),
        PropertyValue::Float(f64::from_bits(0x3FF0_0000_0000_0000 + i)),
        PropertyValue::Blob(vec![0x00, 0xff, i as u8, 0x0a]),
    ]
}

/// Generation `gen`'s values: the level walks `LEVEL_STEP` per
/// generation (wrapping at 200), the ratio and blob walk with it.
/// `gen == 0` is the base.
fn evolved_values(i: u64, generation: u64) -> Vec<PropertyValue> {
    let mut values = base_values(i);
    values[1] = PropertyValue::Int(((i % 200) + generation * LEVEL_STEP).rem_euclid(200) as i64);
    values[2] = PropertyValue::Float(f64::from_bits(0x3FF0_0000_0000_0000 + i + generation));
    values[3] = PropertyValue::Blob(vec![0x00, 0xff, i as u8, 0x0a + generation as u8]);
    values
}

/// The pinned level after the full replay.
fn final_level(i: u64) -> i64 {
    ((i % 200) + REPLAY_GENS * LEVEL_STEP).rem_euclid(200) as i64
}

/// The sample report's level windows: `[start, end)`.
const LEVEL_WINDOWS: [(i64, i64); 4] = [(0, 50), (50, 100), (100, 150), (150, 200)];

/// The report's window math: the count of entities whose final level
/// lands in `[start, end)`.
fn window_count(start: i64, end: i64) -> usize {
    (0..ROWS as u64)
        .filter(|i| {
            let level = final_level(*i);
            level >= start && level < end
        })
        .count()
}

// ---------------------------------------------------------------------------
// The counting allocator (the mercury buffer_alloc.rs pattern)
// ---------------------------------------------------------------------------

/// Allocations seen during a tracked region, on the calling thread
/// only. `peak` is the high-water mark of live bytes inside the
/// window, `base` the live bytes when the window opened — the deltas
/// are the figures the legs report.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Count {
    allocs: usize,
    bytes: usize,
    base: usize,
    peak: usize,
}

impl Count {
    /// Live-bytes high water over the window's opening level.
    fn peak_delta(&self) -> usize {
        self.peak - self.base
    }
}

thread_local! {
    /// const-init: flag and counters never allocate, so turning
    /// tracking on inside the allocator cannot recurse.
    static TRACKING: Cell<bool> = const { Cell::new(false) };
    static ALLOCS: Cell<usize> = const { Cell::new(0) };
    static BYTES: Cell<usize> = const { Cell::new(0) };
    /// The live-set ledger: every allocation this thread made, paired
    /// by pointer, so a free can only ever subtract what an alloc
    /// added. Frees of foreign blocks are ignored, which keeps the
    /// ledger exact where a bare running sum would underflow.
    static LEDGER: RefCell<HashMap<usize, usize>> = RefCell::new(HashMap::new());
    /// Reentrancy guard: the ledger map's own allocations must not hit
    /// the hooks.
    static LEDGERING: Cell<bool> = const { Cell::new(false) };
    /// The ledger's byte sum and its high water (the latter only
    /// advances inside a tracking window).
    static LIVE: Cell<usize> = const { Cell::new(0) };
    static PEAK: Cell<usize> = const { Cell::new(0) };
}

/// Ledger bookkeeping for one allocation; a teardown-time insertion is
/// discarded.
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

/// Ledger bookkeeping for one free: nothing to subtract unless this
/// thread allocated the block.
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

    // `realloc` keeps its default implementation: alloc + copy +
    // dealloc, so the ledger sees both blocks and the sum moves by the
    // delta exactly.
}

#[global_allocator]
static GLOBAL: Counting = Counting;

/// Count the calling thread's allocations inside `body`. Live bytes
/// carry across windows; everything reported is a delta against the
/// window's opening level.
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
// The record sink
// ---------------------------------------------------------------------------

/// Collected `metric<TAB>value` rows. Printed to stdout on flush and
/// appended to `$EVENT_QUERY_OUT` when set.
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
        if let Ok(path) = std::env::var("EVENT_QUERY_OUT") {
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .and_then(|mut f| f.write_all(text.as_bytes()))
                .expect("write EVENT_QUERY_OUT");
            println!("record: {path}");
        }
        let _ = std::io::stdout().flush();
    }
}

// ---------------------------------------------------------------------------
// Server plumbing (the store_baseline.rs pattern)
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

/// Creates a private database and returns its connection string.
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
    // libpq takes the last occurrence of a repeated keyword, so a
    // clean conninfo with the fresh name wins (storage_contract.rs
    // notes the same rule).
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

/// The lookup defs the DBApp role builds from the entity description:
/// property order and the identifier index.
fn lookup_defs() -> LookupDefs {
    let mut defs = LookupDefs::new();
    defs.insert(
        1,
        EntityDefIndex {
            properties: vec![
                "name".to_string(),
                "level".to_string(),
                "ratio".to_string(),
                "avatar".to_string(),
            ],
            identifier: Some(0),
        },
    );
    defs
}

fn mailbox(i: u64) -> BaseMailbox {
    BaseMailbox {
        addr: Address {
            ip: 0x0A00_0001 + i as u32,
            port: 30000,
            salt: 7,
        },
        entity_id: 1000 + i as i32,
    }
}

/// Percentiles `(p50, p99, max)` over a microsecond sample.
fn percentiles(mut samples: Vec<u64>) -> (u64, u64, u64) {
    if samples.is_empty() {
        return (0, 0, 0);
    }
    samples.sort_unstable();
    let at = |pct: f64| samples[((pct * samples.len() as f64) as usize).min(samples.len() - 1)];
    (at(0.50), at(0.99), *samples.last().unwrap())
}

/// Sets up the fixed tables, the entity type and its persisted
/// registration (matching sync_db's `TypesCollector::addType`).
fn setup_import(storage: &mut PostgresStorage) {
    let plan = db_postgres::schema_init::plan();
    db_postgres::schema_init::apply(&mut storage.client_mut(), &plan).unwrap();
    let schema = dataset_schema();
    storage.create_entity_table(&schema).unwrap();
    storage.register_entity_type(1, &schema);
    storage.add_entity_type(1, TYPE_NAME).unwrap();
}

// ---------------------------------------------------------------------------
// The baseline run
// ---------------------------------------------------------------------------

#[test]
fn the_fixed_event_replay_and_query_baseline_runs() {
    let mut record = Record::default();
    let Some(mut admin) = PostgresStorage::connect(&pg_admin_conninfo()).ok() else {
        eprintln!("skipping: no reachable postgres server");
        return;
    };
    let Some(conninfo) = pg_fresh_db(&mut admin, "event", "replay") else {
        eprintln!("skipping: cannot create the test database");
        return;
    };
    let mut storage = PostgresStorage::connect(&conninfo).unwrap();
    setup_import(&mut storage);

    // --- import: the migration corpus through the import face ---------
    let rows: Vec<MySqlEntityRow> = (0..ROWS as u64)
        .map(|i| MySqlEntityRow {
            db_id: (i + 1) as i64,
            values: base_values(i),
        })
        .collect();
    let (import_mem, import) = measure(|| {
        let started = Instant::now();
        let report = import_entities(&mut storage, 1, &dataset_schema(), &rows).unwrap();
        (report, started.elapsed())
    });
    assert_eq!(import.0.written, ROWS, "every row imported");
    assert_eq!(import.0.skipped, 0, "a fresh database skips nothing");
    record.put("import_rows", ROWS);
    record.put(
        "import_wall_ms",
        format!("{:.2}", import.1.as_secs_f64() * 1_000.0),
    );
    record.put(
        "import_rows_per_s",
        format!("{:.0}", ROWS as f64 / import.1.as_secs_f64()),
    );
    record.put("import_mem_allocs", import_mem.allocs);
    record.put("import_mem_bytes", import_mem.bytes);
    record.put("import_mem_peak_kib", import_mem.peak_delta() / 1024);

    // --- replay: deterministic update events through the contract -----
    let events = REPLAY_GENS * ROWS as u64;
    let (replay_mem, replay_wall) = measure(|| {
        let started = Instant::now();
        for generation in 1..=REPLAY_GENS {
            for i in 0..ROWS as u64 {
                EntityStorage::put_entity(
                    &mut storage,
                    &EntityKey {
                        type_id: 1,
                        db_id: (i + 1) as i64,
                    },
                    &evolved_values(i, generation),
                    false,
                )
                .unwrap_or_else(|err| panic!("replay event {generation} entity {i}: {err:?}"));
            }
        }
        started.elapsed()
    });
    record.put("replay_events", events);
    record.put(
        "replay_wall_ms",
        format!("{:.2}", replay_wall.as_secs_f64() * 1_000.0),
    );
    record.put(
        "replay_events_per_s",
        format!("{:.0}", events as f64 / replay_wall.as_secs_f64()),
    );
    record.put("replay_mem_allocs", replay_mem.allocs);
    record.put("replay_mem_bytes", replay_mem.bytes);
    record.put("replay_mem_peak_kib", replay_mem.peak_delta() / 1024);

    // The replay's result is the corpus: every entity at its final
    // generation, equal on read-back.
    for i in 0..ROWS as u64 {
        let read = EntityStorage::get_entity(
            &storage,
            &EntityKey {
                type_id: 1,
                db_id: (i + 1) as i64,
            },
        )
        .unwrap()
        .unwrap_or_else(|| panic!("entity {i} survived the replay"));
        assert_eq!(
            read.values,
            evolved_values(i, REPLAY_GENS),
            "entity {i} ended at its final generation"
        );
    }

    // --- logons: every eighth entity is online for the filter legs ----
    let mut online = 0usize;
    for i in (0..ROWS as u64).step_by(LOGON_STRIDE as usize) {
        let granted = EntityStorage::check_out(
            &mut storage,
            &EntityKey {
                type_id: 1,
                db_id: (i + 1) as i64,
            },
            mailbox(i),
        )
        .unwrap_or_else(|err| panic!("entity {i} checks out: {err:?}"));
        assert_eq!(granted, CheckOut::Granted, "entity {i} is free");
        online += 1;
    }

    // --- query legs ----------------------------------------------------
    let defs = lookup_defs();

    // get by id: a latency sample over every second entity.
    let mut get_samples = Vec::new();
    let get_started = Instant::now();
    for i in (0..ROWS as u64).step_by(2) {
        let op = Instant::now();
        let read = EntityStorage::get_entity(
            &storage,
            &EntityKey {
                type_id: 1,
                db_id: (i + 1) as i64,
            },
        )
        .unwrap()
        .expect("the sampled entity");
        get_samples.push(op.elapsed().as_micros() as u64);
        assert_eq!(read.values, evolved_values(i, REPLAY_GENS));
    }
    let get_wall = get_started.elapsed();
    let (get_p50, get_p99, get_max) = percentiles(get_samples);
    record.put("query_get_ops", ROWS / 2);
    record.put("query_get_p50_us", get_p50);
    record.put("query_get_p99_us", get_p99);
    record.put("query_get_max_us", get_max);
    record.put(
        "query_get_wall_ms",
        format!("{:.2}", get_wall.as_secs_f64() * 1_000.0),
    );

    // name lookups: resolve-and-read over every fourth name.
    let mut name_samples = Vec::new();
    let name_started = Instant::now();
    for i in (0..ROWS as u64).step_by(4) {
        let op = Instant::now();
        let found = lookup_entity_by_name(&storage, &defs, 1, &entity_name(i)).unwrap();
        name_samples.push(op.elapsed().as_micros() as u64);
        assert_eq!(found.db_id, (i + 1) as i64, "the name resolves to its id");
    }
    let name_wall = name_started.elapsed();
    let (name_p50, name_p99, name_max) = percentiles(name_samples);
    record.put("query_name_ops", ROWS / 4);
    record.put("query_name_p50_us", name_p50);
    record.put("query_name_p99_us", name_p99);
    record.put("query_name_max_us", name_max);
    record.put(
        "query_name_wall_ms",
        format!("{:.2}", name_wall.as_secs_f64() * 1_000.0),
    );

    // id-only resolution (the name map leg without the read): the
    // face an aggregated name index would replace.
    let id_started = Instant::now();
    let mut id_hits = 0usize;
    for i in (0..ROWS as u64).step_by(4) {
        let id = get_database_id_from_name(&storage, &defs, 1, &entity_name(i)).unwrap();
        assert_eq!(id, (i + 1) as i64);
        id_hits += 1;
    }
    let id_wall = id_started.elapsed();
    record.put("query_id_from_name_ops", id_hits);
    record.put(
        "query_id_from_name_wall_ms",
        format!("{:.2}", id_wall.as_secs_f64() * 1_000.0),
    );

    // lookup_entities: the full-scan shapes. The result counts are the
    // report OPT-26-01-04 recomputes, so they are asserted here and
    // recorded; the walls are recorded, not asserted.
    let criteria_all = LookUpEntitiesCriteria::default();
    let (all_mem, (all_wall, matched_all)) = measure(|| {
        let started = Instant::now();
        let matched = lookup_entities(&storage, &defs, 1, &criteria_all).unwrap();
        (started.elapsed(), matched)
    });
    assert_eq!(matched_all.len(), ROWS, "the whole corpus matches ALL");
    record.put(
        "query_lookup_all_ms",
        format!("{:.2}", all_wall.as_secs_f64() * 1_000.0),
    );
    record.put("report_lookup_all", matched_all.len());
    record.put("query_lookup_all_allocs", all_mem.allocs);
    record.put("query_lookup_all_peak_kib", all_mem.peak_delta() / 1024);

    let online_criteria = LookUpEntitiesCriteria {
        filter: LookUpEntitiesFilter::Online,
        ..LookUpEntitiesCriteria::default()
    };
    let matched_online = lookup_entities(&storage, &defs, 1, &online_criteria).unwrap();
    assert_eq!(
        matched_online.len(),
        online,
        "the online filter sees exactly the checked-out entities"
    );
    record.put("report_lookup_online", matched_online.len());

    let offline_criteria = LookUpEntitiesCriteria {
        filter: LookUpEntitiesFilter::Offline,
        ..LookUpEntitiesCriteria::default()
    };
    let matched_offline = lookup_entities(&storage, &defs, 1, &offline_criteria).unwrap();
    assert_eq!(matched_offline.len(), ROWS - online);
    record.put("report_lookup_offline", matched_offline.len());

    // A property query on the final level of entity 0 (21), whose
    // seeds are exactly three entities.
    let target_level = final_level(0);
    let mut prop_criteria = LookUpEntitiesCriteria::default();
    prop_criteria.add_property_query("level", target_level.to_string());
    let matched_prop = lookup_entities(&storage, &defs, 1, &prop_criteria).unwrap();
    assert_eq!(
        matched_prop.len(),
        3,
        "level {target_level} has three seeds"
    );
    record.put("report_lookup_prop_level", matched_prop.len());

    // A sorted, limited query: the top 10 by level descending, the
    // report's ordering shape.
    let sorted_criteria = LookUpEntitiesCriteria {
        sort_property: "level".to_string(),
        is_ascending: false,
        limit: 10,
        ..LookUpEntitiesCriteria::default()
    };
    let matched_sorted = lookup_entities(&storage, &defs, 1, &sorted_criteria).unwrap();
    assert_eq!(matched_sorted.len(), 10, "LIMIT 10");
    record.put("report_lookup_sorted_limit", matched_sorted.len());

    // One representative full-scan under the allocation window: what a
    // query-heavy phase costs the caller per lookup.
    let (query_mem, _) = measure(|| {
        let warmup = LookUpEntitiesCriteria::default();
        lookup_entities(&storage, &defs, 1, &warmup).unwrap()
    });
    record.put("query_block_allocs", query_mem.allocs);
    record.put("query_block_bytes", query_mem.bytes);
    record.put("query_block_peak_kib", query_mem.peak_delta() / 1024);

    // --- the sample report: level windows over the final state --------
    for (index, (start, end)) in LEVEL_WINDOWS.iter().enumerate() {
        let count = window_count(*start, *end);
        assert!(count > 0, "window {index} is not empty");
        record.put(&format!("report_win_{index}_{start}_{end}"), count);
    }
    let total: usize = LEVEL_WINDOWS
        .iter()
        .map(|(s, e)| window_count(*s, *e))
        .sum();
    assert_eq!(total, ROWS, "the windows partition the whole corpus");

    record.flush();
    drop(storage);
    pg_drop_db(&mut admin, &conninfo);
}

// ---------------------------------------------------------------------------
// Structural fixtures (serverless)
// ---------------------------------------------------------------------------

#[test]
fn the_corpus_is_deterministic() {
    assert_eq!(base_values(7), base_values(7));
    assert_eq!(evolved_values(7, 0), base_values(7));
    assert_ne!(evolved_values(7, 1), base_values(7));
    assert_ne!(evolved_values(7, 2), evolved_values(7, 1));
}

#[test]
fn the_final_level_follows_the_replay_step() {
    assert_eq!(final_level(0), 21);
    assert_eq!(final_level(199), (199i64 + 21).rem_euclid(200));
    assert_eq!(final_level(511), (111i64 + 21).rem_euclid(200));
}

#[test]
fn the_windows_partition_the_corpus() {
    let total: usize = LEVEL_WINDOWS
        .iter()
        .map(|(s, e)| window_count(*s, *e))
        .sum();
    assert_eq!(total, ROWS, "the four windows cover the whole corpus");
}

#[test]
fn the_property_query_seeds_are_pinned() {
    // The property query the baseline runs: the final level of entity
    // 0, which exactly three seeds reach (0, 200, 400 after the +21
    // step, wrapping around 200).
    assert_eq!(final_level(0), 21);
    let seeds: Vec<u64> = (0..ROWS as u64).filter(|i| final_level(*i) == 21).collect();
    assert_eq!(seeds, vec![0, 200, 400]);
}

#[test]
fn the_online_stride_shapes_the_filter_legs() {
    let online = (0..ROWS as u64).step_by(LOGON_STRIDE as usize).count();
    assert_eq!(online, ROWS / LOGON_STRIDE as usize);
    let offline = ROWS - online;
    assert_eq!(online + offline, ROWS);
}
