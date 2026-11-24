//! Retry and completion-callback discipline for a database operation.
//!
//! The reference runs every entity operation as a background task
//! (`db_storage_mysql/tasks/`), and the retry contract lives in
//! `MySqlBackgroundTask::doBackgroundTask` (`tasks/background_task.cpp:51-100`):
//!
//! ```ignore
//! bool retry;
//! do {
//!     retry = false;
//!     try {
//!         succeeded_ = true;
//!         MySqlTransaction transaction( connection );   // rolls back if it unwinds
//!         performBackgroundTask( connection );
//!         transaction.commit();
//!     } catch (DatabaseException & e) {
//!         if ( e.isLostConnection() ) { reconnect loop; retry = true; onRetry(); }
//!         else if ( e.shouldRetry() )     { retry = true; onRetry(); }
//!         else                            { setFailure(); onException( e ); }
//!     }
//! } while ( retry );
//! mgr.addMainThreadTask( this );                      // fires the handler once
//! ```
//!
//! So an operation is attempted until it either commits or dies on a failure
//! the retry rules do not cover, and the handler callback
//! (`IGetEntityHandler::onGetEntityComplete`, `IPutEntityHandler::onPutEntityComplete`,
//! `IDelEntityHandler::onDelEntityComplete`, the
//! `ILookUpEntitiesHandler::onLookUpEntitiesEnd(hasError)` face) is invoked
//! **exactly once** with the verdict of that final attempt. This module carries
//! that contract: the failure classification, the attempt loop, the
//! `onRetry` state reset, and the once-only completion callback.

use crate::error::DbError;

/// `ER_LOCK_WAIT_TIMEOUT` (`mysqld_error.h`): the attempt is rolled back and
/// run again.
pub const ER_LOCK_WAIT_TIMEOUT: u32 = 1205;
/// `ER_LOCK_DEADLOCK` (`mysqld_error.h`): the attempt is rolled back and run
/// again.
pub const ER_LOCK_DEADLOCK: u32 = 1213;
/// `CR_CONNECTION_ERROR` (`errmsg.h`): the server could not be reached.
pub const CR_CONNECTION_ERROR: u32 = 2002;
/// `CR_CONN_HOST_ERROR` (`errmsg.h`): the server host could not be reached.
pub const CR_CONN_HOST_ERROR: u32 = 2003;
/// `CR_SERVER_GONE_ERROR` (`errmsg.h`): the server closed the connection.
pub const CR_SERVER_GONE_ERROR: u32 = 2006;
/// `CR_SERVER_LOST` (`errmsg.h`): the server went away mid-query.
pub const CR_SERVER_LOST: u32 = 2013;

/// A way a database attempt can fail, and with it the retry verdict the
/// reference attaches to each shape.
///
/// `DatabaseException::shouldRetry` is true only for
/// `ER_LOCK_DEADLOCK`/`ER_LOCK_WAIT_TIMEOUT` (`database_exception.cpp:35-40`)
/// and `isLostConnection` only for the four `CR_` codes
/// (`database_exception.cpp:47-52`). Everything else is reported straight to
/// the handler.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DatabaseFailure {
    /// The attempt is rolled back and run again.
    Retryable(String),
    /// The connection is dead, so it is re-established first and the attempt
    /// run again.
    LostConnection(String),
    /// Reported to the handler as a failed operation; never retried.
    Fatal(String),
}

impl DatabaseFailure {
    /// `DatabaseException::shouldRetry`.
    pub fn should_retry(&self) -> bool {
        matches!(self, DatabaseFailure::Retryable(_))
    }

    /// `DatabaseException::isLostConnection`.
    pub fn is_lost_connection(&self) -> bool {
        matches!(self, DatabaseFailure::LostConnection(_))
    }

    /// The error text, mirroring `DatabaseException::what`.
    pub fn message(&self) -> &str {
        match self {
            DatabaseFailure::Retryable(msg)
            | DatabaseFailure::LostConnection(msg)
            | DatabaseFailure::Fatal(msg) => msg,
        }
    }

