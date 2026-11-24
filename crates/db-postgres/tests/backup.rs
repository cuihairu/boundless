//! The backup acceptance cases (BW-09-07-04): a backup exported from the
//! source database and restored into an empty target ends up with the
//! same records — entity rows (unicode, binary, vector and nested
//! fields, explicit ids included), the account mapping, the fixed
//! tables' defaults, and the next allocatable DBID the source would
//! have handed out next.
//!
//! The reference has no restore half at all (the datadir files come
//! back by operator hand, `snapshot_helper` only ships them), so the
//! restore contract and the record check are this port's own: `psql`
//! replays the `pg_dump` file into an empty database, and the cases
//! compare the two databases through the storage contract itself. The
//! same server gate as `dbid.rs`: `BOUNDLESS_PG_URL`, the CI storage
//! service's `CI_STORE_PG_*`, or a local unix socket; two private
//! databases and one dump file per case, dropped when it finishes.

use std::sync::atomic::{AtomicU32, Ordering};

use db_postgres::{AccountRow, EntitySchema, PostgresStorage, export_database, restore_database};
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

/// Runs `body` against two fresh private databases — the backup source
/// and the empty restore target — handing it both connection strings,
/// one storage each, and the dump file path. The databases and the dump
/// file are dropped when the body returns.
fn with_source_and_empty_target(
    body: impl FnOnce(&str, &mut PostgresStorage, &str, &mut PostgresStorage, &str),
) {
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
    let run = TEST_DB_COUNTER.fetch_add(1, Ordering::SeqCst);
    let source_db = format!("boundless_pg_bk_src_{}_{}", std::process::id(), run);
    let target_db = format!("boundless_pg_bk_dst_{}_{}", std::process::id(), run);
    let quoted_source = source_db.replace('"', "\"\"");
    let quoted_target = target_db.replace('"', "\"\"");
    let mut created = true;
    for database in [&quoted_source, &quoted_target] {
        if let Err(err) = admin
            .client_mut()
            .execute(format!("CREATE DATABASE \"{database}\"").as_str(), &[])
        {
            eprintln!("skipping: cannot create the test databases ({err})");
            created = false;
            break;
        }
    }
    if !created {
        cleanup(&mut admin, &quoted_source, &quoted_target, None);
        return;
    }
    let source_conninfo = format!("{admin_conninfo} dbname={source_db}");
    let target_conninfo = format!("{admin_conninfo} dbname={target_db}");
    let dump_path = std::env::temp_dir()
        .join(format!(
            "boundless_pg_bk_{}_{}.sql",
            std::process::id(),
            run
        ))
        .to_string_lossy()
        .into_owned();
    let result = PostgresStorage::connect(&source_conninfo).and_then(|mut source| {
        PostgresStorage::connect(&target_conninfo).map(|mut target| {
            body(
                &source_conninfo,
                &mut source,
                &target_conninfo,
                &mut target,
                &dump_path,
            );
        })
    });
    cleanup(&mut admin, &quoted_source, &quoted_target, Some(&dump_path));
    if let Err(err) = result {
        panic!("the fresh databases would not open: {err}");
    }
}

/// Drops the case's databases and dump file; best effort, so a failing
/// case still runs its cleanup.
fn cleanup(admin: &mut PostgresStorage, source: &str, target: &str, dump: Option<&str>) {
    for database in [source, target] {
        let _ = admin.client_mut().execute(
            format!("DROP DATABASE \"{database}\" WITH (FORCE)").as_str(),
            &[],
        );
    }
    if let Some(dump) = dump {
        let _ = std::fs::remove_file(dump);
    }
}

/// The sample entity row: a unicode name, a binary blob, a vector and a
/// nested sequence — the field-level set the field-level coverage items
/// round-trip.
fn monster_values() -> Vec<PropertyValue> {
    vec![
        PropertyValue::String("怪物-ø-漢".into()),
        PropertyValue::Blob(vec![0, 255, 1, 254]),
        PropertyValue::Vector(vec![1.5, -2.25, 3.0]),
        PropertyValue::Sequence(vec![PropertyValue::Int(4), PropertyValue::Int(5)]),
    ]
}

fn monster_schema() -> EntitySchema {
    EntitySchema::for_record(
        "Monster",
        &["name", "secret", "position", "loot"],
        &monster_values(),
    )
    .unwrap()
}

