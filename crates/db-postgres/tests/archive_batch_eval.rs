//! OPT-09-01-03: write batches and the archive round trip, evaluated.
//!
//! The acceptance reports commit and recovery durations. One seeded
//! workload — 64 entity rows, values from a fixed LCG — lands through
//! three write shapes on the real PostgreSQL server:
//!
//! 1. **autocommit** — a `put_entity` per row, each its own commit.
//!    64 statements, 64 commits.
//! 2. **transaction batch** — the same 64 writes inside one
//!    `START TRANSACTION` … `COMMIT`. 66 statements, 1 commit.
//! 3. **write batch** — one multi-row `INSERT`, every row's values in a
//!    single statement. 1 statement, 1 implicit commit.
//!
//! Then the archive integration: `export_backup` dumps the whole
//! populated source, `restore_backup` lands it in an empty initialised
//! target, and the target's own export compares equal to the dump —
//! record identity, the recovery's correctness. Both durations print for
//! the report.
//!
//! The structural counts and record identity are asserted; the durations
//! are environment data, printed and recorded in the review doc, never
//! asserted. Runs under the usual connection gate; skips when no server
//! is reachable.

use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Instant;

use db::EntityKey;
use db_postgres::{
    EntitySchema, PostgresStorage, export_backup, import_mysql::MySqlEntityRow, restore_backup,
};
use entitydef::value::PropertyValue;
use postgres::types::ToSql;

static TEST_DB_COUNTER: AtomicU32 = AtomicU32::new(0);

/// The workload every write shape carries.
pub const WORKLOAD: usize = 64;

/// The libpq connection string for the admin connection, without a
/// database.
fn admin_conninfo() -> Option<String> {
    if let Ok(url) = std::env::var("BOUNDLESS_PG_URL") {
        return Some(url);
    }
    if let (Ok(host), Ok(port)) = (
        std::env::var("CI_STORE_PG_HOST"),
        std::env::var("CI_STORE_PG_PORT"),
    ) {
        let password = std::env::var("CI_STORE_PG_PASSWORD").unwrap_or_default();
        return Some(format!(
            "host={host} port={port} dbname=postgres user=boundless password={password}"
        ));
    }
    let user = whoami();
    Some(format!(
        "host=/var/run/postgresql dbname=postgres user={user}"
    ))
}

fn whoami() -> String {
    std::env::var("USER")
        .or_else(|_| std::env::var("LOGNAME"))
        .unwrap_or_else(|_| "postgres".to_string())
}

/// Runs `body` against a fresh private database with the fixed tables
/// created, or skips when no PostgreSQL server is reachable.
fn with_fresh_db(body: impl FnOnce(&mut PostgresStorage, &str)) {
    let Some(admin_conninfo) = admin_conninfo() else {
        eprintln!("skipping: set BOUNDLESS_PG_URL to run the postgres cases");
        return;
    };
    let mut admin = match PostgresStorage::connect(&admin_conninfo) {
        Ok(admin) => admin,
        Err(err) => {
            eprintln!("skipping: no reachable postgres server ({err})");
            return;
        }
    };
    let database = format!(
        "boundless_pg_test_{}_{}",
        std::process::id(),
        TEST_DB_COUNTER.fetch_add(1, Ordering::SeqCst)
    );
    let create = format!("CREATE DATABASE \"{}\"", database.replace('"', "\"\""));
    if let Err(err) = admin.client_mut().execute(create.as_str(), &[]) {
        eprintln!("skipping: cannot create the test database ({err})");
        return;
    }
    let conninfo = format!("{admin_conninfo} dbname={database}");
    {
        let mut storage = PostgresStorage::connect(&conninfo).expect("the fresh database opens");
        let fixed = db_postgres::schema_init::fixed_table_ddl();
        {
            let mut client = storage.client_mut();
            for statement in &fixed {
                client
                    .execute(statement, &[])
                    .expect("the fixed tables create");
            }
        }
        body(&mut storage, &conninfo);
    }
    let drop = format!(
        "DROP DATABASE \"{}\" WITH (FORCE)",
        database.replace('"', "\"\"")
    );
    let _ = admin.client_mut().execute(drop.as_str(), &[]);
}

/// The workload type: a String, two Ints and a Float.
fn batch_schema(storage: &mut PostgresStorage) -> EntitySchema {
    let values = vec![
        PropertyValue::String(String::new()),
        PropertyValue::Int(0),
        PropertyValue::Float(0.0),
        PropertyValue::Int(0),
    ];
    let schema =
        EntitySchema::for_record("EvalBatch", &["name", "health", "speed", "alive"], &values)
            .expect("the schema builds");
    storage
        .create_entity_table(&schema)
        .expect("the table creates");
    storage
        .add_entity_type(4, "EvalBatch")
        .expect("the type adds");
    storage.register_entity_type(4, &schema);
    schema
}

