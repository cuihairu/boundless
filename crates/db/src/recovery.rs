//! Lock-conflict retries and connection recovery.
//!
//! Three reference loops govern a failing statement:
//!
//! * `MySql::realQuery` (`wrapper.cpp:311-345`): a query that dies because
//!   the connection dropped (the `CR_SERVER_*` family) is re-run once on the
//!   re-established connection — but never from inside a transaction, where
//!   the loss is terminal (`MySqlTransaction`'s destructor skips the
//!   `ROLLBACK` because the server aborts the work itself).
//! * `MySqlDatabase::clearSecondaryDBs` (`mysql_database.cpp:838-860`): a
//!   plain statement retries without bound while
//!   `DatabaseException::shouldRetry` holds — a lock deadlock or a lock wait
//!   timeout (`database_exception.cpp:33-37`), nothing else.
//! * `MySqlDatabase::resetGameServerState` (`mysql_database.cpp:960-1006`):
//!   a transactional body runs at most `NUM_ATTEMPTS` times. Each failed
//!   attempt rolls its transaction back, so a retried attempt starts from a
//!   consistent state, and any failure but a lock conflict stops the loop.

use crate::error::DbError;
use crate::retry::failure_from_db_error;
use crate::storage::EntityStorageImage;
use crate::transaction::EntityTransaction;

/// `NUM_ATTEMPTS` in `MySqlDatabase::resetGameServerState`.
pub const TRANSACTION_ATTEMPTS: usize = 3;

/// `MySql::realQuery`'s reconnect leg: run `op`; when it fails with a lost
/// connection and `reconnect` re-establishes the backend, run the very same
/// statement once more. A failure that is not a lost connection is returned
/// untouched and never reconnects.
///
/// The `inTransaction_` guard is the caller's: inside a transaction the
/// reference never reconnects, because the aborted work cannot be re-sent on
/// the new connection.
pub fn query_with_reconnect<T>(
    mut op: impl FnMut() -> Result<T, DbError>,
    mut reconnect: impl FnMut() -> bool,
) -> Result<T, DbError> {
    match op() {
        Err(DbError::LostConnection) if reconnect() => op(),
        outcome => outcome,
    }
}

/// `MySqlDatabase::clearSecondaryDBs`'s loop: run `op` again for as long as
/// the failure is retryable — in the reference, `shouldRetry` (a lock
/// conflict). A fatal failure surfaces instead of being swallowed the way
/// the reference swallows it into a stale result count.
pub fn retry_query<T>(mut op: impl FnMut() -> Result<T, DbError>) -> Result<T, DbError> {
    loop {
        match op() {
            Ok(value) => return Ok(value),
            Err(error) => {
                if !failure_from_db_error(&error, &error.to_string()).should_retry() {
                    return Err(error);
                }
            }
        }
    }
}

