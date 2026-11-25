//! The MySQL import acceptance case (BW-09-07-05, 验收 ID、关系和二进制
//! 字段对照): the ids the source dataset carried, the account relations
//! and the binary fields come through the import verbatim, and a rerun
//! of the same dataset writes nothing new.
//!
//! The source layout is the one the reference writes
//! (`entity_type_mapping.cpp` `createTable`): `tbl_<TypeName>` rows keyed
//! by the explicit `id` column with one `sm_<propName>` column per
//! persistent property, and the account mapping
//! `bigworldLogOnMapping`, keyed by the unique log-on name into the
//! binary password columns. The rows arrive decoded — in the reference
//! they are read into property values through the entity defs
//! (`MySqlDatabase::getEntity`), so the driver owns the decoding and the
//! import owns the landing.
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

const HERO_TYPE: u16 = 3;

/// The source type: a name, a level and a binary payload — the payload
/// is what the binary-field comparison owes.
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

fn key(db_id: i64) -> EntityKey {
    EntityKey {
        type_id: HERO_TYPE,
        db_id,
    }
}

fn connect(conninfo: &str) -> PostgresStorage {
    PostgresStorage::connect(conninfo).expect("the fresh database opens")
}

/// A connection that knows the hero type: the contract faces resolve
/// the table shape by type id through the connection's own registry,
/// so every connection registers the shape itself.
fn server(conninfo: &str) -> PostgresStorage {
    let mut storage = connect(conninfo);
    storage.create_entity_table(&hero_schema()).unwrap();
    storage.register_entity_type(HERO_TYPE, &hero_schema());
    storage
}

/// The source dataset: ids spanning the small, the mid and the large end
/// of the 64-bit column, and payloads carrying the awkward bytes —
/// NULs, high bytes, and a run that is not valid UTF-8.
fn source_rows() -> Vec<MySqlEntityRow> {
    vec![
        hero(1, "cui", 1, &[0x00, 0x01, 0x7f]),
        hero(47, "hash", 2, &[0xff, 0xfe, 0x80, 0x00, 0x00]),
        hero(65_536, "broad", 3, &[0xa3, 0x2b, 0x2b, 0xa3]),
    ]
}

/// 验收: every id the source dataset carried, and its binary fields,
/// come through the import verbatim — the row the storage reads back for
/// each source id is the source row, byte for byte.
#[test]
fn the_imported_rows_match_the_source_ids_and_binary_fields() {
    with_fresh_db(|conninfo| {
        let mut storage = server(conninfo);
        let rows = source_rows();
        let report = import_entities(&mut storage, HERO_TYPE, &hero_schema(), &rows).unwrap();
        assert_eq!(report.written, rows.len());
        assert_eq!(report.skipped, 0);

        for row in &rows {
            let key = key(row.db_id);
            let found = EntityStorage::get_entity(&storage, &key)
                .unwrap()
                .unwrap_or_else(|| panic!("the imported id {} is missing", row.db_id));
            assert_eq!(found.db_id, row.db_id, "the id survived");
            assert_eq!(found.values, row.values);
        }

        // The binary fields byte for byte, straight off the column.
        let payload: Vec<u8> = storage
            .client_mut()
            .query_one(
                "SELECT \"sm_data\" FROM \"tbl_Hero\" WHERE \"id\" = $1",
                &[&47i64],
            )
            .unwrap()
            .get(0);
        assert_eq!(payload, vec![0xffu8, 0xfe, 0x80, 0x00, 0x00]);

        let records = EntityStorage::all_entities(&storage).unwrap();
        assert_eq!(records.len(), rows.len());
    });
}

/// 验收: the account mapping lands as the relation it was — each log-on
/// name resolves to the password it carried and to the entity the
/// mapping names, and that entity is one of the imported rows.
#[test]
fn the_imported_accounts_resolve_to_the_same_entities() {
    with_fresh_db(|conninfo| {
        let mut storage = server(conninfo);
        import_entities(&mut storage, HERO_TYPE, &hero_schema(), &source_rows()).unwrap();

        // Log-on names and passwords are raw bytes in the reference —
        // they carry NULs, so they cannot be text.
        let rows = vec![
            AccountRow {
                log_on_name: b"cui\x00boundless".to_vec(),
                password: vec![0x00, 0xff, 0x10],
                entity_type: HERO_TYPE,
                entity_id: 47,
            },
            AccountRow {
                log_on_name: b"guest".to_vec(),
                password: b"open".to_vec(),
                entity_type: HERO_TYPE,
                entity_id: 65_536,
            },
        ];
        let report = import_accounts(&mut storage, &rows).unwrap();
        assert_eq!(report.written, 2);

        for row in &rows {
            let found = storage
                .get_account(&row.log_on_name)
                .unwrap()
                .expect("the mapping lands");
            assert_eq!(&found, row, "the mapping is the source row");
            // The relation: the account names an entity that the import
            // landed.
            let entity = EntityStorage::get_entity(&storage, &key(row.entity_id))
                .unwrap()
                .unwrap_or_else(|| panic!("the account's entity {} is missing", row.entity_id));
            assert_eq!(entity.values.len(), 3);
        }
    });
}

