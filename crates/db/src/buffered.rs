//! Per-entity serialization of database tasks.
//!
//! Two database operations for the same entity must never run at the same
//! time: two overlapping `writeToDB` calls could otherwise be applied in
//! either order (`buffered_entity_tasks.hpp`). One [`BufferedEntityTasks`]
//! guards every entity with a *lock* — the first task for an entity runs,
//! every task that arrives while the lock is held is buffered, and finishing
//! a task plays exactly one buffered task, in submission order.
//!
//! Tasks for an entity that has no database id yet are queued by entity id
//! instead (`priorToDBIDTasks_`); when the create that assigns the id
//! finishes, the queued tasks learn it (`newEntityMap_`) and move over to
//! the by-database-id queues.
//!
//! The reference hands each played task to a background-thread
//! `TaskManager`, which calls `onFinished` when it is done. The port runs
//! synchronously, so that manager loop is the caller's:
//!
//! ```ignore
//! let mut current = match tasks.admit(task) { Admission::Run(t) => t, .. };
//! loop {
//!     current.perform(storage);                    // the background leg
//!     current = match tasks.finish(current) {      // onFinished
//!         Some(next) => next,                      // doTask's played task
//!         None => break,
//!     };
//! }
//! ```

use std::collections::hash_map::Entry;
use std::collections::{HashMap, VecDeque};

use foundation::ids::{DatabaseId, EntityId, NULL_ENTITY_ID, PENDING_DATABASE_ID};

use crate::storage::{EntityKey, EntityStorage};

/// A unit of database work — the port of `EntityTask`
/// (`tasks/entity_task.hpp`).
pub trait EntityTask {
    /// `entityKey()`: the entity this task targets, with the task's current
    /// database id.
    fn entity_key(&self) -> EntityKey;

    /// `entityID()`: the BigWorld entity id the task came from. `0` when the
    /// task carries none — the `EntityTask` default; the write and delete
    /// tasks override it (`entity_task_with_id.hpp:22`).
    fn entity_id(&self) -> EntityId {
        NULL_ENTITY_ID
    }

    /// `dbID()`.
    fn db_id(&self) -> DatabaseId;

    /// `dbID(...)`: rewrite the id. This is how the queue promotes a task —
    /// a buffered task learns its entity's real database id this way.
    fn set_db_id(&mut self, db_id: DatabaseId);

    /// `performBackgroundTask`. The task reports its outcome through
    /// [`EntityTask::set_db_id`]: a create stores the id it was assigned and
    /// a failed write stores 0, which is how the queue marks the write as
    /// failed (`put_entity_task.cpp:85-92`).
    fn perform(&mut self, storage: &mut dyn EntityStorage);
}

/// What [`BufferedEntityTasks::admit`] decided for a task.
pub enum Admission {
    /// The entity's lock was grabbed: run the task, then hand it to
    /// [`BufferedEntityTasks::finish`].
    Run(Box<dyn EntityTask>),
    /// Another task for the entity is outstanding; the task was buffered and
    /// comes back out of [`BufferedEntityTasks::finish`].
    Buffered,
}
/// The `EntityTaskPtr( NULL )` entry that marks the held lock.
enum Slot {
    Running,
    Queued(Box<dyn EntityTask>),
}

type Queue = VecDeque<Slot>;

/// Ensures there is only a single database operation outstanding for a
/// specific entity (`buffered_entity_tasks.hpp`).
#[derive(Default)]
pub struct BufferedEntityTasks {
    /// Outstanding tasks keyed by entity key — the `tasks_` multimap, one
    /// queue per key with the running lock in front.
    tasks: HashMap<EntityKey, Queue>,
    /// Tasks whose entity has no database id yet — `priorToDBIDTasks_`.
    prior_to_db_id: HashMap<EntityId, Queue>,
    /// Entities created without a database id: entity id → assigned id
    /// (`newEntityMap_`).
    new_entity_map: HashMap<EntityId, DatabaseId>,
}

impl BufferedEntityTasks {
    /// `BufferedEntityTasks( bgTaskManager )`.
    pub fn new() -> Self {
        Self::default()
    }

    /// `size()`: the locks and buffered tasks of the by-database-id map —
    /// the value the watchdog reads (`mysql_database.cpp:97`).
    pub fn size(&self) -> usize {
        self.tasks.values().map(Queue::len).sum()
    }

