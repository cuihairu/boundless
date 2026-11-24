//! Concurrent writes to the same entity on the PostgreSQL backend
//! (BW-09-06-08, 验收 两个写入同一实体的请求按存储契约返回结果，重试后没有
//! 重复记录).
//!
//! The reference retries exactly the two lock conditions
//! (`db_storage_mysql/transaction.cpp:57-60`, `shouldRetry` ==
//! `ER_LOCK_DEADLOCK`; `database_exception.cpp:35-40` adds the lock-wait
//! timeout): the attempt is rolled back and run again, and nothing of it
//! is visible until the retry commits. PostgreSQL spells those two
//! conditions `40P01 deadlock_detected` and `55P03 lock_not_available`,
//! and serializes same-row writers with row locks.
//!
//! The cases run against a real PostgreSQL server, with the connection
//! string from `BOUNDLESS_PG_URL`, or the CI storage service's
//! `CI_STORE_PG_*` settings, or a local server's unix socket; when no
//! server is reachable they skip with a notice. Every case gets its own
//! private database (the fixed tables included), dropped when the case
//! finishes.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::thread;

use db::{EntityKey, EntityStorage};
use db_postgres::EntitySchema;
use db_postgres::PostgresStorage;
use entitydef::value::PropertyValue;

static TEST_DB_COUNTER: AtomicU32 = AtomicU32::new(0);

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
/// the storage and the connection string other sides connect with — the
/// concurrent cases spawn scoped threads, which can borrow it; the
/// database is dropped when the body returns.
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
    // libpq takes the last occurrence of a repeated keyword, so appending
    // the database selects it even when the URL carried one.
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

/// Registers `type_id` with one Int property and creates its table.
fn register_counter(storage: &mut PostgresStorage, type_id: u16) {
    let values = vec![PropertyValue::Int(0)];
    let schema = EntitySchema::for_record("Counter", &["level"], &values).unwrap();
    storage.create_entity_table(&schema).unwrap();
    storage.register_entity_type(type_id, &schema);
}

fn counter(key_db_id: i64, level: i64) -> (EntityKey, Vec<PropertyValue>) {
    (
        EntityKey {
            type_id: 4,
            db_id: key_db_id,
        },
        vec![PropertyValue::Int(level)],
    )
}

/// Connects one concurrent side to the shared database.
fn side(conninfo: &str) -> PostgresStorage {
    let mut storage = PostgresStorage::connect(conninfo).expect("the side opens");
    register_counter(&mut storage, 4);
    storage
}

/// Reads the stored level of a counter entity.
fn stored_level(storage: &PostgresStorage, key: &EntityKey) -> i64 {
    let found = EntityStorage::get_entity(storage, key)
        .expect("the read runs")
        .expect("the row is there");
    match &found.values[0] {
        PropertyValue::Int(value) => *value,
        other => panic!("the counter level is not an int: {other:?}"),
    }
}

/// One side of the deadlock case, finishing its pair: the transaction is
/// already open with the side's first row taken, and this writes the pair
/// in the given order, retrying the whole group when the database
/// resolves a cycle against this side. The flags record whether a retry
/// was needed and whether the side landed.
fn run_side(
    storage: &mut PostgresStorage,
    keys: [EntityKey; 2],
    retried: Arc<AtomicBool>,
    done: Arc<AtomicBool>,
) {
    let values: [Vec<PropertyValue>; 2] = [
        vec![PropertyValue::Int(keys[0].db_id * 10 + 5)],
        vec![PropertyValue::Int(keys[1].db_id * 10 + 5)],
    ];
    for _ in 0..4 {
        let mut failed = None;
        if let Err(error) = EntityStorage::put_entity(storage, &keys[0], &values[0], false) {
            failed = Some(error);
        }
        if failed.is_none() {
            failed = EntityStorage::put_entity(storage, &keys[1], &values[1], false).err();
        }
        if let Some(error) = failed {
            assert_eq!(
                error,
                db::DbError::Retry,
                "only the lock conditions are retried"
            );
            retried.store(true, Ordering::SeqCst);
            storage
                .rollback_transaction()
                .expect("the deadlocked group unwinds");
            storage.begin_transaction().expect("the retry opens");
            continue;
        }
        storage.commit_transaction().expect("the commit lands");
        done.store(true, Ordering::SeqCst);
        return;
    }
    panic!("the writes never landed within the retry budget");
}

