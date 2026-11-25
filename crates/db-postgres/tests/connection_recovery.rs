//! Recovering from a dropped database connection on the PostgreSQL
//! backend (BW-09-06-09, 验收 数据库恢复后新请求成功).
//!
//! The reference's `MySql::realQuery`
//! (`db_storage_mysql/wrapper.cpp:306-332`) re-runs a statement that died
//! on the connection — `CR_SERVER_LOST` / `CR_SERVER_GONE_ERROR` — on a
//! re-established connection, but never from inside a transaction, where
//! the loss is terminal (`transaction.cpp:41-47` skips the `ROLLBACK`
//! for the same reason). `MySql::reconnectTo` (wrapper.cpp:74-91)
//! re-connects with the stored connection string and `MySql::connect`
//! clears `hasLostConnection` on the new connection (wrapper.cpp:94-97);
//! `MySql::throwError` (wrapper.cpp:294-299) flags the connection
//! whenever a failure is connection-level
//! (`database_exception.cpp:43-53`, the four `CR_*` codes).
//!
//! The cases run against a real PostgreSQL server, with the connection
//! string from `BOUNDLESS_PG_URL`, or the CI storage service's
//! `CI_STORE_PG_*` settings, or a local server's unix socket; when no
//! server is reachable they skip with a notice. Every case gets its own
//! private database (the fixed tables included), dropped when the case
//! finishes. The dropped server link is reproduced by terminating the
//! case's own backend from a second connection.

