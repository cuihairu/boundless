//! Staged shutdown: the `ShutDownStage` protocol vocabulary
//! (`lib/server/common.hpp`), the per-app shutdown state that role apps
//! carry, and the `cstdmf::TaskManager` foreground/background task lists
//! (`lib/cstdmf/bgtask_manager.cpp`) in the bootstrap build mode.
//!
//! Reference contracts preserved:
//! - the stage discriminants are the wire values of
//!   `controlledShutDownArgs::stage` (serialized as an int at BW-02);
//! - `shutDownStageToString` prints the exact stage names and falls
//!   through to `"(unknown)"` for any value that is not a stage -- the
//!   wire can carry anything;
//! - shutdown state variables are initialised to `SHUTDOWN_NONE` ("what
//!   shutdown state variables are typically initialised to");
//! - the receiver records the stage, with `SHUTDOWN_INFORM` also
//!   recording the deadline tick; stages the common receiver does not
//!   act on are ignored (the role apps' `default: break` arm);
//! - `TaskManager::tick` drains the main-thread completions in queue
//!   order, and `stopAll` always ends with a final tick so queued
//!   completions still run before the process ends -- the baseline order
//!   the exit pipeline depends on.
//!
//! The background thread pool (`BackgroundTaskThread`, `stopThreads`,
//! `cancel`, `stop`) joins at BW-01-05; until then every
//! `doBackgroundTask` runs inline at enqueue time -- the reference's
//! own `FORCE_MAIN_THREAD` build path (`bgtask_manager.cpp:695`).

/// `GameTime` (`uint32` ticks since app start).
pub type GameTime = u32;

use std::cell::RefCell;
use std::rc::Rc;

/// `ShutDownStage` (`lib/server/common.hpp`). The comments are the
/// reference's own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum ShutDownStage {
    /// "This means 'not shutting down' and is what shutdown state
    /// variables are typically initialised to."
    None = 0,
    /// "This means please start your phase of a controlled shutdown
    /// (which may involve requesting other processes to shutdown)"
    Request = 1,
    /// "This is used to inform a parent process that is waiting for you
    /// to shut down about the stage of the shutdown you're up to."
    Inform = 2,
    /// "This is part of the BaseApp shutdown sequence"
    DisconnectProxies = 3,
    /// "This means please shut down this process (i.e. don't propagate
    /// anything to dependent processes, just shut down now now NOW!)."
    Perform = 4,
    /// "This is only understood by the BaseAppMgr and means please
    /// trigger a controlled shutdown from the most 'senior' process that
    /// currently exists, i.e. try the LoginApp first, then the DBApp,
    /// then the BaseAppMgr."
    Trigger = 5,
}

/// `ServerApp::shutDownStageToString`: the exact reference strings; any
/// value that is not a stage prints `"(unknown)"` (the switch's
/// `default` arm, reachable from wire data).
pub fn shut_down_stage_to_string(value: i32) -> &'static str {
    match value {
        0 => "SHUTDOWN_NONE",
        1 => "SHUTDOWN_REQUEST",
        2 => "SHUTDOWN_INFORM",
        3 => "SHUTDOWN_DISCONNECT_PROXIES",
        4 => "SHUTDOWN_PERFORM",
        5 => "SHUTDOWN_TRIGGER",
        _ => "(unknown)",
    }
}

/// The shutdown state every role app carries (`shutDownStage_` and
/// `shutDownTime_`), initialised to `SHUTDOWN_NONE`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShutdownState {
    stage: ShutDownStage,
    shut_down_time: GameTime,
}

impl Default for ShutdownState {
    fn default() -> Self {
        Self::new()
    }
}

impl ShutdownState {
    pub fn new() -> Self {
        Self {
            stage: ShutDownStage::None,
            shut_down_time: 0,
        }
    }

    /// The recorded stage.
    pub fn stage(&self) -> ShutDownStage {
        self.stage
    }

    /// The deadline tick recorded with `SHUTDOWN_INFORM`.
    pub fn shut_down_time(&self) -> GameTime {
        self.shut_down_time
    }

    /// `isShuttingDown`: any stage other than `SHUTDOWN_NONE`.
    pub fn is_shutting_down(&self) -> bool {
        self.stage != ShutDownStage::None
    }

