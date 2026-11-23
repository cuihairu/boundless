//! Transactions over the storage backend: commit, rollback, and the
//! transactional retry loop.
//!
//! The reference wraps every background task's statements in a
//! `MySqlTransaction` (`db_storage_mysql/transaction.cpp:15-71`): the
//! constructor runs `START TRANSACTION`, `commit()` runs `COMMIT` once
//! (`MF_ASSERT( !committed_ )` guards a second commit), and the destructor
//! runs `ROLLBACK` when `commit()` was never reached — "the transaction
//! will be rolled back if commit() has not been called when an object of
//! this type leaves scope". `shouldRetry()` is the deadlock pin:
//! `sql_.getLastErrorNum() == ER_LOCK_DEADLOCK` (`transaction.cpp:58-60`),
//! which here is [`DbError::Retry`] — the same transient stand-in the
//! retry loop already carries.
//!
//! `MySqlBackgroundTask::doBackgroundTask`
//! (`db_storage_mysql/tasks/background_task.cpp:34-99`) is the loop this
//! module's [`run_transactional`] mirrors: start a transaction, run the
//! task, commit on success; on an exception, a transient error re-runs the
//! whole task (after `onRetry()`), anything else is terminal
//! (`setFailure()` + `onException`). Every failed attempt leaves the
//! database exactly as it was — a multi-statement write such as
//! `resetGameServerState`'s DELETE/INSERT sequence
//! (`mysql_database.cpp:941-997`) either lands whole or not at all.
//!
//! The in-memory backend models InnoDB's undo log: the transaction's
//! writes apply as they are issued (a statement reads its own writes, the
//! way a session inside a transaction does), each success records the
//! inverse step, and a rollback replays the journal in reverse. Rolled
//! back creates do not return their ids to the allocator — an
//! `AUTO_INCREMENT` value a rollback burns stays burned.

use crate::connection::StorageConnection;
use crate::error::DbError;
use crate::log_on::LogOnRecord;
use crate::record::EntityRecord;
use crate::storage::{EntityKey, EntityStorage};
use foundation::ids::{DatabaseId, EntityTypeId, PENDING_DATABASE_ID};

/// `START TRANSACTION` (`transaction.cpp:19`).
pub const START_TRANSACTION_STMT: &str = "START TRANSACTION";
/// `COMMIT` (`transaction.cpp:69`).
pub const COMMIT_STMT: &str = "COMMIT";
/// `ROLLBACK` (`transaction.cpp:39`).
pub const ROLLBACK_STMT: &str = "ROLLBACK";

/// One inverse step of the undo journal.
enum Undo {
    /// The row was created: the rollback removes it again (its name index
    /// and online record go with it, [`MemoryStorage::remove_entity`]).
    RemoveEntity(EntityKey),
    /// The row was updated or deleted: the rollback restores the row as
    /// it was, name index included.
    RestoreEntity(EntityRecord),
    /// The online record was written or cleared: the rollback restores
    /// what was there before.
    RestoreLogOn(EntityKey, Option<LogOnRecord>),
}

/// One transaction over a [`StorageConnection`] — the reference's
/// `MySqlTransaction` (`transaction.cpp:14-31`).
///
/// The transaction implements [`EntityStorage`]: a task runs its writes
/// through it, each write lands in the store immediately and records its
/// undo step. [`commit`](StorageTransaction::commit) makes the writes
/// permanent; dropping the transaction without committing rolls every
/// recorded write back, the destructor's `ROLLBACK`
/// (`transaction.cpp:29-51`).
pub struct StorageTransaction<'a> {
    conn: &'a mut StorageConnection,
    undo: Vec<Undo>,
    committed: bool,
}

impl StorageConnection {
    /// `START TRANSACTION` (`transaction.cpp:19`): begin a transaction on
    /// this connection. Every write issued through the transaction is
    /// undone when it drops uncommitted.
    pub fn transaction(&mut self) -> StorageTransaction<'_> {
        StorageTransaction {
            conn: self,
            undo: Vec::new(),
            committed: false,
        }
    }
}

