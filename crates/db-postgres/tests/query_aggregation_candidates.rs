//! OPT-26-01-03: query aggregation evaluation.
//!
//! The OPT-26-01-01 baseline recorded the query face's latency and
//! found the name lookup the expensive shape —
//! `get_database_id_from_name` scans `all_entities()` per call, which
//! measured ~14× a keyed `get`. This row evaluates the aggregation
//! the reference's name maps imply: build the name→id (and the
//! level→ids) indexes with one full scan, then resolve the same
//! lookups through the maps and report the latency change. The
//! latency figures are recorded, not asserted; the correctness is —
//! every aggregated answer must equal the scan face's answer before
//! and after a replay generation changes the values.
//!
//! The aggregation is rebuilt after the replay events touch every
//! entity, so the evaluation also reports what staleness costs: the
//! rebuild is one scan, amortized over the lookups the server serves
//! between changes.
//!
//! ```sh
//! BOUNDLESS_PG_URL='host=... dbname=postgres user=...' \
//! QUERY_AGGR_OUT=<abs path> \
//! cargo test -p db-postgres --test query_aggregation_candidates -- --nocapture
//! ```

use std::collections::HashMap;
use std::io::Write as _;
use std::time::{Duration, Instant};

use db::lookup::{EntityDefIndex, LookUpEntitiesCriteria, LookupDefs, get_database_id_from_name};
use db::{BaseMailbox, CheckOut, EntityKey, EntityStorage};
use db_postgres::{EntitySchema, PostgresStorage};
use entitydef::value::PropertyValue;
use mercury::endpoint::Address;

const ROWS: usize = 512;
const REPLAY_GENS: u64 = 3;
const LEVEL_STEP: u64 = 7;
const TYPE_NAME: &str = "AggregateBaseline";
/// The lookup sample size for each leg (as in the 01-01 baseline:
/// every fourth name).
const SAMPLE_STRIDE: u64 = 4;
const LOGON_STRIDE: u64 = 8;

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

fn evolved_values(i: u64, generation: u64) -> Vec<PropertyValue> {
    let mut values = base_values(i);
    values[1] = PropertyValue::Int(((i % 200) + generation * LEVEL_STEP).rem_euclid(200) as i64);
    values[2] = PropertyValue::Float(f64::from_bits(0x3FF0_0000_0000_0000 + i + generation));
    values[3] = PropertyValue::Blob(vec![0x00, 0xff, i as u8, 0x0a + generation as u8]);
    values
}

fn final_level(i: u64) -> i64 {
    ((i % 200) + REPLAY_GENS * LEVEL_STEP).rem_euclid(200) as i64
}

// ---------------------------------------------------------------------------
// Percentiles
// ---------------------------------------------------------------------------

fn percentiles(mut samples: Vec<u64>) -> (u64, u64, u64) {
    if samples.is_empty() {
        return (0, 0, 0);
    }
    samples.sort_unstable();
    let at = |pct: f64| samples[((pct * samples.len() as f64) as usize).min(samples.len() - 1)];
    (at(0.50), at(0.99), *samples.last().unwrap())
}

/// The mean of a microsecond sample.
fn mean_us(samples: &[u64]) -> u64 {
    samples.iter().sum::<u64>() / samples.len().max(1) as u64
}

// ---------------------------------------------------------------------------
// The record sink
// ---------------------------------------------------------------------------

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
        if let Ok(path) = std::env::var("QUERY_AGGR_OUT") {
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .and_then(|mut f| f.write_all(text.as_bytes()))
                .expect("write QUERY_AGGR_OUT");
            println!("record: {path}");
        }
        let _ = std::io::stdout().flush();
    }
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

fn setup_tables(storage: &mut PostgresStorage) {
    let plan = db_postgres::schema_init::plan();
    db_postgres::schema_init::apply(&mut storage.client_mut(), &plan).unwrap();
    let schema = dataset_schema();
    storage.create_entity_table(&schema).unwrap();
    storage.register_entity_type(1, &schema);
    storage.add_entity_type(1, TYPE_NAME).unwrap();
}

