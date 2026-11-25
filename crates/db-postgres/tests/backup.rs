//! The backup acceptance case (BW-09-07-04): an empty target database
//! restores identical records.
//!
//! A source database is initialised, registered and populated the way
//! the DBApp populates it — entity rows (binary, Unicode and nested
//! property values), an account mapping, the DBID books, a checkout
//! row, game time, space data, the secondary-database registry and the
//! entity-defs checksum — then exported. A second, freshly initialised
//! database restores the dump, and every record reads back the same:
//! the two dumps compare equal, the contract-level reads agree, and the
//! restored registry's serial allocates past the restored rows.

use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, SystemTime};

use db::EntityKey;
use db_postgres::{EntitySchema, PostgresStorage, export_backup, restore_backup};
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

fn with_fresh_db(body: impl FnOnce(&mut PostgresStorage, &str)) {
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
        "boundless_pg_backup_{}_{}",
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
        body(&mut storage, &conninfo);
    });
    let _ = admin.client_mut().execute(
        format!("DROP DATABASE \"{quoted}\" WITH (FORCE)").as_str(),
        &[],
    );
    if let Err(err) = result {
        panic!("the fresh database would not open: {err}");
    }
}

fn initialise(storage: &mut PostgresStorage) {
    let plan = db_postgres::schema_init::plan();
    db_postgres::schema_init::apply(&mut storage.client_mut(), &plan).unwrap();
}

/// The account schema the population uses: one property per column
/// domain the entity tables carry.
fn account_schema() -> EntitySchema {
    let values = vec![
        PropertyValue::String("名前".into()),
        PropertyValue::Int(-7),
        PropertyValue::Blob(vec![0, 255, 0]),
        PropertyValue::Vector(vec![1.0, 2.5, -3.25]),
        PropertyValue::Float(0.75),
        PropertyValue::Sequence(vec![PropertyValue::Int(2), PropertyValue::FixedDict(None)]),
    ];
    EntitySchema::for_record(
        "Account",
        &["name", "level", "secret", "pos", "weight", "loot"],
        &values,
    )
    .unwrap()
}

fn account_record(db_id: i64, name: &str) -> db::EntityRecord {
    let mut record = db::EntityRecord::new(0);
    record.db_id = db_id;
    record.values = vec![
        PropertyValue::String(name.into()),
        PropertyValue::Int(-7),
        PropertyValue::Blob(vec![0, 255, 0]),
        PropertyValue::Vector(vec![1.0, 2.5, -3.25]),
        PropertyValue::Float(0.75),
        PropertyValue::Sequence(vec![PropertyValue::Int(2), PropertyValue::FixedDict(None)]),
    ];
    record
}

/// Populates a freshly initialised database the way the DBApp does:
/// the entity-type registry, entity rows, an account mapping, the DBID
/// books, a checkout row, game time, space data, the secondary
/// databases and the entity-defs checksum.
fn populate(storage: &mut PostgresStorage) {
    initialise(storage);
    let schema = account_schema();
    storage.create_entity_table(&schema).unwrap();

    // The entity-type registry (the serial gets its first value here)
    // plus a registered name whose table was never created: the dump
    // skips it.
    storage
        .client_mut()
        .execute(
            "INSERT INTO \"bigworldEntityTypes\" (\"bigworldID\", \"name\") \
             VALUES (5, 'Account'), (6, 'Ghost')",
            &[],
        )
        .unwrap();

    storage
        .put_entity(&schema, &account_record(1001, "cui"))
        .unwrap();
    storage
        .put_entity(&schema, &account_record(1002, "崔海如"))
        .unwrap();

    storage
        .put_account(&db_postgres::AccountRow {
            log_on_name: b"alice\0bytes".to_vec(),
            password: b"s3cret".to_vec(),
            entity_type: 1,
            entity_id: 1001,
        })
        .unwrap();

    // The DBID books, a checkout row, game time, space data, the
    // secondary databases and the checksum.
    let mut client = storage.client_mut();
    client
        .execute("INSERT INTO \"bigworldNewID\" (\"id\") VALUES (900)", &[])
        .unwrap();
    client
        .execute(
            "INSERT INTO \"bigworldUsedIDs\" (\"id\") VALUES (1), (2), (3)",
            &[],
        )
        .unwrap();
    client
        .execute(
            "INSERT INTO \"bigworldLogOns\" \
             (\"databaseID\", \"typeID\", \"objectID\", \"ip\", \"port\", \"salt\", \
             \"shouldAutoLoad\") \
             VALUES (1001, 1, NULL, 167772161, 30001, 7, TRUE)",
            &[],
        )
        .unwrap();
    client
        .execute(
            "INSERT INTO \"bigworldGameTime\" (\"time\") VALUES (123456)",
            &[],
        )
        .unwrap();
    client
        .execute("INSERT INTO \"bigworldSpaces\" (\"id\") VALUES (7)", &[])
        .unwrap();
    client
        .execute(
            "INSERT INTO \"bigworldSpaceData\" \
             (\"id\", \"spaceEntryID\", \"entryKey\", \"data\") \
             VALUES (7, 100, 1, '\\x00ff7a'::bytea)",
            &[],
        )
        .unwrap();
    client
        .execute(
            "INSERT INTO \"bigworldSecondaryDatabases\" \
             (\"ip\", \"port\", \"location\") VALUES (167772161, 30002, '\\x64617461'::bytea)",
            &[],
        )
        .unwrap();
    client
        .execute(
            "INSERT INTO \"bigworldEntityDefsChecksum\" (\"checksum\") VALUES ('abc')",
            &[],
        )
        .unwrap();
    client
        .execute(
            "UPDATE \"bigworldInfo\" SET \"isPasswordHashed\" = TRUE, \
             \"snapshotTime\" = to_timestamp(1700000000.123456)",
            &[],
        )
        .unwrap();
}

