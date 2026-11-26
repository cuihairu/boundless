//! The migration failure and rerun acceptance case (BW-09-07-07,
//! 验收 下次执行不重复导入): a batch that fails part-way keeps the rows
//! it already landed, and the next execution over the same dataset
//! completes the migration without importing anything twice.
//!
//! The pass is the BW-09-07-05 import: rows land one by one, so a batch
//! that trips mid-way stops with its earlier rows standing — the pass
//! never rolls back what the caller did not wrap in a transaction, the
//! same way the reference's `buffered_entity_tasks` keeps the writes
//! each task finished. The rerun then compares every slot first: a slot
//! already holding the source's values is skipped, the rest land — so
//! the migrated table ends with one row per source id, no duplicate
//! and no overwrite.
//!
//! The account mapping rides the same contract with its own key: the
//! unique log-on name means a moved mapping aborts the batch without
//! moving the standing row, and the rerun duplicates nothing.
//!
//! The cases run against a real PostgreSQL server, with the connection
//! string from `BOUNDLESS_PG_URL`, or the CI storage service's
//! `CI_STORE_PG_*` settings, or a local server's unix socket; when no
//! server is reachable they skip with a notice. Every case gets its own
//! private database (the fixed tables included), dropped when the case
//! finishes.

use std::sync::atomic::{AtomicU32, Ordering};

use db::{DbError, EntityKey, EntityStorage};
use db_postgres::{
    AccountRow, EntitySchema, MySqlEntityRow, PostgresStorage, import_accounts, import_entities,
};
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

const HERO_TYPE: u16 = 4;

fn hero_schema() -> EntitySchema {
    EntitySchema::for_record(
        "Hero",
        &["name", "level", "data"],
        &[
            PropertyValue::String(String::new()),
            PropertyValue::Int(0),
            PropertyValue::Blob(Vec::new()),
        ],
    )
    .unwrap()
}

fn hero(db_id: i64, name: &str, level: i64, data: &[u8]) -> MySqlEntityRow {
    MySqlEntityRow {
        db_id,
        values: vec![
            PropertyValue::String(name.into()),
            PropertyValue::Int(level),
            PropertyValue::Blob(data.to_vec()),
        ],
    }
}

/// A connection that knows the hero type.
fn server(conninfo: &str) -> PostgresStorage {
    let mut storage = PostgresStorage::connect(conninfo).expect("the database opens");
    let schema = hero_schema();
    storage.create_entity_table(&schema).unwrap();
    storage.register_entity_type(HERO_TYPE, &schema);
    storage
}

fn key(db_id: i64) -> EntityKey {
    EntityKey {
        type_id: HERO_TYPE,
        db_id,
    }
}

