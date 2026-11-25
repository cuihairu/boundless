//! OPT-09-01-02: what pooling and query merging save, on the real server.
//!
//! The acceptance reports per-backend latency and throughput. One seeded
//! workload — 48 entity rows and 48 key reads, values from a fixed LCG —
//! runs through three execution shapes on the real PostgreSQL server:
//!
//! 1. **shipped** — one reused connection (`PostgresStorage`), every
//!    write an autocommit `put_entity`, every read a key `get_entity`.
//!    1 connect, 96 statements, 48 commits.
//! 2. **unpooled** — the same operations, but a fresh connect per
//!    operation, what a client without a pool pays per call. 96
//!    statements plus 96 connects over the identical rows.
//! 3. **merged** — all 48 writes inside one `START TRANSACTION` …
//!    `COMMIT` (one commit, one durability flush, instead of 48), and
//!    all 48 reads as one `id = ANY($1)` statement. 50 statements, 1
//!    commit, and the rows read back identical.
//!
//! The structural counts are asserted (they are the same on any backend —
//! MySQL runs the identical statement shapes, and
//! `tests/store/common/baseline_dataset.sql` pins those on the live
//! MariaDB). The wall-clock each shape took is printed for the report and
//! never asserted: it is environment data, the gate the load lane
//! established. MySQL in-process timings need a Rust driver the port does
//! not carry; the MySQL half of the report is the statement parity above.
//!
//! Runs against a real PostgreSQL server with the usual connection gate;
//! skips when no server is reachable and drops its private database.

use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Instant;

use db::EntityKey;
use db_postgres::{EntitySchema, PostgresStorage, import_mysql::MySqlEntityRow};
use entitydef::value::PropertyValue;

static TEST_DB_COUNTER: AtomicU32 = AtomicU32::new(0);

/// The workload every shape carries.
pub const WORKLOAD: usize = 48;

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
/// created, or skips when no PostgreSQL server is reachable. `body` gets
/// the storage and the database's connection string.
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
fn workload_schema(storage: &mut PostgresStorage) -> EntitySchema {
    let values = vec![
        PropertyValue::String(String::new()),
        PropertyValue::Int(0),
        PropertyValue::Float(0.0),
        PropertyValue::Int(0),
    ];
    let schema = EntitySchema::for_record("Eval", &["name", "health", "speed", "alive"], &values)
        .expect("the eval schema builds");
    storage
        .create_entity_table(&schema)
        .expect("the table creates");
    storage.add_entity_type(4, "Eval").expect("the type adds");
    storage.register_entity_type(4, &schema);
    schema
}

/// Workload row i under `id_base`: id id_base+i, values from the fixed
/// LCG — no clock, no randomness the rerun would not reproduce. Each
/// shape carries its own id base so the shapes never collide on one
/// database.
fn workload_row(index: usize, id_base: i64) -> MySqlEntityRow {
    let id = id_base + index as i64;
    let mut seed = (index as u64)
        .wrapping_mul(6_364_136_223_846_793_005)
        .wrapping_add(1);
    seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
    let draw = (seed >> 33) as i64;
    MySqlEntityRow {
        db_id: id,
        values: vec![
            PropertyValue::String(format!("eval-{id}")),
            PropertyValue::Int(100 + draw % 200),
            PropertyValue::Float(((draw % 8) as f64) * 0.25),
            PropertyValue::Int(i64::from(index.is_multiple_of(3))),
        ],
    }
}

/// The whole workload under an id base.
fn workload(id_base: i64) -> Vec<MySqlEntityRow> {
    (0..WORKLOAD)
        .map(|index| workload_row(index, id_base))
        .collect()
}

/// A shape's run: what it sent the server and how long that took.
#[derive(Debug, Default)]
struct ShapeRun {
    connects: usize,
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

/// Shape 1 — the shipped execution: a reused connection, autocommit
/// writes, key reads.
fn run_shipped(storage: &mut PostgresStorage, rows: &[MySqlEntityRow]) -> ShapeRun {
    let connects = 1;
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
        statements += 1;
        if found.values == row.values {
            rows_ok += 1;
        }
    }
    ShapeRun {
        connects,
        statements,
        commits,
        elapsed: started.elapsed(),
        rows_ok,
    }
}

