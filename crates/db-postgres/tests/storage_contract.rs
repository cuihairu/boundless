//! The shared storage contract cases (`db::EntityStorage`) on PostgreSQL:
//! the same entity read/write/delete/query cases the in-memory backend's
//! unit tests pin, run against a real server (BW-09-06-05, 验收 共用存储
//! 契约用例通过).
//!
//! The reference shapes live in `lib/db_storage_mysql`: a create turns the
//! pending DatabaseID over to the table's key column, an update of a
//! missing row is `isOK=false`, `insertExplicit` refuses a duplicate id,
//! a delete drops the row and its log-on record, and the log-on records
//! are the fixed `bigworldLogOns` table
//! (`entity_type_mapping.cpp:749-816`).
//!
//! These cases run against a real PostgreSQL server. The connection
//! string comes from `BOUNDLESS_PG_URL`, or the CI storage service's
//! `CI_STORE_PG_*` settings, or a local server's unix socket; when no
//! server is reachable the cases skip with a notice. Every case gets its
//! own private database (the fixed tables included), dropped when the
//! case finishes.
//!
//! The schema-shaped inherent methods keep their BW-09-06-02/03 names, so
//! the contract calls here go through the trait:
//! `EntityStorage::put_entity(&mut storage, …)`.

use std::sync::atomic::{AtomicU32, Ordering};

use db::{BaseMailbox, CheckOut, EntityKey, EntityStorage};
use db_postgres::EntitySchema;
use db_postgres::PostgresStorage;
use entitydef::value::PropertyValue;
use foundation::ids::PENDING_DATABASE_ID;
use mercury::endpoint::Address;

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
/// database; the database is dropped when the body returns (a panicking
/// body skips the drop, as in this file's sibling harnesses).
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

/// Registers the `Thing` type under `type_id` and creates its table: one
/// String property.
fn register_thing(storage: &mut PostgresStorage, type_id: u16) -> EntitySchema {
    let values = vec![PropertyValue::String("x".into())];
    let schema = EntitySchema::for_record("Thing", &["name"], &values).unwrap();
    storage.create_entity_table(&schema).unwrap();
    storage.register_entity_type(type_id, &schema);
    schema
}

/// Registers `type_id` with one Int property (the memory backend's
/// contract record shape).
fn register_counter(storage: &mut PostgresStorage, type_id: u16) -> EntitySchema {
    let values = vec![PropertyValue::Int(0)];
    let schema = EntitySchema::for_record("Counter", &["level"], &values).unwrap();
    storage.create_entity_table(&schema).unwrap();
    storage.register_entity_type(type_id, &schema);
    schema
}

fn thing(key_db_id: i64, name: &str) -> (EntityKey, Vec<PropertyValue>) {
    (
        EntityKey {
            type_id: 3,
            db_id: key_db_id,
        },
        vec![PropertyValue::String(name.into())],
    )
}

fn counter(key_db_id: i64, level: i64) -> (EntityKey, Vec<PropertyValue>) {
    (
        EntityKey {
            type_id: 4,
            db_id: key_db_id,
        },
        vec![PropertyValue::Int(level)],
    )
}

fn put(
    storage: &mut PostgresStorage,
    key: &EntityKey,
    values: &[PropertyValue],
    explicit: bool,
) -> Result<i64, db::DbError> {
    EntityStorage::put_entity(storage, key, values, explicit)
}

fn stored_count(storage: &mut PostgresStorage, table: &str) -> i64 {
    let mut client = storage.client_mut();
    client
        .query_one(&format!("SELECT count(*) FROM \"{table}\""), &[])
        .expect("count runs")
        .get::<_, i64>(0)
}

fn mailbox(ip: u32, entity_id: i32) -> BaseMailbox {
    BaseMailbox {
        addr: Address {
            ip,
            port: 30000,
            salt: 7,
        },
        entity_id,
    }
}

