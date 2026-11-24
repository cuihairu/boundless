//! The secondary-database consolidation — `server/tools/consolidate_dbs`
//! merges baseapp-written secondary databases into the primary (BW-09-03-03).
//!
//! The reference flow, per file (`consolidate_dbs_app.cpp:332-380`): the
//! file's `tbl_checksum` must unquote to the loaded definitions'
//! persistent-properties digest, or the whole file is refused; then each
//! flip/flop generation — already ordered older-first by
//! [`SecondaryDatabase::open`](crate::SecondaryDatabase::open) — hands its
//! rows to [`PrimaryDatabaseUpdateQueue::addUpdate`]
//! (`primary_database_update_queue.cpp:60-88`).
//!
//! The arbitration that decides which version of an entity survives is
//! three rules deep:
//!
//! 1. **The queue drop** (`primary_database_update_queue.cpp:65-71`): an
//!    update for a key that was already queued with a *strictly greater*
//!    time is dropped; an equal time re-queues. The queued time per key is
//!    remembered either way.
//! 2. **The primary guard** (`ConsolidateEntityTask::performBackgroundTask`,
//!    `consolidate_entity_task.cpp:32-37` → `hasNewerRecord`,
//!    `entity_type_mapping.cpp:242-249`): the write only happens when the
//!    primary's stored `gameTime` for the row is *older* than the
//!    secondary row's `sm_time` — `gameTime >= ?` matches, the put is
//!    skipped, and the primary record stands.
//! 3. **The write itself** (`PutEntityTask` with `UPDATE_AUTO_LOAD_RETAIN`,
//!    `put_entity_task.cpp:119-122`): a plain update of values plus the
//!    row's time, leaving the auto-load state and the logon record alone.
//!    An update that finds no primary row logs an error and *reports
//!    success* (`put_entity_task.cpp:82-90`) — consolidation never creates
//!    entities, and a missing row is a counted outcome here, not a
//!    failure.
//!
//! The port is the synchronous face of the same flow: the background task
//! manager collapses into [`ConsolidationQueue::consolidate_into`], which
//! applies the queued updates in order against any
//! [`EntityStorage`](crate::EntityStorage) backend.

use std::collections::{HashMap, VecDeque};

use entitydef::digest::Md5;
use foundation::ids::{EntityTypeId, GameTime};

use crate::error::DbError;
use crate::secondary::{SecondaryDatabase, StreamTypeDef};
use crate::storage::{EntityKey, EntityStorage};

// -----------------------------------------------------------------------------
// Section: the queue and its report
// -----------------------------------------------------------------------------

/// What became of one consolidated update — the fates
/// `PrimaryDatabaseUpdateQueue` and `ConsolidateEntityTask` hand out.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConsolidationOutcome {
    /// The write went through (`PutEntityTask` updated the primary row).
    Written,
    /// The primary's stored `gameTime` is at least the row's —
    /// `hasNewerRecord` matched and the primary record stands.
    SkippedPrimaryNewer,
    /// A strictly newer generation of the same entity was already queued
    /// (`consolidatedTimes_`, `primary_database_update_queue.cpp:65-71`).
    SkippedStaleQueue,
    /// The row's entity type has no mapping — the queue's WARNING + skip
    /// (`primary_database_update_queue.cpp:73-79`).
    SkippedUnknownType,
    /// The primary has no row for the id: the reference's update-missed
    /// path logs an error but reports success
    /// (`put_entity_task.cpp:82-90`) — consolidation never creates.
    NoSuchEntity,
}

/// The tally of one consolidation run, plus the errors that mark the file
/// failed (the reference's `hasError` — "consolidation must be re-run",
/// `consolidate_dbs_app.cpp:274-284`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ConsolidationReport {
    /// Updates written.
    pub written: usize,
    /// Updates the primary out-ranked.
    pub skipped_primary_newer: usize,
    /// Updates dropped for a newer queued generation.
    pub skipped_stale_queue: usize,
    /// Rows whose type had no mapping.
    pub skipped_unknown_type: usize,
    /// Rows whose entity the primary never had.
    pub no_such_entity: usize,
    /// The failures: decode errors, storage errors, a refused digest
    /// gate. Any entry fails the file the way the reference's
    /// `consolidationErrors_` does.
    pub errors: Vec<String>,
}

impl ConsolidationReport {
    /// `PrimaryDatabaseUpdateQueue::hasError` — the file must be re-run.
    pub fn has_error(&self) -> bool {
        !self.errors.is_empty()
    }
}

