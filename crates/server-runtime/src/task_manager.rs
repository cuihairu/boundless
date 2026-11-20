//! `lib/cstdmf/bgtask_manager` port: the real `TaskManager` thread pool
//! (BW-01-05-04 -- background phases run on worker threads and their
//! completions are dispatched to the main thread by `tick`).
//!
//! Reference contract preserved:
//! - `addBackgroundTask` pushes into a priority-sorted list -- "Insert
//!   with highest priority value tasks first" -- and one worker ticket
//!   per push; `pullBackgroundTask` waits for a ticket and pops the front,
//!   tracking the task in `activeBgTasks_` until
//!   `onBackgroundTaskFinished`;
//! - `addMainThreadTask` (legal from any thread) appends under
//!   `fgTaskListMutex_`; `tick` (main thread only) swaps the list out and
//!   runs every `doMainThreadTask` on the swapped-out snapshot, so a
//!   completion added mid-tick waits for the next pass; it then flushes
//!   `delayedBgTaskList_` to the *back* of the background queue;
//! - `BackgroundTaskThread::run` loops pull → `doBackgroundTask` →
//!   `onBackgroundTaskFinished`; a NULL (stop sentinel) task ends the
//!   thread, which then "inform[s] the main thread that this thread has
//!   terminated" via a `ThreadFinisher` completion;
//!   `ThreadFinisher::doMainThreadTask` calls `onThreadFinished`, which
//!   decrements `numRunningThreads_`;
//! - `startThreads` raises both `numUnstoppedThreads_` and
//!   `numRunningThreads_`; `stopThreads` lowers the former and pushes that
//!   many stop sentinels;
//! - `stopAll( discardPendingTasks, waitForThreads )`: discard first ("Do
//!   this before stopping threads, because 'stop' tasks are pulled last")
//!   via `clearRealTasks` -- cancel non-sentinel tasks popped from the
//!   *back*, count the sentinels and push them back ("Keep stop (NULL)
//!   tasks or their threads will never be stopped"); then stop all
//!   threads; then the wait loop ticks and calls `stop()` on the active
//!   tasks until `numRunningThreads_` reaches zero; clears again ("in
//!   case addBackgroundTask was called while we were waiting"); and ends
//!   with the final tick ("do a tick to stop all foreground tasks");
//! - `numBgTasksLeft` counts the background queue plus active tasks;
//!   `hasTasks` also includes the foreground list;
//! - `~TaskManager` runs `stopAll()` (discard + wait), mirrored by `Drop`.
//!
//! Flattened away (no bootstrap consumer, recorded for later tasks): the
//! `mgr, pThread` `doBackgroundTask` overload and `BackgroundThreadData`
//! onStart/onEnd (the bootstrap never supplies thread data), watcher
//! instrumentation (`timeEnqueuedTo*`, `TaskWatchers`, `fgTaskListSize_`),
//! the thread-name prefix/index registry, `CStyleBackgroundTask`,
//! `shouldAbortTask`/`setStaticThreadData`/the thread-block callback
//! (Windows/profiler abort machinery), the `warnSize_` list warnings, and
//! `exclusive` (ProcessorAffinity). The reference's `FORCE_MAIN_THREAD`
//! build mode -- background phases run inline on the caller -- is what
//! `crate::shutdown::TaskManager` implements.
//!
//! Like the reference's `volatile bool stop_` contract, tasks are shared
//! across threads without a global lock: `stop()` may be called on a task
//! whose `doBackgroundTask` is running, so task state that both phases
//! touch must be atomic or externally locked (the shared `Mutex` around
//! the whole task would deadlock the `stopAll` wait loop against a
//! running background phase).

use std::collections::VecDeque;
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::Duration;

/// A task that runs in a worker thread (`doBackgroundTask`) and can
/// complete on the main thread (`doMainThreadTask`), exactly like
/// `cstdmf::BackgroundTask`. Tasks "will often add themselves back to the
/// manager at the end of this method by calling
/// `mgr.addMainThreadTask( this )`. This allows this object to complete
/// in the main thread."
pub trait BackgroundTask: Send + Sync {
    /// `doBackgroundTask`: perform the task on a worker thread.
    fn do_background_task(&self, mgr: &TaskManager);

    /// `doMainThreadTask`: the completion phase; runs during `tick` on
    /// the main thread, in enqueue order.
    fn do_main_thread_task(&self, _mgr: &TaskManager) {}

    /// `stop`: "called on a task that is already running and you want it
    /// to stop". Called from the main thread while the background phase
    /// may still be running -- use atomics inside the task.
    fn stop(&self) {}

    /// `cancel`: "called on a task that has not been run yet, but we want
    /// to discard it" (`clearRealTasks` during `stopAll`).
    #[allow(unused_variables)]
    fn cancel(&self, mgr: &TaskManager) {}

    /// `name` (the watcher/profiler label).
    fn name(&self) -> &str;
}

/// `BackgroundTaskPtr`.
pub type TaskPtr = Arc<dyn BackgroundTask>;

/// `TaskManager::BackgroundTaskList`: the priority queue of tasks waiting
/// for a worker, with the semaphore folded into the deque (one push per
/// element, `pull` waits until non-empty -- "This waits on a semaphore
/// for a task to be ready to pull").
struct BackgroundTaskList {
    list: Mutex<VecDeque<(i32, Option<TaskPtr>)>>,
    available: Condvar,
}

impl BackgroundTaskList {
    fn new() -> Self {
        Self {
            list: Mutex::new(VecDeque::new()),
            available: Condvar::new(),
        }
    }

    /// `BackgroundTaskList::push`: "Insert with highest priority value
    /// tasks first" -- walk from the back while the new priority is
    /// strictly greater and insert there, so equal priorities keep their
    /// arrival order. The notify is the semaphore ticket ("Do last
    /// because background tasks wait on the semaphore, not the mutex").
    fn push(&self, task: Option<TaskPtr>, priority: i32) {
        let mut list = self.list.lock().expect("bg task list");
        let mut position = list.len();
        for (index, (queued, _)) in list.iter().enumerate().rev() {
            if priority > *queued {
                position = index;
            } else {
                break;
            }
        }
        list.insert(position, (priority, task));
        drop(list);
        self.available.notify_one();
    }

