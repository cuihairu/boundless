//! The auto-load restore acceptance case (BW-09-06-11, 验收 重启恢复
//! 对应实体): a restart recovers exactly the entities the database
//! flagged, and the archive write-back that feeds it lands whole.
//!
//! The MySQL baseline is the restart chain of the DBApp:
//!
//! * the log-on records NOT flagged `shouldAutoLoad` are deleted —
//!   those sessions are stale at a restart
//!   (`mysql_database.cpp:674-675`);
//! * the flagged ones are listed for the auto-loader, joined with
//!   `bigworldEntityTypes` for the running-server type id
//!   (`mysql_database.cpp:677-683`);
//! * when the list is non-empty the stored holders zero
//!   (`ip = 0, port = 0`, `mysql_database.cpp:702`) — the address
//!   restarts, `objectID` and `salt` stay;
//! * the flag rides the log-on record: the relogon upsert touches only
//!   `objectID`/`ip`/`port`/`salt`, so a flagged entity stays flagged
//!   across a relogon;
//! * the flag is written by `updateAutoLoad`
//!   (`entity_type_mapping.cpp:792-800`), and an entity with no log-on
//!   record matches no rows — the update stays silent;
//! * the `bigworldEntityTypes` row the join needs is the sync pass's
//!   face (`TypesCollector::addType`, table_inspector.cpp:250-272):
//!   absent by name it is added, a different id re-points it, the same
//!   id is a no-op.
//!
//! The SQLite archive's write-back is the entity put the consolidation
//! replays (`ConsolidateEntityTask` driving `putEntity`): an absent row
//! is created with the row's own id, a present one is updated — so the
//! restored database reads back exactly the archived entity. The
//! reader itself is the delivered `db` crate's secondary-database face
//! and stays out of this suite; the case pins the landing.

use std::sync::atomic::{AtomicU32, Ordering};

use db::auto_load_recovery::{EntityAutoLoader, MAX_CONCURRENT_ENTITY_LOADERS, RecoveryOutcome};
use db::{BaseMailbox, CheckOut, EntityKey, EntityStorage};
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
    let user = std::env::var("USER")
        .or_else(|_| std::env::var("LOGNAME"))
        .unwrap_or_else(|_| "postgres".to_string());
    Some(format!(
        "host=/var/run/postgresql dbname=postgres user={user}"
    ))
}

/// Runs `body` with the connection string of a fresh private database
/// (the fixed tables and the entity tables included), or skips when no
/// PostgreSQL server is reachable. The body opens its own connections —
/// a restart is a fresh process. The database is dropped when the body
/// returns.
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
        register_all(&mut setup)?;
        drop(setup);
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

const AGENT_TYPE: u16 = 9;
const PET_TYPE: u16 = 12;

fn agent_values(name: &str, level: i64) -> Vec<PropertyValue> {
    vec![
        PropertyValue::String(name.into()),
        PropertyValue::Int(level),
    ]
}

fn pet_values(name: &str) -> Vec<PropertyValue> {
    vec![PropertyValue::String(name.into()), PropertyValue::Int(0)]
}

/// The shape both types share: a name and a level, so the rows decode
/// whole through the same schema face.
fn schema_for(type_name: &str, defaults: &[PropertyValue]) -> EntitySchema {
    EntitySchema::for_record(type_name, &["name", "level"], defaults).unwrap()
}

/// Registers every type the cases use on a connection: the entity
/// table, the contract registry, and the `bigworldEntityTypes` row the
/// auto-load join resolves through (the sync pass's `addType`).
fn register_all(storage: &mut PostgresStorage) -> Result<(), db::DbError> {
    let agent = schema_for("Agent", &agent_values("a", 1));
    storage.create_entity_table(&agent)?;
    storage.register_entity_type(AGENT_TYPE, &agent);
    storage.add_entity_type(AGENT_TYPE, "Agent")?;

    let pet = schema_for("Pet", &pet_values("p"));
    storage.create_entity_table(&pet)?;
    storage.register_entity_type(PET_TYPE, &pet);
    storage.add_entity_type(PET_TYPE, "Pet")?;
    Ok(())
}

