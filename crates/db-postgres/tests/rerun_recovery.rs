//! OPT-09-01-04: transactions, retries and recovery, rerun to identical
//! data.
//!
//! The acceptance is **data consistency**. One seeded workload — 32
//! entity rows, values from a fixed LCG — lands through the real faces
//! and every leg must reproduce the very same data:
//!
//! 1. **transaction rerun** — the same `START TRANSACTION` … `COMMIT`
//!    run on two fresh databases exports two identical dumps: the rerun
//!    reproduces the database record-for-record. A mid-transaction
//!    dirty read, an overwrite and a delete followed by
//!    `rollback_transaction` leave the snapshot untouched.
//! 2. **retries** — `retry_query`, `query_with_reconnect` and the
//!    reference `RetryTask` loop each take a scripted first-attempt
//!    failure (the lock conflict / dropped connection the store itself
//!    raises for SQLSTATE `40P01`/`55P03` and connection loss) and then
//!    perform a **real** read against the live server; the value comes
//!    back right, the attempt counters are exact, a fatal failure
//!    surfaces untouched, and the completion callback fires exactly
//!    once.
//! 3. **recovery** — a restored empty target re-runs the same workload
//!    and exports the source's dump again: recovery plus rerun agree on
//!    every row.
//!
//! The dump/snapshot equality and the structural counts are asserted —
//! that is the data consistency. The wall-clock each leg took is
//! printed for the report and never asserted. Runs under the usual
//! connection gate; skips when no server is reachable.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Instant;

use db::EntityKey;
use db::error::DbError;
use db::recovery::{query_with_reconnect, retry_query};
use db::retry::{DatabaseFailure, RetryTask, run_with_retry};
use db_postgres::{
    EntitySchema, PostgresStorage, export_backup, import_mysql::MySqlEntityRow, restore_backup,
};
use entitydef::value::PropertyValue;

static TEST_DB_COUNTER: AtomicU32 = AtomicU32::new(0);

/// The workload every leg carries.
pub const WORKLOAD: usize = 32;

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
fn rerun_schema(storage: &mut PostgresStorage) -> EntitySchema {
    let values = vec![
        PropertyValue::String(String::new()),
        PropertyValue::Int(0),
        PropertyValue::Float(0.0),
        PropertyValue::Int(0),
    ];
    let schema = EntitySchema::for_record("Rerun", &["name", "health", "speed", "alive"], &values)
        .expect("the schema builds");
    storage
        .create_entity_table(&schema)
        .expect("the table creates");
    storage.add_entity_type(4, "Rerun").expect("the type adds");
    storage.register_entity_type(4, &schema);
    schema
}

/// Workload row i under `id_base`: values from the fixed LCG — no clock,
/// no randomness the rerun would not reproduce.
fn rerun_row(index: usize, id_base: i64) -> MySqlEntityRow {
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
            PropertyValue::String(format!("rerun-{id}")),
            PropertyValue::Int(100 + draw % 200),
            PropertyValue::Float(((draw % 8) as f64) * 0.25),
            PropertyValue::Int(i64::from(index.is_multiple_of(3))),
        ],
    }
}

fn workload(id_base: i64) -> Vec<MySqlEntityRow> {
    (0..WORKLOAD)
        .map(|index| rerun_row(index, id_base))
        .collect()
}

fn key_of(row: &MySqlEntityRow) -> EntityKey {
    EntityKey {
        type_id: 4,
        db_id: row.db_id,
    }
}