    /// The `push( const List & )` overload: appended at the end, unsorted
    /// (this is the `delayedBgTaskList_` flush path).
    fn push_list(&self, tasks: Vec<(i32, Option<TaskPtr>)>) {
        if tasks.is_empty() {
            return;
        }
        let count = tasks.len();
        let mut list = self.list.lock().expect("bg task list");
        list.extend(tasks);
        drop(list);
        for _ in 0..count {
            self.available.notify_one();
        }
    }

    /// `pushStopTask`: the NULL task that ends a worker.
    fn push_stop_task(&self) {
        self.push(None, TaskManager::DEFAULT);
    }

    /// `BackgroundTaskList::pull`: "Makes thread wait until there is a
    /// task in the queue for it".
    fn pull(&self) -> Option<TaskPtr> {
        let mut list = self.list.lock().expect("bg task list");
        while list.is_empty() {
            list = self
                .available
                .wait(list)
                .expect("bg task list wait");
        }
        list.pop_front().expect("non-empty").1
    }

    /// `BackgroundTaskList::clearRealTasks`: cancel every real task,
    /// counting the stop sentinels and pushing them back afterwards
    /// ("Stop tasks all have the same priority so we just needed to add
    /// them up"). Popping from the back "because other threads might be
    /// in the process of doing a pull and we want to preserve ordering".
    fn clear_real_tasks(&self, mgr: &TaskManager) {
        let mut num_stop = 0usize;
        {
            let mut list = self.list.lock().expect("bg task list");
            while let Some((_, task)) = list.pop_back() {
                match task {
                    Some(task) => task.cancel(mgr),
                    None => num_stop += 1,
                }
            }
        }
        for _ in 0..num_stop {
            self.push_stop_task();
        }
    }

    fn is_empty(&self) -> bool {
        self.list.lock().expect("bg task list").is_empty()
    }

    fn size(&self) -> usize {
        self.list.lock().expect("bg task list").len()
    }
}

/// `cstdmf::TaskManager`: the background/foreground task lists, the
/// active-task registry and the thread counters. Shared as
/// `Arc<TaskManager>`; every method takes `&self` and locks internally
/// exactly like the reference's per-list mutexes.
pub struct TaskManager {
    /// `bgTaskList_`.
    bg_task_list: BackgroundTaskList,
    /// `delayedBgTaskList_` (main-thread writes, flushed by `tick`).
    delayed_bg_task_list: Mutex<Vec<(i32, TaskPtr)>>,
    /// `fgTaskList_` ("Functions that can be accessed from any thread are
    /// ... functions for adding tasks (addBackgroundTask,
    /// addMainThreadTask)").
    fg_task_list: Mutex<Vec<TaskPtr>>,
    /// `numRunningThreads_`.
    num_running_threads: AtomicI32,
    /// `numUnstoppedThreads_`.
    num_unstopped_threads: AtomicI32,
    /// `activeBgTasks_`: pulled but not yet finished (the `stopAll` wait
    /// loop stops these).
    active_bg_tasks: Mutex<Vec<TaskPtr>>,
}

impl TaskManager {
    /// `enum { MIN, LOW, MEDIUM, DEFAULT, HIGH, MAX }`.
    pub const MIN: i32 = 0;
    pub const LOW: i32 = 32;
    pub const MEDIUM: i32 = 64;
    pub const DEFAULT: i32 = Self::MEDIUM;
    pub const HIGH: i32 = 96;
    pub const MAX: i32 = 128;

    pub fn new() -> Self {
        Self {
            bg_task_list: BackgroundTaskList::new(),
            delayed_bg_task_list: Mutex::new(Vec::new()),
            fg_task_list: Mutex::new(Vec::new()),
            num_running_threads: AtomicI32::new(0),
            num_unstopped_threads: AtomicI32::new(0),
            active_bg_tasks: Mutex::new(Vec::new()),
        }
    }

    /// `addBackgroundTask( pTask, priority = DEFAULT )`.
    pub fn add_background_task(&self, task: TaskPtr) {
        self.add_background_task_with_priority(task, Self::DEFAULT);
    }

    /// The priority overload. `timeEnqueuedToBackground_` is watcher
    /// instrumentation and is flattened away.
    pub fn add_background_task_with_priority(&self, task: TaskPtr, priority: i32) {
        self.bg_task_list.push(Some(task), priority);
    }

    /// `addMainThreadTask`: appends under the foreground mutex; legal
    /// from any thread.
    pub fn add_main_thread_task(&self, task: TaskPtr) {
        self.fg_task_list
            .lock()
            .expect("fg task list")
            .push(task);
    }

    /// `addDelayedBackgroundTask`: parked until the next `tick`, then
    /// flushed to the back of the background queue. The reference asserts
    /// main-thread-only (`MainThreadTracker::isCurrentThreadMain()`); the
    /// bootstrap keeps that a documented caller contract.
    pub fn add_delayed_background_task(&self, task: TaskPtr) {
        self.delayed_bg_task_list
            .lock()
            .expect("delayed bg task list")
            .push((Self::MIN, task));
    }

    /// `pullBackgroundTask` (background threads): wait for a task, then
    /// track it in `activeBgTasks_` until it finishes.
    fn pull_background_task(&self) -> Option<TaskPtr> {
        let task = self.bg_task_list.pull();
        if let Some(task) = &task {
            self.active_bg_tasks
                .lock()
                .expect("active bg tasks")
                .push(Arc::clone(task));
        }
        task
    }

    /// `onBackgroundTaskFinished` (background threads): drop the task
    /// from `activeBgTasks_`.
    fn on_background_task_finished(&self, task: &TaskPtr) {
        let mut active = self.active_bg_tasks.lock().expect("active bg tasks");
        if let Some(position) = active.iter().position(|queued| Arc::ptr_eq(queued, task)) {
            active.remove(position);
        }
    }

