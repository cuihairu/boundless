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
    pub fn insert(&mut self, record: EntityRecord) {
        let key = EntityKey {
            type_id: record.type_id,
            db_id: record.db_id,
        };
        self.inner.borrow_mut().entities.insert(key, record);
    }

    /// Number of stored entities.
    pub fn len(&self) -> usize {
        self.inner.borrow().entities.len()
    }

    /// Whether the store is empty.
    pub fn is_empty(&self) -> bool {
        self.inner.borrow().entities.is_empty()
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