/// A connection with every type registered — the running server.
fn server(conninfo: &str) -> PostgresStorage {
    let mut storage = PostgresStorage::connect(conninfo).expect("the database opens");
    register_all(&mut storage).expect("the registration works");
    storage
}

fn agent_key(db_id: i64) -> EntityKey {
    EntityKey {
        type_id: AGENT_TYPE,
        db_id,
    }
}

fn pet_key(db_id: i64) -> EntityKey {
    EntityKey {
        type_id: PET_TYPE,
        db_id,
    }
}

fn mailbox(ip: u32, port: u16, salt: u16, entity_id: i32) -> BaseMailbox {
    BaseMailbox {
        addr: Address { ip, port, salt },
        entity_id,
    }
}

/// 验收: a restart loads exactly the flagged entities. The stale
/// sessions of the unflagged ones are cleared, the holders of the
/// loaded set zero (`objectID` and `salt` stay), and the unflagged
/// entity's row itself stays put — the load itself reads every listed
/// entity back whole through the storage contract.
#[test]
fn a_restart_loads_exactly_the_flagged_entities() {
    with_fresh_db(|conninfo| {
        let mut before = server(conninfo);

        // Three entities: a flagged agent, an unflagged agent, a flagged
        // pet. All three hold sessions.
        let agent_flagged = EntityStorage::put_entity(
            &mut before,
            &agent_key(PENDING_DATABASE_ID),
            &agent_values("keep", 7),
            false,
        )
        .unwrap();
        let agent_plain = EntityStorage::put_entity(
            &mut before,
            &agent_key(PENDING_DATABASE_ID),
            &agent_values("plain", 8),
            false,
        )
        .unwrap();
        let pet_flagged = EntityStorage::put_entity(
            &mut before,
            &pet_key(PENDING_DATABASE_ID),
            &pet_values("rex"),
            false,
        )
        .unwrap();
        EntityStorage::check_out(
            &mut before,
            &agent_key(agent_flagged),
            mailbox(0x0102_0304, 0x1234, 1, 10),
        )
        .unwrap();
        EntityStorage::check_out(
            &mut before,
            &agent_key(agent_plain),
            mailbox(0x0a0b_0c0d, 0x5678, 2, 11),
        )
        .unwrap();
        EntityStorage::check_out(
            &mut before,
            &pet_key(pet_flagged),
            mailbox(0x0506_0708, 0x9abc, 3, 12),
        )
        .unwrap();
        before
            .set_auto_load(&agent_key(agent_flagged), true)
            .unwrap();
        before.set_auto_load(&pet_key(pet_flagged), true).unwrap();

        // The restart: a fresh process, the auto-load pass.
        let mut restarted = server(conninfo);
        let mut list = restarted.auto_load_entities().unwrap();

        // The flagged entities and nothing else — the join hands back
        // the running-server type id with each db id.
        let mut expected = vec![(AGENT_TYPE, agent_flagged), (PET_TYPE, pet_flagged)];
        list.sort();
        expected.sort();
        assert_eq!(list, expected);

        // The load itself: every listed entity reads back whole.
        let mut loader = EntityAutoLoader::new(MAX_CONCURRENT_ENTITY_LOADERS);
        for &(type_id, db_id) in &list {
            loader.add_entity(type_id, db_id);
        }
        loader.start(&mut |type_id, db_id| {
            let key = EntityKey { type_id, db_id };
            EntityStorage::get_entity(&restarted, &key)
                .unwrap()
                .map(|record| {
                    assert_eq!(record.db_id, db_id);
                    if type_id == AGENT_TYPE {
                        assert_eq!(record.values, agent_values("keep", 7));
                    } else {
                        assert_eq!(record.values, pet_values("rex"));
                    }
                })
                .is_some()
        });
        assert_eq!(
            loader.outcome(),
            Some(&RecoveryOutcome::Completed {
                did_auto_load: true
            })
        );

        // The stale session of the unflagged entity is gone; its row
        // stays — only the session was stale.
        assert_eq!(
            EntityStorage::logon_of(&restarted, &agent_key(agent_plain)).unwrap(),
            None
        );
        assert!(
            EntityStorage::get_entity(&restarted, &agent_key(agent_plain))
                .unwrap()
                .is_some()
        );

        // The loaded entity's holder zeroed: the address restarts, the
        // object id and the salt stay.
        let row = restarted
            .client_mut()
            .query_one(
                "SELECT \"objectID\", \"ip\", \"port\", \"salt\" \
                 FROM \"bigworldLogOns\" WHERE \"databaseID\" = $1 AND \"typeID\" = $2",
                &[&agent_flagged, &i32::from(AGENT_TYPE)],
            )
            .unwrap();
        assert_eq!(row.get::<_, i32>(0), 10);
        assert_eq!(row.get::<_, i64>(1), 0);
        assert_eq!(row.get::<_, i32>(2), 0);
        assert_eq!(row.get::<_, i32>(3), 1);
    });
}

