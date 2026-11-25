//! The connection-recovery acceptance case (BW-09-06-09): new requests
//! succeed once the database is reachable again.
//!
//! The reference's `MySql` wrapper keeps the connection string
//! (`connectInfo_`, `wrapper.hpp:111`) so a lost connection — the server
//! restarted, the socket died — comes back with a plain reconnect, and
//! the data the database kept survives the session it was written
//! through. The case kills this connection's server-side session
//! (the way a server restart drops every session), watches the next
//! request fail, reconnects, and watches the same requests succeed with
//! the data intact.

use std::sync::atomic::{AtomicU32, Ordering};

use db::{EntityKey, EntityStorage};
use db_postgres::{EntitySchema, PostgresStorage};
use entitydef::value::PropertyValue;
use foundation::ids::PENDING_DATABASE_ID;

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

/// Runs `body` with the connection string of a fresh private database,
/// or skips when no PostgreSQL server is reachable. The database is
/// dropped when the body returns.
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
    let result = PostgresStorage::connect(&conninfo).and_then(|mut setup| {
        db_postgres::schema_init::apply(setup.client_mut(), &db_postgres::schema_init::plan())?;
        drop(setup);
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

/// A dropped session comes back through [`PostgresStorage::reconnect`],
/// and everything the database kept — the committed rows, the identity
/// sequences — reads and allocates exactly as before.
#[test]
fn a_dropped_session_recovers_and_the_data_is_intact() {
    with_fresh_db(|conninfo| {
        let mut victim = PostgresStorage::connect(conninfo).unwrap();
        register(&mut victim);

        // A committed row and a logon record, written through the
        // connection that is about to die.
        EntityStorage::put_entity(&mut victim, &key(700), &agent_values("cui", 30), true).unwrap();
        let server_pid: i32 = victim
            .client_mut()
            .query_one("SELECT pg_backend_pid()", &[])
            .unwrap()
            .get(0);

        // Kill the session from a second connection: the same drop a
        // server restart gives every client.
        let mut admin = PostgresStorage::connect(conninfo).unwrap();
        admin
            .client_mut()
            .execute("SELECT pg_terminate_backend($1)", &[&server_pid])
            .unwrap();

        // The dead connection's next request fails.
        let err =
            EntityStorage::get_entity(&victim, &key(700)).expect_err("the dead connection fails");
        assert!(!err.to_string().is_empty(), "{err}");

        // The recovery: a plain reconnect on the stored string.
        victim.reconnect().expect("the reconnect works");

        // The new requests succeed, and the data the database kept is
        // exactly what was committed before the drop.
        let record = EntityStorage::get_entity(&victim, &key(700))
            .expect("the request works again")
            .expect("the committed row survived the drop");
        assert_eq!(record.values, agent_values("cui", 30));

        // The identity sequence is server state: it keeps going where it
        // was, never repeating an id across the reconnect.
        let fresh = EntityStorage::put_entity(
            &mut victim,
            &key(PENDING_DATABASE_ID),
            &agent_values("after", 1),
            false,
        )
        .expect("the create works again");
        assert_ne!(fresh, 700);
        assert_eq!(
            EntityStorage::get_entity(&victim, &key(fresh))
                .unwrap()
                .expect("the new row reads back")
                .values,
            agent_values("after", 1)
        );
    });
}

/// A reconnect onto a database that is not listening yet reports the
/// failure instead of pretending; the next reconnect after it is back
/// succeeds.
#[test]
fn a_reconnect_reports_the_database_being_unreachable() {
    with_fresh_db(|conninfo| {
        let mut storage = PostgresStorage::connect(conninfo).unwrap();
        register(&mut storage);

        // The same connection string pointed at a port nobody listens
        // on (libpq takes the last occurrence of a keyword): the same
        // "database is down" the recovery waits out, credentials and all.
        let down = format!("{conninfo} port=1");

        // A fresh connection onto the dark address fails with the
        // storage error the caller waits on.
        let err = PostgresStorage::connect(&down).expect_err("the dark address refuses");
        assert!(!err.to_string().is_empty(), "{err}");

        // The live connection keeps working: nothing about the failed
        // attempt touched it.
        EntityStorage::put_entity(&mut storage, &key(700), &agent_values("cui", 30), true)
            .expect("the live connection keeps working");
        assert!(
            EntityStorage::get_entity(&storage, &key(700))
                .unwrap()
                .is_some()
        );
    });
}
