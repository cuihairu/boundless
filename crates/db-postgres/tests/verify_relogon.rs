//! Account re-logon session state on the PostgreSQL backend
//! (BW-09-06-10, 验收 会话状态与 MySQL 基准相符).
//!
//! The MySQL baseline for a re-logon is the session table
//! `bigworldLogOns`, written through these faces:
//!
//! * `addLogOnRecord` (`entity_type_mapping.cpp:773-783`) is `INSERT ...
//!   ON DUPLICATE KEY UPDATE` — "Add or update the log on record"
//!   (put_entity_task.cpp:110-112), so a re-logon *moves* the session to
//!   the new base mailbox instead of failing on the duplicate key.
//! * a straight check-out against a held entity answers "elsewhere"
//!   with the current holder — the session never moves by check-out
//!   alone; the move is the upsert's job.
//! * `getLogOnRecord` (get_entity_task.cpp:51-54) reports whether a base
//!   holds the entity and where — the absence after a sign-out is part
//!   of the state.
//! * `removeLogOnRecord` (entity_type_mapping.cpp:533-535) deletes
//!   unconditionally, and the entity-row write leg
//!   (put_entity_task.cpp:69-97 vs :99-112) never touches the session:
//!   the legs are independent.
//! * the account mapping `bigworldLogOnMapping` is keyed by the log-on
//!   name and is untouched by re-logons — a second account row with the
//!   same name is the contract's duplicate error, not a silent move.
//!
//! The cases run against a real PostgreSQL server, with the connection
//! string from `BOUNDLESS_PG_URL`, or the CI storage service's
//! `CI_STORE_PG_*` settings, or a local server's unix socket; when no
//! server is reachable they skip with a notice. Every case gets its own
//! private database (the fixed tables included), dropped when the case
//! finishes.

use std::sync::atomic::{AtomicU32, Ordering};

use db::{BaseMailbox, CheckOut, EntityKey, EntityStorage};
use db_postgres::{AccountRow, EntitySchema, PostgresStorage};
use entitydef::value::PropertyValue;
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

