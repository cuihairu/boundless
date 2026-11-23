//! The storage contract (`IDatabase`) and the entity get/load/write/delete
//! operations.
//!
//! The reference's persistence contract is `lib/db_storage/idatabase.hpp`
//! (`class IDatabase`). Each entity operation is one `todo.md` item: this
//! module carries `get_entity` (BW-09-01-01), `put_entity` (BW-09-01-02)
//! and `del_entity` (BW-09-01-03); lookup (BW-09-01-04) and id allocation
//! (BW-09-01-05) join the trait as they land. The in-memory backend lets
//! the entity-operation acceptance cases run before any real backend
//! exists; MySQL/XML/SQLite/PostgreSQL plug into the same trait.

use std::collections::HashMap;

use foundation::ids::{DatabaseId, EntityTypeId, PENDING_DATABASE_ID};

use crate::error::DbError;
use crate::record::EntityRecord;

/// `EntityKey`: the identity of a stored entity, `(type, database id)`.
///
/// `lib/db_storage/entity_key.hpp` `EntityKey`. The reference resolves by
/// `name` when `dbID == 0`; the name leg is `getDatabaseIDFromName`, which
/// joins with the lookup item (BW-09-01-04).
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
}

/// The DBApp load: fetch the record for `key`, or [`DbError::NotFound`]
/// when the entity is not in storage.
pub fn load_entity(storage: &impl EntityStorage, key: &EntityKey) -> Result<EntityRecord, DbError> {
    storage.get_entity(key)?.ok_or(DbError::NotFound)
}

/// An in-memory backend.
///
/// The BW-09-01 entity-operation acceptance cases run against this before a
/// real backend exists. Created rows get ids from a monotonically growing
/// allocator, the way an `AUTO_INCREMENT` primary key does
/// (`entityTypeMapping_.insertNew`, `put_entity_task.cpp:99-104`).
#[derive(Debug, Clone)]
pub struct MemoryStorage {
    entities: HashMap<EntityKey, EntityRecord>,
    next_id: DatabaseId,
}

impl Default for MemoryStorage {
    fn default() -> Self {
        Self {
            entities: HashMap::new(),
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
    /// never hands out an id that is already stored.
    pub fn insert(&mut self, record: EntityRecord) {
        let key = EntityKey {
            type_id: record.type_id,
            db_id: record.db_id,
        };
        self.next_id = self.next_id.max(record.db_id + 1);
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
            self.next_id = self.next_id.max(record.db_id + 1);
            self.entities.insert(key, record.clone());
            return Ok(record.db_id);
        }

        // Update: the db id must identify an existing row
        // (idatabase.hpp:195-196).
        if !self.entities.contains_key(&key) {
            return Err(DbError::NotFound);
        }
        self.entities.insert(key, record.clone());
        Ok(record.db_id)
    }

    fn del_entity(&mut self, key: &EntityKey) -> Result<(), DbError> {
        self.entities
            .remove(key)
            .map(|_| ())
            .ok_or(DbError::NotFound)
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
    use foundation::ids::PENDING_DATABASE_ID;

    fn record(db_id: DatabaseId) -> EntityRecord {
        EntityRecord {
            type_id: 3,
            db_id,
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
}
