//! Committing a real transaction on the PostgreSQL backend
//! (BW-09-06-06, 验收 已提交的实体更新可从另一连接读回).
//!
//! Every database attempt in the reference runs inside a `MySqlTransaction`
//! (`lib/db_storage_mysql/transaction.cpp`): the constructor issues
//! `START TRANSACTION`, `commit()` sends `COMMIT`, and a destructor that
//! never committed sends `ROLLBACK` — so no write is ever partly visible.
//! These cases pin the commit half of that face on a PostgreSQL server.
//!
//! The cases run against a real PostgreSQL server, with the connection
//! string from `BOUNDLESS_PG_URL`, or the CI storage service's
//! `CI_STORE_PG_*` settings, or a local server's unix socket; when no
//! server is reachable they skip with a notice. Every case gets its own
//! private database (the fixed tables included), dropped when the case
//! finishes.

use std::sync::atomic::{AtomicU32, Ordering};

use db::{EntityKey, EntityStorage};
use db_postgres::EntitySchema;
use db_postgres::PostgresStorage;
use entitydef::value::PropertyValue;
use foundation::ids::PENDING_DATABASE_ID;

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

fn register_thing(storage: &mut PostgresStorage, type_id: u16) {
    let values = vec![PropertyValue::String("x".into())];
    let schema = EntitySchema::for_record("Thing", &["name"], &values).unwrap();
    storage.create_entity_table(&schema).unwrap();
    storage.register_entity_type(type_id, &schema);
}

fn level(key_db_id: i64, value: i64) -> (EntityKey, Vec<PropertyValue>) {
    (
        EntityKey {
            type_id: 4,
            db_id: key_db_id,
        },
        vec![PropertyValue::Int(value)],
    )
}

fn name(key_db_id: i64, value: &str) -> (EntityKey, Vec<PropertyValue>) {
    (
        EntityKey {
            type_id: 3,
            db_id: key_db_id,
        },
        vec![PropertyValue::String(value.into())],
    )
}

/// Reads the stored level of a counter entity.
fn read_level(storage: &PostgresStorage, key: &EntityKey) -> i64 {
    let found = EntityStorage::get_entity(storage, key)
        .expect("the read runs")
        .expect("the row is there");
    match &found.values[0] {
        PropertyValue::Int(value) => *value,
        other => panic!("the counter level is not an int: {other:?}"),
    }
}

/// 验收: an entity update committed through one connection is read back,
/// with the new value, from an independent connection.
#[test]
fn a_committed_entity_update_is_read_back_from_another_connection() {
    with_fresh_db(|storage, connect| {
        register_counter(storage, 4);
        let (key, values) = level(PENDING_DATABASE_ID, 1);
        let first =
            EntityStorage::put_entity(storage, &key, &values, false).expect("the seed write lands");

        let updated = level(first, 99);
        storage.begin_transaction().expect("the transaction opens");
        assert!(storage.in_transaction(), "the connection is inside it");
        EntityStorage::put_entity(storage, &updated.0, &updated.1, false)
            .expect("the staged update applies inside the transaction");
        storage.commit_transaction().expect("the commit runs");
        assert!(!storage.in_transaction(), "the transaction closed");

        let mut reader = connect();
        register_counter(&mut reader, 4);
        let committed = EntityKey {
            type_id: 4,
            db_id: first,
        };
        assert_eq!(
            read_level(&reader, &committed),
            99,
            "another connection reads the committed value"
        );
    });
}

/// An independent connection keeps the old value until the commit lands,
/// then sees the new one: nothing is partly visible.
#[test]
fn another_connection_keeps_the_old_value_until_the_commit() {
    with_fresh_db(|storage, connect| {
        register_counter(storage, 4);
        let (key, values) = level(PENDING_DATABASE_ID, 1);
        let first =
            EntityStorage::put_entity(storage, &key, &values, false).expect("the seed write lands");
        let mut reader = connect();
        register_counter(&mut reader, 4);

        let updated = level(first, 55);
        storage.begin_transaction().expect("the transaction opens");
        EntityStorage::put_entity(storage, &updated.0, &updated.1, false)
            .expect("the staged update applies");
        assert_eq!(
            read_level(&reader, &updated.0),
            1,
            "an uncommitted update is not visible"
        );

        storage.commit_transaction().expect("the commit runs");
        assert_eq!(
            read_level(&reader, &updated.0),
            55,
            "the committed update is visible"
        );
    });
}

/// A group of writes made in one transaction becomes visible as a group:
/// nothing of it is listed before the commit.
#[test]
fn a_group_of_writes_becomes_visible_together_with_the_commit() {
    with_fresh_db(|storage, connect| {
        register_thing(storage, 3);
        register_counter(storage, 4);
        let mut reader = connect();
        register_thing(&mut reader, 3);
        register_counter(&mut reader, 4);

        assert!(
            reader
                .all_entities()
                .expect("a fresh database lists nothing")
                .is_empty()
        );

        storage.begin_transaction().expect("the transaction opens");
        let (key, values) = name(PENDING_DATABASE_ID, "inside");
        EntityStorage::put_entity(storage, &key, &values, false)
            .expect("the first staged write applies");
        let (key, values) = level(PENDING_DATABASE_ID, 2);
        EntityStorage::put_entity(storage, &key, &values, false)
            .expect("the second staged write applies");
        assert!(
            reader
                .all_entities()
                .expect("the group is not sent yet")
                .is_empty(),
            "nothing of the group is listed before the commit"
        );

        storage.commit_transaction().expect("the commit runs");
        let listed = reader.all_entities().expect("the group is listed");
        assert_eq!(listed.len(), 2, "the whole group is visible");
    });
}

/// The port guards the transaction state the reference keeps on the
/// connection: a second begin inside one fails, and a commit without an
/// open transaction fails rather than sending `COMMIT` at random.
#[test]
fn the_transaction_state_is_guarded_at_both_ends() {
    with_fresh_db(|storage, _connect| {
        storage.begin_transaction().expect("the transaction opens");
        let err = storage
            .begin_transaction()
            .expect_err("a second begin is refused");
        assert!(
            err.to_string().contains("already has a transaction open"),
            "unexpected error: {err}"
        );

        storage.commit_transaction().expect("the commit runs");
        let err = storage
            .commit_transaction()
            .expect_err("a commit without a transaction is refused");
        assert!(
            err.to_string().contains("no transaction to commit"),
            "unexpected error: {err}"
        );
    });
}
