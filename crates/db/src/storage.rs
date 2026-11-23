//! The storage contract (`IDatabase`) and the entity get/load/write/delete
//! and lookup operations.
//!
//! The reference's persistence contract is `lib/db_storage/idatabase.hpp`
//! (`class IDatabase`). Each entity operation is one `todo.md` item: this
//! module carries `get_entity` (BW-09-01-01), `put_entity` (BW-09-01-02),
//! `del_entity` (BW-09-01-03), the lookups (BW-09-01-04) and the online
//! state (BW-09-01-05, the log-on record in [`crate::log_on`]). The DBID
//! allocation itself is the backend's id allocator — `AUTO_INCREMENT` in
//! the reference, the in-memory allocator's `next_id` here. The in-memory
//! backend lets the entity-operation acceptance cases run before any real
//! backend exists; MySQL/XML/SQLite/PostgreSQL plug into the same trait.

use std::collections::HashMap;

use foundation::ids::{DatabaseId, EntityTypeId, PENDING_DATABASE_ID};

use crate::error::DbError;
use crate::log_on::LogOnRecord;
use crate::record::EntityRecord;

/// `EntityKey`: the identity of a stored entity, `(type, database id)`.
///
/// `lib/db_storage/entity_key.hpp` `EntityKey`. The reference resolves by
/// `name` when `dbID == 0`; that leg is
/// [`EntityStorage::lookup_entity_by_name`] (BW-09-01-04), and
/// [`load_entity_by_name`] composes it into a load.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct EntityKey {
    /// `EntityKey::typeID`.
    pub type_id: EntityTypeId,
    /// `EntityKey::dbID`. `PENDING_DATABASE_ID`/0 means "resolve by name".
    pub db_id: DatabaseId,
}

/// The storage contract (`IDatabase`).
///
/// Each method mirrors a reference `IDatabase` operation. Every backend is
/// a [`EntityStorage`], and every entity operation runs against one.
pub trait EntityStorage {
    /// `IDatabase::getEntity`: resolve the row for `key` and return its
    /// persistent data. `Ok(None)` is the reference's "row missing"
    /// (`isOK=false`) outcome, not a failure.
    fn get_entity(&self, key: &EntityKey) -> Result<Option<EntityRecord>, DbError>;

    /// `IDatabase::putEntity` (`idatabase.hpp:204-208`): write `record` into
    /// storage and return the database id of the row.
    ///
    /// A pending db id is a create — the storage assigns the id, and the
    /// assigned id is the callback's `dbID` ("particularly useful if
    /// ekey.dbID == 0 originally", `idatabase.hpp:196-197`; the reference's
    /// `0` is our [`PENDING_DATABASE_ID`]). A set db id is an update, and it
    /// must identify an existing row — `isOK is false if ekey.dbID != 0 but
    /// didn't identify an existing entity` (`idatabase.hpp:195-196`), which
    /// is [`DbError::NotFound`]. `put_explicit_id` inserts under the
    /// record's own id instead of updating
    /// (`PutEntityTask::performBackgroundTask`, `put_entity_task.cpp:79-124`:
    /// update vs `insertNew` vs `insertExplicit`).
    fn put_entity(
        &mut self,
        record: &EntityRecord,
        put_explicit_id: bool,
    ) -> Result<DatabaseId, DbError>;

    /// `IDatabase::delEntity` (`idatabase.hpp:249-258`): remove the row
    /// `key` names. `isOK is false if the entity doesn't exist`
    /// (`idatabase.hpp:239-240`) — [`DbError::NotFound`]; `DelEntityTask`
    /// `setFailure()`s the same way when the name lookup or
    /// `deleteWithID` finds nothing (`del_entity_task.cpp:44-64`). A
    /// pending db id is never a real key
    /// (`del_entity_task.cpp:35`, `MF_ASSERT( dbID != PENDING_DATABASE_ID )`),
    /// so deleting one is the missing-row outcome too. The db id == 0
    /// by-name leg is `getDbID(conn, name)` (`del_entity_task.cpp:38-40`),
    /// which joins the lookup item (BW-09-01-04).
    fn del_entity(&mut self, key: &EntityKey) -> Result<(), DbError>;

