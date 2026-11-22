//! BW-07-01-03 registering an entity's script timers (`Base.addTimer`).
//!
//! `Base::py_addTimer` (`server/baseapp/base.cpp`) forwards to `PyTimer`
//! (`common/py_timer.cpp`), which registers timers in a `ScriptTimers`
//! (`lib/server/script_timers.cpp`) held per entity. Each timer maps a small
//! script id (`getNewID`: the smallest unused id starting at 1) to a
//! `TimeQueue` handle:
//!
//! * `addTimer` converts the offsets to ticks at the game update rate
//!   (`initialTicks = time() + uint32(initialOffset * hertz + 0.5)`,
//!   `repeatTicks = uint32(repeatOffset * hertz + 0.5)`, at least 1 when
//!   repeating) and adds the timer to the entity's time queue; a negative
//!   initial offset clamps to 0. A timer that the queue refuses returns 0 and
//!   `addTimer` fails ("Unable to add timer");
//! * when the queue fires, `handleTimeout` looks the script id up by handle
//!   and calls `onTimer(id, userArg)` — an unknown handle logs
//!   "Invalid TimerQueueId" instead;
//! * `delTimer` cancels by id; the queue's `onRelease` then erases the map
//!   entry (`releaseTimer`);
//! * on destroy `Base::onDestroy` calls `pyTimer_.cancelAll()` (`base.cpp`),
//!   cancelling every timer so the queue releases them all — the acceptance:
//!   销毁时对应定时器解除.
//!
//! The model drives a [`crate::timers::TimeQueue`] (game-tick stamps) and
//! records the `onTimer` calls.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use crate::timers::{TimeQueue, TimerHandle, TimerHandler};

/// The script timer id (`ScriptTimers::ScriptID`).
pub type ScriptId = i32;

/// A recorded `onTimer(id, userArg)` call.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TimerEvent {
    /// The script timer id.
    pub id: ScriptId,
    /// The user argument passed to `addTimer`.
    pub user_arg: i32,
}

/// The state shared with the time-queue handler: the id→handle map and the
/// recorded callbacks. Kept behind its own `RefCell` so a handler callback
/// (`on_release`, `handle_timeout`) can reach it while the caller holds the
/// [`ScriptTimers`] borrow — the reference stores the same map on the entity
/// and reaches it through a raw pointer.
#[derive(Debug, Default)]
struct Shared {
    map: BTreeMap<ScriptId, TimerHandle<u32>>,
    events: Vec<TimerEvent>,
    invalid_timeouts: usize,
}

/// The time-queue handler for one entity's timers (`PyTimerHandler`): routes
/// a timeout back to the script id and erases the map entry on release.
struct TimerRouter {
    shared: Rc<RefCell<Shared>>,
}

impl TimerHandler<u32> for TimerRouter {
    /// `PyTimer::handleTimeout`: look the id up and record `onTimer`.
    fn handle_timeout(&self, handle: TimerHandle<u32>, user: usize) {
        let mut shared = self.shared.borrow_mut();
        match find_id(&shared.map, &handle) {
            Some(id) => shared.events.push(TimerEvent {
                id,
                user_arg: user as i32,
            }),
            None => shared.invalid_timeouts += 1,
        }
    }

    /// `PyTimer::onTimerReleased`: erase the entry the queue released.
    fn on_release(&self, handle: TimerHandle<u32>, _user: usize) {
        self.shared.borrow_mut().map.retain(|_, h| h != &handle);
    }
}

/// `ScriptTimers`: one entity's script timer registry over a time queue.
pub struct ScriptTimers {
    hertz: u32,
    queue: TimeQueue,
    handler: Rc<dyn TimerHandler<u32>>,
    shared: Rc<RefCell<Shared>>,
    now: u32,
}

impl ScriptTimers {
    /// Constructor: create the registry over `queue` at `hertz` game ticks per
    /// second, and the handler the queue calls back.
    pub fn new(hertz: u32, queue: TimeQueue) -> Rc<RefCell<Self>> {
        let shared = Rc::new(RefCell::new(Shared::default()));
        let handler: Rc<dyn TimerHandler<u32>> = Rc::new(TimerRouter {
            shared: Rc::clone(&shared),
        });
        Rc::new(RefCell::new(Self {
            hertz,
            queue,
            handler,
            shared,
            now: 0,
        }))
    }