/// The contract: a create turns the pending DatabaseID over to the table,
/// and the row it lands in is visible from an independent connection.
#[test]
fn a_create_allocates_a_fresh_id_visible_from_an_independent_connection() {
    with_fresh_db(|storage, connect| {
        register_counter(storage, 4);
        let mut reader = connect();
        register_counter(&mut reader, 4);

        let (key, values) = counter(PENDING_DATABASE_ID, 7);
        let first = put(storage, &key, &values, false).expect("create writes");
        let (key, values) = counter(PENDING_DATABASE_ID, 7);
        let second = put(storage, &key, &values, false).expect("second create writes");
        assert!(second > first, "two creations shared a database id");

        let found = EntityStorage::get_entity(
            &reader,
            &EntityKey {
                type_id: 4,
                db_id: first,
            },
        )
        .expect("independent connection reads")
        .expect("created row exists");
        assert_eq!(found.values, values);
        assert_eq!(stored_count(storage, "tbl_Counter"), 2);
    });
}

/// The contract: an update overwrites the row, visible from an
/// independent connection, and stays one row.
#[test]
fn an_update_overwrites_the_row_visible_from_an_independent_connection() {
    with_fresh_db(|storage, connect| {
        register_counter(storage, 4);
        let mut reader = connect();
        register_counter(&mut reader, 4);

        let (key, values) = counter(PENDING_DATABASE_ID, 1);
        let id = put(storage, &key, &values, false).expect("create writes");

        let (key, values) = counter(id, 2);
        let updated = put(storage, &key, &values, false).expect("update writes");
        assert_eq!(updated, id);
        assert_eq!(stored_count(storage, "tbl_Counter"), 1);

        let found = EntityStorage::get_entity(
            &reader,
            &EntityKey {
                type_id: 4,
                db_id: id,
            },
        )
        .expect("read")
        .expect("updated row exists");
        assert_eq!(found.values, vec![PropertyValue::Int(2)]);
    });
}

/// The contract: an update of a missing row is `isOK=false`, not a
/// created row (the upsert would create — the existence check is what
/// makes the leg).
#[test]
fn an_update_of_a_missing_row_is_not_found() {
    with_fresh_db(|storage, _connect| {
        register_counter(storage, 4);

        let (key, values) = counter(4242, 1);
        let err = put(storage, &key, &values, false).expect_err("missing row update fails");
        assert_eq!(err, db::DbError::NotFound);
        assert_eq!(stored_count(storage, "tbl_Counter"), 0);
    });
}

/// The contract: an explicit id inserts under that id, and a duplicate
/// explicit id fails with the original row intact (`insertExplicit`).
#[test]
fn an_explicit_id_inserts_under_that_id_and_rejects_duplicates() {
    with_fresh_db(|storage, _connect| {
        register_counter(storage, 4);
        let (key, values) = counter(7, 1);

        let id = put(storage, &key, &values, true).expect("explicit insert writes");
        assert_eq!(id, 7);

        let (_, values) = counter(7, 9);
        let err = put(storage, &key, &values, true).expect_err("duplicate explicit insert");
        assert_eq!(err, db::DbError::AlreadyExists);
        let found = EntityStorage::get_entity(storage, &counter(7, 0).0)
            .expect("read")
            .expect("row exists");
        assert_eq!(found.values, vec![PropertyValue::Int(1)]);
    });
}

/// The contract: created ids clear the seeded rows — MySQL's
/// `AUTO_INCREMENT` counter climbs past an explicit id, so after seeding
/// 5 the next created entity gets 6 (the memory and XML backends pin the
/// same shape from `maxID_`). PostgreSQL's identity sequence does not
/// move on explicit inserts, so the port re-seats it.
#[test]
fn created_ids_clear_the_seeded_rows() {
    with_fresh_db(|storage, _connect| {
        register_counter(storage, 4);
        let (key, values) = counter(5, 1);
        put(storage, &key, &values, true).expect("seed writes");

        let (key, values) = counter(PENDING_DATABASE_ID, 1);
        let id = put(storage, &key, &values, false).expect("create writes");
        assert_eq!(id, 6, "the sequence must sit past the seeded row");
    });
}

