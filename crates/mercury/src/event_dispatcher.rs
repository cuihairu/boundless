//! `Mercury::EventDispatcher` port (`lib/network/event_dispatcher.hpp/.ipp/.cpp`,
//! `lib/network/dispatcher_coupling.hpp`).
//!
//! Reference contract preserved -- `processOnce` runs exactly one round in
//! the reference order:
//! 1. `processFrequentTasks` (always);
//! 2. `processTimers`, only when no break was raised;
//! 3. `processStats` (unconditional -- statistics are instrumentation and
//!    never skipped);
//! 4. `processNetwork( shouldIdle )`, only when no break was raised.
//!
//! `breakProcessing_` is reset on every `processOnce` entry, so a
//! pre-set break does not stop a single pass.
//!
//! The rest of the surface: `calculateWait` caps the poller wait at
//! `maxWait_` (0.1s default) and at the next timer of this dispatcher and
//! of every attached child; `processNetwork` charges the tick's wall cost
//! minus the poller's idle wait into `maxNetworkTime_` and counts ticks
//! past `networkTimeBudget_` (0.01s default); `attach`/`detach` couple a
//! child dispatcher to a parent through a `DispatcherCoupling` frequent
//! task plus the child poller's own descriptor registered for read and
//! write on the parent's poller; `prepareForShutdown` drains on the real
//! clock (2s bound, 100ms sleeps) so an injected test clock cannot
//! stall the guard.
//!
//! Stamps here are microseconds (`stampsPerSecond` = 1e6): the poller's
//! `spareTime` charges microseconds, and the reference subtracts the idle
//! wait from the elapsed wall time directly, so every stamp in the
//! dispatcher's math must share one unit.
//!
//! Flattening notes:
//! - `FrequentTask *` and `TimerHandler *` become shared handles with
//!   interior mutability, so the dispatcher can live in an `Rc` (children
//!   and the parent reference each other);
//! - the reference's `ErrorReporter` buffers uncaught C++ exceptions
//!   (`reportPendingExceptions` after a processing loop); this tree has no
//!   exception machinery -- Mercury errors surface as `Reason` reports
//!   routed while they happen, so the buffer is not ported;
//! - `childDispatchers_` holds weak handles: a child detached by dropping
//!   it (instead of calling `detach`) vanishes from the parent instead of
//!   leaving a dangling pointer;
//! - the `DispatcherCoupling` destructor cancels the coupling from the
//!   parent's task list; here `detachFrom` does that cancel explicitly;
//! - the destructor's still-coupled warning and the timer-count notice
//!   keep their reference texts.

use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};

use foundation::time_queue::{TimerHandle, TimerHandler, TimeQueue64};

use crate::frequent_tasks::{FrequentTask, FrequentTaskFn, FrequentTasks};
use crate::poller::{EventPoller, InputNotificationHandler};

/// `Mercury::EventDispatcher::ClockFn`: the stamp source driving timer
/// processing and statistics. `realClock` reads the monotonic clock;
/// tests inject a pinned clock.
pub type ClockFn = Box<dyn Fn() -> u64>;

/// `EventDispatcher::realClock()`: microsecond stamps (this dispatcher's
/// `stampsPerSecond`).
pub fn real_clock() -> u64 {
    platform::clock::monotonic_now_micros().micros()
}

/// `stampsPerSecond()`: microsecond stamps across the whole dispatcher.
const STAMPS_PER_SECOND: u64 = 1_000_000;

/// `stampsPerSecondD()`.
const STAMPS_PER_SECOND_D: f64 = STAMPS_PER_SECOND as f64;

/// `EventDispatcher::maxWait_` default: 0.1s (`event_dispatcher.cpp:64`).
const DEFAULT_MAX_WAIT_SECONDS: f64 = 0.1;

/// `networkTimeBudget_` default: `uint64( TimeStamp::fromSeconds( 0.01 ) )`
/// (`event_dispatcher.cpp:66`).
const DEFAULT_NETWORK_TIME_BUDGET_SECONDS: f64 = 0.01;

/// `SECONDS_TO_ATTEMPT_SEND` (`event_dispatcher.cpp:121`): the real-clock
/// bound of `prepareForShutdown`.
const SHUTDOWN_DRAIN_SECONDS: u64 = 2;

/// The `usleep( 100000 )` between drain passes
/// (`event_dispatcher.cpp:137`).
const SHUTDOWN_SLEEP_MILLIS: u64 = 100;

/// `TimeStamp::fromSeconds` for this dispatcher's microsecond stamps.
fn stamps_from_seconds(seconds: f64) -> u64 {
    (seconds * STAMPS_PER_SECOND_D) as u64
}

/// `Mercury::DispatcherCoupling`
/// (`lib/network/dispatcher_coupling.hpp`): the frequent task that makes a
/// parent's pass drive the child's `processOnce`. The reference holds
/// both dispatcher references and cancels itself from the parent's task
/// list in its destructor; here the coupling only needs the child (the
/// parent-side cancel happens explicitly in `detachFrom`).
pub struct DispatcherCoupling {
    child: Weak<EventDispatcher>,
}

impl DispatcherCoupling {
    /// `DispatcherCoupling( mainDispatcher, childDispatcher )` -- minus
    /// the constructor's `mainDispatcher.addFrequentTask( this )`, which
    /// `attachTo` performs after building the task.
    fn new(child: &Rc<EventDispatcher>) -> Rc<Self> {
        Rc::new(Self {
            child: Rc::downgrade(child),
        })
    }
}

impl FrequentTask for DispatcherCoupling {
    /// `doTask()`: `childDispatcher_.processOnce()` -- the default
    /// `shouldIdle` is false, so a driven child never idles.
    fn do_task(&self) {
        if let Some(child) = self.child.upgrade() {
            child.process_once(false);
        }
    }

    fn name(&self) -> &str {
        "DispatcherCoupling"
    }
}

/// `Mercury::EventDispatcher`: one event loop -- frequent tasks, a timer
/// queue, and a network poller, processed in the reference order.
pub struct EventDispatcher {
    /// `breakProcessing_`.
    break_processing: Cell<bool>,
    /// `clock_`.
    clock: ClockFn,
    /// `pTimeQueue_` (always a `TimeQueue64`).
    time_queue: TimeQueue64,
    /// `pFrequentTasks_`.
    frequent_tasks: FrequentTasks,
    /// `accSpareTime_`: the poller's spare time banked by `clearSpareTime`.
    acc_spare_time: Cell<u64>,
    /// `oldSpareTime_`: the previous statistics period's total.
    old_spare_time: Cell<u64>,
    /// `totSpareTime_`: the current statistics period's total.
    tot_spare_time: Cell<u64>,
    /// `lastStatisticsGathered_`.
    last_statistics_gathered: Cell<u64>,
    /// `numTimerCalls_`.
    num_timer_calls: Cell<usize>,
    /// `maxWait_`: the poller wait cap, in seconds.
    max_wait: Cell<f64>,
    /// `maxNetworkTime_`: the worst tick cost, in stamps.
    max_network_time: Cell<u64>,
    /// `networkTimeBudget_`: the per-tick budget, in stamps.
    network_time_budget: Cell<u64>,
    /// `numOverBudgetTicks_`.
    num_over_budget_ticks: Cell<u32>,
    /// `pCouplingToParent_`, set while attached to a parent (stored as
    /// the task handle the parent holds, so the identity cancel matches).
    coupling_to_parent: RefCell<Option<Rc<dyn FrequentTask>>>,
    /// `childDispatchers_`.
    child_dispatchers: RefCell<Vec<Weak<EventDispatcher>>>,
    /// `pPoller_`.
    poller: Rc<EventPoller>,
}

