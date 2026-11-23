//! The schema-update acceptance case (BW-09-07-03): an old sample,
//! written under an older definition, upgrades and stays readable.
//!
//! The old database has a narrower `tbl_Account` (the first definition's
//! column set) and a row in it. The update plan adds the columns the new
//! definition expects; the old row must read back with its own values in
//! the old columns and the neutral unset values in the new ones, and a
//! record written under the new definition must read back exactly.

use std::sync::atomic::{AtomicU32, Ordering};

use db::{EntityKey, EntityRecord};
use db_postgres::{EntitySchema, PostgresStorage, upgrade};
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
        "boundless_pg_upgrade_{}_{}",
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

/// The old definition: one `TEXT` column, the way an earlier entity
/// version mapped it.
fn old_schema() -> EntitySchema {
    let values = vec![PropertyValue::String("old".into())];
    EntitySchema::for_record("Account", &["name"], &values).unwrap()
}

/// The new definition: the old column plus three kinds of new property.
fn new_schema() -> EntitySchema {
    let values = vec![
        PropertyValue::String("old".into()),
        PropertyValue::Int(0),
        PropertyValue::Sequence(vec![PropertyValue::Int(1)]),
        PropertyValue::Vector(vec![1.0, 2.0, 3.0]),
    ];
    EntitySchema::for_record("Account", &["name", "level", "loot", "pos"], &values).unwrap()
}

fn account_key(db_id: i64) -> EntityKey {
    EntityKey { type_id: 0, db_id }
}

#[test]
fn an_old_sample_upgrades_and_stays_readable() {
    with_fresh_db(|storage| {
        // The old database: narrow table, one old sample row.
        storage.create_entity_table(&old_schema()).unwrap();
        let mut old_record = EntityRecord::new(0);
        old_record.db_id = 1;
        old_record.values = vec![PropertyValue::String("老档案 📜".into())];
        storage.put_entity(&old_schema(), &old_record).unwrap();

        // The update plan and its application.
        let plan = upgrade::plan_schema_update(storage.client_mut(), &new_schema()).unwrap();
        assert_eq!(plan.len(), 3, "three new columns expected: {plan:?}");
        upgrade::apply_schema_update(storage.client_mut(), &plan).unwrap();

        // The old row reads back under the new schema: its own name in
        // the old column, the neutral unset values in the new ones.
        let back = storage
            .get_entity(&new_schema(), &account_key(1))
            .unwrap()
            .expect("the old sample must survive the upgrade");
        assert_eq!(back.values[0], PropertyValue::String("老档案 📜".into()));
        assert_eq!(back.values[1], PropertyValue::Int(0));
        assert_eq!(back.values[2], PropertyValue::FixedDict(None));
        assert_eq!(back.values[3], PropertyValue::Vector(vec![]));

        // The plan is now empty: the table is current.
        assert!(
            upgrade::plan_schema_update(storage.client_mut(), &new_schema())
                .unwrap()
                .is_empty()
        );

        // A record written under the new definition round trips exactly.
        let values = vec![
            PropertyValue::String("新档 ✨".into()),
            PropertyValue::Int(-42),
            PropertyValue::Sequence(vec![PropertyValue::Uint(7)]),
            PropertyValue::Vector(vec![0.5, -1.5, 2.5]),
        ];
        let mut new_record = EntityRecord::new(0);
        new_record.db_id = 2;
        new_record.values = values;
        storage.put_entity(&new_schema(), &new_record).unwrap();
        let back = storage
            .get_entity(&new_schema(), &account_key(2))
            .unwrap()
            .unwrap();
        assert_eq!(back, new_record);

        // And the old sample is still there, alongside it.
        assert!(
            storage
                .get_entity(&new_schema(), &account_key(1))
                .unwrap()
                .is_some()
        );
    });
}

#[test]
fn a_missing_table_plans_a_create() {
    with_fresh_db(|storage| {
        // The table does not exist at all: the plan is the create.
        let plan = upgrade::plan_schema_update(storage.client_mut(), &new_schema()).unwrap();
        assert_eq!(plan.len(), 1);
        assert!(plan[0].starts_with("CREATE TABLE IF NOT EXISTS \"tbl_Account\""));
        upgrade::apply_schema_update(storage.client_mut(), &plan).unwrap();

        // After the create the plan is empty.
        assert!(
            upgrade::plan_schema_update(storage.client_mut(), &new_schema())
                .unwrap()
                .is_empty()
        );
    });
}

#[test]
fn an_unchanged_table_plans_nothing() {
    with_fresh_db(|storage| {
        storage.create_entity_table(&new_schema()).unwrap();
        let plan = upgrade::plan_schema_update(storage.client_mut(), &new_schema()).unwrap();
        assert!(plan.is_empty());
    });
}
