//! The concurrent-write acceptance case (BW-09-06-08): two requests
//! writing the same entity answer per the storage contract, and a retry
//! afterwards leaves no duplicate record.
//!
//! The reference serialises the writers in the DBMS — one `insertExplicit`
//! wins the id's unique key and the other comes back a contract error
//! (`put_entity_task.cpp:93-97`) — and the reference's own retry
//! machinery (`RetryTask`) re-runs the refused request. The port runs
//! two connections through a barrier so the writes really overlap, then
//! checks the outcomes and the row count.

use std::sync::Arc;
use std::sync::Barrier;
use std::sync::atomic::{AtomicU32, Ordering};

use db::{DbError, EntityKey, EntityStorage};
use db_postgres::{EntitySchema, PostgresStorage};
use entitydef::value::PropertyValue;
use foundation::ids::DatabaseId;
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

/// Runs `body` with the connection string of a fresh private database
/// (the fixed tables and the entity table included), or skips when no
/// PostgreSQL server is reachable. The body opens its own connections —
/// the cases need several to race. The database is dropped when the body
/// returns.
fn with_race_db(body: impl FnOnce(&str)) {
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
        register(&mut setup);
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

fn agent_values(name: &str) -> Vec<PropertyValue> {
    vec![PropertyValue::String(name.into()), PropertyValue::Int(1)]
}

/// Registers the agent type on a connection.
fn register(storage: &mut PostgresStorage) {
    let schema = EntitySchema::for_record("Agent", &["name", "level"], &agent_values("a")).unwrap();
    storage.create_entity_table(&schema).unwrap();
    storage.register_entity_type(AGENT_TYPE, &schema);
}

fn key(db_id: DatabaseId) -> EntityKey {
    EntityKey {
        type_id: AGENT_TYPE,
        db_id,
    }
}

/// One connection's racer: connect, register, wait for the barrier, then
/// claim the explicit id.
fn racer(conninfo: &str, barrier: &Arc<Barrier>) -> Result<DatabaseId, DbError> {
    let mut storage = PostgresStorage::connect(conninfo).expect("the racer connects");
    register(&mut storage);
    barrier.wait();
    EntityStorage::put_entity(&mut storage, &key(700), &agent_values("racer"), true)
}

/// Two connections race the same explicit id: exactly one write wins and
/// the other is the contract's duplicate outcome; the retry afterwards
/// leaves one row per id, never two under the same id.
#[test]
fn two_racing_writes_answer_the_contract_and_a_retry_leaves_no_duplicate() {
    with_race_db(|conninfo| {
        let barrier = Arc::new(Barrier::new(2));
        let left = {
            let conninfo = conninfo.to_string();
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || racer(&conninfo, &barrier))
        };
        let right = {
            let conninfo = conninfo.to_string();
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || racer(&conninfo, &barrier))
        };
        let left = left.join().expect("the left racer returns");
        let right = right.join().expect("the right racer returns");

        // Exactly one winner; the loser carries the contract's duplicate.
        let loser = match (&left, &right) {
            (Ok(_), Err(err)) | (Err(err), Ok(_)) => err.clone(),
            _ => panic!("expected one winner and one contract error: {left:?} {right:?}"),
        };
        let winner: DatabaseId = left.unwrap_or(700);
        let other: DatabaseId = right.unwrap_or(700);
        assert_eq!(winner, 700);
        assert_eq!(other, 700);
        assert_eq!(loser, DbError::AlreadyExists);

        // The row count for the id is exactly one — the loser's write
        // never landed a second row.
        let mut observer = PostgresStorage::connect(conninfo).unwrap();
        register(&mut observer);
        let count: i64 = observer
            .client_mut()
            .query_one("SELECT COUNT(*) FROM \"tbl_Agent\" WHERE \"id\" = 700", &[])
            .unwrap()
            .get(0);
        assert_eq!(count, 1, "no duplicate record for the id");

        // The retry: the refused writer comes back as a create, which
        // allocates its own id — a new record, not a duplicate of 700.
        let mut retried = PostgresStorage::connect(conninfo).unwrap();
        register(&mut retried);
        let retry_id = EntityStorage::put_entity(
            &mut retried,
            &key(PENDING_DATABASE_ID),
            &agent_values("retried"),
            false,
        )
        .expect("the retry lands");
        assert_ne!(retry_id, 700);

        // The retry's own id is the sequence's, which starts below the
        // explicit 700 — match by key, not by order.
        let all = EntityStorage::all_entities(&observer).unwrap();
        assert_eq!(all.len(), 2);
        for record in &all {
            if record.db_id == 700 {
                assert_eq!(record.values, agent_values("racer"));
            } else {
                assert_eq!(record.db_id, retry_id);
                assert_eq!(record.values, agent_values("retried"));
            }
        }
    });
}

/// Two racing updates of the same row both answer the contract's Ok and
/// land serially — one row, one of the two values.
#[test]
fn two_racing_updates_serialize_onto_one_row() {
    with_race_db(|conninfo| {
        let setup = PostgresStorage::connect(conninfo).unwrap();
        let mut writer = PostgresStorage::connect(conninfo).unwrap();
        register(&mut writer);
        EntityStorage::put_entity(&mut writer, &key(700), &agent_values("seed"), true).unwrap();
        drop(setup);

        let barrier = Arc::new(Barrier::new(2));
        let racers: Vec<_> = ["left", "right"]
            .iter()
            .map(|name| {
                let conninfo = conninfo.to_string();
                let barrier = Arc::clone(&barrier);
                let name = (*name).to_string();
                std::thread::spawn(move || {
                    let mut storage = PostgresStorage::connect(&conninfo).unwrap();
                    register(&mut storage);
                    barrier.wait();
                    EntityStorage::put_entity(&mut storage, &key(700), &agent_values(&name), false)
                })
            })
            .collect();
        for racer in racers {
            let outcome = racer.join().expect("the racer returns");
            assert_eq!(outcome, Ok(700), "an update never duplicates the row");
        }

        // One row, holding one of the two values.
        let mut observer = PostgresStorage::connect(conninfo).unwrap();
        register(&mut observer);
        let records = EntityStorage::all_entities(&observer).unwrap();
        assert_eq!(records.len(), 1);
        assert!(
            records[0].values == agent_values("left") || records[0].values == agent_values("right"),
            "the row holds one of the racers' values"
        );
    });
}