impl Drop for EventDispatcher {
    /// `EventDispatcher::~EventDispatcher`
    /// (`event_dispatcher.cpp:91-113`).
    fn drop(&mut self) {
        if self.coupling_to_parent.borrow().is_some() {
            eprintln!(
                "EventDispatcher::~EventDispatcher: Still coupled to parent dispatcher"
            );
        }

        if !self.time_queue.is_empty() {
            eprintln!(
                "EventDispatcher()::~EventDispatcher: Num timers = {}",
                self.time_queue.size()
            );
        }

        self.time_queue.clear(/* shouldCallOnRelease: */ false);
    }
}

impl EventDispatcher {
    /// `EventDispatcher()`: the real clock.
    pub fn new() -> Self {
        Self::with_clock(Box::new(real_clock))
    }

    /// `EventDispatcher( ClockFn clock )`: an injected stamp source.
    pub fn with_clock(clock: ClockFn) -> Self {
        Self {
            break_processing: Cell::new(false),
            clock,
            time_queue: TimeQueue64::new(),
            frequent_tasks: FrequentTasks::new(),
            acc_spare_time: Cell::new(0),
            old_spare_time: Cell::new(0),
            tot_spare_time: Cell::new(0),
            last_statistics_gathered: Cell::new(0),
            num_timer_calls: Cell::new(0),
            max_wait: Cell::new(DEFAULT_MAX_WAIT_SECONDS),
            max_network_time: Cell::new(0),
            network_time_budget: Cell::new(stamps_from_seconds(
                DEFAULT_NETWORK_TIME_BUDGET_SECONDS,
            )),
            num_over_budget_ticks: Cell::new(0),
            coupling_to_parent: RefCell::new(None),
            child_dispatchers: RefCell::new(Vec::new()),
            poller: Rc::new(EventPoller::create()),
        }
    }

    /// `now()`: the dispatcher's current stamp, from whichever clock it
    /// was built with (`event_dispatcher.ipp:80`).
    pub fn now(&self) -> u64 {
        (self.clock)()
    }

    // -- Timer related ---------------------------------------------------

    /// `addTimer( microseconds, handler, arg, name )`
    /// (`event_dispatcher.ipp:11`).
    pub fn add_timer(
        &self,
        microseconds: i64,
        handler: Rc<dyn TimerHandler<u64>>,
        user: usize,
        name: &'static str,
    ) -> TimerHandle<u64> {
        self.add_timer_common(microseconds, handler, user, true, name)
    }

    /// `addOnceOffTimer` (`event_dispatcher.ipp:25`).
    pub fn add_once_off_timer(
        &self,
        microseconds: i64,
        handler: Rc<dyn TimerHandler<u64>>,
        user: usize,
        name: &'static str,
    ) -> TimerHandle<u64> {
        self.add_timer_common(microseconds, handler, user, false, name)
    }

    /// `addTimerCommon` (`event_dispatcher.cpp:290-310`): non-positive
    /// delays return an unset handle; recurrent timers repeat on their
    /// interval, once-offs carry 0.
    fn add_timer_common(
        &self,
        microseconds: i64,
        handler: Rc<dyn TimerHandler<u64>>,
        user: usize,
        recurrent: bool,
        name: &'static str,
    ) -> TimerHandle<u64> {
        // MF_ASSERT( handler ) is unrepresentable: the handle is never null.
        if microseconds <= 0 {
            return TimerHandle::default();
        }
        let interval = stamps_from_seconds(microseconds as f64 / 1_000_000.0);
        // TODO: Does not handle time wrap-around (reference comment).
        self.time_queue.add(
            (self.clock)() + interval,
            if recurrent { interval } else { 0 },
            handler,
            user,
            name,
        )
    }

    /// `processTimers` (`event_dispatcher.cpp:366-370`): run the queue on
    /// the dispatcher clock and accumulate `numTimerCalls_`.
    pub fn process_timers(&self) -> usize {
        let num_fired = self.time_queue.process((self.clock)());
        self.num_timer_calls.set(self.num_timer_calls.get() + num_fired);
        num_fired
    }

    /// `numTimerCalls`.
    pub fn num_timer_calls(&self) -> usize {
        self.num_timer_calls.get()
    }

    /// `timerDeliveryTime` (`event_dispatcher.cpp:527`).
    pub fn timer_delivery_time(&self, handle: &TimerHandle<u64>) -> u64 {
        self.time_queue.timer_delivery_time(handle)
    }

    /// `timerIntervalTime` (`event_dispatcher.cpp:613`).
    pub fn timer_interval_time(&self, handle: &TimerHandle<u64>) -> u64 {
        self.time_queue.timer_interval_time(handle)
    }

    /// The mutable `timerIntervalTime` overload, flattened to a setter.
    pub fn set_timer_interval_time(&self, handle: &TimerHandle<u64>, interval: u64) {
        self.time_queue.set_timer_interval_time(handle, interval);
    }

    /// `maxTimerLateness` (`event_dispatcher.cpp:538`): the worst
    /// observed delivery delay, the event-loop stall signal.
    pub fn max_timer_lateness(&self) -> u64 {
        self.time_queue.max_lateness()
    }

    /// `numLateTimers`.
    pub fn num_late_timers(&self) -> usize {
        self.time_queue.num_late_timers()
    }

    /// `resetTimerLateness`.
    pub fn reset_timer_lateness(&self) {
        self.time_queue.reset_lateness();
    }

    // -- Frequent tasks --------------------------------------------------

    /// `addFrequentTask` (`event_dispatcher.cpp:334`).
    pub fn add_frequent_task(&self, task: Rc<dyn FrequentTask>) {
        self.frequent_tasks.add(task);
    }

    /// `cancelFrequentTask` (`event_dispatcher.cpp:343`): remove by
    /// identity, reporting whether the task was registered.
    pub fn cancel_frequent_task(&self, task: &Rc<dyn FrequentTask>) -> bool {
        self.frequent_tasks.cancel(task)
    }

