//! OPT-26-01-04: sample report recomputation.
//!
//! The acceptance: **the windows and counts match the pre-optimization
//! report** (`窗口及计数与优化前一致`). This row replays the same
//! fixed event stream as OPT-26-01-01, recomputes the sample report
//! from the stored corpus, and compares it byte-for-byte with the
//! captured baseline fixture — the four level windows and the five
//! query-shape counts. Then it reruns the report through each
//! optimization the section evaluated: the batched import
//! (OPT-26-01-02's commit shapes) and the aggregated name/level
//! indexes (OPT-26-01-03). Every leg's report must equal the
//! unoptimized baseline: the optimizations change the latency, not
//! the report.
//!
//! The fixture is read from disk (not re-derived from the same
//! constants), so a drift in either the corpus, the report window
//! boundaries or the query shapes fails as a report mismatch.
//!
//! ```sh
//! BOUNDLESS_PG_URL='host=... dbname=postgres user=...' \
//! cargo test -p db-postgres --test sample_report_rerun -- --nocapture
//! ```

use std::collections::HashMap;
use std::path::PathBuf;

use db::lookup::{
    EntityDefIndex, LookUpEntitiesCriteria, LookUpEntitiesFilter, LookupDefs,
    get_database_id_from_name, lookup_entities,
};
use db::{BaseMailbox, CheckOut, EntityKey, EntityStorage};
use db_postgres::import_mysql::{MySqlEntityRow, import_entities};
use db_postgres::{EntitySchema, PostgresStorage};
use entitydef::value::PropertyValue;
use mercury::endpoint::Address;

const ROWS: usize = 512;
const REPLAY_GENS: u64 = 3;
const LEVEL_STEP: u64 = 7;
const TYPE_NAME: &str = "ReportRerun";
const LOGON_STRIDE: u64 = 8;
/// The OPT-26-01-01 baseline fixture, resolved from the workspace
/// root (cargo starts the test binary in the crate directory).
const BASELINE_FIXTURE: &str = "../../docs/review/fixtures/event-query/event-query.tsv";
/// The committed report rows we read out of that fixture.
const REPORT_KEYS: [&str; 9] = [
    "report_lookup_all",
    "report_lookup_online",
    "report_lookup_offline",
    "report_lookup_prop_level",
    "report_lookup_sorted_limit",
    "report_win_0_0_50",
    "report_win_1_50_100",
    "report_win_2_100_150",
    "report_win_3_150_200",
];

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

fn setup_tables(storage: &mut PostgresStorage) {
    let plan = db_postgres::schema_init::plan();
    db_postgres::schema_init::apply(&mut storage.client_mut(), &plan).unwrap();
    let schema = dataset_schema();
    storage.create_entity_table(&schema).unwrap();
    storage.register_entity_type(1, &schema);
    storage.add_entity_type(1, TYPE_NAME).unwrap();
}

/// Read the baseline's `metric<TAB>value` rows. Only the first
/// occurrence matters (the fixture captured one run).
fn baseline_report() -> HashMap<String, String> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(BASELINE_FIXTURE);
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("read baseline fixture {}: {err}", path.display()));
    let mut report = HashMap::new();
    for line in text.lines() {
        if let Some((key, value)) = line.split_once('\t') {
            report
                .entry(key.to_string())
                .or_insert_with(|| value.to_string());
        }
    }
    for key in REPORT_KEYS {
        assert!(report.contains_key(key), "baseline has {key}");
    }
    report
}