/// The contract: a write through one connection is visible from the
/// others, and so is a delete.
#[test]
fn a_write_and_a_delete_through_one_connection_are_visible_from_the_others() {
    with_fresh_db(|storage, connect| {
        register_counter(storage, 4);
        let mut writer = connect();
        let mut reader = connect();
        register_counter(&mut writer, 4);
        register_counter(&mut reader, 4);

        let (key, values) = counter(PENDING_DATABASE_ID, 3);
        let written = put(&mut writer, &key, &values, false).expect("connection write");

        let key = EntityKey {
            type_id: 4,
            db_id: written,
        };
        assert_eq!(
            EntityStorage::get_entity(&reader, &key)
                .expect("read")
                .expect("row exists")
                .values,
            vec![PropertyValue::Int(3)]
        );

        EntityStorage::del_entity(&mut writer, &key).expect("delete through connection");
        assert_eq!(
            EntityStorage::get_entity(&reader, &key).expect("read"),
            None
        );
        assert_eq!(
            EntityStorage::get_entity(storage, &key).expect("read"),
            None
        );
    });
}

/// The contract: a deleted row stops matching, and a repeated delete
/// matches that (`isOK=false`).
#[test]
fn a_deleted_row_stops_matching_and_a_repeat_delete_matches_that() {
    with_fresh_db(|storage, _connect| {
        register_thing(storage, 3);
        let (key, values) = thing(1001, "hi");
        put(storage, &key, &values, true).expect("seed writes");

        EntityStorage::del_entity(storage, &key).expect("first delete succeeds");
        assert_eq!(
            EntityStorage::get_entity(storage, &key).expect("read"),
            None
        );
        let err = EntityStorage::del_entity(storage, &key).expect_err("repeat delete");
        assert_eq!(err, db::DbError::NotFound);
    });
}

/// The contract: a delete only takes its own row — a different type id is
/// a different table, and the id does not exist there.
#[test]
fn a_delete_only_takes_its_own_row() {
    with_fresh_db(|storage, _connect| {
        register_thing(storage, 3);
        register_counter(storage, 4);
        let (key, values) = thing(1001, "kept");
        put(storage, &key, &values, true).expect("seed writes");
        let (other, other_values) = counter(1002, 5);
        put(storage, &other, &other_values, true).expect("seed the other type");

        let (wrong_type, _) = counter(1001, 0);
        let err = EntityStorage::del_entity(storage, &wrong_type).expect_err("type-scoped delete");
        assert_eq!(err, db::DbError::NotFound);
        assert!(
            EntityStorage::get_entity(storage, &key)
                .expect("read")
                .is_some(),
            "the type-scoped delete must not take the other type's row"
        );

        EntityStorage::del_entity(storage, &other).expect("delete the type-4 row");
        assert_eq!(
            EntityStorage::get_entity(storage, &other).expect("read"),
            None
        );
        assert!(
            EntityStorage::get_entity(storage, &key)
                .expect("read")
                .is_some()
        );
    });
}

/// The contract: a checkout grants for a stored, unoccupied row; a second
/// logon is told where the entity lives and never leaves a second
/// record.
#[test]
fn concurrent_logons_never_double_occupy() {
    with_fresh_db(|storage, connect| {
        register_counter(storage, 4);
        let (key, values) = counter(1001, 1);
        put(storage, &key, &values, true).expect("seed writes");

        let mut first = connect();
        let mut second = connect();
        register_counter(&mut first, 4);
        register_counter(&mut second, 4);
        assert_eq!(
            EntityStorage::check_out(&mut first, &key, mailbox(0x0A00_0001, 42))
                .expect("first logon"),
            CheckOut::Granted
        );
        assert_eq!(
            EntityStorage::check_out(&mut second, &key, mailbox(0x0A00_0002, 43))
                .expect("occupied checkout reports, not fails"),
            CheckOut::Elsewhere(mailbox(0x0A00_0001, 42))
        );
        assert_eq!(
            EntityStorage::check_out(&mut second, &key, mailbox(0x0A00_0003, 44))
                .expect("still occupied"),
            CheckOut::Elsewhere(mailbox(0x0A00_0001, 42))
        );
        // The holder is visible from every connection — including the one
        // that won.
        assert_eq!(
            EntityStorage::logon_of(&first, &key).expect("read"),
            Some(mailbox(0x0A00_0001, 42))
        );
        assert_eq!(
            EntityStorage::logon_of(storage, &key).expect("read"),
            Some(mailbox(0x0A00_0001, 42))
        );
    });
}