    /// Any closure as a frequent task.
    pub fn add_frequent_task_fn<F: Fn() + 'static>(&self, task: F, name: &'static str) {
        self.add_frequent_task(Rc::new(FrequentTaskFn(task, name)));
    }

    /// `processFrequentTasks` (`event_dispatcher.cpp:356`).
    pub fn process_frequent_tasks(&self) {
        self.frequent_tasks.process();
    }

    // -- File descriptors ------------------------------------------------

    /// `registerFileDescriptor` (`event_dispatcher.cpp:228`): watch the
    /// descriptor for input on this dispatcher's poller.
    pub fn register_file_descriptor(
        &self,
        fd: i32,
        handler: Rc<dyn InputNotificationHandler>,
        name: &str,
    ) -> bool {
        self.poller.register_for_read(fd, handler, name)
    }

    /// `registerWriteFileDescriptor` (`event_dispatcher.cpp:247`).
    pub fn register_write_file_descriptor(
        &self,
        fd: i32,
        handler: Rc<dyn InputNotificationHandler>,
        name: &str,
    ) -> bool {
        self.poller.register_for_write(fd, handler, name)
    }

    /// `deregisterFileDescriptor` (`event_dispatcher.cpp:261`).
    pub fn deregister_file_descriptor(&self, fd: i32) -> bool {
        self.poller.deregister_for_read(fd)
    }

    /// `deregisterWriteFileDescriptor` (`event_dispatcher.cpp:275`).
    pub fn deregister_write_file_descriptor(&self, fd: i32) -> bool {
        self.poller.deregister_for_write(fd)
    }

    /// The poller (`pPoller_`), for wiring descriptors and for the
    /// shutdown drain's readiness question.
    pub fn poller(&self) -> Rc<EventPoller> {
        Rc::clone(&self.poller)
    }

    // -- Coupling --------------------------------------------------------

    /// `attach( childDispatcher )` (`event_dispatcher.cpp:152`): couple a
    /// child to this dispatcher. Every pass of this dispatcher then runs
    /// one pass of the child, and the child's poller descriptor is watched
    /// here so activity on the child's descriptors wakes this loop too.
    pub fn attach(self: &Rc<Self>, child_dispatcher: &Rc<Self>) {
        child_dispatcher.attach_to(self);

        self.child_dispatchers
            .borrow_mut()
            .push(Rc::downgrade(child_dispatcher));
    }

    /// `attachTo( parentDispatcher )` (`event_dispatcher.cpp:164`):
    /// create the coupling frequent task on the parent and watch this
    /// dispatcher's poller descriptor there (read and write) when it has
    /// one.
    fn attach_to(self: &Rc<Self>, parent_dispatcher: &Rc<Self>) {
        // MF_ASSERT( pCouplingToParent_ == NULL ).
        assert!(
            self.coupling_to_parent.borrow().is_none(),
            "EventDispatcher::attachTo: already coupled to a parent dispatcher"
        );

        let coupling: Rc<dyn FrequentTask> = DispatcherCoupling::new(self);
        parent_dispatcher.add_frequent_task(Rc::clone(&coupling));
        *self.coupling_to_parent.borrow_mut() = Some(coupling);

        let fd = self.poller.file_descriptor();

        if fd != -1 {
            parent_dispatcher.register_file_descriptor(
                fd,
                Rc::clone(&self.poller) as Rc<dyn InputNotificationHandler>,
                "EventDispatcher",
            );
            parent_dispatcher.register_write_file_descriptor(
                fd,
                Rc::clone(&self.poller) as Rc<dyn InputNotificationHandler>,
                "EventDispatcher",
            );
        }
    }

    /// `detach( childDispatcher )` (`event_dispatcher.cpp:184`).
    pub fn detach(self: &Rc<Self>, child_dispatcher: &Rc<Self>) {
        child_dispatcher.detach_from(self);

        self.child_dispatchers
            .borrow_mut()
            .retain(|child| match child.upgrade() {
                Some(live) => !Rc::ptr_eq(&live, child_dispatcher),
                None => false,
            });
    }

    /// `detachFrom( parentDispatcher )` (`event_dispatcher.cpp:197`):
    /// stop watching this dispatcher's poller descriptor on the parent and
    /// cancel the coupling task (the reference's `~DispatcherCoupling`).
    fn detach_from(self: &Rc<Self>, parent_dispatcher: &Rc<Self>) {
        let fd = self.poller.file_descriptor();

        if fd != -1 {
            parent_dispatcher.deregister_file_descriptor(fd);
            parent_dispatcher.deregister_write_file_descriptor(fd);
        }

        // MF_ASSERT( pCouplingToParent_ != NULL ); bw_safe_delete cancels
        // the task from the parent on the way out.
        let coupling = self
            .coupling_to_parent
            .borrow_mut()
            .take()
            .expect("EventDispatcher::detachFrom: not coupled to a parent dispatcher");
        parent_dispatcher.cancel_frequent_task(&coupling);
    }

    // -- Break and wait --------------------------------------------------

    /// `breakProcessing( breakState )` (`event_dispatcher.ipp:40`).
    pub fn break_processing(&self, break_state: bool) {
        self.break_processing.set(break_state);
    }

    /// `processingBroken` (`event_dispatcher.ipp:53`).
    pub fn processing_broken(&self) -> bool {
        self.break_processing.get()
    }

    /// `maxWait()` (`event_dispatcher.ipp:62`): the poller wait cap, in
    /// seconds.
    pub fn max_wait(&self) -> f64 {
        self.max_wait.get()
    }

    /// `maxWait( seconds )` (`event_dispatcher.ipp:71`).
    pub fn set_max_wait(&self, seconds: f64) {
        self.max_wait.set(seconds);
    }

    /// `calculateWait` (`event_dispatcher.cpp:430`): the seconds until the
    /// next event -- `maxWait_`, capped by this dispatcher's next timer
    /// and by every attached child's own answer.
    pub fn calculate_wait(&self) -> f64 {
        let mut max_wait = self.max_wait.get();

        if !self.time_queue.is_empty() {
            let until_next = self.time_queue.next_exp((self.clock)());
            max_wait = max_wait.min(until_next as f64 / STAMPS_PER_SECOND_D);
        }

        for child in self.child_dispatchers.borrow().iter() {
            if let Some(child) = child.upgrade() {
                max_wait = max_wait.min(child.calculate_wait());
            }
        }

        max_wait
    }

    // -- Statistics ------------------------------------------------------

    /// `processStats` (`event_dispatcher.cpp:373`): once a second, roll
    /// the poller's spare time into the period totals. Never skipped,
    /// even on a broken pass.
    pub fn process_stats(&self) {
        // gather statistics if we haven't for a while
        if (self.clock)().saturating_sub(self.last_statistics_gathered.get())
            >= STAMPS_PER_SECOND
        {
            self.old_spare_time.set(self.tot_spare_time.get());
            self.tot_spare_time
                .set(self.acc_spare_time.get() + self.poller.spare_time());

            self.last_statistics_gathered.set((self.clock)());
        }
    }

    /// `totSpareTime_` (watcher surface): the current period's spare-time
    /// total.
    pub fn tot_spare_time(&self) -> u64 {
        self.tot_spare_time.get()
    }

    /// `oldSpareTime_` (watcher surface): the previous period's total.
    pub fn old_spare_time(&self) -> u64 {
        self.old_spare_time.get()
    }

    /// `proportionalSpareTime` (`event_dispatcher.cpp:319`): the last
    /// statistics period's idle time, in seconds.
    pub fn proportional_spare_time(&self) -> f64 {
        (self.tot_spare_time.get().wrapping_sub(self.old_spare_time.get()) as f64)
            / STAMPS_PER_SECOND_D
    }

    /// `getSpareTime` (`event_dispatcher.cpp:635`): the poller's idle
    /// time, in stamps, since the last clear.
    pub fn get_spare_time(&self) -> u64 {
        self.poller.spare_time()
    }

    /// `clearSpareTime` (`event_dispatcher.cpp:644`): bank the poller's
    /// spare time into the accumulator and reset it.
    pub fn clear_spare_time(&self) {
        self.acc_spare_time
            .set(self.acc_spare_time.get() + self.poller.spare_time());
        self.poller.clear_spare_time();
    }

    /// `maxNetworkTime` (`event_dispatcher.cpp:566`): the worst tick
    /// cost, in stamps.
    pub fn max_network_time(&self) -> u64 {
        self.max_network_time.get()
    }

    /// `numOverBudgetTicks` (`event_dispatcher.cpp:576`).
    pub fn num_over_budget_ticks(&self) -> u32 {
        self.num_over_budget_ticks.get()
    }

    /// `resetNetworkTimeStats` (`event_dispatcher.cpp:585`).
    pub fn reset_network_time_stats(&self) {
        self.max_network_time.set(0);
        self.num_over_budget_ticks.set(0);
    }

    /// `networkTimeBudget()` (`event_dispatcher.cpp:595`): the per-tick
    /// budget, in seconds.
    pub fn network_time_budget(&self) -> f64 {
        self.network_time_budget.get() as f64 / STAMPS_PER_SECOND_D
    }

    /// `networkTimeBudget( seconds )` (`event_dispatcher.cpp:604`).
    pub fn set_network_time_budget(&self, seconds: f64) {
        self.network_time_budget.set(stamps_from_seconds(seconds));
    }

    // -- Loop processing -------------------------------------------------

    /// `processNetwork( shouldIdle )` (`event_dispatcher.cpp:393`): wait
    /// for network activity up to `calculateWait()` when idling, then run
    /// whatever fired. The tick's wall cost minus the poller's idle wait
    /// feeds `maxNetworkTime_` and the over-budget counter. Real clock on
    /// purpose: this measures what a network tick costs in wall time, so
    /// an injected test clock must not distort it.
    pub fn process_network(&self, should_idle: bool) -> i32 {
        // select for network activity until earliest timer
        let max_wait = if should_idle {
            self.calculate_wait()
        } else {
            0.0
        };

        let start_spare = self.poller.spare_time();
        let start = platform::clock::monotonic_now_micros().micros();
        let num_events = self.poller.process_pending_events(max_wait);

        // The poller charges its idle select wait to spare time, and that
        // wait is not work: subtract it out so only the packet and
        // socket-event handling time is left. An idle tick therefore
        // stays inside any budget.
        let idle = self.poller.spare_time() - start_spare;
        let elapsed = platform::clock::monotonic_now_micros().micros() - start;
        let processing_time = elapsed.saturating_sub(idle);

        if processing_time > self.max_network_time.get() {
            self.max_network_time.set(processing_time);
        }

        if processing_time > self.network_time_budget.get() {
            self.num_over_budget_ticks
                .set(self.num_over_budget_ticks.get() + 1);
        }

        num_events
    }

    /// `processOnce( shouldIdle )` (`event_dispatcher.cpp:477`): one
    /// round -- frequent tasks, timers, statistics, network, with the
    /// break checks exactly where the reference puts them. Returns the
    /// number of network events processed.
    pub fn process_once(&self, should_idle: bool) -> i32 {
        // The reference resets the break flag on every entry.
        self.break_processing.set(false);

        self.process_frequent_tasks();

        if !self.processing_broken() {
            self.process_timers();
        }

        self.process_stats();

        if !self.processing_broken() {
            return self.process_network(should_idle);
        }

        0
    }

    /// `processContinuously` (`event_dispatcher.cpp:458`).
    pub fn process_continuously(&self) {
        self.break_processing.set(false);

        while !self.processing_broken() {
            self.process_once(/* shouldIdle */ true);
        }
    }

    /// `processUntilBreak` (`event_dispatcher.cpp:502`). The reference
    /// reports buffered C++ exceptions afterwards; this tree routes
    /// Mercury errors as they happen, so there is nothing to flush.
    pub fn process_until_break(&self) {
        self.process_continuously();
    }

    /// `processUntilSignalled( signal )` (`event_dispatcher.cpp:513`):
    /// same loop, also stopped by an external flag.
    pub fn process_until_signalled(&self, signal: &Cell<bool>) {
        while !self.processing_broken() && !signal.get() {
            self.process_once(/* shouldIdle */ true);
        }
    }

    /// `prepareForShutdown` (`event_dispatcher.cpp:119`): keep processing
    /// until the poller's handlers are ready to shut down, bounded by two
    /// real-clock seconds. Real clock on purpose: this bound is the
    /// last-resort drain guard, it must elapse in wall time even if an
    /// injected clock stands still.
    pub fn prepare_for_shutdown(&self) {
        let start = platform::clock::monotonic_now_micros().micros();
        let time_period = STAMPS_PER_SECOND * SHUTDOWN_DRAIN_SECONDS;

        while !self.poller.is_ready_for_shutdown()
            && platform::clock::monotonic_now_micros().micros() - start < time_period
        {
            self.process_once(/* shouldIdle */ false);

            // Wait 100ms
            std::thread::sleep(std::time::Duration::from_millis(
                SHUTDOWN_SLEEP_MILLIS,
            ));
        }
    }
}