    /// `onThreadFinished` (main thread, via `ThreadFinisher`).
    fn on_thread_finished(&self) {
        self.num_running_threads.fetch_sub(1, Ordering::SeqCst);
    }

    /// `startThreads( numThreads )`: "the total number of threads running
    /// is incremented" -- both counters -- and the workers spawn. The
    /// `BackgroundThreadData` onStart/onEnd hook is flattened away.
    pub fn start_threads(self: &Arc<Self>, num_threads: i32) {
        self.num_unstopped_threads
            .fetch_add(num_threads, Ordering::SeqCst);
        self.num_running_threads
            .fetch_add(num_threads, Ordering::SeqCst);
        for _ in 0..num_threads {
            let mgr = Arc::clone(self);
            thread::spawn(move || worker_loop(mgr));
        }
    }

    /// `stopThreads( numThreads )`: lower the unstopped count and push
    /// that many stop sentinels ("Special task indicates that a thread
    /// should shut down").
    pub fn stop_threads(&self, num_threads: i32) {
        self.num_unstopped_threads
            .fetch_sub(num_threads, Ordering::SeqCst);
        for _ in 0..num_threads {
            self.bg_task_list.push_stop_task();
        }
    }

    /// `setNumberOfThreads`: stop the excess, start the missing.
    pub fn set_number_of_threads(self: &Arc<Self>, num_threads: i32) {
        let unstopped = self.num_unstopped_threads();
        if unstopped > num_threads {
            self.stop_threads(unstopped - num_threads);
        } else if unstopped < num_threads {
            self.start_threads(num_threads - unstopped);
        }
    }

    /// `tick` (main thread): swap out the completion list, run every
    /// `doMainThreadTask` on the swapped-out snapshot ("a completion added
    /// mid-tick waits for the next pass"), then flush
    /// `delayedBgTaskList_` ("push( const List & )" -- to the *back*,
    /// unsorted).
    pub fn tick(&self) {
        let snapshot = {
            let mut fg = self.fg_task_list.lock().expect("fg task list");
            std::mem::take(&mut *fg)
        };
        for task in &snapshot {
            task.do_main_thread_task(self);
        }
        // The snapshot drops here -- "Smartpointers to tasks that have
        // finished get deleted here".

        let delayed =
            std::mem::take(&mut *self.delayed_bg_task_list.lock().expect("delayed list"));
        self.bg_task_list
            .push_list(delayed.into_iter().map(|(priority, task)| (priority, Some(task))).collect());
    }

    /// `numBgTasksLeft`: background queue + active tasks.
    pub fn num_bg_tasks_left(&self) -> usize {
        self.bg_task_list.size() + self.active_bg_tasks.lock().expect("active bg tasks").len()
    }

    /// `hasTasks`: background queue, active tasks or foreground list.
    pub fn has_tasks(&self) -> bool {
        !self.bg_task_list.is_empty()
            || !self.active_bg_tasks.lock().expect("active bg tasks").is_empty()
            || !self.fg_task_list.lock().expect("fg task list").is_empty()
    }

    /// `numRunningThreads`.
    pub fn num_running_threads(&self) -> i32 {
        self.num_running_threads.load(Ordering::SeqCst)
    }

    /// `numUnstoppedThreads`.
    pub fn num_unstopped_threads(&self) -> i32 {
        self.num_unstopped_threads.load(Ordering::SeqCst)
    }

    /// `stopAll( discardPendingTasks = true, waitForThreads = true )`.
    pub fn stop_all(&self, discard_pending_tasks: bool, wait_for_threads: bool) {
        // Discard any pending tasks. "Do this before stopping threads,
        // because 'stop' tasks are pulled last."
        if discard_pending_tasks {
            self.bg_task_list.clear_real_tasks(self);
        }

        // Stop threads.
        self.stop_threads(self.num_unstopped_threads());
        assert_eq!(
            self.num_unstopped_threads(),
            0,
            "MF_ASSERT( numUnstoppedThreads_ == 0 )"
        );

        if wait_for_threads && self.num_running_threads() > 0 {
            loop {
                self.tick();

                if self.num_running_threads() > 0 {
                    if discard_pending_tasks {
                        let mut active = self.active_bg_tasks.lock().expect("active bg tasks");
                        while let Some(task) = active.pop() {
                            task.stop();
                        }
                    }
                    thread::sleep(Duration::from_millis(100));
                } else {
                    break;
                }
            }
        }

        // Clear again, "in case addBackgroundTask was called while we
        // were waiting".
        if discard_pending_tasks {
            self.bg_task_list.clear_real_tasks(self);
        }

        // "do a tick to stop all foreground tasks. Note that more
        // foreground tasks could be added while it's stopping the current
        // list."
        self.tick();
    }

    /// `stopAll()` with the reference defaults.
    pub fn stop_all_defaults(&self) {
        self.stop_all(true, true);
    }

    /// The shutdown drain: end the pool but let the queued work finish.
    ///
    /// `stopAll(true, ...)` would cancel every queued task, so the queue
    /// is discarded instead of drained -- hence `discardPendingTasks =
    /// false`, the `waitForThreads = true` half of `stopAll`: the workers
    /// pull their real tasks before the stop sentinels ("stop" tasks are
    /// pulled last) and then exit.
    ///
    /// The reference's `stopAll` ends with one tick -- "do a tick to stop
    /// all foreground tasks. Note that more foreground tasks could be
    /// added while it's stopping the current list" -- so the drain keeps
    /// ticking while anything is still queued: every completion, including
    /// the ones the exiting workers queue on their way out
    /// (`ThreadFinisher`), runs on the calling thread exactly once.
    pub fn drain(&self) {
        self.stop_all(false, true);
        while self.has_tasks() {
            self.tick();
        }
    }
}