/// The contract: a checkout of a missing row is `isOK=false`, a release
/// is unconditional, and the freed entity checks out to a new holder.
#[test]
fn checkin_releases_and_a_later_checkout_grants_again() {
    with_fresh_db(|storage, _connect| {
        register_counter(storage, 4);
        let (key, values) = counter(1001, 1);
        put(storage, &key, &values, true).expect("seed writes");

        let (missing, _) = counter(4242, 0);
        let err = EntityStorage::check_out(storage, &missing, mailbox(1, 1))
            .expect_err("no row to check out");
        assert_eq!(err, db::DbError::NotFound);

        EntityStorage::check_out(storage, &key, mailbox(1, 42)).expect("checkout");
        EntityStorage::check_in(storage, &key).expect("release");
        assert_eq!(EntityStorage::logon_of(storage, &key).expect("read"), None);
        // A release with nothing held is fine — the reference DELETE is
        // unconditional.
        EntityStorage::check_in(storage, &key).expect("release again");

        assert_eq!(
            EntityStorage::check_out(storage, &key, mailbox(2, 43)).expect("re-grant"),
            CheckOut::Granted
        );
    });
}

/// The contract: a successful delete releases the checkout; a failed one
/// (the row is already gone) never touches the log-on record
/// (del_entity_task.cpp:69-76).
#[test]
fn a_successful_delete_releases_its_checkout() {
    with_fresh_db(|storage, _connect| {
        register_counter(storage, 4);
        let (key, values) = counter(1001, 1);
        put(storage, &key, &values, true).expect("seed writes");
        EntityStorage::check_out(storage, &key, mailbox(1, 42)).expect("checkout");

        EntityStorage::del_entity(storage, &key).expect("delete");
        assert_eq!(EntityStorage::logon_of(storage, &key).expect("read"), None);
    });
}

/// The contract: `all_entities` hands over every stored record, sorted by
/// `(type_id, db_id)`, with the values decoded per type.
#[test]
fn all_entities_hands_over_every_record_in_a_deterministic_order() {
    with_fresh_db(|storage, _connect| {
        register_counter(storage, 4);
        register_thing(storage, 3);

        let (key, values) = counter(2, 20);
        put(storage, &key, &values, true).expect("seed");
        let (key, values) = thing(1, "first");
        put(storage, &key, &values, true).expect("seed");
        let (key, values) = counter(9, 90);
        put(storage, &key, &values, true).expect("seed");

        let records = EntityStorage::all_entities(storage).expect("scan");
        let keys: Vec<(u16, i64)> = records
            .iter()
            .map(|record| (record.type_id, record.db_id))
            .collect();
        assert_eq!(keys, vec![(3, 1), (4, 2), (4, 9)]);
        assert_eq!(
            records[0].values,
            vec![PropertyValue::String("first".into())]
        );
        assert_eq!(records[1].values, vec![PropertyValue::Int(20)]);
    });
}

/// The port detail: the base address travels in network byte order
/// (`htonl`/`htons` in, `ntohl`/`ntohs` out —
/// entity_type_mapping.cpp:779-781, :815-816), so the stored columns
/// carry the swapped values.
#[test]
fn the_logon_record_stores_the_address_in_network_byte_order() {
    with_fresh_db(|storage, _connect| {
        register_counter(storage, 4);
        let (key, values) = counter(1001, 1);
        put(storage, &key, &values, true).expect("seed writes");

        EntityStorage::check_out(storage, &key, mailbox(0x0A00_0001, 42)).expect("checkout");

        let mut client = storage.client_mut();
        let row = client
            .query_one(
                "SELECT \"ip\", \"port\", \"salt\", \"objectID\" FROM \"bigworldLogOns\"",
                &[],
            )
            .expect("the log-on row exists");
        assert_eq!(row.get::<_, i64>(0), 0x0100_000A);
        assert_eq!(row.get::<_, i32>(1), 0x3075);
        assert_eq!(row.get::<_, i32>(2), 7);
        assert_eq!(row.get::<_, i32>(3), 42);
    });
}

