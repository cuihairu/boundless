//! The transaction acceptance cases (BW-09-06-06, BW-09-06-07): a commit
//! publishes its writes to the other connections, a rollback keeps the
//! old values readable from an independent connection.
//!
//! Every database attempt in the reference runs inside a
//! `MySqlTransaction` (`db_storage_mysql/transaction.cpp`): the
//! constructor issues `START TRANSACTION`, and the destructor rolls the
//! work back unless `commit()` ran first — so a failed write is never
//! partly visible. The port maps that onto PostgreSQL's `BEGIN` /
//! `COMMIT` / `ROLLBACK` on the same connection.

use std::sync::atomic::{AtomicU32, Ordering};

use db::{BaseMailbox, DbError, EntityKey, EntityStorage};
use db_postgres::{EntitySchema, PostgresStorage};
use entitydef::value::PropertyValue;
use foundation::ids::PENDING_DATABASE_ID;
use mercury::endpoint::Address;

static TEST_DB_COUNTER: AtomicU32 = AtomicU32::new(0);

/// The libpq connection string for the admin connection, without a
/// database: `BOUNDLESS_PG_URL`, the CI storage service, or the local
/// server's unix socket.
fn admin_conninfo() -> Option<String> {
    if let Ok(url) = std::env::var("BOUND_PG_URL").or_else(|_| std::env::var("BOUNDLESS_PG_URL")) {
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
    let user = std::env::var("USER")
        .or_else(|_| std::env::var("LOGNAME"))
        .unwrap_or_else(|_| "postgres".to_string());
    Some(format!(
        "host=/var/run/postgresql dbname=postgres user={user}"
    ))
}

/// Runs `body` with the connection string of a fresh private database
/// (the fixed tables included), or skips when no PostgreSQL server is
/// reachable. The body opens its own connections — the cases need a
/// second, independent one to read through. The database is dropped when
/// the body returns.
fn with_fresh_db(body: impl FnOnce(&str)) {
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
    let result = PostgresStorage::connect(&conninfo).and_then(|mut first| {
        db_postgres::schema_init::apply(first.client_mut(), &db_postgres::schema_init::plan())?;
        drop(first);
        body(&conninfo);
        Ok(())
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

const AGENT_TYPE: u16 = 9;

fn agent_values(name: &str, level: i64) -> Vec<PropertyValue> {
    vec![
        PropertyValue::String(name.into()),
        PropertyValue::Int(level),
    ]
}

/// Registers the agent type on a connection: the table exists and the
/// connection resolves the shape.
fn register(storage: &mut PostgresStorage) {
    let schema =
        EntitySchema::for_record("Agent", &["name", "level"], &agent_values("a", 1)).unwrap();
    storage.create_entity_table(&schema).unwrap();
    storage.register_entity_type(AGENT_TYPE, &schema);
}

fn key(db_id: i64) -> EntityKey {
    EntityKey {
        type_id: AGENT_TYPE,
        db_id,
    }
}

fn mailbox(port: u16) -> BaseMailbox {
    BaseMailbox {
        addr: Address {
            ip: 0x0102_0304,
            port,
            salt: 1,
        },
        entity_id: 5,
    }
}

/// The commit publishes every write since the begin at once: the second
/// connection reads nothing while the transaction is open, and the whole
/// write afterwards.
#[test]
fn a_committed_write_reads_back_from_another_connection() {
    with_fresh_db(|conninfo| {
        let mut writer = PostgresStorage::connect(conninfo).unwrap();
        let mut observer = PostgresStorage::connect(conninfo).unwrap();
        register(&mut writer);
        register(&mut observer);

        writer.begin_transaction().expect("the begin works");
        let db_id = EntityStorage::put_entity(
            &mut writer,
            &EntityKey {
                type_id: AGENT_TYPE,
                db_id: PENDING_DATABASE_ID,
            },
            &agent_values("cui", 30),
            false,
        )
        .expect("the write lands in the transaction");

        // The independent connection reads none of it yet.
        assert_eq!(
            EntityStorage::get_entity(&observer, &key(db_id)).unwrap(),
            None,
            "an uncommitted write is invisible"
        );

        writer.commit().expect("the commit works");

        // Now the whole write is there: row and logon record.
        let record = EntityStorage::get_entity(&observer, &key(db_id))
            .unwrap()
            .expect("the committed row is visible");
        assert_eq!(record.values, agent_values("cui", 30));
    });
}

/// A rollback throws every write since the begin away: the independent
/// connection keeps reading the old values for the entity and its
/// associated logon record.
#[test]
fn a_failed_write_rolls_back_and_the_old_values_survive() {
    with_fresh_db(|conninfo| {
        let mut writer = PostgresStorage::connect(conninfo).unwrap();
        let mut observer = PostgresStorage::connect(conninfo).unwrap();
        register(&mut writer);
        register(&mut observer);

        // The old state, committed before the transaction: the row at
        // level 30, checked out by base 1.
        let db_id = EntityStorage::put_entity(
            &mut writer,
            &EntityKey {
                type_id: AGENT_TYPE,
                db_id: PENDING_DATABASE_ID,
            },
            &agent_values("cui", 30),
            false,
        )
        .unwrap();
        writer.check_out(&key(db_id), mailbox(1)).unwrap();
        let holder = EntityStorage::logon_of(&observer, &key(db_id)).unwrap();
        assert!(holder.is_some(), "the old checkout is committed");

        // The transaction: rewrite the row to level 31, move the
        // checkout, then fail a write mid-way — a duplicate explicit id
        // is a deterministic storage-contract error.
        writer.begin_transaction().expect("the begin works");
        EntityStorage::put_entity(&mut writer, &key(db_id), &agent_values("cui", 31), false)
            .expect("the update lands in the transaction");
        EntityStorage::put_entity(&mut writer, &key(db_id), &agent_values("moved", 1), true)
            .expect_err("the duplicate id fails the write");
        writer.rollback().expect("the rollback works");

        // The independent connection reads the old values, row and
        // checkout alike.
        let record = EntityStorage::get_entity(&observer, &key(db_id))
            .unwrap()
            .expect("the row survived");
        assert_eq!(record.values, agent_values("cui", 30));
        assert_eq!(
            EntityStorage::logon_of(&observer, &key(db_id)).unwrap(),
            holder,
            "the old checkout survived"
        );
    });
}

/// A clean rollback discards the transaction's writes the same way a
/// failed one does.
#[test]
fn a_clean_rollback_keeps_the_old_values() {
    with_fresh_db(|conninfo| {
        let mut writer = PostgresStorage::connect(conninfo).unwrap();
        let mut observer = PostgresStorage::connect(conninfo).unwrap();
        register(&mut writer);
        register(&mut observer);

        let db_id = EntityStorage::put_entity(
            &mut writer,
            &EntityKey {
                type_id: AGENT_TYPE,
                db_id: PENDING_DATABASE_ID,
            },
            &agent_values("cui", 30),
            false,
        )
        .unwrap();

        writer.begin_transaction().unwrap();
        EntityStorage::put_entity(&mut writer, &key(db_id), &agent_values("cui", 31), false)
            .unwrap();
        writer.rollback().unwrap();

        let record = EntityStorage::get_entity(&observer, &key(db_id))
            .unwrap()
            .unwrap();
        assert_eq!(record.values, agent_values("cui", 30));
    });
}

/// One transaction per connection: a second begin is refused, and
/// commit/rollback without an open transaction are refused too.
#[test]
fn the_transaction_flags_follow_the_reference() {
    with_fresh_db(|conninfo| {
        let mut writer = PostgresStorage::connect(conninfo).unwrap();
        register(&mut writer);

        let err = writer.rollback().expect_err("no transaction is open yet");
        assert!(err.to_string().contains("no transaction open"), "{err}");

        writer.begin_transaction().unwrap();
        let err = writer
            .begin_transaction()
            .expect_err("a second begin is refused");
        assert!(
            err.to_string().contains("already has a transaction open"),
            "{err}"
        );

        writer.commit().expect("the commit closes it");
        let err = writer.commit().expect_err("nothing is open now");
        assert!(err.to_string().contains("no transaction open"), "{err}");

        // The connection is usable again: a new begin works.
        writer.begin_transaction().unwrap();
        writer.rollback().unwrap();
    });
}

/// The failure messages are the contract's outcomes, not the server's:
/// the storage error for a refused transaction call states the face.
#[test]
fn the_refusals_are_storage_errors() {
    with_fresh_db(|conninfo| {
        let mut writer = PostgresStorage::connect(conninfo).unwrap();
        register(&mut writer);
        assert_eq!(
            writer.commit().unwrap_err(),
            DbError::Storage("the connection has no transaction open".into())
        );
        assert_eq!(
            writer.rollback().unwrap_err(),
            DbError::Storage("the connection has no transaction open".into())
        );
        writer.begin_transaction().unwrap();
        assert_eq!(
            writer.begin_transaction().unwrap_err(),
            DbError::Storage("the connection already has a transaction open".into())
        );
        writer.rollback().unwrap();
    });
}