    /// `IDatabase::getDatabaseIDFromName` (`idatabase.hpp:131-135`)
    /// composed with the get: the row of `type_id` whose identifier
    /// property is `name`. The identifier is the row's `name` column under
    /// a unique index (`EntityTypeMapping::hasIdentifier()`);
    /// `Ok(None)` is an unknown name, not a failure.
    fn lookup_entity_by_name(
        &self,
        type_id: EntityTypeId,
        name: &str,
    ) -> Result<Option<EntityRecord>, DbError>;

    /// `IDatabase::lookUpEntities` (`idatabase.hpp:176-186`): every row of
    /// `type_id`, in database-id order — the lookup with no property
    /// queries. The criteria form (`LookUpEntitiesCriteria`,
    /// `lib/server/look_up_entities.hpp`: property queries, online filter,
    /// sort, limit, offset) joins with the online state (BW-09-01-05).
    fn lookup_entities(&self, type_id: EntityTypeId) -> Result<Vec<EntityRecord>, DbError>;

    /// `EntityTypeMapping::addLogOnRecord` as reached from `PutEntityTask`
    /// (`put_entity_task.cpp:120-131`): mark the row online at `record`'s
    /// base. The add is an upsert — a relogon replaces the location, and
    /// there is never more than one record per row. A record is refused
    /// for a row that does not exist, the
    /// `definitelyExists || checkExists` guard against invalid log-on
    /// records (`put_entity_task.cpp:123-130`).
    fn set_log_on_record(&mut self, key: &EntityKey, record: LogOnRecord) -> Result<(), DbError>;

    /// `EntityTypeMapping::removeLogOnRecord` as reached from
    /// `DelEntityTask` (`del_entity_task.cpp:66`) and the removeBaseMailbox
    /// put (`put_entity_task.cpp:132-136`): mark the row offline. Removing
    /// where nothing is recorded is fine.
    fn remove_log_on_record(&mut self, key: &EntityKey) -> Result<(), DbError>;

    /// The row's checked-out base location, if it is online.
    fn log_on_record(&self, key: &EntityKey) -> Result<Option<LogOnRecord>, DbError>;
}

/// The DBApp load: fetch the record for `key`, or [`DbError::NotFound`]
/// when the entity is not in storage.
pub fn load_entity(storage: &impl EntityStorage, key: &EntityKey) -> Result<EntityRecord, DbError> {
    storage.get_entity(key)?.ok_or(DbError::NotFound)
}

/// The DBApp load by identifier: fetch the record named `name` of
/// `type_id`, or [`DbError::NotFound`] when no row carries it. This is
/// `getDatabaseIDFromName` + `getEntity`
/// (`idatabase.hpp:131-135`); `del_entity_task.cpp:38-40` resolves the
/// same `getDbID(conn, name)` leg before deleting.
pub fn load_entity_by_name(
    storage: &impl EntityStorage,
    type_id: EntityTypeId,
    name: &str,
) -> Result<EntityRecord, DbError> {
    storage
        .lookup_entity_by_name(type_id, name)?
        .ok_or(DbError::NotFound)
}

/// An in-memory backend.
///
/// The BW-09-01 entity-operation acceptance cases run against this before a
/// real backend exists. Created rows get ids from a monotonically growing
/// allocator, the way an `AUTO_INCREMENT` primary key does
/// (`entityTypeMapping_.insertNew`, `put_entity_task.cpp:99-104`), and
/// named rows sit under a unique `(type, name)` index the way the
/// identifier column does (`EntityTypeMapping::hasIdentifier()`).
#[derive(Debug, Clone)]
pub struct MemoryStorage {
    entities: HashMap<EntityKey, EntityRecord>,
    names: HashMap<(EntityTypeId, String), DatabaseId>,
    log_ons: HashMap<EntityKey, LogOnRecord>,
    next_id: DatabaseId,
}