/// The cleanup leg clears an unflagged session on every restart pass,
/// and the holder zeroing rides the non-empty list
/// (`mysql_database.cpp:693-703`: the `UPDATE ip = 0, port = 0` sits
/// inside the `numResults > 0` guard). Nothing listed means nothing
/// zeroed — and a session unflagged between passes goes the way of
/// every stale session, deleted rather than zeroed.
#[test]
fn the_cleanup_clears_unflagged_sessions_and_zeroing_rides_the_list() {
    with_fresh_db(|conninfo| {
        let mut storage = server(conninfo);

        let db_id = EntityStorage::put_entity(
            &mut storage,
            &agent_key(PENDING_DATABASE_ID),
            &agent_values("plain", 7),
            false,
        )
        .unwrap();
        EntityStorage::check_out(&mut storage, &agent_key(db_id), mailbox(1, 2, 3, 10)).unwrap();

        // Nothing flagged: the pass reports an empty list — the
        // database just starts (NothingToLoad).
        assert_eq!(
            storage.auto_load_entities().unwrap(),
            Vec::<(u16, i64)>::new()
        );
        let mut loader = EntityAutoLoader::new(MAX_CONCURRENT_ENTITY_LOADERS);
        loader.start(&mut |_, _| panic!("nothing loads"));
        assert_eq!(loader.outcome(), Some(&RecoveryOutcome::NothingToLoad));

        // The unflagged session did not survive the pass.
        assert_eq!(
            EntityStorage::logon_of(&storage, &agent_key(db_id)).unwrap(),
            None
        );

        // A flagged session between passes: the next pass lists it and
        // zeroes the holder rather than deleting it.
        EntityStorage::check_out(&mut storage, &agent_key(db_id), mailbox(3, 4, 5, 10)).unwrap();
        storage.set_auto_load(&agent_key(db_id), true).unwrap();
        assert_eq!(
            storage.auto_load_entities().unwrap(),
            vec![(AGENT_TYPE, db_id)]
        );
        let row = storage
            .client_mut()
            .query_one(
                "SELECT \"ip\", \"port\" FROM \"bigworldLogOns\" \
                 WHERE \"databaseID\" = $1 AND \"typeID\" = $2",
                &[&db_id, &i32::from(AGENT_TYPE)],
            )
            .unwrap();
        assert_eq!(row.get::<_, i64>(0), 0);
        assert_eq!(row.get::<_, i32>(1), 0);

        // Unflag it: the next pass takes the session the stale way.
        storage.set_auto_load(&agent_key(db_id), false).unwrap();
        assert_eq!(
            storage.auto_load_entities().unwrap(),
            Vec::<(u16, i64)>::new()
        );
        assert_eq!(
            EntityStorage::logon_of(&storage, &agent_key(db_id)).unwrap(),
            None
        );
    });
}