/// `MySqlDatabase::resetGameServerState`'s loop: run the transactional
/// `body` at most [`TRANSACTION_ATTEMPTS`] times. The body stages its writes
/// on the transaction and returns its result to commit with; a commit
/// failure restores the storage image, so the next attempt starts exactly
/// where the last one began — the state-consistency half of the acceptance.
///
/// Only a lock conflict retries (`DatabaseException::shouldRetry`): a lost
/// connection or a fatal failure stops the loop and surfaces. When every
/// attempt conflicts, the last conflict is returned.
pub fn retry_transaction<T>(
    storage: &mut dyn EntityStorageImage,
    mut body: impl FnMut(&mut EntityTransaction<'_>) -> Result<T, DbError>,
) -> Result<T, DbError> {
    let mut last = None;
    for _ in 0..TRANSACTION_ATTEMPTS {
        let mut transaction = EntityTransaction::new(&mut *storage);
        let outcome = body(&mut transaction).and_then(|value| transaction.commit().map(|_| value));
        match outcome {
            Ok(value) => return Ok(value),
            Err(error) => {
                if !failure_from_db_error(&error, &error.to_string()).should_retry() {
                    return Err(error);
                }
                last = Some(error);
            }
        }
    }
    Err(last.expect("the attempts bound is non-zero"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;

    use entitydef::value::PropertyValue;
    use foundation::ids::{DatabaseId, PENDING_DATABASE_ID};
    use mercury::endpoint::Address;

    use crate::error::DbError;
    use crate::record::EntityRecord;
    use crate::storage::{
        BaseMailbox, CheckOut, EntityKey, EntityStorage, MemoryStorage, StorageImage,
    };

    fn key(type_id: u16, db_id: DatabaseId) -> EntityKey {
        EntityKey { type_id, db_id }
    }

    fn values(marker: &str) -> Vec<PropertyValue> {
        vec![PropertyValue::String(marker.into())]
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

    fn seeded_store() -> MemoryStorage {
        let mut store = MemoryStorage::new();
        store.insert(EntityRecord {
            type_id: 3,
            db_id: 1,
            values: vec![PropertyValue::String("seed".into())],
        });
        store
    }

    /// A scripted statement: it fails with each scripted error, then
    /// succeeds.
    fn scripted(
        script: Rc<RefCell<Vec<DbError>>>,
        runs: Rc<Cell<usize>>,
    ) -> impl FnMut() -> Result<(), DbError> {
        move || {
            runs.set(runs.get() + 1);
            match script.borrow_mut().pop() {
                Some(error) => Err(error),
                None => Ok(()),
            }
        }
    }

    /// A reconnect that counts itself and reports `succeeds`.
    fn counting_reconnect(reconnects: Rc<Cell<usize>>, succeeds: bool) -> impl FnMut() -> bool {
        move || {
            reconnects.set(reconnects.get() + 1);
            succeeds
        }
    }

    /// A connection that fails its next puts with scripted errors, the way a
    /// busy row (`ER_LOCK_*`) or a dropped connection (`CR_SERVER_*`) does.
    struct FlakyStorage {
        inner: MemoryStorage,
        script: Rc<RefCell<Vec<DbError>>>,
    }

    impl FlakyStorage {
        fn wrap(inner: MemoryStorage, script: Vec<DbError>) -> Self {
            Self {
                inner,
                script: Rc::new(RefCell::new(script)),
            }
        }
    }

    impl EntityStorage for FlakyStorage {
        fn get_entity(&self, key: &EntityKey) -> Result<Option<EntityRecord>, DbError> {
            self.inner.get_entity(key)
        }

        fn put_entity(
            &mut self,
            key: &EntityKey,
            values: &[PropertyValue],
            put_explicit_id: bool,
        ) -> Result<DatabaseId, DbError> {
            if let Some(error) = self.script.borrow_mut().pop() {
                return Err(error);
            }
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

        fn all_entities(&self) -> Result<Vec<EntityRecord>, DbError> {
            self.inner.all_entities()
        }
    }

    impl EntityStorageImage for FlakyStorage {
        fn image(&self) -> StorageImage {
            self.inner.image()
        }

        fn restore_image(&mut self, image: &StorageImage) {
            self.inner.restore_image(image);
        }
    }

    #[test]
    fn a_dropped_connection_is_reconnected_and_the_query_rerun_once() {
        let script = Rc::new(RefCell::new(vec![DbError::LostConnection]));
        let runs = Rc::new(Cell::new(0));
        let reconnects = Rc::new(Cell::new(0));

        let script_op = script.clone();
        let runs_op = runs.clone();
        let reconnect_op = reconnects.clone();
        let result = query_with_reconnect(
            move || {
                runs_op.set(runs_op.get() + 1);
                match script_op.borrow_mut().pop() {
                    Some(error) => Err(error),
                    None => Ok(()),
                }
            },
            move || {
                reconnect_op.set(reconnect_op.get() + 1);
                true
            },
        );

        assert_eq!(result, Ok(()));
        assert_eq!(runs.get(), 2, "the very same statement ran twice");
        assert_eq!(reconnects.get(), 1);
    }

    #[test]
    fn a_failed_reconnect_leaves_the_query_failed() {
        let script = Rc::new(RefCell::new(vec![DbError::LostConnection]));
        let runs = Rc::new(Cell::new(0));
        let reconnects = Rc::new(Cell::new(0));

        let result = query_with_reconnect(
            scripted(script, runs.clone()),
            counting_reconnect(reconnects.clone(), false),
        );

        assert_eq!(result, Err(DbError::LostConnection));
        assert_eq!(runs.get(), 1);
        assert_eq!(reconnects.get(), 1);
    }

    #[test]
    fn a_non_connection_failure_never_reconnects() {
        let script = Rc::new(RefCell::new(vec![DbError::Retry]));
        let runs = Rc::new(Cell::new(0));
        let reconnects = Rc::new(Cell::new(0));

        let result = query_with_reconnect(
            scripted(script, runs.clone()),
            counting_reconnect(reconnects.clone(), true),
        );

        assert_eq!(result, Err(DbError::Retry));
        assert_eq!(runs.get(), 1);
        assert_eq!(reconnects.get(), 0, "a busy row is not a lost connection");
    }

    #[test]
    fn a_busy_statement_retries_until_it_succeeds() {
        let script = Rc::new(RefCell::new(vec![DbError::Retry, DbError::Retry]));
        let runs = Rc::new(Cell::new(0));

        let result = retry_query(scripted(script, runs.clone()));

        assert_eq!(result, Ok(()));
        assert_eq!(runs.get(), 3);
    }

    #[test]
    fn a_fatal_statement_error_stops_the_busy_loop() {
        let script = Rc::new(RefCell::new(vec![DbError::NotFound]));
        let runs = Rc::new(Cell::new(0));

        let result = retry_query(scripted(script, runs.clone()));

        assert_eq!(result, Err(DbError::NotFound));
        assert_eq!(runs.get(), 1);
    }

    #[test]
    fn a_lock_conflict_is_retried_from_a_consistent_state() {
        let mut flaky = FlakyStorage::wrap(seeded_store(), vec![DbError::Retry]);
        let attempts = Rc::new(Cell::new(0));

        let attempts_body = attempts.clone();
        let result = retry_transaction(&mut flaky, move |transaction| {
            attempts_body.set(attempts_body.get() + 1);
            // Both writes stage; the commit applies them as one.
            transaction
                .put(key(3, PENDING_DATABASE_ID), values("new"), false)
                .put(key(3, 1), values("update"), false);
            Ok(())
        });

        assert_eq!(result, Ok(()));
        assert_eq!(attempts.get(), 2, "the conflict rolled attempt one back");

        // The retried attempt started exactly where the first began: one
        // seed row and nothing else from the failed attempt.
        let rows = flaky.all_entities().expect("the store lists");
        assert_eq!(rows.len(), 2);
        assert_eq!(
            flaky.get_entity(&key(3, 1)).expect("the store reads"),
            Some(EntityRecord {
                type_id: 3,
                db_id: 1,
                values: values("update"),
            })
        );
        assert_eq!(
            flaky.get_entity(&key(3, 2)).expect("the store reads"),
            Some(EntityRecord {
                type_id: 3,
                db_id: 2,
                values: values("new"),
            })
        );
    }

    #[test]
    fn a_dropped_connection_inside_a_transaction_is_terminal_but_consistent() {
        let mut flaky = FlakyStorage::wrap(seeded_store(), vec![DbError::LostConnection]);
        let attempts = Rc::new(Cell::new(0));

        let attempts_body = attempts.clone();
        let result = retry_transaction(&mut flaky, move |transaction| {
            attempts_body.set(attempts_body.get() + 1);
            transaction
                .put(key(3, PENDING_DATABASE_ID), values("new"), false)
                .put(key(3, 1), values("update"), false);
            Ok(())
        });

        // shouldRetry is a lock-conflict predicate: a lost connection stops
        // the loop after the one attempt, with nothing of it applied.
        assert_eq!(result, Err(DbError::LostConnection));
        assert_eq!(attempts.get(), 1);
        assert_eq!(
            flaky.get_entity(&key(3, 1)).expect("the store reads"),
            Some(EntityRecord {
                type_id: 3,
                db_id: 1,
                values: values("seed"),
            })
        );
        assert_eq!(flaky.all_entities().expect("the store lists").len(), 1);
    }

    #[test]
    fn a_fatal_body_failure_stops_the_loop_at_once() {
        let mut flaky = FlakyStorage::wrap(seeded_store(), vec![]);
        let attempts = Rc::new(Cell::new(0));

        let attempts_body = attempts.clone();
        let result: Result<(), DbError> = retry_transaction(&mut flaky, move |transaction| {
            attempts_body.set(attempts_body.get() + 1);
            transaction.put(key(3, PENDING_DATABASE_ID), values("new"), false);
            Err(DbError::NotFound)
        });

        assert_eq!(result, Err(DbError::NotFound));
        assert_eq!(attempts.get(), 1, "a fatal failure is not a lock conflict");
        assert_eq!(flaky.all_entities().expect("the store lists").len(), 1);
    }

    #[test]
    fn the_attempts_bound_stops_a_permanently_busy_lock() {
        // Enough scripted conflicts for every attempt to fail at its first
        // write.
        let mut flaky = FlakyStorage::wrap(seeded_store(), vec![DbError::Retry; 10]);
        let attempts = Rc::new(Cell::new(0));

        let attempts_body = attempts.clone();
        let result = retry_transaction(&mut flaky, move |transaction| {
            attempts_body.set(attempts_body.get() + 1);
            transaction
                .put(key(3, PENDING_DATABASE_ID), values("new"), false)
                .put(key(3, 1), values("update"), false);
            Ok(())
        });

        assert_eq!(result, Err(DbError::Retry));
        assert_eq!(
            attempts.get(),
            TRANSACTION_ATTEMPTS,
            "the loop stops at the reference's bound"
        );
        assert_eq!(flaky.all_entities().expect("the store lists").len(), 1);
    }

    #[test]
    fn the_flaky_double_fails_only_its_scripted_puts() {
        // Everything but the scripted puts behaves like the plain memory
        // backend, so the recovery loops above exercise real storage.
        let mut flaky = FlakyStorage::wrap(seeded_store(), vec![DbError::Retry]);

        assert_eq!(
            flaky.get_entity(&key(3, 1)).expect("the double reads"),
            Some(EntityRecord {
                type_id: 3,
                db_id: 1,
                values: values("seed"),
            })
        );
        assert_eq!(flaky.all_entities().expect("the double lists").len(), 1);

        assert_eq!(
            flaky.check_out(&key(3, 1), mailbox(1, 42)),
            Ok(CheckOut::Granted)
        );
        assert!(
            flaky
                .logon_of(&key(3, 1))
                .expect("the double reads")
                .is_some()
        );
        assert_eq!(flaky.check_in(&key(3, 1)), Ok(()));

        assert_eq!(
            flaky.put_entity(&key(3, 1), &values("x"), false),
            Err(DbError::Retry),
            "the scripted put still fails"
        );
        assert_eq!(flaky.del_entity(&key(3, 1)), Ok(()));
    }

    #[test]
    fn a_clean_transaction_commits_on_the_first_attempt() {
        let mut flaky = FlakyStorage::wrap(seeded_store(), vec![]);
        let attempts = Rc::new(Cell::new(0));

        let attempts_body = attempts.clone();
        let result = retry_transaction(&mut flaky, move |transaction| {
            attempts_body.set(attempts_body.get() + 1);
            transaction.put(key(3, PENDING_DATABASE_ID), values("new"), false);
            Ok(())
        });

        assert_eq!(result, Ok(()));
        assert_eq!(attempts.get(), 1);
        assert_eq!(flaky.all_entities().expect("the store lists").len(), 2);
    }
}