    /// The arm the role apps' `controlledShutDown` receivers share:
    /// record the stage; `SHUTDOWN_INFORM` also records the deadline
    /// tick (`shutDownTime_ = shutDownTime`). `None`, `DisconnectProxies`
    /// and `Trigger` are ignored (the `default: break` arm). Per-role
    /// behaviour -- acks, script events, the CellApp rejection of
    /// `SHUTDOWN_PERFORM` -- joins with the role apps.
    pub fn handle_controlled_shut_down(&mut self, stage: ShutDownStage, shut_down_time: GameTime) {
        match stage {
            ShutDownStage::Request | ShutDownStage::Perform => {
                self.stage = stage;
            }
            ShutDownStage::Inform => {
                self.stage = stage;
                self.shut_down_time = shut_down_time;
            }
            _ => {}
        }
    }
}

/// `cstdmf::BackgroundTask`. `do_background_task` runs inline (the
/// `FORCE_MAIN_THREAD` branch) until BW-01-05 adds the threads;
/// `do_main_thread_task` is the completion phase, drained per tick.
pub trait BackgroundTask {
    /// `doBackgroundTask`: perform the task. Derived tasks "will often
    /// add themselves back to the manager at the end of this method by
    /// calling `mgr.add_main_thread_task`", letting the task "complete
    /// in the main thread".
    fn do_background_task(&mut self, mgr: &mut TaskManager);

    /// `doMainThreadTask`: the completion phase; runs during `tick` in
    /// enqueue order.
    fn do_main_thread_task(&mut self, _mgr: &mut TaskManager) {}

    /// `name` (the watcher/profiler label).
    fn name(&self) -> &str;
}

/// `cstdmf::TaskManager`, the bootstrap build mode: the foreground list
/// and its per-tick drain exist exactly as in the reference; the
/// background list is always empty because every background phase runs
/// inline (`FORCE_MAIN_THREAD`). The exit pipeline relies on `stopAll`'s
/// final tick: every queued completion runs before the process ends.
pub struct TaskManager {
    /// `fgTaskList_`: completions waiting for the next tick.
    fg_task_list: Vec<Rc<RefCell<dyn BackgroundTask>>>,
    /// `newTasks_`: the swapped-out list tick is currently running.
    new_tasks: Vec<Rc<RefCell<dyn BackgroundTask>>>,
}

impl Default for TaskManager {
    fn default() -> Self {
        Self::new()
    }
}

impl TaskManager {
    pub fn new() -> Self {
        Self {
            fg_task_list: Vec::new(),
            new_tasks: Vec::new(),
        }
    }

    /// `addMainThreadTask`: queue the completion phase for the next
    /// tick. Safe to call from inside a background phase (the reference
    /// queues under `fgTaskListMutex_` while `tick` iterates the
    /// swapped-out list, so mid-tick additions wait for the next tick).
    pub fn add_main_thread_task(&mut self, task: Rc<RefCell<dyn BackgroundTask>>) {
        // timeEnqueuedToMainThread_ -- watcher instrumentation only.
        self.fg_task_list.push(task);
    }

    /// `addBackgroundTask` in the `FORCE_MAIN_THREAD` branch: run the
    /// background phase inline, now. The real thread pool
    /// (`bgTaskList_`, priorities, `pullBackgroundTask`) joins at
    /// BW-01-05.
    pub fn add_background_task(&mut self, task: Rc<RefCell<dyn BackgroundTask>>) {
        task.borrow_mut().do_background_task(self);
    }

    /// `TaskManager::tick`: swap out the completion list, run every
    /// `doMainThreadTask` in queue order, then drop the finished tasks
    /// ("Smartpointers to tasks that have finished get deleted here").
    pub fn tick(&mut self) {
        // fgTaskListMutex_ grab/swap/give -- single-threaded here, the
        // direct swap. `fgTaskList_` ends up empty, so completions added
        // mid-tick wait for the next pass.
        std::mem::swap(&mut self.new_tasks, &mut self.fg_task_list);

        // The reference iterates the swapped-out snapshot; take it out so
        // tasks can reach the manager while it runs.
        let snapshot = std::mem::take(&mut self.new_tasks);
        for task in &snapshot {
            task.borrow_mut().do_main_thread_task(self);
        }
        // The snapshot drops here -- finished tasks are deleted.
        // The delayedBgTaskList_ flush stays empty: background phases
        // never queue in this build mode.
    }

    /// `hasTasks`: any list still holding work. The background lists are
    /// always empty in this build mode; the reference's warning ("you
    /// should also check that you have background threads available")
    /// applies again once BW-01-05 adds them.
    pub fn has_tasks(&self) -> bool {
        !self.fg_task_list.is_empty()
    }