    /// `PyTimer::addTimer`: register a timer and return its script id.
    ///
    /// `initial_offset`/`repeat_offset` are in seconds; a repeat offset of 0
    /// makes the timer fire once. `None` means the queue refused the timer
    /// (the reference raises "Unable to add timer").
    pub fn add_timer(
        &self,
        initial_offset: f32,
        repeat_offset: f32,
        user_arg: i32,
    ) -> Option<ScriptId> {
        let hertz = self.hertz as f32;
        let initial_offset = if initial_offset < 0.0 {
            0.0
        } else {
            initial_offset
        };
        let initial_ticks = self.now.wrapping_add((initial_offset * hertz + 0.5) as u32);
        let repeat_ticks = if repeat_offset > 0.0 {
            let ticks = (repeat_offset * hertz + 0.5) as u32;
            ticks.max(1)
        } else {
            0
        };

        let handle = self.queue.add(
            initial_ticks,
            repeat_ticks,
            Rc::clone(&self.handler),
            user_arg as usize,
            "ScriptTimer",
        );
        if !handle.is_set() {
            return None;
        }

        let mut shared = self.shared.borrow_mut();
        let id = get_new_id(&shared.map);
        shared.map.insert(id, handle);
        Some(id)
    }

    /// `PyTimer::delTimer`: cancel the timer with the given id.
    ///
    /// Returns whether the id was registered. The map entry is erased by the
    /// queue's `onRelease` when the cancel completes.
    pub fn del_timer(&self, id: ScriptId) -> bool {
        let handle = self.shared.borrow().map.get(&id).cloned();
        match handle {
            Some(mut handle) => {
                handle.cancel();
                true
            }
            None => false,
        }
    }

    /// `TimeQueue::process`: advance the queue to `now`, firing the due
    /// timers. Returns the number fired.
    pub fn process(&mut self, now: u32) -> usize {
        self.now = now;
        self.queue.process(now)
    }

    /// `PyTimer::cancelAll`: cancel every timer (the `Base::onDestroy` path).
    ///
    /// Each cancel triggers the queue's `onRelease`, which erases the map
    /// entry, so after this the registry is empty and no timer can fire.
    pub fn cancel_all(&self) {
        let handles: Vec<TimerHandle<u32>> = self.shared.borrow().map.values().cloned().collect();
        for mut handle in handles {
            handle.cancel();
        }
    }

    /// The script id mapped to a handle, if any (`getIDForHandle`).
    pub fn get_id_for_handle(&self, handle: &TimerHandle<u32>) -> Option<ScriptId> {
        find_id(&self.shared.borrow().map, handle)
    }

    /// The number of registered timers.
    pub fn len(&self) -> usize {
        self.shared.borrow().map.len()
    }

    /// Whether no timers are registered.
    pub fn is_empty(&self) -> bool {
        self.shared.borrow().map.is_empty()
    }

    /// The `onTimer` calls recorded so far, in order.
    pub fn events(&self) -> Vec<TimerEvent> {
        self.shared.borrow().events.clone()
    }

    /// How many timeouts arrived for a handle no longer in the registry
    /// (`Invalid TimerQueueId`).
    pub fn invalid_timeouts(&self) -> usize {
        self.shared.borrow().invalid_timeouts
    }
}

/// `ScriptTimers::getNewID`: the smallest unused id starting at 1.
fn get_new_id(map: &BTreeMap<ScriptId, TimerHandle<u32>>) -> ScriptId {
    let mut id = 1;
    while map.contains_key(&id) {
        id += 1;
    }
    id
}