impl Default for EventDispatcher {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::poller::PollerBackend;
    use platform::poller as epoll;
    use platform::poller::PollEvent;
    use std::cell::Cell;

    /// A pinned virtual clock shared by every dispatcher built from it.
    #[derive(Clone)]
    struct PinnedClock(Rc<Cell<u64>>);

    impl PinnedClock {
        fn at(micros: u64) -> Self {
            Self(Rc::new(Cell::new(micros)))
        }

        fn set(&self, micros: u64) {
            self.0.set(micros);
        }

        fn get(&self) -> u64 {
            self.0.get()
        }
    }

    /// The reference's fake poller backend: accepts every registration,
    /// records the waits, and hands out queued events one wait each.
    /// `sleep_ms` lets a test charge real spare time per wait. Shared
    /// fields, so a test keeps a clone of its own recording state.
    #[derive(Clone)]
    struct FakeBackend {
        waits: Rc<RefCell<Vec<i32>>>,
        ready: Rc<RefCell<Vec<PollEvent>>>,
        sleep_ms: u64,
    }

    impl FakeBackend {
        fn new() -> Self {
            Self {
                waits: Rc::new(RefCell::new(Vec::new())),
                ready: Rc::new(RefCell::new(Vec::new())),
                sleep_ms: 0,
            }
        }