/// Reads one entity row through the contract.
fn read(storage: &mut PostgresStorage, schema: &EntitySchema, db_id: i64) -> db::EntityRecord {
    storage
        .get_entity(schema, &db::EntityKey { type_id: 0, db_id })
        .unwrap()
        .unwrap_or_else(|| panic!("row {db_id} is missing"))
}

/// The acceptance: the restore replays every record — the created row,
/// the explicitly inserted one, the account mapping, the fixed tables'
/// default — and the next allocatable DBID lines up, because the dump
/// carried the allocation sequence.
#[test]
fn a_restore_into_an_empty_database_replays_every_record() {
    with_source_and_empty_target(
        |source_conninfo, source, target_conninfo, target, dump_path| {
            // The source: the fixed tables with their version seed, an
            // entity table with a create-allocated row and an explicitly
            // inserted one, and the account mapping for the created entity.
            db_postgres::schema_init::apply(source.client_mut(), &db_postgres::schema_init::plan())
                .unwrap();
            let schema = monster_schema();
            source.create_entity_table(&schema).unwrap();
            let created = source.create_entity(&schema, &monster_values()).unwrap();
            let explicit_id = 77;
            let mut explicit = db::EntityRecord::new(0);
            explicit.db_id = explicit_id;
            explicit.values = monster_values();
            source.put_entity(&schema, &explicit).unwrap();
            source
                .put_account(&AccountRow {
                    log_on_name: b"alice".to_vec(),
                    password: b"secret".to_vec(),
                    entity_type: 0,
                    entity_id: created,
                })
                .unwrap();

            export_database(source_conninfo, dump_path).unwrap();
            restore_database(target_conninfo, dump_path).unwrap();

            // Both entity rows read back identical from both databases.
            for db_id in [created, explicit_id] {
                let from_source = read(source, &schema, db_id);
                let from_target = read(target, &schema, db_id);
                assert_eq!(from_source.values, from_target.values);
            }

            // The account mapping follows the entity.
            let account_on_source = source.get_account(b"alice").unwrap().unwrap();
            let account_on_target = target.get_account(b"alice").unwrap().unwrap();
            assert_eq!(account_on_source, account_on_target);
            assert_eq!(account_on_target.entity_id, created);

            // The fixed tables' default survived: the seed row never wrote
            // isPasswordHashed, so the value is the DDL default on both
            // sides.
            let defaults = |storage: &mut PostgresStorage| {
                storage
                    .client_mut()
                    .query_one(
                        "SELECT \"version\", \"isPasswordHashed\" FROM \"bigworldInfo\"",
                        &[],
                    )
                    .unwrap()
            };
            let on_source = defaults(source);
            let on_target = defaults(target);
            assert_eq!(
                on_source.get::<_, i64>(0),
                on_target.get::<_, i64>(0),
                "the version row drifted"
            );
            assert!(!on_target.get::<_, bool>(1));

            // The next allocatable DBID lines up, and a create on the
            // restored database really takes it — without colliding with
            // anything the source already had.
            let next_on_source = source.next_allocatable_id(&schema).unwrap();
            let next_on_target = target.next_allocatable_id(&schema).unwrap();
            assert_eq!(
                next_on_source, next_on_target,
                "the allocation state did not travel"
            );
            assert_eq!(
                target.create_entity(&schema, &monster_values()).unwrap(),
                next_on_target
            );
            assert_ne!(next_on_target, created);
            assert_ne!(next_on_target, explicit_id);
        },
    );
}

/// The empty-target contract: a plain dump has no `DROP`/`IF NOT
/// EXISTS`, so replaying it into a database that already holds the
/// tables fails loudly instead of half-restoring, and the target keeps
/// its own state.
#[test]
fn a_restore_into_a_used_database_fails() {
    with_source_and_empty_target(
        |source_conninfo, source, target_conninfo, target, dump_path| {
            db_postgres::schema_init::apply(source.client_mut(), &db_postgres::schema_init::plan())
                .unwrap();
            export_database(source_conninfo, dump_path).unwrap();
            db_postgres::schema_init::apply(target.client_mut(), &db_postgres::schema_init::plan())
                .unwrap();

            let err = restore_database(target_conninfo, dump_path).unwrap_err();
            assert!(
                err.to_string()
                    .contains("psql (the database restore) failed with")
            );

            // The replay stopped at the first colliding statement: the
            // target's own version row is still the only one.
            assert_eq!(
                db_postgres::schema_init::read_version(target.client_mut()).unwrap(),
                Some(db_postgres::schema_init::CURRENT_VERSION)
            );
        },
    );
}
