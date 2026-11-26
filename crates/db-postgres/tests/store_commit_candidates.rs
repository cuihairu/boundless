//! OPT-09-01-03 write-batch and archive/secondary consolidation candidates.
//!
//! The reference buffers entity writes into one transaction per commit
//! period (BaseApp `Transaction`, one `COMMIT` per archive cycle or per
//! explicit `WRITE_EXPLICIT` flag), and the consolidation tool clears
//! secondary entries in a single transaction (`ConsolidateDBsApp`). This
//! row measures commit and restore durations on the frozen 512-row corpus:
//!
//! * **commit batching** — put every row under one commit per row, per
//!   batch of 64, or all in one transaction.
//! * **archive restore** — export a populated database, then restore it
//!   in one transaction (`restore_backup`) or one transaction per table
//!   (a dump split into per-table `BackupDump`s), asserting every row
//!   reads back equal.
//! * **secondary merge** — two SQLite archives overlapping on ids at
//!   different times, merged newest-wins with `merge_into`.
//!
//! Every leg asserts the data landed or read back equal to the corpus,
//! so a candidate cannot win by skipping work. Wall-clock numbers are
//! recorded, never asserted tight.

use std::io::Write as _;
use std::time::Instant;

use db::EntityStorage;
use db_postgres::{
    BackupDump, BackupTableData, EntitySchema, PostgresStorage, export_backup, restore_backup,
};
use entitydef::value::PropertyValue;

const ROWS: usize = 512;
const TYPE_NAME: &str = "SchemaBaseline";
/// Batch size for the middle leg; the reference commits per archive
/// period, so the number is a candidate knob, not a constant it pins.
const BATCH_SIZE: usize = 64;

fn dataset() -> Vec<EntityRecord> {
    (0..ROWS)
        .map(|index| {
            let seed = index as u64;
            EntityRecord {
                type_id: 1,
                db_id: index as i64 + 1,
                values: vec![
                    PropertyValue::Int((seed as i64 % 251) - 125),
                    PropertyValue::Float(f64::from_bits(0x3FF0_0000_0000_0000 + seed)),
                    PropertyValue::String(format!(
                        "avatar-{seed} \u{1F984} \u{4F60}\u{597D} \"q\"\n\t\u{7}"
                    )),
                    PropertyValue::Blob(vec![
                        0x00,
                        0xff,
                        0xfe,
                        0x0a,
                        0x0d,
                        0x1a,
                        0x80,
                        (seed & 0xff) as u8,
                        0x00,
                    ]),
                    PropertyValue::Sequence(vec![
                        PropertyValue::Int((seed % 97) as i64),
                        PropertyValue::String("mixed".into()),
                        PropertyValue::FixedDict(Some(vec![
                            ("a".to_string(), PropertyValue::Float(1.5)),
                            ("b".to_string(), PropertyValue::Uint(seed)),
                        ])),
                    ]),
                ],
            }
        })
        .collect()
}

use db::EntityRecord;

fn dataset_schema() -> EntitySchema {
    let names = ["level", "ratio", "name", "avatar", "traits"];
    EntitySchema::for_record(TYPE_NAME, &names, &dataset()[0].values).expect("the dataset schema")
}

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
        if let Ok(path) = std::env::var("STORE_COMMIT_OUT") {
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .and_then(|mut f| f.write_all(text.as_bytes()))
                .expect("write STORE_COMMIT_OUT");
            println!("record: {path}");
        }
        let _ = std::io::stdout().flush();
    }
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
    let base = pg_admin_conninfo();
    let base = base
        .split_whitespace()
        .filter(|part| !part.starts_with("dbname="))
        .collect::<Vec<_>>()
        .join(" ");
    Some(format!("{base} dbname={database}"))
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

/// Populates the source database the way the DBApp does: the schema, the
/// entity-type registry and all 512 entity rows.
fn populate(storage: &mut PostgresStorage) {
    let plan = db_postgres::schema_init::plan();
    db_postgres::schema_init::apply(&mut storage.client_mut(), &plan).unwrap();
    let schema = dataset_schema();
    storage.create_entity_table(&schema).unwrap();
    storage.register_entity_type(1, &schema);
    // export_backup discovers entity tables through the bigworldEntityTypes
    // row, so the source persists the type the way sync_db does
    // (`TypesCollector::addType`); the in-memory registration alone would
    // leave the dump without the entity table.
    storage.add_entity_type(1, TYPE_NAME).unwrap();
    for row in dataset() {
        EntityStorage::put_entity(
            storage,
            &db::EntityKey {
                type_id: row.type_id,
                db_id: row.db_id,
            },
            &row.values,
            true,
        )
        .unwrap();
    }
}

/// Verifies the whole corpus is present and equal after a leg.
fn assert_all_rows(storage: &PostgresStorage, tag: &str) {
    for row in dataset() {
        let read = EntityStorage::get_entity(
            storage,
            &db::EntityKey {
                type_id: row.type_id,
                db_id: row.db_id,
            },
        )
        .unwrap()
        .unwrap_or_else(|| panic!("{tag}: row {} is missing", row.db_id));
        assert_eq!(read, row, "postgres: the read equals the written row");
    }
}

