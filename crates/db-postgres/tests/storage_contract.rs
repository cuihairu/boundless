//! The storage contract acceptance cases (BW-09-06-05) against the real
//! backend: the shared `EntityStorage` suite the in-memory and XML
//! backends already pass, replayed on PostgreSQL — create/read/update/
//! delete, the explicit-id and pending-id legs, the logon-record face,
//! and the `all_entities` scan.
//!
//! The same server gate as `dbid.rs`: `BOUNDLESS_PG_URL`, the CI storage
//! service's `CI_STORE_PG_*`, or a local unix socket; a private database
//! per case, dropped when the case finishes.

use std::sync::atomic::{AtomicU32, Ordering};

use db::{BaseMailbox, CheckOut, EntityKey, EntityRecord, EntityStorage};
use db_postgres::{EntitySchema, PostgresStorage};
use entitydef::value::PropertyValue;
use foundation::ids::PENDING_DATABASE_ID;
use mercury::endpoint::Address;

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
    // A local server's unix socket: peer authentication, the OS account's
    // own role.
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

/// Runs `body` against a fresh private database on its own storage. The
/// database is dropped when the body returns.
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
        "boundless_pg_contract_{}_{}",
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

fn account_values() -> Vec<PropertyValue> {
    vec![
        PropertyValue::String("cui".into()),
        PropertyValue::Int(-7),
        PropertyValue::Blob(vec![9, 9]),
    ]
}

fn other_values() -> Vec<PropertyValue> {
    vec![
        PropertyValue::String("wanderer".into()),
        PropertyValue::Int(3),
        PropertyValue::Blob(vec![1]),
    ]
}

fn account_schema(values: &[PropertyValue]) -> EntitySchema {
    EntitySchema::for_record("Account", &["name", "level", "secret"], values).unwrap()
}

/// Brings the database to the contract's operating state: the fixed
/// tables first (`bigworldLogOns` is one), then the Account type
/// registered and its table created.
fn prepare(storage: &mut PostgresStorage, values: &[PropertyValue]) -> EntitySchema {
    let plan = db_postgres::schema_init::plan();
    db_postgres::schema_init::apply(&mut storage.client_mut(), &plan).unwrap();
    let schema = account_schema(values);
    storage.register_entity_type(1, &schema);
    storage.create_entity_table(&schema).unwrap();
    schema
}

fn key(db_id: i64) -> EntityKey {
    EntityKey { type_id: 1, db_id }
}

fn mailbox(ip: u32, port: u16) -> BaseMailbox {
    BaseMailbox {
        addr: Address { ip, port, salt: 7 },
        entity_id: 3,
    }
}

fn records_agree(record: &EntityRecord, back: &EntityRecord) {
    assert_eq!(back.type_id, record.type_id);
    assert_eq!(back.db_id, record.db_id);
    assert_eq!(back.values.len(), record.values.len());
    for (value, original) in back.values.iter().zip(&record.values) {
        match (value, original) {
            // The blob round trip is exact.
            (PropertyValue::Blob(bytes), PropertyValue::Blob(original)) => {
                assert_eq!(bytes, original);
            }
            (PropertyValue::String(text), PropertyValue::String(original)) => {
                assert_eq!(text, original);
            }
            (PropertyValue::Int(n), PropertyValue::Int(original)) => assert_eq!(n, original),
            (PropertyValue::Uint(n), PropertyValue::Uint(original)) => assert_eq!(n, original),
            (PropertyValue::Float(x), PropertyValue::Float(original)) => assert_eq!(x, original),
            _ => panic!("the read-back value does not match the stored one"),
        }
    }
}

/// The create leg: a pending key comes back with the table's own id, and
/// the row reads back whole.
#[test]
fn a_created_entity_allocates_its_id_and_reads_back() {
    with_fresh_db(|storage| {
        let values = account_values();
        let schema = prepare(storage, &values);
        let allocated =
            EntityStorage::put_entity(storage, &key(PENDING_DATABASE_ID), &values, false).unwrap();
        assert!(allocated > 0, "the table sequence handed out {allocated}");
        let back = EntityStorage::get_entity(storage, &key(allocated))
            .unwrap()
            .unwrap();
        records_agree(
            &EntityRecord {
                type_id: 1,
                db_id: allocated,
                values,
            },
            &back,
        );
        // The schema-shaped read agrees with the contract read.
        let shaped = storage
            .get_entity(&schema, &key(allocated))
            .unwrap()
            .unwrap();
        assert_eq!(shaped.db_id, back.db_id);
    });
}

