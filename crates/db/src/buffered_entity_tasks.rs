//! Same-entity background task serialisation
//! (`db_storage_mysql/buffered_entity_tasks.{hpp,cpp}`).
//!
//! The reference runs every entity operation as a background task, and
//! nothing in the thread pool keeps two writes to one entity ordered. This
//! layer is what does: for each entity at most one task is outstanding —
//! the first task to arrive *grabs the lock* and is dispatched; everyone
//! else *buffers*, and finishes in submission order as each predecessor
//! completes (`buffered_entity_tasks.hpp:14-18`).
//!
//! The lock is the reference's NULL-sentinel trick: a per-key queue whose
//! head entry is `NULL` while the task it locks is running
//! (`grabLockT`, `buffered_entity_tasks.cpp:34-48`). Keys come in two
//! maps — [`EntityKey`] once the row has a database id,
//! [`EntityId`]-keyed *before* it does (`priorToDBIDTasks_`) — and a new
//! entity's id assignment replays the pre-id queue onto the post-id map
//! in one step (`onFinishedNewEntity`,
//! `buffered_entity_tasks.cpp:185-224`). Tasks that carry neither id
//! (global work such as the game-time write) always proceed.
//!
//! Completion is the caller's job: [`BufferedEntityTasks::run_task`]
//! plays a dispatched task's background step and then its main-thread
//! chain, whose tail releases the lock and plays the next buffered task
//! (`entity_task.cpp:32-42`). The dispatch target is the
//! [`TaskSink`] seam — the reference's `cstdmf::TaskManager`
//! (`bgtask_manager.hpp:326-329`) with
//! [`TaskQueue`] as the single-threaded stand-in.

use std::cell::RefCell;
use std::collections::{BTreeMap, VecDeque};
use std::rc::Rc;
use std::time::Instant;

use foundation::ids::{DatabaseId, EntityId, EntityTypeId, PENDING_DATABASE_ID};

use crate::storage::EntityKey;

/// `EntityTaskPtr` (`buffered_entity_tasks.hpp:19`).
pub type EntityTaskPtr = Rc<RefCell<dyn EntityTask>>;

/// Per-key FIFO with the NULL lock head: `tasks_` keyed by
/// [`EntityKey`], `priorToDBIDTasks_` keyed by [`EntityId`].
type TaskQueues<K> = BTreeMap<K, VecDeque<Option<EntityTaskPtr>>>;

/// `isValidDBID` (`buffered_entity_tasks.cpp:24-27`): a row id, not the
/// "no id yet" zero and not the "creation in flight" pending sentinel.
fn is_valid_db_id(db_id: DatabaseId) -> bool {
    db_id != 0 && db_id != PENDING_DATABASE_ID
}

/// One unit of entity database work
/// (`tasks/entity_task.hpp:25-48`). The background step is the thread
/// half (`performBackgroundTask`), the main-thread hook is the owner's
/// completion handler half (`performEntityMainThreadTask`); the id
/// accessors are how the buffering layer re-keys a task when the
/// database id lands.
pub trait EntityTask {
    /// The task name, for the buffered-too-long warning.
    fn name(&self) -> &str;
    /// `EntityTask::entityID` (`entity_task.hpp:36`): `0` when the task
    /// has no entity identity.
    fn entity_id(&self) -> EntityId;
    /// `EntityTask::dbID`.
    fn db_id(&self) -> DatabaseId;
    /// `EntityTask::dbID( dbID )`.
    fn set_db_id(&mut self, db_id: DatabaseId);
    /// `entityTypeMapping_.getTypeID()` — half of [`EntityKey`].
    fn type_id(&self) -> EntityTypeId;
    /// `EntityTask::entityKey` (`entity_task.hpp:39-42`): the map key
    /// from the type mapping and the current database id.
    fn entity_key(&self) -> EntityKey {
        EntityKey {
            type_id: self.type_id(),
            db_id: self.db_id(),
        }
    }
    /// `EntityTask::bufferTimestamp`.
    fn buffer_timestamp(&self) -> Option<Instant>;
    /// `EntityTask::bufferTimestamp( t )`.
    fn set_buffer_timestamp(&mut self, timestamp: Option<Instant>);
    /// `MySqlBackgroundTask::performBackgroundTask`: the work itself,
    /// `false` on failure (the reference's `setFailure()`).
    fn perform_background_task(&mut self) -> bool;
    /// `EntityTask::performEntityMainThreadTask`: the owner's completion
    /// handler, run before the lock release.
    fn perform_entity_main_thread_task(&mut self, succeeded: bool);
}