impl Default for TaskManager {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for TaskManager {
    /// `~TaskManager()` runs `stopAll()` (discard + wait).
    fn drop(&mut self) {
        self.stop_all(true, true);
    }
}

/// `BackgroundTaskThread::run`: pull, run, finish -- until a stop
/// sentinel; then inform the main thread via a `ThreadFinisher`.
fn worker_loop(mgr: Arc<TaskManager>) {
    loop {
        // This waits until a task is ready to pull. Push a "stop" task to
        // the manager to cancel this thread.
        let task = mgr.pull_background_task();

        match task {
            Some(task) => {
                task.do_background_task(&mgr);
                mgr.on_background_task_finished(&task);
            }
            // A NULL task indicates that the thread should terminate.
            None => break,
        }
    }

    // Inform the main thread that this thread has terminated.
    mgr.add_main_thread_task(Arc::new(ThreadFinisher));
}

/// `ThreadFinisher`: "used to help in the completion on a thread. It
/// informs the manager that the thread has gone."
struct ThreadFinisher;

impl BackgroundTask for ThreadFinisher {
    /// `ThreadFinisher::doBackgroundTask` -- empty in the reference.
    fn do_background_task(&self, _mgr: &TaskManager) {}

    fn do_main_thread_task(&self, mgr: &TaskManager) {
        mgr.on_thread_finished();
    }

    fn name(&self) -> &str {
        "ThreadFinisher"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize};

    /// A completion log shared across threads and phases.
    type Log = Arc<Mutex<Vec<&'static str>>>;

    fn push(log: &Log, entry: &'static str) {
        log.lock().expect("log").push(entry);
    }

    /// Bounded spin-wait so a broken ordering assertion fails instead of
    /// hanging the suite.
    fn wait_until(predicate: impl Fn() -> bool) {
        wait_until_for(5000, predicate);
    }

    fn wait_until_for(max_polls: usize, predicate: impl Fn() -> bool) {
        for _ in 0..max_polls {
            if predicate() {
                return;
            }
            thread::sleep(Duration::from_millis(1));
        }
        panic!("condition not reached");
    }

    /// The timeout guard itself: an unreachable condition fails the wait
    /// (in milliseconds) instead of hanging the suite.
    #[test]
    #[should_panic(expected = "condition not reached")]
    fn wait_until_fails_when_the_condition_stays_false() {
        wait_until_for(2, || false);
    }

    /// A named task that records its phases into the shared log.
    struct NamedTask {
        label: &'static str,
        log: Log,
        remaining: Option<Arc<AtomicUsize>>,
    }

    impl NamedTask {
        fn background(label: &'static str, log: Log, remaining: Arc<AtomicUsize>) -> TaskPtr {
            Arc::new(NamedTask {
                label,
                log,
                remaining: Some(remaining),
            })
        }

        fn foreground(label: &'static str, log: Log) -> TaskPtr {
            Arc::new(NamedTask {
                label,
                log,
                remaining: None,
            })
        }
    }

    impl BackgroundTask for NamedTask {
        fn do_background_task(&self, _mgr: &TaskManager) {
            push(&self.log, self.label);
            if let Some(remaining) = &self.remaining {
                remaining.fetch_sub(1, Ordering::SeqCst);
            }
        }

        fn do_main_thread_task(&self, _mgr: &TaskManager) {
            push(&self.log, self.label);
        }

        fn cancel(&self, _mgr: &TaskManager) {
            push(&self.log, self.label);
        }

        fn name(&self) -> &str {
            self.label
        }
    }

    /// The completion runs on the main thread (the caller of `tick`),
    /// the background phase on a worker, and in that order -- the
    /// acceptance's thread-ID and sequencing assertions.
    #[test]
    fn completions_dispatch_to_the_main_thread_in_order() {
        let log: Log = Arc::new(Mutex::new(Vec::new()));
        let mgr = Arc::new(TaskManager::new());
        mgr.start_threads(1);
        let main_thread = thread::current().id();

        let bg_thread_id = Arc::new(Mutex::new(None));
        let fg_thread_id = Arc::new(Mutex::new(None));
        let done = Arc::new(AtomicBool::new(false));

        struct SelfCompleting {
            log: Log,
            bg_thread_id: Arc<Mutex<Option<thread::ThreadId>>>,
            fg_thread_id: Arc<Mutex<Option<thread::ThreadId>>>,
            done: Arc<AtomicBool>,
            me: Mutex<Option<TaskPtr>>,
        }
        impl BackgroundTask for SelfCompleting {
            fn do_background_task(&self, mgr: &TaskManager) {
                *self.bg_thread_id.lock().expect("bg id") = Some(thread::current().id());
                push(&self.log, "bg");
                let me = self.me.lock().expect("me").clone().expect("self set");
                mgr.add_main_thread_task(me);
            }

            fn do_main_thread_task(&self, _mgr: &TaskManager) {
                *self.fg_thread_id.lock().expect("fg id") = Some(thread::current().id());
                push(&self.log, "fg");
                self.done.store(true, Ordering::SeqCst);
            }

            fn name(&self) -> &str {
                "SelfCompleting"
            }
        }

        let task = Arc::new(SelfCompleting {
            log: Arc::clone(&log),
            bg_thread_id: Arc::clone(&bg_thread_id),
            fg_thread_id: Arc::clone(&fg_thread_id),
            done: Arc::clone(&done),
            me: Mutex::new(None),
        });
        *task.me.lock().expect("me") = Some(Arc::clone(&task) as TaskPtr);

        assert_eq!(task.name(), "SelfCompleting");
        mgr.add_background_task(task);

        // The completion is queued by the background phase; running it
        // (and with it setting `done`) is the tick's job.
        wait_until(|| !log.lock().expect("log").is_empty());
        mgr.tick();
        assert!(done.load(Ordering::SeqCst));

        assert_eq!(*log.lock().expect("log"), ["bg", "fg"]);
        let bg_id = bg_thread_id.lock().expect("bg id").expect("bg ran");
        let fg_id = fg_thread_id.lock().expect("fg id").expect("fg ran");
        assert_ne!(bg_id, main_thread, "the background phase ran off-thread");
        assert_ne!(bg_id, fg_id);
        assert_eq!(fg_id, main_thread, "the completion ran on the main thread");
    }