use std::sync::atomic::{AtomicU32, Ordering};

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
/// the storage and a factory for independent connections into the same
/// database; the database is dropped when the body returns.
fn with_fresh_db(body: impl FnOnce(&mut PostgresStorage, &dyn Fn() -> PostgresStorage, &str)) {
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
    let connect = move || PostgresStorage::connect(&conninfo).expect("the fresh database opens");
    {
        let mut storage = connect();
        let fixed = db_postgres::schema_init::fixed_table_ddl();
        {
            let mut client = storage.client_mut();
            for statement in &fixed {
                client
                    .execute(statement, &[])
                    .expect("the fixed tables create");
            }
        }
        body(&mut storage, &connect, &database);
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

/// Terminates `storage`'s backend from an independent connection — the
/// dropped server link of the reference's `CR_SERVER_LOST`.
fn drop_backend(storage: &mut PostgresStorage, other: &mut PostgresStorage) {
    let pid: i32 = storage
        .client_mut()
        .query_one("SELECT pg_backend_pid()", &[])
        .expect("the backend pid is read")
        .get(0);
    let mut client = other.client_mut();
    client
        .execute("SELECT pg_terminate_backend($1)", &[&pid])
        .expect("the backend is terminated");
}

fn stored_level(storage: &PostgresStorage, key: &EntityKey) -> i64 {
    let found = EntityStorage::get_entity(storage, key)
        .expect("the read runs")
        .expect("the row is there");
    match &found.values[0] {
        PropertyValue::Int(value) => *value,
        other => panic!("the counter level is not an int: {other:?}"),
    }
}

/// 验收: after the database link dropped and the server recovered, a new
/// request succeeds — the statement that hit the lost connection is
/// re-established and re-run, on the same row, with no duplicate left
/// behind.
#[test]
fn a_new_request_succeeds_after_the_database_recovers() {
    with_fresh_db(|storage, connect, _database| {
        register_counter(storage, 4);
        let (key, values) = counter(1, 1);
        EntityStorage::put_entity(storage, &key, &values, true).expect("the seed row lands");

        let mut other = connect();
        drop_backend(storage, &mut other);

        // The statement that finds the connection dead: the reconnect leg
        // re-establishes the link and runs it once more.
        let (updated, values) = counter(1, 77);
        EntityStorage::put_entity(storage, &updated, &values, false).expect("the retried put");
        assert!(
            !storage.connection_lost(),
            "the fresh connection cleared the flag"
        );
        assert!(!storage.in_transaction(), "no transaction opened");

        let mut reader = connect();
        register_counter(&mut reader, 4);
        assert_eq!(
            stored_level(&reader, &updated),
            77,
            "the re-run statement landed on the original row"
        );
        assert_eq!(
            reader.all_entities().expect("the listing runs").len(),
            1,
            "the recovery left no duplicate"
        );
        let _ = key;
    });
}

/// A loss inside a transaction is terminal: the error propagates as the
/// contract's lost-connection outcome, the rollback still unwinds
/// quietly, and after an explicit recovery new requests go through.
#[test]
fn a_lost_connection_inside_a_transaction_is_terminal_until_recovered() {
    with_fresh_db(|storage, connect, _database| {
        register_counter(storage, 4);
        let (key, values) = counter(1, 1);
        EntityStorage::put_entity(storage, &key, &values, true).expect("the seed row lands");

        storage.begin_transaction().expect("the transaction opens");
        let mut other = connect();
        drop_backend(storage, &mut other);

        // Inside a transaction the reference never reconnects: the
        // aborted work cannot be re-sent on the new connection.
        let (updated, values) = counter(1, 90);
        let error = EntityStorage::put_entity(storage, &updated, &values, false)
            .expect_err("the loss is terminal inside a transaction");
        assert_eq!(error, db::DbError::LostConnection);
        assert!(
            storage.connection_lost(),
            "the connection is flagged as lost"
        );

        storage
            .rollback_transaction()
            .expect("the dead group unwinds quietly");
        assert!(!storage.in_transaction(), "the transaction closed anyway");

        // The recovery: a fresh connection serves new requests again.
        assert!(storage.reconnect(), "the reconnection answers");
        let (again, values) = counter(1, 300);
        EntityStorage::put_entity(storage, &again, &values, false)
            .expect("the request after the recovery");
        let mut reader = connect();
        register_counter(&mut reader, 4);
        assert_eq!(stored_level(&reader, &again), 300);
        assert!(!storage.connection_lost(), "the new connection is clean");
    });
}

/// The read face recovers the same way: a read that hits the dropped
/// link is re-established and re-run, and the log-on record it reports
/// is the one the fresh connection finds.
#[test]
fn a_read_that_hits_the_dropped_link_is_re_run_on_the_new_connection() {
    with_fresh_db(|storage, connect, _database| {
        register_counter(storage, 4);
        let (key, values) = counter(1, 1);
        EntityStorage::put_entity(storage, &key, &values, true).expect("the seed row lands");
        let held = db::BaseMailbox {
            addr: mercury::endpoint::Address {
                ip: 0x0100_000a,
                port: 30000,
                salt: 7,
            },
            entity_id: 1,
        };
        storage
            .put_logon_record(&key, &held)
            .expect("the log-on record is written");

        let mut other = connect();
        drop_backend(storage, &mut other);

        assert_eq!(stored_level(storage, &key), 1, "the read was re-run");
        assert_eq!(
            EntityStorage::logon_of(storage, &key).expect("the log-on read runs"),
            Some(held),
            "the log-on read was re-run too"
        );
        assert!(
            !storage.connection_lost(),
            "the fresh connection cleared the flag"
        );
    });
}

/// The reference leaves the original failure standing when the
/// re-establishment itself fails (wrapper.cpp:327-330), and the recovery
/// then works once the database is back: the database is dropped while
/// the case is connected, a new request comes back as the contract's
/// lost-connection outcome (no silent retry against the missing
/// database), and once it is recreated a fresh connection serves the
/// request — 验收 数据库恢复后新请求成功 in full.
#[test]
fn a_reconnect_that_cannot_re_establish_reports_lost_until_the_database_returns() {
    with_fresh_db(|storage, connect, database| {
        register_counter(storage, 4);
        let (key, values) = counter(1, 1);
        EntityStorage::put_entity(storage, &key, &values, true).expect("the seed row lands");

        // The database disappears while this connection lives: dropping
        // it terminates the live backend.
        let mut admin =
            PostgresStorage::connect(&admin_conninfo().expect("the admin connection string"))
                .expect("the admin connection opens");
        let drop = format!(
            "DROP DATABASE \"{}\" WITH (FORCE)",
            database.replace('"', "\"\"")
        );
        admin
            .client_mut()
            .execute(drop.as_str(), &[])
            .expect("the database drops");

        // The re-establishment fails: no database answers.
        let (updated, values) = counter(1, 55);
        let error = EntityStorage::put_entity(storage, &updated, &values, false)
            .expect_err("the failed re-establishment surfaces");
        assert_eq!(error, db::DbError::LostConnection);
        assert!(storage.connection_lost(), "the flag stays set");

        // The database comes back; a fresh connection serves the new
        // request.
        let create = format!("CREATE DATABASE \"{}\"", database.replace('"', "\"\""));
        admin
            .client_mut()
            .execute(create.as_str(), &[])
            .expect("the database returns");
        assert!(
            storage.reconnect(),
            "the reconnection answers once the database returns"
        );
        register_counter(storage, 4);
        // The explicit leg: the recreated table has no row yet, so the
        // update leg's existence check would answer "not found" — the
        // insert lands the request on the empty database.
        EntityStorage::put_entity(storage, &updated, &values, true)
            .expect("the request after the recovery lands");

        let mut reader = connect();
        register_counter(&mut reader, 4);
        assert_eq!(
            stored_level(&reader, &updated),
            55,
            "the recovered connection's write landed"
        );
        assert_eq!(
            reader.all_entities().expect("the listing runs").len(),
            1,
            "the recovery left no duplicate"
        );
    });
}