/// The update leg: a missing row is the contract's `NotFound`, an
/// existing row takes the new values.
#[test]
fn an_update_of_a_missing_row_is_not_found_but_an_existing_row_updates() {
    with_fresh_db(|storage| {
        let values = account_values();
        prepare(storage, &values);

        let err = EntityStorage::put_entity(storage, &key(42), &values, false).unwrap_err();
        assert!(matches!(err, db::DbError::NotFound), "got {err:?}");

        let allocated =
            EntityStorage::put_entity(storage, &key(PENDING_DATABASE_ID), &values, false).unwrap();
        let updated =
            EntityStorage::put_entity(storage, &key(allocated), &other_values(), false).unwrap();
        assert_eq!(updated, allocated);
        let back = EntityStorage::get_entity(storage, &key(allocated))
            .unwrap()
            .unwrap();
        records_agree(
            &EntityRecord {
                type_id: 1,
                db_id: allocated,
                values: other_values(),
            },
            &back,
        );
    });
}

/// The explicit leg: a free id lands as given, a taken id is the
/// contract's `AlreadyExists` and the original row survives.
#[test]
fn an_explicit_insert_lands_the_given_id_or_keeps_the_original_row() {
    with_fresh_db(|storage| {
        let values = account_values();
        prepare(storage, &values);

        let id = EntityStorage::put_entity(storage, &key(5000), &values, true).unwrap();
        assert_eq!(id, 5000);

        let err =
            EntityStorage::put_entity(storage, &key(5000), &other_values(), true).unwrap_err();
        assert!(
            matches!(err, db::DbError::AlreadyExists),
            "a duplicate explicit id is {err:?}"
        );
        let back = EntityStorage::get_entity(storage, &key(5000))
            .unwrap()
            .unwrap();
        records_agree(
            &EntityRecord {
                type_id: 1,
                db_id: 5000,
                values,
            },
            &back,
        );
    });
}

/// The delete leg: the row goes, the read goes `None`, and a repeated
/// delete is the contract's `NotFound`.
#[test]
fn a_repeated_delete_is_not_found() {
    with_fresh_db(|storage| {
        let values = account_values();
        prepare(storage, &values);
        let allocated =
            EntityStorage::put_entity(storage, &key(PENDING_DATABASE_ID), &values, false).unwrap();

        EntityStorage::del_entity(storage, &key(allocated)).unwrap();
        assert!(
            EntityStorage::get_entity(storage, &key(allocated))
                .unwrap()
                .is_none()
        );
        let err = EntityStorage::del_entity(storage, &key(allocated)).unwrap_err();
        assert!(matches!(err, db::DbError::NotFound), "got {err:?}");
    });
}

/// The logon face: the grant records the holder, a second logon comes
/// back `Elsewhere` with the current holder, and the release frees the
/// seat. The address travels in network byte order and comes back whole.
#[test]
fn the_checkout_grants_once_and_returns_the_holder() {
    with_fresh_db(|storage| {
        let values = account_values();
        prepare(storage, &values);
        let allocated =
            EntityStorage::put_entity(storage, &key(PENDING_DATABASE_ID), &values, false).unwrap();
        assert_eq!(
            EntityStorage::check_out(storage, &key(allocated), mailbox(0x0102_0304, 8080)).unwrap(),
            CheckOut::Granted
        );
        assert_eq!(
            EntityStorage::logon_of(storage, &key(allocated)).unwrap(),
            Some(mailbox(0x0102_0304, 8080)),
            "the holder reads back with its address whole"
        );
        assert_eq!(
            EntityStorage::check_out(storage, &key(allocated), mailbox(0x0a00_0001, 443)).unwrap(),
            CheckOut::Elsewhere(mailbox(0x0102_0304, 8080)),
            "an occupied seat returns the current holder"
        );

        EntityStorage::check_in(storage, &key(allocated)).unwrap();
        assert_eq!(
            EntityStorage::logon_of(storage, &key(allocated)).unwrap(),
            None
        );
        // Releasing nothing held is fine — the reference deletes
        // unconditionally.
        EntityStorage::check_in(storage, &key(allocated)).unwrap();
        assert_eq!(
            EntityStorage::check_out(storage, &key(allocated), mailbox(0x0a00_0001, 443)).unwrap(),
            CheckOut::Granted
        );
    });
}