/// A rerun of the same dataset is a no-op that says so: nothing new is
/// written, nothing is duplicated, and the stored rows are still the
/// source's.
#[test]
fn a_rerun_of_the_same_dataset_writes_nothing_new() {
    with_fresh_db(|conninfo| {
        let mut storage = server(conninfo);
        let rows = source_rows();
        let first = import_entities(&mut storage, HERO_TYPE, &hero_schema(), &rows).unwrap();
        assert_eq!(first.written, rows.len());

        let again = import_entities(&mut storage, HERO_TYPE, &hero_schema(), &rows).unwrap();
        assert_eq!(again.written, 0);
        assert_eq!(again.skipped, rows.len());

        // One row per id, still the source's values.
        assert_eq!(
            EntityStorage::all_entities(&storage).unwrap().len(),
            rows.len()
        );
        for row in &rows {
            let found = EntityStorage::get_entity(&storage, &key(row.db_id))
                .unwrap()
                .expect("the row is still there");
            assert_eq!(found.values, row.values);
        }

        // The accounts ride the same contract.
        let accounts = vec![AccountRow {
            log_on_name: b"cui".to_vec(),
            password: b"held".to_vec(),
            entity_type: HERO_TYPE,
            entity_id: 1,
        }];
        import_accounts(&mut storage, &accounts).unwrap();
        let again = import_accounts(&mut storage, &accounts).unwrap();
        assert_eq!((again.written, again.skipped), (0, 1));
        assert_eq!(
            storage.get_account(b"cui").unwrap(),
            Some(accounts[0].clone())
        );
    });
}

/// A slot the source dataset disagrees with is a conflict, not an
/// overwrite: the pass fails with both shapes named and the stored row
/// stands — neither the imported values nor a duplicate row land.
#[test]
fn a_conflicting_slot_fails_and_leaves_the_stored_row() {
    with_fresh_db(|conninfo| {
        let mut storage = server(conninfo);
        import_entities(&mut storage, HERO_TYPE, &hero_schema(), &source_rows()).unwrap();

        let conflict = vec![hero(1, "someone-else", 99, &[0xff])];
        let err = import_entities(&mut storage, HERO_TYPE, &hero_schema(), &conflict)
            .expect_err("the disagreement is reported");
        let message = err.to_string();
        assert!(message.contains("imported"), "{message}");
        assert!(message.contains("stored"), "{message}");

        // The stored row is untouched and no duplicate row exists.
        let found = EntityStorage::get_entity(&storage, &key(1))
            .unwrap()
            .expect("the stored row stands");
        assert_eq!(found.values, source_rows()[0].values);
        assert_eq!(EntityStorage::all_entities(&storage).unwrap().len(), 3);
    });
}

/// A log-on name the mapping already holds for another password is the
/// contract's duplicate: the standing row never moves.
#[test]
fn a_name_mapped_elsewhere_is_the_contract_duplicate() {
    with_fresh_db(|conninfo| {
        let mut storage = server(conninfo);
        let standing = AccountRow {
            log_on_name: b"cui".to_vec(),
            password: b"standing".to_vec(),
            entity_type: HERO_TYPE,
            entity_id: 1,
        };
        import_accounts(&mut storage, std::slice::from_ref(&standing)).unwrap();

        let moved = AccountRow {
            password: b"moved".to_vec(),
            ..standing.clone()
        };
        let err = import_accounts(&mut storage, std::slice::from_ref(&moved))
            .expect_err("the moved name is refused");
        assert_eq!(err, DbError::DuplicateAccount);

        assert_eq!(storage.get_account(b"cui").unwrap(), Some(standing));
    });
}
