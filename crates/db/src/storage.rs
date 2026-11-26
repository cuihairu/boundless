//! The storage contract (`IDatabase`) and the entity get/load/put
//! operations.
//!
//! The reference's persistence contract is `lib/db_storage/idatabase.hpp`
//! (`class IDatabase`). Each entity operation is one `todo.md` item: this
//! module carries `get_entity` (BW-09-01-01) and `put_entity`
//! (BW-09-01-02); `del_entity` (BW-09-01-03), lookup (BW-09-01-04) and id
//! allocation (BW-09-01-05) join the trait as they land. The in-memory
//! backend lets the entity-operation acceptance cases run before any real
//! backend exists; MySQL/XML/SQLite/PostgreSQL plug into the same trait.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use entitydef::value::PropertyValue;
use foundation::ids::{DatabaseId, EntityTypeId, PENDING_DATABASE_ID};
use mercury::endpoint::Address;

use crate::error::DbError;
use crate::log_on::LogOnRecord;
use crate::record::EntityRecord;

/// `EntityKey`: the identity of a stored entity, `(type, database id)`.
///
/// `lib/db_storage/entity_key.hpp` `EntityKey`. The reference resolves by
/// `name` when `dbID == 0`; the name leg is `getDatabaseIDFromName`, which
/// joins with the lookup item (BW-09-01-04).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct EntityKey {
    /// `EntityKey::typeID`.
    pub type_id: EntityTypeId,
    /// `EntityKey::dbID`. `PENDING_DATABASE_ID`/0 means "resolve by name".
    pub db_id: DatabaseId,
}

/// The storage face of `EntityMailBoxRef` — where a checked-out
/// entity's base lives and which entity it is (`basictypes.hpp`). The
/// occupancy contract never steals a holder, so the salt is carried but
/// never compared (`Address::eq` ignores it, as in the reference).
#[derive(Debug, Clone, PartialEq)]
pub struct BaseMailbox {
    /// The base's address.
    pub addr: Address,
    /// `EntityMailBoxRef::id`.
    pub entity_id: i32,
}

/// The outcome of a checkout attempt: the relogon face
/// (`DBApp::onLogOnLoggedOnUser`, `dbapp.cpp:2153-2184`) — a free entity
/// is granted, an occupied one is refused with the current holder
/// reported back and the record left untouched.
#[derive(Debug, Clone, PartialEq)]
pub enum CheckOut {
    /// The checkout was recorded.
    Granted,
    /// Already occupied; the current holder.
    Elsewhere(BaseMailbox),
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

    /// `IDatabase::putEntity` (`idatabase.hpp`): write an entity's
    /// persistent data. The legs match the reference backends:
    ///
    /// * `key.db_id == 0` (`PENDING_DATABASE_ID`): create a new row and
    ///   allocate its id — the XML backend's `++maxID_`
    ///   (`xml_database.cpp:626`) or `insertNew`
    ///   (`put_entity_task.cpp:85-88`). The returned id is the
    ///   `onPutEntityComplete` payload.
    /// * `key.db_id != 0`, `put_explicit_id == false`: update the row.
    ///   A missing row is the XML backend's `isOK=false`
    ///   (`xml_database.cpp:617-620`) → [`DbError::NotFound`].
    /// * `put_explicit_id == true`: insert under the given id
    ///   (`insertExplicit`, `put_entity_task.cpp:89-97`); a duplicate id
    ///   is [`DbError::AlreadyExists`] and the original row is kept.
    ///
    /// The reference legs that ride on the same call are out of scope:
    /// the base-mailbox/logon-record and auto-load updates are the
    /// online-state face (BW-09-01-05), and the trailing
    /// position/direction/spaceID/gameTime fields are the snapshot face
    /// (BW-09-04).
    fn put_entity(
        &mut self,
        key: &EntityKey,
        values: &[PropertyValue],
        put_explicit_id: bool,
    ) -> Result<DatabaseId, DbError>;