fn table_names(dump: &db_postgres::BackupDump) -> Vec<String> {
    dump.tables
        .iter()
        .map(|data| data.table.name.clone())
        .collect()
}

#[test]
fn an_empty_target_restores_identical_records() {
    let source_dump = std::cell::RefCell::new(None);
    with_fresh_db(|source, _conninfo| {
        populate(source);
        let dump = export_backup(&mut source.client_mut()).unwrap();

        // The dump carries the fixed tables in creation order, the
        // registered entity table, and skips the registered name whose
        // table does not exist.
        let names = table_names(&dump);
        assert_eq!(
            &names[..11],
            &[
                "bigworldInfo",
                "bigworldEntityTypes",
                "bigworldLogOns",
                "bigworldLogOnMapping",
                "bigworldNewID",
                "bigworldUsedIDs",
                "bigworldGameTime",
                "bigworldSpaces",
                "bigworldSpaceData",
                "bigworldSecondaryDatabases",
                "bigworldEntityDefsChecksum",
            ]
        );
        assert!(names.contains(&"tbl_Account".to_string()));
        assert!(!names.contains(&"tbl_Ghost".to_string()));

        // The rows carry the state the population wrote.
        let info = &dump.tables[0];
        assert_eq!(info.rows.len(), 1);
        assert_eq!(info.rows[0][0], db_postgres::BackupCell::Int(9));
        assert_eq!(info.rows[0][2], db_postgres::BackupCell::Bool(true));
        let registry = &dump.tables[1];
        assert_eq!(registry.rows.len(), 2);
        let entity = dump
            .tables
            .iter()
            .find(|data| data.table.name == "tbl_Account")
            .unwrap();
        assert_eq!(entity.rows.len(), 2);

        *source_dump.borrow_mut() = Some(dump);
    });
    let dump = source_dump.into_inner().unwrap();

    // A freshly initialised target database restores the dump.
    with_fresh_db(|target, _conninfo| {
        initialise(target);
        target.create_entity_table(&account_schema()).unwrap();

        // Every record reads back the same: the restored database's own
        // export compares equal to the source's dump (the dumps are
        // ordered, so equality is record equality).
        restore_backup(&mut target.client_mut(), &dump).unwrap();
        let restored = export_backup(&mut target.client_mut()).unwrap();
        assert_eq!(restored, dump);

        // The contract-level reads agree: the entity rows (binary,
        // Unicode and nested values) and the account mapping.
        let schema = account_schema();
        let key = |db_id| EntityKey { type_id: 1, db_id };
        let read_back = target.get_entity(&schema, &key(1002)).unwrap().unwrap();
        assert_eq!(read_back.db_id, 1002);
        assert_eq!(read_back.values[0], PropertyValue::String("崔海如".into()));
        assert_eq!(read_back.values[2], PropertyValue::Blob(vec![0, 255, 0]));
        assert_eq!(read_back.values[4], PropertyValue::Float(0.75));
        assert_eq!(
            read_back.values[5],
            PropertyValue::Sequence(vec![PropertyValue::Int(2), PropertyValue::FixedDict(None),])
        );
        let account = target.get_account(b"alice\0bytes").unwrap().unwrap();
        assert_eq!(account.entity_id, 1001);
        assert_eq!(account.password, b"s3cret".to_vec());

        // The restored registry's serial allocates past the restored
        // rows: the next registered type continues the sequence.
        let next: i64 = target
            .client_mut()
            .query_one(
                "INSERT INTO \"bigworldEntityTypes\" (\"bigworldID\", \"name\") \
                 VALUES (7, 'Monster') RETURNING \"typeID\"",
                &[],
            )
            .unwrap()
            .get(0);
        assert_eq!(next, 3);

        // The restored version row is the schema version the DBApp
        // requires.
        assert_eq!(
            db_postgres::schema_init::read_version(&mut target.client_mut()).unwrap(),
            Some(db_postgres::CURRENT_VERSION)
        );
    });
}