    /// `push` sorted insert: "Insert with highest priority value tasks
    /// first" -- a single worker pulls MAX before DEFAULT before LOW,
    /// whatever the add order.
    #[test]
    fn higher_priority_tasks_pull_first() {
        let log: Log = Arc::new(Mutex::new(Vec::new()));
        let mgr = Arc::new(TaskManager::new());
        mgr.start_threads(1);
        let remaining = Arc::new(AtomicUsize::new(3));

        let max_task = NamedTask::background("max", Arc::clone(&log), Arc::clone(&remaining));
        let low_task = NamedTask::background("low", Arc::clone(&log), Arc::clone(&remaining));
        let default_task =
            NamedTask::background("default", Arc::clone(&log), Arc::clone(&remaining));

        // Added in this scrambled order on purpose.
        mgr.add_background_task_with_priority(max_task, TaskManager::MAX);
        mgr.add_background_task_with_priority(low_task, TaskManager::LOW);
        mgr.add_background_task_with_priority(default_task, TaskManager::DEFAULT);

        wait_until(|| remaining.load(Ordering::SeqCst) == 0);
        mgr.tick();

        assert_eq!(*log.lock().expect("log"), ["max", "default", "low"]);
    }

    /// Equal priorities keep their arrival order (the sorted insert stops
    /// at the first equal priority).
    #[test]
    fn equal_priorities_keep_arrival_order() {
        let log: Log = Arc::new(Mutex::new(Vec::new()));
        let mgr = Arc::new(TaskManager::new());
        mgr.start_threads(1);
        let remaining = Arc::new(AtomicUsize::new(3));

        for label in ["first", "second", "third"] {
            mgr.add_background_task(NamedTask::background(
                label,
                Arc::clone(&log),
                Arc::clone(&remaining),
            ));
        }

        wait_until(|| remaining.load(Ordering::SeqCst) == 0);
        mgr.tick();

        assert_eq!(*log.lock().expect("log"), ["first", "second", "third"]);
    }

    /// Stop sentinels end the workers and each thread reports its exit
    /// through a `ThreadFinisher` completion, which decrements
    /// `numRunningThreads_`.
    #[test]
    fn stop_sentinels_end_workers_through_thread_finishers() {
        let mgr = Arc::new(TaskManager::new());
        mgr.start_threads(2);
        assert_eq!(mgr.num_running_threads(), 2);
        assert_eq!(mgr.num_unstopped_threads(), 2);

        mgr.stop_threads(2);
        assert_eq!(mgr.num_unstopped_threads(), 0);

        // The exits report through ThreadFinisher completions, which the
        // main thread's ticks run.
        wait_until(|| {
            mgr.tick();
            mgr.num_running_threads() == 0
        });

        assert_eq!(mgr.num_running_threads(), 0);
        assert!(!mgr.has_tasks());
    }

    /// `pull` "makes thread wait until there is a task in the queue for
    /// it": a worker started against an empty queue blocks until the
    /// push wakes it.
    #[test]
    fn a_worker_blocks_on_the_empty_queue_until_a_push_wakes_it() {
        let log: Log = Arc::new(Mutex::new(Vec::new()));
        let mgr = Arc::new(TaskManager::new());
        mgr.start_threads(1);

        // Nothing queued yet: the worker sits in pull's wait.
        mgr.add_background_task(NamedTask::background(
            "woken",
            Arc::clone(&log),
            Arc::new(AtomicUsize::new(1)),
        ));

        wait_until(|| {
            mgr.tick();
            !log.lock().expect("log").is_empty()
        });

        assert_eq!(*log.lock().expect("log"), ["woken"]);
        mgr.stop_all_defaults();
    }

    /// `setNumberOfThreads` stops the excess or starts the missing.
    #[test]
    fn set_number_of_threads_reconciles_the_pool() {
        let mgr = Arc::new(TaskManager::new());
        mgr.set_number_of_threads(1);
        assert_eq!(mgr.num_unstopped_threads(), 1);
        mgr.set_number_of_threads(3);
        assert_eq!(mgr.num_unstopped_threads(), 3);
        mgr.set_number_of_threads(1);
        assert_eq!(mgr.num_unstopped_threads(), 1);
        // The equal case does nothing.
        mgr.set_number_of_threads(1);
        assert_eq!(mgr.num_unstopped_threads(), 1);

        // The two excess threads report their exit via ThreadFinisher.
        wait_until(|| {
            mgr.tick();
            mgr.num_running_threads() == 1
        });
    }

    /// A task that blocks in its background phase until stopped -- the
    /// shape the `stopAll` wait loop exists for.
    struct StoppableTask {
        label: &'static str,
        started: AtomicBool,
        stopped: AtomicBool,
        log: Log,
    }
    impl BackgroundTask for StoppableTask {
        fn do_background_task(&self, _mgr: &TaskManager) {
            self.started.store(true, Ordering::SeqCst);
            while !self.stopped.load(Ordering::SeqCst) {
                thread::sleep(Duration::from_millis(1));
            }
        }
        fn stop(&self) {
            self.stopped.store(true, Ordering::SeqCst);
            push(&self.log, "stopped-long");
        }
        fn cancel(&self, _mgr: &TaskManager) {
            push(&self.log, self.label);
        }
        fn name(&self) -> &str {
            "long"
        }
    }

    fn stoppable_task(log: Log) -> Arc<StoppableTask> {
        Arc::new(StoppableTask {
            label: "cancelled-long",
            started: AtomicBool::new(false),
            stopped: AtomicBool::new(false),
            log,
        })
    }

    /// A task still queued (no worker ever pulled it) is cancelled, not
    /// stopped -- the `clearRealTasks` path.
    #[test]
    fn a_queued_task_is_cancelled_by_stop_all() {
        let log: Log = Arc::new(Mutex::new(Vec::new()));
        let mgr = TaskManager::new();
        let long = stoppable_task(Arc::clone(&log));
        mgr.add_background_task(Arc::clone(&long) as TaskPtr);

        assert_eq!(long.name(), "long");
        mgr.stop_all_defaults();

        assert_eq!(*log.lock().expect("log"), ["cancelled-long"]);
        assert!(!mgr.has_tasks());
    }