/// A relogon overwrites the log-on record instead of failing on the
/// duplicate key (`addLogOnQuery` is INSERT ... ON DUPLICATE KEY UPDATE,
/// entity_type_mapping.cpp:749-758; the raw face is
/// `PostgresStorage::put_logon_record` — the contract's `check_out`
/// refuses an occupied entity, the case above).
#[test]
fn a_relogon_overwrites_the_log_on_record() {
    with_fresh_db(|storage, _connect| {
        register_counter(storage, 4);
        let (key, values) = counter(1001, 1);
        put(storage, &key, &values, true).expect("seed writes");
        EntityStorage::check_out(storage, &key, mailbox(0x0A00_0001, 42)).expect("first checkout");

        storage
            .put_logon_record(
                &key,
                &BaseMailbox {
                    addr: Address {
                        ip: 0x0A00_0002,
                        port: 30000,
                        salt: 7,
                    },
                    entity_id: 43,
                },
            )
            .expect("the relogon overwrites");

        assert_eq!(
            EntityStorage::logon_of(storage, &key).expect("read"),
            Some(BaseMailbox {
                addr: Address {
                    ip: 0x0A00_0002,
                    port: 30000,
                    salt: 7,
                },
                entity_id: 43,
            })
        );
    });
}

/// The unknown-type outcomes: a get answers "no row", a put fails with
/// the XML backend's message, a delete and a checkout report "not found".
#[test]
fn an_unregistered_type_answers_the_contract_outcomes() {
    with_fresh_db(|storage, _connect| {
        register_counter(storage, 4);

        let (key, values) = thing(1, "x");
        let key = EntityKey { type_id: 99, ..key };
        assert_eq!(EntityStorage::get_entity(storage, &key).expect("get"), None);
        let err = put(storage, &key, &values, false).expect_err("unknown type put");
        assert!(
            err.to_string().contains("entity type 99 is invalid"),
            "unexpected error: {err}"
        );
        let err = EntityStorage::del_entity(storage, &key).expect_err("unknown type delete");
        assert_eq!(err, db::DbError::NotFound);
        let err = EntityStorage::check_out(storage, &key, mailbox(1, 1))
            .expect_err("unknown type checkout");
        assert_eq!(err, db::DbError::NotFound);
    });
}

/// A registry entry that has drifted from the table it names reaches the
/// generic error arm instead of the duplicate-id arm. The reference keeps
/// registry and table in step (`entity_type_mapping.cpp` updates both at
/// once), so this is the fault face of the explicit write: the statement
/// names a column the relation does not have.
#[test]
fn a_registry_entry_that_drifted_from_its_table_is_a_storage_fault() {
    with_fresh_db(|storage, _connect| {
        let one = vec![PropertyValue::String("x".into())];
        let narrow = EntitySchema::for_record("Drift", &["name"], &one).unwrap();
        storage.create_entity_table(&narrow).unwrap();

        let two = vec![
            PropertyValue::String("x".into()),
            PropertyValue::String("y".into()),
        ];
        let drifted = EntitySchema::for_record("Drift", &["name", "extra"], &two).unwrap();
        assert!(storage.schema_of(9).is_none());
        storage.register_entity_type(9, &drifted);
        assert_eq!(storage.schema_of(9).unwrap().type_name, "Drift");

        let key = EntityKey {
            type_id: 9,
            db_id: 42,
        };
        let err = put(storage, &key, &two, true).expect_err("no extra column");
        assert!(
            err.to_string().contains("does not exist"),
            "unexpected error: {err}"
        );
    });
}

/// A statement against a registered type whose table was never created is
/// a storage fault, mapped through the entity error mapper's non-duplicate
/// arm.
#[test]
fn a_statement_on_a_missing_table_is_a_storage_fault() {
    with_fresh_db(|storage, _connect| {
        let values = vec![PropertyValue::Int(0)];
        let schema = EntitySchema::for_record("Ghost", &["level"], &values).unwrap();
        storage.register_entity_type(5, &schema);
        let (key, values) = (
            EntityKey {
                type_id: 5,
                db_id: 1,
            },
            values,
        );
        let err = put(storage, &key, &values, false).expect_err("no table");
        assert!(
            err.to_string().contains("does not exist"),
            "unexpected error: {err}"
        );
    });
}