    /// `IDatabase::delEntity` (`idatabase.hpp`): remove the row for
    /// `key`. Both reference backends delete by id and report a missing
    /// row as failure (`xml_database.cpp:811-816` `deleteEntity` returns
    /// false, `del_entity_task.cpp` `deleteWithID` → `setFailure`), so a
    /// missing row — including a repeated delete — is
    /// [`DbError::NotFound`], and a successful delete leaves nothing to
    /// query.
    ///
    /// The name-resolution leg (`key.db_id == 0` resolves via the
    /// identifier property) joins BW-09-01-04; the logon-record,
    /// auto-load and active-set cleanups are the online-state face
    /// (BW-09-01-05).
    fn del_entity(&mut self, key: &EntityKey) -> Result<(), DbError>;

    /// `IDatabase`'s logon-record face (`bigworldLogOnRecords` / the
    /// XML `activeSet_`): the checkout holder of `key`, if any
    /// (`getLogOnRecord`).
    fn logon_of(&self, key: &EntityKey) -> Result<Option<BaseMailbox>, DbError>;

    /// The occupancy grant. It succeeds only for an existing row
    /// (`put_entity_task.cpp:103-108` checks existence before adding an
    /// invalid logon record) that is not already checked out elsewhere.
    /// A concurrent or repeated logon for an occupied entity never
    /// creates a second record — the current holder comes back in
    /// [`CheckOut::Elsewhere`], the acceptance for BW-09-01-05.
    fn check_out(&mut self, key: &EntityKey, mailbox: BaseMailbox) -> Result<CheckOut, DbError>;

    /// `removeLogOnRecord`: release the checkout. Absent records are
    /// fine (the reference's unconditional DELETE).
    fn check_in(&mut self, key: &EntityKey) -> Result<(), DbError>;

    /// Every stored record, in a deterministic order
    /// `(type_id, db_id)`. The reference queries per type with SQL
    /// (`entity_type_mapping.cpp` builds the SELECT); the in-memory
    /// backend hands everything over and the lookup operations filter,
    /// which keeps the contract synchronous and backend-free.
    fn all_entities(&self) -> Result<Vec<EntityRecord>, DbError>;
}

/// The DBApp load: fetch the record for `key`, or [`DbError::NotFound`]
/// when the entity is not in storage.
pub fn load_entity(storage: &impl EntityStorage, key: &EntityKey) -> Result<EntityRecord, DbError> {
    storage.get_entity(key)?.ok_or(DbError::NotFound)
}

/// The shared rows and checkouts of an in-memory database.
#[derive(Debug, Default, Clone)]
pub(crate) struct MemoryInner {
    entities: HashMap<EntityKey, EntityRecord>,
    logons: HashMap<EntityKey, BaseMailbox>,
}

impl MemoryInner {
    /// The `putEntity` legs (`xml_database.cpp:561-743`,
    /// `put_entity_task.cpp:71-100`).
    fn put(
        &mut self,
        key: &EntityKey,
        values: &[PropertyValue],
        put_explicit_id: bool,
    ) -> Result<DatabaseId, DbError> {
        if key.db_id == PENDING_DATABASE_ID {
            // Create: a fresh id above every stored row — the XML
            // backend's `++maxID_`, seeded past the rows loaded at
            // startup (xml_database.cpp:215-230).
            let db_id = self.entities.keys().map(|k| k.db_id).max().unwrap_or(0) + 1;
            self.entities.insert(
                EntityKey {
                    type_id: key.type_id,
                    db_id,
                },
                EntityRecord {
                    type_id: key.type_id,
                    db_id,
                    values: values.to_vec(),
                },
            );
            return Ok(db_id);
        }

        let existing = self.entities.get(key).cloned();
        if put_explicit_id {
            // `insertExplicit`: INSERT under the given id — a duplicate
            // primary key fails the put and keeps the original row.
            if existing.is_some() {
                return Err(DbError::AlreadyExists);
            }
            self.entities.insert(
                *key,
                EntityRecord {
                    type_id: key.type_id,
                    db_id: key.db_id,
                    values: values.to_vec(),
                },
            );
            return Ok(key.db_id);
        }

        // Update of an existing row; a missing row is the XML backend's
        // `isOK=false` (xml_database.cpp:617-620).
        let mut record = existing.ok_or(DbError::NotFound)?;
        record.values = values.to_vec();
        self.entities.insert(*key, record);
        Ok(key.db_id)
    }

