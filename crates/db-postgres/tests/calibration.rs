//! The identity calibration acceptance case (BW-09-07-06, 验收 新建实体
//! 不冲突): after the import writes rows under explicit ids, the first
//! entity created against the table takes a fresh id, not one of the
//! ids the import landed.
//!
//! The reason the step exists is the identity column's own rule: a
//! value given outright does not advance the sequence, so without the
//! calibration the first create after the import asks the sequence for
//! its next value and lands on an id an imported row already holds —
//! the duplicate-primary-key outcome of `insertExplicit`
//! (`put_entity_task.cpp:93-97`). The reference never carries this step:
//! its `AUTO_INCREMENT` state lives in the table and travels with the
//! copied data, and its only row-merge path replays rows into a live
//! primary (`consolidate_entity_task.cpp:27-34`), so the calibration is
//! the port's own seam.
//!
//! The cases run against a real PostgreSQL server, with the connection
//! string from `BOUNDLESS_PG_URL`, or the CI storage service's
//! `CI_STORE_PG_*` settings, or a local server's unix socket; when no
//! server is reachable they skip with a notice. Every case gets its own
//! private database (the fixed tables included), dropped when the case
//! finishes.

use std::sync::atomic::{AtomicU32, Ordering};

use db::{DbError, EntityKey, EntityStorage};
use db_postgres::{EntitySchema, PostgresStorage};
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
    let user = std::env::var("USER")
        .or_else(|_| std::env::var("LOGNAME"))
        .unwrap_or_else(|_| "postgres".to_string());
    Some(format!(
        "host=/var/run/postgresql dbname=postgres user={user}"
    ))
}

/// Runs `body` with the connection string of a fresh private database
/// (the fixed tables included), or skips when no PostgreSQL server is
/// reachable. The database is dropped when the body returns.
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

const GUARD_TYPE: u16 = 7;

fn guard_schema() -> EntitySchema {
    EntitySchema::for_record(
        "Guard",
        &["name", "level"],
        &[PropertyValue::String(String::new()), PropertyValue::Int(0)],
    )
    .unwrap()
}

fn guard_values(name: &str, level: i64) -> Vec<PropertyValue> {
    vec![
        PropertyValue::String(name.into()),
        PropertyValue::Int(level),
    ]
}

/// A connection that knows the guard type — the contract faces resolve
/// the table shape by type id through the connection's own registry.
fn server(conninfo: &str) -> PostgresStorage {
    let mut storage = PostgresStorage::connect(conninfo).expect("the database opens");
    let schema = guard_schema();
    storage.create_entity_table(&schema).unwrap();
    storage.register_entity_type(GUARD_TYPE, &schema);
    storage
}

fn key(db_id: i64) -> EntityKey {
    EntityKey {
        type_id: GUARD_TYPE,
        db_id,
    }
}

/// 验收: an explicit-id import followed by the calibration never hands
/// a created entity one of the imported ids — and the calibration's own
/// answer is the next free value, not a skipped one.
#[test]
fn a_create_after_the_import_never_repeats_an_imported_id() {
    with_fresh_db(|conninfo| {
        let mut storage = server(conninfo);

        // The import writes its rows under the ids it carried.
        let imported = vec![5i64, 900, 40_000];
        for &db_id in &imported {
            EntityStorage::put_entity(&mut storage, &key(db_id), &guard_values("held", 1), true)
                .unwrap();
        }

        // The calibration reports the next free value: the largest
        // imported id plus one, taken rather than skipped.
        let next = storage.calibrate_dbid_allocation(&guard_schema()).unwrap();
        assert_eq!(next, 40_001);

        // The creates afterwards climb past the imported ids, never
        // landing on one of them.
        let created = (0..3)
            .map(|_| {
                EntityStorage::put_entity(
                    &mut storage,
                    &key(PENDING_DATABASE_ID),
                    &guard_values("fresh", 1),
                    false,
                )
                .unwrap()
            })
            .collect::<Vec<_>>();
        for id in &created {
            assert!(!imported.contains(id), "{id} repeats an imported id");
            assert!(*id >= next, "{id} fell below the calibration");
        }
        assert_eq!(created, vec![40_001, 40_002, 40_003]);

        // Every id the table holds now is distinct.
        let rows = EntityStorage::all_entities(&storage).unwrap();
        assert_eq!(rows.len(), 6);
    });
}

/// Without the step the create does collide: the sequence still stands
/// at its start, so the first create takes an id the import already
/// holds. The calibration is what moves it — this is the failure the
/// seam exists to prevent.
#[test]
fn an_uncalibrated_create_after_the_import_is_a_duplicate() {
    with_fresh_db(|conninfo| {
        let mut storage = server(conninfo);
        EntityStorage::put_entity(&mut storage, &key(1), &guard_values("held", 1), true).unwrap();

        // The sequence never moved for the explicit id.
        let err = EntityStorage::put_entity(
            &mut storage,
            &key(PENDING_DATABASE_ID),
            &guard_values("fresh", 1),
            false,
        )
        .expect_err("the uncalibrated create collides");
        assert_eq!(err, DbError::AlreadyExists);
    });
}

/// A table that imported nothing recalibrates to its start value, and
/// the first create takes exactly that — no value is burned.
#[test]
fn an_empty_table_calibrates_to_its_start_value() {
    with_fresh_db(|conninfo| {
        let mut storage = server(conninfo);

        let start = storage.calibrate_dbid_allocation(&guard_schema()).unwrap();
        assert_eq!(start, 1);

        let created = EntityStorage::put_entity(
            &mut storage,
            &key(PENDING_DATABASE_ID),
            &guard_values("fresh", 1),
            false,
        )
        .unwrap();
        assert_eq!(created, start);

        // And the recalibration after it moves on to the next value.
        let next = storage.calibrate_dbid_allocation(&guard_schema()).unwrap();
        assert_eq!(next, start + 1);
    });
}
