//! The database-initialisation acceptance case (BW-09-07-01): an empty
//! database becomes a startable schema.
//!
//! "Startable" is asserted the way the DBApp asserts it: the fixed tables
//! exist, the schema version reads back as the current version, the entity
//! tables can be created and written, and a second initialisation run
//! changes nothing (the statements are all guarded).

use std::sync::atomic::{AtomicU32, Ordering};

use db::EntityKey;
use db_postgres::{CURRENT_VERSION, EntitySchema, PostgresStorage};
use entitydef::value::PropertyValue;

static TEST_DB_COUNTER: AtomicU32 = AtomicU32::new(0);

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
        "boundless_pg_init_{}_{}",
        std::process::id(),
        TEST_DB_COUNTER.fetch_add(1, Ordering::SeqCst)
    );
    let quoted = database.replace('"', "\"\"");
    if let Err(err) = admin
        .client_mut()
        .execute(format!("CREATE DATABASE \"{quoted}\"").as_str(), &[])
    {
        eprintln!("skipping: cannot create the test database ({err})");
        return;
    }
    let conninfo = format!("{admin_conninfo} dbname={database}");
    let result = PostgresStorage::connect(&conninfo).map(|mut storage| {
        body(&mut storage);
    });
    let _ = admin.client_mut().execute(
        format!("DROP DATABASE \"{quoted}\" WITH (FORCE)").as_str(),
        &[],
    );
    if let Err(err) = result {
        panic!("the fresh database would not open: {err}");
    }
}

fn fixed_table_names() -> Vec<String> {
    let plan = db_postgres::schema_init::plan();
    plan.iter()
        .filter_map(|statement| {
            statement
                .strip_prefix("CREATE TABLE IF NOT EXISTS \"")
                .and_then(|rest| rest.split('"').next())
                .map(str::to_string)
        })
        .collect()
}

#[test]
fn an_empty_database_becomes_a_startable_schema() {
    with_fresh_db(|storage| {
        // Empty to start: no tables at all.
        let existing: i64 = storage
            .client_mut()
            .query_one(
                "SELECT count(*) FROM pg_tables WHERE schemaname = 'public'",
                &[],
            )
            .unwrap()
            .get(0);
        assert_eq!(existing, 0);

        let plan = db_postgres::schema_init::plan();
        db_postgres::schema_init::apply(&mut storage.client_mut(), &plan).unwrap();

        // Every fixed table exists.
        for table in fixed_table_names() {
            let present: bool = storage
                .client_mut()
                .query_one(
                    "SELECT EXISTS (SELECT 1 FROM pg_tables \
                     WHERE schemaname = 'public' AND tablename = $1)",
                    &[&table],
                )
                .unwrap()
                .get(0);
            assert!(present, "missing fixed table {table}");
        }

        // The version reads back as current: what the DBApp requires to
        // consider the database startable.
        assert_eq!(
            db_postgres::schema_init::read_version(&mut storage.client_mut()).unwrap(),
            Some(CURRENT_VERSION)
        );

        // An entity table can be created and written on top of it.
        let values = vec![
            PropertyValue::String("init".into()),
            PropertyValue::Sequence(vec![PropertyValue::Int(1)]),
        ];
        let schema = EntitySchema::for_record("Account", &["name", "extra"], &values).unwrap();
        storage.create_entity_table(&schema).unwrap();
        let mut record = db::EntityRecord::new(0);
        record.db_id = 1;
        record.values = values;
        storage.put_entity(&schema, &record).unwrap();
        let key = EntityKey {
            type_id: 0,
            db_id: 1,
        };
        assert_eq!(storage.get_entity(&schema, &key).unwrap().unwrap(), record);

        // A second run changes nothing: same tables, one version row.
        db_postgres::schema_init::apply(&mut storage.client_mut(), &plan).unwrap();
        let version_rows: i64 = storage
            .client_mut()
            .query_one("SELECT count(*) FROM \"bigworldInfo\"", &[])
            .unwrap()
            .get(0);
        assert_eq!(version_rows, 1);
        assert_eq!(
            db_postgres::schema_init::read_version(&mut storage.client_mut()).unwrap(),
            Some(CURRENT_VERSION)
        );
    });
}

#[test]
fn an_uninitialised_database_reads_no_version() {
    with_fresh_db(|storage| {
        // The version table does not exist yet: the read reports the
        // uninitialised state cleanly rather than panicking.
        let result = db_postgres::schema_init::read_version(&mut storage.client_mut());
        assert!(result.is_err());
    });
}