/// A checkout or a delete never points at a missing row.
#[test]
fn a_checkout_of_a_missing_row_is_not_found() {
    with_fresh_db(|storage| {
        let values = account_values();
        prepare(storage, &values);
        let err = EntityStorage::check_out(storage, &key(7), mailbox(1, 1)).unwrap_err();
        assert!(matches!(err, db::DbError::NotFound), "got {err:?}");
    });
}

/// An unknown type has no table: the read answers "no row", the write,
/// delete and checkout report it their own way.
#[test]
fn an_unknown_type_reads_as_none_and_rejects_the_write() {
    with_fresh_db(|storage| {
        let values = account_values();
        prepare(storage, &values);
        let stranger = EntityKey {
            type_id: 9,
            db_id: 42,
        };
        assert_eq!(EntityStorage::get_entity(storage, &stranger).unwrap(), None);
        let err = EntityStorage::put_entity(storage, &stranger, &values, false).unwrap_err();
        assert!(
            err.to_string().contains("entity type 9 is invalid"),
            "got {err:?}"
        );
        assert!(matches!(
            EntityStorage::del_entity(storage, &stranger).unwrap_err(),
            db::DbError::NotFound
        ));
        assert!(matches!(
            EntityStorage::check_out(storage, &stranger, mailbox(1, 1)).unwrap_err(),
            db::DbError::NotFound
        ));
    });
}

/// The full scan: every stored record, types ascending, ids ascending
/// within a type.
#[test]
fn all_entities_orders_by_type_then_dbid() {
    with_fresh_db(|storage| {
        let values = account_values();
        prepare(storage, &values);
        // A second type, so the scan crosses tables.
        let monster_values = vec![PropertyValue::Int(11)];
        let monster = EntitySchema::for_record("Monster", &["level"], &monster_values).unwrap();
        storage.register_entity_type(2, &monster);
        storage.create_entity_table(&monster).unwrap();

        // Create out of order: the scan still sorts.
        let monster_id = EntityStorage::put_entity(
            storage,
            &EntityKey {
                type_id: 2,
                db_id: PENDING_DATABASE_ID,
            },
            &monster_values,
            false,
        )
        .unwrap();
        let one =
            EntityStorage::put_entity(storage, &key(PENDING_DATABASE_ID), &values, false).unwrap();
        let two =
            EntityStorage::put_entity(storage, &key(PENDING_DATABASE_ID), &values, false).unwrap();

        let all = EntityStorage::all_entities(storage).unwrap();
        let ids: Vec<(u16, i64)> = all.iter().map(|r| (r.type_id, r.db_id)).collect();
        assert_eq!(
            ids,
            vec![(1, one), (1, two), (2, monster_id)],
            "type 1's rows go first in id order, then type 2's"
        );
        // The first row's values read back whole through the same
        // statement the contract read uses.
        records_agree(
            &EntityRecord {
                type_id: 1,
                db_id: one,
                values,
            },
            &all[0],
        );
    });
}

/// Deleting removes the log-on record with the row
/// (`del_entity_task.cpp:76`).
#[test]
fn deleting_removes_the_logon_record_with_the_row() {
    with_fresh_db(|storage| {
        let values = account_values();
        prepare(storage, &values);
        let allocated =
            EntityStorage::put_entity(storage, &key(PENDING_DATABASE_ID), &values, false).unwrap();
        EntityStorage::check_out(storage, &key(allocated), mailbox(1, 2)).unwrap();
        EntityStorage::del_entity(storage, &key(allocated)).unwrap();
        assert_eq!(
            EntityStorage::logon_of(storage, &key(allocated)).unwrap(),
            None
        );
    });
}