/// The dispatch target: `cstdmf::TaskManager`'s two queues
/// (`bgtask_manager.hpp:326-329`). The reference hands tasks to a thread
/// pool; the port lets the caller drive them in whatever executor the
/// process has.
pub trait TaskSink {
    /// `TaskManager::addBackgroundTask`.
    fn add_background_task(&mut self, task: EntityTaskPtr);
    /// `TaskManager::addDelayedBackgroundTask` — the
    /// `shouldDelayAdds_` arm (`buffered_entity_tasks.cpp:264-267`).
    fn add_delayed_background_task(&mut self, task: EntityTaskPtr);
}

/// A single-threaded [`TaskSink`]: dispatched tasks queue in arrival
/// order and the caller drives them one at a time through
/// [`BufferedEntityTasks::run_task`].
#[derive(Default)]
pub struct TaskQueue {
    background: VecDeque<EntityTaskPtr>,
    delayed: VecDeque<EntityTaskPtr>,
}

impl TaskQueue {
    /// The next dispatched task, in arrival order.
    pub fn pop_next(&mut self) -> Option<EntityTaskPtr> {
        self.background.pop_front()
    }

    /// The next task held back by `shouldDelayAdds`.
    pub fn pop_delayed(&mut self) -> Option<EntityTaskPtr> {
        self.delayed.pop_front()
    }

    /// Dispatched tasks waiting to run.
    pub fn len(&self) -> usize {
        self.background.len()
    }

    /// No dispatched tasks waiting.
    pub fn is_empty(&self) -> bool {
        self.background.is_empty()
    }
}

impl TaskSink for TaskQueue {
    fn add_background_task(&mut self, task: EntityTaskPtr) {
        self.background.push_back(task);
    }

    fn add_delayed_background_task(&mut self, task: EntityTaskPtr) {
        self.delayed.push_back(task);
    }
}

/// `grabLockT` (`buffered_entity_tasks.cpp:34-48`): an empty queue
/// grants the lock by inserting the NULL sentinel; any entries at all
/// refuse.
fn grab_lock_queue(queue: &mut VecDeque<Option<EntityTaskPtr>>) -> bool {
    if !queue.is_empty() {
        return false;
    }
    queue.push_back(None);
    true
}

/// `bufferT` (`buffered_entity_tasks.cpp:54-68`): the sentinel is the
/// head and the task joins at the end of the key's range.
fn buffer_queue(queue: &mut VecDeque<Option<EntityTaskPtr>>, task: EntityTaskPtr) {
    debug_assert!(!queue.is_empty());
    debug_assert!(queue.front().unwrap().is_none());
    queue.push_back(Some(task));
}

/// `playNextTask` (`buffered_entity_tasks.cpp:277-314`) on one queue.
/// The caller only releases a lock it holds, so the sentinel head is an
/// invariant here — the reference also bails when the head is a real
/// task, but that path is unreachable in the port because `onFinished`
/// is only reached by a task that dispatched through a lock. Pop the
/// lock and promote the next entry to the lock, handing it back for
/// dispatch — the released lock may well be the queue's only entry, in
/// which case nothing is promoted. A promoted still-pending id resets
/// to zero (`buffered_entity_tasks.cpp:301-305`).
fn play_next_queue(queue: &mut VecDeque<Option<EntityTaskPtr>>) -> Option<EntityTaskPtr> {
    debug_assert!(!queue.is_empty());
    debug_assert!(queue.front().unwrap().is_none());
    queue.pop_front();
    let mut next = queue.front_mut().and_then(|head| head.take());
    if let Some(task) = next.as_ref() {
        if task.borrow().db_id() == PENDING_DATABASE_ID {
            task.borrow_mut().set_db_id(0);
        }
    }
    next
}

/// Release the lock held under `id`: `Some(promoted)` — possibly no
/// follower — when the lock was released, `None` when that map does not
/// hold it (the reference's key-lookup miss).
fn play_next_in<K: Ord>(queues: &mut TaskQueues<K>, id: &K) -> Option<Option<EntityTaskPtr>> {
    let queue = queues.get_mut(id)?;
    let next = play_next_queue(queue);
    if queue.is_empty() {
        queues.remove(id);
    }
    Some(next)
}