/// Two writers of one entity run through row locks: both succeed, the row
/// holds one final value, and no duplicate record is left behind.
#[test]
fn two_writers_of_one_entity_land_on_one_row() {
    with_fresh_db(|storage, conninfo| {
        register_counter(storage, 4);
        let (key, values) = counter(1, 1);
        EntityStorage::put_entity(storage, &key, &values, true).expect("the seed row lands");

        let written = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&written);
        thread::scope(|scope| {
            scope.spawn(move || {
                let mut other = side(conninfo);
                let (key, values) = counter(1, 300);
                EntityStorage::put_entity(&mut other, &key, &values, false)
                    .expect("the concurrent write lands");
                flag.store(true, Ordering::SeqCst);
            });
            // Give the side a head start so the two writes really overlap,
            // then run ours through the same row.
            thread::sleep(std::time::Duration::from_millis(150));
            let (key, values) = counter(1, 200);
            EntityStorage::put_entity(storage, &key, &values, false).expect("our write lands");
        });

        assert!(written.load(Ordering::SeqCst), "the side committed");
        let mut reader = PostgresStorage::connect(conninfo).expect("the reader opens");
        register_counter(&mut reader, 4);
        let final_level = stored_level(&reader, &key);
        assert!(
            final_level == 200 || final_level == 300,
            "the final value is one of the two writes: {final_level}"
        );
        assert_eq!(
            reader.all_entities().expect("the listing runs").len(),
            1,
            "no duplicate record was left behind"
        );
    });
}

/// A writer that waits past its lock timeout gets the reference's lock
/// condition — `DbError::Retry` — and the retry lands the write once the
/// holder commits, on one row.
#[test]
fn a_lock_wait_timeout_reports_retry_and_the_retry_lands_once() {
    with_fresh_db(|storage, conninfo| {
        register_counter(storage, 4);
        let (key, values) = counter(1, 1);
        EntityStorage::put_entity(storage, &key, &values, true).expect("the seed row lands");

        let retried = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&retried);
        // Handshake: the holder takes the row first, so the side's write
        // is guaranteed to be the one waiting.
        let (holder_ready_tx, holder_ready_rx) = std::sync::mpsc::channel::<()>();
        thread::scope(|scope| {
            scope.spawn(move || {
                let mut other = side(conninfo);
                holder_ready_rx
                    .recv_timeout(std::time::Duration::from_secs(2))
                    .expect("the holder takes the row");
                let updated = counter(1, 300);
                other.begin_transaction().expect("the transaction opens");
                // The 200ms budget is scoped to this attempt only (`SET
                // LOCAL` reverts when the transaction ends), so the
                // retried attempt below waits the way the reference's
                // would — as long as the write actually needs.
                other
                    .client_mut()
                    .execute("SET LOCAL lock_timeout = '200ms'", &[])
                    .expect("the attempt timeout is set");
                // The holder still owns the row: the write waits 200ms and
                // comes back as the retryable lock condition.
                let error = EntityStorage::put_entity(&mut other, &updated.0, &updated.1, false)
                    .expect_err("the timed-out write is retryable");
                assert_eq!(error, db::DbError::Retry);
                flag.store(true, Ordering::SeqCst);
                other
                    .rollback_transaction()
                    .expect("the timed-out group unwinds");
                // The retry: it waits again, and lands when the holder
                // commits.
                other.begin_transaction().expect("the retry opens");
                EntityStorage::put_entity(&mut other, &updated.0, &updated.1, false)
                    .expect("the retried write lands");
                other.commit_transaction().expect("the retry commits");
            });
            // Hold the row, long enough for the side's timeout and its
            // retry to be waiting, then let the retry through.
            let updated = counter(1, 200);
            storage.begin_transaction().expect("the transaction opens");
            EntityStorage::put_entity(storage, &updated.0, &updated.1, false)
                .expect("the holder's write applies");
            holder_ready_tx.send(()).expect("the handshake holds");
            thread::sleep(std::time::Duration::from_millis(500));
            storage.commit_transaction().expect("the holder commits");
        });

        assert!(retried.load(Ordering::SeqCst), "the side observed Retry");
        let mut reader = PostgresStorage::connect(conninfo).expect("the reader opens");
        register_counter(&mut reader, 4);
        assert_eq!(
            stored_level(&reader, &key),
            300,
            "the retried write is the last one committed"
        );
        assert_eq!(
            reader.all_entities().expect("the listing runs").len(),
            1,
            "the retry left no duplicate"
        );
    });
}