    /// `stopAll( discardPendingTasks, waitForThreads )` at process end.
    /// Both flags govern the threaded machinery that BW-01-05 adds
    /// (`clearRealTasks`, `stopThreads`, the wait loop); the part that
    /// matters for the baseline order already lives in the reference's
    /// tail: "do a tick to stop all foreground tasks" -- every queued
    /// completion runs, in order, before the manager is done.
    pub fn stop_all(&mut self) {
        self.tick();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    /// A completion log shared across task phases.
    type Log = Rc<RefCell<Vec<String>>>;

    fn push(log: &Log, entry: &str) {
        log.borrow_mut().push(entry.to_string());
    }

    /// The documented task pattern: work in the background phase, then
    /// "add themselves back ... by calling mgr.addMainThreadTask" so the
    /// completion runs on the main thread.
    struct CompleteOnMainThread {
        label: &'static str,
        log: Log,
        self_rc: RefCell<Option<Rc<RefCell<CompleteOnMainThread>>>>,
    }

    impl CompleteOnMainThread {
        fn new(label: &'static str, log: &Log) -> Rc<RefCell<Self>> {
            Rc::new(RefCell::new(Self {
                label,
                log: Rc::clone(log),
                self_rc: RefCell::new(None),
            }))
        }
    }

    impl BackgroundTask for CompleteOnMainThread {
        fn do_background_task(&mut self, mgr: &mut TaskManager) {
            push(&self.log, self.label);
            let rc = Rc::clone(self.self_rc.borrow().as_ref().unwrap());
            mgr.add_main_thread_task(rc);
        }

        fn do_main_thread_task(&mut self, _mgr: &mut TaskManager) {
            push(&self.log, "cleanup");
        }

        fn name(&self) -> &str {
            "CompleteOnMainThread"
        }
    }

    // ---- ShutDownStage ----

    /// Every stage string matches the reference switch exactly.
    #[test]
    fn stage_strings_match_the_reference() {
        assert_eq!(
            shut_down_stage_to_string(ShutDownStage::None as i32),
            "SHUTDOWN_NONE"
        );
        assert_eq!(
            shut_down_stage_to_string(ShutDownStage::Request as i32),
            "SHUTDOWN_REQUEST"
        );
        assert_eq!(
            shut_down_stage_to_string(ShutDownStage::Inform as i32),
            "SHUTDOWN_INFORM"
        );
        assert_eq!(
            shut_down_stage_to_string(ShutDownStage::DisconnectProxies as i32),
            "SHUTDOWN_DISCONNECT_PROXIES"
        );
        assert_eq!(
            shut_down_stage_to_string(ShutDownStage::Perform as i32),
            "SHUTDOWN_PERFORM"
        );
        assert_eq!(
            shut_down_stage_to_string(ShutDownStage::Trigger as i32),
            "SHUTDOWN_TRIGGER"
        );
    }

    /// The `default` arm: any non-stage value prints "(unknown)".
    #[test]
    fn unknown_stage_values_print_unknown() {
        assert_eq!(shut_down_stage_to_string(-1), "(unknown)");
        assert_eq!(shut_down_stage_to_string(6), "(unknown)");
        assert_eq!(shut_down_stage_to_string(99), "(unknown)");
    }

    /// The discriminants are the wire values of
    /// `controlledShutDownArgs::stage` (serialized as int at BW-02).
    #[test]
    fn stage_wire_values_match_the_c_enum() {
        assert_eq!(ShutDownStage::None as u32, 0);
        assert_eq!(ShutDownStage::Request as u32, 1);
        assert_eq!(ShutDownStage::Inform as u32, 2);
        assert_eq!(ShutDownStage::DisconnectProxies as u32, 3);
        assert_eq!(ShutDownStage::Perform as u32, 4);
        assert_eq!(ShutDownStage::Trigger as u32, 5);
    }

    // ---- ShutdownState ----

    /// "Shutdown state variables are typically initialised to
    /// SHUTDOWN_NONE."
    #[test]
    fn shutdown_state_starts_not_shutting_down() {
        let state = ShutdownState::new();
        assert_eq!(state.stage(), ShutDownStage::None);
        assert_eq!(state.shut_down_time(), 0);
        assert!(!state.is_shutting_down());

        // Default mirrors new.
        assert_eq!(ShutdownState::default(), state);
    }

    /// SHUTDOWN_INFORM records the stage and the deadline tick; the app
    /// is shutting down from then on.
    #[test]
    fn inform_records_the_stage_and_the_deadline() {
        let mut state = ShutdownState::new();
        state.handle_controlled_shut_down(ShutDownStage::Inform, 600);
        assert_eq!(state.stage(), ShutDownStage::Inform);
        assert_eq!(state.shut_down_time(), 600);
        assert!(state.is_shutting_down());
    }

    /// SHUTDOWN_REQUEST moves the stage without touching the deadline.
    #[test]
    fn request_records_only_the_stage() {
        let mut state = ShutdownState::new();
        state.handle_controlled_shut_down(ShutDownStage::Request, 42);
        assert_eq!(state.stage(), ShutDownStage::Request);
        assert_eq!(state.shut_down_time(), 0);
        assert!(state.is_shutting_down());
    }

    /// SHUTDOWN_PERFORM means "shut down now now NOW!": the stage is
    /// recorded, the deadline untouched.
    #[test]
    fn perform_records_the_immediate_stage() {
        let mut state = ShutdownState::new();
        state.handle_controlled_shut_down(ShutDownStage::Perform, 7);
        assert_eq!(state.stage(), ShutDownStage::Perform);
        assert_eq!(state.shut_down_time(), 0);
        assert!(state.is_shutting_down());
    }

    /// The stages the common receiver does not act on leave the state
    /// alone (the `default: break` arm).
    #[test]
    fn other_stages_are_ignored() {
        let mut state = ShutdownState::new();
        state.handle_controlled_shut_down(ShutDownStage::None, 5);
        state.handle_controlled_shut_down(ShutDownStage::DisconnectProxies, 5);
        state.handle_controlled_shut_down(ShutDownStage::Trigger, 5);
        assert_eq!(state.stage(), ShutDownStage::None);
        assert_eq!(state.shut_down_time(), 0);
        assert!(!state.is_shutting_down());
    }

    // ---- TaskManager ----

    /// `tick` drains the main-thread completions in queue order and
    /// empties the manager. The last task keeps the default
    /// `do_main_thread_task` (a no-op), which the tick still runs.
    #[test]
    fn completions_drain_in_order_on_tick() {
        let log: Log = Rc::new(RefCell::new(Vec::new()));
        let mut mgr = TaskManager::new();

        let mut tasks: Vec<Rc<RefCell<CompleteLabelledTask>>> = Vec::new();
        for label in ["first", "second", "third"] {
            tasks.push(Rc::new(RefCell::new(CompleteLabelledTask {
                label,
                log: Rc::clone(&log),
            })));
        }
        for task in &tasks {
            mgr.add_main_thread_task(Rc::clone(task) as Rc<RefCell<dyn BackgroundTask>>);
        }
        let silent = Rc::new(RefCell::new(SilentTask));
        mgr.add_main_thread_task(Rc::clone(&silent) as Rc<RefCell<dyn BackgroundTask>>);

        // The watcher labels are carried through.
        assert_eq!(tasks[0].borrow().name(), "first");
        assert_eq!(silent.borrow().name(), "SilentTask");
        assert!(mgr.has_tasks());

        // The background phase of a task queued for the main thread still
        // runs when reached directly (the reference's FORCE_MAIN_THREAD
        // tasks do their background work inline at add time).
        silent.borrow_mut().do_background_task(&mut mgr);

        mgr.tick();
        assert_eq!(*log.borrow(), ["first", "second", "third"]);
        assert!(!mgr.has_tasks());

        // The background phase of a foreground-queued task still runs
        // when reached directly (it logs; the scratch manager stays
        // empty).
        let mut scratch = TaskManager::new();
        let mut bg_only = CompleteLabelledTask {
            label: "direct",
            log: Rc::clone(&log),
        };
        bg_only.do_background_task(&mut scratch);
        assert_eq!(*log.borrow(), ["first", "second", "third", "direct"]);
        assert!(!scratch.has_tasks());
    }

    /// A task that only implements the background phase: its
    /// main-thread completion is the trait default (a no-op).
    struct SilentTask;

    impl BackgroundTask for SilentTask {
        fn do_background_task(&mut self, _mgr: &mut TaskManager) {}

        fn name(&self) -> &str {
            "SilentTask"
        }
    }

    struct CompleteLabelledTask {
        label: &'static str,
        log: Log,
    }

    impl BackgroundTask for CompleteLabelledTask {
        fn do_background_task(&mut self, _mgr: &mut TaskManager) {
            push(&self.log, self.label);
        }

        fn do_main_thread_task(&mut self, _mgr: &mut TaskManager) {
            push(&self.log, self.label);
        }

        fn name(&self) -> &str {
            self.label
        }
    }

    /// A background phase runs inline at `addBackgroundTask` time (the
    /// `FORCE_MAIN_THREAD` branch) and its completion waits for the next
    /// tick -- the documented "add themselves back" pattern.
    #[test]
    fn background_phase_runs_inline_and_completes_on_the_next_tick() {
        let log: Log = Rc::new(RefCell::new(Vec::new()));
        let mut mgr = TaskManager::new();

        let task = CompleteOnMainThread::new("bg work", &log);
        task.borrow_mut()
            .self_rc
            .borrow_mut()
            .replace(Rc::clone(&task));

        mgr.add_background_task(Rc::clone(&task) as Rc<RefCell<dyn BackgroundTask>>);

        // The background phase already ran; nothing queued before it.
        assert_eq!(*log.borrow(), ["bg work"]);
        assert!(mgr.has_tasks(), "the completion is queued");

        mgr.tick();
        assert_eq!(*log.borrow(), ["bg work", "cleanup"]);
        assert!(!mgr.has_tasks());

        // The watcher label is carried through.
        assert_eq!(task.borrow().name(), "CompleteOnMainThread");
    }

    /// Completions added while a tick runs wait for the next tick (the
    /// reference iterates the swapped-out `newTasks_`; additions land in
    /// the fresh `fgTaskList_`).
    #[test]
    fn mid_tick_additions_wait_for_the_next_tick() {
        let log: Log = Rc::new(RefCell::new(Vec::new()));
        let mut mgr = TaskManager::new();

        let follower = Rc::new(RefCell::new(ChainedCompletionTask {
            log: Rc::clone(&log),
            chained: false,
        }));
        assert_eq!(follower.borrow().name(), "ChainedCompletionTask");
        // The background phase runs inline before the completion is
        // queued (the reference's addBackgroundTask flattening).
        follower.borrow_mut().do_background_task(&mut mgr);
        mgr.add_main_thread_task(Rc::clone(&follower) as Rc<RefCell<dyn BackgroundTask>>);

        mgr.tick();
        // The first tick ran the task, whose completion chained another
        // one mid-tick.
        assert_eq!(*log.borrow(), ["ran"]);
        assert!(mgr.has_tasks());

        mgr.tick();
        assert_eq!(*log.borrow(), ["ran", "ran"]);
    }

    struct ChainedCompletionTask {
        log: Log,
        chained: bool,
    }

    impl BackgroundTask for ChainedCompletionTask {
        fn do_background_task(&mut self, _mgr: &mut TaskManager) {}

        fn do_main_thread_task(&mut self, mgr: &mut TaskManager) {
            push(&self.log, "ran");
            if !self.chained {
                self.chained = true;
                mgr.add_main_thread_task(Rc::new(RefCell::new(ChainedCompletionTask {
                    log: Rc::clone(&self.log),
                    chained: true,
                })));
            }
        }

        fn name(&self) -> &str {
            "ChainedCompletionTask"
        }
    }

    /// `stopAll`'s tail is the exit-order contract: "do a tick to stop
    /// all foreground tasks" -- completions queued after the last tick
    /// still run, in order, before the manager is done.
    #[test]
    fn stop_all_finishes_the_queued_completions_in_order() {
        let log: Log = Rc::new(RefCell::new(Vec::new()));
        let mut mgr = TaskManager::new();

        let task = CompleteOnMainThread::new("bg work", &log);
        task.borrow_mut()
            .self_rc
            .borrow_mut()
            .replace(Rc::clone(&task));
        mgr.add_background_task(Rc::clone(&task) as Rc<RefCell<dyn BackgroundTask>>);

        // The loop ends without another tick; the shutdown pipeline's
        // stopAll drains the pending completion.
        assert_eq!(*log.borrow(), ["bg work"]);

        mgr.stop_all();
        assert_eq!(*log.borrow(), ["bg work", "cleanup"]);
        assert!(!mgr.has_tasks());

        // Stopping an empty manager stays clean.
        mgr.stop_all();
        assert_eq!(*log.borrow(), ["bg work", "cleanup"]);
    }

    /// `Default` mirrors `new`.
    #[test]
    fn default_task_manager_matches_new() {
        let mgr = TaskManager::default();
        assert!(!mgr.has_tasks());
    }
}
