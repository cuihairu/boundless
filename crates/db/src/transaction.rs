//! All-or-nothing entity writes.
//!
//! Every database attempt in the reference runs inside a `MySqlTransaction`
//! (`db_storage_mysql/transaction.cpp`): the constructor issues
//! `START TRANSACTION`, and the destructor rolls the work back unless
//! `commit()` ran first — so a failed write is never partly visible.
//!
//! ```ignore
//! MySqlTransaction( connection );      // throws if START TRANSACTION fails
//! performBackgroundTask( connection ); // the query
//! transaction.commit();                // otherwise ~MySqlTransaction rolls back
//! ```
//!
//! A lost connection changes the shape: `ROLLBACK` is skipped because there is
//! no connection left to send it to, and the connection is flagged as lost.

use entitydef::value::PropertyValue;
use foundation::ids::DatabaseId;

use crate::error::DbError;
use crate::storage::{EntityKey, EntityStorageImage, StorageImage};

/// One staged write: the `putEntity` arguments the transaction will apply.
#[derive(Debug, Clone)]
pub struct StagedWrite {
    /// The entity's key, with a pending id for a create.
    pub key: EntityKey,
    /// The entity's persistent values.
    pub values: Vec<PropertyValue>,
    /// `putExplicitID`: insert under the key's id.
    pub put_explicit_id: bool,
}

/// A group of entity writes applied as one transaction.
///
/// The writes are staged, not sent, so nothing can be partly visible:
/// [`commit`](EntityTransaction::commit) applies every staged write to the
/// storage, or restores the image it took at construction — leaving the
/// database exactly as it stood — when any of them fails. A transaction that
/// is dropped without committing applies nothing either.
pub struct EntityTransaction<'a> {
    storage: &'a mut dyn EntityStorageImage,
    image: StorageImage,
    staged: Vec<StagedWrite>,
}

impl<'a> EntityTransaction<'a> {
    /// Begin a transaction over `storage`, taking the image a rollback restores.
    pub fn new(storage: &'a mut dyn EntityStorageImage) -> Self {
        let image = storage.image();
        Self {
            storage,
            image,
            staged: Vec::new(),
        }
    }

    /// Stage an entity write for commit.
    pub fn put(
        &mut self,
        key: EntityKey,
        values: Vec<PropertyValue>,
        put_explicit_id: bool,
    ) -> &mut Self {
        self.staged.push(StagedWrite {
            key,
            values,
            put_explicit_id,
        });
        self
    }

    /// Apply every staged write, or restore the image when any of them fails.
    ///
    /// Returns the ids the writes landed under. The error is the failure that
    /// unwound the transaction; the storage is already back where it stood.
    pub fn commit(self) -> Result<Vec<DatabaseId>, DbError> {
        let mut committed = Vec::with_capacity(self.staged.len());
        for write in &self.staged {
            match self
                .storage
                .put_entity(&write.key, &write.values, write.put_explicit_id)
            {
                Ok(db_id) => committed.push(db_id),
                Err(error) => {
                    self.storage.restore_image(&self.image);
                    return Err(error);
                }
            }
        }
        Ok(committed)
    }

    /// Discard every staged write and leave the storage untouched.
    pub fn rollback(self) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use foundation::ids::PENDING_DATABASE_ID;

    use crate::record::EntityRecord;
    use crate::storage::{EntityStorage, MemoryConnection, MemoryStorage};

    fn key(type_id: u16, db_id: i64) -> EntityKey {
        EntityKey { type_id, db_id }
    }

    /// The pending-id key a create is staged under.
    fn pending(type_id: u16) -> EntityKey {
        key(type_id, PENDING_DATABASE_ID)
    }

    fn values() -> Vec<PropertyValue> {
        vec![PropertyValue::Int(7), PropertyValue::String("hi".into())]
    }

    /// Seed one entity of type `type_id` under `db_id`.
    fn seed(store: &mut MemoryStorage, type_id: u16, db_id: i64) {
        store.insert(EntityRecord {
            type_id,
            db_id,
            values: vec![PropertyValue::Int(1), PropertyValue::String("a".into())],
        });
    }