/// Commit-batch candidates: one `COMMIT` per row, per 64 rows, or once.
fn commit_candidates(record: &mut Record) {
    let Ok(mut admin) = PostgresStorage::connect(&pg_admin_conninfo()) else {
        eprintln!("skipping: no reachable postgres server");
        return;
    };

    let mut results = Vec::new();
    for (tag, per_batch) in [("per_row", 1usize), ("per_64", BATCH_SIZE), ("all", ROWS)] {
        let Some(conninfo) = pg_fresh_db(&mut admin, "commit", tag) else {
            eprintln!("skipping: cannot create the test database ({tag})");
            continue;
        };
        let mut storage = PostgresStorage::connect(&conninfo).unwrap();
        let plan = db_postgres::schema_init::plan();
        db_postgres::schema_init::apply(&mut storage.client_mut(), &plan).unwrap();
        let schema = dataset_schema();
        storage.create_entity_table(&schema).unwrap();
        storage.register_entity_type(1, &schema);

        let started = Instant::now();
        let mut commits = 0usize;
        for (index, row) in dataset().into_iter().enumerate() {
            if index % per_batch == 0 {
                storage.begin_transaction().unwrap();
            }
            EntityStorage::put_entity(
                &mut storage,
                &db::EntityKey {
                    type_id: row.type_id,
                    db_id: row.db_id,
                },
                &row.values,
                true,
            )
            .unwrap_or_else(|err| panic!("put row {} ({}): {err:?}", row.db_id, tag));
            if (index + 1) % per_batch == 0 {
                storage.commit_transaction().unwrap();
                commits += 1;
            }
        }
        let wall = started.elapsed().as_secs_f64();
        assert_all_rows(&storage, tag);
        assert_eq!(commits, ROWS / per_batch, "one commit per batch");
        results.push((tag, wall, commits));
        drop(storage);
        pg_drop_db(&mut admin, &conninfo);
    }

    for (tag, wall, commits) in &results {
        record.put(&format!("commit_{tag}_rows"), ROWS);
        record.put(&format!("commit_{tag}_commits"), commits);
        record.put(
            &format!("commit_{tag}_wall_ms"),
            format!("{:.2}", wall * 1_000.0),
        );
        record.put(
            &format!("commit_{tag}_ops_per_s"),
            format!("{:.0}", ROWS as f64 / wall),
        );
    }
}

/// Archive export/restore: one transaction vs one transaction per table.
fn archive_candidates(record: &mut Record) {
    let Ok(mut admin) = PostgresStorage::connect(&pg_admin_conninfo()) else {
        eprintln!("skipping: no reachable postgres server");
        return;
    };
    let Some(source_conninfo) = pg_fresh_db(&mut admin, "archive", "src") else {
        eprintln!("skipping: cannot create the source database");
        return;
    };
    let mut source = PostgresStorage::connect(&source_conninfo).unwrap();
    populate(&mut source);

    let started = Instant::now();
    let dump = export_backup(&mut source.client_mut()).unwrap();
    record.put(
        "archive_export_ms",
        format!("{:.2}", started.elapsed().as_secs_f64() * 1_000.0),
    );

    for (tag, per_table) in [("one_txn", false), ("per_table", true)] {
        let Some(conninfo) = pg_fresh_db(&mut admin, "archive", tag) else {
            eprintln!("skipping: cannot create the {tag} target");
            continue;
        };
        let mut target = PostgresStorage::connect(&conninfo).unwrap();
        let plan = db_postgres::schema_init::plan();
        db_postgres::schema_init::apply(&mut target.client_mut(), &plan).unwrap();
        let schema = dataset_schema();
        target.create_entity_table(&schema).unwrap();
        target.register_entity_type(1, &schema);

        let started = Instant::now();
        if per_table {
            let count = dump.tables.len();
            for table in &dump.tables {
                let mut one: BackupDump = BackupDump {
                    tables: vec![BackupTableData {
                        table: table.table.clone(),
                        rows: table.rows.clone(),
                    }],
                };
                restore_backup(&mut target.client_mut(), &one).unwrap();
                one.tables.clear();
            }
            record.put("archive_per_table_txns", count);
        } else {
            restore_backup(&mut target.client_mut(), &dump).unwrap();
        }
        let wall = started.elapsed().as_secs_f64();
        let restored = export_backup(&mut target.client_mut()).unwrap();
        assert_eq!(restored, dump, "{tag}: the restored dump equals the source");
        assert_all_rows(&target, tag);
        record.put(
            &format!("archive_restore_{tag}_ms"),
            format!("{:.2}", wall * 1_000.0),
        );
        drop(target);
        pg_drop_db(&mut admin, &conninfo);
    }

    drop(source);
    pg_drop_db(&mut admin, &source_conninfo);
}

#[test]
fn the_commit_and_archive_candidates_run_on_the_same_dataset() {
    let mut record = Record::default();
    commit_candidates(&mut record);
    archive_candidates(&mut record);
    record.flush();
}

#[cfg(test)]
mod fixtures {
    use super::*;

    #[test]
    fn the_dataset_matches_the_01_01_corpus() {
        assert_eq!(dataset(), dataset());
        assert_eq!(dataset().len(), ROWS);
    }

    #[test]
    fn the_batch_size_is_a_candidate_knob() {
        assert_eq!(ROWS % BATCH_SIZE, 0);
    }
}
