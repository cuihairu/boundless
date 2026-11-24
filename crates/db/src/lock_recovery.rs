//! Lock conflicts and connection recovery
//! (`db_storage_mysql/{named_lock,database_exception,locked_connection,transaction,wrapper}`).
//!
//! The reference splits failures into two families that never overlap
//! (`database_exception.cpp:35-52`): a *lock conflict* — a deadlock, or a
//! lock wait that outlived its timeout — is transient, the whole
//! operation runs again; a *lost connection* — the server died or the
//! socket went away — is recovered by reconnecting. Which family an error
//! number belongs to decides what happens next.
//!
//! On top of that, whole-process work takes a named lock
//! (`named_lock.cpp`) so that only one process per database runs it, and
//! the connection holds on through lost servers
//! (`locked_connection.cpp`, `wrapper.cpp:306-332`).

/// The MySQL error numbers the reference decides on.
///
/// `ER_LOCK_*` from `mysqld_error.h` (`ER_LOCK_WAIT_TIMEOUT` 1205,
/// `ER_LOCK_DEADLOCK` 1213) and the client-side `CR_*` numbers from
/// `errmsg.h` (`CR_CONNECTION_ERROR` 2002, `CR_CONN_HOST_ERROR` 2003,
/// `CR_SERVER_GONE_ERROR` 2006, `CR_SERVER_LOST` 2013) — the `CR_*` range
/// begins at `CR_ERROR_FIRST` 2000.
pub mod codes {
    /// `ER_LOCK_WAIT_TIMEOUT` (`mysqld_error.h:238`).
    pub const ER_LOCK_WAIT_TIMEOUT: i32 = 1205;
    /// `ER_LOCK_DEADLOCK` (`mysqld_error.h:246`).
    pub const ER_LOCK_DEADLOCK: i32 = 1213;
    /// `CR_CONNECTION_ERROR` (`errmsg.h:61`).
    pub const CR_CONNECTION_ERROR: i32 = 2002;
    /// `CR_CONN_HOST_ERROR` (`errmsg.h:62`).
    pub const CR_CONN_HOST_ERROR: i32 = 2003;
    /// `CR_SERVER_GONE_ERROR` (`errmsg.h:65`).
    pub const CR_SERVER_GONE_ERROR: i32 = 2006;
    /// `CR_SERVER_LOST` (`errmsg.h:72`).
    pub const CR_SERVER_LOST: i32 = 2013;
}

/// `DatabaseException::shouldRetry`
/// (`database_exception.cpp:35-39`): a deadlock or a lock wait that
/// timed out — both let the operation run again.
pub fn should_retry(error_number: i32) -> bool {
    error_number == codes::ER_LOCK_DEADLOCK || error_number == codes::ER_LOCK_WAIT_TIMEOUT
}

/// `DatabaseException::isLostConnection`
/// (`database_exception.cpp:46-52`): the connection to the server has
/// been lost.
pub fn is_lost_connection(error_number: i32) -> bool {
    matches!(
        error_number,
        codes::CR_SERVER_GONE_ERROR
            | codes::CR_SERVER_LOST
            | codes::CR_CONNECTION_ERROR
            | codes::CR_CONN_HOST_ERROR
    )
}

/// The reconnect rule of `MySql::realQuery`
/// (`wrapper.cpp:306-332`): a lost server earns a reconnect-and-retry,
/// but only when the query is *outside* a transaction — inside one the
/// transaction is already compromised, so the error propagates and the
/// whole operation is re-run instead.
pub fn should_reconnect_and_retry(in_transaction: bool, error_number: i32) -> bool {
    !in_transaction
        && matches!(
            error_number,
            codes::CR_SERVER_LOST | codes::CR_SERVER_GONE_ERROR
        )
}

/// `MySqlTransaction::shouldRetry` (`transaction.cpp:57-60`): a dead
/// transaction is worth re-running only for a deadlock — a lock wait
/// timeout inside a transaction is not retried there.
pub fn transaction_should_retry(last_error_number: i32) -> bool {
    last_error_number == codes::ER_LOCK_DEADLOCK
}

/// `ConnectionInfo::generateLockName` (`connection_info.hpp:34-40`): one
/// name for every BigWorld process to lock the database with, so only
/// one connection can hold it at a time.
pub fn generate_lock_name(database: &str) -> String {
    format!("BigWorld {database}")
}