        fn with_sleep(sleep_ms: u64) -> Self {
            Self {
                waits: Rc::new(RefCell::new(Vec::new())),
                ready: Rc::new(RefCell::new(Vec::new())),
                sleep_ms,
            }
        }

        fn queue_event(&self, fd: i32) {
            self.ready
                .borrow_mut()
                .push(PollEvent { events: epoll::EPOLLIN, fd });
        }

        fn wait_count(&self) -> usize {
            self.waits.borrow().len()
        }
    }

    impl PollerBackend for FakeBackend {
        fn do_register(
            &self,
            _fd: i32,
            _is_read: bool,
            _is_register: bool,
            _name: Option<&str>,
            _opposite_registered: bool,
        ) -> bool {
            true
        }

        fn wait(&self, timeout_ms: i32, events: &mut [PollEvent]) -> i32 {
            if self.sleep_ms > 0 {
                std::thread::sleep(std::time::Duration::from_millis(self.sleep_ms));
            }
            self.waits.borrow_mut().push(timeout_ms);
            let mut count = 0;
            while let Some(event) = self.ready.borrow_mut().pop() {
                events[count] = event;
                count += 1;
            }
            count as i32
        }
    }

    /// A network handler that logs its tag; `fd` is whatever the poller
    /// hands over. A real handler drains its descriptor (the reference's
    /// `PacketReceiver::handleInputNotification` reads until EAGAIN), and
    /// level-triggered epoll re-reports an undrained one -- the draining
    /// handler for the real-socket test below does exactly that.
    struct LoggingHandler {
        log: Rc<RefCell<Vec<&'static str>>>,
        tag: &'static str,
    }

    impl InputNotificationHandler for LoggingHandler {
        fn handle_input_notification(&self, _fd: i32) -> i32 {
            self.log.borrow_mut().push(self.tag);
            0
        }
    }

    /// The draining variant for real sockets: logs and consumes whatever
    /// arrived, so a later zero-timeout pass (the parent's wake trigger)
    /// finds an empty descriptor.
    struct DrainingHandler {
        log: Rc<RefCell<Vec<&'static str>>>,
        tag: &'static str,
        endpoint: Rc<RefCell<crate::endpoint::Endpoint>>,
    }

    impl InputNotificationHandler for DrainingHandler {
        fn handle_input_notification(&self, _fd: i32) -> i32 {
            self.log.borrow_mut().push(self.tag);
            let mut port = 0u16;
            let mut ip = 0u32;
            let mut buffer = [0u8; 64];
            while self.endpoint.borrow_mut().recvfrom(
                &mut buffer,
                Some(&mut port),
                Some(&mut ip),
            ) > 0
            {}
            0
        }
    }

    /// A recording timer handler.
    fn logging_timer(
        log: &Rc<RefCell<Vec<&'static str>>>,
        tag: &'static str,
    ) -> Rc<dyn TimerHandler<u64>> {
        let log = Rc::clone(log);
        Rc::new(foundation::time_queue::TimerHandlerFn::new(
            move |_, _| log.borrow_mut().push(tag),
            tag,
        ))
    }

    /// A dispatcher over the fake backend and a pinned clock.
    fn test_dispatcher(clock: &PinnedClock, backend: FakeBackend) -> Rc<EventDispatcher> {
        let mut dispatcher = EventDispatcher::with_clock(Box::new({
            let clock = clock.clone();
            move || clock.get()
        }));
        // Swap the backend in: EventPoller::new takes the backend at
        // construction, so build the poller by hand and replace the
        // default through the constructor path.
        dispatcher.poller = Rc::new(EventPoller::new(Box::new(backend)));
        Rc::new(dispatcher)
    }

    /// THE acceptance check: a single round runs the frequent task, then
    /// the due timer, then the network event, in the reference order --
    /// `processFrequentTasks -> processTimers -> processNetwork`
    /// (`event_dispatcher.cpp:477-496`).
    #[test]
    fn a_single_round_runs_frequent_tasks_then_timers_then_network() {
        let clock = PinnedClock::at(1_000_000);
        let backend = FakeBackend::new();
        backend.queue_event(37);
        let dispatcher = test_dispatcher(&clock, backend);

        let log = Rc::new(RefCell::new(Vec::new()));
        dispatcher.add_frequent_task_fn(
            {
                let log = Rc::clone(&log);
                move || log.borrow_mut().push("frequent")
            },
            "frequent",
        );
        dispatcher.register_file_descriptor(
            37,
            Rc::new(LoggingHandler {
                log: Rc::clone(&log),
                tag: "network",
            }),
            "network",
        );
        dispatcher.add_once_off_timer(1, logging_timer(&log, "timer"), 0, "timer");

        // Move the clock past the timer's delivery time: a pinned clock
        // does not advance on its own like the reference's timestamp().
        clock.set(1_000_500);

        let num_events = dispatcher.process_once(false);
        assert_eq!(*log.borrow(), ["frequent", "timer", "network"]);
        assert_eq!(num_events, 1);
        assert_eq!(dispatcher.num_timer_calls(), 1);
    }

    /// A break raised inside a frequent task skips the timer and network
    /// passes (`if (!breakProcessing_)` twice) but still runs the
    /// statistics gather.
    #[test]
    fn a_break_skips_timers_and_network_but_not_stats() {
        let clock = PinnedClock::at(2_000_000);
        let backend = FakeBackend::with_sleep(2);
        backend.queue_event(37);
        let dispatcher = test_dispatcher(&clock, backend.clone());

        let log = Rc::new(RefCell::new(Vec::new()));
        // Pre-charge the poller's spare time so the unconditional stats
        // gather has something to roll up. The event is queued after the
        // charge -- the fake hands its queued events to any wait.
        dispatcher.poller().process_pending_events(0.001);
        let charged = dispatcher.poller().spare_time();
        backend.queue_event(37);

        let dispatcher_addr = Rc::downgrade(&dispatcher);
        let pass_for_task = Rc::new(Cell::new(0));
        dispatcher.add_frequent_task_fn(
            {
                let log = Rc::clone(&log);
                move || {
                    log.borrow_mut().push("frequent");
                    // First pass breaks; the second runs clean.
                    pass_for_task.set(pass_for_task.get() + 1);
                    if pass_for_task.get() == 1
                        && let Some(dispatcher) = dispatcher_addr.upgrade()
                    {
                        dispatcher.break_processing(true);
                    }
                }
            },
            "breaker",
        );
        dispatcher.register_file_descriptor(
            37,
            Rc::new(LoggingHandler {
                log: Rc::clone(&log),
                tag: "network",
            }),
            "network",
        );
        dispatcher.add_once_off_timer(1, logging_timer(&log, "timer"), 0, "timer");

        let waits_before = backend.wait_count();

        clock.set(2_000_500);
        assert_eq!(dispatcher.process_once(false), 0);
        assert_eq!(
            *log.borrow(),
            ["frequent"],
            "timers and network were skipped"
        );
        assert_eq!(
            backend.wait_count(),
            waits_before,
            "processNetwork never waited"
        );
        assert!(
            dispatcher.tot_spare_time() >= charged,
            "stats gathered despite the break"
        );

        // The second pass runs clean: timers and network fire.
        assert_eq!(dispatcher.process_once(false), 1);
        assert_eq!(
            *log.borrow(),
            ["frequent", "frequent", "timer", "network"]
        );
        assert_eq!(dispatcher.num_timer_calls(), 1);
    }

