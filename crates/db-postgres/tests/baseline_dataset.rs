//! OPT-09-01-01: the same baseline dataset through the storage faces.
//!
//! The SQL fixture `tests/store/common/baseline_dataset.sql` collects the
//! dataset on both real backends — a common store case, so MariaDB and
//! PostgreSQL run the very same statements. This case is the same dataset
//! expressed through the decoded-row faces: 24 entities (row i has id
//! 1000+i, name `ent-<id>`, health 100+i, speed `(i%4)*0.5`, alive when
//! `i%3 = 0`) land through `import_entities`, the four account rows
//! (alpha..delta onto the first four ids) through `import_accounts`, and
//! every row reads back equal. The statistics the SQL fixture pins
//! (24 rows, health sum 2676, 8 alive, speed sum 18.0, 4 accounts) are
//! pinned here over the read-back rows, so both collected baselines are
//! provably over one dataset. The rerun face skips all 28 rows — the
//! baseline is re-collectable without duplicating anything.
//!
//! Runs against a real PostgreSQL server with the usual connection gate
//! (BOUNDLESS_PG_URL, the CI storage service, or the local unix socket);
//! it skips when no server is reachable and drops its private database.

use std::sync::atomic::{AtomicU32, Ordering};

use db::{EntityKey, EntityStorage};
use db_postgres::{
    AccountRow, EntitySchema, ImportReport, MySqlEntityRow, PostgresStorage, import_accounts,
    import_entities,
};
use entitydef::value::PropertyValue;

static TEST_DB_COUNTER: AtomicU32 = AtomicU32::new(0);

/// The entities the dataset carries.
pub const DATASET_ENTITIES: usize = 24;
/// The accounts the dataset carries.
pub const DATASET_ACCOUNTS: usize = 4;
/// The pinned health sum: row i carries 100+i.
pub const PINNED_HEALTH_SUM: i64 = 2676;
/// The pinned alive rows: i%3 = 0.
pub const PINNED_ALIVE: usize = 8;
/// The pinned speed sum: `(i%4)*0.5`, exact in binary floating point.
pub const PINNED_SPEED_SUM: f64 = 18.0;

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
/// created, or skips when no PostgreSQL server is reachable.
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

/// The baseline type: a String, two Ints (health, alive) and a Float —
/// the same four columns the SQL fixture lays out.
fn baseline_schema(storage: &mut PostgresStorage) -> EntitySchema {
    let values = vec![
        PropertyValue::String(String::new()),
        PropertyValue::Int(0),
        PropertyValue::Float(0.0),
        PropertyValue::Int(0),
    ];
    let schema =
        EntitySchema::for_record("Baseline", &["name", "health", "speed", "alive"], &values)
            .expect("the baseline schema builds");
    storage
        .create_entity_table(&schema)
        .expect("the table creates");
    storage
        .add_entity_type(4, "Baseline")
        .expect("the type adds");
    storage.register_entity_type(4, &schema);
    schema
}

/// Dataset row i: id 1000+i, name `ent-<id>`, health 100+i, speed
/// `(i%4)*0.5`, alive when `i%3 = 0` — the SQL fixture's pattern.
fn baseline_row(index: usize) -> MySqlEntityRow {
    let id = 1000 + index as i64;
    MySqlEntityRow {
        db_id: id,
        values: vec![
            PropertyValue::String(format!("ent-{id}")),
            PropertyValue::Int(100 + index as i64),
            PropertyValue::Float((index % 4) as f64 * 0.5),
            PropertyValue::Int(i64::from(index.is_multiple_of(3))),
        ],
    }
}

/// The whole dataset, row 0..24.
fn dataset() -> Vec<MySqlEntityRow> {
    (0..DATASET_ENTITIES).map(baseline_row).collect()
}