    /// The `delEntity` leg (`xml_database.cpp:824-854`): find the row,
    /// and only an existing row deletes — a missing row is the
    /// reference's `isOK=false`.
    fn del(&mut self, key: &EntityKey) -> Result<(), DbError> {
        // The reference delEntity removes the logon record with the row
        // (`xml_database.cpp:805-811`, `del_entity_task.cpp`).
        self.logons.remove(key);
        self.entities
            .remove(key)
            .map(|_| ())
            .ok_or(DbError::NotFound)
    }

    fn logon_of(&self, key: &EntityKey) -> Result<Option<BaseMailbox>, DbError> {
        Ok(self.logons.get(key).cloned())
    }

    /// The occupancy grant: an existing, unoccupied row records the
    /// holder; an occupied one reports the holder and is left alone.
    fn check_out(&mut self, key: &EntityKey, mailbox: BaseMailbox) -> Result<CheckOut, DbError> {
        if !self.entities.contains_key(key) {
            // The reference never adds a logon record for a missing row.
            return Err(DbError::NotFound);
        }
        if let Some(current) = self.logons.get(key) {
            return Ok(CheckOut::Elsewhere(current.clone()));
        }
        self.logons.insert(*key, mailbox);
        Ok(CheckOut::Granted)
    }

    fn check_in(&mut self, key: &EntityKey) -> Result<(), DbError> {
        self.logons.remove(key);
        Ok(())
    }
}

/// An in-memory database. Cloning it yields another handle to the same
/// database — an independent connection — not a snapshot.
///
/// The BW-09-01 entity-operation acceptance cases run against this before
/// a real backend exists. `insert` seeds a known entity; the entity
/// operations go through the [`EntityStorage`] methods.
#[derive(Debug, Default, Clone)]
pub struct MemoryStorage {
    inner: Rc<RefCell<MemoryInner>>,
}

impl MemoryStorage {
    /// An empty store.
    pub fn new() -> Self {
        Self::default()
    }

    /// An independent connection to the same database: writes through one
    /// handle are visible from the others.
    pub fn connection(&self) -> MemoryConnection {
        MemoryConnection {
            inner: Rc::clone(&self.inner),
        }
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
        self.inner.borrow_mut().entities.insert(key, record);
    }

    /// The id the next create will be assigned.
    pub fn next_id(&self) -> DatabaseId {
        self.next_id
    }

    /// Remove the row and everything hanging off it — its name index
    /// entry and its online record. The rollback leg of a transaction
    /// (`crate::transaction`) uses this to take a created row back out;
    /// unlike [`EntityStorage::del_entity`] the row is known to exist, so
    /// there is no missing-row outcome. The id allocator is untouched: an
    /// `AUTO_INCREMENT` value a rollback burns stays burned.
    pub(crate) fn remove_entity(&mut self, key: &EntityKey) {
        if let Some(record) = self.entities.remove(key)
            && let Some(name) = record.name
        {
            self.names.remove(&(key.type_id, name));
        }
        self.log_ons.remove(key);
    }

    /// Put a row back exactly as it was, the rollback leg of an update or
    /// a delete: every name entry pointing at the row goes first, then
    /// the record's own name is indexed again.
    pub(crate) fn restore_entity(&mut self, record: EntityRecord) {
        self.names
            .retain(|(type_id, _), id| !(*type_id == record.type_id && *id == record.db_id));
        self.insert(record);
    }

    /// Put the online record back to what it was, the rollback leg of a
    /// log-on write.
    pub(crate) fn restore_log_on(&mut self, key: &EntityKey, record: Option<LogOnRecord>) {
        match record {
            Some(record) => {
                self.log_ons.insert(*key, record);
            }
            None => {
                self.log_ons.remove(key);
            }
        }
    }