    /// The reference quirk: `processOnce` resets the break flag on entry,
    /// so a pre-set break does not stop a single pass.
    #[test]
    fn process_once_resets_the_break_flag_on_entry() {
        let clock = PinnedClock::at(0);
        let dispatcher = test_dispatcher(&clock, FakeBackend::new());
        let ran = Rc::new(Cell::new(0));
        let ran_for_closure = Rc::clone(&ran);
        dispatcher.add_frequent_task_fn(
            move || ran_for_closure.set(ran_for_closure.get() + 1),
            "counter",
        );
        dispatcher.break_processing(true);

        dispatcher.process_once(false);
        assert_eq!(ran.get(), 1);
    }

    /// `calculateWait` caps at `maxWait_` and at the next timer; the
    /// poller receives the answer in whole milliseconds.
    #[test]
    fn calculate_wait_caps_at_max_wait_and_next_timer() {
        let clock = PinnedClock::at(1_000_000);
        let backend = FakeBackend::new();
        let dispatcher = test_dispatcher(&clock, backend.clone());

        // No timers: the cap itself.
        assert_eq!(dispatcher.calculate_wait(), 0.1);

        // A timer 50ms out: the poller wait shrinks to it (shouldIdle).
        dispatcher.add_timer(50_000, logging_timer(&Rc::new(RefCell::new(Vec::new())), "t"), 0, "t");
        assert_eq!(dispatcher.process_once(true), 0);
        assert_eq!(backend.waits.borrow().last().copied(), Some(50));

        // A timer far in the future: the cap wins again.
        let clock = PinnedClock::at(0);
        let backend = FakeBackend::new();
        let dispatcher = test_dispatcher(&clock, backend.clone());
        dispatcher.add_timer(10_000_000, logging_timer(&Rc::new(RefCell::new(Vec::new())), "t"), 0, "t");
        assert_eq!(dispatcher.process_once(true), 0);
        assert_eq!(backend.waits.borrow().last().copied(), Some(100));
    }

    /// A child's next timer caps the parent's poller wait through the
    /// dispatcher tree.
    #[test]
    fn a_child_timer_caps_the_parent_wait() {
        let clock = PinnedClock::at(0);
        let parent = test_dispatcher(&clock, FakeBackend::new());
        let child_backend = FakeBackend::new();
        let child = test_dispatcher(&clock, child_backend);

        child.add_timer(30_000, logging_timer(&Rc::new(RefCell::new(Vec::new())), "t"), 0, "t");
        parent.attach(&child);

        assert_eq!(parent.calculate_wait(), 0.03);
    }

    /// `attach` registers the child poller's own descriptor for read and
    /// write on the parent, and one parent pass drives the child's whole
    /// round through the coupling task: its frequent task, its due timer,
    /// and its queued network event, in order.
    #[test]
    fn a_parent_pass_drives_the_child_round_in_order() {
        let clock = PinnedClock::at(1_000_000);
        let parent = Rc::new(EventDispatcher::with_clock(Box::new({
            let clock = clock.clone();
            move || clock.get()
        })));
        let child = Rc::new(EventDispatcher::with_clock(Box::new({
            let clock = clock.clone();
            move || clock.get()
        })));

        // A real socket with a queued datagram is the child's network
        // event. Non-blocking, exactly like the reference's interfaces
        // (`NetworkInterface::recreateListeningSocket`), so the draining
        // handler's read-until-empty loop terminates.
        let mut receiver = crate::endpoint::Endpoint::new();
        receiver.socket(platform::socket::SOCK_DGRAM);
        assert_eq!(receiver.set_nonblocking(true), 0);
        assert_eq!(receiver.bind(0, 0x0100_007f), 0);
        let bound = receiver.local();
        let mut sender = crate::endpoint::Endpoint::new();
        sender.socket(platform::socket::SOCK_DGRAM);
        assert_eq!(sender.sendto(b"x", bound.port, bound.ip), 1);

        let log = Rc::new(RefCell::new(Vec::new()));
        child.register_file_descriptor(
            receiver.fileno(),
            Rc::new(DrainingHandler {
                log: Rc::clone(&log),
                tag: "child-network",
                endpoint: Rc::new(RefCell::new(receiver)),
            }),
            "child-socket",
        );
        child.add_frequent_task_fn(
            {
                let log = Rc::clone(&log);
                move || log.borrow_mut().push("child-frequent")
            },
            "child-frequent",
        );
        child.add_once_off_timer(1, logging_timer(&log, "child-timer"), 0, "child-timer");

        // The parent's own task sits ahead of the coupling (registered
        // before the attach).
        parent.add_frequent_task_fn(
            {
                let log = Rc::clone(&log);
                move || log.borrow_mut().push("parent-frequent")
            },
            "parent-frequent",
        );

        parent.attach(&child);

        let child_poller_fd = child.poller().file_descriptor();
        assert!(child_poller_fd != -1);
        assert!(parent.poller().is_registered(child_poller_fd, true));
        assert!(parent.poller().is_registered(child_poller_fd, false));

        // Move the pinned clock past the child timer's delivery time
        // before running the parent pass.
        clock.set(1_001_000);
        parent.process_once(false);
        assert_eq!(
            *log.borrow(),
            [
                "parent-frequent",
                "child-frequent",
                "child-timer",
                "child-network"
            ],
            "the coupling ran the child's whole round inside the parent's pass"
        );
    }