/// The query seam behind [`NamedLock`] — the reference's `MySql`
/// (`named_lock.cpp:30-75`) and its `hasLostConnection` flag
/// (`wrapper.hpp:89-90`). A `false` from either lock call is the
/// reference's swallowed exception: a lock request must never blow up the
/// caller (`named_lock.cpp:36-51`, `:68-74`).
pub trait LockClient {
    /// `obtainNamedLock` (`named_lock.cpp:30-55`): `SELECT GET_LOCK( ?, 0 )` —
    /// non-blocking, `true` when the lock was taken, `false` when it is
    /// already held by another connection.
    fn obtain_named_lock(&mut self, lock_name: &str) -> bool;
    /// `releaseNamedLock` (`named_lock.cpp:61-75`):
    /// `SELECT RELEASE_LOCK( ? )`.
    fn release_named_lock(&mut self, lock_name: &str) -> bool;
    /// `MySql::hasLostConnection` (`wrapper.hpp:89`): the connection
    /// dropped on its own, so the lock went with it.
    fn has_lost_connection(&self) -> bool;
}

/// `NamedLock` (`named_lock.hpp`, `named_lock.cpp`): one held database
/// lock. Taking it binds the client that owns it; releasing asks only
/// that client, and only while it still lives.
pub struct NamedLock<C: LockClient> {
    client: Option<C>,
    lock_name: String,
}

impl<C: LockClient> NamedLock<C> {
    /// The constructor (`named_lock.cpp:86-90`).
    pub fn new(database: &str) -> Self {
        Self {
            client: None,
            lock_name: generate_lock_name(database),
        }
    }

    /// `lock` (`named_lock.cpp:107-116`): acquire the named lock. `false`
    /// when it is already held here or is held by someone else.
    pub fn lock(&mut self, mut client: C) -> bool {
        if !self.is_locked() && client.obtain_named_lock(&self.lock_name) {
            self.client = Some(client);
            true
        } else {
            false
        }
    }

    /// `unlock` (`named_lock.cpp:124-141`): release the lock the bound
    /// client holds. A client that lost its connection cannot be asked —
    /// the server already let the lock go — and is simply forgotten,
    /// which still counts as released.
    pub fn unlock(&mut self) -> bool {
        let Some(mut client) = self.client.take() else {
            return false;
        };
        if !client.has_lost_connection() {
            if !client.release_named_lock(&self.lock_name) {
                eprintln!(
                    "WARNING: NamedLock::unlock: Was unable to successfully unlock. Forgetting connection."
                );
            }
        }
        true
    }

    /// `isLocked` (`named_lock.hpp:25-27`).
    pub fn is_locked(&self) -> bool {
        self.client.is_some()
    }

    /// The lock name.
    pub fn lock_name(&self) -> &str {
        &self.lock_name
    }
}

impl<C: LockClient> Drop for NamedLock<C> {
    fn drop(&mut self) {
        // `~NamedLock` (`named_lock.cpp:96-99`).
        self.unlock();
    }
}

/// `MySqlLockedConnection` (`locked_connection.cpp`): a connection bound
/// to the lock name every process for its database takes
/// (`locked_connection.hpp:15-31`, `connection_info.hpp:34-40`).
pub struct LockedConnection<T: LockClient> {
    client: Option<T>,
    lock_name: String,
}

impl<T: LockClient> LockedConnection<T> {
    /// `MySqlLockedConnection( const DBConfig::ConnectionInfo & )`
    /// (`locked_connection.cpp:15-21`). Nothing is connected yet — the
    /// connection joins with the lock attempt.
    pub fn new(database: &str) -> Self {
        Self {
            client: None,
            lock_name: generate_lock_name(database),
        }
    }

    /// `connectAndLockWithRetry` (`locked_connection.cpp:49-68`) over
    /// the reference's `connectAndLock`/`connect`
    /// (`locked_connection.cpp:89-133`): connect and lock, retrying once a
    /// second until it succeeds or the retry budget is spent. `settle` is
    /// the reference's `sleep( 1 )`.
    ///
    /// The reference refuses a second connect — "A connection has already
    /// been established" (`locked_connection.cpp:91-96`) — so this does
    /// too.
    pub fn connect_and_lock_with_retry(
        &mut self,
        mut client: T,
        num_retries: u8,
        mut settle: impl FnMut(u8, u8),
    ) -> bool {
        if self.client.is_some() {
            eprintln!(
                "ERROR: MySqlLockedConnection::connect: A connection has already been established."
            );
            return false;
        }

        let mut retry: u8 = 0;

        loop {
            if client.obtain_named_lock(&self.lock_name) {
                self.client = Some(client);
                return true;
            }

            if retry >= num_retries {
                // The lock was never obtained: the reference disconnects
                // instead of holding a connection without it
                // (`locked_connection.cpp:110-113`).
                return false;
            }

            retry += 1;
            settle(retry, num_retries);
            eprintln!(
                "MySqlLockedConnection::connectAndLockWithRetry: Retry {retry}/{num_retries}"
            );
        }
    }