fn counter(key_db_id: i64, level: i64) -> (EntityKey, Vec<PropertyValue>) {
    (
        EntityKey {
            type_id: 4,
            db_id: key_db_id,
        },
        vec![PropertyValue::Int(level)],
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

fn account(name: &[u8], entity_id: i64) -> AccountRow {
    AccountRow {
        log_on_name: name.to_vec(),
        password: b"pw".to_vec(),
        entity_type: 4,
        entity_id,
    }
}

/// 验收: a re-logon moves the session to the new base mailbox — the
/// reference's add-or-update upsert — and every later check-out answers
/// with the moved-to holder, never the stale one.
#[test]
fn a_relogon_moves_the_session_to_the_new_base_mailbox() {
    with_fresh_db(|storage, _connect| {
        register_counter(storage, 4);
        let (key, values) = counter(1, 1);
        EntityStorage::put_entity(storage, &key, &values, true).expect("the seed row lands");

        let first = mailbox(0x0100_000a, 7);
        assert_eq!(
            EntityStorage::check_out(storage, &key, first.clone())
                .expect("the first log-on is granted"),
            CheckOut::Granted
        );
        assert_eq!(
            EntityStorage::logon_of(storage, &key).expect("the session read runs"),
            Some(first),
            "the session names the first holder"
        );

        // The re-logon: the same entity, a new base mailbox — the
        // add-or-update face moves the session.
        let second = mailbox(0x0100_000b, 9);
        storage
            .put_logon_record(&key, &second)
            .expect("the re-logon moves the session");
        assert_eq!(
            EntityStorage::logon_of(storage, &key).expect("the session read runs"),
            Some(second.clone()),
            "the session names the new holder"
        );

        // A check-out against the held entity answers with the *current*
        // holder, and the session did not move by the check-out itself.
        let third = mailbox(0x0100_000c, 11);
        assert_eq!(
            EntityStorage::check_out(storage, &key, third).expect("the held check-out answers"),
            CheckOut::Elsewhere(second.clone()),
            "the check-out reports the holder the re-logon installed"
        );
        assert_eq!(
            EntityStorage::logon_of(storage, &key).expect("the session read runs"),
            Some(second),
            "the refused check-out left the session alone"
        );
    });
}

/// A re-logon touches only the session: the account mapping and the
/// entity row counts stay at their baseline, and a second account with
/// the same log-on name is the contract's duplicate error, not a silent
/// move.
#[test]
fn a_relogon_leaves_the_mapping_and_the_rows_at_the_baseline() {
    with_fresh_db(|storage, connect| {
        register_counter(storage, 4);
        let (key, values) = counter(1, 1);
        EntityStorage::put_entity(storage, &key, &values, true).expect("the seed row lands");
        let mapping = account(b"hero", 1);
        storage
            .put_account(&mapping.clone())
            .expect("the account mapping is written");

        let first = mailbox(0x0100_000a, 7);
        assert_eq!(
            EntityStorage::check_out(storage, &key, first.clone())
                .expect("the first log-on is granted"),
            CheckOut::Granted
        );
        let second = mailbox(0x0100_000b, 9);
        storage
            .put_logon_record(&key, &second)
            .expect("the re-logon moves the session");

        // The account mapping survived the re-logon unchanged.
        let mut reader = connect();
        register_counter(&mut reader, 4);
        assert_eq!(
            reader.get_account(b"hero").expect("the mapping read runs"),
            Some(mapping.clone()),
            "the re-logon did not move the account mapping"
        );
        // A second account row with the same name is the duplicate
        // contract error, and the original mapping still stands.
        let moved = account(b"hero", 2);
        assert_eq!(
            reader
                .put_account(&moved)
                .expect_err("the duplicate name fails"),
            db::DbError::DuplicateAccount
        );
        assert_eq!(
            reader.get_account(b"hero").expect("the mapping read runs"),
            Some(mapping.clone()),
            "the duplicate attempt left the mapping alone"
        );

        // One entity row, one session holder.
        assert_eq!(
            reader.all_entities().expect("the listing runs").len(),
            1,
            "the re-logon left no duplicate row"
        );
        assert_eq!(
            EntityStorage::logon_of(storage, &key).expect("the session read runs"),
            Some(second),
            "exactly one session holder stands"
        );
    });
}

/// The session read reports whether a base holds the entity and where —
/// and after a sign-out it reports the absence, which a repeated
/// sign-out does not disturb (the removal deletes unconditionally).
#[test]
fn the_session_read_reports_the_base_or_its_absence() {
    with_fresh_db(|storage, _connect| {
        register_counter(storage, 4);
        let (key, values) = counter(1, 1);
        EntityStorage::put_entity(storage, &key, &values, true).expect("the seed row lands");

        assert_eq!(
            EntityStorage::logon_of(storage, &key).expect("the session read runs"),
            None,
            "no base holds the entity yet"
        );

        let holder = mailbox(0x0100_000a, 7);
        assert_eq!(
            EntityStorage::check_out(storage, &key, holder.clone()).expect("the log-on is granted"),
            CheckOut::Granted
        );
        assert_eq!(
            EntityStorage::logon_of(storage, &key).expect("the session read runs"),
            Some(holder),
            "the base location is the session"
        );

        EntityStorage::check_in(storage, &key).expect("the sign-out runs");
        assert_eq!(
            EntityStorage::logon_of(storage, &key).expect("the session read runs"),
            None,
            "the session is gone"
        );
        // The removal deletes unconditionally: a second sign-out is not
        // an error (entity_type_mapping.cpp:533-535).
        EntityStorage::check_in(storage, &key).expect("the repeated sign-out still runs");
        assert_eq!(
            EntityStorage::logon_of(storage, &key).expect("the session read runs"),
            None
        );
    });
}

/// The entity-row write leg never touches the session (the legs are
/// independent in `PutEntityTask`), and after a sign-out a fresh log-on
/// is granted again rather than reported elsewhere.
#[test]
fn a_row_write_leaves_the_session_alone_and_a_fresh_logon_grants() {
    with_fresh_db(|storage, _connect| {
        register_counter(storage, 4);
        let (key, values) = counter(1, 1);
        EntityStorage::put_entity(storage, &key, &values, true).expect("the seed row lands");

        let first = mailbox(0x0100_000a, 7);
        assert_eq!(
            EntityStorage::check_out(storage, &key, first.clone()).expect("the log-on is granted"),
            CheckOut::Granted
        );

        // The write leg: the entity row updates, the session record does
        // not move and does not duplicate.
        let (updated, values) = counter(1, 42);
        EntityStorage::put_entity(storage, &updated, &values, false)
            .expect("the entity write lands");
        assert_eq!(
            EntityStorage::logon_of(storage, &key).expect("the session read runs"),
            Some(first),
            "the entity write left the session alone"
        );

        // After the sign-out a fresh log-on is granted.
        EntityStorage::check_in(storage, &key).expect("the sign-out runs");
        let second = mailbox(0x0100_000b, 9);
        assert_eq!(
            EntityStorage::check_out(storage, &key, second.clone()).expect("the fresh log-on runs"),
            CheckOut::Granted,
            "a signed-out entity grants a fresh log-on"
        );
    });
}