    /// Classify a raw MySQL error number the way the reference does: the two
    /// lock errors retry, the four client-connection codes lose the connection,
    /// everything else (`ER_ACCESS_DENIED_ERROR`, `ER_DUP_ENTRY`, ...) is
    /// fatal for the operation.
    pub fn classify(num: u32, message: &str) -> DatabaseFailure {
        match num {
            ER_LOCK_DEADLOCK | ER_LOCK_WAIT_TIMEOUT => {
                DatabaseFailure::Retryable(message.to_owned())
            }
            CR_CONNECTION_ERROR | CR_CONN_HOST_ERROR | CR_SERVER_GONE_ERROR | CR_SERVER_LOST => {
                DatabaseFailure::LostConnection(message.to_owned())
            }
            _ => DatabaseFailure::Fatal(message.to_owned()),
        }
    }
}

/// A [`DbError`] a backend raises for one of the transient conditions carries
/// the same verdict as [`DatabaseFailure::Retryable`].
pub fn failure_from_db_error(error: &DbError, message: &str) -> DatabaseFailure {
    if error == &DbError::Retry {
        DatabaseFailure::Retryable(message.to_owned())
    } else {
        DatabaseFailure::Fatal(message.to_owned())
    }
}

/// What one run of the retry loop did, for assertion purposes.
///
/// `reconnect_attempts` counts the reconnect calls exactly the way the
/// reference's `attempts` counter does (started at one, bumped after every
/// failed try, so a successful reconnect is the last one counted).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RunReport {
    /// Background attempts that ran.
    pub attempts: usize,
    /// The verdict handed to the completion callback.
    pub succeeded: bool,
    /// Reconnect calls made while chasing a lost connection.
    pub reconnect_attempts: usize,
    /// `on_retry` invocations.
    pub on_retry_calls: usize,
    /// The message of the fatal failure, when the operation failed.
    pub fatal: Option<String>,
}

/// One database operation, driven the way `EntityTask` is.
///
/// Implementers supply the background work and the completion callback; the
/// default `on_retry`/`on_exception` bodies mirror the tasks that do not
/// override them.
pub trait RetryTask {
    /// The background work of one attempt. `Ok(())` is the committed attempt.
    fn perform_background(&mut self) -> Result<(), DatabaseFailure>;

    /// Called after a failure that will be retried, to drop attempt-local
    /// state. `GetEntityTask::onRetry` resets `threadStream_` so a retry does
    /// not concatenate the partial output of the attempt that failed.
    fn on_retry(&mut self) {}

    /// Called once when the attempt that failed fatally is given up on
    /// (`setFailure` + `onException`).
    fn on_exception(&mut self, _failure: &DatabaseFailure) {}

    /// The main-thread completion callback, e.g.
    /// `handler.onGetEntityComplete(succeeded, entityKey, ...)`. Fires exactly
    /// once per operation, whatever the number of attempts.
    fn complete(&mut self, succeeded: bool);
}

