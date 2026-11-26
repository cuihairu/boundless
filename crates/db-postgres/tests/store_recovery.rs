//! OPT-09-01-04 rerun of transactions, retries and recovery.
//!
//! The reference drives every entity operation as a background task whose
//! `doBackgroundTask` loop retries a rolled-back attempt on a lock
//! failure, chases a lost connection, and fires the completion callback
//! exactly once (`tasks/background_task.cpp:51-100`); the port carries
//! that contract in `db::retry` and the reconnect legs of
//! `PostgresStorage`. This row reruns the OPT-09-01-01 corpus through
//! those faces on the live backend and asserts **the dataset stays
//! consistent** after every scenario:
//!
//! * **rerun** — the whole corpus through the retry-driven task loop,
//!   one transaction per row versus per 64 rows (the 01-03 verdict's
//!   archive-period shape), same-run walls for the before/after pair.
//! * **retry** — a real lock conflict (`lock_timeout` → 55P03, the
//!   40P01/55P03 shapes `map_db_error` calls retryable) rerun after the
//!   blocker commits, and a scripted first-attempt failure through
//!   `run_with_retry`.
//! * **recovery** — a failed transaction leaves nothing half-written;
//!   a terminated backend auto-recovers outside a transaction and is
//!   recovered manually inside one; a wiped table is restored from the
//!   exported backup.
//!
//! Every leg ends with the full 512-row corpus asserted present and
//! equal. Walls are recorded, never asserted tight.

use std::io::Write as _;
use std::time::Instant;

use db::retry::{DatabaseFailure, RetryTask, run_with_retry};
use db::{DbError, EntityRecord, EntityStorage};
use db_postgres::{PostgresStorage, export_backup, restore_backup};
use entitydef::value::PropertyValue;

const ROWS: usize = 512;
const TYPE_NAME: &str = "SchemaBaseline";
/// The archive-period batch shape OPT-09-01-03 selected.
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

fn dataset_schema() -> db_postgres::EntitySchema {
    let names = ["level", "ratio", "name", "avatar", "traits"];
    db_postgres::EntitySchema::for_record(TYPE_NAME, &names, &dataset()[0].values)
        .expect("the dataset schema")
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
        if let Ok(path) = std::env::var("STORE_RECOVERY_OUT") {
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .and_then(|mut f| f.write_all(text.as_bytes()))
                .expect("write STORE_RECOVERY_OUT");
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

/// Creates a private database and returns its connection string, with
/// the base conninfo's own `dbname` stripped so the fresh name wins.
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

/// Sets up the schema and registers the dataset type the way the DBApp
/// does, persisting the `bigworldEntityTypes` row (the backup face
/// discovers entity tables through it).
fn init_schema(storage: &mut PostgresStorage) {
    let plan = db_postgres::schema_init::plan();
    db_postgres::schema_init::apply(&mut storage.client_mut(), &plan).unwrap();
    let schema = dataset_schema();
    storage.create_entity_table(&schema).unwrap();
    storage.register_entity_type(1, &schema);
    storage.add_entity_type(1, TYPE_NAME).unwrap();
}

/// Populates the whole corpus (one transaction per row).
fn populate(storage: &mut PostgresStorage) {
    init_schema(storage);
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

/// The acceptance face: the whole corpus is present and equal.
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
        assert_eq!(read, row, "{tag}: the read equals the written row");
    }
}

/// One rerun task: one batch of the corpus under one transaction, the
/// reference's shape of one background task per operation.
struct RerunPuts<'a> {
    storage: &'a mut PostgresStorage,
    /// The corpus slice this task owns: `start..end` indexes.
    range: (usize, usize),
    /// Explicit ids only fit an empty table; reruns over the populated
    /// corpus go through the update leg.
    explicit: bool,
}

impl RetryTask for RerunPuts<'_> {
    fn perform_background(&mut self) -> Result<(), DatabaseFailure> {
        let rows = dataset();
        self.storage.begin_transaction().unwrap();
        for row in &rows[self.range.0..self.range.1] {
            EntityStorage::put_entity(
                self.storage,
                &db::EntityKey {
                    type_id: row.type_id,
                    db_id: row.db_id,
                },
                &row.values,
                self.explicit,
            )
            .map_err(|err| db::retry::failure_from_db_error(&err, "rerun put"))?;
        }
        self.storage.commit_transaction().unwrap();
        Ok(())
    }

    fn complete(&mut self, _succeeded: bool) {}
}