/// `ScriptTimers::findTimer`: find the id by handle identity.
fn find_id(map: &BTreeMap<ScriptId, TimerHandle<u32>>, handle: &TimerHandle<u32>) -> Option<ScriptId> {
    for (id, h) in map {
        if h == handle {
            return Some(*id);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The game tick at which `now` is 10 seconds in (10 ticks per second).
    const TICKS_AT_10S: u32 = 100;

    fn timers() -> Rc<RefCell<ScriptTimers>> {
        ScriptTimers::new(10, TimeQueue::new())
    }

    #[test]
    fn add_timer_returns_consecutive_ids_and_fires_on_timer() {
        let timers = timers();
        let t = timers.borrow();

        assert_eq!(t.add_timer(1.0, 0.0, 5), Some(1));
        assert_eq!(t.add_timer(2.0, 0.0, 7), Some(2));
        assert_eq!(t.add_timer(3.0, 0.0, 9), Some(3));
        assert_eq!(t.len(), 3);
        drop(t);

        // Advance to 1 s (10 ticks): only the 1 s timer fires once, then is
        // released; the 2 s and 3 s timers are still pending.
        let fired = timers.borrow_mut().process(10);
        assert_eq!(fired, 1);
        assert_eq!(
            timers.borrow().events(),
            vec![TimerEvent { id: 1, user_arg: 5 }]
        );
        // The once-off timer was released; the others remain.
        assert_eq!(timers.borrow().len(), 2);
    }

    #[test]
    fn a_repeating_timer_fires_every_interval() {
        let timers = timers();
        assert_eq!(timers.borrow().add_timer(0.0, 1.0, 3), Some(1));
        drop(timers.borrow());

        timers.borrow_mut().process(10); // fires immediately (offset 0)
        timers.borrow_mut().process(15); // interval 1 s -> 10 ticks -> fires
        timers.borrow_mut().process(19); // no fire
        timers.borrow_mut().process(20); // fires again

        assert_eq!(
            timers.borrow().events(),
            vec![
                TimerEvent { id: 1, user_arg: 3 },
                TimerEvent { id: 1, user_arg: 3 },
                TimerEvent { id: 1, user_arg: 3 },
            ]
        );
        // A repeating timer is not released.
        assert_eq!(timers.borrow().len(), 1);
    }

    #[test]
    fn del_timer_cancels_and_releases_the_entry() {
        let timers = timers();
        assert_eq!(timers.borrow().add_timer(1.0, 0.0, 0), Some(1));
        assert_eq!(timers.borrow().len(), 1);

        assert!(timers.borrow().del_timer(1));
        // The cancel triggered the queue's onRelease -> map erased.
        assert_eq!(timers.borrow().len(), 0);
        // It cannot fire any more.
        timers.borrow_mut().process(TICKS_AT_10S);
        assert!(timers.borrow().events().is_empty());

        // An unknown id is refused.
        assert!(!timers.borrow().del_timer(99));
    }

    #[test]
    fn cancel_all_releases_every_timer_on_destroy() {
        let timers = timers();
        assert_eq!(timers.borrow().add_timer(1.0, 0.0, 1), Some(1));
        assert_eq!(timers.borrow().add_timer(1.0, 1.0, 2), Some(2));
        assert_eq!(timers.borrow().add_timer(1.0, 0.0, 3), Some(3));
        assert_eq!(timers.borrow().len(), 3);

        // Base::onDestroy -> pyTimer_.cancelAll().
        timers.borrow().cancel_all();
        assert_eq!(timers.borrow().len(), 0);

        // Nothing can fire: the queue released every handle.
        timers.borrow_mut().process(TICKS_AT_10S);
        assert!(timers.borrow().events().is_empty());
    }

    #[test]
    fn a_negative_offset_clamps_to_zero_and_ids_reuse_smallest_gap() {
        let timers = timers();
        assert_eq!(timers.borrow().add_timer(-5.0, 0.0, 0), Some(1));
        assert_eq!(timers.borrow().add_timer(0.0, 0.0, 0), Some(2));
        // Release id 1, then the next registration reuses the gap.
        timers.borrow().del_timer(1);
        assert_eq!(timers.borrow().add_timer(0.0, 0.0, 0), Some(1));
    }

    #[test]
    fn an_unknown_handle_timeout_counts_an_invalid_timeout() {
        let timers = timers();
        let t = timers.borrow();
        assert_eq!(t.add_timer(1.0, 0.0, 4), Some(1));
        let handle = {
            let map = &t.shared.borrow().map;
            map.values().next().cloned().unwrap()
        };
        // A handle that is no longer registered (as if a del-then-fire race).
        t.del_timer(1);
        drop(t);

        timers.borrow().shared.borrow_mut().events.clear();
        // Fire the released handle directly: no id maps to it.
        t2_fire(&timers, handle);

        assert_eq!(timers.borrow().invalid_timeouts(), 1);
        assert!(timers.borrow().events().is_empty());
    }

    /// Drive one handle's timeout through the router, bypassing the queue.
    fn t2_fire(timers: &Rc<RefCell<ScriptTimers>>, handle: TimerHandle<u32>) {
        let router = Rc::clone(&timers.borrow().handler);
        router.handle_timeout(handle, 4);
    }

    #[test]
    fn get_id_for_handle_matches_the_registered_timer() {
        let timers = timers();
        let t = timers.borrow();
        assert_eq!(t.add_timer(1.0, 0.0, 2), Some(1));
        assert_eq!(t.add_timer(2.0, 0.0, 3), Some(2));
        let handle = {
            let map = &t.shared.borrow().map;
            map.values().next().cloned().unwrap()
        };
        assert_eq!(t.get_id_for_handle(&handle), Some(1));
        assert_eq!(t.get_id_for_handle(&TimerHandle::default()), None);
    }
}