    /// Number of stored entities.
    pub fn len(&self) -> usize {
        self.inner.borrow().entities.len()
    }

    /// Whether the store is empty.
    pub fn is_empty(&self) -> bool {
        self.inner.borrow().entities.is_empty()
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
        Ok(self.inner.borrow().entities.get(key).cloned())
    }

    fn put_entity(
        &mut self,
        key: &EntityKey,
        values: &[PropertyValue],
        put_explicit_id: bool,
    ) -> Result<DatabaseId, DbError> {
        self.inner.borrow_mut().put(key, values, put_explicit_id)
    }

    fn del_entity(&mut self, key: &EntityKey) -> Result<(), DbError> {
        self.inner.borrow_mut().del(key)
    }

    fn logon_of(&self, key: &EntityKey) -> Result<Option<BaseMailbox>, DbError> {
        self.inner.borrow().logon_of(key)
    }

    fn check_out(&mut self, key: &EntityKey, mailbox: BaseMailbox) -> Result<CheckOut, DbError> {
        self.inner.borrow_mut().check_out(key, mailbox)
    }

    fn check_in(&mut self, key: &EntityKey) -> Result<(), DbError> {
        self.inner.borrow_mut().check_in(key)
    }

    fn all_entities(&self) -> Result<Vec<EntityRecord>, DbError> {
        let mut records: Vec<EntityRecord> =
            self.inner.borrow().entities.values().cloned().collect();
        records.sort_by_key(|r| (r.type_id, r.db_id));
        Ok(records)
    }
}

/// A connection to a [`MemoryStorage`]: the read/write face a caller
/// holds. Independent connections see each other's writes — the
/// `put_entity` acceptance.
#[derive(Debug, Clone)]
pub struct MemoryConnection {
    inner: Rc<RefCell<MemoryInner>>,
}

impl EntityStorage for MemoryConnection {
    fn get_entity(&self, key: &EntityKey) -> Result<Option<EntityRecord>, DbError> {
        Ok(self.inner.borrow().entities.get(key).cloned())
    }

    fn put_entity(
        &mut self,
        key: &EntityKey,
        values: &[PropertyValue],
        put_explicit_id: bool,
    ) -> Result<DatabaseId, DbError> {
        self.inner.borrow_mut().put(key, values, put_explicit_id)
    }

    fn del_entity(&mut self, key: &EntityKey) -> Result<(), DbError> {
        self.inner.borrow_mut().del(key)
    }

    fn logon_of(&self, key: &EntityKey) -> Result<Option<BaseMailbox>, DbError> {
        self.inner.borrow().logon_of(key)
    }

    fn check_out(&mut self, key: &EntityKey, mailbox: BaseMailbox) -> Result<CheckOut, DbError> {
        self.inner.borrow_mut().check_out(key, mailbox)
    }

    fn check_in(&mut self, key: &EntityKey) -> Result<(), DbError> {
        self.inner.borrow_mut().check_in(key)
    }