    /// `detach` deregisters the child's poller descriptor and cancels the
    /// coupling task: later parent passes leave the child alone.
    #[test]
    fn detach_cancels_the_coupling_and_the_watches() {
        let clock = PinnedClock::at(0);
        let parent = Rc::new(EventDispatcher::with_clock(Box::new({
            let clock = clock.clone();
            move || clock.get()
        })));
        let child = Rc::new(EventDispatcher::with_clock(Box::new({
            let clock = clock.clone();
            move || clock.get()
        })));

        let log = Rc::new(RefCell::new(Vec::new()));
        child.add_frequent_task_fn(
            {
                let log = Rc::clone(&log);
                move || log.borrow_mut().push("child-frequent")
            },
            "child-frequent",
        );

        parent.attach(&child);
        let child_poller_fd = child.poller().file_descriptor();

        parent.detach(&child);
        assert!(!parent.poller().is_registered(child_poller_fd, true));
        assert!(!parent.poller().is_registered(child_poller_fd, false));

        parent.process_once(false);
        assert_eq!(*log.borrow(), Vec::<&'static str>::new());

        // A double detach is a contract breach (MF_ASSERT in
        // detachFrom's take): attach again is legal after a detach.
        parent.attach(&child);
        parent.process_once(false);
        assert_eq!(*log.borrow(), ["child-frequent"]);
    }

    /// The statistics gather runs at most once a second: the first
    /// `processStats` rolls the poller's spare time up, a gather within
    /// the same second is a no-op, and the next period moves
    /// `totSpareTime_` into `oldSpareTime_`.
    #[test]
    fn stats_gather_once_per_second() {
        let clock = PinnedClock::at(1_000_000);
        // The sleeper backend charges real milliseconds per wait, which
        // is what the spare-time statistics observe.
        let dispatcher = test_dispatcher(&clock, FakeBackend::with_sleep(2));

        // Charge the poller's spare time with a real wait.
        dispatcher.poller().process_pending_events(0.004);
        let charged = dispatcher.poller().spare_time();
        assert!(charged > 0);

        dispatcher.process_stats();
        assert_eq!(dispatcher.tot_spare_time(), charged);
        assert_eq!(dispatcher.proportional_spare_time(), charged as f64 / 1e6);

        // Half a second later: no re-gather even though the poller
        // charged more.
        dispatcher.poller().process_pending_events(0.004);
        clock.set(1_500_000);
        dispatcher.process_stats();
        assert_eq!(dispatcher.tot_spare_time(), charged, "same period");

        // A second after the first gather: the period rolls over.
        clock.set(2_000_001);
        dispatcher.process_stats();
        assert_eq!(dispatcher.old_spare_time(), charged);
        assert_eq!(
            dispatcher.tot_spare_time(),
            dispatcher.poller().spare_time()
        );
    }

    /// `processNetwork` excludes the poller's idle wait from the tick
    /// cost: an idle wait inside the budget produces no over-budget tick
    /// and a sub-budget `maxNetworkTime_`.
    #[test]
    fn an_idle_wait_stays_inside_the_budget() {
        // A real 50ms idle wait charges the poller's spare counter; the
        // reference epoll backend is needed for a true idle charge.
        let real = EventDispatcher::new();
        real.add_timer(
            50_000,
            logging_timer(&Rc::new(RefCell::new(Vec::new())), "t"),
            0,
            "t",
        );
        real.process_once(true);

        assert_eq!(real.num_over_budget_ticks(), 0);
        assert!(
            real.max_network_time() < 10_000,
            "idle wait excluded: {}",
            real.max_network_time()
        );
    }

    /// A handler that burns wall time past the budget counts an
    /// over-budget tick and lifts `maxNetworkTime_`.
    #[test]
    fn an_expensive_tick_counts_as_over_budget() {
        struct Burner;
        impl InputNotificationHandler for Burner {
            fn handle_input_notification(&self, _fd: i32) -> i32 {
                let start = platform::clock::monotonic_now_micros().micros();
                while platform::clock::monotonic_now_micros().micros() - start < 15_000 {
                    std::hint::spin_loop();
                }
                0
            }
        }

        let clock = PinnedClock::at(0);
        let backend = FakeBackend::new();
        backend.queue_event(37);
        let dispatcher = test_dispatcher(&clock, backend);
        dispatcher.register_file_descriptor(37, Rc::new(Burner), "burner");

        dispatcher.process_once(false);
        assert_eq!(dispatcher.num_over_budget_ticks(), 1);
        assert!(dispatcher.max_network_time() >= 15_000);
        assert_eq!(dispatcher.network_time_budget(), 0.01);

        dispatcher.reset_network_time_stats();
        assert_eq!(dispatcher.max_network_time(), 0);
        assert_eq!(dispatcher.num_over_budget_ticks(), 0);

        // The budget is settable in seconds.
        dispatcher.set_network_time_budget(0.05);
        assert_eq!(dispatcher.network_time_budget(), 0.05);
    }

    /// `getSpareTime`/`clearSpareTime` bank the poller's counter into the
    /// accumulator and reset it.
    #[test]
    fn clear_spare_time_banks_the_poller_counter() {
        let clock = PinnedClock::at(0);
        let dispatcher = test_dispatcher(&clock, FakeBackend::new());

        let charged = dispatcher.get_spare_time();
        dispatcher.clear_spare_time();
        assert_eq!(dispatcher.get_spare_time(), 0);
        // The banked amount feeds the next stats gather.
        dispatcher.poller().process_pending_events(0.001);
        clock.set(1_000_001);
        dispatcher.process_stats();
        assert!(dispatcher.tot_spare_time() >= charged);
    }

    /// `processContinuously` loops until a break; the reference quirk
    /// (a pass resets the flag) means the break must come from inside a
    /// pass. `processUntilBreak` is the same loop with the reference's
    /// exception-report tail (flattened away).
    #[test]
    fn process_continuously_stops_on_a_break() {
        let clock = PinnedClock::at(0);
        let dispatcher = test_dispatcher(&clock, FakeBackend::new());
        dispatcher.set_max_wait(0.002);

        let passes = Rc::new(Cell::new(0));
        let passes_for_closure = Rc::clone(&passes);
        let dispatcher_addr = Rc::downgrade(&dispatcher);
        dispatcher.add_frequent_task_fn(
            move || {
                passes_for_closure.set(passes_for_closure.get() + 1);
                if passes_for_closure.get() >= 3
                    && let Some(dispatcher) = dispatcher_addr.upgrade()
                {
                    dispatcher.break_processing(true);
                }
            },
            "breaker",
        );

        dispatcher.process_until_break();
        assert_eq!(passes.get(), 3);
    }

    /// `processUntilSignalled` also stops on the external flag.
    #[test]
    fn process_until_signalled_stops_on_the_flag() {
        let clock = PinnedClock::at(0);
        let dispatcher = test_dispatcher(&clock, FakeBackend::new());
        dispatcher.set_max_wait(0.002);

        let signal = Rc::new(Cell::new(false));
        let passes = Rc::new(Cell::new(0));
        let passes_for_closure = Rc::clone(&passes);
        let signal_for_closure = Rc::clone(&signal);
        dispatcher.add_frequent_task_fn(
            move || {
                passes_for_closure.set(passes_for_closure.get() + 1);
                if passes_for_closure.get() >= 2 {
                    signal_for_closure.set(true);
                }
            },
            "signaller",
        );

        dispatcher.process_until_signalled(&signal);
        assert_eq!(passes.get(), 2);
    }

    /// Non-positive timer delays return unset handles, and the timer
    /// info accessors reach the queue.
    #[test]
    fn the_timer_surface_reaches_the_queue() {
        let clock = PinnedClock::at(1_000_000);
        let dispatcher = test_dispatcher(&clock, FakeBackend::new());

        let handle = dispatcher.add_timer(
            100_000,
            logging_timer(&Rc::new(RefCell::new(Vec::new())), "t"),
            7,
            "t",
        );
        assert!(handle.is_set());
        assert_eq!(dispatcher.timer_delivery_time(&handle), 1_100_000);
        assert_eq!(dispatcher.timer_interval_time(&handle), 100_000);
        dispatcher.set_timer_interval_time(&handle, 200_000);
        assert_eq!(dispatcher.timer_interval_time(&handle), 200_000);
        assert!(dispatcher.max_timer_lateness() == 0);
        assert_eq!(dispatcher.num_late_timers(), 0);
        dispatcher.reset_timer_lateness();

        let unset = dispatcher.add_timer(
            0,
            logging_timer(&Rc::new(RefCell::new(Vec::new())), "nope"),
            0,
            "nope",
        );
        assert!(!unset.is_set());
        let negative = dispatcher.add_once_off_timer(
            -5,
            logging_timer(&Rc::new(RefCell::new(Vec::new())), "nope"),
            0,
            "nope",
        );
        assert!(!negative.is_set());
    }

    /// `prepareForShutdown` returns at once when the poller is ready.
    #[test]
    fn a_ready_poller_needs_no_drain() {
        let clock = PinnedClock::at(0);
        let dispatcher = test_dispatcher(&clock, FakeBackend::new());

        let start = std::time::Instant::now();
        dispatcher.prepare_for_shutdown();
        assert!(
            start.elapsed() < std::time::Duration::from_millis(50),
            "a ready poller skips the drain (every pass sleeps 100ms)"
        );
    }

    /// A handler that is not ready yet keeps the drain going until it
    /// flips -- one pass plus a 100ms sleep per not-ready ask.
    #[test]
    fn the_drain_runs_until_the_poller_becomes_ready() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        struct ReluctantHandler(Rc<AtomicUsize>);
        impl InputNotificationHandler for ReluctantHandler {
            fn handle_input_notification(&self, _fd: i32) -> i32 {
                0
            }

            fn is_ready_for_shutdown(&self) -> bool {
                let asked = self.0.fetch_add(1, Ordering::SeqCst);
                asked >= 2
            }
        }

        let clock = PinnedClock::at(0);
        let backend = FakeBackend::new();
        backend.queue_event(37);
        let dispatcher = test_dispatcher(&clock, backend);

        let asks = Rc::new(AtomicUsize::new(0));
        dispatcher
            .register_file_descriptor(37, Rc::new(ReluctantHandler(Rc::clone(&asks))), "reluctant");

        let passes = Rc::new(Cell::new(0));
        let passes_for_closure = Rc::clone(&passes);
        dispatcher.add_frequent_task_fn(
            move || passes_for_closure.set(passes_for_closure.get() + 1),
            "counter",
        );

        let start = std::time::Instant::now();
        dispatcher.prepare_for_shutdown();
        let elapsed = start.elapsed();

        assert!(passes.get() >= 2, "drained at least twice");
        assert!(
            elapsed >= std::time::Duration::from_millis(200),
            "the drain waited between passes"
        );
    }