/// The reference retry loop plus its main-thread completion step.
///
/// `reconnect` stands in for `threadData.reconnect()` and is called until it
/// reports the connection back; the reference sleeps a second between tries,
/// which the port drops because the loop here is driven synchronously.
///
/// `reconnect` is handed the run report while it is being filled in, the way
/// the reference reads `attempts` out of the enclosing scope.
pub fn run_with_retry(
    task: &mut dyn RetryTask,
    reconnect: &mut dyn FnMut(&mut RunReport) -> bool,
) -> RunReport {
    let mut report = RunReport::default();
    let mut retry = true;
    while retry {
        retry = false;
        report.attempts += 1;
        report.succeeded = true;
        match task.perform_background() {
            Ok(()) => {}
            Err(failure) => {
                if failure.is_lost_connection() {
                    while !reconnect(&mut report) {
                        report.reconnect_attempts += 1;
                    }
                    report.reconnect_attempts += 1;
                    report.on_retry_calls += 1;
                    task.on_retry();
                    retry = true;
                } else if failure.should_retry() {
                    report.on_retry_calls += 1;
                    task.on_retry();
                    retry = true;
                } else {
                    report.fatal = Some(failure.message().to_owned());
                    report.succeeded = false;
                    task.on_exception(&failure);
                }
            }
        }
    }
    task.complete(report.succeeded);
    report
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An operation whose attempts fail with a scripted sequence of failures
    /// and then commit.
    struct ScriptedTask {
        script: Vec<DatabaseFailure>,
        retries: usize,
        exceptions: Vec<String>,
        completed: Vec<bool>,
    }

    impl RetryTask for ScriptedTask {
        fn perform_background(&mut self) -> Result<(), DatabaseFailure> {
            if self.script.is_empty() {
                return Ok(());
            }
            Err(self.script.remove(0))
        }

        fn on_retry(&mut self) {
            self.retries += 1;
        }

        fn on_exception(&mut self, failure: &DatabaseFailure) {
            self.exceptions.push(failure.message().to_owned());
        }

        fn complete(&mut self, succeeded: bool) {
            self.completed.push(succeeded);
        }
    }

    fn script(failures: &[DatabaseFailure]) -> ScriptedTask {
        ScriptedTask {
            script: failures.to_vec(),
            retries: 0,
            exceptions: Vec::new(),
            completed: Vec::new(),
        }
    }

    #[test]
    fn a_clean_attempt_completes_once_and_succeeds() {
        let mut task = script(&[]);
        let mut reconnect_attempts = 0usize;
        let report = run_with_retry(&mut task, &mut |_tried: &mut RunReport| {
            reconnect_attempts += 1;
            true
        });
        assert_eq!(reconnect_attempts, 0);

        assert_eq!(report.attempts, 1);
        assert!(report.succeeded);
        assert_eq!(report.reconnect_attempts, 0);
        assert_eq!(report.on_retry_calls, 0);
        assert_eq!(report.fatal, None);
        assert_eq!(task.completed, [true]);
    }

    #[test]
    fn retryable_failures_are_retried_until_the_attempt_commits() {
        let failures = [
            DatabaseFailure::Retryable("deadlock".into()),
            DatabaseFailure::Retryable("lock wait timeout".into()),
        ];
        let mut task = script(&failures);
        let mut attempts = 0usize;
        let report = run_with_retry(&mut task, &mut |_tried: &mut RunReport| {
            attempts += 1;
            true
        });

        assert_eq!(report.attempts, 3);
        assert!(report.succeeded);
        // The reconnect hook is never touched on a pure retry.
        assert_eq!(attempts, 0);
        assert_eq!(report.reconnect_attempts, 0);
        assert_eq!(report.on_retry_calls, 2);
        assert_eq!(task.retries, 2);
        assert!(task.exceptions.is_empty());
        // One callback for the whole operation.
        assert_eq!(task.completed, [true]);
    }

    #[test]
    fn a_lost_connection_reconnects_before_retrying() {
        let failures = [DatabaseFailure::LostConnection("server gone away".into())];
        let mut task = script(&failures);
        let mut attempts = 0usize;
        let report = run_with_retry(&mut task, &mut |_tried: &mut RunReport| {
            attempts += 1;
            true
        });

        assert_eq!(report.attempts, 2);
        assert!(report.succeeded);
        assert_eq!(attempts, 1);
        assert_eq!(report.reconnect_attempts, 1);
        assert_eq!(report.on_retry_calls, 1);
        assert_eq!(task.completed, [true]);
    }

    #[test]
    fn reconnects_are_counted_until_the_connection_comes_back() {
        let failures = [DatabaseFailure::LostConnection("lost".into())];
        let mut task = script(&failures);
        let mut live = 0usize;
        let report = run_with_retry(&mut task, &mut |_tried: &mut RunReport| {
            live += 1;
            live >= 3
        });

        // The reference counts its first try as attempt one and bumps after
        // every failed try, so a success on the third try reports three.
        assert_eq!(live, 3);
        assert_eq!(report.reconnect_attempts, 3);
        assert_eq!(report.attempts, 2);
        assert!(report.succeeded);
        assert_eq!(task.completed, [true]);
    }

    #[test]
    fn a_fatal_failure_fails_the_operation_once() {
        let failures = [DatabaseFailure::Fatal("access denied".into())];
        let mut task = script(&failures);
        let report = run_with_retry(&mut task, &mut |_report: &mut RunReport| true);

        assert_eq!(report.attempts, 1);
        assert!(!report.succeeded);
        assert_eq!(report.fatal, Some("access denied".to_owned()));
        assert_eq!(report.on_retry_calls, 0);
        assert_eq!(task.exceptions, ["access denied"]);
        assert_eq!(task.completed, [false]);
    }

    /// An attempt that produces output part-way through: the reference resets
    /// that output on `onRetry` so a retry does not concatenate it with the
    /// attempt that succeeded.
    struct StreamingTask {
        script: Vec<DatabaseFailure>,
        buffer: Vec<u8>,
        flushed: Vec<Vec<u8>>,
    }

    impl RetryTask for StreamingTask {
        fn perform_background(&mut self) -> Result<(), DatabaseFailure> {
            self.buffer.extend([4, 2]);
            if self.script.is_empty() {
                return Ok(());
            }
            Err(self.script.remove(0))
        }

        fn on_retry(&mut self) {
            self.buffer.clear();
        }

        fn complete(&mut self, _succeeded: bool) {
            self.flushed.push(std::mem::take(&mut self.buffer));
        }
    }

    #[test]
    fn on_retry_drops_the_attempt_local_state() {
        let mut task = StreamingTask {
            script: vec![DatabaseFailure::Retryable("deadlock".into())],
            buffer: Vec::new(),
            flushed: Vec::new(),
        };
        let report = run_with_retry(&mut task, &mut |_report: &mut RunReport| true);

        assert_eq!(report.attempts, 2);
        assert!(report.succeeded);
        assert_eq!(task.flushed, [vec![4, 2]]);
    }

    /// A task that overrides neither `on_retry` nor `on_exception` still gets
    /// its once-only completion callback.
    struct MinimalTask {
        done: Vec<bool>,
    }

    impl RetryTask for MinimalTask {
        fn perform_background(&mut self) -> Result<(), DatabaseFailure> {
            Err(DatabaseFailure::Fatal("no such table".into()))
        }

        fn complete(&mut self, succeeded: bool) {
            self.done.push(succeeded);
        }
    }

    #[test]
    fn a_task_that_overrides_nothing_still_completes_once() {
        let mut task = MinimalTask { done: Vec::new() };
        let report = run_with_retry(&mut task, &mut |_report: &mut RunReport| true);

        assert_eq!(report.attempts, 1);
        assert!(!report.succeeded);
        assert_eq!(report.fatal, Some("no such table".to_owned()));
        assert_eq!(task.done, [false]);
        // The default `on_retry` body is a no-op.
        task.on_retry();
    }

    #[test]
    fn failures_classify_the_way_the_reference_does() {
        let retryable = DatabaseFailure::classify(ER_LOCK_DEADLOCK, "deadlock");
        let wait = DatabaseFailure::classify(ER_LOCK_WAIT_TIMEOUT, "wait timeout");
        assert!(retryable.should_retry());
        assert!(!retryable.is_lost_connection());
        assert!(wait.should_retry());
        assert_eq!(retryable.message(), "deadlock");

        for code in [
            CR_CONNECTION_ERROR,
            CR_CONN_HOST_ERROR,
            CR_SERVER_GONE_ERROR,
            CR_SERVER_LOST,
        ] {
            let lost = DatabaseFailure::classify(code, "gone");
            assert!(lost.is_lost_connection(), "{code}");
            assert!(!lost.should_retry(), "{code}");
            assert_eq!(lost.message(), "gone");
        }

        for code in [1045u32, 1062, 1146] {
            let fatal = DatabaseFailure::classify(code, "no");
            assert!(!fatal.should_retry(), "{code}");
            assert!(!fatal.is_lost_connection(), "{code}");
            assert_eq!(fatal.message(), "no");
        }

        assert_eq!(
            failure_from_db_error(&DbError::Retry, "busy"),
            DatabaseFailure::Retryable("busy".to_owned())
        );
        assert_eq!(
            failure_from_db_error(&DbError::NotFound, "missing"),
            DatabaseFailure::Fatal("missing".to_owned())
        );
    }
}
