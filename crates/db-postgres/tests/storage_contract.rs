//! The storage-contract acceptance case (BW-09-06-05): the entity
//! read/write/delete/lookup operations of the shared `EntityStorage`
//! contract run unchanged against this backend.
//!
//! The contract's cases first ran on the in-memory backend
//! (`crates/db/src/storage.rs`), then on the XML backend; this suite runs
//! the same case set against PostgreSQL: the create leg allocates a
//! fresh id, the update leg rewrites an existing row and refuses a
//! missing one, the explicit-id insert keeps the original row on a
//! duplicate, a delete removes the row exactly once, the logon record is
//! a single checkout per entity that dies with the row, `all_entities`
//! lists every row in key order, and an unregistered type folds into
//! each operation's own "no row" outcome. Each case gets its own private
//! database, dropped when the case finishes.

use std::sync::atomic::{AtomicU32, Ordering};

use db::{BaseMailbox, CheckOut, DbError, EntityKey, EntityRecord, EntityStorage};
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

/// Runs `body` against a fresh private database — the fixed tables
/// included, because the contract's logon-record face writes
/// `bigworldLogOns` — or skips when no PostgreSQL server is reachable.
/// The database is dropped when the body returns.
fn with_contract_db(body: impl FnOnce(&mut PostgresStorage)) {
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
    let result = PostgresStorage::connect(&conninfo).and_then(|mut storage| {
        db_postgres::schema_init::apply(storage.client_mut(), &db_postgres::schema_init::plan())?;
        body(&mut storage);
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

/// The two registered types the cases run against: an agent with two
/// properties and a pet with one, registered the way the DBApp registers
/// its entity mappings at startup.
const AGENT_TYPE: u16 = 9;
const PET_TYPE: u16 = 12;

fn agent_values(name: &str, level: i64) -> Vec<PropertyValue> {
    vec![
        PropertyValue::String(name.into()),
        PropertyValue::Int(level),
        PropertyValue::Blob(vec![1, 2, 3]),
    ]
}

fn pet_values(name: &str) -> Vec<PropertyValue> {
    vec![PropertyValue::String(name.into())]
}

/// Registers both types and creates their tables (the DBApp's startup
/// face: the sync pass makes the tables, the mappings index them).
fn register_types(storage: &mut PostgresStorage) {
    let agent_schema =
        EntitySchema::for_record("Agent", &["name", "level", "pack"], &agent_values("a", 1))
            .unwrap();
    storage.create_entity_table(&agent_schema).unwrap();
    storage.register_entity_type(AGENT_TYPE, &agent_schema);

    let pet_schema = EntitySchema::for_record("Pet", &["name"], &pet_values("p")).unwrap();
    storage.create_entity_table(&pet_schema).unwrap();
    storage.register_entity_type(PET_TYPE, &pet_schema);
}

/// A create through the contract: a pending id comes back as the row's
/// fresh, never-used key, and the row reads back whole.
#[test]
fn a_create_allocates_a_fresh_id_and_the_row_reads_back() {
    with_contract_db(|storage| {
        register_types(storage);
        let values = agent_values("cui", 30);

        let db_id = storage
            .put_entity(
                &EntityKey {
                    type_id: AGENT_TYPE,
                    db_id: PENDING_DATABASE_ID,
                },
                &values,
                false,
            )
            .expect("the create leg allocates");
        assert!(db_id > 0);

        let key = EntityKey {
            type_id: AGENT_TYPE,
            db_id,
        };
        let record = EntityStorage::get_entity(storage, &key)
            .expect("the read works")
            .expect("the created row is there");
        assert_eq!(
            record,
            EntityRecord {
                type_id: AGENT_TYPE,
                db_id,
                values,
            }
        );

        // A second create never repeats the id.
        let second = storage
            .put_entity(
                &EntityKey {
                    type_id: AGENT_TYPE,
                    db_id: PENDING_DATABASE_ID,
                },
                &agent_values("meng", 31),
                false,
            )
            .expect("the second create allocates");
        assert_ne!(second, db_id);
    });
}

/// An update rewrites the row in place; a missing row is the contract's
/// "not found", not a create.
#[test]
fn an_update_rewrites_the_row_and_a_missing_row_is_not_found() {
    with_contract_db(|storage| {
        register_types(storage);
        let key = storage
            .put_entity(
                &EntityKey {
                    type_id: AGENT_TYPE,
                    db_id: PENDING_DATABASE_ID,
                },
                &agent_values("cui", 30),
                false,
            )
            .unwrap();
        let key = EntityKey {
            type_id: AGENT_TYPE,
            db_id: key,
        };

        storage
            .put_entity(&key, &agent_values("cui", 31), false)
            .expect("the update rewrites");
        let record = EntityStorage::get_entity(storage, &key).unwrap().unwrap();
        assert_eq!(record.values, agent_values("cui", 31));

        let missing = EntityKey {
            type_id: AGENT_TYPE,
            db_id: 4242,
        };
        let err = storage
            .put_entity(&missing, &agent_values("cui", 1), false)
            .expect_err("a missing row is not found");
        assert_eq!(err, DbError::NotFound);
    });
}

/// The explicit-id insert places a row under a chosen key; a duplicate
/// fails the put and the original row survives.
#[test]
fn an_explicit_insert_places_the_row_and_a_duplicate_keeps_the_original() {
    with_contract_db(|storage| {
        register_types(storage);
        let key = EntityKey {
            type_id: AGENT_TYPE,
            db_id: 500,
        };
        storage
            .put_entity(&key, &agent_values("imported", 7), true)
            .expect("the explicit insert places the row");
        assert_eq!(
            EntityStorage::get_entity(storage, &key)
                .unwrap()
                .unwrap()
                .values,
            agent_values("imported", 7)
        );

        let err = storage
            .put_entity(&key, &agent_values("usurper", 8), true)
            .expect_err("a duplicate id fails");
        assert_eq!(err, DbError::AlreadyExists);
        // The original row is untouched.
        assert_eq!(
            EntityStorage::get_entity(storage, &key)
                .unwrap()
                .unwrap()
                .values,
            agent_values("imported", 7)
        );
    });
}

/// A delete removes the row exactly once, and nothing reads back after.
#[test]
fn a_delete_removes_the_row_and_a_repeat_is_not_found() {
    with_contract_db(|storage| {
        register_types(storage);
        let db_id = storage
            .put_entity(
                &EntityKey {
                    type_id: PET_TYPE,
                    db_id: PENDING_DATABASE_ID,
                },
                &pet_values("rex"),
                false,
            )
            .unwrap();
        let key = EntityKey {
            type_id: PET_TYPE,
            db_id,
        };

        storage.del_entity(&key).expect("the first delete works");
        assert_eq!(EntityStorage::get_entity(storage, &key).unwrap(), None);
        let err = storage
            .del_entity(&key)
            .expect_err("the second delete is not found");
        assert_eq!(err, DbError::NotFound);
    });
}

/// The logon record is a single checkout per entity: the first grant
/// records the holder, a second claim reports that holder and leaves the
/// record alone, and the release empties it.
#[test]
fn the_checkout_grants_once_then_reports_the_holder() {
    with_contract_db(|storage| {
        register_types(storage);
        let db_id = storage
            .put_entity(
                &EntityKey {
                    type_id: AGENT_TYPE,
                    db_id: PENDING_DATABASE_ID,
                },
                &agent_values("cui", 30),
                false,
            )
            .unwrap();
        let key = EntityKey {
            type_id: AGENT_TYPE,
            db_id,
        };

        let first = BaseMailbox {
            addr: Address {
                ip: 0x0102_0304,
                port: 0x1234,
                salt: 1,
            },
            entity_id: 77,
        };
        assert_eq!(
            storage.check_out(&key, first.clone()).unwrap(),
            CheckOut::Granted
        );
        // The holder reads back through the network-order round trip.
        assert_eq!(storage.logon_of(&key).unwrap(), Some(first.clone()));

        // A second claim — even from the same base — reports the holder
        // and never creates a second record.
        let second = BaseMailbox {
            addr: Address {
                ip: 0x0102_0304,
                port: 0x1234,
                salt: 99, // the salt never counts
            },
            entity_id: 78,
        };
        match storage.check_out(&key, second).unwrap() {
            CheckOut::Elsewhere(holder) => {
                assert_eq!(holder, first);
                assert_eq!(holder.addr.salt, 1);
            }
            other => panic!("the checkout was granted twice: {other:?}"),
        }
        assert_eq!(storage.logon_of(&key).unwrap(), Some(first));

        storage.check_in(&key).expect("the release works");
        assert_eq!(storage.logon_of(&key).unwrap(), None);
        // Releasing nothing held is fine.
        storage.check_in(&key).expect("the second release works");
    });
}

/// No log-on record ever points at a missing row: a checkout of an
/// absent entity is refused.
#[test]
fn a_checkout_refuses_a_missing_row() {
    with_contract_db(|storage| {
        register_types(storage);
        let mailbox = BaseMailbox {
            addr: Address {
                ip: 1,
                port: 2,
                salt: 3,
            },
            entity_id: 1,
        };
        let key = EntityKey {
            type_id: AGENT_TYPE,
            db_id: 909,
        };
        let err = storage
            .check_out(&key, mailbox)
            .expect_err("the checkout is refused");
        assert_eq!(err, DbError::NotFound);
        assert_eq!(storage.logon_of(&key).unwrap(), None);
    });
}

/// The reference's `delEntity` removes the log-on record with the row.
#[test]
fn deleting_the_row_releases_its_logon_record() {
    with_contract_db(|storage| {
        register_types(storage);
        let db_id = storage
            .put_entity(
                &EntityKey {
                    type_id: AGENT_TYPE,
                    db_id: PENDING_DATABASE_ID,
                },
                &agent_values("cui", 30),
                false,
            )
            .unwrap();
        let key = EntityKey {
            type_id: AGENT_TYPE,
            db_id,
        };
        let mailbox = BaseMailbox {
            addr: Address {
                ip: 1,
                port: 2,
                salt: 3,
            },
            entity_id: 5,
        };
        storage.check_out(&key, mailbox).unwrap();
        storage.del_entity(&key).expect("the delete works");
        assert_eq!(storage.logon_of(&key).unwrap(), None);
    });
}

/// `all_entities` lists every row of every registered type in
/// `(type_id, db_id)` order.
#[test]
fn all_entities_lists_the_registered_types_in_key_order() {
    with_contract_db(|storage| {
        register_types(storage);
        let pet = storage
            .put_entity(
                &EntityKey {
                    type_id: PET_TYPE,
                    db_id: PENDING_DATABASE_ID,
                },
                &pet_values("rex"),
                false,
            )
            .unwrap();
        let agent_a = storage
            .put_entity(
                &EntityKey {
                    type_id: AGENT_TYPE,
                    db_id: PENDING_DATABASE_ID,
                },
                &agent_values("cui", 30),
                false,
            )
            .unwrap();
        let agent_b = storage
            .put_entity(
                &EntityKey {
                    type_id: AGENT_TYPE,
                    db_id: PENDING_DATABASE_ID,
                },
                &agent_values("meng", 31),
                false,
            )
            .unwrap();

        let records = storage.all_entities().expect("the listing works");
        let keys: Vec<(u16, i64)> = records.iter().map(|r| (r.type_id, r.db_id)).collect();
        assert_eq!(
            keys,
            vec![
                (AGENT_TYPE, agent_a.min(agent_b)),
                (AGENT_TYPE, agent_a.max(agent_b)),
                (PET_TYPE, pet),
            ]
        );
        let values: Vec<Vec<PropertyValue>> = records.iter().map(|r| r.values.clone()).collect();
        assert_eq!(
            values,
            vec![
                agent_values("cui", 30),
                agent_values("meng", 31),
                pet_values("rex"),
            ]
        );
    });
}

/// An unregistered type has no table, which each operation folds into
/// its own outcome: a get answers "no row", a put fails the way the XML
/// backend rejects an unknown type, a delete and a checkout report "not
/// found".
#[test]
fn an_unregistered_type_folds_into_each_operation() {
    with_contract_db(|storage| {
        register_types(storage);
        let key = EntityKey {
            type_id: 77,
            db_id: 1,
        };
        assert_eq!(EntityStorage::get_entity(storage, &key).unwrap(), None);
        assert_eq!(storage.logon_of(&key).unwrap(), None);

        let err = storage
            .put_entity(&key, &pet_values("x"), false)
            .expect_err("the put fails");
        assert!(err.to_string().contains("unknown entity type 77"), "{err}");

        assert_eq!(
            storage.del_entity(&key).expect_err("the delete is refused"),
            DbError::NotFound
        );
        let mailbox = BaseMailbox {
            addr: Address {
                ip: 1,
                port: 2,
                salt: 3,
            },
            entity_id: 1,
        };
        assert_eq!(
            storage
                .check_out(&key, mailbox)
                .expect_err("the checkout is refused"),
            DbError::NotFound
        );
    });
}

/// The contract's storage legs and the schema tools' faces agree on the
/// same table: a row written through the contract reads back through the
/// schema-tool read, and `create_entity_table` twice changes nothing.
#[test]
fn the_contract_and_the_schema_tools_agree_on_the_table() {
    with_contract_db(|storage| {
        register_types(storage);
        // Re-running the table creation is the sync pass's idempotence.
        let agent_schema = storage.schema_of(AGENT_TYPE).unwrap().clone();
        storage.create_entity_table(&agent_schema).unwrap();

        let key = EntityKey {
            type_id: AGENT_TYPE,
            db_id: 600,
        };
        storage
            .put_entity(&key, &agent_values("explicit", 5), true)
            .unwrap();

        // The schema-tool read (the BW-09-06-02 face) sees the row.
        let via_tools = PostgresStorage::get_entity(storage, &agent_schema, &key)
            .expect("the tool read works")
            .expect("the row is there");
        let via_contract = EntityStorage::get_entity(storage, &key).unwrap().unwrap();
        assert_eq!(via_tools, via_contract);
    });
}
