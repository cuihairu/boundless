//! The PostgreSQL acceptance cases (BW-09-06-02): the entity round trips.
//!
//! These cases run against a real PostgreSQL server. The connection
//! string comes from `BOUNDLESS_PG_URL`, or the CI storage service's
//! `CI_STORE_PG_*` settings, or a local server's unix socket; when no
//! server is reachable the cases skip with a notice, the same gate the
//! entitydef differential cases use. Every case gets its own private
//! database, dropped when the case finishes.

use std::sync::atomic::{AtomicU32, Ordering};

use db::{DbError, EntityKey, EntityRecord};
use db_postgres::EntitySchema;
use db_postgres::PostgresStorage;
use entitydef::value::PropertyValue;

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

/// Runs `body` against a fresh private database, or skips when no
/// PostgreSQL server is reachable. The database is dropped when the body
/// returns.
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
    let result = PostgresStorage::connect(&conninfo).map(|mut storage| {
        body(&mut storage);
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

/// The acceptance schema: one column per kind, the values the acceptance
/// case round trips.
fn account_values() -> Vec<PropertyValue> {
    vec![
        // Unicode: astral-plane characters, CJK, control bytes, quotes.
        PropertyValue::String("伤害 π 🎮 \\ \"line\nfeed\" \t\u{1}".into()),
        // Binary: every awkward byte, NULs first and last.
        PropertyValue::Blob(vec![0x00, 0xff, 0xfe, 0x0a, 0x0d, 0x1a, 0x80, 0x00]),
        // Negative and wide integers, signed and unsigned.
        PropertyValue::Int(-9_223_372_036_854_775_000),
        PropertyValue::Uint(9_223_372_036_854_770_000),
        // Floats at the precision edges.
        PropertyValue::Float(std::f64::consts::PI),
        // A vector: three components, exact in REAL.
        PropertyValue::Vector(vec![1.5, -2.25, 0.0]),
        // Nested: a sequence of mixed shapes.
        PropertyValue::Sequence(vec![
            PropertyValue::Int(2),
            PropertyValue::Uint(3),
            PropertyValue::String("four".into()),
            PropertyValue::Blob(vec![0x00, 0xc0]),
            PropertyValue::Vector(vec![f32::MIN_POSITIVE]),
        ]),
        // Nested: a fixed dict with unsorted field order and a nested
        // sequence, plus the unset (`None`) dict leg.
        PropertyValue::FixedDict(Some(vec![
            ("zebra".to_string(), PropertyValue::Float(f64::NEG_INFINITY)),
            (
                "alpha".to_string(),
                PropertyValue::MailBox {
                    id: -2147483648,
                    ip: 3232235777,
                    port: 50570,
                    salt: 65535,
                },
            ),
            (
                "middle".to_string(),
                PropertyValue::Sequence(vec![
                    PropertyValue::FixedDict(None),
                    PropertyValue::Float(f64::NAN),
                ]),
            ),
        ])),
        PropertyValue::FixedDict(None),
    ]
}

fn account_schema() -> EntitySchema {
    let names = [
        "name", "secret", "level", "money", "pi", "pos", "loot", "details", "spare",
    ];
    let values = account_values();
    assert_eq!(names.len(), values.len());
    EntitySchema::for_record("Account", &names, &values).unwrap()
}

fn account_record(db_id: i64, values: Vec<PropertyValue>) -> EntityRecord {
    let mut record = EntityRecord::new(0);
    record.db_id = db_id;
    record.values = values;
    record
}

/// `assert_eq!` with the one exception `PropertyValue`'s `PartialEq` carries
/// from `f64`: a `NaN` never equals itself, so the comparison reads the
/// non-finite markers instead.
fn records_agree(record: &EntityRecord, back: &EntityRecord) -> bool {
    fn values_agree(value: &PropertyValue, back: &PropertyValue) -> bool {
        match (value, back) {
            (PropertyValue::Float(a), PropertyValue::Float(b)) => {
                (a.is_nan() && b.is_nan()) || a == b
            }
            (PropertyValue::Sequence(a), PropertyValue::Sequence(b)) => {
                a.len() == b.len() && a.iter().zip(b).all(|(x, y)| values_agree(x, y))
            }
            (PropertyValue::FixedDict(a), PropertyValue::FixedDict(b)) => match (a, b) {
                (None, None) => true,
                (Some(a), Some(b)) => {
                    a.len() == b.len()
                        && a.iter()
                            .zip(b)
                            .all(|((ka, va), (kb, vb))| ka == kb && values_agree(va, vb))
                }
                _ => false,
            },
            _ => value == back,
        }
    }
    record.db_id == back.db_id
        && record.values.len() == back.values.len()
        && record
            .values
            .iter()
            .zip(&back.values)
            .all(|(a, b)| values_agree(a, b))
}

#[test]
fn unicode_binary_and_nested_values_round_trip() {
    with_fresh_db(|storage| {
        let schema = account_schema();
        storage.create_entity_table(&schema).unwrap();
        let record = account_record(7, account_values());
        storage.put_entity(&schema, &record).unwrap();

        let key = EntityKey {
            type_id: 0,
            db_id: 7,
        };
        let back = storage.get_entity(&schema, &key).unwrap().unwrap();
        assert!(records_agree(&record, &back), "back was {back:?}");
    });
}

#[test]
fn a_row_keeps_only_the_last_write() {
    with_fresh_db(|storage| {
        let schema = account_schema();
        storage.create_entity_table(&schema).unwrap();
        let first = account_record(9, account_values());
        storage.put_entity(&schema, &first).unwrap();
        let mut second = account_values();
        second[0] = PropertyValue::String("伤害改 🉐".into());
        second[2] = PropertyValue::Int(42);
        let second = account_record(9, second);
        storage.put_entity(&schema, &second).unwrap();

        let key = EntityKey {
            type_id: 0,
            db_id: 9,
        };
        let back = storage.get_entity(&schema, &key).unwrap().unwrap();
        assert!(records_agree(&second, &back), "back was {back:?}");
    });
}

#[test]
fn a_missing_row_reads_as_none() {
    with_fresh_db(|storage| {
        let schema = account_schema();
        storage.create_entity_table(&schema).unwrap();
        let key = EntityKey {
            type_id: 0,
            db_id: 404,
        };
        assert!(storage.get_entity(&schema, &key).unwrap().is_none());
    });
}

#[test]
fn a_pending_database_id_reads_as_none() {
    with_fresh_db(|storage| {
        let schema = account_schema();
        storage.create_entity_table(&schema).unwrap();
        let key = EntityKey {
            type_id: 0,
            db_id: -1,
        };
        assert!(storage.get_entity(&schema, &key).unwrap().is_none());
    });
}

#[test]
fn the_debug_output_names_the_backend() {
    with_fresh_db(|storage| {
        let debug = format!("{storage:?}");
        assert!(debug.contains("PostgresStorage"));
    });
}

#[test]
fn backend_errors_map_to_the_contract() {
    with_fresh_db(|storage| {
        let client = storage.client_mut();
        client
            .execute("CREATE TABLE probe_uq (name TEXT NOT NULL)", &[])
            .unwrap();
        client
            .execute("CREATE UNIQUE INDEX probe_uq_name ON probe_uq (name)", &[])
            .unwrap();
        client
            .execute("INSERT INTO probe_uq VALUES ('a')", &[])
            .unwrap();

        // A unique-key violation is the contract's duplicate outcome, not
        // a storage fault.
        let err = client
            .execute("INSERT INTO probe_uq VALUES ('a')", &[])
            .unwrap_err();
        assert_eq!(
            db_postgres::store::map_db_error(err),
            DbError::DuplicateAccount
        );

        // Everything else is a storage fault carrying the message.
        let err = client
            .execute("SELECT * FROM a_missing_relation", &[])
            .unwrap_err();
        let mapped = db_postgres::store::map_db_error(err);
        match mapped {
            DbError::Storage(message) => assert!(message.contains("does not exist")),
            other => panic!("expected a storage fault, got {other:?}"),
        }
    });
}
