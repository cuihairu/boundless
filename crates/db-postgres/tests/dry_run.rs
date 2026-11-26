//! The schema dry-run acceptance case (BW-09-07-02): the database comes
//! out unchanged and the report is the list of statements that would
//! have executed.
//!
//! Two shapes, both against a real server: a fresh database reports the
//! whole initialisation and still has no tables afterwards; an
//! initialised database reports nothing.

use std::sync::atomic::{AtomicU32, Ordering};

use db_postgres::{PostgresStorage, dry_run};

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
        "boundless_pg_dryrun_{}_{}",
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

fn table_count(storage: &mut PostgresStorage) -> i64 {
    storage
        .client_mut()
        .query_one(
            "SELECT count(*) FROM pg_tables WHERE schemaname = 'public'",
            &[],
        )
        .unwrap()
        .get(0)
}

#[test]
fn a_fresh_database_is_reported_and_left_unchanged() {
    with_fresh_db(|storage| {
        assert_eq!(table_count(storage), 0);

        let missing = dry_run::plan_missing_schema(storage.client_mut()).unwrap();
        // The plan is the whole initialisation: eleven tables, two
        // indexes, the version seed.
        assert_eq!(missing.len(), 14);
        assert!(
            missing
                .iter()
                .any(|statement| statement.contains("\"bigworldInfo\""))
        );
        assert!(
            missing
                .iter()
                .any(|statement| statement.contains("\"bigworldLogOnMapping\""))
        );

        // The report carries the reference's `DRYRUN:` prefix.
        let lines = dry_run::dry_run_lines(&missing);
        assert_eq!(lines.len(), 14);
        assert!(lines[0].starts_with("DRYRUN:\t"));
        assert!(lines[0].ends_with(';'));

        // The acceptance: the database is unchanged — still no tables,
        // and the version table (reported but not run) does not exist.
        assert_eq!(table_count(storage), 0);
        assert!(
            db_postgres::schema_init::read_version(storage.client_mut()).is_err(),
            "the version table must not exist yet"
        );

        // And the reported statements, applied, reach the target.
        db_postgres::schema_init::apply(storage.client_mut(), &missing).unwrap();
        assert_eq!(table_count(storage), 11);
        assert_eq!(
            db_postgres::schema_init::read_version(storage.client_mut()).unwrap(),
            Some(db_postgres::CURRENT_VERSION)
        );
    });
}

#[test]
fn an_up_to_date_database_reports_nothing() {
    with_fresh_db(|storage| {
        let plan = db_postgres::schema_init::plan();
        db_postgres::schema_init::apply(storage.client_mut(), &plan).unwrap();
        let tables_before = table_count(storage);

        let missing = dry_run::plan_missing_schema(storage.client_mut()).unwrap();
        assert!(missing.is_empty(), "unexpected plan: {missing:?}");

        // Unchanged still.
        assert_eq!(table_count(storage), tables_before);
    });
}

#[test]
fn a_partially_initialised_database_reports_its_gaps() {
    with_fresh_db(|storage| {
        // Only the version table exists, without a version row.
        storage
            .client_mut()
            .execute(
                "CREATE TABLE \"bigworldInfo\" (\"version\" BIGINT NOT NULL, \
                 \"snapshotTime\" TIMESTAMPTZ NULL, \
                 \"isPasswordHashed\" BOOLEAN NOT NULL DEFAULT FALSE)",
                &[],
            )
            .unwrap();

        let missing = dry_run::plan_missing_schema(storage.client_mut()).unwrap();
        // The version table's CREATE is not in the plan; the other ten
        // tables, both indexes and the seed are.
        assert_eq!(missing.len(), 10 + 2 + 1);
        assert!(
            !missing
                .iter()
                .any(|statement| statement.contains("CREATE TABLE IF NOT EXISTS \"bigworldInfo\""))
        );
        assert!(
            missing
                .iter()
                .any(|statement| statement.contains("CREATE TABLE IF NOT EXISTS \"bigworldNewID\""))
        );
        assert!(
            missing
                .iter()
                .any(|statement| statement.starts_with("INSERT INTO \"bigworldInfo\""))
        );

        // Applying the gap plan finishes the initialisation.
        db_postgres::schema_init::apply(storage.client_mut(), &missing).unwrap();
        assert_eq!(
            db_postgres::schema_init::read_version(storage.client_mut()).unwrap(),
            Some(db_postgres::CURRENT_VERSION)
        );
        assert!(
            dry_run::plan_missing_schema(storage.client_mut())
                .unwrap()
                .is_empty()
        );
    });
}