/// The archive write-back: an entity whose last write was still in a
/// secondary database at crash time has a missing (or stale) row in
/// the primary, and the consolidated write lands with the row's own
/// id — absent rows are created, present rows are updated — so the
/// restored database reads back exactly the archived entity.
#[test]
fn an_archived_write_restores_the_row_with_its_own_id() {
    with_fresh_db(|conninfo| {
        let mut primary = server(conninfo);

        // The primary holds a stale row for the archived id: the last
        // archived write carries newer values.
        EntityStorage::put_entity(&mut primary, &agent_key(5), &agent_values("stale", 1), true)
            .unwrap();

        // The consolidated write lands with the row's own id: the
        // absent row (an entity the primary never saw) is created, the
        // present one is updated.
        EntityStorage::put_entity(&mut primary, &agent_key(6), &agent_values("new", 30), true)
            .unwrap();
        EntityStorage::put_entity(
            &mut primary,
            &agent_key(5),
            &agent_values("newer", 42),
            false,
        )
        .unwrap();

        // The restored database reads back exactly the archived
        // entities, whole.
        assert_eq!(
            EntityStorage::get_entity(&primary, &agent_key(6))
                .unwrap()
                .expect("the archived row reads back")
                .values,
            agent_values("new", 30)
        );
        assert_eq!(
            EntityStorage::get_entity(&primary, &agent_key(5))
                .unwrap()
                .expect("the refreshed row reads back")
                .values,
            agent_values("newer", 42)
        );
    });
}

/// The flag rides the log-on record: a relogon moves the session but
/// the upsert touches only `objectID`/`ip`/`port`/`salt`, so the
/// flagged entity is on the next restart's list again — and a flag
/// written for an entity with no log-on record yet stays a silent
/// no-op.
#[test]
fn the_flag_survives_a_relogon_and_stays_silent_without_a_record() {
    with_fresh_db(|conninfo| {
        let mut storage = server(conninfo);

        let db_id = EntityStorage::put_entity(
            &mut storage,
            &agent_key(PENDING_DATABASE_ID),
            &agent_values("holder", 7),
            false,
        )
        .unwrap();
        EntityStorage::check_out(&mut storage, &agent_key(db_id), mailbox(1, 2, 3, 10)).unwrap();
        storage.set_auto_load(&agent_key(db_id), true).unwrap();

        // The relogon moves the session; the flag survives it.
        storage
            .put_logon_record(&agent_key(db_id), &mailbox(9, 10, 11, 20))
            .unwrap();
        assert_eq!(
            storage.auto_load_entities().unwrap(),
            vec![(AGENT_TYPE, db_id)]
        );

        // A flag for an entity with no log-on record matches no rows:
        // no record appears, the listing does not pick it up.
        let free = EntityStorage::put_entity(
            &mut storage,
            &agent_key(PENDING_DATABASE_ID),
            &agent_values("free", 8),
            false,
        )
        .unwrap();
        storage.set_auto_load(&agent_key(free), true).unwrap();
        let count: i64 = storage
            .client_mut()
            .query_one(
                "SELECT COUNT(*) FROM \"bigworldLogOns\" WHERE \"databaseID\" = $1",
                &[&free],
            )
            .unwrap()
            .get(0);
        assert_eq!(count, 0, "no record was created by the flag alone");
        let listed: Vec<(u16, i64)> = storage
            .auto_load_entities()
            .unwrap()
            .into_iter()
            .filter(|&(_, id)| id == free)
            .collect();
        assert_eq!(listed, Vec::<(u16, i64)>::new());
    });
}