    /// The connection, while one is established
    /// (`locked_connection.cpp:89-133`).
    pub fn client_mut(&mut self) -> Option<&mut T> {
        self.client.as_mut()
    }

    /// `dbLock_.isLocked()` / the bound connection
    /// (`locked_connection.cpp:27-39`).
    pub fn is_locked(&self) -> bool {
        self.client.is_some()
    }
}

impl<T: LockClient> Drop for LockedConnection<T> {
    fn drop(&mut self) {
        // `~MySqlLockedConnection` (`locked_connection.cpp:27-39`): unlock,
        // then give the connection back up.
        let Some(mut client) = self.client.take() else {
            // Never connected: nothing to unlock or close.
            return;
        };
        if !client.has_lost_connection() {
            client.release_named_lock(&self.lock_name);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::DbError;
    use crate::retry::{Completion, run_with_retry};
    use std::cell::RefCell;
    use std::collections::HashSet;
    use std::rc::Rc;

    /// Only the two lock errors are retryable
    /// (`database_exception.cpp:35-39`).
    #[test]
    fn only_lock_errors_are_retryable() {
        for error_number in [
            codes::ER_LOCK_WAIT_TIMEOUT,
            codes::ER_LOCK_DEADLOCK,
            codes::CR_CONNECTION_ERROR,
            codes::CR_CONN_HOST_ERROR,
            codes::CR_SERVER_GONE_ERROR,
            codes::CR_SERVER_LOST,
        ] {
            assert_eq!(
                should_retry(error_number),
                matches!(
                    error_number,
                    codes::ER_LOCK_WAIT_TIMEOUT | codes::ER_LOCK_DEADLOCK
                ),
                "{error_number}"
            );
        }
    }

    /// Only the four connection-loss errors are lost connections
    /// (`database_exception.cpp:46-52`).
    #[test]
    fn only_connection_errors_are_lost() {
        for error_number in [
            codes::ER_LOCK_WAIT_TIMEOUT,
            codes::ER_LOCK_DEADLOCK,
            codes::CR_CONNECTION_ERROR,
            codes::CR_CONN_HOST_ERROR,
            codes::CR_SERVER_GONE_ERROR,
            codes::CR_SERVER_LOST,
        ] {
            assert_eq!(
                is_lost_connection(error_number),
                !matches!(
                    error_number,
                    codes::ER_LOCK_WAIT_TIMEOUT | codes::ER_LOCK_DEADLOCK
                ),
                "{error_number}"
            );
        }
    }

    /// The two families never overlap — that disjointness is what makes
    /// the recovery decision well defined.
    #[test]
    fn the_retryable_and_lost_families_are_disjoint() {
        for error_number in [
            codes::ER_LOCK_WAIT_TIMEOUT,
            codes::ER_LOCK_DEADLOCK,
            codes::CR_CONNECTION_ERROR,
            codes::CR_CONN_HOST_ERROR,
            codes::CR_SERVER_GONE_ERROR,
            codes::CR_SERVER_LOST,
        ] {
            assert_ne!(
                should_retry(error_number),
                is_lost_connection(error_number),
                "{error_number}"
            );
        }
    }

    /// The reconnect rule: outside a transaction only, and only for the
    /// two "the server is gone" codes (`wrapper.cpp:306-332`).
    #[test]
    fn the_reconnect_rule_needs_no_transaction_and_a_gone_server() {
        assert!(should_reconnect_and_retry(false, codes::CR_SERVER_LOST));
        assert!(should_reconnect_and_retry(
            false,
            codes::CR_SERVER_GONE_ERROR
        ));
        assert!(!should_reconnect_and_retry(true, codes::CR_SERVER_LOST));
        assert!(!should_reconnect_and_retry(
            true,
            codes::CR_SERVER_GONE_ERROR
        ));
        assert!(!should_reconnect_and_retry(
            false,
            codes::CR_CONNECTION_ERROR
        ));
        assert!(!should_reconnect_and_retry(false, codes::ER_LOCK_DEADLOCK));
    }

    /// Only a deadlock re-runs a dead transaction
    /// (`transaction.cpp:57-60`).
    #[test]
    fn only_a_deadlock_reruns_the_dead_transaction() {
        assert!(transaction_should_retry(codes::ER_LOCK_DEADLOCK));
        assert!(!transaction_should_retry(codes::ER_LOCK_WAIT_TIMEOUT));
        assert!(!transaction_should_retry(codes::CR_SERVER_LOST));
    }

    /// The lock name is `BigWorld ` plus the database
    /// (`connection_info.hpp:34-40`).
    #[test]
    fn the_lock_name_covers_every_process_for_one_database() {
        assert_eq!(generate_lock_name("bigworld"), "BigWorld bigworld");
        assert_ne!(generate_lock_name("bigworld"), generate_lock_name("other"));
    }

    /// Acceptance: a lock conflict is resolved by whoever releases first —
    /// the contender gets `false` while the name is held, and takes it
    /// the moment the holder gives it up (`named_lock.cpp:30-116`).
    #[test]
    fn a_named_lock_is_held_by_exactly_one_client() {
        let shared = Rc::new(RefCell::new(Shared::default()));

        let mut lock = NamedLock::<TestClient>::new("bigworld");
        let holder = client_of(&shared, false);
        assert!(!lock.is_locked());
        assert!(lock.lock(holder), "the first client takes it");
        assert!(lock.is_locked());

        let mut contending = NamedLock::<TestClient>::new("bigworld");
        assert!(
            !contending.lock(client_of(&shared, false)),
            "the name is already held, so the contender refuses"
        );
        assert!(!contending.is_locked());

        assert!(lock.unlock(), "the holder releases");
        assert!(!lock.is_locked());
        assert!(
            contending.lock(client_of(&shared, false)),
            "the contender takes it once it is free"
        );
        assert!(contending.is_locked());
        contending.unlock();
    }

    /// `lock` is a no-op while the lock is already held here — it neither
    /// steals nor re-acquires (`named_lock.cpp:107-116`).
    #[test]
    fn a_held_lock_refuses_a_second_lock() {
        let shared = Rc::new(RefCell::new(Shared::default()));

        let mut lock = NamedLock::<TestClient>::new("bigworld");
        assert!(lock.lock(client_of(&shared, false)));

        assert!(
            !lock.lock(client_of(&shared, false)),
            "already locked: refuse without asking the server"
        );
        assert!(lock.is_locked());
        assert_eq!(shared.borrow().acquisitions, 1, "one GET_LOCK, not two");
    }

    /// `unlock` returns `false` when nothing was held
    /// (`named_lock.cpp:124-141`).
    #[test]
    fn unlocking_an_unlocked_lock_is_a_noop() {
        let mut lock = NamedLock::<TestClient>::new("bigworld");
        assert!(!lock.unlock());
        assert!(!lock.is_locked());
    }

    /// A client that lost its connection is simply forgotten — the
    /// release is skipped and the lock still counts as given up
    /// (`named_lock.cpp:124-141`).
    #[test]
    fn a_lost_client_is_forgotten_without_a_release() {
        let shared = Rc::new(RefCell::new(Shared::default()));
        let client = client_of(&shared, true);
        let mut lock = NamedLock::<TestClient>::new("bigworld");

        assert!(lock.lock(client));
        assert!(lock.unlock());
        assert_eq!(
            shared.borrow().releases,
            Vec::<String>::new(),
            "a dead connection cannot be asked to release"
        );
        assert!(!lock.is_locked());
    }

    /// A release that fails is forgotten anyway, with the warning
    /// (`named_lock.cpp:124-141`).
    #[test]
    fn a_failed_release_forgets_the_client_and_warns() {
        let shared = Rc::new(RefCell::new(Shared::default()));
        let client = client_of(&shared, false);
        shared.borrow_mut().refuse_release = true;
        let mut lock = NamedLock::<TestClient>::new("bigworld");

        assert!(lock.lock(client));
        assert!(lock.unlock(), "forgetting still counts as released");
        assert!(!lock.is_locked());
        assert_eq!(
            shared.borrow().release_attempts,
            1,
            "the release was asked for"
        );
        assert_eq!(
            shared.borrow().releases,
            Vec::<String>::new(),
            "but nothing was confirmed"
        );
    }

    /// `~NamedLock` releases what was taken (`named_lock.cpp:96-99`).
    #[test]
    fn dropping_releases_the_lock() {
        let shared = Rc::new(RefCell::new(Shared::default()));
        let mut lock = NamedLock::<TestClient>::new("bigworld");
        assert!(lock.lock(client_of(&shared, false)));
        assert!(lock.is_locked());
        assert_eq!(lock.lock_name(), "BigWorld bigworld");

        drop(lock);
        assert_eq!(shared.borrow().releases, ["BigWorld bigworld"]);
    }

    /// The retry budget is what ends the loop when nothing releases the
    /// lock (`locked_connection.cpp:49-68`).
    #[test]
    fn the_retry_loop_gives_up_after_the_budget() {
        let shared = Rc::new(RefCell::new(Shared::default()));
        let mut locked = LockedConnection::new("bigworld");

        // Nothing is connected yet.
        assert!(locked.client_mut().is_none());

        // Someone else already holds the name.
        shared.borrow_mut().held.insert("BigWorld bigworld".into());

        let mut settles = Vec::new();
        let outcome =
            locked.connect_and_lock_with_retry(client_of(&shared, false), 2, |attempt, total| {
                settles.push((attempt, total));
            });

        assert!(!outcome, "the budget ran out");
        assert_eq!(settles, [(1, 2), (2, 2)], "one settle per retry");

        // A failed connect holds nothing: dropping it releases no lock.
        assert!(!locked.is_locked());
        drop(locked);
        assert_eq!(shared.borrow().releases, Vec::<String>::new());
    }

    /// Acceptance: the conflict clears mid-wait and the next attempt
    /// succeeds — the result is retryable, the state consistent
    /// (`locked_connection.cpp:49-68`).
    #[test]
    fn a_release_mid_wait_makes_the_next_attempt_succeed() {
        let shared = Rc::new(RefCell::new(Shared::default()));
        let mut locked = LockedConnection::new("bigworld");
        shared.borrow_mut().held.insert("BigWorld bigworld".into());

        let mut settles = Vec::new();
        let outcome =
            locked.connect_and_lock_with_retry(client_of(&shared, false), 3, |attempt, _total| {
                settles.push(attempt);
                if attempt == 2 {
                    // The other process gives up the lock.
                    shared.borrow_mut().held.remove("BigWorld bigworld");
                }
            });

        assert!(outcome, "the third attempt takes the freed lock");
        assert_eq!(settles, [1, 2]);
        // The connection is established and the live client is reachable.
        assert!(locked.is_locked());
        assert!(!locked.client_mut().unwrap().has_lost_connection());
        assert!(
            shared.borrow().held.contains("BigWorld bigworld"),
            "the connection now holds the lock"
        );

        // Giving it back up is a release, not a no-op.
        drop(locked);
        assert_eq!(shared.borrow().releases, ["BigWorld bigworld"]);
        assert!(!shared.borrow().held.contains("BigWorld bigworld"));
    }

    /// A second connect is refused, and the already-established
    /// connection stays untouched
    /// (`locked_connection.cpp:91-96`).
    #[test]
    fn a_second_connect_is_refused_and_leaves_the_lock_alone() {
        let shared = Rc::new(RefCell::new(Shared::default()));
        let mut locked = LockedConnection::new("bigworld");
        shared.borrow_mut().held.insert("BigWorld bigworld".into());

        let mut settles = Vec::new();
        let mut settle = |attempt: u8, _total: u8| {
            settles.push(attempt);
            // The other process gives up the lock while we wait.
            shared.borrow_mut().held.remove("BigWorld bigworld");
        };

        assert!(
            locked.connect_and_lock_with_retry(client_of(&shared, false), 2, &mut settle),
            "the second attempt takes the freed lock"
        );
        assert!(locked.is_locked());
        let acquisitions = shared.borrow().acquisitions;

        let refused = locked.connect_and_lock_with_retry(client_of(&shared, true), 2, &mut settle);

        assert!(!refused, "a connection is already established");
        assert!(locked.is_locked(), "the first connection is still bound");
        assert_eq!(settles, [1], "the refused call never waits");
        assert_eq!(
            shared.borrow().acquisitions,
            acquisitions,
            "the refused call takes no lock"
        );
        assert!(shared.borrow().releases.is_empty());

        drop(locked);
        assert_eq!(shared.borrow().releases, ["BigWorld bigworld"]);
    }

    /// A connection that lost itself already had its lock released by the
    /// server, so dropping it releases nothing again
    /// (`named_lock.cpp:124-141`, `locked_connection.cpp:27-39`).
    #[test]
    fn a_lost_connection_releases_nothing_on_drop() {
        let shared = Rc::new(RefCell::new(Shared::default()));
        let mut locked = LockedConnection::new("bigworld");
        shared.borrow_mut().held.insert("BigWorld bigworld".into());

        let mut settles = Vec::new();
        let outcome =
            locked.connect_and_lock_with_retry(client_of(&shared, true), 2, |attempt, _total| {
                settles.push(attempt);
                // The other process gives up the lock while we wait.
                shared.borrow_mut().held.remove("BigWorld bigworld");
            });

        assert!(outcome);
        assert_eq!(settles, [1]);
        assert!(locked.is_locked());
        assert!(locked.client_mut().unwrap().has_lost_connection());

        drop(locked);
        assert!(
            shared.borrow().releases.is_empty(),
            "the server already let the lock go"
        );
        assert_eq!(shared.borrow().release_attempts, 0);
    }

    /// The acceptance for the whole item, 结果可重试且状态一致: a lock
    /// conflict re-runs the operation and it completes once, while an
    /// error the transaction does not retry is terminal.
    #[test]
    fn a_lock_conflict_retries_and_an_unretriable_one_is_terminal() {
        // A deadlock inside a transaction is retried
        // (`transaction.cpp:57-60`), so it feeds the retry loop.
        let mut completion = Completion::new();
        let mut attempts = 0u32;
        let error_number = codes::ER_LOCK_DEADLOCK;
        let (count, outcome) = run_with_retry(|| {
            attempts += 1;
            if attempts < 3 && transaction_should_retry(error_number) {
                return Err(DbError::Retry);
            }
            Ok(7)
        });
        completion.complete(outcome);

        assert_eq!(count, 3, "the deadlock re-ran twice");
        assert_eq!(attempts, 3);
        assert_eq!(completion.completions(), 1, "one completion");
        assert_eq!(completion.outcome(), Some(&Ok(7)));

        // A lock wait timeout is not retried in a transaction: terminal.
        let mut completion: Completion<i32> = Completion::new();
        let mut attempts = 0u32;
        let error_number = codes::ER_LOCK_WAIT_TIMEOUT;
        let (count, outcome) = run_with_retry(|| {
            attempts += 1;
            // A lock wait timeout is not retried in a transaction
            // (`transaction.cpp:57-60`): the error is terminal.
            assert!(!transaction_should_retry(error_number));
            Err(DbError::Storage("lock wait timed out".into()))
        });
        completion.complete(outcome);

        assert_eq!(count, 1, "an unretriable error does not re-run");
        assert_eq!(attempts, 1);
        assert_eq!(completion.completions(), 1);
        assert!(completion.outcome().unwrap().is_err());
    }

    // --- helpers -----------------------------------------------------

    /// The server-side half of `GET_LOCK` (`named_lock.cpp:30-55`): a
    /// registry of held names every connection sees, plus the release
    /// log.
    #[derive(Default)]
    struct Shared {
        held: HashSet<String>,
        releases: Vec<String>,
        release_attempts: u32,
        acquisitions: u32,
        refuse_release: bool,
    }

    type SharedRegistry = Rc<RefCell<Shared>>;

    fn client_of(shared: &SharedRegistry, lost: bool) -> TestClient {
        TestClient {
            shared: Rc::clone(shared),
            lost,
        }
    }

    /// A recording connection onto the shared lock registry.
    struct TestClient {
        shared: SharedRegistry,
        lost: bool,
    }

    impl LockClient for TestClient {
        fn obtain_named_lock(&mut self, lock_name: &str) -> bool {
            let mut shared = self.shared.borrow_mut();
            shared.acquisitions += 1;
            if shared.held.contains(lock_name) {
                return false;
            }
            shared.held.insert(lock_name.into());
            true
        }

        fn release_named_lock(&mut self, lock_name: &str) -> bool {
            let mut shared = self.shared.borrow_mut();
            shared.release_attempts += 1;
            if shared.refuse_release {
                return false;
            }
            shared.releases.push(lock_name.into());
            shared.held.remove(lock_name);
            true
        }

        fn has_lost_connection(&self) -> bool {
            self.lost
        }
    }
}