#[test]
fn a_re_restore_replaces_the_rows_without_duplicating() {
    let dump_cell = std::cell::RefCell::new(None);
    with_fresh_db(|source, _conninfo| {
        populate(source);
        *dump_cell.borrow_mut() = Some(export_backup(&mut source.client_mut()).unwrap());
    });
    let dump = dump_cell.into_inner().unwrap();

    with_fresh_db(|target, _conninfo| {
        initialise(target);
        target.create_entity_table(&account_schema()).unwrap();
        restore_backup(&mut target.client_mut(), &dump).unwrap();
        // Restoring again replaces, not appends: the records still
        // compare equal, so no row was duplicated.
        restore_backup(&mut target.client_mut(), &dump).unwrap();
        let restored = export_backup(&mut target.client_mut()).unwrap();
        assert_eq!(restored, dump);
        let entity_rows: i64 = target
            .client_mut()
            .query_one("SELECT count(*) FROM \"tbl_Account\"", &[])
            .unwrap()
            .get(0);
        assert_eq!(entity_rows, 2);
    });
}

#[test]
fn a_missing_target_table_is_a_restore_error() {
    let dump_cell = std::cell::RefCell::new(None);
    with_fresh_db(|source, _conninfo| {
        populate(source);
        *dump_cell.borrow_mut() = Some(export_backup(&mut source.client_mut()).unwrap());
    });
    let mut dump = dump_cell.into_inner().unwrap();

    // A dump naming a table the target has never heard of fails, and
    // the target's earlier restores survive it (one transaction).
    dump.tables.push(db_postgres::BackupTableData {
        table: db_postgres::BackupTable {
            name: "bigworldNoSuchTable".to_string(),
            columns: vec![db_postgres::BackupColumn {
                name: "id".to_string(),
                kind: db_postgres::BackupColumnKind::Int64,
            }],
            order_by: vec!["id".to_string()],
        },
        rows: vec![vec![db_postgres::BackupCell::Int(1)]],
    });

    with_fresh_db(|target, _conninfo| {
        initialise(target);
        target.create_entity_table(&account_schema()).unwrap();
        let good = db_postgres::BackupDump {
            tables: dump.tables[..dump.tables.len() - 1].to_vec(),
        };
        restore_backup(&mut target.client_mut(), &good).unwrap();

        let err = restore_backup(&mut target.client_mut(), &dump).unwrap_err();
        assert!(
            err.to_string()
                .contains("bigworldNoSuchTable is missing in the target database")
        );

        // The failed restore left the earlier records untouched: the
        // transaction rolled its work back.
        let entity_rows: i64 = target
            .client_mut()
            .query_one("SELECT count(*) FROM \"tbl_Account\"", &[])
            .unwrap()
            .get(0);
        assert_eq!(entity_rows, 2);
    });
}

#[test]
fn an_uninitialised_database_exports_no_tables() {
    with_fresh_db(|storage, _conninfo| {
        // No initialisation: no fixed tables exist, and without the
        // registry there is nothing registered either.
        let dump = export_backup(&mut storage.client_mut()).unwrap();
        assert!(dump.tables.is_empty());
    });
}

#[test]
fn a_null_snapshot_time_restores_as_null() {
    // The version row the initialisation seeds carries a NULL
    // snapshotTime: the dump reads it as a null cell and the restore
    // binds it back as one.
    let dump_cell = std::cell::RefCell::new(None);
    with_fresh_db(|source, _conninfo| {
        initialise(source);
        *dump_cell.borrow_mut() = Some(export_backup(&mut source.client_mut()).unwrap());
    });
    let dump = dump_cell.into_inner().unwrap();
    let info = &dump.tables[0];
    assert_eq!(info.rows[0][1], db_postgres::BackupCell::Null);

    with_fresh_db(|target, _conninfo| {
        initialise(target);
        restore_backup(&mut target.client_mut(), &dump).unwrap();
        let read: Option<SystemTime> = target
            .client_mut()
            .query_one("SELECT \"snapshotTime\" FROM \"bigworldInfo\"", &[])
            .unwrap()
            .get(0);
        assert_eq!(read, None);
    });
}

#[test]
fn a_timestamp_round_trips_at_microsecond_precision() {
    // The timestamp cell domain: microsecond precision both ways, so a
    // restored snapshotTime compares equal to the source's.
    let when = SystemTime::UNIX_EPOCH + Duration::from_millis(1_700_000_000_123);
    let dump_cell = std::cell::RefCell::new(None);
    with_fresh_db(|source, _conninfo| {
        initialise(source);
        source
            .client_mut()
            .execute(
                "UPDATE \"bigworldInfo\" SET \"snapshotTime\" = $1",
                &[&when],
            )
            .unwrap();
        *dump_cell.borrow_mut() = Some(export_backup(&mut source.client_mut()).unwrap());
    });
    let dump = dump_cell.into_inner().unwrap();

    with_fresh_db(|target, _conninfo| {
        initialise(target);
        restore_backup(&mut target.client_mut(), &dump).unwrap();
        let read: Option<SystemTime> = target
            .client_mut()
            .query_one("SELECT \"snapshotTime\" FROM \"bigworldInfo\"", &[])
            .unwrap()
            .get(0);
        assert_eq!(read, Some(when));
    });
}