    /// `stopAll(true, true)`: pending tasks are cancelled ("clearRealTasks"
    /// popping from the back), the running task is stopped from the wait
    /// loop, the threads end and the manager reports no tasks left.
    #[test]
    fn stop_all_cancels_pending_and_stops_the_running_task() {
        let log: Log = Arc::new(Mutex::new(Vec::new()));
        let mgr = Arc::new(TaskManager::new());
        mgr.start_threads(1);

        let long = stoppable_task(Arc::clone(&log));
        mgr.add_background_task(Arc::clone(&long) as TaskPtr);
        wait_until(|| long.started.load(Ordering::SeqCst));

        // Queued behind the running one: still in the background list
        // when stopAll discards.
        mgr.add_background_task(NamedTask::background(
            "cancelled-pending-1",
            Arc::clone(&log),
            Arc::new(AtomicUsize::new(1)),
        ));
        mgr.add_background_task(NamedTask::background(
            "cancelled-pending-2",
            Arc::clone(&log),
            Arc::new(AtomicUsize::new(1)),
        ));

        mgr.stop_all(true, true);

        // "Pop from the back": pending-2 cancels before pending-1; the
        // running task is stopped afterwards, from the wait loop.
        assert_eq!(
            *log.lock().expect("log"),
            ["cancelled-pending-2", "cancelled-pending-1", "stopped-long"]
        );
        assert_eq!(mgr.num_running_threads(), 0);
        assert_eq!(mgr.num_unstopped_threads(), 0);
        assert!(!mgr.has_tasks());
    }

    /// `stopAll(false, false)`: nothing is discarded or waited on; the
    /// caller keeps the pool and its pending tasks (no threads started,
    /// so the wait loop is skipped and the queue stays intact).
    #[test]
    fn stop_all_without_discard_or_wait_keeps_everything() {
        let log: Log = Arc::new(Mutex::new(Vec::new()));
        let mgr = TaskManager::new();

        mgr.add_background_task(NamedTask::background(
            "kept",
            Arc::clone(&log),
            Arc::new(AtomicUsize::new(1)),
        ));
        mgr.stop_all(false, false);

        assert_eq!(mgr.num_unstopped_threads(), 0);
        assert_eq!(mgr.num_bg_tasks_left(), 1, "the task was not discarded");
        assert!(mgr.has_tasks());
        assert!(log.lock().expect("log").is_empty(), "nothing ran");

        // The manager's Drop runs stopAll() with defaults, which
        // discards the kept task through its cancel path.
    }

    /// A completion added mid-tick waits for the next pass (the swap
    /// semantics of `fgTaskList_` / `newTasks_`).
    #[test]
    fn a_completion_added_mid_tick_waits_for_the_next_pass() {
        let log: Log = Arc::new(Mutex::new(Vec::new()));
        let mgr = TaskManager::new();

        struct ChainedCompletion {
            label: &'static str,
            log: Log,
            next: Mutex<Option<TaskPtr>>,
        }
        impl BackgroundTask for ChainedCompletion {
            fn do_background_task(&self, _mgr: &TaskManager) {}
            fn do_main_thread_task(&self, mgr: &TaskManager) {
                push(&self.log, self.label);
                // Chain the follow-up completion mid-tick: it must wait
                // for the next pass.
                if let Some(next) = self.next.lock().expect("next").clone() {
                    mgr.add_main_thread_task(next);
                }
            }
            fn name(&self) -> &str {
                self.label
            }
        }

        let follower = Arc::new(ChainedCompletion {
            label: "second",
            log: Arc::clone(&log),
            next: Mutex::new(None),
        });
        let leader = Arc::new(ChainedCompletion {
            label: "first",
            log: Arc::clone(&log),
            next: Mutex::new(Some(Arc::clone(&follower) as TaskPtr)),
        });

        assert_eq!(leader.name(), "first");
        assert_eq!(follower.name(), "second");
        // The finisher-like inert background phases are empty by contract.
        leader.do_background_task(&mgr);
        follower.do_background_task(&mgr);

        mgr.add_main_thread_task(leader);
        mgr.tick();
        assert_eq!(*log.lock().expect("log"), ["first"]);

        // The mid-tick addition waits for the next pass.
        mgr.tick();
        assert_eq!(*log.lock().expect("log"), ["first", "second"]);
    }

    /// `addDelayedBackgroundTask` parks until `tick` flushes it to the
    /// back of the background queue -- behind tasks queued earlier --
    /// and does not count in `numBgTasksLeft` while parked.
    #[test]
    fn delayed_tasks_flush_to_the_back_on_tick() {
        let log: Log = Arc::new(Mutex::new(Vec::new()));
        let mgr = Arc::new(TaskManager::new());
        mgr.start_threads(1);

        mgr.add_background_task(NamedTask::background(
            "queued",
            Arc::clone(&log),
            Arc::new(AtomicUsize::new(1)),
        ));
        mgr.add_delayed_background_task(NamedTask::background(
            "delayed",
            Arc::clone(&log),
            Arc::new(AtomicUsize::new(1)),
        ));

        // Parked: only the queued task counts (the reference's
        // numBgTasksLeft sees neither the delayed list nor the fg list).
        assert_eq!(mgr.num_bg_tasks_left(), 1);

        mgr.tick();
        assert_eq!(mgr.num_bg_tasks_left(), 2, "flushed behind the queued task");

        wait_until(|| {
            let log = log.lock().expect("log");
            log.len() >= 2 && log[0] == "queued"
        });
        mgr.tick();
        assert_eq!(*log.lock().expect("log"), ["queued", "delayed"]);
    }