/// `PrimaryDatabaseUpdateQueue` without the threads: the drop rule and the
/// ordered updates it guards, applied by [`ConsolidationQueue::consolidate_into`].
#[derive(Debug, Default)]
pub struct ConsolidationQueue {
    updates: VecDeque<(EntityKey, Vec<entitydef::value::PropertyValue>, GameTime)>,
    /// `consolidatedTimes_` (`primary_database_update_queue.hpp:64-65`):
    /// the greatest queued time per entity.
    consolidated_times: HashMap<EntityKey, GameTime>,
}

impl ConsolidationQueue {
    /// `PrimaryDatabaseUpdateQueue::addUpdate`
    /// (`primary_database_update_queue.cpp:60-88`): stage an update unless
    /// a strictly newer one for the same entity was queued already.
    /// Returns `false` for the dropped case.
    pub fn add_update(
        &mut self,
        key: EntityKey,
        values: Vec<entitydef::value::PropertyValue>,
        time: GameTime,
    ) -> bool {
        if let Some(&queued) = self.consolidated_times.get(&key)
            && time < queued
        {
            return false;
        }
        self.consolidated_times.insert(key, time);
        self.updates.push_back((key, values, time));
        true
    }

    /// How many updates are still queued.
    pub fn len(&self) -> usize {
        self.updates.len()
    }

    /// Whether nothing is queued.
    pub fn is_empty(&self) -> bool {
        self.updates.is_empty()
    }

    /// Apply every queued update in order — the synchronous collapse of
    /// `ConsolidateEntityTask::performBackgroundTask`
    /// (`consolidate_entity_task.cpp:32-37`). A storage error stops the
    /// application (the reference's `hasError` stops stepping the table)
    /// and lands in `report.errors`; the remaining queued updates of
    /// later tables still run, as they do under the default
    /// `shouldStopOnError = false`.
    pub fn consolidate_into(
        &mut self,
        storage: &mut dyn EntityStorage,
        report: &mut ConsolidationReport,
    ) {
        while let Some((key, values, time)) = self.updates.pop_front() {
            // hasNewerRecord: `SELECT gameTime ... WHERE id=? AND
            // gameTime >= ?` — a stored time that ties or beats the row's
            // keeps the primary record.
            let primary_newer = match storage.game_time_of(&key) {
                Ok(stored) => stored.is_some_and(|stored| stored >= time),
                Err(error) => {
                    report
                        .errors
                        .push(format!("dbID {} gameTime read failed: {error}", key.db_id));
                    break;
                }
            };
            if primary_newer {
                report.skipped_primary_newer += 1;
                continue;
            }
            match storage.put_consolidated(&key, &values, time) {
                Ok(()) => report.written += 1,
                Err(DbError::NotFound) => report.no_such_entity += 1,
                Err(error) => {
                    report
                        .errors
                        .push(format!("dbID {} consolidation failed: {error}", key.db_id));
                    break;
                }
            }
        }
    }
}

// -----------------------------------------------------------------------------
// Section: the digest gate and the per-file driver
// -----------------------------------------------------------------------------

/// `ConsolidateDBsApp::checkEntityDefsDigestMatch`
/// (`consolidate_dbs_app.cpp:422-438`): the quoted digest must unquote —
/// the reference's only validity check is the length — and equal the
/// loaded definitions' persistent-properties digest, compared as raw
/// digest bytes.
fn check_entity_defs_digest_match(quoted_digest: &str, expected: &str) -> bool {
    match Md5::unquote(quoted_digest) {
        Some(digest) => Md5::unquote(expected) == Some(digest),
        None => false,
    }
}