    #[test]
    fn a_committed_transaction_applies_every_staged_write() {
        let mut store = MemoryStorage::new();
        let mut tx = EntityTransaction::new(&mut store);
        tx.put(pending(3), values(), false)
            .put(pending(4), values(), false);
        let ids: Vec<DatabaseId> = tx.commit().expect("both writes land");

        assert_eq!(ids.len(), 2);
        assert_eq!(store.len(), 2);
        assert!(
            store
                .get_entity(&key(3, ids[0]))
                .expect("the create is stored")
                .is_some()
        );
        assert!(
            store
                .get_entity(&key(4, ids[1]))
                .expect("the second create is stored")
                .is_some()
        );
    }

    #[test]
    fn a_failed_write_leaves_no_partial_state() {
        let mut store = MemoryStorage::new();
        seed(&mut store, 3, 1);
        // The writes go through a connection rather than the store itself, so
        // the rollback restores the connection's view of the same database.
        let mut connection: MemoryConnection = store.connection();

        // The second write inserts explicitly under an id a row already
        // holds, so the transaction must unwind as a whole: the first write
        // lands nowhere.
        let mut tx = EntityTransaction::new(&mut connection);
        tx.put(pending(3), values(), false)
            .put(key(3, 1), values(), true);
        let ids = tx.commit();

        let error = ids.expect_err("the duplicate insert fails");
        assert_eq!(
            error,
            DbError::AlreadyExists,
            "the unwinding error is the failing write's"
        );
        assert_eq!(store.len(), 1, "nothing else of the transaction survived");
        assert_eq!(
            store
                .get_entity(&key(3, 1))
                .expect("the seeded row is back"),
            Some(EntityRecord {
                type_id: 3,
                db_id: 1,
                values: vec![PropertyValue::Int(1), PropertyValue::String("a".into())],
            }),
            "the restored row is the one that stood there"
        );
    }

    #[test]
    fn an_update_that_finds_no_row_rolls_the_group_back() {
        let mut store = MemoryStorage::new();
        seed(&mut store, 3, 1);

        let mut tx = EntityTransaction::new(&mut store);
        tx.put(pending(3), values(), false)
            .put(key(9, 42), values(), false);
        let error = tx
            .commit()
            .expect_err("the missing row makes the update fail");

        assert_eq!(error, DbError::NotFound);
        assert_eq!(store.len(), 1, "the create in the same transaction is gone");
    }

    #[test]
    fn a_rollback_discards_everything_it_staged() {
        let mut store = MemoryStorage::new();
        let mut tx = EntityTransaction::new(&mut store);
        tx.put(pending(3), values(), false);
        tx.rollback();
        assert!(store.is_empty());
    }

    #[test]
    fn a_dropped_transaction_applies_nothing() {
        let mut store = MemoryStorage::new();
        {
            let mut tx = EntityTransaction::new(&mut store);
            tx.put(pending(3), values(), false);
        }
        assert!(store.is_empty());
    }

    #[test]
    fn another_connection_does_not_see_a_transaction_before_it_commits() {
        let store = MemoryStorage::new();
        let mut connection: MemoryConnection = store.connection();
        assert!(
            connection
                .all_entities()
                .expect("a fresh connection lists the database")
                .is_empty(),
            "a fresh connection sees an empty database"
        );

        let mut tx = EntityTransaction::new(&mut connection);
        tx.put(pending(3), values(), false);
        tx.rollback();
        assert!(
            connection
                .all_entities()
                .expect("the database is still empty")
                .is_empty(),
            "a rolled-back write is not sent at all"
        );

        let mut tx = EntityTransaction::new(&mut connection);
        tx.put(pending(3), values(), false);
        tx.commit().expect("the commit reaches the shared database");
        assert_eq!(
            connection
                .all_entities()
                .expect("the committed row is listed")
                .len(),
            1
        );
    }
}
