//! The storage contract (`IDatabase`) and the entity get/load operations.
//!
//! The reference's persistence contract is `lib/db_storage/idatabase.hpp`
//! (`class IDatabase`). Each entity operation is one `todo.md` item: this
//! module carries `get_entity` (BW-09-01-01); `put_entity` (BW-09-01-02),
//! `del_entity` (BW-09-01-03), lookup (BW-09-01-04) and id allocation
//! (BW-09-01-05) join the trait as they land. The in-memory backend lets
//! the entity-operation acceptance cases run before any real backend
//! exists; MySQL/XML/SQLite/PostgreSQL plug into the same trait.

use std::collections::HashMap;

use foundation::ids::{DatabaseId, EntityTypeId};

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
}

/// The DBApp load: fetch the record for `key`, or [`DbError::NotFound`]
/// when the entity is not in storage.
pub fn load_entity(storage: &impl EntityStorage, key: &EntityKey) -> Result<EntityRecord, DbError> {
    storage.get_entity(key)?.ok_or(DbError::NotFound)
}

/// An in-memory backend.
///
/// The BW-09-01 entity-operation acceptance cases run against this before a
/// real backend exists. `insert` seeds a known entity and becomes the
/// `put_entity` implementation in BW-09-01-02.
#[derive(Debug, Default, Clone)]
pub struct MemoryStorage {
    entities: HashMap<EntityKey, EntityRecord>,
}

impl MemoryStorage {
    /// An empty store.
    pub fn new() -> Self {
        Self::default()
    }

    /// Store `record` under its key.
    pub fn insert(&mut self, record: EntityRecord) {
        let key = EntityKey {
            type_id: record.type_id,
            db_id: record.db_id,
        };
        self.entities.insert(key, record);
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
}