    /// `grabLock` + (`doTask` | `buffer`): run the task when its entity is
    /// idle, queue it behind the held lock otherwise.
    pub fn admit(&mut self, mut task: Box<dyn EntityTask>) -> Admission {
        let mut db_id = task.db_id();
        let entity_id = task.entity_id();

        if db_id == PENDING_DATABASE_ID {
            // A task for an entity being created picks up the id the create
            // was assigned in the meantime.
            if let Some(assigned) = self.new_entity_map.get(&entity_id) {
                db_id = *assigned;
                task.set_db_id(db_id);
            }
        } else if db_id != 0 {
            // The entity has a real id again; the create bookkeeping is done.
            self.new_entity_map.remove(&entity_id);
        }

        if is_valid_db_id(db_id) {
            if Self::grab_lock(&mut self.tasks, task.entity_key()) {
                Admission::Run(task)
            } else {
                self.buffer(task);
                Admission::Buffered
            }
        } else if entity_id != NULL_ENTITY_ID {
            if Self::grab_lock(&mut self.prior_to_db_id, entity_id) {
                Admission::Run(task)
            } else {
                self.buffer(task);
                Admission::Buffered
            }
        } else {
            // Always allow a task with no database id or entity id to proceed.
            Admission::Run(task)
        }
    }

    /// `onFinished`: release the finished task's lock and return the next
    /// task to run, if any.
    pub fn finish(&mut self, task: Box<dyn EntityTask>) -> Option<Box<dyn EntityTask>> {
        let key = task.entity_key();

        if let Some(next) = Self::release_lock(&mut self.tasks, key) {
            return Some(next);
        }

        if key.db_id != 0 {
            self.on_finished_new_entity(task)
        } else {
            // The write failed. Play the next task even though we know it is
            // going to fail. This could be optimised but not worthwhile for
            // such a rare event. (buffered_entity_tasks.cpp:157-162)
            Self::release_lock(&mut self.prior_to_db_id, task.entity_id())
        }
    }

    /// `onFinishedNewEntity`: remember the assigned id and move the tasks
    /// that were queued without one onto the by-database-id queues.
    fn on_finished_new_entity(&mut self, task: Box<dyn EntityTask>) -> Option<Box<dyn EntityTask>> {
        let db_id = task.db_id();
        let entity_id = task.entity_id();

        self.new_entity_map.insert(entity_id, db_id);
        if entity_id == NULL_ENTITY_ID {
            return None;
        }

        let queue = self.prior_to_db_id.get_mut(&entity_id)?;
        debug_assert!(matches!(queue.front(), Some(Slot::Running)));
        // Everything after the running lock, in order.
        let mut followups = Vec::new();
        while let Some(slot) = queue.pop_back() {
            match slot {
                Slot::Queued(task) => followups.push(task),
                Slot::Running => break,
            }
        }
        followups.reverse();
        self.prior_to_db_id.remove(&entity_id);

        // The first promoted task that can grab its key's lock runs now; the
        // rest re-buffer behind the lock it just grabbed.
        let mut play = None;
        for mut task in followups {
            task.set_db_id(db_id);
            let key = task.entity_key();
            if Self::grab_lock(&mut self.tasks, key) {
                debug_assert!(
                    play.is_none(),
                    "only the first promoted task can grab a lock"
                );
                play = Some(task);
            } else {
                self.buffer(task);
            }
        }
        play
    }

    /// `buffer`: queue a task behind its entity's held lock.
    fn buffer(&mut self, task: Box<dyn EntityTask>) {
        let key = task.entity_key();
        if is_valid_db_id(key.db_id) {
            let queue = self.tasks.entry(key).or_default();
            debug_assert!(matches!(queue.front(), Some(Slot::Running)));
            queue.push_back(Slot::Queued(task));
        } else {
            let queue = self.prior_to_db_id.entry(task.entity_id()).or_default();
            debug_assert!(matches!(queue.front(), Some(Slot::Running)));
            queue.push_back(Slot::Queued(task));
        }
    }

    /// `grabLockT`: an absent entry inserts the lock and succeeds; a held
    /// lock refuses.
    fn grab_lock<K: Eq + std::hash::Hash>(map: &mut HashMap<K, Queue>, id: K) -> bool {
        match map.entry(id) {
            Entry::Occupied(_) => false,
            Entry::Vacant(slot) => {
                let mut queue = Queue::new();
                queue.push_back(Slot::Running);
                slot.insert(queue);
                true
            }
        }
    }

