//! Exporting a backup and restoring it into an empty target
//! (BW-09-07-04, 验收 空目标库恢复相同记录).
//!
//! What the restored copy owes is what BW-09-07 checks after an import
//! (docs/operations/database.md): entity ids, account links, binary
//! fields, defaults, **and the next allocatable DBID** — if the next
//! DBID does not line up after a restore, the allocation sequence
//! (BW-09-06-04) was not part of the backup. So the acceptance seeds a
//! source whose Hero seat sits *above* its largest id (allocations a
//! later delete removed — headroom a max(id) restore would lose),
//! exports, restores into an empty target, and compares the two
//! databases record-for-record — including what each identity hands
//! out next.
//!
//! The cases run against a real PostgreSQL server, with the connection
//! string from `BOUNDLESS_PG_URL`, or the CI storage service's
//! `CI_STORE_PG_*` settings, or a local server's unix socket; when no
//! server is reachable they skip with a notice. Every case gets its own
//! private database (the fixed tables included), dropped when the case
//! finishes.

use std::sync::atomic::{AtomicU32, Ordering};

use db::{EntityKey, EntityRecord, EntityStorage};
use db_postgres::{
    AccountRow, EntitySchema, ImportReport, PostgresStorage, export_backup, restore_backup,
};
use entitydef::value::PropertyValue;
use foundation::ids::{DatabaseId, EntityTypeId};

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

fn hero_values() -> Vec<PropertyValue> {
    vec![
        PropertyValue::String(String::new()),
        PropertyValue::Int(0),
        PropertyValue::Blob(vec![]),
    ]
}

fn item_values() -> Vec<PropertyValue> {
    vec![
        PropertyValue::String(String::new()),
        PropertyValue::Vector(vec![]),
        PropertyValue::Blob(vec![]),
    ]
}

fn ghost_values() -> Vec<PropertyValue> {
    vec![PropertyValue::String(String::new())]
}

/// The source types: Hero's Blob carries every awkward byte, Item's
/// Vector carries the empty default shape, Ghost stays empty — its
/// identity never hands anything out, so the backup's seat for it is
/// the virgin 1.
fn seed_source_types(storage: &mut PostgresStorage) -> Vec<(EntityTypeId, EntitySchema)> {
    let mut seeded = Vec::new();
    for (type_id, name, props, values) in [
        (4, "Hero", &["name", "level", "data"][..], hero_values()),
        (9, "Item", &["name", "position", "data"][..], item_values()),
        (5, "Ghost", &["name"][..], ghost_values()),
    ] {
        let schema = EntitySchema::for_record(name, props, &values).unwrap();
        storage.create_entity_table(&schema).unwrap();
        storage.add_entity_type(type_id, name).unwrap();
        storage.register_entity_type(type_id, &schema);
        seeded.push((type_id, schema));
    }
    seeded
}

/// The target's empty-schema state (BW-09-07-01's): tables created and
/// types registered, but no mapping rows and no rows — the mapping rows
/// are the restore's to land.
fn prepare_target(storage: &mut PostgresStorage) -> Vec<(EntityTypeId, EntitySchema)> {
    let mut prepared = Vec::new();
    for (type_id, name, props, values) in [
        (4, "Hero", &["name", "level", "data"][..], hero_values()),
        (9, "Item", &["name", "position", "data"][..], item_values()),
        (5, "Ghost", &["name"][..], ghost_values()),
    ] {
        let schema = EntitySchema::for_record(name, props, &values).unwrap();
        storage.create_entity_table(&schema).unwrap();
        storage.register_entity_type(type_id, &schema);
        prepared.push((type_id, schema));
    }
    prepared
}

/// Registers the types on an independent connection, the way every
/// reader in these cases must before `all_entities` decodes rows.
fn register_reader(storage: &mut PostgresStorage, prepared: &[(EntityTypeId, EntitySchema)]) {
    for (type_id, schema) in prepared {
        storage.register_entity_type(*type_id, schema);
    }
}

fn account(name: &[u8], password: &[u8], entity_id: DatabaseId) -> AccountRow {
    AccountRow {
        log_on_name: name.to_vec(),
        password: password.to_vec(),
        entity_type: 4,
        entity_id,
    }
}

fn key(type_id: EntityTypeId, db_id: DatabaseId) -> EntityKey {
    EntityKey { type_id, db_id }
}