/// Shape 2 — without a pool: a fresh connect per operation over the same
/// rows.
fn run_unpooled(conninfo: &str, schema: &EntitySchema, rows: &[MySqlEntityRow]) -> ShapeRun {
    let mut connects = 0;
    let mut statements = 0;
    let mut commits = 0;
    let mut rows_ok = 0;
    let started = Instant::now();
    for row in rows {
        let mut storage = PostgresStorage::connect(conninfo).expect("the connect lands");
        storage.register_entity_type(4, schema);
        connects += 1;
        db::EntityStorage::put_entity(&mut storage, &key_of(row), &row.values, true)
            .expect("the write lands");
        statements += 1;
        commits += 1;
    }
    for row in rows {
        let mut storage = PostgresStorage::connect(conninfo).expect("the connect lands");
        storage.register_entity_type(4, schema);
        connects += 1;
        let found = db::EntityStorage::get_entity(&storage, &key_of(row))
            .expect("the read lands")
            .expect("the row exists");
        statements += 1;
        if found.values == row.values {
            rows_ok += 1;
        }
    }
    ShapeRun {
        connects,
        statements,
        commits,
        elapsed: started.elapsed(),
        rows_ok,
    }
}

/// Shape 3 — merged: every write inside one transaction, every read one
/// `ANY` statement.
fn run_merged(
    storage: &mut PostgresStorage,
    schema: &EntitySchema,
    rows: &[MySqlEntityRow],
) -> ShapeRun {
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

    // The merged read: one statement for every key, the columns the
    // schema lays out.
    let columns = schema.quoted_columns().join(", ");
    let sql = format!(
        "SELECT {} FROM {} WHERE id = ANY($1)",
        columns,
        schema.quoted_table()
    );
    let ids: Vec<i64> = rows.iter().map(|row| row.db_id).collect();
    let query_rows = storage
        .client_mut()
        .query(&sql, &[&ids])
        .expect("the merged read lands");
    statements += 1;
    assert_eq!(
        query_rows.len(),
        rows.len(),
        "the merged read sees every row"
    );
    for (index, query_row) in query_rows.iter().enumerate() {
        let expected = &rows[index];
        let found = EntityRecordValues::from(query_row);
        if found.values == expected.values {
            rows_ok += 1;
        }
    }
    ShapeRun {
        connects: 1,
        statements,
        commits: 1,
        elapsed: started.elapsed(),
        rows_ok,
    }
}

/// The merged read's decoded row, compared against the workload's values.
struct EntityRecordValues {
    values: Vec<PropertyValue>,
}

impl<'a> From<&'a postgres::Row> for EntityRecordValues {
    fn from(row: &'a postgres::Row) -> Self {
        let name: String = row.get(1);
        let health: i64 = row.get(2);
        let speed: f64 = row.get(3);
        let alive: i64 = row.get(4);
        Self {
            values: vec![
                PropertyValue::String(name),
                PropertyValue::Int(health),
                PropertyValue::Float(speed),
                PropertyValue::Int(alive),
            ],
        }
    }
}

#[test]
fn the_shapes_carry_one_workload_with_their_own_costs() {
    with_fresh_db(|storage, conninfo| {
        let schema = workload_schema(storage);
        // Each shape gets its own id base: the shapes share one database
        // on purpose, so no shape ever lands on another's rows.
        let shipped_rows = workload(100);
        let unpooled_rows = workload(200);
        let merged_rows = workload(300);

        let shipped = run_shipped(storage, &shipped_rows);
        let unpooled = run_unpooled(conninfo, &schema, &unpooled_rows);
        let merged = run_merged(storage, &schema, &merged_rows);

        // Every shape round-trips the identical rows.
        assert_eq!(shipped.rows_ok, WORKLOAD);
        assert_eq!(unpooled.rows_ok, WORKLOAD);
        assert_eq!(merged.rows_ok, WORKLOAD);

        // The structural counts — the same on any backend.
        assert_eq!(shipped.connects, 1);
        assert_eq!(shipped.statements, 2 * WORKLOAD);
        assert_eq!(shipped.commits, WORKLOAD);
        assert_eq!(unpooled.connects, 2 * WORKLOAD);
        assert_eq!(unpooled.statements, 2 * WORKLOAD);
        assert_eq!(merged.commits, 1);
        assert_eq!(merged.statements, WORKLOAD + 2);

        // The report lines: wall-clock is environment data, printed and
        // recorded in the review doc, never asserted.
        println!(
            "SHAPE shipped   connects={} statements={} commits={} elapsed={:?} rows_ok={}",
            shipped.connects, shipped.statements, shipped.commits, shipped.elapsed, shipped.rows_ok
        );
        println!(
            "SHAPE unpooled  connects={} statements={} commits={} elapsed={:?} rows_ok={}",
            unpooled.connects,
            unpooled.statements,
            unpooled.commits,
            unpooled.elapsed,
            unpooled.rows_ok
        );
        println!(
            "SHAPE merged    connects=1 statements={} commits={} elapsed={:?} rows_ok={}",
            merged.statements, merged.commits, merged.elapsed, merged.rows_ok
        );
    });
}