/// The meta row re-points: the sync pass over a renamed-in-entities.xml
/// type updates the stored ids (`TypesCollector::addType`'s
/// `setEntityTypeID` leg, table_inspector.cpp:269-271), a same-id pass
/// is a no-op, and the auto-load join resolves through the new id.
#[test]
fn a_repointed_type_updates_its_meta_row_and_the_join_follows() {
    with_fresh_db(|conninfo| {
        let mut storage = server(conninfo);

        // The same-id pass is a no-op: one row, unchanged.
        storage.add_entity_type(AGENT_TYPE, "Agent").unwrap();
        let rows = storage
            .client_mut()
            .query(
                "SELECT \"typeID\", \"bigworldID\", \"name\" FROM \"bigworldEntityTypes\"",
                &[],
            )
            .unwrap();
        assert_eq!(rows.len(), 2, "the two registered types, no extra rows");
        for row in &rows {
            let name: String = row.get(2);
            let bigworld_id: i64 = row.get(1);
            if name == "Agent" {
                assert_eq!(bigworld_id, i64::from(AGENT_TYPE));
            }
        }

        // The re-point: the running server now numbers the type 20.
        let repointed = 20u16;
        storage.add_entity_type(repointed, "Agent").unwrap();
        let agent_row = storage
            .client_mut()
            .query_one(
                "SELECT \"typeID\", \"bigworldID\" FROM \"bigworldEntityTypes\" WHERE \"name\" = $1",
                &[&"Agent"],
            )
            .unwrap();
        assert_eq!(agent_row.get::<_, i64>(0), i64::from(repointed));
        assert_eq!(agent_row.get::<_, i64>(1), i64::from(repointed));

        // A session written under the new id joins and lists; the old
        // id no longer names the type, so its sessions stop joining.
        let schema = schema_for("Agent", &agent_values("a", 1));
        storage.register_entity_type(repointed, &schema);
        let moved_key = EntityKey {
            type_id: repointed,
            db_id: 700,
        };
        EntityStorage::put_entity(&mut storage, &moved_key, &agent_values("moved", 7), true)
            .unwrap();
        EntityStorage::check_out(&mut storage, &moved_key, mailbox(1, 2, 3, 10)).unwrap();
        storage.set_auto_load(&moved_key, true).unwrap();

        let listed = storage.auto_load_entities().unwrap();
        assert!(
            listed.contains(&(repointed, 700)),
            "the new id joins: {listed:?}"
        );

        // A flagged session under the pre-point id does not join
        // anymore — the port's one type-id space moves both columns
        // together (the reference's stable internal id keeps old
        // records joining; the divergence is the collapsed space's
        // price) — and the cleanup leg, which only removes unflagged
        // records, leaves it standing.
        storage
            .client_mut()
            .execute(
                "INSERT INTO \"bigworldLogOns\" (\"databaseID\", \"typeID\", \"objectID\", \
                 \"ip\", \"port\", \"salt\", \"shouldAutoLoad\") \
                 VALUES ($1, $2, $3, $4, $5, $6, TRUE)",
                &[&701i64, &i32::from(AGENT_TYPE), &10, &0i64, &0, &1],
            )
            .unwrap();
        let listed: Vec<(u16, i64)> = storage
            .auto_load_entities()
            .unwrap()
            .into_iter()
            .filter(|&(_, id)| id == 701)
            .collect();
        assert_eq!(listed, Vec::<(u16, i64)>::new(), "the old id stops joining");
        let count: i64 = storage
            .client_mut()
            .query_one(
                "SELECT COUNT(*) FROM \"bigworldLogOns\" WHERE \"databaseID\" = $1",
                &[&701i64],
            )
            .unwrap()
            .get(0);
        assert_eq!(count, 1, "a flagged record survives the cleanup leg");
    });
}

/// The occupancy grant still holds the flag's record whole: a flagged
/// entity checked out elsewhere is refused — the flag does not hand
/// the session to a second base.
#[test]
fn a_flagged_entity_still_grants_one_session_only() {
    with_fresh_db(|conninfo| {
        let mut storage = server(conninfo);

        let db_id = EntityStorage::put_entity(
            &mut storage,
            &agent_key(PENDING_DATABASE_ID),
            &agent_values("held", 7),
            false,
        )
        .unwrap();
        let standing = mailbox(0x0102_0304, 0x1234, 1, 10);
        EntityStorage::check_out(&mut storage, &agent_key(db_id), standing.clone()).unwrap();
        storage.set_auto_load(&agent_key(db_id), true).unwrap();

        // The flag does not bypass the occupancy grant.
        assert_eq!(
            EntityStorage::check_out(&mut storage, &agent_key(db_id), mailbox(9, 9, 9, 99))
                .unwrap(),
            CheckOut::Elsewhere(standing)
        );

        // And the listing still reports it — the flag is about the
        // restart, not the live session.
        assert_eq!(
            storage.auto_load_entities().unwrap(),
            vec![(AGENT_TYPE, db_id)]
        );
    });
}