/// Drives `batches` tasks of `per_batch` rows each through the retry
/// loop, returning the accumulated attempts and the wall time.
fn rerun_batches(storage: &mut PostgresStorage, per_batch: usize, explicit: bool) -> (usize, f64) {
    let started = Instant::now();
    let mut attempts = 0usize;
    for start in (0..ROWS).step_by(per_batch) {
        let mut task = RerunPuts {
            storage,
            range: (start, start + per_batch),
            explicit,
        };
        let report = run_with_retry(&mut task, &mut |_| true);
        assert!(report.succeeded, "the batch commits on a retryable loop");
        attempts += report.attempts;
    }
    (attempts, started.elapsed().as_secs_f64())
}

/// Rerun legs: per-row and per-64 commit shapes through the retry loop.
fn rerun_candidates(record: &mut Record) -> Option<()> {
    let Ok(mut admin) = PostgresStorage::connect(&pg_admin_conninfo()) else {
        eprintln!("skipping: no reachable postgres server");
        return None;
    };
    for (tag, per_batch) in [("per_row", 1usize), ("per_64", BATCH_SIZE)] {
        let conninfo = pg_fresh_db(&mut admin, "rerun", tag)?;
        let mut storage = PostgresStorage::connect(&conninfo).unwrap();
        init_schema(&mut storage);
        let (attempts, wall) = rerun_batches(&mut storage, per_batch, true);
        assert_eq!(
            attempts,
            ROWS / per_batch,
            "one task per batch, one attempt"
        );
        assert_all_rows(&storage, &format!("rerun_{tag}"));
        record.put(&format!("rerun_{tag}_tasks"), ROWS / per_batch);
        record.put(&format!("rerun_{tag}_attempts"), attempts);
        record.put(
            &format!("rerun_{tag}_wall_ms"),
            format!("{:.2}", wall * 1_000.0),
        );
        record.put(
            &format!("rerun_{tag}_ops_per_s"),
            format!("{:.0}", ROWS as f64 / wall),
        );
        drop(storage);
        pg_drop_db(&mut admin, &conninfo);
    }
    Some(())
}

/// A real retryable lock: the blocker holds row 1 under `FOR UPDATE`,
/// the writer's update times out on `lock_timeout` (55P03, mapped to
/// `DbError::Retry`), the blocker commits, and the rerun lands.
fn retry_lock_scenario(record: &mut Record) -> Option<()> {
    let Ok(mut admin) = PostgresStorage::connect(&pg_admin_conninfo()) else {
        eprintln!("skipping: no reachable postgres server");
        return None;
    };
    let conninfo = pg_fresh_db(&mut admin, "retry", "lock")?;
    let mut storage = PostgresStorage::connect(&conninfo).unwrap();
    populate(&mut storage);
    let mut blocker = PostgresStorage::connect(&conninfo).unwrap();

    // The writer's session gives up on a held lock after 200 ms instead
    // of waiting the server default out.
    storage
        .client_mut()
        .execute("SET lock_timeout = '200ms'", &[])
        .unwrap();
    blocker.begin_transaction().unwrap();
    blocker
        .client_mut()
        .execute(
            "SELECT \"id\" FROM \"tbl_SchemaBaseline\" WHERE \"id\" = 1 FOR UPDATE",
            &[],
        )
        .unwrap();

    // Attempt 1: the update leg dies on the held lock.
    let row = &dataset()[0];
    let update = |storage: &mut PostgresStorage| {
        EntityStorage::put_entity(
            storage,
            &db::EntityKey {
                type_id: row.type_id,
                db_id: row.db_id,
            },
            &row.values,
            false,
        )
    };
    let started = Instant::now();
    let first = update(&mut storage);
    assert!(
        matches!(first, Err(DbError::Retry)),
        "the held lock is retryable, got {first:?}"
    );

    // The blocker finishes; the rerun is the same operation on the same
    // connection.
    blocker.commit_transaction().unwrap();
    let second = update(&mut storage);
    assert!(
        second.is_ok(),
        "the rerun after the blocker commits: {second:?}"
    );
    let wall = started.elapsed().as_secs_f64();
    assert_all_rows(&storage, "retry_lock");
    record.put("retry_lock_attempts", 2);
    record.put("retry_lock_retried_error", "55P03 lock_timeout");
    record.put("retry_lock_wall_ms", format!("{:.2}", wall * 1_000.0));
    drop(blocker);
    drop(storage);
    pg_drop_db(&mut admin, &conninfo);
    Some(())
}