/// 验收: a batch that fails part-way keeps the rows it already landed,
/// and the next execution completes the migration without importing
/// anything twice.
#[test]
fn a_failing_batch_keeps_its_landed_rows_and_the_rerun_duplicates_nothing() {
    with_fresh_db(|conninfo| {
        let mut storage = server(conninfo);
        let schema = hero_schema();

        let good = hero(7, "warrior", 3, &[0x01]);
        // A row the schema rejects — one value for three columns — so the
        // batch trips after the good row landed.
        let broken = MySqlEntityRow {
            db_id: 8,
            values: vec![PropertyValue::String("bad".into())],
        };
        let tail = hero(9, "mage", 9, &[0x02]);

        let err = import_entities(
            &mut storage,
            HERO_TYPE,
            &schema,
            &[good.clone(), broken, tail.clone()],
        )
        .expect_err("the broken row trips the batch");
        assert!(matches!(err, DbError::Storage(_)), "{err:?}");

        // The failure state: the landed row stands whole, nothing after
        // it does.
        let found = EntityStorage::get_entity(&storage, &key(7))
            .unwrap()
            .expect("the landed row stands");
        assert_eq!(found.values, good.values);
        for absent in [8, 9] {
            assert_eq!(
                EntityStorage::get_entity(&storage, &key(absent)).unwrap(),
                None,
                "row {absent} never landed"
            );
        }

        // The next execution: the corrected dataset. The landed row is
        // skipped as it stands, the rest land once.
        let fixed = hero(8, "healer", 5, &[0x03]);
        let report = import_entities(
            &mut storage,
            HERO_TYPE,
            &schema,
            &[good.clone(), fixed.clone(), tail.clone()],
        )
        .expect("the rerun completes");
        assert_eq!(
            report,
            db_postgres::ImportReport {
                written: 2,
                skipped: 1
            }
        );

        // One row per source id, each the source's values — row 7 was
        // neither duplicated nor overwritten.
        assert_eq!(EntityStorage::all_entities(&storage).unwrap().len(), 3);
        let found = EntityStorage::get_entity(&storage, &key(7))
            .unwrap()
            .expect("row 7 stands");
        assert_eq!(found.values, good.values);
        let found = EntityStorage::get_entity(&storage, &key(8))
            .unwrap()
            .expect("row 8 landed");
        assert_eq!(found.values, fixed.values);
        let found = EntityStorage::get_entity(&storage, &key(9))
            .unwrap()
            .expect("row 9 landed");
        assert_eq!(found.values, tail.values);

        // And a third pass over the same dataset is pure skip.
        let report = import_entities(
            &mut storage,
            HERO_TYPE,
            &schema,
            &[good.clone(), fixed.clone(), tail.clone()],
        )
        .expect("the third pass runs");
        assert_eq!(
            report,
            db_postgres::ImportReport {
                written: 0,
                skipped: 3
            }
        );
        assert_eq!(EntityStorage::all_entities(&storage).unwrap().len(), 3);
    });
}

/// 验收: the account mapping batch aborts on a moved name without
/// moving the standing row, and the next execution duplicates nothing —
/// the unique log-on key keeps one row per name through every rerun.
#[test]
fn an_aborted_account_batch_leaves_the_mapping_and_the_rerun_duplicates_nothing() {
    with_fresh_db(|conninfo| {
        let mut storage = server(conninfo);

        let held = AccountRow {
            log_on_name: b"hero".to_vec(),
            password: b"pw".to_vec(),
            entity_type: HERO_TYPE,
            entity_id: 7,
        };
        import_accounts(&mut storage, std::slice::from_ref(&held)).unwrap();

        // The batch: a fresh name lands, then a name the mapping already
        // holds for another entity trips the batch.
        let fresh = AccountRow {
            log_on_name: b"scout".to_vec(),
            password: b"pw2".to_vec(),
            entity_type: HERO_TYPE,
            entity_id: 8,
        };
        let moved = AccountRow {
            log_on_name: b"hero".to_vec(),
            password: b"pw".to_vec(),
            entity_type: HERO_TYPE,
            entity_id: 99,
        };
        let err = import_accounts(&mut storage, &[fresh.clone(), moved])
            .expect_err("the moved name trips the batch");
        assert_eq!(err, DbError::DuplicateAccount);

        // The failure state: the standing mapping never moved, the fresh
        // row stands.
        assert_eq!(storage.get_account(b"hero").unwrap(), Some(held.clone()));
        assert_eq!(storage.get_account(b"scout").unwrap(), Some(fresh.clone()));

        // The next execution over the whole set: everything already
        // stands, everything skipped, nothing duplicated.
        let report = import_accounts(&mut storage, &[held.clone(), fresh.clone()]).unwrap();
        assert_eq!(
            report,
            db_postgres::ImportReport {
                written: 0,
                skipped: 2
            }
        );
        let names: i64 = storage
            .client_mut()
            .query_one("SELECT COUNT(*) FROM \"bigworldLogOnMapping\"", &[])
            .unwrap()
            .get(0);
        assert_eq!(names, 2, "one row per name, through every rerun");
        assert_eq!(storage.get_account(b"hero").unwrap(), Some(held));
    });
}