/// Serialises entity tasks so one entity has at most one outstanding
/// database operation (`BufferedEntityTasks`,
/// `buffered_entity_tasks.hpp:22-48`).
pub struct BufferedEntityTasks<S: TaskSink> {
    sink: S,
    /// `tasks_`: keyed by [`EntityKey`] once the row's database id is
    /// known. Queue head `None` = the lock.
    tasks: TaskQueues<EntityKey>,
    /// `priorToDBIDTasks_`: keyed by [`EntityId`] before the row has a
    /// database id.
    prior_to_dbid_tasks: TaskQueues<EntityId>,
    /// `newEntityMap_`: entities whose creation assigned a database id,
    /// consulted by pre-id tasks on the way in.
    new_entity_map: BTreeMap<EntityId, DatabaseId>,
    /// `shouldDelayAdds_`.
    should_delay_adds: bool,
}

impl<S: TaskSink> BufferedEntityTasks<S> {
    /// The constructor (`buffered_entity_tasks.cpp:96-102`).
    pub fn new(sink: S) -> Self {
        Self {
            sink,
            tasks: BTreeMap::new(),
            prior_to_dbid_tasks: BTreeMap::new(),
            new_entity_map: BTreeMap::new(),
            should_delay_adds: false,
        }
    }

    /// `shouldDelayAdds()` (`buffered_entity_tasks.hpp:32-34`).
    pub fn should_delay_adds(&self) -> bool {
        self.should_delay_adds
    }

    /// `shouldDelayAdds( bool )` (`buffered_entity_tasks.hpp:36-39`).
    pub fn set_should_delay_adds(&mut self, should_delay_adds: bool) {
        self.should_delay_adds = should_delay_adds;
    }

    /// `size()` (`buffered_entity_tasks.hpp:41`): entries in the
    /// post-id map — the running lock and everything buffered behind
    /// it. Pre-id tasks are not counted.
    pub fn size(&self) -> usize {
        self.tasks.values().map(|queue| queue.len()).sum()
    }

    /// `addBackgroundTask` (`buffered_entity_tasks.cpp:137-146`): grab
    /// the entity's lock and dispatch, or buffer behind whoever holds
    /// it.
    pub fn add_background_task(&mut self, task: EntityTaskPtr) {
        if self.grab_lock(&task) {
            self.do_task(task);
        } else {
            self.buffer(task);
        }
    }

    /// `grabLock` (`buffered_entity_tasks.cpp:104-133`): resolve the
    /// task's id — a pending database id picks up the assigned id from
    /// [`Self::new_entity_map`], an assigned one retires the entry —
    /// then lock the map the task now belongs to. A task with neither
    /// id always proceeds.
    fn grab_lock(&mut self, task: &EntityTaskPtr) -> bool {
        let entity_id = task.borrow().entity_id();
        let mut db_id = task.borrow().db_id();

        if db_id == PENDING_DATABASE_ID {
            if let Some(assigned) = self.new_entity_map.get(&entity_id) {
                db_id = *assigned;
                task.borrow_mut().set_db_id(db_id);
            }
        } else if db_id != 0 {
            self.new_entity_map.remove(&entity_id);
        }

        if is_valid_db_id(db_id) {
            let key = task.borrow().entity_key();
            grab_lock_queue(self.tasks.entry(key).or_default())
        } else if entity_id != 0 {
            grab_lock_queue(self.prior_to_dbid_tasks.entry(entity_id).or_default())
        } else {
            // Always allow a task with no databaseID or entityID to
            // proceed (`buffered_entity_tasks.cpp:130-132`).
            true
        }
    }

    /// `buffer` (`buffered_entity_tasks.cpp:148-168`): stamp the wait
    /// and join the end of the key's queue.
    fn buffer(&mut self, task: EntityTaskPtr) {
        task.borrow_mut().set_buffer_timestamp(Some(Instant::now()));
        let (entity_id, db_id, key) = {
            let t = task.borrow();
            (t.entity_id(), t.db_id(), t.entity_key())
        };
        if is_valid_db_id(db_id) {
            buffer_queue(self.tasks.entry(key).or_default(), task);
        } else {
            buffer_queue(self.prior_to_dbid_tasks.entry(entity_id).or_default(), task);
        }
    }

    /// `doTask` (`buffered_entity_tasks.cpp:240-270`): warn about
    /// long-buffered work and hand the task to the sink (immediately,
    /// or to the delayed queue when `shouldDelayAdds` is set). The
    /// reference also wires its raw-pointer backref here — the port's
    /// completion path is [`Self::run_task`] instead.
    fn do_task(&mut self, task: EntityTaskPtr) {
        let (name, db_id, type_id) = {
            let t = task.borrow();
            (t.name().to_string(), t.db_id(), t.type_id())
        };
        let seconds = buffered_seconds(&task);
        if seconds > 1.0 {
            eprintln!(
                "WARNING: {name} for dbID {db_id}, type {type_id} was buffered for {seconds:.2} seconds"
            );
        }

        if self.should_delay_adds {
            self.sink.add_delayed_background_task(task);
        } else {
            self.sink.add_background_task(task);
        }
    }

