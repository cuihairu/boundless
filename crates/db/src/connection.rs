//! Independent connections onto one shared storage backend.
//!
//! The BW-09-01-02 acceptance, "创建与更新可从独立连接复读", is that a write
//! performed over one connection is re-read over another: the data lives in
//! the storage, never in a per-connection cache. The reference's
//! connections are MySQL sessions — `MySqlDatabase::putEntity`
//! (`mysql_database.cpp:451-473`) queues the `PutEntityTask` and it runs on
//! whatever pooled `MySql` session picks it up
//! (`put_entity_task.cpp:50`, `performBackgroundTask(MySql & conn)`). Here
//! one shared [`MemoryStorage`] sits behind any number of
//! [`StorageConnection`]s, each an independent handle with no state of its
//! own.

use std::cell::RefCell;
use std::rc::Rc;

use foundation::ids::DatabaseId;

use crate::error::DbError;
use crate::record::EntityRecord;
use crate::storage::{EntityKey, EntityStorage, MemoryStorage};

/// One connection onto a shared storage backend.
///
/// A handle with no per-connection state: every operation goes through to
/// the store, so a reader opened before a write still sees it afterwards.
#[derive(Debug, Clone)]
pub struct StorageConnection {
    store: Rc<RefCell<MemoryStorage>>,
}

impl StorageConnection {
    /// Open a connection onto `store`.
    pub fn open(store: Rc<RefCell<MemoryStorage>>) -> Self {
        StorageConnection { store }
    }

    /// The shared store behind every connection onto it.
    pub fn store(&self) -> Rc<RefCell<MemoryStorage>> {
        Rc::clone(&self.store)
    }
}

impl EntityStorage for StorageConnection {
    fn get_entity(&self, key: &EntityKey) -> Result<Option<EntityRecord>, DbError> {
        self.store.borrow().get_entity(key)
    }

    fn put_entity(
        &mut self,
        record: &EntityRecord,
        put_explicit_id: bool,
    ) -> Result<DatabaseId, DbError> {
        self.store.borrow_mut().put_entity(record, put_explicit_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::load_entity;
    use entitydef::value::PropertyValue;
    use foundation::ids::PENDING_DATABASE_ID;

    fn avatar(name: &str) -> EntityRecord {
        let mut record = EntityRecord::new(1);
        record.values = vec![PropertyValue::String(name.into())];
        record
    }

    /// The acceptance: 创建与更新可从独立连接复读 — a create and an update
    /// performed on the writer connection are re-read on a second, fully
    /// independent connection.
    #[test]
    fn creates_and_updates_are_re_read_from_an_independent_connection() {
        let store = Rc::new(RefCell::new(MemoryStorage::new()));
        let mut writer = StorageConnection::open(Rc::clone(&store));
        let reader = StorageConnection::open(Rc::clone(&store));

        // Create on the writer; the store assigns the id.
        let db_id = writer
            .put_entity(&avatar("wenchuan"), false)
            .expect("create over the writer connection");
        assert_ne!(db_id, PENDING_DATABASE_ID);

        // The reader connection, opened before the write, sees the row.
        let key = EntityKey { type_id: 1, db_id };
        let found = reader
            .get_entity(&key)
            .expect("read over the reader connection")
            .expect("the created row is there");
        assert_eq!(found.values, vec![PropertyValue::String("wenchuan".into())]);

        // Update on the writer; the id is kept.
        let mut written = found.clone();
        written.values = vec![PropertyValue::String("renamed".into())];
        let updated_id = writer
            .put_entity(&written, false)
            .expect("update over the writer connection");
        assert_eq!(updated_id, db_id);

        // The reader sees the new values under the same id.
        let found = reader
            .get_entity(&key)
            .expect("read over the reader connection")
            .expect("the updated row is there");
        assert_eq!(found.values, vec![PropertyValue::String("renamed".into())]);
    }

    /// Connections share one allocator: two writers never hand out the same
    /// id, and each connection's writes are visible to the other
    /// (`put_entity_task.cpp:99-104`, one `AUTO_INCREMENT` per table).
    #[test]
    fn connections_share_the_id_allocator_and_the_rows() {
        let store = Rc::new(RefCell::new(MemoryStorage::new()));
        let mut first = StorageConnection::open(Rc::clone(&store));
        let mut second = StorageConnection::open(Rc::clone(&store));

        let a = first.put_entity(&avatar("a"), false).expect("create");
        let b = second.put_entity(&avatar("b"), false).expect("create");
        assert_ne!(a, b, "the allocator is shared");
        assert_eq!(
            first.store().borrow().len(),
            2,
            "both writes landed in the one shared store"
        );

        // Each connection reads what the other wrote.
        for (connection, db_id, name) in [(&first, a, "a"), (&second, b, "b")] {
            let found = load_entity(connection, &EntityKey { type_id: 1, db_id })
                .expect("the other connection's write is visible");
            assert_eq!(found.values, vec![PropertyValue::String(name.into())]);
        }
    }

    /// A connection reports another connection's failed writes the same way:
    /// an update against a db id neither connection created is not ok
    /// (`idatabase.hpp:195-196`).
    #[test]
    fn a_missing_id_fails_on_every_connection() {
        let store = Rc::new(RefCell::new(MemoryStorage::new()));
        let mut writer = StorageConnection::open(Rc::clone(&store));
        let mut other = StorageConnection::open(Rc::clone(&store));

        writer
            .put_entity(&avatar("wenchuan"), false)
            .expect("create");

        let mut ghost = avatar("ghost");
        ghost.db_id = 42;
        let err = other
            .put_entity(&ghost, false)
            .expect_err("the id exists on no connection");
        assert_eq!(err, DbError::NotFound);
    }
}