/// A record with the reference's pending DatabaseID: the table has not
/// handed the entity one out.
fn pending_record(type_id: EntityTypeId, values: Vec<PropertyValue>) -> EntityRecord {
    let mut record = EntityRecord::new(type_id);
    record.db_id = -1;
    record.values = values;
    record
}

/// 验收: an empty target database ends up with the same records — every
/// entity id, the account links, the binary fields, the defaults, and
/// what each identity hands out next. The source's Hero seat sits at
/// 9004, above its largest id 9000 (three allocations a delete
/// removed), so a restore that lost the seat would hand out 9001
/// instead of 9004.
#[test]
fn a_backup_restores_into_an_empty_target_with_identical_records() {
    with_fresh_db(|source, source_connect| {
        let seeded = seed_source_types(source);
        let hero = seeded.iter().find(|(t, _)| *t == 4).unwrap().1.clone();
        let item = seeded.iter().find(|(t, _)| *t == 9).unwrap().1.clone();

        // Two Hero rows: one with every awkward byte, one all-defaults.
        let blob: Vec<u8> = [0x00u8, 0x01, 0xFF, 0x7F, b'a', 0x00, 0xFE]
            .into_iter()
            .chain(0..=255u8)
            .collect();
        EntityStorage::put_entity(
            source,
            &key(4, 7),
            &[
                PropertyValue::String("玩家·β".to_string()),
                PropertyValue::Int(3),
                PropertyValue::Blob(blob),
            ],
            true,
        )
        .unwrap();
        EntityStorage::put_entity(source, &key(4, 9000), &hero_values(), true).unwrap();
        // Three allocated-then-deleted rows: the seat climbs to 9004
        // while the largest surviving id is 9000.
        for expected in 9001..=9003 {
            let allocated = source
                .put_new_entity(
                    &hero,
                    &pending_record(
                        4,
                        vec![
                            PropertyValue::String(format!("gone{expected}")),
                            PropertyValue::Int(expected),
                            PropertyValue::Blob(vec![expected as u8]),
                        ],
                    ),
                )
                .unwrap();
            assert_eq!(allocated, expected);
            EntityStorage::del_entity(source, &key(4, allocated)).unwrap();
        }
        // One Item row, allocated by the table (id 1) and kept: the
        // empty-Vector default shape rides along.
        let lantern = pending_record(
            9,
            vec![
                PropertyValue::String("lantern".to_string()),
                PropertyValue::Vector(vec![]),
                PropertyValue::Blob((0..=255u8).rev().collect()),
            ],
        );
        let allocated = source.put_new_entity(&item, &lantern).unwrap();
        assert_eq!(allocated, 1);

        let hero_account = account(b"hero", b"pw\x00\xFF\x7F", 7);
        let item_account = account(b"item\xC3\xBF", b"pw2\x00", 1);
        source.put_account(&hero_account).unwrap();
        source.put_account(&item_account).unwrap();

        let backup = export_backup(source).unwrap();
        // The frozen shape: the mapping table's order (typeID), every
        // row counted, and the seats — Hero 9004 (the deleted
        // allocations' headroom, not 9001), Item 2, Ghost the virgin 1.
        let shapes: Vec<(EntityTypeId, &str, usize, DatabaseId)> = backup
            .entity_types
            .iter()
            .map(|entry| {
                (
                    entry.type_id,
                    entry.name.as_str(),
                    entry.rows.len(),
                    entry.next_dbid,
                )
            })
            .collect();
        assert_eq!(
            shapes,
            vec![(4, "Hero", 2, 9004), (5, "Ghost", 0, 1), (9, "Item", 1, 2)]
        );
        assert_eq!(backup.accounts.len(), 2);

        with_fresh_db(|target, target_connect| {
            let prepared = prepare_target(target);
            let report = restore_backup(target, &backup).unwrap();
            assert_eq!(
                report,
                ImportReport {
                    written: 5,
                    skipped: 0
                }
            );

            // An independent reader on each side, both registered.
            let mut source_reader = source_connect();
            register_reader(&mut source_reader, &prepared);
            let mut target_reader = target_connect();
            register_reader(&mut target_reader, &prepared);

            // Every record, compared across the two databases.
            assert_eq!(
                EntityStorage::all_entities(&source_reader).unwrap(),
                EntityStorage::all_entities(&target_reader).unwrap()
            );
            // The whole frozen shape, re-exported from the restored
            // target: mappings, rows, account links and seats all
            // identical.
            assert_eq!(export_backup(&mut target_reader).unwrap(), backup);

            // The next allocatable DBID, on both sides. Hero is the
            // criterion case: 9004 both sides — a max(id) restore would
            // hand out 9001. Item hands out 2, Ghost the virgin 1.
            for (type_id, values) in [(4, hero_values()), (9, item_values()), (5, ghost_values())] {
                let schema = prepared
                    .iter()
                    .find(|(t, _)| *t == type_id)
                    .unwrap()
                    .1
                    .clone();
                let on_source = source_reader
                    .put_new_entity(&schema, &pending_record(type_id, values.clone()))
                    .unwrap();
                let on_target = target_reader
                    .put_new_entity(&schema, &pending_record(type_id, values))
                    .unwrap();
                assert_eq!(
                    on_source, on_target,
                    "type {type_id}: the restored identity hands out what the source's does"
                );
            }
            assert_eq!(
                backup.entity_types[0].next_dbid, 9004,
                "the Hero seat rode with the backup, above the largest id 9000"
            );
        });
    });
}