    /// `onFinished` (`buffered_entity_tasks.cpp:170-183`): release this
    /// task's lock and play the next buffered task. A post-id task
    /// whose write just assigned the row's database id first replays
    /// the pre-id queue (`onFinishedNewEntity`); a failed write with no
    /// database id plays the next pre-id task anyway — it is going to
    /// fail too, but the queue must drain
    /// (`buffered_entity_tasks.cpp:179-182`).
    pub fn on_finished(&mut self, task: &EntityTaskPtr) {
        let (key, entity_id, db_id) = {
            let t = task.borrow();
            (t.entity_key(), t.entity_id(), t.db_id())
        };

        if !self.play_next_post_id(&key) {
            if db_id != 0 {
                self.on_finished_new_entity(entity_id, db_id);
            } else {
                self.play_next_pre_id(entity_id);
            }
        }
    }

    /// `onFinishedNewEntity` (`buffered_entity_tasks.cpp:185-224`):
    /// record the assigned id, then replay the entity's pre-id queue —
    /// the first replayed task grabs the new key's lock and runs, the
    /// rest re-buffer behind it with the id already filled in.
    fn on_finished_new_entity(&mut self, entity_id: EntityId, db_id: DatabaseId) {
        self.new_entity_map.insert(entity_id, db_id);

        if entity_id == 0 {
            return;
        }

        let mut replayed = Vec::new();
        if let Some(queue) = self.prior_to_dbid_tasks.get_mut(&entity_id) {
            debug_assert!(!queue.is_empty());
            debug_assert!(queue.front().unwrap().is_none());
            // The sentinel head first, then everything behind it in
            // submission order.
            while let Some(entry) = queue.pop_front() {
                if let Some(t) = entry {
                    replayed.push(t);
                }
            }
        }
        self.prior_to_dbid_tasks.remove(&entity_id);

        let mut task_to_play: Option<EntityTaskPtr> = None;
        for task in replayed {
            task.borrow_mut().set_db_id(db_id);
            let key = task.borrow().entity_key();
            if grab_lock_queue(self.tasks.entry(key).or_default()) {
                debug_assert!(task_to_play.is_none());
                task_to_play = Some(task);
            } else {
                debug_assert!(task_to_play.is_some());
                self.buffer(task);
            }
        }

        if let Some(task) = task_to_play {
            self.do_task(task);
        }
    }

    /// `playNextTask` on the post-id map
    /// (`buffered_entity_tasks.cpp:277-314`): the reference looks the
    /// finished task's key up in `tasks_` and misses when the lock was
    /// held in the pre-id map instead.
    fn play_next_post_id(&mut self, key: &EntityKey) -> bool {
        match play_next_in(&mut self.tasks, key) {
            Some(Some(next)) => {
                self.do_task(next);
                true
            }
            Some(None) => true,
            None => false,
        }
    }

    /// `playNextTask` on the pre-id map.
    fn play_next_pre_id(&mut self, entity_id: EntityId) -> bool {
        match play_next_in(&mut self.prior_to_dbid_tasks, &entity_id) {
            Some(Some(next)) => {
                self.do_task(next);
                true
            }
            Some(None) => true,
            None => false,
        }
    }

    /// The dispatch queue, for the caller's executor loop.
    pub fn sink_mut(&mut self) -> &mut S {
        &mut self.sink
    }

    /// Drive one dispatched task: the background step, then the
    /// main-thread chain whose tail releases the lock
    /// (`entity_task.cpp:32-42`: `performMainThreadTask` runs the
    /// entity hook and notifies `BufferedEntityTasks`; the port makes
    /// that notification this explicit call instead of the reference's
    /// raw-pointer backref, which only tasks dispatched through
    /// [`Self::do_task`] ever carried).
    pub fn run_task(&mut self, task: &EntityTaskPtr) {
        let succeeded = task.borrow_mut().perform_background_task();
        task.borrow_mut().perform_entity_main_thread_task(succeeded);
        self.on_finished(task);
    }
}