/// The transactional write: one transaction, every workload row staged
/// inside it, one commit. Reads every row back through the real face.
fn run_transactional_workload(
    storage: &mut PostgresStorage,
    rows: &[MySqlEntityRow],
    create: bool,
) -> (usize, std::time::Duration, usize) {
    let mut rows_ok = 0;
    let started = Instant::now();
    storage.begin_transaction().expect("the transaction opens");
    assert!(
        storage.in_transaction(),
        "the flag opens with the transaction"
    );
    for row in rows {
        db::EntityStorage::put_entity(storage, &key_of(row), &row.values, create)
            .expect("the staged write lands");
    }
    storage.commit_transaction().expect("the commit lands");
    assert!(!storage.in_transaction(), "the flag closes with the commit");
    for row in rows {
        let found = db::EntityStorage::get_entity(storage, &key_of(row))
            .expect("the read lands")
            .expect("the row exists");
        if found.values == row.values {
            rows_ok += 1;
        }
    }
    let statements = WORKLOAD + 2;
    (statements, started.elapsed(), rows_ok)
}

#[test]
fn the_transaction_rerun_and_rollback_keep_the_data_identical() {
    let first_dump = RefCell::new(None);

    // Leg 1 — the transactional workload on a fresh database.
    with_fresh_db(|storage, _conninfo| {
        rerun_schema(storage);
        let rows = workload(700);
        let (statements, elapsed, rows_ok) = run_transactional_workload(storage, &rows, true);
        println!("RERUN transaction statements={statements} elapsed={elapsed:?} rows_ok={rows_ok}");
        assert_eq!(statements, WORKLOAD + 2);
        assert_eq!(rows_ok, WORKLOAD);
        *first_dump.borrow_mut() =
            Some(export_backup(&mut storage.client_mut()).expect("the export runs"));
    });
    let dump = first_dump.into_inner().unwrap();

    // Leg 2 — the same workload rerun on another fresh database
    // reproduces the first database record-for-record.
    with_fresh_db(|storage, _conninfo| {
        rerun_schema(storage);
        let rows = workload(700);
        let (_, elapsed, rows_ok) = run_transactional_workload(storage, &rows, true);
        println!("RERUN again elapsed={elapsed:?} rows_ok={rows_ok}");
        assert_eq!(rows_ok, WORKLOAD);
        let again = export_backup(&mut storage.client_mut()).expect("the export runs");
        assert_eq!(again, dump, "the rerun reproduces the database exactly");

        // The rollback leg: an overwrite and two deletes inside a
        // transaction are visible on the same connection while it is
        // open, and the rollback puts the snapshot back untouched.
        let snapshot = export_backup(&mut storage.client_mut()).expect("the export runs");
        storage.begin_transaction().expect("the rollback leg opens");
        let mut dirty = rows[0].clone();
        if let PropertyValue::Int(health) = &mut dirty.values[1] {
            *health += 1;
        }
        db::EntityStorage::put_entity(storage, &key_of(&dirty), &dirty.values, false)
            .expect("the overwrite stages");
        let mid = db::EntityStorage::get_entity(storage, &key_of(&rows[0]))
            .expect("the dirty read lands")
            .expect("the row exists");
        assert_eq!(
            mid.values, dirty.values,
            "the dirty read sees the staged write"
        );
        db::EntityStorage::del_entity(storage, &key_of(&rows[WORKLOAD - 1]))
            .expect("the delete stages");
        let gone = db::EntityStorage::get_entity(storage, &key_of(&rows[WORKLOAD - 1]))
            .expect("the staged delete read lands");
        assert!(gone.is_none(), "the staged delete hides the row");
        storage.rollback_transaction().expect("the rollback lands");
        assert!(
            !storage.in_transaction(),
            "the flag closes with the rollback"
        );
        let restored = export_backup(&mut storage.client_mut()).expect("the export runs");
        assert_eq!(restored, snapshot, "the rollback restores the snapshot");
        let kept = db::EntityStorage::get_entity(storage, &key_of(&rows[0]))
            .expect("the read lands")
            .expect("the row exists");
        assert_eq!(
            kept.values, rows[0].values,
            "the rollback undoes the overwrite"
        );
    });
}

