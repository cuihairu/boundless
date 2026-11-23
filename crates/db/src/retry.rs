//! The retry loop and the exactly-once completion of an entity operation.
//!
//! The reference runs every entity operation as a
//! `MySqlBackgroundTask::doBackgroundTask`
//! (`tasks/background_task.cpp:27-88`): the task runs in a transaction,
//! and a `DatabaseException` either re-runs the whole task — a lost
//! connection (which reconnects first) or a transient `shouldRetry()`
//! error, both after `onRetry()` — or fails the task for good. Either
//! way exactly one completion comes out: `doMainThreadTask` calls
//! `performMainThreadTask( succeeded_ )` once
//! (`background_task.cpp:100-110`), which is the handler's
//! `onPutEntityComplete`/`onDelEntityComplete`/... single callback.
//!
//! `onRetry` is the operation's own re-runnability — `PutEntityTask`
//! rewinds its stream so the transfer is read again from the start
//! (`put_entity_task.cpp:137-141`) — so here the operation closure is
//! simply called again. [`run_with_retry`] returns the attempt count,
//! the observable of the reference's reconnect bookkeeping
//! (`background_task.cpp:45-73` counts its reconnect attempts).

use crate::error::DbError;

/// Run `operation`, re-running it whenever it reports [`DbError::Retry`],
/// and return `(attempts, outcome)`.
///
/// The transient branch mirrors the reference's lost-connection and
/// `shouldRetry()` exceptions (`background_task.cpp:41-87`): the whole
/// operation runs again — it must be re-runnable the way the put's
/// rewound stream is. Any other error is terminal (`setFailure()`,
/// `background_task.cpp:88-95`); the caller then completes its
/// [`Completion`] exactly once with it. Like the reference's reconnect
/// loop, a stream of transient failures retries without bound; the count
/// starts at 1, so an operation that never retried ran once.
pub fn run_with_retry<T>(
    mut operation: impl FnMut() -> Result<T, DbError>,
) -> (u32, Result<T, DbError>) {
    let mut attempts = 0;
    loop {
        attempts += 1;
        match operation() {
            Ok(value) => return (attempts, Ok(value)),
            Err(DbError::Retry) => {}
            Err(err) => return (attempts, Err(err)),
        }
    }
}

/// The single completion of an entity operation — the reference's handler
/// objects (`IDatabase::IPutEntityHandler` and friends, `idatabase.hpp:181-200`,
/// `:231-243`). A handler is called back exactly once per operation, with
/// the outcome the task finished on; a second callback is a contract
/// breach.
///
/// `completions()` is the response count: it reads 1 for every delivered
/// outcome, whatever the operation went through on the way.
#[derive(Debug)]
pub struct Completion<T> {
    completions: u32,
    outcome: Option<Result<T, DbError>>,
}

impl<T> Default for Completion<T> {
    fn default() -> Self {
        Self {
            completions: 0,
            outcome: None,
        }
    }
}

impl<T> Completion<T> {
    /// An operation still in flight.
    pub fn new() -> Self {
        Self::default()
    }

    /// Deliver the operation's outcome. Panics on a second delivery — the
    /// reference fires a handler once.
    pub fn complete(&mut self, outcome: Result<T, DbError>) {
        assert!(
            self.outcome.is_none(),
            "the handler fires exactly once, got two completions"
        );
        self.completions += 1;
        self.outcome = Some(outcome);
    }

    /// How many times the handler has been called back: 0 while the
    /// operation is in flight, 1 after its completion.
    pub fn completions(&self) -> u32 {
        self.completions
    }

