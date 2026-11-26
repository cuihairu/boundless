//! The PostgreSQL acceptance cases (BW-09-06-04): the DatabaseID allocation.
//!
//! The reference leaves the DBID to the entity table's `AUTO_INCREMENT`
//! key column — the buffered task path turns a `PENDING_DATABASE_ID` into
//! `0` before the insert, so the database hands the value out. In
//! PostgreSQL the same thing is the table's identity column, and
//! `put_new_entity` reads it back with `RETURNING "id"`.
//!
//! These cases run against a real PostgreSQL server. The connection
//! string comes from `BOUNDLESS_PG_URL`, or the CI storage service's
//! `CI_STORE_PG_*` settings, or a local server's unix socket; when no
//! server is reachable the cases skip with a notice, the same gate the
//! entitydef differential cases use. Every case gets its own private
//! database, dropped when the case finishes.

use std::sync::atomic::{AtomicU32, Ordering};
use std::thread;

use db::EntityRecord;
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

/// Runs `body` against a fresh private database, or skips when no
/// PostgreSQL server is reachable. `body` gets the first storage (the
/// caller creates the schema there) and the connection string so the body
/// can open further connections into the same database; the database is
/// dropped when the body returns.
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
    let result = PostgresStorage::connect(&conninfo).map(|mut storage| {
        body(&mut storage, &conninfo);
    });
    let drop = format!(
        "DROP DATABASE \"{}\" WITH (FORCE)",
        database.replace('"', "\"\"")
    );
    let _ = admin.client_mut().execute(drop.as_str(), &[]);
    if let Err(err) = result {
        panic!("the fresh database would not open: {err}");
    }
}

fn schema() -> EntitySchema {
    EntitySchema::for_record("Thing", &["name"], &[PropertyValue::String("x".into())]).unwrap()
}

/// A record with the reference's pending DatabaseID: the table has not
/// handed the entity one out.
fn pending_record() -> EntityRecord {
    let mut record = EntityRecord::new(0);
    record.db_id = -1;
    record.values = vec![PropertyValue::String("x".into())];
    record
}

/// The acceptance case: two clients creating entities at the same time
/// must not be given the same DatabaseID.
#[test]
fn concurrent_creations_get_distinct_ids() {
    with_fresh_db(|storage, conninfo| {
        let schema = schema();
        storage.create_entity_table(&schema).unwrap();

        let workers = 4;
        let per_worker = 25;
        let collected = std::sync::Mutex::new(Vec::new());
        thread::scope(|scope| {
            for _ in 0..workers {
                scope.spawn(|| {
                    let mut storage = PostgresStorage::connect(conninfo).unwrap();
                    let record = pending_record();
                    let worker_ids: Vec<i64> = (0..per_worker)
                        .map(|_| storage.put_new_entity(&schema, &record).unwrap())
                        .collect();
                    collected.lock().unwrap().extend(worker_ids);
                });
            }
        });
        let ids = collected.into_inner().unwrap();

        assert_eq!(ids.len(), workers * per_worker);
        let mut sorted = ids.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(
            sorted.len(),
            ids.len(),
            "two creations shared a database id: {ids:?}"
        );
        assert!(
            ids.iter().all(|id| *id > 0),
            "a creation got a non-positive database id: {ids:?}"
        );
    });
}

/// The acceptance case: the allocation keeps going after the storage is
/// gone and comes back — the reference's `AUTO_INCREMENT` never resets
/// and never hands a DatabaseID back.
#[test]
fn allocation_continues_after_a_restart() {
    with_fresh_db(|storage, conninfo| {
        let schema = schema();
        storage.create_entity_table(&schema).unwrap();
        // The storage goes away with the block: the "restart" is a fresh
        // connection into the same database.
        let first = {
            let storage = &mut *storage;
            storage.put_new_entity(&schema, &pending_record()).unwrap()
        };

        let mut restarted = PostgresStorage::connect(conninfo).unwrap();
        let second = restarted
            .put_new_entity(&schema, &pending_record())
            .unwrap();

        assert!(
            second > first,
            "the allocation restarted or reused: {first} then {second}"
        );
    });
}

/// An explicit DatabaseID (the row the caller already knows) still writes
/// over the identity column, and the identity keeps handing out its own
/// values afterwards.
#[test]
fn an_explicit_dbid_coexists_with_the_allocation() {
    with_fresh_db(|storage, _conninfo| {
        let schema = schema();
        storage.create_entity_table(&schema).unwrap();

        let allocated = storage.put_new_entity(&schema, &pending_record()).unwrap();

        let mut explicit = EntityRecord::new(0);
        explicit.db_id = 1_000_000;
        explicit.values = vec![PropertyValue::String("kept".into())];
        storage.put_entity(&schema, &explicit).unwrap();

        let next = storage.put_new_entity(&schema, &pending_record()).unwrap();

        let read_back = storage
            .get_entity(
                &schema,
                &db::EntityKey {
                    type_id: 0,
                    db_id: 1_000_000,
                },
            )
            .unwrap();
        assert!(
            read_back.is_some(),
            "the explicit database id was not stored as given"
        );
        assert!(
            next > allocated,
            "the identity stopped advancing: {allocated} then {next}"
        );
    });
}