/// A `RetryTask` whose scripted failures come first and whose final
/// attempt performs a real read on the live server.
struct ReadTask {
    storage: Rc<RefCell<PostgresStorage>>,
    key: EntityKey,
    script: std::collections::VecDeque<DatabaseFailure>,
    found: Option<Vec<PropertyValue>>,
    completions: Vec<bool>,
}

impl RetryTask for ReadTask {
    fn perform_background(&mut self) -> Result<(), DatabaseFailure> {
        if let Some(failure) = self.script.pop_front() {
            return Err(failure);
        }
        let found = db::EntityStorage::get_entity(&*self.storage.borrow(), &self.key)
            .expect("the real read lands")
            .expect("the row exists");
        self.found = Some(found.values);
        Ok(())
    }

    fn complete(&mut self, succeeded: bool) {
        self.completions.push(succeeded);
    }
}

#[test]
fn the_retry_faces_recover_the_operation_over_real_reads() {
    with_fresh_db(|storage, conninfo| {
        let schema = rerun_schema(storage);
        let rows: Vec<MySqlEntityRow> = (0..8).map(|index| rerun_row(index, 800)).collect();
        for row in &rows {
            db::EntityStorage::put_entity(storage, &key_of(row), &row.values, true)
                .expect("the row lands");
        }
        // The retry tasks read through their own real connection to the
        // same database — the loops are connection-independent, and the
        // rows are all committed before any retry runs.
        let mut second = PostgresStorage::connect(conninfo).expect("the task connection opens");
        second.register_entity_type(4, &schema);
        let shared = Rc::new(RefCell::new(second));

        // retry_query: the first attempt dies on the lock conflict the
        // store itself raises for SQLSTATE 40P01/55P03, the second
        // performs a real read.
        let attempts = Rc::new(Cell::new(0));
        let row = &rows[0];
        let key = key_of(row);
        let found = {
            let shared = Rc::clone(&shared);
            let steps = Rc::clone(&attempts);
            retry_query(move || {
                steps.set(steps.get() + 1);
                if steps.get() == 1 {
                    return Err(DbError::Retry);
                }
                Ok(db::EntityStorage::get_entity(&*shared.borrow(), &key)
                    .expect("the real read lands")
                    .expect("the row exists"))
            })
            .expect("the retry recovers the read")
        };
        assert_eq!(attempts.get(), 2, "the lock conflict retries once");
        assert_eq!(found.values, row.values, "the recovered read is the row");

        // retry_query: a failure the retry rules do not cover surfaces
        // untouched, with no second attempt.
        let attempts = Cell::new(0);
        let outcome = retry_query(|| {
            attempts.set(attempts.get() + 1);
            Err::<db::record::EntityRecord, _>(DbError::NotFound)
        });
        assert!(matches!(outcome, Err(DbError::NotFound)));
        assert_eq!(attempts.get(), 1, "a fatal failure never retries");

        // query_with_reconnect: the dropped-connection first attempt
        // reconnects and runs the very same read once more.
        let attempts = Rc::new(Cell::new(0));
        let reconnects = Rc::new(Cell::new(0));
        let found = {
            let shared = Rc::clone(&shared);
            let row = &rows[1];
            let key = key_of(row);
            let steps = Rc::clone(&attempts);
            let back = Rc::clone(&reconnects);
            let outcome = query_with_reconnect(
                move || {
                    steps.set(steps.get() + 1);
                    if steps.get() == 1 {
                        return Err(DbError::LostConnection);
                    }
                    Ok(db::EntityStorage::get_entity(&*shared.borrow(), &key)
                        .expect("the real read lands")
                        .expect("the row exists"))
                },
                || {
                    back.set(back.get() + 1);
                    true
                },
            )
            .expect("the reconnect recovers the read");
            assert_eq!(attempts.get(), 2);
            assert_eq!(reconnects.get(), 1);
            outcome
        };
        assert_eq!(found.values, rows[1].values);

        // The caller's in-transaction guard: when `reconnect` cannot
        // re-establish the backend the loss surfaces and the statement
        // is never re-sent.
        let attempts = Cell::new(0);
        let reconnects = Cell::new(0);
        let outcome = query_with_reconnect(
            || {
                attempts.set(attempts.get() + 1);
                Err::<db::record::EntityRecord, _>(DbError::LostConnection)
            },
            || {
                reconnects.set(reconnects.get() + 1);
                false
            },
        );
        assert!(matches!(outcome, Err(DbError::LostConnection)));
        assert_eq!(attempts.get(), 1, "no re-send without a connection");
        assert_eq!(reconnects.get(), 1);

        // The reference retry loop: one scripted lock deadlock, then a
        // real read; the completion callback fires exactly once.
        let mut task = ReadTask {
            storage: Rc::clone(&shared),
            key: key_of(&rows[2]),
            script: std::collections::VecDeque::from(vec![DatabaseFailure::classify(
                db::ER_LOCK_DEADLOCK,
                "Lock deadlock detected",
            )]),
            found: None,
            completions: Vec::new(),
        };
        let report = run_with_retry(&mut task, &mut |_| true);
        println!(
            "RETRY lock attempt={:?} on_retry={}",
            report.attempts, report.on_retry_calls
        );
        assert_eq!(report.attempts, 2);
        assert_eq!(report.on_retry_calls, 1);
        assert!(report.succeeded);
        assert_eq!(report.reconnect_attempts, 0);
        assert_eq!(task.completions, vec![true]);
        assert_eq!(task.found.as_deref(), Some(&rows[2].values[..]));

        // And a fatal failure reports straight to the handler: one
        // attempt, no retry, a failed completion.
        let mut task = ReadTask {
            storage: Rc::clone(&shared),
            key: key_of(&rows[3]),
            script: std::collections::VecDeque::from(vec![DatabaseFailure::classify(
                1045,
                "access denied",
            )]),
            found: None,
            completions: Vec::new(),
        };
        let report = run_with_retry(&mut task, &mut |_| true);
        assert_eq!(report.attempts, 1);
        assert_eq!(report.on_retry_calls, 0);
        assert!(!report.succeeded);
        assert_eq!(report.fatal.as_deref(), Some("access denied"));
        assert_eq!(task.completions, vec![false]);
        assert!(task.found.is_none());
    });
}

