//! The accounts table acceptance cases (BW-09-06-03): a duplicate log-on
//! name is the contract's "account already exists" error, and the read
//! path joins the mapping back to its entity.
//!
//! The same server gate as `round_trip.rs`: `BOUNDLESS_PG_URL`, the CI
//! storage service's `CI_STORE_PG_*`, or a local unix socket; a private
//! database per case, dropped when the case finishes.

use std::sync::atomic::{AtomicU32, Ordering};

use db::DbError;
use db_postgres::{AccountRow, PostgresStorage};

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
        "boundless_pg_accounts_{}_{}",
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

fn account(name: &str, password: &str, entity_type: u16, entity_id: i64) -> AccountRow {
    AccountRow {
        log_on_name: name.as_bytes().to_vec(),
        password: password.as_bytes().to_vec(),
        entity_type,
        entity_id,
    }
}

#[test]
fn a_duplicate_account_name_is_a_contract_error() {
    with_fresh_db(|storage| {
        storage.create_account_table().unwrap();
        storage
            .put_account(&account("alice", "s3cret", 2, 100))
            .unwrap();

        // The same name a second time: the acceptance case.
        let err = storage
            .put_account(&account("alice", "other", 2, 200))
            .unwrap_err();
        assert_eq!(err, DbError::DuplicateAccount);
        assert_eq!(err.to_string(), "account already exists");

        // The first row is untouched by the rejected insert.
        let row = storage.get_account(b"alice").unwrap().unwrap();
        assert_eq!(row.entity_id, 100);
        assert_eq!(row.password, b"s3cret");

        // A different name still inserts.
        storage.put_account(&account("bob", "pw", 3, 101)).unwrap();
        assert_eq!(storage.get_account(b"bob").unwrap().unwrap().entity_id, 101);
    });
}

#[test]
fn the_name_key_is_bytes_not_text() {
    with_fresh_db(|storage| {
        storage.create_account_table().unwrap();
        // The reference stores the log-on name as VARBINARY, so a name
        // that is not valid UTF-8 is a legal account name.
        let mut row = account("placeholder", "pw", 1, 5);
        row.log_on_name = vec![0xff, 0x00, 0x80];
        storage.put_account(&row).unwrap();
        assert_eq!(
            storage.get_account(&[0xff, 0x00, 0x80]).unwrap().unwrap(),
            row
        );
        // And it is distinct from a different byte sequence.
        assert!(storage.get_account(&[0xff, 0x00, 0x81]).unwrap().is_none());
    });
}

#[test]
fn a_missing_account_reads_as_none() {
    with_fresh_db(|storage| {
        storage.create_account_table().unwrap();
        assert!(storage.get_account(b"nobody").unwrap().is_none());
    });
}

#[test]
fn the_entity_type_and_id_survive_the_round_trip() {
    with_fresh_db(|storage| {
        storage.create_account_table().unwrap();
        // The full ranges of both id columns.
        storage
            .put_account(&account("wide", "pw", u16::MAX, i64::MAX))
            .unwrap();
        storage
            .put_account(&account("negative", "pw", 0, i64::MIN))
            .unwrap();
        let wide = storage.get_account(b"wide").unwrap().unwrap();
        assert_eq!(wide.entity_type, u16::MAX);
        assert_eq!(wide.entity_id, i64::MAX);
        let negative = storage.get_account(b"negative").unwrap().unwrap();
        assert_eq!(negative.entity_type, 0);
        assert_eq!(negative.entity_id, i64::MIN);
    });
}