    fn all_entities(&self) -> Result<Vec<EntityRecord>, DbError> {
        let mut records: Vec<EntityRecord> =
            self.inner.borrow().entities.values().cloned().collect();
        records.sort_by_key(|r| (r.type_id, r.db_id));
        Ok(records)
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
        if self.entities.contains_key(key) {
            // The checked-out entity goes offline with its row
            // (del_entity_task.cpp:66).
            self.remove_entity(key);
            Ok(())
        } else {
            Err(DbError::NotFound)
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
    fn a_create_allocates_a_fresh_id_visible_from_an_independent_connection() {
        let mut store = MemoryStorage::new();
        // A connection opened before any write still sees the rows
        // written afterwards — the writes land in the database, not in
        // a caller-side cache.
        let reader = store.connection();

        let values = vec![PropertyValue::Int(7), PropertyValue::String("hi".into())];
        let first = store
            .put_entity(
                &EntityKey {
                    type_id: 3,
                    db_id: PENDING_DATABASE_ID,
                },
                &values,
                false,
            )
            .expect("create writes");
        assert_eq!(first, 1);

        let second = store
            .put_entity(
                &EntityKey {
                    type_id: 3,
                    db_id: PENDING_DATABASE_ID,
                },
                &values,
                false,
            )
            .expect("second create writes");
        assert_eq!(second, 2);

        assert_eq!(store.len(), 2);
        let found = reader
            .get_entity(&EntityKey {
                type_id: 3,
                db_id: first,
            })
            .expect("independent connection reads")
            .expect("created row exists");
        assert_eq!(found.values, values);
    }

    #[test]
    fn an_update_overwrites_the_row_visible_from_an_independent_connection() {
        let mut store = MemoryStorage::new();
        let key = EntityKey {
            type_id: 3,
            db_id: PENDING_DATABASE_ID,
        };
        let id = store
            .put_entity(&key, &[PropertyValue::Int(1)], false)
            .expect("create writes");

        let reader = store.connection();
        let updated = store
            .put_entity(
                &EntityKey {
                    type_id: 3,
                    db_id: id,
                },
                &[PropertyValue::Int(2)],
                false,
            )
            .expect("update writes");
        assert_eq!(updated, id);
        assert_eq!(store.len(), 1);
        let found = reader
            .get_entity(&EntityKey {
                type_id: 3,
                db_id: id,
            })
            .expect("independent connection reads")
            .expect("updated row exists");
        assert_eq!(found.values, vec![PropertyValue::Int(2)]);
    }

    #[test]
    fn an_update_of_a_missing_row_is_not_found() {
        let mut store = MemoryStorage::new();

        let err = store
            .put_entity(
                &EntityKey {
                    type_id: 3,
                    db_id: 4242,
                },
                &[PropertyValue::Int(1)],
                false,
            )
            .expect_err("missing row update fails");
        assert_eq!(err, DbError::NotFound);
        assert!(store.is_empty());
    }

    #[test]
    fn an_explicit_id_inserts_under_that_id_and_rejects_duplicates() {
        let mut store = MemoryStorage::new();
        let key = EntityKey {
            type_id: 3,
            db_id: 7,
        };

        let id = store
            .put_entity(&key, &[PropertyValue::Int(1)], true)
            .expect("explicit insert writes");
        assert_eq!(id, 7);

        let err = store
            .put_entity(&key, &[PropertyValue::Int(9)], true)
            .expect_err("duplicate explicit insert fails");
        assert_eq!(err, DbError::AlreadyExists);
        // The original row is intact.
        let found = store.get_entity(&key).expect("read").expect("row exists");
        assert_eq!(found.values, vec![PropertyValue::Int(1)]);
    }

    #[test]
    fn created_ids_clear_the_seeded_rows() {
        // XMLDatabase seeds maxID_ past the rows loaded at startup
        // (xml_database.cpp:215-230).
        let mut store = MemoryStorage::new();
        store.insert(record(5));

        let id = store
            .put_entity(
                &EntityKey {
                    type_id: 3,
                    db_id: PENDING_DATABASE_ID,
                },
                &[PropertyValue::Int(1)],
                false,
            )
            .expect("create writes");
        assert_eq!(id, 6);
    }

    #[test]
    fn a_write_through_one_connection_is_visible_from_the_others() {
        let store = MemoryStorage::new();
        let mut writer = store.connection();
        let reader = store.connection();

        let written = writer
            .put_entity(
                &EntityKey {
                    type_id: 3,
                    db_id: PENDING_DATABASE_ID,
                },
                &[PropertyValue::Int(3)],
                false,
            )
            .expect("connection write");
        assert_eq!(written, 1);

        let key = EntityKey {
            type_id: 3,
            db_id: written,
        };
        assert_eq!(
            reader
                .get_entity(&key)
                .expect("read")
                .expect("row exists")
                .values,
            vec![PropertyValue::Int(3)]
        );
        assert_eq!(
            store
                .get_entity(&key)
                .expect("read")
                .expect("row exists")
                .values,
            vec![PropertyValue::Int(3)]
        );
    }

    #[test]
    fn a_deleted_row_stops_matching_and_a_repeat_delete_matches_that() {
        let mut store = MemoryStorage::new();
        store.insert(record(1001));
        let key = EntityKey {
            type_id: 3,
            db_id: 1001,
        };

        store.del_entity(&key).expect("first delete succeeds");
        // The query after the delete matches: the row is gone.
        assert_eq!(store.get_entity(&key).expect("read"), None);
        assert_eq!(store.len(), 0);
        let err = load_entity(&store, &key).expect_err("load after delete");
        assert_eq!(err, DbError::NotFound);

        // And a repeated delete matches the reference's isOK=false.
        let err = store.del_entity(&key).expect_err("repeat delete");
        assert_eq!(err, DbError::NotFound);
    }

    #[test]
    fn a_delete_through_one_connection_is_visible_from_the_others() {
        let mut store = MemoryStorage::new();
        store.insert(record(1001));
        let mut deleter = store.connection();
        let reader = store.connection();
        let key = EntityKey {
            type_id: 3,
            db_id: 1001,
        };

        deleter.del_entity(&key).expect("delete through connection");
        assert_eq!(reader.get_entity(&key).expect("read"), None);
        assert_eq!(store.get_entity(&key).expect("read"), None);
    }

    #[test]
    fn a_delete_only_takes_its_own_row() {
        let mut store = MemoryStorage::new();
        store.insert(record(1001));
        let mut other = record(1002);
        other.type_id = 4;
        store.insert(other.clone());

        // A type mismatch is a different entity: not deleted.
        let err = store
            .del_entity(&EntityKey {
                type_id: 4,
                db_id: 1001,
            })
            .expect_err("type-scoped delete misses");
        assert_eq!(err, DbError::NotFound);
        assert_eq!(
            store
                .get_entity(&EntityKey {
                    type_id: 3,
                    db_id: 1001,
                })
                .expect("read")
                .expect("row still there"),
            record(1001)
        );

        store
            .del_entity(&EntityKey {
                type_id: 4,
                db_id: 1002,
            })
            .expect("delete the type-4 row");
        assert_eq!(store.len(), 1);
        assert_eq!(
            store
                .get_entity(&EntityKey {
                    type_id: 4,
                    db_id: 1002
                })
                .expect("read"),
            None
        );
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

    #[test]
    fn a_checkout_grants_and_reads_back() {
        let mut store = MemoryStorage::new();
        store.insert(record(1001));
        let key = EntityKey {
            type_id: 3,
            db_id: 1001,
        };

        let outcome = store
            .check_out(&key, mailbox(0x0A00_0001, 42))
            .expect("checkout of a stored row");
        assert_eq!(outcome, CheckOut::Granted);
        assert_eq!(
            store.logon_of(&key).expect("read"),
            Some(mailbox(0x0A00_0001, 42))
        );
    }

    #[test]
    fn concurrent_logons_never_double_occupy() {
        let mut store = MemoryStorage::new();
        store.insert(record(1001));
        let mut first = store.connection();
        let mut second = store.connection();
        let key = EntityKey {
            type_id: 3,
            db_id: 1001,
        };

        // Two logons race for the same entity: exactly one wins.
        assert_eq!(
            first
                .check_out(&key, mailbox(0x0A00_0001, 42))
                .expect("first logon"),
            CheckOut::Granted
        );

        // The second is told where the entity lives — twice, for the
        // stragglers — and neither leaves a second record behind.
        assert_eq!(
            second
                .check_out(&key, mailbox(0x0A00_0002, 43))
                .expect("occupied checkout reports, not fails"),
            CheckOut::Elsewhere(mailbox(0x0A00_0001, 42))
        );
        assert_eq!(
            second
                .check_out(&key, mailbox(0x0A00_0003, 44))
                .expect("still occupied"),
            CheckOut::Elsewhere(mailbox(0x0A00_0001, 42))
        );
        // The holder is visible from every connection — including the
        // one that won.
        assert_eq!(
            first.logon_of(&key).expect("read"),
            Some(mailbox(0x0A00_0001, 42))
        );
        assert_eq!(
            store.logon_of(&key).expect("read"),
            Some(mailbox(0x0A00_0001, 42))
        );
    }

    #[test]
    fn checkout_of_a_missing_row_is_not_found() {
        let mut store = MemoryStorage::new();

        let err = store
            .check_out(
                &EntityKey {
                    type_id: 3,
                    db_id: 4242,
                },
                mailbox(1, 1),
            )
            .expect_err("no row to check out");
        assert_eq!(err, DbError::NotFound);
    }

    #[test]
    fn checkin_releases_and_a_later_checkout_grants_again() {
        let mut store = MemoryStorage::new();
        store.insert(record(1001));
        let mut holder = store.connection();
        let reader = store.connection();
        let key = EntityKey {
            type_id: 3,
            db_id: 1001,
        };
        holder.check_out(&key, mailbox(1, 42)).expect("checkout");

        holder.check_in(&key).expect("release");
        assert_eq!(reader.logon_of(&key).expect("read"), None);

        // A release with nothing held is fine — the reference DELETE is
        // unconditional.
        holder.check_in(&key).expect("release again");

        // And the freed entity checks out to a new holder.
        assert_eq!(
            store.check_out(&key, mailbox(2, 43)).expect("re-grant"),
            CheckOut::Granted
        );
    }

    #[test]
    fn deleting_an_entity_releases_its_checkout() {
        let mut store = MemoryStorage::new();
        store.insert(record(1001));
        let key = EntityKey {
            type_id: 3,
            db_id: 1001,
        };
        store.check_out(&key, mailbox(1, 42)).expect("checkout");
        // A check-in ahead of the delete goes through the store handle.
        store.check_in(&key).expect("release");
        assert_eq!(store.logon_of(&key).expect("read"), None);

        store.check_out(&key, mailbox(1, 43)).expect("re-checkout");
        store.del_entity(&key).expect("delete");
        assert_eq!(store.logon_of(&key).expect("read"), None);
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

/// A point-in-time image of a database's state, used to roll a transaction
/// back when there is no server to send `ROLLBACK` to. One image kind per
/// backend: the in-memory rows and checkouts, or the XML database
/// document.
#[derive(Clone, Debug)]
pub struct StorageImage(StorageImageInner);

#[derive(Clone, Debug)]
pub(crate) enum StorageImageInner {
    Memory(MemoryInner),
    Xml(crate::xml::XmlDatabase),
}

impl StorageImage {
    pub(crate) fn of_memory(inner: MemoryInner) -> Self {
        Self(StorageImageInner::Memory(inner))
    }

    pub(crate) fn of_xml(database: crate::xml::XmlDatabase) -> Self {
        Self(StorageImageInner::Xml(database))
    }

    /// The XML database the image carries.
    pub(crate) fn as_xml(&self) -> &crate::xml::XmlDatabase {
        match &self.0 {
            StorageImageInner::Xml(database) => database,
            StorageImageInner::Memory(_) => unreachable!("an xml backend restored a memory image"),
        }
    }
}

/// Storage that can be rolled back to an [`StorageImage`].
pub trait EntityStorageImage: EntityStorage {
    /// The database as it stands.
    fn image(&self) -> StorageImage;
    /// Put the database back the way it stood in `image`.
    fn restore_image(&mut self, image: &StorageImage);
}

impl EntityStorageImage for MemoryStorage {
    fn image(&self) -> StorageImage {
        StorageImage::of_memory(self.inner.borrow().clone())
    }

    fn restore_image(&mut self, image: &StorageImage) {
        let StorageImageInner::Memory(inner) = &image.0 else {
            unreachable!("a memory backend restored an xml image")
        };
        *self.inner.borrow_mut() = inner.clone();
    }
}

impl EntityStorageImage for MemoryConnection {
    fn image(&self) -> StorageImage {
        StorageImage::of_memory(self.inner.borrow().clone())
    }

    fn restore_image(&mut self, image: &StorageImage) {
        let StorageImageInner::Memory(inner) = &image.0 else {
            unreachable!("a memory backend restored an xml image")
        };
        *self.inner.borrow_mut() = inner.clone();
    }
}
