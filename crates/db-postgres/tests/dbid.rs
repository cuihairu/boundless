//! The DBID allocation acceptance cases (BW-09-06-04): a create gets its
//! id from the table itself, and concurrent creates never repeat one.
//!
//! The reference decides this in the database, not in C++: `putEntity`'s
//! create leg takes `insertNew` (`tasks/put_entity_task.cpp:60-98`), the
//! statement omits the id column (`utils.cpp:48-76`), the column is the
//! table's `AUTO_INCREMENT` (`column_type.hpp:144-146`,
//! `mysql_table_initialiser.cpp:366-369`), and the caller reads the DB's
//! choice back with `LAST_INSERT_ID` (`wrapper.hpp:75`). There is no
//! watermark table and no id lock — the sequence is the guarantee.
//!
//! The same server gate as `round_trip.rs`: `BOUNDLESS_PG_URL`, the CI
//! storage service's `CI_STORE_PG_*`, or a local unix socket; a private
//! database per case, dropped when the case finishes.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use db::DbError;
use db_postgres::{EntitySchema, PostgresStorage};
use entitydef::value::PropertyValue;

static TEST_DB_COUNTER: AtomicU32 = AtomicU32::new(0);

/// The libpq connection string for the admin connection, without a
/// database: `BOUNDLESS_PG_URL`, the CI storage service, or the local
/// server's unix socket.
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
    // A local server's unix socket: peer authentication, the OS account's
    // own role.
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

/// Runs `body` against a fresh private database, handing it the connection
/// string the body can use to open more sessions (a reconnect, a second
/// writer) and one storage already connected. The database is dropped
/// when the body returns.
fn with_fresh_db_conninfo(body: impl FnOnce(&str, &mut PostgresStorage)) {
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
        "boundless_pg_dbid_{}_{}",
        std::process::id(),
        TEST_DB_COUNTER.fetch_add(1, Ordering::SeqCst)
    );
    let quoted = database.replace('"', "\"\"");
    if let Err(err) = admin
        .client_mut()
        .execute(format!("CREATE DATABASE \"{quoted}\"").as_str(), &[])
    {
        eprintln!("skipping: cannot create the test database ({err})");
        return;
    }
    let conninfo = format!("{admin_conninfo} dbname={database}");
    let result = PostgresStorage::connect(&conninfo).map(|mut storage| {
        body(&conninfo, &mut storage);
    });
    let _ = admin.client_mut().execute(
        format!("DROP DATABASE \"{quoted}\" WITH (FORCE)").as_str(),
        &[],
    );
    if let Err(err) = result {
        panic!("the fresh database would not open: {err}");
    }
}

/// Runs `body` against a fresh private database on its own storage.
fn with_fresh_db(body: impl FnOnce(&mut PostgresStorage)) {
    with_fresh_db_conninfo(|_conninfo, storage| body(storage));
}

fn account_schema() -> EntitySchema {
    EntitySchema::for_record(
        "Account",
        &["name", "level"],
        &[PropertyValue::String("cui".into()), PropertyValue::Int(1)],
    )
    .unwrap()
}

fn values() -> Vec<PropertyValue> {
    vec![PropertyValue::String("cui".into()), PropertyValue::Int(1)]
}

fn record(db_id: i64) -> db::EntityRecord {
    let mut record = db::EntityRecord::new(0);
    record.db_id = db_id;
    record.values = values();
    record
}