    /// The delivered outcome, if there is one.
    pub fn outcome(&self) -> Option<&Result<T, DbError>> {
        self.outcome.as_ref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::record::EntityRecord;
    use crate::storage::{EntityKey, EntityStorage, MemoryStorage};

    /// A transient failure re-runs the whole operation and completes once.
    #[test]
    fn a_transient_failure_retries_and_completes_once() {
        let mut completion = Completion::new();
        let outcomes = [Err(DbError::Retry), Err(DbError::Retry), Ok(5)];
        let mut ran = 0;

        let (attempts, outcome) = run_with_retry(|| {
            let result = outcomes[ran].clone();
            ran += 1;
            result
        });
        completion.complete(outcome);

        assert_eq!(attempts, 3, "the operation ran once per attempt");
        assert_eq!(ran, 3);
        assert_eq!(completion.completions(), 1, "one response, not three");
        assert_eq!(completion.outcome(), Some(&Ok(5)));
    }

    /// A terminal error fails the operation — the reference's
    /// `setFailure()` branch (`background_task.cpp:88-95`). A task that
    /// first retried and then hit the terminal error still ends in one
    /// failure response.
    #[test]
    fn a_terminal_error_fails_the_operation() {
        let mut completion = Completion::new();
        let outcomes = [Err(DbError::Retry), Err(DbError::NotFound)];
        let mut ran = 0;

        let (attempts, outcome): (u32, Result<(), DbError>) = run_with_retry(|| {
            let result = outcomes[ran].clone();
            ran += 1;
            result
        });
        completion.complete(outcome);

        assert_eq!(attempts, 2, "the transient error re-ran once");
        assert_eq!(ran, 2);
        assert_eq!(completion.completions(), 1);
        assert_eq!(completion.outcome(), Some(&Err(DbError::NotFound)));

        // A task with no transient error at all runs once.
        let mut ran = 0;
        let (attempts, outcome): (u32, Result<(), DbError>) = run_with_retry(|| {
            ran += 1;
            Err(DbError::NotFound)
        });
        assert_eq!(attempts, 1, "a terminal error does not re-run");
        assert_eq!(ran, 1);
        assert_eq!(outcome, Err(DbError::NotFound));
    }

    /// The acceptance, 响应次数和错误与原版一致: every entity operation
    /// driven through the retry loop comes out with exactly one response
    /// whose error matches the reference's isOK conditions — the update of
    /// a missing db id is `isOK=false` (`idatabase.hpp:195-196`), the
    /// repeated delete is `isOK=false` (`idatabase.hpp:239-240`), and the
    /// create hands back the allocated id
    /// (`idatabase.hpp:196-197`).
    #[test]
    fn the_entity_operations_match_the_reference_responses() {
        let mut store = MemoryStorage::new();
        let mut record = EntityRecord::new(1);
        record.name = Some("wenchuan".into());

        // A create that survives two transient failures completes once
        // with the allocated id.
        let mut completion = Completion::new();
        let mut flakiness = 2;
        let (attempts, outcome) = run_with_retry(|| {
            if flakiness > 0 {
                flakiness -= 1;
                return Err(DbError::Retry);
            }
            store.put_entity(&record, false)
        });
        completion.complete(outcome);
        let db_id = *completion
            .outcome()
            .expect("the create completed")
            .as_ref()
            .expect("the create is ok");
        assert_eq!(db_id, 1);
        assert_eq!(attempts, 3);
        assert_eq!(completion.completions(), 1);

        // The update of a db id that identifies nothing is terminal
        // isOK=false — it does not re-run.
        let mut completion = Completion::new();
        let mut ghost = EntityRecord::new(1);
        ghost.db_id = 99;
        let (attempts, outcome) = run_with_retry(|| store.put_entity(&ghost, false));
        completion.complete(outcome);
        assert_eq!(
            completion.outcome(),
            Some(&Err(DbError::NotFound)),
            "isOK=false for a dbID that identifies no existing entity"
        );
        assert_eq!(attempts, 1, "a terminal error never retries");
        assert_eq!(completion.completions(), 1);

        // The delete of the row succeeds once; the repeated delete is
        // terminal isOK=false.
        let key = EntityKey { type_id: 1, db_id };
        let mut completion = Completion::new();
        let (attempts, outcome) = run_with_retry(|| store.del_entity(&key));
        completion.complete(outcome);
        assert_eq!(completion.outcome(), Some(&Ok(())));
        assert_eq!(attempts, 1);

        let mut completion = Completion::new();
        let (attempts, outcome) = run_with_retry(|| store.del_entity(&key));
        completion.complete(outcome);
        assert_eq!(
            completion.outcome(),
            Some(&Err(DbError::NotFound)),
            "isOK=false for the entity that no longer exists"
        );
        assert_eq!(attempts, 1);
        assert_eq!(completion.completions(), 1);
    }

    /// A second callback on one handler is the breach the reference never
    /// commits — one completion per operation.
    #[test]
    #[should_panic(expected = "the handler fires exactly once")]
    fn a_second_completion_is_a_breach() {
        let mut completion: Completion<()> = Completion::new();
        completion.complete(Ok(()));
        completion.complete(Ok(()));
    }

    /// The completion starts in flight: no outcome, zero responses.
    #[test]
    fn an_in_flight_operation_has_not_completed() {
        let completion: Completion<()> = Completion::new();
        assert_eq!(completion.completions(), 0);
        assert!(completion.outcome().is_none());
    }
}
