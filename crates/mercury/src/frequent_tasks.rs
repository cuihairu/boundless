//! `Mercury::FrequentTask` + `FrequentTasks` port
//! (`lib/network/frequent_tasks.hpp/.cpp`).
//!
//! Reference contract preserved:
//! - `add`/`cancel` mark the container dirty; `process` stops the current
//!   pass when the container was modified during a `doTask` (the
//!   `isDirty_` iterator-invalidation guard);
//! - a re-entrant `process` (a task that runs the same container again)
//!   returns immediately when the container is dirty, and otherwise
//!   starts a full nested pass from the head -- both arms of the
//!   reference's `wasInProcess` check;
//! - `cancel` removes by pointer identity and reports whether the task
//!   was registered;
//! - tasks are shared handles, so a task re-adding itself or destroying
//!   sibling registrations mid-pass is safe (the pass holds its own
//!   reference to the task it is running).

use std::cell::{Cell, RefCell};
use std::rc::Rc;

/// `Mercury::FrequentTask`: run once per `processOnce` pass.
pub trait FrequentTask {
    /// `doTask`.
    fn do_task(&self);

    /// `taskName` (profiler labels).
    fn name(&self) -> &str {
        "UnnamedTask"
    }
}

/// Any closure as a `FrequentTask`.
pub struct FrequentTaskFn<F: Fn()>(pub F, pub &'static str);

impl<F: Fn()> FrequentTask for FrequentTaskFn<F> {
    fn do_task(&self) {
        (self.0)()
    }

    fn name(&self) -> &str {
        self.1
    }
}

/// `FrequentTasks`: the ordered container the dispatcher walks every
/// pass. The reference's `pGotDestroyed_` escape flag (a task destroying
/// the container signals back before any member is touched again) is
/// unrepresentable here: the container lives as long as its dispatcher's
/// last shared handle, and the pass clones each task before calling it,
/// so destruction mid-pass cannot leave the walk holding dangling state.
pub struct FrequentTasks {
    /// `container_`.
    container: RefCell<Vec<Rc<dyn FrequentTask>>>,
    /// `isDirty_`: set by add/cancel, stops the current pass.
    is_dirty: Cell<bool>,
    /// Non-null `pGotDestroyed_` while a pass is on the stack.
    in_process: Cell<bool>,
}

impl FrequentTasks {
    /// `FrequentTasks()`.
    pub fn new() -> Self {
        Self {
            container: RefCell::new(Vec::new()),
            is_dirty: Cell::new(false),
            in_process: Cell::new(false),
        }
    }

    /// `add`.
    pub fn add(&self, task: Rc<dyn FrequentTask>) {
        self.is_dirty.set(true);
        self.container.borrow_mut().push(task);
    }

    /// `cancel`.
    pub fn cancel(&self, task: &Rc<dyn FrequentTask>) -> bool {
        self.is_dirty.set(true);
        let mut container = self.container.borrow_mut();
        if let Some(index) = container
            .iter()
            .position(|registered| Rc::ptr_eq(registered, task))
        {
            container.remove(index);
            return true;
        }
        false
    }

    /// `process`: walk the tasks in registration order, stopping the pass
    /// when the container was modified during a task.
    pub fn process(&self) {
        // The reference's `wasInProcess` arm: a nested pass on a dirty
        // container refuses ("not processing"); a nested pass on a clean
        // one starts over from the head.
        let was_in_process = self.in_process.get();
        if was_in_process && self.is_dirty.get() {
            return;
        }
        self.is_dirty.set(false);

        if !was_in_process {
            self.in_process.set(true);
        }

        let mut index = 0;
        loop {
            let task = {
                let container = self.container.borrow();
                match container.get(index) {
                    Some(task) => Rc::clone(task),
                    None => break,
                }
            };
            // doTask may add, cancel, or re-enter this container; the
            // borrow is dropped around the call for exactly that.
            task.do_task();
            // A modification invalidates the walk's position -- the
            // reference breaks the pass and the next one starts from
            // the head again.
            if self.is_dirty.get() {
                break;
            }
            index += 1;
        }

        if !was_in_process {
            self.in_process.set(false);
        }
    }

    /// How many tasks are registered (test/observer surface).
    pub fn size(&self) -> usize {
        self.container.borrow().len()
    }
}

impl Default for FrequentTasks {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    /// A logging task; `log` records the order of every `doTask`.
    fn logging_task(
        log: &Rc<RefCell<Vec<&'static str>>>,
        name: &'static str,
    ) -> Rc<dyn FrequentTask> {
        let log = Rc::clone(log);
        Rc::new(FrequentTaskFn(
            move || log.borrow_mut().push(name),
            name,
        ))
    }

    /// Tasks run in registration order on every pass.
    #[test]
    fn tasks_run_in_registration_order_every_pass() {
        let log = Rc::new(RefCell::new(Vec::new()));
        let tasks = FrequentTasks::new();
        tasks.add(logging_task(&log, "first"));
        tasks.add(logging_task(&log, "second"));

        tasks.process();
        tasks.process();
        assert_eq!(*log.borrow(), ["first", "second", "first", "second"]);
    }

    /// `add`/`cancel` during a task stop the pass: the tasks after the
    /// modifying one wait for the next pass.
    #[test]
    fn a_modified_container_stops_the_current_pass() {
        let log = Rc::new(RefCell::new(Vec::new()));
        let tasks = Rc::new(FrequentTasks::new());

        let ran = Rc::new(Cell::new(false));
        let tasks_for_task = Rc::clone(&tasks);
        let ran_for_task = Rc::clone(&ran);
        let log_for_task = Rc::clone(&log);
        tasks.add(Rc::new(FrequentTaskFn(
            move || {
                log_for_task.borrow_mut().push("mutator");
                if !ran_for_task.get() {
                    ran_for_task.set(true);
                    // A mid-pass add: the container goes dirty.
                    let log = Rc::clone(&log_for_task);
                    tasks_for_task.add(Rc::new(FrequentTaskFn(
                        move || log.borrow_mut().push("late"),
                        "late",
                    )));
                    // The nested pass on a dirty container refuses
                    // ("not processing") -- nothing extra runs.
                    tasks_for_task.process();
                }
            },
            "mutator",
        )));
        tasks.add(logging_task(&log, "follower"));

        tasks.process();
        assert_eq!(*log.borrow(), ["mutator"], "the dirty pass stopped");

        tasks.process();
        assert_eq!(
            *log.borrow(),
            ["mutator", "mutator", "follower", "late"],
            "the next pass starts from the head, the late task at the tail"
        );
    }

    /// `cancel` removes by identity and reports whether the task was
    /// registered.
    #[test]
    fn cancel_removes_by_identity() {
        let tasks = FrequentTasks::new();
        let log = Rc::new(RefCell::new(Vec::new()));
        let kept = logging_task(&log, "kept");
        let dropped = logging_task(&log, "dropped");
        tasks.add(Rc::clone(&kept));
        tasks.add(Rc::clone(&dropped));

        assert!(tasks.cancel(&dropped));
        assert!(!tasks.cancel(&dropped));
        assert_eq!(tasks.size(), 1);

        tasks.process();
        assert_eq!(*log.borrow(), ["kept"]);
    }

    /// The default task name matches the reference's
    /// `FrequentTask( "UnnamedTask" )`, and `default()` builds the same
    /// empty container as `new()`.
    #[test]
    fn task_names_match_the_reference() {
        let defaulted = FrequentTasks::default();
        assert_eq!(defaulted.size(), 0);

        struct Bare;
        impl FrequentTask for Bare {
            fn do_task(&self) {}
        }
        let bare: Rc<dyn FrequentTask> = Rc::new(Bare);
        assert_eq!(bare.name(), "UnnamedTask");
        bare.do_task();

        let named: Rc<dyn FrequentTask> = Rc::new(FrequentTaskFn(|| {}, "SignalProcessor"));
        assert_eq!(named.name(), "SignalProcessor");
    }

    /// A re-entrant pass on a clean container runs a full nested pass
    /// from the head; on a dirty one it refuses. This is the reference's
    /// `wasInProcess` pair. The nesting task guards itself with a
    /// once-flag -- unguarded re-entry is unbounded recursion in the
    /// reference too.
    #[test]
    fn a_nested_pass_restarts_clean_and_refuses_dirty() {
        let log = Rc::new(RefCell::new(Vec::new()));
        let tasks = Rc::new(FrequentTasks::new());

        let tasks_for_nested = Rc::clone(&tasks);
        let log_for_nested = Rc::clone(&log);
        let nested = Rc::new(Cell::new(false));
        let nested_for_task = Rc::clone(&nested);
        tasks.add(Rc::new(FrequentTaskFn(
            move || {
                log_for_nested.borrow_mut().push("outer");
                if !nested_for_task.get() {
                    nested_for_task.set(true);
                    tasks_for_nested.process();
                }
            },
            "outer",
        )));
        tasks.add(logging_task(&log, "tail"));

        tasks.process();
        // The nested pass runs "outer" then "tail"; the outer pass then
        // continues (clean again) with "tail".
        assert_eq!(*log.borrow(), ["outer", "outer", "tail", "tail"]);
    }
}
