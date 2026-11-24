//! Importing a MySQL dataset into the PostgreSQL backend
//! (BW-09-07-05, 验收 ID、关系和二进制字段对照).
//!
//! The source is the MySQL table layout the reference writes
//! (`entity_type_mapping.cpp` `createTable`): `tbl_<TypeName>` rows
//! keyed by the explicit `id` with one `sm_*` column per persistent
//! property, and the account mapping `bigworldLogOnMapping` keyed by
//! the unique log-on name into the binary password columns. The rows
//! arrive decoded, and the import lands them with the explicit-id
//! write (`insertExplicit`, put_entity_task.cpp:89-97) — every source
//! DBID survives verbatim, the account relation resolves to the same
//! entity, and binary fields (the `BLOB`/`VARBINARY` columns) come back
//! byte-for-byte.
//!
//! The import is re-runnable: an id that already holds equal content is
//! skipped and reported, a conflicting one fails with both shapes named
//! (BW-09-07-07's rerun contract rides on the same faces).
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

/// The source type: a String, an Int and a Blob property — the Blob
/// carries every awkward byte the binary-field comparison owes.
fn hero_schema(storage: &mut PostgresStorage, type_id: u16) -> EntitySchema {
    let values = vec![
        PropertyValue::String("".to_string()),
        PropertyValue::Int(0),
        PropertyValue::Blob(vec![]),
    ];
    let schema = EntitySchema::for_record("Hero", &["name", "level", "data"], &values).unwrap();
    storage.create_entity_table(&schema).unwrap();
    storage.add_entity_type(type_id, "Hero").unwrap();
    storage.register_entity_type(type_id, &schema);
    schema
}

fn hero(db_id: i64, name: &str, level: i64, blob: &[u8]) -> MySqlEntityRow {
    MySqlEntityRow {
        db_id,
        values: vec![
            PropertyValue::String(name.to_string()),
            PropertyValue::Int(level),
            PropertyValue::Blob(blob.to_vec()),
        ],
    }
}

fn account(name: &[u8], password: &[u8], entity_id: i64) -> AccountRow {
    AccountRow {
        log_on_name: name.to_vec(),
        password: password.to_vec(),
        entity_type: 4,
        entity_id,
    }
}

/// 验收: the import lands the source ids verbatim, the account relations
/// resolve to the imported entities, and the binary fields come back
/// byte-for-byte — including the NULs and high bytes a text dump would
/// have to escape.
#[test]
fn the_import_lands_ids_relations_and_binary_fields_verbatim() {
    with_fresh_db(|storage, connect| {
        let schema = hero_schema(storage, 4);
        let blob_a = [0x00u8, 0x01, 0xFF, 0x7F, b'a', 0x00, 0xFE];
        let blob_b: Vec<u8> = (0..=255u8).chain((0..=255u8).rev()).collect();
        let source = [
            hero(7, "warrior", 3, &blob_a),
            hero(42, "守护者·α", 99, &blob_b),
            hero(9000, "scout", 1, &[]),
        ];
        let accounts = [
            account(b"hero\x00one", &[0x00, 0xDE, 0xAD, 0xBE, 0xEF, 0xFF], 7),
            account("玩家·β".as_bytes(), b"\x00\x01\x02", 42),
        ];

        let entity_report =
            import_entities(storage, 4, &schema, &source).expect("the entity import runs");
        assert_eq!(
            entity_report,
            db_postgres::ImportReport {
                written: 3,
                skipped: 0
            }
        );
        let account_report = import_accounts(storage, &accounts).expect("the account import runs");
        assert_eq!(
            account_report,
            db_postgres::ImportReport {
                written: 2,
                skipped: 0
            }
        );

        // The comparison, on an independent connection: every source id
        // reads back with its own values, blobs byte-for-byte.
        let mut reader = connect();
        reader.register_entity_type(4, &schema);
        for row in &source {
            let key = EntityKey {
                type_id: 4,
                db_id: row.db_id,
            };
            let found = EntityStorage::get_entity(&reader, &key)
                .expect("the imported row reads back")
                .expect("the id exists");
            assert_eq!(found.db_id, row.db_id, "the id survives verbatim");
            assert_eq!(found.values, row.values, "the values compare equal");
        }

        // The relations: each log-on name resolves to the same entity,
        // and the binary names and passwords compare byte-for-byte.
        for row in &accounts {
            assert_eq!(
                reader
                    .get_account(&row.log_on_name)
                    .expect("the mapping read runs"),
                Some(row.clone()),
                "the account relation and binary fields survive verbatim"
            );
        }
    });
}

/// The rerun contract: a pass over already-imported rows skips them all
/// (nothing duplicates), while a conflicting shape at a taken slot fails
/// with the contract's error and leaves the stored row alone.
#[test]
fn a_rerun_skips_the_landed_rows_and_a_conflict_fails_without_moving_them() {
    with_fresh_db(|storage, _connect| {
        let schema = hero_schema(storage, 4);
        let source = [hero(7, "warrior", 3, &[1, 2]), hero(42, "mage", 9, &[])];
        let accounts = [account(b"hero", b"pw", 7)];
        import_entities(storage, 4, &schema, &source).expect("the first pass lands");
        import_accounts(storage, &accounts).expect("the first pass lands");

        // The rerun: identical rows, everything skipped.
        assert_eq!(
            import_entities(storage, 4, &schema, &source).expect("the rerun runs"),
            db_postgres::ImportReport {
                written: 0,
                skipped: 2
            }
        );
        assert_eq!(
            import_accounts(storage, &accounts).expect("the rerun runs"),
            db_postgres::ImportReport {
                written: 0,
                skipped: 1
            }
        );

        // A conflicting entity shape at a taken id: the import fails and
        // the stored row stays as it was.
        let conflicting = [hero(7, "warrior", 4, &[1, 2])];
        let err = import_entities(storage, 4, &schema, &conflicting)
            .expect_err("the conflicting shape fails");
        assert!(matches!(err, DbError::Storage(_)), "{err:?}");
        let key = EntityKey {
            type_id: 4,
            db_id: 7,
        };
        let found = EntityStorage::get_entity(storage, &key)
            .expect("the stored row reads back")
            .expect("the row exists");
        assert_eq!(
            found.values, source[0].values,
            "the stored row did not move"
        );

        // A conflicting mapping at a taken name: the duplicate contract
        // error, and the mapping stays put.
        let moved = account(b"hero", b"pw", 42);
        assert_eq!(
            import_accounts(storage, &[moved]).expect_err("the moved mapping fails"),
            DbError::DuplicateAccount
        );
        assert_eq!(
            storage.get_account(b"hero").expect("the mapping read runs"),
            Some(account(b"hero", b"pw", 7)),
            "the mapping did not move"
        );
    });
}