/// A scripted first-attempt failure through the reference's retry loop:
/// the operation lands exactly once, on the second attempt.
struct ScriptedPut<'a> {
    storage: &'a mut PostgresStorage,
    row: EntityRecord,
    attempt: usize,
}

impl RetryTask for ScriptedPut<'_> {
    fn perform_background(&mut self) -> Result<(), DatabaseFailure> {
        self.attempt += 1;
        if self.attempt == 1 {
            // The first attempt fails before touching the backend.
            return Err(DatabaseFailure::Retryable("lock wait timeout".into()));
        }
        EntityStorage::put_entity(
            self.storage,
            &db::EntityKey {
                type_id: self.row.type_id,
                db_id: self.row.db_id,
            },
            &self.row.values,
            false,
        )
        .map_err(|err| db::retry::failure_from_db_error(&err, "scripted put"))?;
        Ok(())
    }

    fn complete(&mut self, succeeded: bool) {
        assert!(succeeded, "the scripted put reports success once");
    }
}

fn retry_scripted_scenario(record: &mut Record) -> Option<()> {
    let Ok(mut admin) = PostgresStorage::connect(&pg_admin_conninfo()) else {
        eprintln!("skipping: no reachable postgres server");
        return None;
    };
    let conninfo = pg_fresh_db(&mut admin, "retry", "scripted")?;
    let mut storage = PostgresStorage::connect(&conninfo).unwrap();
    populate(&mut storage);

    // Row 1 is rewritten under the loop with an injected retryable
    // first attempt; the value must land exactly once.
    let mut rewritten = dataset()[0].clone();
    rewritten.values[0] = PropertyValue::Int(7);
    let mut task = ScriptedPut {
        storage: &mut storage,
        row: rewritten.clone(),
        attempt: 0,
    };
    let report = run_with_retry(&mut task, &mut |_| true);
    assert_eq!(report.attempts, 2, "one retry, then the commit");
    assert_eq!(report.on_retry_calls, 1, "onRetry fired once");
    assert!(report.succeeded);
    let read = EntityStorage::get_entity(
        &storage,
        &db::EntityKey {
            type_id: 1,
            db_id: 1,
        },
    )
    .unwrap()
    .expect("the rewritten row");
    assert_eq!(read.values[0], PropertyValue::Int(7), "the rerun's value");
    // The scenario's rewrite goes back to the corpus value so the leg
    // ends on the same dataset it started on.
    let original = dataset()[0].clone();
    EntityStorage::put_entity(
        &mut storage,
        &db::EntityKey {
            type_id: 1,
            db_id: 1,
        },
        &original.values,
        false,
    )
    .unwrap();
    assert_all_rows(&storage, "retry_scripted");
    record.put("retry_scripted_attempts", report.attempts);
    record.put("retry_scripted_on_retry_calls", report.on_retry_calls);
    drop(storage);
    pg_drop_db(&mut admin, &conninfo);
    Some(())
}