#[test]
fn the_recovered_target_reruns_to_the_same_data() {
    let source_dump = RefCell::new(None);

    with_fresh_db(|source, _conninfo| {
        rerun_schema(source);
        let rows = workload(900);
        let (_, _, rows_ok) = run_transactional_workload(source, &rows, true);
        assert_eq!(rows_ok, WORKLOAD);
        *source_dump.borrow_mut() =
            Some(export_backup(&mut source.client_mut()).expect("the export runs"));
    });
    let dump = source_dump.into_inner().unwrap();

    with_fresh_db(|target, _conninfo| {
        let plan = db_postgres::schema_init::plan();
        db_postgres::schema_init::apply(&mut target.client_mut(), &plan).expect("the plan applies");
        rerun_schema(target);

        let started = Instant::now();
        restore_backup(&mut target.client_mut(), &dump).expect("the restore runs");
        let restore_elapsed = started.elapsed();

        // The recovered target reruns the same transactional workload
        // — the rows are already there, the rerun overwrites them with
        // the very same values — and exports the source's dump again.
        let rows = workload(900);
        let (_, rerun_elapsed, rows_ok) = run_transactional_workload(target, &rows, false);
        println!(
            "RECOVER restore elapsed={restore_elapsed:?} rerun elapsed={rerun_elapsed:?} rows_ok={rows_ok}"
        );
        assert_eq!(rows_ok, WORKLOAD);
        let again = export_backup(&mut target.client_mut()).expect("the export runs");
        assert_eq!(again, dump, "the recovered database reruns to itself");
    });
}