/// The account mapping: alpha..delta onto the first four ids.
fn dataset_accounts() -> Vec<AccountRow> {
    ["alpha", "bravo", "charlie", "delta"]
        .iter()
        .enumerate()
        .map(|(index, name)| AccountRow {
            log_on_name: name.as_bytes().to_vec(),
            password: format!("pw-{name}").into_bytes(),
            entity_type: 4,
            entity_id: 1000 + index as i64,
        })
        .collect()
}

#[test]
fn the_statistics_match_the_sql_fixture() {
    let rows = dataset();
    let health_sum: i64 = rows
        .iter()
        .filter_map(|row| match &row.values[1] {
            PropertyValue::Int(health) => Some(*health),
            _ => None,
        })
        .sum();
    assert_eq!(health_sum, PINNED_HEALTH_SUM);
    assert_eq!(
        rows.iter()
            .filter(|row| matches!(&row.values[3], PropertyValue::Int(state) if *state != 0))
            .count(),
        PINNED_ALIVE
    );
    let speed_sum: f64 = rows
        .iter()
        .filter_map(|row| match &row.values[2] {
            PropertyValue::Float(speed) => Some(*speed),
            _ => None,
        })
        .sum();
    assert!((speed_sum - PINNED_SPEED_SUM).abs() < 1e-9);
    let accounts = dataset_accounts();
    assert_eq!(accounts.len(), DATASET_ACCOUNTS);
    assert_eq!(
        accounts
            .iter()
            .map(|account| account.entity_id)
            .collect::<Vec<_>>(),
        (0..DATASET_ACCOUNTS as i64)
            .map(|index| 1000 + index)
            .collect::<Vec<_>>()
    );
}

#[test]
fn the_same_dataset_lands_reads_and_recollects() {
    with_fresh_db(|storage, connect| {
        let schema = baseline_schema(storage);

        // The import lands all 24 entity rows and all 4 accounts.
        let rows = dataset();
        let entities = import_entities(storage, 4, &schema, &rows).expect("the import runs");
        assert_eq!(
            entities,
            ImportReport {
                written: DATASET_ENTITIES,
                skipped: 0
            }
        );
        let accounts = import_accounts(storage, &dataset_accounts()).expect("the import runs");
        assert_eq!(
            accounts,
            ImportReport {
                written: DATASET_ACCOUNTS,
                skipped: 0
            }
        );

        // The read-back: every row equal, and the statistics the SQL
        // fixture pins hold over the stored rows.
        let mut reader = connect();
        reader.register_entity_type(4, &schema);
        let mut health_sum = 0i64;
        let mut alive = 0usize;
        let mut speed_sum = 0.0f64;
        for row in &rows {
            let key = EntityKey {
                type_id: 4,
                db_id: row.db_id,
            };
            let found = EntityStorage::get_entity(&reader, &key)
                .expect("the row reads back")
                .expect("the id exists");
            assert_eq!(found.values, row.values, "row {} compares equal", row.db_id);
            if let [
                _,
                PropertyValue::Int(health),
                PropertyValue::Float(speed),
                PropertyValue::Int(state),
            ] = &found.values[..]
            {
                health_sum += health;
                speed_sum += speed;
                alive += usize::from(*state != 0);
            } else {
                panic!("row {} carries the four columns", row.db_id);
            }
        }
        assert_eq!(health_sum, PINNED_HEALTH_SUM);
        assert_eq!(alive, PINNED_ALIVE);
        assert_eq!(speed_sum, PINNED_SPEED_SUM);

        // The recollect: the same dataset imports again with every row
        // skipped — the baseline is re-collectable without duplication.
        let rerun_rows = import_entities(storage, 4, &schema, &rows).expect("the rerun runs");
        assert_eq!(
            rerun_rows,
            ImportReport {
                written: 0,
                skipped: DATASET_ENTITIES
            }
        );
        let rerun_accounts = import_accounts(storage, &dataset_accounts()).expect("the rerun runs");
        assert_eq!(
            rerun_accounts,
            ImportReport {
                written: 0,
                skipped: DATASET_ACCOUNTS
            }
        );
    });
}