/// A transaction that dies mid-batch leaves nothing behind, and the
/// same batch reruns cleanly on the same connection.
fn rollback_scenario(record: &mut Record) -> Option<()> {
    let Ok(mut admin) = PostgresStorage::connect(&pg_admin_conninfo()) else {
        eprintln!("skipping: no reachable postgres server");
        return None;
    };
    let conninfo = pg_fresh_db(&mut admin, "rollback", "txn")?;
    let mut storage = PostgresStorage::connect(&conninfo).unwrap();
    init_schema(&mut storage);

    storage.begin_transaction().unwrap();
    let rows = dataset();
    let mut failed_at = None;
    for (index, row) in rows.iter().enumerate() {
        let put = EntityStorage::put_entity(
            &mut storage,
            &db::EntityKey {
                type_id: row.type_id,
                db_id: row.db_id,
            },
            &row.values,
            true,
        );
        if put.is_err() {
            failed_at = Some(index);
            break;
        }
    }
    // The corpus is duplicate-free, so the failure is forced: the same
    // row 1 again, an explicit-id duplicate inside the open transaction.
    let duplicate = EntityStorage::put_entity(
        &mut storage,
        &db::EntityKey {
            type_id: 1,
            db_id: 1,
        },
        &rows[0].values,
        true,
    );
    assert!(
        matches!(duplicate, Err(DbError::AlreadyExists)),
        "the duplicate id fails the batch: {duplicate:?}"
    );
    assert_eq!(failed_at, None, "the first pass of the corpus puts clean");
    storage.rollback_transaction().unwrap();

    // Nothing the dead transaction wrote is visible, on this connection
    // or any other.
    let mut witness = PostgresStorage::connect(&conninfo).unwrap();
    witness.register_entity_type(1, &dataset_schema());
    for row in rows.iter().take(8) {
        let read = EntityStorage::get_entity(
            &witness,
            &db::EntityKey {
                type_id: row.type_id,
                db_id: row.db_id,
            },
        )
        .unwrap();
        assert!(
            read.is_none(),
            "row {} did not survive the rollback",
            row.db_id
        );
    }

    // The same batch reruns clean and lands whole.
    let (attempts, _) = rerun_batches(&mut storage, BATCH_SIZE, true);
    assert_all_rows(&storage, "rollback_rerun");
    record.put("rollback_surviving_rows", 0);
    record.put("rollback_rerun_attempts", attempts);
    drop(witness);
    drop(storage);
    pg_drop_db(&mut admin, &conninfo);
    Some(())
}

/// Kills every backend of the test database except the admin's own.
fn terminate_backends(admin: &mut PostgresStorage, database: &str) {
    let rows = admin
        .client_mut()
        .query(
            "SELECT pid FROM pg_stat_activity \
             WHERE datname = $1 AND pid <> pg_backend_pid()",
            &[&database],
        )
        .unwrap();
    for row in rows {
        let pid: i32 = row.get(0);
        admin
            .client_mut()
            .execute("SELECT pg_terminate_backend($1)", &[&pid])
            .unwrap();
    }
}