impl Default for MemoryStorage {
    fn default() -> Self {
        Self {
            entities: HashMap::new(),
            names: HashMap::new(),
            log_ons: HashMap::new(),
            next_id: 1,
        }
    }
}

impl MemoryStorage {
    /// An empty store.
    pub fn new() -> Self {
        Self::default()
    }

    /// Store `record` under its key.
    ///
    /// Seeding a real id moves the allocator past it, so a later create
    /// never hands out an id that is already stored. A named record is
    /// indexed; the uniqueness contract is `put_entity`'s to enforce.
    pub fn insert(&mut self, record: EntityRecord) {
        let key = EntityKey {
            type_id: record.type_id,
            db_id: record.db_id,
        };
        self.next_id = self.next_id.max(record.db_id + 1);
        if let Some(name) = record.name.clone() {
            self.names.insert((key.type_id, name), record.db_id);
        }
        self.entities.insert(key, record);
    }

    /// The id the next create will be assigned.
    pub fn next_id(&self) -> DatabaseId {
        self.next_id
    }

    /// Number of stored entities.
    pub fn len(&self) -> usize {
        self.entities.len()
    }

    /// Whether the store is empty.
    pub fn is_empty(&self) -> bool {
        self.entities.is_empty()
    }

    /// Reject `name` when another row of the type already owns it — the
    /// unique index the identifier column carries
    /// (`EntityTypeMapping::hasIdentifier()`). The row's own id always
    /// passes, so a rename to the same name is a no-op.
    fn check_name(
        &self,
        type_id: EntityTypeId,
        name: Option<&String>,
        db_id: DatabaseId,
    ) -> Result<(), DbError> {
        if let Some(name) = name
            && let Some(&owner) = self.names.get(&(type_id, name.clone()))
            && owner != db_id
        {
            return Err(DbError::Storage("entity name already in use".into()));
        }
        Ok(())
    }

    /// Move the row's name index entry from its previous name to the
    /// record's.
    fn reindex_name(
        &mut self,
        type_id: EntityTypeId,
        old: Option<&String>,
        new: Option<&String>,
        db_id: DatabaseId,
    ) {
        if old != new
            && let Some(name) = old
        {
            self.names.remove(&(type_id, name.clone()));
        }
        if let Some(name) = new {
            self.names.insert((type_id, name.clone()), db_id);
        }
    }
}

impl EntityStorage for MemoryStorage {
    fn get_entity(&self, key: &EntityKey) -> Result<Option<EntityRecord>, DbError> {
        Ok(self.entities.get(key).cloned())
    }

    fn put_entity(
        &mut self,
        record: &EntityRecord,
        put_explicit_id: bool,
    ) -> Result<DatabaseId, DbError> {
        if record.db_id == PENDING_DATABASE_ID {
            // Create: the storage assigns the id (put_entity_task.cpp:99-104;
            // the callback receives it, idatabase.hpp:196-197).
            let db_id = self.next_id;
            self.next_id += 1;
            let mut stored = record.clone();
            stored.db_id = db_id;
            self.check_name(stored.type_id, stored.name.as_ref(), db_id)?;
            self.names.extend(
                stored
                    .name
                    .clone()
                    .map(|name| ((stored.type_id, name), db_id)),
            );
            self.entities.insert(key(stored.type_id, db_id), stored);
            return Ok(db_id);
        }

        let key = key(record.type_id, record.db_id);
        if put_explicit_id {
            // Insert under the record's own id (put_entity_task.cpp:105-110).
            // A taken id is a failed insert — the reference's `insertExplicit`
            // returns 0 and the callback reports isOK=false.
            if self.entities.contains_key(&key) {
                return Err(DbError::Storage("entity id already in use".into()));
            }
            self.check_name(record.type_id, record.name.as_ref(), record.db_id)?;
            self.next_id = self.next_id.max(record.db_id + 1);
            self.names.extend(
                record
                    .name
                    .clone()
                    .map(|name| ((record.type_id, name), record.db_id)),
            );
            self.entities.insert(key, record.clone());
            return Ok(record.db_id);
        }

        // Update: the db id must identify an existing row
        // (idatabase.hpp:195-196).
        let Some(old) = self.entities.get(&key) else {
            return Err(DbError::NotFound);
        };
        let old_name = old.name.clone();
        self.check_name(record.type_id, record.name.as_ref(), record.db_id)?;
        self.reindex_name(
            record.type_id,
            old_name.as_ref(),
            record.name.as_ref(),
            record.db_id,
        );
        self.entities.insert(key, record.clone());
        Ok(record.db_id)
    }