    /// The bookkeeping surface: counters and `hasTasks` track the three
    /// lists through a full lifecycle.
    #[test]
    fn the_bookkeeping_surface_tracks_the_lists() {
        let mgr = TaskManager::new();
        assert!(!mgr.has_tasks());
        assert_eq!(mgr.num_bg_tasks_left(), 0);

        let pending = NamedTask::background("pending", Arc::new(Mutex::new(Vec::new())), Arc::new(AtomicUsize::new(1)));
        mgr.add_background_task(pending);
        assert!(mgr.has_tasks());
        assert_eq!(mgr.num_bg_tasks_left(), 1);

        mgr.add_main_thread_task(NamedTask::foreground("fg", Arc::new(Mutex::new(Vec::new()))));
        assert!(mgr.has_tasks());
        assert_eq!(mgr.num_bg_tasks_left(), 1, "fg tasks do not count");

        // The remaining-None shape runs its background phase without a
        // decrement.
        NamedTask::foreground("fg-phase", Arc::new(Mutex::new(Vec::new())))
            .do_background_task(&mgr);

        // With no threads started, stop_all's wait loop is skipped and
        // the discard drains the queue (cancel path).
        mgr.stop_all_defaults();
        assert!(!mgr.has_tasks());
        assert_eq!(mgr.num_bg_tasks_left(), 0);
        assert_eq!(mgr.num_running_threads(), 0);
    }

    /// Dropping the manager runs `~TaskManager`'s `stopAll()`: a live
    /// worker is stopped and the pool does not leak threads.
    #[test]
    fn dropping_the_manager_stops_the_pool() {
        let mgr = Arc::new(TaskManager::new());
        mgr.start_threads(1);
        drop(mgr);
        // Reaching here without hanging is the assertion: stopAll waited
        // for the worker's ThreadFinisher.
    }

    /// The watcher/profiler labels (`name()`) match the reference for
    /// every task kind in this module; the finisher's background phase is
    /// empty by contract.
    #[test]
    fn task_names_match_the_reference_labels() {
        assert_eq!(Arc::new(ThreadFinisher).name(), "ThreadFinisher");
        // The finisher only ever runs as a foreground task; its
        // background phase is an empty body in the reference too.
        Arc::new(ThreadFinisher).do_background_task(&TaskManager::new());

        let log: Log = Arc::new(Mutex::new(Vec::new()));
        assert_eq!(
            NamedTask::background("labelled", log, Arc::new(AtomicUsize::new(1)))
                .name(),
            "labelled"
        );

        struct Quiet;
        impl BackgroundTask for Quiet {
            fn do_background_task(&self, _mgr: &TaskManager) {}
            fn name(&self) -> &str {
                "SelfCompleting"
            }
        }
        assert_eq!(Quiet.name(), "SelfCompleting");
        Quiet.do_background_task(&TaskManager::new());
    }

    /// The default hooks are inert, exactly like the reference's empty
    /// bodies (`doMainThreadTask`/`stop`/`cancel` defaults).
    #[test]
    fn the_default_hooks_are_inert() {
        let mgr = TaskManager::new();
        struct Quiet;
        impl BackgroundTask for Quiet {
            fn do_background_task(&self, _mgr: &TaskManager) {}
            fn name(&self) -> &str {
                "Quiet"
            }
        }
        let quiet = Quiet;
        assert_eq!(quiet.name(), "Quiet");
        quiet.do_background_task(&mgr);
        quiet.do_main_thread_task(&mgr);
        quiet.stop();
        quiet.cancel(&mgr);
    }

    /// `stopAll` after a manual `stopThreads`: `clearRealTasks` counts
    /// the already-pushed stop sentinels and pushes them back ("Keep
    /// stop (NULL) tasks or their threads will never be stopped"), so
    /// the worker still terminates.
    #[test]
    fn stop_all_after_stop_threads_preserves_the_sentinels() {
        let mgr = Arc::new(TaskManager::new());
        mgr.start_threads(1);
        mgr.stop_threads(1);

        mgr.stop_all(true, true);

        assert_eq!(mgr.num_running_threads(), 0);
        assert_eq!(mgr.num_unstopped_threads(), 0);
        assert!(!mgr.has_tasks());
    }

    /// `TaskManager::default()` constructs the same empty pool state.
    #[test]
    fn default_task_manager_matches_new() {
        let mgr = TaskManager::default();
        assert!(!mgr.has_tasks());
        assert_eq!(mgr.num_bg_tasks_left(), 0);
        assert_eq!(mgr.num_running_threads(), 0);
        assert_eq!(mgr.num_unstopped_threads(), 0);
    }

    /// A task that reports back to the main thread from its background
    /// phase -- the "add themselves back" pattern. Its completion may
    /// reach the foreground list while `stopAll` is already running.
    struct SelfCompletingTask {
        label: &'static str,
        log: Log,
        me: Mutex<Option<TaskPtr>>,
    }

    impl SelfCompletingTask {
        fn new(label: &'static str, log: &Log) -> Arc<Self> {
            let task = Arc::new(Self {
                label,
                log: Arc::clone(log),
                me: Mutex::new(None),
            });
            *task.me.lock().expect("me") = Some(Arc::clone(&task) as TaskPtr);
            task
        }
    }

    impl BackgroundTask for SelfCompletingTask {
        fn do_background_task(&self, mgr: &TaskManager) {
            push(&self.log, self.label);
            let me = self.me.lock().expect("me").clone().expect("self set");
            mgr.add_main_thread_task(me);
        }

        fn do_main_thread_task(&self, _mgr: &TaskManager) {
            push(&self.log, "done");
        }

        fn name(&self) -> &str {
            self.label
        }
    }

    /// The shutdown drain: the queued work finishes rather than being
    /// cancelled, the workers exit, and every completion queued by a
    /// worker runs on the calling thread. Whatever the interleaving
    /// between the workers and the shutdown, the end state is the same.
    #[test]
    fn drain_ends_in_the_same_state_no_matter_the_interleaving() {
        let log: Log = Arc::new(Mutex::new(Vec::new()));
        let mgr = Arc::new(TaskManager::new());
        mgr.start_threads(1);

        // Running: a completion arrives from the worker.
        let worker = SelfCompletingTask::new("worker", &log);
        assert_eq!(worker.name(), "worker");
        mgr.add_background_task(worker);
        // Still queued when the shutdown starts; the drain lets it
        // finish ("stop" tasks are pulled last).
        mgr.add_background_task(NamedTask::background(
            "late-pending",
            Arc::clone(&log),
            Arc::new(AtomicUsize::new(1)),
        ));

        mgr.drain();

        // Both background phases ran in queue order, then the
        // completion -- dispatched on this thread, exactly once.
        assert_eq!(*log.lock().expect("log"), ["worker", "late-pending", "done"]);
        assert_eq!(mgr.num_running_threads(), 0);
        assert_eq!(mgr.num_unstopped_threads(), 0);
        assert_eq!(mgr.num_bg_tasks_left(), 0);
        assert!(!mgr.has_tasks());

        // Draining again changes nothing: the shutdown is repeatable.
        mgr.drain();
        assert_eq!(*log.lock().expect("log"), ["worker", "late-pending", "done"]);
        assert!(!mgr.has_tasks());
    }