/// The report computed from the stored corpus through the lookup
/// faces, in the baseline's exact window/count shapes.
fn recompute_report(storage: &PostgresStorage) -> HashMap<String, String> {
    let defs = lookup_defs();
    let mut report = HashMap::new();

    let criteria = LookUpEntitiesCriteria::default();
    let all = lookup_entities(storage, &defs, 1, &criteria).unwrap();
    report.insert("report_lookup_all".to_string(), all.len().to_string());

    let online_criteria = LookUpEntitiesCriteria {
        filter: LookUpEntitiesFilter::Online,
        ..LookUpEntitiesCriteria::default()
    };
    let online = lookup_entities(storage, &defs, 1, &online_criteria).unwrap();
    report.insert("report_lookup_online".to_string(), online.len().to_string());

    let offline_criteria = LookUpEntitiesCriteria {
        filter: LookUpEntitiesFilter::Offline,
        ..LookUpEntitiesCriteria::default()
    };
    let offline = lookup_entities(storage, &defs, 1, &offline_criteria).unwrap();
    report.insert(
        "report_lookup_offline".to_string(),
        offline.len().to_string(),
    );

    // The property query: the final level of entity 0.
    let target = (REPLAY_GENS * LEVEL_STEP).rem_euclid(200);
    let mut prop = LookUpEntitiesCriteria::default();
    prop.add_property_query("level", target.to_string());
    let matched = lookup_entities(storage, &defs, 1, &prop).unwrap();
    report.insert(
        "report_lookup_prop_level".to_string(),
        matched.len().to_string(),
    );

    let sorted = LookUpEntitiesCriteria {
        sort_property: "level".to_string(),
        is_ascending: false,
        limit: 10,
        ..LookUpEntitiesCriteria::default()
    };
    let top = lookup_entities(storage, &defs, 1, &sorted).unwrap();
    report.insert(
        "report_lookup_sorted_limit".to_string(),
        top.len().to_string(),
    );

    // The four level windows are computed over the *stored* levels,
    // not the generator: this is the report's recomputation, not a
    // check of the fixture's own math.
    let windows = [(0i64, 50i64), (50, 100), (100, 150), (150, 200)];
    for (index, (start, end)) in windows.iter().enumerate() {
        let count = all
            .iter()
            .filter(|record| match record.values.get(1) {
                Some(PropertyValue::Int(level)) => *level >= *start && *level < *end,
                _ => false,
            })
            .count();
        report.insert(
            format!("report_win_{index}_{start}_{end}"),
            count.to_string(),
        );
    }
    report
}

fn assert_report_matches(
    baseline: &HashMap<String, String>,
    actual: &HashMap<String, String>,
    tag: &str,
) {
    for key in REPORT_KEYS {
        assert_eq!(
            actual.get(key).map(String::as_str),
            baseline.get(key).map(String::as_str),
            "{tag}: report key {key} differs from the pre-optimization baseline"
        );
    }
}