    fn del_entity(&mut self, key: &EntityKey) -> Result<(), DbError> {
        match self.entities.remove(key) {
            Some(record) => {
                if let Some(name) = record.name {
                    self.names.remove(&(key.type_id, name));
                }
                // The checked-out entity goes offline with its row
                // (del_entity_task.cpp:66).
                self.log_ons.remove(key);
                Ok(())
            }
            None => Err(DbError::NotFound),
        }
    }

    fn lookup_entity_by_name(
        &self,
        type_id: EntityTypeId,
        name: &str,
    ) -> Result<Option<EntityRecord>, DbError> {
        let Some(&db_id) = self.names.get(&(type_id, name.to_string())) else {
            return Ok(None);
        };
        self.get_entity(&key(type_id, db_id))
    }

    fn lookup_entities(&self, type_id: EntityTypeId) -> Result<Vec<EntityRecord>, DbError> {
        let mut rows: Vec<EntityRecord> = self
            .entities
            .values()
            .filter(|record| record.type_id == type_id)
            .cloned()
            .collect();
        rows.sort_by_key(|record| record.db_id);
        Ok(rows)
    }

    fn set_log_on_record(&mut self, key: &EntityKey, record: LogOnRecord) -> Result<(), DbError> {
        if !self.entities.contains_key(key) {
            return Err(DbError::NotFound);
        }
        self.log_ons.insert(*key, record);
        Ok(())
    }

    fn remove_log_on_record(&mut self, key: &EntityKey) -> Result<(), DbError> {
        self.log_ons.remove(key);
        Ok(())
    }

    fn log_on_record(&self, key: &EntityKey) -> Result<Option<LogOnRecord>, DbError> {
        Ok(self.log_ons.get(key).copied())
    }
}