/// Drives statements until the terminated backend's death is noticed,
/// so the loss is a closed connection by the time the scenario's real
/// statement runs (`pg_terminate_backend` may leave the error message
/// in flight for a moment).
fn notice_backend_loss(storage: &mut PostgresStorage) {
    for _ in 0..100 {
        if storage.client_mut().simple_query("SELECT 1").is_err() {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    panic!("the terminated backend was never noticed");
}

/// Lost-connection recovery: outside a transaction the statement
/// reconnects and reruns by itself; inside one the loss is terminal and
/// the caller reconnects and reruns the batch.
fn recovery_scenario(record: &mut Record) -> Option<()> {
    let Ok(mut admin) = PostgresStorage::connect(&pg_admin_conninfo()) else {
        eprintln!("skipping: no reachable postgres server");
        return None;
    };
    let conninfo = pg_fresh_db(&mut admin, "recovery", "lost")?;
    let database = db_name(&conninfo).to_string();
    let mut storage = PostgresStorage::connect(&conninfo).unwrap();
    populate(&mut storage);

    // Outside a transaction: the killed backend is replaced under the
    // statement's own reconnect leg, and the put lands.
    let row = &dataset()[1];
    terminate_backends(&mut admin, &database);
    notice_backend_loss(&mut storage);
    let rewritten = {
        let mut values = row.values.clone();
        values[0] = PropertyValue::Int(9);
        values
    };
    EntityStorage::put_entity(
        &mut storage,
        &db::EntityKey {
            type_id: row.type_id,
            db_id: row.db_id,
        },
        &rewritten,
        false,
    )
    .unwrap_or_else(|err| panic!("the auto-recovered put: {err:?}"));
    assert!(
        !storage.connection_lost(),
        "the reconnect cleared the lost flag"
    );
    let read = EntityStorage::get_entity(
        &storage,
        &db::EntityKey {
            type_id: row.type_id,
            db_id: row.db_id,
        },
    )
    .unwrap()
    .expect("the recovered row");
    assert_eq!(read.values[0], PropertyValue::Int(9));
    // The rewrite goes back to the corpus value; the rerun's updates
    // write the same values anyway, but the invariant stays explicit.
    EntityStorage::put_entity(
        &mut storage,
        &db::EntityKey {
            type_id: row.type_id,
            db_id: row.db_id,
        },
        &row.values,
        false,
    )
    .unwrap();

    // Inside a transaction the loss is terminal: the statement errors,
    // the rollback finds the connection gone, the caller reconnects and
    // reruns the batch.
    let rows = dataset();
    storage.begin_transaction().unwrap();
    for (index, row) in rows.iter().enumerate() {
        if index == 4 {
            terminate_backends(&mut admin, &database);
            notice_backend_loss(&mut storage);
        }
        let put = EntityStorage::put_entity(
            &mut storage,
            &db::EntityKey {
                type_id: row.type_id,
                db_id: row.db_id,
            },
            &row.values,
            false,
        );
        if put.is_err() {
            assert!(
                matches!(put, Err(DbError::LostConnection)),
                "the in-transaction loss is terminal, got {put:?}"
            );
            assert!(storage.connection_lost());
            break;
        }
    }
    let _ = storage.rollback_transaction();
    assert!(
        storage.reconnect(),
        "the manual reconnect after the terminal loss"
    );
    assert!(!storage.connection_lost());
    let (attempts, _) = rerun_batches(&mut storage, BATCH_SIZE, false);
    assert_all_rows(&storage, "recovery_rerun");
    record.put("recovery_auto_recovered_puts", 1);
    record.put("recovery_manual_reconnects", 1);
    record.put("recovery_rerun_attempts", attempts);
    drop(storage);
    pg_drop_db(&mut admin, &conninfo);
    Some(())
}

/// Backup recovery: a wiped table is rebuilt from the exported dump.
fn restore_recovery_scenario(record: &mut Record) -> Option<()> {
    let Ok(mut admin) = PostgresStorage::connect(&pg_admin_conninfo()) else {
        eprintln!("skipping: no reachable postgres server");
        return None;
    };
    let conninfo = pg_fresh_db(&mut admin, "recovery", "restore")?;
    let mut storage = PostgresStorage::connect(&conninfo).unwrap();
    populate(&mut storage);
    let dump = export_backup(&mut storage.client_mut()).unwrap();

    // The loss: every entity row goes away.
    let wiped = storage
        .client_mut()
        .execute("DELETE FROM \"tbl_SchemaBaseline\"", &[])
        .unwrap();
    assert_eq!(wiped, ROWS as u64, "the wipe takes the whole corpus");
    let gone = EntityStorage::get_entity(
        &storage,
        &db::EntityKey {
            type_id: 1,
            db_id: 1,
        },
    )
    .unwrap();
    assert!(gone.is_none(), "row 1 is gone after the wipe");

    let started = Instant::now();
    restore_backup(&mut storage.client_mut(), &dump).unwrap();
    let wall = started.elapsed().as_secs_f64();
    assert_all_rows(&storage, "restore_recovery");
    record.put("restore_wiped_rows", wiped);
    record.put("restore_tables", dump.tables.len());
    record.put("restore_wall_ms", format!("{:.2}", wall * 1_000.0));
    drop(storage);
    pg_drop_db(&mut admin, &conninfo);
    Some(())
}

#[test]
fn the_transaction_retry_and_recovery_scenarios_keep_the_dataset_consistent() {
    let mut record = Record::default();
    rerun_candidates(&mut record);
    retry_lock_scenario(&mut record);
    retry_scripted_scenario(&mut record);
    rollback_scenario(&mut record);
    recovery_scenario(&mut record);
    restore_recovery_scenario(&mut record);
    record.flush();
}

#[cfg(test)]
mod fixtures {
    use super::*;

    #[test]
    fn the_dataset_matches_the_01_01_corpus() {
        assert_eq!(dataset().len(), ROWS);
        let first = &dataset()[0];
        assert_eq!(first.db_id, 1);
        assert_eq!(first.values.len(), 5);
    }

    #[test]
    fn the_batch_size_divides_the_corpus() {
        assert_eq!(ROWS % BATCH_SIZE, 0);
    }
}