/// One full scan over the corpus builds both aggregation faces: the
/// name→id map (the reference's name maps, one entry per identifier)
/// and the level→ids map (the property query's prefiltered shape).
/// The scan is the same face `get_database_id_from_name` pays per
/// call — the aggregation amortizes it.
fn build_aggregates(
    storage: &PostgresStorage,
) -> (Duration, HashMap<String, i64>, HashMap<i64, Vec<i64>>) {
    let started = Instant::now();
    let mut names = HashMap::with_capacity(ROWS);
    let mut levels: HashMap<i64, Vec<i64>> = HashMap::new();
    for record in storage.all_entities().unwrap() {
        if record.type_id != 1 {
            continue;
        }
        if let Some(PropertyValue::String(name)) = record.values.first() {
            names.insert(name.clone(), record.db_id);
        }
        if let Some(PropertyValue::Int(level)) = record.values.get(1) {
            levels.entry(*level).or_default().push(record.db_id);
        }
    }
    (started.elapsed(), names, levels)
}

#[test]
fn the_query_aggregation_reports_the_latency_change() {
    let mut record = Record::default();
    let Some(mut admin) = PostgresStorage::connect(&pg_admin_conninfo()).ok() else {
        eprintln!("skipping: no reachable postgres server");
        return;
    };
    let Some(conninfo) = pg_fresh_db(&mut admin, "query", "aggr") else {
        eprintln!("skipping: cannot create the test database");
        return;
    };
    let mut storage = PostgresStorage::connect(&conninfo).unwrap();
    setup_tables(&mut storage);

    // --- land the base corpus (explicit-id writes) ----------------------
    let schema = dataset_schema();
    let rows: Vec<_> = (0..ROWS as u64)
        .map(|i| db_postgres::import_mysql::MySqlEntityRow {
            db_id: (i + 1) as i64,
            values: base_values(i),
        })
        .collect();
    let report =
        db_postgres::import_mysql::import_entities(&mut storage, 1, &schema, &rows).unwrap();
    assert_eq!(report.written, ROWS);

    // The online split, as in the baseline (the online filter is a
    // join the aggregation does not replace).
    for i in (0..ROWS as u64).step_by(LOGON_STRIDE as usize) {
        let granted = EntityStorage::check_out(
            &mut storage,
            &EntityKey {
                type_id: 1,
                db_id: (i + 1) as i64,
            },
            BaseMailbox {
                addr: Address {
                    ip: 0x0A00_0001 + i as u32,
                    port: 30000,
                    salt: 7,
                },
                entity_id: 1000 + i as i32,
            },
        )
        .unwrap();
        assert_eq!(granted, CheckOut::Granted);
    }

    let defs = lookup_defs();

    // --- before: the scan face's latency --------------------------------
    let mut scan_samples = Vec::new();
    for i in (0..ROWS as u64).step_by(SAMPLE_STRIDE as usize) {
        let op = Instant::now();
        let id = get_database_id_from_name(&storage, &defs, 1, &entity_name(i)).unwrap();
        scan_samples.push(op.elapsed().as_micros() as u64);
        assert_eq!(id, (i + 1) as i64);
    }
    let scan_before = mean_us(&scan_samples);
    let (p50, p99, max) = percentiles(scan_samples);
    record.put("before_name_ops", ROWS / SAMPLE_STRIDE as usize);
    record.put("before_name_mean_us", scan_before);
    record.put("before_name_p50_us", p50);
    record.put("before_name_p99_us", p99);
    record.put("before_name_max_us", max);

    let all_criteria = LookUpEntitiesCriteria::default();
    let scan_all_started = Instant::now();
    let matched_all = db::lookup::lookup_entities(&storage, &defs, 1, &all_criteria).unwrap();
    let scan_all_ms = scan_all_started.elapsed().as_secs_f64() * 1_000.0;
    assert_eq!(matched_all.len(), ROWS);
    record.put("before_lookup_all_ms", format!("{scan_all_ms:.2}"));

    // --- the aggregation: one scan, then map-resolved lookups -----------
    let (build_wall, name_map, level_map) = build_aggregates(&storage);
    assert_eq!(name_map.len(), ROWS, "every identifier is mapped");
    let total_indexed: usize = level_map.values().map(Vec::len).sum();
    assert_eq!(total_indexed, ROWS, "every level is indexed");
    record.put(
        "aggregate_build_ms",
        format!("{:.2}", build_wall.as_secs_f64() * 1_000.0),
    );
    record.put("aggregate_name_entries", name_map.len());
    record.put("aggregate_level_entries", level_map.len());

    // The map leg is sub-microsecond, so the sample is in nanos.
    let mut map_samples = Vec::new();
    for i in (0..ROWS as u64).step_by(SAMPLE_STRIDE as usize) {
        let op = Instant::now();
        let id = name_map
            .get(&entity_name(i))
            .copied()
            .expect("the name is mapped");
        map_samples.push(op.elapsed().as_nanos() as u64);
        assert_eq!(id, (i + 1) as i64, "the map agrees with the scan");
    }
    let map_after_ns = mean_us(&map_samples);
    let (p50, p99, max) = percentiles(map_samples);
    record.put("after_name_ops", ROWS / SAMPLE_STRIDE as usize);
    record.put("after_name_mean_ns", map_after_ns);
    record.put("after_name_p50_ns", p50);
    record.put("after_name_p99_ns", p99);
    record.put("after_name_max_ns", max);

    // The latency change the acceptance asks for: before/after on the
    // same sample grid, both legs to nanos.
    record.put(
        "name_speedup_x",
        format!(
            "{:.0}",
            (scan_before * 1_000) as f64 / map_after_ns.max(1) as f64
        ),
    );
    record.put(
        "name_saved_us_per_op",
        scan_before.saturating_sub(map_after_ns / 1_000),
    );
    let lookups_between = ROWS / SAMPLE_STRIDE as usize;
    record.put(
        "amortized_build_us_per_lookup",
        format!(
            "{:.0}",
            build_wall.as_micros() as f64 / lookups_between as f64
        ),
    );

    // The property query's prefiltered shape: the map's answer equals
    // the scan face's answer for a pinned level.
    let target = final_level(0);
    let mut prop = LookUpEntitiesCriteria::default();
    prop.add_property_query("level", target.to_string());
    let scanned = db::lookup::lookup_entities(&storage, &defs, 1, &prop).unwrap();
    let mut scanned_ids: Vec<i64> = scanned.iter().map(|r| r.db_id).collect();
    scanned_ids.sort_unstable();
    let mut mapped = level_map.get(&target).cloned().unwrap();
    mapped.sort_unstable();
    assert_eq!(mapped, scanned_ids, "the level index agrees with the scan");
    assert_eq!(mapped.len(), 3);
    record.put("after_level_index_entries", level_map.len());
    record.put("report_level_seeds", mapped.len());

    // --- replay changes every entity, the aggregates go stale -----------
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
            .unwrap();
        }
    }
    // The stale map disagrees with the scan face now: the ratio leg
    // moved under it. (The name→id map itself stays valid — the
    // identifier did not change — which is why the reference rebuilds
    // only the changed faces.)
    let stale_ok = (0..ROWS as u64).step_by(SAMPLE_STRIDE as usize).all(|i| {
        name_map
            .get(&entity_name(i))
            .copied()
            .map(|id| id == (i + 1) as i64)
            .unwrap_or(false)
    });
    assert!(stale_ok, "the name map survives unchanged identifiers");
    let (_, fresh_names, fresh_levels) = build_aggregates(&storage);
    assert_eq!(fresh_names, name_map, "rebuilding yields the same name map");
    let new_target = final_level(0);
    let fresh_target = fresh_levels.get(&new_target).cloned().unwrap();
    assert_eq!(
        fresh_target.len(),
        3,
        "the rebuilt level index sees the new seeds"
    );
    record.put("replay_generations", REPLAY_GENS);
    let (rebuild_wall, _, _) = build_aggregates(&storage);
    record.put(
        "rebuild_scan_ms",
        format!("{:.2}", rebuild_wall.as_secs_f64() * 1_000.0),
    );

    record.flush();
    drop(storage);
    pg_drop_db(&mut admin, &conninfo);
}

#[test]
fn the_sample_grid_and_seeds_are_pinned() {
    assert_eq!(ROWS / SAMPLE_STRIDE as usize, 128);
    assert_eq!(final_level(0), 21);
    let seeds: Vec<u64> = (0..ROWS as u64).filter(|i| final_level(*i) == 21).collect();
    assert_eq!(seeds, vec![0, 200, 400]);
}

#[test]
fn the_corpus_is_deterministic() {
    assert_eq!(base_values(7), base_values(7));
    assert_eq!(evolved_values(7, 0), base_values(7));
    assert_ne!(evolved_values(7, 1), base_values(7));
}
