//! Rolling a real transaction back on the PostgreSQL backend
//! (BW-09-06-07, 验收 写入中途返回错误时，实体和关联记录从独立连接读出旧值).
//!
//! The reference's `MySqlTransaction` destructor rolls back unless
//! `commit()` ran first (`lib/db_storage_mysql/transaction.cpp:29-51`),
//! skipping the `ROLLBACK` when the connection is already gone and
//! clearing the connection's `inTransaction` flag whatever happens. These
//! cases pin that face on a PostgreSQL server.
//!
//! The cases run against a real PostgreSQL server, with the connection
//! string from `BOUNDLESS_PG_URL`, or the CI storage service's
//! `CI_STORE_PG_*` settings, or a local server's unix socket; when no
//! server is reachable they skip with a notice. Every case gets its own
//! private database (the fixed tables included), dropped when the case
//! finishes.

use std::sync::atomic::{AtomicU32, Ordering};

use db::{BaseMailbox, EntityKey, EntityStorage};
use db_postgres::EntitySchema;
use db_postgres::PostgresStorage;
use entitydef::value::PropertyValue;
use foundation::ids::PENDING_DATABASE_ID;
use mercury::endpoint::Address;

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
fn with_fresh_db(body: impl FnOnce(&mut PostgresStorage, &dyn Fn() -> PostgresStorage)) {
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
        body(&mut storage, &connect);
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

/// Registers `type_id` with one String property and creates its table.
fn register_thing(storage: &mut PostgresStorage, type_id: u16) {
    let values = vec![PropertyValue::String("x".into())];
    let schema = EntitySchema::for_record("Thing", &["name"], &values).unwrap();
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

fn thing(key_db_id: i64, name: &str) -> (EntityKey, Vec<PropertyValue>) {
    (
        EntityKey {
            type_id: 3,
            db_id: key_db_id,
        },
        vec![PropertyValue::String(name.into())],
    )
}

fn mailbox(ip: u32, entity_id: i32) -> BaseMailbox {
    BaseMailbox {
        addr: Address {
            ip,
            port: 30000,
            salt: 7,
        },
        entity_id,
    }
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

fn stored_name(storage: &PostgresStorage, key: &EntityKey) -> String {
    let found = EntityStorage::get_entity(storage, key)
        .expect("the read runs")
        .expect("the row is there");
    match &found.values[0] {
        PropertyValue::String(name) => name.clone(),
        other => panic!("the thing name is not a string: {other:?}"),
    }
}

/// 验收: a write that returns an error in the middle of the transaction
/// leaves the entity *and its associated records* — here the entity's
/// log-on record — reading back their old values from an independent
/// connection.
#[test]
fn a_failed_write_leaves_the_entity_and_its_records_at_their_old_values() {
    with_fresh_db(|storage, connect| {
        register_counter(storage, 4);
        register_thing(storage, 3);
        let (key, values) = counter(1, 1);
        EntityStorage::put_entity(storage, &key, &values, true).expect("the seed counter lands");
        let (key, values) = thing(1, "a");
        EntityStorage::put_entity(storage, &key, &values, true).expect("the seed thing lands");
        let thing_key = EntityKey {
            type_id: 3,
            db_id: 1,
        };
        let held = mailbox(0x0100_000a, 1);
        storage
            .put_logon_record(&thing_key, &held)
            .expect("the log-on record is written");

        // The transaction: one write lands, then a duplicate explicit id
        // returns an error in the middle of it.
        storage.begin_transaction().expect("the transaction opens");
        let (key, values) = counter(1, 77);
        EntityStorage::put_entity(storage, &key, &values, false).expect("the first write applies");
        let duplicate = counter(1, 5);
        let error = EntityStorage::put_entity(storage, &duplicate.0, &duplicate.1, true)
            .expect_err("the duplicate id fails inside the transaction");
        assert_eq!(error, db::DbError::AlreadyExists);
        storage
            .rollback_transaction()
            .expect("the rollback withdraws the group");

        let mut reader = connect();
        register_counter(&mut reader, 4);
        register_thing(&mut reader, 3);
        let counter_key = EntityKey {
            type_id: 4,
            db_id: 1,
        };
        assert_eq!(
            stored_level(&reader, &counter_key),
            1,
            "the entity keeps its old value"
        );
        assert_eq!(
            stored_name(&reader, &thing_key),
            "a",
            "the related entity keeps its old value"
        );
        assert_eq!(
            reader.logon_of(&thing_key).expect("the log-on read runs"),
            Some(held),
            "the associated log-on record is untouched"
        );
        assert!(!storage.in_transaction(), "the transaction closed");
    });
}

/// An explicit rollback withdraws everything the transaction staged: an
/// independent connection never sees any of it.
#[test]
fn a_rollback_discards_everything_staged_before_it() {
    with_fresh_db(|storage, connect| {
        register_counter(storage, 4);
        let mut reader = connect();
        register_counter(&mut reader, 4);

        storage.begin_transaction().expect("the transaction opens");
        let (key, values) = counter(PENDING_DATABASE_ID, 9);
        EntityStorage::put_entity(storage, &key, &values, false)
            .expect("the staged create applies");
        let listed = reader.all_entities().expect("the reader lists");
        assert!(listed.is_empty(), "nothing is sent before the rollback");

        storage.rollback_transaction().expect("the rollback runs");
        assert!(
            reader.all_entities().expect("the reader lists").is_empty(),
            "a rolled-back write is not visible at all"
        );
        assert!(!storage.in_transaction(), "the transaction closed");
    });
}

/// The reference's destructor rolls back only a transaction it is inside,
/// so a rollback without one is refused rather than sent at random.
#[test]
fn rolling_back_without_a_transaction_is_refused() {
    with_fresh_db(|storage, _connect| {
        let err = storage
            .rollback_transaction()
            .expect_err("there is nothing to roll back");
        assert!(
            err.to_string().contains("no transaction to roll back"),
            "unexpected error: {err}"
        );
    });
}

/// A rollback that finds the connection already gone does not fail: the
/// failure is swallowed and the connection is flagged as lost
/// (`transaction.cpp:41-47`), with `inTransaction( false )` still applied.
#[test]
fn a_lost_connection_during_the_rollback_flags_the_connection() {
    with_fresh_db(|storage, connect| {
        register_counter(storage, 4);
        storage.begin_transaction().expect("the transaction opens");

        // Kill this connection from the outside, the way a dropped server
        // link would: another connection terminates our backend.
        let pid: i32 = storage
            .client_mut()
            .query_one("SELECT pg_backend_pid()", &[])
            .expect("the backend pid is read")
            .get(0);
        let mut other = connect();
        {
            let mut client = other.client_mut();
            client
                .execute("SELECT pg_terminate_backend($1)", &[&pid])
                .expect("the backend is terminated");
        }

        storage
            .rollback_transaction()
            .expect("a lost rollback is not an error");
        assert!(
            storage.connection_lost(),
            "the connection is flagged as lost"
        );
        assert!(!storage.in_transaction(), "the transaction closed anyway");
    });
}