/// `doTask`'s wait measure
/// (`buffered_entity_tasks.cpp:249-252`): zero from dispatch, else the
/// seconds since the buffer stamp.
fn buffered_seconds(task: &EntityTaskPtr) -> f64 {
    match task.borrow().buffer_timestamp() {
        None => 0.0,
        Some(stamp) => stamp.elapsed().as_secs_f64(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A task double that records its background step and completion
    /// into a shared order log, and can fail or assign its database id
    /// on the way through (the put's assigned-id completion).
    struct TestTask {
        label: u32,
        type_id: EntityTypeId,
        db_id: DatabaseId,
        entity_id: EntityId,
        fail: bool,
        assign_db_id_on_background: Option<DatabaseId>,
        order: Rc<RefCell<Vec<String>>>,
        buffer_timestamp: Option<Instant>,
    }

    impl TestTask {
        fn new(label: u32, type_id: EntityTypeId, db_id: DatabaseId, entity_id: EntityId) -> Self {
            Self {
                label,
                type_id,
                db_id,
                entity_id,
                fail: false,
                assign_db_id_on_background: None,
                order: Rc::new(RefCell::new(Vec::new())),
                buffer_timestamp: None,
            }
        }

        fn sharing_order(mut self, order: &Rc<RefCell<Vec<String>>>) -> Self {
            self.order = Rc::clone(order);
            self
        }

        fn failing(mut self) -> Self {
            self.fail = true;
            self
        }

        fn assigning(mut self, db_id: DatabaseId) -> Self {
            self.assign_db_id_on_background = Some(db_id);
            self
        }
    }

    impl EntityTask for TestTask {
        fn name(&self) -> &str {
            "TestTask"
        }

        fn entity_id(&self) -> EntityId {
            self.entity_id
        }

        fn db_id(&self) -> DatabaseId {
            self.db_id
        }

        fn set_db_id(&mut self, db_id: DatabaseId) {
            self.db_id = db_id;
        }

        fn type_id(&self) -> EntityTypeId {
            self.type_id
        }

        fn buffer_timestamp(&self) -> Option<Instant> {
            self.buffer_timestamp
        }

        fn set_buffer_timestamp(&mut self, timestamp: Option<Instant>) {
            self.buffer_timestamp = timestamp;
        }

        fn perform_background_task(&mut self) -> bool {
            self.order.borrow_mut().push(format!("bg{}", self.label));
            if let Some(db_id) = self.assign_db_id_on_background {
                self.db_id = db_id;
            }
            !self.fail
        }

        fn perform_entity_main_thread_task(&mut self, succeeded: bool) {
            self.order
                .borrow_mut()
                .push(format!("main{}:{}", self.label, succeeded));
        }
    }

    fn task(
        label: u32,
        type_id: EntityTypeId,
        db_id: DatabaseId,
        entity_id: EntityId,
        order: &Rc<RefCell<Vec<String>>>,
    ) -> EntityTaskPtr {
        Rc::new(RefCell::new(
            TestTask::new(label, type_id, db_id, entity_id).sharing_order(order),
        ))
    }

    fn failing_task(
        label: u32,
        type_id: EntityTypeId,
        db_id: DatabaseId,
        entity_id: EntityId,
        order: &Rc<RefCell<Vec<String>>>,
    ) -> EntityTaskPtr {
        Rc::new(RefCell::new(
            TestTask::new(label, type_id, db_id, entity_id)
                .sharing_order(order)
                .failing(),
        ))
    }

    fn assigning_task(
        label: u32,
        type_id: EntityTypeId,
        db_id: DatabaseId,
        entity_id: EntityId,
        assigned: DatabaseId,
        order: &Rc<RefCell<Vec<String>>>,
    ) -> EntityTaskPtr {
        Rc::new(RefCell::new(
            TestTask::new(label, type_id, db_id, entity_id)
                .sharing_order(order)
                .assigning(assigned),
        ))
    }

    /// Acceptance: concurrent requests for one entity apply in
    /// submission order — the first grabs the lock, the rest buffer and
    /// finish FIFO (`buffered_entity_tasks.hpp:14-18`).
    #[test]
    fn concurrent_same_entity_tasks_apply_in_submission_order() {
        let order = Rc::new(RefCell::new(Vec::new()));
        let mut buffered = BufferedEntityTasks::new(TaskQueue::default());

        // Three writes to the same row arrive back to back.
        let t1 = task(1, 5, 42, 7, &order);
        let t2 = task(2, 5, 42, 7, &order);
        let t3 = task(3, 5, 42, 7, &order);
        buffered.add_background_task(Rc::clone(&t1));
        buffered.add_background_task(Rc::clone(&t2));
        buffered.add_background_task(Rc::clone(&t3));

        // Only the first is outstanding; the other two are buffered
        // behind its lock (`size()` counts the post-id entries).
        assert_eq!(buffered.size(), 3);
        assert_eq!(buffered.sink_mut().len(), 1);

        // Drain: each completion releases the lock to the next task.
        while let Some(next) = buffered.sink_mut().pop_next() {
            buffered.run_task(&next);
        }

        assert_eq!(
            *order.borrow(),
            [
                "bg1",
                "main1:true",
                "bg2",
                "main2:true",
                "bg3",
                "main3:true"
            ]
        );
        assert_eq!(buffered.size(), 0);
        assert!(buffered.sink_mut().is_empty());
    }

    /// Different entities hold independent locks — both dispatch
    /// immediately (`grabLockT` refuses only the busy key).
    #[test]
    fn different_entities_dispatch_immediately() {
        let order = Rc::new(RefCell::new(Vec::new()));
        let mut buffered = BufferedEntityTasks::new(TaskQueue::default());

        buffered.add_background_task(task(1, 5, 42, 7, &order));
        buffered.add_background_task(task(2, 5, 43, 8, &order));
        buffered.add_background_task(task(3, 6, 42, 7, &order)); // other type, same dbID

        assert_eq!(buffered.sink_mut().len(), 3);
        assert_eq!(buffered.size(), 3);
    }

    /// A pre-id task is keyed by entity id (`priorToDBIDTasks_`): it
    /// dispatches when the entity has no row id yet, and `size()`
    /// counts only the post-id map.
    #[test]
    fn pre_dbid_tasks_key_on_the_entity_id() {
        let order = Rc::new(RefCell::new(Vec::new()));
        let mut buffered = BufferedEntityTasks::new(TaskQueue::default());

        let t1 = task(1, 5, PENDING_DATABASE_ID, 9, &order);
        buffered.add_background_task(Rc::clone(&t1));

        assert_eq!(buffered.sink_mut().len(), 1);
        assert_eq!(
            buffered.size(),
            0,
            "pre-id tasks are not in the post-id map"
        );

        // Its completion drains through the pre-id map.
        let dispatched = buffered.sink_mut().pop_next().unwrap();
        assert!(Rc::ptr_eq(&dispatched, &t1));
        buffered.run_task(&dispatched);
        assert_eq!(*order.borrow(), ["bg1", "main1:true"]);
    }

    /// Acceptance: a new entity's id assignment replays the pre-id
    /// queue — the buffered tasks re-key onto the assigned database id
    /// and run in order (`onFinishedNewEntity`,
    /// `buffered_entity_tasks.cpp:185-224`).
    #[test]
    fn new_entity_id_assignment_replays_pre_dbid_tasks_in_order() {
        let order = Rc::new(RefCell::new(Vec::new()));
        let mut buffered = BufferedEntityTasks::new(TaskQueue::default());

        // Creation in flight (pending id), then two followers arrive.
        let t1 = assigning_task(1, 5, PENDING_DATABASE_ID, 7, 42, &order);
        let t2 = task(2, 5, PENDING_DATABASE_ID, 7, &order);
        let t3 = task(3, 5, PENDING_DATABASE_ID, 7, &order);
        buffered.add_background_task(Rc::clone(&t1));
        buffered.add_background_task(Rc::clone(&t2));
        buffered.add_background_task(Rc::clone(&t3));

        // The put assigns database id 42 in the background step; its
        // completion replays the pre-id queue: t2 dispatched under the
        // assigned id, t3 re-buffered behind it.
        let dispatched = buffered.sink_mut().pop_next().unwrap();
        assert!(Rc::ptr_eq(&dispatched, &t1));
        buffered.run_task(&dispatched);

        assert_eq!(*order.borrow(), ["bg1", "main1:true"]);
        assert_eq!(buffered.sink_mut().len(), 1);
        assert_eq!(t2.borrow().db_id(), 42);

        let next = buffered.sink_mut().pop_next().unwrap();
        assert!(Rc::ptr_eq(&next, &t2));
        buffered.run_task(&next);

        // t2's completion plays t3 through the post-id map.
        assert_eq!(t3.borrow().db_id(), 42);
        let next = buffered.sink_mut().pop_next().unwrap();
        assert!(Rc::ptr_eq(&next, &t3));
        buffered.run_task(&next);
        assert_eq!(
            *order.borrow(),
            [
                "bg1",
                "main1:true",
                "bg2",
                "main2:true",
                "bg3",
                "main3:true"
            ]
        );
        assert_eq!(buffered.size(), 0);
    }

    /// A pending task arriving after the creation resolves its id from
    /// `newEntityMap_` and locks the post-id map
    /// (`grabLock`, `buffered_entity_tasks.cpp:110-118`).
    #[test]
    fn pending_task_resolves_the_assigned_id_from_the_new_entity_map() {
        let order = Rc::new(RefCell::new(Vec::new()));
        let mut buffered = BufferedEntityTasks::new(TaskQueue::default());

        let create = assigning_task(1, 5, PENDING_DATABASE_ID, 7, 42, &order);
        buffered.add_background_task(Rc::clone(&create));
        let dispatched = buffered.sink_mut().pop_next().unwrap();
        assert!(Rc::ptr_eq(&dispatched, &create));
        buffered.run_task(&dispatched);

        // The follow-up write arrives while the entity id is still
        // known; it resolves to 42 and goes to the post-id map.
        let follow = task(2, 5, PENDING_DATABASE_ID, 7, &order);
        buffered.add_background_task(Rc::clone(&follow));
        assert_eq!(follow.borrow().db_id(), 42);
        assert_eq!(buffered.size(), 1, "locked on the post-id key");
        assert_eq!(buffered.sink_mut().len(), 1);

        let next = buffered.sink_mut().pop_next().unwrap();
        assert!(Rc::ptr_eq(&next, &follow));
        buffered.run_task(&next);

        // An explicit-id write retires the new-entity entry
        // (`buffered_entity_tasks.cpp:120-123`).
        buffered.add_background_task(task(3, 5, 42, 7, &order));
        assert!(
            buffered.prior_to_dbid_tasks.is_empty(),
            "nothing pre-id is left"
        );
        assert_eq!(buffered.new_entity_map.len(), 0, "the entry retired");
    }

    /// Acceptance: a failed write still plays the next pre-id task —
    /// the queue must drain even though the follower is going to fail
    /// too (`buffered_entity_tasks.cpp:179-182`).
    #[test]
    fn failed_write_still_plays_the_next_pre_dbid_task() {
        let order = Rc::new(RefCell::new(Vec::new()));
        let mut buffered = BufferedEntityTasks::new(TaskQueue::default());

        let t1 = failing_task(1, 5, 0, 3, &order);
        let t2 = task(2, 5, 0, 3, &order);
        buffered.add_background_task(Rc::clone(&t1));
        buffered.add_background_task(Rc::clone(&t2));

        let dispatched = buffered.sink_mut().pop_next().unwrap();
        assert!(Rc::ptr_eq(&dispatched, &t1));
        buffered.run_task(&dispatched);
        assert_eq!(*order.borrow(), ["bg1", "main1:false"]);

        let next = buffered.sink_mut().pop_next().unwrap();
        assert!(Rc::ptr_eq(&next, &t2));
        buffered.run_task(&next);
        assert_eq!(
            *order.borrow(),
            ["bg1", "main1:false", "bg2", "main2:true"],
            "t2 ran despite t1 failing"
        );
    }

    /// Global tasks — no database id, no entity id — always proceed
    /// without touching either map, and their completion passes over
    /// both (`buffered_entity_tasks.cpp:130-132`).
    #[test]
    fn global_tasks_always_proceed() {
        let order = Rc::new(RefCell::new(Vec::new()));
        let mut buffered = BufferedEntityTasks::new(TaskQueue::default());

        buffered.add_background_task(task(1, 0, 0, 0, &order));
        buffered.add_background_task(task(2, 0, 0, 0, &order));

        assert_eq!(buffered.sink_mut().len(), 2);
        assert_eq!(buffered.size(), 0);
        assert!(buffered.prior_to_dbid_tasks.is_empty());

        let g1 = buffered.sink_mut().pop_next().unwrap();
        buffered.run_task(&g1);
        assert_eq!(*order.borrow(), ["bg1", "main1:true"]);
        assert!(buffered.prior_to_dbid_tasks.is_empty());
    }

    /// `shouldDelayAdds` routes dispatch to the delayed queue
    /// (`buffered_entity_tasks.cpp:264-267`).
    #[test]
    fn delayed_adds_route_to_the_delayed_queue() {
        let order = Rc::new(RefCell::new(Vec::new()));
        let mut buffered = BufferedEntityTasks::new(TaskQueue::default());
        assert!(!buffered.should_delay_adds());
        buffered.set_should_delay_adds(true);
        assert!(buffered.should_delay_adds());

        let t1 = task(1, 5, 42, 7, &order);
        buffered.add_background_task(Rc::clone(&t1));

        assert!(buffered.sink_mut().is_empty(), "nothing dispatched yet");
        let held = buffered.sink_mut().pop_delayed().unwrap();
        assert!(Rc::ptr_eq(&held, &t1));
    }

    /// A still-pending task promoted out of the pre-id map resets its
    /// pending id to zero before dispatch
    /// (`buffered_entity_tasks.cpp:301-305`).
    #[test]
    fn pending_follower_resets_to_zero_when_promoted() {
        let order = Rc::new(RefCell::new(Vec::new()));
        let mut buffered = BufferedEntityTasks::new(TaskQueue::default());

        // dbID 0 with an entity id: the pre-id map holds the lock. The
        // follower arrives pending (as an out-of-order caller could
        // construct).
        let t1 = task(1, 5, 0, 3, &order);
        let t2 = task(2, 5, PENDING_DATABASE_ID, 3, &order);
        buffered.add_background_task(Rc::clone(&t1));
        buffered.add_background_task(Rc::clone(&t2));

        let dispatched = buffered.sink_mut().pop_next().unwrap();
        assert!(Rc::ptr_eq(&dispatched, &t1));
        buffered.run_task(&dispatched);
        let next = buffered.sink_mut().pop_next().unwrap();
        assert!(Rc::ptr_eq(&next, &t2));
        assert_eq!(
            next.borrow().db_id(),
            0,
            "the promoted pending id reset to zero"
        );
        buffered.run_task(&next);
        assert_eq!(*order.borrow(), ["bg1", "main1:true", "bg2", "main2:true"]);
    }

    /// A task buffered longer than a second warns with its name, ids
    /// and wait (`doTask`, `buffered_entity_tasks.cpp:249-261`).
    #[test]
    fn long_buffered_tasks_warn_with_the_reference_fields() {
        let order = Rc::new(RefCell::new(Vec::new()));
        let mut buffered = BufferedEntityTasks::new(TaskQueue::default());

        let t1 = task(1, 5, 42, 7, &order);
        let t2 = task(2, 5, 42, 7, &order);
        buffered.add_background_task(Rc::clone(&t1));
        buffered.add_background_task(Rc::clone(&t2));

        // Backdate the buffered stamp past the one-second threshold.
        t2.borrow_mut()
            .set_buffer_timestamp(Some(Instant::now() - std::time::Duration::from_secs(2)));

        assert!(buffered_seconds(&t2) > 1.0);
        assert_eq!(
            buffered_seconds(&t1),
            0.0,
            "a dispatched task never buffered"
        );

        // t1's completion dispatches t2, emitting the warning line.
        let dispatched = buffered.sink_mut().pop_next().unwrap();
        assert!(Rc::ptr_eq(&dispatched, &t1));
        buffered.run_task(&dispatched);
        assert_eq!(*order.borrow(), ["bg1", "main1:true"]);
        assert_eq!(buffered.sink_mut().len(), 1);
    }

    /// A task with a valid id and no entity id locks on its key alone
    /// and its completion plays the post-id map, not the pre-id one.
    #[test]
    fn post_id_task_without_entity_id_completes_through_the_key_map() {
        let order = Rc::new(RefCell::new(Vec::new()));
        let mut buffered = BufferedEntityTasks::new(TaskQueue::default());

        let t1 = task(1, 5, 42, 0, &order);
        let t2 = task(2, 5, 42, 0, &order);
        buffered.add_background_task(Rc::clone(&t1));
        buffered.add_background_task(Rc::clone(&t2));

        assert_eq!(buffered.size(), 2);

        let dispatched = buffered.sink_mut().pop_next().unwrap();
        assert!(Rc::ptr_eq(&dispatched, &t1));
        buffered.run_task(&dispatched);
        let next = buffered.sink_mut().pop_next().unwrap();
        assert!(Rc::ptr_eq(&next, &t2));
        buffered.run_task(&next);
        assert_eq!(*order.borrow(), ["bg1", "main1:true", "bg2", "main2:true"]);
        assert_eq!(buffered.size(), 0);
    }

    /// A completed creation with no entity id records the id and stops
    /// (`onFinishedNewEntity`, `buffered_entity_tasks.cpp:197-200`).
    #[test]
    fn new_entity_completion_without_entity_id_only_records_the_map() {
        let order = Rc::new(RefCell::new(Vec::new()));
        let mut buffered = BufferedEntityTasks::new(TaskQueue::default());

        let t1 = assigning_task(1, 5, PENDING_DATABASE_ID, 0, 11, &order);
        buffered.add_background_task(Rc::clone(&t1));
        let dispatched = buffered.sink_mut().pop_next().unwrap();
        assert!(Rc::ptr_eq(&dispatched, &t1));
        buffered.run_task(&dispatched);

        assert_eq!(*order.borrow(), ["bg1", "main1:true"]);
        assert_eq!(buffered.new_entity_map.get(&0), Some(&11));
    }
}
