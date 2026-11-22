//! Minimal `Mercury::EventDispatcher` + `FrequentTasks` port
//! (`lib/network/event_dispatcher.cpp`, `lib/network/frequent_tasks.cpp`)
//! for the BW-01 bootstrap loop.
//!
//! Reference contract preserved:
//! - `processContinuously()` resets `breakProcessing_` and loops
//!   `processOnce( shouldIdle )` until a break;
//! - `processOnce` resets `breakProcessing_ = false` *at entry*, then
//!   runs the frequent tasks, then the timer pass
//!   (`if (!breakProcessing_) processTimers()`) -- a break raised inside a
//!   frequent task skips the rest of the pass (timers/network) and unwinds
//!   the outer loop, exactly like the reference;
//! - `FrequentTasks::process` stops the current pass when the task list
//!   was modified during a `doTask` (the `isDirty_` guard);
//! - `cancelFrequentTask` returns whether the task existed;
//! - the dispatcher owns the `TimeQueue64` and the `ClockFn` exactly like
//!   `EventDispatcher` (`pTimeQueue_`, `clock_`, `numTimerCalls_`).
//!
//! The network pass is a `NetworkPass` seam (`pPoller_->processPendingEvents`
//! reduced to its call shape): until one is wired, the step idles with a
//! short sleep so a never-broken loop does not busy-spin. `stats` processing
//! is instrumentation-only and not ported.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::thread;
use std::time::Duration;

use crate::timers::{DispatcherTimers, TimerHandle, TimerHandler};

/// `EventDispatcher::processNetwork`'s wait surface, reduced to its call
/// shape: run one poller pass, waiting up to `max_wait` seconds, and report
/// how many descriptors triggered. The `mercury::poller::EventPoller`
/// satisfies it (see the impl below); a fake backs the ordering tests.
pub trait NetworkPass {
    /// `pPoller_->processPendingEvents( maxWait )`.
    fn process_pending_events(&mut self, max_wait: f64) -> i32;
}

/// The `EventPoller` implements the dispatcher's network pass directly, as
/// `EventDispatcher::pPoller_` does in the reference. Linux only, matching
/// the poller's own port gate.
#[cfg(target_os = "linux")]
impl NetworkPass for mercury::poller::EventPoller {
    fn process_pending_events(&mut self, max_wait: f64) -> i32 {
        mercury::poller::EventPoller::process_pending_events(self, max_wait)
    }
}

/// `Mercury::FrequentTask`: run once per `processOnce`.
pub trait FrequentTask {
    fn do_task(&mut self);

    /// `taskName` (profiler labels; the default matches the reference).
    fn task_name(&self) -> &str {
        "UnnamedTask"
    }
}