/// Two writers that take the same two rows in opposite orders run into
/// the reference's deadlock condition: the loser is told to retry, its
/// rolled-back group leaves nothing behind, and the retry lands — two
/// rows, no duplicates.
#[test]
fn a_deadlock_reports_retry_and_the_retries_leave_no_duplicates() {
    with_fresh_db(|storage, conninfo| {
        register_counter(storage, 4);
        let (first, values) = counter(1, 100);
        EntityStorage::put_entity(storage, &first, &values, true).expect("the first seed lands");
        let (second, values) = counter(2, 200);
        EntityStorage::put_entity(storage, &second, &values, true).expect("the second seed lands");

        // Handshake so both sides really hold their first row before
        // either asks for the second one: side one takes row 1, side two
        // takes row 2, then both ask for the other's row — a cycle the
        // database resolves by failing one side with the deadlock
        // condition.
        let (first_holds_tx, first_holds_rx) = std::sync::mpsc::channel::<()>();
        let (second_holds_tx, second_holds_rx) = std::sync::mpsc::channel::<()>();
        let first_retried = Arc::new(AtomicBool::new(false));
        let second_retried = Arc::new(AtomicBool::new(false));
        let first_done = Arc::new(AtomicBool::new(false));
        let second_done = Arc::new(AtomicBool::new(false));

        thread::scope(|scope| {
            let (f_retried, f_done) = (Arc::clone(&first_retried), Arc::clone(&first_done));
            scope.spawn(move || {
                let mut side_one = side(conninfo);
                side_one.begin_transaction().expect("the transaction opens");
                let (key, values) = counter(first.db_id, 105);
                EntityStorage::put_entity(&mut side_one, &key, &values, false)
                    .expect("side one takes row 1");
                first_holds_tx.send(()).expect("the handshake holds");
                second_holds_rx
                    .recv_timeout(std::time::Duration::from_secs(2))
                    .expect("side two takes row 2");
                // Asking for side two's row blocks; if the cycle resolves
                // against us, the whole group is retried.
                run_side(&mut side_one, [first, second], f_retried, f_done);
            });
            let (s_retried, s_done) = (Arc::clone(&second_retried), Arc::clone(&second_done));
            scope.spawn(move || {
                let mut side_two = side(conninfo);
                side_two.begin_transaction().expect("the transaction opens");
                let (key, values) = counter(second.db_id, 205);
                EntityStorage::put_entity(&mut side_two, &key, &values, false)
                    .expect("side two takes row 2");
                second_holds_tx.send(()).expect("the handshake holds");
                first_holds_rx
                    .recv_timeout(std::time::Duration::from_secs(2))
                    .expect("side one takes row 1");
                run_side(&mut side_two, [second, first], s_retried, s_done);
            });
        });

        assert!(
            first_retried.load(Ordering::SeqCst) || second_retried.load(Ordering::SeqCst),
            "one side must have been told to retry"
        );
        assert!(
            first_done.load(Ordering::SeqCst) && second_done.load(Ordering::SeqCst),
            "both sides landed within the retry budget"
        );
        let mut reader = PostgresStorage::connect(conninfo).expect("the reader opens");
        register_counter(&mut reader, 4);
        assert_eq!(
            reader.all_entities().expect("the listing runs").len(),
            2,
            "no duplicate record was left behind"
        );
    });
}

/// Two deleters of one entity run through the row lock too: the winner
/// removes the row, the loser — whose delete then matches nothing — is
/// told the entity is not found, and nothing is left duplicated.
#[test]
fn a_concurrent_delete_of_one_entity_reports_not_found_for_the_loser() {
    with_fresh_db(|storage, conninfo| {
        register_counter(storage, 4);
        let (key, values) = counter(1, 1);
        EntityStorage::put_entity(storage, &key, &values, true).expect("the seed row lands");

        let lost = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&lost);
        // Handshake: the winner deletes first, so the loser's delete is
        // guaranteed to be the one waiting.
        let (winner_ready_tx, winner_ready_rx) = std::sync::mpsc::channel::<()>();
        thread::scope(|scope| {
            scope.spawn(move || {
                winner_ready_rx
                    .recv_timeout(std::time::Duration::from_secs(2))
                    .expect("the winner takes the row");
                let mut other = side(conninfo);
                other.begin_transaction().expect("the transaction opens");
                let error =
                    EntityStorage::del_entity(&mut other, &key).expect_err("the loser misses");
                assert_eq!(error, db::DbError::NotFound);
                flag.store(true, Ordering::SeqCst);
                other
                    .rollback_transaction()
                    .expect("the empty group unwinds");
            });
            // Take the row first and hold it while the loser's delete
            // waits, then win.
            storage.begin_transaction().expect("the transaction opens");
            EntityStorage::del_entity(storage, &key).expect("the winner deletes");
            winner_ready_tx.send(()).expect("the handshake holds");
            thread::sleep(std::time::Duration::from_millis(500));
            storage.commit_transaction().expect("the winner commits");
        });

        assert!(lost.load(Ordering::SeqCst), "the loser observed NotFound");
        let mut reader = PostgresStorage::connect(conninfo).expect("the reader opens");
        register_counter(&mut reader, 4);
        assert!(
            reader.all_entities().expect("the listing runs").is_empty(),
            "the row is gone, exactly once"
        );
    });
}
