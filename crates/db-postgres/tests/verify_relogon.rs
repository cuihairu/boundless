//! The relogon acceptance case (BW-09-06-10): the session state in
//! `bigworldLogOns` matches what the reference's MySQL backend keeps.
//!
//! The MySQL baseline (`entity_type_mapping.cpp`): `addLogOnRecord` is
//! `INSERT ... ON DUPLICATE KEY UPDATE`, so a relogon overwrites the
//! holder's whole record — one row per `(typeID, databaseID)`, the
//! address stored `htonl`/`htons` (`addLogOnRecord`, :779-781), the salt
//! whole; a refused relogon (`DBApp::onLogOnLoggedOnUser`,
//! `dbapp.cpp:2153-2184`) leaves the standing holder's record alone; a
//! released account reads back as free and takes a new holder cleanly.

use std::sync::atomic::{AtomicU32, Ordering};

use db::{BaseMailbox, CheckOut, EntityKey, EntityStorage};
use db_postgres::{EntitySchema, PostgresStorage};
use entitydef::value::PropertyValue;
use foundation::ids::PENDING_DATABASE_ID;
use mercury::endpoint::Address;

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
    let user = std::env::var("USER")
        .or_else(|_| std::env::var("LOGNAME"))
        .unwrap_or_else(|_| "postgres".to_string());
    Some(format!(
        "host=/var/run/postgresql dbname=postgres user={user}"
    ))
}

/// Runs `body` against a fresh private database (the fixed tables and
/// the entity table included), or skips when no PostgreSQL server is
/// reachable. The database is dropped when the body returns.
fn with_fresh_db(body: impl FnOnce(&mut PostgresStorage)) {
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
        db_postgres::schema_init::apply(storage.client_mut(), &db_postgres::schema_init::plan())
            .unwrap();
        register(&mut storage);
        body(&mut storage);
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

fn agent_values(name: &str) -> Vec<PropertyValue> {
    vec![PropertyValue::String(name.into()), PropertyValue::Int(1)]
}

/// Registers the agent type on a connection.
fn register(storage: &mut PostgresStorage) {
    let schema = EntitySchema::for_record("Agent", &["name", "level"], &agent_values("a")).unwrap();
    storage.create_entity_table(&schema).unwrap();
    storage.register_entity_type(AGENT_TYPE, &schema);
}

fn key(db_id: i64) -> EntityKey {
    EntityKey {
        type_id: AGENT_TYPE,
        db_id,
    }
}

fn mailbox(ip: u32, port: u16, salt: u16, entity_id: i32) -> BaseMailbox {
    BaseMailbox {
        addr: Address { ip, port, salt },
        entity_id,
    }
}

/// A relogon overwrites the whole record the way the reference's
/// `ON DUPLICATE KEY UPDATE` does: still one row, every column of the
/// new holder, the address stored in network byte order.
#[test]
fn a_relogon_overwrites_the_record_like_the_mysql_upsert() {
    with_fresh_db(|storage| {
        let db_id = EntityStorage::put_entity(
            storage,
            &key(PENDING_DATABASE_ID),
            &agent_values("cui"),
            false,
        )
        .unwrap();
        let key = key(db_id);

        // The first session.
        storage
            .check_out(&key, mailbox(0x0102_0304, 0x1234, 1, 10))
            .unwrap();

        // The relogon goes through the raw record face — the same
        // statement shape the reference's addLogOnRecord is.
        storage
            .put_logon_record(&key, &mailbox(0x0506_0708, 0x5678, 2, 11))
            .expect("the relogon overwrites");

        // One row per entity, every column the new holder's.
        let row = storage
            .client_mut()
            .query_one(
                "SELECT \"databaseID\", \"typeID\", \"objectID\", \"ip\", \"port\", \"salt\" \
                 FROM \"bigworldLogOns\" WHERE \"databaseID\" = $1 AND \"typeID\" = $2",
                &[&db_id, &i32::from(AGENT_TYPE)],
            )
            .unwrap();
        let database_id: i64 = row.get(0);
        let type_id: i32 = row.get(1);
        let object_id: i32 = row.get(2);
        let ip: i64 = row.get(3);
        let port: i32 = row.get(4);
        let salt: i32 = row.get(5);
        assert_eq!(database_id, db_id);
        assert_eq!(type_id, i32::from(AGENT_TYPE));
        assert_eq!(object_id, 11);
        // htonl/htons: the stored bytes are the swapped address.
        assert_eq!(ip as u32, 0x0506_0708u32.swap_bytes());
        assert_eq!(port as u16, 0x5678u16.swap_bytes());
        assert_eq!(salt, 2);

        // The contract read gives the new holder back.
        let holder = EntityStorage::logon_of(storage, &key)
            .unwrap()
            .expect("the new holder reads back");
        assert_eq!(holder.addr.ip, 0x0506_0708);
        assert_eq!(holder.addr.port, 0x5678);
        assert_eq!(holder.addr.salt, 2);
        assert_eq!(holder.entity_id, 11);

        // And still exactly one row for the entity.
        let count: i64 = storage
            .client_mut()
            .query_one("SELECT COUNT(*) FROM \"bigworldLogOns\"", &[])
            .unwrap()
            .get(0);
        assert_eq!(count, 1);
    });
}

/// A refused relogon — the entity is checked out elsewhere — leaves the
/// standing holder's session state untouched.
#[test]
fn a_refused_relogon_leaves_the_standing_holder() {
    with_fresh_db(|storage| {
        let db_id = EntityStorage::put_entity(
            storage,
            &key(PENDING_DATABASE_ID),
            &agent_values("cui"),
            false,
        )
        .unwrap();
        let key = key(db_id);
        let standing = mailbox(0x0102_0304, 0x1234, 1, 10);
        assert_eq!(
            EntityStorage::check_out(storage, &key, standing.clone()).unwrap(),
            CheckOut::Granted
        );

        // The second base's logon attempt is the Elsewhere outcome.
        let outcome = EntityStorage::check_out(storage, &key, mailbox(9, 9, 9, 99)).unwrap();
        assert_eq!(outcome, CheckOut::Elsewhere(standing.clone()));

        // The session state is still the standing holder's, whole.
        assert_eq!(
            EntityStorage::logon_of(storage, &key).unwrap(),
            Some(standing)
        );
    });
}

/// A released account reads back as free and takes a new holder cleanly
/// — the logoff-logon cycle the reference's removeLogOnRecord /
/// addLogOnRecord pair gives.
#[test]
fn a_released_account_relogs_cleanly() {
    with_fresh_db(|storage| {
        let db_id = EntityStorage::put_entity(
            storage,
            &key(PENDING_DATABASE_ID),
            &agent_values("cui"),
            false,
        )
        .unwrap();
        let key = key(db_id);

        EntityStorage::check_out(storage, &key, mailbox(1, 2, 3, 10)).unwrap();
        EntityStorage::check_in(storage, &key).unwrap();
        assert_eq!(EntityStorage::logon_of(storage, &key).unwrap(), None);

        // The relogon after the logoff is a fresh grant.
        let newcomer = mailbox(0x0a0b_0c0d, 0x9abc, 4, 20);
        assert_eq!(
            EntityStorage::check_out(storage, &key, newcomer.clone()).unwrap(),
            CheckOut::Granted
        );
        assert_eq!(
            EntityStorage::logon_of(storage, &key).unwrap(),
            Some(newcomer)
        );
    });
}