/// Any closure as a `FrequentTask`.
pub struct FrequentTaskFn<F: FnMut()>(pub F, pub &'static str);

impl<F: FnMut()> FrequentTask for FrequentTaskFn<F> {
    fn do_task(&mut self) {
        (self.0)()
    }

    fn task_name(&self) -> &str {
        self.1
    }
}

/// `Mercury::EventDispatcher` (bootstrap subset).
pub struct Dispatcher {
    tasks: Vec<Rc<RefCell<dyn FrequentTask>>>,
    /// `FrequentTasks::isDirty_`: set by add/cancel (and by tasks once
    /// BW-02 handlers can mutate the task list mid-pass), stops the
    /// current processing pass. Shared for the same reason as the break
    /// flag.
    tasks_dirty: Rc<Cell<bool>>,
    /// `breakProcessing_`, shared so app code that only holds the flag
    /// (the `mainDispatcher_` member flattened to an `Rc`) can break the
    /// loop.
    break_flag: Rc<Cell<bool>>,
    /// `pTimeQueue_` + `clock_` + `numTimerCalls_` (the event dispatcher's
    /// timer surface, see `crate::timers`).
    timers: DispatcherTimers,
    /// `pPoller_`: the network pass, absent until the app wires one (a
    /// `mercury` poller or a test fake).
    network_pass: Option<Box<dyn NetworkPass>>,
}

impl Default for Dispatcher {
    fn default() -> Self {
        Self::new()
    }
}

impl Dispatcher {
    /// `maxWait_`: the ceiling on any single pass's idle wait, in seconds.
    const MAX_WAIT: f64 = 0.1;

    pub fn new() -> Self {
        Self::with_timers(DispatcherTimers::new())
    }

    /// `EventDispatcher( ClockFn )`: build the loop around an injected
    /// timer clock for virtual-clock tests.
    pub fn with_timers(timers: DispatcherTimers) -> Self {
        Self {
            tasks: Vec::new(),
            tasks_dirty: Rc::new(Cell::new(false)),
            break_flag: Rc::new(Cell::new(false)),
            timers,
            network_pass: None,
        }
    }

    /// Wire `pPoller_`: the dispatcher's network pass.
    pub fn set_network_pass(&mut self, pass: Box<dyn NetworkPass>) {
        self.network_pass = Some(pass);
    }

    /// The timer surface (`pTimeQueue_` etc.).
    pub fn timers(&self) -> &DispatcherTimers {
        &self.timers
    }

    pub fn timers_mut(&mut self) -> &mut DispatcherTimers {
        &mut self.timers
    }

    /// `EventDispatcher::addTimer( microseconds, handler, arg, name )`.
    pub fn add_timer(
        &mut self,
        microseconds: i64,
        handler: Rc<dyn TimerHandler<u64>>,
        user: usize,
        name: &'static str,
    ) -> TimerHandle<u64> {
        self.timers.add_timer(microseconds, handler, user, name)
    }

    /// `EventDispatcher::addOnceOffTimer`.
    pub fn add_once_off_timer(
        &mut self,
        microseconds: i64,
        handler: Rc<dyn TimerHandler<u64>>,
        user: usize,
        name: &'static str,
    ) -> TimerHandle<u64> {
        self.timers
            .add_once_off_timer(microseconds, handler, user, name)
    }

    /// `addFrequentTask`.
    pub fn add_frequent_task(&mut self, task: Rc<RefCell<dyn FrequentTask>>) {
        self.tasks_dirty.set(true);
        self.tasks.push(task);
    }

    /// `cancelFrequentTask`: remove by pointer identity, returning
    /// whether it was registered.
    pub fn cancel_frequent_task(&mut self, task: &Rc<RefCell<dyn FrequentTask>>) -> bool {
        self.tasks_dirty.set(true);
        if let Some(index) = self
            .tasks
            .iter()
            .position(|registered| Rc::ptr_eq(registered, task))
        {
            self.tasks.remove(index);
            return true;
        }
        false
    }

    /// `breakProcessing( breakState )`.
    pub fn break_processing(&self, break_state: bool) {
        self.break_flag.set(break_state);
    }

    /// The `mainDispatcher_` handle flattened: app code that must break
    /// processing without owning the dispatcher (ServerApp::shutDown's
    /// `mainDispatcher_.breakProcessing()`).
    pub fn break_flag(&self) -> Rc<Cell<bool>> {
        Rc::clone(&self.break_flag)
    }

    /// The `isDirty_` state, shared like the break flag for tasks that
    /// modify the task list during a pass (the `isDirty_` guard).
    pub fn tasks_dirty_flag(&self) -> Rc<Cell<bool>> {
        Rc::clone(&self.tasks_dirty)
    }

    /// `FrequentTasks::process` / `processFrequentTasks`.
    pub fn process_frequent_tasks(&mut self) {
        self.tasks_dirty.set(false);
        let mut index = 0;
        while index < self.tasks.len() {
            let task = Rc::clone(&self.tasks[index]);
            task.borrow_mut().do_task();
            if self.tasks_dirty.get() {
                // The reference stops the pass; the next processOnce
                // starts from the head again.
                break;
            }
            index += 1;
        }
    }

    /// `processOnce( shouldIdle )`: the frequent-task pass, then the timer
    /// pass, then the network pass, in the reference's order. Returns the
    /// number of descriptors the network pass triggered (0 when a break
    /// short-circuits the pass, or none is wired).
    pub fn process_once(&mut self, should_idle: bool) -> i32 {
        // The reference resets the break flag on every entry.
        self.break_flag.set(false);

        self.process_frequent_tasks();

        // `if (!breakProcessing_) this->processTimers();`
        if !self.break_flag.get() {
            self.timers.process_timers();
        }

        // `this->processStats()` runs here; instrumentation-only, so not
        // ported.

        // `if (!breakProcessing_) return this->processNetwork( shouldIdle );`
        if !self.break_flag.get() {
            self.process_network(should_idle)
        } else {
            0
        }
    }

    /// `processNetwork( shouldIdle )`: wait until the earliest timer (the
    /// pass's `maxWait`), then run one poller pass.
    fn process_network(&mut self, should_idle: bool) -> i32 {
        // `double maxWait = shouldIdle ? this->calculateWait() : 0.0;`
        let max_wait = if should_idle {
            self.calculate_wait()
        } else {
            0.0
        };

        match &mut self.network_pass {
            Some(pass) => pass.process_pending_events(max_wait),
            None => {
                // Bootstrap: no poller wired yet, so an idle pass sleeps
                // instead of busy-spinning.
                if should_idle {
                    thread::sleep(Duration::from_millis(1));
                }
                0
            }
        }
    }

    /// `calculateWait`: the longest this pass may wait, bounded by the
    /// next timer. Child dispatchers (`childDispatchers_`) are not ported,
    /// so no recursion here.
    pub fn calculate_wait(&self) -> f64 {
        // `double maxWait = maxWait_;`
        let mut max_wait = Self::MAX_WAIT;

        // `if (!pTimeQueue_->empty()) maxWait = min(maxWait,
        // pTimeQueue_->nextExp( clock_() ) / stampsPerSecondD());`
        if !self.timers.time_queue.is_empty() {
            let next = self.timers.next_exp_seconds();
            max_wait = max_wait.min(next);
        }

        max_wait
    }

    /// `processContinuously` / `processUntilBreak`.
    pub fn process_until_break(&mut self) {
        self.break_flag.set(false);
        while !self.break_flag.get() {
            self.process_once(true);
        }
    }

    /// `processUntilSignalled( signal )`: same loop, also stopped by an
    /// external flag.
    pub fn process_until_signalled(&mut self, signal: &Cell<bool>) {
        while !self.break_flag.get() && !signal.get() {
            self.process_once(true);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A task that counts invocations.
    fn counting_task(counter: Rc<AtomicUsize>) -> Rc<RefCell<dyn FrequentTask>> {
        Rc::new(RefCell::new(FrequentTaskFn(
            move || {
                counter.fetch_add(1, Ordering::SeqCst);
            },
            "counter",
        )))
    }

    /// Frequent tasks run in registration order and repeat every pass.
    #[test]
    fn frequent_tasks_run_in_order_every_pass() {
        let log = Rc::new(RefCell::new(Vec::new()));
        let mut dispatcher = Dispatcher::new();

        for name in ["first", "second"] {
            let log = Rc::clone(&log);
            let name: &'static str = Box::leak(name.to_string().into_boxed_str());
            dispatcher.add_frequent_task(Rc::new(RefCell::new(FrequentTaskFn(
                move || log.borrow_mut().push(name),
                name,
            ))));
        }

        dispatcher.process_once(false);
        dispatcher.process_once(false);
        assert_eq!(*log.borrow(), vec!["first", "second", "first", "second"]);
    }

    /// `task_name` defaults to the reference's "UnnamedTask" and the
    /// adapter reports its own; both task kinds execute.
    #[test]
    fn task_names_match_the_reference() {
        struct DefaultTask;
        impl FrequentTask for DefaultTask {
            fn do_task(&mut self) {}
        }
        let default_task: Rc<RefCell<dyn FrequentTask>> = Rc::new(RefCell::new(DefaultTask));
        assert_eq!(default_task.borrow().task_name(), "UnnamedTask");

        let named = Rc::new(RefCell::new(FrequentTaskFn(|| {}, "SignalProcessor")));
        assert_eq!(named.borrow().task_name(), "SignalProcessor");

        let mut dispatcher = Dispatcher::new();
        dispatcher.add_frequent_task(default_task);
        dispatcher.add_frequent_task(named);
        dispatcher.process_frequent_tasks();
    }

    /// `Dispatcher::default()` constructs the same empty loop state.
    #[test]
    fn default_dispatcher_starts_clean() {
        let dispatcher = Dispatcher::default();
        assert!(!dispatcher.break_flag().get());
        assert!(!dispatcher.tasks_dirty_flag().get());
    }

    /// `isDirty_` set during a pass stops it: the tasks after the
    /// modifying one wait for the next pass (the reference's guard for
    /// tasks that change the task list while it runs).
    #[test]
    fn a_dirty_pass_stops_and_the_next_pass_completes() {
        let log = Rc::new(RefCell::new(Vec::new()));
        let mut dispatcher = Dispatcher::new();

        let dirty = dispatcher.tasks_dirty_flag();
        let ran = Cell::new(false);
        dispatcher.add_frequent_task(Rc::new(RefCell::new(FrequentTaskFn(
            {
                let log = Rc::clone(&log);
                move || {
                    log.borrow_mut().push("mutator".to_string());
                    if !ran.get() {
                        ran.set(true);
                        dirty.set(true);
                    }
                }
            },
            "mutator",
        ))));
        dispatcher.add_frequent_task(Rc::new(RefCell::new(FrequentTaskFn(
            {
                let log = Rc::clone(&log);
                move || log.borrow_mut().push("follower".to_string())
            },
            "follower",
        ))));

        // Pass 1: the mutator dirties the list, the follower is skipped.
        dispatcher.process_frequent_tasks();
        assert_eq!(*log.borrow(), ["mutator"]);

        // Pass 2: clean start, both run.
        dispatcher.process_frequent_tasks();
        assert_eq!(*log.borrow(), ["mutator", "mutator", "follower"]);
    }

    /// `cancelFrequentTask` removes by identity and reports whether the
    /// task existed.
    #[test]
    fn cancel_frequent_task_removes_by_identity() {
        let counter = Rc::new(AtomicUsize::new(0));
        let mut dispatcher = Dispatcher::new();
        let kept = counting_task(Rc::clone(&counter));
        let dropped = counting_task(Rc::clone(&counter));
        dispatcher.add_frequent_task(Rc::clone(&kept));
        dispatcher.add_frequent_task(Rc::clone(&dropped));

        assert!(dispatcher.cancel_frequent_task(&dropped));
        assert!(!dispatcher.cancel_frequent_task(&dropped));

        dispatcher.process_once(false);
        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }

    /// `breakProcessing(true)` during a frequent task stops the current
    /// pass and unwinds `processUntilBreak`.
    #[test]
    fn a_break_inside_a_frequent_task_unwinds_the_loop() {
        let counter = Rc::new(AtomicUsize::new(0));
        let mut dispatcher = Dispatcher::new();
        dispatcher.add_frequent_task(counting_task(Rc::clone(&counter)));

        let break_flag = dispatcher.break_flag();
        dispatcher.add_frequent_task(Rc::new(RefCell::new(FrequentTaskFn(
            move || break_flag.set(true),
            "breaker",
        ))));

        dispatcher.process_until_break();
        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }

    /// The reference quirk: `processOnce` resets the break flag at
    /// entry, so a pre-set break does not stop a single pass.
    #[test]
    fn process_once_resets_the_break_flag_on_entry() {
        let counter = Rc::new(AtomicUsize::new(0));
        let mut dispatcher = Dispatcher::new();
        dispatcher.add_frequent_task(counting_task(Rc::clone(&counter)));
        dispatcher.break_processing(true);

        dispatcher.process_once(false);
        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }

    /// `processUntilSignalled` stops on the external flag as well as a
    /// break.
    #[test]
    fn process_until_signalled_stops_on_the_flag() {
        let counter = Rc::new(AtomicUsize::new(0));
        let mut dispatcher = Dispatcher::new();
        dispatcher.add_frequent_task(counting_task(Rc::clone(&counter)));

        let signal = Rc::new(Cell::new(false));
        let passes = Rc::new(AtomicUsize::new(0));
        let break_flag = dispatcher.break_flag();
        dispatcher.add_frequent_task(Rc::new(RefCell::new(FrequentTaskFn(
            {
                let passes = Rc::clone(&passes);
                let signal = Rc::clone(&signal);
                move || {
                    passes.fetch_add(1, Ordering::SeqCst);
                    if passes.load(Ordering::SeqCst) >= 2 {
                        signal.set(true);
                        break_flag.set(true);
                    }
                }
            },
            "signaller",
        ))));

        dispatcher.process_until_signalled(&signal);
        assert_eq!(counter.load(Ordering::SeqCst), 2);
    }

    /// The dispatcher's timer surface is reachable directly: an empty
    /// timer pass fires nothing.
    #[test]
    fn the_timer_surface_is_reachable_from_the_dispatcher() {
        let mut dispatcher = Dispatcher::new();
        assert_eq!(dispatcher.timers_mut().process_timers(), 0);
        assert_eq!(dispatcher.timers().num_timer_calls(), 0);
    }

    /// A scripted `NetworkPass`: records the waits it was handed and
    /// returns a fixed event count.
    struct FakePass {
        waits: Rc<RefCell<Vec<f64>>>,
        result: i32,
    }

    impl NetworkPass for FakePass {
        fn process_pending_events(&mut self, max_wait: f64) -> i32 {
            self.waits.borrow_mut().push(max_wait);
            self.result
        }
    }

    /// A `NetworkPass` that also records its run into the shared ordering
    /// log, so the three-pass order is observable.
    struct NetworkLoggingPass {
        log: Rc<RefCell<Vec<&'static str>>>,
        waits: Rc<RefCell<Vec<f64>>>,
        result: i32,
    }

    impl NetworkPass for NetworkLoggingPass {
        fn process_pending_events(&mut self, max_wait: f64) -> i32 {
            self.log.borrow_mut().push("network");
            self.waits.borrow_mut().push(max_wait);
            self.result
        }
    }

    /// The reference order inside one pass: frequent tasks, then timers,
    /// then network events. A break raised in any earlier pass skips the
    /// rest.
    #[test]
    fn a_single_pass_runs_frequent_then_timers_then_network() {
        let log = Rc::new(RefCell::new(Vec::new()));
        let clock = Rc::new(Cell::new(1_000_000_000u64));
        let clock_for_closure = Rc::clone(&clock);
        let timers =
            crate::timers::DispatcherTimers::with_clock(Box::new(move || clock_for_closure.get()));
        let mut dispatcher = Dispatcher::with_timers(timers);
        dispatcher.set_network_pass(Box::new(NetworkLoggingPass {
            log: Rc::clone(&log),
            waits: Rc::new(RefCell::new(Vec::new())),
            result: 3,
        }));

        let log_for_task = Rc::clone(&log);
        dispatcher.add_frequent_task(Rc::new(RefCell::new(FrequentTaskFn(
            move || log_for_task.borrow_mut().push("frequent"),
            "frequent",
        ))));
        let log_for_timer = Rc::clone(&log);
        dispatcher.add_once_off_timer(
            500_000,
            Rc::new(crate::timers::TimerHandlerFn::new(
                move |_, _| log_for_timer.borrow_mut().push("timer"),
                "timer",
            )),
            0,
            "timer",
        );

        // Due at 1.5s; the pass must see the timer after the frequent task.
        clock.set(1_500_000_000);
        let events = dispatcher.process_once(false);
        assert_eq!(*log.borrow(), ["frequent", "timer", "network"]);
        assert_eq!(events, 3, "the network pass's count is surfaced");
    }

    /// A break inside a frequent task skips both the timer and the network
    /// pass, exactly like `if (!breakProcessing_)` at each step.
    #[test]
    fn a_break_in_a_frequent_task_skips_timers_and_network() {
        let log = Rc::new(RefCell::new(Vec::new()));
        let mut dispatcher = Dispatcher::new();
        let break_flag = dispatcher.break_flag();
        dispatcher.add_frequent_task(Rc::new(RefCell::new(FrequentTaskFn(
            {
                let log = Rc::clone(&log);
                move || {
                    log.borrow_mut().push("frequent");
                    break_flag.set(true);
                }
            },
            "breaker",
        ))));
        let log_for_timer = Rc::clone(&log);
        dispatcher.add_once_off_timer(
            1,
            Rc::new(crate::timers::TimerHandlerFn::new(
                move |_, _| log_for_timer.borrow_mut().push("timer"),
                "timer",
            )),
            0,
            "timer",
        );
        dispatcher.set_network_pass(Box::new(NetworkLoggingPass {
            log: Rc::clone(&log),
            waits: Rc::new(RefCell::new(Vec::new())),
            result: 0,
        }));

        let events = dispatcher.process_once(false);
        assert_eq!(*log.borrow(), ["frequent"], "no timer or network");
        assert_eq!(events, 0, "the break short-circuits the network pass");
    }

    /// A break inside a timer callback skips the network pass.
    #[test]
    fn a_break_in_a_timer_skips_network() {
        let log = Rc::new(RefCell::new(Vec::new()));
        let mut dispatcher = Dispatcher::new();
        let break_flag = dispatcher.break_flag();
        let log_for_timer = Rc::clone(&log);
        dispatcher.add_once_off_timer(
            1,
            Rc::new(crate::timers::TimerHandlerFn::new(
                {
                    let log = Rc::clone(&log_for_timer);
                    move |_, _| {
                        log.borrow_mut().push("timer");
                        break_flag.set(true);
                    }
                },
                "timer",
            )),
            0,
            "timer",
        );
        dispatcher.set_network_pass(Box::new(NetworkLoggingPass {
            log: Rc::clone(&log),
            waits: Rc::new(RefCell::new(Vec::new())),
            result: 0,
        }));

        let events = dispatcher.process_once(false);
        assert_eq!(*log.borrow(), ["timer"], "no network after the timer");
        assert_eq!(events, 0);
    }

    /// `calculateWait`: the pass waits at most `maxWait_` (0.1s), and less
    /// when a timer is due sooner. An empty queue returns the ceiling.
    #[test]
    fn calculate_wait_is_capped_by_max_wait_and_the_next_timer() {
        // Empty queue: the 0.1s ceiling.
        let dispatcher = Dispatcher::new();
        assert!((dispatcher.calculate_wait() - 0.1).abs() < f64::EPSILON);

        let clock = Rc::new(Cell::new(0u64));
        let clock_for_closure = Rc::clone(&clock);
        let timers =
            crate::timers::DispatcherTimers::with_clock(Box::new(move || clock_for_closure.get()));
        let mut dispatcher = Dispatcher::with_timers(timers);

        // A timer due in 250ms: 0.25s of stamps, but capped at 0.1 by
        // maxWait_ -- a timer beyond the ceiling does not change the wait.
        dispatcher.add_once_off_timer(
            250_000,
            Rc::new(crate::timers::TimerHandlerFn::new(|_, _| {}, "far")),
            0,
            "far",
        );
        assert!((dispatcher.calculate_wait() - 0.1).abs() < f64::EPSILON);

        // A timer due in 50ms: 0.05s of stamps, under the ceiling, so the
        // wait drops to the timer's distance.
        dispatcher.add_once_off_timer(
            50_000,
            Rc::new(crate::timers::TimerHandlerFn::new(|_, _| {}, "near")),
            0,
            "near",
        );
        assert!((dispatcher.calculate_wait() - 0.05).abs() < 1e-9);
    }

    /// The network pass is handed `calculateWait()` when idling, and 0.0
    /// when the pass must not block.
    #[test]
    fn the_network_pass_receives_the_computed_wait() {
        let waits = Rc::new(RefCell::new(Vec::new()));
        let mut dispatcher = Dispatcher::new();
        dispatcher.set_network_pass(Box::new(FakePass {
            waits: Rc::clone(&waits),
            result: 0,
        }));

        // Idling: the 0.1s ceiling (empty timer queue).
        dispatcher.process_once(true);
        {
            let handed = waits.borrow();
            assert_eq!(handed.len(), 1);
            assert!((handed[0] - 0.1).abs() < f64::EPSILON);
        }

        // Non-idling: no wait at all.
        dispatcher.process_once(false);
        let handed = waits.borrow();
        assert_eq!(handed.len(), 2);
        assert_eq!(handed[1], 0.0);
    }

    /// A pass with no network pass wired keeps the bootstrap idle: it must
    /// not busy-spin, and it reports no events.
    #[test]
    fn a_pass_without_a_network_pass_idles_and_reports_nothing() {
        let mut dispatcher = Dispatcher::new();
        let started = std::time::Instant::now();
        let events = dispatcher.process_once(true);
        let elapsed = started.elapsed();
        assert_eq!(events, 0);
        assert!(
            elapsed.as_millis() >= 1,
            "the idle sleep runs so the loop does not busy-spin"
        );
    }

    /// The real `mercury::poller::EventPoller` satisfies the dispatcher's
    /// `NetworkPass` seam: a wired poller dispatches an actual network
    /// event inside the pass, and reports the count.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_wired_mercury_poller_dispatches_network_events() {
        use mercury::poller::{EventPoller, InputNotificationHandler};
        use std::os::fd::RawFd;

        /// A pipe standing in for a socket, closed on drop.
        struct PipePair {
            fds: [RawFd; 2],
        }
        impl PipePair {
            fn new() -> Self {
                let mut fds = [-1i32; 2];
                let rc = unsafe { libc::pipe(fds.as_mut_ptr()) };
                assert_eq!(rc, 0, "pipe failed");
                Self { fds }
            }
            fn read_end(&self) -> RawFd {
                self.fds[0]
            }
            fn write_end(&self) -> RawFd {
                self.fds[1]
            }
        }
        impl Drop for PipePair {
            fn drop(&mut self) {
                for fd in self.fds {
                    if fd >= 0 {
                        unsafe {
                            libc::close(fd);
                        }
                    }
                }
            }
        }

        /// The reference's `FakeHandler`: count input notifications.
        struct Counter(Cell<u32>);
        impl InputNotificationHandler for Counter {
            fn handle_input_notification(&self, _fd: RawFd) -> i32 {
                self.0.set(self.0.get() + 1);
                0
            }
        }

        let pipes = PipePair::new();
        let poller = EventPoller::create();
        let counter = Rc::new(Counter(Cell::new(0)));
        assert!(poller.register_for_read(
            pipes.read_end(),
            Rc::clone(&counter) as Rc<dyn InputNotificationHandler>,
            "pipe",
        ));

        let mut dispatcher = Dispatcher::new();
        dispatcher.set_network_pass(Box::new(poller));

        // Nothing pending: the pass reports no events.
        assert_eq!(dispatcher.process_once(false), 0);
        assert_eq!(counter.0.get(), 0);

        // A byte wakes the poll pass, dispatching inside `processOnce`.
        let byte = b"x";
        let written =
            unsafe { libc::write(pipes.write_end(), byte.as_ptr() as *const libc::c_void, 1) };
        assert_eq!(written, 1);
        assert_eq!(dispatcher.process_once(false), 1);
        assert_eq!(counter.0.get(), 1);

        // Drained: nothing pending any more.
        let mut drain = [0u8; 1];
        let read =
            unsafe { libc::read(pipes.read_end(), drain.as_mut_ptr() as *mut libc::c_void, 1) };
        assert_eq!(read, 1);
        assert_eq!(dispatcher.process_once(false), 0);
        assert_eq!(counter.0.get(), 1);
    }

    /// delivery time passes.
    #[test]
    fn the_dispatcher_runs_frequent_tasks_then_timers() {
        use crate::timers::TimerHandlerFn;

        let log = Rc::new(RefCell::new(Vec::new()));
        let clock = Rc::new(Cell::new(1_000_000_000u64));
        let clock_for_closure = Rc::clone(&clock);
        let timers = DispatcherTimers::with_clock(Box::new(move || clock_for_closure.get()));
        let mut dispatcher = Dispatcher::with_timers(timers);

        let log_for_task = Rc::clone(&log);
        dispatcher.add_frequent_task(Rc::new(RefCell::new(FrequentTaskFn(
            move || log_for_task.borrow_mut().push("frequent"),
            "frequent",
        ))));
        let log_for_timer = Rc::clone(&log);
        dispatcher.add_once_off_timer(
            500_000,
            Rc::new(TimerHandlerFn::new(
                move |_, _| log_for_timer.borrow_mut().push("timer"),
                "timer",
            )),
            0,
            "timer",
        );

        // addTimer maps 500000us to 0.5s of stamps on the pinned clock.
        assert_eq!(dispatcher.timers().time_queue.next_exp(0), 1_500_000_000);

        // First pass: not due yet.
        dispatcher.process_once(false);
        assert_eq!(*log.borrow(), ["frequent"]);
        assert_eq!(dispatcher.timers().num_timer_calls(), 0);

        // Second pass at t=1.5s: the frequent task repeats, the timer
        // fires.
        clock.set(1_500_000_000);
        dispatcher.process_once(false);
        assert_eq!(*log.borrow(), ["frequent", "frequent", "timer"]);
        assert_eq!(dispatcher.timers().num_timer_calls(), 1);
    }

    /// the timer pass (`if (!breakProcessing_) processTimers()`).
    #[test]
    fn a_break_inside_a_frequent_task_skips_the_timer_pass() {
        use crate::timers::TimerHandlerFn;

        let clock = Rc::new(Cell::new(1_000_000_000u64));
        let clock_for_closure = Rc::clone(&clock);
        let timers = DispatcherTimers::with_clock(Box::new(move || clock_for_closure.get()));
        let mut dispatcher = Dispatcher::with_timers(timers);

        let break_flag = dispatcher.break_flag();
        dispatcher.add_frequent_task(Rc::new(RefCell::new(FrequentTaskFn(
            move || break_flag.set(true),
            "breaker",
        ))));

        // Due at 1.1s; the clock is moved there first so the timer would
        // fire if the pass ran.
        let skipped_log = Rc::new(RefCell::new(Vec::new()));
        let skipped_for_closure = Rc::clone(&skipped_log);
        dispatcher.add_timer(
            100_000,
            Rc::new(TimerHandlerFn::new(
                move |_, _| skipped_for_closure.borrow_mut().push("skipped"),
                "skipped",
            )),
            0,
            "skipped",
        );
        clock.set(1_100_000_000);

        dispatcher.process_once(false);
        assert_eq!(*skipped_log.borrow(), Vec::<&'static str>::new());
        assert_eq!(dispatcher.timers().num_timer_calls(), 0);
    }
}
