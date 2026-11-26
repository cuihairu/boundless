//! Auto-load restore and the archive write-back on the PostgreSQL
//! backend (BW-09-06-11, 验收 重启恢复对应实体).
//!
//! The MySQL baseline is the restart chain of the DBApp:
//!
//! * pending BaseApp secondary databases (the SQLite archives) are
//!   consolidated into the primary first; an archive write lands as the
//!   entity put — an absent row is created with the row's own id, a
//!   present one is updated (`ConsolidateEntityTask` driving `putEntity`,
//!   the same merge the reference's consolidate tool replays).
//! * then `MySqlDatabase::autoLoadEntities` (mysql_database.cpp:669-715)
//!   runs: the log-on records NOT flagged `shouldAutoLoad` are deleted,
//!   the flagged ones are listed for the auto-loader (joined with
//!   `bigworldEntityTypes` for the running-server type id), and when the
//!   list is non-empty the holders are zeroed (`ip = 0, port = 0` —
//!   `objectID` and `salt` stay, the address restarts, the entity does
//!   not).
//! * the flag rides the log-on record, and the relogon upsert
//!   (`addLogOnRecord`, entity_type_mapping.cpp:773-783) touches only
//!   `objectID`/`ip`/`port`/`salt` — so a flagged entity stays flagged
//!   across a relogon.
//! * the flag is written by `EntityTypeMapping::updateAutoLoad`
//!   (entity_type_mapping.cpp:792-800); an entity with no log-on record
//!   matches no rows and the update stays silent.
//! * the `bigworldEntityTypes` row the join needs is maintained by the
//!   sync pass (`TypesCollector::addType`, table_inspector.cpp:250-272,
//!   driving `BigWorldMetaData::getEntityTypeID`/`addEntityType`/
//!   `setEntityTypeID`, bw_meta_data.cpp:42-70): absent by name the row
//!   is added, a different id updates it, the same id is a no-op. The
//!   reference's two type-id spaces — the database-internal `typeID` and
//!   the `entities.xml` `bigworldID` — collapse into the port's one
//!   `EntityTypeId` space, so both columns carry the same value.
//!
//! The cases run against a real PostgreSQL server, with the connection
//! string from `BOUNDLESS_PG_URL`, or the CI storage service's
//! `CI_STORE_PG_*` settings, or a local server's unix socket; when no
//! server is reachable they skip with a notice. Every case gets its own
//! private database (the fixed tables included), dropped when the case
//! finishes.
//!
//! The SQLite reader and the auto-loader driver are the delivered
//! `db::SecondaryDatabase` and `db::auto_load_recovery` modules; on this
//! branch both are still orphan modules (their `lib.rs` declarations are
//! pending the cross-seat revival), so the archive write stands in as the
//! decoded values it consolidates to and a load closure stands in for the
//! loader seam (`reserve`/`addEntity`/`start`).

use std::sync::atomic::{AtomicU32, Ordering};

use db::{BaseMailbox, CheckOut, EntityKey, EntityStorage};
use db_postgres::{EntitySchema, PostgresStorage};
use entitydef::value::PropertyValue;
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