/// A re-run of a restore over the restored target lands nothing twice:
/// every row and account is skipped, the mappings are already there,
/// and the re-set seats leave the state — and the next re-export —
/// exactly as the backup froze it (BW-09-07-07's rerun contract riding
/// through the restore).
#[test]
fn a_rerun_of_a_restore_lands_nothing_and_leaves_the_state_alone() {
    with_fresh_db(|source, _source_connect| {
        seed_source_types(source);
        EntityStorage::put_entity(
            source,
            &key(4, 7),
            &[
                PropertyValue::String("warrior".to_string()),
                PropertyValue::Int(3),
                PropertyValue::Blob(vec![0, 255]),
            ],
            true,
        )
        .unwrap();
        let held = account(b"hero", b"pw", 7);
        source.put_account(&held).unwrap();
        let backup = export_backup(source).unwrap();

        with_fresh_db(|target, target_connect| {
            let prepared = prepare_target(target);
            assert_eq!(
                restore_backup(target, &backup).unwrap(),
                ImportReport {
                    written: 2,
                    skipped: 0
                }
            );
            // The re-run: the row and the account already stand with
            // equal content, so both are skipped — nothing lands twice.
            assert_eq!(
                restore_backup(target, &backup).unwrap(),
                ImportReport {
                    written: 0,
                    skipped: 2
                }
            );
            let mut reader = target_connect();
            register_reader(&mut reader, &prepared);
            assert_eq!(export_backup(&mut reader).unwrap(), backup);
        });
    });
}

/// The two named failure shapes: a restore whose backup names a type
/// the target has not registered fails with the type named, and an
/// export whose mapping row points at a table the database never
/// created fails with the table named — the mapping-without-table
/// state a backup refuses to freeze silently.
#[test]
fn restore_names_an_unregistered_type_and_export_names_a_missing_table() {
    with_fresh_db(|source, source_connect| {
        seed_source_types(source);
        EntityStorage::put_entity(
            source,
            &key(4, 7),
            &[
                PropertyValue::String("warrior".to_string()),
                PropertyValue::Int(3),
                PropertyValue::Blob(vec![1]),
            ],
            true,
        )
        .unwrap();
        let backup = export_backup(source).unwrap();

        with_fresh_db(|target, _target_connect| {
            // The target carries the Hero table but never registers the
            // type: the restore lands the mapping row and then refuses
            // to move the rows it cannot decode.
            let schema =
                EntitySchema::for_record("Hero", &["name", "level", "data"], &hero_values())
                    .unwrap();
            target.create_entity_table(&schema).unwrap();
            let err = restore_backup(target, &backup).unwrap_err();
            assert!(err.to_string().contains("entity type 4 is not registered"));

            // The export side: a mapping row whose table was never
            // created has no identity sequence to freeze.
            let mut no_table = source_connect();
            no_table.add_entity_type(11, "Phantom").unwrap();
            let err = export_backup(&mut no_table).unwrap_err();
            assert!(err.to_string().contains("has no identity sequence"));
        });
    });
}