/// Consolidate one secondary database file into `storage`, in the
/// reference's per-file shape (`consolidate_dbs_app.cpp:332-380`): the
/// checksum gate first, then the flip/flop generations in
/// [`SecondaryDatabase`]'s already older-first order, each table's rows
/// queued and applied before the next table is read.
///
/// `defs` maps every entity type id that can appear in the file to its
/// stream face; rows of any other type are skipped and counted, the
/// queue's WARNING path (`primary_database_update_queue.cpp:73-79`).
/// `expected_digest` is the quoted digest of the loaded definitions'
/// persistent properties (`EntityDefs::getPersistentPropertiesDigest`).
pub fn consolidate_database(
    storage: &mut dyn EntityStorage,
    database: &SecondaryDatabase,
    defs: &HashMap<EntityTypeId, StreamTypeDef>,
    expected_digest: &str,
) -> ConsolidationReport {
    let mut report = ConsolidationReport::default();

    // The digest gate: a refused file consolidates nothing.
    let digest = match database.checksum_digest() {
        Ok(digest) => digest,
        Err(error) => {
            report.errors.push(error.to_string());
            return report;
        }
    };
    if !check_entity_defs_digest_match(&digest, expected_digest) {
        report
            .errors
            .push(format!("secondary db failed entity digest check: {digest}"));
        return report;
    }

    let mut queue = ConsolidationQueue::default();
    for table in database.tables() {
        let rows = match database.rows(table) {
            Ok(rows) => rows,
            Err(error) => {
                report.errors.push(error.to_string());
                continue;
            }
        };
        for row in rows {
            // The unknown-type skip (`entityTypeMappings_[typeID] == NULL`).
            let Some(def) = defs.get(&row.type_id) else {
                report.skipped_unknown_type += 1;
                continue;
            };
            // A corrupt blob fails the file the way a failed stream read
            // fails the task; the remaining rows of this table stay
            // unread, the other tables still go.
            let values = match row.values(def) {
                Ok(values) => values,
                Err(error) => {
                    report.errors.push(error.to_string());
                    break;
                }
            };
            let key = EntityKey {
                type_id: row.type_id,
                db_id: row.db_id,
            };
            if !queue.add_update(key, values, row.time) {
                report.skipped_stale_queue += 1;
            }
        }
        queue.consolidate_into(storage, &mut report);
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use entitydef::value::PropertyValue;
    use foundation::ids::{DatabaseId, PENDING_DATABASE_ID};

    use crate::error::DbError;
    use crate::secondary::{FLIP_TABLE, FLOP_TABLE, StreamKind, StreamPropDef, StreamTypeDef};
    use crate::storage::{BaseMailbox, CheckOut, MemoryStorage};
    use crate::xml::{XmlDatabase, XmlKind, XmlPropDef, XmlTypeDef};

    /// One temporary file per call: the tests run in parallel threads and
    /// share the process, so a shared path would be a shared database.
    static TEMP_FILE: AtomicUsize = AtomicUsize::new(0);

    fn unique_path() -> PathBuf {
        let index = TEMP_FILE.fetch_add(1, Ordering::SeqCst);
        std::env::temp_dir().join(format!("db-consolidate-{}-{}", std::process::id(), index))
    }

    fn prop(name: &str, kind: StreamKind) -> StreamPropDef {
        StreamPropDef {
            name: name.to_owned(),
            kind,
        }
    }

    /// An Avatar-shaped stream def: the persistent pair.
    fn avatar_def() -> StreamTypeDef {
        StreamTypeDef {
            name: "Avatar".to_owned(),
            type_id: 1,
            properties: vec![
                prop("playerName", StreamKind::String),
                prop("level", StreamKind::Int32),
            ],
        }
    }

    fn avatar_values(name: &str, level: i64) -> Vec<PropertyValue> {
        vec![
            PropertyValue::String(name.to_owned()),
            PropertyValue::Int(level),
        ]
    }

    /// A goblin def sharing the file, so multi-type files have a second
    /// shape.
    fn goblin_def() -> StreamTypeDef {
        StreamTypeDef {
            name: "Goblin".to_owned(),
            type_id: 2,
            properties: vec![
                prop("playerName", StreamKind::String),
                prop("level", StreamKind::Int32),
            ],
        }
    }

    fn defs() -> HashMap<EntityTypeId, StreamTypeDef> {
        HashMap::from([(1, avatar_def()), (2, goblin_def())])
    }

    /// `DIGEST-0`..: a 32-character quoted-looking digest, distinct per
    /// argument, that both sides of the gate unquote identically.
    fn digest(seed: u8) -> String {
        format!("{}{:02X}", &"0123456789ABCDEF".repeat(2)[..30], seed)
    }

    /// One secondary row in a fixture: (dbID, typeID, time, values).
    type FixtureRow = (i64, i64, i64, Vec<PropertyValue>);
    /// One table's slice of rows in a fixture.
    type FixtureGeneration<'a> = (&'a str, &'a [FixtureRow]);

    /// The reference's schema statements, verbatim.
    fn create_tables(connection: &rusqlite::Connection, table: &str) {
        connection
            .execute(
                &format!(
                    "CREATE TABLE {table} (sm_dbID INTEGER, sm_typeID INTEGER, \
                     sm_time INTEGER, sm_blob BLOB)"
                ),
                [],
            )
            .expect("the entity table creates");
    }

    /// Build a secondary database file: `generations` are (table, rows) in
    /// probe order; rows are (dbID, typeID, time, values).
    fn fixture_file(generations: &[FixtureGeneration<'_>], checksum: &str) -> SecondaryDatabase {
        fixture_file_at(generations, checksum).0
    }

    /// [`fixture_file`] that also hands back the path, for tests that
    /// damage the file after it has been opened.
    fn fixture_file_at(
        generations: &[FixtureGeneration<'_>],
        checksum: &str,
    ) -> (SecondaryDatabase, std::path::PathBuf) {
        let path = unique_path();
        let connection = rusqlite::Connection::open(&path).expect("the fixture file opens");
        connection
            .execute("CREATE TABLE tbl_checksum (sm_checksum TEXT)", [])
            .expect("the checksum table creates");
        connection
            .execute(
                &format!("INSERT INTO tbl_checksum VALUES (\"{checksum}\")"),
                [],
            )
            .expect("the checksum inserts");
        for (table, rows) in generations {
            create_tables(&connection, table);
            for (db_id, type_id, time, values) in *rows {
                let def = if *type_id == 2 {
                    goblin_def()
                } else {
                    avatar_def()
                };
                let blob = crate::secondary::values_to_blob(values, &def);
                connection
                    .execute(
                        &format!("INSERT INTO {table} VALUES (?, ?, ?, ?)"),
                        rusqlite::params![db_id, type_id, time, blob],
                    )
                    .expect("the row inserts");
            }
        }
        drop(connection);
        (
            SecondaryDatabase::open(&path).expect("the fixture reads"),
            path,
        )
    }

    /// A primary with one Avatar (db 7) and one Goblin (db 3), each
    /// carrying a stored game time.
    fn seeded_primary() -> MemoryStorage {
        let mut store = MemoryStorage::new();
        store.insert(crate::record::EntityRecord {
            type_id: 1,
            db_id: 7,
            values: avatar_values("OldAvatar", 1),
        });
        store.insert(crate::record::EntityRecord {
            type_id: 2,
            db_id: 3,
            values: goblin_values("OldGoblin", 2),
        });
        store
            .put_consolidated(&avatar_key(7), &avatar_values("OldAvatar", 1), 100)
            .expect("the avatar seeds");
        store
            .put_consolidated(&goblin_key(3), &goblin_values("OldGoblin", 2), 100)
            .expect("the goblin seeds");
        store
    }

    fn goblin_values(name: &str, level: i64) -> Vec<PropertyValue> {
        vec![
            PropertyValue::String(name.to_owned()),
            PropertyValue::Int(level),
        ]
    }

    fn avatar_key(db_id: DatabaseId) -> EntityKey {
        EntityKey { type_id: 1, db_id }
    }

    fn goblin_key(db_id: DatabaseId) -> EntityKey {
        EntityKey { type_id: 2, db_id }
    }

    #[test]
    fn a_newer_secondary_row_overwrites_an_older_primary_record() {
        let mut store = seeded_primary();
        let database = fixture_file(
            &[(FLIP_TABLE, &[(7, 1, 200, avatar_values("NewAvatar", 9))])],
            &digest(1),
        );
        let report = consolidate_database(&mut store, &database, &defs(), &digest(1));
        assert!(!report.has_error(), "{:?}", report.errors);
        assert_eq!(
            report,
            ConsolidationReport {
                written: 1,
                ..ConsolidationReport::default()
            }
        );
        assert_eq!(
            store.get_entity(&avatar_key(7)).expect("the row reads"),
            Some(crate::record::EntityRecord {
                type_id: 1,
                db_id: 7,
                values: avatar_values("NewAvatar", 9),
            }),
            "the secondary values land"
        );
        assert_eq!(
            store.game_time_of(&avatar_key(7)).expect("the time reads"),
            Some(200),
            "the row's sm_time is the record's new gameTime"
        );
    }

    #[test]
    fn a_primary_at_least_as_new_keeps_its_record() {
        let mut store = seeded_primary();
        // sm_time ties the stored gameTime (>= matches), and a strictly
        // older row is behind too.
        let database = fixture_file(
            &[(
                FLIP_TABLE,
                &[
                    (7, 1, 100, avatar_values("Stale", 1)),
                    (3, 2, 100, goblin_values("Tied", 1)),
                ],
            )],
            &digest(2),
        );
        let report = consolidate_database(&mut store, &database, &defs(), &digest(2));
        assert!(!report.has_error(), "{:?}", report.errors);
        assert_eq!(report.written, 0);
        assert_eq!(report.skipped_primary_newer, 2);
        assert_eq!(
            store.get_entity(&avatar_key(7)).expect("the row reads"),
            Some(crate::record::EntityRecord {
                type_id: 1,
                db_id: 7,
                values: avatar_values("OldAvatar", 1),
            }),
            "the primary values stand"
        );
        assert_eq!(
            store.game_time_of(&avatar_key(7)).expect("the time reads"),
            Some(100)
        );
    }

    #[test]
    fn an_entity_the_primary_never_had_is_not_created() {
        let mut store = seeded_primary();
        let database = fixture_file(
            &[(FLIP_TABLE, &[(40, 1, 500, avatar_values("FreshHero", 4))])],
            &digest(3),
        );
        let report = consolidate_database(&mut store, &database, &defs(), &digest(3));
        assert!(
            !report.has_error(),
            "the reference reports success on the missed update: {:?}",
            report.errors
        );
        assert_eq!(report.written, 0);
        assert_eq!(report.no_such_entity, 1);
        assert_eq!(
            store.get_entity(&avatar_key(40)).expect("the row reads"),
            None,
            "consolidation never creates"
        );
    }

    #[test]
    fn a_newer_generation_of_the_same_entity_wins_across_tables() {
        let mut store = seeded_primary();
        // The older generation consolidates first, then the newer one
        // outranks the stored time and lands.
        let database = fixture_file(
            &[
                (FLIP_TABLE, &[(7, 1, 150, avatar_values("MidAvatar", 2))]),
                (FLOP_TABLE, &[(7, 1, 300, avatar_values("NewAvatar", 8))]),
            ],
            &digest(4),
        );
        let report = consolidate_database(&mut store, &database, &defs(), &digest(4));
        assert!(!report.has_error(), "{:?}", report.errors);
        assert_eq!(report.written, 2, "both generations wrote in order");
        assert_eq!(
            store.get_entity(&avatar_key(7)).expect("the row reads"),
            Some(crate::record::EntityRecord {
                type_id: 1,
                db_id: 7,
                values: avatar_values("NewAvatar", 8),
            })
        );
        assert_eq!(
            store.game_time_of(&avatar_key(7)).expect("the time reads"),
            Some(300)
        );
    }

    #[test]
    fn the_queue_drops_a_strictly_older_repeat_and_requeues_a_tie() {
        let mut queue = ConsolidationQueue::default();
        let key = avatar_key(7);
        assert!(
            queue.add_update(key, avatar_values("new", 1), 200),
            "the first update queues"
        );
        assert!(
            !queue.add_update(key, avatar_values("stale", 1), 100),
            "a strictly older repeat drops"
        );
        assert!(
            queue.add_update(key, avatar_values("tied", 1), 200),
            "an equal time re-queues"
        );
        assert_eq!(queue.len(), 2, "new then tied, not the stale one");
    }

    #[test]
    fn rows_of_unknown_types_are_skipped_and_counted() {
        let mut store = seeded_primary();
        let database = fixture_file(
            &[(
                FLIP_TABLE,
                &[
                    (7, 1, 200, avatar_values("NewAvatar", 9)),
                    (8, 66, 200, avatar_values("Unknown", 9)),
                ],
            )],
            &digest(5),
        );
        let report = consolidate_database(&mut store, &database, &defs(), &digest(5));
        assert!(!report.has_error(), "{:?}", report.errors);
        assert_eq!(report.skipped_unknown_type, 1);
        assert_eq!(report.written, 1, "the known row still wrote");
    }

    #[test]
    fn a_digest_mismatch_refuses_the_whole_file() {
        let mut store = seeded_primary();
        let database = fixture_file(
            &[(FLIP_TABLE, &[(7, 1, 200, avatar_values("NewAvatar", 9))])],
            &digest(6),
        );
        let report = consolidate_database(&mut store, &database, &defs(), &digest(7));
        assert!(report.has_error());
        assert_eq!(report.written, 0, "a refused file writes nothing");
        assert_eq!(
            store.get_entity(&avatar_key(7)).expect("the row reads"),
            Some(crate::record::EntityRecord {
                type_id: 1,
                db_id: 7,
                values: avatar_values("OldAvatar", 1),
            })
        );
    }

    #[test]
    fn a_digest_that_does_not_unquote_refuses_the_file() {
        let mut store = seeded_primary();
        let database = fixture_file(
            &[(FLIP_TABLE, &[(7, 1, 200, avatar_values("NewAvatar", 9))])],
            "too-short",
        );
        let report = consolidate_database(&mut store, &database, &defs(), &digest(1));
        assert!(report.has_error());
        assert_eq!(report.written, 0);
    }

    #[test]
    fn a_missing_checksum_table_refuses_the_file() {
        let mut store = seeded_primary();
        // A file whose checksum table never got written: the gate errors
        // before any row is touched.
        let path = unique_path();
        {
            let connection = rusqlite::Connection::open(&path).expect("the fixture file opens");
            create_tables(&connection, FLIP_TABLE);
            drop(connection);
        }
        let database = SecondaryDatabase::open(&path).expect("the file reads");
        let report = consolidate_database(&mut store, &database, &defs(), &digest(1));
        assert!(report.has_error(), "{:?}", report.errors);
        assert_eq!(report.written, 0);
    }

    #[test]
    fn a_corrupt_blob_fails_its_table_but_not_the_whole_file() {
        let mut store = seeded_primary();
        // The flip table's only row has a truncated blob; the flop table's
        // rows still consolidate.
        let path = unique_path();
        {
            let connection = rusqlite::Connection::open(&path).expect("the fixture file opens");
            create_tables(&connection, FLIP_TABLE);
            create_tables(&connection, FLOP_TABLE);
            connection
                .execute("CREATE TABLE tbl_checksum (sm_checksum TEXT)", [])
                .expect("the checksum table creates");
            connection
                .execute(
                    &format!("INSERT INTO tbl_checksum VALUES (\"{}\")", digest(8)),
                    [],
                )
                .expect("the checksum inserts");
            let good =
                crate::secondary::values_to_blob(&avatar_values("NewAvatar", 9), &avatar_def());
            connection
                .execute(
                    &format!("INSERT INTO {FLIP_TABLE} VALUES (?, ?, ?, ?)"),
                    rusqlite::params![7, 1, 200, &good[..good.len() - 1]],
                )
                .expect("the corrupt row inserts");
            let goblin =
                crate::secondary::values_to_blob(&goblin_values("NewGoblin", 5), &goblin_def());
            connection
                .execute(
                    &format!("INSERT INTO {FLOP_TABLE} VALUES (?, ?, ?, ?)"),
                    rusqlite::params![3, 2, 200, goblin],
                )
                .expect("the good row inserts");
            drop(connection);
        }
        let database = SecondaryDatabase::open(&path).expect("the file reads");
        let report = consolidate_database(&mut store, &database, &defs(), &digest(8));
        assert!(report.has_error(), "the corrupt row fails the file");
        assert_eq!(report.written, 1, "the other generation still wrote");
        assert_eq!(
            store.get_entity(&goblin_key(3)).expect("the row reads"),
            Some(crate::record::EntityRecord {
                type_id: 2,
                db_id: 3,
                values: goblin_values("NewGoblin", 5),
            })
        );
    }

    /// A storage whose `put_consolidated` fails the first call, the way a
    /// lost connection does mid-consolidation.
    struct FailingPutStorage {
        inner: MemoryStorage,
        failures: Vec<bool>,
        fail_reads: bool,
    }

    impl EntityStorage for FailingPutStorage {
        fn get_entity(
            &self,
            key: &EntityKey,
        ) -> Result<Option<crate::record::EntityRecord>, DbError> {
            self.inner.get_entity(key)
        }

        fn put_entity(
            &mut self,
            key: &EntityKey,
            values: &[PropertyValue],
            put_explicit_id: bool,
        ) -> Result<DatabaseId, DbError> {
            self.inner.put_entity(key, values, put_explicit_id)
        }

        fn del_entity(&mut self, key: &EntityKey) -> Result<(), DbError> {
            self.inner.del_entity(key)
        }

        fn logon_of(&self, key: &EntityKey) -> Result<Option<BaseMailbox>, DbError> {
            self.inner.logon_of(key)
        }

        fn check_out(
            &mut self,
            key: &EntityKey,
            mailbox: BaseMailbox,
        ) -> Result<CheckOut, DbError> {
            self.inner.check_out(key, mailbox)
        }

        fn check_in(&mut self, key: &EntityKey) -> Result<(), DbError> {
            self.inner.check_in(key)
        }

        fn all_entities(&self) -> Result<Vec<crate::record::EntityRecord>, DbError> {
            self.inner.all_entities()
        }

        fn put_consolidated(
            &mut self,
            key: &EntityKey,
            values: &[PropertyValue],
            game_time: GameTime,
        ) -> Result<(), DbError> {
            if !self.failures.is_empty() {
                let failure = self.failures.remove(0);
                if failure {
                    return Err(DbError::Storage("put failed".to_owned()));
                }
            }
            self.inner.put_consolidated(key, values, game_time)
        }

        fn game_time_of(&self, key: &EntityKey) -> Result<Option<GameTime>, DbError> {
            if self.fail_reads {
                return Err(DbError::Storage("time read failed".to_owned()));
            }
            self.inner.game_time_of(key)
        }
    }

    #[test]
    fn a_failed_write_stops_the_updates_and_fails_the_file() {
        let mut store = FailingPutStorage {
            inner: seeded_primary(),
            failures: vec![true, false],
            fail_reads: false,
        };
        // The avatar write fails; the goblin write in the next table goes
        // through, the way shouldStopOnError keeps the file going.
        let database = fixture_file(
            &[
                (FLIP_TABLE, &[(7, 1, 200, avatar_values("NewAvatar", 9))]),
                (FLOP_TABLE, &[(3, 2, 200, goblin_values("NewGoblin", 5))]),
            ],
            &digest(9),
        );
        let report = consolidate_database(&mut store, &database, &defs(), &digest(9));
        assert!(report.has_error(), "{:?}", report.errors);
        assert_eq!(report.written, 1);
        assert_eq!(
            store
                .get_entity(&goblin_key(3))
                .expect("the row reads")
                .expect("the row exists")
                .values,
            goblin_values("NewGoblin", 5)
        );
    }

    #[test]
    fn a_failed_time_read_fails_the_file() {
        let mut store = FailingPutStorage {
            inner: seeded_primary(),
            failures: vec![],
            fail_reads: true,
        };
        let database = fixture_file(
            &[(FLIP_TABLE, &[(7, 1, 200, avatar_values("NewAvatar", 9))])],
            &digest(12),
        );
        let report = consolidate_database(&mut store, &database, &defs(), &digest(12));
        assert!(
            report
                .errors
                .iter()
                .any(|e| e.contains("gameTime read failed")),
            "{:?}",
            report.errors
        );
        assert_eq!(report.written, 0);
    }

    #[test]
    fn a_failed_table_read_fails_just_that_table() {
        let mut store = seeded_primary();
        // The flip table vanishes between open and read — the driver
        // reports it and still consolidates the flop table.
        let (database, path) = fixture_file_at(
            &[
                (FLIP_TABLE, &[(7, 1, 200, avatar_values("NewAvatar", 9))]),
                (FLOP_TABLE, &[(3, 2, 200, goblin_values("NewGoblin", 5))]),
            ],
            &digest(13),
        );
        {
            let other = rusqlite::Connection::open(&path).expect("the second connection opens");
            other
                .execute(&format!("DROP TABLE {FLIP_TABLE}"), [])
                .expect("the table drops");
        }
        let report = consolidate_database(&mut store, &database, &defs(), &digest(13));
        assert!(report.has_error(), "{:?}", report.errors);
        assert_eq!(report.written, 1);
        assert_eq!(
            store
                .get_entity(&goblin_key(3))
                .expect("the row reads")
                .expect("the row exists")
                .values,
            goblin_values("NewGoblin", 5)
        );
    }

    #[test]
    fn a_strictly_older_repeat_in_the_file_is_dropped_by_the_queue() {
        let mut store = seeded_primary();
        // One table holds the entity twice, rows in rowid order 300 then
        // 150: the queue keeps the newer first sight, exactly like the
        // reference's consolidatedTimes_ map guarding the queued updates.
        let database = fixture_file(
            &[(
                FLIP_TABLE,
                &[
                    (7, 1, 300, avatar_values("MidAvatar", 2)),
                    (7, 1, 150, avatar_values("OldAvatar", 1)),
                ],
            )],
            &digest(14),
        );
        let report = consolidate_database(&mut store, &database, &defs(), &digest(14));
        assert!(!report.has_error(), "{:?}", report.errors);
        assert_eq!(report.written, 1);
        assert_eq!(report.skipped_stale_queue, 1);
        assert_eq!(
            store
                .get_entity(&avatar_key(7))
                .expect("the row reads")
                .expect("the row exists")
                .values,
            avatar_values("MidAvatar", 2)
        );
    }

    #[test]
    fn the_failing_storage_wrapper_passes_the_rest_through() {
        let mut store = FailingPutStorage {
            inner: seeded_primary(),
            failures: vec![],
            fail_reads: false,
        };
        assert_eq!(
            store.get_entity(&avatar_key(7)).expect("the read"),
            Some(crate::record::EntityRecord {
                type_id: 1,
                db_id: 7,
                values: avatar_values("OldAvatar", 1),
            })
        );
        assert_eq!(store.logon_of(&avatar_key(7)).expect("the logon"), None);
        assert_eq!(store.all_entities().expect("the scan").len(), 2);
        let holder = BaseMailbox {
            addr: mercury::endpoint::Address {
                ip: 1,
                port: 30000,
                salt: 5,
            },
            entity_id: 42,
        };
        let seed_key = EntityKey {
            type_id: 1,
            db_id: PENDING_DATABASE_ID,
        };
        let db_id = store
            .put_entity(&seed_key, &avatar_values("MadeAvatar", 3), false)
            .expect("the create puts");
        let made = EntityKey { type_id: 1, db_id };
        assert!(matches!(
            store.check_out(&made, holder).expect("the checkout"),
            CheckOut::Granted
        ));
        store.check_in(&made).expect("the check in");
        store.del_entity(&made).expect("the delete");
        assert_eq!(store.get_entity(&made).expect("the read"), None);
    }

    #[test]
    fn the_write_retains_the_logon_record_and_auto_load_face() {
        // UPDATE_AUTO_LOAD_RETAIN and the absent mailbox: the checkout the
        // primary already holds is untouched by consolidation.
        let mut store = MemoryStorage::new();
        store.insert(crate::record::EntityRecord {
            type_id: 1,
            db_id: 7,
            values: avatar_values("OldAvatar", 1),
        });
        let holder = BaseMailbox {
            addr: mercury::endpoint::Address {
                ip: 1,
                port: 20933,
                salt: 0x0103,
            },
            entity_id: 55,
        };
        assert_eq!(
            store.connection().check_out(&avatar_key(7), holder.clone()),
            Ok(CheckOut::Granted)
        );
        let database = fixture_file(
            &[(FLIP_TABLE, &[(7, 1, 200, avatar_values("NewAvatar", 9))])],
            &digest(10),
        );
        let mut connection = store.connection();
        let report = consolidate_database(&mut connection, &database, &defs(), &digest(10));
        assert!(!report.has_error(), "{:?}", report.errors);
        assert_eq!(report.written, 1);
        assert_eq!(
            connection
                .logon_of(&avatar_key(7))
                .expect("the logon reads"),
            Some(holder),
            "the checkout stands"
        );
    }

    #[test]
    fn an_xml_primary_takes_every_write_it_has_no_time_for() {
        // The XML database reads and discards the consolidation game time
        // ("Used in MySQL only"), so its records are never "newer" and a
        // newer-generation fight across tables falls to the queue rule.
        let xml_defs = vec![XmlTypeDef {
            name: "Avatar".to_owned(),
            type_id: 1,
            properties: vec![
                XmlPropDef {
                    name: "playerName".to_owned(),
                    kind: XmlKind::String,
                },
                XmlPropDef {
                    name: "level".to_owned(),
                    kind: XmlKind::Int { signed: true },
                },
            ],
            identifier: Some(0),
        }];
        let mut xml = XmlDatabase::create(xml_defs);

        // Seed the primary with an existing Avatar through the create leg —
        // an update of a row that has no id is the XML backend's isOK=false.
        let seed_key = EntityKey {
            type_id: 1,
            db_id: PENDING_DATABASE_ID,
        };
        let db_id = xml
            .put_entity(&seed_key, &avatar_values("OldAvatar", 1), false)
            .expect("the seed puts");
        let key = EntityKey { type_id: 1, db_id };

        let rows: Vec<(&str, Vec<FixtureRow>)> = vec![
            (
                FLIP_TABLE,
                vec![(db_id, 1, 150, avatar_values("MidAvatar", 2))],
            ),
            (
                FLOP_TABLE,
                vec![(db_id, 1, 300, avatar_values("NewAvatar", 8))],
            ),
        ];
        let generations: Vec<FixtureGeneration<'_>> =
            rows.iter().map(|(t, r)| (*t, r.as_slice())).collect();
        let database = fixture_file(&generations, &digest(11));
        let report = consolidate_database(&mut xml, &database, &defs(), &digest(11));
        assert!(!report.has_error(), "{:?}", report.errors);
        assert_eq!(report.written, 2, "no stored time, every row writes");
        assert_eq!(
            xml.get_entity(&key).expect("the row reads"),
            Some(crate::record::EntityRecord {
                type_id: 1,
                db_id,
                values: avatar_values("NewAvatar", 8),
            })
        );
    }

    #[test]
    fn the_digest_gate_compares_quoted_bytes() {
        assert!(check_entity_defs_digest_match(&digest(1), &digest(1)));
        assert!(!check_entity_defs_digest_match(&digest(1), &digest(2)));
        assert!(!check_entity_defs_digest_match("nope", &digest(1)));
        assert!(!check_entity_defs_digest_match(&digest(1), "nope"));
    }

    #[test]
    fn an_empty_file_consolidates_nothing() {
        let mut store = seeded_primary();
        let database = fixture_file(&[], &digest(12));
        let report = consolidate_database(&mut store, &database, &defs(), &digest(12));
        assert_eq!(report, ConsolidationReport::default());
    }

    #[test]
    fn the_queue_face_counts_and_drains() {
        let mut queue = ConsolidationQueue::default();
        assert!(queue.is_empty());
        assert!(queue.add_update(avatar_key(1), avatar_values("a", 1), 10));
        assert_eq!(queue.len(), 1);
        let mut store = seeded_primary();
        let mut report = ConsolidationReport::default();
        queue.consolidate_into(&mut store, &mut report);
        assert!(queue.is_empty());
        // db 1 does not exist in the seeded primary.
        assert_eq!(report.no_such_entity, 1);
    }
}