/// The key for one row.
fn key(type_id: EntityTypeId, db_id: DatabaseId) -> EntityKey {
    EntityKey { type_id, db_id }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::record::EntityRecord;
    use entitydef::value::PropertyValue;
    use foundation::ids::{EntityId, PENDING_DATABASE_ID};

    fn record(db_id: DatabaseId) -> EntityRecord {
        EntityRecord {
            type_id: 3,
            db_id,
            name: None,
            values: vec![PropertyValue::Int(7), PropertyValue::String("hi".into())],
        }
    }

    #[test]
    fn existing_db_id_returns_the_stored_record() {
        let mut store = MemoryStorage::new();
        store.insert(record(1001));

        let found = load_entity(
            &store,
            &EntityKey {
                type_id: 3,
                db_id: 1001,
            },
        )
        .expect("existing db id loads");
        assert_eq!(found, record(1001));
    }

    #[test]
    fn missing_db_id_is_not_found() {
        let mut store = MemoryStorage::new();
        store.insert(record(1001));

        let err = load_entity(
            &store,
            &EntityKey {
                type_id: 3,
                db_id: 9999,
            },
        )
        .expect_err("missing db id is not found");
        assert_eq!(err, DbError::NotFound);
    }

    #[test]
    fn wrong_type_is_not_found() {
        let mut store = MemoryStorage::new();
        store.insert(record(1001));

        let err = load_entity(
            &store,
            &EntityKey {
                type_id: 4,
                db_id: 1001,
            },
        )
        .expect_err("a different type is a different entity");
        assert_eq!(err, DbError::NotFound);
    }

    #[test]
    fn pending_db_id_is_storable_but_not_a_real_key() {
        // PENDING_DATABASE_ID is never a real stored id: a record must be
        // assigned a real db_id before it is inserted.
        let mut store = MemoryStorage::new();
        let mut rec = record(1001);
        rec.db_id = PENDING_DATABASE_ID;
        store.insert(rec.clone());

        assert_eq!(store.len(), 1);
        let found = store.get_entity(&EntityKey {
            type_id: 3,
            db_id: PENDING_DATABASE_ID,
        });
        assert_eq!(found.expect("pending id is a valid map key"), Some(rec));
    }

    #[test]
    fn a_pending_record_creates_with_the_next_allocated_id() {
        let mut store = MemoryStorage::new();

        let first = store
            .put_entity(&record(PENDING_DATABASE_ID), false)
            .expect("create assigns an id");
        let second = store
            .put_entity(&record(PENDING_DATABASE_ID), false)
            .expect("create assigns the next id");
        assert_eq!(first, 1);
        assert_eq!(second, 2);
        assert_eq!(store.next_id(), 3);

        // The stored row carries the assigned id and the written values.
        let found = store
            .get_entity(&EntityKey {
                type_id: 3,
                db_id: first,
            })
            .expect("read back");
        let mut expected = record(PENDING_DATABASE_ID);
        expected.db_id = first;
        assert_eq!(found, Some(expected));
    }

    #[test]
    fn an_update_rewrites_the_existing_row() {
        let mut store = MemoryStorage::new();
        store.insert(record(1001));

        let mut written = record(1001);
        written.values = vec![PropertyValue::Int(9)];
        let db_id = store
            .put_entity(&written, false)
            .expect("update of an existing id");
        assert_eq!(db_id, 1001, "the id is kept");
        assert_eq!(store.len(), 1, "no second row appears");

        let found = store
            .get_entity(&EntityKey {
                type_id: 3,
                db_id: 1001,
            })
            .expect("read back");
        assert_eq!(found, Some(written));
    }

    #[test]
    fn an_update_of_a_missing_id_is_not_ok() {
        let mut store = MemoryStorage::new();

        let err = store
            .put_entity(&record(9999), false)
            .expect_err("a db id that identifies nothing fails");
        assert_eq!(err, DbError::NotFound);
        assert!(store.is_empty());
    }

    #[test]
    fn an_explicit_id_inserts_under_the_given_id() {
        // A fresh store steps its allocator past the explicit id.
        let mut store = MemoryStorage::new();
        let db_id = store
            .put_entity(&record(500), true)
            .expect("explicit insert");
        assert_eq!(db_id, 500);
        assert_eq!(store.next_id(), 501);
        assert!(
            store
                .get_entity(&EntityKey {
                    type_id: 3,
                    db_id: 500
                })
                .expect("read back")
                .is_some()
        );

        // The allocator steps past every stored id, seeded or explicit.
        store.insert(record(1001));
        let next = store
            .put_entity(&record(PENDING_DATABASE_ID), false)
            .expect("create after the explicit insert");
        assert_eq!(next, 1002);
        assert_eq!(store.len(), 3);

        // An explicit insert indexes its name too — the restore-by-id path.
        store
            .put_entity(&named(501, 1, "wenchuan"), true)
            .expect("explicit named insert");
        let found = store
            .lookup_entity_by_name(1, "wenchuan")
            .expect("read by name")
            .expect("the restored row resolves by name");
        assert_eq!(found.db_id, 501);
    }

    #[test]
    fn an_explicit_id_already_in_use_fails_the_write() {
        let mut store = MemoryStorage::new();
        store.insert(record(1001));

        let err = store
            .put_entity(&record(1001), true)
            .expect_err("a taken explicit id is a failed insert");
        assert!(matches!(err, DbError::Storage(_)));

        // The original row is untouched.
        let found = store
            .get_entity(&EntityKey {
                type_id: 3,
                db_id: 1001,
            })
            .expect("read back");
        assert_eq!(found, Some(record(1001)));
    }

    /// The acceptance: a deleted row queries back as missing.
    #[test]
    fn a_deleted_row_queries_back_as_missing() {
        let mut store = MemoryStorage::new();
        store.insert(record(1001));

        let key = EntityKey {
            type_id: 3,
            db_id: 1001,
        };
        store
            .del_entity(&key)
            .expect("the row exists, so it deletes");

        let found = store.get_entity(&key).expect("read back");
        assert_eq!(found, None, "the deleted row is gone");
        assert!(store.is_empty());
    }

    /// The acceptance: a repeated delete is the same missing-row outcome as
    /// the first delete's aftermath (`idatabase.hpp:239-240`, `isOK is
    /// false if the entity doesn't exist`).
    #[test]
    fn a_repeated_delete_is_the_missing_row_outcome() {
        let mut store = MemoryStorage::new();
        store.insert(record(1001));

        let key = EntityKey {
            type_id: 3,
            db_id: 1001,
        };
        store.del_entity(&key).expect("first delete");
        let err = store
            .del_entity(&key)
            .expect_err("the second delete finds nothing");
        assert_eq!(err, DbError::NotFound);
    }

    /// A pending db id is never a real key
    /// (`del_entity_task.cpp:35`, the `MF_ASSERT`).
    #[test]
    fn deleting_a_pending_key_is_the_missing_row_outcome() {
        let mut store = MemoryStorage::new();
        store.insert(record(1001));

        let err = store
            .del_entity(&EntityKey {
                type_id: 3,
                db_id: PENDING_DATABASE_ID,
            })
            .expect_err("a pending id names no row");
        assert_eq!(err, DbError::NotFound);
    }

    /// A named row of one type.
    fn named(db_id: DatabaseId, type_id: EntityTypeId, name: &str) -> EntityRecord {
        EntityRecord {
            type_id,
            db_id,
            name: Some(name.into()),
            values: Vec::new(),
        }
    }

    /// The acceptance: 按名称或类型返回对应记录 — a name resolves its
    /// record and a type returns every record of that type.
    #[test]
    fn a_name_or_a_type_returns_the_matching_records() {
        let mut store = MemoryStorage::new();
        store.insert(named(1, 1, "wenchuan"));
        store.insert(named(2, 1, "pinghu"));
        store.insert(named(3, 2, "the dragon"));

        // By name: the identifier resolves the one row of that type.
        let found = store
            .lookup_entity_by_name(1, "wenchuan")
            .expect("read by name")
            .expect("the name is in the store");
        assert_eq!(found.db_id, 1);

        // The same name under another type is a different entity.
        assert_eq!(
            store
                .lookup_entity_by_name(2, "wenchuan")
                .expect("read by name"),
            None
        );
        assert_eq!(
            store.lookup_entity_by_name(1, "nobody").expect("read"),
            None,
            "an unknown name is not a failure"
        );

        // By type: every row of the type, in db-id order, and only those.
        let avatars = store.lookup_entities(1).expect("read by type");
        assert_eq!(
            avatars.iter().map(|row| row.db_id).collect::<Vec<_>>(),
            vec![1, 2]
        );
        assert_eq!(store.lookup_entities(2).expect("read").len(), 1);
        assert_eq!(
            store
                .lookup_entities(9)
                .expect("a type with no rows is empty, not failed"),
            Vec::<EntityRecord>::new()
        );

        // The composed load by identifier.
        let found = load_entity_by_name(&store, 2, "the dragon").expect("load by name");
        assert_eq!(found.db_id, 3);
        let err = load_entity_by_name(&store, 1, "nobody").expect_err("unknown name");
        assert_eq!(err, DbError::NotFound);
    }

    /// The identifier column carries a unique index per type
    /// (`EntityTypeMapping::hasIdentifier()`): a second row claiming a
    /// taken name fails the write, and the original row is untouched.
    #[test]
    fn a_taken_name_fails_the_write() {
        let mut store = MemoryStorage::new();
        let db_id = store
            .put_entity(&named(PENDING_DATABASE_ID, 1, "wenchuan"), false)
            .expect("create the first wenchuan");

        let mut second = named(PENDING_DATABASE_ID, 1, "wenchuan");
        second.values.push(PropertyValue::Int(1));
        let err = store
            .put_entity(&second, false)
            .expect_err("the name is taken");
        assert!(matches!(err, DbError::Storage(_)));

        // The original row is still the only wenchuan.
        let found = store
            .lookup_entity_by_name(1, "wenchuan")
            .expect("read by name")
            .expect("the first row holds the name");
        assert_eq!(found.db_id, db_id);
        assert_eq!(store.len(), 1);

        // The same name under another type is free.
        store
            .put_entity(&named(PENDING_DATABASE_ID, 2, "wenchuan"), false)
            .expect("another type's identifier namespace is its own");
    }

    /// An update can rename a row: the old name is released, the new one
    /// resolves, and a rename to the same name is a no-op.
    #[test]
    fn an_update_can_rename_and_releases_the_old_name() {
        let mut store = MemoryStorage::new();
        let db_id = store
            .put_entity(&named(PENDING_DATABASE_ID, 1, "wenchuan"), false)
            .expect("create");

        let mut renamed = named(PENDING_DATABASE_ID, 1, "pinghu");
        renamed.db_id = db_id;
        store
            .put_entity(&renamed, false)
            .expect("rename via update");

        assert_eq!(
            store.lookup_entity_by_name(1, "wenchuan").expect("read"),
            None,
            "the old name is released"
        );
        let found = store
            .lookup_entity_by_name(1, "pinghu")
            .expect("read")
            .expect("the new name resolves");
        assert_eq!(found.db_id, db_id);

        store
            .put_entity(&renamed, false)
            .expect("renaming to the same name passes the unique index");
    }

    /// An unnamed row can gain an identifier, and a named row can drop
    /// it — the index moves both ways with the update.
    #[test]
    fn an_update_can_gain_or_drop_a_name() {
        let mut store = MemoryStorage::new();
        let db_id = store
            .put_entity(&record(PENDING_DATABASE_ID), false)
            .expect("create unnamed");

        let mut gaining = named(PENDING_DATABASE_ID, 3, "wenchuan");
        gaining.db_id = db_id;
        store.put_entity(&gaining, false).expect("gain the name");
        assert!(
            store
                .lookup_entity_by_name(3, "wenchuan")
                .expect("read by name")
                .is_some()
        );

        store
            .put_entity(&record(db_id), false)
            .expect("drop the name");
        assert_eq!(
            store
                .lookup_entity_by_name(3, "wenchuan")
                .expect("read by name"),
            None,
            "the dropped name resolves nothing"
        );
    }

    /// A log-on record online.
    fn log_on(entity_id: EntityId) -> LogOnRecord {
        LogOnRecord {
            entity_id,
            ip: 0x0A00_0001,
            port: 31234,
            salt: 42,
        }
    }

    /// The acceptance: 并发登录无重复占用 — concurrent logons of one name
    /// leave exactly one row and one online record for it. The first
    /// create claims the name; the loser logs on the existing row instead,
    /// and the upsert replaces the location rather than adding a second
    /// record (`put_entity_task.cpp:120-131`).
    #[test]
    fn concurrent_logons_do_not_double_occupy() {
        let mut store = MemoryStorage::new();

        // The first logon creates the row and goes online.
        let db_id = store
            .put_entity(&named(PENDING_DATABASE_ID, 1, "wenchuan"), false)
            .expect("the first logon claims the name");
        let key = EntityKey { type_id: 1, db_id };
        store
            .set_log_on_record(&key, log_on(101))
            .expect("the row exists, so it goes online");

        // The concurrent logon of the same name cannot create a second
        // row — the unique index refuses it.
        let err = store
            .put_entity(&named(PENDING_DATABASE_ID, 1, "wenchuan"), false)
            .expect_err("the name is already occupied");
        assert!(matches!(err, DbError::Storage(_)));

        // It logs on the existing row instead: the record is an upsert,
        // still exactly one for the row, holding the newest location.
        store
            .set_log_on_record(&key, log_on(202))
            .expect("the relogon replaces the location");
        let online = store
            .log_on_record(&key)
            .expect("read the online state")
            .expect("the row is online");
        assert_eq!(online, log_on(202));
        assert_eq!(store.len(), 1, "no second row appeared");

        // A different name at the same time takes its own row and record.
        let other = store
            .put_entity(&named(PENDING_DATABASE_ID, 1, "pinghu"), false)
            .expect("a distinct name occupies its own row");
        let other_key = EntityKey {
            type_id: 1,
            db_id: other,
        };
        store
            .set_log_on_record(&other_key, log_on(303))
            .expect("the other row goes online");
        assert_eq!(store.log_on_record(&key).expect("read"), Some(log_on(202)));
        assert_eq!(
            store.log_on_record(&other_key).expect("read"),
            Some(log_on(303)),
            "the two occupants hold distinct locations"
        );
    }

    /// A log-on record is refused for a row that does not exist — the
    /// `definitelyExists || checkExists` guard
    /// (`put_entity_task.cpp:123-130`).
    #[test]
    fn a_log_on_record_refuses_a_missing_row() {
        let mut store = MemoryStorage::new();

        let err = store
            .set_log_on_record(
                &EntityKey {
                    type_id: 1,
                    db_id: 7,
                },
                log_on(101),
            )
            .expect_err("no row to check out");
        assert_eq!(err, DbError::NotFound);
        assert_eq!(
            store
                .log_on_record(&EntityKey {
                    type_id: 1,
                    db_id: 7
                })
                .expect("read"),
            None
        );
    }

    /// `DelEntityTask` removes the log-on record with the row
    /// (`del_entity_task.cpp:66`): a deleted entity is offline.
    #[test]
    fn a_delete_takes_the_online_record_with_it() {
        let mut store = MemoryStorage::new();
        let db_id = store
            .put_entity(&named(PENDING_DATABASE_ID, 1, "wenchuan"), false)
            .expect("create");
        let key = EntityKey { type_id: 1, db_id };
        store.set_log_on_record(&key, log_on(101)).expect("online");

        store.del_entity(&key).expect("delete");

        assert_eq!(
            store.log_on_record(&key).expect("read"),
            None,
            "the deleted row is offline"
        );
    }

    /// `removeLogOnRecord` takes the row offline; removing where nothing
    /// is recorded is fine (`put_entity_task.cpp:132-136`).
    #[test]
    fn a_logoff_takes_the_row_offline() {
        let mut store = MemoryStorage::new();
        let db_id = store
            .put_entity(&named(PENDING_DATABASE_ID, 1, "wenchuan"), false)
            .expect("create");
        let key = EntityKey { type_id: 1, db_id };
        store.set_log_on_record(&key, log_on(101)).expect("online");

        store.remove_log_on_record(&key).expect("logoff");
        assert_eq!(store.log_on_record(&key).expect("read"), None);

        store
            .remove_log_on_record(&key)
            .expect("a second logoff is fine");
    }

    /// Deleting a row releases its name for the next claimant.
    #[test]
    fn a_delete_releases_its_name() {
        let mut store = MemoryStorage::new();
        let db_id = store
            .put_entity(&named(PENDING_DATABASE_ID, 1, "wenchuan"), false)
            .expect("create");

        store
            .del_entity(&EntityKey { type_id: 1, db_id })
            .expect("delete");

        assert_eq!(
            store.lookup_entity_by_name(1, "wenchuan").expect("read"),
            None,
            "the deleted row's name resolves nothing"
        );
        store
            .put_entity(&named(PENDING_DATABASE_ID, 1, "wenchuan"), false)
            .expect("the name is free again");
    }
}
