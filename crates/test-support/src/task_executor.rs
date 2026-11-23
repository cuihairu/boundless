//! BW-27-03-04: executing a long task that can be cancelled.
//!
//! A rescan or a load run can outlast the caller's patience. The executor
//! runs a long task's steps in order and checks the shared cancel flag before
//! each one, so a cancellation stops the run at its next step boundary — no
//! step is half-done. The executor exits either way and reports how it ended:
//! `Finished` when every step ran, `Cancelled` when the flag stopped it, with
//! the count of steps it actually ran.

use crate::request_cancel::Cancel;

/// One unit of work a long task is split into, run by the executor in order.
pub trait TaskStep {
    /// Do the step's work.
    fn run(&mut self);
}

/// How an executor's run of a long task ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Every step ran to the end.
    Finished,
    /// A cancellation was seen; the executor stopped at a step boundary.
    Cancelled,
}

impl Outcome {
    /// The stable line the tool prints for the outcome, naming how many of the
    /// task's steps ran.
    pub fn report(&self, executed: usize, total: usize) -> String {
        match self {
            Outcome::Finished => format!("FINISHED {executed}/{total} steps"),
            Outcome::Cancelled => format!("CANCELLED {executed}/{total} steps"),
        }
    }
}

/// The executor of one long task: runs its steps in order, checking a shared
/// cancel flag before each one.
pub struct TaskExecutor {
    cancel: Cancel,
    executed: usize,
}

impl TaskExecutor {
    /// An executor that watches `cancel`.
    pub fn new(cancel: Cancel) -> Self {
        Self {
            cancel,
            executed: 0,
        }
    }

    /// Run the task's steps in order. A raised flag stops the run before the
    /// next step; the outcome reports which way the run ended.
    pub fn run<S: TaskStep>(&mut self, steps: &mut [S]) -> Outcome {
        for step in steps {
            if self.cancel.is_cancelled() {
                return Outcome::Cancelled;
            }
            step.run();
            self.executed += 1;
        }
        Outcome::Finished
    }

    /// The steps the run completed, whether it finished or was cancelled.
    pub fn executed(&self) -> usize {
        self.executed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A test step that counts the whole run's completed steps in one shared
    /// counter, and raises the shared cancel flag after a given number of them
    /// — a step that discovers the work should stop.
    struct Tick {
        cancel: Cancel,
        total: std::sync::Arc<std::sync::atomic::AtomicUsize>,
        cancel_after: usize,
    }

    impl TaskStep for Tick {
        fn run(&mut self) {
            use std::sync::atomic::Ordering;
            let ran = self.total.fetch_add(1, Ordering::SeqCst) + 1;
            if ran == self.cancel_after {
                self.cancel.cancel();
            }
        }
    }

    fn ticks(
        cancel: &Cancel,
        count: usize,
        cancel_after: usize,
    ) -> (Vec<Tick>, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
        let total = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let steps = (0..count)
            .map(|_| Tick {
                cancel: cancel.clone(),
                total: total.clone(),
                cancel_after,
            })
            .collect();
        (steps, total)
    }

    #[test]
    fn an_uncancelled_run_finishes_every_step() {
        let cancel = Cancel::new();
        let mut executor = TaskExecutor::new(cancel.clone());
        let (mut steps, _total) = ticks(&cancel, 5, usize::MAX);

        // No flag ever raises: every step runs, the run finishes.
        assert_eq!(executor.run(&mut steps), Outcome::Finished);
        assert_eq!(executor.executed(), 5);
        assert_eq!(Outcome::Finished.report(5, 5), "FINISHED 5/5 steps");
    }

    #[test]
    fn a_cancelled_run_stops_at_the_next_step_and_reports_it() {
        let cancel = Cancel::new();
        let mut executor = TaskExecutor::new(cancel.clone());
        let (mut steps, _total) = ticks(&cancel, 5, 2);

        // The third step's pre-check sees the flag the second step raised:
        // the run stops at the boundary, no step is half-done.
        assert_eq!(executor.run(&mut steps), Outcome::Cancelled);
        assert_eq!(executor.executed(), 2);
        assert_eq!(Outcome::Cancelled.report(2, 5), "CANCELLED 2/5 steps");
    }

    #[test]
    fn a_run_that_starts_cancelled_does_no_work() {
        let cancel = Cancel::new();
        cancel.cancel();
        let mut executor = TaskExecutor::new(cancel.clone());
        let (mut steps, _total) = ticks(&cancel, 5, usize::MAX);

        // The flag is already up: the executor exits without a step.
        assert_eq!(executor.run(&mut steps), Outcome::Cancelled);
        assert_eq!(executor.executed(), 0);
    }

    #[test]
    fn the_two_outcomes_report_distinguishably() {
        // A client tells a cancelled run from a finished one by the line.
        assert_ne!(
            Outcome::Finished.report(3, 5),
            Outcome::Cancelled.report(3, 5)
        );
    }

    #[test]
    fn the_executed_count_is_only_the_steps_that_ran() {
        let cancel = Cancel::new();
        let mut executor = TaskExecutor::new(cancel.clone());
        let (mut steps, _total) = ticks(&cancel, 10, 4);

        assert_eq!(executor.run(&mut steps), Outcome::Cancelled);
        // Two steps ran after the one that raised the flag, so four ran.
        assert_eq!(executor.executed(), 4);
    }
}