/// The acceptance: many creates at once, from sessions that share nothing,
/// hand out distinct ids — the sequence advances atomically, and the
/// primary key would have refused a repeat.
#[test]
fn concurrent_creations_never_repeat_an_id() {
    with_fresh_db_conninfo(|conninfo, storage| {
        let schema = account_schema();
        storage.create_entity_table(&schema).unwrap();

        let workers = 8;
        let per_worker = 6;
        let shared = Arc::new(schema);
        let collected = Arc::new(Mutex::new(Vec::<i64>::new()));
        let mut threads = Vec::new();
        for worker in 0..workers {
            let shared = Arc::clone(&shared);
            let collected = Arc::clone(&collected);
            let conninfo = conninfo.to_string();
            threads.push(std::thread::spawn(move || {
                let mut worker_storage = PostgresStorage::connect(&conninfo)
                    .unwrap_or_else(|err| panic!("worker {worker} would not connect: {err}"));
                for _ in 0..per_worker {
                    let id = worker_storage
                        .create_entity(&shared, &values())
                        .unwrap_or_else(|err| panic!("worker {worker} create failed: {err}"));
                    assert!(id > 0, "worker {worker} got a non-positive id {id}");
                    collected.lock().unwrap().push(id);
                }
            }));
        }
        for thread in threads {
            thread.join().unwrap();
        }

        let ids = collected.lock().unwrap();
        let expected = workers * per_worker;
        assert_eq!(ids.len(), expected);
        assert_eq!(
            ids.len(),
            ids.iter().collect::<std::collections::HashSet<_>>().len()
        );

        // Every allocated id is a row, and nothing else was written.
        let rows = storage
            .client_mut()
            .query_one("SELECT count(*) FROM \"tbl_Account\"", &[])
            .unwrap()
            .get::<_, i64>(0);
        assert_eq!(rows as usize, expected);
    });
}

/// Allocation is table state, not session state: a connection that
/// reconnects to the database continues where the table left off instead
/// of reusing the ids it handed out.
#[test]
fn allocation_continues_after_a_reconnect() {
    with_fresh_db_conninfo(|conninfo, storage| {
        let schema = account_schema();
        storage.create_entity_table(&schema).unwrap();

        let mut handed_out = Vec::new();
        for _ in 0..3 {
            let id = storage.create_entity(&schema, &values()).unwrap();
            handed_out.push(id);
        }
        assert_eq!(handed_out, vec![1, 2, 3]);

        // A restarted DBApp opens a new connection to the same database:
        // the next id is past everything the table has already been given.
        let mut restarted = PostgresStorage::connect(conninfo).unwrap();
        let next = restarted.create_entity(&schema, &values()).unwrap();
        assert_eq!(next, 4, "allocation restarted instead of continuing");
        let after = restarted.create_entity(&schema, &values()).unwrap();
        assert_eq!(after, 5);
    });
}

/// A created id is the row's key: the row reads back under it.
#[test]
fn a_created_id_is_the_row_key() {
    with_fresh_db(|storage| {
        let schema = account_schema();
        storage.create_entity_table(&schema).unwrap();

        let id = storage.create_entity(&schema, &values()).unwrap();
        let back = storage
            .get_entity(
                &schema,
                &db::EntityKey {
                    type_id: 0,
                    db_id: id,
                },
            )
            .unwrap()
            .unwrap_or_else(|| panic!("row for the allocated id {id} is missing"));
        assert_eq!(back.db_id, id);
        assert_eq!(back.values, values());
    });
}

/// The reference has no id watermark: explicit ids do not advance the
/// table's sequence, so a create that runs into one of them is the
/// contract's duplicate. Recalibrating an imported table is
/// BW-09-07-06's job.
#[test]
fn an_explicit_id_does_not_advance_the_sequence() {
    with_fresh_db(|storage| {
        let schema = account_schema();
        storage.create_entity_table(&schema).unwrap();

        storage.put_entity(&schema, &record(1)).unwrap();
        let err = storage.create_entity(&schema, &values()).unwrap_err();
        assert_eq!(err, DbError::AlreadyExists);

        // The explicit row is untouched, and it is what the create ran
        // into.
        let back = storage
            .get_entity(
                &schema,
                &db::EntityKey {
                    type_id: 0,
                    db_id: 1,
                },
            )
            .unwrap()
            .unwrap();
        assert_eq!(back.values, values());
    });
}

/// The reference's `AUTO_INCREMENT` is per table (`EntityKey` pairs a
/// type with an id), so two entity tables each allocate from one.
#[test]
fn each_entity_table_allocates_its_own_ids() {
    with_fresh_db(|storage| {
        let account = account_schema();
        storage.create_entity_table(&account).unwrap();
        let monster =
            EntitySchema::for_record("Monster", &["name"], &[PropertyValue::String("cui".into())])
                .unwrap();
        storage.create_entity_table(&monster).unwrap();

        assert_eq!(storage.create_entity(&account, &values()).unwrap(), 1);
        assert_eq!(
            storage
                .create_entity(&monster, &[PropertyValue::String("cui".into())])
                .unwrap(),
            1,
            "the two tables share an allocation"
        );
        assert_eq!(storage.create_entity(&account, &values()).unwrap(), 2);
    });
}