/// Workload row i under `id_base`: values from the fixed LCG.
fn batch_row(index: usize, id_base: i64) -> MySqlEntityRow {
    let id = id_base + index as i64;
    let seed = (index as u64)
        .wrapping_mul(6_364_136_223_846_793_005)
        .wrapping_add(1)
        .wrapping_mul(6_364_136_223_846_793_005)
        .wrapping_add(1);
    let draw = (seed >> 33) as i64;
    MySqlEntityRow {
        db_id: id,
        values: vec![
            PropertyValue::String(format!("batch-{id}")),
            PropertyValue::Int(100 + draw % 200),
            PropertyValue::Float(((draw % 8) as f64) * 0.25),
            PropertyValue::Int(i64::from(index.is_multiple_of(3))),
        ],
    }
}

fn workload(id_base: i64) -> Vec<MySqlEntityRow> {
    (0..WORKLOAD)
        .map(|index| batch_row(index, id_base))
        .collect()
}

/// A write shape's run: what it sent and how long that took.
#[derive(Debug)]
struct BatchRun {
    statements: usize,
    commits: usize,
    elapsed: std::time::Duration,
    rows_ok: usize,
}

fn key_of(row: &MySqlEntityRow) -> EntityKey {
    EntityKey {
        type_id: 4,
        db_id: row.db_id,
    }
}

/// Shape 1 — a `put_entity` per row, each its own commit.
fn run_autocommit(storage: &mut PostgresStorage, rows: &[MySqlEntityRow]) -> BatchRun {
    let mut statements = 0;
    let mut commits = 0;
    let mut rows_ok = 0;
    let started = Instant::now();
    for row in rows {
        db::EntityStorage::put_entity(storage, &key_of(row), &row.values, true)
            .expect("the write lands");
        statements += 1;
        commits += 1;
    }
    for row in rows {
        let found = db::EntityStorage::get_entity(storage, &key_of(row))
            .expect("the read lands")
            .expect("the row exists");
        if found.values == row.values {
            rows_ok += 1;
        }
    }
    BatchRun {
        statements,
        commits,
        elapsed: started.elapsed(),
        rows_ok,
    }
}

/// Shape 2 — the same writes inside one transaction.
fn run_transaction(storage: &mut PostgresStorage, rows: &[MySqlEntityRow]) -> BatchRun {
    let mut statements = 0;
    let mut rows_ok = 0;
    let started = Instant::now();
    storage.begin_transaction().expect("the transaction opens");
    statements += 1;
    for row in rows {
        db::EntityStorage::put_entity(storage, &key_of(row), &row.values, true)
            .expect("the staged write lands");
        statements += 1;
    }
    storage.commit_transaction().expect("the commit lands");
    statements += 1;
    for row in rows {
        let found = db::EntityStorage::get_entity(storage, &key_of(row))
            .expect("the read lands")
            .expect("the row exists");
        if found.values == row.values {
            rows_ok += 1;
        }
    }
    BatchRun {
        statements,
        commits: 1,
        elapsed: started.elapsed(),
        rows_ok,
    }
}

/// Shape 3 — one multi-row `INSERT`: every row's values in a single
/// statement, built from the schema's own columns.
fn run_write_batch(storage: &mut PostgresStorage, schema: &EntitySchema, rows: &[MySqlEntityRow]) {
    let started = Instant::now();
    let mut sql = format!(
        "INSERT INTO {} ({}) VALUES ",
        schema.quoted_table(),
        schema.quoted_columns().join(", ")
    );
    let mut tuples: Vec<(i64, String, i64, f64, i64)> = Vec::with_capacity(rows.len());
    for (index, row) in rows.iter().enumerate() {
        if index > 0 {
            sql.push_str(", ");
        }
        let base = index * 5;
        sql.push_str(&format!(
            "(${},${},${},${},${})",
            base + 1,
            base + 2,
            base + 3,
            base + 4,
            base + 5
        ));
        tuples.push(extract_tuple(row));
    }
    let mut params: Vec<&(dyn ToSql + Sync)> = Vec::with_capacity(tuples.len() * 5);
    for (id, name, health, speed, alive) in &tuples {
        params.push(id);
        params.push(name);
        params.push(health);
        params.push(speed);
        params.push(alive);
    }
    let landed = storage
        .client_mut()
        .execute(&sql, &params)
        .expect("the batch lands");
    assert_eq!(landed as usize, rows.len(), "the batch lands every row");

    // The rows read back identical.
    let ids: Vec<i64> = rows.iter().map(|row| row.db_id).collect();
    let columns = schema.quoted_columns().join(", ");
    let read = format!(
        "SELECT {} FROM {} WHERE id = ANY($1)",
        columns,
        schema.quoted_table()
    );
    let query_rows = storage
        .client_mut()
        .query(&read, &[&ids])
        .expect("the merged read lands");
    let mut rows_ok = 0;
    for (index, query_row) in query_rows.iter().enumerate() {
        let name: String = query_row.get(1);
        let health: i64 = query_row.get(2);
        let speed: f64 = query_row.get(3);
        let alive: i64 = query_row.get(4);
        let expected = extract_tuple(&rows[index]);
        if (name, health, speed, alive) == (expected.1, expected.2, expected.3, expected.4) {
            rows_ok += 1;
        }
    }
    let elapsed = started.elapsed();
    println!(
        "SHAPE write-batch statements=1 commits=1 (implicit) elapsed={elapsed:?} rows_ok={rows_ok}"
    );
    assert_eq!(rows_ok, rows.len(), "the batch writes every row identical");
}