impl StorageTransaction<'_> {
    /// `COMMIT` (`transaction.cpp:66-71`): the writes become permanent.
    ///
    /// # Panics
    /// On a second commit — the reference's
    /// `MF_ASSERT( !committed_ )` (`transaction.cpp:68`).
    pub fn commit(&mut self) {
        assert!(
            !self.committed,
            "MySqlTransaction::commit: already committed"
        );
        self.committed = true;
    }

    /// `ROLLBACK` (`transaction.cpp:37-40`): replay the undo journal in
    /// reverse, leaving the store as it was before the transaction.
    fn rollback(&mut self) {
        while let Some(step) = self.undo.pop() {
            let store = self.conn.store();
            let mut store = store.borrow_mut();
            match step {
                Undo::RemoveEntity(key) => store.remove_entity(&key),
                Undo::RestoreEntity(record) => store.restore_entity(record),
                Undo::RestoreLogOn(key, record) => store.restore_log_on(&key, record),
            }
        }
    }
}

impl Drop for StorageTransaction<'_> {
    /// The destructor's rollback (`transaction.cpp:29-51`): an
    /// uncommitted transaction rolls back when it leaves scope, whatever
    /// the reason — an error return, a panic, a plain early exit.
    fn drop(&mut self) {
        if !self.committed {
            self.rollback();
        }
    }
}

impl EntityStorage for StorageTransaction<'_> {
    fn get_entity(&self, key: &EntityKey) -> Result<Option<EntityRecord>, DbError> {
        self.conn.get_entity(key)
    }

    fn put_entity(
        &mut self,
        record: &EntityRecord,
        put_explicit_id: bool,
    ) -> Result<DatabaseId, DbError> {
        let store = self.conn.store();
        let mut store = store.borrow_mut();
        // An update rewrites an existing row: capture it before the write
        // so the rollback can restore it. Nothing is journaled when the
        // write fails — a failed write leaves no partial state.
        let updating = !put_explicit_id && record.db_id != PENDING_DATABASE_ID;
        let old = if updating {
            let key = EntityKey {
                type_id: record.type_id,
                db_id: record.db_id,
            };
            Some(store.get_entity(&key)?.ok_or(DbError::NotFound)?)
        } else {
            None
        };
        let db_id = store.put_entity(record, put_explicit_id)?;
        match old {
            Some(old) => self.undo.push(Undo::RestoreEntity(old)),
            None => self.undo.push(Undo::RemoveEntity(EntityKey {
                type_id: record.type_id,
                db_id,
            })),
        }
        Ok(db_id)
    }

    fn del_entity(&mut self, key: &EntityKey) -> Result<(), DbError> {
        let store = self.conn.store();
        let mut store = store.borrow_mut();
        let Some(old) = store.get_entity(key)? else {
            return Err(DbError::NotFound);
        };
        let prior_log_on = store.log_on_record(key)?;
        store.del_entity(key)?;
        // Reverse order: the row is restored before its online record.
        self.undo.push(Undo::RestoreLogOn(*key, prior_log_on));
        self.undo.push(Undo::RestoreEntity(old));
        Ok(())
    }

    fn lookup_entity_by_name(
        &self,
        type_id: EntityTypeId,
        name: &str,
    ) -> Result<Option<EntityRecord>, DbError> {
        self.conn.lookup_entity_by_name(type_id, name)
    }

    fn lookup_entities(&self, type_id: EntityTypeId) -> Result<Vec<EntityRecord>, DbError> {
        self.conn.lookup_entities(type_id)
    }

    fn set_log_on_record(&mut self, key: &EntityKey, record: LogOnRecord) -> Result<(), DbError> {
        let store = self.conn.store();
        let mut store = store.borrow_mut();
        let prior = store.log_on_record(key)?;
        store.set_log_on_record(key, record)?;
        self.undo.push(Undo::RestoreLogOn(*key, prior));
        Ok(())
    }

    fn remove_log_on_record(&mut self, key: &EntityKey) -> Result<(), DbError> {
        let store = self.conn.store();
        let mut store = store.borrow_mut();
        let prior = store.log_on_record(key)?;
        store.remove_log_on_record(key)?;
        self.undo.push(Undo::RestoreLogOn(*key, prior));
        Ok(())
    }

    fn log_on_record(&self, key: &EntityKey) -> Result<Option<LogOnRecord>, DbError> {
        self.conn.log_on_record(key)
    }
}