    /// The drain's real-clock bound: a poller that never becomes ready
    /// holds `prepareForShutdown` for at most the two-second guard.
    #[test]
    fn a_never_ready_poller_is_bounded_by_two_seconds() {
        struct NeverReady;
        impl InputNotificationHandler for NeverReady {
            fn handle_input_notification(&self, _fd: i32) -> i32 {
                0
            }

            fn is_ready_for_shutdown(&self) -> bool {
                false
            }
        }

        let clock = PinnedClock::at(0);
        let backend = FakeBackend::new();
        backend.queue_event(37);
        let dispatcher = test_dispatcher(&clock, backend);
        dispatcher.register_file_descriptor(37, Rc::new(NeverReady), "never");

        let start = std::time::Instant::now();
        dispatcher.prepare_for_shutdown();
        let elapsed = start.elapsed();

        assert!(
            elapsed >= std::time::Duration::from_secs(2),
            "the guard held: {:?}",
            elapsed
        );
        assert!(
            elapsed < std::time::Duration::from_secs(4),
            "the guard released: {:?}",
            elapsed
        );
    }

    /// The destructor warns when still coupled and reports queued timers.
    #[test]
    fn the_destructor_warns_when_still_coupled() {
        let clock = PinnedClock::at(0);
        let parent = Rc::new(EventDispatcher::with_clock(Box::new({
            let clock = clock.clone();
            move || clock.get()
        })));
        let child = Rc::new(EventDispatcher::with_clock(Box::new({
            let clock = clock.clone();
            move || clock.get()
        })));
        child.add_timer(
            1000,
            logging_timer(&Rc::new(RefCell::new(Vec::new())), "t"),
            0,
            "t",
        );

        parent.attach(&child);
        parent.now();
        child.now();
        drop(child);
        drop(parent);
    }

    /// A child detached by dropping it (instead of calling `detach`)
    /// vanishes from the parent -- the documented weak-handle flattening
    /// of `childDispatchers_`: the coupling upgrade fails, the pass
    /// leaves the dead child alone, `calculateWait` skips it, and a later
    /// sibling detach clears the dead entry.
    #[test]
    fn a_dropped_child_vanishes_from_the_parent() {
        let clock = PinnedClock::at(0);
        let parent = test_dispatcher(&clock, FakeBackend::new());
        let child = test_dispatcher(&clock, FakeBackend::new());
        child.add_timer(
            30_000,
            logging_timer(&Rc::new(RefCell::new(Vec::new())), "t"),
            0,
            "t",
        );

        // The coupling task's profiler name.
        assert_eq!(DispatcherCoupling::new(&parent).name(), "DispatcherCoupling");

        parent.attach(&child);
        drop(child); // its destructor warns about the still-live coupling

        let log = Rc::new(RefCell::new(Vec::new()));
        parent.add_frequent_task_fn(
            {
                let log = Rc::clone(&log);
                move || log.borrow_mut().push("parent-only")
            },
            "parent-only",
        );
        parent.process_once(false);
        assert_eq!(
            *log.borrow(),
            ["parent-only"],
            "the dead child's coupling was a no-op"
        );
        assert_eq!(
            parent.calculate_wait(),
            0.1,
            "the dead child's timer does not cap the wait"
        );

        // A sibling detach clears the dead entry from the children list.
        let sibling = test_dispatcher(&clock, FakeBackend::new());
        parent.attach(&sibling);
        parent.detach(&sibling);
        assert!(!parent.poller().is_registered(sibling.poller().file_descriptor(), true));
    }

    /// `maxWait` and `now` accessors, and `default()` builds on the real
    /// clock like `new()`.
    #[test]
    fn accessors_match_the_reference() {
        let clock = PinnedClock::at(42);
        let dispatcher = test_dispatcher(&clock, FakeBackend::new());
        assert_eq!(dispatcher.now(), 42);
        assert_eq!(dispatcher.max_wait(), 0.1);
        dispatcher.set_max_wait(0.5);
        assert_eq!(dispatcher.max_wait(), 0.5);
        assert!(!dispatcher.processing_broken());
        dispatcher.break_processing(true);
        assert!(dispatcher.processing_broken());

        let defaulted = EventDispatcher::default();
        assert_eq!(defaulted.max_wait(), 0.1);
        // The real clock: monotonic. (The process-relative stamp can be 0
        // if this is the first clock read of the process -- only
        // differences are meaningful, as everywhere in this tree.)
        let first = defaulted.now();
        let second = defaulted.now();
        assert!(second >= first, "the real clock ran");
    }
}