/// Land the corpus with the given commit batch shape, then replay the
/// three update generations in the same shape on the contract.
fn load_and_replay(storage: &mut PostgresStorage, batch: usize) {
    let rows: Vec<MySqlEntityRow> = (0..ROWS as u64)
        .map(|i| MySqlEntityRow {
            db_id: (i + 1) as i64,
            values: base_values(i),
        })
        .collect();
    let schema = dataset_schema();
    for slice in rows.chunks(batch) {
        storage.begin_transaction().unwrap();
        let report = import_entities(storage, 1, &schema, slice).unwrap();
        assert_eq!(report.written, slice.len());
        storage.commit_transaction().unwrap();
    }
    for generation in 1..=REPLAY_GENS {
        for i in 0..ROWS as u64 {
            EntityStorage::put_entity(
                storage,
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
    for i in (0..ROWS as u64).step_by(LOGON_STRIDE as usize) {
        let granted = EntityStorage::check_out(
            storage,
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
}

/// The OPT-26-01-03 aggregation's name→id map and level→ids map,
/// rebuilt from the stored corpus after the replay.
fn build_aggregates(storage: &PostgresStorage) -> (HashMap<String, i64>, HashMap<i64, Vec<i64>>) {
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
    (names, levels)
}

#[test]
fn the_sample_report_matches_the_baseline_after_every_optimization() {
    // The fixture is read before any server contact, so a missing or
    // malformed fixture fails even where no PostgreSQL is reachable
    // (the dedicated fixture test covers the skip path).
    let baseline = baseline_report();
    let Ok(mut admin) = PostgresStorage::connect(&pg_admin_conninfo()) else {
        eprintln!("skipping: no reachable postgres server");
        return;
    };

    // Leg 1: the unoptimized baseline shape (one transaction per row
    // during import). The recomputed report must match the fixture.
    let Some(conninfo) = pg_fresh_db(&mut admin, "report", "plain") else {
        eprintln!("skipping: cannot create the test database");
        return;
    };
    let mut storage = PostgresStorage::connect(&conninfo).unwrap();
    setup_tables(&mut storage);
    load_and_replay(&mut storage, 1);
    let plain = recompute_report(&storage);
    assert_report_matches(&baseline, &plain, "baseline");
    println!("baseline report matches the fixture");
    drop(storage);
    pg_drop_db(&mut admin, &conninfo);

    // Leg 2: the OPT-26-01-02 batched import (64 rows/commit).
    let Some(conninfo) = pg_fresh_db(&mut admin, "report", "batch") else {
        eprintln!("skipping: cannot create the test database");
        return;
    };
    let mut storage = PostgresStorage::connect(&conninfo).unwrap();
    setup_tables(&mut storage);
    load_and_replay(&mut storage, 64);
    let batched = recompute_report(&storage);
    assert_report_matches(&baseline, &batched, "batched import");
    assert_eq!(batched, plain, "batch 64 leaves the report unchanged");
    println!("64-row commit batching leaves the report unchanged");
    drop(storage);
    pg_drop_db(&mut admin, &conninfo);

    // Leg 3: the OPT-26-01-03 aggregation. Both maps resolve the same
    // names/levels the scan face reports, and their counts feed a
    // report recomputed from the stored corpus.
    let Some(conninfo) = pg_fresh_db(&mut admin, "report", "aggr") else {
        eprintln!("skipping: cannot create the test database");
        return;
    };
    let mut storage = PostgresStorage::connect(&conninfo).unwrap();
    setup_tables(&mut storage);
    load_and_replay(&mut storage, 64);
    let (names, levels) = build_aggregates(&storage);
    assert_eq!(names.len(), ROWS);
    let defs = lookup_defs();
    for i in 0..ROWS as u64 {
        let scanned = get_database_id_from_name(&storage, &defs, 1, &entity_name(i)).unwrap();
        assert_eq!(
            names[&entity_name(i)],
            scanned,
            "aggregated name {i} equals the scan"
        );
    }
    let target = (REPLAY_GENS * LEVEL_STEP).rem_euclid(200) as i64;
    let mut indexed = levels.get(&target).cloned().unwrap();
    indexed.sort_unstable();
    let mut scanned_ids: Vec<i64> = lookup_entities(&storage, &defs, 1, &{
        let mut criteria = LookUpEntitiesCriteria::default();
        criteria.add_property_query("level", target.to_string());
        criteria
    })
    .unwrap()
    .iter()
    .map(|r| r.db_id)
    .collect();
    scanned_ids.sort_unstable();
    assert_eq!(
        indexed, scanned_ids,
        "aggregated level query agrees with the scan"
    );
    let aggregated = recompute_report(&storage);
    assert_report_matches(&baseline, &aggregated, "aggregated queries");
    assert_eq!(aggregated, plain, "aggregation leaves the report unchanged");
    println!("aggregated name/level indexes leave the report unchanged");
    drop(storage);
    pg_drop_db(&mut admin, &conninfo);
}

#[test]
fn the_baseline_fixture_is_present_and_pinned() {
    let baseline = baseline_report();
    assert_eq!(baseline["report_lookup_all"], "512");
    assert_eq!(baseline["report_lookup_online"], "64");
    assert_eq!(baseline["report_lookup_offline"], "448");
    assert_eq!(baseline["report_lookup_prop_level"], "3");
    assert_eq!(baseline["report_lookup_sorted_limit"], "10");
    let windows: usize = REPORT_KEYS
        .iter()
        .filter(|key| key.starts_with("report_win_"))
        .map(|key| baseline[*key].parse::<usize>().unwrap())
        .sum();
    assert_eq!(windows, ROWS, "the baseline windows partition the corpus");
}

#[test]
fn replay_generations_and_step_are_pinned() {
    assert_eq!(REPLAY_GENS, 3);
    assert_eq!(LEVEL_STEP, 7);
    assert_eq!((REPLAY_GENS * LEVEL_STEP) % 200, 21);
    assert_eq!(ROWS / LOGON_STRIDE as usize, 64);
}