/// The workload row's values, in statement order.
fn extract_tuple(row: &MySqlEntityRow) -> (i64, String, i64, f64, i64) {
    (
        row.db_id,
        match &row.values[0] {
            PropertyValue::String(name) => name.clone(),
            _ => unreachable!("the name is a string"),
        },
        match &row.values[1] {
            PropertyValue::Int(health) => *health,
            _ => unreachable!("the health is an int"),
        },
        match &row.values[2] {
            PropertyValue::Float(speed) => *speed,
            _ => unreachable!("the speed is a float"),
        },
        match &row.values[3] {
            PropertyValue::Int(alive) => *alive,
            _ => unreachable!("the alive is an int"),
        },
    )
}

#[test]
fn the_write_shapes_and_the_archive_report_their_costs() {
    let source_dump = std::cell::RefCell::new(None);
    let mut export_elapsed = std::time::Duration::ZERO;
    with_fresh_db(|source, _conninfo| {
        let schema = batch_schema(source);
        // Each shape its own id base: the shapes share one database on
        // purpose, so no shape lands on another's rows.
        let autocommit_rows = workload(400);
        let transaction_rows = workload(500);
        let batch_rows = workload(600);

        let autocommit = run_autocommit(source, &autocommit_rows);
        let transaction = run_transaction(source, &transaction_rows);
        println!(
            "SHAPE autocommit  statements={} commits={} elapsed={:?} rows_ok={}",
            autocommit.statements, autocommit.commits, autocommit.elapsed, autocommit.rows_ok
        );
        println!(
            "SHAPE transaction statements={} commits=1 elapsed={:?} rows_ok={}",
            transaction.statements, transaction.elapsed, transaction.rows_ok
        );
        assert_eq!(autocommit.statements, WORKLOAD);
        assert_eq!(autocommit.commits, WORKLOAD);
        assert_eq!(autocommit.rows_ok, WORKLOAD);
        assert_eq!(transaction.statements, WORKLOAD + 2);
        assert_eq!(transaction.rows_ok, WORKLOAD);
        run_write_batch(source, &schema, &batch_rows);

        // The archive integration: dump the populated source.
        let started = Instant::now();
        let dump = export_backup(&mut source.client_mut()).expect("the export runs");
        export_elapsed = started.elapsed();
        println!(
            "ARCHIVE export tables={} elapsed={export_elapsed:?}",
            dump.tables.len()
        );
        *source_dump.borrow_mut() = Some(dump);
    });
    let dump = source_dump.into_inner().unwrap();

    // The recovery: an empty initialised target restores the dump, and
    // its own export compares equal — record identity.
    with_fresh_db(|target, _conninfo| {
        let plan = db_postgres::schema_init::plan();
        db_postgres::schema_init::apply(&mut target.client_mut(), &plan).expect("the plan applies");
        let schema = batch_schema(target);
        target
            .create_entity_table(&schema)
            .expect("the table creates");

        let started = Instant::now();
        restore_backup(&mut target.client_mut(), &dump).expect("the restore runs");
        let restore_elapsed = started.elapsed();
        println!("ARCHIVE restore rows={} elapsed={restore_elapsed:?}", {
            dump.tables
                .iter()
                .map(|table| table.rows.len())
                .sum::<usize>()
        });

        let started = Instant::now();
        let restored = export_backup(&mut target.client_mut()).expect("the export runs");
        let verify_elapsed = started.elapsed();
        println!("ARCHIVE re-export elapsed={verify_elapsed:?}");
        assert_eq!(
            restored, dump,
            "the restored database exports identical records"
        );
    });
    let _ = export_elapsed;
}