/// The `MySqlBackgroundTask::doBackgroundTask` loop
/// (`background_task.cpp:34-99`): run `task` inside a transaction and
/// return `(attempts, outcome)`.
///
/// A task that returns `Ok` is committed and done. A task that returns
/// [`DbError::Retry`] — the `shouldRetry()` deadlock stand-in
/// (`transaction.cpp:58-60`) — has had its attempt rolled back and the
/// whole task re-runs, the retryable branch (`background_task.cpp:79-87`);
/// there is no bound on the retries, like the reference's loop. Any other
/// error is terminal: the attempt is rolled back and the error comes out
/// — `setFailure()` + `onException` (`background_task.cpp:88-95`). Either
/// way the caller observes one outcome, and a failed attempt leaves no
/// partial state behind.
pub fn run_transactional<T>(
    conn: &mut StorageConnection,
    mut task: impl FnMut(&mut StorageTransaction<'_>) -> Result<T, DbError>,
) -> (u32, Result<T, DbError>) {
    let mut attempts = 0;
    loop {
        attempts += 1;
        let mut transaction = conn.transaction();
        match task(&mut transaction) {
            Ok(value) => {
                transaction.commit();
                return (attempts, Ok(value));
            }
            // The rollback happened on the drop; the task runs again.
            Err(DbError::Retry) => {}
            Err(err) => return (attempts, Err(err)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    use crate::storage::{MemoryStorage, load_entity};
    use entitydef::value::PropertyValue;

    fn named(db_id: DatabaseId, name: &str) -> EntityRecord {
        EntityRecord {
            type_id: 1,
            db_id,
            name: Some(name.into()),
            values: vec![PropertyValue::String(name.into())],
        }
    }

    fn log_on(entity_id: i32) -> LogOnRecord {
        LogOnRecord {
            entity_id,
            ip: 0x0A00_0001,
            port: 31234,
            salt: 42,
        }
    }

    fn open() -> (Rc<RefCell<MemoryStorage>>, StorageConnection) {
        let store = Rc::new(RefCell::new(MemoryStorage::new()));
        let conn = StorageConnection::open(Rc::clone(&store));
        (store, conn)
    }

    /// The statements are the reference's literal strings
    /// (`transaction.cpp:19,39,69`).
    #[test]
    fn the_transaction_statements_match_the_reference() {
        assert_eq!(START_TRANSACTION_STMT, "START TRANSACTION");
        assert_eq!(COMMIT_STMT, "COMMIT");
        assert_eq!(ROLLBACK_STMT, "ROLLBACK");
    }

    /// The acceptance, 失败写入无部分状态: a task that writes a create, an
    /// update and an online record and then fails leaves the store exactly
    /// as it was — the rollback replays every inverse step, and an
    /// independent reader never sees the wreckage.
    #[test]
    fn a_failed_write_leaves_no_partial_state() {
        let (store, mut conn) = open();

        // The world before the transaction: two rows, one online, none
        // called "newcomer".
        conn.put_entity(&named(1001, "wenchuan"), true)
            .expect("seed the first row");
        conn.put_entity(&named(1002, "pinghu"), true)
            .expect("seed the second row");
        let wenchuan = EntityKey {
            type_id: 1,
            db_id: 1001,
        };
        conn.set_log_on_record(&wenchuan, log_on(101))
            .expect("seed the online row");

        // The doomed attempt: three writes, then the failure. The ghost
        // update names a db id that identifies nothing, the terminal
        // outcome the reference fails the task on.
        let err = {
            let mut transaction = conn.transaction();
            transaction
                .put_entity(&named(PENDING_DATABASE_ID, "newcomer"), false)
                .expect("create inside the transaction");
            let mut rewrite = named(1001, "rewritten");
            rewrite.values = vec![PropertyValue::Int(9)];
            transaction
                .put_entity(&rewrite, false)
                .expect("update inside the transaction");
            transaction
                .set_log_on_record(
                    &EntityKey {
                        type_id: 1,
                        db_id: 1002,
                    },
                    log_on(202),
                )
                .expect("online inside the transaction");
            transaction
                .put_entity(&named(9999, "ghost"), false)
                .expect_err("the ghost update is the failure")
        };
        assert_eq!(err, DbError::NotFound);

        // Every write of the attempt is gone: no row residue, and the
        // only allocator movement is the burned id — InnoDB does not
        // return a rolled-back create's id to the AUTO_INCREMENT either.
        assert_eq!(store.borrow().len(), 2, "no row residue");
        assert_eq!(store.borrow().next_id(), 1004, "one burned id");
        let reader = StorageConnection::open(Rc::clone(&store));
        assert_eq!(
            reader
                .lookup_entity_by_name(1, "newcomer")
                .expect("read over the reader"),
            None,
            "the rolled-back create resolves nothing"
        );
        assert_eq!(
            reader
                .get_entity(&wenchuan)
                .expect("read over the reader")
                .expect("the seeded row survives"),
            named(1001, "wenchuan"),
            "the rolled-back update is undone"
        );
        assert_eq!(
            reader
                .log_on_record(&EntityKey {
                    type_id: 1,
                    db_id: 1002
                })
                .expect("read over the reader"),
            None,
            "the rolled-back online record is undone"
        );
    }

    /// A committed transaction is permanent: every write of it is visible
    /// to a connection opened before it ran.
    #[test]
    fn a_committed_transaction_is_visible_to_an_independent_connection() {
        let (store, mut conn) = open();
        let reader = StorageConnection::open(Rc::clone(&store));

        let (_attempts, outcome) = run_transactional(&mut conn, |transaction| {
            let db_id = transaction.put_entity(&named(PENDING_DATABASE_ID, "wenchuan"), false)?;
            transaction.set_log_on_record(&EntityKey { type_id: 1, db_id }, log_on(101))?;
            Ok(db_id)
        });
        let db_id: DatabaseId = outcome.expect("the task commits");
        let key = EntityKey { type_id: 1, db_id };

        let found = reader
            .get_entity(&key)
            .expect("read over the reader")
            .expect("the committed create is there");
        assert_eq!(found.values, vec![PropertyValue::String("wenchuan".into())]);
        assert_eq!(
            reader.log_on_record(&key).expect("read over the reader"),
            Some(log_on(101)),
            "the committed online record is there"
        );
    }

    /// A statement reads its own writes: the transaction sees its earlier
    /// create, the way a session inside a transaction does.
    #[test]
    fn reads_inside_the_transaction_see_its_own_writes() {
        let (_store, mut conn) = open();

        let (_attempts, outcome) = run_transactional(&mut conn, |transaction| {
            let db_id = transaction
                .put_entity(&named(PENDING_DATABASE_ID, "wenchuan"), false)
                .expect("create");
            let found = transaction
                .get_entity(&EntityKey { type_id: 1, db_id })
                .expect("read inside the transaction")
                .expect("the transaction reads its own write");
            assert_eq!(found.name.as_deref(), Some("wenchuan"));
            // The lookups see it too.
            let by_name = transaction
                .lookup_entity_by_name(1, "wenchuan")
                .expect("read by name inside the transaction");
            assert_eq!(by_name.map(|row| row.db_id), Some(db_id));
            Ok(())
        });
        outcome.expect("committed");
    }

    /// The retryable branch (`background_task.cpp:79-87`): a
    /// [`DbError::Retry`] attempt is rolled back — the partial writes of
    /// the failed attempt leave nothing behind — and the task re-runs to
    /// its commit.
    #[test]
    fn a_transient_failure_rolls_back_and_re_runs_the_task() {
        let (_store, mut conn) = open();

        let mut flakiness = 1;
        let (attempts, outcome): (u32, Result<DatabaseId, DbError>) =
            run_transactional(&mut conn, |transaction| {
                transaction
                    .put_entity(&named(PENDING_DATABASE_ID, "wenchuan"), false)
                    .expect("create");
                // A partial write sits in the transaction when the
                // transient error hits.
                if flakiness > 0 {
                    flakiness -= 1;
                    return Err(DbError::Retry);
                }
                transaction.put_entity(&named(PENDING_DATABASE_ID, "pinghu"), false)
            });
        outcome.expect("the re-run commits");

        assert_eq!(attempts, 2, "the transient attempt re-ran");
        assert_eq!(conn.store().borrow().len(), 2, "only the committed rows");
        assert!(
            conn.lookup_entity_by_name(1, "wenchuan")
                .expect("read by name")
                .is_some()
        );
    }

    /// The terminal branch (`background_task.cpp:88-95`): a non-retryable
    /// error fails the task — one attempt, no re-run, nothing committed.
    #[test]
    fn a_terminal_error_fails_the_task_without_partial_state() {
        let (_store, mut conn) = open();
        conn.put_entity(&named(1001, "wenchuan"), true)
            .expect("seed a row");

        let (attempts, outcome): (u32, Result<DatabaseId, DbError>) =
            run_transactional(&mut conn, |transaction| {
                transaction
                    .put_entity(&named(PENDING_DATABASE_ID, "newcomer"), false)
                    .expect("create");
                // The terminal failure: an update of a missing id.
                transaction.put_entity(&named(9999, "ghost"), false)
            });

        assert_eq!(outcome, Err(DbError::NotFound));
        assert_eq!(attempts, 1, "a terminal error never re-runs");
        assert_eq!(
            conn.store().borrow().len(),
            1,
            "only the seeded row remains"
        );
        assert_eq!(
            conn.lookup_entity_by_name(1, "newcomer").expect("read"),
            None,
            "the doomed create is gone"
        );
    }

    /// The rollback replays every kind of inverse step in reverse: an
    /// update restores its old row, a delete brings its row and its
    /// online record back, a create and an explicit insert disappear, and
    /// a written online record returns to what was there before. A write
    /// refused inside the transaction journals nothing.
    #[test]
    fn rolling_back_replays_updates_deletes_and_online_state() {
        let (store, mut conn) = open();
        conn.put_entity(&named(1001, "wenchuan"), true)
            .expect("seed the updated row");
        let online_id = conn
            .put_entity(&named(1002, "pinghu"), true)
            .expect("seed the deleted row");
        let online_key = EntityKey {
            type_id: 1,
            db_id: online_id,
        };
        conn.set_log_on_record(&online_key, log_on(101))
            .expect("seed the online row");

        let mut transaction = conn.transaction();
        // An update whose rollback restores the old values and name.
        let mut rewrite = named(1001, "rewritten");
        rewrite.values = vec![PropertyValue::Int(9)];
        transaction.put_entity(&rewrite, false).expect("update");
        // An online record whose rollback leaves none behind.
        transaction
            .set_log_on_record(&wenchuan_key(), log_on(202))
            .expect("go online");
        // A delete whose rollback restores the row and its online record.
        transaction.del_entity(&online_key).expect("delete");
        // A create and an explicit insert whose rollbacks remove them.
        transaction
            .put_entity(&named(PENDING_DATABASE_ID, "newcomer"), false)
            .expect("create");
        transaction
            .put_entity(&named(5000, "restored"), true)
            .expect("explicit insert");
        // A logoff whose rollback returns to online.
        transaction
            .remove_log_on_record(&wenchuan_key())
            .expect("logoff");
        // Writes refused inside the transaction journal nothing.
        transaction
            .put_entity(&named(9999, "ghost"), false)
            .expect_err("the update of a missing id is the failure");
        transaction
            .del_entity(&EntityKey {
                type_id: 1,
                db_id: 1,
            })
            .expect_err("the delete of a missing id is the failure");
        transaction
            .set_log_on_record(
                &EntityKey {
                    type_id: 1,
                    db_id: 1,
                },
                log_on(1),
            )
            .expect_err("the online record of a missing row is the failure");
        drop(transaction);

        // The store is as it was: the two seeded rows, no residue — the
        // create and the explicit insert burned their ids, nothing else
        // moved.
        assert_eq!(store.borrow().len(), 2, "no row residue");
        assert_eq!(
            store.borrow().next_id(),
            5001,
            "the burned ids are not reused"
        );
        assert!(
            load_entity(&*store.borrow(), &online_key).is_ok(),
            "the deleted row is back"
        );
        let found = load_entity(
            &*store.borrow(),
            &EntityKey {
                type_id: 1,
                db_id: 1001,
            },
        )
        .expect("the updated row is back to its old self");
        assert_eq!(found.values, vec![PropertyValue::String("wenchuan".into())]);
        assert_eq!(
            store.borrow().log_on_record(&online_key).expect("read"),
            Some(log_on(101)),
            "the seeded online record is back"
        );
    }

    /// The key of the first seeded row.
    fn wenchuan_key() -> EntityKey {
        EntityKey {
            type_id: 1,
            db_id: 1001,
        }
    }

    /// The destructor's contract (`transaction.cpp:29-51`): a transaction
    /// dropped without commit — no error, no panic, just an early exit —
    /// rolls back all the same.
    #[test]
    fn a_transaction_dropped_without_commit_rolls_back() {
        let (store, mut conn) = open();

        {
            let mut transaction = conn.transaction();
            transaction
                .put_entity(&named(PENDING_DATABASE_ID, "wenchuan"), false)
                .expect("create");
            // No commit: the scope ends here.
        }

        assert_eq!(
            conn.lookup_entity_by_name(1, "wenchuan").expect("read"),
            None,
            "the uncommitted create is gone"
        );
        assert!(store.borrow().is_empty());
    }

    /// `MF_ASSERT( !committed_ )` (`transaction.cpp:68`): a transaction
    /// commits once.
    #[test]
    #[should_panic(expected = "already committed")]
    fn a_double_commit_is_a_breach() {
        let (_store, mut conn) = open();
        let mut transaction = conn.transaction();
        transaction.commit();
        transaction.commit();
    }

    /// A rolled-back create does not return its id: an `AUTO_INCREMENT`
    /// value a rollback burns stays burned, so the next create never hands
    /// out the same id.
    #[test]
    fn rolled_back_ids_are_not_reused() {
        let (_store, mut conn) = open();

        let burned = {
            let mut transaction = conn.transaction();
            transaction
                .put_entity(&named(PENDING_DATABASE_ID, "doomed"), false)
                .expect("create")
            // Dropped uncommitted: the id burns.
        };
        let (_attempts, outcome) = run_transactional(&mut conn, |transaction| {
            transaction.put_entity(&named(PENDING_DATABASE_ID, "wenchuan"), false)
        });
        let committed: DatabaseId = outcome.expect("committed");
        assert!(committed > burned, "the burned id is never reused");
    }
}