    /// `playNextTask`: remove the finished task's lock and hand back the
    /// task that becomes the new lock holder.
    fn release_lock<K: Eq + std::hash::Hash + Copy>(
        map: &mut HashMap<K, Queue>,
        id: K,
    ) -> Option<Box<dyn EntityTask>> {
        let queue = map.get_mut(&id)?;
        if !matches!(queue.front(), Some(Slot::Running)) {
            // Not from this map.
            return None;
        }

        queue.pop_front();
        let mut next = match queue.pop_front() {
            Some(Slot::Queued(task)) => task,
            _ => {
                map.remove(&id);
                return None;
            }
        };
        queue.push_front(Slot::Running);

        if next.db_id() == PENDING_DATABASE_ID {
            next.set_db_id(0);
        }
        Some(next)
    }
}

/// `isValidDBID`: an id a row can be addressed by — not the "unknown" 0 and
/// not the pending id of an entity being created.
fn is_valid_db_id(db_id: DatabaseId) -> bool {
    db_id != 0 && db_id != PENDING_DATABASE_ID
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    use entitydef::value::PropertyValue;

    use crate::auto_load::UpdateAutoLoad;
    use crate::error::DbError;
    use crate::record::EntityRecord;
    use crate::storage::MemoryStorage;

    /// A write task that records when it ran and under which database id.
    struct WriteTask {
        key: EntityKey,
        entity_id: EntityId,
        marker: &'static str,
        /// Force the write to fail the way a failed put does: no row to
        /// update, so the task reports database id 0.
        fail: bool,
        order: Rc<RefCell<Vec<String>>>,
    }

    impl WriteTask {
        fn create(type_id: u16, entity_id: EntityId, marker: &'static str) -> Box<Self> {
            Self::write(
                EntityKey {
                    type_id,
                    db_id: PENDING_DATABASE_ID,
                },
                entity_id,
                marker,
            )
        }

        fn by_id(
            type_id: u16,
            db_id: DatabaseId,
            entity_id: EntityId,
            marker: &'static str,
        ) -> Box<Self> {
            Self::write(EntityKey { type_id, db_id }, entity_id, marker)
        }

        fn write(key: EntityKey, entity_id: EntityId, marker: &'static str) -> Box<Self> {
            Box::new(Self {
                key,
                entity_id,
                marker,
                fail: false,
                order: Rc::new(RefCell::new(Vec::new())),
            })
        }

        /// Share the order log with a test assertion.
        fn log(&self) -> Rc<RefCell<Vec<String>>> {
            self.order.clone()
        }
    }

    impl EntityTask for WriteTask {
        fn entity_key(&self) -> EntityKey {
            self.key
        }

        fn entity_id(&self) -> EntityId {
            self.entity_id
        }

        fn db_id(&self) -> DatabaseId {
            self.key.db_id
        }

        fn set_db_id(&mut self, db_id: DatabaseId) {
            self.key.db_id = db_id;
        }

        fn perform(&mut self, storage: &mut dyn EntityStorage) {
            let values = vec![PropertyValue::String(self.marker.into())];
            let outcome = if self.fail {
                Err(DbError::NotFound)
            } else if self.key.db_id == PENDING_DATABASE_ID {
                // A create: insert without an id and take the assigned one.
                storage
                    .put_entity(
                        &EntityKey {
                            type_id: self.key.type_id,
                            db_id: PENDING_DATABASE_ID,
                        },
                        &values,
                        false,
                        UpdateAutoLoad::Retain,
                    )
                    .inspect(|db_id| self.set_db_id(*db_id))
            } else {
                // An update of the entity the task now addresses.
                storage.put_entity(&self.key, &values, false, UpdateAutoLoad::Retain)
            };

            if outcome.is_err() {
                self.set_db_id(0);
            }
            self.order
                .borrow_mut()
                .push(format!("{}@{}", self.marker, self.key.db_id));
        }
    }

    /// A read-only task, like the reference's get tasks: it carries no
    /// entity id, so the `EntityTask` default applies (`entity_task.hpp:27`).
    struct FetchTask {
        key: EntityKey,
        order: Rc<RefCell<Vec<String>>>,
    }

    impl EntityTask for FetchTask {
        fn entity_key(&self) -> EntityKey {
            self.key
        }

        fn db_id(&self) -> DatabaseId {
            self.key.db_id
        }

        fn set_db_id(&mut self, db_id: DatabaseId) {
            self.key.db_id = db_id;
        }

        fn perform(&mut self, storage: &mut dyn EntityStorage) {
            let found = storage
                .get_entity(&self.key)
                .expect("the fetch reads")
                .is_some();
            self.order
                .borrow_mut()
                .push(format!("fetch@{}:{}", self.key.db_id, found));
        }
    }

    fn run(task: &mut dyn EntityTask, storage: &mut dyn EntityStorage) {
        task.perform(storage);
    }

    /// Mark a write task as one that fails the way a failed put does.
    fn failing(mut task: Box<WriteTask>) -> Box<WriteTask> {
        task.fail = true;
        task
    }

    /// Admit a task that must grab its entity's lock.
    fn must_run(tasks: &mut BufferedEntityTasks, task: Box<dyn EntityTask>) -> Box<dyn EntityTask> {
        match tasks.admit(task) {
            Admission::Run(task) => task,
            Admission::Buffered => panic!("the task should have grabbed the lock"),
        }
    }

    fn row_of(store: &MemoryStorage, type_id: u16, db_id: DatabaseId) -> EntityRecord {
        store
            .get_entity(&EntityKey { type_id, db_id })
            .expect("the store reads")
            .expect("the row exists")
    }

    #[test]
    fn same_entity_writes_finish_in_submission_order() {
        let mut store = MemoryStorage::new();
        let mut tasks = BufferedEntityTasks::new();

        // Two writes to the same new entity, the first while the second
        // arrives: the second must wait for the first's lock and then run
        // with the database id the create was assigned.
        let a = WriteTask::create(3, 7, "a");
        let log = a.log();
        let b = WriteTask::create(3, 7, "b");
        let b_log = b.log();
        let c = WriteTask::create(3, 7, "c");
        let c_log = c.log();

        let mut a = must_run(&mut tasks, a);
        assert!(matches!(tasks.admit(b), Admission::Buffered));
        assert!(matches!(tasks.admit(c), Admission::Buffered));

        run(a.as_mut(), &mut store);
        let mut b = tasks.finish(a).expect("the buffered write is played next");
        run(b.as_mut(), &mut store);
        let mut c = tasks
            .finish(b)
            .expect("the second buffered write is played next");
        run(c.as_mut(), &mut store);
        assert!(tasks.finish(c).is_none());

        assert_eq!(
            *log.borrow(),
            ["a@1".to_owned()],
            "the create reports its assigned id"
        );
        assert_eq!(
            *b_log.borrow(),
            ["b@1".to_owned()],
            "the buffered write runs with the real id"
        );
        assert_eq!(*c_log.borrow(), ["c@1".to_owned()]);
        assert_eq!(
            row_of(&store, 3, 1).values,
            vec![PropertyValue::String("c".into())],
            "the last write landed last"
        );
        assert_eq!(tasks.size(), 0);
    }

    #[test]
    fn a_pending_write_picks_up_the_new_entity_s_id_on_admission() {
        let mut store = MemoryStorage::new();
        let mut tasks = BufferedEntityTasks::new();

        // The create finishes before the next write arrives: its id is now
        // known, so the pending write is rewritten on admission instead of
        // being queued by entity id.
        let a = WriteTask::create(3, 7, "a");
        let b = WriteTask::create(3, 7, "b");
        let c = WriteTask::create(3, 7, "c");
        let c_log = c.log();

        let mut a = must_run(&mut tasks, a);
        run(a.as_mut(), &mut store);
        assert!(tasks.finish(a).is_none());

        // The next write is rewritten to id 1 and takes the key's lock; the
        // one after that finds the lock held and buffers.
        let mut b = must_run(&mut tasks, b);
        assert!(matches!(tasks.admit(c), Admission::Buffered));

        run(b.as_mut(), &mut store);
        let mut c = tasks.finish(b).expect("the buffered write is played next");
        run(c.as_mut(), &mut store);
        assert!(tasks.finish(c).is_none());

        assert_eq!(*c_log.borrow(), ["c@1".to_owned()]);
        assert_eq!(store.all_entities().expect("the store lists").len(), 1);
        assert_eq!(tasks.size(), 0);
    }

    #[test]
    fn a_failed_write_still_plays_the_next_task() {
        let mut store = MemoryStorage::new();
        let mut tasks = BufferedEntityTasks::new();

        // The first write fails and reports id 0; the queued one still runs
        // — with its pending id rewritten to 0 — so both report failure in
        // order and the queue drains.
        let a = failing(WriteTask::create(3, 7, "a"));
        let b = WriteTask::create(3, 7, "b");
        let a_log = a.log();
        let b_log = b.log();

        let mut a = must_run(&mut tasks, a);
        assert!(matches!(tasks.admit(b), Admission::Buffered));

        run(a.as_mut(), &mut store);
        let mut b = tasks
            .finish(a)
            .expect("the next task plays even after a failure");
        assert_eq!(
            b.db_id(),
            0,
            "the pending id was rewritten to the failed marker"
        );
        run(b.as_mut(), &mut store);
        assert!(tasks.finish(b).is_none());

        assert_eq!(*a_log.borrow(), ["a@0".to_owned()]);
        assert_eq!(*b_log.borrow(), ["b@0".to_owned()]);
        assert!(store.all_entities().expect("the store lists").is_empty());
    }

    #[test]
    fn a_write_to_a_known_entity_waits_for_its_lock() {
        let mut store = MemoryStorage::new();
        store.insert(EntityRecord {
            type_id: 3,
            db_id: 5,
            values: vec![PropertyValue::String("seed".into())],
        });
        let mut tasks = BufferedEntityTasks::new();

        let a = WriteTask::by_id(3, 5, 42, "a");
        let b = WriteTask::by_id(3, 5, 42, "b");
        let log = a.log();
        let b_log = b.log();

        let mut a = must_run(&mut tasks, a);
        assert!(matches!(tasks.admit(b), Admission::Buffered));
        assert_eq!(tasks.size(), 2, "the lock and the buffered task");

        run(a.as_mut(), &mut store);
        let mut b = tasks.finish(a).expect("the buffered write is played next");
        assert_eq!(tasks.size(), 1, "the played task now holds the lock");

        run(b.as_mut(), &mut store);
        assert!(tasks.finish(b).is_none());
        assert_eq!(tasks.size(), 0);

        assert_eq!(*log.borrow(), ["a@5".to_owned()]);
        assert_eq!(*b_log.borrow(), ["b@5".to_owned()]);
    }

    #[test]
    fn tasks_without_ids_are_never_serialized() {
        let mut store = MemoryStorage::new();
        let mut tasks = BufferedEntityTasks::new();

        // A task with neither a database id nor an entity id cannot collide
        // with anything, so both proceed without touching any queue.
        let a = failing(WriteTask::by_id(3, 0, 0, "a"));
        let b = failing(WriteTask::by_id(3, 0, 0, "b"));
        let log = a.log();
        let b_log = b.log();

        let mut a = must_run(&mut tasks, a);
        let mut b = must_run(&mut tasks, b);
        assert_eq!(tasks.size(), 0, "no lock was ever taken");

        run(a.as_mut(), &mut store);
        run(b.as_mut(), &mut store);
        assert!(tasks.finish(a).is_none());
        assert!(tasks.finish(b).is_none());
        assert_eq!(*log.borrow(), ["a@0".to_owned()]);
        assert_eq!(*b_log.borrow(), ["b@0".to_owned()]);
    }

    #[test]
    fn a_real_id_write_clears_the_new_entity_bookkeeping() {
        let mut store = MemoryStorage::new();
        let mut tasks = BufferedEntityTasks::new();

        // The create records entity 7's id; a write with the real id erases
        // it, so the next pending write queues by entity id again instead of
        // being rewritten onto the busy key.
        let a = WriteTask::create(3, 7, "a");
        let g = WriteTask::by_id(3, 1, 7, "g");
        let h = WriteTask::create(3, 7, "h");
        let h_log = h.log();

        let mut a = must_run(&mut tasks, a);
        run(a.as_mut(), &mut store);
        assert!(tasks.finish(a).is_none(), "the create records the id");

        let mut g = must_run(&mut tasks, g);
        let mut h = must_run(&mut tasks, h);

        run(g.as_mut(), &mut store);
        assert!(tasks.finish(g).is_none());
        run(h.as_mut(), &mut store);
        assert!(tasks.finish(h).is_none());

        // h ran with its pending id — it was never rewritten — and created
        // the second row.
        assert_eq!(*h_log.borrow(), ["h@2".to_owned()]);
        assert_eq!(store.all_entities().expect("the store lists").len(), 2);
    }

    #[test]
    fn a_task_that_arrives_midflight_is_played_by_the_next_finish() {
        let mut store = MemoryStorage::new();
        store.insert(EntityRecord {
            type_id: 3,
            db_id: 5,
            values: vec![PropertyValue::String("seed".into())],
        });
        let mut tasks = BufferedEntityTasks::new();

        // A write holds the lock mid-flight when a second one arrives: it
        // buffers and nothing of it runs.
        let a = WriteTask::by_id(3, 5, 42, "a");
        let log = a.log();
        let b = WriteTask::by_id(3, 5, 42, "b");
        let b_log = b.log();

        let mut held = must_run(&mut tasks, a);
        assert!(
            matches!(tasks.admit(b), Admission::Buffered),
            "the midflight write buffers"
        );
        assert!(log.borrow().is_empty(), "the buffered write did not run");
        assert_eq!(tasks.size(), 2);

        // Running the first write and finishing it plays the buffered one.
        held.perform(&mut store);
        let mut played = tasks.finish(held).expect("the buffered write is played");
        assert_eq!(tasks.size(), 1, "the played task now holds the lock");

        played.perform(&mut store);
        assert!(tasks.finish(played).is_none());

        assert_eq!(*log.borrow(), ["a@5".to_owned()]);
        assert_eq!(*b_log.borrow(), ["b@5".to_owned()]);
        assert_eq!(
            row_of(&store, 3, 5).values,
            vec![PropertyValue::String("b".into())]
        );
    }

    #[test]
    fn a_chain_of_writes_to_one_entity_lands_in_submission_order() {
        let mut store = MemoryStorage::new();
        let mut tasks = BufferedEntityTasks::new();

        // Three writes to one new entity submitted one after another: the
        // first creates the row, the rest run against the assigned id, all
        // in submission order.
        let a = WriteTask::create(3, 7, "a");
        let log = a.log();
        let b = WriteTask::create(3, 7, "b");
        let b_log = b.log();
        let c = WriteTask::create(3, 7, "c");
        let c_log = c.log();

        let mut a = must_run(&mut tasks, a);
        a.perform(&mut store);
        assert!(tasks.finish(a).is_none());

        let mut b = must_run(&mut tasks, b);
        b.perform(&mut store);
        assert!(tasks.finish(b).is_none());

        let mut c = must_run(&mut tasks, c);
        c.perform(&mut store);
        assert!(tasks.finish(c).is_none());

        assert_eq!(*log.borrow(), ["a@1".to_owned()]);
        assert_eq!(*b_log.borrow(), ["b@1".to_owned()]);
        assert_eq!(*c_log.borrow(), ["c@1".to_owned()]);
        assert_eq!(tasks.size(), 0);
        assert_eq!(
            row_of(&store, 3, 1).values,
            vec![PropertyValue::String("c".into())],
            "the last write landed last"
        );
    }

    #[test]
    fn a_fetch_without_an_entity_id_serializes_by_database_id_alone() {
        let mut store = MemoryStorage::new();
        store.insert(EntityRecord {
            type_id: 3,
            db_id: 5,
            values: vec![PropertyValue::String("seed".into())],
        });
        let mut tasks = BufferedEntityTasks::new();

        // Gets carry no entity id, so the trait default's 0 applies and
        // finishing one registers nothing about any entity.
        let mut fetch = Box::new(FetchTask {
            key: EntityKey {
                type_id: 3,
                db_id: 5,
            },
            order: Rc::new(RefCell::new(Vec::new())),
        });
        let log = fetch.order.clone();

        // A task's id may be rewritten by the queue (a promoted write learns
        // its entity's id); the fetch carries the same accessors.
        fetch.set_db_id(7);
        assert_eq!(fetch.entity_key().db_id, 7);
        fetch.set_db_id(5);
        assert_eq!(fetch.entity_key().db_id, 5);

        let mut fetch = must_run(&mut tasks, fetch);
        fetch.perform(&mut store);
        assert!(tasks.finish(fetch).is_none());

        assert_eq!(*log.borrow(), ["fetch@5:true".to_owned()]);
        assert_eq!(tasks.size(), 0);
    }
}