    /// `stopAll`'s final tick is one pass; a completion the exiting
    /// workers queue afterwards (the `ThreadFinisher`) would sit queued
    /// into `fini`. The drain keeps ticking until nothing is left, so a
    /// task completing at the same moment the process shuts down is
    /// never dropped.
    #[test]
    fn drain_runs_completions_queued_after_the_first_tick() {
        let log: Log = Arc::new(Mutex::new(Vec::new()));
        let mgr = Arc::new(TaskManager::new());
        mgr.start_threads(1);

        // The finishing worker pushes a completion from off-thread, so
        // the foreground list is non-empty whenever the drain looks.
        let notifier = Arc::new(Notifier {
            log: Arc::clone(&log),
            mgr: Arc::clone(&mgr),
        });
        assert_eq!(notifier.name(), "Notifier");
        // A completion only: its background phase never runs, and its
        // label is carried through for the profiler.
        let completion = Arc::new(ForegroundOnly {
            log: Arc::clone(&log),
        });
        assert_eq!(completion.name(), "ForegroundOnly");
        completion.do_background_task(&mgr);
        mgr.add_background_task(notifier);

        mgr.drain();

        // The worker's completion plus the ThreadFinisher completions
        // all ran on this thread, in order.
        assert_eq!(*log.lock().expect("log"), ["bg", "done"]);
        assert!(!mgr.has_tasks());
        assert_eq!(mgr.num_running_threads(), 0);
    }

    /// A task that notifies the main thread from its background phase.
    struct Notifier {
        log: Log,
        mgr: Arc<TaskManager>,
    }

    impl BackgroundTask for Notifier {
        fn do_background_task(&self, _mgr: &TaskManager) {
            push(&self.log, "bg");
            self.mgr.add_main_thread_task(Arc::new(ForegroundOnly {
                log: Arc::clone(&self.log),
            }));
        }

        fn name(&self) -> &str {
            "Notifier"
        }
    }

    /// A completion only: `doBackgroundTask` never runs for it.
    struct ForegroundOnly {
        log: Log,
    }

    impl BackgroundTask for ForegroundOnly {
        fn do_background_task(&self, _mgr: &TaskManager) {}

        fn do_main_thread_task(&self, _mgr: &TaskManager) {
            push(&self.log, "done");
        }

        fn name(&self) -> &str {
            "ForegroundOnly"
        }
    }

    /// A completion that queues another completion as it runs -- the
    /// "more foreground tasks could be added while it's stopping the
    /// current list" case the reference warns about in `stopAll`.
    struct ChainingCompletion {
        log: Log,
        queued: bool,
    }

    impl BackgroundTask for ChainingCompletion {
        fn do_background_task(&self, _mgr: &TaskManager) {}

        fn do_main_thread_task(&self, mgr: &TaskManager) {
            push(&self.log, "first");
            if !self.queued {
                mgr.add_main_thread_task(Arc::new(ForegroundOnly {
                    log: Arc::clone(&self.log),
                }));
            }
        }

        fn name(&self) -> &str {
            "ChainingCompletion"
        }
    }

    /// A task whose background phase is a no-op: used to spend a worker
    /// so the pool is idle when the shutdown starts.
    struct IdleWorkerTask {
        done: Arc<AtomicBool>,
    }

    impl BackgroundTask for IdleWorkerTask {
        fn do_background_task(&self, _mgr: &TaskManager) {
            self.done.store(true, Ordering::SeqCst);
        }

        fn name(&self) -> &str {
            "IdleWorkerTask"
        }
    }

    /// The completion queued while the drain was already ticking is not
    /// lost: `stopAll`'s tail tick runs the first pass, and the drain
    /// keeps ticking until the queue is really empty -- the state at the
    /// end of the shutdown is the same however the completion arrives.
    #[test]
    fn drain_keeps_ticking_while_completions_arrive() {
        let log: Log = Arc::new(Mutex::new(Vec::new()));
        let mgr = Arc::new(TaskManager::new());
        mgr.start_threads(1);

        // Spend the worker and let it exit, so the pool reports no
        // running threads when the shutdown starts: `stopAll`'s wait
        // loop is skipped and its single tail tick is the only pass.
        let done = Arc::new(AtomicBool::new(false));
        let idle = Arc::new(IdleWorkerTask {
            done: Arc::clone(&done),
        });
        assert_eq!(idle.name(), "IdleWorkerTask");
        // Its background phase is what the worker spends; the completion
        // phase is the trait default.
        mgr.add_background_task(idle);
        wait_until(|| done.load(Ordering::SeqCst));
        mgr.stop_threads(1);
        wait_until(|| {
            mgr.tick();
            mgr.num_running_threads() == 0
        });

        // A completion landing while the drain runs: its own pass
        // chains a second one, which only the drain's loop reaches.
        let head = Arc::new(ChainingCompletion {
            log: Arc::clone(&log),
            queued: false,
        });
        assert_eq!(head.name(), "ChainingCompletion");
        // A completion never has its background phase run for it.
        head.do_background_task(&mgr);
        mgr.add_main_thread_task(head);

        mgr.drain();

        assert_eq!(*log.lock().expect("log"), ["first", "done"]);
        assert!(!mgr.has_tasks());
        assert_eq!(mgr.num_running_threads(), 0);
    }
}