/// Registers `type_id` with one Int property and creates its table, and
/// writes the `bigworldEntityTypes` row the auto-load join needs
/// (the sync pass's `addType`).
fn register_counter(storage: &mut PostgresStorage, type_id: u16) {
    let values = vec![PropertyValue::Int(0)];
    let schema = EntitySchema::for_record("Counter", &["level"], &values).unwrap();
    storage.create_entity_table(&schema).unwrap();
    storage.add_entity_type(type_id, "Counter").unwrap();
    storage.register_entity_type(type_id, &schema);
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

/// The loader seam: every listed entity must still load — the restart
/// restores the corresponding entities (`EntityAutoLoader::start`
/// handing each queued entity to the load sink).
fn loads_every_entity(storage: &mut PostgresStorage, list: &[(u16, i64)]) -> bool {
    list.iter().all(|&(type_id, db_id)| {
        EntityStorage::get_entity(storage, &EntityKey { type_id, db_id })
            .expect("the restored entity reads back")
            .is_some()
    })
}

/// 验收: a restart loads exactly the flagged entities — the stale
/// sessions of the unflagged ones are cleared, the holders of the loaded
/// set are zeroed (`objectID` and `salt` stay), and the unflagged
/// entity's row itself stays put.
#[test]
fn a_restart_loads_exactly_the_flagged_entities() {
    with_fresh_db(|storage, connect| {
        register_counter(storage, 4);
        for (db_id, level) in [(1, 11), (2, 22), (3, 33)] {
            let (key, values) = counter(db_id, level);
            EntityStorage::put_entity(storage, &key, &values, true).expect("the seed row lands");
            storage
                .put_logon_record(&key, &mailbox(0x0100_0000 + db_id as u32, 7))
                .expect("the session is written");
        }
        storage.set_auto_load(&counter(1, 0).0, true).unwrap();
        storage.set_auto_load(&counter(2, 0).0, false).unwrap();
        storage.set_auto_load(&counter(3, 0).0, true).unwrap();

        // The restart: a fresh connection against the same database.
        let mut restart = connect();
        register_counter(&mut restart, 4);

        let mut list = restart.auto_load_entities().expect("the restart list runs");
        list.sort();
        assert_eq!(list, [(4, 1), (4, 3)], "exactly the flagged entities load");

        // The loaded set restores: every listed entity reads back with
        // its own row.
        assert!(
            loads_every_entity(&mut restart, &list),
            "the restart restores the corresponding entities"
        );

        // The loaded holder restarts at the zero address, `objectID` and
        // `salt` untouched.
        assert_eq!(
            EntityStorage::logon_of(&restart, &counter(1, 0).0).expect("the session read runs"),
            Some(BaseMailbox {
                addr: Address {
                    ip: 0,
                    port: 0,
                    salt: 7
                },
                entity_id: 7,
            }),
            "the loaded holder is zeroed, the object and salt stay"
        );

        // The unflagged entity keeps its row but loses its stale session.
        let (key_b, values_b) = counter(2, 22);
        assert_eq!(
            EntityStorage::get_entity(&restart, &key_b).expect("the row read runs"),
            Some(db::EntityRecord {
                type_id: 4,
                db_id: 2,
                values: values_b,
            }),
            "the unflagged row is not loaded but stays put"
        );
        assert_eq!(
            EntityStorage::logon_of(&restart, &key_b).expect("the session read runs"),
            None,
            "the stale session of the unflagged entity is cleared"
        );
    });
}

/// The cleanup leg clears an unflagged session on every restart pass —
/// stale sessions do not survive — while the holder zeroing rides the
/// non-empty list (mysql_database.cpp:693-703: the `UPDATE ip = 0,
/// port = 0` sits inside the `numResults > 0` guard, so nothing listed
/// means nothing zeroed).
#[test]
fn the_cleanup_clears_unflagged_sessions_and_zeroing_rides_the_list() {
    with_fresh_db(|storage, connect| {
        register_counter(storage, 4);
        let (key, values) = counter(1, 11);
        EntityStorage::put_entity(storage, &key, &values, true).expect("the seed row lands");
        let holder = mailbox(0x0100_000a, 7);
        storage
            .put_logon_record(&key, &holder)
            .expect("the session is written");

        // Nothing flagged: the list is empty, and the unflagged session
        // is gone — the cleanup leg runs before the listing.
        let mut restart = connect();
        register_counter(&mut restart, 4);
        assert_eq!(
            restart.auto_load_entities().expect("the restart list runs"),
            [],
            "nothing flagged, nothing loads"
        );
        assert_eq!(
            EntityStorage::logon_of(&restart, &key).expect("the session read runs"),
            None,
            "the unflagged session is cleared by the pass"
        );

        // Flagged and relogged-on: the next pass lists the entity and
        // zeroes the fresh holder. The session comes back first — the
        // flag rides the log-on record, so it lands after the record
        // (the task order, put_entity_task.cpp:99-121).
        restart
            .put_logon_record(&key, &mailbox(0x0100_000b, 7))
            .expect("the fresh session is written");
        restart.set_auto_load(&key, true).expect("the flag lands");
        assert_eq!(
            restart.auto_load_entities().expect("the restart list runs"),
            [(4, 1)],
            "the flagged entity loads"
        );
        assert_eq!(
            EntityStorage::logon_of(&restart, &key).expect("the session read runs"),
            Some(BaseMailbox {
                addr: Address {
                    ip: 0,
                    port: 0,
                    salt: 7
                },
                entity_id: 7,
            }),
            "the non-empty pass zeroed the holder"
        );
    });
}

/// The archive write-back: an entity whose last write was still in a
/// secondary database at crash time has a missing (or stale) row in the
/// primary, and the consolidated write lands with the row's own id —
/// absent rows are created, present rows are updated — so the restored
/// database reads back exactly the archived entity.
#[test]
fn an_archived_write_restores_the_row_with_its_own_id() {
    with_fresh_db(|storage, connect| {
        register_counter(storage, 4);
        // Entity 1 made it to the primary before the crash; entity 2's
        // last write only ever reached the archive.
        let (key_a, values_a) = counter(1, 11);
        EntityStorage::put_entity(storage, &key_a, &values_a, true).expect("the primary row lands");

        // The restart consolidates the archive first: the merge checks
        // the row, creates an absent one with the row's own id
        // (`put_explicit_id` — `insertExplicit`, put_entity_task.cpp:89-97)
        // and updates a present one in place.
        let mut restart = connect();
        register_counter(&mut restart, 4);
        EntityStorage::put_entity(&mut restart, &key_a, &counter(1, 77).1, false)
            .expect("the archived write updates the present row");
        let (key_b, values_b) = counter(2, 99);
        EntityStorage::put_entity(&mut restart, &key_b, &values_b, true)
            .expect("the archived write creates the absent row");

        let read = |storage: &PostgresStorage, key: &EntityKey| {
            let mut record = EntityStorage::get_entity(storage, key)
                .expect("the restored row reads back")
                .expect("the row exists");
            record.values.pop().unwrap()
        };
        assert_eq!(read(&restart, &key_a), PropertyValue::Int(77));
        assert_eq!(read(&restart, &key_b), PropertyValue::Int(99));
    });
}

/// The flag rides the log-on record: a relogon moves the session but the
/// upsert touches only `objectID`/`ip`/`port`/`salt`, so the flagged
/// entity is on the next restart's list again — and a flag written for
/// an entity with no log-on record yet stays a silent no-op.
#[test]
fn the_flag_survives_a_relogon_and_stays_silent_without_a_record() {
    with_fresh_db(|storage, connect| {
        register_counter(storage, 4);
        let (key, values) = counter(1, 11);
        EntityStorage::put_entity(storage, &key, &values, true).expect("the seed row lands");
        storage
            .put_logon_record(&key, &mailbox(0x0100_000a, 7))
            .expect("the session is written");
        storage.set_auto_load(&key, true).expect("the flag lands");

        // An entity with no log-on record: the update matches no rows
        // and stays silent.
        let (fresh, _) = counter(9, 0);
        storage
            .set_auto_load(&fresh, true)
            .expect("no record yet, still not an error");

        // Restart pass one zeroes the holder.
        let mut restart = connect();
        register_counter(&mut restart, 4);
        assert_eq!(
            restart.auto_load_entities().expect("the restart list runs"),
            [(4, 1)]
        );

        // The relogon moves the session to a new base mailbox; the
        // auto-load flag survives it.
        restart
            .put_logon_record(&key, &mailbox(0x0100_000b, 9))
            .expect("the relogon moves the session");
        assert_eq!(
            CheckOut::Elsewhere(mailbox(0x0100_000b, 9)),
            EntityStorage::check_out(&mut restart, &key, mailbox(0x0100_000c, 11))
                .expect("the held check-out answers"),
            "the relogon installed the new holder"
        );

        // Restart pass two: still flagged, and the new holder is the one
        // zeroed.
        assert_eq!(
            restart.auto_load_entities().expect("the restart list runs"),
            [(4, 1)],
            "the flag survived the relogon"
        );
        assert_eq!(
            EntityStorage::logon_of(&restart, &key).expect("the session read runs"),
            Some(BaseMailbox {
                addr: Address {
                    ip: 0,
                    port: 0,
                    salt: 7
                },
                entity_id: 9,
            }),
            "the pass zeroed the holder the relogon installed"
        );
    });
}

/// The meta row re-points: the sync pass over a renamed-in-entities.xml
/// type updates the stored `bigworldID` (`TypesCollector::addType`'s
/// `setEntityTypeID` leg, table_inspector.cpp:269-271), a same-id pass
/// is a no-op, and the auto-load join resolves through the new id.
#[test]
fn a_repointed_type_updates_its_meta_row_and_the_join_follows() {
    with_fresh_db(|storage, _connect| {
        register_counter(storage, 4);

        // The same id again: the no-op leg.
        storage
            .add_entity_type(4, "Counter")
            .expect("the same-id pass is a no-op");

        // The type moves to id 9 in the running server: the meta row
        // updates, and the join resolves through the new id.
        let values = vec![PropertyValue::Int(0)];
        let schema = EntitySchema::for_record("Counter", &["level"], &values).unwrap();
        storage.create_entity_table(&schema).unwrap();
        storage
            .add_entity_type(9, "Counter")
            .expect("the re-point runs");
        storage.register_entity_type(9, &schema);
        let key = EntityKey {
            type_id: 9,
            db_id: 1,
        };
        EntityStorage::put_entity(storage, &key, &values, true).expect("the seed row lands");
        storage
            .put_logon_record(&key, &mailbox(0x0100_000a, 7))
            .expect("the session is written");
        storage.set_auto_load(&key, true).expect("the flag lands");

        assert_eq!(
            storage.auto_load_entities().expect("the restart list runs"),
            [(9, 1)],
            "the join follows the re-pointed id"
        );
    });
}
